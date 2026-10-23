use std::path::Path;

use vapor_gui_core::semantic::{SemanticObject, SemanticObjectId, SemanticObjectKind, semantic_range_from_byte_span};

#[cfg(test)]
mod tests {
    use super::{authored_manifest_diagnostics, parse_toml_semantic_objects};
    use std::path::Path;
    use vapor_gui_core::semantic::SemanticObject;

    fn find<'a>(objects: &'a [SemanticObject], qualified_name: &str) -> Option<&'a SemanticObject> {
        objects.iter().find_map(|object| {
            (object.id.qualified_name() == qualified_name)
                .then_some(object)
                .or_else(|| find(&object.children, qualified_name))
        })
    }

    #[test]
    fn packagepack_projection_labels_vapor_and_steam_fields_by_role() {
        let source = r#"
schema = 1

[packagepack]
name = "demo"
version.workspace = true
binaries = ["demo-bin"]

[packagepack.engine]
id = "ghf-studios/demo/engine"
version = "^0.5"
workshop-id = "42"

[packagepack.steam]
app-id = 1
published-file-id = "99"
visibility = "public"
"#;
        let objects = parse_toml_semantic_objects(Path::new("Packagepack.vapor.toml"), source)
            .expect("valid packagepack TOML");

        assert!(find(&objects, "packagepack.engine.id")
            .unwrap()
            .attributes
            .contains(&"Vapor content reference".to_owned()));
        assert!(find(&objects, "packagepack.engine.version")
            .unwrap()
            .attributes
            .contains(&"Vapor version requirement".to_owned()));
        assert!(find(&objects, "packagepack.engine.workshop-id")
            .unwrap()
            .attributes
            .contains(&"Workshop item identity".to_owned()));
        assert!(find(&objects, "packagepack.steam.published-file-id")
            .unwrap()
            .attributes
            .contains(&"Workshop item identity".to_owned()));
        assert!(find(&objects, "packagepack.steam.app-id")
            .unwrap()
            .attributes
            .contains(&"Steam application identity".to_owned()));
        assert!(find(&objects, "packagepack.binaries")
            .unwrap()
            .attributes
            .contains(&"Runtime executable names".to_owned()));
    }

    #[test]
    fn cargo_manifest_projection_identifies_package_dependencies_and_features() {
        let source = r#"
[package]
name = "demo-game"
version = "0.1.0"
edition = "2024"

[dependencies.bevy]
version = "0.17"
features = ["bevy_asset"]

[features]
default = []
"#;
        let objects = parse_toml_semantic_objects(Path::new("Cargo.toml"), source)
            .expect("valid Cargo manifest");
        assert!(find(&objects, "package.name").unwrap().attributes.contains(&"Rust package identity".to_owned()));
        assert!(find(&objects, "package.version").unwrap().attributes.contains(&"Rust package version".to_owned()));
        assert!(find(&objects, "dependencies.bevy.version").unwrap().attributes.contains(&"Cargo dependency version".to_owned()));
        assert!(find(&objects, "features.default").unwrap().attributes.contains(&"Cargo feature configuration".to_owned()));
    }

    #[test]
    fn vapor_workspace_projection_identifies_registration_and_toolchain_fields() {
        let source = r#"
schema = 1

[workspace]
name = "sample"
organization = "ghf-studios"
version = "0.5.0"
binaries = ["sample"]

[toolchain]
channel = "stable"
version = "1.97.0"

[[project]]
name = "sample-game"
path = "content/sample-game"
"#;
        let objects = parse_toml_semantic_objects(Path::new("Workspace.vapor.toml"), source)
            .expect("valid Vapor workspace TOML");
        assert!(find(&objects, "workspace.organization").unwrap().attributes.contains(&"Vapor identity namespace".to_owned()));
        assert!(find(&objects, "workspace.binaries").unwrap().attributes.contains(&"Workspace runtime executables".to_owned()));
        assert!(find(&objects, "toolchain.version").unwrap().attributes.contains(&"Pinned Rust toolchain version".to_owned()));
        assert!(find(&objects, "project[0].path").unwrap().attributes.contains(&"Registered workspace project path".to_owned()));
    }

    #[test]
    fn large_toml_arrays_and_nesting_obey_the_shared_tree_limits() {
        let dense = format!("values = [{}]", (0..5_000).map(|value| value.to_string()).collect::<Vec<_>>().join(","));
        let objects = parse_toml_semantic_objects(Path::new("dense-array.toml"), &dense)
            .expect("valid large array");
        assert!(semantic_object_count(&objects) <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
        assert!(objects[0].attributes.iter().any(|attribute| attribute.contains("outline truncated")));

        let nested = format!("value = {}0{}", "[".repeat(40), "]".repeat(40));
        let objects = parse_toml_semantic_objects(Path::new("nested.toml"), &nested)
            .expect("valid nested arrays within TOML's parser limit");
        assert!(semantic_object_count(&objects) <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
    }

    fn semantic_object_count(objects: &[SemanticObject]) -> usize {
        objects.iter().map(|object| 1 + semantic_object_count(&object.children)).sum()
    }

    #[test]
    fn dense_toml_projection_is_bounded_and_marks_the_partial_outline() {
        let source = (0..5_000)
            .map(|index| format!("key_{index} = {index}\n"))
            .collect::<String>();
        let objects = parse_toml_semantic_objects(Path::new("dense.toml"), &source)
            .expect("valid dense TOML");
        assert!(objects.len() <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
        assert!(objects[0]
            .attributes
            .iter()
            .any(|attribute| attribute.contains("outline truncated")));
    }

    #[test]
    fn authored_content_schema_checks_manifest_role_and_hides_invalid_values() {
        let missing_role = authored_manifest_diagnostics(
            Path::new("Packagepack.vapor.toml"),
            "[game]\nname = \"sample\"\n",
        );
        assert_eq!(missing_role.len(), 1);
        assert_eq!(missing_role[0].code.as_deref(), Some("vapor.manifest.role"));

        let malformed = authored_manifest_diagnostics(
            Path::new("Packagepack.vapor.toml"),
            "[packagepack]\nname = 17\nversion = \"private-marker\"\n",
        );
        assert_eq!(malformed.len(), 1);
        assert_eq!(malformed[0].code.as_deref(), Some("vapor.manifest.shape"));
        assert!(!malformed[0].raw_payload.contains("private-marker"));

        let valid = authored_manifest_diagnostics(
            Path::new("Packagepack.vapor.toml"),
            "[packagepack]\nname = \"demo\"\nversion.workspace = true\n",
        );
        assert!(valid.is_empty());
    }
}

pub fn parse_toml_semantic_objects(
    document: &Path,
    source: &str,
) -> Result<Vec<SemanticObject>, toml_edit::TomlError> {
    let domain = toml_document_domain(document);
    let parsed = source.parse::<toml_edit::ImDocument<String>>()?;
    let mut budget = SemanticBudget::new();
    let mut objects = Vec::new();
    let mut entries = parsed.iter().peekable();
    while let Some((name, item)) = entries.next() {
        let Some(key) = parsed.as_table().key(name) else { continue; };
        if let Some(object) = toml_item_semantic_object(
            &domain, document, source, &[], key.get(), key.span(), item, 0, &mut budget,
        ) {
            objects.push(object);
        }
        if budget.remaining == 0 && entries.peek().is_some() {
            budget.truncated = true;
            break;
        }
    }
    if budget.truncated {
        vapor_gui_core::semantic::mark_semantic_outline_truncated(&mut objects);
    }
    Ok(objects)
}

fn toml_document_domain(document: &Path) -> String {
    let Some(file_name) = document.file_name().and_then(|name| name.to_str()) else {
        return "toml".to_owned();
    };
    if let Some(kind) = vapor_client_domain::ContentKind::ALL
        .into_iter()
        .find(|kind| kind.manifest_file_name() == file_name)
    {
        return format!("vapor-{}", kind.as_str());
    }
    match file_name {
        "Cargo.toml" => "cargo",
        "Workspace.vapor.toml" => "vapor-workspace",
        "App-Source.vapor.toml" => "vapor-app-source",
        "App.vapor.toml" => "vapor-app",
        "Registry.vapor.toml" => "vapor-registry",
        _ => "toml",
    }
    .to_owned()
}

/// Validate the typed shape of a canonical source-authored content manifest.
///
/// The shared `vapor_content` model owns the manifest's meaning. This adapter
/// adds source-oriented diagnostics without copying or displaying TOML values.
pub fn authored_manifest_diagnostics(
    document: &Path,
    source: &str,
) -> Vec<vapor_gui_core::StructuredDiagnostic> {
    use vapor_client_domain::{AuthoredContentManifest, ContentKind};

    let Some(kind) = ContentKind::ALL.into_iter().find(|kind| {
        document.file_name().and_then(|name| name.to_str()) == Some(kind.manifest_file_name())
    }) else {
        return Vec::new();
    };
    let manifest = match AuthoredContentManifest::parse(source) {
        Ok(manifest) => manifest,
        Err(error) => {
            let primary = error.span().map(|span| {
                let range = semantic_range_from_byte_span(source, span);
                vapor_gui_core::DiagnosticSpan {
                    file: document.to_string_lossy().into_owned(),
                    start_line: range.start.line.saturating_add(1),
                    start_column: range.start.character.saturating_add(1),
                    end_line: range.end.line.saturating_add(1),
                    end_column: range.end.character.saturating_add(1),
                    primary: true,
                    label: Some("Vapor manifest field type".to_owned()),
                }
            });
            return vec![vapor_gui_core::StructuredDiagnostic {
                origin: vapor_gui_core::DiagnosticOrigin::Vapor,
                severity: vapor_gui_core::DiagnosticSeverity::Error,
                code: Some("vapor.manifest.shape".to_owned()),
                message: format!(
                    "This {kind} manifest does not match the shared authored-content schema."
                ),
                rendered: None,
                primary,
                related: Vec::new(),
                raw_payload: format!("{kind} manifest schema mismatch"),
            }];
        }
    };
    if manifest.content(kind).is_some() {
        return Vec::new();
    }
    vec![vapor_gui_core::StructuredDiagnostic {
        origin: vapor_gui_core::DiagnosticOrigin::Vapor,
        severity: vapor_gui_core::DiagnosticSeverity::Error,
        code: Some("vapor.manifest.role".to_owned()),
        message: format!("Expected a [{}] declaration in this {kind} manifest.", kind),
        rendered: None,
        primary: None,
        related: Vec::new(),
        raw_payload: format!("missing canonical {kind} section"),
    }]
}

#[derive(Default)]
struct SemanticBudget {
    remaining: usize,
    truncated: bool,
}

impl SemanticBudget {
    fn new() -> Self {
        Self { remaining: vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS, truncated: false }
    }

    fn take(&mut self, depth: usize) -> bool {
        if depth >= vapor_gui_core::semantic::MAX_SEMANTIC_DEPTH || self.remaining == 0 {
            self.truncated = true;
            return false;
        }
        self.remaining -= 1;
        true
    }
}


fn toml_item_semantic_object(
    domain: &str,
    document: &Path,
    source: &str,
    parent_path: &[String],
    name: &str,
    key_span: Option<std::ops::Range<usize>>,
    item: &toml_edit::Item,
    depth: usize,
    budget: &mut SemanticBudget,
) -> Option<SemanticObject> {
    if !budget.take(depth) { return None; }
    let source_span = item.span().or(key_span.clone())?;
    let mut path = parent_path.to_vec();
    path.push(name.to_owned());
    let mut children = Vec::new();
    let kind = if let Some(table) = item.as_table() {
        for (name, child) in table.iter() {
            if budget.remaining == 0 { budget.truncated = true; break; }
            let Some(key) = table.key(name) else { continue; };
            if let Some(child) = toml_item_semantic_object(
                domain, document, source, &path, key.get(), key.span(), child, depth + 1, budget,
            ) { children.push(child); }
        }
        SemanticObjectKind::Configuration
    } else if let Some(tables) = item.as_array_of_tables() {
        for (index, table) in tables.iter().enumerate() {
            if !budget.take(depth + 1) { break; }
            let entry_name = format!("[{index}]");
            let mut entry_path = path.clone();
            entry_path.push(entry_name.clone());
            let Some(span) = table.span() else { continue; };
            let mut members = Vec::new();
            for (name, child) in table.iter() {
                if budget.remaining == 0 { budget.truncated = true; break; }
                let Some(key) = table.key(name) else { continue; };
                if let Some(child) = toml_item_semantic_object(
                    domain, document, source, &entry_path, key.get(), key.span(), child, depth + 2, budget,
                ) { members.push(child); }
            }
            children.push(toml_semantic_node(
                domain, document, source, entry_path, entry_name, span.clone(), span,
                Some("table".to_owned()), SemanticObjectKind::Configuration, members,
            ));
        }
        SemanticObjectKind::Configuration
    } else if let Some(value) = item.as_value() {
        toml_value_semantic_children(domain, document, source, &path, value, &mut children, depth + 1, budget);
        toml_value_semantic_kind(value)
    } else {
        SemanticObjectKind::Other
    };
    let detail = item.as_value().map(|value| {
        if value.is_array() {
            format!("array · {} items", value.as_array().map_or(0, |array| array.len()))
        } else {
            value.type_name().to_owned()
        }
    }).or_else(|| item.as_table().map(|_| "table".to_owned()))
      .or_else(|| item.as_array_of_tables().map(|tables| format!("array of tables · {} entries", tables.len())));
    Some(toml_semantic_node(
        domain, document, source, path, name.to_owned(), source_span,
        key_span.unwrap_or_else(|| item.span().unwrap_or_default()),
        detail, kind, children,
    ))
}

fn toml_value_semantic_children(
    domain: &str,
    document: &Path,
    source: &str,
    parent_path: &[String],
    value: &toml_edit::Value,
    children: &mut Vec<SemanticObject>,
    depth: usize,
    budget: &mut SemanticBudget,
) {
    if let Some(array) = value.as_array() {
        for (index, value) in array.iter().enumerate() {
            if !budget.take(depth) { break; }
            let name = format!("[{index}]");
            let Some(span) = value.span() else { continue; };
            let mut path = parent_path.to_vec();
            path.push(name.clone());
            let mut nested = Vec::new();
            toml_value_semantic_children(domain, document, source, &path, value, &mut nested, depth + 1, budget);
            children.push(toml_semantic_node(
                domain, document, source, path, name, span.clone(), span,
                Some(value.type_name().to_owned()), toml_value_semantic_kind(value), nested,
            ));
        }
    } else if let Some(table) = value.as_inline_table() {
        for (key_name, _) in table.iter() {
            if budget.remaining == 0 { budget.truncated = true; break; }
            if let Some((key, item)) = table.get_key_value(key_name)
                && let Some(object) = toml_item_semantic_object(
                    domain, document, source, parent_path, key.get(), key.span(), item, depth, budget,
                )
            {
                children.push(object);
            }
        }
    }
}

fn toml_value_semantic_kind(value: &toml_edit::Value) -> SemanticObjectKind {
    if value.is_inline_table() {
        SemanticObjectKind::Configuration
    } else if value.is_array() {
        SemanticObjectKind::Array
    } else {
        SemanticObjectKind::Other
    }
}

fn toml_semantic_node(
    domain: &str,
    document: &Path,
    source: &str,
    path: Vec<String>,
    name: String,
    source_span: std::ops::Range<usize>,
    selection_span: std::ops::Range<usize>,
    detail: Option<String>,
    kind: SemanticObjectKind,
    children: Vec<SemanticObject>,
) -> SemanticObject {
    let range = semantic_range_from_byte_span(source, source_span);
    let selection_range = semantic_range_from_byte_span(source, selection_span);
    let discriminator = format!(
        "{}:{}:{}:{}",
        range.start.line, range.start.character, range.end.line, range.end.character
    );
    let attributes = toml_semantic_attributes(domain, &path);
    SemanticObject {
        id: SemanticObjectId::with_discriminator(domain, document, path, discriminator),
        kind,
        name,
        detail,
        range,
        selection_range,
        attributes,
        children,
    }
}

fn toml_semantic_attributes(domain: &str, path: &[String]) -> Vec<String> {
    let attribute = match domain {
        "cargo" => cargo_semantic_attribute(path),
        "vapor-workspace" => workspace_semantic_attribute(path),
        domain if domain.starts_with("vapor-") => authored_vapor_semantic_attribute(path),
        _ => None,
    };
    attribute.map(str::to_owned).into_iter().collect()
}

fn path_is(path: &[String], expected: &[&str]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == expected)
}

fn path_starts_with(path: &[String], prefix: &[&str]) -> bool {
    path.len() >= prefix.len()
        && path
            .iter()
            .zip(prefix)
            .all(|(actual, expected)| actual == expected)
}

fn cargo_semantic_attribute(path: &[String]) -> Option<&'static str> {
    let name = path.last()?.as_str();
    if path.first().is_some_and(|segment| segment == "package") {
        return match name {
            "name" => Some("Rust package identity"),
            "version" => Some("Rust package version"),
            "edition" => Some("Rust language edition"),
            "rust-version" => Some("Minimum Rust toolchain version"),
            _ => None,
        };
    }
    if path.iter().any(|segment| {
        matches!(segment.as_str(), "dependencies" | "dev-dependencies" | "build-dependencies")
    }) {
        return match name {
            "version" => Some("Cargo dependency version"),
            "features" => Some("Cargo dependency features"),
            "optional" => Some("Optional Cargo dependency"),
            "default-features" => Some("Cargo default-feature policy"),
            "dependencies" | "dev-dependencies" | "build-dependencies" => None,
            _ => Some("Cargo dependency declaration"),
        };
    }
    if path.iter().any(|segment| segment == "features") {
        return Some("Cargo feature configuration");
    }
    if path_starts_with(path, &["workspace", "members"]) {
        return Some("Cargo workspace member");
    }
    if path_is(path, &["lib", "name"]) {
        return Some("Rust library target name");
    }
    if path_is(path, &["lib", "path"]) {
        return Some("Rust library source path");
    }
    if path.len() == 3 && path_starts_with(path, &["bin"]) {
        return match name {
            "name" => Some("Rust binary target name"),
            "path" => Some("Rust binary source path"),
            _ => None,
        };
    }
    None
}

fn workspace_semantic_attribute(path: &[String]) -> Option<&'static str> {
    if path_is(path, &["schema"]) {
        return Some("Vapor workspace schema");
    }
    if path_is(path, &["workspace", "name"]) {
        return Some("Workspace identity name");
    }
    if path_is(path, &["workspace", "organization"]) {
        return Some("Vapor identity namespace");
    }
    if path_is(path, &["workspace", "version"]) {
        return Some("Workspace content version");
    }
    if path_is(path, &["workspace", "repository"]) {
        return Some("Workspace source repository");
    }
    if path_is(path, &["workspace", "binaries"]) {
        return Some("Workspace runtime executables");
    }
    if path_is(path, &["workspace", "runtime", "targets"]) {
        return Some("Supported runtime targets");
    }
    if path_starts_with(path, &["workspace", "runtime", "libraries"]) {
        return Some("Runtime platform library");
    }
    if path_is(path, &["toolchain", "channel"]) {
        return Some("Pinned Rust toolchain channel");
    }
    if path_is(path, &["toolchain", "version"]) {
        return Some("Pinned Rust toolchain version");
    }
    if path_is(path, &["toolchain", "date"]) {
        return Some("Pinned Rust toolchain date");
    }
    if path.len() == 3
        && path.first().is_some_and(|segment| segment == "project")
        && path.get(1).is_some_and(|segment| segment.starts_with('['))
    {
        return match path.last().map(String::as_str) {
            Some("name") => Some("Registered workspace project name"),
            Some("path") => Some("Registered workspace project path"),
            Some("schema") => Some("Workspace project schema"),
            _ => None,
        };
    }
    None
}

fn authored_vapor_semantic_attribute(path: &[String]) -> Option<&'static str> {
    let name = path.last()?.as_str();
    let is_content_reference_version = name == "version"
        && path[..path.len().saturating_sub(1)].iter().any(|segment| {
            matches!(segment.as_str(), "engine" | "game" | "dependencies" | "extends")
        });
    match name {
        "schema" => Some("Vapor manifest schema"),
        "organization" => Some("Identity namespace"),
        "name" if path.len() == 2 => Some("Declared local identity"),
        "version" if is_content_reference_version => Some("Vapor version requirement"),
        "version" => Some("Declared version"),
        "workshop-id" | "published-file-id" => Some("Workshop item identity"),
        "app-id" => Some("Steam application identity"),
        "binaries" => Some("Runtime executable names"),
        "libraries" => Some("Static Rust library artifacts"),
        "id" if path.iter().any(|segment| {
            matches!(
                segment.as_str(),
                "engine" | "game" | "enginepack" | "gamepack" | "modpack"
                    | "packagepack" | "engine-mod" | "game-mod" | "extension-mod"
            )
        }) => Some("Vapor content reference"),
        _ => None,
    }
}
