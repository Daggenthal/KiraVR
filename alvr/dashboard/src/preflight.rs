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
}

impl Fix {
    pub fn label(&self) -> &'static str {
        match self {
            Fix::DisablePrism => "Turn Off Prism Mode",
            Fix::DisableSteamVrHome => "Turn Off SteamVR Home",
            Fix::KillLeftovers(_) => "Close Leftover SteamVR Processes",
            Fix::AddFirewallRules => "Add Firewall Rules",
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

const CHECKS: [(&str, fn() -> Check); 10] = [
    ("steamvr_installed", steamvr_installed),
    ("leftover_processes", leftover_processes),
    ("prism", prism),
    ("steamvr_home", steamvr_home),
    ("launch_options", launch_options),
    ("firewall", firewall),
    ("render_node", render_node),
    ("vaapi", vaapi),
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

fn steam_root() -> Option<PathBuf> {
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

fn vaapi() -> Check {
    const TITLE: &str = "Hardware Encoding (VA-API)";
    let Some(display) = libva::Display::open() else {
        return Check::new(
            TITLE,
            Status::Warning,
            "VA-API is not available. AMD and Intel GPUs need it for hardware encoding (NVIDIA uses NVENC \
            instead, so ignore this there).",
        );
    };
    let encoders = [
        (libva::VAProfile::VAProfileH264Main, "H.264"),
        (libva::VAProfile::VAProfileHEVCMain, "HEVC"),
        (libva::VAProfile::VAProfileAV1Profile0, "AV1"),
    ]
    .into_iter()
    .filter(|(profile, _)| {
        display.query_config_entrypoints(*profile).is_ok_and(|e| {
            e.contains(&libva::VAEntrypoint::VAEntrypointEncSlice)
                || e.contains(&libva::VAEntrypoint::VAEntrypointEncSliceLP)
        })
    })
    .map(|(_, name)| name)
    .collect::<Vec<_>>();
    let vendor = display.query_vendor_string().unwrap_or_default();
    if encoders.is_empty() {
        Check::new(
            TITLE,
            Status::Warning,
            format!(
                "VA-API ({vendor}) offers no video encoders. KiraVR would fall back to slow software encoding."
            ),
        )
    } else {
        Check::new(
            TITLE,
            Status::Ok,
            format!("{} via {vendor}.", encoders.join(", ")),
        )
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
}
