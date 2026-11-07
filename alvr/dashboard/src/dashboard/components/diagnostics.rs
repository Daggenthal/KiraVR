// KiraVR: Diagnostics tab. Collects a bug report with personal details
// removed, shows it for review (it can be edited before sharing) and copies,
// saves or opens a new GitHub issue.

use alvr_gui_common::theme;
use eframe::egui::{Button, CollapsingHeader, Frame, OpenUrl, RichText, ScrollArea, TextEdit, Ui};
use std::sync::mpsc::{self, Receiver};

const NEW_ISSUE_URL: &str =
    "https://github.com/Daggenthal/KiraVR/issues/new?template=bug_report.yml";

pub struct DiagnosticsTab {
    include_logs: bool,
    text: String,
    removed: Option<String>,
    status: Option<(bool, String)>,
    // The last saved file, for "Open Folder".
    saved: Option<std::path::PathBuf>,
    running: Option<Receiver<crate::diagnostics::Report>>,
    // Mirrors Settings, Extra, Logging, Log to Disk. None until the session
    // is known.
    log_to_disk: Option<bool>,
}

impl DiagnosticsTab {
    pub fn new() -> Self {
        Self {
            include_logs: true,
            text: String::new(),
            removed: None,
            status: None,
            saved: None,
            running: None,
            log_to_disk: None,
        }
    }

    pub fn update_log_to_disk(&mut self, enabled: bool) {
        self.log_to_disk = Some(enabled);
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<crate::dashboard::ServerRequest> {
        let mut requests = vec![];
        if let Some(receiver) = &self.running
            && let Ok(report) = receiver.try_recv()
        {
            self.removed = Some(report.removed_summary());
            self.text = report.text;
            self.running = None;
            self.status = None;
        }
        let busy = self.running.is_some();
        if busy {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }

        ScrollArea::vertical()
            .id_salt("diagnostics_tab_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                Frame::group(ui.style())
                    .fill(theme::SECTION_BG)
                    .inner_margin(theme::FRAME_PADDING)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Diagnostics").size(18.0));
                        ui.add_space(4.0);
                        ui.label(
                            "Collects what is needed to look into a problem: KiraVR and OS \
                             versions, CPU, GPU and RAM, SteamVR, your headsets, the System \
                             Check, settings you changed and recent log lines.",
                        );
                        ui.label(
                            RichText::new(
                                "Your user name, PC name, home folder path, IP and MAC addresses, \
                                 email addresses, Steam IDs and headset serial numbers are removed \
                                 before anything is shown. Nothing is sent anywhere: You decide \
                                 what to copy or save.",
                            )
                            .color(theme::OK_GREEN),
                        );
                        ui.add_space(6.0);

                        // The session log is the most useful part of a report and
                        // off by default, so the switch is right here.
                        if let Some(mut enabled) = self.log_to_disk {
                            let changed = ui
                                .checkbox(&mut enabled, "Log to Disk")
                                .on_hover_text(
                                    "Writes the session log (alvr_session_log.txt in your home folder). Same as Settings, Extra, Logging, Log to Disk.",
                                )
                                .changed();
                            if changed {
                                self.log_to_disk = Some(enabled);
                                requests.push(crate::dashboard::ServerRequest::SetSessionValues(vec![
                                    alvr_packets::PathValuePair {
                                        path: alvr_packets::parse_path(
                                            "session_settings.extra.logging.log_to_disk",
                                        )
                                        .unwrap(),
                                        value: serde_json::Value::Bool(enabled),
                                    },
                                ]));
                            }
                            let note = if enabled {
                                "The session log is on. After turning it on, restart SteamVR and reproduce the problem, then collect."
                            } else {
                                "The session log is off, so reports only contain errors. Turn it on, restart SteamVR and reproduce the problem first."
                            };
                            ui.label(RichText::new(note).weak());
                            ui.add_space(4.0);
                        }

                        ui.horizontal_wrapped(|ui| {
                            ui.checkbox(&mut self.include_logs, "Include Recent Logs");
                            let label = if busy {
                                "Collecting..."
                            } else if self.text.is_empty() {
                                "Collect Diagnostics"
                            } else {
                                "Collect Again"
                            };
                            if ui.add_enabled(!busy, Button::new(label)).clicked() {
                                self.start(ui.ctx());
                            }
                        });
                    });

                if self.text.is_empty() {
                    return;
                }
                ui.add_space(8.0);

                if let Some(removed) = &self.removed {
                    ui.label(RichText::new(format!("Privacy check: {removed}")).strong());
                }
                ui.label(
                    "Read it over before sharing. You can edit the text below, for example to \
                     remove a game name you would rather keep private.",
                );
                ui.add_space(4.0);

                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy to Clipboard").clicked() {
                        ui.ctx().copy_text(self.text.clone());
                        self.status = Some((true, "Copied. Paste it into the Diagnostics field of the issue.".into()));
                    }
                    if ui.button("Save to File").clicked() {
                        match save(&self.text) {
                            Ok((path, message)) => {
                                self.saved = Some(path);
                                self.status = Some((true, message));
                            }
                            Err(message) => self.status = Some((false, message)),
                        }
                    }
                    if let Some(path) = &self.saved
                        && ui
                            .button("Open Folder")
                            .on_hover_text("Shows the saved file, to attach it to an issue or a message.")
                            .clicked()
                        && let Err(e) = show_in_folder(path)
                    {
                        self.status = Some((false, format!("Could not open the folder: {e}")));
                    }
                    if ui.button("Open New Issue").clicked() {
                        ui.ctx().copy_text(self.text.clone());
                        ui.ctx().open_url(OpenUrl::new_tab(NEW_ISSUE_URL));
                        self.status = Some((
                            true,
                            "Copied, and the issue form is opening in your browser. Paste it into the Diagnostics field.".into(),
                        ));
                    }
                    ui.label(
                        RichText::new(format!("{} characters", self.text.chars().count()))
                            .weak(),
                    );
                });
                if let Some((ok, message)) = &self.status {
                    let color = if *ok {
                        theme::OK_GREEN
                    } else {
                        alvr_gui_common::theme::log_colors::ERROR_LIGHT
                    };
                    ui.label(RichText::new(message).color(color));
                }
                ui.add_space(4.0);

                // Open after collecting, so it gets read before sharing. One
                // click folds it away, the buttons above keep working.
                let lines = self.text.lines().count();
                CollapsingHeader::new(RichText::new(format!("Report ({lines} lines)")).strong())
                    .id_salt("diagnostics_report")
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut self.text)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(24),
                        );
                    });
            });
        requests
    }

    fn start(&mut self, ctx: &eframe::egui::Context) {
        let (sender, receiver) = mpsc::channel();
        let options = crate::diagnostics::Options {
            include_logs: self.include_logs,
        };
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            sender.send(crate::diagnostics::collect(&options)).ok();
            ctx.request_repaint();
        });
        self.running = Some(receiver);
    }
}

// Next to the logs, so it is easy to find and attach.
fn save(text: &str) -> Result<(std::path::PathBuf, String), String> {
    let dir = crate::get_filesystem_layout().log_dir;
    let name = format!(
        "kiravr-diagnostics-{}.md",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = dir.join(&name);
    std::fs::write(&path, text).map_err(|e| format!("Could not save {name}: {e}"))?;
    let shown = std::env::var("HOME")
        .ok()
        .and_then(|home| {
            path.strip_prefix(home)
                .ok()
                .map(|p| format!("~/{}", p.display()))
        })
        .unwrap_or_else(|| path.display().to_string());
    Ok((path, format!("Saved as {shown}")))
}

// Opens the file manager with the file selected where that is possible
// (Dolphin, Nautilus and others through the FileManager1 D-Bus service,
// Explorer on Windows), otherwise just its folder.
fn show_in_folder(path: &std::path::Path) -> std::io::Result<()> {
    use std::process::Command;

    #[cfg(windows)]
    {
        Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let uri = format!("file://{}", path.display());
        let selected = Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{uri}"),
                "string:",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !selected {
            let folder = path.parent().unwrap_or(path);
            Command::new("xdg-open").arg(folder).spawn()?;
        }
        Ok(())
    }
}
