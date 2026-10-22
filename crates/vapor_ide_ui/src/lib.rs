use eframe::egui;
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use vapor_client::{Client, ClientHostConfig, service_sibling_path};
use vapor_client_protocol::{
    ClientAction, ClientEvent, LanguageServerAction, OperationStatus, ProtocolFrame,
    WorkspaceAction, WorkspaceFileResult, WorkspaceProjectSelection, WorkspaceToolOperation,
};
use vapor_gui_core::{ConsoleLevel, ConsoleRecord};
use vapor_gui_core::workspace_tree::{WorkspaceFileTree, WorkspaceTreeEntry};

#[derive(Clone)]
struct Document {
    contents: String,
    fingerprint: String,
    saved_contents: String,
    version: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Diagnostic {
    line: usize,
    character: usize,
    severity: Option<u64>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceLocation {
    path: PathBuf,
    line: usize,
    character: usize,
}

#[derive(Clone, Copy)]
enum LanguageServerRequest {
    Start,
    Stop,
}

#[derive(Clone, Copy)]
enum LspRequest {
    Definition,
    References,
    Hover,
    Completion,
    Formatting,
}

struct PendingLspRequest {
    request: LspRequest,
    path: PathBuf,
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
    inspector_open: bool,
    new_file_dialog: bool,
    new_file_path: String,
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
    completion_items: Vec<CompletionItem>,
    diagnostics: BTreeMap<PathBuf, Vec<Diagnostic>>,
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
            inspector_open: true,
            new_file_dialog: false,
            new_file_path: String::new(),
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
            completion_items: Vec::new(),
            diagnostics: BTreeMap::new(),
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
        self.diagnostics.clear();
        self.send(
            ClientAction::Workspace(WorkspaceAction::Open {
                root: root.display().to_string(),
            }),
            PendingAction::OpenWorkspace(root),
        );
    }

    fn send_language_server_action(
        &mut self,
        action: LanguageServerAction,
        request: LanguageServerRequest,
    ) {
        let Some(client) = self.service.as_mut() else {
            self.language_server_status = "Vapor service is unavailable".to_owned();
            return;
        };
        match client.send(ClientAction::LanguageServer(action), None) {
            Ok(request_id) => {
                self.language_server_requests.insert(request_id.0, request);
            }
            Err(error) => {
                self.language_server_ready = false;
                self.language_server_status = format!("Rust Analyzer request failed: {error}");
            }
        }
    }

    fn start_language_server(&mut self) {
        let Some(root) = &self.workspace else {
            return;
        };
        self.language_server_ready = false;
        self.next_lsp_request_id = 2;
        self.lsp_requests.clear();
        self.completion_items.clear();
        self.language_server_status = "Starting Vapor-managed Rust Analyzer...".to_owned();
        self.send_language_server_action(
            LanguageServerAction::Start {
                workspace_root: root.display().to_string(),
            },
            LanguageServerRequest::Start,
        );
    }

    fn stop_language_server(&mut self) {
        if self.service.is_none() {
            self.language_server_ready = false;
            return;
        }
        self.send_language_server_action(LanguageServerAction::Stop, LanguageServerRequest::Stop);
        self.language_server_ready = false;
        self.language_server_status = "Rust Analyzer is stopped".to_owned();
    }

    fn initialize_language_server(&mut self) {
        let Some(root) = self.workspace.as_ref() else {
            return;
        };
        let root_uri = file_uri(root);
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": root_uri.clone(),
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": {"relatedInformation": true},
                        "synchronization": {"didSave": true}
                    },
                    "workspace": {"workspaceFolders": true}
                },
                "workspaceFolders": [{"uri": root_uri, "name": root.file_name().unwrap_or_default().to_string_lossy()}]
            }
        });
        self.language_server_status = "Initializing Rust workspace...".to_owned();
        self.send_language_server_notification(initialize);
    }

    fn send_language_server_notification(&mut self, message: serde_json::Value) {
        let Some(client) = self.service.as_mut() else {
            self.language_server_status = "Vapor service is unavailable".to_owned();
            return;
        };
        if let Err(error) = client.send_language_server_message(message.to_string()) {
            self.language_server_ready = false;
            self.language_server_status = format!("Could not send Rust Analyzer message: {error}");
        }
    }

    fn request_lsp_at_cursor(&mut self, method: &str, request: LspRequest) {
        if !self.language_server_ready {
            self.status = "Rust Analyzer is not ready yet.".to_owned();
            return;
        }
        let Some(path) = self.selected_file.clone() else {
            return;
        };
        if path.extension().is_none_or(|extension| extension != "rs") {
            self.status = "This language feature is available for Rust source files.".to_owned();
            return;
        }
        let Some(document) = self.documents.get(&path) else {
            return;
        };
        let message_id = self.next_lsp_request_id;
        self.next_lsp_request_id = self.next_lsp_request_id.saturating_add(1);
        let mut message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "method": method,
            "params": {
                "textDocument": {"uri": self.document_uri(&path)},
                "position": lsp_position(&document.contents, self.cursor_offset)
            }
        });
        if matches!(request, LspRequest::References) {
            message["params"]["context"] = serde_json::json!({"includeDeclaration": true});
            self.references.clear();
        }
        self.lsp_requests.insert(
            message_id,
            PendingLspRequest {
                request,
                path: path.clone(),
            },
        );
        self.send_language_server_notification(message);
    }

    fn request_lsp_formatting(&mut self, path: &Path) {
        if !self.language_server_ready {
            self.status = "Rust Analyzer is not ready yet.".to_owned();
            return;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            self.status = "Formatting is available for Rust source files.".to_owned();
            return;
        }
        if !self.documents.contains_key(path) {
            return;
        }
        let message_id = self.next_lsp_request_id;
        self.next_lsp_request_id = self.next_lsp_request_id.saturating_add(1);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "method": "textDocument/formatting",
            "params": {
                "textDocument": {"uri": self.document_uri(path)},
                "options": {"tabSize": 4, "insertSpaces": true}
            }
        });
        self.lsp_requests.insert(
            message_id,
            PendingLspRequest {
                request: LspRequest::Formatting,
                path: path.to_path_buf(),
            },
        );
        self.send_language_server_notification(message);
    }

    fn apply_lsp_formatting(&mut self, path: &Path, result: &serde_json::Value) {
        if result.is_null() {
            self.status = "File is already formatted.".to_owned();
            return;
        }
        let Some(edits) = result.as_array() else {
            self.status = "Rust Analyzer returned invalid formatting edits.".to_owned();
            return;
        };
        let Some(document) = self.documents.get(path) else {
            self.status = "The formatted file is no longer open.".to_owned();
            return;
        };
        let formatted = match apply_text_edits(&document.contents, edits) {
            Ok(formatted) => formatted,
            Err(error) => {
                self.status = format!("Could not apply Rust Analyzer formatting: {error}");
                return;
            }
        };
        if formatted == document.contents {
            self.status = "File is already formatted.".to_owned();
            return;
        }
        let document = self.documents.get_mut(path).expect("open document checked");
        document.contents = formatted.clone();
        document.version += 1;
        let version = document.version;
        self.cursor_offset = self.cursor_offset.min(formatted.chars().count());
        self.cursor_position = cursor_position(&formatted, self.cursor_offset);
        self.did_change_document(path, formatted, version);
        self.status = format!("Formatted {} with Rust Analyzer.", path.display());
    }

    fn apply_completion(&mut self, context: &egui::Context, index: usize) {
        let Some(item) = self.completion_items.get(index).cloned() else {
            return;
        };
        let Some(path) = self.selected_file.clone() else {
            return;
        };
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        let (start, end) = identifier_byte_range(&document.contents, self.cursor_offset);
        document
            .contents
            .replace_range(start..end, &item.insert_text);
        document.version += 1;
        let contents = document.contents.clone();
        let version = document.version;
        let cursor_byte = start + item.insert_text.len();
        self.cursor_offset = contents[..cursor_byte].chars().count();
        self.completion_items.clear();
        self.hover = None;
        vapor_gui_core::code_editor::select_range(
            context,
            &path,
            &contents,
            cursor_byte..cursor_byte,
        );
        self.did_change_document(&path, contents, version);
    }

    fn navigate_to_definition(&mut self, result: &serde_json::Value) {
        let location = result
            .as_array()
            .and_then(|locations| locations.first())
            .unwrap_or(result);
        let Some(uri) = location
            .get("uri")
            .or_else(|| location.get("targetUri"))
            .and_then(serde_json::Value::as_str)
        else {
            self.status = "Rust Analyzer found no definition.".to_owned();
            return;
        };
        let range = location
            .get("targetSelectionRange")
            .or_else(|| location.get("range"));
        let Some(position) = range.and_then(|range| range.get("start")) else {
            return;
        };
        let Some(line) = position.get("line").and_then(serde_json::Value::as_u64) else {
            return;
        };
        let Some(character) = position
            .get("character")
            .and_then(serde_json::Value::as_u64)
        else {
            return;
        };
        let Some(absolute_path) = path_from_file_uri(uri) else {
            return;
        };
        let Some(root) = self.workspace.as_ref() else {
            return;
        };
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let canonical_path = absolute_path.canonicalize().unwrap_or(absolute_path);
        let Ok(path) = canonical_path.strip_prefix(&canonical_root) else {
            self.status = "Rust Analyzer returned a definition outside this workspace.".to_owned();
            return;
        };
        self.navigate_to_workspace_location(path, line as usize, character as usize);
    }

    fn navigate_to_workspace_location(&mut self, path: &Path, line: usize, character: usize) {
        let path = path.to_path_buf();
        self.pending_navigation = Some((path.clone(), line, character));
        if self.documents.contains_key(&path) {
            self.selected_file = Some(path);
        } else {
            self.open_file(path);
        }
    }

    fn did_open_document(&mut self, path: &Path) {
        if !self.language_server_ready || path.extension().is_none_or(|extension| extension != "rs")
        {
            return;
        }
        let (uri, text, version) = {
            let Some(document) = self.documents.get(path) else {
                return;
            };
            (
                self.document_uri(path),
                document.contents.clone(),
                document.version,
            )
        };
        self.send_language_server_notification(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": uri, "languageId": "rust", "version": version, "text": text}}
        }));
    }

    fn document_uri(&self, path: &Path) -> String {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace
                .as_ref()
                .map_or_else(|| path.to_path_buf(), |root| root.join(path))
        };
        file_uri(&path.canonicalize().unwrap_or(path))
    }

    fn did_change_document(&mut self, path: &Path, text: String, version: i32) {
        if !self.language_server_ready || path.extension().is_none_or(|extension| extension != "rs")
        {
            return;
        }
        let uri = self.document_uri(path);
        self.send_language_server_notification(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}
        }));
    }

    fn did_close_document(&mut self, path: &Path) {
        if !self.language_server_ready || path.extension().is_none_or(|extension| extension != "rs")
        {
            return;
        }
        let uri = self.document_uri(path);
        self.send_language_server_notification(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": uri}}
        }));
    }

    fn did_save_document(&mut self, path: &Path) {
        if !self.language_server_ready || path.extension().is_none_or(|extension| extension != "rs")
        {
            return;
        }
        let uri = self.document_uri(path);
        self.send_language_server_notification(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": {"textDocument": {"uri": uri}}
        }));
    }

    fn handle_language_server_message(&mut self, message: serde_json::Value) {
        let id = message.get("id").and_then(serde_json::Value::as_i64);
        if id == Some(1) {
            if let Some(error) = message.get("error") {
                self.language_server_ready = false;
                self.language_server_status = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Rust Analyzer failed to initialize")
                    .to_owned();
                return;
            }
            if message.get("result").is_some() {
                self.language_server_ready = true;
                self.language_server_status = "Rust Analyzer ready".to_owned();
                self.send_language_server_notification(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "initialized",
                    "params": {}
                }));
                let open = self.documents.keys().cloned().collect::<Vec<_>>();
                for path in open {
                    self.did_open_document(&path);
                }
                return;
            }
        }
        if let Some(id) = id
            && let Some(request) = self.lsp_requests.remove(&id)
        {
            if let Some(error) = message.get("error") {
                self.status = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Rust Analyzer request failed")
                    .to_owned();
                return;
            }
            let Some(result) = message.get("result") else {
                return;
            };
            match request.request {
                LspRequest::Definition => self.navigate_to_definition(result),
                LspRequest::References if self.selected_file.as_ref() == Some(&request.path) => {
                    self.references = self
                        .workspace
                        .as_deref()
                        .map_or_else(Vec::new, |root| reference_locations(result, root));
                }
                LspRequest::Hover if self.selected_file.as_ref() == Some(&request.path) => {
                    self.hover = hover_text(result);
                }
                LspRequest::Completion if self.selected_file.as_ref() == Some(&request.path) => {
                    self.completion_items = completion_items(result);
                }
                LspRequest::Formatting => self.apply_lsp_formatting(&request.path, result),
                LspRequest::References | LspRequest::Hover | LspRequest::Completion => {}
            }
            return;
        }
        let Some((path, diagnostics)) = parse_diagnostics(&message) else {
            return;
        };
        let Some(root) = self.workspace.as_ref() else {
            return;
        };
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let Ok(relative_path) = path.strip_prefix(canonical_root) else {
            return;
        };
        self.diagnostics
            .insert(relative_path.to_path_buf(), diagnostics);
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

    fn create_workspace_file(&mut self) {
        let path = self.new_file_path.trim();
        if path.is_empty() {
            self.status = "Enter a file name inside the project.".to_owned();
            return;
        }
        if self.documents.values().any(Document::is_dirty) {
            self.status = "Save your open files before creating a file.".to_owned();
            return;
        }
        self.send(
            ClientAction::Workspace(WorkspaceAction::CreateFile {
                path: path.to_owned(),
                contents: String::new(),
            }),
            PendingAction::CreateFile,
        );
        self.new_file_dialog = false;
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
                            self.console.history.push(ConsoleRecord::new(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
                                    .unwrap_or_default(),
                                ConsoleLevel::Error,
                                "vapor",
                                error.message,
                            ));
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
                            self.new_file_dialog = true;
                        }
                        self.pending = None;
                        continue;
                    }
                    if let PendingAction::OpenWorkspace(root) = pending.action.clone() {
                        self.workspace = Some(root.clone());
                        self.workspace_input = root.display().to_string();
                        self.files.clear();
                        self.file_tree = WorkspaceFileTree::default();
                        self.documents.clear();
                        self.selected_file = None;
                        self.start_language_server();
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
                    for line in output.text.lines().filter(|line| !line.trim().is_empty()) {
                        let source = if output.source.is_empty() { "command" } else { &output.source };
                        let mut record = ConsoleRecord::new(
                            output.timestamp_unix_ms,
                            output.level,
                            source,
                            line,
                        )
                        .target(output.action.clone())
                        .field("operation", output.id.clone())
                        .field("stream", format!("{:?}", output.stream).to_ascii_lowercase());
                        if let Some(process) = &output.process {
                            record = record.field("process", process.clone());
                        }
                        self.console.history.push(record);
                    }
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
            self.console.history.push(ConsoleRecord::new(
                timestamp,
                ConsoleLevel::Info,
                "ide",
                line,
            ));
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
                self.did_open_document(&path);
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
                self.new_file_path.clear();
                self.new_file_dialog = false;
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
        let mut create_file = false;
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
            ui.heading("Vapor IDE");
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
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.inspector_open, "Inspector");
                    ui.checkbox(&mut self.bottom_open, "Problems & Console");
                });
                ui.menu_button("Actions", |ui| {
                    if ui
                        .add_enabled(
                            self.workspace.is_some() && self.pending.is_none(),
                            egui::Button::new("New file..."),
                        )
                        .clicked()
                    {
                        create_file = true;
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
        if create_file {
            self.new_file_dialog = true;
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
                ui.weak(format!("{} files", self.file_tree.matching_file_count("")));
            });
        });
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.file_query).hint_text("Filter files | Ctrl/Cmd+P"),
        );
        if self.focus_file_query {
            search.request_focus();
            self.focus_file_query = false;
        }
        if !self.file_query.is_empty() && ui.small_button("Clear").clicked() {
            self.file_query.clear();
        }
        let filter = self.file_query.trim().to_lowercase();
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
                        },
                        &mut requested_file,
                    );
                }
            });
        if let Some(path) = requested_file {
            self.open_file(path);
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
            let offset = position_to_byte_offset(&document.contents, line, character);
            vapor_gui_core::code_editor::select_range(
                &context,
                &path,
                &document.contents,
                offset..offset,
            );
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
            let issue_count = self.diagnostics.values().map(Vec::len).sum::<usize>();
            ui.label(format!("{issue_count} workspace diagnostics"));
            if let Some(hover) = &self.hover {
                ui.add_space(8.0);
                ui.label("Hover");
                ui.monospace(hover);
            }
            if !self.references.is_empty() {
                ui.add_space(8.0);
                ui.label(format!("References | {}", self.references.len()));
                let references = self.references.clone();
                let mut selected_reference = None;
                egui::ScrollArea::vertical()
                    .id_salt("ide-references")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for location in references {
                            if ui
                                .selectable_label(
                                    false,
                                    format!(
                                        "{}:{}:{}",
                                        location.path.display(),
                                        location.line,
                                        location.character
                                    ),
                                )
                                .clicked()
                            {
                                selected_reference = Some(location);
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
        ui.horizontal(|ui| {
            let diagnostic_count = self.diagnostics.values().map(Vec::len).sum::<usize>();
            ui.strong(format!("Problems | {diagnostic_count}"));
            ui.weak(&self.language_server_status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear").clicked() {
                    self.console.history.clear();
                }
            });
        });
        ui.separator();
        let mut selected_diagnostic = None;
        egui::ScrollArea::vertical()
            .id_salt("ide-diagnostics")
            .max_height(150.0)
            .show(ui, |ui| {
                for (path, diagnostics) in &self.diagnostics {
                    for diagnostic in diagnostics {
                        let marker = match diagnostic.severity {
                            Some(1) => "Error",
                            Some(2) => "Warning",
                            Some(3) => "Info",
                            Some(4) => "Hint",
                            _ => "Diagnostic",
                        };
                        if ui
                            .selectable_label(
                                false,
                                format!(
                                    "{marker}  {}:{}:{}  {}",
                                    path.display(),
                                    diagnostic.line,
                                    diagnostic.character,
                                    diagnostic.message
                                ),
                            )
                            .clicked()
                        {
                            selected_diagnostic = Some(path.clone());
                        }
                    }
                }
            });
        if let Some(path) = selected_diagnostic {
            self.open_file(path);
        }
        ui.separator();
        if self.console.history.is_empty() {
            ui.weak("Managed commands and toolchain output appear here.");
        }
        if let Some(command) = self.console.show(ui, true) {
            self.submit_console_command(&command);
        }
    }

    fn submit_console_command(&mut self, line: &str) {
        use vapor_client_domain::{
            ConsoleCommand, ConsoleCommandOutcome, ConsoleCommandScope,
            console_command_catalog,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or_default();
        self.console.history.push(ConsoleRecord::new(now, ConsoleLevel::Input, "console", line));
        match ConsoleCommand::parse(line) {
            Ok(ConsoleCommand::Help) => {
                for command in console_command_catalog(ConsoleCommandScope::Workspace) {
                    self.console.history.push(
                        ConsoleRecord::new(now, ConsoleLevel::Info, "help", command.path)
                            .field("description", command.summary),
                    );
                }
            }
            Ok(ConsoleCommand::Status) => self.console.history.push(
                ConsoleRecord::new(now, ConsoleLevel::Info, "ide", self.status.clone()),
            ),
            Ok(command) => match command.outcome() {
                ConsoleCommandOutcome::Action(action) => {
                    if let Some(service) = self.service.as_mut() {
                        match service.send(action, None) {
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
                            now, ConsoleLevel::Error, "vapor", "Vapor service is disconnected",
                        ));
                    }
                }
                ConsoleCommandOutcome::Help | ConsoleCommandOutcome::Status => unreachable!(),
            },
            Err(error) => self.console.history.push(ConsoleRecord::new(
                now, ConsoleLevel::Error, "console", format!("{error:?}"),
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
            self.new_file_dialog = true;
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
        if self.new_file_dialog {
            egui::Window::new("Create a file")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label("Create an empty file inside this project.");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_file_path)
                            .hint_text("src/example.rs")
                            .desired_width(340.0),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.pending.is_none(), egui::Button::new("Create"))
                            .clicked()
                        {
                            self.create_workspace_file();
                        }
                        if ui.button("Cancel").clicked() {
                            self.new_file_dialog = false;
                        }
                    });
                });
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
}

fn draw_file_tree(
    ui: &mut egui::Ui,
    tree: &WorkspaceFileTree,
    parent: &Path,
    depth: usize,
    view: &FileTreeView<'_>,
    requested: &mut Option<PathBuf>,
) {
    for (name, entry) in tree.entries() {
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
                        format!("{name}{}", if dirty { " *" } else { "" }),
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

fn parse_diagnostics(message: &serde_json::Value) -> Option<(PathBuf, Vec<Diagnostic>)> {
    if message.get("method").and_then(serde_json::Value::as_str)
        != Some("textDocument/publishDiagnostics")
    {
        return None;
    }
    let params = message.get("params")?;
    let path = path_from_file_uri(params.get("uri")?.as_str()?)?;
    let diagnostics = params
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|diagnostic| {
            let start = diagnostic.get("range")?.get("start")?;
            Some(Diagnostic {
                line: start.get("line")?.as_u64()? as usize + 1,
                character: start.get("character")?.as_u64()? as usize + 1,
                severity: diagnostic
                    .get("severity")
                    .and_then(serde_json::Value::as_u64),
                message: diagnostic.get("message")?.as_str()?.to_owned(),
            })
        })
        .collect();
    Some((path, diagnostics))
}

fn reference_locations(result: &serde_json::Value, root: &Path) -> Vec<ReferenceLocation> {
    let Some(locations) = result.as_array() else {
        return Vec::new();
    };
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    locations
        .iter()
        .filter_map(|location| {
            let absolute_path = path_from_file_uri(location.get("uri")?.as_str()?)?;
            let canonical_path = absolute_path.canonicalize().unwrap_or(absolute_path);
            let path = canonical_path
                .strip_prefix(&canonical_root)
                .ok()?
                .to_path_buf();
            let start = location.get("range")?.get("start")?;
            Some(ReferenceLocation {
                path,
                line: start.get("line")?.as_u64()? as usize + 1,
                character: start.get("character")?.as_u64()? as usize + 1,
            })
        })
        .collect()
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

fn position_to_byte_offset(source: &str, line: usize, character: usize) -> usize {
    let mut byte_offset = 0;
    for (line_index, line_text) in source.split_inclusive('\n').enumerate() {
        if line_index == line {
            let line_text = line_text.strip_suffix('\n').unwrap_or(line_text);
            let line_text = line_text.strip_suffix('\r').unwrap_or(line_text);
            let mut utf16_offset = 0;
            for (offset, scalar) in line_text.char_indices() {
                if utf16_offset >= character {
                    return byte_offset + offset;
                }
                utf16_offset += scalar.len_utf16();
                if utf16_offset > character {
                    return byte_offset + offset;
                }
            }
            return byte_offset + line_text.len();
        }
        byte_offset += line_text.len();
    }
    source.len()
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
            );
            let end = position_to_byte_offset(
                source,
                end.get("line")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("an edit end has no line")? as usize,
                end.get("character")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("an edit end has no character")? as usize,
            );
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

#[cfg(test)]
mod tests {
    use super::{
        Diagnostic, Document, ReferenceLocation, apply_text_edits, completion_items,
        cursor_position, document_tab_label, file_uri, find_matches, hover_text,
        identifier_byte_range, lsp_position, nonempty_path, parse_diagnostics, path_from_file_uri,
        position_to_byte_offset, reference_locations,
    };
    use std::collections::BTreeMap;
    use std::path::Path;

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
    fn rust_analyzer_diagnostics_use_one_based_editor_locations() {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/src/main.rs",
                "diagnostics": [{
                    "range": {"start": {"line": 3, "character": 8}},
                    "severity": 1,
                    "message": "expected expression"
                }]
            }
        });
        assert_eq!(
            parse_diagnostics(&message),
            Some((
                Path::new("/tmp/src/main.rs").to_path_buf(),
                vec![Diagnostic {
                    line: 4,
                    character: 9,
                    severity: Some(1),
                    message: "expected expression".to_owned(),
                }]
            ))
        );
    }

    #[test]
    fn reference_results_are_workspace_relative_and_skip_external_paths() {
        let references = reference_locations(
            &serde_json::json!([
                {
                    "uri": "file:///workspace/src/lib.rs",
                    "range": {"start": {"line": 4, "character": 8}}
                },
                {
                    "uri": "file:///outside/lib.rs",
                    "range": {"start": {"line": 0, "character": 0}}
                }
            ]),
            Path::new("/workspace"),
        );
        assert_eq!(
            references,
            [ReferenceLocation {
                path: Path::new("src/lib.rs").to_path_buf(),
                line: 5,
                character: 9,
            }]
        );
    }

    #[test]
    fn lsp_positions_use_utf16_and_navigate_back_to_utf8_offsets() {
        let source = "aλ🦀\r\nnext";
        assert_eq!(
            lsp_position(source, 3),
            serde_json::json!({"line": 0, "character": 4})
        );
        assert_eq!(position_to_byte_offset(source, 0, 4), "aλ🦀".len());
        assert_eq!(position_to_byte_offset(source, 1, 2), "aλ🦀\r\nne".len());
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
            "a/mod.rs"
        );
        assert_eq!(
            document_tab_label(Path::new("src/b/mod.rs"), &open),
            "b/mod.rs"
        );
        assert_eq!(
            document_tab_label(Path::new("src/main.rs"), &open),
            "main.rs"
        );
    }
}
