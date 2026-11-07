// KiraVR: "Install on Headset". Puts the headset app that ships with this
// streamer on a Quest connected over USB, so nobody needs adb or a terminal.
// Checks and installs run on background threads, the UI only reads the state.

use alvr_common::{ALVR_VERSION, is_stable, parking_lot::Mutex};
use std::{
    collections::VecDeque,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const CHECK_INTERVAL: Duration = Duration::from_secs(3);
// Speed is averaged over this much recent history, so it reads steady.
const RATE_WINDOW: Duration = Duration::from_millis(1000);
const STEPS: u8 = 4;

#[derive(Clone, Debug, PartialEq)]
pub enum HeadsetState {
    // adb is not there yet, it is downloaded on install.
    NoAdb,
    NotConnected,
    // Connected, but the PC is not allowed yet (prompt inside the headset).
    Unauthorized,
    Connected {
        model: String,
        installed_version: Option<String>,
    },
}

// Bytes moving somewhere, with a speed once there is enough history.
#[derive(Clone, Debug, PartialEq)]
pub struct Transfer {
    pub done: u64,
    pub total: Option<u64>,
    pub bytes_per_second: Option<f64>,
}

impl Transfer {
    pub fn fraction(&self) -> Option<f32> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.done as f64 / t as f64).clamp(0.0, 1.0) as f32)
    }

    // "12.8 MB of 28.5 MB", "21.4 MB/s", "About 2 s left"
    pub fn readout(&self) -> Vec<String> {
        let mut parts = vec![match self.total {
            Some(total) => format!("{} of {}", format_bytes(self.done), format_bytes(total)),
            None => format_bytes(self.done),
        }];
        if let Some(rate) = self.bytes_per_second {
            parts.push(format!("{}/s", format_bytes(rate as u64)));
            if let Some(total) = self.total
                && rate > 0.0
                && total > self.done
            {
                let seconds = ((total - self.done) as f64 / rate).ceil() as u64;
                parts.push(format!("About {} left", format_seconds(seconds)));
            }
        }
        parts
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Idle,
    Working {
        step: u8,
        steps: u8,
        title: String,
        transfer: Option<Transfer>,
        // What earlier steps did, e.g. how fast the app was sent.
        note: Option<String>,
    },
    Done {
        message: String,
        note: Option<String>,
    },
    Failed(String),
}

pub fn format_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 100.0 {
        format!("{mb:.0} MB")
    } else if mb >= 0.1 {
        format!("{mb:.1} MB")
    } else {
        format!("{} KB", bytes / 1000)
    }
}

fn format_seconds(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

// Speed over the last RATE_WINDOW.
#[derive(Default)]
struct RateMeter {
    samples: VecDeque<(Instant, u64)>,
}

impl RateMeter {
    fn add(&mut self, at: Instant, done: u64) -> Option<f64> {
        self.samples.push_back((at, done));
        while self.samples.len() > 2 && at.duration_since(self.samples[1].0) >= RATE_WINDOW {
            self.samples.pop_front();
        }
        let (first_at, first_done) = *self.samples.front()?;
        let span = at.duration_since(first_at).as_secs_f64();
        // Under 0.2 s the number jumps around too much to be useful.
        (span >= 0.2).then(|| done.saturating_sub(first_done) as f64 / span)
    }
}

struct Shared {
    state: Option<HeadsetState>,
    job: Job,
    checking: bool,
    last_check: Option<Instant>,
}

#[derive(Clone)]
pub struct HeadsetApp {
    shared: Arc<Mutex<Shared>>,
}

pub fn application_id() -> &'static str {
    if is_stable() {
        alvr_system_info::PACKAGE_NAME_GITHUB_STABLE
    } else {
        alvr_system_info::PACKAGE_NAME_GITHUB_DEV
    }
}

pub fn bundled_version() -> String {
    ALVR_VERSION.to_string()
}

impl Default for HeadsetApp {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                state: None,
                job: Job::Idle,
                checking: false,
                last_check: None,
            })),
        }
    }
}

impl HeadsetApp {
    pub fn bundled_apk_exists(&self) -> bool {
        crate::get_filesystem_layout().client_apk().exists()
    }

    pub fn state(&self) -> Option<HeadsetState> {
        self.shared.lock().state.clone()
    }

    pub fn job(&self) -> Job {
        self.shared.lock().job.clone()
    }

    // Call from the UI. Starts a background check every few seconds while
    // nothing else is running.
    pub fn poll(&self, ctx: &eframe::egui::Context) {
        let mut shared = self.shared.lock();
        let busy = shared.checking || matches!(shared.job, Job::Working { .. });
        if busy
            || shared
                .last_check
                .is_some_and(|t| t.elapsed() < CHECK_INTERVAL)
        {
            return;
        }
        shared.checking = true;
        drop(shared);

        let this = self.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let state = check();
            let mut shared = this.shared.lock();
            shared.state = Some(state);
            shared.checking = false;
            shared.last_check = Some(Instant::now());
            ctx.request_repaint();
        });
    }

    pub fn install(&self, ctx: &eframe::egui::Context) {
        {
            let mut shared = self.shared.lock();
            if matches!(shared.job, Job::Working { .. }) {
                return;
            }
            shared.job = Job::Working {
                step: 1,
                steps: STEPS,
                title: "Getting ready...".into(),
                transfer: None,
                note: None,
            };
        }
        let this = self.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let set = |job: Job| {
                this.shared.lock().job = job;
                ctx.request_repaint();
            };
            let result = install(&|step: u8,
                                   title: &str,
                                   transfer: Option<Transfer>,
                                   note: Option<String>| {
                set(Job::Working {
                    step,
                    steps: STEPS,
                    title: title.to_owned(),
                    transfer,
                    note,
                })
            });
            set(match result {
                Ok((message, note)) => Job::Done { message, note },
                Err(e) => Job::Failed(format!("{e:#}")),
            });
            // Show the new state right away.
            this.shared.lock().last_check = None;
        });
    }
}

fn check() -> HeadsetState {
    let layout = crate::get_filesystem_layout();
    let Some(adb) = alvr_adb::commands::get_adb_path(&layout) else {
        return HeadsetState::NoAdb;
    };
    let Ok(devices) = alvr_adb::commands::list_devices(&adb) else {
        return HeadsetState::NotConnected;
    };
    for device in devices {
        use alvr_adb::parse::ConnectionState as C;
        match (device.connection_state, device.serial) {
            (Some(C::Device), Some(serial)) => {
                let model = device
                    .model
                    .map(|m| m.replace('_', " "))
                    .unwrap_or_else(|| "Headset".into());
                let installed_version =
                    alvr_adb::commands::installed_version(&adb, &serial, application_id())
                        .ok()
                        .flatten();
                return HeadsetState::Connected {
                    model,
                    installed_version,
                };
            }
            (Some(C::Unauthorized | C::Authorizing), _) => return HeadsetState::Unauthorized,
            _ => (),
        }
    }
    HeadsetState::NotConnected
}

type Report<'a> = &'a dyn Fn(u8, &str, Option<Transfer>, Option<String>);

fn install(report: Report) -> alvr_common::anyhow::Result<(String, Option<String>)> {
    use alvr_common::anyhow::{Context, bail};

    let layout = crate::get_filesystem_layout();
    let apk = layout.client_apk();
    if !apk.exists() {
        bail!(
            "This build has no headset app bundled ({} is missing).",
            apk.display()
        );
    }

    report(1, "Getting adb ready...", None, None);
    let meter = std::cell::RefCell::new(RateMeter::default());
    let adb = alvr_adb::commands::require_adb(&layout, |downloaded, total| {
        let rate = meter.borrow_mut().add(Instant::now(), downloaded as u64);
        let transfer = Transfer {
            done: downloaded as u64,
            total: total.map(|t| t as u64),
            bytes_per_second: rate,
        };
        report(1, "Downloading adb from Google...", Some(transfer), None);
    })?;

    report(1, "Looking for the headset...", None, None);
    let serial = match check() {
        HeadsetState::Connected { .. } => alvr_adb::commands::list_devices(&adb)?
            .into_iter()
            .find(|d| {
                matches!(
                    d.connection_state,
                    Some(alvr_adb::parse::ConnectionState::Device)
                )
            })
            .and_then(|d| d.serial)
            .context("The headset disconnected")?,
        HeadsetState::Unauthorized => {
            bail!("Put on the headset and allow USB debugging, then try again.")
        }
        _ => bail!(
            "No headset found. Connect it with a USB cable and make sure developer mode is on."
        ),
    };

    let apk_path = apk.to_string_lossy();
    let summary = std::cell::RefCell::new(None::<String>);
    let send = || {
        let started = Instant::now();
        let mut meter = RateMeter::default();
        let mut sent_all = false;
        alvr_adb::commands::install_package_streamed(&adb, &serial, &apk_path, |sent, total| {
            let rate = meter.add(Instant::now(), sent);
            if sent < total {
                report(
                    2,
                    "Sending the app to the headset...",
                    Some(Transfer {
                        done: sent,
                        total: Some(total),
                        bytes_per_second: rate,
                    }),
                    None,
                );
            } else if !sent_all {
                sent_all = true;
                let seconds = started.elapsed().as_secs_f64().max(0.001);
                let note = format!(
                    "Sent {} in {seconds:.1} s ({}/s)",
                    format_bytes(total),
                    format_bytes((total as f64 / seconds) as u64)
                );
                *summary.borrow_mut() = Some(note.clone());
                // The headset now checks and installs it, which takes a few
                // seconds with nothing to count.
                report(3, "Installing on the headset...", None, Some(note));
            }
        })
    };

    let installed_at = Instant::now();
    if let Err(e) = send() {
        let text = format!("{e}");
        if text.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE")
            || text.contains("INSTALL_FAILED_VERSION_DOWNGRADE")
        {
            // Signed with another key, or an older version (the user
            // confirmed going back twice): Cannot replace in place. Only
            // then remove it first, since that also clears its saved data.
            report(
                3,
                "Replacing an incompatible older version...",
                None,
                summary.borrow().clone(),
            );
            alvr_adb::commands::uninstall_package(&adb, &serial, application_id())?;
            send()?;
        } else if text.contains("INSTALL_FAILED") {
            return Err(e);
        } else {
            // Streaming is not supported somewhere: Plain adb install.
            alvr_common::warn!("Streamed install failed ({text}), using adb install");
            report(3, "Installing on the headset...", None, None);
            alvr_adb::commands::install_package(&adb, &serial, &apk_path)?;
        }
    }
    let total_seconds = installed_at.elapsed().as_secs_f64();

    report(
        4,
        "Starting the app on the headset...",
        None,
        summary.borrow().clone(),
    );
    alvr_adb::commands::start_application(&adb, &serial, application_id()).ok();

    let note = match summary.into_inner() {
        Some(sent) => format!("{sent}, installed in {total_seconds:.1} s total."),
        None => format!("Installed in {total_seconds:.1} s."),
    };
    Ok((
        format!("KiraVR v{} is installed on the headset.", bundled_version()),
        Some(note),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readout_is_short_and_clear() {
        let transfer = Transfer {
            done: 12_800_000,
            total: Some(28_500_000),
            bytes_per_second: Some(21_400_000.0),
        };
        assert_eq!(
            transfer.readout(),
            ["12.8 MB of 28.5 MB", "21.4 MB/s", "About 1 s left"]
        );
        assert!((transfer.fraction().unwrap() - 0.449).abs() < 0.001);

        let unknown = Transfer {
            done: 50_000,
            total: None,
            bytes_per_second: None,
        };
        assert_eq!(unknown.readout(), ["50 KB"]);
        assert_eq!(unknown.fraction(), None);
        assert_eq!(format_bytes(250_000_000), "250 MB");
        assert_eq!(format_seconds(75), "1 min 15 s");
    }

    #[test]
    fn speed_is_averaged_over_the_last_second() {
        let mut meter = RateMeter::default();
        let t0 = Instant::now();
        assert_eq!(meter.add(t0, 0), None);
        assert_eq!(
            meter.add(t0 + Duration::from_millis(100), 1_000_000),
            None,
            "too early"
        );
        // 10 MB/s for 3 s, then 20 MB/s: The reading follows the recent speed.
        for i in 2..=30u64 {
            meter.add(t0 + Duration::from_millis(100 * i), 1_000_000 * i);
        }
        let mut last = None;
        for i in 1..=15u64 {
            last = meter.add(
                t0 + Duration::from_millis(3000 + 100 * i),
                30_000_000 + 2_000_000 * i,
            );
        }
        let rate = last.unwrap();
        assert!((rate - 20_000_000.0).abs() < 1_500_000.0, "{rate}");
    }
}
