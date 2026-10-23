use std::path::Path;

use crate::semantic::{
    SemanticObject, SemanticObjectId, SemanticObjectKind, semantic_range_from_byte_span,
};
use crate::{
    DiagnosticContext, DiagnosticOrigin, DiagnosticScope, DiagnosticSeverity, DiagnosticSpan,
    StructuredDiagnostic,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    text: String,
    range: std::ops::Range<usize>,
}

struct LexedTokens {
    tokens: Vec<Token>,
    truncated: bool,
}

const MAX_RHAI_TOKENS: usize = 65_536;

/// Build a syntax-only outline for Rhai scripts without evaluating them.
///
/// This intentionally recognizes declarations, not expressions or inferred
/// runtime types. Rust remains responsible for the script host's capabilities
/// and runtime behavior.
pub fn parse_rhai_semantic_objects(document: &Path, source: &str) -> Vec<SemanticObject> {
    if source.len() > crate::semantic::MAX_SEMANTIC_DOCUMENT_BYTES {
        return Vec::new();
    }
    let lexed = lex_declarations(source);
    let tokens = lexed.tokens;
    let mut declarations = Vec::new();
    let mut function_token_ranges = Vec::new();
    let mut index = 0;
    while index + 1 < tokens.len() {
        if tokens[index].text != "fn" || !is_identifier(&tokens[index + 1].text) {
            index += 1;
            continue;
        }
        if index > 0 && matches!(tokens[index - 1].text.as_str(), "." | "::") {
            index += 1;
            continue;
        }
        let Some(_) = tokens.get(index + 2).filter(|token| token.text == "(") else {
            index += 1;
            continue;
        };
        let Some(close_paren_index) = matching_delimiter(&tokens, index + 2, "(", ")") else {
            index += 1;
            continue;
        };
        if tokens
            .get(close_paren_index + 1)
            .is_some_and(|token| token.text != "{")
        {
            index = close_paren_index + 1;
            continue;
        }
        let name = tokens[index + 1].text.clone();
        let parameters = parameter_objects(document, source, &tokens, index + 3, close_paren_index);
        let body_open = close_paren_index + 1;
        let body_close = tokens
            .get(body_open)
            .filter(|token| token.text == "{")
            .and_then(|_| matching_delimiter(&tokens, body_open, "{", "}"));
        let body_end = body_close
            .and_then(|close| tokens.get(close).map(|token| token.range.end))
            .unwrap_or(source.len());
        let mut children = parameters;
        if tokens.get(body_open).is_some_and(|token| token.text == "{") {
            let end = body_close.unwrap_or(tokens.len());
            children.extend(binding_objects(
                document,
                source,
                &tokens,
                body_open + 1,
                end,
            ));
        }
        children.sort_by_key(|object| (object.range.start.line, object.range.start.character));
        let declaration_start = tokens[index].range.start;
        let selection = tokens[index + 1].range.clone();
        declarations.push(SemanticObject {
            id: SemanticObjectId::with_discriminator(
                "rhai",
                document,
                vec![name.clone()],
                format!("{}:{}", selection.start, selection.end),
            ),
            kind: SemanticObjectKind::Function,
            name,
            detail: Some(function_signature(&tokens, index + 1, close_paren_index)),
            range: semantic_range_from_byte_span(source, declaration_start..body_end),
            selection_range: semantic_range_from_byte_span(source, selection),
            attributes: vec!["Rhai function".to_owned()],
            children,
        });
        function_token_ranges
            .push(index..body_close.map_or(close_paren_index + 1, |close| close + 1));
        index = body_close.map_or(close_paren_index + 1, |close| close + 1);
    }

    let mut function_range_index = 0;
    for index in 0..tokens.len().saturating_sub(1) {
        while function_token_ranges
            .get(function_range_index)
            .is_some_and(|range| range.end <= index)
        {
            function_range_index += 1;
        }
        if function_token_ranges
            .get(function_range_index)
            .is_some_and(|range| range.contains(&index))
        {
            continue;
        }
        let (kind, label) = match tokens[index].text.as_str() {
            "let" => (SemanticObjectKind::Field, "mutable binding"),
            "const" => (SemanticObjectKind::Constant, "constant binding"),
            _ => continue,
        };
        if is_identifier(&tokens[index + 1].text) {
            declarations.push(binding_object(
                document,
                source,
                &tokens[index + 1],
                label,
                kind,
            ));
        }
    }
    declarations.sort_by_key(|object| (object.range.start.line, object.range.start.character));
    if declarations.is_empty() && !lexed.truncated {
        return Vec::new();
    }

    let mut remaining = crate::semantic::MAX_SEMANTIC_OBJECTS.saturating_sub(1);
    let mut truncated = lexed.truncated;
    truncate_semantic_objects(&mut declarations, &mut remaining, 1, &mut truncated);
    let mut root_attributes = vec!["Syntax outline · no execution".to_owned()];
    if truncated {
        root_attributes.push(format!(
            "Semantic outline truncated at {} objects, {} levels, or {} tokens",
            crate::semantic::MAX_SEMANTIC_OBJECTS,
            crate::semantic::MAX_SEMANTIC_DEPTH,
            MAX_RHAI_TOKENS,
        ));
    }
    vec![SemanticObject {
        id: SemanticObjectId::new("rhai", document, vec!["script".to_owned()]),
        kind: SemanticObjectKind::Script,
        name: document.file_name().map_or_else(
            || "Script".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
        detail: Some(format!("{} declarations", declarations.len())),
        range: semantic_range_from_byte_span(source, 0..source.len()),
        selection_range: semantic_range_from_byte_span(source, 0..source.len().min(1)),
        attributes: root_attributes,
        children: declarations,
    }]
}

/// Report bounded Rhai parser and delimiter diagnostics without evaluating scripts.
pub fn rhai_structure_diagnostics(document: &Path, source: &str) -> Vec<StructuredDiagnostic> {
    if source.len() > crate::semantic::MAX_SEMANTIC_DOCUMENT_BYTES {
        return Vec::new();
    }
    let lexed = lex_declarations(source);
    if lexed.truncated {
        let message = "Rhai structural diagnostics stopped at the local token limit; the script remains editable.".to_owned();
        return vec![StructuredDiagnostic {
            origin: DiagnosticOrigin::Vapor,
            severity: DiagnosticSeverity::Information,
            code: Some("vapor.semantic.token-limit".to_owned()),
            message: message.clone(),
            rendered: Some(message.clone()),
            primary: None,
            related: Vec::new(),

            raw_payload: "Rhai token limit reached".to_owned(),
            context: DiagnosticContext {
                domain: Some("rhai".to_owned()),
                scope: DiagnosticScope::Document,
                ..Default::default()
            },
        }];
    }
    let tokens = lexed.tokens;
    let mut open = Vec::<(&str, &Token)>::new();
    let mut diagnostics = Vec::new();
    for token in &tokens {
        match token.text.as_str() {
            "(" | "[" | "{" => open.push((token.text.as_str(), token)),
            ")" | "]" | "}" => {
                let expected_open = match token.text.as_str() {
                    ")" => "(",
                    "]" => "[",
                    "}" => "{",
                    _ => unreachable!(),
                };
                match open.pop() {
                    Some((opening, _)) if opening == expected_open => {}
                    Some((opening, opener)) => {
                        diagnostics.push(structure_diagnostic(
                            document,
                            source,
                            token,
                            format!(
                                "Mismatched delimiter: '{}' closes '{}'",
                                token.text, opener.text
                            ),
                        ));
                        open.push((opening, opener));
                    }
                    None => diagnostics.push(structure_diagnostic(
                        document,
                        source,
                        token,
                        format!("Unexpected '{}' delimiter", token.text),
                    )),
                }
            }
            _ => {}
        }
        if diagnostics.len() >= 64 {
            break;
        }
    }
    for (_, token) in open
        .into_iter()
        .take(64usize.saturating_sub(diagnostics.len()))
    {
        diagnostics.push(structure_diagnostic(
            document,
            source,
            token,
            format!("Unclosed '{}' delimiter", token.text),
        ));
    }
    if diagnostics.is_empty() {
        let mut engine = rhai::Engine::new();
        engine.set_max_expr_depths(128, 64);
        if let Err(error) = engine.compile(source) {
            diagnostics.push(rhai_parse_diagnostic(document, source, &error));
        }
    }
    diagnostics
}

fn rhai_parse_diagnostic(
    document: &Path,
    source: &str,
    error: &rhai::ParseError,
) -> StructuredDiagnostic {
    let position = error.position();
    let primary = position
        .line()
        .and_then(|line| rhai_position_span(source, line, position.position().unwrap_or(1)))
        .map(|span| {
            let range = semantic_range_from_byte_span(source, span);
            DiagnosticSpan {
                file: document.to_string_lossy().into_owned(),
                start_line: range.start.line.saturating_add(1),
                start_column: range.start.character.saturating_add(1),
                end_line: range.end.line.saturating_add(1),
                end_column: range.end.character.saturating_add(1),
                primary: true,
                label: Some("Rhai syntax error".to_owned()),
            }
        });
    let message = format!("Rhai syntax error: {error}");
    StructuredDiagnostic {
        origin: DiagnosticOrigin::Vapor,
        severity: DiagnosticSeverity::Error,
        code: Some("rhai.parse".to_owned()),
        message: message.clone(),
        rendered: None,
        primary,
        related: Vec::new(),
        raw_payload: error.to_string(),
        context: DiagnosticContext {
            domain: Some("rhai".to_owned()),
            scope: DiagnosticScope::Document,
            ..Default::default()
        },
    }
}

fn rhai_position_span(source: &str, line: usize, column: usize) -> Option<std::ops::Range<usize>> {
    let line_start = source
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let line_text = source.get(line_start..)?.split('\n').next()?;
    let character_offset = column.saturating_sub(1);
    let start = line_text
        .char_indices()
        .nth(character_offset)
        .map_or(line_text.len(), |(offset, _)| offset);
    let end = line_text
        .get(start..)?
        .chars()
        .next()
        .map_or(start, |character| start + character.len_utf8());
    Some((line_start + start)..(line_start + end))
}

fn structure_diagnostic(
    document: &Path,
    source: &str,
    token: &Token,
    message: String,
) -> StructuredDiagnostic {
    let range = semantic_range_from_byte_span(source, token.range.clone());
    StructuredDiagnostic {
        origin: DiagnosticOrigin::Vapor,
        severity: DiagnosticSeverity::Error,
        code: Some("rhai.structure".to_owned()),
        message: message.clone(),
        rendered: None,
        primary: Some(DiagnosticSpan {
            file: document.to_string_lossy().into_owned(),
            start_line: range.start.line.saturating_add(1),
            start_column: range.start.character.saturating_add(1),
            end_line: range.end.line.saturating_add(1),
            end_column: range.end.character.saturating_add(1),
            primary: true,
            label: Some("Rhai delimiter".to_owned()),
        }),
        related: Vec::new(),

        raw_payload: message,
        context: DiagnosticContext {
            domain: Some("rhai".to_owned()),
            scope: DiagnosticScope::Document,
            ..Default::default()
        },
    }
}

fn parameter_objects(
    document: &Path,
    source: &str,
    tokens: &[Token],
    start: usize,
    end: usize,
) -> Vec<SemanticObject> {
    let mut parameters = Vec::new();
    let mut depth = 0_i32;
    for token in tokens.get(start..end).unwrap_or_default() {
        if token.text == "(" || token.text == "[" || token.text == "{" {
            depth += 1;
        } else if token.text == ")" || token.text == "]" || token.text == "}" {
            depth -= 1;
        }
        if depth == 0 && token.text == "," {
            continue;
        }
        if depth == 0 && is_identifier(&token.text) {
            parameters.push(binding_object(
                document,
                source,
                token,
                "parameter",
                SemanticObjectKind::Parameter,
            ));
        }
    }
    parameters
}

fn binding_objects(
    document: &Path,
    source: &str,
    tokens: &[Token],
    start: usize,
    end: usize,
) -> Vec<SemanticObject> {
    let mut bindings = Vec::new();
    let mut index = start;
    while index + 1 < end {
        let (kind, label) = match tokens[index].text.as_str() {
            "let" => (SemanticObjectKind::Field, "mutable binding"),
            "const" => (SemanticObjectKind::Constant, "constant binding"),
            _ => {
                index += 1;
                continue;
            }
        };
        if is_identifier(&tokens[index + 1].text) {
            bindings.push(binding_object(
                document,
                source,
                &tokens[index + 1],
                label,
                kind,
            ));
            index += 2;
        } else {
            index += 1;
        }
    }
    bindings
}

fn binding_object(
    document: &Path,
    source: &str,
    token: &Token,
    detail: &str,
    kind: SemanticObjectKind,
) -> SemanticObject {
    let range = semantic_range_from_byte_span(source, token.range.clone());
    SemanticObject {
        id: SemanticObjectId::with_discriminator(
            "rhai",
            document,
            vec![token.text.clone()],
            format!("{}:{}", token.range.start, detail),
        ),
        kind,
        name: token.text.clone(),
        detail: Some(detail.to_owned()),
        range,
        selection_range: range,
        attributes: Vec::new(),
        children: Vec::new(),
    }
}

fn function_signature(tokens: &[Token], name_index: usize, close_paren: usize) -> String {
    format!(
        "fn {}({})",
        tokens[name_index].text,
        tokens[name_index + 2..close_paren]
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>()
    )
}

fn matching_delimiter(tokens: &[Token], start: usize, open: &str, close: &str) -> Option<usize> {
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if token.text == open {
            depth = depth.checked_add(1)?;
        } else if token.text == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && chars.all(|character| character == '_' || character.is_alphanumeric())
}

fn lex_declarations(source: &str) -> LexedTokens {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < source.len() {
        if tokens.len() >= MAX_RHAI_TOKENS {
            return LexedTokens {
                tokens,
                truncated: true,
            };
        }
        let Some(character) = source[cursor..].chars().next() else {
            break;
        };
        let start = cursor;
        if character.is_whitespace() {
            cursor += character.len_utf8();
            continue;
        }
        if source[cursor..].starts_with("//") {
            cursor += source[cursor..].find('\n').unwrap_or(source.len() - cursor);
            continue;
        }
        if source[cursor..].starts_with("/*") {
            cursor += 2;
            let mut depth = 1_u32;
            while cursor < source.len() && depth > 0 {
                if source[cursor..].starts_with("/*") {
                    depth += 1;
                    cursor += 2;
                } else if source[cursor..].starts_with("*/") {
                    depth -= 1;
                    cursor += 2;
                } else if let Some(next) = source[cursor..].chars().next() {
                    cursor += next.len_utf8();
                } else {
                    break;
                }
            }
            continue;
        }
        if matches!(character, '\'' | '"' | '`') {
            let quote = character;
            cursor += character.len_utf8();
            let mut escaped = false;
            while cursor < source.len() {
                let Some(next) = source[cursor..].chars().next() else {
                    break;
                };
                cursor += next.len_utf8();
                if escaped {
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                } else if next == quote {
                    break;
                }
            }
            continue;
        }
        if character == '_' || character.is_alphabetic() {
            cursor += character.len_utf8();
            while cursor < source.len() {
                let Some(next) = source[cursor..].chars().next() else {
                    break;
                };
                if next == '_' || next.is_alphanumeric() {
                    cursor += next.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(Token {
                text: source[start..cursor].to_owned(),
                range: start..cursor,
            });
            continue;
        }
        cursor += character.len_utf8();
        tokens.push(Token {
            text: character.to_string(),
            range: start..cursor,
        });
    }
    LexedTokens {
        tokens,
        truncated: false,
    }
}

fn truncate_semantic_objects(
    objects: &mut Vec<SemanticObject>,
    remaining: &mut usize,
    depth: usize,
    truncated: &mut bool,
) {
    if depth >= crate::semantic::MAX_SEMANTIC_DEPTH {
        if !objects.is_empty() {
            *truncated = true;
        }
        objects.clear();
        return;
    }
    let keep = objects.len().min(*remaining);
    if keep < objects.len() {
        objects.truncate(keep);
        *truncated = true;
    }
    *remaining -= keep;
    for object in objects {
        truncate_semantic_objects(&mut object.children, remaining, depth + 1, truncated);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::semantic::{SemanticObject, SemanticObjectKind};

    use super::parse_rhai_semantic_objects;

    #[test]
    fn outlines_rhai_functions_parameters_and_bindings_without_evaluating_source() {
        let source = "fn launch(target, speed) { let thrust = speed; const label = target; }";
        let objects = parse_rhai_semantic_objects(Path::new("scripts/flight.rhai"), source);
        let function = &objects[0].children[0];
        assert_eq!(function.kind, SemanticObjectKind::Function);
        assert_eq!(function.name, "launch");
        assert_eq!(function.detail.as_deref(), Some("fn launch(target,speed)"));
        assert!(
            function
                .children
                .iter()
                .any(|child| child.name == "target" && child.kind == SemanticObjectKind::Parameter)
        );
        assert!(
            function
                .children
                .iter()
                .any(|child| child.name == "thrust" && child.kind == SemanticObjectKind::Field)
        );
        assert!(
            function
                .children
                .iter()
                .any(|child| child.name == "label" && child.kind == SemanticObjectKind::Constant)
        );
    }

    #[test]
    fn comments_and_string_literals_do_not_create_false_rhai_declarations() {
        let source = "// fn fake() {}\nlet note = \"fn also_fake() {}\"; /* const hidden = 1; */ fn real() {}";
        let objects = parse_rhai_semantic_objects(Path::new("scripts/main.rhai"), source);
        let names = objects[0]
            .children
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["note", "real"]);
    }

    #[test]
    fn method_calls_named_fn_are_not_classified_as_function_declarations() {
        let source = "let value = target.fn(speed); fn real() {}";
        let objects = parse_rhai_semantic_objects(Path::new("scripts/main.rhai"), source);
        let names = objects[0]
            .children
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["value", "real"]);
    }

    #[test]
    fn rhai_outlines_use_utf16_ranges_for_unicode_source() {
        let source = "// 🚀\nfn launch(船) {}";
        let function = &parse_rhai_semantic_objects(Path::new("main.rhai"), source)[0].children[0];
        assert_eq!(function.selection_range.start.line, 1);
        assert_eq!(function.selection_range.start.character, 3);
    }

    #[test]
    fn dense_rhai_outline_is_bounded_and_announces_partial_indexing() {
        let source = (0..5_000)
            .map(|index| format!("let value_{index} = {index};\n"))
            .collect::<String>();
        let objects = parse_rhai_semantic_objects(Path::new("dense.rhai"), &source);
        assert!(semantic_object_count(&objects) <= crate::semantic::MAX_SEMANTIC_OBJECTS);
        assert!(
            objects[0]
                .attributes
                .iter()
                .any(|attribute| attribute.contains("outline truncated"))
        );
    }

    #[test]
    fn many_rhai_functions_keep_body_bindings_nested_without_duplicate_top_level_nodes() {
        let source = (0..3_000)
            .map(|index| format!("fn function_{index}() {{ let local_{index} = {index}; }}\n"))
            .collect::<String>();
        let objects = parse_rhai_semantic_objects(Path::new("many-functions.rhai"), &source);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].children[0].name, "function_0");
        assert_eq!(objects[0].children[0].children[0].name, "local_0");
        assert!(semantic_object_count(&objects) <= crate::semantic::MAX_SEMANTIC_OBJECTS);
    }

    #[test]
    fn token_limit_suppresses_untrustworthy_partial_delimiter_diagnostics() {
        let source = "(".repeat(super::MAX_RHAI_TOKENS + 1);
        let diagnostics = super::rhai_structure_diagnostics(Path::new("large.rhai"), &source);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code.as_deref(),
            Some("vapor.semantic.token-limit")
        );
    }

    fn semantic_object_count(objects: &[SemanticObject]) -> usize {
        objects
            .iter()
            .map(|object| 1 + semantic_object_count(&object.children))
            .sum()
    }

    #[test]
    fn structural_diagnostics_ignore_delimiters_inside_comments_and_strings() {
        let source = "let text = \"( [ {\"; // } )\nfn launch() { let speed = 1; }";
        assert!(super::rhai_structure_diagnostics(Path::new("main.rhai"), source).is_empty());
    }

    #[test]
    fn structural_diagnostics_point_to_unclosed_and_unexpected_delimiters() {
        let source = "fn launch() { let speed = (1; ]";
        let diagnostics = super::rhai_structure_diagnostics(Path::new("main.rhai"), source);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "Mismatched delimiter: ']' closes '('")
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "Unclosed '(' delimiter")
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "Unclosed '{' delimiter")
        );
        let structural = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_deref() == Some("rhai.structure"))
            .collect::<Vec<_>>();
        assert!(!structural.is_empty());
        assert!(
            structural
                .iter()
                .all(|diagnostic| diagnostic.raw_payload == diagnostic.message)
        );
    }

    #[test]
    fn structural_diagnostics_use_rhai_parser_for_invalid_expressions() {
        let source = "let speed = ;";
        let diagnostics = super::rhai_structure_diagnostics(Path::new("main.rhai"), source);

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("rhai.parse")
                && diagnostic.primary.is_some()
                && diagnostic.context.domain.as_deref() == Some("rhai")
        }));
    }

    #[test]
    fn parser_diagnostic_positions_are_mapped_after_unicode_lines() {
        let source = "// 🚀\nlet speed = ;";
        let diagnostic = super::rhai_structure_diagnostics(Path::new("main.rhai"), source)
            .into_iter()
            .find(|diagnostic| diagnostic.code.as_deref() == Some("rhai.parse"))
            .expect("Rhai parser diagnostic");
        let span = diagnostic.primary.expect("parser source location");

        assert_eq!(span.start_line, 2);
        assert_eq!(span.start_column, 13);
    }

    #[test]
    fn parser_diagnostic_columns_are_mapped_after_unicode_on_the_same_line() {
        let source = "let rocket = \"🚀\"; let speed = ;";
        let diagnostic = super::rhai_structure_diagnostics(Path::new("main.rhai"), source)
            .into_iter()
            .find(|diagnostic| diagnostic.code.as_deref() == Some("rhai.parse"))
            .expect("Rhai parser diagnostic");
        let span = diagnostic.primary.expect("parser source location");

        assert_eq!(span.start_line, 1);
        assert_eq!(span.start_column, 32);
    }

    #[test]
    fn parser_does_not_duplicate_a_precise_delimiter_diagnostic() {
        let diagnostics =
            super::rhai_structure_diagnostics(Path::new("main.rhai"), "fn launch() { ] }");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("rhai.structure"))
        );
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("rhai.parse"))
        );
    }

    #[test]
    fn oversized_rhai_sources_skip_local_tokenization() {
        let source = format!(
            "{}fn should_not_be_indexed() {{}}",
            " ".repeat(crate::semantic::MAX_SEMANTIC_DOCUMENT_BYTES)
        );
        assert!(parse_rhai_semantic_objects(Path::new("large.rhai"), &source).is_empty());
        assert!(super::rhai_structure_diagnostics(Path::new("large.rhai"), &source).is_empty());
    }
}
