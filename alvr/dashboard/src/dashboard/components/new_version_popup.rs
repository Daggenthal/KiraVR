use crate::dashboard::ServerRequest;
use alvr_gui_common::ModalButton;
use alvr_packets::PathValuePair;
use eframe::egui::{self, Context, OpenUrl, Ui};
use std::{path::PathBuf, process::Command};

pub enum CloseAction {
    Close,
    CloseWithRequest(ServerRequest),
}

pub struct NewVersionPopup {
    version: String,
    message: String,
    launcher_path: Option<PathBuf>,
}

impl NewVersionPopup {
    pub fn new(version: String, message: String) -> Self {
        let mut launcher_path = None;

        let layout = crate::get_filesystem_layout();
        if let Some(path) = layout.launcher_exe()
            && path.exists()
        {
            launcher_path = Some(path);
        }

        Self {
            version,
            message,
            launcher_path,
        }
    }

    pub fn ui(&self, context: &Context, shutdown_alvr_cb: impl Fn()) -> Option<CloseAction> {
        let no_remind_button =
            ModalButton::Custom("Don't remind me again for this version".to_string());

        let result = alvr_gui_common::modal(
            context,
            "New KiraVR version available",
            Some(|ui: &mut Ui| {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);

                    ui.vertical(|ui| {
                        ui.heading(format!("KiraVR v{}", self.version));

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            ui.style_mut().spacing.button_padding = egui::vec2(10.0, 4.0);

                            ui.heading(if self.launcher_path.is_some() {
                                "You can download this version using the launcher:"
                            } else {
                                "Download the new version and start it:"
                            });

                            if let Some(path) = &self.launcher_path {
                                if ui.button("Open Launcher").clicked()
                                    && Command::new(path).spawn().is_ok()
                                {
                                    shutdown_alvr_cb();
                                }
                            } else if ui.button("Download").clicked() {
                                // The release workflow's file names.
                                let version = &self.version;
                                let file = if cfg!(windows) {
                                    format!("KiraVR-{version}-windows-x64.zip")
                                } else {
                                    format!("KiraVR-{version}-x86_64.AppImage")
                                };
                                context.open_url(OpenUrl::new_tab(format!(
                                    "https://github.com/Daggenthal/KiraVR/releases/download/kiravr-v{version}/{file}"
                                )));
                            }
                        });

                        ui.add_space(10.0);

                        ui.label(&self.message);
                        ui.hyperlink_to(
                            "Releases page",
                            "https://github.com/Daggenthal/KiraVR/releases",
                        );
                    });

                    ui.add_space(10.0);
                });
            }),
            &[no_remind_button.clone(), ModalButton::Close],
            Some(490.0),
        );

        if let Some(button) = result {
            if button == no_remind_button {
                Some(CloseAction::CloseWithRequest(
                    ServerRequest::SetSessionValues(vec![PathValuePair {
                        path: alvr_packets::parse_path(
                            "session_settings.extra.new_version_popup.content.hide_while_version",
                        )
                        .unwrap(),
                        value: serde_json::Value::String(self.version.clone()),
                    }]),
                ))
            } else {
                Some(CloseAction::Close)
            }
        } else {
            None
        }
    }
}
