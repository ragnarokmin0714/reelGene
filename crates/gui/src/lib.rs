//! reelgene's window: everything that needs a desktop. The rest of the
//! program reaches it only through [`run`], so the toolkit can change without
//! touching the CLI, the settings or the engine.

mod clipboard;
mod gui;
mod icon;
mod notify;
mod sound;
mod tray;

pub use gui::run;

/// The licences of the fonts built into the window, which their terms ask
/// to travel with every copy (`reelgene licenses`).
pub fn notices() -> String {
    let parts = [
        (
            "Inter (the window's typeface)",
            include_str!("../assets/licenses/Inter-OFL.txt"),
        ),
        (
            "Ubuntu Light (egui's built-in font)",
            include_str!("../assets/licenses/Ubuntu-UFL.txt"),
        ),
        (
            "Hack (egui's built-in monospace font)",
            include_str!("../assets/licenses/Hack.txt"),
        ),
        (
            "egui-icons (egui's built-in symbols)",
            include_str!("../assets/licenses/egui-icons.txt"),
        ),
        (
            "emoji-icon-font (source of egui-icons)",
            include_str!("../assets/licenses/emoji-icon-font-MIT.txt"),
        ),
    ];
    parts
        .iter()
        .map(|(name, text)| format!("==== {name} ====\n\n{}\n", text.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}
