use super::{Document, IdeApp, PendingAction, canonical_rust_item_kind, rust_semantics};
use eframe::egui;
use vapor_client_protocol::{ClientAction, WorkspaceAction};

#[derive(Default)]
pub(super) struct WorkspaceAuthoring {
    pub(super) new_file_dialog: bool,
    pub(super) new_file_path: String,
    new_rust_item_dialog: bool,
    new_rust_item_name: String,
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
    }
}
