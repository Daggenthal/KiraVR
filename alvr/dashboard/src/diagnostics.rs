// KiraVR: Diagnostics for bug reports. Collects what is needed to understand
// a problem (versions, hardware, SteamVR, headsets, changed settings, recent
// log lines) and removes personal details before anyone sees the text: the
// home folder path, user and PC names, IP and MAC addresses, email addresses,
// Steam IDs, headset IDs and serial numbers.
//
// Collecting runs adb and the system check, so it takes a few seconds. Call
// it from a background thread.

use alvr_common::ALVR_VERSION;
use regex::{Captures, Regex};
use std::{
    collections::BTreeMap,
    fmt::Write,
    fs,
    io::{Read, Seek, SeekFrom},
    net::Ipv4Addr,
    path::Path,
    process::Command,
};

// GitHub cuts issue bodies at 65536 characters.
const MAX_REPORT_CHARS: usize = 60_000;
const MAX_LINE_CHARS: usize = 400;
const MAX_CHANGED_SETTINGS: usize = 120;
// Common default account names that are not personal. "deck" is every
// SteamOS user, and replacing it would also mangle "Steam Deck".
const GENERIC_USER_NAMES: &[&str] = &["deck", "steam", "user", "root", "admin", "gamer", "steamos"];
// Default PC names of SteamOS and common distributions. "steamdeck" is also
// SteamOS's variant name, which is useful to keep.
const GENERIC_HOST_NAMES: &[&str] = &[
    "steamdeck",
    "steamos",
    "localhost",
    "fedora",
    "ubuntu",
    "archlinux",
    "bazzite",
    "cachyos",
];

pub struct Options {
    pub include_logs: bool,
}

pub struct Report {
    pub text: String,
    // What was removed and how often, for the summary.
    pub removed: Vec<(&'static str, usize)>,
}

impl Report {
    pub fn removed_summary(&self) -> String {
        if self.removed.is_empty() {
            return "No personal details found.".into();
        }
        let parts = self
            .removed
            .iter()
            .map(|(label, n)| format!("{n} × {label}"))
            .collect::<Vec<_>>();
        format!("Removed: {}.", parts.join(", "))
    }
}

pub fn collect(options: &Options) -> Report {
    let mut secrets = vec![];
    let facts = facts(&mut secrets);

    let mut redactor = Redactor::for_this_pc();
    for (value, label) in &secrets {
        redactor.add_secret(value, label);
    }

    let mut limits = LogLimits::default();
    loop {
        let logs = if options.include_logs {
            logs(&limits)
        } else {
            String::new()
        };
        let mut candidate = Redactor {
            counts: BTreeMap::new(),
            ..redactor.clone()
        };
        let mut text = candidate.redact(&format!("{facts}{logs}"));
        if text.len() <= MAX_REPORT_CHARS || !limits.shrink() {
            let removed = candidate.counts.into_iter().collect::<Vec<_>>();
            let summary = Report {
                text: String::new(),
                removed: removed.clone(),
            }
            .removed_summary();
            writeln!(
                text,
                "\n<sub>Collected by KiraVR {}. {summary}</sub>",
                *ALVR_VERSION
            )
            .ok();
            return Report { text, removed };
        }
    }
}

////////////
// Redaction

#[derive(Clone)]
pub struct Redactor {
    // Exact text to replace, longest first.
    literals: Vec<(Regex, &'static str, &'static str)>,
    counts: BTreeMap<&'static str, usize>,
}

impl Redactor {
    pub fn new(home: Option<&str>, user: Option<&str>, host: Option<&str>) -> Self {
        let mut this = Self {
            literals: vec![],
            counts: BTreeMap::new(),
        };
        if let Some(home) = home.filter(|h| h.len() > 1) {
            let pattern = Regex::new(&regex::escape(home.trim_end_matches('/'))).unwrap();
            this.literals.push((pattern, "~", "home folder path"));
        }
        // The PC name often contains the user name ("kira-desktop"), so it goes first.
        if let Some(host) = host
            .filter(|h| h.len() >= 3 && !GENERIC_HOST_NAMES.contains(&h.to_lowercase().as_str()))
        {
            this.add_word(host, "<pc-name>", "PC name");
        }
        if let Some(user) = user
            .filter(|u| u.len() >= 3 && !GENERIC_USER_NAMES.contains(&u.to_lowercase().as_str()))
        {
            this.add_word(user, "<user>", "user name");
        }
        this
    }

    fn for_this_pc() -> Self {
        let home = std::env::var("HOME").ok();
        let user = std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("USERNAME").ok());
        let host = sysinfo::System::host_name();
        Self::new(home.as_deref(), user.as_deref(), host.as_deref())
    }

    fn add_word(&mut self, word: &str, replacement: &'static str, label: &'static str) {
        let pattern = Regex::new(&format!(r"(?i)\b{}\b", regex::escape(word))).unwrap();
        self.literals.push((pattern, replacement, label));
    }

    pub fn add_secret(&mut self, value: &str, label: &'static str) {
        if value.len() >= 4 {
            let pattern = Regex::new(&regex::escape(value)).unwrap();
            self.literals.push((pattern, "<headset-serial>", label));
        }
    }

    pub fn redact(&mut self, text: &str) -> String {
        let (email, others) = patterns().split_at(1);
        // Email addresses first: Once a user name inside one is replaced,
        // the rest of the address would no longer be recognized.
        let mut text = self.apply_patterns(text.to_owned(), email);
        for (pattern, replacement, label) in &self.literals {
            let n = pattern.find_iter(&text).count();
            if n > 0 {
                text = pattern.replace_all(&text, *replacement).into_owned();
                *self.counts.entry(label).or_default() += n;
            }
        }
        self.apply_patterns(text, others)
    }

    fn apply_patterns(
        &mut self,
        mut text: String,
        patterns: &[(Regex, &'static str, Replace)],
    ) -> String {
        for (pattern, label, replace) in patterns {
            let mut n = 0;
            text = pattern
                .replace_all(&text, |caps: &Captures| match replace(&caps[0]) {
                    Some(replacement) => {
                        n += 1;
                        replacement
                    }
                    None => caps[0].to_owned(),
                })
                .into_owned();
            if n > 0 {
                *self.counts.entry(label).or_default() += n;
            }
        }
        text
    }
}

type Replace = fn(&str) -> Option<String>;

fn patterns() -> &'static [(Regex, &'static str, Replace)] {
    static PATTERNS: std::sync::OnceLock<Vec<(Regex, &'static str, Replace)>> =
        std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        let fixed = |s: &'static str| -> Replace {
            match s {
                "<email>" => |_| Some("<email>".into()),
                "<mac>" => |_| Some("<mac>".into()),
                "<ipv6>" => |_| Some("<ipv6>".into()),
                "<steam-id>" => |_| Some("<steam-id>".into()),
                "userdata/<id>" => |_| Some("userdata/<id>".into()),
                _ => |_| Some("<headset-id>".into()),
            }
        };
        vec![
            (
                Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
                "email address",
                fixed("<email>"),
            ),
            (
                Regex::new(r"\b[0-9A-Fa-f]{2}(?::[0-9A-Fa-f]{2}){5}\b").unwrap(),
                "MAC address",
                fixed("<mac>"),
            ),
            // Full IPv6 (4 or more groups, so clock times like 18:26:42 stay)
            // and the short form with "::" (fe80::1a2b).
            (
                Regex::new(
                    r"(?i)\b(?:[0-9a-f]{1,4}:){3,7}[0-9a-f]{1,4}\b|\b(?:[0-9a-f]{1,4}:){1,6}:[0-9a-f]{1,4}(?::[0-9a-f]{1,4}){0,5}\b",
                )
                .unwrap(),
                "IP address",
                fixed("<ipv6>"),
            ),
            (
                Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap(),
                "IP address",
                redact_ipv4,
            ),
            (
                Regex::new(r"\b7656119\d{10}\b").unwrap(),
                "Steam ID",
                fixed("<steam-id>"),
            ),
            (
                Regex::new(r"userdata/\d+").unwrap(),
                "Steam ID",
                fixed("userdata/<id>"),
            ),
            // The random name each headset app picks ("1234.client.local.").
            (
                Regex::new(r"\b\d{4}\.client\.local\b\.?").unwrap(),
                "headset ID",
                fixed("<headset-id>"),
            ),
        ]
    })
}

// Keeps addresses that say nothing about the user (localhost, any, broadcast),
// and tells LAN addresses apart from public ones, which helps with network
// problems without giving either away.
fn redact_ipv4(text: &str) -> Option<String> {
    let ip = text.parse::<Ipv4Addr>().ok()?;
    if ip.is_loopback() || ip.is_unspecified() || ip.is_broadcast() {
        return None;
    }
    Some(if ip.is_private() || ip.is_link_local() {
        "<lan-ip>".into()
    } else {
        "<ip>".into()
    })
}

////////
// Facts

fn facts(secrets: &mut Vec<(String, &'static str)>) -> String {
    let mut out = String::new();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M");
    writeln!(out, "## KiraVR Diagnostics\n").ok();
    writeln!(
        out,
        "Collected {now}. Personal details are removed, see the note at the end.\n"
    )
    .ok();

    section(&mut out, "KiraVR", kiravr_lines());
    section(&mut out, "PC", pc_lines());
    section(&mut out, "Steam and SteamVR", steamvr_lines());
    section(&mut out, "Headsets", headset_lines(secrets));
    #[cfg(target_os = "linux")]
    section(&mut out, "Network", network_lines());

    #[cfg(target_os = "linux")]
    {
        let checks = crate::preflight::run_all();
        let count = |s| checks.iter().filter(|c| c.status == s).count();
        use crate::preflight::Status;
        let summary = format!(
            "System Check ({} OK, {} info, {} warnings, {} problems)",
            count(Status::Ok),
            count(Status::Info),
            count(Status::Warning),
            count(Status::Error)
        );
        let lines = checks
            .iter()
            .map(|c| {
                let tag = match c.status {
                    Status::Ok => "OK",
                    Status::Info => "Info",
                    Status::Warning => "Warning",
                    Status::Error => "Problem",
                };
                format!("- **{tag}** {}: {}", c.title, c.detail)
            })
            .collect::<Vec<_>>()
            .join("\n");
        details(&mut out, &summary, &lines);
    }

    match changed_settings() {
        Some(changed) if changed.is_empty() => {
            writeln!(out, "**Settings:** All at their defaults.\n").ok();
        }
        Some(changed) => {
            let title = format!("Settings changed from the defaults ({})", changed.len());
            let mut body = changed
                .iter()
                .take(MAX_CHANGED_SETTINGS)
                .map(|line| format!("- `{line}`"))
                .collect::<Vec<_>>()
                .join("\n");
            if changed.len() > MAX_CHANGED_SETTINGS {
                write!(
                    body,
                    "\n- ...and {} more",
                    changed.len() - MAX_CHANGED_SETTINGS
                )
                .ok();
            }
            details(&mut out, &title, &body);
        }
        None => {
            writeln!(out, "**Settings:** Could not read the settings file.\n").ok();
        }
    }
    out
}

fn section(out: &mut String, title: &str, lines: Vec<(String, String)>) {
    writeln!(out, "### {title}\n").ok();
    for (label, value) in lines {
        writeln!(out, "- **{label}:** {value}").ok();
    }
    writeln!(out).ok();
}

fn details(out: &mut String, summary: &str, body: &str) {
    writeln!(
        out,
        "<details>\n<summary>{summary}</summary>\n\n{body}\n\n</details>\n"
    )
    .ok();
}

fn kiravr_lines() -> Vec<(String, String)> {
    let channel = if alvr_common::is_stable() {
        "release"
    } else {
        "pre-release"
    };
    let exe = std::env::current_exe().unwrap_or_default();
    let exe = exe.to_string_lossy();
    let install = if exe.contains("/.local/share/kiravr/streamer/") {
        "AppImage"
    } else if exe.contains("/build/alvr_streamer_") {
        "Built from source"
    } else {
        "Other"
    };
    let apk = if crate::get_filesystem_layout().client_apk().exists() {
        "Bundled"
    } else {
        "Not bundled"
    };
    vec![
        ("Version".into(), format!("{} ({channel})", *ALVR_VERSION)),
        ("Installed as".into(), install.into()),
        ("Headset app".into(), apk.into()),
    ]
}

fn pc_lines() -> Vec<(String, String)> {
    use sysinfo::System;

    let mut lines = vec![];
    // Only Linux appends to it.
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut os = System::long_os_version().unwrap_or_else(|| "Unknown".into());
    #[cfg(target_os = "linux")]
    if let Ok(release) = fs::read_to_string("/etc/os-release") {
        // SteamOS puts its channel and build here.
        let field = |key: &str| {
            release
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .map(|v| v.trim_matches('"').to_owned())
        };
        for key in ["VARIANT_ID", "BUILD_ID"] {
            if let Some(value) = field(key).filter(|v| !v.is_empty()) {
                write!(os, ", {key} {value}").ok();
            }
        }
    }
    lines.push(("OS".into(), os));
    lines.push((
        "Kernel".into(),
        System::kernel_version().unwrap_or_else(|| "Unknown".into()),
    ));
    #[cfg(target_os = "linux")]
    {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "Unknown".into());
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
        lines.push(("Desktop".into(), format!("{desktop} ({session})")));
    }

    let mut system = System::new();
    system.refresh_cpu_all();
    system.refresh_memory();
    let cpu = system
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_owned())
        .unwrap_or_else(|| "Unknown".into());
    lines.push((
        "CPU".into(),
        format!("{cpu} ({} threads)", system.cpus().len()),
    ));
    lines.push((
        "RAM".into(),
        format!("{:.1} GB", system.total_memory() as f64 / 1e9),
    ));
    #[cfg(target_os = "linux")]
    for gpu in linux_gpus() {
        lines.push(("GPU".into(), gpu));
    }
    lines
}

#[cfg(target_os = "linux")]
fn linux_gpus() -> Vec<String> {
    let mut gpus = vec![];
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return gpus;
    };
    let mut cards = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("card") && !n.contains('-'))
        })
        .collect::<Vec<_>>();
    cards.sort();
    for card in cards {
        let device = card.join("device");
        let read = |name: &str| {
            fs::read_to_string(device.join(name))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default()
        };
        let vendor_id = read("vendor");
        let vendor = match vendor_id.as_str() {
            "0x1002" => "AMD",
            "0x10de" => "NVIDIA",
            "0x8086" => "Intel",
            _ => "Other",
        };
        let driver = fs::read_link(device.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "no driver".into());
        let product = read("product_name");
        let mut text = format!("{vendor} {} ({driver})", read("device"));
        if !product.is_empty() {
            write!(text, ", {product}").ok();
        }
        gpus.push(text);
    }
    gpus
}

fn steamvr_lines() -> Vec<(String, String)> {
    let mut lines = vec![];
    match alvr_server_io::steamvr_root_dir() {
        Ok(root) => {
            // .../steamapps/common/SteamVR -> .../steamapps/appmanifest_250820.acf
            let manifest = root
                .ancestors()
                .nth(2)
                .map(|steamapps| steamapps.join("appmanifest_250820.acf"))
                .and_then(|path| fs::read_to_string(path).ok())
                .unwrap_or_default();
            let build = acf_value(&manifest, "buildid").unwrap_or_else(|| "unknown".into());
            let branch = acf_value(&manifest, "BetaKey")
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| "public".into());
            lines.push(("SteamVR".into(), format!("Build {build}, branch {branch}")));
        }
        Err(_) => lines.push(("SteamVR".into(), "Not found".into())),
    }
    match alvr_server_io::get_registered_drivers() {
        Ok(drivers) if drivers.is_empty() => {
            lines.push(("Registered drivers".into(), "None".into()));
        }
        Ok(drivers) => {
            let list = drivers
                .iter()
                .map(|d| format!("`{}`", d.display()))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(("Registered drivers".into(), list));
        }
        Err(_) => lines.push(("Registered drivers".into(), "Could not read".into())),
    }
    lines
}

fn acf_value(acf: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    acf.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(&needle)?;
        Some(rest.trim().trim_matches('"').to_owned())
    })
}

fn headset_lines(secrets: &mut Vec<(String, &'static str)>) -> Vec<(String, String)> {
    let mut lines = vec![];

    // Headsets the streamer knows, without their addresses.
    let layout = crate::get_filesystem_layout();
    if let Some(session) = fs::read_to_string(layout.session())
        .ok()
        .and_then(|text| serde_json::from_str::<alvr_session::SessionConfig>(&text).ok())
    {
        let mut known = session.client_connections.values().collect::<Vec<_>>();
        known.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        for client in known {
            let manual = if client.manual_ips.is_empty() {
                String::new()
            } else {
                format!(", {} manual IP(s)", client.manual_ips.len())
            };
            lines.push((
                "Known headset".into(),
                format!(
                    "{}, {}, {:?}{manual}",
                    client.display_name,
                    if client.trusted {
                        "trusted"
                    } else {
                        "not trusted"
                    },
                    client.connection_state
                ),
            ));
        }
    }
    if lines.is_empty() {
        lines.push(("Known headsets".into(), "None".into()));
    }

    // Headsets on USB. Only uses adb if it is already there.
    let Some(adb) = alvr_adb::commands::get_adb_path(&layout) else {
        lines.push(("USB".into(), "adb not installed".into()));
        return lines;
    };
    let devices = alvr_adb::commands::list_devices(&adb).unwrap_or_default();
    if devices.is_empty() {
        lines.push(("USB".into(), "No headset connected".into()));
    }
    for device in devices {
        use alvr_adb::parse::ConnectionState as C;
        if let Some(serial) = &device.serial {
            secrets.push((serial.clone(), "headset serial number"));
        }
        let (Some(C::Device), Some(serial)) = (&device.connection_state, &device.serial) else {
            lines.push((
                "USB headset".into(),
                format!("{:?}", device.connection_state),
            ));
            continue;
        };
        let shell = |args: &[&str]| {
            Command::new(&adb)
                .args(["-s", serial, "shell"])
                .args(args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default()
        };
        let prop = |name: &str| shell(&["getprop", name]);
        let model = prop("ro.product.model");
        lines.push((
            "USB headset".into(),
            format!(
                "{model}, Android {}, build {}",
                prop("ro.build.version.release"),
                prop("ro.build.display.id")
            ),
        ));
        let battery = shell(&["dumpsys", "battery"]);
        let field = |key: &str| {
            battery.lines().find_map(|l| {
                l.trim()
                    .strip_prefix(&format!("{key}: "))
                    .map(str::to_owned)
            })
        };
        if let Some(level) = field("level") {
            let plugged = field("AC powered").is_some_and(|v| v == "true")
                || field("USB powered").is_some_and(|v| v == "true");
            lines.push((
                "Battery".into(),
                format!("{level}%{}", if plugged { ", plugged in" } else { "" }),
            ));
        }
        let mut apps = vec![];
        for (id, name) in [
            ("dev.daggenthal.kiravr.stable", "KiraVR"),
            ("dev.daggenthal.kiravr.dev", "KiraVR dev"),
            ("dev.daggenthal.kiravr", "KiraVR store"),
            ("alvr.client.stable", "ALVR"),
            ("alvr.client.dev", "ALVR nightly"),
            ("alvr.client", "ALVR store"),
        ] {
            if let Ok(Some(version)) = alvr_adb::commands::installed_version(&adb, serial, id) {
                apps.push(format!("{name} {version}"));
            }
        }
        lines.push((
            "Installed apps".into(),
            if apps.is_empty() {
                "No KiraVR or ALVR app".into()
            } else {
                apps.join(", ")
            },
        ));
    }
    lines
}

#[cfg(target_os = "linux")]
fn network_lines() -> Vec<(String, String)> {
    // The interface with the default route.
    let route = fs::read_to_string("/proc/net/route").unwrap_or_default();
    let Some(interface) = route.lines().skip(1).find_map(|line| {
        let mut columns = line.split_whitespace();
        let name = columns.next()?;
        (columns.next()? == "00000000").then(|| name.to_owned())
    }) else {
        return vec![("Connection".into(), "No default route".into())];
    };
    let sys = Path::new("/sys/class/net").join(&interface);
    let kind = if sys.join("wireless").exists() {
        "Wi-Fi"
    } else {
        "Wired"
    };
    let speed = fs::read_to_string(sys.join("speed"))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|s| *s > 0)
        .map(|s| format!(", {s} Mbit/s"))
        .unwrap_or_default();
    vec![("Connection".into(), format!("{kind} ({interface}{speed})"))]
}

// Settings that differ from a fresh install, as "path = value".
fn changed_settings() -> Option<Vec<String>> {
    let text = fs::read_to_string(crate::get_filesystem_layout().session()).ok()?;
    let session = serde_json::from_str::<alvr_session::SessionConfig>(&text).ok()?;
    let current = serde_json::to_value(session.to_settings()).ok()?;
    let default =
        serde_json::to_value(alvr_session::SessionConfig::default().to_settings()).ok()?;
    let mut changed = vec![];
    diff_json("", &default, &current, &mut changed);
    Some(changed)
}

fn diff_json(
    path: &str,
    default: &serde_json::Value,
    current: &serde_json::Value,
    out: &mut Vec<String>,
) {
    use serde_json::Value;
    if let (Value::Object(default), Value::Object(current)) = (default, current) {
        for (key, value) in current {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            diff_json(&child, default.get(key).unwrap_or(&Value::Null), value, out);
        }
        return;
    }
    if default != current {
        let mut value = current.to_string();
        if value.len() > 120 {
            value = format!("{}...", value.chars().take(117).collect::<String>());
        }
        out.push(format!("{path} = {value}"));
    }
}

///////
// Logs

struct LogLimits {
    crash: usize,
    session: usize,
    steamvr: usize,
}

impl Default for LogLimits {
    fn default() -> Self {
        Self {
            crash: 150,
            session: 250,
            steamvr: 150,
        }
    }
}

impl LogLimits {
    // Halves everything. False when there is nothing left to cut.
    fn shrink(&mut self) -> bool {
        if self.session < 10 {
            return false;
        }
        self.crash /= 2;
        self.session /= 2;
        self.steamvr /= 2;
        true
    }
}

fn logs(limits: &LogLimits) -> String {
    let layout = crate::get_filesystem_layout();
    let mut out = String::from("### Recent Logs\n\n");
    log_block(
        &mut out,
        "KiraVR errors (crash_log.txt)",
        &layout.log_dir.join("crash_log.txt"),
        limits.crash,
        |_| true,
    );
    log_block(
        &mut out,
        "KiraVR session log (needs Log to Disk)",
        &layout.session_log(),
        limits.session,
        |_| true,
    );
    #[cfg(target_os = "linux")]
    if let Some(steam) = crate::preflight::steam_root() {
        log_block(
            &mut out,
            "SteamVR (vrserver.txt, KiraVR and error lines)",
            &steam.join("logs/vrserver.txt"),
            limits.steamvr,
            |line| {
                let lower = line.to_lowercase();
                lower.contains("alvr") || lower.contains("kiravr") || lower.contains("error")
            },
        );
    }
    out
}

fn log_block(
    out: &mut String,
    title: &str,
    path: &Path,
    max_lines: usize,
    keep: impl Fn(&str) -> bool,
) {
    let Some(text) = read_tail(path, 1024 * 1024) else {
        writeln!(out, "- {title}: Not found\n").ok();
        return;
    };
    let lines = text
        .lines()
        .filter(|l| !l.trim().is_empty() && keep(l))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        writeln!(out, "- {title}: Empty\n").ok();
        return;
    }
    let shown = &lines[lines.len().saturating_sub(max_lines)..];
    let body = shown
        .iter()
        .map(|line| {
            let line = line.replace("```", "'''");
            if line.chars().count() > MAX_LINE_CHARS {
                format!(
                    "{}...",
                    line.chars().take(MAX_LINE_CHARS).collect::<String>()
                )
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    details(
        out,
        &format!("{title}, last {} lines", shown.len()),
        &format!("```text\n{body}\n```"),
    );
}

// The end of a file, so a huge log does not have to be read whole.
fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes)).ok()?;
    }
    let mut bytes = vec![];
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // Drop the first, probably cut off line.
    Some(if len > max_bytes {
        text.split_once('\n')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or(text)
    } else {
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(Some("/home/kira"), Some("kira"), Some("kira-desktop"))
    }

    #[test]
    fn removes_paths_names_and_addresses() {
        let mut r = redactor();
        r.add_secret("2G97C5ZH5900S7", "headset serial number");
        let text = r.redact(
            "Loaded /home/kira/.local/share/kiravr/streamer as kira on kira-desktop\n\
             Headset 2G97C5ZH5900S7 at 192.168.1.42, router 8.8.8.8, mac aa:bb:cc:dd:ee:ff\n\
             fe80::1a2b:3c4d and 2001:db8:85a3:0:0:8a2e:370:7334, mail kira@example.com\n\
             Steam /home/kira/.steam/userdata/123456/config, id 76561198000000001\n\
             Client 4821.client.local. connected",
        );
        assert!(!text.contains("kira-desktop"), "{text}");
        assert!(!text.contains("/home/kira"), "{text}");
        assert!(text.contains("~/.local/share/kiravr/streamer"), "{text}");
        assert!(text.contains("as <user> on <pc-name>"), "{text}");
        assert!(text.contains("<headset-serial>"), "{text}");
        assert!(
            text.contains("<lan-ip>") && text.contains("router <ip>"),
            "{text}"
        );
        assert!(text.contains("mac <mac>"), "{text}");
        assert_eq!(text.matches("<ipv6>").count(), 2, "{text}");
        assert!(
            text.contains("<email>") && !text.contains("example.com"),
            "{text}"
        );
        assert!(
            text.contains("userdata/<id>") && text.contains("<steam-id>"),
            "{text}"
        );
        assert!(text.contains("<headset-id>"), "{text}");
        assert_eq!(r.counts["IP address"], 4);
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        let mut r = Redactor::new(Some("/home/deck"), Some("deck"), Some("steamdeck"));
        let input = "[18:26:42.665 INFO alvr_server_core::connection] Steam Deck user, \
                     Mesa 26.1.2, kernel 7.2.7, localhost 127.0.0.1 bound to 0.0.0.0:9943";
        let text = r.redact(input);
        assert_eq!(text, input, "nothing personal in here");
        // The generic SteamOS account name is kept, the path still goes.
        assert_eq!(r.redact("/home/deck/x"), "~/x");
    }

    #[test]
    fn reads_steam_app_manifests() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"250820\"\n\t\"buildid\"\t\t\"20512345\"\n\t\t\"BetaKey\"\t\t\"beta\"\n}";
        assert_eq!(acf_value(acf, "buildid").as_deref(), Some("20512345"));
        assert_eq!(acf_value(acf, "BetaKey").as_deref(), Some("beta"));
        assert_eq!(acf_value(acf, "missing"), None);
    }

    #[test]
    fn lists_only_changed_settings() {
        let default =
            serde_json::json!({"video": {"bitrate": 30, "codec": "Hevc"}, "audio": {"on": true}});
        let current =
            serde_json::json!({"video": {"bitrate": 50, "codec": "Hevc"}, "audio": {"on": true}});
        let mut out = vec![];
        diff_json("", &default, &current, &mut out);
        assert_eq!(out, ["video.bitrate = 50"]);
    }

    #[test]
    fn summary_reads_naturally() {
        let report = Report {
            text: String::new(),
            removed: vec![("IP address", 3), ("user name", 1)],
        };
        assert_eq!(
            report.removed_summary(),
            "Removed: 3 × IP address, 1 × user name."
        );
    }
}
