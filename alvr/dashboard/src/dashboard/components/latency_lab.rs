// KiraVR: Latency Lab. Everything for the latency tests in one place (see
// docs/latency-investigation.md): Live latency breakdown, the settings that
// move it, the test plan as steps with Apply and Measure, and the results as
// a table to copy. The user's own values are remembered before the first
// change and can be restored with one click.

use crate::dashboard::ServerRequest;
use alvr_events::GraphStatistics;
use alvr_gui_common::theme;
use alvr_packets::{PathValuePair, parse_path};
use alvr_session::{CodecType, Settings};
use eframe::egui::{Button, Frame, Grid, ProgressBar, RichText, ScrollArea, Slider, Ui};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const LIVE_WINDOW: Duration = Duration::from_secs(5);
const MEASURE_TIME: Duration = Duration::from_secs(60);
const NO_DATA_TIMEOUT: Duration = Duration::from_secs(5);
const STATE_FILE: &str = "kiravr_latency_lab.json";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
struct Knobs {
    max_buffering: f32,
    history_weight: f32,
    fps: f32,
    codec: CodecType,
    // Encoding width per eye when the resolution is set as Absolute. None in
    // Scale mode. Saved results from before this field read as None.
    #[serde(default)]
    width: Option<u32>,
}

// Encoding widths per eye to try, multiples of 32 as the setting requires.
const WIDTHS: [(u32, &str); 4] = [
    (2144, "100 %"),
    (1824, "85 %"),
    (1600, "75 %"),
    (1280, "60 %"),
];

impl Knobs {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            max_buffering: settings.video.max_buffering_frames,
            history_weight: settings.video.buffering_history_weight,
            fps: settings.video.preferred_fps,
            codec: settings.video.preferred_codec,
            width: match settings.video.transcoding_view_resolution {
                alvr_session::FrameSize::Absolute { width, .. } => Some(width),
                _ => None,
            },
        }
    }

    fn describe(&self) -> String {
        let width = self.width.map(|w| format!(", {w} px")).unwrap_or_default();
        format!(
            "{} Hz, {}{width}, buffering {:.1} (weight {:.2})",
            self.fps,
            codec_name(self.codec),
            self.max_buffering,
            self.history_weight
        )
    }
}

fn codec_name(codec: CodecType) -> &'static str {
    match codec {
        CodecType::H264 => "H.264",
        CodecType::Hevc => "HEVC",
        CodecType::AV1 => "AV1",
    }
}

fn codec_value(codec: CodecType) -> &'static str {
    match codec {
        CodecType::H264 => "H264",
        CodecType::Hevc => "Hevc",
        CodecType::AV1 => "AV1",
    }
}

// One step of the test plan: What it changes, starting from the user's own
// settings, and when that applies.
struct Step {
    name: &'static str,
    expect: &'static str,
    change: fn(Knobs) -> Knobs,
    needs_restart: bool,
}

const STEPS: &[Step] = &[
    Step {
        name: "Baseline",
        expect: "Your own settings, the reference for everything else",
        change: |k| k,
        needs_restart: false,
    },
    Step {
        name: "Buffering 1.0",
        expect: "Decode queue 7 to 14 ms lower, watch for stutter",
        change: |k| Knobs {
            max_buffering: 1.0,
            ..k
        },
        needs_restart: true,
    },
    Step {
        name: "Buffering 1.5",
        expect: "Between baseline and 1.0",
        change: |k| Knobs {
            max_buffering: 1.5,
            ..k
        },
        needs_restart: true,
    },
    Step {
        name: "90 Hz",
        expect: "Frame-based parts about 20 % lower",
        change: |k| Knobs { fps: 90.0, ..k },
        needs_restart: true,
    },
    Step {
        name: "120 Hz",
        expect: "About 40 % lower, if the GPU and encoder keep up",
        change: |k| Knobs { fps: 120.0, ..k },
        needs_restart: true,
    },
    Step {
        name: "90 Hz, 85 % resolution",
        expect: "If more new frames arrive than at full resolution, the encoder is the limit",
        change: |k| Knobs {
            fps: 90.0,
            width: Some(1824),
            ..k
        },
        needs_restart: true,
    },
    Step {
        name: "90 Hz, 75 % resolution",
        expect: "Same question with more headroom for the encoder",
        change: |k| Knobs {
            fps: 90.0,
            width: Some(1600),
            ..k
        },
        needs_restart: true,
    },
    Step {
        name: "H.264",
        expect: "Compare decode time between codecs",
        change: |k| Knobs {
            codec: CodecType::H264,
            ..k
        },
        needs_restart: true,
    },
    Step {
        name: "HEVC",
        expect: "Compare decode time between codecs",
        change: |k| Knobs {
            codec: CodecType::Hevc,
            ..k
        },
        needs_restart: true,
    },
    Step {
        name: "AV1",
        expect: "Compare decode time between codecs",
        change: |k| Knobs {
            codec: CodecType::AV1,
            ..k
        },
        needs_restart: true,
    },
];

#[derive(Serialize, Deserialize, Clone, Debug)]
struct TestResult {
    label: String,
    when: String,
    knobs: Knobs,
    samples: usize,
    total_ms: f32,
    game_ms: f32,
    compose_ms: f32,
    encode_ms: f32,
    network_ms: f32,
    decode_ms: f32,
    queue_ms: f32,
    render_ms: f32,
    vsync_ms: f32,
    fps_avg: f32,
    // The 5th percentile: What the worst moments looked like.
    fps_low: f32,
    // Share of frames below 90 % of the refresh rate, a stutter measure.
    slow_percent: f32,
    // Frames SteamVR handed to the streamer per second.
    #[serde(default)]
    server_fps: f32,
    // New stream frames the headset showed per second, counted.
    #[serde(default)]
    new_fps: f32,
    // Share of the headset's display refreshes that showed an old frame again.
    #[serde(default)]
    repeated_percent: f32,
    // Frames the streamer dropped before encoding, per second.
    #[serde(default)]
    dropped_fps: f32,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedState {
    original: Option<Knobs>,
    results: Vec<TestResult>,
}

struct Measurement {
    label: String,
    knobs: Knobs,
    started: Instant,
    last_sample: Instant,
    samples: Vec<GraphStatistics>,
}

pub struct LatencyLab {
    current: Option<Knobs>,
    log_to_disk: Option<bool>,
    saved: SavedState,
    live: VecDeque<(Instant, GraphStatistics)>,
    measuring: Option<Measurement>,
    status: Option<(bool, String)>,
    restart_pending: bool,
    confirm_clear: bool,
    // Slider values while they are being dragged, sent on release.
    draft: Option<Knobs>,
    // A game that was running when Restart SteamVR was clicked.
    confirm_restart: Option<String>,
}

fn state_path() -> std::path::PathBuf {
    crate::get_filesystem_layout().config_dir.join(STATE_FILE)
}

impl LatencyLab {
    pub fn new() -> Self {
        let saved = std::fs::read_to_string(state_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            current: None,
            log_to_disk: None,
            saved,
            live: VecDeque::new(),
            measuring: None,
            status: None,
            restart_pending: false,
            confirm_clear: false,
            draft: None,
            confirm_restart: None,
        }
    }

    pub fn update_settings(&mut self, settings: &Settings) {
        self.current = Some(Knobs::from_settings(settings));
        self.log_to_disk = Some(settings.extra.logging.log_to_disk);
    }

    pub fn add_sample(&mut self, statistics: &GraphStatistics) {
        let now = Instant::now();
        self.live.push_back((now, statistics.clone()));
        while self
            .live
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > LIVE_WINDOW)
        {
            self.live.pop_front();
        }
        if let Some(measurement) = &mut self.measuring {
            measurement.samples.push(statistics.clone());
            measurement.last_sample = now;
        }
    }

    fn save(&self) {
        if let Ok(text) = serde_json::to_string_pretty(&self.saved) {
            std::fs::write(state_path(), text).ok();
        }
    }

    // Writes the knobs that differ from the current values. The first change
    // remembers the user's own settings.
    fn apply(&mut self, target: Knobs, requests: &mut Vec<ServerRequest>) {
        let Some(current) = self.current else {
            return;
        };
        if self.saved.original.is_none() {
            self.saved.original = Some(current);
            self.save();
        }
        let mut values = vec![];
        let mut set = |path: &str, value: serde_json::Value| {
            values.push(PathValuePair {
                path: parse_path(&format!("session_settings.video.{path}")).unwrap(),
                value,
            });
        };
        if target.max_buffering != current.max_buffering {
            set("max_buffering_frames", target.max_buffering.into());
        }
        if target.history_weight != current.history_weight {
            set("buffering_history_weight", target.history_weight.into());
        }
        if target.fps != current.fps {
            set("preferred_fps", target.fps.into());
        }
        if target.codec != current.codec {
            set("preferred_codec.variant", codec_value(target.codec).into());
        }
        if target.width != current.width
            && let Some(width) = target.width
        {
            set("transcoding_view_resolution.variant", "Absolute".into());
            set("transcoding_view_resolution.Absolute.width", width.into());
        }
        if !values.is_empty() {
            requests.push(ServerRequest::SetSessionValues(values));
            self.current = Some(target);
            self.restart_pending = true;
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<ServerRequest> {
        let mut requests = vec![];
        self.finish_measurement();
        ui.ctx()
            .request_repaint_after(Duration::from_millis(if self.measuring.is_some() {
                200
            } else {
                1000
            }));

        ScrollArea::vertical()
            .id_salt("latency_lab_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(RichText::new("Latency Lab").size(20.0));
                ui.label(
                    "Change one thing, restart SteamVR so it applies, reconnect, then measure 60 seconds \
                     in the same spot with your head still. Your own settings are remembered and can be \
                     restored at any time.",
                );
                ui.add_space(6.0);

                ui.columns(2, |columns| {
                    self.live_ui(&mut columns[0]);
                    self.knobs_ui(&mut columns[1], &mut requests);
                });
                ui.add_space(6.0);
                self.steps_ui(ui, &mut requests);
                ui.add_space(6.0);
                self.results_ui(ui);
            });
        requests
    }

    fn live_ui(&self, ui: &mut Ui) {
        section(ui, "Live (Last 5 Seconds)", |ui| {
            let Some(mut avg) = average(self.live.iter().map(|(_, s)| s)) else {
                ui.label(
                    RichText::new("Not streaming. Numbers appear once the headset is connected.")
                        .weak(),
                );
                return;
            };
            if let (Some((first, _)), Some((last, _))) = (self.live.front(), self.live.back()) {
                let samples = self.live.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>();
                let refresh = self.current.map(|k| k.fps).unwrap_or(72.0);
                (avg.new_fps, avg.repeated_percent) =
                    frame_rates(&samples, refresh, last.duration_since(*first).as_secs_f32());
            }
            Grid::new("latency_lab_live")
                .num_columns(2)
                .spacing([16.0, 3.0])
                .show(ui, |ui| {
                    let mut row = |label: &str, value: String, strong: bool| {
                        ui.label(label);
                        let text = RichText::new(value).monospace();
                        ui.label(if strong { text.strong() } else { text });
                        ui.end_row();
                    };
                    row("Total", format!("{:.1} ms", avg.total_ms), true);
                    row(
                        "Game / Compose",
                        format!("{:.1} / {:.1} ms", avg.game_ms, avg.compose_ms),
                        false,
                    );
                    row(
                        "Encode / Network",
                        format!("{:.1} / {:.1} ms", avg.encode_ms, avg.network_ms),
                        false,
                    );
                    row("Decode", format!("{:.1} ms", avg.decode_ms), false);
                    row("Decode queue", format!("{:.1} ms", avg.queue_ms), true);
                    row(
                        "Render / Vsync queue",
                        format!("{:.1} / {:.1} ms", avg.render_ms, avg.vsync_ms),
                        false,
                    );
                    row("Streamer fps", format!("{:.0}", avg.server_fps), false);
                    row(
                        "New frames on headset",
                        format!(
                            "{:.0} /s ({:.0} % repeated)",
                            avg.new_fps, avg.repeated_percent
                        ),
                        true,
                    );
                    row(
                        "Dropped by streamer",
                        format!("{:.1} /s", avg.dropped_fps),
                        avg.dropped_fps >= 0.5,
                    );
                });
        });
    }

    fn knobs_ui(&mut self, ui: &mut Ui, requests: &mut Vec<ServerRequest>) {
        section(ui, "Settings Under Test", |ui| {
            let Some(current) = self.current else {
                ui.label(RichText::new("Waiting for the settings...").weak());
                return;
            };
            let mut target = self.draft.unwrap_or(current);

            ui.label("Maximum buffering (frames)");
            let response = ui.add(Slider::new(&mut target.max_buffering, 1.0..=4.0).step_by(0.1));
            let buffering_done =
                response.drag_stopped() || (response.changed() && !response.dragged());
            ui.label("Buffering history weight");
            let response =
                ui.add(Slider::new(&mut target.history_weight, 0.5..=0.99).step_by(0.01));
            let weight_done =
                response.drag_stopped() || (response.changed() && !response.dragged());

            ui.label("Refresh rate");
            let mut picked = false;
            ui.horizontal(|ui| {
                for fps in [72.0, 80.0, 90.0, 120.0] {
                    if ui
                        .selectable_label(target.fps == fps, format!("{fps} Hz"))
                        .clicked()
                    {
                        target.fps = fps;
                        picked = true;
                    }
                }
            });
            ui.label("Resolution (encoded width per eye)");
            if target.width.is_some() {
                ui.horizontal_wrapped(|ui| {
                    for (width, share) in WIDTHS {
                        if ui
                            .selectable_label(
                                target.width == Some(width),
                                format!("{share} ({width})"),
                            )
                            .clicked()
                        {
                            target.width = Some(width);
                            picked = true;
                        }
                    }
                });
            } else {
                ui.label(
                    RichText::new(
                        "Set as a scale in Settings, Video. Switch it to Absolute to test it here.",
                    )
                    .weak(),
                );
            }
            ui.label("Codec");
            ui.horizontal(|ui| {
                for codec in [CodecType::H264, CodecType::Hevc, CodecType::AV1] {
                    if ui
                        .selectable_label(target.codec == codec, codec_name(codec))
                        .clicked()
                    {
                        target.codec = codec;
                        picked = true;
                    }
                }
            });
            if buffering_done || weight_done || picked {
                self.draft = None;
                self.apply(target, requests);
            } else if target != current {
                self.draft = Some(target);
            } else {
                self.draft = None;
            }

            if let Some(mut enabled) = self.log_to_disk
                && ui
                    .checkbox(
                        &mut enabled,
                        "Log to Disk (keeps the numbers in the session log)",
                    )
                    .changed()
            {
                self.log_to_disk = Some(enabled);
                requests.push(ServerRequest::SetSessionValues(vec![PathValuePair {
                    path: parse_path("session_settings.extra.logging.log_to_disk").unwrap(),
                    value: enabled.into(),
                }]));
            }

            ui.add_space(4.0);
            if self.restart_pending {
                ui.label(
                    RichText::new(
                        "Changed: Restart SteamVR so it applies, then reconnect the headset.",
                    )
                    .color(theme::log_colors::WARNING_LIGHT),
                );
            }
            if let Some(game) = self.confirm_restart.clone() {
                ui.label(
                    RichText::new(format!(
                        "{game} is running. Quit it first: Restarting SteamVR under a game kills it, \
                         and Steam can then hang on its cloud sync."
                    ))
                    .color(theme::log_colors::WARNING_LIGHT),
                );
                ui.horizontal(|ui| {
                    if ui.button("Check Again").clicked() {
                        match running_game() {
                            Some(game) => self.confirm_restart = Some(game),
                            None => {
                                self.confirm_restart = None;
                                self.restart_steamvr(requests);
                            }
                        }
                    }
                    if ui.button("Restart Anyway").clicked() {
                        self.confirm_restart = None;
                        self.restart_steamvr(requests);
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_restart = None;
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.confirm_restart.is_none(),
                        Button::new("Restart SteamVR"),
                    )
                    .clicked()
                {
                    // Restarting under a running game kills it, and Steam
                    // then hangs on that game's cloud sync.
                    match running_game() {
                        Some(game) => self.confirm_restart = Some(game),
                        None => self.restart_steamvr(requests),
                    }
                }
                if let Some(original) = self.saved.original
                    && ui
                        .button("Restore My Settings")
                        .on_hover_text(original.describe())
                        .clicked()
                {
                    self.apply(original, requests);
                    self.saved.original = None;
                    self.save();
                    self.status = Some((
                        true,
                        format!(
                            "Restored: {}. Restart SteamVR to apply.",
                            original.describe()
                        ),
                    ));
                }
            });
        });
    }

    fn restart_steamvr(&mut self, requests: &mut Vec<ServerRequest>) {
        requests.push(ServerRequest::RestartSteamvr);
        self.restart_pending = false;
        self.live.clear();
    }

    fn steps_ui(&mut self, ui: &mut Ui, requests: &mut Vec<ServerRequest>) {
        let base = self.saved.original.or(self.current);
        section(ui, "Test Plan", |ui| {
            if let Some(measurement) = &self.measuring {
                let elapsed = measurement.started.elapsed().min(MEASURE_TIME);
                ui.add(
                    ProgressBar::new(elapsed.as_secs_f32() / MEASURE_TIME.as_secs_f32()).text(
                        format!(
                            "Measuring \"{}\": {} of {} s, keep your head still",
                            measurement.label,
                            elapsed.as_secs(),
                            MEASURE_TIME.as_secs()
                        ),
                    ),
                );
                if ui.button("Stop Measuring").clicked() {
                    self.measuring = None;
                    self.status = Some((false, "Measurement stopped, nothing saved.".into()));
                }
                ui.add_space(4.0);
            }
            let busy = self.measuring.is_some();
            Grid::new("latency_lab_steps").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                for (index, step) in STEPS.iter().enumerate() {
                    let target = base.map(step.change);
                    let active = target.is_some() && target == self.current;
                    let mut name = RichText::new(format!("{index}  {}", step.name));
                    if active {
                        name = name.strong().color(theme::OK_GREEN);
                    }
                    ui.label(name);
                    if ui
                        .add_enabled(!busy && target.is_some() && !active, Button::new("Apply"))
                        .on_hover_text(target.map(|t| t.describe()).unwrap_or_default())
                        .clicked()
                        && let Some(target) = target
                    {
                        self.apply(target, requests);
                        if !step.needs_restart && target == base.unwrap_or(target) {
                            self.status = Some((true, "Back to your own settings. Restart SteamVR if anything changed.".into()));
                        }
                    }
                    if ui
                        .add_enabled(!busy && active, Button::new("Measure 60 s"))
                        .on_hover_text("Apply this step and restart SteamVR first")
                        .clicked()
                    {
                        self.start_measurement(step.name);
                    }
                    let done = self.saved.results.iter().rev().find(|r| r.label == step.name);
                    match done {
                        Some(result) => ui.label(
                            RichText::new(format!(
                                "{:.1} ms total, queue {:.1} ms, low {:.0} fps",
                                result.total_ms, result.queue_ms, result.fps_low
                            ))
                            .monospace(),
                        ),
                        None => ui.label(RichText::new(step.expect).weak()),
                    };
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            if ui
                .add_enabled(
                    !busy && self.current.is_some(),
                    Button::new("Measure Current Settings"),
                )
                .on_hover_text("For anything else you want to try")
                .clicked()
            {
                self.start_measurement("Custom");
            }
            if let Some((ok, message)) = &self.status {
                let color = if *ok {
                    theme::OK_GREEN
                } else {
                    theme::log_colors::WARNING_LIGHT
                };
                ui.label(RichText::new(message).color(color));
            }
        });
    }

    fn results_ui(&mut self, ui: &mut Ui) {
        section(ui, "Results", |ui| {
            if self.saved.results.is_empty() {
                ui.label(RichText::new("No measurements yet.").weak());
                return;
            }
            ScrollArea::horizontal()
                .id_salt("latency_lab_results")
                .show(ui, |ui| {
                    Grid::new("latency_lab_results_grid")
                        .striped(true)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            for title in [
                                "Test",
                                "Settings",
                                "Total",
                                "Encode",
                                "Network",
                                "Decode",
                                "Queue",
                                "Vsync",
                                "Streamer",
                                "New/s",
                                "Repeated",
                                "Dropped/s",
                                "Slow",
                            ] {
                                ui.label(RichText::new(title).strong());
                            }
                            ui.end_row();
                            for r in &self.saved.results {
                                ui.label(&r.label);
                                ui.label(r.knobs.describe());
                                for value in [
                                    r.total_ms,
                                    r.encode_ms,
                                    r.network_ms,
                                    r.decode_ms,
                                    r.queue_ms,
                                    r.vsync_ms,
                                ] {
                                    ui.label(RichText::new(format!("{value:.1}")).monospace());
                                }
                                for (value, digits) in [(r.server_fps, 0), (r.new_fps, 0)] {
                                    ui.label(
                                        RichText::new(format!("{value:.digits$}")).monospace(),
                                    );
                                }
                                ui.label(
                                    RichText::new(format!("{:.0}%", r.repeated_percent))
                                        .monospace(),
                                );
                                ui.label(
                                    RichText::new(format!("{:.1}", r.dropped_fps)).monospace(),
                                );
                                ui.label(
                                    RichText::new(format!("{:.1}%", r.slow_percent)).monospace(),
                                );
                                ui.end_row();
                            }
                        });
                });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Copy Results").clicked() {
                    ui.ctx().copy_text(results_markdown(&self.saved.results));
                    self.status = Some((true, "Copied as a Markdown table.".into()));
                }
                if self.confirm_clear {
                    ui.label("Delete all results?");
                    if ui.button("Delete").clicked() {
                        self.saved.results.clear();
                        self.save();
                        self.confirm_clear = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_clear = false;
                    }
                } else if ui.button("Clear Results").clicked() {
                    self.confirm_clear = true;
                }
            });
        });
    }

    fn start_measurement(&mut self, label: &str) {
        let Some(knobs) = self.current else {
            return;
        };
        let now = Instant::now();
        self.measuring = Some(Measurement {
            label: label.to_owned(),
            knobs,
            started: now,
            last_sample: now,
            samples: vec![],
        });
        self.status = None;
    }

    fn finish_measurement(&mut self) {
        let Some(measurement) = &self.measuring else {
            return;
        };
        if measurement.last_sample.elapsed() > NO_DATA_TIMEOUT {
            self.measuring = None;
            self.status = Some((
                false,
                "No numbers arriving. Is the headset connected and streaming? Nothing was saved."
                    .into(),
            ));
            return;
        }
        if measurement.started.elapsed() < MEASURE_TIME {
            return;
        }
        let measurement = self.measuring.take().unwrap();
        let Some(avg) = average(measurement.samples.iter()) else {
            return;
        };
        let refresh = measurement.knobs.fps;
        let (new_fps, repeated_percent) = frame_rates(
            &measurement.samples,
            refresh,
            measurement.started.elapsed().as_secs_f32(),
        );
        let slow = measurement
            .samples
            .iter()
            .filter(|s| s.client_fps < refresh * 0.9)
            .count();
        self.saved.results.push(TestResult {
            label: measurement.label.clone(),
            when: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            knobs: measurement.knobs,
            samples: measurement.samples.len(),
            slow_percent: 100.0 * slow as f32 / measurement.samples.len() as f32,
            new_fps,
            repeated_percent,
            ..avg
        });
        self.save();
        self.status = Some((true, format!("Saved \"{}\".", measurement.label)));
    }
}

// Shared with the sidebar's Restart SteamVR.
fn running_game() -> Option<String> {
    crate::steamvr_launcher::running_game()
}

fn section(ui: &mut Ui, title: &str, content: impl FnOnce(&mut Ui)) {
    Frame::group(ui.style())
        .fill(theme::SECTION_BG)
        .inner_margin(theme::FRAME_PADDING)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(16.0).strong());
            ui.add_space(4.0);
            content(ui);
        });
}

// Averages in milliseconds, with the 5th percentile of the fps.
fn average<'a>(samples: impl Iterator<Item = &'a GraphStatistics>) -> Option<TestResult> {
    let samples = samples.collect::<Vec<_>>();
    if samples.is_empty() {
        return None;
    }
    let n = samples.len() as f32;
    let ms =
        |f: fn(&GraphStatistics) -> f32| samples.iter().map(|s| f(s)).sum::<f32>() / n * 1000.0;
    let mut fps = samples.iter().map(|s| s.client_fps).collect::<Vec<_>>();
    fps.sort_by(f32::total_cmp);
    Some(TestResult {
        label: String::new(),
        when: String::new(),
        knobs: Knobs {
            max_buffering: 0.0,
            history_weight: 0.0,
            fps: 0.0,
            codec: CodecType::H264,
            width: None,
        },
        samples: samples.len(),
        total_ms: ms(|s| s.total_pipeline_latency_s),
        game_ms: ms(|s| s.game_time_s),
        compose_ms: ms(|s| s.server_compositor_s),
        encode_ms: ms(|s| s.encoder_s),
        network_ms: ms(|s| s.network_s),
        decode_ms: ms(|s| s.decoder_s),
        queue_ms: ms(|s| s.decoder_queue_s),
        render_ms: ms(|s| s.client_compositor_s),
        vsync_ms: ms(|s| s.vsync_queue_s),
        fps_avg: fps.iter().sum::<f32>() / n,
        fps_low: fps[(fps.len() as f32 * 0.05) as usize],
        slow_percent: 0.0,
        server_fps: samples.iter().map(|s| s.server_fps).sum::<f32>() / n,
        new_fps: 0.0,
        repeated_percent: 0.0,
        dropped_fps: samples.iter().map(|s| s.streamer_dropped_fps).sum::<f32>() / n,
    })
}

// New frames per second, and the share of display refreshes that repeated an
// old frame. Each sample is one new frame; its interval says for how many
// refreshes it stayed on screen.
fn frame_rates(samples: &[GraphStatistics], refresh: f32, seconds: f32) -> (f32, f32) {
    if samples.is_empty() || seconds <= 0.0 || refresh <= 0.0 {
        return (0.0, 0.0);
    }
    let slots = samples
        .iter()
        .map(|s| (refresh / s.client_fps.max(1.0)).round().max(1.0))
        .sum::<f32>();
    let new = samples.len() as f32;
    (new / seconds, 100.0 * (slots - new) / slots)
}

fn results_markdown(results: &[TestResult]) -> String {
    let mut text = String::from(
        "| Test | Settings | Total | Game | Compose | Encode | Network | Decode | Queue | Render | Vsync | Streamer fps | New frames/s | Repeated | Dropped/s | FPS | Low | Slow | When |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for r in results {
        text += &format!(
            "| {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.0} | {:.0} | {:.0}% | {:.1} | {:.0} | {:.0} | {:.1}% | {} |\n",
            r.label,
            r.knobs.describe(),
            r.total_ms,
            r.game_ms,
            r.compose_ms,
            r.encode_ms,
            r.network_ms,
            r.decode_ms,
            r.queue_ms,
            r.render_ms,
            r.vsync_ms,
            r.server_fps,
            r.new_fps,
            r.repeated_percent,
            r.dropped_fps,
            r.fps_avg,
            r.fps_low,
            r.slow_percent,
            r.when
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(total_s: f32, queue_s: f32, fps: f32) -> GraphStatistics {
        GraphStatistics {
            total_pipeline_latency_s: total_s,
            decoder_queue_s: queue_s,
            client_fps: fps,
            ..Default::default()
        }
    }

    #[test]
    fn averages_in_milliseconds_with_a_low_fps() {
        let mut samples = vec![stats(0.050, 0.010, 72.0); 95];
        samples.extend(vec![stats(0.060, 0.020, 40.0); 5]);
        let avg = average(samples.iter()).unwrap();
        assert!((avg.total_ms - 50.5).abs() < 0.01, "{}", avg.total_ms);
        assert!((avg.queue_ms - 10.5).abs() < 0.01);
        assert_eq!(avg.fps_low, 72.0, "5 % of 100 samples is the boundary");
        assert!(average(std::iter::empty()).is_none());
    }

    #[test]
    fn steps_change_one_thing() {
        let base = Knobs {
            max_buffering: 2.0,
            history_weight: 0.9,
            fps: 72.0,
            codec: CodecType::Hevc,
            width: Some(2144),
        };
        assert_eq!((STEPS[0].change)(base), base);
        for step in &STEPS[1..] {
            let changed = (step.change)(base);
            let differences = [
                changed.max_buffering != base.max_buffering,
                changed.fps != base.fps,
                changed.codec != base.codec,
                changed.width != base.width,
            ]
            .iter()
            .filter(|d| **d)
            .count();
            // The resolution steps pair 90 Hz with a lower resolution on purpose.
            let allowed = if step.name.contains("resolution") {
                2
            } else {
                1
            };
            assert!(
                differences <= allowed,
                "{} changes more than it should",
                step.name
            );
        }
    }

    #[test]
    fn counts_new_and_repeated_frames() {
        // 90 Hz display, 72 new frames per second: 54 shown once (90 fps
        // interval) and 18 shown twice (45 fps interval) fill 90 refreshes.
        let mut samples = vec![stats(0.05, 0.0, 90.0); 54];
        samples.extend(vec![stats(0.05, 0.0, 45.0); 18]);
        let (new_fps, repeated) = frame_rates(&samples, 90.0, 1.0);
        assert_eq!(new_fps, 72.0);
        assert!((repeated - 20.0).abs() < 0.01, "{repeated}");
        assert_eq!(frame_rates(&[], 90.0, 1.0), (0.0, 0.0));
    }

    #[test]
    fn markdown_has_a_row_per_result() {
        let mut result = average([stats(0.05, 0.01, 72.0)].iter()).unwrap();
        result.label = "Baseline".into();
        let text = results_markdown(&[result.clone(), result]);
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains("| Baseline |"));
    }
}
