use std::{collections::BTreeMap, path::Path};

use quote::ToTokens;
use vapor_gui_core::semantic::{
    SemanticObject, SemanticObjectId, SemanticObjectKind, SemanticPosition, SemanticRange,
};
use vapor_gui_core::{DiagnosticOrigin, DiagnosticSeverity, DiagnosticSpan, StructuredDiagnostic};

const WORKSPACE_SYMBOL_LIMIT: usize = 500;
const DOCUMENT_SYMBOL_LIMIT: usize = vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS;
const DOCUMENT_SYMBOL_DEPTH_LIMIT: usize = vapor_gui_core::semantic::MAX_SEMANTIC_DEPTH;


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CanonicalRustItemKind {
    Component,
    Resource,
    Event,
    Type,
    Function,
    System,
    RunCondition,
    Constant,
    Static,
}

impl CanonicalRustItemKind {
    pub(super) fn for_document(document: &Path) -> Option<Self> {
        if document.extension().is_none_or(|extension| extension != "rs")
            || !document.components().any(|component| component.as_os_str() == "src")
        {
            return None;
        }
        match document.file_name()?.to_str()? {
            "components.rs" => Some(Self::Component),
            "resources.rs" => Some(Self::Resource),
            "events.rs" => Some(Self::Event),
            "types.rs" => Some(Self::Type),
            "functions.rs" => Some(Self::Function),
            "systems.rs" => Some(Self::System),
            "run_conditions.rs" => Some(Self::RunCondition),
            "constants.rs" => Some(Self::Constant),
            "statics.rs" => Some(Self::Static),
            _ => None,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Resource => "resource",
            Self::Event => "event",
            Self::Type => "type",
            Self::Function => "function",
            Self::System => "system",
            Self::RunCondition => "run condition",
            Self::Constant => "constant",
            Self::Static => "static",
        }
    }

    pub(super) fn default_name(self) -> &'static str {
        match self {
            Self::Component => "NewComponent",
            Self::Resource => "NewResource",
            Self::Event => "NewEvent",
            Self::Type => "NewType",
            Self::Function => "new_function",
            Self::System => "new_system",
            Self::RunCondition => "is_ready",
            Self::Constant | Self::Static => "NEW_VALUE",
        }
    }

    fn item_conflicts(self, item: &syn::Item, name: &str) -> bool {
        let (existing_name, declares_type, declares_value) = match item {
            syn::Item::Struct(item) => (
                item.ident.to_string(),
                true,
                matches!(item.fields, syn::Fields::Unit | syn::Fields::Unnamed(_)),
            ),
            syn::Item::Enum(item) => (item.ident.to_string(), true, false),
            syn::Item::Trait(item) => (item.ident.to_string(), true, false),
            syn::Item::Type(item) => (item.ident.to_string(), true, false),
            syn::Item::Union(item) => (item.ident.to_string(), true, false),
            syn::Item::Mod(item) => (item.ident.to_string(), true, false),
            syn::Item::ExternCrate(item) => (item.ident.to_string(), true, false),
            syn::Item::Fn(item) => (item.sig.ident.to_string(), false, true),
            syn::Item::Const(item) => (item.ident.to_string(), false, true),
            syn::Item::Static(item) => (item.ident.to_string(), false, true),
            syn::Item::Use(item) => {
                let aliases = use_tree_names(&item.tree);
                let generated_type = matches!(
                    self,
                    Self::Component | Self::Resource | Self::Event | Self::Type
                );
                let generated_value = generated_type
                    || matches!(
                        self,
                        Self::Function | Self::System | Self::RunCondition | Self::Constant | Self::Static
                    );
                return aliases.iter().any(|alias| {
                    normalize_ident(alias) == normalize_ident(name)
                        && (generated_type || generated_value)
                });
            }
            _ => return false,
        };
        let generated_type = matches!(
            self,
            Self::Component | Self::Resource | Self::Event | Self::Type
        );
        let generated_value = generated_type
            || matches!(
                self,
                Self::Function | Self::System | Self::RunCondition | Self::Constant | Self::Static
            );
        normalize_ident(&existing_name) == normalize_ident(name)
            && ((generated_type && declares_type) || (generated_value && declares_value))
    }
}

fn normalize_ident(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn use_tree_names(tree: &syn::UseTree) -> Vec<String> {
    match tree {
        syn::UseTree::Path(path) => use_tree_names(&path.tree),
        syn::UseTree::Name(name) => vec![name.ident.to_string()],
        syn::UseTree::Rename(rename) => vec![rename.rename.to_string()],
        syn::UseTree::Group(group) => group.items.iter().flat_map(use_tree_names).collect(),
        syn::UseTree::Glob(_) => Vec::new(),
    }
}

fn name_style_error(kind: CanonicalRustItemKind, name: &str) -> Option<&'static str> {
    let name = normalize_ident(name);
    let chars = name.chars().collect::<Vec<_>>();
    let valid = match kind {
        CanonicalRustItemKind::Component
        | CanonicalRustItemKind::Resource
        | CanonicalRustItemKind::Event
        | CanonicalRustItemKind::Type => chars.first().is_some_and(|character| character.is_uppercase())
            && !name.contains('_'),
        CanonicalRustItemKind::Function
        | CanonicalRustItemKind::System
        | CanonicalRustItemKind::RunCondition => {
            chars.first().is_some_and(|character| character.is_lowercase() || *character == '_')
                && chars.iter().all(|character| character.is_lowercase() || character.is_ascii_digit() || *character == '_')
        }
        CanonicalRustItemKind::Constant | CanonicalRustItemKind::Static => {
            chars.iter().any(|character| character.is_ascii_uppercase())
                && chars.iter().all(|character| character.is_ascii_uppercase() || character.is_ascii_digit() || *character == '_')
        }
    };
    if valid {
        None
    } else {
        Some(match kind {
            CanonicalRustItemKind::Component
            | CanonicalRustItemKind::Resource
            | CanonicalRustItemKind::Event
            | CanonicalRustItemKind::Type => "Use UpperCamelCase for Rust types.",
            CanonicalRustItemKind::Function
            | CanonicalRustItemKind::System
            | CanonicalRustItemKind::RunCondition => "Use snake_case for Rust functions.",
            CanonicalRustItemKind::Constant | CanonicalRustItemKind::Static => {
                "Use SCREAMING_SNAKE_CASE for constants and statics."
            }
        })
    }
}

pub(super) fn render_canonical_rust_item(
    document: &Path,
    source: &str,
    name: &str,
) -> Result<String, String> {
    let Some(kind) = CanonicalRustItemKind::for_document(document) else {
        return Err("Open a canonical Rust module before creating a semantic item.".to_owned());
    };
    if name.trim() != name || name.is_empty() {
        return Err("Enter one Rust identifier without surrounding whitespace.".to_owned());
    }
    if name.len() > 128 {
        return Err("Rust item names are limited to 128 bytes.".to_owned());
    }
    let snippet = match kind {
        CanonicalRustItemKind::Component => format!("#[derive(bevy::prelude::Component)]\npub struct {name};\n"),
        CanonicalRustItemKind::Resource => format!("#[derive(bevy::prelude::Resource)]\npub struct {name};\n"),
        CanonicalRustItemKind::Event => format!("#[derive(bevy::prelude::Event)]\npub struct {name};\n"),
        CanonicalRustItemKind::Type => format!("pub struct {name};\n"),
        CanonicalRustItemKind::Function | CanonicalRustItemKind::System => format!("pub fn {name}() {{\n}}\n"),
        CanonicalRustItemKind::RunCondition => format!("pub fn {name}() -> bool {{\n    true\n}}\n"),
        CanonicalRustItemKind::Constant => format!("pub const {name}: &str = \"\";\n"),
        CanonicalRustItemKind::Static => format!("pub static {name}: &str = \"\";\n"),
    };
    let parsed_item = syn::parse_str::<syn::Item>(&snippet)
        .map_err(|_| format!("`{name}` is not a valid Rust {} declaration name.", kind.label()))?;
    if let Some(message) = name_style_error(kind, name) {
        return Err(message.to_owned());
    }
    let existing = syn::parse_file(source)
        .map_err(|_| "Fix syntax errors in this file before inserting a semantic item.".to_owned())?;
    let identifier = syn::parse_str::<syn::Ident>(name)
        .map_err(|_| format!("`{name}` is not a valid Rust {} declaration name.", kind.label()))?;
    let canonical_name = normalize_ident(&identifier.to_string()).to_owned();
    if existing
        .items
        .iter()
        .any(|item| kind.item_conflicts(item, &canonical_name))
    {
        return Err(format!("A {} named `{name}` already exists in this module.", kind.label()));
    }
    let _ = parsed_item;
    let separator = if source.is_empty() || source.ends_with("\n\n") {
        ""
    } else if source.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let mut updated = String::with_capacity(source.len() + separator.len() + snippet.len());
    updated.push_str(source);
    updated.push_str(separator);
    updated.push_str(&snippet);
    if updated.len() > vapor_gui_core::semantic::MAX_SEMANTIC_DOCUMENT_BYTES {
        return Err("This file is at the semantic indexing size limit; item creation is unavailable.".to_owned());
    }
    Ok(updated)
}

pub(super) fn convention_diagnostics(
    document: &Path,
    objects: &[SemanticObject],
) -> Vec<StructuredDiagnostic> {
    objects
        .iter()
        .filter_map(|object| {
            let (expected_file, code_kind) = match object.kind {
                SemanticObjectKind::Component => ("components.rs", "component"),
                SemanticObjectKind::Resource => ("resources.rs", "resource"),
                SemanticObjectKind::Event => ("events.rs", "event"),
                SemanticObjectKind::Struct
                | SemanticObjectKind::Enum
                | SemanticObjectKind::Variant
                | SemanticObjectKind::Trait
                | SemanticObjectKind::TypeAlias => ("types.rs", "type"),
                SemanticObjectKind::Function => ("functions.rs", "function"),
                SemanticObjectKind::Constant => ("constants.rs", "constant"),
                SemanticObjectKind::Static => ("statics.rs", "static"),
                _ => return None,
            };
            if document.file_name().and_then(|name| name.to_str()) == Some(expected_file) {
                return None;
            }
            let message = format!(
                "{} `{}` belongs in `{expected_file}`.",
                object.kind.label(),
                object.name
            );
            Some(StructuredDiagnostic {
                origin: DiagnosticOrigin::Vapor,
                severity: DiagnosticSeverity::Warning,
                code: Some(format!("vapor.rust.layout.{code_kind}")),
                message: message.clone(),
                rendered: None,
                primary: Some(DiagnosticSpan {
                    file: document.to_string_lossy().into_owned(),
                    start_line: object.selection_range.start.line.saturating_add(1),
                    start_column: object.selection_range.start.character.saturating_add(1),
                    end_line: object.range.end.line.saturating_add(1),
                    end_column: object.range.end.character.saturating_add(1),
                    primary: true,
                    label: Some("Canonical Rust module".to_owned()),
                }),
                related: Vec::new(),
                raw_payload: message,
            })
        })
        .collect()
}

fn compact_rust_tokens(tokens: impl std::fmt::Display) -> String {
    tokens.to_string()
        .replace(" ::", "::")
        .replace(":: ", "::")
        .replace(" (", "(")
        .replace(" ,", ",")
        .replace("< ", "<")
        .replace(" >", ">")
}

#[derive(Debug, Clone, Default)]
struct RustSyntaxMetadata {
    kind: Option<SemanticObjectKind>,
    attributes: Vec<String>,
    fields: BTreeMap<String, (String, Vec<String>)>,
}

fn rust_syntax_metadata(source: &str, document: &Path) -> BTreeMap<Vec<String>, RustSyntaxMetadata> {
    let Ok(file) = syn::parse_file(source) else {
        return BTreeMap::new();
    };
    fn compact_attributes(attributes: &[syn::Attribute]) -> Vec<String> {
        attributes.iter().map(|attribute| {
            format!("#[{}]", compact_rust_tokens(attribute.meta.to_token_stream()))
        }).collect()
    }
    fn derive_kind(attributes: &[syn::Attribute]) -> Option<SemanticObjectKind> {
        let mut kind = None;
        for attribute in attributes {
            if attribute.path().is_ident("component") {
                kind = Some(SemanticObjectKind::Component);
            }
            if attribute.path().is_ident("derive") {
                let _ = attribute.parse_nested_meta(|meta| {
                    if let Some(segment) = meta.path.segments.last() {
                        kind = match segment.ident.to_string().as_str() {
                            "Component" => Some(SemanticObjectKind::Component),
                            "Resource" => Some(SemanticObjectKind::Resource),
                            "Event" => Some(SemanticObjectKind::Event),
                            _ => kind,
                        };
                    }
                    Ok(())
                });
            }
        }
        kind
    }
    fn visit(
        items: &[syn::Item],
        parent: &mut Vec<String>,
        system_file_kind: Option<SemanticObjectKind>,
        output: &mut BTreeMap<Vec<String>, RustSyntaxMetadata>,
    ) {
        for item in items {
            let (ident, attributes, base_kind) = match item {
                syn::Item::Struct(item) => (Some(item.ident.to_string()), item.attrs.as_slice(), Some(SemanticObjectKind::Struct)),
                syn::Item::Enum(item) => (Some(item.ident.to_string()), item.attrs.as_slice(), Some(SemanticObjectKind::Enum)),
                syn::Item::Trait(item) => (Some(item.ident.to_string()), item.attrs.as_slice(), Some(SemanticObjectKind::Trait)),
                syn::Item::Type(item) => (Some(item.ident.to_string()), item.attrs.as_slice(), Some(SemanticObjectKind::TypeAlias)),
                syn::Item::Const(item) => (Some(item.ident.to_string()), item.attrs.as_slice(), Some(SemanticObjectKind::Constant)),
                syn::Item::Static(item) => (Some(item.ident.to_string()), item.attrs.as_slice(), Some(SemanticObjectKind::Static)),
                syn::Item::Fn(item) => (Some(item.sig.ident.to_string()), item.attrs.as_slice(), system_file_kind),
                syn::Item::Mod(module) => {
                    if let Some((_, nested)) = &module.content {
                        parent.push(module.ident.to_string());
                        visit(nested, parent, system_file_kind, output);
                        parent.pop();
                    }
                    (None, &[][..], None)
                }
                _ => (None, &[][..], None),
            };
            let Some(ident) = ident else { continue; };
            let mut metadata = RustSyntaxMetadata {
                kind: base_kind,
                attributes: compact_attributes(attributes),
                fields: BTreeMap::new(),
            };
            if matches!(item, syn::Item::Struct(_)) {
                metadata.kind = derive_kind(attributes).or(metadata.kind);
            }
            if matches!(item, syn::Item::Struct(_)) {
                let syn::Item::Struct(item) = item else { unreachable!() };
                match &item.fields {
                    syn::Fields::Named(fields) => {
                        for field in &fields.named {
                            let name = field.ident.as_ref().map_or_else(String::new, ToString::to_string);
                            metadata.fields.insert(name, (
                                compact_rust_tokens(field.ty.to_token_stream()),
                                compact_attributes(&field.attrs),
                            ));
                        }
                    }
                    syn::Fields::Unnamed(fields) => {
                        for (index, field) in fields.unnamed.iter().enumerate() {
                            metadata.fields.insert(index.to_string(), (
                                compact_rust_tokens(field.ty.to_token_stream()),
                                compact_attributes(&field.attrs),
                            ));
                        }
                    }
                    syn::Fields::Unit => {}
                }
            }
            let mut path = parent.clone();
            path.push(ident);
            output.insert(path, metadata);
        }
    }
    let system_file_kind = match document.file_stem().and_then(|stem| stem.to_str()) {
        Some("systems") => Some(SemanticObjectKind::System),
        Some("run_conditions") => Some(SemanticObjectKind::RunCondition),
        _ => None,
    };
    let mut output = BTreeMap::new();
    visit(&file.items, &mut Vec::new(), system_file_kind, &mut output);
    output
}

#[derive(Debug, Default)]
pub(super) struct ParsedSemanticDocument {
    pub(super) objects: Vec<SemanticObject>,
    pub(super) truncated: bool,
}

#[cfg(test)]
pub(super) fn parse_semantic_symbols(
    result: &serde_json::Value,
    document: &Path,
    document_version: u64,
    source: &str,
) -> Vec<SemanticObject> {
    parse_semantic_document_symbols(result, document, document_version, source).objects
}

pub(super) fn parse_semantic_document_symbols(
    result: &serde_json::Value,
    document: &Path,
    _document_version: u64,
    source: &str,
) -> ParsedSemanticDocument {
    if source.len() > vapor_gui_core::semantic::MAX_SEMANTIC_DOCUMENT_BYTES {
        return ParsedSemanticDocument {
            objects: Vec::new(),
            truncated: true,
        };
    }
    let metadata = rust_syntax_metadata(source, document);
    let Some(symbols) = result.as_array() else {
        return ParsedSemanticDocument::default();
    };
    let mut parsed = ParsedSemanticDocument::default();
    let mut remaining = DOCUMENT_SYMBOL_LIMIT;
    for symbol in symbols {
        if remaining == 0 {
            parsed.truncated = true;
            break;
        }
        if let Some(object) = parse_semantic_symbol(
            symbol,
            document,
            &[],
            &metadata,
            &mut remaining,
            0,
            &mut parsed.truncated,
        ) {
            parsed.objects.push(object);
        }
    }
    parsed
}

pub(super) fn parse_workspace_symbols(
    result: &serde_json::Value,
    workspace_root: &Path,
) -> Vec<SemanticObject> {
    let Some(symbols) = result.as_array() else {
        return Vec::new();
    };
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    symbols
        .iter()
        .take(WORKSPACE_SYMBOL_LIMIT)
        .filter_map(|symbol| {
            let name = symbol.get("name")?.as_str()?.to_owned();
            let location = symbol.get("location")?;
            let uri = location.get("uri")?.as_str()?;
            let absolute_path = super::path_from_file_uri(uri)?;
            let canonical_path = absolute_path
                .canonicalize()
                .unwrap_or(absolute_path);
            let document = canonical_path.strip_prefix(&canonical_root).ok()?.to_path_buf();
            let range = location
                .get("range")
                .map(semantic_range)
                .unwrap_or_default();
            let container = symbol
                .get("containerName")
                .and_then(serde_json::Value::as_str)
                .filter(|container| !container.is_empty());
            let mut path = container
                .map(|container| {
                    container
                        .split("::")
                        .filter(|segment| !segment.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            path.push(name.clone());
            let kind = rust_lsp_symbol_kind(
                symbol
                    .get("kind")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
            );
            let discriminator = format!(
                "{}:{}:{}",
                range.start.line,
                range.start.character,
                kind.label()
            );
            Some(SemanticObject {
                id: SemanticObjectId::with_discriminator(
                    "rust",
                    document,
                    path,
                    discriminator,
                ),
                kind,
                name,
                detail: container.map(str::to_owned),
                range,
                selection_range: range,
                attributes: Vec::new(),
                children: Vec::new(),
            })
        })
        .collect()
}

fn parse_semantic_symbol(
    symbol: &serde_json::Value,
    document: &Path,
    parent_path: &[String],
    metadata: &BTreeMap<Vec<String>, RustSyntaxMetadata>,
    remaining: &mut usize,
    depth: usize,
    truncated: &mut bool,
) -> Option<SemanticObject> {
    if *remaining == 0 {
        *truncated = true;
        return None;
    }
    let name = symbol.get("name")?.as_str()?.to_owned();
    *remaining -= 1;
    let kind = symbol.get("kind").and_then(serde_json::Value::as_u64).unwrap_or_default();
    let semantic_kind = rust_lsp_symbol_kind(kind);
    let range = symbol.get("range")
        .or_else(|| symbol.get("location").and_then(|location| location.get("range")))
        .map(semantic_range)
        .unwrap_or_default();
    let selection_range = symbol.get("selectionRange")
        .map(semantic_range)
        .unwrap_or(range);
    let mut object_path = parent_path.to_vec();
    object_path.push(name.clone());
    let children = symbol.get("children").and_then(serde_json::Value::as_array)
        .map(|children| {
            if depth >= DOCUMENT_SYMBOL_DEPTH_LIMIT {
                if !children.is_empty() {
                    *truncated = true;
                }
                return Vec::new();
            }
            let mut parsed = Vec::new();
            for child in children {
                if *remaining == 0 {
                    *truncated = true;
                    break;
                }
                if let Some(child) = parse_semantic_symbol(
                    child,
                    document,
                    &object_path,
                    metadata,
                    remaining,
                    depth + 1,
                    truncated,
                ) {
                    parsed.push(child);
                }
            }
            parsed
        })
        .unwrap_or_default();
    let identity = format!(
        "{}:{}:{}:{}",
        selection_range.start.line,
        selection_range.start.character,
        semantic_kind.label(),
        symbol.get("detail").and_then(serde_json::Value::as_str).unwrap_or_default()
    );
    let mut object = SemanticObject {
        id: SemanticObjectId::with_discriminator("rust", document, object_path.clone(), identity),
        kind: semantic_kind,
        name: name.clone(),
        detail: symbol.get("detail").and_then(serde_json::Value::as_str).map(str::to_owned),
        range,
        selection_range,
        attributes: Vec::new(),
        children,
    };
    if let Some(syntax_metadata) = metadata.get(&object_path) {
        if let Some(kind) = syntax_metadata.kind {
            object.kind = kind;
        }
        object.attributes = syntax_metadata.attributes.clone();
        for field in &mut object.children {
            if field.kind == SemanticObjectKind::Field
                && let Some((field_type, attributes)) = syntax_metadata.fields.get(&field.name)
            {
                field.detail = Some(field_type.clone());
                field.attributes = attributes.clone();
            }
        }
    }
    Some(object)
}

pub(super) fn rust_lsp_symbol_kind(kind: u64) -> SemanticObjectKind {
    match kind {
        2 | 3 => SemanticObjectKind::Module,
        5 => SemanticObjectKind::Struct,
        6 => SemanticObjectKind::Method,
        8 => SemanticObjectKind::Field,
        10 => SemanticObjectKind::Enum,
        11 => SemanticObjectKind::Trait,
        12 => SemanticObjectKind::Function,
        13 => SemanticObjectKind::Other, // Variable
        14 => SemanticObjectKind::Constant,
        22 => SemanticObjectKind::Variant,
        23 => SemanticObjectKind::Struct,
        24 => SemanticObjectKind::Other, // Event
        25 => SemanticObjectKind::Other, // Operator
        26 => SemanticObjectKind::Other, // TypeParameter
        _ => SemanticObjectKind::Other,
    }
}

fn semantic_range(value: &serde_json::Value) -> SemanticRange {
    fn position(value: &serde_json::Value) -> SemanticPosition {
        SemanticPosition {
            line: value.get("line").and_then(serde_json::Value::as_u64).unwrap_or_default().min(u32::MAX as u64) as u32,
            character: value.get("character").and_then(serde_json::Value::as_u64).unwrap_or_default().min(u32::MAX as u64) as u32,
        }
    }
    SemanticRange {
        start: value.get("start").map(position).unwrap_or_default(),
        end: value.get("end").map(position).unwrap_or_default(),
    }
}

