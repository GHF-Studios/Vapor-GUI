use std::{collections::BTreeMap, ops::Range, path::{Path, PathBuf}};

use vapor_gui_core::{DiagnosticOrigin, DiagnosticSeverity, StructuredDiagnostic, semantic::SemanticRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProblemRow {
    pub(super) path: Option<PathBuf>,
    pub(super) operation: Option<String>,
    pub(super) diagnostic: StructuredDiagnostic,
}

pub(super) struct ProblemRowRef<'a> {
    pub(super) path: Option<&'a PathBuf>,
    pub(super) operation: Option<&'a str>,
    pub(super) diagnostic: &'a StructuredDiagnostic,
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

    pub(super) fn push_build(&mut self, operation_id: &str, row: ProblemRow) {
        if self.build_operation.as_deref() != Some(operation_id) || self.build.contains(&row) {
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
                        operation: None,
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
            operation: row.operation.as_deref(),
            diagnostic: &row.diagnostic,
        }));
        offset = offset.saturating_add(self.build.len());
        if self.omitted_build_problems > 0 && start <= offset && end > offset {
            rows.push(ProblemRowRef {
                path: None,
                operation: None,
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
