use alvr_common::ALVR_VERSION;
use alvr_gui_common::theme;
use eframe::egui::{Frame, RichText, ScrollArea, Ui};

pub fn about_tab_ui(ui: &mut Ui) {
    ui.label(RichText::new(format!("KiraVR streamer v{}", *ALVR_VERSION)).size(30.0));
    ui.add_space(10.0);
    ui.hyperlink_to("KiraVR on GitHub", "https://github.com/Daggenthal/KiraVR");
    ui.hyperlink_to(
        "Report a KiraVR problem",
        "https://github.com/Daggenthal/KiraVR/issues",
    );
    ui.hyperlink_to(
        "Latest release",
        "https://github.com/Daggenthal/KiraVR/releases/latest",
    );
    ui.add_space(10.0);
    // KiraVR is a fork. Credit the original project and keep its community
    // links clearly labeled as ALVR's, not ours.
    ui.label("KiraVR is an unofficial fork of ALVR and is not affiliated with the ALVR team.");
    ui.hyperlink_to("ALVR on GitHub", "https://github.com/alvr-org/ALVR");
    ui.hyperlink_to(
        "Donate to ALVR on Open Collective",
        "https://opencollective.com/alvr",
    );
    ui.add_space(10.0);
    ui.label("License:");
    Frame::group(ui.style())
        .fill(theme::DARKER_BG)
        .inner_margin(theme::FRAME_PADDING)
        .show(ui, |ui| {
            ScrollArea::new([false, true])
                .id_salt("license_scroll")
                .show(ui, |ui| ui.label(include_str!("../../../../../LICENSE")))
        });
}
