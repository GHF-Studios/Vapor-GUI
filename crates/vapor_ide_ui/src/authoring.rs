use super::{Document, IdeApp, PendingAction, canonical_rust_item_kind, rust_semantics};
use eframe::egui;
use vapor_client_domain::ContentKind;
use vapor_client_protocol::{
    AuthoringAction, ClientAction, WorkshopMetadataDraft, WorkspaceAction,
};

#[derive(Default)]
pub(super) struct WorkspaceAuthoring {
    pub(super) new_file_dialog: bool,
    pub(super) new_file_path: String,
    new_rust_item_dialog: bool,
    new_rust_item_name: String,
    pub(super) new_content_dialog: bool,
    new_content_kind: String,
    new_content_id: String,
    new_content_version: String,
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
                    ui.label("Required dependencies (one Vapor selector per line; optional).");
                    ui.add(
                        egui::TextEdit::multiline(
                            &mut self.interface.authoring.new_content_dependencies,
                        )
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                    );
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
    let dependencies = dependencies
        .lines()
        .map(str::trim)
        .filter(|dependency| !dependency.is_empty())
        .map(str::to_owned)
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

#[cfg(test)]
mod tests {
    use super::vapor_content_creation_action;
    use vapor_client_protocol::{AuthoringAction, ClientAction, WorkshopMetadataDraft};

    #[test]
    fn vapor_content_creation_uses_typed_role_and_normalizes_dependency_lines() {
        assert_eq!(
            vapor_content_creation_action(
                std::path::Path::new("/workspace"),
                "game-mod",
                "ghf-studios/example/bonus-mod",
                "0.5.0",
                "\n ghf-studios/example/game@^0.5 \n\n",
            ),
            Ok(ClientAction::Authoring(AuthoringAction::CreateContent {
                workspace_root: "/workspace".to_owned(),
                kind: "game-mod".to_owned(),
                id: "ghf-studios/example/bonus-mod".to_owned(),
                version: "0.5.0".to_owned(),
                dependencies: vec!["ghf-studios/example/game@^0.5".to_owned()],
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
                "",
            )
            .is_err()
        );
    }
}
