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
