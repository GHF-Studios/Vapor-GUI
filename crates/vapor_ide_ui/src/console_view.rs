use super::*;

impl IdeApp {
    pub(super) fn draw_bottom(&mut self, ui: &mut egui::Ui) {
        let diagnostic_count = self.problems.count();
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    !self.bottom_console_active,
                    format!("Problems ({diagnostic_count})"),
                )
                .clicked()
            {
                self.bottom_console_active = false;
            }
            if ui
                .selectable_label(self.bottom_console_active, "Console")
                .clicked()
            {
                self.bottom_console_active = true;
            }
            ui.separator();
            ui.weak(&self.language_server_status);
        });
        ui.separator();
        if self.bottom_console_active {
            if self.console.history.is_empty() {
                ui.weak("Managed commands and toolchain output appear here.");
            }
            if let Some(command) = self.console.show(ui, true) {
                self.submit_console_command(&command);
            }
        } else {
            let mut selected_diagnostic = None;
            let row_height = ui
                .text_style_height(&egui::TextStyle::Body)
                .max(ui.spacing().interact_size.y);
            egui::ScrollArea::vertical()
                .id_salt("ide-diagnostics")
                .auto_shrink([false, false])
                .show_rows(ui, row_height, diagnostic_count, |ui, visible_rows| {
                    for ProblemRowRef { path, operation, diagnostic } in
                        self.problems.rows_in_range(visible_rows)
                    {
                        let marker = match diagnostic.severity {
                            vapor_gui_core::DiagnosticSeverity::Error => "Error",
                            vapor_gui_core::DiagnosticSeverity::Warning => "Warning",
                            vapor_gui_core::DiagnosticSeverity::Information => "Info",
                            vapor_gui_core::DiagnosticSeverity::Hint => "Hint",
                        };
                        let span = diagnostic.primary.as_ref();
                        let navigation_span = span.or_else(|| diagnostic.related.first());
                        let line = navigation_span.map_or(1, |span| span.start_line as usize);
                        let column = navigation_span.map_or(1, |span| span.start_column as usize);
                        let code = diagnostic.code.as_deref().map_or(String::new(), |code| format!(" [{code}]"));
                        let origin = diagnostic_origin_label(diagnostic.origin);
                        let operation = operation
                            .filter(|operation| !operation.is_empty())
                            .map_or_else(String::new, |operation| format!(" · {operation}"));
                        let location = match (path, navigation_span) {
                            (Some(path), Some(span)) => format!(
                                "{}:{}:{}", path.display(), span.start_line, span.start_column
                            ),
                            (Some(path), None) => path.display().to_string(),
                            (None, Some(span)) => format!(
                                "{}:{}:{}", span.file, span.start_line, span.start_column
                            ),
                            (None, None) => "workspace".to_owned(),
                        };
                        let response = ui.selectable_label(
                            false,
                            format!("{marker} · {origin}{operation}{code}  {location}  {}", diagnostic.message),
                        );
                        let mut details = diagnostic.message.clone();
                        for related in &diagnostic.related {
                            details.push_str(&format!(
                                "\n{}:{}:{} {}",
                                related.file,
                                related.start_line,
                                related.start_column,
                                related.label.as_deref().unwrap_or("related")
                            ));
                        }
                        details.push_str(&format!("\n\nRaw diagnostic\n{}", diagnostic.raw_payload));
                        let response = response.on_hover_text(details);
                        if response.clicked() {
                            if let Some(path) = path {
                                selected_diagnostic = Some((path.clone(), line, column));
                            }
                        }
                    }
                });
            if let Some((path, line, column)) = selected_diagnostic {
                self.navigate_to_workspace_location(
                    &path,
                    line.saturating_sub(1),
                    column.saturating_sub(1),
                );
            }
            if diagnostic_count == 0 {
                ui.weak("Rust Analyzer and managed build problems appear here.");
            }
        }
    }

    pub(super) fn submit_console_command(&mut self, line: &str) {
        use vapor_client_domain::{
            ConsoleCommand, ConsoleCommandOutcome, ConsoleCommandScope, console_command_catalog,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or_default();
        self.console.history.push(ConsoleRecord::new(
            now,
            ConsoleLevel::Input,
            "console",
            line,
        ));
        match ConsoleCommand::parse(line) {
            Ok(ConsoleCommand::Help) => {
                for command in console_command_catalog(ConsoleCommandScope::Workspace) {
                    self.console.history.push(
                        ConsoleRecord::new(now, ConsoleLevel::Info, "help", command.path)
                            .field("description", command.summary),
                    );
                }
            }
            Ok(ConsoleCommand::Status) => self.console.history.push(ConsoleRecord::new(
                now,
                ConsoleLevel::Info,
                "ide",
                self.status.clone(),
            )),
            Ok(command) => match command.outcome() {
                ConsoleCommandOutcome::Action(action) => {
                    if let Some(service) = self.service.as_mut() {
                        match service.send(*action, None) {
                            Ok(request_id) => {
                                self.console_requests.insert(request_id.0);
                            }
                            Err(error) => self.console.history.push(ConsoleRecord::new(
                                now,
                                ConsoleLevel::Error,
                                "vapor",
                                error.to_string(),
                            )),
                        }
                    } else {
                        self.console.history.push(ConsoleRecord::new(
                            now,
                            ConsoleLevel::Error,
                            "vapor",
                            "Vapor service is disconnected",
                        ));
                    }
                }
                ConsoleCommandOutcome::Help | ConsoleCommandOutcome::Status => unreachable!(),
            },
            Err(error) => self.console.history.push(ConsoleRecord::new(
                now,
                ConsoleLevel::Error,
                "console",
                format!("{error:?}"),
            )),
        }
    }
}

