use alvr_common::{
    ToAny,
    anyhow::{Result, bail},
    error,
    parking_lot::Mutex,
};
use flume::TryRecvError;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::LazyLock,
};

// KiraVR: What each headset announced, kept for the handshake, which only gets
// a protocol hash and needs the readable versions to explain a mismatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientAnnounce {
    pub protocol: String,
    // Full version and app name, only sent by KiraVR clients.
    pub version: Option<String>,
    pub is_kiravr: bool,
}

pub static CLIENT_ANNOUNCES: LazyLock<Mutex<HashMap<String, ClientAnnounce>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// Mismatches already reported, so a headset that announces every second does
// not flood the log. Reported again if the headset's version changes.
static REPORTED_MISMATCHES: LazyLock<Mutex<HashSet<(String, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub struct Mismatch {
    // For the dashboard and the log.
    pub pc_message: String,
    // Shown on the headset, written from its point of view.
    pub headset_message: String,
}

// Explains a version mismatch with the headset `hostname`, using what it
// announced. None when the protocols match.
pub fn describe_mismatch(hostname: &str, announce: Option<&ClientAnnounce>) -> Option<Mismatch> {
    let server_protocol = alvr_common::protocol_id();
    let server_version = alvr_common::ALVR_VERSION.to_string();

    let Some(announce) = announce else {
        // Hash mismatch without an announce (manual IP): versions unknown.
        return Some(Mismatch {
            pc_message: format!(
                "Headset {hostname} runs an incompatible app version. KiraVR on the PC is v{server_version}. \
                Install the KiraVR app with the same version on the headset."
            ),
            headset_message: format!(
                "This app does not match KiraVR v{server_version} on the PC.\nInstall the KiraVR app with the same version."
            ),
        });
    };
    if announce.protocol == server_protocol {
        return None;
    }

    let app = if announce.is_kiravr { "KiraVR" } else { "ALVR" };
    let client_version = announce
        .version
        .clone()
        .unwrap_or_else(|| announce.protocol.clone());

    if !announce.is_kiravr {
        return Some(Mismatch {
            pc_message: format!(
                "Headset {hostname} runs ALVR (protocol {}), not KiraVR v{server_version}. Install the KiraVR app \
                on the headset and open that one instead.",
                announce.protocol
            ),
            headset_message: format!(
                "This is the ALVR app. The PC runs KiraVR v{server_version}.\nOpen the KiraVR app instead."
            ),
        });
    }

    let (pc_message, headset_message) = match alvr_common::compare_protocols(
        &announce.protocol,
        &server_protocol,
    ) {
        Some(Ordering::Less) => (
            format!(
                "Headset {hostname} has an older {app} app (v{client_version}) than the PC (v{server_version}). \
                    Update the app on the headset."
            ),
            format!(
                "This {app} app (v{client_version}) is older than KiraVR on the PC (v{server_version}).\n\
                    Update the app on the headset."
            ),
        ),
        Some(Ordering::Greater) => (
            format!(
                "Headset {hostname} has a newer {app} app (v{client_version}) than the PC (v{server_version}). \
                    Update KiraVR on the PC, or install the matching app version."
            ),
            format!(
                "This {app} app (v{client_version}) is newer than KiraVR on the PC (v{server_version}).\n\
                    Update KiraVR on the PC."
            ),
        ),
        _ => (
            format!(
                "Headset {hostname} runs {app} v{client_version}, the PC runs v{server_version}. They need the \
                    same version."
            ),
            format!(
                "This {app} app (v{client_version}) does not match KiraVR on the PC (v{server_version}).\n\
                    Install matching versions."
            ),
        ),
    };

    Some(Mismatch {
        pc_message,
        headset_message,
    })
}

// Logs a mismatch once per headset and version. Errors show up as dashboard
// notifications.
pub fn report_mismatch(hostname: &str, announce: Option<&ClientAnnounce>, mismatch: &Mismatch) {
    let key = (
        hostname.to_owned(),
        announce.map(|a| a.protocol.clone()).unwrap_or_default(),
    );
    if REPORTED_MISMATCHES.lock().insert(key) {
        error!("{}", mismatch.pc_message);
    }
}

pub struct WelcomeSocket {
    mdns_receiver: Receiver<ServiceEvent>,
}

impl WelcomeSocket {
    pub fn new() -> Result<Self> {
        let mdns_receiver = ServiceDaemon::new()?.browse(alvr_sockets::MDNS_SERVICE_TYPE)?;

        Ok(Self { mdns_receiver })
    }

    // Returns: client IP, client hostname
    pub fn recv_all(&self) -> Result<HashMap<String, IpAddr>> {
        let mut clients = HashMap::new();

        loop {
            match self.mdns_receiver.try_recv() {
                Ok(event) => {
                    if let ServiceEvent::ServiceResolved(info) = event {
                        let hostname = info
                            .get_property_val_str(alvr_sockets::MDNS_DEVICE_ID_KEY)
                            .unwrap_or_else(|| info.get_hostname());
                        let address = info.get_addresses().iter().next().to_any()?;

                        // KiraVR: Skip an announce without a protocol (some
                        // other device on the network) instead of aborting
                        // the whole pass, which dropped real headsets too.
                        let Some(protocol) =
                            info.get_property_val_str(alvr_sockets::MDNS_PROTOCOL_KEY)
                        else {
                            continue;
                        };
                        let announce = ClientAnnounce {
                            protocol: protocol.to_owned(),
                            version: info
                                .get_property_val_str(alvr_sockets::MDNS_VERSION_KEY)
                                .map(str::to_owned),
                            is_kiravr: info.get_property_val_str(alvr_sockets::MDNS_APP_KEY)
                                == Some(alvr_sockets::MDNS_APP_KIRAVR),
                        };

                        if let Some(mismatch) = describe_mismatch(hostname, Some(&announce)) {
                            report_mismatch(hostname, Some(&announce), &mismatch);
                        }

                        CLIENT_ANNOUNCES
                            .lock()
                            .insert(hostname.to_owned(), announce);
                        clients.insert(hostname.into(), address.to_ip_addr());
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(e) => bail!(e),
            }
        }

        Ok(clients)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce(protocol: &str, version: Option<&str>, is_kiravr: bool) -> ClientAnnounce {
        ClientAnnounce {
            protocol: protocol.into(),
            version: version.map(Into::into),
            is_kiravr,
        }
    }

    #[test]
    fn matching_protocol_is_fine() {
        let ours = announce(&alvr_common::protocol_id(), None, true);
        assert!(describe_mismatch("h", Some(&ours)).is_none());
    }

    #[test]
    fn says_which_side_is_older() {
        let old =
            describe_mismatch("h", Some(&announce("1-dev1", Some("1.0.0-dev1"), true))).unwrap();
        assert!(
            old.headset_message.contains("older"),
            "{}",
            old.headset_message
        );
        assert!(old.headset_message.contains("v1.0.0-dev1"));

        let new = describe_mismatch("h", Some(&announce("999", None, true))).unwrap();
        assert!(
            new.headset_message.contains("newer"),
            "{}",
            new.headset_message
        );
    }

    #[test]
    fn spots_the_alvr_app() {
        let alvr = describe_mismatch("h", Some(&announce("20", None, false))).unwrap();
        assert!(
            alvr.headset_message.contains("ALVR app"),
            "{}",
            alvr.headset_message
        );
        assert!(alvr.pc_message.contains("not KiraVR"));
    }
}
