use crate::dashboard::ServerRequest;
use alvr_gui_common::theme;
use eframe::egui::{Frame, Grid, RichText, ScrollArea, Ui};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const DRIVER_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

pub enum InstallationTabRequest {
    OpenSetupWizard,
    ServerRequest(ServerRequest),
}

pub struct InstallationTab {
    drivers: Vec<PathBuf>,
    last_update_instant: Instant,
    #[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
    system_check: SystemCheck,
}

impl InstallationTab {
    pub fn new() -> Self {
        Self {
            drivers: vec![],
            last_update_instant: Instant::now(),
            #[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
            system_check: SystemCheck::default(),
        }
    }

    pub fn update_drivers(&mut self, list: Vec<PathBuf>) {
        self.drivers = list;
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<InstallationTabRequest> {
        let mut requests = vec![];

        let now = Instant::now();
        if now > self.last_update_instant + DRIVER_UPDATE_INTERVAL {
            requests.push(InstallationTabRequest::ServerRequest(
                ServerRequest::GetDriverList,
            ));

            self.last_update_instant = now;
        }

        // KiraVR: The tab scrolls, so an expanded system check cannot push the
        // rest out of reach.
        ScrollArea::vertical()
            .id_salt("installation_tab_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                #[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
                self.system_check.ui(ui);

                ui.vertical_centered_justified(|ui| {
                    if ui.button("Run setup wizard").clicked() {
                        requests.push(InstallationTabRequest::OpenSetupWizard);
                    }
                    ui.columns(2, |ui| {
                        if ui[0].button("Add firewall rules").clicked() {
                            requests.push(InstallationTabRequest::ServerRequest(
                                ServerRequest::AddFirewallRules,
                            ));
                        }
                        if ui[1].button("Remove firewall rules").clicked() {
                            requests.push(InstallationTabRequest::ServerRequest(
                                ServerRequest::RemoveFirewallRules,
                            ));
                        }
                    });

                    Frame::group(ui.style())
                        .fill(theme::SECTION_BG)
                        .inner_margin(theme::FRAME_PADDING)
                        .show(ui, |ui| {
                            ui.vertical_centered_justified(|ui| {
                                ui.label(RichText::new("Registered drivers").size(18.0));
                            });

                            Grid::new(0).num_columns(2).show(ui, |ui| {
                                for driver_path in &self.drivers {
                                    if ui.button("Remove").clicked() {
                                        requests.push(InstallationTabRequest::ServerRequest(
                                            ServerRequest::UnregisterDriver(driver_path.clone()),
                                        ));
                                    }

                                    ScrollArea::new([true, false])
                                        .auto_shrink([false, false])
                                        .id_salt(driver_path)
                                        .show(ui, |ui| {
                                            ui.label(driver_path.to_string_lossy());
                                        });
                                    ui.end_row();
                                }
                            });

                            if ui.button("Register KiraVR driver").clicked() {
                                requests.push(InstallationTabRequest::ServerRequest(
                                    ServerRequest::RegisterAlvrDriver,
                                ));
                            }
                        });
                });
            });

        requests
    }
}

// KiraVR: The preflight results, with a fix button where one exists. Checks
// and fixes run on a background thread (firewall-cmd and the leftover cleanup
// take up to a few seconds), first when the tab is shown, then when asked.
#[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
struct CheckRun {
    checks: Vec<crate::preflight::Check>,
    message: Option<(bool, String)>,
}

#[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
#[derive(Default)]
struct SystemCheck {
    checks: Vec<crate::preflight::Check>,
    message: Option<(bool, String)>,
    running: Option<std::sync::mpsc::Receiver<CheckRun>>,
    started: bool,
}

#[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
impl SystemCheck {
    // Optionally applies a fix, then checks everything again.
    fn start(&mut self, fix: Option<crate::preflight::Fix>, ctx: &eframe::egui::Context) {
        use crate::preflight;

        let (sender, receiver) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let message =
                fix.map(
                    |fix| match preflight::apply_fix(&fix, &crate::get_filesystem_layout()) {
                        Ok(message) => (true, message),
                        Err(e) => (false, format!("{e:#}")),
                    },
                );
            let checks = preflight::run_all();
            sender.send(CheckRun { checks, message }).ok();
            ctx.request_repaint();
        });
        self.running = Some(receiver);
        self.started = true;
    }

    fn ui(&mut self, ui: &mut Ui) {
        use crate::preflight::Status;
        use alvr_gui_common::theme::log_colors;
        use eframe::egui::{Button, CollapsingHeader};

        if !self.started {
            self.start(None, ui.ctx());
        }
        if let Some(receiver) = &self.running
            && let Ok(run) = receiver.try_recv()
        {
            self.checks = run.checks;
            if run.message.is_some() {
                self.message = run.message;
            }
            self.running = None;
        }
        let busy = self.running.is_some();

        let mut fix_to_apply = None;
        let mut recheck = false;
        let checks = &self.checks;

        // Collapsed by default. The header carries the outcome, so it is
        // visible without opening the section.
        let count = |status| checks.iter().filter(|c| c.status == status).count();
        let (problems, warnings) = (count(Status::Error), count(Status::Warning));
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let (summary, summary_color) = if checks.is_empty() {
            ("Checking...".to_owned(), theme::FG)
        } else if problems > 0 {
            let mut text = plural(problems, "problem");
            if warnings > 0 {
                text += &format!(", {}", plural(warnings, "warning"));
            }
            (text, log_colors::ERROR_LIGHT)
        } else if warnings > 0 {
            (plural(warnings, "warning"), log_colors::WARNING_LIGHT)
        } else {
            ("All good".to_owned(), theme::OK_GREEN)
        };

        let header = RichText::new(format!("System Check: {summary}"))
            .size(18.0)
            .color(summary_color);
        CollapsingHeader::new(header)
            .id_salt("system_check_header")
            .default_open(false)
            .show(ui, |ui| {
                Frame::group(ui.style())
                    .fill(theme::SECTION_BG)
                    .inner_margin(theme::FRAME_PADDING)
                    .show(ui, |ui| {
                        let label = if busy { "Checking..." } else { "Check Again" };
                        if ui.add_enabled(!busy, Button::new(label)).clicked() {
                            recheck = true;
                        }
                        ui.add_space(5.0);

                        if checks.is_empty() {
                            ui.label("Checking your system...");
                        }

                        ScrollArea::vertical()
                            .id_salt("system_check_scroll")
                            .max_height(320.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                Grid::new("system_check")
                                    .num_columns(2)
                                    .spacing([10.0, 6.0])
                                    .show(ui, |ui| {
                                        for (index, check) in checks.iter().enumerate() {
                                            let (tag, color) = match check.status {
                                                Status::Ok => ("OK", theme::OK_GREEN),
                                                Status::Info => ("Info", log_colors::INFO_LIGHT),
                                                Status::Warning => {
                                                    ("Warning", log_colors::WARNING_LIGHT)
                                                }
                                                Status::Error => {
                                                    ("Problem", log_colors::ERROR_LIGHT)
                                                }
                                            };
                                            ui.label(RichText::new(tag).color(color).strong());
                                            ui.vertical(|ui| {
                                                ui.label(RichText::new(check.title).strong());
                                                ui.label(&check.detail);
                                                if let Some(fix) = &check.fix
                                                    && ui
                                                        .add_enabled(
                                                            !busy,
                                                            Button::new(fix.label()),
                                                        )
                                                        .clicked()
                                                {
                                                    fix_to_apply = Some(index);
                                                }
                                            });
                                            ui.end_row();
                                        }
                                    });
                            });

                        if let Some((ok, message)) = &self.message {
                            let color = if *ok {
                                theme::OK_GREEN
                            } else {
                                log_colors::ERROR_LIGHT
                            };
                            ui.label(RichText::new(message).color(color));
                        }
                    });
            });

        if let Some(index) = fix_to_apply {
            let fix = self.checks[index].fix.clone();
            self.message = None;
            self.start(fix, ui.ctx());
        } else if recheck {
            self.message = None;
            self.start(None, ui.ctx());
        }
    }
}
