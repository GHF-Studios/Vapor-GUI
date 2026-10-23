use super::*;

impl IdeApp {
    pub(super) fn header(&mut self, ui: &mut egui::Ui) {
        let mut run_action = None;
        let mut run_project = false;
        let mut create_file = false;
        let mut create_rust_item = false;
        let mut open_project = false;
        let mut save_requested = false;
        let mut save_all_requested = false;
        let mut close_file = None;
        let mut find_requested = false;
        let mut find_next = None;
        let dirty_count = self
            .documents
            .values()
            .filter(|document| document.is_dirty())
            .count();
        let has_unsaved_edits = dirty_count > 0;
        let project_action = if self.workspace.is_some() {
            "Switch project"
        } else {
            "Open project"
        };
        ui.horizontal(|ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open project…").clicked() {
                    open_project = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.workspace.is_some() && self.pending.is_none(),
                        egui::Button::new("New empty file…"),
                    )
                    .clicked()
                {
                    create_file = true;
                    ui.close();
                }
                let canonical_module_open = self.selected_file.as_deref()
                    .is_some_and(|path| canonical_rust_item_kind(path, &self.rust_crate_roots).is_some());
                if ui.add_enabled(
                    canonical_module_open && self.pending.is_none(),
                    egui::Button::new("New Rust item…"),
                ).on_disabled_hover_text("Open a canonical Rust module first.").clicked() {
                    create_rust_item = true;
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        self.pending.is_none() && dirty_count > 0,
                        egui::Button::new("Save"),
                    )
                    .clicked()
                {
                    save_requested = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.pending.is_none() && dirty_count > 0,
                        egui::Button::new("Save all"),
                    )
                    .clicked()
                {
                    save_all_requested = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.selected_file.is_some() && self.pending.is_none(),
                        egui::Button::new("Close file"),
                    )
                    .clicked()
                {
                    close_file = self.selected_file.clone();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(self.selected_file.is_some(), egui::Button::new("Find…"))
                    .clicked()
                {
                    find_requested = true;
                    ui.close();
                }
                if ui
                    .add_enabled(self.find_open, egui::Button::new("Find next"))
                    .clicked()
                {
                    find_next = Some(false);
                    ui.close();
                }
                if ui
                    .add_enabled(self.find_open, egui::Button::new("Find previous"))
                    .clicked()
                {
                    find_next = Some(true);
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.inspector_open, "Inspector");
                ui.checkbox(&mut self.bottom_open, "Bottom panel");
            });
            ui.menu_button("Run", |ui| {
                if ui
                    .add_enabled(
                        self.workspace.is_some() && self.pending.is_none() && !has_unsaved_edits,
                        egui::Button::new("Run project"),
                    )
                    .clicked()
                {
                    run_project = true;
                    ui.close();
                }
                ui.separator();
                for (label, operation) in [
                    ("Check", WorkspaceToolOperation::Check),
                    ("Test", WorkspaceToolOperation::Test),
                    ("Build", WorkspaceToolOperation::Build),
                    ("Format", WorkspaceToolOperation::Format),
                ] {
                    if ui
                        .add_enabled(
                            self.workspace.is_some() && self.pending.is_none(),
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        run_action = Some(operation);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About Vapor IDE").clicked() {
                    self.about_open = true;
                    ui.close();
                }
                ui.weak(&self.language_server_status);
            });
            ui.separator();
            ui.strong("Vapor IDE");
            ui.separator();
            if let Some(root) = &self.workspace {
                ui.strong(root.file_name().unwrap_or_default().to_string_lossy());
            } else {
                ui.weak("Open a project to start coding");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.workspace.is_some() && self.pending.is_none() && !has_unsaved_edits,
                        egui::Button::new(egui::RichText::new("Run").strong()),
                    )
                    .on_disabled_hover_text(if self.pending.is_some() {
                        "Wait for the current operation to finish."
                    } else if has_unsaved_edits {
                        "Save your open files before running the project."
                    } else {
                        "Open a project to run its default Vapor configuration."
                    })
                    .clicked()
                {
                    self.run_default_configuration();
                }
                if ui
                    .add_enabled(
                        self.pending.is_none() && dirty_count > 0,
                        egui::Button::new(if dirty_count > 1 {
                            format!("Save all ({dirty_count})")
                        } else {
                            "Save".to_owned()
                        }),
                    )
                    .clicked()
                {
                    if dirty_count > 1 {
                        self.save_all();
                    } else {
                        self.save_file();
                    }
                }
                if ui
                    .add_enabled(self.pending.is_none(), egui::Button::new(project_action))
                    .clicked()
                {
                    self.project_switcher_open = true;
                }
            });
        });
        if let Some(operation) = run_action {
            self.run_workspace_tool(operation);
        }
        if run_project {
            self.run_default_configuration();
        }
        if open_project {
            self.project_switcher_open = true;
        }
        if save_requested {
            self.save_file();
        }
        if save_all_requested {
            self.save_all();
        }
        if let Some(path) = close_file {
            self.close_document(&path);
        }
        if find_requested {
            self.find_open = true;
            self.focus_find_query = true;
        }
        if let Some(previous) = find_next {
            self.find_navigation = Some(previous);
        }
        if create_file {
            self.authoring.new_file_dialog = true;
        }
        if create_rust_item {
            self.open_semantic_rust_item_dialog();
        }
        ui.separator();
    }

    pub(super) fn draw_welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.heading("Your development workspace");
            ui.label("Open a project from your Vapor Superworkspace, or choose a folder by path.");
        });
        ui.add_space(20.0);
        let discovered = self.service.as_ref().map(|service| {
            let snapshot = &service.latest_snapshot().superworkspace;
            (
                snapshot.root.clone(),
                snapshot.repositories.clone(),
                snapshot.error.clone(),
            )
        });
        match discovered {
            Some((Some(root), repositories, error)) => {
                ui.horizontal(|ui| {
                    ui.strong("Superworkspace");
                    ui.weak(root);
                });
                ui.add_space(8.0);
                if let Some(error) = error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                } else if repositories.is_empty() {
                    ui.label("No project folders are registered yet. Choose a folder to open one directly.");
                } else {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for repository in repositories {
                            egui::Frame::group(ui.style())
                                .inner_margin(egui::Margin::symmetric(10, 7))
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.vertical(|ui| {
                                            ui.strong(&repository.name);
                                            ui.weak(&repository.kind);
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add_enabled(
                                                        self.pending.is_none(),
                                                        egui::Button::new("Open project"),
                                                    )
                                                    .clicked()
                                                {
                                                    self.workspace_input = repository.root.clone();
                                                    self.open_workspace(PathBuf::from(
                                                        repository.root,
                                                    ));
                                                }
                                            },
                                        );
                                    });
                                });
                        }
                    });
                }
            }
            Some((None, _, Some(error))) => {
                ui.colored_label(egui::Color32::YELLOW, error);
            }
            _ => {
                ui.label("Connect to Vapor to discover your projects.");
            }
        }
    }

    pub(super) fn draw_project_switcher(&mut self, context: &egui::Context) {
        if !self.project_switcher_open {
            return;
        }
        let discovered = self.service.as_ref().map(|service| {
            let snapshot = &service.latest_snapshot().superworkspace;
            (
                snapshot.root.clone(),
                snapshot.repositories.clone(),
                snapshot.error.clone(),
            )
        });
        let can_switch = self.pending.is_none() && !self.documents.values().any(Document::is_dirty);
        let mut window_open = true;
        let mut requested_workspace = None;
        egui::Window::new("Open a project")
            .id(egui::Id::new("ide-project-switcher"))
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 480.0])
            .open(&mut window_open)
            .show(context, |ui| {
                if !can_switch {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        if self.pending.is_some() {
                            "Wait for the current operation to finish before switching."
                        } else {
                            "Save open files before switching projects."
                        },
                    );
                    if self.documents.values().any(Document::is_dirty)
                        && ui.button("Save all open files").clicked()
                    {
                        self.save_all();
                    }
                    ui.separator();
                }
                match &discovered {
                    Some((Some(root), repositories, error)) => {
                        ui.horizontal(|ui| {
                            ui.strong("Superworkspace");
                            ui.weak(root);
                        });
                        if let Some(error) = error {
                            ui.colored_label(egui::Color32::LIGHT_RED, error);
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut self.project_query)
                                .hint_text("Find a project"),
                        );
                        let query = self.project_query.trim().to_lowercase();
                        egui::ScrollArea::vertical()
                            .max_height(280.0)
                            .show(ui, |ui| {
                                for repository in repositories.iter().filter(|repository| {
                                    query.is_empty()
                                        || repository.name.to_lowercase().contains(&query)
                                        || repository.kind.to_lowercase().contains(&query)
                                        || repository.root.to_lowercase().contains(&query)
                                }) {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.strong(&repository.name);
                                            ui.weak(&repository.kind);
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add_enabled(
                                                        can_switch,
                                                        egui::Button::new("Open"),
                                                    )
                                                    .clicked()
                                                {
                                                    requested_workspace =
                                                        Some(PathBuf::from(&repository.root));
                                                }
                                            },
                                        );
                                    });
                                    ui.weak(&repository.root);
                                    ui.separator();
                                }
                            });
                    }
                    Some((None, _, Some(error))) => {
                        ui.colored_label(egui::Color32::YELLOW, error);
                    }
                    _ => {
                        ui.weak("Connect to Vapor to discover Superworkspace projects.");
                    }
                }
                ui.separator();
                egui::CollapsingHeader::new("Open a folder by path")
                    .default_open(discovered.is_none())
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.workspace_input)
                                .hint_text("/path/to/project")
                                .desired_width(ui.available_width()),
                        );
                        if ui
                            .add_enabled(can_switch, egui::Button::new("Open folder"))
                            .clicked()
                        {
                            requested_workspace = nonempty_path(&self.workspace_input);
                        }
                    });
            });
        self.project_switcher_open = window_open && requested_workspace.is_none();
        if let Some(root) = requested_workspace {
            self.workspace_input = root.display().to_string();
            self.open_workspace(root);
            self.project_switcher_open = false;
        }
    }

    pub(super) fn draw_files(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Explorer");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.explorer_symbols {
                    ui.weak(format!("{} open", self.semantic_index.documents().count()));
                } else {
                    ui.weak(format!("{} files", self.file_tree.matching_file_count("")));
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.selectable_label(!self.explorer_symbols, "Files").clicked() {
                self.explorer_symbols = false;
            }
            if ui.selectable_label(self.explorer_symbols, "Structure").clicked() {
                self.explorer_symbols = true;
            }
        });
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.file_query)
                .hint_text(if self.explorer_symbols { "Search Rust workspace + open-file structure" } else { "Filter files | Ctrl/Cmd+P" }),
        );
        if self.focus_file_query {
            search.request_focus();
            self.focus_file_query = false;
        }
        if !self.file_query.is_empty() && ui.small_button("Clear").clicked() {
            self.file_query.clear();
        }
        let filter = self.file_query.trim().to_lowercase();
        if self.explorer_symbols {
            self.semantic_match_cache
                .prepare(&self.semantic_index, &filter);
            let symbol_query = self.file_query.trim().to_owned();
            self.request_workspace_symbols(&symbol_query);
            let mut requested_symbol = None;
            let mut requested_location = None;
            let mut outline_budget = semantic_view::SemanticRenderBudget::default();
            egui::ScrollArea::vertical()
                .id_salt("ide-semantic-symbol-tree")
                .show(ui, |ui| {
                    if !filter.is_empty() {
                        ui.strong("Workspace Rust declarations");
                        if !self.language_server_ready {
                            ui.weak("Rust Analyzer will search the project when it is ready.");
                        } else if self.workspace_symbol_query.as_deref()
                            != Some(self.file_query.trim())
                        {
                            ui.weak("Searching workspace symbols…");
                        } else if self.workspace_symbols.is_empty() {
                            ui.weak("No workspace symbols match this search.");
                        }
                        for object in &self.workspace_symbols {
                            let label = format!("{}  {}", object.kind.label(), object.name);
                            let response = ui.selectable_label(false, label);
                            response.clone().on_hover_text(format!(
                                "{} · {}:{}",
                                object.id.qualified_name(),
                                object.id.document.display(),
                                object.range.start.line.saturating_add(1),
                            ));
                            if response.clicked() {
                                requested_location = Some((
                                    object.id.document.clone(),
                                    object.range.start.line as usize,
                                    object.range.start.character as usize,
                                ));
                            }
                        }
                    }
                    let outline_document_count = self
                        .semantic_index
                        .documents()
                        .filter(|document| semantic_view::has_semantic_outline(&document.objects))
                        .count();
                    ui.weak(semantic_view::semantic_explorer_guidance(
                        outline_document_count,
                        self.language_server_ready,
                    ));
                    if !filter.is_empty() {
                        ui.separator();
                        ui.strong("Open-file outlines");
                    }
                    if outline_document_count == 0 {
                        ui.weak("No open-file outlines yet.");
                    }
                    for document in self.semantic_index.documents() {
                        egui::CollapsingHeader::new(document.path.display().to_string())
                            .id_salt(("ide-symbol-file", &document.path))
                            .default_open(true)
                            .show(ui, |ui| {
                                if !semantic_view::has_semantic_outline(&document.objects) {
                                    ui.weak("No semantic outline is available for this document.");
                                } else {
                                    semantic_view::draw_semantic_outline(
                                        ui,
                                        &document.objects,
                                        document.version,
                                        self.semantic_match_cache
                                            .get(&document.path)
                                            .expect("open semantic documents have cached filters"),
                                        self.selected_semantic.as_ref(),
                                        &mut requested_symbol,
                                        &mut outline_budget,
                                    );
                                }
                            });
                    }
                    if outline_budget.was_truncated() {
                        ui.weak(format!(
                            "Outline display is capped at {} visible objects. Filter the structure to reach more matches.",
                            semantic_view::MAX_VISIBLE_SEMANTIC_OBJECTS,
                        ));
                    }
                });
            if let Some(reference) = requested_symbol {
                if self.selected_semantic.as_ref() != Some(&reference) {
                    self.references.clear();
                    self.references_omitted = 0;
                    self.rename_target = None;
                }
                self.selected_semantic = Some(reference.clone());
                let position = reference_range(&self.semantic_index, &reference).unwrap_or_default().start;
                self.navigate_to_workspace_location(
                    &reference.id.document,
                    position.line as usize,
                    position.character as usize,
                );
            }
            if let Some((path, line, character)) = requested_location {
                self.selected_semantic = None;
                self.navigate_to_workspace_location(&path, line, character);
            }
        } else {
            let mut requested_file = None;
            egui::ScrollArea::vertical()
                .id_salt("ide-project-tree")
                .show(ui, |ui| {
                    if self.file_tree.matching_file_count(&filter) == 0 {
                        ui.weak(if filter.is_empty() {
                            "No files in this project."
                        } else {
                            "No files match this filter."
                        });
                    } else {
                        draw_file_tree(
                            ui,
                            &self.file_tree,
                            Path::new(""),
                            0,
                            &FileTreeView {
                                files: &self.files,
                                documents: &self.documents,
                                selected: self.selected_file.as_ref(),
                                enabled: self.pending.is_none(),
                                filter: &filter,
                                rust_crate_roots: &self.rust_crate_roots,
                            },
                            &mut requested_file,
                        );
                    }
                });
            if let Some(path) = requested_file {
                self.selected_semantic = None;
                self.open_file(path);
            }
        }
    }
}
