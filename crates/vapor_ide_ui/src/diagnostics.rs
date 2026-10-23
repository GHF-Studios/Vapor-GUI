use std::{collections::BTreeMap, ops::Range, path::{Path, PathBuf}};

use vapor_gui_core::{DiagnosticOrigin, DiagnosticSeverity, StructuredDiagnostic, semantic::SemanticRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProblemRow {
    pub(super) path: Option<PathBuf>,
    pub(super) action: Option<String>,
    pub(super) diagnostic: StructuredDiagnostic,
}

pub(super) struct ProblemRowRef<'a> {
    pub(super) path: Option<&'a PathBuf>,
    pub(super) action: Option<&'a str>,
    pub(super) diagnostic: &'a StructuredDiagnostic,
}

pub(super) fn attach_document_context(
    diagnostics: &mut [StructuredDiagnostic],
    workspace: &Path,
    document_version: u64,
) {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .into_owned();
    for diagnostic in diagnostics {
        diagnostic.context.workspace = Some(workspace.clone());
        diagnostic.context.document_version = Some(document_version);
        if diagnostic.context.scope == vapor_gui_core::DiagnosticScope::Unspecified {
            diagnostic.context.scope = vapor_gui_core::DiagnosticScope::Document;
        }
    }
}

const MAX_BUILD_PROBLEMS: usize = 2_000;

pub(super) struct ProblemStore {
    source: BTreeMap<PathBuf, Vec<StructuredDiagnostic>>,
    build_operation: Option<String>,
    build: Vec<ProblemRow>,
    omitted_build_problems: usize,
    omitted_summary: StructuredDiagnostic,
}

impl Default for ProblemStore {
    fn default() -> Self {
        Self {
            source: BTreeMap::new(),
            build_operation: None,
            build: Vec::new(),
            omitted_build_problems: 0,
            omitted_summary: StructuredDiagnostic {
                origin: DiagnosticOrigin::Vapor,
                severity: DiagnosticSeverity::Information,
                code: None,
                message: String::new(),
                rendered: None,
                primary: None,
                related: Vec::new(),

                raw_payload: String::new(),
                context: Default::default(),
            },
        }
    }
}

impl ProblemStore {
    pub(super) fn begin_build(&mut self, operation_id: &str) {
        if self.build_operation.as_deref() != Some(operation_id) {
            self.build_operation = Some(operation_id.to_owned());
            self.build.clear();
            self.omitted_build_problems = 0;
            self.omitted_summary.message.clear();
        }
    }

    pub(super) fn push_build(&mut self, operation_id: &str, mut row: ProblemRow) {
        if self.build_operation.as_deref() != Some(operation_id) {
            return;
        }
        row.diagnostic.context.operation_id = Some(operation_id.to_owned());
        if row.diagnostic.context.scope == vapor_gui_core::DiagnosticScope::Unspecified {
            row.diagnostic.context.scope = vapor_gui_core::DiagnosticScope::Operation;
        }
        if self.build.contains(&row) {
            return;
        }
        if self.build.len() < MAX_BUILD_PROBLEMS {
            self.build.push(row);
        } else {
            self.omitted_build_problems = self.omitted_build_problems.saturating_add(1);
            self.omitted_summary.message = format!(
                "{} additional build diagnostics omitted from this view",
                self.omitted_build_problems
            );
        }
    }

    pub(super) fn clear(&mut self) {
        self.source.clear();
        self.build_operation = None;
        self.build.clear();
        self.omitted_build_problems = 0;
        self.omitted_summary.message.clear();
    }

    pub(super) fn remove_source_file(&mut self, path: &Path) {
        self.source.remove(path);
    }

    pub(super) fn remove_source_diagnostics(&mut self, path: &Path, origin: DiagnosticOrigin) {
        let remove_file = if let Some(diagnostics) = self.source.get_mut(path) {
            diagnostics.retain(|diagnostic| diagnostic.origin != origin);
            diagnostics.is_empty()
        } else {
            false
        };
        if remove_file {
            self.source.remove(path);
        }
    }

    pub(super) fn replace_source_diagnostics(
        &mut self,
        path: PathBuf,
        origin: DiagnosticOrigin,
        diagnostics: Vec<StructuredDiagnostic>,
    ) {
        self.remove_source_diagnostics(&path, origin);
        if !diagnostics.is_empty() {
            self.source.entry(path).or_default().extend(diagnostics);
        }
    }

    pub(super) fn for_semantic_range(
        &self,
        path: &Path,
        range: SemanticRange,
    ) -> Vec<&StructuredDiagnostic> {
        self.source
            .get(path)
            .into_iter()
            .flatten()
            .filter(|diagnostic| {
                diagnostic.primary.as_ref().is_some_and(|span| {
                    let start = (span.start_line.saturating_sub(1), span.start_column.saturating_sub(1));
                    let end = (span.end_line.saturating_sub(1), span.end_column.saturating_sub(1));
                    let object_start = (range.start.line, range.start.character);
                    let object_end = (range.end.line, range.end.character);
                    if start == end {
                        object_start <= start && start <= object_end
                    } else {
                        start < object_end && end > object_start
                    }
                })
            })
            .collect()
    }

    pub(super) fn count(&self) -> usize {
        self.source.values().map(Vec::len).sum::<usize>()
            + self.build.len()
            + usize::from(self.omitted_build_problems > 0)
    }

    #[cfg(test)]
    pub(super) fn rows(&self) -> Vec<ProblemRowRef<'_>> {
        self.rows_in_range(0..self.count())
    }

    pub(super) fn rows_in_range(&self, range: Range<usize>) -> Vec<ProblemRowRef<'_>> {
        let count = self.count();
        let start = range.start.min(count);
        let end = range.end.min(count).max(start);
        let mut rows = Vec::with_capacity(end - start);
        let mut offset = 0usize;
        for (path, diagnostics) in &self.source {
            let next_offset = offset.saturating_add(diagnostics.len());
            if next_offset > start && offset < end {
                let local_start = start.saturating_sub(offset).min(diagnostics.len());
                let local_end = end.saturating_sub(offset).min(diagnostics.len());
                rows.extend(diagnostics[local_start..local_end].iter().map(|diagnostic| {
                    ProblemRowRef {
                        path: Some(path),
                        action: None,
                        diagnostic,
                    }
                }));
            }
            offset = next_offset;
            if offset >= end {
                return rows;
            }
        }
        let build_start = start.saturating_sub(offset).min(self.build.len());
        let build_end = end.saturating_sub(offset).min(self.build.len());
        rows.extend(self.build[build_start..build_end].iter().map(|row| ProblemRowRef {
            path: row.path.as_ref(),
            action: row.action.as_deref(),
            diagnostic: &row.diagnostic,
        }));
        offset = offset.saturating_add(self.build.len());
        if self.omitted_build_problems > 0 && start <= offset && end > offset {
            rows.push(ProblemRowRef {
                path: None,
                action: None,
                diagnostic: &self.omitted_summary,
            });
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vapor_gui_core::DiagnosticSpan;
    use vapor_gui_core::semantic::SemanticPosition;

    fn source_diagnostic(line: u32, start: u32, end: u32) -> StructuredDiagnostic {
        StructuredDiagnostic {
            origin: DiagnosticOrigin::RustAnalyzer,
            severity: DiagnosticSeverity::Warning,
            code: Some("rust.test".to_owned()),
            message: "warning on declaration".to_owned(),
            rendered: None,
            primary: Some(DiagnosticSpan {
                file: "src/components.rs".to_owned(),
                start_line: line,
                start_column: start,
                end_line: line,
                end_column: end,
                primary: true,
                label: None,
            }),
            related: Vec::new(),

            raw_payload: String::new(),
            context: Default::default(),
        }
    }

    #[test]
    fn semantic_inspector_gets_only_overlapping_source_diagnostics() {
        let mut store = ProblemStore::default();
        store.replace_source_diagnostics(
            PathBuf::from("src/components.rs"),
            DiagnosticOrigin::RustAnalyzer,
            vec![source_diagnostic(5, 4, 10)],
        );

        let overlapping = store.for_semantic_range(
            Path::new("src/components.rs"),
            SemanticRange {
                start: SemanticPosition { line: 4, character: 0 },
                end: SemanticPosition { line: 5, character: 2 },
            },
        );
        let outside = store.for_semantic_range(
            Path::new("src/components.rs"),
            SemanticRange {
                start: SemanticPosition { line: 5, character: 10 },
                end: SemanticPosition { line: 6, character: 0 },
            },
        );

        assert_eq!(overlapping.len(), 1);
        assert_eq!(overlapping[0].code.as_deref(), Some("rust.test"));
        assert!(outside.is_empty());
    }
}

pub(super) fn truncate_diagnostic_payload(payload: &str) -> String {
    const MAX_BYTES: usize = 64 * 1024;
    if payload.len() <= MAX_BYTES { return payload.to_owned(); }
    let mut end = MAX_BYTES;
    while !payload.is_char_boundary(end) { end -= 1; }
    format!("{}…", &payload[..end])
}

pub(super) fn workspace_diagnostic_path(
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

pub(super) fn operation_produces_build_diagnostics(action: &str) -> bool {
    matches!(
        action.rsplit('.').next(),
        Some("build" | "check" | "test" | "validate")
    )
}

pub(super) fn diagnostic_origin_label(origin: vapor_gui_core::DiagnosticOrigin) -> &'static str {
    match origin {
        vapor_gui_core::DiagnosticOrigin::RustAnalyzer => "Rust Analyzer",
        vapor_gui_core::DiagnosticOrigin::Cargo => "Cargo",
        vapor_gui_core::DiagnosticOrigin::Rustc => "rustc",
        vapor_gui_core::DiagnosticOrigin::Rustdoc => "rustdoc",
        vapor_gui_core::DiagnosticOrigin::Vapor => "Vapor",
    }
}

pub(super) fn diagnostic_details(diagnostic: &StructuredDiagnostic) -> String {
    let mut details = diagnostic.message.clone();
    let context = &diagnostic.context;
    let mut context_lines = Vec::new();
    if let Some(domain) = &context.domain {
        context_lines.push(format!("Domain: {domain}"));
    }
    if context.scope != vapor_gui_core::DiagnosticScope::Unspecified {
        context_lines.push(format!("Scope: {}", context.scope.label()));
    }
    if let Some(workspace) = &context.workspace {
        context_lines.push(format!("Workspace: {workspace}"));
    }
    if let Some(version) = context.document_version {
        context_lines.push(format!("Document revision: {version}"));
    }
    if let Some(semantic_path) = &context.semantic_path {
        context_lines.push(format!("Semantic object: {semantic_path}"));
    }
    if let Some(operation_id) = &context.operation_id {
        context_lines.push(format!("Operation: {operation_id}"));
    }
    if !context_lines.is_empty() {
        details.push_str("\n\n");
        details.push_str(&context_lines.join("\n"));
    }
    if !diagnostic.related.is_empty() {
        details.push_str("\n\nRelated locations");
        for related in &diagnostic.related {
            details.push_str(&format!(
                "\n{}:{}:{} {}",
                related.file,
                related.start_line,
                related.start_column,
                related.label.as_deref().unwrap_or("related")
            ));
        }
    }
    if !diagnostic.raw_payload.is_empty() {
        details.push_str(&format!("\n\nRaw diagnostic\n{}", diagnostic.raw_payload));
    }
    details
}

pub(super) fn semantic_source_limit_diagnostic() -> vapor_gui_core::StructuredDiagnostic {
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
        context: vapor_gui_core::DiagnosticContext {
            domain: Some("semantic".to_owned()),
            scope: vapor_gui_core::DiagnosticScope::Document,
            ..Default::default()
        },
    }
}
