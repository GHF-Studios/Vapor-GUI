use eframe::egui;
use std::collections::BTreeSet;
use vapor_console_core::{ConsoleFilter, ConsoleHistory, ConsoleLevel, ConsoleStyleRole};

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

/// Show a compact, expandable, searchable command log using shared console records.
pub fn show_operation_log(
    ui: &mut egui::Ui,
    history: &mut ConsoleHistory,
    filter: &mut ConsoleFilter,
    expanded: &mut bool,
) {
    let response = egui::CollapsingHeader::new(format!("Command details ({})", history.len()))
        .default_open(*expanded)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut filter.search)
                        .hint_text("Find in command output")
                        .desired_width(220.0),
                );
                let sources = history
                    .records()
                    .map(|record| record.source.0.clone())
                    .collect::<BTreeSet<_>>();
                egui::ComboBox::from_id_salt("operation-log-source")
                    .selected_text(filter.source.as_deref().unwrap_or("All sources"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut filter.source, None, "All sources");
                        for source in sources {
                            ui.selectable_value(&mut filter.source, Some(source.clone()), source);
                        }
                    });
                if ui.button("Clear log").clicked() {
                    history.clear();
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.weak("Levels");
                for level in LEVELS {
                    let mut selected = filter.levels.is_empty() || filter.levels.contains(&level);
                    if ui.toggle_value(&mut selected, level.label()).changed() {
                        if filter.levels.is_empty() {
                            filter.levels = LEVELS.to_vec();
                        }
                        if selected {
                            if !filter.levels.contains(&level) {
                                filter.levels.push(level);
                            }
                        } else {
                            filter.levels.retain(|candidate| *candidate != level);
                        }
                        if filter.levels.len() == LEVELS.len() {
                            filter.levels.clear();
                        }
                    }
                }
            });
            let filter = filter.clone();
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for record in history.filtered(&filter) {
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
                            ConsoleStyleRole::Normal => ui.visuals().text_color(),
                        };
                        ui.horizontal_wrapped(|ui| {
                            super::icons::console_mark(ui, record.level);
                            ui.colored_label(color, record.level.label());
                            let seconds = record.timestamp_unix_ms / 1_000;
                            ui.weak(format!(
                                "{:02}:{:02}:{:02}.{:03}",
                                (seconds / 3_600) % 24,
                                (seconds / 60) % 60,
                                seconds % 60,
                                record.timestamp_unix_ms % 1_000
                            ));
                            ui.weak(&record.source.0);
                            if let Some(target) = &record.target {
                                ui.weak(target);
                            }
                            ui.label(egui::RichText::new(&record.message).monospace());
                            for field in &record.fields {
                                ui.weak(format!("{}={}", field.key, field.value));
                            }
                        });
                    }
                });
        });
    *expanded = response.body_returned.is_some();
}
