use alvr_common::{error, info};
use app_dirs2::{AppDataType, AppInfo};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

fn config_path() -> PathBuf {
    app_dirs2::app_root(
        AppDataType::UserConfig,
        &AppInfo {
            name: "ALVR Client",
            author: "ALVR",
        },
    )
    .unwrap()
    .join("session.json")
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub hostname: String,
    pub protocol_id: String,
    // KiraVR: Where the performance overlay was moved to in VR. Defaulted so
    // config files from before this field still parse (a parse failure resets
    // the whole config, hostname included, which means trusting again).
    #[serde(default)]
    pub overlay_poses: OverlayPoses,
}

// Panel poses relative to their anchor: The wrist's controller grip, or the
// head for In View. None means the built-in placement.
#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug)]
pub struct OverlayPoses {
    pub left_wrist: Option<alvr_common::Pose>,
    pub right_wrist: Option<alvr_common::Pose>,
    pub in_view: Option<alvr_common::Pose>,
}

impl Default for Config {
    fn default() -> Self {
        let mut rng = rand::rng();

        Self {
            hostname: format!(
                "{}{}{}{}.client.local.",
                rng.random_range(0..10),
                rng.random_range(0..10),
                rng.random_range(0..10),
                rng.random_range(0..10),
            ),
            protocol_id: alvr_common::protocol_id(),
            overlay_poses: OverlayPoses::default(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        if let Ok(config_string) = fs::read_to_string(config_path()) {
            // Failure happens if the Config signature changed between versions.
            // todo: recover data from mismatched Config signature. low priority
            if let Ok(config) = serde_json::from_str(&config_string) {
                return config;
            } else {
                info!("Error parsing ALVR config. Using default");
            }
        } else {
            info!("Error reading ALVR config. Using default");
        }

        let config = Config::default();
        config.store();

        config
    }

    pub fn store(&self) {
        let config_string = serde_json::to_string(self).unwrap();
        if let Err(e) = fs::write(config_path(), config_string) {
            error!("Error writing ALVR config: {e}")
        }
    }
}
