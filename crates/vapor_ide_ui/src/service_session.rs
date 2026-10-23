use super::*;

impl IdeApp {
    pub(super) fn connect(&mut self) {
        if self.service.connecting.is_some()
            || self.service.client.is_some()
            || Instant::now() < self.service.next_connect_attempt
        {
            return;
        }
        self.service.next_connect_attempt = Instant::now() + self.service.reconnect_delay;
        self.service.reconnect_delay =
            (self.service.reconnect_delay * 2).min(Duration::from_secs(30));
        let workspace = nonempty_path(&self.workspace.input);
        let (sender, receiver) = mpsc::channel();
        self.service.connecting = Some(receiver);
        self.workspace.status = "Connecting to the shared Vapor service...".to_owned();
        let result = thread::Builder::new()
            .name("vapor-ide-service-connect".to_owned())
            .spawn(move || {
                let result = connect_service(workspace).map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        if let Err(error) = result {
            self.service.connecting = None;
            self.service.error = Some(error.to_string());
            self.workspace.status = "Vapor could not be reached.".to_owned();
        }
    }

    pub(super) fn poll_connection(&mut self) {
        let result = self.service.connecting.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(Ok(client))) => {
                self.service.connecting = None;
                self.service.error = None;
                self.service.reconnect_delay = Duration::from_secs(1);
                self.service.client = Some(client);
                self.workspace.status = "Connected to Vapor.".to_owned();
                if let Some(root) = nonempty_path(&self.workspace.input) {
                    self.open_workspace(root);
                }
            }
            Some(Ok(Err(error))) => {
                self.service.connecting = None;
                self.service.error = Some(error);
                self.workspace.status = "Vapor could not be reached.".to_owned();
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.service.connecting = None;
                self.service.error = Some("Vapor could not finish starting.".to_owned());
                self.workspace.status = "Vapor could not be reached.".to_owned();
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    pub(super) fn open_workspace(&mut self, root: PathBuf) {
        if self.editor.documents.values().any(Document::is_dirty) {
            self.workspace.status = "Save your open files before switching projects.".to_owned();
            return;
        }
        if !root.is_dir() {
            self.workspace.status = format!("That folder does not exist: {}", root.display());
            return;
        }
        self.stop_language_server();
        self.semantic.problems.clear();
        self.language_server.workspace_symbols.clear();
        self.language_server.workspace_symbol_query = None;
        self.language_server.code_actions.clear();
        self.language_server.code_action_request = None;
        self.send(
            ClientAction::Workspace(WorkspaceAction::Open {
                root: root.display().to_string(),
            }),
            PendingAction::OpenWorkspace(root),
        );
    }

    pub(super) fn send(&mut self, action: ClientAction, pending: PendingAction) {
        let Some(client) = self.service.client.as_mut() else {
            self.workspace.status = "Connect to Vapor before starting an operation.".to_owned();
            return;
        };
        match client.send(action, None) {
            Ok(request_id) => {
                self.workspace.pending = Some(PendingRequest {
                    request_id: request_id.0,
                    action: pending,
                    operation_id: None,
                });
                self.workspace.status = "Working...".to_owned();
            }
            Err(error) => {
                self.service.client = None;
                self.service.error = Some(error.to_string());
                self.workspace.status = "The Vapor connection was lost.".to_owned();
            }
        }
    }

    pub(super) fn request_file_list(&mut self) {
        self.send(
            ClientAction::Workspace(WorkspaceAction::ListFiles {
                path: ".".to_owned(),
            }),
            PendingAction::ListFiles,
        );
    }

    pub(super) fn open_file(&mut self, path: PathBuf) {
        if self.editor.documents.contains_key(&path) {
            self.editor.selected_file = Some(path);
            return;
        }
        self.send(
            ClientAction::Workspace(WorkspaceAction::OpenFile {
                path: path.display().to_string(),
            }),
            PendingAction::OpenFile,
        );
    }

    pub(super) fn save_file(&mut self) {
        let Some(path) = self.editor.selected_file.clone() else {
            return;
        };
        self.save_path(path);
    }

    pub(super) fn save_path(&mut self, path: PathBuf) {
        let Some(document) = self.editor.documents.get(&path).cloned() else {
            return;
        };
        if !document.is_dirty() {
            self.workspace.status = "This file has no unsaved changes.".to_owned();
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

    pub(super) fn save_all(&mut self) {
        if self.workspace.pending.is_some() {
            return;
        }
        self.editor.save_queue = self
            .editor
            .documents
            .iter()
            .filter(|(_, document)| document.is_dirty())
            .map(|(path, _)| path.clone())
            .collect();
        if self.editor.save_queue.is_empty() {
            self.workspace.status = "All open files are saved.".to_owned();
            return;
        }
        self.save_next();
    }

    pub(super) fn save_next(&mut self) {
        if self.workspace.pending.is_some() {
            return;
        }
        while let Some(path) = self.editor.save_queue.pop_front() {
            if !self
                .editor
                .documents
                .get(&path)
                .is_some_and(Document::is_dirty)
            {
                continue;
            }
            self.save_path(path);
            if self.workspace.pending.is_some() {
                return;
            }
            self.editor.save_queue.clear();
            return;
        }
        self.workspace.status = if self.editor.documents.values().any(Document::is_dirty) {
            "Some files changed while saving. Save again to keep those edits.".to_owned()
        } else {
            "All open files are saved.".to_owned()
        };
    }

    pub(super) fn close_document(&mut self, path: &Path) {
        if self
            .editor
            .documents
            .get(path)
            .is_some_and(Document::is_dirty)
        {
            self.workspace.status = "Save this file before closing it.".to_owned();
            return;
        }
        self.did_close_document(path);
        self.semantic.index.invalidate_document(path);
        if self
            .semantic
            .selected
            .as_ref()
            .is_some_and(|selected| selected.id.document == path)
        {
            self.semantic.selected = None;
        }
        self.editor.documents.remove(path);
        if self.editor.selected_file.as_deref() == Some(path) {
            self.editor.selected_file = self.editor.documents.keys().next().cloned();
        }
    }

    pub(super) fn run_workspace_tool(&mut self, operation: WorkspaceToolOperation) {
        if self.workspace.root.is_none() {
            self.workspace.status = "Open a project before running a workspace action.".to_owned();
            return;
        }
        if self.editor.documents.values().any(Document::is_dirty) {
            self.workspace.status =
                "Save or discard your edits before running a workspace action.".to_owned();
            return;
        }
        self.interface.console.history.clear();
        self.send(
            ClientAction::Workspace(WorkspaceAction::RunTool {
                operation,
                project_selection: WorkspaceProjectSelection::All,
                target_triples: Vec::new(),
            }),
            PendingAction::Tool,
        );
    }

    pub(super) fn run_default_configuration(&mut self) {
        let Some(root) = self.workspace.root.as_ref() else {
            self.workspace.status = "Open a project before running it.".to_owned();
            return;
        };
        if self.editor.documents.values().any(Document::is_dirty) {
            self.workspace.status =
                "Save or discard your edits before running the project.".to_owned();
            return;
        }
        self.interface.console.history.clear();
        self.send(
            ClientAction::Workspace(WorkspaceAction::RunConfiguration {
                workspace_root: root.display().to_string(),
                configuration: "default".to_owned(),
                profiling: false,
            }),
            PendingAction::Tool,
        );
    }

    pub(super) fn poll_service(&mut self) {
        while let Some(client) = self.service.client.as_mut() {
            let frame = match client.try_recv() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    self.service.client = None;
                    self.service.error = Some(error.to_string());
                    self.workspace.status = "The Vapor connection was lost.".to_owned();
                    break;
                }
            };
            match frame {
                ProtocolFrame::Response(response) => {
                    if self
                        .interface
                        .console_requests
                        .remove(&response.request_id.0)
                    {
                        if let Some(error) = response.error {
                            self.interface.console.history.push(
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
                        self.language_server.requests.remove(&response.request_id.0)
                    {
                        if let Some(error) = response.error {
                            self.language_server.ready = false;
                            self.language_server.status = error.message.clone();
                            self.append_output(&format!("Rust Analyzer: {}", error.message));
                            continue;
                        }
                        match language_server_request {
                            LanguageServerRequest::Start => self.initialize_language_server(),
                            LanguageServerRequest::Stop => {}
                        }
                        continue;
                    }
                    let Some(pending) = self.workspace.pending.as_mut() else {
                        continue;
                    };
                    if response.request_id.0 != pending.request_id {
                        continue;
                    }
                    if let Some(error) = response.error {
                        self.workspace.status = error.message;
                        if matches!(pending.action, PendingAction::SaveFile { .. }) {
                            self.editor.save_queue.clear();
                        }
                        if matches!(pending.action, PendingAction::CreateFile) {
                            self.interface.authoring.new_file_dialog = true;
                        }
                        self.workspace.pending = None;
                        continue;
                    }
                    if let PendingAction::OpenWorkspace(root) = pending.action.clone() {
                        self.workspace.root = Some(root.clone());
                        self.workspace.input = root.display().to_string();
                        self.workspace.files.clear();
                        self.workspace.file_tree = WorkspaceFileTree::default();
                        self.workspace.rust_crate_roots.clear();
                        self.editor.documents.clear();
                        self.semantic.index = SemanticWorkspaceIndex::default();
                        self.semantic.selected = None;
                        self.editor.selected_file = None;
                        self.start_language_server();
                        self.workspace.status = format!("Opened {}", root.display());
                        self.workspace.pending = None;
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
                        self.interface.console.history.push(record);
                    }
                    if let (Some(root), Some(diagnostic)) =
                        (self.workspace.root.as_deref(), output.diagnostic)
                    {
                        let mut diagnostic = diagnostic;
                        diagnostic.context.workspace = Some(root.to_string_lossy().into_owned());
                        let path = diagnostic
                            .primary
                            .as_ref()
                            .or_else(|| diagnostic.related.first())
                            .and_then(|span| {
                                workspace_diagnostic_path(root, &span.file, &self.workspace.files)
                            });
                        self.semantic.problems.push_build(
                            &output.id,
                            ProblemRow {
                                path,
                                action: Some(output.action.clone()),
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
                        self.semantic.problems.begin_build(&event.id);
                    }
                    if let Some(pending) = self.workspace.pending.as_mut()
                        && pending.operation_id.as_deref() == Some(event.id.as_str())
                    {
                        match event.status {
                            OperationStatus::Succeeded => {
                                if let Some(message) = event.message {
                                    self.workspace.status = message;
                                }
                                if matches!(pending.action, PendingAction::Tool) {
                                    self.workspace.pending = None;
                                }
                            }
                            OperationStatus::Failed { message } => {
                                self.workspace.status = message;
                                if matches!(pending.action, PendingAction::SaveFile { .. }) {
                                    self.editor.save_queue.clear();
                                }
                                self.workspace.pending = None;
                            }
                            OperationStatus::Cancelled => {
                                self.workspace.status = "Operation cancelled.".to_owned();
                                if matches!(pending.action, PendingAction::SaveFile { .. }) {
                                    self.editor.save_queue.clear();
                                }
                                self.workspace.pending = None;
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
                    self.language_server.ready =
                        status == vapor_client_protocol::LanguageServerStatus::Ready;
                    self.language_server.status = message.unwrap_or_else(|| match status {
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
                    self.workspace.status = error.message;
                }
                _ => {}
            }
        }
    }

    pub(super) fn append_output(&mut self, output: &str) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or_default();
        for line in vapor_gui_core::plain_terminal_text(output)
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            self.interface.console.history.push(
                ConsoleRecord::new(timestamp, ConsoleLevel::Info, "ide", line)
                    .field("origin", "ide"),
            );
        }
    }

    pub(super) fn apply_file_result(&mut self, result: WorkspaceFileResult) {
        match result {
            WorkspaceFileResult::Listed {
                operation_id,
                files,
            } if self.pending_matches(&operation_id) => {
                self.workspace.files = files.into_iter().map(PathBuf::from).collect();
                self.workspace.files.sort();
                self.workspace.file_tree = WorkspaceFileTree::new(&self.workspace.files);
                self.workspace.rust_crate_roots = rust_crate_roots(&self.workspace.files);
                self.workspace.status =
                    format!("{} files in workspace.", self.workspace.files.len());
                self.workspace.pending = None;
            }
            WorkspaceFileResult::Opened {
                operation_id,
                path,
                contents,
                fingerprint,
            } if self.pending_matches(&operation_id) => {
                let path = PathBuf::from(path);
                self.editor.selected_file = Some(path.clone());
                self.editor.documents.insert(
                    path.clone(),
                    Document {
                        saved_contents: contents.clone(),
                        contents,
                        fingerprint,
                        version: 1,
                    },
                );
                if let Some((version, contents)) = self
                    .editor
                    .documents
                    .get(&path)
                    .map(|document| (document.version as u64, document.contents.clone()))
                {
                    self.index_local_semantic_document(&path, version, &contents);
                }
                self.did_open_document(&path);
                self.request_document_symbols(&path);
                self.workspace.status = "File opened.".to_owned();
                self.workspace.pending = None;
            }
            WorkspaceFileResult::Saved {
                operation_id,
                path,
                fingerprint,
            } if self.pending_save_matches(&operation_id, &path) => {
                let path = PathBuf::from(path);
                let saved_contents =
                    self.workspace
                        .pending
                        .as_ref()
                        .and_then(|pending| match &pending.action {
                            PendingAction::SaveFile { contents, .. } => Some(contents.clone()),
                            _ => None,
                        });
                if let (Some(document), Some(saved_contents)) =
                    (self.editor.documents.get_mut(&path), saved_contents)
                {
                    document.record_saved_contents(saved_contents, fingerprint);
                    if document.is_dirty() && !self.editor.save_queue.contains(&path) {
                        self.editor.save_queue.push_front(path.clone());
                    }
                }
                self.did_save_document(&path);
                self.workspace.status = "Changes saved.".to_owned();
                self.workspace.pending = None;
                self.save_next();
            }
            WorkspaceFileResult::Created { operation_id, path }
                if self.pending_matches(&operation_id) =>
            {
                let path = PathBuf::from(path);
                if !self.workspace.files.contains(&path) {
                    self.workspace.files.push(path.clone());
                    self.workspace.files.sort();
                    self.workspace.file_tree = WorkspaceFileTree::new(&self.workspace.files);
                }
                self.workspace.pending = None;
                self.interface.authoring.new_file_path.clear();
                self.interface.authoring.new_file_dialog = false;
                self.open_file(path);
            }
            _ => {}
        }
    }

    pub(super) fn pending_matches(&self, operation_id: &str) -> bool {
        self.workspace
            .pending
            .as_ref()
            .is_some_and(|pending| pending.operation_id.as_deref() == Some(operation_id))
    }

    pub(super) fn pending_save_matches(&self, operation_id: &str, path: &str) -> bool {
        self.workspace.pending.as_ref().is_some_and(|pending| {
            pending.operation_id.as_deref() == Some(operation_id)
                && matches!(&pending.action, PendingAction::SaveFile { path: expected, .. } if expected.as_path() == Path::new(path))
        })
    }
}

pub(super) fn connect_service(
    workspace: Option<PathBuf>,
) -> Result<Client, vapor_client::ClientError> {
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
