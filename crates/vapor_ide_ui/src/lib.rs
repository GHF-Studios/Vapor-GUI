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
mod lsp_text;

use authoring::WorkspaceAuthoring;
use diagnostics::{
    ProblemRow, ProblemRowRef, ProblemStore, diagnostic_origin_label,
    operation_produces_build_diagnostics, semantic_source_limit_diagnostic,
    workspace_diagnostic_path,
};
use language_server::{LanguageServerRequest, LspRequest, PendingLspRequest};
use lsp_text::{
    apply_text_edits, completion_items, cursor_position, file_uri, find_matches,
    hover_text, identifier_byte_range, lsp_position, path_from_file_uri,
    position_to_byte_offset, reference_locations, relative_workspace_path,
    rust_analyzer_initialization_options, semantic_object_byte_range,
};
use toml_semantics::toml_parse_diagnostic;
use xml_semantics::xml_parse_diagnostic;
use workspace_navigation::{
    FileTreeView, canonical_rust_item_kind, document_tab_label, draw_file_tree,
    reference_range, rust_crate_roots,
};
#[cfg(test)]
use language_server::{
    pending_lsp_requests_for_cancellation, reference_request_matches_selection,
    workspace_versions_match,
};
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

fn nonempty_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}


#[cfg(test)]
mod tests;
