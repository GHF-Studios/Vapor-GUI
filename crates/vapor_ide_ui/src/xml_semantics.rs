use std::path::Path;

use vapor_gui_core::semantic::{SemanticObject, SemanticObjectId, SemanticObjectKind, semantic_range_from_byte_span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct XmlParseError {
    pub(super) offset: usize,
}

#[cfg(test)]
mod tests {
    use super::parse_xml_semantic_objects;
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
    fn xml_outline_indexes_nested_elements_and_attribute_names_without_values() {
        let source = r#"<project version="4"><component name="RustProjectSettings"><option name="toolchainHomeDirectory" value="/private/toolchain" /></component></project>"#;
        let objects = parse_xml_semantic_objects(Path::new("project.xml"), source)
            .expect("valid XML");

        let option = find(&objects, "project.component.option").expect("option node");
        assert_eq!(option.kind, vapor_gui_core::semantic::SemanticObjectKind::Configuration);
        assert!(find(&objects, "project.component.option.@name").is_some());
        let value = find(&objects, "project.component.option.@value").expect("value attribute name");
        assert_eq!(&source[value.selection_range.start.character as usize..][..5], "value");
        let all_names = format!("{objects:#?}");
        assert!(!all_names.contains("/private/toolchain"));
        assert!(option.range.end.line == 0);
        assert!(option.range.end.character > option.range.start.character);
    }

    #[test]
    fn repeated_siblings_get_distinct_semantic_paths_and_unicode_offsets() {
        let source = "<root><item name=\"one\"/><item name=\"é\"/></root>";
        let objects = parse_xml_semantic_objects(Path::new("config.xml"), source).expect("valid XML");
        assert!(find(&objects, "root.item").is_some());
        let second_item = find(&objects, "root.item[2]").expect("second item");
        assert_eq!(second_item.name, "item [2]");
        let second_name = find(&objects, "root.item[2].@name").expect("second attribute");
        assert!(second_name.range.start.character > 0);
        assert!(second_name.range.end.character > second_name.range.start.character);
    }

    #[test]
    fn xml_outline_handles_self_closing_elements_and_reports_malformed_input() {
        let objects = parse_xml_semantic_objects(Path::new("vapor.xml"), "<project><component name=\"Vapor\" /></project>")
            .expect("valid XML");
        assert!(find(&objects, "project.component").is_some());
        assert!(parse_xml_semantic_objects(Path::new("broken.xml"), "<project><component></project>").is_err());
    }

    #[test]
    fn dense_xml_projection_is_bounded_and_marks_the_partial_outline() {
        let source = format!("<root>{}</root>", "<item />".repeat(5_000));
        let objects = parse_xml_semantic_objects(Path::new("dense.xml"), &source)
            .expect("valid dense XML");
        assert!(semantic_object_count(&objects) <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
        assert!(objects[0]
            .attributes
            .iter()
            .any(|attribute| attribute.contains("outline truncated")));
    }

    #[test]
    fn deep_xml_projection_is_bounded_and_marks_the_partial_outline() {
        let source = format!("{}{}", "<node>".repeat(80), "</node>".repeat(80));
        let objects = parse_xml_semantic_objects(Path::new("deep.xml"), &source)
            .expect("valid nested XML");
        assert!(objects[0]
            .attributes
            .iter()
            .any(|attribute| attribute.contains("outline truncated")));
        assert!(semantic_object_count(&objects) <= vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS);
    }

    fn semantic_object_count(objects: &[SemanticObject]) -> usize {
        objects.iter().map(|object| 1 + semantic_object_count(&object.children)).sum()
    }
}

struct ElementFrame {
    object: SemanticObject,
    path: Vec<String>,
    child_counts: std::collections::BTreeMap<String, usize>,
}

struct SemanticBudget {
    remaining: usize,
    truncated: bool,
}

impl Default for SemanticBudget {
    fn default() -> Self {
        Self { remaining: vapor_gui_core::semantic::MAX_SEMANTIC_OBJECTS, truncated: false }
    }
}

impl SemanticBudget {
    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.truncated = true;
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}

pub fn parse_xml_semantic_objects(
    document: &Path,
    source: &str,
) -> Result<Vec<SemanticObject>, XmlParseError> {
    let mut reader = quick_xml::Reader::from_str(source);
    reader.config_mut().trim_text(false);
    let mut roots = Vec::new();
    let mut root_counts = std::collections::BTreeMap::new();
    let mut stack: Vec<ElementFrame> = Vec::new();
    let mut budget = SemanticBudget::default();
    let mut ignored_depth = 0_usize;
    loop {
        let event = reader.read_event().map_err(|_| XmlParseError {
            offset: reader.buffer_position() as usize,
        })?;
        let event_end = reader.buffer_position() as usize;
        match event {
            quick_xml::events::Event::Start(event) => {
                validate_xml_attributes(&event).map_err(|_| XmlParseError { offset: event_end })?;
                if ignored_depth > 0 {
                    ignored_depth += 1;
                    continue;
                }
                if stack.len() >= vapor_gui_core::semantic::MAX_SEMANTIC_DEPTH || !budget.take() {
                    budget.truncated = true;
                    ignored_depth = 1;
                    continue;
                }
                let (path, _) = next_element_path(&mut stack, &mut root_counts, &event);
                let object = element_object(document, source, event_end, &event, path.clone(), &mut budget)
                    .map_err(|_| XmlParseError { offset: event_end })?;
                stack.push(ElementFrame { object, path, child_counts: Default::default() });
            }
            quick_xml::events::Event::Empty(event) => {
                validate_xml_attributes(&event).map_err(|_| XmlParseError { offset: event_end })?;
                if ignored_depth > 0 || stack.len() >= vapor_gui_core::semantic::MAX_SEMANTIC_DEPTH || !budget.take() {
                    budget.truncated = true;
                    continue;
                }
                let (path, _) = next_element_path(&mut stack, &mut root_counts, &event);
                let object = element_object(document, source, event_end, &event, path.clone(), &mut budget)
                    .map_err(|_| XmlParseError { offset: event_end })?;
                append_object(&mut roots, &mut stack, ElementFrame { object, path, child_counts: Default::default() });
            }
            quick_xml::events::Event::End(_) => {
                if ignored_depth > 0 {
                    ignored_depth -= 1;
                } else if let Some(frame) = stack.pop() {
                    append_object(&mut roots, &mut stack, frame);
                }
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    while let Some(frame) = stack.pop() {
        append_object(&mut roots, &mut stack, frame);
    }
    let mut objects = roots.into_iter().map(|frame| frame.object).collect::<Vec<_>>();
    if budget.truncated {
        vapor_gui_core::semantic::mark_semantic_outline_truncated(&mut objects);
    }
    Ok(objects)
}

fn validate_xml_attributes(event: &quick_xml::events::BytesStart<'_>) -> Result<(), quick_xml::Error> {
    for attribute in event.attributes() { attribute?; }
    Ok(())
}

fn next_element_path(
    stack: &mut [ElementFrame],
    root_counts: &mut std::collections::BTreeMap<String, usize>,
    event: &quick_xml::events::BytesStart<'_>,
) -> (Vec<String>, String) {
    let display_name = String::from_utf8_lossy(event.local_name().as_ref()).into_owned();
    let qualified_name = String::from_utf8_lossy(event.name().as_ref()).into_owned();
    let count = if let Some(parent) = stack.last_mut() {
        let count = parent.child_counts.entry(display_name.clone()).or_default();
        *count += 1;
        *count
    } else {
        let count = root_counts.entry(display_name.clone()).or_default();
        *count += 1;
        *count
    };
    let segment = if count == 1 { qualified_name } else { format!("{qualified_name}[{count}]") };
    let mut path = stack.last().map_or_else(Vec::new, |parent| parent.path.clone());
    path.push(segment.clone());
    (path, segment)
}

fn append_object(roots: &mut Vec<ElementFrame>, stack: &mut [ElementFrame], frame: ElementFrame) {
    if let Some(parent) = stack.last_mut() {
        parent.object.children.push(frame.object);
    } else {
        roots.push(frame);
    }
}

fn element_object(
    document: &Path,
    source: &str,
    event_start: usize,
    event: &quick_xml::events::BytesStart<'_>,
    path: Vec<String>,
    budget: &mut SemanticBudget,
) -> Result<SemanticObject, quick_xml::Error> {
    let local_name = String::from_utf8_lossy(event.local_name().as_ref()).into_owned();
    let indexed_name = path.last().map(String::as_str).unwrap_or(&local_name);
    let name = indexed_name
        .rsplit_once('[')
        .and_then(|(base, occurrence)| {
            occurrence
                .strip_suffix(']')
                .and_then(|number| number.parse::<usize>().ok())
                .map(|number| format!("{base} [{number}]"))
        })
        .unwrap_or_else(|| local_name.clone());
    let raw_name = event.name().as_ref().to_vec();
    let event_end = event_start.min(source.len());
    let start = source
        .get(..event_end)
        .and_then(|prefix| prefix.rfind('<'))
        .unwrap_or(0);
    let end = source
        .get(start..)
        .and_then(|opening| opening.find('>').map(|offset| start + offset + 1))
        .unwrap_or(event_end);
    let name_span = locate_xml_name(source, start, end, &raw_name)
        .unwrap_or_else(|| start..end);
    let mut children = Vec::new();
    for attribute in event.attributes() {
        let attribute = attribute?;
        if !budget.take() { continue; }
        let attribute_name = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
        let qualified_attribute = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let span = locate_xml_attribute(source, name_span.end, end, attribute.key.as_ref())
            .unwrap_or_else(|| name_span.clone());
        let object_name = format!("@{attribute_name}");
        let mut attribute_path = path.clone();
        attribute_path.push(object_name.clone());
        let range = semantic_range_from_byte_span(source, span.clone());
        let selection_range = semantic_range_from_byte_span(source, span);
        children.push(SemanticObject {
            id: SemanticObjectId::with_discriminator("xml", document, attribute_path, format!("{}:{}", range.start.line, range.start.character)),
            kind: SemanticObjectKind::Other,
            name: object_name,
            detail: Some(format!("XML attribute · {qualified_attribute}")),
            range,
            selection_range,
            attributes: vec!["XML attribute".to_owned()],
            children: Vec::new(),
        });
    }
    let source_span = start..end;
    let range = semantic_range_from_byte_span(source, source_span);
    let selection_range = semantic_range_from_byte_span(source, name_span);
    Ok(SemanticObject {
        id: SemanticObjectId::with_discriminator("xml", document, path, format!("{}:{}", range.start.line, range.start.character)),
        kind: SemanticObjectKind::Configuration,
        name,
        detail: Some("XML element".to_owned()),
        range,
        selection_range,
        attributes: vec!["XML element".to_owned()],
        children,
    })
}

fn locate_xml_name(source: &str, start: usize, end: usize, name: &[u8]) -> Option<std::ops::Range<usize>> {
    let opening = source.get(start..end)?.as_bytes();
    let offset = opening.iter().position(|byte| *byte == b'<')? + 1;
    let local = opening.get(offset..)?.windows(name.len()).position(|window| window == name)? + offset;
    Some(start + local..start + local + name.len())
}

fn locate_xml_attribute(source: &str, start: usize, end: usize, name: &[u8]) -> Option<std::ops::Range<usize>> {
    let bytes = source.get(start..end)?.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) { cursor += 1; }
        let key_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'=' && *byte != b'/' && *byte != b'>') { cursor += 1; }
        if cursor > key_start && &bytes[key_start..cursor] == name {
            return Some(start + key_start..start + cursor);
        }
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) { cursor += 1; }
        if bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) { cursor += 1; }
            if let Some(quote @ (b'\'' | b'"')) = bytes.get(cursor) {
                cursor += 1;
                while bytes.get(cursor).is_some_and(|byte| byte != quote) { cursor += 1; }
                cursor += usize::from(cursor < bytes.len());
            } else {
                while bytes.get(cursor).is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>') { cursor += 1; }
            }
        } else if cursor == key_start {
            cursor += 1;
        }
    }
    None
}
