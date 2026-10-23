use eframe::egui;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use vapor_client::{Client, ClientHostConfig, service_sibling_path};
use vapor_client_protocol::{
    ClientAction, ClientEvent, LanguageServerAction, LanguageServerDiagnostics, OperationStatus,
    ProtocolFrame,
    WorkspaceAction, WorkspaceFileResult, WorkspaceProjectSelection, WorkspaceToolOperation,
};
use vapor_gui_core::semantic::{
    SemanticObject, SemanticObjectRef, SemanticRange, SemanticWorkspaceIndex,
};
use vapor_gui_core::workspace_tree::{WorkspaceFileTree, WorkspaceTreeEntry, WorkspaceTreeOrdering};
use vapor_gui_core::{ConsoleLevel, ConsoleRecord};

mod authoring;
mod semantic_view;
mod rust_semantics;
mod diagnostics;
mod toml_semantics;
mod xml_semantics;
mod language_server;

use authoring::WorkspaceAuthoring;
use diagnostics::{ProblemRow, ProblemRowRef, ProblemStore};
use language_server::{LanguageServerRequest, LspRequest, PendingLspRequest};
#[cfg(test)]
use language_server::{pending_lsp_requests_for_cancellation, workspace_versions_match};

#[derive(Clone)]
struct Document {
    contents: String,
    fingerprint: String,
    saved_contents: String,
    version: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceLocation {
    path: PathBuf,
    line: usize,
    character: usize,
}

const MAX_REFERENCE_LOCATIONS: usize = 500;

struct VersionedEditPreview {
    path: PathBuf,
    document_version: i32,
    original_contents: String,
    proposed_contents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditPreviewApplyError {
    Stale,
    NoChange,
    RevisionExhausted,
}

fn apply_versioned_edit(
    document: &mut Document,
    preview: &VersionedEditPreview,
) -> Result<i32, EditPreviewApplyError> {
    if document.version != preview.document_version || document.contents != preview.original_contents {
        return Err(EditPreviewApplyError::Stale);
    }
    if document.contents == preview.proposed_contents {
        return Err(EditPreviewApplyError::NoChange);
    }
    let next_version = document.version.checked_add(1).ok_or(EditPreviewApplyError::RevisionExhausted)?;
    document.contents = preview.proposed_contents.clone();
    document.version = next_version;
    Ok(next_version)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletionItem {
    label: String,
    insert_text: String,
    detail: Option<String>,
}

impl Document {
    fn is_dirty(&self) -> bool {
        self.contents != self.saved_contents
    }

    fn record_saved_contents(&mut self, contents: String, fingerprint: String) {
        self.saved_contents = contents;
        self.fingerprint = fingerprint;
    }
}

#[derive(Clone)]
enum PendingAction {
    OpenWorkspace(PathBuf),
    ListFiles,
    OpenFile,
    SaveFile { path: PathBuf, contents: String },
    CreateFile,
    Tool,
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
    file_tree: WorkspaceFileTree,
    rust_crate_roots: BTreeSet<PathBuf>,
    documents: BTreeMap<PathBuf, Document>,
    selected_file: Option<PathBuf>,
    file_query: String,
    focus_file_query: bool,
    project_switcher_open: bool,
    project_query: String,
    save_queue: VecDeque<PathBuf>,
    pending: Option<PendingRequest>,
    console_requests: HashSet<String>,
    status: String,
    console: vapor_gui_core::console::ConsolePanelState,
    bottom_open: bool,
    bottom_console_active: bool,
    inspector_open: bool,
    about_open: bool,
    authoring: WorkspaceAuthoring,
    find_open: bool,
    focus_find_query: bool,
    find_query: String,
    find_match_index: Option<usize>,
    find_navigation: Option<bool>,
    cursor_position: (usize, usize),
    language_server_status: String,
    language_server_ready: bool,
    language_server_requests: BTreeMap<String, LanguageServerRequest>,
    lsp_requests: BTreeMap<i64, PendingLspRequest>,
    next_lsp_request_id: i64,
    cursor_offset: usize,
    pending_navigation: Option<(PathBuf, usize, usize)>,
    hover: Option<String>,
    references: Vec<ReferenceLocation>,
    references_omitted: usize,
    completion_items: Vec<CompletionItem>,
    problems: ProblemStore,
    semantic_index: vapor_gui_core::semantic::SemanticWorkspaceIndex,
    semantic_match_cache: semantic_view::SemanticMatchCache,
    workspace_symbols: Vec<SemanticObject>,
    workspace_symbol_query: Option<String>,
    selected_semantic: Option<vapor_gui_core::semantic::SemanticObjectRef>,
    explorer_symbols: bool,
    edit_preview: Option<VersionedEditPreview>,
    workspace_edit_preview: Option<Vec<VersionedEditPreview>>,
    rename_target: Option<SemanticObjectRef>,
    rename_input: String,
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
            file_tree: WorkspaceFileTree::default(),
            rust_crate_roots: BTreeSet::new(),
            documents: BTreeMap::new(),
            selected_file: None,
            file_query: String::new(),
            focus_file_query: false,
            project_switcher_open: false,
            project_query: String::new(),
            save_queue: VecDeque::new(),
            pending: None,
            console_requests: HashSet::new(),
            status: "Connecting to Vapor...".to_owned(),
            console: vapor_gui_core::console::ConsolePanelState::default(),
            bottom_open: true,
            bottom_console_active: false,
            inspector_open: true,
            about_open: false,
            authoring: WorkspaceAuthoring::default(),
            find_open: false,
            focus_find_query: false,
            find_query: String::new(),
            find_match_index: None,
            find_navigation: None,
            cursor_position: (1, 1),
            language_server_status: "Rust Analyzer is stopped".to_owned(),
            language_server_ready: false,
            language_server_requests: BTreeMap::new(),
            lsp_requests: BTreeMap::new(),
            next_lsp_request_id: 2,
            cursor_offset: 0,
            pending_navigation: None,
            hover: None,
            references: Vec::new(),
            references_omitted: 0,
            completion_items: Vec::new(),
            problems: ProblemStore::default(),
            semantic_index: vapor_gui_core::semantic::SemanticWorkspaceIndex::default(),
            semantic_match_cache: semantic_view::SemanticMatchCache::default(),
            workspace_symbols: Vec::new(),
            workspace_symbol_query: None,
            selected_semantic: None,
            explorer_symbols: false,
            edit_preview: None,
            workspace_edit_preview: None,
            rename_target: None,
            rename_input: String::new(),
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
        self.status = "Connecting to the shared Vapor service...".to_owned();
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
        if self.documents.values().any(Document::is_dirty) {
            self.status = "Save your open files before switching projects.".to_owned();
            return;
        }
        if !root.is_dir() {
            self.status = format!("That folder does not exist: {}", root.display());
            return;
        }
        self.stop_language_server();
        self.problems.clear();
        self.workspace_symbols.clear();
        self.workspace_symbol_query = None;
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
                self.status = "Working...".to_owned();
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
        self.save_path(path);
    }

    fn save_path(&mut self, path: PathBuf) {
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
                contents: document.contents.clone(),
            }),
            PendingAction::SaveFile {
                path,
                contents: document.contents,
            },
        );
    }

    fn save_all(&mut self) {
        if self.pending.is_some() {
            return;
        }
        self.save_queue = self
            .documents
            .iter()
            .filter(|(_, document)| document.is_dirty())
            .map(|(path, _)| path.clone())
            .collect();
        if self.save_queue.is_empty() {
            self.status = "All open files are saved.".to_owned();
            return;
        }
        self.save_next();
    }

    fn save_next(&mut self) {
        if self.pending.is_some() {
            return;
        }
        while let Some(path) = self.save_queue.pop_front() {
            if !self.documents.get(&path).is_some_and(Document::is_dirty) {
                continue;
            }
            self.save_path(path);
            if self.pending.is_some() {
                return;
            }
            self.save_queue.clear();
            return;
        }
        self.status = if self.documents.values().any(Document::is_dirty) {
            "Some files changed while saving. Save again to keep those edits.".to_owned()
        } else {
            "All open files are saved.".to_owned()
        };
    }

    fn close_document(&mut self, path: &Path) {
        if self.documents.get(path).is_some_and(Document::is_dirty) {
            self.status = "Save this file before closing it.".to_owned();
            return;
        }
        self.did_close_document(path);
        self.semantic_index.invalidate_document(path);
        if self.selected_semantic.as_ref().is_some_and(|selected| &selected.id.document == path) {
            self.selected_semantic = None;
        }
        self.documents.remove(path);
        if self.selected_file.as_deref() == Some(path) {
            self.selected_file = self.documents.keys().next().cloned();
        }
    }

    fn run_workspace_tool(&mut self, operation: WorkspaceToolOperation) {
        if self.workspace.is_none() {
            self.status = "Open a project before running a workspace action.".to_owned();
            return;
        }
        if self.documents.values().any(Document::is_dirty) {
            self.status =
                "Save or discard your edits before running a workspace action.".to_owned();
            return;
        }
        self.console.history.clear();
        self.send(
            ClientAction::Workspace(WorkspaceAction::RunTool {
                operation,
                project_selection: WorkspaceProjectSelection::All,
                target_triples: Vec::new(),
            }),
            PendingAction::Tool,
        );
    }

    fn run_default_configuration(&mut self) {
        let Some(root) = self.workspace.as_ref() else {
            self.status = "Open a project before running it.".to_owned();
            return;
        };
        if self.documents.values().any(Document::is_dirty) {
            self.status = "Save or discard your edits before running the project.".to_owned();
            return;
        }
        self.console.history.clear();
        self.send(
            ClientAction::Workspace(WorkspaceAction::RunConfiguration {
                workspace_root: root.display().to_string(),
                configuration: "default".to_owned(),
                profiling: false,
            }),
            PendingAction::Tool,
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
                    if self.console_requests.remove(&response.request_id.0) {
                        if let Some(error) = response.error {
                            self.console.history.push(
                                ConsoleRecord::new(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|duration| {
                                            duration.as_millis().min(u64::MAX as u128) as u64
                                        })
                                        .unwrap_or_default(),
                                    ConsoleLevel::Error,
                                    "vapor",
                                    error.message,
                                )
                                .field("origin", "ide"),
                            );
                        }
                        continue;
                    }
                    if let Some(language_server_request) =
                        self.language_server_requests.remove(&response.request_id.0)
                    {
                        if let Some(error) = response.error {
                            self.language_server_ready = false;
                            self.language_server_status = error.message.clone();
                            self.append_output(&format!("Rust Analyzer: {}", error.message));
                            continue;
                        }
                        match language_server_request {
                            LanguageServerRequest::Start => self.initialize_language_server(),
                            LanguageServerRequest::Stop => {}
                        }
                        continue;
                    }
                    let Some(pending) = self.pending.as_mut() else {
                        continue;
                    };
                    if response.request_id.0 != pending.request_id {
                        continue;
                    }
                    if let Some(error) = response.error {
                        self.status = error.message;
                        if matches!(pending.action, PendingAction::SaveFile { .. }) {
                            self.save_queue.clear();
                        }
                        if matches!(pending.action, PendingAction::CreateFile) {
                            self.authoring.new_file_dialog = true;
                        }
                        self.pending = None;
                        continue;
                    }
                    if let PendingAction::OpenWorkspace(root) = pending.action.clone() {
                        self.workspace = Some(root.clone());
                        self.workspace_input = root.display().to_string();
                        self.files.clear();
                        self.file_tree = WorkspaceFileTree::default();
                        self.rust_crate_roots.clear();
                        self.documents.clear();
                        self.semantic_index = SemanticWorkspaceIndex::default();
                        self.selected_semantic = None;
                        self.selected_file = None;
                        self.start_language_server();
                        self.status = format!("Opened {}", root.display());
                        self.pending = None;
                        self.request_file_list();
                    } else {
                        // The snapshot's primary operation can belong to another
                        // frontend. File results correlate to this request's
                        // accepted operation, never whichever operation is first.
                        pending.operation_id = response.operation_id.clone();
                    }
                }
                ProtocolFrame::Event(ClientEvent::OperationOutput(output)) => {
                    for record in vapor_gui_core::console::records_from_operation_output(&output) {
                        self.console.history.push(record);
                    }
                    if let (Some(root), Some(diagnostic)) =
                        (self.workspace.as_deref(), output.diagnostic)
                    {
                        let path = diagnostic
                            .primary
                            .as_ref()
                            .or_else(|| diagnostic.related.first())
                            .and_then(|span| {
                                workspace_diagnostic_path(root, &span.file, &self.files)
                            });
                        self.problems.push_build(
                            &output.id,
                            ProblemRow {
                                path,
                                operation: Some(output.action.clone()),
                                diagnostic,
                            },
                        );
                    }
                }
                ProtocolFrame::Event(ClientEvent::OperationChanged(event)) => {
                    if matches!(event.status, OperationStatus::Running)
                        && (event.phase == vapor_client_protocol::OperationPhase::Building
                            || (event.phase == vapor_client_protocol::OperationPhase::Queued
                                && operation_produces_build_diagnostics(&event.action)))
                    {
                        self.problems.begin_build(&event.id);
                    }
                    if let Some(pending) = self.pending.as_mut()
                        && pending.operation_id.as_deref() == Some(event.id.as_str())
                    {
                        match event.status {
                            OperationStatus::Succeeded => {
                                if let Some(message) = event.message {
                                    self.status = message;
                                }
                                if matches!(pending.action, PendingAction::Tool) {
                                    self.pending = None;
                                }
                            }
                            OperationStatus::Failed { message } => {
                                self.status = message;
                                if matches!(pending.action, PendingAction::SaveFile { .. }) {
                                    self.save_queue.clear();
                                }
                                self.pending = None;
                            }
                            OperationStatus::Cancelled => {
                                self.status = "Operation cancelled.".to_owned();
                                if matches!(pending.action, PendingAction::SaveFile { .. }) {
                                    self.save_queue.clear();
                                }
                                self.pending = None;
                            }
                            OperationStatus::Running | OperationStatus::Cancelling => {}
                        }
                    }
                }
                ProtocolFrame::Event(ClientEvent::WorkspaceFileResult(result)) => {
                    self.apply_file_result(*result);
                }
                ProtocolFrame::Event(ClientEvent::LanguageServerMessage(message)) => {
                    self.handle_language_server_message(message);
                }
                ProtocolFrame::Event(ClientEvent::LanguageServerDiagnostics(diagnostics)) => {
                    self.handle_language_server_diagnostics(*diagnostics);
                }
                ProtocolFrame::Event(ClientEvent::LanguageServerStatus { status, message }) => {
                    self.language_server_ready =
                        status == vapor_client_protocol::LanguageServerStatus::Ready;
                    self.language_server_status = message.unwrap_or_else(|| match status {
                        vapor_client_protocol::LanguageServerStatus::Ready => {
                            "Rust Analyzer ready".to_owned()
                        }
                        vapor_client_protocol::LanguageServerStatus::Stopped => {
                            "Rust Analyzer stopped".to_owned()
                        }
                        vapor_client_protocol::LanguageServerStatus::Failed => {
                            "Rust Analyzer failed".to_owned()
                        }
                    });
                }
                ProtocolFrame::Event(ClientEvent::ProtocolError(error)) => {
                    self.status = error.message;
                }
                _ => {}
            }
        }
    }

    fn append_output(&mut self, output: &str) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or_default();
        for line in vapor_gui_core::plain_terminal_text(output)
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            self.console.history.push(
                ConsoleRecord::new(timestamp, ConsoleLevel::Info, "ide", line)
                    .field("origin", "ide"),
            );
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
                self.file_tree = WorkspaceFileTree::new(&self.files);
                self.rust_crate_roots = rust_crate_roots(&self.files);
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
                    path.clone(),
                    Document {
                        saved_contents: contents.clone(),
                        contents,
                        fingerprint,
                        version: 1,
                    },
                );
                if let Some((version, contents)) = self
                    .documents
                    .get(&path)
                    .map(|document| (document.version as u64, document.contents.clone()))
                {
                    self.index_local_semantic_document(&path, version, &contents);
                }
                self.did_open_document(&path);
                self.request_document_symbols(&path);
                self.status = "File opened.".to_owned();
                self.pending = None;
            }
            WorkspaceFileResult::Saved {
                operation_id,
                path,
                fingerprint,
            } if self.pending_save_matches(&operation_id, &path) => {
                let path = PathBuf::from(path);
                let saved_contents =
                    self.pending
                        .as_ref()
                        .and_then(|pending| match &pending.action {
                            PendingAction::SaveFile { contents, .. } => Some(contents.clone()),
                            _ => None,
                        });
                if let (Some(document), Some(saved_contents)) =
                    (self.documents.get_mut(&path), saved_contents)
                {
                    document.record_saved_contents(saved_contents, fingerprint);
                    if document.is_dirty() && !self.save_queue.contains(&path) {
                        self.save_queue.push_front(path.clone());
                    }
                }
                self.did_save_document(&path);
                self.status = "Changes saved.".to_owned();
                self.pending = None;
                self.save_next();
            }
            WorkspaceFileResult::Created { operation_id, path }
                if self.pending_matches(&operation_id) =>
            {
                let path = PathBuf::from(path);
                if !self.files.contains(&path) {
                    self.files.push(path.clone());
                    self.files.sort();
                    self.file_tree = WorkspaceFileTree::new(&self.files);
                }
                self.pending = None;
                self.authoring.new_file_path.clear();
                self.authoring.new_file_dialog = false;
                self.open_file(path);
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
                && matches!(&pending.action, PendingAction::SaveFile { path: expected, .. } if expected.as_path() == Path::new(path))
        })
    }

    fn header(&mut self, ui: &mut egui::Ui) {
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

    fn draw_welcome(&mut self, ui: &mut egui::Ui) {
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

    fn draw_project_switcher(&mut self, context: &egui::Context) {
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

    fn draw_files(&mut self, ui: &mut egui::Ui) {
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

    fn draw_editor(&mut self, ui: &mut egui::Ui) {
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

    fn draw_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.separator();
        if let Some(reference) = self.selected_semantic.clone()
            && let Some(object) = self.semantic_index.resolve(&reference).cloned()
        {
            ui.strong(object.kind.label());
            ui.heading(&object.name);
            ui.monospace(object.id.qualified_name());
            ui.weak(format!("{} · document revision {}", object.id.document.display(), reference.document_version));
            if let Some(detail) = &object.detail {
                ui.add_space(6.0);
                ui.label(detail);
            }
            if let Some(status) = semantic_view::semantic_convention_status(&object) {
                ui.colored_label(egui::Color32::LIGHT_BLUE, status);
            }
            let mut diagnostic_navigation = None;
            if let Some(relative_path) = self.workspace.as_deref().and_then(|root| {
                relative_workspace_path(root, &object.id.document)
            }) {
                let diagnostics = self.problems.for_semantic_range(&relative_path, object.range);
                if !diagnostics.is_empty() {
                    ui.add_space(8.0);
                    ui.strong(format!("Problems · {}", diagnostics.len()));
                    for diagnostic in diagnostics {
                        let severity = match diagnostic.severity {
                            vapor_gui_core::DiagnosticSeverity::Error => ("Error", egui::Color32::LIGHT_RED),
                            vapor_gui_core::DiagnosticSeverity::Warning => ("Warning", egui::Color32::YELLOW),
                            vapor_gui_core::DiagnosticSeverity::Information => ("Info", egui::Color32::LIGHT_BLUE),
                            vapor_gui_core::DiagnosticSeverity::Hint => ("Hint", egui::Color32::GRAY),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(severity.1, severity.0);
                            ui.weak(format!("{:?}", diagnostic.origin));
                            if let Some(code) = &diagnostic.code {
                                ui.monospace(code);
                            }
                            if ui.small_button("Open").clicked()
                                && let Some(span) = &diagnostic.primary
                            {
                                diagnostic_navigation = Some((
                                    object.id.document.clone(),
                                    span.start_line.saturating_sub(1) as usize,
                                    span.start_column.saturating_sub(1) as usize,
                                ));
                            }
                        });
                        ui.label(&diagnostic.message);
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
                    self.selected_semantic.as_ref(),
                    &mut selected_member,
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_rename = self.language_server_ready
                    && object.id.domain == "rust"
                    && object.id.document.extension().is_some_and(|extension| extension == "rs");
                if ui
                    .add_enabled(can_rename, egui::Button::new("Rename…"))
                    .on_hover_text("Preview a Rust Analyzer rename across open workspace files")
                    .clicked()
                {
                    self.rename_target = Some(reference.clone());
                    self.rename_input.clone_from(&object.name);
                }
                if ui.button("Go to declaration").clicked() {
                    self.navigate_to_workspace_location(
                        &object.id.document,
                        object.selection_range.start.line as usize,
                        object.selection_range.start.character as usize,
                    );
                }
                let can_query_references = self.language_server_ready
                    && self.documents.contains_key(&object.id.document)
                    && object.id.document.extension().is_some_and(|extension| extension == "rs");
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
            if self.rename_target.as_ref() == Some(&reference) {
                let mut submit_rename = false;
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rename_input)
                            .hint_text("New Rust identifier")
                            .desired_width(150.0),
                    );
                    if ui.button("Preview").clicked() {
                        submit_rename = true;
                    }
                    if ui.small_button("Cancel").clicked() {
                        self.rename_target = None;
                    }
                });
                if submit_rename {
                    let new_name = self.rename_input.clone();
                    self.request_lsp_rename(reference.clone(), &new_name);
                }
            }
            if let Some((path, line, character)) = diagnostic_navigation {
                self.navigate_to_workspace_location(&path, line, character);
            }
            if let Some(member) = selected_member {
                if self.selected_semantic.as_ref() != Some(&member) {
                    self.references.clear();
                    self.references_omitted = 0;
                    self.rename_target = None;
                }
                self.selected_semantic = Some(member.clone());
                if let Some(range) = reference_range(&self.semantic_index, &member) {
                    self.navigate_to_workspace_location(
                        &member.id.document,
                        range.start.line as usize,
                        range.start.character as usize,
                    );
                }
            }
            ui.separator();
        }
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
            ui.label(&self.language_server_status);
            ui.label("Rust Analyzer runs through the shared Vapor service and pinned toolchain.");
            ui.label(format!("{} workspace problems", self.problems.count()));
            if let Some(hover) = &self.hover {
                ui.add_space(8.0);
                ui.label("Hover");
                ui.monospace(hover);
            }
            if !self.references.is_empty() {
                ui.add_space(8.0);
                ui.label(if self.references_omitted == 0 {
                    format!("References | {}", self.references.len())
                } else {
                    format!("References | {} shown · {} omitted", self.references.len(), self.references_omitted)
                });
                let mut selected_reference = None;
                egui::ScrollArea::vertical()
                    .id_salt("ide-references")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for location in &self.references {
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

    fn draw_bottom(&mut self, ui: &mut egui::Ui) {
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

    fn submit_console_command(&mut self, line: &str) {
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

impl eframe::App for IdeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.connect();
        self.poll_connection();
        self.poll_service();
        if ui.input(|input| input.key_pressed(egui::Key::F2)) {
            if !self.bottom_open {
                self.bottom_console_active = true;
            }
            self.bottom_open = !self.bottom_open;
        }
        if ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::S)) {
            if ui.input(|input| input.modifiers.shift) {
                self.save_all();
            } else {
                self.save_file();
            }
        }
        if ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::P)) {
            if self.workspace.is_some() {
                self.focus_file_query = true;
            } else {
                self.project_switcher_open = true;
            }
        }
        if ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::F)) {
            if self.selected_file.is_some() {
                self.find_open = true;
                self.focus_find_query = true;
            } else {
                self.focus_file_query = self.workspace.is_some();
            }
        }
        if ui.input(|input| input.key_pressed(egui::Key::F12)) {
            self.request_lsp_at_cursor("textDocument/definition", LspRequest::Definition);
        }
        if ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::Space)) {
            self.request_lsp_at_cursor("textDocument/completion", LspRequest::Completion);
        }
        if self.find_open
            && ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::G))
        {
            self.find_navigation = Some(ui.input(|input| input.modifiers.shift));
        }
        if ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::W))
            && let Some(path) = self.selected_file.clone()
        {
            self.close_document(&path);
        }
        if ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::N))
            && self.workspace.is_some()
            && self.pending.is_none()
        {
            self.authoring.new_file_dialog = true;
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
        if self.about_open {
            egui::Window::new("About Vapor IDE")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .open(&mut self.about_open)
                .show(ui.ctx(), |ui| {
                    ui.heading("Vapor IDE");
                    ui.label("A Vapor-managed workspace editor and Rust development environment.");
                    ui.label(
                        "Rust Analyzer and project tools run through the shared Vapor service.",
                    );
                });
        }
        if self.workspace.is_none() {
            egui::CentralPanel::default().show(ui, |ui| self.draw_welcome(ui));
            self.draw_project_switcher(ui.ctx());
            ui.ctx().request_repaint_after(Duration::from_millis(80));
            return;
        }
        if self.bottom_open {
            egui::Panel::bottom("ide-build")
                .resizable(true)
                .default_size(190.0)
                .size_range(140.0..=520.0)
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
        self.draw_project_switcher(ui.ctx());
        self.draw_authoring_dialogs(ui.ctx());
        if self.edit_preview.is_some() {
            let mut preview_open = true;
            let mut apply = false;
            let mut discard = false;
            egui::Window::new("Review formatting")
                .collapsible(false)
                .resizable(true)
                .default_size([900.0, 560.0])
                .open(&mut preview_open)
                .show(ui.ctx(), |ui| {
                    if let Some(preview) = self.edit_preview.as_mut() {
                        ui.label(preview.path.display().to_string());
                        ui.weak(format!("Document revision {} · no changes are written until you apply and save", preview.document_version));
                        ui.separator();
                        ui.columns(2, |columns| {
                            let left_size = [columns[0].available_width(), columns[0].available_height()];
                            let right_size = [columns[1].available_width(), columns[1].available_height()];
                            columns[0].strong("Current");
                            columns[0].add_sized(
                                left_size,
                                egui::TextEdit::multiline(&mut preview.original_contents)
                                    .code_editor()
                                    .interactive(false),
                            );
                            columns[1].strong("Proposed");
                            columns[1].add_sized(
                                right_size,
                                egui::TextEdit::multiline(&mut preview.proposed_contents)
                                    .code_editor()
                                    .interactive(false),
                            );
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Apply preview").clicked() {
                                apply = true;
                            }
                            if ui.button("Discard").clicked() {
                                discard = true;
                            }
                        });
                    }
                });
            if apply {
                self.apply_edit_preview();
            } else if discard || !preview_open {
                self.edit_preview = None;
                self.status = "Formatting preview discarded.".to_owned();
            }
        }
        if self.workspace_edit_preview.is_some() {
            let mut preview_open = true;
            let mut apply = false;
            let mut discard = false;
            egui::Window::new("Review Rust refactor")
                .collapsible(false)
                .resizable(true)
                .default_size([1100.0, 720.0])
                .open(&mut preview_open)
                .show(ui.ctx(), |ui| {
                    if let Some(previews) = self.workspace_edit_preview.as_mut() {
                        ui.strong(format!("{} open files will change", previews.len()));
                        ui.weak("Nothing is written until you apply the preview and save the changed files.");
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for preview in previews {
                                egui::CollapsingHeader::new(preview.path.display().to_string())
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        ui.weak(format!("Document revision {}", preview.document_version));
                                        ui.columns(2, |columns| {
                                            let left_size = [columns[0].available_width(), 180.0];
                                            let right_size = [columns[1].available_width(), 180.0];
                                            columns[0].strong("Current");
                                            columns[0].add_sized(
                                                left_size,
                                                egui::TextEdit::multiline(&mut preview.original_contents)
                                                    .code_editor()
                                                    .interactive(false),
                                            );
                                            columns[1].strong("Proposed");
                                            columns[1].add_sized(
                                                right_size,
                                                egui::TextEdit::multiline(&mut preview.proposed_contents)
                                                    .code_editor()
                                                    .interactive(false),
                                            );
                                        });
                                    });
                            }
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Apply refactor").clicked() {
                                apply = true;
                            }
                            if ui.button("Discard").clicked() {
                                discard = true;
                            }
                        });
                    }
                });
            if apply {
                self.apply_workspace_edit_preview();
                self.rename_target = None;
            } else if discard || !preview_open {
                self.workspace_edit_preview = None;
                self.status = "Rust refactor preview discarded.".to_owned();
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(80));
    }
}

struct FileTreeView<'a> {
    files: &'a [PathBuf],
    documents: &'a BTreeMap<PathBuf, Document>,
    selected: Option<&'a PathBuf>,
    enabled: bool,
    filter: &'a str,
    rust_crate_roots: &'a BTreeSet<PathBuf>,
}

fn draw_file_tree(
    ui: &mut egui::Ui,
    tree: &WorkspaceFileTree,
    parent: &Path,
    depth: usize,
    view: &FileTreeView<'_>,
    requested: &mut Option<PathBuf>,
) {
    let ordering = workspace_tree_ordering(parent, view.rust_crate_roots);
    for (name, entry) in tree.ordered_entries(ordering) {
        match entry {
            WorkspaceTreeEntry::Directory(directory) => {
                if !directory.contains_matching_file(view.filter) {
                    continue;
                }
                let path = parent.join(name);
                egui::CollapsingHeader::new(name)
                    .id_salt(("ide-tree-folder", &path, !view.filter.is_empty()))
                    .default_open(depth == 0 || !view.filter.is_empty())
                    .show(ui, |ui| {
                        draw_file_tree(ui, directory, &path, depth + 1, view, requested);
                    });
            }
            WorkspaceTreeEntry::File { index, .. } => {
                if !entry.matches_filter(view.filter) {
                    continue;
                }
                let Some(path) = view.files.get(*index) else {
                    continue;
                };
                let dirty = view.documents.get(path).is_some_and(Document::is_dirty);
                let response = ui.add_enabled(
                    view.enabled,
                    egui::Button::selectable(
                        view.selected == Some(path),
                        format!("{}{}", file_display_label(path, view.files), if dirty { " *" } else { "" }),
                    ),
                );
                if response.clicked() {
                    *requested = Some(path.clone());
                }
                response.on_hover_text(path.display().to_string());
            }
        }
    }
}

fn reference_range(
    index: &SemanticWorkspaceIndex,
    reference: &SemanticObjectRef,
) -> Option<SemanticRange> {
    index.resolve(reference).map(|object| object.selection_range)
}

fn xml_parse_diagnostic(
    path: &Path,
    source: &str,
    error: &xml_semantics::XmlParseError,
) -> vapor_gui_core::StructuredDiagnostic {
    let message = "This XML document could not be parsed. Inspect its syntax near the highlighted file.".to_owned();
    let mut offset = error.offset.min(source.len());
    while !source.is_char_boundary(offset) { offset = offset.saturating_sub(1); }
    let end = source[offset..].chars().next().map_or(offset, |character| offset + character.len_utf8());
    let range = vapor_gui_core::semantic::semantic_range_from_byte_span(source, offset..end);
    vapor_gui_core::StructuredDiagnostic {
        origin: vapor_gui_core::DiagnosticOrigin::Vapor,
        severity: vapor_gui_core::DiagnosticSeverity::Error,
        code: Some("vapor.xml.parse".to_owned()),
        message: message.clone(),
        rendered: Some(message.clone()),
        primary: Some(vapor_gui_core::DiagnosticSpan {
            file: path.to_string_lossy().into_owned(),
            start_line: range.start.line + 1,
            start_column: range.start.character + 1,
            end_line: range.end.line + 1,
            end_column: range.end.character + 1,
            primary: true,
            label: Some("XML syntax error".to_owned()),
        }),
        related: Vec::new(),
        raw_payload: "XML parser reported invalid structure".to_owned(),
    }
}

fn toml_parse_diagnostic(
    path: &Path,
    source: &str,
    error: &toml_edit::TomlError,
) -> vapor_gui_core::StructuredDiagnostic {
    let primary = error.span().map(|span| {
        let range = byte_span_to_semantic_range(source, span);
        vapor_gui_core::DiagnosticSpan {
            file: path.to_string_lossy().into_owned(),
            start_line: range.start.line.saturating_add(1),
            start_column: range.start.character.saturating_add(1),
            end_line: range.end.line.saturating_add(1),
            end_column: range.end.character.saturating_add(1),
            primary: true,
            label: Some("TOML syntax error".to_owned()),
        }
    });
    let message = truncate_diagnostic_payload(error.message());
    vapor_gui_core::StructuredDiagnostic {
        origin: vapor_gui_core::DiagnosticOrigin::Vapor,
        severity: vapor_gui_core::DiagnosticSeverity::Error,
        code: Some("vapor.toml.parse".to_owned()),
        message: message.clone(),
        rendered: None,
        primary,
        related: Vec::new(),
        // Keep the machine detail but avoid retaining a formatted TOML parser
        // error, which may quote source text such as credentials or tokens.
        raw_payload: message,
    }
}

fn byte_span_to_semantic_range(
    source: &str,
    span: std::ops::Range<usize>,
) -> SemanticRange {
    vapor_gui_core::semantic::semantic_range_from_byte_span(source, span)
}

fn file_display_label(path: &Path, all_files: &[PathBuf]) -> String {
    if path.extension().is_some_and(|extension| extension == "rs") {
        shortest_rust_label(path, all_files)
    } else {
        path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
    }
}

fn rust_label_parts(path: &Path) -> Vec<String> {
    let Some(file_name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
        return vec![path.display().to_string()];
    };
    let mut directories = path.parent().into_iter().flat_map(Path::components).filter_map(|component| match component {
        std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
        _ => None,
    }).collect::<Vec<_>>();
    if let Some(src_index) = directories.iter().rposition(|part| part == "src") {
        directories = directories.split_off(src_index + 1);
    }
    if file_name == "mod.rs" {
        if directories.is_empty() { vec!["crate".to_owned()] } else { directories }
    } else {
        let stem = Path::new(&file_name)
            .file_stem()
            .map_or_else(|| file_name.clone(), |stem| stem.to_string_lossy().into_owned());
        directories.push(stem);
        directories
    }
}

fn rust_crate_label(path: &Path) -> Option<String> {
    let directories = path
        .parent()?
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let src_index = directories.iter().rposition(|part| part == "src")?;
    if src_index >= 2 && directories[src_index - 2] == "crates" {
        Some(directories[src_index - 1].clone())
    } else {
        directories.get(src_index.checked_sub(1)?).cloned()
    }
}

fn canonical_rust_item_kind(
    document: &Path,
    crate_roots: &BTreeSet<PathBuf>,
) -> Option<rust_semantics::CanonicalRustItemKind> {
    let is_cargo_source = crate_roots.iter().any(|root| {
        document
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| component.as_os_str() == "src")
    });
    is_cargo_source
        .then(|| rust_semantics::CanonicalRustItemKind::for_document(document))
        .flatten()
}

fn rust_crate_roots(files: &[PathBuf]) -> BTreeSet<PathBuf> {
    let manifests = files
        .iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "Cargo.toml"))
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    let mut roots = BTreeSet::new();
    for source in files
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
    {
        let nearest_manifest = manifests
            .iter()
            .filter(|root| source.starts_with(root))
            .max_by_key(|root| root.components().count());
        if let Some(root) = nearest_manifest
            && let Ok(relative) = source.strip_prefix(root)
        {
            let first = relative.components().next().and_then(|component| match component {
                std::path::Component::Normal(name) => name.to_str(),
                _ => None,
            });
            let cargo_source = matches!(first, Some("src" | "tests" | "benches" | "examples"))
                || relative == Path::new("build.rs");
            if cargo_source {
                roots.insert(root.clone());
            }
        }
    }
    roots
}

fn workspace_tree_ordering(
    parent: &Path,
    rust_crate_roots: &BTreeSet<PathBuf>,
) -> WorkspaceTreeOrdering {
    if rust_crate_roots
        .iter()
        .any(|root| parent == root || parent.starts_with(root))
    {
        WorkspaceTreeOrdering::Alphabetical
    } else {
        WorkspaceTreeOrdering::DirectoriesFirst
    }
}

fn shortest_rust_label(path: &Path, candidates: &[PathBuf]) -> String {
    let parts = rust_label_parts(path);
    for suffix_len in 1..=parts.len() {
        let suffix = parts[parts.len() - suffix_len..].join("/");
        let ambiguous = candidates.iter().filter(|other| *other != path).any(|other| {
            if other.extension().is_none_or(|extension| extension != "rs") { return false; }
            let other_parts = rust_label_parts(other);
            other_parts.len() >= suffix_len
                && other_parts[other_parts.len() - suffix_len..].join("/") == suffix
        });
        if !ambiguous { return suffix; }
    }
    let mut qualified = rust_crate_label(path).into_iter().collect::<Vec<_>>();
    qualified.extend(parts);
    let label = qualified.join("/");
    let fallback_still_collides = candidates.iter().filter(|other| *other != path).any(|other| {
        let mut other_qualified = rust_crate_label(other).into_iter().collect::<Vec<_>>();
        other_qualified.extend(rust_label_parts(other));
        other_qualified.join("/") == label
    });
    if path.file_name().is_some_and(|name| name == "mod.rs") && fallback_still_collides {
        qualified.push("mod".to_owned());
    }
    qualified.join("/")
}

fn nonempty_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let prefix = if normalized.as_bytes().get(1) == Some(&b':') {
        "file:///"
    } else {
        "file://"
    };
    #[cfg(not(windows))]
    let prefix = "file://";
    let mut uri = String::from(prefix);
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let encoded_path = uri.strip_prefix("file://")?;
    let encoded_path = encoded_path.as_bytes();
    let mut decoded = Vec::with_capacity(encoded_path.len());
    let mut index = 0;
    while index < encoded_path.len() {
        if encoded_path[index] == b'%' {
            let high = *encoded_path.get(index + 1)?;
            let low = *encoded_path.get(index + 2)?;
            decoded.push((hex_value(high)? << 4) | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(encoded_path[index]);
            index += 1;
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    #[cfg(windows)]
    let path = path
        .strip_prefix('/')
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&path)
        .to_owned();
    Some(PathBuf::from(path))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn rust_analyzer_initialization_options() -> serde_json::Value {
    serde_json::json!({
        // Opening source remains analysis-only. Builds are explicit Vapor
        // operations so the user can see and control their work and output.
        "checkOnSave": false,
        "cargo": {"buildScripts": {"enable": false}},
        "procMacro": {"enable": false}
    })
}

fn relative_workspace_path(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let absolute = absolute.canonicalize().unwrap_or(absolute);
    absolute.strip_prefix(root).ok().map(Path::to_path_buf)
}

fn truncate_diagnostic_payload(payload: &str) -> String {
    const MAX_BYTES: usize = 64 * 1024;
    if payload.len() <= MAX_BYTES { return payload.to_owned(); }
    let mut end = MAX_BYTES;
    while !payload.is_char_boundary(end) { end -= 1; }
    format!("{}…", &payload[..end])
}

fn reference_locations(result: &serde_json::Value, root: &Path) -> (Vec<ReferenceLocation>, usize) {
    let Some(locations) = result.as_array() else {
        return (Vec::new(), 0);
    };
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut references = Vec::with_capacity(locations.len().min(MAX_REFERENCE_LOCATIONS));
    let mut omitted = 0usize;
    for location in locations {
        let Some(absolute_path) = location.get("uri").and_then(serde_json::Value::as_str).and_then(path_from_file_uri) else {
            continue;
        };
        let canonical_path = absolute_path.canonicalize().unwrap_or(absolute_path);
        let Ok(path) = canonical_path.strip_prefix(&canonical_root) else {
            continue;
        };
        let Some(start) = location.get("range").and_then(|range| range.get("start")) else {
            continue;
        };
        let (Some(line), Some(character)) = (
            start.get("line").and_then(serde_json::Value::as_u64).and_then(|value| usize::try_from(value).ok()),
            start.get("character").and_then(serde_json::Value::as_u64).and_then(|value| usize::try_from(value).ok()),
        ) else {
            continue;
        };
        if references.len() == MAX_REFERENCE_LOCATIONS {
            omitted = omitted.saturating_add(1);
        } else {
            references.push(ReferenceLocation {
                path: path.to_path_buf(),
                line,
                character,
            });
        }
    }
    (references, omitted)
}

fn lsp_position(source: &str, character_offset: usize) -> serde_json::Value {
    let byte_offset = source
        .char_indices()
        .nth(character_offset)
        .map_or(source.len(), |(offset, _)| offset);
    let before_cursor = &source[..byte_offset];
    let line = before_cursor.bytes().filter(|byte| *byte == b'\n').count();
    let character = before_cursor
        .rsplit('\n')
        .next()
        .map_or(0, |line| line.encode_utf16().count());
    serde_json::json!({"line": line, "character": character})
}

fn position_to_byte_offset(source: &str, line: usize, character: usize) -> Option<usize> {
    let mut line_start = 0;
    for (line_index, line_text) in source.split('\n').enumerate() {
        if line_index == line {
            let line_text = line_text.strip_suffix('\r').unwrap_or(line_text);
            let mut utf16_offset = 0;
            for (offset, scalar) in line_text.char_indices() {
                if utf16_offset == character {
                    return Some(line_start + offset);
                }
                utf16_offset += scalar.len_utf16();
                if utf16_offset > character {
                    return None;
                }
            }
            return (utf16_offset == character).then_some(line_start + line_text.len());
        }
        line_start += line_text.len() + 1;
    }
    None
}

fn semantic_object_byte_range(
    index: &SemanticWorkspaceIndex,
    reference: &SemanticObjectRef,
    source: &str,
) -> Option<Range<usize>> {
    let object = index.resolve(reference)?;
    let start = position_to_byte_offset(
        source,
        object.range.start.line as usize,
        object.range.start.character as usize,
    )?;
    let end = position_to_byte_offset(
        source,
        object.range.end.line as usize,
        object.range.end.character as usize,
    )?;
    (start <= end).then_some(start..end)
}

fn apply_text_edits(source: &str, edits: &[serde_json::Value]) -> Result<String, &'static str> {
    let mut ranges = edits
        .iter()
        .map(|edit| {
            let range = edit.get("range").ok_or("an edit has no range")?;
            let start = range.get("start").ok_or("an edit has no start")?;
            let end = range.get("end").ok_or("an edit has no end")?;
            let start = position_to_byte_offset(
                source,
                start
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("an edit start has no line")? as usize,
                start
                    .get("character")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("an edit start has no character")? as usize,
            )
            .ok_or("an edit start is outside the document or splits a Unicode character")?;
            let end = position_to_byte_offset(
                source,
                end.get("line")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("an edit end has no line")? as usize,
                end.get("character")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("an edit end has no character")? as usize,
            )
            .ok_or("an edit end is outside the document or splits a Unicode character")?;
            let replacement = edit
                .get("newText")
                .and_then(serde_json::Value::as_str)
                .ok_or("an edit has no replacement text")?;
            if start > end {
                return Err("an edit range is reversed");
            }
            Ok((start, end, replacement.to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err("formatting edits overlap");
    }
    if ranges.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("formatting edits share an ambiguous insertion point");
    }
    let mut formatted = source.to_owned();
    for (start, end, replacement) in ranges.into_iter().rev() {
        if !formatted.is_char_boundary(start) || !formatted.is_char_boundary(end) {
            return Err("an edit splits a UTF-8 character");
        }
        formatted.replace_range(start..end, &replacement);
    }
    Ok(formatted)
}

fn hover_text(result: &serde_json::Value) -> Option<String> {
    let contents = result.get("contents")?;
    match contents {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Object(_) => contents
            .get("value")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        serde_json::Value::Array(values) => {
            let rendered = values
                .iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .or_else(|| value.get("value").and_then(serde_json::Value::as_str))
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            (!rendered.is_empty()).then_some(rendered)
        }
        _ => None,
    }
}

fn completion_items(result: &serde_json::Value) -> Vec<CompletionItem> {
    result
        .get("items")
        .and_then(serde_json::Value::as_array)
        .or_else(|| result.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_owned();
            let insert_text = if item
                .get("insertTextFormat")
                .and_then(serde_json::Value::as_u64)
                == Some(2)
            {
                label.clone()
            } else {
                item.get("insertText")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&label)
                    .to_owned()
            };
            Some(CompletionItem {
                label,
                insert_text,
                detail: item
                    .get("detail")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

fn identifier_byte_range(source: &str, character_offset: usize) -> (usize, usize) {
    let cursor = source
        .char_indices()
        .nth(character_offset)
        .map_or(source.len(), |(offset, _)| offset);
    let before = &source[..cursor];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
        .last()
        .map_or(cursor, |(offset, _)| offset);
    let end = source[cursor..]
        .char_indices()
        .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
        .last()
        .map_or(cursor, |(offset, character)| {
            cursor + offset + character.len_utf8()
        });
    (start, end)
}

fn find_matches(source: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    source
        .char_indices()
        .filter_map(|(start, _)| {
            let end = start.checked_add(query.len())?;
            source
                .get(start..end)
                .filter(|matched| matched.eq_ignore_ascii_case(query))
                .map(|matched| start..start + matched.len())
        })
        .collect()
}

fn cursor_position(source: &str, character_index: usize) -> (usize, usize) {
    let before_cursor = source.chars().take(character_index).collect::<String>();
    let line = before_cursor.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = before_cursor
        .rsplit('\n')
        .next()
        .map_or(1, |line| line.chars().count() + 1);
    (line, column)
}

fn document_tab_label(path: &Path, open_documents: &BTreeMap<PathBuf, Document>) -> String {
    if path.extension().is_some_and(|extension| extension == "rs") {
        return shortest_rust_label(path, &open_documents.keys().cloned().collect::<Vec<_>>());
    }
    let parts = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for suffix_len in 1..=parts.len() {
        let suffix = parts[parts.len() - suffix_len..].join("/");
        let is_ambiguous = open_documents.keys().any(|other| {
            if other == path {
                return false;
            }
            let other_parts = other
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            other_parts.len() >= suffix_len
                && other_parts[other_parts.len() - suffix_len..].join("/") == suffix
        });
        if !is_ambiguous {
            return suffix;
        }
    }
    path.display().to_string()
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

fn workspace_diagnostic_path(
    workspace: &Path,
    diagnostic_file: &str,
    known_files: &[PathBuf],
) -> Option<PathBuf> {
    let workspace = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let path = Path::new(diagnostic_file);
    if diagnostic_file.trim().is_empty() || diagnostic_file.starts_with("file:") {
        return None;
    }
    if path.is_absolute() {
        let canonical = path.canonicalize().ok()?;
        return canonical.strip_prefix(&workspace).ok().map(Path::to_path_buf);
    }

    // Cargo may report a package-relative path when the package is nested in
    // the workspace. Resolve only a unique suffix match from the IDE's file
    // snapshot; guessing between duplicate module names would misnavigate.
    let matches = known_files
        .iter()
        .filter(|candidate| candidate.ends_with(path))
        .take(2)
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        return Some(matches[0].to_path_buf());
    }
    if matches.len() > 1 {
        return None;
    }

    let canonical = workspace.join(path).canonicalize().ok()?;
    canonical.strip_prefix(workspace).ok().map(Path::to_path_buf)
}

fn reference_request_matches_selection(
    request_path: &Path,
    semantic_target: Option<&SemanticObjectRef>,
    requested_cursor_offset: Option<usize>,
    current_cursor_offset: usize,
    selected_file: Option<&PathBuf>,
    selected_semantic: Option<&SemanticObjectRef>,
) -> bool {
    semantic_target.map_or_else(
        || {
            selected_file.is_some_and(|selected| selected == request_path)
                && requested_cursor_offset.is_none_or(|offset| offset == current_cursor_offset)
        },
        |target| selected_semantic == Some(target),
    )
}

fn operation_produces_build_diagnostics(action: &str) -> bool {
    matches!(
        action.rsplit('.').next(),
        Some("build" | "check" | "test" | "validate")
    )
}

fn diagnostic_origin_label(origin: vapor_gui_core::DiagnosticOrigin) -> &'static str {
    match origin {
        vapor_gui_core::DiagnosticOrigin::RustAnalyzer => "Rust Analyzer",
        vapor_gui_core::DiagnosticOrigin::Cargo => "Cargo",
        vapor_gui_core::DiagnosticOrigin::Rustc => "rustc",
        vapor_gui_core::DiagnosticOrigin::Rustdoc => "rustdoc",
        vapor_gui_core::DiagnosticOrigin::Vapor => "Vapor",
    }
}

fn semantic_source_limit_diagnostic() -> vapor_gui_core::StructuredDiagnostic {
    let message = "Local semantic indexing is paused for files larger than 2 MiB; source editing remains available.";
    vapor_gui_core::StructuredDiagnostic {
        origin: vapor_gui_core::DiagnosticOrigin::Vapor,
        severity: vapor_gui_core::DiagnosticSeverity::Information,
        code: Some("vapor.semantic.source-limit".to_owned()),
        message: message.to_owned(),
        rendered: None,
        primary: None,
        related: Vec::new(),
        raw_payload: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Document, EditPreviewApplyError, ProblemRow, ProblemStore, ReferenceLocation, VersionedEditPreview,
        apply_text_edits, apply_versioned_edit, completion_items,
        operation_produces_build_diagnostics, reference_request_matches_selection,
        pending_lsp_requests_for_cancellation,
        workspace_versions_match,
        semantic_source_limit_diagnostic,
        workspace_diagnostic_path,
        canonical_rust_item_kind,
        cursor_position, document_tab_label, file_display_label, file_uri, find_matches, hover_text,
        identifier_byte_range, lsp_position, nonempty_path,
        path_from_file_uri, position_to_byte_offset, reference_locations,
        relative_workspace_path, rust_analyzer_initialization_options, rust_label_parts,
        rust_crate_roots,
        shortest_rust_label, toml_parse_diagnostic, workspace_tree_ordering,
        semantic_object_byte_range,
    };
    use super::rust_semantics::{
        CanonicalRustItemKind, parse_semantic_symbols, render_canonical_rust_item,
        rust_lsp_symbol_kind,
    };
    use vapor_gui_core::semantic::SemanticObjectKind;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    #[test]
    fn cargo_diagnostics_map_to_workspace_files_and_problem_rows_keep_provenance() {
        let root = std::env::temp_dir().join(format!("vapor-cargo-problems-{}", std::process::id()));
        let source = root.join("src/main.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "fn main() {}\n").unwrap();

        assert_eq!(
            workspace_diagnostic_path(&root, "src/main.rs", &[PathBuf::from("src/main.rs")]),
            Some(PathBuf::from("src/main.rs"))
        );
        assert_eq!(
            workspace_diagnostic_path(&root, "src/main.rs", &[PathBuf::from("crates/game/src/main.rs")]),
            Some(PathBuf::from("crates/game/src/main.rs")),
            "nested package-relative diagnostic paths resolve through the known file list"
        );
        assert_eq!(
            workspace_diagnostic_path(
                &root,
                "src/main.rs",
                &[PathBuf::from("crates/a/src/main.rs"), PathBuf::from("crates/b/src/main.rs")],
            ),
            None,
            "ambiguous suffix matches must not navigate to an arbitrary file"
        );
        let outside = root.with_extension("outside.rs");
        std::fs::write(&outside, "fn outside() {}\n").unwrap();
        assert_eq!(workspace_diagnostic_path(&root, outside.to_str().unwrap(), &[]), None);

        let mut store = ProblemStore::default();
        let language_diagnostic = vapor_gui_core::StructuredDiagnostic {
            origin: vapor_gui_core::DiagnosticOrigin::RustAnalyzer,
            severity: vapor_gui_core::DiagnosticSeverity::Warning,
            code: None,
            message: "language warning".to_owned(),
            rendered: None,
            primary: None,
            related: Vec::new(),
            raw_payload: "{}".to_owned(),
        };
        store.replace_source_diagnostics(
            PathBuf::from("src/main.rs"),
            vapor_gui_core::DiagnosticOrigin::RustAnalyzer,
            vec![language_diagnostic],
        );
        let build_diagnostic = vapor_gui_core::StructuredDiagnostic {
            origin: vapor_gui_core::DiagnosticOrigin::Cargo,
            severity: vapor_gui_core::DiagnosticSeverity::Error,
            code: Some("E0425".to_owned()),
            message: "cannot find value".to_owned(),
            rendered: None,
            primary: None,
            related: Vec::new(),
            raw_payload: "{\"reason\":\"compiler-message\"}".to_owned(),
        };
        store.begin_build("build-1");
        store.push_build("build-1", ProblemRow {
            path: workspace_diagnostic_path(&root, "src/main.rs", &[PathBuf::from("src/main.rs")]),
            operation: Some("workspace.test".to_owned()),
            diagnostic: build_diagnostic.clone(),
        });
        store.push_build("build-1", ProblemRow {
            path: workspace_diagnostic_path(&root, "src/main.rs", &[PathBuf::from("src/main.rs")]),
            operation: Some("workspace.test".to_owned()),
            diagnostic: build_diagnostic,
        });
        assert_eq!(store.count(), 2, "repeated output should not duplicate a build problem");
        let rows = store.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].diagnostic.origin, vapor_gui_core::DiagnosticOrigin::RustAnalyzer);
        assert_eq!(rows[1].diagnostic.origin, vapor_gui_core::DiagnosticOrigin::Cargo);
        assert_eq!(rows[1].operation, Some("workspace.test"));

        store.begin_build("build-2");
        store.push_build("build-1", ProblemRow {
            path: Some(PathBuf::from("src/main.rs")),
            operation: Some("workspace.test".to_owned()),
            diagnostic: vapor_gui_core::StructuredDiagnostic {
                origin: vapor_gui_core::DiagnosticOrigin::Cargo,
                severity: vapor_gui_core::DiagnosticSeverity::Error,
                code: Some("E0425".to_owned()),
                message: "cannot find value".to_owned(),
                rendered: None,
                primary: None,
                related: Vec::new(),
                raw_payload: "{\"reason\":\"compiler-message\"}".to_owned(),
            },
        });
        let rows = store.rows();
        assert_eq!(rows.len(), 1, "late output from a prior build is ignored");
        assert_eq!(rows[0].diagnostic.origin, vapor_gui_core::DiagnosticOrigin::RustAnalyzer);

        std::fs::remove_dir_all(&root).unwrap();
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn source_producers_keep_independent_diagnostics_for_one_document() {
        let mut store = ProblemStore::default();
        let path = PathBuf::from("src/components.rs");
        let rust_analyzer = vapor_gui_core::StructuredDiagnostic {
            origin: vapor_gui_core::DiagnosticOrigin::RustAnalyzer,
            severity: vapor_gui_core::DiagnosticSeverity::Warning,
            code: Some("ra.unused".to_owned()),
            message: "unused field".to_owned(),
            rendered: None,
            primary: None,
            related: Vec::new(),
            raw_payload: String::new(),
        };
        let vapor = vapor_gui_core::StructuredDiagnostic {
            origin: vapor_gui_core::DiagnosticOrigin::Vapor,
            severity: vapor_gui_core::DiagnosticSeverity::Warning,
            code: Some("vapor.rust.layout".to_owned()),
            message: "move this component into components.rs".to_owned(),
            rendered: None,
            primary: None,
            related: Vec::new(),
            raw_payload: String::new(),
        };

        store.replace_source_diagnostics(
            path.clone(),
            vapor_gui_core::DiagnosticOrigin::RustAnalyzer,
            vec![rust_analyzer],
        );
        store.replace_source_diagnostics(
            path.clone(),
            vapor_gui_core::DiagnosticOrigin::Vapor,
            vec![vapor],
        );
        assert_eq!(store.count(), 2);

        store.replace_source_diagnostics(
            path,
            vapor_gui_core::DiagnosticOrigin::RustAnalyzer,
            Vec::new(),
        );
        let rows = store.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].diagnostic.origin, vapor_gui_core::DiagnosticOrigin::Vapor);
    }

    #[test]
    fn problem_rows_materialize_only_the_visible_virtualized_range() {
        let mut store = ProblemStore::default();
        let diagnostics = (0..10_000)
            .map(|index| vapor_gui_core::StructuredDiagnostic {
                origin: vapor_gui_core::DiagnosticOrigin::Vapor,
                severity: vapor_gui_core::DiagnosticSeverity::Information,
                code: None,
                message: index.to_string(),
                rendered: None,
                primary: None,
                related: Vec::new(),
                raw_payload: String::new(),
            })
            .collect();
        store.replace_source_diagnostics(
            PathBuf::from("src/large.rs"),
            vapor_gui_core::DiagnosticOrigin::Vapor,
            diagnostics,
        );
        store.begin_build("build-1");
        store.push_build(
            "build-1",
            ProblemRow {
                path: None,
                operation: Some("workspace.test".to_owned()),
                diagnostic: vapor_gui_core::StructuredDiagnostic {
                    origin: vapor_gui_core::DiagnosticOrigin::Cargo,
                    severity: vapor_gui_core::DiagnosticSeverity::Error,
                    code: None,
                    message: "build row".to_owned(),
                    rendered: None,
                    primary: None,
                    related: Vec::new(),
                    raw_payload: String::new(),
                },
            },
        );

        assert_eq!(store.count(), 10_001);
        let visible = store.rows_in_range(9_998..10_001);
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].diagnostic.message, "9998");
        assert_eq!(visible[1].diagnostic.message, "9999");
        assert_eq!(visible[2].diagnostic.message, "build row");
    }

    #[test]
    fn semantic_reference_results_are_discarded_after_selection_changes() {
        let selected = vapor_gui_core::semantic::SemanticObjectRef {
            id: vapor_gui_core::semantic::SemanticObjectId::new(
                "rust",
                "src/components.rs",
                vec!["Ship".to_owned()],
            ),
            document_version: 4,
        };
        let other = vapor_gui_core::semantic::SemanticObjectRef {
            id: vapor_gui_core::semantic::SemanticObjectId::new(
                "rust",
                "src/components.rs",
                vec!["Pilot".to_owned()],
            ),
            document_version: 4,
        };
        let path = Path::new("src/components.rs");
        assert!(reference_request_matches_selection(
            path, Some(&selected), None, 0, None, Some(&selected)
        ));
        assert!(!reference_request_matches_selection(
            path, Some(&selected), None, 0, None, Some(&other)
        ));
        assert!(reference_request_matches_selection(
            path, None, Some(4), 4, Some(&path.to_path_buf()), None
        ));
        assert!(!reference_request_matches_selection(
            path, None, Some(4), 5, Some(&path.to_path_buf()), None
        ));
        assert!(!reference_request_matches_selection(
            path, None, None, 0, Some(&PathBuf::from("src/lib.rs")), None
        ));
    }

    #[test]
    fn semantic_inspector_reports_the_rust_marker_that_classified_a_type() {
        let component = vapor_gui_core::semantic::SemanticObject {
            id: vapor_gui_core::semantic::SemanticObjectId::new(
                "rust",
                "src/components.rs",
                vec!["Ship".to_owned()],
            ),
            kind: SemanticObjectKind::Component,
            name: "Ship".to_owned(),
            detail: Some("pub struct Ship".to_owned()),
            range: vapor_gui_core::semantic::SemanticRange::default(),
            selection_range: vapor_gui_core::semantic::SemanticRange::default(),
            attributes: vec!["#[derive(bevy::Component, Debug)]".to_owned()],
            children: Vec::new(),
        };
        assert_eq!(
            super::semantic_view::semantic_convention_status(&component).as_deref(),
            Some("Detected from #[derive(bevy::Component, Debug)]")
        );

        let mut resource = component.clone();
        resource.kind = SemanticObjectKind::Resource;
        resource.attributes = vec!["#[derive(Resource)]".to_owned()];
        assert_eq!(
            super::semantic_view::semantic_convention_status(&resource).as_deref(),
            Some("Detected from #[derive(Resource)]")
        );
    }

    #[test]
    fn only_compiler_workflows_replace_build_problems() {
        for action in ["workspace.build", "workspace.check", "workspace.test", "workspace.validate"] {
            assert!(operation_produces_build_diagnostics(action), "{action}");
        }
        for action in ["workspace.fmt", "workspace.publish", "composition.launch"] {
            assert!(!operation_produces_build_diagnostics(action), "{action}");
        }
    }

    #[test]
    fn document_dirty_state_tracks_edits_and_save_baseline() {
        let mut document = Document {
            contents: "fn main() {}".to_owned(),
            saved_contents: "fn main() {}".to_owned(),
            fingerprint: "initial".to_owned(),
            version: 1,
        };
        assert!(!document.is_dirty());
        document.contents.push('\n');
        assert!(document.is_dirty());
        document.saved_contents.clone_from(&document.contents);
        assert!(!document.is_dirty());
    }

    #[test]
    fn edit_preview_requires_the_exact_document_revision_and_original_text() {
        let preview = VersionedEditPreview {
            path: PathBuf::from("src/lib.rs"),
            document_version: 4,
            original_contents: "fn main(){ }".to_owned(),
            proposed_contents: "fn main() { }".to_owned(),
        };
        let mut current = Document {
            contents: preview.original_contents.clone(),
            saved_contents: preview.original_contents.clone(),
            fingerprint: "unchanged-on-disk".to_owned(),
            version: 4,
        };
        assert_eq!(apply_versioned_edit(&mut current, &preview), Ok(5));
        assert_eq!(current.contents, preview.proposed_contents);
        assert!(current.is_dirty());

        let mut stale = Document { version: 5, ..current.clone() };
        assert_eq!(apply_versioned_edit(&mut stale, &preview), Err(EditPreviewApplyError::Stale));

        let mut changed = Document { contents: "concurrent edit".to_owned(), ..current };
        assert_eq!(apply_versioned_edit(&mut changed, &preview), Err(EditPreviewApplyError::Stale));
    }

    #[test]
    fn save_completion_preserves_edits_made_while_the_save_was_running() {
        let mut document = Document {
            contents: "fn main() { first(); }".to_owned(),
            saved_contents: "fn main() {}".to_owned(),
            fingerprint: "before-save".to_owned(),
            version: 1,
        };
        let submitted_contents = document.contents.clone();
        document.contents.push_str(" second();");

        document.record_saved_contents(submitted_contents, "after-save".to_owned());

        assert!(document.is_dirty());
        assert_eq!(document.fingerprint, "after-save");
        assert_eq!(document.saved_contents, "fn main() { first(); }");
    }

    #[test]
    fn workspace_input_treats_blank_as_no_selection() {
        assert_eq!(nonempty_path("  "), None);
        assert_eq!(
            nonempty_path(" /workspace ").unwrap().to_string_lossy(),
            "/workspace"
        );
    }

    #[test]
    fn find_matches_returns_case_insensitive_utf8_byte_ranges() {
        assert_eq!(find_matches("Run λx run λx", "λx"), [4..7, 12..15]);
        assert_eq!(find_matches("Run λx run λx", "RUN"), [0..3, 8..11]);
        assert!(find_matches("nothing here", "").is_empty());
    }

    #[test]
    fn cursor_status_counts_unicode_characters_and_lines() {
        assert_eq!(cursor_position("αx\nok", 4), (2, 2));
        assert_eq!(cursor_position("", 0), (1, 1));
    }

    #[test]
    fn file_uri_round_trips_spaces_and_unicode() {
        let path = Path::new("/tmp/Vapor IDE/α file.rs");
        let uri = file_uri(path);
        assert!(uri.contains("%20"));
        assert!(uri.contains("%CE%B1"));
        assert_eq!(path_from_file_uri(&uri).as_deref(), Some(path));
    }

    #[test]
    fn rust_analyzer_initialization_disables_implicit_cargo_build_work() {
        let options = rust_analyzer_initialization_options();
        assert_eq!(options["checkOnSave"], false);
        assert_eq!(options["cargo"]["buildScripts"]["enable"], false);
        assert_eq!(options["procMacro"]["enable"], false);
    }

    #[test]
    fn workspace_diagnostic_paths_normalize_absolute_and_relative_files() {
        let root = std::env::temp_dir().join("vapor-ide-workspace-path-test");
        let file = root.join("src/main.rs");
        assert_eq!(relative_workspace_path(&root, &file), Some(PathBuf::from("src/main.rs")));
        assert_eq!(relative_workspace_path(&root, Path::new("src/main.rs")), Some(PathBuf::from("src/main.rs")));
    }

    #[test]
    fn rust_analyzer_symbol_kinds_follow_lsp_symbol_kind_numbers() {
        assert_eq!(rust_lsp_symbol_kind(23), SemanticObjectKind::Struct);
        assert_eq!(rust_lsp_symbol_kind(24), SemanticObjectKind::Other);
        assert_eq!(rust_lsp_symbol_kind(25), SemanticObjectKind::Other);
        assert_eq!(rust_lsp_symbol_kind(26), SemanticObjectKind::Other);
    }

    #[test]
    fn semantic_search_finds_component_metadata_and_member_types() {
        let component = vapor_gui_core::semantic::SemanticObject {
            id: vapor_gui_core::semantic::SemanticObjectId::new(
                "rust",
                "src/components.rs",
                vec!["Ship".to_owned()],
            ),
            kind: SemanticObjectKind::Component,
            name: "Ship".to_owned(),
            detail: Some("pub struct Ship".to_owned()),
            range: vapor_gui_core::semantic::SemanticRange::default(),
            selection_range: vapor_gui_core::semantic::SemanticRange::default(),
            attributes: vec!["#[derive(Component)]".to_owned()],
            children: vec![vapor_gui_core::semantic::SemanticObject {
                id: vapor_gui_core::semantic::SemanticObjectId::new(
                    "rust",
                    "src/components.rs",
                    vec!["Ship".to_owned(), "velocity".to_owned()],
                ),
                kind: SemanticObjectKind::Field,
                name: "velocity".to_owned(),
                detail: Some("Vec3".to_owned()),
                range: vapor_gui_core::semantic::SemanticRange::default(),
                selection_range: vapor_gui_core::semantic::SemanticRange::default(),
                attributes: vec!["#[serde(default)]".to_owned()],
                children: Vec::new(),
            }],
        };

        assert!(super::semantic_view::semantic_tree_matches(&component, "vec3"));
        assert!(super::semantic_view::semantic_tree_matches(&component, "derive(component)"));
        assert!(super::semantic_view::semantic_tree_matches(&component, "component"));
        assert!(super::semantic_view::semantic_tree_matches(&component, "serde(default)"));
    }

    #[test]
    fn rust_analyzer_symbols_become_component_objects_with_typed_fields() {
        let source = "#[derive(bevy::ecs::component::Component, Debug)]\npub struct Ship {\n    #[serde(default)]\n    pub velocity: Vec3,\n}\n";
        let response = serde_json::json!([{
            "name": "Ship",
            "kind": 23,
            "detail": "pub struct Ship",
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 4, "character": 1}},
            "selectionRange": {"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 15}},
            "children": [{
                "name": "velocity",
                "kind": 8,
                "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 23}},
                "selectionRange": {"start": {"line": 3, "character": 8}, "end": {"line": 3, "character": 16}}
            }]
        }]);
        let symbols = parse_semantic_symbols(&response, Path::new("src/components.rs"), 7, source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, SemanticObjectKind::Component);
        assert!(symbols[0].attributes.iter().any(|attribute| attribute.contains("derive")));
        assert_eq!(symbols[0].children[0].detail.as_deref(), Some("Vec3"));
        assert_eq!(symbols[0].children[0].attributes, ["#[serde(default)]"]);
    }

    #[test]
    fn rust_syntax_adapter_classifies_resources_and_system_files_without_building() {
        let source = "#[derive(Resource)]\npub struct StarMap;\npub fn update_stars() {}\n";
        let symbols = serde_json::json!([
            {"name":"StarMap","kind":23,"detail":"pub struct StarMap","range":{"start":{"line":0,"character":0},"end":{"line":1,"character":16}},"selectionRange":{"start":{"line":1,"character":11},"end":{"line":1,"character":18}}},
            {"name":"update_stars","kind":12,"detail":"pub fn update_stars()","range":{"start":{"line":2,"character":0},"end":{"line":2,"character":24}},"selectionRange":{"start":{"line":2,"character":7},"end":{"line":2,"character":19}}}
        ]);
        let indexed = parse_semantic_symbols(&symbols, Path::new("src/systems.rs"), 1, source);
        assert_eq!(indexed[0].kind, SemanticObjectKind::Resource);
        assert_eq!(indexed[1].kind, SemanticObjectKind::System);
    }

    #[test]
    fn rust_document_symbol_index_has_a_visible_bound_for_large_responses() {
        let symbols = (0..5_000)
            .map(|index| serde_json::json!({
                "name": format!("Generated{index}"),
                "kind": 5,
                "range": {"start":{"line":index,"character":0},"end":{"line":index,"character":8}},
                "selectionRange": {"start":{"line":index,"character":6},"end":{"line":index,"character":8}}
            }))
            .collect::<Vec<_>>();
        let response = serde_json::Value::Array(symbols);

        let parsed = super::rust_semantics::parse_semantic_document_symbols(
            &response,
            Path::new("src/generated.rs"),
            1,
            "",
        );

        assert_eq!(parsed.objects.len(), 4_096);
        assert!(parsed.truncated);
    }

    #[test]
    fn rust_document_symbol_index_stops_pathological_nesting() {
        let mut nested = serde_json::json!({"name":"Leaf","kind":5});
        for index in (0..80).rev() {
            nested = serde_json::json!({
                "name": format!("Module{index}"),
                "kind": 2,
                "children": [nested]
            });
        }
        let parsed = super::rust_semantics::parse_semantic_document_symbols(
            &serde_json::Value::Array(vec![nested]),
            Path::new("src/generated.rs"),
            1,
            "",
        );

        assert!(parsed.truncated);
        assert_eq!(parsed.objects.len(), 1);
        let mut depth = 0;
        let mut current = &parsed.objects[0];
        while let Some(child) = current.children.first() {
            current = child;
            depth += 1;
        }
        assert_eq!(depth, 64);
    }

    #[test]
    fn rust_layout_rules_report_specialized_types_outside_their_canonical_modules() {
        let source = "#[derive(Component)]\npub struct Ship;\n";
        let response = serde_json::json!([{
            "name": "Ship",
            "kind": 23,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 1, "character": 15}},
            "selectionRange": {"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 15}}
        }]);
        let misplaced = parse_semantic_symbols(&response, Path::new("src/engine.rs"), 1, source);
        let diagnostics = super::rust_semantics::convention_diagnostics(
            Path::new("src/engine.rs"),
            &misplaced,
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("vapor.rust.layout.component"));
        assert_eq!(diagnostics[0].primary.as_ref().unwrap().start_line, 2);

        let canonical = parse_semantic_symbols(&response, Path::new("src/components.rs"), 1, source);
        assert!(super::rust_semantics::convention_diagnostics(
            Path::new("src/components.rs"),
            &canonical,
        )
        .is_empty());
    }

    #[test]
    fn rust_mod_labels_disambiguate_same_named_file_and_module() {
        let paths = [PathBuf::from("crates/flight/src/game.rs"), PathBuf::from("crates/flight/src/game/mod.rs")];
        let file_label = file_display_label(&paths[0], &paths);
        let module_label = file_display_label(&paths[1], &paths);

        assert_ne!(file_label, module_label);
        assert_eq!(file_label, "flight/game");
        assert_eq!(module_label, "flight/game/mod");
        assert!(!module_label.ends_with(".rs"));
    }

    #[test]
    fn rust_layout_rules_cover_generic_types_functions_constants_and_statics() {
        let source = "pub struct FlightModel;\npub fn update_flight() {}\npub const MAX_THRUST: f32 = 1.0;\npub static VERSION: &str = \"0.1\";\n";
        let response = serde_json::json!([
            {"name":"FlightModel","kind":23,"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":22}},"selectionRange":{"start":{"line":0,"character":11},"end":{"line":0,"character":22}}},
            {"name":"update_flight","kind":12,"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":23}},"selectionRange":{"start":{"line":1,"character":7},"end":{"line":1,"character":20}}},
            {"name":"MAX_THRUST","kind":14,"range":{"start":{"line":2,"character":0},"end":{"line":2,"character":32}},"selectionRange":{"start":{"line":2,"character":10},"end":{"line":2,"character":20}}},
            {"name":"VERSION","kind":13,"range":{"start":{"line":3,"character":0},"end":{"line":3,"character":36}},"selectionRange":{"start":{"line":3,"character":11},"end":{"line":3,"character":18}}}
        ]);
        let objects = parse_semantic_symbols(&response, Path::new("src/engine.rs"), 1, source);
        let diagnostics = super::rust_semantics::convention_diagnostics(
            Path::new("src/engine.rs"),
            &objects,
        );
        let codes = diagnostics.iter().filter_map(|diagnostic| diagnostic.code.as_deref()).collect::<Vec<_>>();
        assert_eq!(codes, [
            "vapor.rust.layout.type",
            "vapor.rust.layout.function",
            "vapor.rust.layout.constant",
            "vapor.rust.layout.static",
        ]);
        assert!(diagnostics.iter().all(|diagnostic| diagnostic.message.contains("belongs in")));
    }

    #[test]
    fn rust_workspace_symbols_are_project_scoped_and_keep_source_ranges() {
        let response = serde_json::json!([
            {
                "name": "Ship",
                "kind": 23,
                "containerName": "game::components",
                "location": {
                    "uri": "file:///fixture/workspace/src/components.rs",
                    "range": {
                        "start": {"line": 8, "character": 10},
                        "end": {"line": 8, "character": 14}
                    }
                }
            },
            {
                "name": "External",
                "kind": 23,
                "location": {
                    "uri": "file:///fixture/outside.rs",
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 8}
                    }
                }
            }
        ]);
        let symbols = super::rust_semantics::parse_workspace_symbols(
            &response,
            Path::new("/fixture/workspace"),
        );

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Ship");
        assert_eq!(symbols[0].id.document, Path::new("src/components.rs"));
        assert_eq!(symbols[0].id.qualified_name(), "game::components::Ship");
        assert_eq!(symbols[0].range.start.line, 8);
        assert_eq!(symbols[0].detail.as_deref(), Some("game::components"));
    }

    #[test]
    fn superseded_document_symbol_requests_are_scoped_to_the_same_file() {
        let pending = |request, path: &str| super::PendingLspRequest {
            request,
            path: PathBuf::from(path),
            document_version: 1,
            semantic_target: None,
            cursor_offset: None,
            workspace_versions: None,
            query: None,
        };
        let requests = BTreeMap::from([
            (3, pending(super::LspRequest::DocumentSymbols, "src/a.rs")),
            (4, pending(super::LspRequest::DocumentSymbols, "src/b.rs")),
            (5, pending(super::LspRequest::Hover, "src/a.rs")),
            (6, pending(super::LspRequest::WorkspaceSymbols, "src/a.rs")),
        ]);

        assert_eq!(
            pending_lsp_requests_for_cancellation(
                &requests,
                super::LspRequest::DocumentSymbols,
                Some(Path::new("src/a.rs")),
            ),
            [3]
        );
    }

    #[test]
    fn rust_workspace_results_require_the_exact_open_document_revision_set() {
        let mut documents = BTreeMap::from([
            (PathBuf::from("src/lib.rs"), Document { contents: String::new(), fingerprint: String::new(), saved_contents: String::new(), version: 4 }),
            (PathBuf::from("src/game.rs"), Document { contents: String::new(), fingerprint: String::new(), saved_contents: String::new(), version: 9 }),
        ]);
        let snapshot = BTreeMap::from([
            (PathBuf::from("src/lib.rs"), 4),
            (PathBuf::from("src/game.rs"), 9),
        ]);
        assert!(workspace_versions_match(&snapshot, &documents));

        documents.get_mut(Path::new("src/game.rs")).unwrap().version += 1;
        assert!(!workspace_versions_match(&snapshot, &documents));
        documents.get_mut(Path::new("src/game.rs")).unwrap().version = 9;
        documents.remove(Path::new("src/lib.rs"));
        assert!(!workspace_versions_match(&snapshot, &documents));
    }

    #[test]
    fn toml_semantics_preserve_nested_source_ranges_and_utf16_positions() {
        let source = "title = \"λ\"\n\n[[root.content]]\nid = \"ghf/example\"\nversion = \"0.1.0\"\n";
        let objects = super::toml_semantics::parse_toml_semantic_objects(Path::new("Workspace.vapor.toml"), source).unwrap();
        assert_eq!(objects[0].id.domain, "vapor-workspace");
        assert_eq!(objects[0].selection_range.start.line, 0);
        let root = &objects[1];
        assert_eq!(root.name, "root");
        let content = &root.children[0];
        assert_eq!(content.name, "content");
        let entry = &content.children[0];
        assert_eq!(entry.name, "[0]");
        assert_eq!(entry.children[0].name, "id");
        assert_eq!(entry.children[0].detail.as_deref(), Some("string"));
        assert!(entry.children[0].selection_range.start.line > entry.selection_range.start.line);
    }

    #[test]
    fn vapor_manifest_objects_explain_identity_and_content_references() {
        let source = "[packagepack]\nname = \"demo\"\nversion = \"0.1.0\"\n[packagepack.game]\nid = \"ghf-studios/example/game\"\n";
        let objects = super::toml_semantics::parse_toml_semantic_objects(Path::new("Packagepack.vapor.toml"), source)
            .expect("valid Vapor TOML");
        let pack = &objects[0];
        assert_eq!(pack.id.domain, "vapor-packagepack");
        let name = pack.children.iter().find(|child| child.name == "name").unwrap();
        assert_eq!(name.attributes, ["Declared local identity"]);
        let game = pack.children.iter().find(|child| child.name == "game").unwrap();
        assert_eq!(game.name, "game");
        assert_eq!(game.children[0].attributes, ["Vapor content reference"]);
        assert_eq!(game.children[0].id.qualified_name(), "packagepack.game.id");
    }

    #[test]
    fn toml_semantics_index_inline_tables_and_array_entries_without_values() {
        let source = "items = [{ id = \"private-value\", flags = [\"a\", \"b\"] }]\n";
        let objects = super::toml_semantics::parse_toml_semantic_objects(Path::new("Packagepack.vapor.toml"), source)
            .expect("valid TOML");
        let items = &objects[0];
        assert_eq!(items.kind, SemanticObjectKind::Array);
        let entry = &items.children[0];
        assert_eq!(entry.kind, SemanticObjectKind::Configuration);
        assert_eq!(entry.children[0].name, "id");
        assert_eq!(entry.children[0].detail.as_deref(), Some("string"));
        assert!(!format!("{objects:?}").contains("private-value"));
        assert_eq!(items.children[0].children[1].children.len(), 2);
    }

    #[test]
    fn xml_parse_errors_anchor_the_parser_offset_without_copying_source_values() {
        let source = "<root><item value=\"secret-marker\"></root>";
        let error = super::xml_semantics::parse_xml_semantic_objects(Path::new("config.xml"), source)
            .expect_err("mismatched XML elements");
        let diagnostic = super::xml_parse_diagnostic(Path::new("config.xml"), source, &error);
        assert_eq!(diagnostic.code.as_deref(), Some("vapor.xml.parse"));
        assert!(diagnostic.primary.is_some());
        assert!(!diagnostic.message.contains("secret-marker"));
        assert!(!diagnostic.raw_payload.contains("secret-marker"));
    }

    #[test]
    fn toml_parse_errors_are_anchored_and_do_not_retain_source_values() {
        let source = "secret = \"sensitive-value\"\n[broken\n";
        let error = super::toml_semantics::parse_toml_semantic_objects(Path::new("Vapor.toml"), source)
            .expect_err("malformed TOML should be rejected");
        let diagnostic = toml_parse_diagnostic(Path::new("Vapor.toml"), source, &error);
        assert_eq!(diagnostic.origin, vapor_gui_core::DiagnosticOrigin::Vapor);
        assert_eq!(diagnostic.code.as_deref(), Some("vapor.toml.parse"));
        assert!(diagnostic.primary.as_ref().is_some_and(|span| span.primary));
        assert!(!diagnostic.raw_payload.contains("sensitive-value"));
    }

    #[test]
    fn rust_labels_hide_extensions_and_use_shortest_unique_module_path() {
        let candidates = [
            Path::new("crates/engine/src/game/mod.rs").to_path_buf(),
            Path::new("crates/editor/src/game/mod.rs").to_path_buf(),
            Path::new("crates/engine/src/main.rs").to_path_buf(),
        ];
        assert_eq!(rust_label_parts(&candidates[2]), ["main"]);
        assert_eq!(file_display_label(&candidates[2], &candidates), "main");
        assert_eq!(shortest_rust_label(&candidates[0], &candidates), "engine/game");
        assert_eq!(shortest_rust_label(&candidates[1], &candidates), "editor/game");
    }

    #[test]
    fn rust_tree_ordering_is_limited_to_actual_cargo_crate_roots() {
        let files = [
            PathBuf::from("Cargo.toml"),
            PathBuf::from("scripts/helper.rs"),
            PathBuf::from("crates/core/Cargo.toml"),
            PathBuf::from("crates/core/src/lib.rs"),
            PathBuf::from("crates/core/src/z.rs"),
            PathBuf::from("crates/core/src/a/mod.rs"),
        ];
        assert_eq!(
            rust_crate_roots(&files),
            std::collections::BTreeSet::from([PathBuf::from("crates/core")])
        );
        let crate_roots = rust_crate_roots(&files);
        assert_eq!(
            workspace_tree_ordering(Path::new(""), &crate_roots),
            super::WorkspaceTreeOrdering::DirectoriesFirst
        );
        assert_eq!(
            workspace_tree_ordering(Path::new("crates/core"), &crate_roots),
            super::WorkspaceTreeOrdering::Alphabetical
        );
        assert_eq!(
            workspace_tree_ordering(Path::new("docs"), &crate_roots),
            super::WorkspaceTreeOrdering::DirectoriesFirst
        );
        let all_files = [
            PathBuf::from("crates/core/src/main.rs"),
            PathBuf::from("crates/core/src/a/mod.rs"),
            PathBuf::from("crates/other/src/main.rs"),
        ];
        assert_eq!(shortest_rust_label(&all_files[0], &all_files), "core/main");
        assert_eq!(shortest_rust_label(&all_files[1], &all_files), "a");
        assert_eq!(shortest_rust_label(&all_files[2], &all_files), "other/main");
    }

    #[test]
    fn reference_results_are_workspace_relative_and_skip_external_paths() {
        let mut locations = (0..650)
            .map(|_| serde_json::json!({
                "uri": "file:///workspace/src/lib.rs",
                "range": {"start": {"line": 4, "character": 8}}
            }))
            .collect::<Vec<_>>();
        locations.push(serde_json::json!({
            "uri": "file:///outside/lib.rs",
            "range": {"start": {"line": 0, "character": 0}}
        }));
        let (references, omitted) = reference_locations(
            &serde_json::Value::Array(locations),
            Path::new("/workspace"),
        );
        assert_eq!(references.len(), 500, "reference results must be bounded");
        assert_eq!(omitted, 150, "the UI should explain its visible-result cap");
        assert_eq!(
            references[0],
            ReferenceLocation {
                path: Path::new("src/lib.rs").to_path_buf(),
                line: 4,
                character: 8,
            },
            "LSP source positions stay zero-based UTF-16 offsets for navigation"
        );
    }

    #[test]
    fn lsp_positions_use_utf16_and_navigate_back_to_utf8_offsets() {
        let source = "aλ🦀\r\nnext";
        assert_eq!(
            lsp_position(source, 3),
            serde_json::json!({"line": 0, "character": 4})
        );
        assert_eq!(position_to_byte_offset(source, 0, 4), Some("aλ🦀".len()));
        assert_eq!(position_to_byte_offset(source, 1, 2), Some("aλ🦀\r\nne".len()));
    }

    #[test]
    fn selecting_a_semantic_object_highlights_its_current_source_range() {
        let source = "struct Ship {\n    velocity: Vec3,\n}\n";
        let object = vapor_gui_core::semantic::SemanticObject {
            id: vapor_gui_core::semantic::SemanticObjectId::new(
                "rust",
                "src/components.rs",
                vec!["Ship".to_owned()],
            ),
            kind: SemanticObjectKind::Component,
            name: "Ship".to_owned(),
            detail: None,
            range: vapor_gui_core::semantic::SemanticRange {
                start: vapor_gui_core::semantic::SemanticPosition { line: 0, character: 0 },
                end: vapor_gui_core::semantic::SemanticPosition { line: 2, character: 1 },
            },
            selection_range: vapor_gui_core::semantic::SemanticRange::default(),
            attributes: Vec::new(),
            children: Vec::new(),
        };
        let reference = object.reference(5);
        let mut index = vapor_gui_core::semantic::SemanticWorkspaceIndex::default();
        index.replace_document("src/components.rs", 5, vec![object]);

        assert_eq!(
            semantic_object_byte_range(&index, &reference, source),
            Some(0..source.find("}\n").unwrap() + 1),
        );
        index.replace_document("src/components.rs", 6, Vec::new());
        assert!(semantic_object_byte_range(&index, &reference, source).is_none());
    }

    #[test]
    fn large_rust_files_disable_semantic_expansion_without_losing_editing_diagnostic() {
        let source = "x".repeat(vapor_gui_core::semantic::MAX_SEMANTIC_DOCUMENT_BYTES + 1);
        let parsed = super::rust_semantics::parse_semantic_document_symbols(
            &serde_json::json!([{"name":"large"}]),
            Path::new("src/large.rs"),
            1,
            &source,
        );
        let diagnostic = semantic_source_limit_diagnostic();

        assert!(parsed.objects.is_empty());
        assert!(parsed.truncated);
        assert_eq!(diagnostic.code.as_deref(), Some("vapor.semantic.source-limit"));
        assert!(diagnostic.message.contains("editing remains available"));
        assert!(diagnostic.raw_payload.len() < 256);
    }

    #[test]
    fn invalid_lsp_coordinates_are_rejected_instead_of_clamped() {
        let source = "a🦀\nnext";
        assert_eq!(position_to_byte_offset(source, 0, 2), None);
        assert_eq!(position_to_byte_offset(source, 0, 4), None);
        assert_eq!(position_to_byte_offset(source, 2, 0), None);
    }

    #[test]
    fn formatting_edits_apply_in_reverse_order_with_utf16_ranges() {
        let source = "fn main() { let café = 1; }\n";
        let edits = serde_json::json!([
            {
                "range": {"start": {"line": 0, "character": 16}, "end": {"line": 0, "character": 20}},
                "newText": "coffee"
            },
            {
                "range": {"start": {"line": 0, "character": 21}, "end": {"line": 0, "character": 24}},
                "newText": "= 2"
            }
        ]);
        assert_eq!(
            apply_text_edits(source, edits.as_array().unwrap()).unwrap(),
            "fn main() { let coffee = 2; }\n"
        );
    }

    #[test]
    fn formatting_edits_reject_overlapping_ranges() {
        let edits = serde_json::json!([
            {
                "range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 4}},
                "newText": "a"
            },
            {
                "range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 5}},
                "newText": "b"
            }
        ]);
        assert_eq!(
            apply_text_edits("abcdef", edits.as_array().unwrap()),
            Err("formatting edits overlap")
        );
    }

    #[test]
    fn workspace_edits_reject_out_of_range_and_split_surrogate_positions() {
        let split_surrogate = serde_json::json!([{
            "range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 3}},
            "newText": "x"
        }]);
        assert!(apply_text_edits("a🦀", split_surrogate.as_array().unwrap()).is_err());

        let invalid_line = serde_json::json!([{
            "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}},
            "newText": "x"
        }]);
        assert!(apply_text_edits("a🦀", invalid_line.as_array().unwrap()).is_err());
    }

    #[test]
    fn hover_supports_plain_markup_and_marked_string_contents() {
        assert_eq!(
            hover_text(&serde_json::json!({"contents": {"kind": "markdown", "value": "`Thing`"}})),
            Some("`Thing`".to_owned())
        );
        assert_eq!(
            hover_text(
                &serde_json::json!({"contents": [{"language": "rust", "value": "fn f()"}, "extra"]})
            ),
            Some("fn f()\n\nextra".to_owned())
        );
    }

    #[test]
    fn completion_payloads_keep_labels_details_and_plain_insert_text() {
        let items = completion_items(&serde_json::json!({"items": [
            {"label": "println!", "insertText": "println!", "detail": "macro"},
            {"label": "format!", "insertText": "format!($0)", "insertTextFormat": 2}
        ]}));
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].insert_text, "println!");
        assert_eq!(items[0].detail.as_deref(), Some("macro"));
        assert_eq!(items[1].insert_text, "format!");
    }

    #[test]
    fn completion_replacement_range_tracks_identifier_edges_with_unicode() {
        assert_eq!(identifier_byte_range("let λname = 1", 7), (4, 10));
        assert_eq!(identifier_byte_range("let name = 1", 5), (4, 8));
    }

    #[test]
    fn editor_tabs_disambiguate_duplicate_file_names_with_shortest_path_suffix() {
        let open = BTreeMap::from([
            (
                "src/a/mod.rs".into(),
                Document {
                    contents: String::new(),
                    saved_contents: String::new(),
                    fingerprint: String::new(),
                    version: 1,
                },
            ),
            (
                "src/b/mod.rs".into(),
                Document {
                    contents: String::new(),
                    saved_contents: String::new(),
                    fingerprint: String::new(),
                    version: 1,
                },
            ),
            (
                "src/main.rs".into(),
                Document {
                    contents: String::new(),
                    saved_contents: String::new(),
                    fingerprint: String::new(),
                    version: 1,
                },
            ),
        ]);

        assert_eq!(
            document_tab_label(Path::new("src/a/mod.rs"), &open),
            "a"
        );
        assert_eq!(
            document_tab_label(Path::new("src/b/mod.rs"), &open),
            "b"
        );
        assert_eq!(
            document_tab_label(Path::new("src/main.rs"), &open),
            "main"
        );
    }
    #[test]
    fn canonical_rust_item_templates_follow_the_selected_module() {
        let source = "//! Components for the game.\n";
        let rendered = render_canonical_rust_item(
            Path::new("src/components.rs"),
            source,
            "Ship",
        )
        .expect("component template");
        assert_eq!(
            rendered,
            "//! Components for the game.\n\n#[derive(bevy::prelude::Component)]\npub struct Ship;\n"
        );
        assert_eq!(
            render_canonical_rust_item(Path::new("src/types.rs"), "", "Ship").unwrap(),
            "pub struct Ship;\n"
        );
    }

    #[test]
    fn canonical_rust_item_templates_reject_invalid_or_duplicate_names() {
        let path = Path::new("src/components.rs");
        assert!(render_canonical_rust_item(path, "", "not a type").is_err());
        assert!(render_canonical_rust_item(path, "pub struct Ship;", "Ship").is_err());
        assert!(render_canonical_rust_item(path, "pub fn Ship() {}", "Ship").is_err());
        assert!(render_canonical_rust_item(path, "use crate::Existing as Ship;", "Ship").is_err());
        assert!(render_canonical_rust_item(path, "pub mod Ship {}", "Ship").is_err());
        assert!(render_canonical_rust_item(path, "", "ship").is_err());
        assert!(render_canonical_rust_item(path, "", &"A".repeat(129)).is_err());
        assert!(render_canonical_rust_item(Path::new("src/functions.rs"), "", "buildScene").is_err());
        assert!(render_canonical_rust_item(Path::new("src/constants.rs"), "", "Title").is_err());
        assert!(render_canonical_rust_item(Path::new("src/functions.rs"), "pub const run: &str = \"\";", "run").is_err());
        assert!(render_canonical_rust_item(Path::new("src/functions.rs"), "not rust", "new_item").is_err());
        assert!(render_canonical_rust_item(Path::new("tests/components.rs"), "", "Ship").is_err());
        assert!(render_canonical_rust_item(path, "pub struct r#type;", "r#type").is_err());
    }

    #[test]
    fn canonical_rust_templates_only_enable_inside_a_cargo_src_tree() {
        let roots = BTreeSet::from([PathBuf::from("crate")]);
        assert!(canonical_rust_item_kind(Path::new("crate/src/components.rs"), &roots).is_some());
        assert!(canonical_rust_item_kind(Path::new("crate/tests/components.rs"), &roots).is_none());
        assert!(canonical_rust_item_kind(Path::new("other/src/components.rs"), &roots).is_none());
        assert!(canonical_rust_item_kind(Path::new("crate/src/components.txt"), &roots).is_none());
    }

    #[test]
    fn canonical_rust_item_templates_cover_each_owned_module_kind() {
        for (path, name, expected) in [
            ("src/components.rs", "Ship", "bevy::prelude::Component"),
            ("src/resources.rs", "Assets", "bevy::prelude::Resource"),
            ("src/events.rs", "Changed", "bevy::prelude::Event"),
            ("src/types.rs", "GameState", "pub struct GameState;"),
            ("src/functions.rs", "build_scene", "pub fn build_scene()"),
            ("src/systems.rs", "update_scene", "pub fn update_scene()"),
            ("src/run_conditions.rs", "is_ready", "pub fn is_ready() -> bool"),
            ("src/constants.rs", "TITLE", "pub const TITLE: &str"),
            ("src/statics.rs", "DEFAULT_TITLE", "pub static DEFAULT_TITLE: &str"),
        ] {
            let rendered = render_canonical_rust_item(Path::new(path), "", name).unwrap();
            assert!(rendered.contains(expected), "{path} should use its own template");
            syn::parse_file(&rendered).expect("template produces valid Rust syntax");
        }
        assert!(matches!(
            CanonicalRustItemKind::for_document(Path::new("src/systems.rs")),
            Some(CanonicalRustItemKind::System)
        ));
        assert!(matches!(
            CanonicalRustItemKind::for_document(Path::new("src/run_conditions.rs")),
            Some(CanonicalRustItemKind::RunCondition)
        ));
        assert!(CanonicalRustItemKind::for_document(Path::new("src/lib.rs")).is_none());
    }

}
