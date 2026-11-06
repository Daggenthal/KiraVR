use alvr_common::anyhow::{Result, bail};
use mdns_sd::{ServiceDaemon, ServiceInfo};

pub struct AnnouncerSocket {
    hostname: String,
    daemon: ServiceDaemon,
}

impl AnnouncerSocket {
    pub fn new(hostname: &str) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;

        Ok(Self {
            daemon,
            hostname: hostname.to_owned(),
        })
    }

    pub fn announce(&self) -> Result<()> {
        let local_ip = alvr_system_info::local_ip();
        if local_ip.is_unspecified() {
            bail!("IP is unspecified");
        }

        self.daemon.register(ServiceInfo::new(
            alvr_sockets::MDNS_SERVICE_TYPE,
            &format!("alvr{}", rand::random::<u16>()),
            &self.hostname,
            local_ip,
            5353,
            &[
                (
                    alvr_sockets::MDNS_PROTOCOL_KEY,
                    alvr_common::protocol_id().as_str(),
                ),
                (
                    alvr_sockets::MDNS_VERSION_KEY,
                    alvr_common::ALVR_VERSION.to_string().as_str(),
                ),
                (alvr_sockets::MDNS_APP_KEY, alvr_sockets::MDNS_APP_KIRAVR),
            ][..],
        )?)?;

        Ok(())
    }
}
