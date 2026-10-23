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

#[derive(Default)]
pub(super) struct WorkspaceAuthoring {
    pub(super) new_file_dialog: bool,
    pub(super) new_file_path: String,
    new_rust_item_dialog: bool,
    new_rust_item_name: String,
    pub(super) new_rust_module_dialog: bool,
    pub(super) new_rust_module_kind: CanonicalRustModuleKind,
    pub(super) new_content_dialog: bool,
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
        let updated = match rust_semantics::render_canonical_rust_item(
            &path,
            &document.contents,
            &self.interface.authoring.new_rust_item_name,
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
            egui::Window::new("New Rust item")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut open)
                .show(context, |ui| {
                    if let Some(path) = self.editor.selected_file.as_ref()
                        && let Some(kind) =
                            canonical_rust_item_kind(path, &self.workspace.rust_crate_roots)
                    {
                        ui.strong(format!("{} in {}", kind.label(), path.display()));
                    }
                    ui.label("The template edits this open buffer. Save writes it to disk.");
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.interface.authoring.new_rust_item_name,
                        )
                        .hint_text("Rust identifier")
                        .desired_width(340.0),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Insert into file").clicked() {
                            self.create_semantic_rust_item();
                        }
                        if ui.button("Cancel").clicked() {
                            self.interface.authoring.new_rust_item_dialog = false;
                            self.interface.authoring.new_rust_item_name.clear();
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
            egui::Window::new("New Vapor content")
                .collapsible(false)
                .resizable(true)
                .default_width(420.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut open)
                .show(context, |ui| {
                    ui.label("Vapor creates the canonical source project and registers it in this workspace.");
                    ui.label("No build runs. Edit the generated manifest and Rust source afterward.");
                    let selected_kind = self
                        .interface
                        .authoring
                        .new_content_kind
                        .parse::<ContentKind>()
                        .unwrap_or(ContentKind::Game);
                    let previous_kind = self.interface.authoring.new_content_kind.clone();
                    egui::ComboBox::from_label("Content type")
                        .selected_text(selected_kind.as_str())
                        .show_ui(ui, |ui| {
                            for kind in ContentKind::ALL {
                                ui.selectable_value(
                                    &mut self.interface.authoring.new_content_kind,
                                    kind.as_str().to_owned(),
                                    kind.as_str(),
                                );
                            }
                        });
                    if self.interface.authoring.new_content_kind != previous_kind {
                        self.interface
                            .authoring
                            .new_content_selected_dependencies
                            .clear();
                    }
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
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                self.workspace.pending.is_none(),
                                egui::Button::new("Create content"),
                            )
                            .clicked()
                        {
                            create_requested = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.interface.authoring.new_content_dialog = false;
                        }
                    });
                });
            if !open {
                self.interface.authoring.new_content_dialog = false;
            }
            if create_requested {
                self.create_vapor_content();
            }
        }
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
    use super::{compatible_source_projects, vapor_content_creation_action};
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
