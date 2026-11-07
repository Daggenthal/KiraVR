// KiraVR: Version history of the AppImage install. Nothing here installs a
// new version: The AppImage's launcher asks (twice) before it replaces the
// installed copy, and keeps the replaced one in versions/. This module lists
// those, switches back to one (also after two confirmations, in the UI), and
// checks GitHub for a newer release when asked.
//
// Layout, shared with packaging/appimage/AppRun:
//   ~/.local/share/kiravr/streamer/             the active copy
//   ~/.local/share/kiravr/versions/<ver>-<id>/  replaced copies, newest 3
//   ~/.local/share/kiravr/declined-build-id     a build the user said no to

use alvr_common::{
    ALVR_VERSION,
    anyhow::{Context, Result, bail},
    semver::Version,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub const KEEP_VERSIONS: usize = 3;
const RELEASES_API: &str = "https://api.github.com/repos/Daggenthal/KiraVR/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/Daggenthal/KiraVR/releases";

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledVersion {
    pub dir: PathBuf,
    pub version: String,
    pub build_id: String,
}

pub fn install_root() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("kiravr"))
}

// True when this dashboard runs from the AppImage's installed copy, the only
// setup with a version history.
pub fn is_appimage_install() -> bool {
    let (Some(root), Ok(exe)) = (install_root(), std::env::current_exe()) else {
        return false;
    };
    exe.starts_with(root.join("streamer"))
}

fn read_version(dir: &Path) -> Option<InstalledVersion> {
    let read = |name: &str| {
        fs::read_to_string(dir.join(name))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    Some(InstalledVersion {
        dir: dir.to_owned(),
        version: read("kiravr-version")?,
        build_id: read("kiravr-build-id")?,
    })
}

pub fn active(root: &Path) -> Option<InstalledVersion> {
    read_version(&root.join("streamer"))
}

// Kept versions, newest first.
pub fn previous(root: &Path) -> Vec<InstalledVersion> {
    let Ok(entries) = fs::read_dir(root.join("versions")) else {
        return vec![];
    };
    let mut found = entries
        .flatten()
        .filter_map(|e| {
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, read_version(&e.path())?))
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(_, v)| v).collect()
}

// Swaps the active copy with a kept one. The active copy goes to versions/,
// so the switch can be undone the same way. SteamVR must be closed: It has
// the driver from the active copy loaded.
pub fn switch_to(root: &Path, target: &InstalledVersion) -> Result<()> {
    if crate::steamvr_launcher::is_steamvr_running() {
        bail!("Close SteamVR first. It is using the current version's driver.");
    }
    let current = active(root).context("No installed KiraVR found")?;
    if !target.dir.starts_with(root.join("versions")) || !target.dir.is_dir() {
        bail!("That version is no longer there.");
    }
    let parked = root
        .join("versions")
        .join(format!("{}-{}", current.version, current.build_id));
    if parked.exists() {
        fs::remove_dir_all(&parked).ok();
    }
    fs::rename(root.join("streamer"), &parked)
        .context("Could not move the current version aside")?;
    if let Err(e) = fs::rename(&target.dir, root.join("streamer")) {
        // Put everything back as it was.
        fs::rename(&parked, root.join("streamer")).ok();
        return Err(e).context("Could not activate the older version");
    }
    // Opening the AppImage of the version we came from should not offer to
    // install it again right away.
    fs::write(root.join("declined-build-id"), &current.build_id).ok();
    touch(&parked);
    prune(root);
    Ok(())
}

// Marks a folder as the most recent, so it is listed first and kept longest.
fn touch(dir: &Path) {
    if let Ok(file) = fs::File::open(dir) {
        file.set_modified(std::time::SystemTime::now()).ok();
    }
}

fn prune(root: &Path) {
    for old in previous(root).into_iter().skip(KEEP_VERSIONS) {
        fs::remove_dir_all(old.dir).ok();
    }
}

// Starts the now active dashboard. The caller exits right after.
pub fn restart_dashboard(root: &Path) -> Result<()> {
    std::process::Command::new(root.join("streamer/bin/alvr_dashboard"))
        .spawn()
        .context("Could not start the dashboard")?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateCheck {
    UpToDate,
    Available { version: String },
}

// One request to GitHub, only when the user asks or has the start check on.
pub fn check_for_update() -> Result<UpdateCheck> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let release = agent
        .get(RELEASES_API)
        .call()
        .context("Could not reach GitHub")?
        .into_body()
        .read_json::<serde_json::Value>()
        .context("GitHub sent an unexpected answer")?;
    let tag = release
        .get("tag_name")
        .and_then(|t| t.as_str())
        .context("No release found yet")?;
    newer_than_this(tag)
}

fn newer_than_this(tag: &str) -> Result<UpdateCheck> {
    let version = Version::parse(tag.trim_start_matches("kiravr-").trim_start_matches('v'))
        .with_context(|| format!("Unknown release tag {tag}"))?;
    Ok(if version > *ALVR_VERSION {
        UpdateCheck::Available {
            version: version.to_string(),
        }
    } else {
        UpdateCheck::UpToDate
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(root: &Path, dir: &str, version: &str, id: &str) -> PathBuf {
        let path = root.join(dir);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("kiravr-version"), version).unwrap();
        fs::write(path.join("kiravr-build-id"), id).unwrap();
        path
    }

    #[test]
    fn switching_back_keeps_both_versions() {
        let tmp = std::env::temp_dir().join(format!("kiravr-versions-{}", std::process::id()));
        fs::remove_dir_all(&tmp).ok();
        make(&tmp, "streamer", "1.1.0", "bbbb");
        make(&tmp, "versions/1.0.0-aaaa", "1.0.0", "aaaa");

        let older = previous(&tmp).remove(0);
        assert_eq!(older.version, "1.0.0");
        switch_to(&tmp, &older).unwrap();

        assert_eq!(active(&tmp).unwrap().version, "1.0.0");
        let kept = previous(&tmp);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0].version, "1.1.0",
            "the newer one can be switched back to"
        );
        assert_eq!(
            fs::read_to_string(tmp.join("declined-build-id")).unwrap(),
            "bbbb"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn only_newer_releases_count() {
        let this = ALVR_VERSION.to_string();
        assert_eq!(
            newer_than_this(&format!("kiravr-v{this}")).unwrap(),
            UpdateCheck::UpToDate
        );
        assert!(matches!(
            newer_than_this("kiravr-v999.0.0").unwrap(),
            UpdateCheck::Available { .. }
        ));
        assert_eq!(newer_than_this("v0.1.0").unwrap(), UpdateCheck::UpToDate);
        assert!(newer_than_this("nonsense").is_err());
    }
}
