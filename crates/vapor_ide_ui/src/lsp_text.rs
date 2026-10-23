use super::*;

pub(super) fn file_uri(path: &Path) -> String {
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

pub(super) fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
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

pub(super) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(super) fn rust_analyzer_initialization_options() -> serde_json::Value {
    serde_json::json!({
        // Opening source remains analysis-only. Builds are explicit Vapor
        // operations so the user can see and control their work and output.
        "checkOnSave": false,
        "cargo": {"buildScripts": {"enable": false}},
        "procMacro": {"enable": false}
    })
}

pub(super) fn rust_analyzer_client_capabilities() -> serde_json::Value {
    serde_json::json!({
        "textDocument": {
            "publishDiagnostics": {"relatedInformation": true},
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
            "codeAction": {
                "dynamicRegistration": false,
                "codeActionLiteralSupport": {
                    "codeActionKind": {"valueSet": ["quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite"]}
                }
            },
            "synchronization": {"didSave": true}
        },
        "workspace": {
            "workspaceFolders": true,
            // Edits are previewed and applied to open buffers by the IDE. Rust Analyzer
            // must not write files behind the editor's revision and save tracking.
            "applyEdit": false,
            "workspaceEdit": {"documentChanges": true, "resourceOperations": []}
        }
    })
}

pub(super) fn relative_workspace_path(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let absolute = absolute.canonicalize().unwrap_or(absolute);
    absolute.strip_prefix(root).ok().map(Path::to_path_buf)
}

pub(super) fn reference_locations(result: &serde_json::Value, root: &Path) -> (Vec<ReferenceLocation>, usize) {
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

pub(super) fn lsp_position(source: &str, character_offset: usize) -> serde_json::Value {
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

pub(super) fn position_to_byte_offset(source: &str, line: usize, character: usize) -> Option<usize> {
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

pub(super) fn semantic_object_byte_range(
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

pub(super) fn apply_text_edits(source: &str, edits: &[serde_json::Value]) -> Result<String, &'static str> {
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

pub(super) fn hover_text(result: &serde_json::Value) -> Option<String> {
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

pub(super) fn completion_items(result: &serde_json::Value) -> Vec<CompletionItem> {
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

pub(super) fn identifier_byte_range(source: &str, character_offset: usize) -> (usize, usize) {
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

pub(super) fn find_matches(source: &str, query: &str) -> Vec<Range<usize>> {
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

pub(super) fn cursor_position(source: &str, character_index: usize) -> (usize, usize) {
    let before_cursor = source.chars().take(character_index).collect::<String>();
    let line = before_cursor.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = before_cursor
        .rsplit('\n')
        .next()
        .map_or(1, |line| line.chars().count() + 1);
    (line, column)
}
