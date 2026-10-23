use super::*;

impl IdeApp {
    pub(super) fn draw_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.separator();
        if let Some(reference) = self.semantic.selected.clone()
            && let Some(object) = self.semantic.index.resolve(&reference).cloned()
        {
            ui.strong(object.kind.label());
            ui.heading(&object.name);
            ui.monospace(object.id.qualified_name());
            ui.weak(format!(
                "{} · document revision {}",
                object.id.document.display(),
                reference.document_version
            ));
            if let Some(detail) = &object.detail {
                ui.add_space(6.0);
                ui.label(detail);
            }
            if let Some(status) = semantic_view::semantic_convention_status(&object) {
                ui.colored_label(egui::Color32::LIGHT_BLUE, status);
            }
            let mut diagnostic_navigation = None;
            let mut related_navigation = None;
            let mut quick_fix_request = None;
            if let Some(relative_path) = self
                .workspace
                .root
                .as_deref()
                .and_then(|root| relative_workspace_path(root, &object.id.document))
            {
                let diagnostics = self
                    .semantic
                    .problems
                    .for_semantic_range(&relative_path, object.range);
                if !diagnostics.is_empty() {
                    ui.add_space(8.0);
                    ui.strong(format!("Problems · {}", diagnostics.len()));
                    for diagnostic in diagnostics {
                        let severity = match diagnostic.severity {
                            vapor_gui_core::DiagnosticSeverity::Error => {
                                ("Error", egui::Color32::LIGHT_RED)
                            }
                            vapor_gui_core::DiagnosticSeverity::Warning => {
                                ("Warning", egui::Color32::YELLOW)
                            }
                            vapor_gui_core::DiagnosticSeverity::Information => {
                                ("Info", egui::Color32::LIGHT_BLUE)
                            }
                            vapor_gui_core::DiagnosticSeverity::Hint => {
                                ("Hint", egui::Color32::GRAY)
                            }
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(severity.1, severity.0);
                            ui.weak(diagnostic_origin_label(diagnostic.origin));
                            if let Some(code) = &diagnostic.code {
                                ui.monospace(code);
                            }
                            if let Some(span) = &diagnostic.primary
                                && ui.small_button("Open").clicked()
                            {
                                diagnostic_navigation = Some((
                                    object.id.document.clone(),
                                    span.start_line.saturating_sub(1) as usize,
                                    span.start_column.saturating_sub(1) as usize,
                                ));
                            }
                            if self.language_server.ready
                                && diagnostic.origin
                                    == vapor_gui_core::DiagnosticOrigin::RustAnalyzer
                                && object
                                    .id
                                    .document
                                    .extension()
                                    .is_some_and(|extension| extension == "rs")
                                && self.editor.documents.contains_key(&object.id.document)
                                && ui.small_button("Quick fix").clicked()
                            {
                                quick_fix_request = Some(diagnostic.clone());
                            }
                            if !diagnostic.related.is_empty() {
                                let root = self.workspace.root.as_deref();
                                ui.menu_button(
                                    format!("Related ({})", diagnostic.related.len()),
                                    |ui| {
                                        for related in &diagnostic.related {
                                            let label = format!(
                                                "{}:{}:{} · {}",
                                                related.file,
                                                related.start_line,
                                                related.start_column,
                                                related
                                                    .label
                                                    .as_deref()
                                                    .unwrap_or("related location")
                                            );
                                            if ui.button(label).clicked()
                                                && let Some(path) = root.and_then(|root| {
                                                    workspace_diagnostic_path(
                                                        root,
                                                        &related.file,
                                                        &self.workspace.files,
                                                    )
                                                })
                                            {
                                                related_navigation = Some((
                                                    path,
                                                    related.start_line.saturating_sub(1) as usize,
                                                    related.start_column.saturating_sub(1) as usize,
                                                ));
                                            }
                                        }
                                    },
                                );
                            }
                        });
                        ui.label(&diagnostic.message)
                            .on_hover_text(diagnostics::diagnostic_details(diagnostic));
                    }
                }
            }
            if !object.attributes.is_empty() {
                ui.add_space(10.0);
                ui.strong("Attributes");
                for attribute in &object.attributes {
                    ui.monospace(attribute);
                }
            }
            let mut selected_member = None;
            if !object.children.is_empty() {
                ui.add_space(10.0);
                ui.strong("Members");
                semantic_view::draw_semantic_members(
                    ui,
                    &object.children,
                    reference.document_version,
                    self.semantic.selected.as_ref(),
                    &mut selected_member,
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_rename = self.language_server.ready
                    && object.id.domain == "rust"
                    && object
                        .id
                        .document
                        .extension()
                        .is_some_and(|extension| extension == "rs");
                if ui
                    .add_enabled(can_rename, egui::Button::new("Rename…"))
                    .on_hover_text("Preview a Rust Analyzer rename across open workspace files")
                    .clicked()
                {
                    self.semantic.rename_target = Some(reference.clone());
                    self.semantic.rename_input.clone_from(&object.name);
                }
                if ui.button("Go to declaration").clicked() {
                    self.navigate_to_workspace_location(
                        &object.id.document,
                        object.selection_range.start.line as usize,
                        object.selection_range.start.character as usize,
                    );
                }
                let can_query_references = self.language_server.ready
                    && self.editor.documents.contains_key(&object.id.document)
                    && object
                        .id
                        .document
                        .extension()
                        .is_some_and(|extension| extension == "rs");
                if ui
                    .add_enabled(can_query_references, egui::Button::new("Find references"))
                    .on_hover_text("Find references to this declaration")
                    .clicked()
                {
                    self.request_lsp_at_position(
                        &object.id.document,
                        serde_json::json!({
                            "line": object.selection_range.start.line,
                            "character": object.selection_range.start.character
                        }),
                        "textDocument/references",
                        LspRequest::References,
                        Some(reference.clone()),
                    );
                }
            });
            if self.semantic.rename_target.as_ref() == Some(&reference) {
                let mut submit_rename = false;
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.semantic.rename_input)
                            .hint_text("New Rust identifier")
                            .desired_width(150.0),
                    );
                    if ui.button("Preview").clicked() {
                        submit_rename = true;
                    }
                    if ui.small_button("Cancel").clicked() {
                        self.semantic.rename_target = None;
                    }
                });
                if submit_rename {
                    let new_name = self.semantic.rename_input.clone();
                    self.request_lsp_rename(reference.clone(), &new_name);
                }
            }
            if let Some((path, line, character)) = diagnostic_navigation {
                self.navigate_to_workspace_location(&path, line, character);
            }
            if let Some((path, line, character)) = related_navigation {
                self.navigate_to_workspace_location(&path, line, character);
            }
            if let Some(diagnostic) = quick_fix_request {
                self.request_diagnostic_code_actions(&object.id.document, &diagnostic);
            }
            if let Some(member) = selected_member {
                if self.semantic.selected.as_ref() != Some(&member) {
                    self.language_server.references.clear();
                    self.language_server.references_omitted = 0;
                    self.semantic.rename_target = None;
                }
                self.semantic.selected = Some(member.clone());
                if let Some(range) = reference_range(&self.semantic.index, &member) {
                    self.navigate_to_workspace_location(
                        &member.id.document,
                        range.start.line as usize,
                        range.start.character as usize,
                    );
                }
            }
            ui.separator();
        }
        if let Some(path) = &self.editor.selected_file {
            ui.label("File");
            ui.monospace(path.display().to_string());
            ui.add_space(12.0);
            ui.label("Workspace");
            if let Some(root) = &self.workspace.root {
                ui.monospace(root.display().to_string());
            }
            ui.add_space(12.0);
            ui.label("Language tooling");
            ui.label(&self.language_server.status);
            ui.label("Rust Analyzer runs through the shared Vapor service and pinned toolchain.");
            ui.label(format!(
                "{} workspace problems",
                self.semantic.problems.count()
            ));
            if let Some(hover) = &self.language_server.hover {
                ui.add_space(8.0);
                ui.label("Hover");
                ui.monospace(hover);
            }
            if !self.language_server.references.is_empty() {
                ui.add_space(8.0);
                ui.label(if self.language_server.references_omitted == 0 {
                    format!("References | {}", self.language_server.references.len())
                } else {
                    format!(
                        "References | {} shown · {} omitted",
                        self.language_server.references.len(),
                        self.language_server.references_omitted
                    )
                });
                let mut selected_reference = None;
                egui::ScrollArea::vertical()
                    .id_salt("ide-references")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for location in &self.language_server.references {
                            if ui
                                .selectable_label(
                                    false,
                                    format!(
                                        "{}:{}:{}",
                                        location.path.display(),
                                        location.line.saturating_add(1),
                                        location.character.saturating_add(1)
                                    ),
                                )
                                .clicked()
                            {
                                selected_reference = Some(location.clone());
                            }
                        }
                    });
                if let Some(location) = selected_reference {
                    self.navigate_to_workspace_location(
                        &location.path,
                        location.line.saturating_sub(1),
                        location.character.saturating_sub(1),
                    );
                }
            }
        } else {
            ui.label("Select a file to inspect it.");
        }
    }
}
