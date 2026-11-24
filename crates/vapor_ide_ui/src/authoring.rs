use super::{
    Document, IdeApp, PendingAction, WorkspaceSourceProject, canonical_rust_item_kind,
    canonical_rust_module_parent, rust_semantics,
};
use eframe::egui;
use std::collections::BTreeSet;
use vapor_client_domain::{CanonicalRustModuleKind, ContentKind};
use vapor_client_protocol::{
    AuthoringAction, ClientAction, WorkshopMetadataDraft, WorkspaceAction,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ContentWizardStep {
    #[default]
    ChooseType,
    Configure,
    Review,
}

impl ContentWizardStep {
    fn next(self) -> Self {
        match self {
            Self::ChooseType => Self::Configure,
            Self::Configure => Self::Review,
            Self::Review => Self::Review,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::ChooseType => Self::ChooseType,
            Self::Configure => Self::ChooseType,
            Self::Review => Self::Configure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentTemplateGroup {
    Pack,
    Content,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RustFieldParameter {
    name: String,
    ty: String,
}

fn rust_field_source(fields: &[RustFieldParameter]) -> String {
    fields
        .iter()
        .map(|field| format!("{}: {}", field.name.trim(), field.ty.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn canonical_module_target(
    parent: &std::path::Path,
    kind: CanonicalRustModuleKind,
) -> Option<std::path::PathBuf> {
    let file_name = parent.file_name()?.to_str()?;
    let parent_directory = parent.parent()?;
    let module_directory = match file_name {
        "lib.rs" | "main.rs" | "mod.rs" => parent_directory.to_path_buf(),
        _ => parent_directory.join(parent.file_stem()?),
    };
    Some(module_directory.join(format!("{}.rs", kind.name())))
}

fn content_template_group(kind: ContentKind) -> ContentTemplateGroup {
    match kind {
        ContentKind::Packagepack
        | ContentKind::Enginepack
        | ContentKind::Gamepack
        | ContentKind::Modpack => ContentTemplateGroup::Pack,
        ContentKind::Engine
        | ContentKind::Game
        | ContentKind::EngineMod
        | ContentKind::GameMod
        | ContentKind::ExtensionMod
        | ContentKind::Crate => ContentTemplateGroup::Content,
    }
}

fn content_template_description(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Packagepack => "Playable composition root; may carry its built executable.",
        ContentKind::Enginepack => "Manifest bundling an engine with selected engine extensions.",
        ContentKind::Gamepack => "Manifest bundling a game with selected game extensions.",
        ContentKind::Modpack => "Curated additions for a particular game and engine stack.",
        ContentKind::Engine => "Reusable engine implementation and its compiled artifact.",
        ContentKind::Game => "Game implementation built on an engine and its APIs.",
        ContentKind::EngineMod => "Reusable engine-level capability or API extension.",
        ContentKind::GameMod => "Reusable gameplay content built on a game API.",
        ContentKind::ExtensionMod => "A mod that extends another mod.",
        ContentKind::Crate => {
            "Rust library compiled for static composition; no Vapor runtime role."
        }
    }
}

#[derive(Default)]
pub(super) struct WorkspaceAuthoring {
    pub(super) new_file_dialog: bool,
    pub(super) new_file_path: String,
    new_rust_item_dialog: bool,
    new_rust_item_name: String,
    new_rust_item_fields: Vec<RustFieldParameter>,
    pub(super) new_rust_module_dialog: bool,
    pub(super) new_rust_module_kind: CanonicalRustModuleKind,
    pub(super) new_content_dialog: bool,
    new_content_step: ContentWizardStep,
    new_content_kind: String,
    new_content_id: String,
    new_content_version: String,
    new_content_selected_dependencies: BTreeSet<String>,
    new_content_dependencies: String,
}

impl IdeApp {
    pub(super) fn create_semantic_rust_item(&mut self) {
        let Some(path) = self.editor.selected_file.clone() else {
            self.workspace.status =
                "Open a canonical Rust module before creating an item.".to_owned();
            return;
        };
        if canonical_rust_item_kind(&path, &self.workspace.rust_crate_roots).is_none() {
            self.workspace.status = "Open a canonical Rust module under a Cargo `src` directory before creating an item.".to_owned();
            return;
        }
        let Some(document) = self.editor.documents.get(&path) else {
            self.workspace.status =
                "Open a canonical Rust module before creating an item.".to_owned();
            return;
        };
        let field_source = rust_field_source(&self.interface.authoring.new_rust_item_fields);
        let updated = match rust_semantics::render_canonical_rust_item_with_fields(
            &path,
            &document.contents,
            &self.interface.authoring.new_rust_item_name,
            &field_source,
        ) {
            Ok(updated) => updated,
            Err(error) => {
                self.workspace.status = error;
                return;
            }
        };
        let Some(document) = self.editor.documents.get_mut(&path) else {
            return;
        };
        let Some(version) = document.version.checked_add(1) else {
            self.workspace.status =
                "This file has reached the editor revision limit; close and reopen it.".to_owned();
            return;
        };
        document.contents = updated.clone();
        document.version = version;
        self.did_change_document(&path, updated, version);
        self.interface.authoring.new_rust_item_dialog = false;
        self.interface.authoring.new_rust_item_name.clear();
        self.interface.authoring.new_rust_item_fields.clear();
        self.workspace.status =
            "Rust item added to the open file. Save to write it to disk.".to_owned();
    }

    pub(super) fn open_semantic_rust_item_dialog(&mut self) {
        let Some(path) = self.editor.selected_file.as_deref() else {
            self.workspace.status =
                "Open a canonical Rust module before creating an item.".to_owned();
            return;
        };
        let Some(kind) = canonical_rust_item_kind(path, &self.workspace.rust_crate_roots) else {
            self.workspace.status =
                "Open a canonical Rust module before creating an item.".to_owned();
            return;
        };
        self.interface.authoring.new_rust_item_name = kind.default_name().to_owned();
        self.interface.authoring.new_rust_item_fields.clear();
        self.interface.authoring.new_rust_item_dialog = true;
    }

    pub(super) fn open_canonical_rust_module_dialog(&mut self) {
        let Some(path) = self.editor.selected_file.as_ref() else {
            self.workspace.status =
                "Open a Rust module under a Cargo `src` directory first.".to_owned();
            return;
        };
        if !canonical_rust_module_parent(path, &self.workspace.rust_crate_roots) {
            self.workspace.status =
                "Open a Rust module under a Cargo `src` directory first.".to_owned();
            return;
        }
        self.interface.authoring.new_rust_module_kind = CanonicalRustModuleKind::Components;
        self.interface.authoring.new_rust_module_dialog = true;
    }

    pub(super) fn create_canonical_rust_module(&mut self) {
        if self.editor.documents.values().any(Document::is_dirty) {
            self.workspace.status = "Save open files before creating a Rust module.".to_owned();
            return;
        }
        let Some(parent_path) = self.editor.selected_file.clone() else {
            self.workspace.status = "Open the parent Rust module first.".to_owned();
            return;
        };
        let Some(parent) = self.editor.documents.get(&parent_path) else {
            self.workspace.status = "Wait for the parent Rust module to finish opening.".to_owned();
            return;
        };
        if !canonical_rust_module_parent(&parent_path, &self.workspace.rust_crate_roots) {
            self.workspace.status =
                "Canonical Rust modules belong inside a Cargo crate's `src` directory.".to_owned();
            return;
        }
        let expected_parent_fingerprint = parent.fingerprint.clone();
        self.send(
            ClientAction::Workspace(WorkspaceAction::CreateCanonicalRustModule {
                parent_path: parent_path.to_string_lossy().into_owned(),
                expected_parent_fingerprint: expected_parent_fingerprint.clone(),
                module_kind: self.interface.authoring.new_rust_module_kind,
            }),
            PendingAction::CreateCanonicalRustModule {
                parent_path,
                expected_parent_fingerprint,
            },
        );
        if self.workspace.pending.is_some() {
            self.interface.authoring.new_rust_module_dialog = false;
        }
    }

    pub(super) fn create_workspace_file(&mut self) {
        let path = self.interface.authoring.new_file_path.trim();
        if path.is_empty() {
            self.workspace.status = "Enter a file name inside the project.".to_owned();
            return;
        }
        if self.editor.documents.values().any(Document::is_dirty) {
            self.workspace.status = "Save your open files before creating a file.".to_owned();
            return;
        }
        self.send(
            ClientAction::Workspace(WorkspaceAction::CreateFile {
                path: path.to_owned(),
                contents: String::new(),
            }),
            PendingAction::CreateFile,
        );
        self.interface.authoring.new_file_dialog = false;
    }

    pub(super) fn open_vapor_content_dialog(&mut self) {
        if self.workspace.root.is_none() {
            self.workspace.status = "Open a Vapor workspace before creating content.".to_owned();
            return;
        }
        self.interface.authoring.new_content_kind = ContentKind::Game.as_str().to_owned();
        self.interface.authoring.new_content_step = ContentWizardStep::ChooseType;
        self.interface.authoring.new_content_id.clear();
        self.interface.authoring.new_content_version = "0.5.0".to_owned();
        self.interface
            .authoring
            .new_content_selected_dependencies
            .clear();
        self.interface.authoring.new_content_dependencies.clear();
        self.interface.authoring.new_content_dialog = true;
    }

    pub(super) fn create_vapor_content(&mut self) {
        if self.editor.documents.values().any(Document::is_dirty) {
            self.workspace.status =
                "Save open files before Vapor updates the workspace for new content.".to_owned();
            return;
        }
        let Some(root) = self.workspace.root.as_deref() else {
            self.workspace.status = "Open a Vapor workspace before creating content.".to_owned();
            return;
        };
        let action = match vapor_content_creation_action(
            root,
            &self.interface.authoring.new_content_kind,
            &self.interface.authoring.new_content_id,
            &self.interface.authoring.new_content_version,
            &self.interface.authoring.new_content_selected_dependencies,
            &self.interface.authoring.new_content_dependencies,
        ) {
            Ok(action) => action,
            Err(error) => {
                self.workspace.status = error;
                return;
            }
        };
        self.send(action, PendingAction::CreateContent);
        if self.workspace.pending.is_some() {
            self.interface.authoring.new_content_dialog = false;
        }
    }

    pub(super) fn draw_authoring_dialogs(&mut self, context: &egui::Context) {
        if self.interface.authoring.new_rust_module_dialog {
            let mut open = true;
            egui::Window::new("New canonical Rust module")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut open)
                .show(context, |ui| {
                    if let Some(parent) = &self.editor.selected_file {
                        ui.strong(format!("Child of {}", parent.display()));
                        if let Some(target) = canonical_module_target(
                            parent,
                            self.interface.authoring.new_rust_module_kind,
                        ) {
                            ui.weak("New source file");
                            ui.monospace(target.display().to_string());
                        }
                    }
                    ui.label("Creates the module file and registers it from this parent.");
                    egui::ComboBox::from_label("Module")
                        .selected_text(format!(
                            "{}.rs",
                            self.interface.authoring.new_rust_module_kind.name()
                        ))
                        .show_ui(ui, |ui| {
                            for kind in CanonicalRustModuleKind::ALL {
                                ui.selectable_value(
                                    &mut self.interface.authoring.new_rust_module_kind,
                                    kind,
                                    format!("{}.rs", kind.name()),
                                );
                            }
                        });
                    ui.collapsing("Module template", |ui| {
                        ui.label(
                            self.interface
                                .authoring
                                .new_rust_module_kind
                                .documentation(),
                        );
                        ui.weak("Vapor creates this fixed module category and adds its declaration to the parent file.");
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                self.workspace.pending.is_none(),
                                egui::Button::new("Create module"),
                            )
                            .clicked()
                        {
                            self.create_canonical_rust_module();
                        }
                        if ui.button("Cancel").clicked() {
                            self.interface.authoring.new_rust_module_dialog = false;
                        }
                    });
                });
            self.interface.authoring.new_rust_module_dialog =
                open && self.interface.authoring.new_rust_module_dialog;
        }
        if self.interface.authoring.new_rust_item_dialog {
            let mut open = true;
            let field_source = rust_field_source(&self.interface.authoring.new_rust_item_fields);
            let preview_result = self.editor.selected_file.as_deref().map(|path| {
                rust_semantics::render_canonical_rust_item_with_fields(
                    path,
                    "",
                    &self.interface.authoring.new_rust_item_name,
                    &field_source,
                )
            });
            let preview = preview_result
                .as_ref()
                .and_then(|result| result.as_ref().ok());
            let name_error = preview_result
                .as_ref()
                .and_then(|result| result.as_ref().err());
            egui::Window::new("New Rust item")
                .collapsible(false)
                .resizable(true)
                .default_size([560.0, 420.0])
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut open)
                .show(context, |ui| {
                    if let Some(path) = self.editor.selected_file.as_ref()
                        && let Some(kind) =
                            canonical_rust_item_kind(path, &self.workspace.rust_crate_roots)
                    {
                        ui.strong(format!("{} in {}", kind.label(), path.display()));
                    }
                    ui.label("Choose the name parameter for this fixed Rust template. The preview does not change your file.");
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.interface.authoring.new_rust_item_name,
                        )
                        .hint_text("Rust identifier")
                        .desired_width(340.0),
                    );
                    let supports_fields = self
                        .editor
                        .selected_file
                        .as_deref()
                        .and_then(|path| {
                            canonical_rust_item_kind(path, &self.workspace.rust_crate_roots)
                        })
                        .is_some_and(
                            rust_semantics::CanonicalRustItemKind::supports_named_fields,
                        );
                    if supports_fields {
                        ui.label("Optional named fields for this data template.");
                        let mut remove_field = None;
                        for (index, field) in self
                            .interface
                            .authoring
                            .new_rust_item_fields
                            .iter_mut()
                            .enumerate()
                        {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut field.name)
                                        .hint_text("field")
                                        .desired_width(160.0),
                                );
                                ui.label(":");
                                ui.add(
                                    egui::TextEdit::singleline(&mut field.ty)
                                        .hint_text("Rust type")
                                        .desired_width(200.0),
                                );
                                if ui.small_button("Remove").clicked() {
                                    remove_field = Some(index);
                                }
                            });
                        }
                        if let Some(index) = remove_field {
                            self.interface.authoring.new_rust_item_fields.remove(index);
                        }
                        if ui.small_button("Add field").clicked() {
                            self.interface
                                .authoring
                                .new_rust_item_fields
                                .push(RustFieldParameter::default());
                        }
                    }
                    if let Some(error) = name_error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    }
                    ui.add_space(8.0);
                    ui.strong("Generated Rust");
                    if let Some(preview) = preview {
                        let mut preview_text = preview.clone();
                        ui.add(
                            egui::TextEdit::multiline(&mut preview_text)
                                .code_editor()
                                .desired_rows(8)
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    } else {
                        ui.label("Enter a valid name to preview this template.");
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(preview.is_some(), egui::Button::new("Insert into file"))
                            .on_hover_text("Insert into the open buffer; saving remains explicit")
                            .clicked()
                        {
                            self.create_semantic_rust_item();
                        }
                        if ui.button("Cancel").clicked() {
                            self.interface.authoring.new_rust_item_dialog = false;
                            self.interface.authoring.new_rust_item_name.clear();
                            self.interface.authoring.new_rust_item_fields.clear();
                        }
                    });
                });
            if !open {
                self.interface.authoring.new_rust_item_dialog = false;
            }
        }
        if self.interface.authoring.new_file_dialog {
            egui::Window::new("Create a file")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(context, |ui| {
                    ui.label("Create an empty file inside this project. Use New Rust item for canonical declarations.");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.interface.authoring.new_file_path)
                            .hint_text("src/example.rs")
                            .desired_width(340.0),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.workspace.pending.is_none(), egui::Button::new("Create"))
                            .clicked()
                        {
                            self.create_workspace_file();
                        }
                        if ui.button("Cancel").clicked() {
                            self.interface.authoring.new_file_dialog = false;
                        }
                    });
                });
        }
        if self.interface.authoring.new_content_dialog {
            let mut open = true;
            let mut create_requested = false;
            let mut step_back = false;
            let mut step_forward = false;
            let step = self.interface.authoring.new_content_step;
            let selected_kind = self
                .interface
                .authoring
                .new_content_kind
                .parse::<ContentKind>()
                .unwrap_or(ContentKind::Game);
            let draft_validation = self.workspace.root.as_deref().map(|root| {
                vapor_content_creation_action(
                    root,
                    &self.interface.authoring.new_content_kind,
                    &self.interface.authoring.new_content_id,
                    &self.interface.authoring.new_content_version,
                    &self.interface.authoring.new_content_selected_dependencies,
                    &self.interface.authoring.new_content_dependencies,
                )
            });
            egui::Window::new("New Vapor content")
                .collapsible(false)
                .resizable(true)
                .default_size([700.0, 560.0])
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut open)
                .show(context, |ui| {
                    ui.horizontal(|ui| {
                        for (index, (title, wizard_step)) in [
                            ("1  Type", ContentWizardStep::ChooseType),
                            ("2  Configure", ContentWizardStep::Configure),
                            ("3  Review", ContentWizardStep::Review),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let current = step == wizard_step;
                            ui.label(egui::RichText::new(title).strong().color(if current {
                                vapor_gui_core::theme::IDE_PALETTE.accent
                            } else {
                                vapor_gui_core::theme::IDE_PALETTE.muted_text
                            }));
                            if index < 2 {
                                ui.weak(">");
                            }
                        }
                    });
                    ui.separator();
                    match step {
                        ContentWizardStep::ChooseType => {
                            ui.heading("Choose what you are authoring");
                            ui.label("Packs describe selections. Content provides implementations and compiled artifacts.");
                            ui.add_space(10.0);
                            let previous_kind = self.interface.authoring.new_content_kind.clone();
                            egui::ScrollArea::vertical()
                                .id_salt("ide-content-template-catalog")
                                .show(ui, |ui| {
                                    for (group, title, guidance) in [
                                        (
                                            ContentTemplateGroup::Pack,
                                            "Packs",
                                            "A pack is a typed manifest. Packagepack is the runnable root; the other packs describe bundles of content.",
                                        ),
                                        (
                                            ContentTemplateGroup::Content,
                                            "Content",
                                            "Content supplies reusable implementations. Vapor builds its artifacts with the pinned toolchain.",
                                        ),
                                    ] {
                                        ui.heading(title);
                                        ui.weak(guidance);
                                        ui.add_space(4.0);
                                        let kinds = ContentKind::ALL
                                            .into_iter()
                                            .filter(|kind| content_template_group(*kind) == group)
                                            .collect::<Vec<_>>();
                                        egui::Grid::new(("ide-content-template-grid", title))
                                            .num_columns(2)
                                            .spacing([12.0, 8.0])
                                            .show(ui, |ui| {
                                                for (index, kind) in kinds.iter().enumerate() {
                                                    let selected = self.interface.authoring.new_content_kind == kind.as_str();
                                                    egui::Frame::new()
                                                        .fill(if selected { vapor_gui_core::theme::IDE_PALETTE.accent_soft } else { vapor_gui_core::theme::IDE_PALETTE.panel })
                                                        .stroke(egui::Stroke::new(1.0, if selected { vapor_gui_core::theme::IDE_PALETTE.accent } else { vapor_gui_core::theme::IDE_PALETTE.border }))
                                                        .corner_radius(egui::CornerRadius::same(5))
                                                        .inner_margin(egui::Margin::same(10))
                                                        .show(ui, |ui| {
                                                            ui.set_min_width((ui.available_width() * 0.45).max(180.0));
                                                            ui.selectable_value(&mut self.interface.authoring.new_content_kind, kind.as_str().to_owned(), egui::RichText::new(kind.as_str()).strong());
                                                            ui.weak(content_template_description(*kind));
                                                        });
                                                    if index % 2 == 1 {
                                                        ui.end_row();
                                                    }
                                                }
                                            });
                                        ui.add_space(12.0);
                                    }
                                });
                            if self.interface.authoring.new_content_kind != previous_kind {
                                self.interface.authoring.new_content_selected_dependencies.clear();
                            }
                        }
                        ContentWizardStep::Configure => {
                            ui.heading(format!("Configure {}", selected_kind.as_str()));
                            ui.label("Vapor creates the canonical source project and registers it in this workspace. Creating it never starts a build.");
                    ui.label("Vapor ID");
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.interface.authoring.new_content_id,
                        )
                        .hint_text("workspace-id/new-content")
                        .desired_width(f32::INFINITY),
                    );
                    ui.weak("Use this workspace's Vapor ID prefix, followed by one new name.");
                    ui.horizontal(|ui| {
                        ui.label("Version");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.interface.authoring.new_content_version,
                            )
                            .hint_text("0.5.0")
                            .desired_width(120.0),
                        );
                    });
                    let selected_kind = self
                        .interface
                        .authoring
                        .new_content_kind
                        .parse::<ContentKind>()
                        .ok();
                    let eligible = selected_kind.map_or_else(Vec::new, |kind| {
                        compatible_source_projects(
                            kind,
                            &self.workspace.content.source_projects,
                        )
                    });
                    ui.label("Required dependencies");
                    if eligible.is_empty() {
                        ui.weak("No compatible local source projects are registered here.");
                    } else {
                        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                        egui::ScrollArea::vertical()
                            .max_height(132.0)
                            .show_rows(ui, row_height, eligible.len(), |ui, rows| {
                                for index in rows {
                                    let Some((selector, label)) = eligible.get(index) else {
                                        continue;
                                    };
                                    let mut checked = self
                                        .interface
                                        .authoring
                                        .new_content_selected_dependencies
                                        .contains(selector);
                                    if ui.checkbox(&mut checked, label).changed() {
                                        if checked {
                                            self.interface
                                                .authoring
                                                .new_content_selected_dependencies
                                                .insert(selector.clone());
                                        } else {
                                            self.interface
                                                .authoring
                                                .new_content_selected_dependencies
                                                .remove(selector);
                                        }
                                    }
                                }
                            });
                    }
                    ui.collapsing("Advanced selectors", |ui| {
                        ui.label("One required Vapor selector per line.");
                        ui.add(
                            egui::TextEdit::multiline(
                                &mut self.interface.authoring.new_content_dependencies,
                            )
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                        );
                    });
                        }
                        ContentWizardStep::Review => {
                            ui.heading("Review source template");
                            ui.label("This is a preview of the requested content identity and dependencies. No files change until you create it.");
                            ui.add_space(12.0);
                            egui::Grid::new("ide-content-review")
                                .num_columns(2)
                                .spacing([18.0, 8.0])
                                .show(ui, |ui| {
                                    ui.weak("Type"); ui.label(selected_kind.as_str()); ui.end_row();
                                    ui.weak("Vapor ID"); ui.label(self.interface.authoring.new_content_id.trim()); ui.end_row();
                                    ui.weak("Version"); ui.label(self.interface.authoring.new_content_version.trim()); ui.end_row();
                                    ui.weak("Required local projects"); ui.label(self.interface.authoring.new_content_selected_dependencies.len().to_string()); ui.end_row();
                                });
                            let dependencies = self.interface.authoring.new_content_selected_dependencies.iter().cloned().chain(self.interface.authoring.new_content_dependencies.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned)).collect::<Vec<_>>();
                            if !dependencies.is_empty() {
                                ui.add_space(10.0);
                                ui.weak("Dependency selectors");
                                for dependency in dependencies {
                                    ui.label(dependency);
                                }
                            }
                            ui.add_space(12.0);
                            ui.weak("No compile or build is started by this action.");
                            if let Some(Err(error)) = &draft_validation {
                                ui.colored_label(egui::Color32::LIGHT_RED, error);
                            }
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.interface.authoring.new_content_dialog = false;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if step == ContentWizardStep::Review {
                                if ui.add_enabled(self.workspace.pending.is_none() && draft_validation.as_ref().is_some_and(Result::is_ok), egui::Button::new("Create content").fill(vapor_gui_core::theme::IDE_PALETTE.accent_soft)).clicked() {
                                    create_requested = true;
                                }
                            } else if ui.button("Continue").clicked() {
                                step_forward = true;
                            }
                            if step != ContentWizardStep::ChooseType && ui.button("Back").clicked() {
                                step_back = true;
                            }
                        });
                    });
                });
            if !open {
                self.interface.authoring.new_content_dialog = false;
            }
            if create_requested {
                self.create_vapor_content();
            }
            if step_forward {
                self.interface.authoring.new_content_step = step.next();
            }
            if step_back {
                self.interface.authoring.new_content_step = step.previous();
            }
        }
    }
}

#[cfg(test)]
mod content_wizard_tests {
    use super::ContentWizardStep;

    #[test]
    fn fixed_template_wizard_moves_forward_and_back_without_mutation() {
        assert_eq!(ContentWizardStep::default(), ContentWizardStep::ChooseType);
        assert_eq!(
            ContentWizardStep::ChooseType.next(),
            ContentWizardStep::Configure
        );
        assert_eq!(
            ContentWizardStep::Configure.next(),
            ContentWizardStep::Review
        );
        assert_eq!(
            ContentWizardStep::Review.previous(),
            ContentWizardStep::Configure
        );
        assert_eq!(
            ContentWizardStep::Configure.previous(),
            ContentWizardStep::ChooseType
        );
    }

    #[test]
    fn review_is_the_last_step() {
        assert_eq!(ContentWizardStep::Review.next(), ContentWizardStep::Review);
    }
}

fn vapor_content_creation_action(
    workspace_root: &std::path::Path,
    kind: &str,
    id: &str,
    version: &str,
    selected_dependencies: &BTreeSet<String>,
    dependencies: &str,
) -> Result<ClientAction, String> {
    let kind = kind
        .parse::<ContentKind>()
        .map_err(|_| "Choose one of Vapor's supported content types.".to_owned())?;
    let id = id.trim();
    if id.is_empty() {
        return Err("Enter the new content's full Vapor ID.".to_owned());
    }
    let version = version.trim();
    if version.is_empty() {
        return Err("Enter a content version.".to_owned());
    }
    let dependencies = selected_dependencies
        .iter()
        .cloned()
        .chain(
            dependencies
                .lines()
                .map(str::trim)
                .filter(|dependency| !dependency.is_empty())
                .map(str::to_owned),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(ClientAction::Authoring(AuthoringAction::CreateContent {
        workspace_root: workspace_root.to_string_lossy().into_owned(),
        kind: kind.as_str().to_owned(),
        id: id.to_owned(),
        version: version.to_owned(),
        dependencies,
        optional_dependencies: Vec::new(),
        workshop: WorkshopMetadataDraft::default(),
    }))
}

fn compatible_source_projects(
    kind: ContentKind,
    projects: &[WorkspaceSourceProject],
) -> Vec<(String, String)> {
    let mut projects = projects
        .iter()
        .filter_map(|project| {
            let project_kind = project.kind.parse::<ContentKind>().ok()?;
            vapor_client_domain::dependency_kind_allowed(kind, project_kind).then(|| {
                let selector = project.version.as_ref().map_or_else(
                    || project.id.clone(),
                    |version| format!("{}@{version}", project.id),
                );
                (selector, format!("{} · {}", project.id, project.kind))
            })
        })
        .collect::<Vec<_>>();
    projects.sort_by(|left, right| left.1.cmp(&right.1));
    projects
}

#[cfg(test)]
mod tests {
    use super::{
        ContentTemplateGroup, RustFieldParameter, canonical_module_target,
        compatible_source_projects, content_template_description, content_template_group,
        rust_field_source, vapor_content_creation_action,
    };
    use std::collections::BTreeSet;
    use vapor_client_domain::ContentKind;
    use vapor_client_protocol::{AuthoringAction, ClientAction, WorkshopMetadataDraft};

    fn source_project(
        id: &str,
        kind: &str,
        version: &str,
    ) -> vapor_client_protocol::WorkspaceSourceProject {
        vapor_client_protocol::WorkspaceSourceProject {
            id: id.to_owned(),
            version: Some(version.to_owned()),
            kind: kind.to_owned(),
            root: format!("/workspace/{}", id.rsplit('/').next().unwrap()),
            manifest: format!(
                "/workspace/{}/{}.vapor.toml",
                id.rsplit('/').next().unwrap(),
                kind
            ),
            binaries: Vec::new(),
            libraries: Vec::new(),
            dependencies: Vec::new(),
            workshop: Default::default(),
        }
    }

    #[test]
    fn content_creation_dependency_picker_uses_vapor_compatibility_and_pinned_versions() {
        let projects = vec![
            source_project("ghf-studios/demo/engine", "engine", "0.5.0"),
            source_project("ghf-studios/demo/crate", "crate", "0.5.0"),
            source_project("ghf-studios/demo/mod", "game-mod", "0.5.0"),
        ];
        assert_eq!(
            compatible_source_projects(ContentKind::Game, &projects),
            vec![
                (
                    "ghf-studios/demo/crate@0.5.0".to_owned(),
                    "ghf-studios/demo/crate · crate".to_owned()
                ),
                (
                    "ghf-studios/demo/engine@0.5.0".to_owned(),
                    "ghf-studios/demo/engine · engine".to_owned()
                )
            ]
        );
        assert_eq!(
            compatible_source_projects(ContentKind::Crate, &projects).len(),
            1
        );
    }

    #[test]
    fn fixed_content_catalog_distinguishes_packs_from_implementation_content() {
        let packs = ContentKind::ALL
            .into_iter()
            .filter(|kind| content_template_group(*kind) == ContentTemplateGroup::Pack)
            .collect::<Vec<_>>();
        let content = ContentKind::ALL
            .into_iter()
            .filter(|kind| content_template_group(*kind) == ContentTemplateGroup::Content)
            .collect::<Vec<_>>();

        assert_eq!(
            packs,
            [
                ContentKind::Packagepack,
                ContentKind::Enginepack,
                ContentKind::Gamepack,
                ContentKind::Modpack,
            ]
        );
        assert_eq!(content.len(), 6);
        assert_eq!(packs.len() + content.len(), ContentKind::ALL.len());
        assert!(
            ContentKind::ALL
                .into_iter()
                .all(|kind| !content_template_description(kind).is_empty())
        );
        assert!(content_template_description(ContentKind::Packagepack).contains("Playable"));
        assert!(content_template_description(ContentKind::Crate).contains("Rust library"));
    }

    #[test]
    fn rust_template_fields_are_composed_from_explicit_parameters() {
        let fields = vec![
            RustFieldParameter {
                name: " health ".to_owned(),
                ty: " f32 ".to_owned(),
            },
            RustFieldParameter::default(),
            RustFieldParameter {
                name: "position".to_owned(),
                ty: "Vec3".to_owned(),
            },
        ];

        assert_eq!(
            rust_field_source(&fields),
            "health: f32\n: \nposition: Vec3"
        );
    }

    #[test]
    fn canonical_module_preview_matches_rust_module_directory_rules() {
        use vapor_client_domain::CanonicalRustModuleKind;

        assert_eq!(
            canonical_module_target(
                std::path::Path::new("/workspace/src/lib.rs"),
                CanonicalRustModuleKind::Systems,
            ),
            Some(std::path::PathBuf::from("/workspace/src/systems.rs"))
        );
        assert_eq!(
            canonical_module_target(
                std::path::Path::new("/workspace/src/player/mod.rs"),
                CanonicalRustModuleKind::Components,
            ),
            Some(std::path::PathBuf::from(
                "/workspace/src/player/components.rs"
            ))
        );
        assert_eq!(
            canonical_module_target(
                std::path::Path::new("/workspace/src/player.rs"),
                CanonicalRustModuleKind::Systems,
            ),
            Some(std::path::PathBuf::from("/workspace/src/player/systems.rs"))
        );
    }

    #[test]
    fn vapor_content_creation_uses_typed_role_and_normalizes_dependency_lines() {
        let selected = ["ghf-studios/example/engine@0.5.0".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(
            vapor_content_creation_action(
                std::path::Path::new("/workspace"),
                "game-mod",
                "ghf-studios/example/bonus-mod",
                "0.5.0",
                &selected,
                "\n ghf-studios/example/game@^0.5 \n\n",
            ),
            Ok(ClientAction::Authoring(AuthoringAction::CreateContent {
                workspace_root: "/workspace".to_owned(),
                kind: "game-mod".to_owned(),
                id: "ghf-studios/example/bonus-mod".to_owned(),
                version: "0.5.0".to_owned(),
                dependencies: vec![
                    "ghf-studios/example/engine@0.5.0".to_owned(),
                    "ghf-studios/example/game@^0.5".to_owned()
                ],
                optional_dependencies: Vec::new(),
                workshop: WorkshopMetadataDraft::default(),
            }))
        );
    }

    #[test]
    fn vapor_content_creation_rejects_missing_identity_or_version() {
        assert!(
            vapor_content_creation_action(
                std::path::Path::new("/workspace"),
                "game",
                "",
                "0.5.0",
                &BTreeSet::new(),
                "",
            )
            .is_err()
        );
        assert!(
            vapor_content_creation_action(
                std::path::Path::new("/workspace"),
                "game",
                "ghf-studios/example/game",
                "",
                &BTreeSet::new(),
                "",
            )
            .is_err()
        );
    }
}
