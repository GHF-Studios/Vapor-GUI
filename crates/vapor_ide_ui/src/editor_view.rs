use super::*;

impl IdeApp {
    pub(super) fn draw_editor(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        let open_documents = self.documents.keys().cloned().collect::<Vec<_>>();
        let mut close_document = None;
        if !open_documents.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for path in open_documents {
                    let label = document_tab_label(&path, &self.documents);
                    let dirty = self.documents.get(&path).is_some_and(Document::is_dirty);
                    let selected = self.selected_file.as_ref() == Some(&path);
                    if ui
                        .selectable_label(
                            selected,
                            format!("{label}{}", if dirty { " *" } else { "" }),
                        )
                        .clicked()
                    {
                        self.selected_file = Some(path.clone());
                    }
                    if ui
                        .add_enabled(!dirty && self.pending.is_none(), egui::Button::new("Close"))
                        .on_hover_text(if dirty {
                            "Save this file before closing it."
                        } else {
                            "Close file"
                        })
                        .clicked()
                    {
                        close_document = Some(path.clone());
                    }
                }
            });
            ui.separator();
        }
        if let Some(path) = close_document {
            self.close_document(&path);
        }
        let Some(path) = self.selected_file.clone() else {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.32);
                ui.heading("Open a Rust workspace");
                ui.label(
                    "Choose a folder above to browse files, edit source, and run a managed check.",
                );
            });
            return;
        };
        if !self.documents.contains_key(&path) {
            ui.label("Opening file...");
            return;
        }
        if self
            .pending_navigation
            .as_ref()
            .is_some_and(|(target, _, _)| target == &path)
            && let Some((_, line, character)) = self.pending_navigation.take()
            && let Some(document) = self.documents.get(&path)
        {
            if let Some(offset) = position_to_byte_offset(&document.contents, line, character) {
                let semantic_range = self
                    .selected_semantic
                    .as_ref()
                    .filter(|reference| reference.id.document == path)
                    .and_then(|reference| {
                        semantic_object_byte_range(
                            &self.semantic_index,
                            reference,
                            &document.contents,
                        )
                    });
                vapor_gui_core::code_editor::select_range(
                    &context,
                    &path,
                    &document.contents,
                    semantic_range.unwrap_or(offset..offset),
                );
            } else {
                self.status = "The language server returned an invalid source position.".to_owned();
            }
        }
        let mut find_action = self.find_navigation.take();
        let mut close_find = false;
        if self.find_open {
            let mut query_changed = false;
            ui.horizontal(|ui| {
                ui.label("Find");
                let query = ui.add(
                    egui::TextEdit::singleline(&mut self.find_query)
                        .hint_text("Find in this file")
                        .desired_width(240.0),
                );
                if self.focus_find_query {
                    query.request_focus();
                    self.focus_find_query = false;
                }
                query_changed = query.changed();
                let count = self.documents.get(&path).map_or(0, |document| {
                    find_matches(&document.contents, &self.find_query).len()
                });
                let match_status = match (self.find_match_index, count) {
                    (Some(index), count) if count > 0 => format!("{} / {count}", index + 1),
                    (_, 0) if self.find_query.is_empty() => String::new(),
                    (_, 0) => "No matches".to_owned(),
                    _ => format!("{count} matches"),
                };
                ui.weak(match_status);
                if query.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    find_action = Some(false);
                }
                if ui
                    .add_enabled(count > 0, egui::Button::new("Previous"))
                    .clicked()
                {
                    find_action = Some(true);
                }
                if ui
                    .add_enabled(count > 0, egui::Button::new("Next"))
                    .clicked()
                {
                    find_action = Some(false);
                }
                if ui
                    .small_button("Close")
                    .on_hover_text("Close find")
                    .clicked()
                {
                    close_find = true;
                }
            });
            if query_changed {
                self.find_match_index = None;
            }
            if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                close_find = true;
            }
        }
        if close_find {
            self.find_open = false;
            self.find_match_index = None;
        }
        if let Some(previous) = find_action {
            let source = self
                .documents
                .get(&path)
                .map(|document| document.contents.as_str())
                .unwrap_or_default();
            let matches = find_matches(source, &self.find_query);
            if !matches.is_empty() {
                let index = match (self.find_match_index, previous) {
                    (Some(index), true) => index.checked_sub(1).unwrap_or(matches.len() - 1),
                    (Some(index), false) => (index + 1) % matches.len(),
                    (None, true) => matches.len() - 1,
                    (None, false) => 0,
                };
                self.find_match_index = Some(index);
                vapor_gui_core::code_editor::select_range(
                    &context,
                    &path,
                    source,
                    matches[index].clone(),
                );
            } else {
                self.find_match_index = None;
            }
        }
        let Some(document) = self.documents.get(&path) else {
            return;
        };
        let mut request_definition = false;
        let mut request_references = false;
        let mut request_hover = false;
        let mut request_formatting = false;
        ui.horizontal(|ui| {
            ui.strong(path.display().to_string());
            ui.weak(vapor_gui_core::code_editor::language_for_path(&path).unwrap_or("Plain text"));
            if document.is_dirty() {
                ui.colored_label(egui::Color32::YELLOW, "Unsaved");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!(
                    "Ln {}, Col {}",
                    self.cursor_position.0, self.cursor_position.1
                ));
                ui.weak(&self.language_server_status);
                if ui
                    .add_enabled(self.language_server_ready, egui::Button::new("Hover"))
                    .clicked()
                {
                    request_hover = true;
                }
                if ui
                    .add_enabled(self.language_server_ready, egui::Button::new("Definition"))
                    .on_hover_text("Go to Definition | F12")
                    .clicked()
                {
                    request_definition = true;
                }
                if ui
                    .add_enabled(self.language_server_ready, egui::Button::new("References"))
                    .on_hover_text("Find references to the symbol under the cursor")
                    .clicked()
                {
                    request_references = true;
                }
                if ui
                    .add_enabled(self.language_server_ready, egui::Button::new("Format"))
                    .on_hover_text("Format this Rust file with the pinned Rust Analyzer")
                    .clicked()
                {
                    request_formatting = true;
                }
            });
        });
        ui.separator();
        if request_definition {
            self.request_lsp_at_cursor("textDocument/definition", LspRequest::Definition);
        }
        if request_references {
            self.request_lsp_at_cursor("textDocument/references", LspRequest::References);
        }
        if request_hover {
            self.request_lsp_at_cursor("textDocument/hover", LspRequest::Hover);
        }
        if request_formatting {
            self.request_lsp_formatting(&path);
        }
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        let (cursor, changed_document) = {
            let output = vapor_gui_core::code_editor::show_with_cursor(
                ui,
                &path,
                &mut document.contents,
                ui.available_size(),
            );
            let changed_document = if output.response.changed() {
                document.version += 1;
                Some((document.contents.clone(), document.version))
            } else {
                None
            };
            (output.cursor, changed_document)
        };
        if let Some(cursor) = cursor {
            self.cursor_offset = cursor.primary.index.into();
            self.cursor_position = cursor_position(&document.contents, self.cursor_offset);
        }
        if let Some((contents, version)) = changed_document {
            self.completion_items.clear();
            self.hover = None;
            self.did_change_document(&path, contents, version);
        }
        if !self.completion_items.is_empty() {
            let mut selected_completion = None;
            ui.separator();
            ui.strong("Completions");
            egui::ScrollArea::vertical()
                .id_salt("ide-completions")
                .max_height(120.0)
                .show(ui, |ui| {
                    for (index, item) in self.completion_items.iter().enumerate() {
                        let response = ui.selectable_label(false, &item.label);
                        if let Some(detail) = &item.detail {
                            response.clone().on_hover_text(detail);
                        }
                        if response.clicked() {
                            selected_completion = Some(index);
                        }
                    }
                });
            if let Some(index) = selected_completion {
                self.apply_completion(&context, index);
            }
        }
    }
}
