use alvr_common::{ConnectionError, anyhow::Result, debug, error, info, parking_lot::Mutex, warn};
use alvr_session::AudioBufferingConfig;
use alvr_sockets::{StreamReceiver, StreamSender};

use std::os::unix::fs::FileTypeExt;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    fs, io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, sleep},
    time::Duration,
};

use pipewire::{
    channel::Receiver,
    context::ContextRc,
    core::CoreRc,
    keys,
    main_loop::MainLoopRc,
    metadata::{Metadata, MetadataListener},
    properties,
    registry::GlobalObject,
    spa::utils::dict::DictRef,
    types::ObjectType,
    spa::{
        param::audio::{AudioFormat, AudioInfoRaw},
        pod::{self, Pod, Value, serialize::PodSerializer},
        utils::Direction,
    },
    stream::{StreamFlags, StreamListener, StreamRc, StreamState},
};

pub fn try_load_pipewire() -> Result<()> {
    if let Err(e) = probe_pipewire() {
        if !matches!(e, pipewire::Error::CreationFailed) {
            return Err(e.into());
        }
        error!("Could not initialize PipeWire.");

        let is_under_flatpak = std::env::var("FLATPAK_ID").is_ok();
        let is_pw_socket_available =
            std::env::var("XDG_RUNTIME_DIR").is_ok_and(|xdg_runtime_dir| {
                let pw_socket_path = Path::new(&xdg_runtime_dir).join("pipewire-0");
                fs::metadata(&pw_socket_path).is_ok_and(|m| m.file_type().is_socket())
            });

        if is_under_flatpak && !is_pw_socket_available {
            error!(
                "Please visit the following page to find help on how to fix broken audio on flatpak."
            );
            error!(
                "https://github.com/alvr-org/ALVR/wiki/Installing-ALVR-and-using-SteamVR-on-Linux-through-Flatpak#failed-to-create-pipewire-errors"
            );
        }
        error!("Make sure PipeWire is installed on your system, running and it's version is at least 0.3.49.
        To retry, please restart SteamVR with ALVR.");
    }
    Ok(())
}

fn probe_pipewire() -> Result<(), pipewire::Error> {
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    context.connect(None)?;
    Ok(())
}

#[derive(Clone, Copy)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channel_count: u32,
}

struct Terminate;

// fixme: Opening pavucontrol while audio is actively streaming
//  will cause audio cut out for short time,
//  possibly related to fast state changes caused by pavucontrol
static MIC_STREAMING: AtomicBool = AtomicBool::new(false);

pub fn audio_loop(
    is_running: impl Fn() -> bool,
    sender: StreamSender<()>,
    speaker_info: Option<AudioInfo>,
    receiver: &mut StreamReceiver<()>,
    mic_info: Option<(AudioInfo, AudioBufferingConfig)>,
    switch_default_devices: bool,
) {
    let sample_queue = Arc::new(Mutex::new(VecDeque::new()));
    MIC_STREAMING.store(false, Ordering::Relaxed);

    let (pw_sender, pw_receiver) = pipewire::channel::channel();

    // Stall pipewire startup until we're actually streaming to not cause latency by packet buildup
    if !is_running() {
        return;
    }

    let pw_thread = thread::spawn({
        let sample_queue = sample_queue.clone();
        let mic_info = mic_info.as_ref().map(|(info, _)| *info);

        move || {
            if let Err(e) = pw_main_loop(
                pw_receiver,
                sender,
                speaker_info,
                sample_queue,
                mic_info,
                switch_default_devices,
            ) {
                error!("Unhandled pipewire audio device error, please report it on GitHub: {e}");
            }
            debug!("Pipewire audio loop exiting");
        }
    });

    while is_running() {
        if let Some((mic_info, buffering)) = &mic_info {
            let rate = mic_info.sample_rate as usize;

            let batch_frames_count = rate * buffering.batch_ms as usize / 1000;
            let average_buffer_frames_count = rate * buffering.average_buffering_ms as usize / 1000;

            if let Err(e) = crate::receive_samples_loop(
                || is_running() && MIC_STREAMING.load(Ordering::Relaxed),
                receiver,
                sample_queue.clone(),
                mic_info.channel_count as usize,
                batch_frames_count,
                average_buffer_frames_count,
            ) {
                error!("Receive samples loop encountered error {e:?}");
            }

            // if we end up here then no consumer is currently connected to the output
            // so discard audio packets to not cause a buildup
            if matches!(
                receiver.recv(Duration::from_millis(500)),
                Err(ConnectionError::Other(_))
            ) {
                break;
            }
        } else {
            sleep(Duration::from_millis(500));
        }
    }

    if pw_sender.send(Terminate).is_err() {
        error!(
            "Couldn't send pipewire termination signal, deinitializing forcefully.
                Restart the VR app to reinitialize the audio device."
        );

        unsafe { pipewire::deinit() };
    }

    pw_thread.join().ok();
}

fn pw_main_loop(
    pw_receiver: Receiver<Terminate>,
    audio_sender: StreamSender<()>,
    speaker_info: Option<AudioInfo>,
    sample_queue: Arc<Mutex<VecDeque<f32>>>,
    mic_info: Option<AudioInfo>,
    switch_default_devices: bool,
) -> Result<(), pipewire::Error> {
    debug!("Starting pipewire thread");
    let mainloop = MainLoopRc::new(None)?;

    let _receiver = pw_receiver.attach(mainloop.as_ref(), {
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });

    let context = ContextRc::new(&mainloop, None)?;
    let pw_core = context.connect_rc(None)?;

    let has_speaker = speaker_info.is_some();
    let has_mic = mic_info.is_some();

    let _speaker = if let Some(info) = speaker_info {
        debug!("Creating pw output audio stream");
        Some(create_speaker_stream(
            &pw_core,
            audio_sender,
            info.sample_rate,
            info.channel_count,
        )?)
    } else {
        None
    };

    let _mic = if let Some(info) = mic_info {
        debug!("Creating pw microphone stream");
        Some(create_mic_stream(
            &pw_core,
            sample_queue,
            info.sample_rate,
            info.channel_count,
        )?)
    } else {
        None
    };

    let default_switch = if switch_default_devices && (has_speaker || has_mic) {
        match DefaultDeviceSwitch::start(&pw_core, has_speaker, has_mic) {
            Ok(switch) => Some(switch),
            Err(e) => {
                warn!("Could not switch the default audio devices to the headset: {e}");
                None
            }
        }
    } else {
        None
    };

    debug!("Running pipewire thread");
    mainloop.run();

    if let Some(switch) = default_switch {
        switch.restore(&mainloop, &pw_core);
    }

    Ok(())
}

// KiraVR: Point the PipeWire defaults at the headset while streaming and put
// back whatever the user had before. These are the same keys `wpctl
// set-default` and desktop sound settings write.
const SPEAKER_NODE_NAME: &str = "KiraVR Audio";
const MIC_NODE_NAME: &str = "KiraVR Microphone";
// Names used before the rename. A saved default pointing at one of these is
// stale and must not be restored.
const LEGACY_NODE_NAMES: &[&str] = &["ALVR Audio", "ALVR Microphone"];
const DEFAULT_SINK_KEY: &str = "default.configured.audio.sink";
const DEFAULT_SOURCE_KEY: &str = "default.configured.audio.source";
// What the session manager is actually using right now. Used when the
// configured value is missing or still names an ALVR node.
const EFFECTIVE_SINK_KEY: &str = "default.audio.sink";
const EFFECTIVE_SOURCE_KEY: &str = "default.audio.source";
const JSON_TYPE: &str = "Spa:String:JSON";
const RESTORE_TIMEOUT: Duration = Duration::from_millis(500);

fn node_json(name: &str) -> String {
    format!("{{\"name\":\"{name}\"}}")
}

fn is_alvr_node(value: &str) -> bool {
    value.contains(SPEAKER_NODE_NAME)
        || value.contains(MIC_NODE_NAME)
        || LEGACY_NODE_NAMES.iter().any(|n| value.contains(n))
}

#[derive(Default)]
struct DefaultSwitchState {
    metadata: RefCell<Option<Metadata>>,
    metadata_listener: RefCell<Option<MetadataListener>>,
    // The user's values from before the switch. None means the key was unset,
    // so restoring removes it and the session manager picks a default again.
    saved_sink: RefCell<Option<String>>,
    saved_source: RefCell<Option<String>>,
    effective_sink: RefCell<Option<String>>,
    effective_source: RefCell<Option<String>>,
    pending_apply: RefCell<Option<pipewire::spa::utils::result::AsyncSeq>>,
    pending_restore: RefCell<Option<pipewire::spa::utils::result::AsyncSeq>>,
    applied: Cell<bool>,
    switch_sink: bool,
    switch_source: bool,
}

struct DefaultDeviceSwitch {
    state: Rc<DefaultSwitchState>,
    _registry: pipewire::registry::RegistryRc,
    _registry_listener: pipewire::registry::Listener,
    _core_listener: pipewire::core::Listener,
}

impl DefaultDeviceSwitch {
    fn start(pw_core: &CoreRc, switch_sink: bool, switch_source: bool) -> Result<Self, pipewire::Error> {
        let state = Rc::new(DefaultSwitchState {
            switch_sink,
            switch_source,
            ..Default::default()
        });

        // Apply once the server has answered the sync issued right after
        // binding. By then it has sent the metadata's current values, so the
        // user's defaults are saved before they are overwritten.
        let core_listener = pw_core
            .add_listener_local()
            .done({
                let state = Rc::clone(&state);
                move |id, seq| {
                    if id != pipewire::core::PW_ID_CORE {
                        return;
                    }
                    if state.pending_apply.borrow().as_ref() == Some(&seq) {
                        state.pending_apply.replace(None);
                        state.apply();
                    }
                }
            })
            .register();

        let registry = pw_core.get_registry_rc()?;
        let registry_weak = registry.downgrade();
        let registry_listener = registry
            .add_listener_local()
            .global({
                let state = Rc::clone(&state);
                let pw_core = pw_core.clone();
                move |global: &GlobalObject<&DictRef>| {
                    if global.type_ != ObjectType::Metadata
                        || state.metadata.borrow().is_some()
                        || global.props.and_then(|p| p.get("metadata.name")) != Some("default")
                    {
                        return;
                    }
                    let Some(registry) = registry_weak.upgrade() else {
                        return;
                    };
                    let metadata: Metadata = match registry.bind(global) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!("Could not bind the PipeWire default metadata: {e}");
                            return;
                        }
                    };

                    let listener = metadata
                        .add_listener_local()
                        .property({
                            let state = Rc::clone(&state);
                            move |subject, key, _type, value| {
                                state.record(subject, key, value);
                                0
                            }
                        })
                        .register();

                    state.metadata.replace(Some(metadata));
                    state.metadata_listener.replace(Some(listener));

                    match pw_core.sync(0) {
                        Ok(seq) => {
                            state.pending_apply.replace(Some(seq));
                        }
                        Err(e) => warn!("PipeWire sync failed, not switching audio defaults: {e}"),
                    }
                }
            })
            .register();

        Ok(Self {
            state,
            _registry: registry,
            _registry_listener: registry_listener,
            _core_listener: core_listener,
        })
    }

    // Puts the user's defaults back, then waits (bounded) for the server to
    // take the change, since the connection closes right after this.
    fn restore(self, mainloop: &MainLoopRc, pw_core: &CoreRc) {
        let state = &self.state;
        if !state.applied.get() {
            return;
        }
        let Some(metadata) = state.metadata.borrow_mut().take() else {
            return;
        };

        let put_back = |key: &str, saved: &Option<String>| match saved {
            Some(value) => metadata.set_property(0, key, Some(JSON_TYPE), Some(value)),
            None => metadata.set_property(0, key, None, None),
        };
        let pick = |saved: &RefCell<Option<String>>, effective: &RefCell<Option<String>>| {
            saved.borrow().clone().or_else(|| effective.borrow().clone())
        };
        if state.switch_sink {
            put_back(DEFAULT_SINK_KEY, &pick(&state.saved_sink, &state.effective_sink));
        }
        if state.switch_source {
            put_back(DEFAULT_SOURCE_KEY, &pick(&state.saved_source, &state.effective_source));
        }

        let seq = match pw_core.sync(0) {
            Ok(seq) => seq,
            Err(e) => {
                warn!("PipeWire sync failed while restoring audio defaults: {e}");
                return;
            }
        };
        state.pending_restore.replace(Some(seq));

        let _done_listener = pw_core
            .add_listener_local()
            .done({
                let state = Rc::clone(state);
                let mainloop = mainloop.clone();
                move |id, seq| {
                    if id == pipewire::core::PW_ID_CORE
                        && state.pending_restore.borrow().as_ref() == Some(&seq)
                    {
                        state.pending_restore.replace(None);
                        mainloop.quit();
                    }
                }
            })
            .register();
        let timeout = mainloop.loop_().add_timer({
            let mainloop = mainloop.clone();
            move |_| mainloop.quit()
        });
        timeout.update_timer(Some(RESTORE_TIMEOUT), None);

        mainloop.run();

        if state.pending_restore.borrow().is_some() {
            warn!("PipeWire did not confirm the restored audio defaults in time");
        } else {
            info!("Restored the previous default audio devices");
        }
    }
}

impl DefaultSwitchState {
    fn record(&self, subject: u32, key: Option<&str>, value: Option<&str>) {
        // Only the user's values from before the switch matter, and never our
        // own (a previous session that crashed before restoring leaves ALVR
        // names behind, which must not be "restored").
        if subject != 0 || self.applied.get() {
            return;
        }
        let value = value.filter(|v| !is_alvr_node(v)).map(str::to_owned);
        match key {
            Some(DEFAULT_SINK_KEY) => {
                self.saved_sink.replace(value);
            }
            Some(DEFAULT_SOURCE_KEY) => {
                self.saved_source.replace(value);
            }
            Some(EFFECTIVE_SINK_KEY) => {
                self.effective_sink.replace(value);
            }
            Some(EFFECTIVE_SOURCE_KEY) => {
                self.effective_source.replace(value);
            }
            _ => (),
        }
    }

    fn apply(&self) {
        let metadata = self.metadata.borrow();
        let Some(metadata) = metadata.as_ref() else {
            return;
        };
        if self.switch_sink {
            metadata.set_property(
                0,
                DEFAULT_SINK_KEY,
                Some(JSON_TYPE),
                Some(&node_json(SPEAKER_NODE_NAME)),
            );
        }
        if self.switch_source {
            metadata.set_property(
                0,
                DEFAULT_SOURCE_KEY,
                Some(JSON_TYPE),
                Some(&node_json(MIC_NODE_NAME)),
            );
        }
        self.applied.set(true);
        info!(
            "Switched default audio devices to the headset (speaker: {}, microphone: {})",
            self.switch_sink, self.switch_source
        );
    }
}

fn audio_info_to_vec(audio_info: AudioInfoRaw) -> Vec<u8> {
    PodSerializer::serialize(
        io::Cursor::new(Vec::new()),
        &Value::Object(pod::Object {
            type_: libspa_sys::SPA_TYPE_OBJECT_Format,
            id: libspa_sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .unwrap()
    .0
    .into_inner()
}

fn create_speaker_stream(
    pw_core: &CoreRc,
    mut sender: StreamSender<()>,
    sample_rate: u32,
    channel_count: u32,
) -> Result<(StreamRc, StreamListener<i16>), pipewire::Error> {
    let stream = StreamRc::new(
        pw_core.clone(),
        "alvr-audio",
        properties::properties! {
            *keys::NODE_NAME => SPEAKER_NODE_NAME,
            *keys::MEDIA_NAME => "alvr-audio",
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Capture",
            *keys::MEDIA_CLASS => "Audio/Sink",
            *keys::MEDIA_ROLE => "Game",
        },
    )?;

    let listener: StreamListener<i16> = stream
        .add_local_listener()
        .process(move |stream, _| {
            if let Some(mut pw_buf) = stream.dequeue_buffer()
                && let Some(pw_buf) = pw_buf.datas_mut().first_mut()
            {
                let size = pw_buf.chunk_mut().size() as usize;

                if let Some(data) = pw_buf.data() {
                    // Data is given as s16le in the correct layout by pipewire already,
                    // no need to do conversions
                    sender.send_header_with_payload(&(), &data[0..size]).ok();
                }
            }
        })
        .register()?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::S16LE);
    audio_info.set_rate(sample_rate);
    audio_info.set_channels(channel_count);

    stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut [Pod::from_bytes(&audio_info_to_vec(audio_info)).unwrap()],
    )?;

    Ok((stream, listener))
}

fn create_mic_stream(
    pw_core: &CoreRc,
    sample_queue: Arc<Mutex<VecDeque<f32>>>,
    sample_rate: u32,
    channel_count: u32,
) -> Result<(StreamRc, StreamListener<f32>), pipewire::Error> {
    let stream = StreamRc::new(
        pw_core.clone(),
        "alvr-mic",
        properties::properties! {
            *keys::NODE_NAME => MIC_NODE_NAME,
            *keys::MEDIA_NAME => "alvr-mic",
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Playback",
            *keys::MEDIA_CLASS => "Audio/Source",
            *keys::MEDIA_ROLE => "Communication",
        },
    )?;

    let chan_size = std::mem::size_of::<f32>();
    let listener: StreamListener<f32> = stream
        .add_local_listener()
        .state_changed(move |_, _, _, new_state| {
            MIC_STREAMING.store(new_state == StreamState::Streaming, Ordering::Relaxed);
        })
        .process(move |stream, _| {
            fill_pw_buf(
                sample_queue.clone(),
                chan_size,
                channel_count as usize,
                stream,
            );
        })
        .register()?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(sample_rate);
    audio_info.set_channels(channel_count);

    stream.connect(
        Direction::Output,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut [Pod::from_bytes(&audio_info_to_vec(audio_info)).unwrap()],
    )?;

    Ok((stream, listener))
}

fn fill_pw_buf(
    sample_queue: Arc<Mutex<VecDeque<f32>>>,
    chan_size: usize,
    chan_count: usize,
    stream: &pipewire::stream::Stream,
) {
    if let Some(mut pw_buf) = stream.dequeue_buffer() {
        let requested = pw_buf.requested() as usize;

        if let Some(pw_data) = pw_buf.datas_mut().first_mut()
            && let Some(slice) = pw_data.data()
            && let Some(mut samples) = sample_queue.try_lock()
        {
            // TODO: Would it be more correct to try to split it up over multiple datas?
            // Or is the requested size already right for the first data chunk?

            let mut it = slice
                .chunks_exact_mut(chan_size)
                .take(requested * chan_count);
            let pw_sample_count = it.len();

            let (front, back) = samples.as_slices();
            let copy_sample =
                |(chunk, sample): (&mut [u8], &f32)| chunk.copy_from_slice(&sample.to_le_bytes());

            // Split up so the compiler actually optimizes this properly
            it.by_ref().zip(front).for_each(copy_sample);
            it.zip(back).for_each(copy_sample);

            let sample_count = pw_sample_count.min(samples.len());
            drop(samples.drain(..sample_count));

            let chunk = pw_data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = (chan_size * chan_count) as _;
            *chunk.size_mut() = (sample_count * chan_size) as _;
        }
    }
}
