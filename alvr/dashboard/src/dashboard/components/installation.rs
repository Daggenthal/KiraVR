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

        requests
    }
}

// KiraVR: The preflight results, with a fix button where one exists. Checks
// run when the tab is first shown and when asked, not every frame, since some
// of them call firewall-cmd.
#[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
#[derive(Default)]
struct SystemCheck {
    checks: Option<Vec<crate::preflight::Check>>,
    message: Option<(bool, String)>,
}

#[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
impl SystemCheck {
    fn ui(&mut self, ui: &mut Ui) {
        use crate::preflight::{self, Status};
        use alvr_gui_common::theme::log_colors;

        let checks = self.checks.get_or_insert_with(preflight::run_all);
        let mut fix_to_apply = None;
        let mut recheck = false;

        Frame::group(ui.style())
            .fill(theme::SECTION_BG)
            .inner_margin(theme::FRAME_PADDING)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("System check").size(18.0));
                    if ui.button("Check again").clicked() {
                        recheck = true;
                    }
                });
                ui.add_space(5.0);

                Grid::new("system_check")
                    .num_columns(2)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        for (index, check) in checks.iter().enumerate() {
                            let (tag, color) = match check.status {
                                Status::Ok => ("OK", theme::OK_GREEN),
                                Status::Info => ("Info", log_colors::INFO_LIGHT),
                                Status::Warning => ("Warning", log_colors::WARNING_LIGHT),
                                Status::Error => ("Problem", log_colors::ERROR_LIGHT),
                            };
                            ui.label(RichText::new(tag).color(color).strong());
                            ui.vertical(|ui| {
                                ui.label(RichText::new(check.title).strong());
                                ui.label(&check.detail);
                                if let Some(fix) = &check.fix
                                    && ui.button(fix.label()).clicked()
                                {
                                    fix_to_apply = Some(index);
                                }
                            });
                            ui.end_row();
                        }
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

        if let Some(index) = fix_to_apply
            && let Some(fix) = checks[index].fix.clone()
        {
            self.message = Some(
                match preflight::apply_fix(&fix, &crate::get_filesystem_layout()) {
                    Ok(message) => (true, message),
                    Err(e) => (false, format!("{e:#}")),
                },
            );
            recheck = true;
        }
        if recheck {
            self.checks = Some(preflight::run_all());
        }
    }
}
