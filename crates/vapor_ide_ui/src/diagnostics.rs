use std::{
    collections::BTreeMap,
    ops::Range,
    path::{Path, PathBuf},
};

use vapor_gui_core::{
    DiagnosticOrigin, DiagnosticSeverity, StructuredDiagnostic, semantic::SemanticRange,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProblemRow {
    pub(super) path: Option<PathBuf>,
    pub(super) action: Option<String>,
    pub(super) diagnostic: StructuredDiagnostic,
    pub(super) linked_diagnostics: Vec<StructuredDiagnostic>,
}

pub(super) struct ProblemRowRef<'a> {
    pub(super) path: Option<&'a PathBuf>,
    pub(super) action: Option<&'a str>,
    pub(super) diagnostic: &'a StructuredDiagnostic,
    pub(super) linked_diagnostics: &'a [StructuredDiagnostic],
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct ProblemFilter {
    pub(super) severity: Option<DiagnosticSeverity>,
    pub(super) origin: Option<DiagnosticOrigin>,
    pub(super) domain: Option<String>,
    pub(super) scope: Option<vapor_gui_core::DiagnosticScope>,
    pub(super) query: String,
}

impl ProblemFilter {
    fn is_empty(&self) -> bool {
        self.severity.is_none()
            && self.origin.is_none()
            && self.domain.is_none()
            && self.scope.is_none()
            && self.query.trim().is_empty()
    }
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
    source: BTreeMap<PathBuf, Vec<ProblemRow>>,
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
            for rows in self.source.values_mut() {
                rows.retain_mut(remove_build_diagnostics);
            }
            self.source.retain(|_, rows| !rows.is_empty());
            let mut remaining = Vec::new();
            for mut row in std::mem::take(&mut self.build) {
                if !remove_build_diagnostics(&mut row) {
                    continue;
                }
                row.action = None;
                if let Some(path) = row.path.clone() {
                    let rows = self.source.entry(path).or_default();
                    if !rows
                        .iter_mut()
                        .any(|existing| merge_problem_row(existing, &row))
                    {
                        rows.push(row);
                    }
                } else {
                    remaining.push(row);
                }
            }
            self.build_operation = Some(operation_id.to_owned());
            self.build = remaining;
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
        if self.merge_report(&row) {
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
        let remove_source_origins = |row: &mut ProblemRow| {
            remove_diagnostics(row, |origin| {
                matches!(
                    origin,
                    DiagnosticOrigin::RustAnalyzer | DiagnosticOrigin::Vapor
                )
            })
        };
        if let Some(rows) = self.source.get_mut(path) {
            rows.retain_mut(remove_source_origins);
        }
        self.source.retain(|_, rows| !rows.is_empty());
        self.build
            .retain_mut(|row| row.path.as_deref() != Some(path) || remove_source_origins(row));
    }

    pub(super) fn remove_source_diagnostics(&mut self, path: &Path, origin: DiagnosticOrigin) {
        if let Some(rows) = self.source.get_mut(path) {
            rows.retain_mut(|row| remove_diagnostics(row, |candidate| candidate == origin));
        }
        self.source.retain(|_, rows| !rows.is_empty());
        self.build.retain_mut(|row| {
            row.path.as_deref() != Some(path)
                || remove_diagnostics(row, |candidate| candidate == origin)
        });
    }

    pub(super) fn replace_source_diagnostics(
        &mut self,
        path: PathBuf,
        origin: DiagnosticOrigin,
        diagnostics: Vec<StructuredDiagnostic>,
    ) {
        self.remove_source_diagnostics(&path, origin);
        for diagnostic in diagnostics {
            let row = ProblemRow {
                path: Some(path.clone()),
                action: None,
                diagnostic,
                linked_diagnostics: Vec::new(),
            };
            if !self.merge_report(&row) {
                self.source.entry(path.clone()).or_default().push(row);
            }
        }
    }

    pub(super) fn for_semantic_range(
        &self,
        path: &Path,
        range: SemanticRange,
    ) -> Vec<ProblemRowRef<'_>> {
        let mut diagnostics = self
            .source
            .get(path)
            .into_iter()
            .flatten()
            .filter(|row| {
                diagnostic_overlaps_range(&row.diagnostic, range)
                    || row
                        .linked_diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic_overlaps_range(diagnostic, range))
            })
            .map(|row| ProblemRowRef {
                path: row.path.as_ref(),
                action: row.action.as_deref(),
                diagnostic: &row.diagnostic,
                linked_diagnostics: &row.linked_diagnostics,
            })
            .collect::<Vec<_>>();
        diagnostics.extend(
            self.build
                .iter()
                .filter(|row| row.path.as_deref() == Some(path))
                .filter(|row| {
                    diagnostic_overlaps_range(&row.diagnostic, range)
                        || row
                            .linked_diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic_overlaps_range(diagnostic, range))
                })
                .map(|row| ProblemRowRef {
                    path: row.path.as_ref(),
                    action: row.action.as_deref(),
                    diagnostic: &row.diagnostic,
                    linked_diagnostics: &row.linked_diagnostics,
                }),
        );
        diagnostics
    }

    pub(super) fn count(&self) -> usize {
        self.source.values().map(Vec::len).sum::<usize>()
            + self.build.len()
            + usize::from(self.omitted_build_problems > 0)
    }

    pub(super) fn count_filtered(&self, filter: &ProblemFilter) -> usize {
        if filter.is_empty() {
            return self.count();
        }
        self.source
            .values()
            .flatten()
            .chain(self.build.iter())
            .filter(|row| problem_row_matches(row, filter))
            .count()
            + usize::from(
                self.omitted_build_problems > 0
                    && filter_matches_diagnostic(&self.omitted_summary, filter),
            )
    }

    pub(super) fn filter_domains(&self) -> std::collections::BTreeSet<String> {
        self.source
            .values()
            .flatten()
            .chain(self.build.iter())
            .flat_map(|row| {
                std::iter::once(&row.diagnostic)
                    .chain(row.linked_diagnostics.iter())
                    .filter_map(|diagnostic| diagnostic.context.domain.clone())
            })
            .collect()
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
                rows.extend(
                    diagnostics[local_start..local_end]
                        .iter()
                        .map(|diagnostic| ProblemRowRef {
                            path: diagnostic.path.as_ref().or(Some(path)),
                            action: diagnostic.action.as_deref(),
                            diagnostic: &diagnostic.diagnostic,
                            linked_diagnostics: &diagnostic.linked_diagnostics,
                        }),
                );
            }
            offset = next_offset;
            if offset >= end {
                return rows;
            }
        }
        let build_start = start.saturating_sub(offset).min(self.build.len());
        let build_end = end.saturating_sub(offset).min(self.build.len());
        rows.extend(
            self.build[build_start..build_end]
                .iter()
                .map(|row| ProblemRowRef {
                    path: row.path.as_ref(),
                    action: row.action.as_deref(),
                    diagnostic: &row.diagnostic,
                    linked_diagnostics: &row.linked_diagnostics,
                }),
        );
        offset = offset.saturating_add(self.build.len());
        if self.omitted_build_problems > 0 && start <= offset && end > offset {
            rows.push(ProblemRowRef {
                path: None,
                action: None,
                diagnostic: &self.omitted_summary,
                linked_diagnostics: &[],
            });
        }
        rows
    }

    pub(super) fn rows_in_filtered_range(
        &self,
        range: Range<usize>,
        filter: &ProblemFilter,
    ) -> Vec<ProblemRowRef<'_>> {
        if filter.is_empty() {
            return self.rows_in_range(range);
        }
        let count = self.count_filtered(filter);
        let start = range.start.min(count);
        let end = range.end.min(count).max(start);
        let mut rows = Vec::with_capacity(end - start);
        let mut offset = 0usize;
        for row in self.source.values().flatten().chain(self.build.iter()) {
            if !problem_row_matches(row, filter) {
                continue;
            }
            if offset >= start && offset < end {
                rows.push(ProblemRowRef {
                    path: row.path.as_ref(),
                    action: row.action.as_deref(),
                    diagnostic: &row.diagnostic,
                    linked_diagnostics: &row.linked_diagnostics,
                });
            }
            offset += 1;
            if offset >= end {
                return rows;
            }
        }
        if self.omitted_build_problems > 0
            && filter_matches_diagnostic(&self.omitted_summary, filter)
            && offset >= start
            && offset < end
        {
            rows.push(ProblemRowRef {
                path: None,
                action: None,
                diagnostic: &self.omitted_summary,
                linked_diagnostics: &[],
            });
        }
        rows
    }

    fn merge_report(&mut self, incoming: &ProblemRow) -> bool {
        if let Some(path) = incoming.path.as_deref()
            && let Some(rows) = self.source.get_mut(path)
            && rows.iter_mut().any(|row| merge_problem_row(row, incoming))
        {
            return true;
        }
        self.build
            .iter_mut()
            .any(|row| merge_problem_row(row, incoming))
    }
}

fn problem_row_matches(row: &ProblemRow, filter: &ProblemFilter) -> bool {
    std::iter::once(&row.diagnostic)
        .chain(row.linked_diagnostics.iter())
        .any(|diagnostic| filter_matches_diagnostic(diagnostic, filter))
}

fn filter_matches_diagnostic(diagnostic: &StructuredDiagnostic, filter: &ProblemFilter) -> bool {
    filter
        .severity
        .is_none_or(|severity| diagnostic.severity == severity)
        && filter
            .origin
            .is_none_or(|origin| diagnostic.origin == origin)
        && filter
            .domain
            .as_deref()
            .is_none_or(|domain| diagnostic.context.domain.as_deref() == Some(domain))
        && filter
            .scope
            .is_none_or(|scope| diagnostic.context.scope == scope)
        && {
            let query = filter.query.trim().to_lowercase();
            query.is_empty()
                || diagnostic.message.to_lowercase().contains(&query)
                || diagnostic
                    .code
                    .as_ref()
                    .is_some_and(|code| code.to_lowercase().contains(&query))
                || diagnostic
                    .rendered
                    .as_ref()
                    .is_some_and(|rendered| rendered.to_lowercase().contains(&query))
                || diagnostic.raw_payload.to_lowercase().contains(&query)
                || diagnostic
                    .context
                    .operation_id
                    .as_ref()
                    .is_some_and(|operation| operation.to_lowercase().contains(&query))
        }
}

fn merge_problem_row(existing: &mut ProblemRow, incoming: &ProblemRow) -> bool {
    if existing.path != incoming.path {
        return false;
    }
    if existing.diagnostic == incoming.diagnostic
        || existing.linked_diagnostics.contains(&incoming.diagnostic)
    {
        return true;
    }
    if !diagnostics_correlate(&existing.diagnostic, &incoming.diagnostic)
        && !existing
            .linked_diagnostics
            .iter()
            .any(|diagnostic| diagnostics_correlate(diagnostic, &incoming.diagnostic))
    {
        return false;
    }
    if existing.action.is_none() {
        existing.action.clone_from(&incoming.action);
    }
    if origin_priority(incoming.diagnostic.origin) > origin_priority(existing.diagnostic.origin) {
        existing.linked_diagnostics.push(std::mem::replace(
            &mut existing.diagnostic,
            incoming.diagnostic.clone(),
        ));
    } else {
        existing
            .linked_diagnostics
            .push(incoming.diagnostic.clone());
    }
    true
}

fn diagnostics_correlate(left: &StructuredDiagnostic, right: &StructuredDiagnostic) -> bool {
    if left.severity != right.severity || left.code != right.code || left.message != right.message {
        return false;
    }
    let (Some(left), Some(right)) = (&left.primary, &right.primary) else {
        return false;
    };
    left.start_line == right.start_line
        && left.start_column == right.start_column
        && left.end_line == right.end_line
        && left.end_column == right.end_column
}

fn origin_priority(origin: DiagnosticOrigin) -> u8 {
    match origin {
        DiagnosticOrigin::RustAnalyzer => 5,
        DiagnosticOrigin::Rustc => 4,
        DiagnosticOrigin::Cargo => 3,
        DiagnosticOrigin::Rustdoc => 2,
        DiagnosticOrigin::Vapor => 1,
    }
}

fn remove_build_diagnostics(row: &mut ProblemRow) -> bool {
    let has_build_diagnostic = [row.diagnostic.origin]
        .into_iter()
        .chain(
            row.linked_diagnostics
                .iter()
                .map(|diagnostic| diagnostic.origin),
        )
        .any(|origin| {
            matches!(
                origin,
                DiagnosticOrigin::Cargo | DiagnosticOrigin::Rustc | DiagnosticOrigin::Rustdoc
            )
        });
    let remains = remove_diagnostics(row, |origin| {
        matches!(
            origin,
            DiagnosticOrigin::Cargo | DiagnosticOrigin::Rustc | DiagnosticOrigin::Rustdoc
        )
    });
    if has_build_diagnostic {
        row.action = None;
    }
    remains
}

fn remove_diagnostics(
    row: &mut ProblemRow,
    mut should_remove: impl FnMut(DiagnosticOrigin) -> bool,
) -> bool {
    if should_remove(row.diagnostic.origin) {
        let Some(index) = row
            .linked_diagnostics
            .iter()
            .position(|diagnostic| !should_remove(diagnostic.origin))
        else {
            return false;
        };
        row.diagnostic = row.linked_diagnostics.remove(index);
    }
    row.linked_diagnostics
        .retain(|diagnostic| !should_remove(diagnostic.origin));
    true
}

fn diagnostic_overlaps_range(diagnostic: &StructuredDiagnostic, range: SemanticRange) -> bool {
    diagnostic.primary.as_ref().is_some_and(|span| {
        let start = (
            span.start_line.saturating_sub(1),
            span.start_column.saturating_sub(1),
        );
        let end = (
            span.end_line.saturating_sub(1),
            span.end_column.saturating_sub(1),
        );
        let object_start = (range.start.line, range.start.character);
        let object_end = (range.end.line, range.end.character);
        if start == end {
            object_start <= start && start <= object_end
        } else {
            start < object_end && end > object_start
        }
    })
}

pub(super) fn truncate_diagnostic_payload(payload: &str) -> String {
    const MAX_BYTES: usize = 64 * 1024;
    if payload.len() <= MAX_BYTES {
        return payload.to_owned();
    }
    let mut end = MAX_BYTES;
    while !payload.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &payload[..end])
}

pub(super) fn workspace_diagnostic_path(
    workspace: &Path,
    diagnostic_file: &str,
    known_files: &[PathBuf],
) -> Option<PathBuf> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let path = Path::new(diagnostic_file);
    if diagnostic_file.trim().is_empty() || diagnostic_file.starts_with("file:") {
        return None;
    }
    if path.is_absolute() {
        let canonical = path.canonicalize().ok()?;
        return canonical
            .strip_prefix(&workspace)
            .ok()
            .map(Path::to_path_buf);
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
    canonical
        .strip_prefix(workspace)
        .ok()
        .map(Path::to_path_buf)
}

pub(super) fn resolve_registered_workspace_project(
    workspace: &Path,
    declared_path: &Path,
) -> Option<PathBuf> {
    if declared_path.as_os_str().is_empty() || declared_path.is_absolute() {
        return None;
    }
    let workspace = workspace.canonicalize().ok()?;
    let project = workspace.join(declared_path).canonicalize().ok()?;
    if !project.starts_with(&workspace) || !project.is_dir() {
        return None;
    }
    Some(project)
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
    fn matching_language_and_build_reports_share_a_row_without_losing_evidence() {
        let mut store = ProblemStore::default();
        let mut language = source_diagnostic(5, 4, 10);
        language.severity = DiagnosticSeverity::Error;
        language.code = Some("E0425".to_owned());
        language.message = "cannot find value `ship`".to_owned();
        language.raw_payload = "rust-analyzer record".to_owned();
        store.replace_source_diagnostics(
            PathBuf::from("src/components.rs"),
            DiagnosticOrigin::RustAnalyzer,
            vec![language],
        );

        let mut cargo = source_diagnostic(5, 4, 10);
        cargo.origin = DiagnosticOrigin::Cargo;
        cargo.severity = DiagnosticSeverity::Error;
        cargo.code = Some("E0425".to_owned());
        cargo.message = "cannot find value `ship`".to_owned();
        cargo.raw_payload = "cargo record".to_owned();
        store.begin_build("build-1");
        store.push_build(
            "build-1",
            ProblemRow {
                path: Some(PathBuf::from("src/components.rs")),
                action: Some("workspace.check".to_owned()),
                diagnostic: cargo,
                linked_diagnostics: Vec::new(),
            },
        );

        let rows = store.rows();
        assert_eq!(rows.len(), 1, "one issue should have one visible row");
        assert_eq!(rows[0].diagnostic.origin, DiagnosticOrigin::RustAnalyzer);
        assert_eq!(rows[0].action, Some("workspace.check"));
        assert_eq!(rows[0].linked_diagnostics.len(), 1);
        assert_eq!(
            rows[0].linked_diagnostics[0].origin,
            DiagnosticOrigin::Cargo
        );
        assert_eq!(
            rows[0].linked_diagnostics[0]
                .context
                .operation_id
                .as_deref(),
            Some("build-1")
        );
        assert_eq!(rows[0].linked_diagnostics[0].raw_payload, "cargo record");
        let selected = store.for_semantic_range(
            Path::new("src/components.rs"),
            SemanticRange {
                start: SemanticPosition {
                    line: 4,
                    character: 3,
                },
                end: SemanticPosition {
                    line: 4,
                    character: 9,
                },
            },
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].linked_diagnostics.len(), 1);

        store.begin_build("build-2");
        let rows = store.rows();
        assert_eq!(rows.len(), 1, "the current source report survives rebuild");
        assert!(rows[0].linked_diagnostics.is_empty());
        assert_eq!(rows[0].action, None, "stale build action is cleared");
    }

    #[test]
    fn language_report_arriving_after_build_report_becomes_the_primary_row() {
        let mut store = ProblemStore::default();
        store.begin_build("build-1");
        let mut cargo = source_diagnostic(8, 2, 7);
        cargo.origin = DiagnosticOrigin::Cargo;
        cargo.raw_payload = "cargo payload".to_owned();
        store.push_build(
            "build-1",
            ProblemRow {
                path: Some(PathBuf::from("src/components.rs")),
                action: Some("workspace.check".to_owned()),
                diagnostic: cargo,
                linked_diagnostics: Vec::new(),
            },
        );

        let mut language = source_diagnostic(8, 2, 7);
        language.origin = DiagnosticOrigin::RustAnalyzer;
        language.raw_payload = "language payload".to_owned();
        store.replace_source_diagnostics(
            PathBuf::from("src/components.rs"),
            DiagnosticOrigin::RustAnalyzer,
            vec![language],
        );

        let rows = store.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].diagnostic.origin, DiagnosticOrigin::RustAnalyzer);
        assert_eq!(rows[0].linked_diagnostics.len(), 1);
        assert_eq!(
            rows[0].linked_diagnostics[0].origin,
            DiagnosticOrigin::Cargo
        );

        store.begin_build("build-2");
        let rows = store.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].diagnostic.origin, DiagnosticOrigin::RustAnalyzer);
        assert!(rows[0].linked_diagnostics.is_empty());
        assert_eq!(rows[0].action, None);
    }

    #[test]
    fn problem_filters_match_any_linked_producer_but_keep_the_grouped_row() {
        let mut store = ProblemStore::default();
        let mut language = source_diagnostic(4, 1, 6);
        language.context.domain = Some("rust".to_owned());
        language.context.scope = vapor_gui_core::DiagnosticScope::Document;
        store.replace_source_diagnostics(
            PathBuf::from("src/components.rs"),
            DiagnosticOrigin::RustAnalyzer,
            vec![language],
        );
        store.begin_build("compile-17");
        let mut cargo = source_diagnostic(4, 1, 6);
        cargo.origin = DiagnosticOrigin::Cargo;
        cargo.context.domain = Some("rust".to_owned());
        cargo.context.scope = vapor_gui_core::DiagnosticScope::Document;
        store.push_build(
            "compile-17",
            ProblemRow {
                path: Some(PathBuf::from("src/components.rs")),
                action: Some("workspace.check".to_owned()),
                diagnostic: cargo,
                linked_diagnostics: Vec::new(),
            },
        );

        let filter = ProblemFilter {
            origin: Some(DiagnosticOrigin::Cargo),
            domain: Some("rust".to_owned()),
            scope: Some(vapor_gui_core::DiagnosticScope::Document),
            query: "compile-17".to_owned(),
            ..Default::default()
        };
        assert_eq!(store.count_filtered(&filter), 1);
        let rows = store.rows_in_filtered_range(0..1, &filter);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].diagnostic.origin, DiagnosticOrigin::RustAnalyzer);
        assert_eq!(rows[0].linked_diagnostics.len(), 1);
        assert_eq!(
            store.filter_domains().into_iter().collect::<Vec<_>>(),
            vec!["rust"]
        );
    }

    #[test]
    fn semantic_inspector_includes_overlapping_build_diagnostics_for_the_document() {
        let mut store = ProblemStore::default();
        store.begin_build("build-1");
        let mut matching = source_diagnostic(5, 4, 10);
        matching.origin = DiagnosticOrigin::Cargo;
        matching.code = Some("rustc.type-mismatch".to_owned());
        store.push_build(
            "build-1",
            ProblemRow {
                path: Some(PathBuf::from("src/components.rs")),
                action: Some("check".to_owned()),
                diagnostic: matching,
                linked_diagnostics: Vec::new(),
            },
        );
        let mut other_document = source_diagnostic(5, 4, 10);
        other_document.origin = DiagnosticOrigin::Cargo;
        store.push_build(
            "build-1",
            ProblemRow {
                path: Some(PathBuf::from("src/systems.rs")),
                action: Some("check".to_owned()),
                diagnostic: other_document,
                linked_diagnostics: Vec::new(),
            },
        );

        let overlapping = store.for_semantic_range(
            Path::new("src/components.rs"),
            SemanticRange {
                start: SemanticPosition {
                    line: 4,
                    character: 0,
                },
                end: SemanticPosition {
                    line: 5,
                    character: 2,
                },
            },
        );

        assert_eq!(overlapping.len(), 1);
        assert_eq!(overlapping[0].diagnostic.origin, DiagnosticOrigin::Cargo);
        assert_eq!(
            overlapping[0].diagnostic.code.as_deref(),
            Some("rustc.type-mismatch")
        );
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
                start: SemanticPosition {
                    line: 4,
                    character: 0,
                },
                end: SemanticPosition {
                    line: 5,
                    character: 2,
                },
            },
        );
        let outside = store.for_semantic_range(
            Path::new("src/components.rs"),
            SemanticRange {
                start: SemanticPosition {
                    line: 5,
                    character: 10,
                },
                end: SemanticPosition {
                    line: 6,
                    character: 0,
                },
            },
        );

        assert_eq!(overlapping.len(), 1);
        assert_eq!(overlapping[0].diagnostic.code.as_deref(), Some("rust.test"));
        assert!(outside.is_empty());
    }

    #[test]
    fn registered_project_resolution_requires_an_existing_directory_inside_workspace() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("vapor-ide-project-{unique}"));
        let workspace = base.join("workspace");
        let project = workspace.join("content/game");
        let outside = base.join("outside");
        std::fs::create_dir_all(&project).expect("create in-workspace project");
        std::fs::create_dir_all(&outside).expect("create outside directory");

        assert_eq!(
            resolve_registered_workspace_project(&workspace, Path::new("content/game")),
            project.canonicalize().ok()
        );
        assert!(
            resolve_registered_workspace_project(&workspace, Path::new("../outside")).is_none()
        );
        assert!(resolve_registered_workspace_project(&workspace, &outside).is_none());
        assert!(resolve_registered_workspace_project(&workspace, Path::new("missing")).is_none());

        std::fs::remove_dir_all(base).expect("remove test directories");
    }
}
