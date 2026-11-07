// KiraVR: Starting, stopping and watching SteamVR without confusing Steam.
//
// What went wrong before (2026-11-06):
// - Restart gave SteamVR 10 s to close, then killed vrmonitor and vrserver in
//   the middle of their shutdown and asked Steam to start SteamVR at once,
//   while Steam still listed it as running. Steam ignored the start, or ended
//   up with two SteamVR instances.
// - Launch with Steam closed ran `steam steam://rungameid/250820`. Steam also
//   starts SteamVR by itself when it was running at Steam's last exit, so
//   SteamVR started twice and one copy shut down through Steam ("Finishing
//   Steam Cloud Sync").
// - After a Steam restart, Steam can start vrserver alone, without vrstartup
//   and vrmonitor. vrserver waits 30 s for vrmonitor, exits and sends Quit to
//   Steam, which can take Steam (and a running game) down.
//
// Everything here reads /proc once per check, so it is cheap to call often.

use alvr_common::{info, warn};
use std::{
    fs,
    process::Command,
    thread,
    time::{Duration, Instant},
};

const STEAMVR_APP_ID: &str = "250820";
// Steam's shared redistributables, run alongside games.
const REDIST_APP_ID: &str = "228980";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VrProcesses {
    pub steam: Vec<u32>,
    pub steamwebhelper: bool,
    // Steam's own record of a running SteamVR: Its launch wrapper.
    pub steamvr_app: Vec<u32>,
    pub vrserver: Vec<u32>,
    pub vrmonitor: Vec<u32>,
    pub vrcompositor: Vec<u32>,
    // App ID of a running game, if any.
    pub game: Option<String>,
}

impl VrProcesses {
    pub fn steamvr_gone(&self) -> bool {
        self.vrserver.is_empty()
            && self.vrmonitor.is_empty()
            && self.vrcompositor.is_empty()
            && self.steamvr_app.is_empty()
    }

    // vrserver up without its monitor window: Started by Steam on its own
    // after a Steam restart. It gives up after about 30 s.
    pub fn monitorless_vrserver(&self) -> bool {
        !self.vrserver.is_empty() && self.vrmonitor.is_empty()
    }
}

#[derive(Debug, PartialEq)]
enum Kind {
    Steam,
    SteamWebHelper,
    Reaper(String),
    VrServer,
    VrMonitor,
    VrCompositor,
    Other,
}

fn classify(args: &[String]) -> Kind {
    let Some(first) = args.first() else {
        return Kind::Other;
    };
    let name = first.rsplit('/').next().unwrap_or(first);
    match name {
        "steam" => Kind::Steam,
        "steamwebhelper" => Kind::SteamWebHelper,
        "vrserver" if first.contains("SteamVR") => Kind::VrServer,
        "vrmonitor" if first.contains("SteamVR") => Kind::VrMonitor,
        "vrcompositor" if first.contains("SteamVR") => Kind::VrCompositor,
        "reaper" if args.iter().any(|a| a == "SteamLaunch") => args
            .iter()
            .find_map(|a| a.strip_prefix("AppId="))
            .map(|id| Kind::Reaper(id.to_owned()))
            .unwrap_or(Kind::Other),
        _ => Kind::Other,
    }
}

pub fn scan() -> VrProcesses {
    let mut found = VrProcesses::default();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args = cmdline
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect::<Vec<_>>();
        match classify(&args) {
            Kind::Steam => found.steam.push(pid),
            Kind::SteamWebHelper => found.steamwebhelper = true,
            Kind::Reaper(id) if id == STEAMVR_APP_ID => found.steamvr_app.push(pid),
            Kind::Reaper(id) if id != REDIST_APP_ID => found.game = Some(id),
            Kind::VrServer => found.vrserver.push(pid),
            Kind::VrMonitor => found.vrmonitor.push(pid),
            Kind::VrCompositor => found.vrcompositor.push(pid),
            _ => (),
        }
    }
    found
}

// "Beat Saber", or "A game (App ID 620980)" when Steam's manifest is not found.
pub fn running_game() -> Option<String> {
    let id = scan().game?;
    let name = crate::preflight::steam_root()
        .and_then(|root| {
            fs::read_to_string(root.join(format!("steamapps/appmanifest_{id}.acf"))).ok()
        })
        .and_then(|acf| {
            acf.lines().find_map(|line| {
                let rest = line.trim().strip_prefix("\"name\"")?;
                Some(rest.trim().trim_matches('"').to_owned())
            })
        });
    Some(name.unwrap_or_else(|| format!("A game (App ID {id})")))
}

fn signal(pids: &[u32], signal: &str) {
    for pid in pids {
        Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()
            .ok();
    }
}

// Waits until the check passes or the time is up. True if it passed.
fn wait_for(timeout: Duration, check: impl Fn(&VrProcesses) -> bool) -> bool {
    let start = Instant::now();
    loop {
        if check(&scan()) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

// After SteamVR was asked to quit: Waits for every part of it, Steam's record
// included, so the next start is not ignored. Stops what hangs, in order:
// vrserver first (it ends the session), then compositor and monitor, and
// Steam's launch wrapper last.
pub fn wait_for_full_exit(set_status: &dyn Fn(Option<&str>)) {
    set_status(Some("Waiting for SteamVR to close..."));
    // A game shutting down and its cloud sync can take a while.
    if wait_for(Duration::from_secs(45), VrProcesses::steamvr_gone) {
        set_status(None);
        return;
    }

    let state = scan();
    if !state.vrserver.is_empty() {
        warn!("SteamVR did not close within 45 s, stopping vrserver");
        set_status(Some("SteamVR is not closing, stopping it..."));
        signal(&state.vrserver, "-TERM");
        wait_for(Duration::from_secs(8), |s| s.vrserver.is_empty());
    }
    let state = scan();
    let leftovers = [state.vrcompositor.clone(), state.vrmonitor.clone()].concat();
    if !leftovers.is_empty() {
        info!("Stopping leftover SteamVR processes {leftovers:?}");
        signal(&leftovers, "-TERM");
        wait_for(Duration::from_secs(5), |s| {
            s.vrcompositor.is_empty() && s.vrmonitor.is_empty()
        });
    }
    // Steam's wrapper normally ends right after the processes above.
    if !wait_for(Duration::from_secs(10), |s| s.steamvr_app.is_empty()) {
        warn!("Steam still lists SteamVR as running, ending its launch wrapper");
        signal(&scan().steamvr_app, "-TERM");
        wait_for(Duration::from_secs(5), |s| s.steamvr_app.is_empty());
    }
    let state = scan();
    let stuck = [state.vrserver, state.vrcompositor, state.vrmonitor].concat();
    if !stuck.is_empty() {
        warn!("Killing SteamVR processes that ignored the stop request: {stuck:?}");
        signal(&stuck, "-KILL");
    }
    set_status(None);
}

// Starts SteamVR through Steam, exactly once.
pub fn careful_launch(set_status: &dyn Fn(Option<&str>)) {
    let state = scan();

    // Steam is starting or closing SteamVR right now: Another start request
    // would be ignored or start a second copy.
    if !state.steamvr_app.is_empty() && state.vrserver.is_empty() {
        set_status(Some("Steam is busy with SteamVR, waiting..."));
        if wait_for(Duration::from_secs(20), |s| {
            !s.vrserver.is_empty() || s.steamvr_app.is_empty()
        }) && !scan().vrserver.is_empty()
        {
            set_status(None);
            return;
        }
        if !scan().steamvr_app.is_empty() {
            warn!("Steam still lists SteamVR as running without a vrserver, not starting another");
            set_status(Some(
                "Steam still lists SteamVR as running. Close it in Steam, or restart Steam, then try again.",
            ));
            return;
        }
    }

    if state.steam.is_empty() {
        set_status(Some("Starting Steam..."));
        info!("Steam is not running, starting it first");
        if let Err(e) = super::spawn_and_reap(&mut Command::new("steam")) {
            warn!("Could not start Steam: {e}");
            set_status(Some(
                "Could not start Steam. Start it yourself, then launch SteamVR.",
            ));
            return;
        }
        if !wait_for(Duration::from_secs(90), |s| {
            !s.steam.is_empty() && s.steamwebhelper
        }) {
            set_status(Some(
                "Steam did not finish starting. Launch SteamVR again once it is up.",
            ));
            return;
        }
        // Steam starts SteamVR by itself when it was running at Steam's last
        // exit. Give it the chance, so SteamVR is not started twice.
        set_status(Some(
            "Steam is up, checking whether it starts SteamVR by itself...",
        ));
        if wait_for(Duration::from_secs(15), |s| {
            !s.steamvr_app.is_empty() || !s.vrserver.is_empty()
        }) {
            info!("Steam started SteamVR by itself");
            set_status(Some("Steam is starting SteamVR..."));
            wait_for(Duration::from_secs(30), |s| !s.vrserver.is_empty());
            set_status(None);
            return;
        }
    }

    set_status(Some("Starting SteamVR..."));
    if let Err(e) = super::spawn_and_reap(Command::new("steam").args(["steam://rungameid/250820"]))
    {
        warn!("Failed to launch SteamVR through Steam: {e}");
    }
    if !wait_for(Duration::from_secs(45), |s| !s.vrserver.is_empty()) {
        warn!("SteamVR did not start within 45 s");
        set_status(Some("SteamVR did not start. Check Steam, then try again."));
        return;
    }
    set_status(None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn recognizes_steam_and_steamvr_processes() {
        assert_eq!(
            classify(&args(
                "/home/deck/.local/share/Steam/steamrt64/steam -srt-logger-opened -steamdeck"
            )),
            Kind::Steam
        );
        assert_eq!(
            classify(&args(
                "/home/deck/.local/share/Steam/steamrt64/reaper SteamLaunch AppId=250820 -- x"
            )),
            Kind::Reaper("250820".into())
        );
        assert_eq!(
            classify(&args(
                "/home/deck/.local/share/Steam/steamapps/common/SteamVR/bin/linux64/vrserver -waitformonitor"
            )),
            Kind::VrServer
        );
        assert_eq!(
            classify(&args(
                "/home/deck/.local/share/Steam/steamapps/common/SteamVR/bin/linux64/vrmonitor -nokillprocess"
            )),
            Kind::VrMonitor
        );
        assert_eq!(
            classify(&args("./steamwebhelper -lang=en")),
            Kind::SteamWebHelper
        );
        // Not fooled by other programs mentioning the names.
        assert_eq!(classify(&args("/bin/bash -c pgrep vrserver")), Kind::Other);
        assert_eq!(classify(&args("/usr/bin/reaper something")), Kind::Other);
        assert_eq!(classify(&args("/opt/other/vrserver")), Kind::Other);
    }

    #[test]
    fn spots_a_monitorless_vrserver() {
        let mut state = VrProcesses {
            vrserver: vec![1],
            ..Default::default()
        };
        assert!(state.monitorless_vrserver());
        assert!(!state.steamvr_gone());
        state.vrmonitor.push(2);
        assert!(!state.monitorless_vrserver());
        assert!(VrProcesses::default().steamvr_gone());
    }
}
