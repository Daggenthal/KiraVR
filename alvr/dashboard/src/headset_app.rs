// KiraVR: "Install on Headset". Puts the headset app that ships with this
// streamer on a Quest connected over USB, so nobody needs adb or a terminal.
// Checks and installs run on background threads, the UI only reads the state.

use alvr_common::{ALVR_VERSION, is_stable, parking_lot::Mutex};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const CHECK_INTERVAL: Duration = Duration::from_secs(3);

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

#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Idle,
    Working {
        message: String,
        progress: Option<f32>,
    },
    Done(String),
    Failed(String),
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
                message: "Starting...".into(),
                progress: None,
            };
        }
        let this = self.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let set = |job: Job| {
                this.shared.lock().job = job;
                ctx.request_repaint();
            };
            let result = install(&|message: &str, progress: Option<f32>| {
                set(Job::Working {
                    message: message.to_owned(),
                    progress,
                })
            });
            set(match result {
                Ok(message) => Job::Done(message),
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

fn install(report: &dyn Fn(&str, Option<f32>)) -> alvr_common::anyhow::Result<String> {
    use alvr_common::anyhow::{Context, bail};

    let layout = crate::get_filesystem_layout();
    let apk = layout.client_apk();
    if !apk.exists() {
        bail!(
            "This build has no headset app bundled ({} is missing).",
            apk.display()
        );
    }

    report("Getting adb ready...", None);
    let adb = alvr_adb::commands::require_adb(&layout, |downloaded, total| {
        let progress = total.map(|t| downloaded as f32 / t as f32);
        report("Downloading adb...", progress);
    })?;

    report("Looking for the headset...", None);
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

    report("Installing the headset app...", None);
    let apk_path = apk.to_string_lossy();
    if let Err(e) = alvr_adb::commands::install_package(&adb, &serial, &apk_path) {
        // An app signed with another key cannot be updated in place. Only
        // then remove it first, since that also clears its saved data.
        if format!("{e}").contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
            report("Replacing an incompatible older version...", None);
            alvr_adb::commands::uninstall_package(&adb, &serial, application_id())?;
            alvr_adb::commands::install_package(&adb, &serial, &apk_path)?;
        } else {
            return Err(e);
        }
    }

    report("Starting the app on the headset...", None);
    alvr_adb::commands::start_application(&adb, &serial, application_id()).ok();

    Ok(format!(
        "KiraVR v{} is installed on the headset.",
        bundled_version()
    ))
}
