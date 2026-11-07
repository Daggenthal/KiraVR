#[cfg(target_os = "linux")]
mod linux_lifecycle;
#[cfg(target_os = "linux")]
mod linux_steamvr;
#[cfg(windows)]
mod windows_steamvr;

use crate::data_sources;
use alvr_adb::commands as adb;
use alvr_common::{
    anyhow::{Context, Result},
    debug, error,
    glam::bool,
    parking_lot::Mutex,
    warn,
};
use alvr_filesystem::{self as afs};
use serde_json::{self, json};
use std::{
    ffi::OsStr,
    fs,
    marker::PhantomData,
    process::Command,
    thread,
    time::{Duration, Instant},
};
#[cfg(not(target_os = "linux"))]
use sysinfo::ProcessesToUpdate;
use sysinfo::System;

// Windows only: Linux waits for every part of SteamVR (linux_lifecycle).
#[cfg(not(target_os = "linux"))]
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

// KiraVR: Set while a launch or restart is running in the background, so a
// second click does not start SteamVR twice.
static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// What the launcher is doing right now, shown under the SteamVR status.
static STATUS: Mutex<Option<String>> = Mutex::new(None);

pub fn status() -> Option<String> {
    STATUS.lock().clone()
}

pub fn is_busy() -> bool {
    BUSY.load(std::sync::atomic::Ordering::Relaxed)
}

fn set_status(status: Option<&str>) {
    *STATUS.lock() = status.map(str::to_owned);
}

// The name of a running Steam game, if any (Linux only for now).
pub fn running_game() -> Option<String> {
    #[cfg(target_os = "linux")]
    return linux_lifecycle::running_game();
    #[cfg(not(target_os = "linux"))]
    None
}

// Dropping a Child does not reap it, and a zombie's pid still passes
// kill(pid, 0), which vrmonitor's single-instance check runs on the last holder
// of its semaphore. An unreaped child blocks every later launch until this
// process exits.
pub(super) fn spawn_and_reap(command: &mut Command) -> Result<()> {
    let mut child = command.spawn()?;

    thread::spawn(move || {
        child.wait().ok();
    });

    Ok(())
}
const DRIVER_KEY: &str = "driver_alvr_server";
const BLOCKED_KEY: &str = "blocked_by_safe_mode";

pub fn is_steamvr_running() -> bool {
    System::new_all()
        .processes_by_name(OsStr::new(&afs::exec_fname("vrserver")))
        .count()
        != 0
}

#[cfg(not(target_os = "linux"))]
pub fn maybe_kill_steamvr() {
    let mut system = System::new_all();

    #[allow(unused_variables)]
    for process in system.processes_by_name(OsStr::new(&afs::exec_fname("vrmonitor"))) {
        debug!("Killing vrmonitor");

        #[cfg(windows)]
        windows_steamvr::kill_process(process.pid().as_u32());

        thread::sleep(Duration::from_secs(1));
    }

    system.refresh_processes(ProcessesToUpdate::All, true);

    #[allow(unused_variables)]
    for process in system.processes_by_name(OsStr::new(&afs::exec_fname("vrserver"))) {
        debug!("Killing vrserver");

        #[cfg(windows)]
        windows_steamvr::kill_process(process.pid().as_u32());

        thread::sleep(Duration::from_secs(1));
    }
}

fn unblock_alvr_driver() -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }

    let path = alvr_server_io::steamvr_settings_file_path()?;
    let text = fs::read_to_string(&path).with_context(|| format!("Failed to read {path:?}"))?;
    let new_text = unblock_alvr_driver_within_vrsettings(text.as_str())
        .with_context(|| "Failed to rewrite .vrsettings.")?;
    fs::write(&path, new_text)
        .with_context(|| "Failed to write .vrsettings back after changing it.")?;
    Ok(())
}

// Reads and writes back steamvr.vrsettings in order to
// ensure the ALVR driver is not blocked (safe mode).
fn unblock_alvr_driver_within_vrsettings(text: &str) -> Result<String> {
    let mut settings = serde_json::from_str::<serde_json::Value>(text)?;
    let values = settings
        .as_object_mut()
        .with_context(|| "Failed to parse .vrsettings.")?;
    let blocked = values
        .get(DRIVER_KEY)
        .and_then(|driver| driver.get(BLOCKED_KEY))
        .and_then(|blocked| blocked.as_bool())
        .unwrap_or(false);

    if blocked {
        debug!("Unblocking KiraVR driver in SteamVR.");
        if !values.contains_key(DRIVER_KEY) {
            values.insert(DRIVER_KEY.into(), json!({}));
        }
        let driver = settings[DRIVER_KEY]
            .as_object_mut()
            .with_context(|| "Did not find the KiraVR driver key in SteamVR settings.")?;
        driver.insert(BLOCKED_KEY.into(), json!(false)); // overwrites if present
    } else {
        debug!("KiraVR is not blocked in SteamVR.");
    }

    Ok(serde_json::to_string_pretty(&settings)?)
}

pub struct Launcher {
    _phantom: PhantomData<()>,
}

impl Launcher {
    // Returns at once, the launch runs in the background (it can wait for
    // Steam to start). Ignored while another launch or restart runs.
    pub fn launch_steamvr(&self) {
        if BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        thread::spawn(|| {
            launch_blocking();
            BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }

    pub fn ensure_steamvr_shutdown(&self) {
        shutdown_blocking();
    }
}

// Blocks until SteamVR is closed and started again. Call it from a background
// thread, without holding LAUNCHER, so the dashboard stays responsive.
pub fn restart_steamvr_blocking() {
    BUSY.store(true, std::sync::atomic::Ordering::SeqCst);
    shutdown_blocking();
    launch_blocking();
    BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
}

fn launch_blocking() {
    {
        // The ADB server might be left running because of an unclean termination of SteamVR.
        // Kill it unconditionally to ensure clean state, regardless of current connection mode.
        // Note: this will also kill a system-wide ADB server not started by ALVR.
        if let Some(path) = adb::get_adb_path(&crate::get_filesystem_layout()) {
            adb::kill_server(&path).ok();
        }

        #[cfg(target_os = "linux")]
        {
            if let Err(e) = linux_steamvr::maybe_unwrap_vrcompositor_launcher() {
                error!("Could not remove a leftover vrcompositor wrapper: {e}");
            }
            linux_steamvr::linux_hardware_checks();
            crate::preflight::before_launch();
        }

        let alvr_driver_dir = crate::get_filesystem_layout().openvr_driver_root_dir;

        // Make sure to unregister any other ALVR driver because it would cause a socket conflict
        let other_alvr_dirs = alvr_server_io::get_registered_drivers()
            .unwrap_or_default()
            .into_iter()
            .filter(|path| {
                path.to_string_lossy().to_lowercase().contains("alvr") && *path != alvr_driver_dir
            })
            .collect::<Vec<_>>();
        alvr_server_io::driver_registration(&other_alvr_dirs, false).ok();

        alvr_server_io::driver_registration(&[alvr_driver_dir], true).ok();

        if let Err(err) = unblock_alvr_driver() {
            warn!("Failed to unblock KiraVR driver: {:?}", err);
        }

        if is_steamvr_running() {
            return;
        }

        debug!("SteamVR is dead. Launching...");

        if data_sources::get_read_only_local_session()
            .settings()
            .extra
            .steamvr_launcher
            .direct_launch
        {
            let start_script = afs::filesystem_layout_invalid().server_start_script();

            if start_script.exists() {
                debug!("Running VR server start script: {}", start_script.display());

                if let Err(e) = spawn_and_reap(&mut Command::new(&start_script)) {
                    error!("Failed to run VR server start script: {e}");
                }
            } else if let Ok(steamvr_bin_dir) =
                alvr_server_io::steamvr_root_dir().map(|root| root.join("bin"))
            {
                let steamvr_path = if cfg!(windows) {
                    steamvr_bin_dir.join("win64").join("vrstartup.exe")
                } else {
                    steamvr_bin_dir.join("vrmonitor.sh")
                };

                debug!("Launching SteamVR from path: {}", steamvr_path.display());

                if let Err(e) = spawn_and_reap(&mut Command::new(&steamvr_path)) {
                    error!(
                        "Failed to run SteamVR from automatically detected path {} with error: {e}",
                        steamvr_path.display()
                    );
                }
            } else {
                error!("Failed to find SteamVR files to directly launch SteamVR");
            }
        } else {
            #[cfg(windows)]
            windows_steamvr::launch_steamvr_with_steam();

            // KiraVR: Starts Steam first if needed and never starts
            // SteamVR twice, see linux_lifecycle.
            #[cfg(target_os = "linux")]
            linux_lifecycle::careful_launch(&set_status);
        }
    }
}

fn shutdown_blocking() {
    #[cfg(target_os = "linux")]
    linux_lifecycle::wait_for_full_exit(&set_status);

    #[cfg(not(target_os = "linux"))]
    {
        debug!("Waiting for SteamVR to shutdown...");
        let start_time = Instant::now();
        while start_time.elapsed() < SHUTDOWN_TIMEOUT && is_steamvr_running() {
            thread::sleep(Duration::from_millis(500));
        }

        maybe_kill_steamvr();
    }
}

// KiraVR: Watches for a vrserver that Steam started without vrmonitor (after
// a Steam restart). It exits by itself after about 30 s and sends Quit to
// Steam on the way. Once it is gone, SteamVR is started again the normal way.
#[cfg(target_os = "linux")]
pub fn start_health_monitor() {
    thread::spawn(|| {
        let mut monitorless_since: Option<Instant> = None;
        let mut repair_pending = false;
        let mut last_repair: Option<Instant> = None;
        loop {
            thread::sleep(Duration::from_secs(2));
            if is_busy() {
                monitorless_since = None;
                continue;
            }
            let state = linux_lifecycle::scan();
            if state.monitorless_vrserver() {
                let since = *monitorless_since.get_or_insert_with(Instant::now);
                // vrmonitor normally shows up within a few seconds.
                if since.elapsed() > Duration::from_secs(8) && !repair_pending {
                    warn!("vrserver is running without vrmonitor (started by Steam on its own)");
                    set_status(Some(
                        "SteamVR was started without its window (Steam restarted it by itself). \
                         It closes on its own shortly, then KiraVR starts it again properly.",
                    ));
                    repair_pending = true;
                }
                continue;
            }
            monitorless_since = None;
            if repair_pending && state.steamvr_gone() {
                repair_pending = false;
                // Not in a loop: At most one repair every two minutes.
                if last_repair.is_some_and(|t| t.elapsed() < Duration::from_secs(120)) {
                    set_status(Some("SteamVR closed again. Start it with Launch SteamVR."));
                    continue;
                }
                last_repair = Some(Instant::now());
                if !BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    launch_blocking();
                    BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            } else if repair_pending && !state.vrmonitor.is_empty() {
                // It recovered by itself.
                repair_pending = false;
                set_status(None);
            }
        }
    });
}

// Singleton with exclusive access
pub static LAUNCHER: Mutex<Launcher> = Mutex::new(Launcher {
    _phantom: PhantomData,
});
