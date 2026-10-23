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
mod workspace_view;
mod editor_view;
mod inspector_view;
mod console_view;
mod service_session;
mod workspace_navigation;

use authoring::WorkspaceAuthoring;
use diagnostics::{ProblemRow, ProblemRowRef, ProblemStore};
use language_server::{LanguageServerRequest, LspRequest, PendingLspRequest};
use workspace_navigation::{
    FileTreeView, canonical_rust_item_kind, document_tab_label, draw_file_tree,
    reference_range, rust_crate_roots,
};
#[cfg(test)]
use language_server::{pending_lsp_requests_for_cancellation, workspace_versions_match};
#[cfg(test)]
use workspace_navigation::{
    file_display_label, rust_label_parts, shortest_rust_label, workspace_tree_ordering,
};

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
mod tests;
