use super::*;

impl IdeApp {
    pub(super) fn connect(&mut self) {
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

    pub(super) fn poll_connection(&mut self) {
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

    pub(super) fn open_workspace(&mut self, root: PathBuf) {
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

    pub(super) fn send(&mut self, action: ClientAction, pending: PendingAction) {
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

    pub(super) fn request_file_list(&mut self) {
        self.send(
            ClientAction::Workspace(WorkspaceAction::ListFiles {
                path: ".".to_owned(),
            }),
            PendingAction::ListFiles,
        );
    }

    pub(super) fn open_file(&mut self, path: PathBuf) {
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

    pub(super) fn save_file(&mut self) {
        let Some(path) = self.selected_file.clone() else {
            return;
        };
        self.save_path(path);
    }

    pub(super) fn save_path(&mut self, path: PathBuf) {
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

    pub(super) fn save_all(&mut self) {
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

    pub(super) fn save_next(&mut self) {
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

    pub(super) fn close_document(&mut self, path: &Path) {
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

    pub(super) fn run_workspace_tool(&mut self, operation: WorkspaceToolOperation) {
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

    pub(super) fn run_default_configuration(&mut self) {
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

    pub(super) fn poll_service(&mut self) {
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

    pub(super) fn append_output(&mut self, output: &str) {
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

    pub(super) fn apply_file_result(&mut self, result: WorkspaceFileResult) {
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

    pub(super) fn pending_matches(&self, operation_id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.operation_id.as_deref() == Some(operation_id))
    }

    pub(super) fn pending_save_matches(&self, operation_id: &str, path: &str) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            pending.operation_id.as_deref() == Some(operation_id)
                && matches!(&pending.action, PendingAction::SaveFile { path: expected, .. } if expected.as_path() == Path::new(path))
        })
    }
}

pub(super) fn connect_service(workspace: Option<PathBuf>) -> Result<Client, vapor_client::ClientError> {
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
