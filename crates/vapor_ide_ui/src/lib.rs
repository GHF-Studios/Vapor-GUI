use eframe::egui;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use vapor_client::{Client, ClientHostConfig, service_sibling_path};
use vapor_client_protocol::{
    ClientAction, ClientEvent, OperationStatus, ProtocolFrame, WorkspaceAction,
    WorkspaceFileResult, WorkspaceProjectSelection, WorkspaceToolOperation,
};

#[derive(Clone)]
struct Document {
    contents: String,
    fingerprint: String,
    saved_contents: String,
}

impl Document {
    fn is_dirty(&self) -> bool {
        self.contents != self.saved_contents
    }
}

#[derive(Clone)]
enum PendingAction {
    OpenWorkspace(PathBuf),
    ListFiles,
    OpenFile,
    SaveFile(PathBuf),
    Check,
}

struct PendingRequest {
    request_id: String,
    action: PendingAction,
    operation_id: Option<String>,
}

pub struct IdeApp {
    service: Option<Client>,
    connecting: Option<Receiver<Result<Client, String>>>,
    service_error: Option<String>,
    reconnect_delay: Duration,
    next_connect_attempt: Instant,
    workspace_input: String,
    workspace: Option<PathBuf>,
    files: Vec<PathBuf>,
    documents: BTreeMap<PathBuf, Document>,
    selected_file: Option<PathBuf>,
    file_query: String,
    pending: Option<PendingRequest>,
    status: String,
    build_output: Vec<String>,
    bottom_open: bool,
    inspector_open: bool,
}

impl IdeApp {
    pub fn new(_creation: &eframe::CreationContext<'_>, launch: vapor_gui_core::GuiLaunch) -> Self {
        let workspace_input = launch
            .workspace
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let mut app = Self {
            service: None,
            connecting: None,
            service_error: None,
            reconnect_delay: Duration::from_secs(1),
            next_connect_attempt: Instant::now(),
            workspace_input,
            workspace: None,
            files: Vec::new(),
            documents: BTreeMap::new(),
            selected_file: None,
            file_query: String::new(),
            pending: None,
            status: "Connecting to Vapor…".to_owned(),
            build_output: Vec::new(),
            bottom_open: true,
            inspector_open: true,
        };
        app.connect();
        app
    }

    fn connect(&mut self) {
        if self.connecting.is_some()
            || self.service.is_some()
            || Instant::now() < self.next_connect_attempt
        {
            return;
        }
        self.next_connect_attempt = Instant::now() + self.reconnect_delay;
        self.reconnect_delay = (self.reconnect_delay * 2).min(Duration::from_secs(30));
        let workspace = nonempty_path(&self.workspace_input);
        let (sender, receiver) = mpsc::channel();
        self.connecting = Some(receiver);
        self.status = "Connecting to the shared Vapor service…".to_owned();
        let result = thread::Builder::new()
            .name("vapor-ide-service-connect".to_owned())
            .spawn(move || {
                let result = connect_service(workspace).map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        if let Err(error) = result {
            self.connecting = None;
            self.service_error = Some(error.to_string());
            self.status = "Vapor could not be reached.".to_owned();
        }
    }

    fn poll_connection(&mut self) {
        let result = self.connecting.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(Ok(client))) => {
                self.connecting = None;
                self.service_error = None;
                self.reconnect_delay = Duration::from_secs(1);
                self.service = Some(client);
                self.status = "Connected to Vapor.".to_owned();
                if let Some(root) = nonempty_path(&self.workspace_input) {
                    self.open_workspace(root);
                }
            }
            Some(Ok(Err(error))) => {
                self.connecting = None;
                self.service_error = Some(error);
                self.status = "Vapor could not be reached.".to_owned();
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.connecting = None;
                self.service_error = Some("Vapor could not finish starting.".to_owned());
                self.status = "Vapor could not be reached.".to_owned();
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    fn open_workspace(&mut self, root: PathBuf) {
        if !root.is_dir() {
            self.status = format!("That folder does not exist: {}", root.display());
            return;
        }
        self.send(
            ClientAction::Workspace(WorkspaceAction::Open {
                root: root.display().to_string(),
            }),
            PendingAction::OpenWorkspace(root),
        );
    }

    fn send(&mut self, action: ClientAction, pending: PendingAction) {
        let Some(client) = self.service.as_mut() else {
            self.status = "Connect to Vapor before starting an operation.".to_owned();
            return;
        };
        match client.send(action, None) {
            Ok(request_id) => {
                self.pending = Some(PendingRequest {
                    request_id: request_id.0,
                    action: pending,
                    operation_id: None,
                });
                self.status = "Working…".to_owned();
            }
            Err(error) => {
                self.service = None;
                self.service_error = Some(error.to_string());
                self.status = "The Vapor connection was lost.".to_owned();
            }
        }
    }

    fn request_file_list(&mut self) {
        self.send(
            ClientAction::Workspace(WorkspaceAction::ListFiles {
                path: ".".to_owned(),
            }),
            PendingAction::ListFiles,
        );
    }

    fn open_file(&mut self, path: PathBuf) {
        if self.documents.contains_key(&path) {
            self.selected_file = Some(path);
            return;
        }
        self.send(
            ClientAction::Workspace(WorkspaceAction::OpenFile {
                path: path.display().to_string(),
            }),
            PendingAction::OpenFile,
        );
    }

    fn save_file(&mut self) {
        let Some(path) = self.selected_file.clone() else {
            return;
        };
        let Some(document) = self.documents.get(&path).cloned() else {
            return;
        };
        if !document.is_dirty() {
            self.status = "This file has no unsaved changes.".to_owned();
            return;
        }
        self.send(
            ClientAction::Workspace(WorkspaceAction::SaveFile {
                path: path.display().to_string(),
                expected_fingerprint: document.fingerprint,
                contents: document.contents,
            }),
            PendingAction::SaveFile(path),
        );
    }

    fn check_workspace(&mut self) {
        if self.documents.values().any(Document::is_dirty) {
            self.status = "Save or discard your edits before checking the workspace.".to_owned();
            return;
        }
        self.build_output.clear();
        self.send(
            ClientAction::Workspace(WorkspaceAction::RunTool {
                operation: WorkspaceToolOperation::Check,
                project_selection: WorkspaceProjectSelection::All,
                target_triples: Vec::new(),
            }),
            PendingAction::Check,
        );
    }

    fn poll_service(&mut self) {
        while let Some(client) = self.service.as_mut() {
            let frame = match client.try_recv() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    self.service = None;
                    self.service_error = Some(error.to_string());
                    self.status = "The Vapor connection was lost.".to_owned();
                    break;
                }
            };
            match frame {
                ProtocolFrame::Response(response) => {
                    let Some(pending) = self.pending.as_mut() else {
                        continue;
                    };
                    if response.request_id.0 != pending.request_id {
                        continue;
                    }
                    if let Some(error) = response.error {
                        self.status = error.message;
                        self.pending = None;
                        continue;
                    }
                    if let PendingAction::OpenWorkspace(root) = pending.action.clone() {
                        self.workspace = Some(root.clone());
                        self.workspace_input = root.display().to_string();
                        self.files.clear();
                        self.documents.clear();
                        self.selected_file = None;
                        self.status = format!("Opened {}", root.display());
                        self.pending = None;
                        self.request_file_list();
                    } else {
                        pending.operation_id = response
                            .snapshot
                            .active_operation
                            .as_ref()
                            .map(|operation| operation.id.clone());
                    }
                }
                ProtocolFrame::Event(ClientEvent::OperationOutput(output)) => {
                    self.append_output(&output.text);
                }
                ProtocolFrame::Event(ClientEvent::OperationChanged(event)) => {
                    if let Some(pending) = self.pending.as_mut()
                        && pending.operation_id.as_deref() == Some(event.id.as_str())
                    {
                        match event.status {
                            OperationStatus::Succeeded => {
                                if let Some(message) = event.message {
                                    self.status = message;
                                }
                                if matches!(pending.action, PendingAction::Check) {
                                    self.pending = None;
                                }
                            }
                            OperationStatus::Failed { message } => {
                                self.status = message;
                                self.pending = None;
                            }
                            OperationStatus::Cancelled => {
                                self.status = "Operation cancelled.".to_owned();
                                self.pending = None;
                            }
                            OperationStatus::Running | OperationStatus::Cancelling => {}
                        }
                    }
                }
                ProtocolFrame::Event(ClientEvent::WorkspaceFileResult(result)) => {
                    self.apply_file_result(*result);
                }
                ProtocolFrame::Event(ClientEvent::ProtocolError(error)) => {
                    self.status = error.message;
                }
                _ => {}
            }
        }
    }

    fn append_output(&mut self, output: &str) {
        self.build_output.extend(output.lines().map(str::to_owned));
        const MAX_LINES: usize = 500;
        if self.build_output.len() > MAX_LINES {
            let remove = self.build_output.len() - MAX_LINES;
            self.build_output.drain(..remove);
        }
    }

    fn apply_file_result(&mut self, result: WorkspaceFileResult) {
        match result {
            WorkspaceFileResult::Listed {
                operation_id,
                files,
            } if self.pending_matches(&operation_id) => {
                self.files = files.into_iter().map(PathBuf::from).collect();
                self.files.sort();
                self.status = format!("{} files in workspace.", self.files.len());
                self.pending = None;
            }
            WorkspaceFileResult::Opened {
                operation_id,
                path,
                contents,
                fingerprint,
            } if self.pending_matches(&operation_id) => {
                let path = PathBuf::from(path);
                self.selected_file = Some(path.clone());
                self.documents.insert(
                    path,
                    Document {
                        saved_contents: contents.clone(),
                        contents,
                        fingerprint,
                    },
                );
                self.status = "File opened.".to_owned();
                self.pending = None;
            }
            WorkspaceFileResult::Saved {
                operation_id,
                path,
                fingerprint,
            } if self.pending_save_matches(&operation_id, &path) => {
                let path = PathBuf::from(path);
                if let Some(document) = self.documents.get_mut(&path) {
                    document.saved_contents.clone_from(&document.contents);
                    document.fingerprint = fingerprint;
                }
                self.status = "Changes saved.".to_owned();
                self.pending = None;
            }
            _ => {}
        }
    }

    fn pending_matches(&self, operation_id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.operation_id.as_deref() == Some(operation_id))
    }

    fn pending_save_matches(&self, operation_id: &str, path: &str) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            pending.operation_id.as_deref() == Some(operation_id)
                && matches!(&pending.action, PendingAction::SaveFile(expected) if expected.as_path() == Path::new(path))
        })
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Vapor IDE");
            ui.separator();
            if let Some(root) = &self.workspace {
                ui.label(root.file_name().unwrap_or_default().to_string_lossy());
            } else {
                ui.label("Rust workspace");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.pending.is_none(), egui::Button::new("Check"))
                    .clicked()
                {
                    self.check_workspace();
                }
                if ui
                    .add_enabled(self.pending.is_none(), egui::Button::new("Save"))
                    .clicked()
                {
                    self.save_file();
                }
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.inspector_open, "Inspector");
                    ui.checkbox(&mut self.bottom_open, "Build & Problems");
                });
                if ui
                    .add_enabled(self.pending.is_none(), egui::Button::new("Open folder"))
                    .clicked()
                {
                    self.open_workspace(PathBuf::from(self.workspace_input.trim()));
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Workspace");
            ui.add_enabled(
                self.pending.is_none(),
                egui::TextEdit::singleline(&mut self.workspace_input)
                    .desired_width(ui.available_width()),
            );
        });
        ui.separator();
    }

    fn draw_files(&mut self, ui: &mut egui::Ui) {
        ui.heading("Project");
        ui.add_space(6.0);
        ui.text_edit_singleline(&mut self.file_query);
        let query = self.file_query.to_lowercase();
        let matching = self
            .files
            .iter()
            .filter(|path| {
                query.is_empty() || path.display().to_string().to_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for path in matching {
                let selected = self.selected_file.as_ref() == Some(&path);
                let dirty = self.documents.get(&path).is_some_and(Document::is_dirty);
                let name = path.display().to_string();
                if ui
                    .selectable_label(
                        selected,
                        format!("{}{}", name, if dirty { " •" } else { "" }),
                    )
                    .clicked()
                    && self.pending.is_none()
                {
                    self.open_file(path);
                }
            }
        });
    }

    fn draw_editor(&mut self, ui: &mut egui::Ui) {
        let open_documents = self.documents.keys().cloned().collect::<Vec<_>>();
        let mut close_document = None;
        if !open_documents.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for path in open_documents {
                    let label = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let dirty = self.documents.get(&path).is_some_and(Document::is_dirty);
                    let selected = self.selected_file.as_ref() == Some(&path);
                    if ui
                        .selectable_label(
                            selected,
                            format!("{label}{}", if dirty { " •" } else { "" }),
                        )
                        .clicked()
                    {
                        self.selected_file = Some(path.clone());
                    }
                    if ui
                        .add_enabled(!dirty && self.pending.is_none(), egui::Button::new("×"))
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
            self.documents.remove(&path);
            if self.selected_file.as_ref() == Some(&path) {
                self.selected_file = self.documents.keys().next().cloned();
            }
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
        let Some(document) = self.documents.get_mut(&path) else {
            ui.label("Opening file…");
            return;
        };
        ui.horizontal(|ui| {
            ui.strong(path.display().to_string());
            if document.is_dirty() {
                ui.colored_label(egui::Color32::YELLOW, "Unsaved");
            }
        });
        ui.separator();
        ui.add_sized(
            ui.available_size(),
            egui::TextEdit::multiline(&mut document.contents)
                .font(egui::TextStyle::Monospace)
                .code_editor()
                .desired_width(f32::INFINITY),
        );
    }

    fn draw_inspector(&self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.separator();
        if let Some(path) = &self.selected_file {
            ui.label("File");
            ui.monospace(path.display().to_string());
            ui.add_space(12.0);
            ui.label("Workspace");
            if let Some(root) = &self.workspace {
                ui.monospace(root.display().to_string());
            }
            ui.add_space(12.0);
            ui.label("Language tooling");
            ui.label("Workspace checks use Vapor’s managed toolchain.");
            ui.label("Rust Analyzer integration is not connected yet.");
        } else {
            ui.label("Select a file to inspect it.");
        }
    }

    fn draw_bottom(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Build & Problems");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear").clicked() {
                    self.build_output.clear();
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.build_output.is_empty() {
                    ui.label("Check output and diagnostics will appear here.");
                }
                for line in &self.build_output {
                    ui.monospace(line);
                }
            });
    }
}

impl eframe::App for IdeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.connect();
        self.poll_connection();
        self.poll_service();
        if ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::S)) {
            self.save_file();
        }
        egui::Panel::top("ide-toolbar").show(ui, |ui| self.header(ui));
        egui::Panel::bottom("ide-status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if let Some(error) = &self.service_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                    if ui.button("Reconnect").clicked() {
                        self.connect();
                    }
                }
            });
        });
        if self.bottom_open {
            egui::Panel::bottom("ide-build")
                .resizable(true)
                .default_size(190.0)
                .show(ui, |ui| self.draw_bottom(ui));
        }
        if self.inspector_open {
            egui::Panel::right("ide-inspector")
                .default_size(240.0)
                .size_range(180.0..=380.0)
                .resizable(true)
                .show(ui, |ui| self.draw_inspector(ui));
        }
        egui::Panel::left("ide-project")
            .default_size(260.0)
            .size_range(190.0..=520.0)
            .resizable(true)
            .show(ui, |ui| self.draw_files(ui));
        egui::CentralPanel::default().show(ui, |ui| self.draw_editor(ui));
        ui.ctx().request_repaint_after(Duration::from_millis(80));
    }
}

fn nonempty_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn connect_service(workspace: Option<PathBuf>) -> Result<Client, vapor_client::ClientError> {
    let application_executable = std::env::current_exe()
        .map_err(|error| vapor_client::ClientError::Io(error.to_string()))?;
    let service_path = service_sibling_path(&application_executable);
    Client::connect_or_start(
        &service_path,
        ClientHostConfig {
            application_executable,
            workshop_app_id: None,
            workspace_root: workspace,
            active_composition_id: None,
            fixture_mode: false,
            legacy_storage: Default::default(),
            state_directory: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{Document, nonempty_path};

    #[test]
    fn document_dirty_state_tracks_edits_and_save_baseline() {
        let mut document = Document {
            contents: "fn main() {}".to_owned(),
            saved_contents: "fn main() {}".to_owned(),
            fingerprint: "initial".to_owned(),
        };
        assert!(!document.is_dirty());
        document.contents.push('\n');
        assert!(document.is_dirty());
        document.saved_contents.clone_from(&document.contents);
        assert!(!document.is_dirty());
    }

    #[test]
    fn workspace_input_treats_blank_as_no_selection() {
        assert_eq!(nonempty_path("  "), None);
        assert_eq!(
            nonempty_path(" /workspace ").unwrap().to_string_lossy(),
            "/workspace"
        );
    }
}
