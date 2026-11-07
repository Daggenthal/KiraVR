// KiraVR preflight: Catches host problems that otherwise show up as SteamVR
// restart loops, black screens or crashes, and says how to fix them.
//
// Every check is read-only. Fixes only run when the user asks for one, with a
// single exception: Leftover SteamVR processes are cleaned up before a launch,
// because SteamVR cannot start while they are around.
//
// Each check comes from something that broke on a real setup, see
// docs/baseline-upstream-dev14.md.

use alvr_common::{
    anyhow::{Context, Result, bail},
    error, info, warn,
};
use alvr_packets::FirewallRulesAction;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::IsTerminal,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessesToUpdate, Signal, System};

const STEAMVR_APP_ID: &str = "250820";
const STREAM_PORTS: [u16; 2] = [9943, 9944];
// Processes SteamVR leaves behind after a crash or an unclean exit. With no
// vrserver running, any of these blocks the next launch.
const STEAMVR_HELPERS: &[&str] = &[
    "vrcompositor",
    "vrmonitor",
    "vrwebhelper",
    "vrstartup",
    "vrdashboard",
    "vrprismhost",
];
// Implicit Vulkan layers that ship with the driver stack or Steam and are fine.
const KNOWN_GOOD_LAYER_PREFIXES: &[&str] = &[
    "VK_LAYER_MESA_",
    "VK_LAYER_VALVE_",
    "VK_LAYER_FROG_",
    "VK_LAYER_NV_optimus",
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status {
    Ok,
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub enum Fix {
    DisablePrism,
    DisableSteamVrHome,
    KillLeftovers(Vec<u32>),
    AddFirewallRules,
    DeleteLeftoverShm(Vec<PathBuf>),
}

impl Fix {
    pub fn label(&self) -> &'static str {
        match self {
            Fix::DisablePrism => "Turn Off Prism Mode",
            Fix::DisableSteamVrHome => "Turn Off SteamVR Home",
            Fix::KillLeftovers(_) => "Close Leftover SteamVR Processes",
            Fix::AddFirewallRules => "Add Firewall Rules",
            Fix::DeleteLeftoverShm(_) => "Delete Leftover Files",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Check {
    pub title: &'static str,
    pub status: Status,
    pub detail: String,
    pub fix: Option<Fix>,
}

impl Check {
    fn new(title: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            title,
            status,
            detail: detail.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }
}

const CHECKS: [(&str, fn() -> Check); 14] = [
    ("steam_install", steam_install),
    ("steam_client", steam_client),
    ("memory", memory),
    ("temporary_files", temporary_files),
    ("steamvr_installed", steamvr_installed),
    ("leftover_processes", leftover_processes),
    ("prism", prism),
    ("steamvr_home", steamvr_home),
    ("launch_options", launch_options),
    ("firewall", firewall),
    ("render_node", render_node),
    ("hardware_encoding", hardware_encoding),
    ("vulkan_layers", vulkan_layers),
    ("network", network),
];

pub fn run_all() -> Vec<Check> {
    CHECKS
        .iter()
        .map(|(name, check)| {
            let start = Instant::now();
            let result = check();
            alvr_common::debug!("Preflight {name} took {:?}", start.elapsed());
            result
        })
        .collect()
}

// Runs before every SteamVR launch. Cleans up leftovers, reports the rest as
// dashboard notifications.
pub fn before_launch() {
    for check in run_all() {
        if let Some(Fix::KillLeftovers(pids)) = &check.fix {
            info!("System check: {}", check.detail);
            match kill_leftovers(pids) {
                Ok(message) => info!("System check: {message}"),
                Err(e) => error!("System check: {e}"),
            }
            continue;
        }
        match check.status {
            Status::Error => error!("System check, {}: {}", check.title, check.detail),
            Status::Warning => warn!("System check, {}: {}", check.title, check.detail),
            Status::Ok | Status::Info => (),
        }
    }
}

pub fn apply_fix(fix: &Fix, layout: &alvr_filesystem::Layout) -> Result<String> {
    match fix {
        Fix::DisablePrism => {
            edit_steamvr_settings(|steamvr| {
                steamvr.insert("usePrism".into(), Value::Bool(false));
            })?;
            Ok("Turned off Prism mode in SteamVR's settings.".into())
        }
        Fix::DisableSteamVrHome => {
            edit_steamvr_settings(|steamvr| {
                steamvr.insert("enableHomeApp".into(), Value::Bool(false));
            })?;
            Ok("Turned off SteamVR Home. You get the plain grid instead.".into())
        }
        Fix::KillLeftovers(pids) => kill_leftovers(pids),
        Fix::AddFirewallRules => {
            alvr_server_io::firewall_rules(FirewallRulesAction::Add, layout).map_err(|code| {
                alvr_common::anyhow::anyhow!("The firewall script failed with exit code {code}.")
            })?;
            Ok("Added the firewall rules.".into())
        }
        Fix::DeleteLeftoverShm(paths) => delete_leftover_shm(paths),
    }
}

// --- Helpers ---

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn steamvr_settings() -> Option<Value> {
    let path = alvr_server_io::steamvr_settings_file_path().ok()?;
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

// SteamVR only writes values that differ from its defaults, so a missing key
// means whatever resources/settings/default.vrsettings says.
fn steamvr_bool(key: &str) -> Option<bool> {
    if let Some(value) = steamvr_settings().and_then(|s| s["steamvr"][key].as_bool()) {
        return Some(value);
    }
    let defaults = alvr_server_io::steamvr_root_dir()
        .ok()?
        .join("resources/settings/default.vrsettings");
    let defaults: Value = serde_json::from_str(&fs::read_to_string(defaults).ok()?).ok()?;
    defaults["steamvr"][key].as_bool()
}

pub(crate) fn steam_root() -> Option<PathBuf> {
    if let Ok(path) = alvr_server_io::steamvr_settings_file_path() {
        // .../Steam/config/steamvr.vrsettings
        return path.parent()?.parent().map(Path::to_owned);
    }
    // .../Steam/steamapps/common/SteamVR
    alvr_server_io::steamvr_root_dir()
        .ok()?
        .ancestors()
        .nth(3)
        .map(Path::to_owned)
}

// Writes go through a one-time backup next to the file. SteamVR rewrites the
// file while it runs, so edits are refused until it is closed.
fn edit_steamvr_settings(edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<()> {
    if crate::steamvr_launcher::is_steamvr_running() {
        bail!("Close SteamVR first. It overwrites its settings file while running.");
    }
    let path = alvr_server_io::steamvr_settings_file_path()?;
    let text = fs::read_to_string(&path).with_context(|| format!("Failed to read {path:?}"))?;
    let new_text = edit_steamvr_section(&text, edit)?;

    let backup = path.with_extension("vrsettings.kiravr-backup");
    if !backup.exists() {
        fs::write(&backup, &text)
            .with_context(|| format!("Failed to write the backup {backup:?}"))?;
    }
    fs::write(&path, new_text).with_context(|| format!("Failed to write {path:?}"))?;
    Ok(())
}

fn edit_steamvr_section(
    text: &str,
    edit: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> Result<String> {
    let mut settings: Value =
        serde_json::from_str(text).context("SteamVR's settings file is not valid JSON")?;
    let root = settings
        .as_object_mut()
        .context("SteamVR's settings are not a JSON object")?;
    let steamvr = root
        .entry("steamvr")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .context("The \"steamvr\" section is not a JSON object")?;
    edit(steamvr);
    Ok(serde_json::to_string_pretty(&settings)?)
}

fn leftover_pids(system: &System) -> Vec<(u32, String)> {
    system
        .processes()
        .values()
        .filter(|p| {
            let name = p.name().to_string_lossy();
            STEAMVR_HELPERS.contains(&name.as_ref())
                || (name == "reaper"
                    && p.cmd().iter().any(|a| {
                        a.to_string_lossy()
                            .contains(&format!("AppId={STEAMVR_APP_ID}"))
                    }))
        })
        .map(|p| (p.pid().as_u32(), p.name().to_string_lossy().into_owned()))
        .collect()
}

fn kill_leftovers(pids: &[u32]) -> Result<String> {
    if crate::steamvr_launcher::is_steamvr_running() {
        bail!("SteamVR is running again, leaving its processes alone.");
    }
    let mut system = System::new_all();
    for pid in pids {
        if let Some(process) = system.process(Pid::from_u32(*pid)) {
            process.kill_with(Signal::Term);
        }
    }
    thread::sleep(Duration::from_secs(2));
    system.refresh_processes(ProcessesToUpdate::All, true);
    let mut forced = 0;
    for pid in pids {
        if let Some(process) = system.process(Pid::from_u32(*pid)) {
            process.kill();
            forced += 1;
        }
    }
    Ok(if forced == 0 {
        format!("Closed {} leftover SteamVR processes.", pids.len())
    } else {
        format!(
            "Closed {} leftover SteamVR processes ({forced} had to be killed).",
            pids.len()
        )
    })
}

// Pulls the value of `key` from the block of `app_id` in a Valve KeyValues
// (VDF) file. Small on purpose: Only quoted keys, quoted values and braces.
fn vdf_app_value(text: &str, app_id: &str, key: &str) -> Option<String> {
    let needle = format!("\"{app_id}\"");
    let mut search_from = 0;
    while let Some(found) = text[search_from..].find(&needle) {
        let after = search_from + found + needle.len();
        search_from = after;
        let rest = text[after..].trim_start();
        if !rest.starts_with('{') {
            continue;
        }
        let block_start = text.len() - rest.len() + 1;
        let block = &text[block_start..block_start + block_len(&text[block_start..])?];
        if let Some(value) = vdf_value_at_depth0(block, key) {
            return Some(value);
        }
    }
    None
}

fn block_len(body: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => (),
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => (),
        }
    }
    None
}

fn vdf_value_at_depth0(block: &str, key: &str) -> Option<String> {
    let tokens = vdf_tokens(block);
    let mut depth = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "{" => depth += 1,
            "}" => depth = depth.saturating_sub(1),
            t if depth == 0 && t.eq_ignore_ascii_case(key) => {
                return tokens.get(i + 1).filter(|v| *v != "{").cloned();
            }
            _ => (),
        }
        i += 1;
    }
    None
}

fn vdf_tokens(text: &str) -> Vec<String> {
    let mut tokens = vec![];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' => tokens.push(c.to_string()),
            '"' => {
                let mut value = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(next) = chars.next() {
                                value.push(match next {
                                    'n' => '\n',
                                    't' => '\t',
                                    other => other,
                                });
                            }
                        }
                        '"' => break,
                        other => value.push(other),
                    }
                }
                tokens.push(value);
            }
            _ => (),
        }
    }
    tokens
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(cmd).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn service_active(name: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", name])
        .status()
        .is_ok_and(|s| s.success())
}

// "1024-65535/tcp 9943/udp" covers `port` for `proto`?
fn ports_cover(list: &str, port: u16, proto: &str) -> bool {
    list.split_whitespace().any(|entry| {
        let Some((range, p)) = entry.split_once('/') else {
            return false;
        };
        if p != proto {
            return false;
        }
        let (lo, hi) = range.split_once('-').unwrap_or((range, range));
        matches!((lo.parse::<u16>(), hi.parse::<u16>()), (Ok(lo), Ok(hi)) if lo <= port && port <= hi)
    })
}

// --- Checks ---

fn steamvr_installed() -> Check {
    const TITLE: &str = "SteamVR";
    match alvr_server_io::steamvr_root_dir() {
        Ok(dir) if dir.join("bin/linux64/vrserver").exists() => {
            Check::new(TITLE, Status::Ok, format!("Found at {}", dir.display()))
        }
        Ok(dir) => Check::new(
            TITLE,
            Status::Error,
            format!(
                "SteamVR is registered at {} but its files are missing. Reinstall SteamVR in Steam.",
                dir.display()
            ),
        ),
        Err(_) => Check::new(
            TITLE,
            Status::Error,
            "SteamVR was not found. Install SteamVR from Steam and start it once.",
        ),
    }
}

fn leftover_processes() -> Check {
    const TITLE: &str = "SteamVR Processes";
    let system = System::new_all();
    if crate::steamvr_launcher::is_steamvr_running() {
        return Check::new(TITLE, Status::Ok, "SteamVR is running.");
    }
    let leftovers = leftover_pids(&system);
    if leftovers.is_empty() {
        return Check::new(
            TITLE,
            Status::Ok,
            "Nothing left over from a previous SteamVR run.",
        );
    }
    let mut names = leftovers
        .iter()
        .map(|(_, n)| n.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    Check::new(
        TITLE,
        Status::Error,
        format!(
            "SteamVR did not fully close ({} still running). SteamVR cannot start again until they are gone.",
            names.join(", ")
        ),
    )
    .with_fix(Fix::KillLeftovers(leftovers.into_iter().map(|(pid, _)| pid).collect()))
}

// Prism is a newer, undocumented SteamVR render host (vrprismhost, off by
// default in desktop SteamVR). It talks to drivers through internal interfaces
// and has its own VRInitError_Init_PrismNeedsNewDrivers, so a public-API
// driver like ours cannot run under it. On the PC only the Lighthouse driver
// references it, Steam Link (which Steam Frame streams through) does not. It
// may be the default in the Frame's own on-headset SteamVR, unverified.
// Revisit when Valve documents it.
fn prism() -> Check {
    const TITLE: &str = "SteamVR Prism Mode";
    let on = steamvr_bool("usePrism").unwrap_or(false);
    if on {
        Check::new(
            TITLE,
            Status::Error,
            "SteamVR's experimental Prism mode (\"usePrism\") is on. It is off by default in desktop SteamVR \
            and does not work with KiraVR yet: Prism needs drivers built against interfaces Valve has not published, it never \
            starts, and SteamVR restarts itself every couple of minutes (error 156). Turning it off returns \
            SteamVR to its standard mode.",
        )
        .with_fix(Fix::DisablePrism)
    } else {
        Check::new(TITLE, Status::Ok, "Prism mode is off (SteamVR's default).")
    }
}

fn steamvr_home() -> Check {
    const TITLE: &str = "SteamVR Home";
    let enabled = steamvr_bool("enableHomeApp").unwrap_or(false);
    if !enabled {
        return Check::new(TITLE, Status::Ok, "SteamVR Home is off.");
    }
    let installed = alvr_server_io::steamvr_root_dir().is_ok_and(|d| {
        d.join("tools/steamvr_environments/game/steamtours.sh")
            .exists()
    });
    if !installed {
        return Check::new(TITLE, Status::Ok, "SteamVR Home is not installed.");
    }
    // steamtours.sh hardcodes this path. The steamrt3 Steam client points
    // ~/.steam/bin at steamrt32, which does not have it.
    let runtime = home_dir().map(|h| h.join(".steam/bin/steam-runtime/run.sh"));
    if runtime.as_ref().is_some_and(|p| p.exists()) {
        return Check::new(TITLE, Status::Ok, "SteamVR Home can start.");
    }
    Check::new(
        TITLE,
        Status::Warning,
        "SteamVR Home cannot start: It needs ~/.steam/bin/steam-runtime/run.sh, which the current Steam \
        client no longer provides. SteamVR retries it forever, Home keeps loading and the SteamVR menu \
        stops responding.",
    )
    .with_fix(Fix::DisableSteamVrHome)
}

fn launch_options() -> Check {
    const TITLE: &str = "SteamVR Launch Options";
    const WHERE: &str =
        "Change them in Steam: Library, right-click SteamVR, Properties, General, Launch Options.";
    let Some(root) = steam_root() else {
        return Check::new(
            TITLE,
            Status::Info,
            "Could not find the Steam folder to check them.",
        );
    };
    let mut options = vec![];
    if let Ok(users) = fs::read_dir(root.join("userdata")) {
        for user in users.flatten() {
            let file = user.path().join("config/localconfig.vdf");
            if let Ok(text) = fs::read_to_string(&file)
                && let Some(value) = vdf_app_value(&text, STEAMVR_APP_ID, "LaunchOptions")
                && !value.trim().is_empty()
                && !options.contains(&value)
            {
                options.push(value);
            }
        }
    }

    let Some(value) = options.first() else {
        return Check::new(TITLE, Status::Ok, "None set (recommended).");
    };
    if let Some(token) = value
        .split_whitespace()
        .find(|t| t.contains("vrmonitor.sh"))
    {
        let path = Path::new(token.trim_matches('"'));
        if path.is_absolute() && path.exists() {
            return Check::new(
                TITLE,
                Status::Warning,
                format!(
                    "\"{value}\" is set. KiraVR does not need it, SteamVR already starts vrmonitor itself. \
                    Removing it rules out problems. {WHERE}"
                ),
            );
        }
        return Check::new(
            TITLE,
            Status::Error,
            format!(
                "\"{value}\" points to \"{token}\", which does not exist. Steam cannot start vrmonitor, so \
                SteamVR shuts down after about 25 seconds and takes the Steam client with it. Clear the \
                launch options. {WHERE}"
            ),
        );
    }
    Check::new(
        TITLE,
        Status::Info,
        format!(
            "Custom launch options are set: \"{value}\". If SteamVR misbehaves, try without them. {WHERE}"
        ),
    )
}

fn firewall() -> Check {
    const TITLE: &str = "Firewall";
    if service_active("firewalld") {
        let Some(zones) = run("firewall-cmd", &["--get-active-zones"]) else {
            return Check::new(
                TITLE,
                Status::Info,
                "firewalld is running but its rules could not be read.",
            );
        };
        let zones = zones
            .lines()
            .filter(|l| !l.starts_with(char::is_whitespace) && !l.trim().is_empty())
            .filter_map(|l| l.split_whitespace().next())
            .collect::<Vec<_>>();
        let blocked = zones
            .iter()
            .filter(|zone| {
                let zone_arg = format!("--zone={zone}");
                let services =
                    run("firewall-cmd", &[&zone_arg, "--list-services"]).unwrap_or_default();
                let ports = run("firewall-cmd", &[&zone_arg, "--list-ports"]).unwrap_or_default();
                let allowed = services.split_whitespace().any(|s| s == "alvr")
                    || STREAM_PORTS
                        .iter()
                        .all(|p| ports_cover(&ports, *p, "tcp") && ports_cover(&ports, *p, "udp"));
                !allowed
            })
            .copied()
            .collect::<Vec<_>>();
        return if blocked.is_empty() {
            Check::new(TITLE, Status::Ok, "firewalld allows ports 9943 and 9944.")
        } else {
            Check::new(
                TITLE,
                Status::Warning,
                format!(
                    "firewalld zone {} does not allow ports 9943 and 9944 (TCP and UDP). The headset will \
                    not find the PC.",
                    blocked.join(", ")
                ),
            )
            .with_fix(Fix::AddFirewallRules)
        };
    }
    if service_active("ufw") {
        return Check::new(
            TITLE,
            Status::Info,
            "ufw is active. Its rules cannot be read without root. If the headset cannot find the PC, add the \
            firewall rules.",
        )
        .with_fix(Fix::AddFirewallRules);
    }
    Check::new(TITLE, Status::Ok, "No firewalld or ufw running.")
}

fn render_node() -> Check {
    const TITLE: &str = "GPU Access";
    let mut nodes = fs::read_dir("/dev/dri")
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("renderD"))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    nodes.sort();
    if nodes.is_empty() {
        return Check::new(
            TITLE,
            Status::Error,
            "No GPU render node in /dev/dri. Is the GPU driver loaded?",
        );
    }
    let usable = nodes
        .iter()
        .filter(|p| OpenOptions::new().read(true).write(true).open(p).is_ok())
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();
    if usable.is_empty() {
        Check::new(
            TITLE,
            Status::Error,
            "No access to the GPU render nodes. Add your user to the \"render\" group (or \"video\" on some \
            distros), then log out and back in.",
        )
    } else {
        Check::new(TITLE, Status::Ok, format!("Can use {}.", usable.join(", ")))
    }
}

// GPU vendors by PCI ID, from the render nodes' sysfs entries.
fn gpu_vendors() -> Vec<&'static str> {
    let mut vendors = vec![];
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("renderD") {
                continue;
            }
            let id = fs::read_to_string(entry.path().join("device/vendor")).unwrap_or_default();
            let vendor = match id.trim() {
                "0x1002" => "AMD",
                "0x8086" => "Intel",
                "0x10de" => "NVIDIA",
                _ => continue,
            };
            if !vendors.contains(&vendor) {
                vendors.push(vendor);
            }
        }
    }
    vendors
}

// Opens a real encode session (config, surfaces, context), like the stream
// does. Some drivers list an encoder and then fail to open it, which used to
// surface only as a black screen once streaming started.
fn open_vaapi_encoder(
    display: &std::rc::Rc<libva::Display>,
    profile: libva::VAProfile::Type,
    entrypoint: libva::VAEntrypoint::Type,
) -> Result<(), String> {
    const WIDTH: u32 = 1920;
    const HEIGHT: u32 = 1088;
    let format = libva::constants::VA_RT_FORMAT_YUV420;
    let attrs = vec![libva::VAConfigAttrib {
        type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
        value: format,
    }];
    let config = display
        .create_config(attrs, profile, entrypoint)
        .map_err(|e| format!("config: {e:?}"))?;
    let surfaces = display
        .create_surfaces(format, None, WIDTH, HEIGHT, None, vec![(); 2])
        .map_err(|e| format!("surfaces: {e:?}"))?;
    display
        .create_context(&config, WIDTH, HEIGHT, Some(&surfaces), true)
        .map_err(|e| format!("context: {e:?}"))?;
    Ok(())
}

fn hardware_encoding() -> Check {
    const TITLE: &str = "Hardware Encoding";
    let vendors = gpu_vendors();

    // NVIDIA streams through NVENC, not VA-API.
    if vendors == ["NVIDIA"] {
        let found = [
            "/usr/lib/libnvidia-encode.so.1",
            "/usr/lib64/libnvidia-encode.so.1",
            "/usr/lib/x86_64-linux-gnu/libnvidia-encode.so.1",
        ]
        .iter()
        .any(|p| Path::new(p).exists());
        return if found {
            Check::new(
                TITLE,
                Status::Info,
                "NVIDIA GPU with the NVENC library installed. Note that KiraVR's NVENC path is still \
                untested on Linux, AMD and Intel are the tested setups.",
            )
        } else {
            Check::new(
                TITLE,
                Status::Error,
                "NVIDIA GPU, but the NVENC library (libnvidia-encode.so.1) was not found. Install your \
                distribution's NVIDIA driver package that includes it.",
            )
        };
    }

    let Some(display) = libva::Display::open() else {
        return Check::new(
            TITLE,
            Status::Error,
            format!(
                "VA-API is not available, so the {} GPU cannot encode in hardware. Install your \
                distribution's VA-API driver (Mesa for AMD, intel-media-driver for Intel).",
                vendors.first().unwrap_or(&"current")
            ),
        );
    };
    let vendor = display.query_vendor_string().unwrap_or_default();

    let mut working = vec![];
    let mut broken = vec![];
    for (profile, name) in [
        (libva::VAProfile::VAProfileH264Main, "H.264"),
        (libva::VAProfile::VAProfileHEVCMain, "HEVC"),
        (libva::VAProfile::VAProfileAV1Profile0, "AV1"),
    ] {
        let Ok(entrypoints) = display.query_config_entrypoints(profile) else {
            continue;
        };
        let entrypoint = [
            libva::VAEntrypoint::VAEntrypointEncSlice,
            libva::VAEntrypoint::VAEntrypointEncSliceLP,
        ]
        .into_iter()
        .find(|e| entrypoints.contains(e));
        let Some(entrypoint) = entrypoint else {
            continue;
        };
        match open_vaapi_encoder(&display, profile, entrypoint) {
            Ok(()) => working.push(name),
            Err(e) => broken.push(format!("{name} ({e})")),
        }
    }

    if working.is_empty() {
        let detail = if broken.is_empty() {
            format!(
                "VA-API ({vendor}) offers no video encoders. KiraVR would fall back to slow software encoding."
            )
        } else {
            format!(
                "VA-API ({vendor}) lists encoders but none of them opens: {}. KiraVR would fall back to slow \
                software encoding.",
                broken.join(", ")
            )
        };
        return Check::new(TITLE, Status::Error, detail);
    }
    if !broken.is_empty() {
        return Check::new(
            TITLE,
            Status::Warning,
            format!(
                "{} work via {vendor}, but {} failed to open. Pick a working codec in Settings, Video, \
                Preferred codec.",
                working.join(", "),
                broken.join(", ")
            ),
        );
    }
    Check::new(
        TITLE,
        Status::Ok,
        format!("{} open via {vendor}.", working.join(", ")),
    )
}

// Flatpak Steam keeps its files in the Flatpak sandbox, which a dashboard
// outside it cannot drive (the driver path and SteamVR settings live there).
fn steam_install() -> Check {
    const TITLE: &str = "Steam Install";
    let Some(home) = home_dir() else {
        return Check::new(TITLE, Status::Info, "HOME is not set, could not check.");
    };
    let native = home.join(".local/share/Steam/steamapps").exists()
        || home.join(".steam/steam/steamapps").exists();
    let flatpak = home
        .join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps")
        .exists()
        || home
            .join(".var/app/com.valvesoftware.Steam/data/Steam/steamapps")
            .exists();
    match (native, flatpak) {
        (true, false) => Check::new(TITLE, Status::Ok, "Steam is installed natively."),
        (true, true) => Check::new(
            TITLE,
            Status::Info,
            "Both a native and a Flatpak Steam are installed. KiraVR uses the native one, so start \
            SteamVR from that Steam.",
        ),
        (false, true) => Check::new(
            TITLE,
            Status::Error,
            "Steam is the Flatpak version. KiraVR cannot drive SteamVR inside the Flatpak sandbox yet. \
            Install Steam from your distribution (not Flatpak) to use KiraVR.",
        ),
        (false, false) => Check::new(
            TITLE,
            Status::Error,
            "Steam was not found. Install Steam first.",
        ),
    }
}

fn vulkan_layers() -> Check {
    const TITLE: &str = "Vulkan Layers";
    let home = home_dir();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let dirs = [
        data_home.map(|d| d.join("vulkan/implicit_layer.d")),
        config_home.map(|d| d.join("vulkan/implicit_layer.d")),
        Some(PathBuf::from("/etc/vulkan/implicit_layer.d")),
        Some(PathBuf::from("/usr/local/share/vulkan/implicit_layer.d")),
        Some(PathBuf::from("/usr/share/vulkan/implicit_layer.d")),
    ];

    let mut always_on = vec![];
    for dir in dirs.into_iter().flatten() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(json) =
                fs::read_to_string(entry.path()).map(|t| serde_json::from_str::<Value>(&t))
            else {
                continue;
            };
            let Ok(json) = json else {
                continue;
            };
            let layers = match (&json["layer"], json["layers"].as_array()) {
                (layer @ Value::Object(_), _) => vec![layer.clone()],
                (_, Some(list)) => list.clone(),
                _ => continue,
            };
            for layer in layers {
                let Some(name) = layer["name"].as_str() else {
                    continue;
                };
                if KNOWN_GOOD_LAYER_PREFIXES
                    .iter()
                    .any(|p| name.starts_with(p))
                {
                    continue;
                }
                // Layers with enable_environment are off unless that variable is set.
                let env_set = |key: &str| {
                    layer[key]
                        .as_object()
                        .is_some_and(|m| m.keys().any(|k| std::env::var_os(k).is_some()))
                };
                if layer["enable_environment"].is_object() && !env_set("enable_environment") {
                    continue;
                }
                if env_set("disable_environment") {
                    continue;
                }
                if !always_on.iter().any(|n: &String| n == name) {
                    always_on.push(name.to_owned());
                }
            }
        }
    }

    if always_on.is_empty() {
        return Check::new(TITLE, Status::Ok, "No always-on third-party Vulkan layers.");
    }
    Check::new(
        TITLE,
        Status::Info,
        format!(
            "Always-on Vulkan layers load into SteamVR's compositor and every VR game: {}. They usually just \
            pass frames through, but a frame generation or overlay layer that activates for VR can cause \
            stutter or black frames. If you see either, turn the layer off for SteamVR in its own settings.",
            always_on.join(", ")
        ),
    )
}

fn network() -> Check {
    const TITLE: &str = "Network";
    let Ok(routes) = fs::read_to_string("/proc/net/route") else {
        return Check::new(TITLE, Status::Info, "Could not read the routing table.");
    };
    // Default route with the lowest metric
    let iface = routes
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f = l.split_whitespace().collect::<Vec<_>>();
            (f.get(1) == Some(&"00000000")).then(|| {
                (
                    f.get(6)
                        .and_then(|m| m.parse::<u32>().ok())
                        .unwrap_or(u32::MAX),
                    f[0].to_owned(),
                )
            })
        })
        .min()
        .map(|(_, iface)| iface);
    let Some(iface) = iface else {
        return Check::new(TITLE, Status::Info, "No default network route found.");
    };
    let sys = Path::new("/sys/class/net").join(&iface);
    if sys.join("wireless").exists() || sys.join("phy80211").exists() {
        Check::new(
            TITLE,
            Status::Warning,
            format!(
                "The PC is on Wi-Fi ({iface}). The stream then crosses Wi-Fi twice (PC, router, headset), which \
                causes short freezes. Connect the PC with Ethernet, or use a USB cable with Wired mode."
            ),
        )
    } else {
        Check::new(
            TITLE,
            Status::Ok,
            format!("The PC uses a wired connection ({iface})."),
        )
    }
}

// --- Terminal report (alvr_dashboard --preflight) ---

// Colors only on a TTY and only when NO_COLOR is unset. KIRAVR_DEBUG=1 adds
// how long each check took.
pub fn print_report(apply_fixes: bool, layout: &alvr_filesystem::Layout) -> i32 {
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let debug = std::env::var("KIRAVR_DEBUG").is_ok_and(|v| v == "1");
    let paint = |code: &str, text: &str| {
        if color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    };

    println!("{}", paint("1", "KiraVR System Check"));
    let mut worst = Status::Ok;
    for (name, check) in CHECKS {
        let start = Instant::now();
        let result = check();
        let elapsed = start.elapsed();
        let tag = match result.status {
            Status::Ok => paint("32", "  OK  "),
            Status::Info => paint("1", " INFO "),
            Status::Warning => paint("33", " WARN "),
            Status::Error => paint("31", "ERROR "),
        };
        println!("[{tag}] {}: {}", result.title, result.detail);
        if debug {
            println!(
                "         {}",
                paint("36", &format!("{name} took {elapsed:?}"))
            );
        }
        worst = worst.max(result.status);

        if let Some(fix) = &result.fix {
            if apply_fixes {
                match apply_fix(fix, layout) {
                    Ok(message) => println!("         {} {message}", paint("32", "Fixed:")),
                    Err(e) => println!("         {} {e}", paint("31", "Fix failed:")),
                }
            } else {
                println!("         Fix available: {} (run with --fix)", fix.label());
            }
        }
    }

    match worst {
        Status::Error => {
            println!(
                "{}",
                paint("1;31", "Problems found that will stop VR from working.")
            );
            1
        }
        Status::Warning => {
            println!("{}", paint("33", "Warnings found, VR may still work."));
            0
        }
        Status::Ok | Status::Info => {
            println!("{}", paint("32", "Everything looks good."));
            0
        }
    }
}

// --- Memory, temporary files and the Steam client (2026-11-06) ---
//
// Low memory made Steam's web helper crash every few seconds ("double free",
// fontconfig "out of memory"), worst with SteamVR running, until Steam shut
// itself down and took the game with it. Swap was full because 3.6 GB of
// RAM-backed temporary files were left over.

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct MemInfo {
    total: u64,
    available: u64,
    swap_total: u64,
    swap_free: u64,
}

fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let field = |key: &str| {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(key)?.strip_prefix(':')?;
            rest.split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
                .map(|kb| kb * 1024)
        })
    };
    Some(MemInfo {
        total: field("MemTotal")?,
        available: field("MemAvailable")?,
        swap_total: field("SwapTotal").unwrap_or(0),
        swap_free: field("SwapFree").unwrap_or(0),
    })
}

fn assess_memory(m: MemInfo) -> (Status, String) {
    let gb = |b: u64| b as f64 / GB;
    let swap_used = m.swap_total.saturating_sub(m.swap_free);
    let swap_share = if m.swap_total > 0 {
        swap_used as f64 / m.swap_total as f64
    } else {
        0.0
    };
    let summary = format!(
        "{:.1} of {:.1} GB of RAM free, swap {:.1} of {:.1} GB used.",
        gb(m.available),
        gb(m.total),
        gb(swap_used),
        gb(m.swap_total)
    );
    let advice = "Programs that need more memory now crash, Steam's web helper first, and Steam \
                  can shut down in the middle of a game. Close programs you do not need, check \
                  Temporary Files below, then restart Steam.";
    if swap_share >= 0.95 || m.available < (1.5 * GB) as u64 {
        (
            Status::Error,
            format!("Memory is almost gone: {summary} {advice}"),
        )
    } else if swap_share >= 0.75 || m.available < (3.0 * GB) as u64 {
        (
            Status::Warning,
            format!(
                "Memory is getting tight: {summary} Close programs you do not need before playing."
            ),
        )
    } else if m.swap_total < (4.0 * GB) as u64 {
        (
            Status::Info,
            format!(
                "{summary} Only {:.1} GB of swap: A swap file of 8 GB gives more room before things crash.",
                gb(m.swap_total)
            ),
        )
    } else {
        (Status::Ok, summary)
    }
}

fn memory() -> Check {
    const TITLE: &str = "Memory";
    match fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| parse_meminfo(&t))
    {
        Some(info) => {
            let (status, detail) = assess_memory(info);
            Check::new(TITLE, status, detail)
        }
        None => Check::new(TITLE, Status::Info, "Could not read /proc/meminfo."),
    }
}

// Files younger than this are never called leftovers: A program may have
// just created them, or be restarting.
const SHM_MIN_AGE: Duration = Duration::from_secs(30 * 60);
// Files that exist on purpose without being open: Podman's lock (distrobox
// needs it), POSIX semaphores, and Steam's own IPC files.
const SHM_KEEP_PREFIXES: &[&str] = &["libpod", "sem.", ".com.valvesoftware.", "lttng"];

// Every /dev/shm file some process has open or mapped, and how many of the
// user's own processes could not be looked into (they run with extra rights,
// like Decky plugins), so their files are unknown.
fn shm_files_in_use() -> (std::collections::HashSet<PathBuf>, usize) {
    use std::os::unix::fs::MetadataExt;
    let mut used = std::collections::HashSet::new();
    let mut hidden = 0;
    let me = fs::metadata("/proc/self").map(|m| m.uid()).ok();
    let Ok(entries) = fs::read_dir("/proc") else {
        return (used, hidden);
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let own = me.is_some() && entry.metadata().map(|m| m.uid()).ok() == me;
        let fds = fs::read_dir(entry.path().join("fd"));
        let maps = fs::read_to_string(entry.path().join("maps"));
        if own && (fds.is_err() || maps.is_err()) {
            hidden += 1;
        }
        for fd in fds.into_iter().flatten().flatten() {
            if let Ok(target) = fs::read_link(fd.path())
                && target.starts_with("/dev/shm/")
            {
                used.insert(target);
            }
        }
        for line in maps.iter().flat_map(|m| m.lines()) {
            if let Some(at) = line.find("/dev/shm/") {
                used.insert(PathBuf::from(line[at..].trim_end_matches(" (deleted)")));
            }
        }
    }
    (used, hidden)
}

// "u1000-Shm_*" for "u1000-Shm_8e10b10a": Groups leftovers by program.
fn shm_group(name: &str) -> String {
    let stem = name.trim_end_matches(|c: char| c.is_ascii_hexdigit());
    if stem.len() < name.len() && stem.len() >= 3 {
        format!("{stem}*")
    } else {
        name.to_owned()
    }
}

struct ShmLeftovers {
    paths: Vec<PathBuf>,
    bytes: u64,
    // (group, files, bytes), largest first.
    groups: Vec<(String, usize, u64)>,
    // Own processes that could not be checked.
    hidden: usize,
}

// The user's own /dev/shm files that no running program uses and that are
// older than SHM_MIN_AGE, with their size in memory (allocated blocks, they
// are often sparse).
fn leftover_shm() -> ShmLeftovers {
    use std::os::unix::fs::MetadataExt;
    let mut result = ShmLeftovers {
        paths: vec![],
        bytes: 0,
        groups: vec![],
        hidden: 0,
    };
    let Ok(me) = fs::metadata("/proc/self").map(|m| m.uid()) else {
        return result;
    };
    let (used, hidden) = shm_files_in_use();
    result.hidden = hidden;
    let mut groups = std::collections::BTreeMap::<String, (usize, u64)>::new();
    for entry in fs::read_dir("/dev/shm").into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old_enough = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= SHM_MIN_AGE);
        let name = entry.file_name().to_string_lossy().into_owned();
        let keep = SHM_KEEP_PREFIXES.iter().any(|p| name.starts_with(p));
        if meta.is_file() && meta.uid() == me && !used.contains(&path) && old_enough && !keep {
            let size = meta.blocks() * 512;
            let group = groups.entry(shm_group(&name)).or_default();
            group.0 += 1;
            group.1 += size;
            result.bytes += size;
            result.paths.push(path);
        }
    }
    result.groups = groups
        .into_iter()
        .map(|(name, (files, bytes))| (name, files, bytes))
        .collect();
    result.groups.sort_by_key(|g| std::cmp::Reverse(g.2));
    result
}

// Used bytes of a RAM-backed (tmpfs) mount, None if it is on disk.
fn tmpfs_used(mount: &str) -> Option<u64> {
    let output = Command::new("df")
        .args(["-B1", "--output=fstype,used", mount])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.lines().nth(1)?.split_whitespace();
    (fields.next()? == "tmpfs").then_some(())?;
    fields.next()?.parse().ok()
}

fn temporary_files() -> Check {
    const TITLE: &str = "Temporary Files";
    let leftovers = leftover_shm();
    let tmp = tmpfs_used("/tmp").filter(|b| *b as f64 >= 2.0 * GB);
    let tmp_note = tmp
        .map(|b| {
            format!(
                " /tmp is in RAM and holds {:.1} GB, look there for large files you no longer need.",
                b as f64 / GB
            )
        })
        .unwrap_or_default();
    let hidden_note = if leftovers.hidden > 0 {
        format!(
            " {} of your programs run with extra rights (like Decky plugins) and could not be checked.",
            leftovers.hidden
        )
    } else {
        String::new()
    };

    if leftovers.bytes as f64 >= 256.0 * 1024.0 * 1024.0 {
        let groups = leftovers
            .groups
            .iter()
            .take(4)
            .map(|(name, files, bytes)| {
                format!("{name} ({files} files, {:.1} GB)", *bytes as f64 / GB)
            })
            .collect::<Vec<_>>()
            .join(", ");
        Check::new(
            TITLE,
            Status::Warning,
            format!(
                "Leftover files in /dev/shm hold {:.1} GB of RAM: {groups}. No running program has \
                 them open and they are older than 30 minutes, so a program that crashed or quit \
                 left them behind (Steam's web helper does this when it crashes).{hidden_note}{tmp_note}",
                leftovers.bytes as f64 / GB
            ),
        )
        .with_fix(Fix::DeleteLeftoverShm(leftovers.paths))
    } else if tmp.is_some() {
        Check::new(TITLE, Status::Warning, tmp_note.trim_start().to_owned())
    } else {
        Check::new(
            TITLE,
            Status::Ok,
            "No large leftovers in RAM-backed temporary folders.",
        )
    }
}

// Deletes only files that are still unused right now.
fn delete_leftover_shm(paths: &[PathBuf]) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let (used, _) = shm_files_in_use();
    let mut freed = 0;
    let mut deleted = 0;
    for path in paths {
        if used.contains(path) || !path.starts_with("/dev/shm/") {
            continue;
        }
        let size = fs::metadata(path).map(|m| m.blocks() * 512).unwrap_or(0);
        if fs::remove_file(path).is_ok() {
            freed += size;
            deleted += 1;
        }
    }
    Ok(format!(
        "Deleted {deleted} leftover files and freed {:.1} GB of RAM.",
        freed as f64 / GB
    ))
}

// "beta" for beta channels, "stable" otherwise. SteamOS writes
// steamdeck_stable into the beta file for its stable channel.
fn steam_channel(beta_file: Option<&str>) -> (&'static str, String) {
    match beta_file.map(str::trim).filter(|b| !b.is_empty()) {
        Some(name) if !name.contains("stable") => ("beta", name.to_owned()),
        Some(name) => ("stable", name.to_owned()),
        None => ("stable", "stable".to_owned()),
    }
}

// Crash and assert dumps Steam wrote in the last hour.
fn recent_steam_dumps() -> usize {
    let Ok(entries) = fs::read_dir("/tmp/dumps") else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".dmp"))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < Duration::from_secs(3600))
        })
        .count()
}

fn steam_client() -> Check {
    const TITLE: &str = "Steam Client";
    let beta = steam_root().and_then(|root| fs::read_to_string(root.join("package/beta")).ok());
    let (kind, name) = steam_channel(beta.as_deref());
    let dumps = recent_steam_dumps();
    let channel = if kind == "beta" {
        format!("Steam is on the beta channel ({name}).")
    } else {
        "Steam is on the stable channel.".to_owned()
    };
    if dumps >= 10 {
        let beta_hint = if kind == "beta" {
            " Switching Steam to stable can help: Steam, Settings, Interface, Client Beta Participation."
        } else {
            ""
        };
        Check::new(
            TITLE,
            Status::Warning,
            format!(
                "Steam crashed or hit an error {dumps} times in the last hour (crash dumps in /tmp/dumps). \
                 This usually means memory ran out, see Memory and Temporary Files.{beta_hint} {channel}"
            ),
        )
    } else if kind == "beta" {
        Check::new(
            TITLE,
            Status::Info,
            format!(
                "{channel} If Steam gets unstable, switch to stable: Steam, Settings, Interface, Client Beta Participation."
            ),
        )
    } else {
        Check::new(TITLE, Status::Ok, channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_launch_options_from_vdf() {
        let vdf = r#""UserLocalConfigStore" { "Software" { "Valve" { "Steam" { "apps" {
            "620980" { "LaunchOptions" "%command% -foo" }
            "250820" { "LastPlayed" "1" "LaunchOptions" "/a/b/vrmonitor.sh %command%" "cloud" { "x" "y" } }
        } } } } }"#;
        assert_eq!(
            vdf_app_value(vdf, "250820", "LaunchOptions").as_deref(),
            Some("/a/b/vrmonitor.sh %command%")
        );
        assert_eq!(vdf_app_value(vdf, "999", "LaunchOptions"), None);
    }

    #[test]
    fn handles_escaped_quotes_in_vdf() {
        let vdf = r#""250820" { "LaunchOptions" "FOO=\"a b\" %command%" }"#;
        assert_eq!(
            vdf_app_value(vdf, "250820", "LaunchOptions").as_deref(),
            Some(r#"FOO="a b" %command%"#)
        );
    }

    #[test]
    fn edits_only_the_requested_key() {
        let text = r#"{"steamvr":{"usePrism":true,"ipd":0.063},"driver_alvr_server":{"x":1}}"#;
        let out = edit_steamvr_section(text, |s| {
            s.insert("usePrism".into(), Value::Bool(false));
        })
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["steamvr"]["usePrism"], Value::Bool(false));
        assert_eq!(v["steamvr"]["ipd"], serde_json::json!(0.063));
        assert_eq!(v["driver_alvr_server"]["x"], serde_json::json!(1));

        // A file without a "steamvr" section gets one
        let out = edit_steamvr_section("{}", |s| {
            s.insert("enableHomeApp".into(), Value::Bool(false));
        })
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["steamvr"]["enableHomeApp"], Value::Bool(false));

        assert!(edit_steamvr_section("not json", |_| ()).is_err());
    }

    #[test]
    fn port_ranges() {
        assert!(ports_cover("1024-65535/tcp 1024-65535/udp", 9943, "udp"));
        assert!(ports_cover("9943/tcp 9944/tcp", 9944, "tcp"));
        assert!(!ports_cover("9943/tcp", 9943, "udp"));
        assert!(!ports_cover("", 9943, "tcp"));
    }

    fn mem(total_gb: f64, available_gb: f64, swap_total_gb: f64, swap_free_gb: f64) -> MemInfo {
        let b = |g: f64| (g * GB) as u64;
        MemInfo {
            total: b(total_gb),
            available: b(available_gb),
            swap_total: b(swap_total_gb),
            swap_free: b(swap_free_gb),
        }
    }

    #[test]
    fn memory_levels() {
        // The 2026-11-06 crash loop: swap full.
        assert_eq!(assess_memory(mem(15.0, 4.7, 8.7, 0.3)).0, Status::Error);
        assert_eq!(assess_memory(mem(15.0, 1.0, 16.0, 15.0)).0, Status::Error);
        assert_eq!(assess_memory(mem(15.0, 5.0, 16.0, 3.0)).0, Status::Warning);
        assert_eq!(assess_memory(mem(15.0, 7.0, 2.0, 2.0)).0, Status::Info);
        let (status, detail) = assess_memory(mem(15.0, 7.4, 16.0, 10.0));
        assert_eq!(status, Status::Ok);
        assert!(detail.contains("7.4 of 15.0 GB"), "{detail}");
    }

    #[test]
    fn reads_meminfo() {
        let text = "MemTotal:       16000000 kB\nMemFree:  1 kB\nMemAvailable:    8000000 kB\nSwapTotal:       9000000 kB\nSwapFree:          1000 kB\n";
        let info = parse_meminfo(text).unwrap();
        assert_eq!(info.total, 16_000_000 * 1024);
        assert_eq!(info.swap_free, 1000 * 1024);
        assert!(parse_meminfo("nothing").is_none());
    }

    #[test]
    fn steam_channels() {
        assert_eq!(steam_channel(Some("steamdeck_publicbeta\n")).0, "beta");
        assert_eq!(steam_channel(Some("steamdeck_stable")).0, "stable");
        assert_eq!(steam_channel(None).0, "stable");
        assert_eq!(steam_channel(Some("  ")).0, "stable");
    }

    #[test]
    fn groups_leftovers_by_program() {
        assert_eq!(shm_group("u1000-Shm_8e10b10a"), "u1000-Shm_*");
        assert_eq!(shm_group("inc1000.098c1cebef83c6d1"), "inc1000.*");
        assert_eq!(
            shm_group("u1000-ValveIPCSharedObj-Steam"),
            "u1000-ValveIPCSharedObj-Steam"
        );
    }

    #[test]
    fn keeps_files_that_exist_on_purpose() {
        for name in [
            "libpod_rootless_lock_1000",
            "sem.foo",
            ".com.valvesoftware.Steam.awgizR",
        ] {
            assert!(
                SHM_KEEP_PREFIXES.iter().any(|p| name.starts_with(p)),
                "{name}"
            );
        }
        assert!(
            !SHM_KEEP_PREFIXES
                .iter()
                .any(|p| "u1000-Shm_8e10b10a".starts_with(p))
        );
    }
}
