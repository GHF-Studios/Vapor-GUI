use eframe::egui;
use std::collections::BTreeSet;
use vapor_console_core::{
    ConsoleFilter, ConsoleHistory, ConsoleLevel, ConsoleRecord, ConsoleStyleRole, PromptHistory,
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
    view_cache: ConsoleViewCache,
}

#[derive(Default)]
struct ConsoleViewCache {
    history_revision: Option<u64>,
    filter: Option<ConsoleFilter>,
    filtered_indices: Vec<usize>,
    sources: BTreeSet<String>,
    origins: BTreeSet<String>,
    compositions: BTreeSet<String>,
}

impl ConsoleViewCache {
    fn refresh(&mut self, history: &ConsoleHistory, filter: &ConsoleFilter) {
        let history_changed = self.history_revision != Some(history.revision());
        if history_changed {
            self.sources.clear();
            self.origins.clear();
            self.compositions.clear();
            for record in history.records() {
                self.sources.insert(record.source.0.clone());
                for field in &record.fields {
                    match field.key.as_str() {
                        "origin" => {
                            self.origins.insert(field.value.clone());
                        }
                        "composition" => {
                            self.compositions.insert(field.value.clone());
                        }
                        _ => {}
                    }
                }
            }
            self.history_revision = Some(history.revision());
        }

        if history_changed || self.filter.as_ref() != Some(filter) {
            self.filtered_indices = history
                .records()
                .enumerate()
                .filter_map(|(index, record)| filter.matches(record).then_some(index))
                .collect();
            self.filter = Some(filter.clone());
        }
    }
}

impl Default for ConsolePanelState {
    fn default() -> Self {
        Self {
            history: ConsoleHistory::new(4_000),
            filter: ConsoleFilter::default(),
            prompt_history: PromptHistory::default(),
            command: String::new(),
            history_cursor: None,
            view_cache: ConsoleViewCache::default(),
        }
    }
}

impl ConsolePanelState {
    /// Draw a dark, filterable console. Read-only surfaces omit the prompt.
    /// Returns one submitted command; the caller sends it to Vapor's service.
    pub fn show(&mut self, ui: &mut egui::Ui, prompt_enabled: bool) -> Option<String> {
        self.view_cache.refresh(&self.history, &self.filter);
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
                    egui::ComboBox::from_id_salt(ui.id().with("console-source"))
                        .selected_text(self.filter.source.as_deref().unwrap_or("All sources"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.filter.source, None, "All sources");
                            for source in &self.view_cache.sources {
                                ui.selectable_value(
                                    &mut self.filter.source,
                                    Some(source.clone()),
                                    source,
                                );
                            }
                        });
                    egui::ComboBox::from_id_salt(ui.id().with("console-origin"))
                        .selected_text(self.filter.origin.as_deref().unwrap_or("All apps"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.filter.origin, None, "All apps");
                            for origin in &self.view_cache.origins {
                                ui.selectable_value(
                                    &mut self.filter.origin,
                                    Some(origin.clone()),
                                    origin,
                                );
                            }
                        });
                    egui::ComboBox::from_id_salt(ui.id().with("console-composition"))
                        .selected_text(
                            self.filter
                                .composition
                                .as_deref()
                                .unwrap_or("All compositions"),
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.filter.composition,
                                None,
                                "All compositions",
                            );
                            for composition in &self.view_cache.compositions {
                                ui.selectable_value(
                                    &mut self.filter.composition,
                                    Some(composition.clone()),
                                    composition,
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
                        let mut selected =
                            self.filter.levels.is_empty() || self.filter.levels.contains(&level);
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
                show_records(ui, &self.history, &self.view_cache.filtered_indices);
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
                            self.command = self
                                .history_cursor
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

/// Draw timestamped, fixed-height console rows while only creating widgets for
/// visible records. Long messages and metadata remain available on hover.
pub fn show_records(ui: &mut egui::Ui, history: &ConsoleHistory, indices: &[usize]) {
    if indices.is_empty() {
        return;
    }

    let row_height = ui
        .text_style_height(&egui::TextStyle::Body)
        .max(ui.spacing().interact_size.y);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show_rows(ui, row_height, indices.len(), |ui, visible_rows| {
            for row in visible_rows {
                let Some(record) = indices.get(row).and_then(|index| history.record_at(*index)) else {
                    continue;
                };
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
                let mut details = record.message.clone();
                for field in &record.fields {
                    details.push_str(&format!("\n{}={}", field.key, field.value));
                }
                ui.horizontal(|ui| {
                    super::icons::console_mark(ui, record.level);
                    ui.weak(format_timestamp(record.timestamp_unix_ms));
                    ui.weak(&record.source.0);
                    if let Some(origin) = record.fields.iter().find(|field| field.key == "origin") {
                        ui.strong(&origin.value);
                    }
                    if let Some(target) = &record.target {
                        ui.weak(target);
                    }
                    if let Some(stream) = record.fields.iter().find(|field| field.key == "stream") {
                        let color = match stream.value.as_str() {
                            "stdout" => egui::Color32::from_rgb(130, 205, 150),
                            "stderr" => egui::Color32::from_rgb(235, 175, 105),
                            _ => ui.visuals().weak_text_color(),
                        };
                        ui.colored_label(color, &stream.value);
                    }
                    let response = ui.add_sized(
                        [ui.available_width(), row_height],
                        egui::Label::new(
                            egui::RichText::new(&record.message)
                                .monospace()
                                .color(color),
                        )
                        .truncate(),
                    );
                    response.on_hover_text(details);
                });
            }
        });
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

#[cfg(test)]
mod tests {
    use super::{ConsoleViewCache, records_from_operation_output};
    use vapor_client_protocol::{
        ClientOrigin, OperationOutputEvent, OperationOutputStream,
    };
    use vapor_console_core::{
        ConsoleFilter, ConsoleHistory, ConsoleLevel, ConsoleRecord, DiagnosticOrigin,
        DiagnosticSeverity, DiagnosticSpan, StructuredDiagnostic,
    };

    #[test]
    fn console_view_cache_rebuilds_filter_indexes_only_when_inputs_change() {
        let mut history = ConsoleHistory::new(8);
        history.push(
            ConsoleRecord::new(1, ConsoleLevel::Info, "service", "ready")
                .field("origin", "studio")
                .field("composition", "demo"),
        );
        history.push(ConsoleRecord::new(2, ConsoleLevel::Error, "cargo", "failed"));

        let mut cache = ConsoleViewCache::default();
        let mut filter = ConsoleFilter::default();
        cache.refresh(&history, &filter);
        assert_eq!(cache.filtered_indices, vec![0, 1]);
        assert_eq!(cache.sources.iter().cloned().collect::<Vec<_>>(), ["cargo", "service"]);
        assert_eq!(cache.origins.iter().cloned().collect::<Vec<_>>(), ["studio"]);
        assert_eq!(cache.compositions.iter().cloned().collect::<Vec<_>>(), ["demo"]);

        filter.search = "failed".to_owned();
        cache.refresh(&history, &filter);
        assert_eq!(cache.filtered_indices, vec![1]);
        assert_eq!(history.record_at(cache.filtered_indices[0]).unwrap().message, "failed");
    }

    #[test]
    fn console_view_cache_tracks_fifo_eviction_and_clear() {
        let mut history = ConsoleHistory::new(2);
        history.push(ConsoleRecord::new(1, ConsoleLevel::Info, "old", "old row"));
        history.push(ConsoleRecord::new(2, ConsoleLevel::Info, "kept", "kept row"));
        let mut cache = ConsoleViewCache::default();
        let filter = ConsoleFilter::default();
        cache.refresh(&history, &filter);

        history.push(ConsoleRecord::new(3, ConsoleLevel::Info, "new", "new row"));
        cache.refresh(&history, &filter);
        assert_eq!(cache.filtered_indices, vec![0, 1]);
        assert_eq!(history.record_at(0).unwrap().message, "kept row");
        assert!(cache.sources.contains("new"));
        assert!(!cache.sources.contains("old"));

        history.clear();
        cache.refresh(&history, &filter);
        assert!(cache.filtered_indices.is_empty());
        assert!(cache.sources.is_empty());
    }

    #[test]
    fn structured_operation_output_becomes_one_readable_record_with_expandable_details() {
        let output = OperationOutputEvent {
            id: "op-7".to_owned(),
            origin: ClientOrigin::Ide,
            composition_id: None,
            stream: OperationOutputStream::Stdout,
            timestamp_unix_ms: 42,
            level: super::super::ConsoleLevel::Info,
            source: "workspace".to_owned(),
            action: "workspace.check".to_owned(),
            process: Some("cargo".to_owned()),
            diagnostic: Some(StructuredDiagnostic {
                origin: DiagnosticOrigin::Cargo,
                severity: DiagnosticSeverity::Error,
                code: Some("E0425".to_owned()),
                message: "cannot find value `ship`".to_owned(),
                rendered: Some("error[E0425]: cannot find value `ship`\n".to_owned()),
                primary: Some(DiagnosticSpan {
                    file: "src/main.rs".to_owned(),
                    start_line: 4,
                    start_column: 5,
                    end_line: 4,
                    end_column: 9,
                    primary: true,
                    label: None,
                }),
                related: Vec::new(),
                raw_payload: r#"{"reason":"compiler-message"}"#.to_owned(),
            }),
            raw_payload: None,
            text: "error[E0425]: cannot find value `ship`".to_owned(),
        };

        let records = records_from_operation_output(&output);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "cannot find value `ship`");
        assert_eq!(records[0].level, super::super::ConsoleLevel::Error);
        assert!(records[0].fields.iter().any(|field| field.key == "code" && field.value == "E0425"));
        assert!(records[0].fields.iter().any(|field| field.key == "location" && field.value == "src/main.rs:4:5"));
        assert!(records[0].fields.iter().any(|field| field.key == "raw" && field.value.contains("compiler-message")));
    }

    #[test]
    fn plain_operation_output_keeps_each_nonempty_line_and_origin() {
        let output = OperationOutputEvent {
            id: "op-8".to_owned(),
            origin: ClientOrigin::Launcher,
            composition_id: Some("example".to_owned()),
            stream: OperationOutputStream::Stderr,
            timestamp_unix_ms: 43,
            level: super::super::ConsoleLevel::Info,
            source: "workspace".to_owned(),
            action: "workspace.test".to_owned(),
            process: Some("cargo".to_owned()),
            diagnostic: None,
            raw_payload: Some(r#"{"reason":"compiler-artifact"}"#.to_owned()),
            text: "one\ntwo\n".to_owned(),
        };

        let records = records_from_operation_output(&output);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].message, "one");
        assert_eq!(records[1].message, "two");
        assert!(records[0].fields.iter().any(|field| field.key == "origin" && field.value == "launcher"));
        assert!(records[0].fields.iter().any(|field| field.key == "raw" && field.value.contains("compiler-artifact")));
    }
}

/// Convert service operation output into the canonical console history format.
/// Structured diagnostics stay one searchable row and retain source and raw
/// producer details as fields for hover inspection.
pub fn records_from_operation_output(
    output: &vapor_client_protocol::OperationOutputEvent,
) -> Vec<ConsoleRecord> {
    let source = if output.source.is_empty() {
        "command"
    } else {
        output.source.as_str()
    };
    let stream = format!("{:?}", output.stream).to_ascii_lowercase();
    let mut messages = if let Some(diagnostic) = &output.diagnostic {
        vec![
            diagnostic
                .message
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
        ]
    } else {
        super::plain_terminal_text(&output.text)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
    };
    if messages.is_empty() {
        return Vec::new();
    }
    let mut records = Vec::with_capacity(messages.len());
    for message in messages.drain(..) {
        let level = output
            .diagnostic
            .as_ref()
            .map_or(output.level, |diagnostic| diagnostic.severity.console_level());
        let mut record = ConsoleRecord::new(
            output.timestamp_unix_ms,
            level,
            source,
            super::plain_terminal_text(&message),
        )
        .target(output.action.clone())
        .field("origin", output.origin.label())
        .field("operation", output.id.clone())
        .field("stream", stream.clone());
        if let Some(process) = &output.process {
            record = record.field("process", process.clone());
        }
        if let Some(composition_id) = &output.composition_id {
            record = record.field("composition", composition_id.clone());
        }
        if let Some(diagnostic) = &output.diagnostic {
            record = record.field("producer", format!("{:?}", diagnostic.origin).to_ascii_lowercase());
            if let Some(code) = &diagnostic.code {
                record = record.field("code", code.clone());
            }
            if let Some(primary) = &diagnostic.primary {
                record = record.field(
                    "location",
                    format!("{}:{}:{}", primary.file, primary.start_line, primary.start_column),
                );
                if let Some(label) = &primary.label {
                    record = record.field("location detail", label.clone());
                }
            }
            for related in &diagnostic.related {
                let label = related.label.as_deref().unwrap_or("related");
                record = record.field(
                    "related location",
                    format!("{label}: {}:{}:{}", related.file, related.start_line, related.start_column),
                );
            }
            if let Some(rendered) = &diagnostic.rendered {
                record = record.field("rendered", super::plain_terminal_text(rendered));
            }
            record = record.field("raw", diagnostic.raw_payload.clone());
        } else if let Some(raw_payload) = &output.raw_payload {
            record = record.field("raw", raw_payload.clone());
        }
        records.push(record);
    }
    records
}
