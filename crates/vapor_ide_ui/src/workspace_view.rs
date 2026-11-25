use super::*;

#[derive(Default)]
pub(super) struct WorkspaceSymbolNode<'a> {
    pub(super) declarations: Vec<&'a SemanticObject>,
    pub(super) children: BTreeMap<String, WorkspaceSymbolNode<'a>>,
}

type WorkspaceSymbolFiles<'a> = BTreeMap<PathBuf, Vec<&'a SemanticObject>>;

pub(super) fn workspace_symbol_files_by_crate<'a>(
    symbols: &'a [SemanticObject],
    crate_roots: &BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, WorkspaceSymbolFiles<'a>> {
    let mut crates = BTreeMap::new();
    for symbol in symbols {
        let file = &symbol.id.document;
        let crate_root = crate_roots
            .iter()
            .filter(|root| file.starts_with(root))
            .max_by_key(|root| root.components().count())
            .cloned()
            .unwrap_or_else(|| PathBuf::from("Other Rust files"));
        crates
            .entry(crate_root)
            .or_insert_with(BTreeMap::new)
            .entry(file.clone())
            .or_insert_with(Vec::new)
            .push(symbol);
    }
    crates
}

pub(super) fn workspace_symbol_tree<'a>(
    symbols: impl IntoIterator<Item = &'a SemanticObject>,
) -> WorkspaceSymbolNode<'a> {
    let mut root = WorkspaceSymbolNode::default();
    for symbol in symbols {
        let mut node = &mut root;
        for segment in &symbol.id.path {
            node = node.children.entry(segment.clone()).or_default();
        }
        node.declarations.push(symbol);
    }
    root
}

fn draw_workspace_symbol_tree(
    ui: &mut egui::Ui,
    node: &WorkspaceSymbolNode<'_>,
    selected: Option<&vapor_gui_core::semantic::SemanticObjectId>,
    requested: &mut Option<SemanticObject>,
) {
    for (name, child) in &node.children {
        if child.children.is_empty() {
            for declaration in &child.declarations {
                let label = format!("{}  {}", declaration.kind.label(), declaration.name);
                let response = ui.selectable_label(
                    selected.is_some_and(|selected| selected == &declaration.id),
                    label,
                );
                response.clone().on_hover_text(format!(
                    "{}:{} · {}",
                    declaration.id.document.display(),
                    declaration.range.start.line.saturating_add(1),
                    declaration.id.qualified_name(),
                ));
                if response.clicked() {
                    *requested = Some((*declaration).clone());
                }
            }
            continue;
        }

        let suffix = child
            .declarations
            .first()
            .map_or_else(String::new, |declaration| {
                format!("  ·  {}", declaration.kind.label())
            });
        egui::CollapsingHeader::new(format!("{name}{suffix}"))
            .id_salt(("ide-workspace-symbol-node", name))
            .default_open(false)
            .show(ui, |ui| {
                for declaration in &child.declarations {
                    let label = format!("{}  {}", declaration.kind.label(), declaration.name);
                    let response = ui.selectable_label(
                        selected.is_some_and(|selected| selected == &declaration.id),
                        label,
                    );
                    response.clone().on_hover_text(format!(
                        "{}:{} · {}",
                        declaration.id.document.display(),
                        declaration.range.start.line.saturating_add(1),
                        declaration.id.qualified_name(),
                    ));
                    if response.clicked() {
                        *requested = Some((*declaration).clone());
                    }
                }
                draw_workspace_symbol_tree(ui, child, selected, requested);
            });
    }
}

impl IdeApp {
    pub(super) fn header(&mut self, ui: &mut egui::Ui) {
        let mut run_action = None;
        let mut run_project = false;
        let mut create_file = false;
        let mut create_rust_item = false;
        let mut create_rust_module = false;
        let mut create_vapor_content = false;
        let mut open_project = false;
        let mut save_requested = false;
        let mut save_all_requested = false;
        let mut close_file = None;
        let mut find_requested = false;
        let mut find_next = None;
        let dirty_count = self
            .editor
            .documents
            .values()
            .filter(|document| document.is_dirty())
            .count();
        let has_unsaved_edits = dirty_count > 0;
        let project_action = if self.workspace.root.is_some() {
            "Switch project"
        } else {
            "Open project"
        };
        let layout = layout_mode(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open project…").clicked() {
                    open_project = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.workspace.root.is_some() && self.workspace.pending.is_none(),
                        egui::Button::new("New empty file…"),
                    )
                    .clicked()
                {
                    create_file = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.workspace.root.is_some()
                            && self.workspace.pending.is_none()
                            && !has_unsaved_edits,
                        egui::Button::new("New Vapor content…"),
                    )
                    .on_disabled_hover_text(
                        "Save open files and open a workspace before creating content.",
                    )
                    .clicked()
                {
                    create_vapor_content = true;
                    ui.close();
                }
                let canonical_module_open =
                    self.editor.selected_file.as_deref().is_some_and(|path| {
                        canonical_rust_item_kind(path, &self.workspace.rust_crate_roots).is_some()
                    });
                if ui
                    .add_enabled(
                        canonical_module_open && self.workspace.pending.is_none(),
                        egui::Button::new("New Rust item…"),
                    )
                    .on_disabled_hover_text("Open a canonical Rust module first.")
                    .clicked()
                {
                    create_rust_item = true;
                    ui.close();
                }
                let rust_module_parent_open = self
                    .editor
                    .selected_file
                    .as_deref()
                    .is_some_and(|path| {
                        canonical_rust_module_parent(path, &self.workspace.rust_crate_roots)
                    });
                if ui
                    .add_enabled(
                        rust_module_parent_open
                            && self.workspace.pending.is_none()
                            && !has_unsaved_edits,
                        egui::Button::new("New canonical Rust module…"),
                    )
                    .on_disabled_hover_text("Open and save a Rust source file under a Cargo crate's `src` directory first.")
                    .clicked()
                {
                    create_rust_module = true;
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        self.workspace.pending.is_none() && dirty_count > 0,
                        egui::Button::new("Save"),
                    )
                    .clicked()
                {
                    save_requested = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.workspace.pending.is_none() && dirty_count > 0,
                        egui::Button::new("Save all"),
                    )
                    .clicked()
                {
                    save_all_requested = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.editor.selected_file.is_some() && self.workspace.pending.is_none(),
                        egui::Button::new("Close file"),
                    )
                    .clicked()
                {
                    close_file = self.editor.selected_file.clone();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(
                        self.editor.selected_file.is_some(),
                        egui::Button::new("Find…"),
                    )
                    .clicked()
                {
                    find_requested = true;
                    ui.close();
                }
                if ui
                    .add_enabled(self.editor.find_open, egui::Button::new("Find next"))
                    .clicked()
                {
                    find_next = Some(false);
                    ui.close();
                }
                if ui
                    .add_enabled(self.editor.find_open, egui::Button::new("Find previous"))
                    .clicked()
                {
                    find_next = Some(true);
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                if layout == LayoutMode::Compact {
                    ui.weak("Compact view");
                    for (view, label) in [
                        (CompactView::Explorer, "Explorer"),
                        (CompactView::Editor, "Editor"),
                        (CompactView::Inspector, "Inspector"),
                    ] {
                        ui.selectable_value(
                            &mut self.interface.workbench.compact_view,
                            view,
                            label,
                        );
                    }
                    ui.separator();
                } else {
                    ui.checkbox(&mut self.interface.workbench.explorer_visible, "Explorer");
                }
                ui.checkbox(&mut self.interface.workbench.inspector_visible, "Inspector");
                ui.checkbox(&mut self.interface.workbench.bottom_visible, "Bottom panel");
            });
            ui.menu_button("Run", |ui| {
                if ui
                    .add_enabled(
                        self.workspace.root.is_some()
                            && self.workspace.pending.is_none()
                            && !has_unsaved_edits,
                        egui::Button::new("Run project"),
                    )
                    .clicked()
                {
                    run_project = true;
                    ui.close();
                }
                ui.separator();
                for (label, operation) in [
                    ("Resolve Cargo lockfile", WorkspaceToolOperation::ResolveLockfile),
                    ("Check", WorkspaceToolOperation::Check),
                    ("Test", WorkspaceToolOperation::Test),
                    ("Build", WorkspaceToolOperation::Build),
                    ("Format", WorkspaceToolOperation::Format),
                ] {
                    if ui
                        .add_enabled(
                            self.workspace.root.is_some() && self.workspace.pending.is_none(),
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
                    self.interface.about_open = true;
                    ui.close();
                }
                ui.weak(&self.language_server.status);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.strong("VAPOR IDE");
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.horizontal(|ui| {
                if let Some(root) = &self.workspace.root {
                    ui.strong(root.file_name().unwrap_or_default().to_string_lossy())
                        .on_hover_text(root.display().to_string());
                } else {
                    ui.weak("No workspace open");
                }
            });
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.workspace.root.is_some()
                            && self.workspace.pending.is_none()
                            && !has_unsaved_edits,
                        egui::Button::new(egui::RichText::new("Run").strong()),
                    )
                    .on_disabled_hover_text(if self.workspace.pending.is_some() {
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
                        self.workspace.pending.is_none() && dirty_count > 0,
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
                    .add_enabled(
                        self.workspace.pending.is_none(),
                        egui::Button::new(project_action),
                    )
                    .clicked()
                {
                    self.workspace.project_switcher_open = true;
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
            self.workspace.project_switcher_open = true;
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
            self.editor.find_open = true;
            self.editor.focus_find_query = true;
        }
        if let Some(previous) = find_next {
            self.editor.find_navigation = Some(previous);
        }
        if create_file {
            self.interface.authoring.new_file_dialog = true;
        }
        if create_rust_item {
            self.open_semantic_rust_item_dialog();
        }
        if create_rust_module {
            self.open_canonical_rust_module_dialog();
        }
        if create_vapor_content {
            self.open_vapor_content_dialog();
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
        let discovered = self.service.client.as_ref().map(|service| {
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
                                                        self.workspace.pending.is_none(),
                                                        egui::Button::new("Open project"),
                                                    )
                                                    .clicked()
                                                {
                                                    self.workspace.input = repository.root.clone();
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
        if !self.workspace.project_switcher_open {
            return;
        }
        let discovered = self.service.client.as_ref().map(|service| {
            let snapshot = &service.latest_snapshot().superworkspace;
            (
                snapshot.root.clone(),
                snapshot.repositories.clone(),
                snapshot.error.clone(),
            )
        });
        let can_switch = self.workspace.pending.is_none()
            && !self.editor.documents.values().any(Document::is_dirty);
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
                        if self.workspace.pending.is_some() {
                            "Wait for the current operation to finish before switching."
                        } else {
                            "Save open files before switching projects."
                        },
                    );
                    if self.editor.documents.values().any(Document::is_dirty)
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
                            egui::TextEdit::singleline(&mut self.workspace.project_query)
                                .hint_text("Find a project"),
                        );
                        let query = self.workspace.project_query.trim().to_lowercase();
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
                            egui::TextEdit::singleline(&mut self.workspace.input)
                                .hint_text("/path/to/project")
                                .desired_width(ui.available_width()),
                        );
                        if ui
                            .add_enabled(can_switch, egui::Button::new("Open folder"))
                            .clicked()
                        {
                            requested_workspace = nonempty_path(&self.workspace.input);
                        }
                    });
            });
        self.workspace.project_switcher_open = window_open && requested_workspace.is_none();
        if let Some(root) = requested_workspace {
            self.workspace.input = root.display().to_string();
            self.open_workspace(root);
            self.workspace.project_switcher_open = false;
        }
    }

    pub(super) fn draw_files(&mut self, ui: &mut egui::Ui) {
        vapor_gui_core::theme::tool_window_header(
            ui,
            "Explorer",
            Some(if self.semantic.explorer_symbols {
                "Structure"
            } else {
                "Files"
            }),
        );
        ui.horizontal(|ui| {
            if ui
                .selectable_label(!self.semantic.explorer_symbols, "Files")
                .clicked()
            {
                self.semantic.explorer_symbols = false;
                self.semantic.pending_workspace_symbol = None;
                self.request_workspace_symbols("");
            }
            if ui
                .selectable_label(self.semantic.explorer_symbols, "Structure")
                .clicked()
            {
                self.semantic.explorer_symbols = true;
            }
        });
        let search = if self.semantic.explorer_symbols {
            ui.horizontal(|ui| {
                let clear_width = if self.semantic.structure_query.is_empty() {
                    0.0
                } else {
                    56.0
                };
                let width = (ui.available_width() - clear_width).max(80.0);
                let response = ui.add_sized(
                    [width, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut self.semantic.structure_query)
                        .hint_text("Search declarations"),
                );
                if !self.semantic.structure_query.is_empty() && ui.small_button("Clear").clicked() {
                    self.semantic.structure_query.clear();
                }
                response
            })
            .inner
        } else {
            ui.horizontal(|ui| {
                let clear_width = if self.workspace.file_query.is_empty() {
                    0.0
                } else {
                    56.0
                };
                let width = (ui.available_width() - clear_width).max(80.0);
                let response = ui.add_sized(
                    [width, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut self.workspace.file_query)
                        .hint_text("Filter files · Ctrl/Cmd+P"),
                );
                if !self.workspace.file_query.is_empty() && ui.small_button("Clear").clicked() {
                    self.workspace.file_query.clear();
                }
                response
            })
            .inner
        };
        if self.workspace.focus_file_query && !self.semantic.explorer_symbols {
            search.request_focus();
            self.workspace.focus_file_query = false;
        }
        let filter = if self.semantic.explorer_symbols {
            self.semantic.structure_query.trim().to_lowercase()
        } else {
            self.workspace.file_query.trim().to_lowercase()
        };
        if self.semantic.explorer_symbols {
            self.semantic
                .match_cache
                .prepare(&self.semantic.index, &filter);
            let symbol_query = self.semantic.structure_query.trim().to_owned();
            self.request_workspace_symbols(&symbol_query);
            let mut requested_symbol = None;
            let mut requested_workspace_symbol = None;
            let mut outline_budget = semantic_view::SemanticRenderBudget::default();
            egui::ScrollArea::vertical()
                .id_salt("ide-semantic-symbol-tree")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(if filter.is_empty() {
                            "Workspace declarations"
                        } else {
                            "Matching declarations"
                        });
                        ui.weak(format!("{}", self.language_server.workspace_symbols.len()));
                    });
                    if !self.language_server.ready {
                        ui.weak("Rust Analyzer is starting; workspace structure will appear when it is ready.");
                    } else if let Some(error) = &self.language_server.workspace_symbol_error {
                        ui.colored_label(
                            egui::Color32::LIGHT_RED,
                            format!("Rust Analyzer could not load workspace structure: {error}"),
                        );
                    } else if self.language_server.workspace_symbol_query.as_deref()
                        != Some(self.semantic.structure_query.trim())
                        || self.language_server.pending_requests.values().any(|request| {
                            request.request == LspRequest::WorkspaceSymbols
                                && request.query.as_deref()
                                    == Some(self.semantic.structure_query.trim())
                        })
                    {
                        ui.weak(if filter.is_empty() {
                            "Loading declarations from the whole Rust workspace…"
                        } else {
                            "Searching workspace declarations…"
                        });
                    } else if self.language_server.workspace_symbols.is_empty() {
                        ui.weak(if filter.is_empty() {
                            "Rust Analyzer found no workspace declarations. Check that this project contains Rust crates."
                        } else {
                            "No workspace declarations match this search."
                        });
                    } else {
                        let symbols_by_crate = workspace_symbol_files_by_crate(
                            &self.language_server.workspace_symbols,
                            &self.workspace.rust_crate_roots,
                        );
                        for (crate_root, files) in symbols_by_crate {
                            let active_crate = self.editor.selected_file.as_ref().is_some_and(
                                |selected| selected.starts_with(&crate_root),
                            );
                            let declaration_count = files.values().map(Vec::len).sum::<usize>();
                            let crate_label = crate_root.file_name().map_or_else(
                                || "Other Rust files".to_owned(),
                                |name| name.to_string_lossy().into_owned(),
                            );
                            egui::CollapsingHeader::new(format!(
                                "{crate_label}  ·  {} files · {declaration_count} declarations",
                                files.len(),
                            ))
                            .id_salt(("ide-workspace-symbol-crate", &crate_root))
                            .default_open(active_crate)
                            .show(ui, |ui| {
                                ui.weak(format!("Cargo crate · {}", crate_root.display()));
                                for (path, symbols) in files {
                                    let selected_file = self.editor.selected_file.as_deref()
                                        == Some(path.as_path());
                                    let file_label =
                                        file_display_label(&path, &self.workspace.files);
                                    let symbol_tree = workspace_symbol_tree(symbols.iter().copied());
                                    let file = egui::CollapsingHeader::new(format!(
                                        "{}  ·  {} declarations",
                                        file_label,
                                        symbols.len(),
                                    ))
                                    .id_salt(("ide-workspace-symbol-file", &path))
                                    .default_open(selected_file)
                                    .show(ui, |ui| {
                                        draw_workspace_symbol_tree(
                                            ui,
                                            &symbol_tree,
                                            self.semantic
                                                .selected
                                                .as_ref()
                                                .map(|selected| &selected.id),
                                            &mut requested_workspace_symbol,
                                        );
                                    });
                                    file.header_response
                                        .on_hover_text(path.display().to_string());
                                }
                            });
                        }
                    }
                    let outline_document_count = self.semantic.index
                        .documents()
                        .filter(|document| semantic_view::has_semantic_outline(&document.objects))
                        .count();
                    ui.separator();
                    egui::CollapsingHeader::new(format!(
                        "Open-file outlines · {outline_document_count}"
                    ))
                    .id_salt("ide-open-file-outlines")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.weak(semantic_view::semantic_explorer_guidance(
                            outline_document_count,
                            self.language_server.ready,
                        ));
                        if outline_document_count == 0 {
                            ui.weak("Open a supported file to inspect its detailed local structure.");
                        }
                        for document in self.semantic.index.documents() {
                            egui::CollapsingHeader::new(document.path.display().to_string())
                                .id_salt(("ide-symbol-file", &document.path))
                                .default_open(semantic_document_default_open(
                                    &document.path,
                                    self.editor.selected_file.as_deref(),
                                ))
                                .show(ui, |ui| {
                                    if !semantic_view::has_semantic_outline(&document.objects) {
                                        ui.weak("No semantic outline is available for this document.");
                                    } else {
                                        semantic_view::draw_semantic_outline(
                                            ui,
                                            &document.objects,
                                            document.version,
                                            self.semantic.match_cache
                                                .get(&document.path)
                                                .expect("open semantic documents have cached filters"),
                                            self.semantic.selected.as_ref(),
                                            &mut requested_symbol,
                                            &mut outline_budget,
                                        );
                                    }
                                });
                        }
                    });
                    if outline_budget.was_truncated() {
                        ui.weak(format!(
                            "Outline display is capped at {} visible objects. Filter the structure to reach more matches.",
                            semantic_view::MAX_VISIBLE_SEMANTIC_OBJECTS,
                        ));
                    }
                });
            if let Some(reference) = requested_symbol {
                self.semantic.pending_workspace_symbol = None;
                if self.semantic.selected.as_ref() != Some(&reference) {
                    self.language_server.references.clear();
                    self.language_server.references_omitted = 0;
                    self.semantic.rename_target = None;
                }
                self.semantic.selected = Some(reference.clone());
                let position = reference_range(&self.semantic.index, &reference)
                    .unwrap_or_default()
                    .start;
                self.navigate_to_workspace_location(
                    &reference.id.document,
                    position.line as usize,
                    position.character as usize,
                );
            }
            if let Some(symbol) = requested_workspace_symbol {
                self.semantic.selected = None;
                self.interface.workbench.inspector_visible = true;
                self.navigate_to_workspace_symbol(&symbol);
            }
        } else {
            let mut requested_file = None;
            egui::ScrollArea::vertical()
                .id_salt("ide-project-tree")
                .show(ui, |ui| {
                    if self.workspace.file_tree.matching_file_count(&filter) == 0 {
                        ui.weak(if filter.is_empty() {
                            "No files in this project."
                        } else {
                            "No files match this filter."
                        });
                    } else {
                        draw_file_tree(
                            ui,
                            &self.workspace.file_tree,
                            Path::new(""),
                            &FileTreeView {
                                files: &self.workspace.files,
                                documents: &self.editor.documents,
                                selected: self.editor.selected_file.as_ref(),
                                enabled: self.workspace.pending.is_none(),
                                filter: &filter,
                                rust_crate_roots: &self.workspace.rust_crate_roots,
                            },
                            &mut requested_file,
                        );
                    }
                });
            if let Some(path) = requested_file {
                self.semantic.selected = None;
                self.semantic.pending_workspace_symbol = None;
                self.open_file(path);
            }
        }
    }
}
