#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    vapor_gui::run(vapor_gui_core::GuiMode::Studio)
}
