use super::rust_semantics::{
    CanonicalRustItemKind, parse_semantic_symbols, render_canonical_rust_item, rust_lsp_symbol_kind,
};
use super::{
    Document, EditPreviewApplyError, ProblemRow, ProblemStore, ReferenceLocation,
    VersionedEditPreview, apply_text_edits, apply_versioned_edit, canonical_rust_item_kind,
    completion_items, cursor_position, document_tab_label, file_display_label, file_uri,
    find_matches, hover_text, identifier_byte_range, lsp_position, nonempty_path,
    operation_produces_build_diagnostics, path_from_file_uri,
    pending_lsp_requests_for_cancellation, pending_request_snapshot_is_current,
    position_to_byte_offset, reference_locations, reference_request_matches_selection,
    relative_workspace_path, rust_analyzer_client_capabilities,
    rust_analyzer_initialization_options, rust_crate_roots, rust_label_parts,
    semantic_object_byte_range, semantic_source_limit_diagnostic, shortest_rust_label,
    toml_parse_diagnostic, workspace_diagnostic_path, workspace_tree_ordering,
    workspace_versions_match,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use vapor_gui_core::semantic::SemanticObjectKind;

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
        workspace_diagnostic_path(
            &root,
            "src/main.rs",
            &[PathBuf::from("crates/game/src/main.rs")]
        ),
        Some(PathBuf::from("crates/game/src/main.rs")),
        "nested package-relative diagnostic paths resolve through the known file list"
    );
    assert_eq!(
        workspace_diagnostic_path(
            &root,
            "src/main.rs",
            &[
                PathBuf::from("crates/a/src/main.rs"),
                PathBuf::from("crates/b/src/main.rs")
            ],
        ),
        None,
        "ambiguous suffix matches must not navigate to an arbitrary file"
    );
    let outside = root.with_extension("outside.rs");
    std::fs::write(&outside, "fn outside() {}\n").unwrap();
    assert_eq!(
        workspace_diagnostic_path(&root, outside.to_str().unwrap(), &[]),
        None
    );

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
        context: Default::default(),
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
        context: Default::default(),
    };
    store.begin_build("build-1");
    store.push_build(
        "build-1",
        ProblemRow {
            path: workspace_diagnostic_path(&root, "src/main.rs", &[PathBuf::from("src/main.rs")]),
            action: Some("workspace.test".to_owned()),
            diagnostic: build_diagnostic.clone(),
        },
    );
    store.push_build(
        "build-1",
        ProblemRow {
            path: workspace_diagnostic_path(&root, "src/main.rs", &[PathBuf::from("src/main.rs")]),
            action: Some("workspace.test".to_owned()),
            diagnostic: build_diagnostic,
        },
    );
    assert_eq!(
        store.count(),
        2,
        "repeated output should not duplicate a build problem"
    );
    let rows = store.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].diagnostic.origin,
        vapor_gui_core::DiagnosticOrigin::RustAnalyzer
    );
    assert_eq!(
        rows[1].diagnostic.origin,
        vapor_gui_core::DiagnosticOrigin::Cargo
    );
    assert_eq!(rows[1].action, Some("workspace.test"));
    assert_eq!(
        rows[1].diagnostic.context.operation_id.as_deref(),
        Some("build-1")
    );

    store.begin_build("build-2");
    store.push_build(
        "build-1",
        ProblemRow {
            path: Some(PathBuf::from("src/main.rs")),
            action: Some("workspace.test".to_owned()),
            diagnostic: vapor_gui_core::StructuredDiagnostic {
                origin: vapor_gui_core::DiagnosticOrigin::Cargo,
                severity: vapor_gui_core::DiagnosticSeverity::Error,
                code: Some("E0425".to_owned()),
                message: "cannot find value".to_owned(),
                rendered: None,
                primary: None,
                related: Vec::new(),

                raw_payload: "{\"reason\":\"compiler-message\"}".to_owned(),
                context: Default::default(),
            },
        },
    );
    let rows = store.rows();
    assert_eq!(rows.len(), 1, "late output from a prior build is ignored");
    assert_eq!(
        rows[0].diagnostic.origin,
        vapor_gui_core::DiagnosticOrigin::RustAnalyzer
    );

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
        context: Default::default(),
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
        context: Default::default(),
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
    assert_eq!(
        rows[0].diagnostic.origin,
        vapor_gui_core::DiagnosticOrigin::Vapor
    );
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
            context: Default::default(),
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
            action: Some("workspace.test".to_owned()),
            diagnostic: vapor_gui_core::StructuredDiagnostic {
                origin: vapor_gui_core::DiagnosticOrigin::Cargo,
                severity: vapor_gui_core::DiagnosticSeverity::Error,
                code: None,
                message: "build row".to_owned(),
                rendered: None,
                primary: None,
                related: Vec::new(),

                raw_payload: String::new(),
                context: Default::default(),
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
        path,
        Some(&selected),
        None,
        0,
        None,
        Some(&selected)
    ));
    assert!(!reference_request_matches_selection(
        path,
        Some(&selected),
        None,
        0,
        None,
        Some(&other)
    ));
    assert!(reference_request_matches_selection(
        path,
        None,
        Some(4),
        4,
        Some(&path.to_path_buf()),
        None
    ));
    assert!(!reference_request_matches_selection(
        path,
        None,
        Some(4),
        5,
        Some(&path.to_path_buf()),
        None
    ));
    assert!(!reference_request_matches_selection(
        path,
        None,
        None,
        0,
        Some(&PathBuf::from("src/lib.rs")),
        None
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
    for action in [
        "workspace.build",
        "workspace.check",
        "workspace.test",
        "workspace.validate",
    ] {
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

    let mut stale = Document {
        version: 5,
        ..current.clone()
    };
    assert_eq!(
        apply_versioned_edit(&mut stale, &preview),
        Err(EditPreviewApplyError::Stale)
    );

    let mut changed = Document {
        contents: "concurrent edit".to_owned(),
        ..current
    };
    assert_eq!(
        apply_versioned_edit(&mut changed, &preview),
        Err(EditPreviewApplyError::Stale)
    );
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
fn rust_analyzer_client_capabilities_advertise_previewable_quick_fixes_without_server_writes() {
    let capabilities = rust_analyzer_client_capabilities();
    assert_eq!(
        capabilities["textDocument"]["codeAction"]["dynamicRegistration"],
        false
    );
    assert_eq!(
        capabilities["textDocument"]["codeAction"]["codeActionLiteralSupport"]["codeActionKind"]["valueSet"]
            [0],
        "quickfix"
    );
    assert_eq!(capabilities["workspace"]["applyEdit"], false);
    assert_eq!(
        capabilities["workspace"]["workspaceEdit"]["documentChanges"],
        true
    );
}

#[test]
fn workspace_diagnostic_paths_normalize_absolute_and_relative_files() {
    let root = std::env::temp_dir().join("vapor-ide-workspace-path-test");
    let file = root.join("src/main.rs");
    assert_eq!(
        relative_workspace_path(&root, &file),
        Some(PathBuf::from("src/main.rs"))
    );
    assert_eq!(
        relative_workspace_path(&root, Path::new("src/main.rs")),
        Some(PathBuf::from("src/main.rs"))
    );
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

    assert!(super::semantic_view::semantic_tree_matches(
        &component, "vec3"
    ));
    assert!(super::semantic_view::semantic_tree_matches(
        &component,
        "derive(component)"
    ));
    assert!(super::semantic_view::semantic_tree_matches(
        &component,
        "component"
    ));
    assert!(super::semantic_view::semantic_tree_matches(
        &component,
        "serde(default)"
    ));
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
    assert!(
        symbols[0]
            .attributes
            .iter()
            .any(|attribute| attribute.contains("derive"))
    );
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
fn rust_source_has_a_local_semantic_outline_before_rust_analyzer_replies() {
    let source = "#[derive(bevy::prelude::Component)]\npub struct Ship {\n    pub id: u64,\n    velocity: Vec3,\n}\n\nimpl Ship {\n    pub fn thrust(&mut self, amount: f32) {}\n}\n";

    let parsed = super::rust_semantics::parse_local_rust_semantic_document(
        Path::new("src/components.rs"),
        source,
    );

    assert_eq!(parsed.objects.len(), 2);
    let ship = &parsed.objects[0];
    assert_eq!(ship.kind, SemanticObjectKind::Component);
    assert_eq!(ship.name, "Ship");
    assert_eq!(ship.selection_range.start.line, 1);
    let id = ship
        .children
        .iter()
        .find(|member| member.name == "id")
        .unwrap();
    assert_eq!(id.kind, SemanticObjectKind::Field);
    assert_eq!(id.detail.as_deref(), Some("u64"));
    assert_eq!(id.selection_range.start.line, 2);
    assert_eq!(id.id.path, ["Ship", "id"]);
    let implementation = &parsed.objects[1];
    let thrust = implementation
        .children
        .iter()
        .find(|member| member.name == "thrust")
        .unwrap();
    assert_eq!(thrust.kind, SemanticObjectKind::Method);
    assert_eq!(thrust.selection_range.start.line, 7);
    assert_eq!(thrust.id.path, ["impl Ship", "thrust"]);
    assert_eq!(
        thrust.children[0].id.path,
        ["impl Ship", "thrust", "self#0"]
    );
    assert!(!parsed.truncated);
}

#[test]
fn local_rust_outline_bounds_nested_members_by_the_document_budget() {
    let fields = (0..4_200)
        .map(|index| format!("field_{index}: u8"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("struct Large {{ {fields} }}");
    let parsed = super::rust_semantics::parse_local_rust_semantic_document(
        Path::new("src/types.rs"),
        &source,
    );

    assert_eq!(parsed.objects.len(), 1);
    assert_eq!(parsed.objects[0].children.len(), 4_095);
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
    let diagnostics =
        super::rust_semantics::convention_diagnostics(Path::new("src/engine.rs"), &misplaced);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].code.as_deref(),
        Some("vapor.rust.layout.component")
    );
    assert_eq!(diagnostics[0].primary.as_ref().unwrap().start_line, 2);

    let canonical = parse_semantic_symbols(&response, Path::new("src/components.rs"), 1, source);
    assert!(
        super::rust_semantics::convention_diagnostics(Path::new("src/components.rs"), &canonical,)
            .is_empty()
    );
}

#[test]
fn rust_mod_labels_disambiguate_same_named_file_and_module() {
    let paths = [
        PathBuf::from("crates/flight/src/game.rs"),
        PathBuf::from("crates/flight/src/game/mod.rs"),
    ];
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
    let diagnostics =
        super::rust_semantics::convention_diagnostics(Path::new("src/engine.rs"), &objects);
    let codes = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        codes,
        [
            "vapor.rust.layout.type",
            "vapor.rust.layout.function",
            "vapor.rust.layout.constant",
            "vapor.rust.layout.static",
        ]
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message.contains("belongs in"))
    );
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
    let symbols =
        super::rust_semantics::parse_workspace_symbols(&response, Path::new("/fixture/workspace"));

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
        (
            PathBuf::from("src/lib.rs"),
            Document {
                contents: String::new(),
                fingerprint: String::new(),
                saved_contents: String::new(),
                version: 4,
            },
        ),
        (
            PathBuf::from("src/game.rs"),
            Document {
                contents: String::new(),
                fingerprint: String::new(),
                saved_contents: String::new(),
                version: 9,
            },
        ),
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
fn quick_fix_results_require_the_origin_document_and_full_workspace_snapshot() {
    let source = PathBuf::from("src/lib.rs");
    let mut documents = BTreeMap::from([
        (
            source.clone(),
            Document {
                contents: String::new(),
                fingerprint: String::new(),
                saved_contents: String::new(),
                version: 4,
            },
        ),
        (
            PathBuf::from("src/game.rs"),
            Document {
                contents: String::new(),
                fingerprint: String::new(),
                saved_contents: String::new(),
                version: 9,
            },
        ),
    ]);
    let request = super::PendingLspRequest {
        request: super::LspRequest::CodeActions,
        path: source.clone(),
        document_version: 4,
        semantic_target: None,
        cursor_offset: None,
        workspace_versions: Some(BTreeMap::from([
            (source.clone(), 4),
            (PathBuf::from("src/game.rs"), 9),
        ])),
        query: None,
    };
    assert!(pending_request_snapshot_is_current(&request, &documents));

    documents.get_mut(&source).unwrap().version += 1;
    assert!(!pending_request_snapshot_is_current(&request, &documents));
    documents.get_mut(&source).unwrap().version = 4;
    documents.remove(Path::new("src/game.rs"));
    assert!(!pending_request_snapshot_is_current(&request, &documents));
}

#[test]
fn toml_semantics_preserve_nested_source_ranges_and_utf16_positions() {
    let source = "title = \"λ\"\n\n[[root.content]]\nid = \"ghf/example\"\nversion = \"0.1.0\"\n";
    let objects = super::toml_semantics::parse_toml_semantic_objects(
        Path::new("Workspace.vapor.toml"),
        source,
    )
    .unwrap();
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
    let objects = super::toml_semantics::parse_toml_semantic_objects(
        Path::new("Packagepack.vapor.toml"),
        source,
    )
    .expect("valid Vapor TOML");
    let pack = &objects[0];
    assert_eq!(pack.id.domain, "vapor-packagepack");
    let name = pack
        .children
        .iter()
        .find(|child| child.name == "name")
        .unwrap();
    assert_eq!(name.attributes, ["Declared local identity"]);
    let game = pack
        .children
        .iter()
        .find(|child| child.name == "game")
        .unwrap();
    assert_eq!(game.name, "game");
    assert_eq!(game.children[0].attributes, ["Vapor content reference"]);
    assert_eq!(game.children[0].id.qualified_name(), "packagepack.game.id");
}

#[test]
fn toml_semantics_index_inline_tables_and_array_entries_without_values() {
    let source = "items = [{ id = \"private-value\", flags = [\"a\", \"b\"] }]\n";
    let objects = super::toml_semantics::parse_toml_semantic_objects(
        Path::new("Packagepack.vapor.toml"),
        source,
    )
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
    assert_eq!(
        shortest_rust_label(&candidates[0], &candidates),
        "engine/game"
    );
    assert_eq!(
        shortest_rust_label(&candidates[1], &candidates),
        "editor/game"
    );
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
        .map(|_| {
            serde_json::json!({
                "uri": "file:///workspace/src/lib.rs",
                "range": {"start": {"line": 4, "character": 8}}
            })
        })
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
    assert_eq!(
        position_to_byte_offset(source, 1, 2),
        Some("aλ🦀\r\nne".len())
    );
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
            start: vapor_gui_core::semantic::SemanticPosition {
                line: 0,
                character: 0,
            },
            end: vapor_gui_core::semantic::SemanticPosition {
                line: 2,
                character: 1,
            },
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
    assert_eq!(
        diagnostic.code.as_deref(),
        Some("vapor.semantic.source-limit")
    );
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

    assert_eq!(document_tab_label(Path::new("src/a/mod.rs"), &open), "a");
    assert_eq!(document_tab_label(Path::new("src/b/mod.rs"), &open), "b");
    assert_eq!(document_tab_label(Path::new("src/main.rs"), &open), "main");
}
#[test]
fn canonical_rust_item_templates_follow_the_selected_module() {
    let source = "//! Components for the game.\n";
    let rendered = render_canonical_rust_item(Path::new("src/components.rs"), source, "Ship")
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
    assert!(
        render_canonical_rust_item(
            Path::new("src/functions.rs"),
            "pub const run: &str = \"\";",
            "run"
        )
        .is_err()
    );
    assert!(
        render_canonical_rust_item(Path::new("src/functions.rs"), "not rust", "new_item").is_err()
    );
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
        (
            "src/run_conditions.rs",
            "is_ready",
            "pub fn is_ready() -> bool",
        ),
        ("src/constants.rs", "TITLE", "pub const TITLE: &str"),
        (
            "src/statics.rs",
            "DEFAULT_TITLE",
            "pub static DEFAULT_TITLE: &str",
        ),
    ] {
        let rendered = render_canonical_rust_item(Path::new(path), "", name).unwrap();
        assert!(
            rendered.contains(expected),
            "{path} should use its own template"
        );
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

#[test]
fn diagnostic_details_show_typed_context_without_losing_raw_payload() {
    let diagnostic = vapor_gui_core::StructuredDiagnostic {
        origin: vapor_gui_core::DiagnosticOrigin::Cargo,
        severity: vapor_gui_core::DiagnosticSeverity::Warning,
        code: Some("rust.test".to_owned()),
        message: "test warning".to_owned(),
        rendered: None,
        primary: None,
        related: Vec::new(),
        raw_payload: "opaque payload".to_owned(),
        context: vapor_gui_core::DiagnosticContext {
            domain: Some("rust".to_owned()),
            scope: vapor_gui_core::DiagnosticScope::SemanticObject,
            workspace: Some("/workspace/game".to_owned()),
            document_version: Some(12),
            semantic_path: Some("game::Ship::velocity".to_owned()),
            operation_id: Some("check-4".to_owned()),
        },
    };

    let details = super::diagnostics::diagnostic_details(&diagnostic);

    assert!(details.contains("Domain: rust"));
    assert!(details.contains("Scope: semantic-object"));
    assert!(details.contains("Workspace: /workspace/game"));
    assert!(details.contains("Document revision: 12"));
    assert!(details.contains("Semantic object: game::Ship::velocity"));
    assert!(details.contains("Operation: check-4"));
    assert!(details.contains("Raw diagnostic\nopaque payload"));
}

#[test]
fn source_diagnostic_context_adds_workspace_revision_without_erasing_domain_scope() {
    let document = vapor_gui_core::StructuredDiagnostic {
        origin: vapor_gui_core::DiagnosticOrigin::Vapor,
        severity: vapor_gui_core::DiagnosticSeverity::Warning,
        code: Some("vapor.toml.shape".to_owned()),
        message: "missing section".to_owned(),
        rendered: None,
        primary: None,
        related: Vec::new(),
        raw_payload: String::new(),
        context: vapor_gui_core::DiagnosticContext {
            domain: Some("vapor-packagepack".to_owned()),
            scope: vapor_gui_core::DiagnosticScope::Document,
            semantic_path: Some("packagepack.dependencies".to_owned()),
            ..Default::default()
        },
    };
    let unspecified = vapor_gui_core::StructuredDiagnostic {
        origin: vapor_gui_core::DiagnosticOrigin::Vapor,
        severity: vapor_gui_core::DiagnosticSeverity::Information,
        code: None,
        message: "semantic indexing paused".to_owned(),
        rendered: None,
        primary: None,
        related: Vec::new(),
        raw_payload: String::new(),
        context: Default::default(),
    };

    let mut diagnostics = vec![document, unspecified];
    super::diagnostics::attach_document_context(&mut diagnostics, Path::new("/workspace/game"), 12);

    assert_eq!(
        diagnostics[0].context.domain.as_deref(),
        Some("vapor-packagepack")
    );
    assert_eq!(
        diagnostics[0].context.scope,
        vapor_gui_core::DiagnosticScope::Document
    );
    assert_eq!(
        diagnostics[0].context.semantic_path.as_deref(),
        Some("packagepack.dependencies")
    );
    assert_eq!(
        diagnostics[0].context.workspace.as_deref(),
        Some("/workspace/game")
    );
    assert_eq!(diagnostics[0].context.document_version, Some(12));
    assert_eq!(
        diagnostics[1].context.scope,
        vapor_gui_core::DiagnosticScope::Document
    );
    assert_eq!(diagnostics[1].context.document_version, Some(12));
}

#[test]
fn rust_quick_fixes_keep_only_direct_edit_actions_with_titles() {
    let actions = super::language_server::parse_edit_code_actions(&serde_json::json!([
        {"title": "Add derive", "kind": "quickfix", "edit": {"changes": {}}},
        {"title": "Run command", "command": {"command": "rust-analyzer.run"}},
        {"kind": "quickfix", "edit": {"changes": {}}},
        {"title": "Edit and run", "edit": {}, "command": {"command": "unsafe"}}
    ]));

    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].title, "Add derive");
}
