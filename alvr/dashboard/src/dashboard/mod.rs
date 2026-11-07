mod components;

use self::components::{
    DevicesTab, LogsTab, NotificationBar, SettingsTab, SetupWizard, SetupWizardRequest,
};
use crate::{
    DataSources,
    dashboard::components::{CloseAction, NewVersionPopup, StatisticsTab},
};
use alvr_common::{
    LogEntry,
    parking_lot::{Condvar, Mutex},
};
use alvr_events::EventType;
use alvr_gui_common::theme;
use alvr_packets::{ClientConnectionsAction, PathValuePair};
use alvr_session::SessionConfig;
use eframe::egui::{
    self, Align, Button, CentralPanel, Direction, Frame, Layout, Margin, Panel, RichText, Ui,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerRequest {
    Log(LogEntry),
    GetSession,
    UpdateSession(Box<SessionConfig>),
    SetSessionValues(Vec<PathValuePair>),
    UpdateClientList {
        hostname: String,
        action: ClientConnectionsAction,
    },
    InsertIdr,
    StartRecording,
    StopRecording,
    AddFirewallRules,
    RemoveFirewallRules,
    GetDriverList,
    RegisterAlvrDriver,
    UnregisterDriver(PathBuf),
    RestartSteamvr,
    ShutdownSteamvr,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Tab {
    Devices,
    Statistics,
    #[cfg(not(target_arch = "wasm32"))]
    LatencyLab,
    Settings,
    #[cfg(not(target_arch = "wasm32"))]
    Installation,
    Logs,
    #[cfg(not(target_arch = "wasm32"))]
    Diagnostics,
    Debug,
    About,
}

pub struct Dashboard {
    data_sources: DataSources,
    just_opened: bool,
    server_restarting: Arc<Mutex<bool>>,
    server_restarting_condvar: Arc<Condvar>,
    selected_tab: Tab,
    tab_labels: BTreeMap<Tab, &'static str>,
    connections_tab: DevicesTab,
    statistics_tab: StatisticsTab,
    settings_tab: SettingsTab,
    #[cfg(not(target_arch = "wasm32"))]
    installation_tab: components::InstallationTab,
    #[cfg(not(target_arch = "wasm32"))]
    diagnostics_tab: components::DiagnosticsTab,
    #[cfg(not(target_arch = "wasm32"))]
    latency_lab: components::LatencyLab,
    logs_tab: LogsTab,
    notification_bar: NotificationBar,
    setup_wizard: SetupWizard,
    new_version_popup: Option<components::NewVersionPopup>,
    // KiraVR: A game that was running when Restart SteamVR was clicked.
    restart_game_confirm: Option<String>,
    setup_wizard_open: bool,
    session: Option<SessionConfig>,
}

impl Dashboard {
    pub fn new(creation_context: &eframe::CreationContext<'_>, data_sources: DataSources) -> Self {
        alvr_gui_common::theme::set_theme(&creation_context.egui_ctx);

        data_sources.request(ServerRequest::GetSession);

        Self {
            data_sources,
            just_opened: true,
            server_restarting: Arc::new(Mutex::new(false)),
            server_restarting_condvar: Arc::new(Condvar::new()),
            selected_tab: Tab::Devices,
            tab_labels: [
                (Tab::Devices, "🔌  Devices"),
                (Tab::Statistics, "📈  Statistics"),
                #[cfg(not(target_arch = "wasm32"))]
                (Tab::LatencyLab, "⏱  Latency Lab"),
                (Tab::Settings, "🔧  Settings"),
                #[cfg(not(target_arch = "wasm32"))]
                (Tab::Installation, "💾  Installation"),
                (Tab::Logs, "📝  Logs"),
                #[cfg(not(target_arch = "wasm32"))]
                (Tab::Diagnostics, "🩺  Diagnostics"),
                (Tab::Debug, "🐞  Debug"),
                (Tab::About, "ℹ  About"),
            ]
            .into_iter()
            .collect(),
            connections_tab: DevicesTab::new(),
            statistics_tab: StatisticsTab::new(),
            settings_tab: SettingsTab::new(),
            #[cfg(not(target_arch = "wasm32"))]
            installation_tab: components::InstallationTab::new(),
            #[cfg(not(target_arch = "wasm32"))]
            diagnostics_tab: components::DiagnosticsTab::new(),
            #[cfg(not(target_arch = "wasm32"))]
            latency_lab: components::LatencyLab::new(),
            logs_tab: LogsTab::new(),
            notification_bar: NotificationBar::new(),
            setup_wizard: SetupWizard::new(),
            setup_wizard_open: false,
            session: None,
            new_version_popup: None,
            restart_game_confirm: None,
        }
    }

    // This call may block
    fn restart_steamvr(&self, requests: &mut Vec<ServerRequest>) {
        requests.push(ServerRequest::RestartSteamvr);

        let mut server_restarting_lock = self.server_restarting.lock();

        if *server_restarting_lock {
            self.server_restarting_condvar
                .wait(&mut server_restarting_lock);
        }

        *server_restarting_lock = true;

        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn({
            let server_restarting = Arc::clone(&self.server_restarting);
            let condvar = Arc::clone(&self.server_restarting_condvar);
            move || {
                // Without holding LAUNCHER: The restart can wait a minute.
                crate::steamvr_launcher::restart_steamvr_blocking();

                *server_restarting.lock() = false;
                condvar.notify_one();
            }
        });
    }
}

impl eframe::App for Dashboard {
    fn ui(&mut self, ui: &mut Ui, _: &mut eframe::Frame) {
        let mut requests = vec![];

        let connected_to_server = self.data_sources.server_connected();

        while let Some(event) = self.data_sources.poll_event() {
            self.logs_tab.push_event(event.inner.clone());

            match event.inner.event_type {
                EventType::Log(log_event) => {
                    self.notification_bar
                        .push_notification(log_event, event.from_dashboard);
                }
                EventType::GraphStatistics(graph_statistics) => {
                    #[cfg(not(target_arch = "wasm32"))]
                    self.latency_lab.add_sample(&graph_statistics);
                    self.statistics_tab
                        .update_graph_statistics(graph_statistics)
                }
                EventType::StatisticsSummary(statistics) => {
                    self.statistics_tab.update_statistics(statistics)
                }
                EventType::Session(session) => {
                    let settings = session.to_settings();

                    self.connections_tab.update_client_list(&session);
                    #[cfg(not(target_arch = "wasm32"))]
                    self.installation_tab
                        .update_auto_check(settings.extra.new_version_popup.as_option().is_some());
                    #[cfg(not(target_arch = "wasm32"))]
                    self.diagnostics_tab
                        .update_log_to_disk(settings.extra.logging.log_to_disk);
                    #[cfg(not(target_arch = "wasm32"))]
                    self.latency_lab.update_settings(&settings);
                    self.settings_tab.update_session(&session.session_settings);
                    self.logs_tab.update_settings(&settings);
                    self.notification_bar.update_settings(&settings);
                    if self.just_opened {
                        if settings.extra.open_setup_wizard {
                            self.setup_wizard_open = true;
                        }

                        self.just_opened = false;
                    }

                    self.session = Some(*session);
                }
                EventType::ServerRequestsSelfRestart => self.restart_steamvr(&mut requests),
                #[cfg(not(target_arch = "wasm32"))]
                EventType::DriversList(list) => self.installation_tab.update_drivers(list),
                EventType::Adb(adb_event) => self
                    .connections_tab
                    .update_adb_download_progress(adb_event.download_progress),
                EventType::NewVersionFound { version, message } => {
                    self.new_version_popup = Some(NewVersionPopup::new(version, message));
                }
                EventType::DebugGroup { .. }
                | EventType::Tracking(_)
                | EventType::Buttons(_)
                | EventType::Haptics(_) => (),
            }
        }

        if *self.server_restarting.lock() {
            CentralPanel::default().show(ui, |ui| {
                // todo: find a way to center both vertically and horizontally
                ui.vertical_centered(|ui| {
                    ui.add_space(100.0);
                    ui.heading(RichText::new("SteamVR is restarting").size(30.0));
                });
            });

            return;
        }

        self.notification_bar.ui(ui);

        if self.setup_wizard_open {
            CentralPanel::default().show(ui, |ui| {
                if let Some(request) = self.setup_wizard.ui(ui) {
                    match request {
                        SetupWizardRequest::ServerRequest(request) => {
                            requests.push(request);
                        }
                        SetupWizardRequest::Close { finished } => {
                            if finished {
                                requests.push(ServerRequest::SetSessionValues(vec![
                                    PathValuePair {
                                        path: alvr_packets::parse_path(
                                            "session_settings.extra.open_setup_wizard",
                                        )
                                        .unwrap(),
                                        value: serde_json::Value::Bool(false),
                                    },
                                ]))
                            }

                            self.setup_wizard_open = false;
                        }
                    }
                }
            });
        } else {
            Panel::left("side_panel")
                .resizable(false)
                .frame(
                    Frame::new()
                        .fill(theme::LIGHTER_BG)
                        .inner_margin(Margin::same(7)),
                )
                .exact_size(160.0)
                .show(ui, |ui| {
                    ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                        ui.add_space(13.0);
                        ui.heading(RichText::new("KiraVR").size(25.0).strong());
                        egui::warn_if_debug_build(ui);
                    });

                    ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                        for (tab, label) in &self.tab_labels {
                            ui.selectable_value(&mut self.selected_tab, *tab, *label);
                        }
                    });

                    #[cfg(not(target_arch = "wasm32"))]
                    ui.with_layout(
                        Layout::bottom_up(Align::Center).with_cross_justify(true),
                        |ui| {
                            ui.add_space(5.0);

                            let busy = crate::steamvr_launcher::is_busy();
                            if let Some(game) = self.restart_game_confirm.clone() {
                                // Bottom-up layout: The buttons come first.
                                if ui.button("Cancel").clicked() {
                                    self.restart_game_confirm = None;
                                }
                                if ui.button("Restart Anyway").clicked() {
                                    self.restart_game_confirm = None;
                                    self.restart_steamvr(&mut requests);
                                }
                                Frame::new().inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{game} is running. Quit it first, restarting SteamVR kills it."
                                        ))
                                        .size(12.0)
                                        .color(theme::log_colors::WARNING_LIGHT),
                                    );
                                });
                            } else if connected_to_server {
                                if ui.add_enabled(!busy, Button::new("Restart SteamVR")).clicked() {
                                    match crate::steamvr_launcher::running_game() {
                                        Some(game) => self.restart_game_confirm = Some(game),
                                        None => self.restart_steamvr(&mut requests),
                                    }
                                }
                            } else if ui
                                .add_enabled(
                                    !busy,
                                    Button::new(if busy { "Starting..." } else { "Launch SteamVR" }),
                                )
                                .clicked()
                            {
                                crate::steamvr_launcher::LAUNCHER.lock().launch_steamvr();
                            }
                            if let Some(status) = crate::steamvr_launcher::status() {
                                Frame::new().inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(status)
                                                .size(12.0)
                                                .color(theme::log_colors::WARNING_LIGHT),
                                        );
                                        // Waits end early once everything is ready, so this
                                        // is the longest it can take.
                                        if let Some((elapsed, total)) =
                                            crate::steamvr_launcher::wait_progress()
                                        {
                                            let left = total.saturating_sub(elapsed).as_secs();
                                            ui.add(
                                                egui::ProgressBar::new(
                                                    elapsed.as_secs_f32() / total.as_secs_f32(),
                                                )
                                                .desired_height(14.0)
                                                .text(
                                                    RichText::new(format!("up to {left} s"))
                                                        .size(11.0),
                                                ),
                                            );
                                        }
                                    });
                                });
                                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                            }

                            ui.horizontal(|ui| {
                                ui.add_space(10.0);
                                ui.label(RichText::new("SteamVR:").size(13.0));
                                ui.with_layout(
                                    Layout::centered_and_justified(Direction::LeftToRight),
                                    |ui| {
                                        ui.label(
                                            if connected_to_server {
                                                RichText::new("Connected").color(theme::OK_GREEN)
                                            } else {
                                                RichText::new("Disconnected").color(theme::KO_RED)
                                            }
                                            .size(13.0),
                                        )
                                    },
                                );
                            })
                        },
                    )
                });

            CentralPanel::default()
                .frame(Frame::new().inner_margin(Margin::same(20)).fill(theme::BG))
                .show(ui, |ui| {
                    ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                        ui.heading(RichText::new(self.tab_labels[&self.selected_tab]).size(25.0));
                        match self.selected_tab {
                            Tab::Devices => {
                                requests.extend(self.connections_tab.ui(ui, connected_to_server));
                            }
                            Tab::Statistics => {
                                if let Some(request) = self.statistics_tab.ui(ui) {
                                    requests.push(request);
                                }
                            }
                            Tab::Settings => {
                                requests.extend(self.settings_tab.ui(ui));
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            Tab::Installation => {
                                for request in self.installation_tab.ui(ui) {
                                    match request {
                                        components::InstallationTabRequest::OpenSetupWizard => {
                                            self.setup_wizard_open = true
                                        }
                                        components::InstallationTabRequest::ServerRequest(
                                            request,
                                        ) => {
                                            requests.push(request);
                                        }
                                    }
                                }
                            }
                            Tab::Logs => self.logs_tab.ui(ui),
                            #[cfg(not(target_arch = "wasm32"))]
                            Tab::Diagnostics => requests.extend(self.diagnostics_tab.ui(ui)),
                            #[cfg(not(target_arch = "wasm32"))]
                            Tab::LatencyLab => requests.extend(self.latency_lab.ui(ui)),
                            Tab::Debug => {
                                if let Some(request) = components::debug_tab_ui(ui) {
                                    requests.push(request);
                                }
                            }
                            Tab::About => components::about_tab_ui(ui),
                        }
                    })
                });
        }

        let shutdown_alvr = || {
            self.data_sources.request(ServerRequest::ShutdownSteamvr);

            crate::steamvr_launcher::LAUNCHER
                .lock()
                .ensure_steamvr_shutdown();
        };

        if let Some(popup) = &self.new_version_popup
            && let Some(action) = popup.ui(ui, shutdown_alvr)
        {
            if let CloseAction::CloseWithRequest(request) = action {
                requests.push(request);
            }

            self.new_version_popup = None;
        }

        for request in requests {
            self.data_sources.request(request);
        }

        if ui.input(|state| state.viewport().close_requested())
            && self.session.as_ref().is_some_and(|s| {
                s.to_settings()
                    .extra
                    .steamvr_launcher
                    .open_close_steamvr_with_dashboard
            })
        {
            shutdown_alvr();
        }
    }
}
