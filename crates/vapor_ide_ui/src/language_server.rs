use super::*;

#[derive(Clone, Copy)]
pub(super) enum LanguageServerRequest {
    Start,
    Stop,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LspRequest {
    Definition,
    References,
    Rename,
    Hover,
    Completion,
    Formatting,
    DocumentSymbols,
    WorkspaceSymbols,
}

pub(super) struct PendingLspRequest {
    pub(super) request: LspRequest,
    pub(super) path: PathBuf,
    pub(super) document_version: i32,
    pub(super) semantic_target: Option<SemanticObjectRef>,
    pub(super) cursor_offset: Option<usize>,
    pub(super) workspace_versions: Option<BTreeMap<PathBuf, i32>>,
    pub(super) query: Option<String>,
}

pub(super) fn pending_lsp_requests_for_cancellation(
    requests: &BTreeMap<i64, PendingLspRequest>,
    request: LspRequest,
    path: Option<&Path>,
) -> Vec<i64> {
    requests
        .iter()
        .filter_map(|(id, pending)| {
            (pending.request == request
                && path.is_none_or(|path| pending.path == path))
                .then_some(*id)
        })
        .collect()
}

pub(super) fn workspace_versions_match(
    versions: &BTreeMap<PathBuf, i32>,
    documents: &BTreeMap<PathBuf, Document>,
) -> bool {
    versions.len() == documents.len()
        && versions.iter().all(|(path, version)| {
            documents
                .get(path)
                .is_some_and(|document| document.version == *version)
        })
}


impl IdeApp {
    pub(super) fn send_language_server_action(
        &mut self,
        action: LanguageServerAction,
        request: LanguageServerRequest,
    ) {
        let Some(client) = self.service.client.as_mut() else {
            self.language_server.status = "Vapor service is unavailable".to_owned();
            return;
        };
        match client.send(ClientAction::LanguageServer(action), None) {
            Ok(request_id) => {
                self.language_server.requests.insert(request_id.0, request);
            }
            Err(error) => {
                self.language_server.ready = false;
                self.language_server.status = format!("Rust Analyzer request failed: {error}");
            }
        }
    }

    pub(super) fn start_language_server(&mut self) {
        let Some(root) = &self.workspace.root else {
            return;
        };
        self.language_server.ready = false;
        self.language_server.next_request_id = 2;
        self.language_server.pending_requests.clear();
        self.language_server.completion_items.clear();
        self.language_server.workspace_symbols.clear();
        self.language_server.workspace_symbol_query = None;
        self.language_server.status = "Starting Vapor-managed Rust Analyzer...".to_owned();
        self.send_language_server_action(
            LanguageServerAction::Start {
                workspace_root: root.display().to_string(),
            },
            LanguageServerRequest::Start,
        );
    }

    pub(super) fn stop_language_server(&mut self) {
        if self.service.client.is_none() {
            self.language_server.ready = false;
            return;
        }
        self.send_language_server_action(LanguageServerAction::Stop, LanguageServerRequest::Stop);
        self.language_server.ready = false;
        self.language_server.status = "Rust Analyzer is stopped".to_owned();
    }

    pub(super) fn initialize_language_server(&mut self) {
        let Some(root) = self.workspace.root.as_ref() else {
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
                        "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                        "synchronization": {"didSave": true}
                    },
                    "workspace": {"workspaceFolders": true}
                },
                "initializationOptions": rust_analyzer_initialization_options(),
                "workspaceFolders": [{"uri": root_uri, "name": root.file_name().unwrap_or_default().to_string_lossy()}]
            }
        });
        self.language_server.status = "Initializing Rust workspace...".to_owned();
        self.send_language_server_notification(initialize);
    }

    pub(super) fn send_language_server_notification(&mut self, message: serde_json::Value) {
        let Some(client) = self.service.client.as_mut() else {
            self.language_server.status = "Vapor service is unavailable".to_owned();
            return;
        };
        if let Err(error) = client.send_language_server_message(message.to_string()) {
            self.language_server.ready = false;
            self.language_server.status = format!("Could not send Rust Analyzer message: {error}");
        }
    }

    pub(super) fn request_lsp_at_cursor(&mut self, method: &str, request: LspRequest) {
        let Some(path) = self.editor.selected_file.clone() else {
            return;
        };
        let Some(document) = self.editor.documents.get(&path) else {
            return;
        };
        let position = lsp_position(&document.contents, self.editor.cursor_offset);
        self.request_lsp_at_position(&path, position, method, request, None);
    }

    pub(super) fn request_lsp_at_position(
        &mut self,
        path: &Path,
        position: serde_json::Value,
        method: &str,
        request: LspRequest,
        semantic_target: Option<SemanticObjectRef>,
    ) {
        if !self.language_server.ready {
            self.workspace.status = "Rust Analyzer is not ready yet.".to_owned();
            return;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            self.workspace.status = "This language feature is available for Rust source files.".to_owned();
            return;
        }
        let Some(document) = self.editor.documents.get(path) else {
            self.workspace.status = "Open the source file before requesting Rust Analyzer references.".to_owned();
            return;
        };
        let document_version = document.version;
        let uri = self.document_uri(path);
        let workspace_versions = matches!(request, LspRequest::References).then(|| {
            self.editor.documents
                .iter()
                .map(|(path, document)| (path.clone(), document.version))
                .collect()
        });
        let message_id = self.language_server.next_request_id;
        self.language_server.next_request_id = self.language_server.next_request_id.saturating_add(1);
        let mut message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "method": method,
            "params": {
                "textDocument": {"uri": uri},
                "position": position
            }
        });
        if matches!(request, LspRequest::References) {
            message["params"]["context"] = serde_json::json!({"includeDeclaration": true});
            self.language_server.references.clear();
            self.language_server.references_omitted = 0;
        }
        self.language_server.pending_requests.insert(
            message_id,
            PendingLspRequest {
                request,
                path: path.to_path_buf(),
                document_version,
                cursor_offset: (matches!(request, LspRequest::References) && semantic_target.is_none())
                    .then_some(self.editor.cursor_offset),
                semantic_target,
                workspace_versions,
                query: None,
            },
        );
        self.send_language_server_notification(message);
    }

    pub(super) fn request_document_symbols(&mut self, path: &Path) {
        if !self.language_server.ready || path.extension().is_none_or(|extension| extension != "rs") {
            return;
        }
        let Some(document) = self.editor.documents.get(path) else {
            return;
        };
        let version = document.version;
        for id in pending_lsp_requests_for_cancellation(
            &self.language_server.pending_requests,
            LspRequest::DocumentSymbols,
            Some(path),
        ) {
            self.language_server.pending_requests.remove(&id);
            self.send_language_server_notification(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": id}
            }));
        }
        let message_id = self.language_server.next_request_id;
        self.language_server.next_request_id = self.language_server.next_request_id.saturating_add(1);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "method": "textDocument/documentSymbol",
            "params": {"textDocument": {"uri": self.document_uri(path)}}
        });
        self.language_server.pending_requests.insert(message_id, PendingLspRequest {
            request: LspRequest::DocumentSymbols,
            path: path.to_path_buf(),
            document_version: version,
            semantic_target: None,
            cursor_offset: None,
            workspace_versions: None,
            query: None,
        });
        self.send_language_server_notification(message);
    }

    pub(super) fn request_workspace_symbols(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            self.language_server.workspace_symbol_query = None;
            self.language_server.workspace_symbols.clear();
            return;
        }
        if !self.language_server.ready || self.language_server.workspace_symbol_query.as_deref() == Some(query) {
            return;
        }
        let Some(root) = self.workspace.root.clone() else {
            return;
        };

        let cancelled = pending_lsp_requests_for_cancellation(
            &self.language_server.pending_requests,
            LspRequest::WorkspaceSymbols,
            None,
        );
        for id in cancelled {
            self.language_server.pending_requests.remove(&id);
            self.send_language_server_notification(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": id}
            }));
        }

        let message_id = self.language_server.next_request_id;
        self.language_server.next_request_id = self.language_server.next_request_id.saturating_add(1);
        let workspace_versions = self.editor.documents
            .iter()
            .map(|(path, document)| (path.clone(), document.version))
            .collect();
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "method": "workspace/symbol",
            "params": {"query": query}
        });
        self.language_server.pending_requests.insert(message_id, PendingLspRequest {
            request: LspRequest::WorkspaceSymbols,
            path: root,
            document_version: 0,
            semantic_target: None,
            cursor_offset: None,
            workspace_versions: Some(workspace_versions),
            query: Some(query.to_owned()),
        });
        self.language_server.workspace_symbols.clear();
        self.language_server.workspace_symbol_query = Some(query.to_owned());
        self.workspace.status = format!("Searching Rust Analyzer for `{query}`…");
        self.send_language_server_notification(message);
    }

    pub(super) fn request_lsp_formatting(&mut self, path: &Path) {
        if !self.language_server.ready {
            self.workspace.status = "Rust Analyzer is not ready yet.".to_owned();
            return;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            self.workspace.status = "Formatting is available for Rust source files.".to_owned();
            return;
        }
        if !self.editor.documents.contains_key(path) {
            return;
        }
        let message_id = self.language_server.next_request_id;
        self.language_server.next_request_id = self.language_server.next_request_id.saturating_add(1);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "method": "textDocument/formatting",
            "params": {
                "textDocument": {"uri": self.document_uri(path)},
                "options": {"tabSize": 4, "insertSpaces": true}
            }
        });
        self.language_server.pending_requests.insert(
            message_id,
            PendingLspRequest {
                request: LspRequest::Formatting,
                path: path.to_path_buf(),
                document_version: self.editor.documents.get(path).map_or(0, |document| document.version),
                semantic_target: None,
                cursor_offset: None,
                workspace_versions: None,
                query: None,
            },
        );
        self.send_language_server_notification(message);
    }

    pub(super) fn request_lsp_rename(&mut self, target: SemanticObjectRef, new_name: &str) {
        let Some(object) = self.semantic.index.resolve(&target) else {
            self.workspace.status = "That semantic selection is out of date; select it again.".to_owned();
            return;
        };
        if !self.language_server.ready {
            self.workspace.status = "Rust Analyzer is not ready yet.".to_owned();
            return;
        }
        if object.id.domain != "rust" || object.id.document.extension().is_none_or(|ext| ext != "rs") {
            self.workspace.status = "Rename is currently available for Rust declarations.".to_owned();
            return;
        }
        let new_name = new_name.trim();
        if new_name == object.name || syn::parse_str::<syn::Ident>(new_name).is_err() {
            self.workspace.status = "Enter a valid, different Rust identifier.".to_owned();
            return;
        }
        let path = object.id.document.clone();
        let Some(document) = self.editor.documents.get(&path) else {
            self.workspace.status = "Open the declaration file before renaming it.".to_owned();
            return;
        };
        let document_version = document.version;
        let position = serde_json::json!({
            "line": object.selection_range.start.line,
            "character": object.selection_range.start.character
        });
        let workspace_versions = self.editor.documents
            .iter()
            .map(|(path, document)| (path.clone(), document.version))
            .collect();
        let message_id = self.language_server.next_request_id;
        self.language_server.next_request_id = self.language_server.next_request_id.saturating_add(1);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "method": "textDocument/rename",
            "params": {
                "textDocument": {"uri": self.document_uri(&path)},
                "position": position,
                "newName": new_name
            }
        });
        self.language_server.pending_requests.insert(message_id, PendingLspRequest {
            request: LspRequest::Rename,
            path,
            document_version,
            semantic_target: Some(target),
            cursor_offset: None,
            workspace_versions: Some(workspace_versions),
            query: None,
        });
        self.workspace.status = format!("Preparing Rust Analyzer rename to `{new_name}`…");
        self.send_language_server_notification(message);
    }

    pub(super) fn apply_lsp_formatting(&mut self, path: &Path, result: &serde_json::Value) {
        if result.is_null() {
            self.workspace.status = "File is already formatted.".to_owned();
            return;
        }
        let Some(edits) = result.as_array() else {
            self.workspace.status = "Rust Analyzer returned invalid formatting edits.".to_owned();
            return;
        };
        let Some(document) = self.editor.documents.get(path) else {
            self.workspace.status = "The formatted file is no longer open.".to_owned();
            return;
        };
        let formatted = match apply_text_edits(&document.contents, edits) {
            Ok(formatted) => formatted,
            Err(error) => {
                self.workspace.status = format!("Could not apply Rust Analyzer formatting: {error}");
                return;
            }
        };
        if formatted == document.contents {
            self.workspace.status = "File is already formatted.".to_owned();
            return;
        }
        self.semantic.edit_preview = Some(VersionedEditPreview {
            path: path.to_path_buf(),
            document_version: document.version,
            original_contents: document.contents.clone(),
            proposed_contents: formatted,
        });
        self.workspace.status = format!("Review the Rust Analyzer formatting preview for {}.", path.display());
    }

    pub(super) fn prepare_workspace_edit(&mut self, request: &PendingLspRequest, result: &serde_json::Value) {
        let Some(root) = self.workspace.root.as_deref() else {
            self.workspace.status = "Open a workspace before applying a Rust refactor.".to_owned();
            return;
        };
        let Some(versions) = request.workspace_versions.as_ref() else {
            self.workspace.status = "The rename request has no workspace revision snapshot.".to_owned();
            return;
        };
        if !workspace_versions_match(versions, &self.editor.documents) {
            self.workspace.status = "Open files changed while rename was being prepared; request a fresh preview.".to_owned();
            return;
        }
        let mut grouped = BTreeMap::<String, (Option<i32>, Vec<serde_json::Value>)>::new();
        if let Some(changes) = result.get("changes").and_then(serde_json::Value::as_object) {
            for (uri, edits) in changes {
                let Some(edits) = edits.as_array() else {
                    self.workspace.status = "Rust Analyzer returned an invalid workspace edit.".to_owned();
                    return;
                };
                grouped.entry(uri.clone()).or_default().1.extend(edits.iter().cloned());
            }
        }
        if let Some(changes) = result.get("documentChanges").and_then(serde_json::Value::as_array) {
            for change in changes {
                let (Some(uri), Some(edits)) = (
                    change.pointer("/textDocument/uri").and_then(serde_json::Value::as_str),
                    change.get("edits").and_then(serde_json::Value::as_array),
                ) else {
                    self.workspace.status = "Rust Analyzer returned a file operation or malformed edit; it was not applied.".to_owned();
                    return;
                };
                let version = change.pointer("/textDocument/version")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|version| i32::try_from(version).ok());
                let entry = grouped.entry(uri.to_owned()).or_default();
                if entry.0.is_some() && version.is_some() && entry.0 != version {
                    self.workspace.status = "Rust Analyzer returned conflicting document versions.".to_owned();
                    return;
                }
                entry.0 = version.or(entry.0);
                entry.1.extend(edits.iter().cloned());
            }
        }
        if grouped.is_empty() {
            self.workspace.status = "Rust Analyzer found no references to rename.".to_owned();
            return;
        }
        if grouped.len() > 64 {
            self.workspace.status = "This rename touches more than 64 files; narrow the operation before previewing.".to_owned();
            return;
        }
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut previews = Vec::with_capacity(grouped.len());
        let mut addressed_paths = HashSet::new();
        let mut total_bytes = 0usize;
        for (uri, (lsp_version, edits)) in grouped {
            let Some(absolute_path) = path_from_file_uri(&uri) else {
                self.workspace.status = "Rust Analyzer returned a non-local file URI; rename was not applied.".to_owned();
                return;
            };
            let canonical_path = absolute_path.canonicalize().unwrap_or(absolute_path);
            if !canonical_path.starts_with(&canonical_root) {
                self.workspace.status = "Rust Analyzer returned an edit outside this workspace; rename was not applied.".to_owned();
                return;
            }
            if !addressed_paths.insert(canonical_path.clone()) {
                self.workspace.status = "Rust Analyzer returned multiple edits for the same canonical file; rename was not applied.".to_owned();
                return;
            }
            let Some((path, document)) = self.editor.documents.iter().find(|(path, _)| {
                let candidate = if path.is_absolute() { (*path).clone() } else { canonical_root.join(path) };
                candidate.canonicalize().unwrap_or(candidate) == canonical_path
            }) else {
                self.workspace.status = format!("Open {} before previewing this rename; closed files are never changed implicitly.", canonical_path.display());
                return;
            };
            let Some(snapshot_version) = versions.get(path) else {
                self.workspace.status = format!("{} was opened after the rename began; request a fresh preview.", path.display());
                return;
            };
            if document.version != *snapshot_version
                || lsp_version.is_some_and(|version| version != document.version)
                || (path == &request.path && document.version != request.document_version)
            {
                self.workspace.status = format!("{} changed while rename was being prepared; request a fresh preview.", path.display());
                return;
            }
            let proposed_contents = match apply_text_edits(&document.contents, &edits) {
                Ok(contents) => contents,
                Err(error) => {
                    self.workspace.status = format!("Rust Analyzer returned unsafe edits for {}: {error}", path.display());
                    return;
                }
            };
            total_bytes = total_bytes.saturating_add(proposed_contents.len());
            if total_bytes > 4 * 1024 * 1024 {
                self.workspace.status = "This rename preview exceeds 4 MiB; it was not prepared.".to_owned();
                return;
            }
            if proposed_contents != document.contents {
                previews.push(VersionedEditPreview {
                    path: path.clone(),
                    document_version: document.version,
                    original_contents: document.contents.clone(),
                    proposed_contents,
                });
            }
        }
        if previews.is_empty() {
            self.workspace.status = "Rust Analyzer found no changes to apply.".to_owned();
            return;
        }
        previews.sort_by(|left, right| left.path.cmp(&right.path));
        self.semantic.edit_preview = None;
        self.semantic.workspace_edit_preview = Some(previews);
        self.workspace.status = "Review the multi-file Rust rename preview before applying it.".to_owned();
    }

    pub(super) fn apply_edit_preview(&mut self) {
        let Some(preview) = self.semantic.edit_preview.take() else {
            return;
        };
        let Some(document) = self.editor.documents.get_mut(&preview.path) else {
            self.workspace.status = "The file closed before the edit could be applied.".to_owned();
            return;
        };
        let version = match apply_versioned_edit(document, &preview) {
            Ok(version) => version,
            Err(EditPreviewApplyError::Stale) => {
                self.workspace.status = "The file changed after preview; the edit was discarded. Preview it again.".to_owned();
                return;
            }
            Err(EditPreviewApplyError::NoChange) => {
                self.workspace.status = "The edit no longer changes this file.".to_owned();
                return;
            }
            Err(EditPreviewApplyError::RevisionExhausted) => {
                self.workspace.status = "The document revision is exhausted; the edit was not applied.".to_owned();
                return;
            }
        };
        self.editor.cursor_offset = self.editor.cursor_offset.min(preview.proposed_contents.chars().count());
        self.editor.cursor_position = cursor_position(&preview.proposed_contents, self.editor.cursor_offset);
        self.did_change_document(&preview.path, preview.proposed_contents, version);
        self.workspace.status = format!("Applied formatting to {}. Save to write it to disk.", preview.path.display());
    }

    pub(super) fn apply_workspace_edit_preview(&mut self) {
        let Some(previews) = self.semantic.workspace_edit_preview.take() else {
            return;
        };
        let mut updates = Vec::with_capacity(previews.len());
        for preview in &previews {
            let Some(document) = self.editor.documents.get(&preview.path) else {
                self.workspace.status = "A file closed before the refactor could be applied; no files changed.".to_owned();
                return;
            };
            if document.version != preview.document_version || document.contents != preview.original_contents {
                self.workspace.status = "A file changed after preview; no files changed. Prepare the refactor again.".to_owned();
                return;
            }
            let Some(version) = document.version.checked_add(1) else {
                self.workspace.status = "A document revision is exhausted; no files changed.".to_owned();
                return;
            };
            updates.push((preview.path.clone(), preview.proposed_contents.clone(), version));
        }
        if updates.is_empty() {
            self.workspace.status = "The refactor preview is empty.".to_owned();
            return;
        }
        for (path, contents, version) in &updates {
            if let Some(document) = self.editor.documents.get_mut(path) {
                document.contents.clone_from(contents);
                document.version = *version;
            }
        }
        for (path, contents, version) in updates {
            if self.editor.selected_file.as_ref() == Some(&path) {
                self.editor.cursor_offset = self.editor.cursor_offset.min(contents.chars().count());
                self.editor.cursor_position = cursor_position(&contents, self.editor.cursor_offset);
            }
            self.did_change_document(&path, contents, version);
        }
        self.workspace.status = format!("Applied the refactor to {} open files. Save each changed file to write it to disk.", previews.len());
    }

    pub(super) fn apply_completion(&mut self, context: &egui::Context, index: usize) {
        let Some(item) = self.language_server.completion_items.get(index).cloned() else {
            return;
        };
        let Some(path) = self.editor.selected_file.clone() else {
            return;
        };
        let Some(document) = self.editor.documents.get_mut(&path) else {
            return;
        };
        let (start, end) = identifier_byte_range(&document.contents, self.editor.cursor_offset);
        document
            .contents
            .replace_range(start..end, &item.insert_text);
        document.version += 1;
        let contents = document.contents.clone();
        let version = document.version;
        let cursor_byte = start + item.insert_text.len();
        self.editor.cursor_offset = contents[..cursor_byte].chars().count();
        self.language_server.completion_items.clear();
        self.language_server.hover = None;
        vapor_gui_core::code_editor::select_range(
            context,
            &path,
            &contents,
            cursor_byte..cursor_byte,
        );
        self.did_change_document(&path, contents, version);
    }

    pub(super) fn navigate_to_definition(&mut self, result: &serde_json::Value) {
        let location = result
            .as_array()
            .and_then(|locations| locations.first())
            .unwrap_or(result);
        let Some(uri) = location
            .get("uri")
            .or_else(|| location.get("targetUri"))
            .and_then(serde_json::Value::as_str)
        else {
            self.workspace.status = "Rust Analyzer found no definition.".to_owned();
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
        let Some(root) = self.workspace.root.as_ref() else {
            return;
        };
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let canonical_path = absolute_path.canonicalize().unwrap_or(absolute_path);
        let Ok(path) = canonical_path.strip_prefix(&canonical_root) else {
            self.workspace.status = "Rust Analyzer returned a definition outside this workspace.".to_owned();
            return;
        };
        self.navigate_to_workspace_location(path, line as usize, character as usize);
    }

    pub(super) fn navigate_to_workspace_location(&mut self, path: &Path, line: usize, character: usize) {
        let path = path.to_path_buf();
        self.editor.pending_navigation = Some((path.clone(), line, character));
        if self.editor.documents.contains_key(&path) {
            self.editor.selected_file = Some(path);
        } else {
            self.open_file(path);
        }
    }

    pub(super) fn index_local_semantic_document(&mut self, path: &Path, version: u64, source: &str) {
        if source.len() > vapor_gui_core::semantic::MAX_SEMANTIC_DOCUMENT_BYTES {
            self.semantic.index
                .replace_document(path.to_path_buf(), version, Vec::new());
            if let Some(relative_path) = self
                .workspace.root.as_deref()
                .and_then(|root| relative_workspace_path(root, path))
            {
                self.semantic.problems.replace_source_diagnostics(
                    relative_path,
                    vapor_gui_core::DiagnosticOrigin::Vapor,
                    vec![semantic_source_limit_diagnostic()],
                );
            }
            self.workspace.status = "Semantic indexing paused for this file because it exceeds 2 MiB; editing remains available.".to_owned();
            return;
        }
        let extension = path.extension().and_then(|extension| extension.to_str());
        let relative_path = self
            .workspace.root.as_deref()
            .and_then(|root| relative_workspace_path(root, path));
        match extension {
            Some("toml") => {
                match toml_semantics::parse_toml_semantic_objects(path, source) {
                    Ok(objects) => {
                        self.semantic.index
                            .replace_document(path.to_path_buf(), version, objects);
                        if let Some(relative_path) = relative_path {
                            let diagnostics =
                                toml_semantics::authored_manifest_diagnostics(path, source);
                            if diagnostics.is_empty() {
                                self.semantic.problems.remove_source_diagnostics(
                                    &relative_path,
                                    vapor_gui_core::DiagnosticOrigin::Vapor,
                                );
                            } else {
                                self.semantic.problems.replace_source_diagnostics(
                                    relative_path,
                                    vapor_gui_core::DiagnosticOrigin::Vapor,
                                    diagnostics,
                                );
                            }
                        }
                    }
                    Err(error) => {
                        self.semantic.index
                            .replace_document(path.to_path_buf(), version, Vec::new());
                        if let Some(relative_path) = relative_path {
                            self.semantic.problems.replace_source_diagnostics(
                                relative_path,
                                vapor_gui_core::DiagnosticOrigin::Vapor,
                                vec![toml_parse_diagnostic(path, source, &error)],
                            );
                        }
                    }
                }
            }
            Some("xml") => {
                match xml_semantics::parse_xml_semantic_objects(path, source) {
                    Ok(objects) => {
                        self.semantic.index.replace_document(path.to_path_buf(), version, objects);
                        if let Some(relative_path) = relative_path {
                            self.semantic.problems.remove_source_diagnostics(
                                &relative_path,
                                vapor_gui_core::DiagnosticOrigin::Vapor,
                            );
                        }
                    }
                    Err(error) => {
                        self.semantic.index.replace_document(path.to_path_buf(), version, Vec::new());
                        if let Some(relative_path) = relative_path {
                            self.semantic.problems.replace_source_diagnostics(
                                relative_path,
                                vapor_gui_core::DiagnosticOrigin::Vapor,
                                vec![xml_parse_diagnostic(path, source, &error)],
                            );
                        }
                    }
                }
            }
            Some("rhai") => {
                let objects = vapor_gui_core::rhai_semantics::parse_rhai_semantic_objects(path, source);
                self.semantic.index
                    .replace_document(path.to_path_buf(), version, objects);
                if let Some(relative_path) = relative_path {
                    let diagnostics =
                        vapor_gui_core::rhai_semantics::rhai_structure_diagnostics(path, source);
                    if diagnostics.is_empty() {
                        self.semantic.problems.remove_source_diagnostics(
                            &relative_path,
                            vapor_gui_core::DiagnosticOrigin::Vapor,
                        );
                    } else {
                        self.semantic.problems.replace_source_diagnostics(
                            relative_path,
                            vapor_gui_core::DiagnosticOrigin::Vapor,
                            diagnostics,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn did_open_document(&mut self, path: &Path) {
        if !self.language_server.ready || path.extension().is_none_or(|extension| extension != "rs")
        {
            return;
        }
        let (uri, text, version) = {
            let Some(document) = self.editor.documents.get(path) else {
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

    pub(super) fn document_uri(&self, path: &Path) -> String {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.root
                .as_ref()
                .map_or_else(|| path.to_path_buf(), |root| root.join(path))
        };
        file_uri(&path.canonicalize().unwrap_or(path))
    }

    pub(super) fn did_change_document(&mut self, path: &Path, text: String, version: i32) {
        self.semantic.index.invalidate_document(path);
        self.language_server.workspace_symbols.clear();
        self.language_server.workspace_symbol_query = None;
        if let Some(root) = self.workspace.root.as_deref()
            && let Some(relative_path) = relative_workspace_path(root, path)
        {
            self.semantic.problems.remove_source_file(&relative_path);
        }
        if let Some((document_version, contents)) = self.editor.documents
            .get(path)
            .map(|document| (document.version as u64, document.contents.clone()))
        {
            self.index_local_semantic_document(path, document_version, &contents);
        }
        if self.semantic.selected.as_ref().is_some_and(|selected| &selected.id.document == path) {
            self.semantic.selected = None;
            self.semantic.rename_target = None;
        }
        if !self.language_server.ready || path.extension().is_none_or(|extension| extension != "rs")
        {
            return;
        }
        let uri = self.document_uri(path);
        self.send_language_server_notification(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}
        }));
        self.request_document_symbols(path);
    }

    pub(super) fn did_close_document(&mut self, path: &Path) {
        if !self.language_server.ready || path.extension().is_none_or(|extension| extension != "rs")
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

    pub(super) fn did_save_document(&mut self, path: &Path) {
        if !self.language_server.ready || path.extension().is_none_or(|extension| extension != "rs")
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

    pub(super) fn handle_language_server_diagnostics(&mut self, mut batch: LanguageServerDiagnostics) {
        let Some(path) = path_from_file_uri(&batch.uri) else {
            return;
        };
        if batch.version.is_some_and(|version| {
            self.editor.documents
                .get(&path)
                .is_some_and(|document| document.version != version)
        }) {
            return;
        }
        let Some(root) = self.workspace.root.as_ref() else {
            return;
        };
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let canonical_path = path.canonicalize().unwrap_or(path);
        let Ok(relative_path) = canonical_path.strip_prefix(&canonical_root) else {
            return;
        };
        for diagnostic in &mut batch.diagnostics {
            for span in diagnostic.primary.iter_mut().chain(diagnostic.related.iter_mut()) {
                if let Some(path) = path_from_file_uri(&span.file) {
                    span.file = path.to_string_lossy().into_owned();
                }
            }
        }
        self.semantic.problems.replace_source_diagnostics(
            relative_path.to_path_buf(),
            vapor_gui_core::DiagnosticOrigin::RustAnalyzer,
            batch.diagnostics,
        );
        if batch.omitted_diagnostics > 0 {
            self.workspace.status = format!(
                "Rust Analyzer omitted {} malformed or excess diagnostics from one update.",
                batch.omitted_diagnostics
            );
        }
    }

    pub(super) fn handle_language_server_message(&mut self, message: serde_json::Value) {
        let id = message.get("id").and_then(serde_json::Value::as_i64);
        if id == Some(1) {
            if let Some(error) = message.get("error") {
                self.language_server.ready = false;
                self.language_server.status = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Rust Analyzer failed to initialize")
                    .to_owned();
                return;
            }
            if message.get("result").is_some() {
                self.language_server.ready = true;
                self.language_server.status = "Rust Analyzer ready".to_owned();
                self.send_language_server_notification(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "initialized",
                    "params": {}
                }));
                let open = self.editor.documents.keys().cloned().collect::<Vec<_>>();
                for path in open {
                    self.did_open_document(&path);
                    self.request_document_symbols(&path);
                }
                if self.semantic.explorer_symbols {
                    self.request_workspace_symbols(&self.workspace.file_query.clone());
                }
                return;
            }
        }
        if let Some(id) = id
            && let Some(request) = self.language_server.pending_requests.remove(&id)
        {
            if let Some(error) = message.get("error") {
                self.workspace.status = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Rust Analyzer request failed")
                    .to_owned();
                return;
            }
            let Some(result) = message.get("result") else {
                return;
            };
            if request.request == LspRequest::WorkspaceSymbols {
                if self.workspace.root.as_deref() == Some(request.path.as_path())
                    && request.query.as_deref() == Some(self.workspace.file_query.trim())
                    && request
                        .workspace_versions
                        .as_ref()
                        .is_some_and(|versions| workspace_versions_match(versions, &self.editor.documents))
                {
                    self.language_server.workspace_symbols = rust_semantics::parse_workspace_symbols(
                        result,
                        &request.path,
                    );
                    self.workspace.status = format!(
                        "Rust Analyzer found {} workspace symbols.",
                        self.language_server.workspace_symbols.len()
                    );
                } else if request.query.as_deref() == Some(self.workspace.file_query.trim()) {
                    self.workspace.status = "Open files changed during symbol search; search again for current results.".to_owned();
                }
                return;
            }
            if self.editor.documents.get(&request.path).is_none_or(|document| document.version != request.document_version) {
                return;
            }
            match request.request {
                LspRequest::Definition => self.navigate_to_definition(result),
                LspRequest::References => {
                    let target_is_selected = reference_request_matches_selection(
                        &request.path,
                        request.semantic_target.as_ref(),
                        request.cursor_offset,
                        self.editor.cursor_offset,
                        self.editor.selected_file.as_ref(),
                        self.semantic.selected.as_ref(),
                    );
                    let document_is_current = self.editor.documents
                        .get(&request.path)
                        .is_some_and(|document| document.version == request.document_version);
                    let workspace_is_current = request
                        .workspace_versions
                        .as_ref()
                        .is_some_and(|versions| workspace_versions_match(versions, &self.editor.documents));
                    if target_is_selected && document_is_current && workspace_is_current {
                        (self.language_server.references, self.language_server.references_omitted) = self
                            .workspace.root.as_deref()
                            .map_or_else(|| (Vec::new(), 0), |root| reference_locations(result, root));
                    }
                }
                LspRequest::Rename => {
                    let target_is_selected = request.semantic_target.as_ref()
                        == self.semantic.selected.as_ref();
                    if target_is_selected {
                        self.prepare_workspace_edit(&request, result);
                    }
                }
                LspRequest::Hover if self.editor.selected_file.as_ref() == Some(&request.path) => {
                    self.language_server.hover = hover_text(result);
                }
                LspRequest::Completion if self.editor.selected_file.as_ref() == Some(&request.path) => {
                    self.language_server.completion_items = completion_items(result);
                }
                LspRequest::Formatting => self.apply_lsp_formatting(&request.path, result),
                LspRequest::DocumentSymbols => {
                    if let Some(document) = self.editor.documents.get(&request.path) {
                        let parsed = rust_semantics::parse_semantic_document_symbols(
                            result,
                            &request.path,
                            document.version as u64,
                            &document.contents,
                        );
                        let symbol_count = parsed.objects.len();
                        let source_too_large = document.contents.len()
                            > vapor_gui_core::semantic::MAX_SEMANTIC_DOCUMENT_BYTES;
                        if let Some(relative_path) = self
                            .workspace.root.as_deref()
                            .and_then(|workspace| relative_workspace_path(workspace, &request.path))
                        {
                            let mut diagnostics = rust_semantics::convention_diagnostics(
                                &request.path,
                                &parsed.objects,
                            );
                            if source_too_large {
                                diagnostics.push(semantic_source_limit_diagnostic());
                            }
                            if diagnostics.is_empty() {
                                self.semantic.problems.remove_source_diagnostics(
                                    &relative_path,
                                    vapor_gui_core::DiagnosticOrigin::Vapor,
                                );
                            } else {
                                self.semantic.problems.replace_source_diagnostics(
                                    relative_path,
                                    vapor_gui_core::DiagnosticOrigin::Vapor,
                                    diagnostics,
                                );
                            }
                        }
                        self.semantic.index.replace_document(
                            request.path.clone(),
                            document.version as u64,
                            parsed.objects,
                        );
                        if parsed.truncated {
                            self.workspace.status = if source_too_large {
                                "Semantic indexing paused for this file because it exceeds 2 MiB; editing remains available.".to_owned()
                            } else {
                                format!("Rust Analyzer returned a large document; indexed the first {symbol_count} semantic objects. Narrow the file or query to inspect the rest.")
                            };
                        }
                    }
                }
                LspRequest::WorkspaceSymbols => {}
                LspRequest::Hover | LspRequest::Completion => {}
            }
            return;
        }

    }

}

pub(super) fn reference_request_matches_selection(
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
