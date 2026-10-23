use super::diagnostics::truncate_diagnostic_payload;
use std::path::Path;

use vapor_gui_core::semantic::{
    SemanticObject, SemanticObjectId, SemanticObjectKind, semantic_range_from_byte_span,
};

pub fn parse_toml_semantic_objects(
    document: &Path,
    source: &str,
) -> Result<Vec<SemanticObject>, toml_edit::TomlError> {
    let parsed = source.parse::<toml_edit::ImDocument<String>>()?;
    let mut projection = TomlSemanticProjection::new(document, source);
    let mut objects = Vec::new();
    let mut entries = parsed.iter().peekable();
    while let Some((name, item)) = entries.next() {
        let Some(key) = parsed.as_table().key(name) else {
            continue;
        };
        if let Some(object) = projection.item(&[], key.get(), key.span(), item, 0) {
            objects.push(object);
        }
        if projection.budget.remaining == 0 && entries.peek().is_some() {
            projection.budget.truncated = true;
            break;
        }
    }
    if projection.budget.truncated {
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
                context: vapor_gui_core::DiagnosticContext {
                    domain: Some(toml_document_domain(document)),
                    scope: vapor_gui_core::DiagnosticScope::Document,
                    ..Default::default()
                },
            }];
        }
    };
    if manifest.content(kind).is_some() {
        let Some(other_kind) = ContentKind::ALL
            .into_iter()
            .find(|candidate| *candidate != kind && manifest.content(*candidate).is_some())
        else {
            return Vec::new();
        };
        let primary = authored_manifest_section_span(source, other_kind).map(|span| {
            let range = semantic_range_from_byte_span(source, span);
            vapor_gui_core::DiagnosticSpan {
                file: document.to_string_lossy().into_owned(),
                start_line: range.start.line.saturating_add(1),
                start_column: range.start.character.saturating_add(1),
                end_line: range.end.line.saturating_add(1),
                end_column: range.end.character.saturating_add(1),
                primary: true,
                label: Some("Unexpected content role".to_owned()),
            }
        });
        return vec![vapor_gui_core::StructuredDiagnostic {
            origin: vapor_gui_core::DiagnosticOrigin::Vapor,
            severity: vapor_gui_core::DiagnosticSeverity::Error,
            code: Some("vapor.manifest.multiple-roles".to_owned()),
            message: format!(
                "This {kind} manifest also declares [{other_kind}]. Keep each content role in its own manifest."
            ),
            rendered: None,
            primary,
            related: Vec::new(),
            raw_payload: format!("unexpected {other_kind} role in {kind} manifest"),
            context: vapor_gui_core::DiagnosticContext {
                domain: Some(toml_document_domain(document)),
                scope: vapor_gui_core::DiagnosticScope::Document,
                ..Default::default()
            },
        }];
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
        context: vapor_gui_core::DiagnosticContext {
            domain: Some(toml_document_domain(document)),
            scope: vapor_gui_core::DiagnosticScope::Document,
            ..Default::default()
        },
    }]
}

fn authored_manifest_section_span(
    source: &str,
    kind: vapor_client_domain::ContentKind,
) -> Option<std::ops::Range<usize>> {
    let document = source.parse::<toml_edit::ImDocument<String>>().ok()?;
    document.as_table().get(kind.as_str())?.span()
}

/// Resolve a registered project path from the authoritative open workspace
/// document when the selected semantic object is its `project[n].path` field.
/// The value is intentionally read on demand rather than copied into the
/// semantic index.
pub(super) fn registered_workspace_project_path(
    source: &str,
    id: &vapor_gui_core::semantic::SemanticObjectId,
) -> Option<std::path::PathBuf> {
    let (collection, index) = registered_project_index(id)?;
    let document = source.parse::<toml_edit::ImDocument<String>>().ok()?;
    let projects = match collection {
        WorkspaceProjectCollection::Source => document.get("project")?.as_array_of_tables()?,
        WorkspaceProjectCollection::Content => document
            .get("workspace")?
            .get("projects")?
            .as_array_of_tables()?,
    };
    let path = projects.get(index)?.get("path")?.as_str()?;
    Some(std::path::PathBuf::from(path))
}

pub(super) fn is_registered_workspace_project_path(
    id: &vapor_gui_core::semantic::SemanticObjectId,
) -> bool {
    registered_project_index(id).is_some()
}

pub(super) fn authored_reference_id(
    source: &str,
    id: &vapor_gui_core::semantic::SemanticObjectId,
) -> Option<String> {
    let role = id.domain.strip_prefix("vapor-")?;
    let fields = id.path.strip_prefix(&[role.to_owned()])?;
    if !vapor_client_domain::ContentKind::ALL
        .into_iter()
        .any(|kind| kind.as_str() == role)
        || !is_authored_reference_field(fields, "id")
    {
        return None;
    }
    let document = source.parse::<toml_edit::ImDocument<String>>().ok()?;
    let content = document.get(role)?;
    let id = match fields {
        [relationship, field] if field == "id" => {
            content.get(relationship)?.get(field)?.as_str()?
        }
        [collection, index, field] if field == "id" => content
            .get(collection)?
            .as_array_of_tables()?
            .get(array_index(index)?)?
            .get(field)?
            .as_str()?,
        _ => return None,
    };
    Some(id.to_owned())
}

fn array_index(segment: &str) -> Option<usize> {
    segment
        .strip_prefix('[')?
        .strip_suffix(']')?
        .parse::<usize>()
        .ok()
}

#[derive(Clone, Copy)]
enum WorkspaceProjectCollection {
    Source,
    Content,
}

fn registered_project_index(
    id: &vapor_gui_core::semantic::SemanticObjectId,
) -> Option<(WorkspaceProjectCollection, usize)> {
    if id.domain != "vapor-workspace" {
        return None;
    }
    let (collection, index) = match id.path.as_slice() {
        [project, index, field] if project == "project" && field == "path" => {
            (WorkspaceProjectCollection::Source, index)
        }
        [workspace, projects, index, field]
            if workspace == "workspace" && projects == "projects" && field == "path" =>
        {
            (WorkspaceProjectCollection::Content, index)
        }
        _ => return None,
    };
    let index = index
        .strip_prefix('[')?
        .strip_suffix(']')?
        .parse::<usize>()
        .ok()?;
    Some((collection, index))
}

#[derive(Default)]
struct SemanticBudget {
    remaining: usize,
    truncated: bool,
}

impl SemanticBudget {
    fn new() -> Self {
        Self {
            remaining: vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS,
            truncated: false,
        }
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

struct TomlSemanticProjection<'a> {
    domain: String,
    document: &'a Path,
    source: &'a str,
    budget: SemanticBudget,
}

#[derive(Clone)]
struct SemanticSpans {
    range: std::ops::Range<usize>,
    selection: std::ops::Range<usize>,
}

impl<'a> TomlSemanticProjection<'a> {
    fn new(document: &'a Path, source: &'a str) -> Self {
        Self {
            domain: toml_document_domain(document),
            document,
            source,
            budget: SemanticBudget::new(),
        }
    }

    fn item(
        &mut self,
        parent_path: &[String],
        name: &str,
        key_span: Option<std::ops::Range<usize>>,
        item: &toml_edit::Item,
        depth: usize,
    ) -> Option<SemanticObject> {
        if !self.budget.take(depth) {
            return None;
        }
        let source_span = item.span().or(key_span.clone())?;
        let mut path = parent_path.to_vec();
        path.push(name.to_owned());
        let mut children = Vec::new();
        let kind = if let Some(table) = item.as_table() {
            for (name, child) in table.iter() {
                if self.budget.remaining == 0 {
                    self.budget.truncated = true;
                    break;
                }
                let Some(key) = table.key(name) else {
                    continue;
                };
                if let Some(child) = self.item(&path, key.get(), key.span(), child, depth + 1) {
                    children.push(child);
                }
            }
            SemanticObjectKind::Configuration
        } else if let Some(tables) = item.as_array_of_tables() {
            for (index, table) in tables.iter().enumerate() {
                if !self.budget.take(depth + 1) {
                    break;
                }
                let entry_name = format!("[{index}]");
                let mut entry_path = path.clone();
                entry_path.push(entry_name.clone());
                let Some(span) = table.span() else {
                    continue;
                };
                let mut members = Vec::new();
                for (name, child) in table.iter() {
                    if self.budget.remaining == 0 {
                        self.budget.truncated = true;
                        break;
                    }
                    let Some(key) = table.key(name) else {
                        continue;
                    };
                    if let Some(child) =
                        self.item(&entry_path, key.get(), key.span(), child, depth + 2)
                    {
                        members.push(child);
                    }
                }
                children.push(self.node(
                    entry_path,
                    entry_name,
                    SemanticSpans {
                        range: span.clone(),
                        selection: span,
                    },
                    Some("table".to_owned()),
                    SemanticObjectKind::Configuration,
                    members,
                ));
            }
            SemanticObjectKind::Configuration
        } else if let Some(value) = item.as_value() {
            self.value_children(&path, value, &mut children, depth + 1);
            toml_value_semantic_kind(value)
        } else {
            SemanticObjectKind::Other
        };
        let detail = item
            .as_value()
            .map(|value| {
                if value.is_array() {
                    format!(
                        "array · {} items",
                        value.as_array().map_or(0, |array| array.len())
                    )
                } else {
                    value.type_name().to_owned()
                }
            })
            .or_else(|| item.as_table().map(|_| "table".to_owned()))
            .or_else(|| {
                item.as_array_of_tables()
                    .map(|tables| format!("array of tables · {} entries", tables.len()))
            });
        let selection_span = key_span.unwrap_or_else(|| item.span().unwrap_or_default());
        Some(self.node(
            path,
            name.to_owned(),
            SemanticSpans {
                range: source_span,
                selection: selection_span,
            },
            detail,
            kind,
            children,
        ))
    }

    fn value_children(
        &mut self,
        parent_path: &[String],
        value: &toml_edit::Value,
        children: &mut Vec<SemanticObject>,
        depth: usize,
    ) {
        if let Some(array) = value.as_array() {
            for (index, value) in array.iter().enumerate() {
                if !self.budget.take(depth) {
                    break;
                }
                let name = format!("[{index}]");
                let Some(span) = value.span() else {
                    continue;
                };
                let mut path = parent_path.to_vec();
                path.push(name.clone());
                let mut nested = Vec::new();
                self.value_children(&path, value, &mut nested, depth + 1);
                children.push(self.node(
                    path,
                    name,
                    SemanticSpans {
                        range: span.clone(),
                        selection: span,
                    },
                    Some(value.type_name().to_owned()),
                    toml_value_semantic_kind(value),
                    nested,
                ));
            }
        } else if let Some(table) = value.as_inline_table() {
            for (key_name, _) in table.iter() {
                if self.budget.remaining == 0 {
                    self.budget.truncated = true;
                    break;
                }
                if let Some((key, item)) = table.get_key_value(key_name)
                    && let Some(object) = self.item(parent_path, key.get(), key.span(), item, depth)
                {
                    children.push(object);
                }
            }
        }
    }

    fn node(
        &self,
        path: Vec<String>,
        name: String,
        spans: SemanticSpans,
        detail: Option<String>,
        kind: SemanticObjectKind,
        children: Vec<SemanticObject>,
    ) -> SemanticObject {
        let range = semantic_range_from_byte_span(self.source, spans.range);
        let selection_range = semantic_range_from_byte_span(self.source, spans.selection);
        let discriminator = format!(
            "{}:{}:{}:{}",
            range.start.line, range.start.character, range.end.line, range.end.character
        );
        let attributes = toml_semantic_attributes(&self.domain, &path);
        SemanticObject {
            id: SemanticObjectId::with_discriminator(
                &self.domain,
                self.document,
                path,
                discriminator,
            ),
            kind,
            name,
            detail,
            range,
            selection_range,
            attributes,
            children,
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

fn toml_semantic_attributes(domain: &str, path: &[String]) -> Vec<String> {
    let attribute = match domain {
        "cargo" => cargo_semantic_attribute(path),
        "vapor-workspace" => workspace_semantic_attribute(path),
        domain if domain.starts_with("vapor-") => authored_vapor_semantic_attribute(domain, path),
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
        matches!(
            segment.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        )
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
    if path.len() == 4
        && path_starts_with(path, &["workspace", "projects"])
        && path[2].starts_with('[')
        && path[3] == "path"
    {
        return Some("Registered workspace content project path");
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
            Some("name") => Some("Registered source project name"),
            Some("path") => Some("Registered source project path"),
            Some("schema") => Some("Source project schema"),
            _ => None,
        };
    }
    None
}

fn authored_vapor_semantic_attribute(domain: &str, path: &[String]) -> Option<&'static str> {
    let content_kind = domain.strip_prefix("vapor-")?;
    if fields_equal(path, &["schema"]) {
        return Some("Vapor manifest schema");
    }
    let (role, fields) = path.split_first()?;
    if role != content_kind
        || !vapor_client_domain::ContentKind::ALL
            .into_iter()
            .any(|kind| kind.as_str() == content_kind)
    {
        return None;
    }

    if fields.len() == 1 {
        return match fields[0].as_str() {
            "id" => Some("Vapor content identity"),
            "name" => Some("Content display name"),
            "version" => Some("Declared content version"),
            "binaries" => Some("Runtime executable names"),
            "libraries" => Some("Static Rust library artifacts"),
            "presentation" => Some("Runtime presentation mode"),
            "runtime" => Some("Runtime payloads"),
            _ => None,
        };
    }

    if fields_equal(fields, &["version", "workspace"]) {
        return Some("Inherit workspace content version");
    }
    if is_authored_reference_field(fields, "id") {
        return Some("Vapor content reference");
    }
    if is_authored_reference_field(fields, "version") {
        return Some("Vapor version requirement");
    }
    if is_authored_reference_field(fields, "workshop-id") {
        return Some("Workshop acquisition ID");
    }

    for (field, label) in [
        ("optional", "Optional content reference"),
        ("relationship", "Content relationship"),
        ("reason", "Content reference rationale"),
    ] {
        if is_authored_reference_field(fields, field) {
            if field == "optional" && fields.iter().any(|segment| segment == "dependencies") {
                return Some("Optional dependency");
            }
            if field == "optional" && fields.iter().any(|segment| segment == "conflicts") {
                return Some("Optional conflict");
            }
            if field == "reason" && fields.iter().any(|segment| segment == "dependencies") {
                return Some("Dependency explanation");
            }
            if field == "reason" && fields.iter().any(|segment| segment == "conflicts") {
                return Some("Conflict explanation");
            }
            return Some(label);
        }
    }
    if fields_equal(fields, &["steam", "app-id"]) {
        return Some("Steam application ID");
    }
    if fields_equal(fields, &["steam", "published-file-id"]) {
        return Some("Published Workshop item ID");
    }
    if fields_equal(fields, &["steam", "visibility"]) {
        return Some("Workshop visibility");
    }
    if fields.len() == 2
        && matches!(
            fields[0].as_str(),
            "engine" | "enginepack" | "game" | "gamepack"
        )
        && fields[1] == "version"
    {
        return Some("Vapor version requirement");
    }
    if fields.len() == 2
        && fields[0] == "steam"
        && matches!(
            fields[1].as_str(),
            "title" | "description" | "tags" | "change-note"
        )
    {
        return Some("Workshop publication metadata");
    }
    None
}

fn is_authored_reference_field(fields: &[String], field: &str) -> bool {
    match fields {
        [role, name] if name == field => {
            matches!(role.as_str(), "engine" | "enginepack" | "game" | "gamepack")
        }
        [collection, index, name]
            if matches!(collection.as_str(), "dependencies" | "conflicts")
                && name == field
                && is_array_index(index) =>
        {
            matches!(collection.as_str(), "dependencies" | "conflicts")
        }
        _ => false,
    }
}

fn fields_equal(fields: &[String], expected: &[&str]) -> bool {
    fields.len() == expected.len()
        && fields
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == expected)
}

fn is_array_index(segment: &str) -> bool {
    segment
        .strip_prefix('[')
        .and_then(|index| index.strip_suffix(']'))
        .is_some_and(|index| index.parse::<usize>().is_ok())
}

pub(super) fn toml_parse_diagnostic(
    path: &Path,
    source: &str,
    error: &toml_edit::TomlError,
) -> vapor_gui_core::StructuredDiagnostic {
    let primary = error.span().map(|span| {
        let range = semantic_range_from_byte_span(source, span);
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
        context: vapor_gui_core::DiagnosticContext {
            domain: Some(toml_document_domain(path)),
            scope: vapor_gui_core::DiagnosticScope::Document,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        authored_manifest_diagnostics, authored_reference_id, parse_toml_semantic_objects,
        registered_workspace_project_path,
    };
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

        assert!(
            find(&objects, "packagepack.engine.id")
                .unwrap()
                .attributes
                .contains(&"Vapor content reference".to_owned())
        );
        assert!(
            find(&objects, "packagepack.engine.version")
                .unwrap()
                .attributes
                .contains(&"Vapor version requirement".to_owned())
        );
        assert!(
            find(&objects, "packagepack.engine.workshop-id")
                .unwrap()
                .attributes
                .contains(&"Workshop acquisition ID".to_owned())
        );
        assert!(
            find(&objects, "packagepack.steam.published-file-id")
                .unwrap()
                .attributes
                .contains(&"Published Workshop item ID".to_owned())
        );
        assert!(
            find(&objects, "packagepack.steam.app-id")
                .unwrap()
                .attributes
                .contains(&"Steam application ID".to_owned())
        );
        assert!(
            find(&objects, "packagepack.binaries")
                .unwrap()
                .attributes
                .contains(&"Runtime executable names".to_owned())
        );
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
        assert!(
            find(&objects, "package.name")
                .unwrap()
                .attributes
                .contains(&"Rust package identity".to_owned())
        );
        assert!(
            find(&objects, "package.version")
                .unwrap()
                .attributes
                .contains(&"Rust package version".to_owned())
        );
        assert!(
            find(&objects, "dependencies.bevy.version")
                .unwrap()
                .attributes
                .contains(&"Cargo dependency version".to_owned())
        );
        assert!(
            find(&objects, "features.default")
                .unwrap()
                .attributes
                .contains(&"Cargo feature configuration".to_owned())
        );
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
        assert!(
            find(&objects, "workspace.organization")
                .unwrap()
                .attributes
                .contains(&"Vapor identity namespace".to_owned())
        );
        assert!(
            find(&objects, "workspace.binaries")
                .unwrap()
                .attributes
                .contains(&"Workspace runtime executables".to_owned())
        );
        assert!(
            find(&objects, "toolchain.version")
                .unwrap()
                .attributes
                .contains(&"Pinned Rust toolchain version".to_owned())
        );
        assert!(
            find(&objects, "project[0].path")
                .unwrap()
                .attributes
                .contains(&"Registered source project path".to_owned())
        );
    }

    #[test]
    fn registered_project_path_is_resolved_from_its_semantic_identity() {
        let source = "[[project]]\nname = \"first\"\npath = \"content/first\"\n\
            [[project]]\nname = \"second\"\npath = \"content/second\"\n";
        let objects = parse_toml_semantic_objects(Path::new("Workspace.vapor.toml"), source)
            .expect("valid workspace manifest");
        let selected = find(&objects, "project[1].path").expect("second project path");

        assert_eq!(
            registered_workspace_project_path(source, &selected.id).as_deref(),
            Some(Path::new("content/second"))
        );
    }

    #[test]
    fn registered_project_path_does_not_accept_other_semantic_fields() {
        let source = "[[project]]\nname = \"first\"\npath = \"content/first\"\n";
        let objects = parse_toml_semantic_objects(Path::new("Workspace.vapor.toml"), source)
            .expect("valid workspace manifest");
        let name = find(&objects, "project[0].name").expect("project name");

        assert!(registered_workspace_project_path(source, &name.id).is_none());
        let wrong_domain = vapor_gui_core::semantic::SemanticObjectId::new(
            "vapor-packagepack",
            "Workspace.vapor.toml",
            vec!["project".into(), "[0]".into(), "path".into()],
        );
        assert!(registered_workspace_project_path(source, &wrong_domain).is_none());
    }

    #[test]
    fn workspace_content_project_paths_are_semantic_and_resolvable() {
        let source = "[workspace]\nname = \"sample\"\n\
            [[workspace.projects]]\npath = \"content/first\"\n\
            [[workspace.projects]]\npath = \"content/second\"\n";
        let objects = parse_toml_semantic_objects(Path::new("Workspace.vapor.toml"), source)
            .expect("valid workspace manifest");
        let selected = find(&objects, "workspace.projects[1].path")
            .expect("second registered content project path");

        assert!(
            selected
                .attributes
                .contains(&"Registered workspace content project path".to_owned())
        );
        assert_eq!(
            registered_workspace_project_path(source, &selected.id).as_deref(),
            Some(Path::new("content/second"))
        );
    }

    #[test]
    fn large_toml_arrays_and_nesting_obey_the_shared_tree_limits() {
        let dense = format!(
            "values = [{}]",
            (0..5_000)
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let objects = parse_toml_semantic_objects(Path::new("dense-array.toml"), &dense)
            .expect("valid large array");
        assert!(semantic_object_count(&objects) <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
        assert!(
            objects[0]
                .attributes
                .iter()
                .any(|attribute| attribute.contains("outline truncated"))
        );

        let nested = format!("value = {}0{}", "[".repeat(40), "]".repeat(40));
        let objects = parse_toml_semantic_objects(Path::new("nested.toml"), &nested)
            .expect("valid nested arrays within TOML's parser limit");
        assert!(semantic_object_count(&objects) <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
    }

    fn semantic_object_count(objects: &[SemanticObject]) -> usize {
        objects
            .iter()
            .map(|object| 1 + semantic_object_count(&object.children))
            .sum()
    }

    #[test]
    fn dense_toml_projection_is_bounded_and_marks_the_partial_outline() {
        let source = (0..5_000)
            .map(|index| format!("key_{index} = {index}\n"))
            .collect::<String>();
        let objects = parse_toml_semantic_objects(Path::new("dense.toml"), &source)
            .expect("valid dense TOML");
        assert!(objects.len() <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
        assert!(
            objects[0]
                .attributes
                .iter()
                .any(|attribute| attribute.contains("outline truncated"))
        );
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

        let multiple_roles = authored_manifest_diagnostics(
            Path::new("Packagepack.vapor.toml"),
            "[packagepack]\nname = \"demo\"\n[game]\nname = \"other\"\n",
        );
        assert_eq!(multiple_roles.len(), 1);
        assert_eq!(
            multiple_roles[0].code.as_deref(),
            Some("vapor.manifest.multiple-roles")
        );
        assert!(multiple_roles[0].primary.is_some());
    }

    #[test]
    fn authored_content_fields_distinguish_identity_versions_and_reference_roles() {
        let source = r#"
schema = 1

[game]
id = "ghf-studios/example/game"
name = "example-game"
version.workspace = true
libraries = ["example-game"]

[game.engine]
id = "ghf-studios/example/engine"
version = "^0.5"
workshop-id = "12345"

[[game.dependencies]]
id = "ghf-studios/example/library"
relationship = "requires"
optional = true
version = "^0.5"
reason = "shared rules"

[game.steam]
app-id = 2122620
published-file-id = "67890"
"#;
        let objects = parse_toml_semantic_objects(Path::new("Game.vapor.toml"), source)
            .expect("valid authored game manifest");
        let attribute = |path: &str| {
            find(&objects, path)
                .unwrap_or_else(|| panic!("missing {path}"))
                .attributes
                .first()
                .map(String::as_str)
        };

        assert_eq!(attribute("schema"), Some("Vapor manifest schema"));
        assert_eq!(attribute("game.id"), Some("Vapor content identity"));
        assert_eq!(attribute("game.version"), Some("Declared content version"));
        assert_eq!(attribute("game.engine.id"), Some("Vapor content reference"));
        assert_eq!(
            attribute("game.engine.version"),
            Some("Vapor version requirement")
        );
        assert_eq!(
            attribute("game.engine.workshop-id"),
            Some("Workshop acquisition ID")
        );
        assert_eq!(
            attribute("game.dependencies[0].id"),
            Some("Vapor content reference")
        );
        assert_eq!(
            attribute("game.dependencies[0].optional"),
            Some("Optional dependency")
        );
        assert_eq!(
            attribute("game.dependencies[0].reason"),
            Some("Dependency explanation")
        );
        assert_eq!(attribute("game.steam.app-id"), Some("Steam application ID"));
        assert_eq!(
            attribute("game.steam.published-file-id"),
            Some("Published Workshop item ID")
        );
        let reference = find(&objects, "game.engine.id").unwrap();
        assert_eq!(
            authored_reference_id(source, &reference.id).as_deref(),
            Some("ghf-studios/example/engine")
        );
        let dependency = find(&objects, "game.dependencies[0].id").unwrap();
        assert_eq!(
            authored_reference_id(source, &dependency.id).as_deref(),
            Some("ghf-studios/example/library")
        );
        let identity = find(&objects, "game.id").unwrap();
        assert!(authored_reference_id(source, &identity.id).is_none());
    }
}
