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
    #[cfg(not(target_arch = "wasm32"))]
    headset_app: crate::headset_app::HeadsetApp,
    #[cfg(not(target_arch = "wasm32"))]
    headset_confirm: TwoStepConfirm,
    #[cfg(not(target_arch = "wasm32"))]
    updates: UpdatesSection,
}

impl InstallationTab {
    pub fn new() -> Self {
        Self {
            drivers: vec![],
            last_update_instant: Instant::now(),
            #[cfg(all(target_os = "linux", not(target_arch = "wasm32")))]
            system_check: SystemCheck::default(),
            #[cfg(not(target_arch = "wasm32"))]
            headset_app: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            headset_confirm: TwoStepConfirm::default(),
            #[cfg(not(target_arch = "wasm32"))]
            updates: UpdatesSection::default(),
        }
    }

    pub fn update_drivers(&mut self, list: Vec<PathBuf>) {
        self.drivers = list;
    }

    // KiraVR: Mirrors the "Check for Updates on Start" setting.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn update_auto_check(&mut self, enabled: bool) {
        self.updates.auto_check = Some(enabled);
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

                #[cfg(not(target_arch = "wasm32"))]
                headset_app_ui(&self.headset_app, &mut self.headset_confirm, ui);

                #[cfg(not(target_arch = "wasm32"))]
                self.updates.ui(ui, &mut requests);

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

// KiraVR: "Headset App" section: Shows what is connected over USB and
// installs the app bundled with this streamer.
#[cfg(not(target_arch = "wasm32"))]
fn headset_app_ui(app: &crate::headset_app::HeadsetApp, confirm: &mut TwoStepConfirm, ui: &mut Ui) {
    use crate::headset_app::{HeadsetState, Job, bundled_version};
    use alvr_gui_common::theme::log_colors;
    use eframe::egui::{Button, ProgressBar};

    app.poll(ui.ctx());
    let job = app.job();
    let working = matches!(job, Job::Working { .. });
    if working {
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    } else {
        // Notice plugging in and out without user input.
        ui.ctx().request_repaint_after(Duration::from_secs(3));
    }
    let bundled = bundled_version();
    let has_apk = app.bundled_apk_exists();

    Frame::group(ui.style())
        .fill(theme::SECTION_BG)
        .inner_margin(theme::FRAME_PADDING)
        .show(ui, |ui| {
            ui.vertical_centered_justified(|ui| {
                ui.label(RichText::new("Headset App").size(18.0));
            });
            ui.add_space(4.0);

            // Changing the version of an installed app is asked twice.
            let mut change: Option<(String, String)> = None;
            let (status, color, can_install, button) = match app.state() {
                None => ("Looking for a headset...".to_owned(), theme::FG, false, "Install on Headset"),
                Some(HeadsetState::NoAdb) => (
                    "Connect the headset with a USB cable. adb is downloaded on the first install."
                        .to_owned(),
                    theme::FG,
                    true,
                    "Install on Headset",
                ),
                Some(HeadsetState::NotConnected) => (
                    "No headset found. Connect it with a USB cable and turn on developer mode."
                        .to_owned(),
                    log_colors::WARNING_LIGHT,
                    false,
                    "Install on Headset",
                ),
                Some(HeadsetState::Unauthorized) => (
                    "Put on the headset and allow USB debugging for this PC.".to_owned(),
                    log_colors::WARNING_LIGHT,
                    false,
                    "Install on Headset",
                ),
                Some(HeadsetState::Connected {
                    model,
                    installed_version: None,
                }) => (
                    format!("{model} connected, KiraVR is not installed."),
                    theme::FG,
                    true,
                    "Install on Headset",
                ),
                Some(HeadsetState::Connected {
                    model,
                    installed_version: Some(installed),
                }) if installed == bundled => (
                    format!("{model} connected, KiraVR v{installed} is up to date."),
                    theme::OK_GREEN,
                    true,
                    "Reinstall",
                ),
                Some(HeadsetState::Connected {
                    model,
                    installed_version: Some(installed),
                }) => {
                    let older = matches!(
                        (
                            alvr_common::semver::Version::parse(&installed),
                            alvr_common::semver::Version::parse(&bundled),
                        ),
                        (Ok(i), Ok(b)) if b < i
                    );
                    change = Some(if older {
                        (
                            format!("Go back from v{installed} to the older v{bundled} on the headset?"),
                            "Going back to an older app can reset its settings on the headset, so you may have to trust the headset again. Continue?".to_owned(),
                        )
                    } else {
                        (
                            format!("Update the headset app from v{installed} to v{bundled}?"),
                            "The headset app is replaced now. You can go back later by switching the PC to an older version in Updates and installing again. Continue?".to_owned(),
                        )
                    });
                    (
                        format!("{model} connected, has v{installed}, this streamer is v{bundled}. They must match."),
                        log_colors::WARNING_LIGHT,
                        true,
                        if older { "Install Older Headset App" } else { "Update Headset App" },
                    )
                }
            };
            ui.label(RichText::new(status).color(color));

            if !has_apk {
                ui.label(
                    RichText::new("This build has no headset app bundled.")
                        .color(log_colors::WARNING_LIGHT),
                );
            }

            ui.add_space(4.0);
            let label = if working { "Installing..." } else { button };
            if ui
                .add_enabled(
                    can_install && has_apk && !working && !confirm.is_open(),
                    Button::new(label),
                )
                .clicked()
            {
                if change.is_some() {
                    confirm.open("headset");
                } else {
                    app.install(ui.ctx());
                }
            }
            if let Some((first, second)) = &change
                && confirm.ui(ui, "headset", first, second, button)
            {
                app.install(ui.ctx());
            }

            match job {
                Job::Idle => (),
                Job::Working {
                    step,
                    steps,
                    title,
                    transfer,
                    note,
                } => {
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("Step {step} of {steps}: {title}")).strong());
                    let fraction = transfer.as_ref().and_then(|t| t.fraction());
                    let bar = match fraction {
                        Some(f) => ProgressBar::new(f).text(
                            RichText::new(format!("{:.0}%", f * 100.0)).monospace(),
                        ),
                        // Nothing to count (the headset is installing): Show
                        // the overall step instead, animated.
                        None => ProgressBar::new((step - 1) as f32 / steps as f32).animate(true),
                    };
                    ui.add(bar.desired_height(22.0));
                    if let Some(transfer) = &transfer {
                        // Monospace, so the numbers do not jitter while they change.
                        ui.label(RichText::new(transfer.readout().join("  ·  ")).monospace());
                    }
                    if let Some(note) = note {
                        ui.label(RichText::new(note).color(theme::FG.gamma_multiply(0.7)));
                    }
                }
                Job::Done { message, note } => {
                    ui.add_space(4.0);
                    ui.label(RichText::new(message).color(theme::OK_GREEN).strong());
                    if let Some(note) = note {
                        ui.label(RichText::new(note).color(theme::FG.gamma_multiply(0.7)));
                    }
                }
                Job::Failed(message) => {
                    ui.add_space(4.0);
                    ui.label(RichText::new(message).color(log_colors::ERROR_LIGHT));
                }
            }
        });
    ui.add_space(6.0);
}

// KiraVR: Asks twice before something that changes what is installed. Either
// step can be cancelled, and nothing happens until the second "yes".
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct TwoStepConfirm {
    key: Option<String>,
    second: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl TwoStepConfirm {
    fn open(&mut self, key: &str) {
        self.key = Some(key.to_owned());
        self.second = false;
    }

    fn is_open(&self) -> bool {
        self.key.is_some()
    }

    // Shows the open step for this key. True once the user confirmed both.
    fn ui(&mut self, ui: &mut Ui, key: &str, first: &str, second: &str, action: &str) -> bool {
        use alvr_gui_common::theme::log_colors;

        if self.key.as_deref() != Some(key) {
            return false;
        }
        let mut confirmed = false;
        Frame::group(ui.style())
            .fill(theme::DARKER_BG)
            .inner_margin(theme::FRAME_PADDING)
            .show(ui, |ui| {
                let (step, text) = if self.second { (2, second) } else { (1, first) };
                ui.label(
                    RichText::new(format!("Confirm ({step} of 2)"))
                        .strong()
                        .color(log_colors::WARNING_LIGHT),
                );
                ui.label(text);
                ui.horizontal(|ui| {
                    let yes = if self.second { action } else { "Continue" };
                    if ui.button(yes).clicked() {
                        if self.second {
                            confirmed = true;
                            self.key = None;
                        } else {
                            self.second = true;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.key = None;
                    }
                });
            });
        confirmed
    }
}

// KiraVR: Updates. Checking is always the user's choice, installing too:
// KiraVR never replaces itself. Kept versions can be switched back to.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct UpdatesSection {
    auto_check: Option<bool>,
    checking: Option<std::sync::mpsc::Receiver<Result<crate::versions::UpdateCheck, String>>>,
    check_result: Option<Result<crate::versions::UpdateCheck, String>>,
    confirm: TwoStepConfirm,
    message: Option<(bool, String)>,
}

#[cfg(not(target_arch = "wasm32"))]
impl UpdatesSection {
    fn ui(&mut self, ui: &mut Ui, requests: &mut Vec<InstallationTabRequest>) {
        use crate::versions::{self, UpdateCheck};
        use alvr_gui_common::theme::log_colors;
        use eframe::egui::{Button, OpenUrl};

        if let Some(receiver) = &self.checking
            && let Ok(result) = receiver.try_recv()
        {
            self.check_result = Some(result);
            self.checking = None;
        }

        Frame::group(ui.style())
            .fill(theme::SECTION_BG)
            .inner_margin(theme::FRAME_PADDING)
            .show(ui, |ui| {
                ui.vertical_centered_justified(|ui| {
                    ui.label(RichText::new("Updates").size(18.0));
                });
                ui.add_space(4.0);
                let install = if versions::is_appimage_install() {
                    "AppImage"
                } else {
                    "not an AppImage install"
                };
                ui.label(RichText::new(format!("KiraVR v{} ({install})", *alvr_common::ALVR_VERSION)).strong());
                ui.label(
                    "KiraVR never downloads or installs updates by itself. You decide when to \
                     update, an AppImage asks twice before it replaces this version, and older \
                     versions are kept so you can go back.",
                );
                ui.add_space(4.0);

                ui.horizontal_wrapped(|ui| {
                    let busy = self.checking.is_some();
                    let label = if busy { "Checking..." } else { "Check for Updates" };
                    if ui.add_enabled(!busy, Button::new(label)).clicked() {
                        let (sender, receiver) = std::sync::mpsc::channel();
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            sender
                                .send(versions::check_for_update().map_err(|e| format!("{e:#}")))
                                .ok();
                            ctx.request_repaint();
                        });
                        self.checking = Some(receiver);
                        self.check_result = None;
                    }
                    if let Some(mut enabled) = self.auto_check
                        && ui
                            .checkbox(&mut enabled, "Check for Updates on Start")
                            .on_hover_text(
                                "One request to GitHub when the dashboard opens. Off means KiraVR never contacts GitHub by itself.",
                            )
                            .changed()
                    {
                        self.auto_check = Some(enabled);
                        requests.push(InstallationTabRequest::ServerRequest(
                            crate::dashboard::ServerRequest::SetSessionValues(vec![
                                alvr_packets::PathValuePair {
                                    path: alvr_packets::parse_path(
                                        "session_settings.extra.new_version_popup.enabled",
                                    )
                                    .unwrap(),
                                    value: serde_json::Value::Bool(enabled),
                                },
                            ]),
                        ));
                    }
                });
                match &self.check_result {
                    Some(Ok(UpdateCheck::UpToDate)) => {
                        ui.label(RichText::new("You have the latest version.").color(theme::OK_GREEN));
                    }
                    Some(Ok(UpdateCheck::Available { version })) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("KiraVR v{version} is available."))
                                    .color(log_colors::INFO_LIGHT),
                            );
                            if ui.button("Open Download Page").clicked() {
                                ui.ctx().open_url(OpenUrl::new_tab(versions::RELEASES_PAGE));
                            }
                        });
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(log_colors::ERROR_LIGHT));
                    }
                    None => (),
                }

                let Some(root) = versions::install_root().filter(|_| versions::is_appimage_install())
                else {
                    return;
                };
                ui.add_space(6.0);
                let kept = versions::previous(&root);
                if kept.is_empty() {
                    ui.label(
                        RichText::new("No older versions kept yet. They appear here after an update.")
                            .weak(),
                    );
                    return;
                }
                ui.label(RichText::new("Previous Versions").strong());
                for (index, old) in kept.iter().enumerate() {
                    let key = format!("version-{index}");
                    ui.horizontal(|ui| {
                        ui.label(format!("v{} (build {})", old.version, old.build_id));
                        if ui
                            .add_enabled(!self.confirm.is_open(), Button::new("Switch to This Version"))
                            .clicked()
                        {
                            self.confirm.open(&key);
                            self.message = None;
                        }
                    });
                    let current = &*alvr_common::ALVR_VERSION;
                    let first = format!("Switch KiraVR from v{current} to v{}?", old.version);
                    let second = format!(
                        "The dashboard restarts as v{}. v{current} is kept, so you can switch back here. \
                         Install the matching headset app afterwards. Switch now?",
                        old.version
                    );
                    if self.confirm.ui(ui, &key, &first, &second, "Switch Now") {
                        match versions::switch_to(&root, old)
                            .and_then(|()| versions::restart_dashboard(&root))
                        {
                            Ok(()) => std::process::exit(0),
                            Err(e) => self.message = Some((false, format!("{e:#}"))),
                        }
                    }
                }
                if let Some((ok, message)) = &self.message {
                    let color = if *ok { theme::OK_GREEN } else { log_colors::ERROR_LIGHT };
                    ui.label(RichText::new(message).color(color));
                }
            });
        ui.add_space(6.0);
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
