use eframe::egui;
use std::collections::BTreeSet;
use vapor_console_core::{
    ConsoleFilter, ConsoleHistory, ConsoleLevel, ConsoleStyleRole, PromptHistory,
};

const LEVELS: [ConsoleLevel; 8] = [
    ConsoleLevel::Trace,
    ConsoleLevel::Debug,
    ConsoleLevel::Info,
    ConsoleLevel::Success,
    ConsoleLevel::Warn,
    ConsoleLevel::Error,
    ConsoleLevel::Input,
    ConsoleLevel::System,
];

/// UI state shared by the Launcher, Studio and IDE console surfaces.
///
/// The state owns only presentation history and prompt text. Command parsing,
/// authorization and execution remain with the connected Vapor Client service.
pub struct ConsolePanelState {
    pub history: ConsoleHistory,
    pub filter: ConsoleFilter,
    pub prompt_history: PromptHistory,
    command: String,
    history_cursor: Option<usize>,
}

impl Default for ConsolePanelState {
    fn default() -> Self {
        Self {
            history: ConsoleHistory::new(4_000),
            filter: ConsoleFilter::default(),
            prompt_history: PromptHistory::default(),
            command: String::new(),
            history_cursor: None,
        }
    }
}

impl ConsolePanelState {
    /// Draw a dark, filterable console. Read-only surfaces omit the prompt.
    /// Returns one submitted command; the caller sends it to Vapor's service.
    pub fn show(&mut self, ui: &mut egui::Ui, prompt_enabled: bool) -> Option<String> {
        let mut submitted = None;
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(8, 9, 12))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter.search)
                            .hint_text("Search output")
                            .desired_width(200.0),
                    );
                    let sources = self
                        .history
                        .records()
                        .map(|record| record.source.0.clone())
                        .collect::<BTreeSet<_>>();
                    egui::ComboBox::from_id_salt(ui.id().with("console-source"))
                        .selected_text(self.filter.source.as_deref().unwrap_or("All sources"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.filter.source, None, "All sources");
                            for source in sources {
                                ui.selectable_value(
                                    &mut self.filter.source,
                                    Some(source.clone()),
                                    source,
                                );
                            }
                        });
                    if ui.button("Clear").clicked() {
                        self.history.clear();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.weak("Show");
                    for level in LEVELS {
                        let mut selected = self.filter.levels.is_empty()
                            || self.filter.levels.contains(&level);
                        if ui.toggle_value(&mut selected, level.label()).changed() {
                            if self.filter.levels.is_empty() {
                                self.filter.levels = LEVELS.to_vec();
                            }
                            if selected {
                                if !self.filter.levels.contains(&level) {
                                    self.filter.levels.push(level);
                                }
                            } else {
                                self.filter.levels.retain(|candidate| *candidate != level);
                            }
                            if self.filter.levels.len() == LEVELS.len() {
                                self.filter.levels.clear();
                            }
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for record in self.history.filtered(&self.filter) {
                            let color = match record.level.style_role() {
                                ConsoleStyleRole::Trace | ConsoleStyleRole::Muted => {
                                    ui.visuals().weak_text_color()
                                }
                                ConsoleStyleRole::Debug | ConsoleStyleRole::Metadata => {
                                    egui::Color32::LIGHT_BLUE
                                }
                                ConsoleStyleRole::Success => egui::Color32::LIGHT_GREEN,
                                ConsoleStyleRole::Warning => egui::Color32::YELLOW,
                                ConsoleStyleRole::Error => egui::Color32::LIGHT_RED,
                                ConsoleStyleRole::Input => egui::Color32::from_rgb(220, 180, 255),
                                ConsoleStyleRole::Normal => egui::Color32::from_rgb(225, 228, 234),
                            };
                            ui.horizontal_wrapped(|ui| {
                                super::icons::console_mark(ui, record.level);
                                ui.weak(format_timestamp(record.timestamp_unix_ms));
                                ui.weak(&record.source.0);
                                if let Some(target) = &record.target {
                                    ui.weak(target);
                                }
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&record.message)
                                            .monospace()
                                            .color(color),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                                );
                                for field in &record.fields {
                                    if field.key == "stream" {
                                        let color = match field.value.as_str() {
                                            "stdout" => egui::Color32::from_rgb(130, 205, 150),
                                            "stderr" => egui::Color32::from_rgb(235, 175, 105),
                                            _ => ui.visuals().weak_text_color(),
                                        };
                                        ui.colored_label(
                                            color,
                                            format!("{}={}", field.key, field.value),
                                        );
                                    } else {
                                        ui.weak(format!("{}={}", field.key, field.value));
                                    }
                                }
                            });
                        }
                    });
                if prompt_enabled {
                    ui.separator();
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.command)
                            .hint_text("Enter a Vapor command")
                            .desired_width(f32::INFINITY),
                    );
                    if response.has_focus() {
                        let (up, down, enter) = ui.input(|input| {
                            (
                                input.key_pressed(egui::Key::ArrowUp),
                                input.key_pressed(egui::Key::ArrowDown),
                                input.key_pressed(egui::Key::Enter),
                            )
                        });
                        if up {
                            let next = self
                                .history_cursor
                                .unwrap_or(self.prompt_history.len())
                                .saturating_sub(1);
                            self.history_cursor = Some(next);
                            if let Some(command) = self.prompt_history.get(next) {
                                self.command = command.to_owned();
                            }
                        } else if down {
                            let next = self.history_cursor.map(|index| index + 1);
                            self.history_cursor =
                                next.filter(|index| *index < self.prompt_history.len());
                            self.command = self.history_cursor
                                .and_then(|index| self.prompt_history.get(index))
                                .unwrap_or("")
                                .to_owned();
                        } else if enter {
                            let command = self.command.trim().to_owned();
                            if !command.is_empty() {
                                self.prompt_history.push(command.clone());
                                self.command.clear();
                                self.history_cursor = None;
                                submitted = Some(command);
                            }
                        }
                    }
                }
            });
        submitted
    }
}

fn format_timestamp(timestamp_unix_ms: u64) -> String {
    let seconds = timestamp_unix_ms / 1_000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        (seconds / 3_600) % 24,
        (seconds / 60) % 60,
        seconds % 60,
        timestamp_unix_ms % 1_000
    )
}
