use eframe::egui;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};
use vapor_gui_core::semantic::{
    SemanticObject, SemanticObjectKind, SemanticObjectRef, SemanticWorkspaceIndex,
};

pub(super) const MAX_VISIBLE_SEMANTIC_OBJECTS: usize = 2_048;

pub(super) struct SemanticRenderBudget {
    remaining: usize,
    truncated: bool,
}

pub(super) struct SemanticFilterMatches {
    query_is_empty: bool,
    direct: HashSet<vapor_gui_core::semantic::SemanticObjectId>,
    visible: HashSet<vapor_gui_core::semantic::SemanticObjectId>,
}

struct CachedDocumentMatches {
    version: u64,
    query: String,
    matches: SemanticFilterMatches,
}

#[derive(Default)]
pub(super) struct SemanticMatchCache {
    documents: BTreeMap<PathBuf, CachedDocumentMatches>,
    #[cfg(test)]
    build_count: usize,
}

impl SemanticMatchCache {
    pub(super) fn prepare(&mut self, index: &SemanticWorkspaceIndex, filter: &str) {
        let query = filter.trim().to_lowercase();
        self.documents
            .retain(|path, _| index.document(path).is_some());
        for document in index.documents() {
            let cache_is_current = self
                .documents
                .get(&document.path)
                .is_some_and(|cached| cached.version == document.version && cached.query == query);
            if !cache_is_current {
                self.documents.insert(
                    document.path.clone(),
                    CachedDocumentMatches {
                        version: document.version,
                        query: query.clone(),
                        matches: SemanticFilterMatches::build(&document.objects, &query),
                    },
                );
                #[cfg(test)]
                {
                    self.build_count += 1;
                }
            }
        }
    }

    pub(super) fn get(&self, path: &Path) -> Option<&SemanticFilterMatches> {
        self.documents.get(path).map(|cached| &cached.matches)
    }
}

impl SemanticFilterMatches {
    fn build(objects: &[SemanticObject], filter: &str) -> Self {
        if filter.is_empty() {
            return Self {
                query_is_empty: true,
                direct: HashSet::new(),
                visible: HashSet::new(),
            };
        }
        let query = filter.to_lowercase();
        let mut matches = Self {
            query_is_empty: false,
            direct: HashSet::new(),
            visible: HashSet::new(),
        };
        collect_filter_matches(objects, &query, &mut matches);
        matches
    }

    fn is_visible(&self, id: &vapor_gui_core::semantic::SemanticObjectId) -> bool {
        self.query_is_empty || self.visible.contains(id)
    }

    fn directly_matches(&self, id: &vapor_gui_core::semantic::SemanticObjectId) -> bool {
        self.query_is_empty || self.direct.contains(id)
    }
}

fn collect_filter_matches(
    objects: &[SemanticObject],
    query: &str,
    matches: &mut SemanticFilterMatches,
) -> bool {
    let mut any_match = false;
    for object in objects {
        let direct = semantic_object_matches_lowered(object, query);
        let child_match = collect_filter_matches(&object.children, query, matches);
        if direct {
            matches.direct.insert(object.id.clone());
        }
        if direct || child_match {
            matches.visible.insert(object.id.clone());
            any_match = true;
        }
    }
    any_match
}

impl Default for SemanticRenderBudget {
    fn default() -> Self {
        Self::with_limit(MAX_VISIBLE_SEMANTIC_OBJECTS)
    }
}

impl SemanticRenderBudget {
    fn with_limit(limit: usize) -> Self {
        Self {
            remaining: limit,
            truncated: false,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.truncated = true;
            false
        } else {
            self.remaining -= 1;
            true
        }
    }

    pub(super) fn was_truncated(&self) -> bool {
        self.truncated
    }
}

pub(super) fn has_semantic_outline(objects: &[SemanticObject]) -> bool {
    !objects.is_empty()
}

pub(super) fn semantic_explorer_guidance(
    open_documents: usize,
    language_server_ready: bool,
) -> String {
    let indexed = match open_documents {
        0 => "Open a supported file to see its semantic outline.".to_owned(),
        1 => "1 open document has a semantic outline.".to_owned(),
        count => format!("{count} open documents have semantic outlines."),
    };
    if language_server_ready {
        format!(
            "{indexed} Search also finds workspace-wide Rust declarations; open-file outlines cover the other supported formats."
        )
    } else {
        format!(
            "{indexed} Rust Analyzer is not connected, so workspace-wide Rust search is unavailable."
        )
    }
}

pub(super) fn draw_semantic_outline(
    ui: &mut egui::Ui,
    objects: &[SemanticObject],
    document_version: u64,
    matches: &SemanticFilterMatches,
    selected: Option<&SemanticObjectRef>,
    requested: &mut Option<SemanticObjectRef>,
    budget: &mut SemanticRenderBudget,
) {
    for object in objects {
        draw_semantic_object(
            ui,
            object,
            matches,
            document_version,
            selected,
            requested,
            budget,
        );
    }
}

fn draw_semantic_object(
    ui: &mut egui::Ui,
    object: &SemanticObject,
    matches: &SemanticFilterMatches,
    document_version: u64,
    selected: Option<&SemanticObjectRef>,
    requested: &mut Option<SemanticObjectRef>,
    budget: &mut SemanticRenderBudget,
) {
    if budget.remaining == 0 {
        budget.truncated = true;
        return;
    }
    if !matches.is_visible(&object.id) {
        return;
    }
    if !budget.take() {
        return;
    }
    let label = format!("{}  {}", object.kind.label(), object.name);
    let is_selected = selected.is_some_and(|selected| selected.id == object.id);
    if object.children.is_empty() {
        if ui.selectable_label(is_selected, label).clicked() {
            *requested = Some(object.reference(document_version));
        }
        return;
    }
    let response = egui::CollapsingHeader::new(label)
        .id_salt(("semantic-object", &object.id))
        .default_open(!matches.query_is_empty)
        .show(ui, |ui| {
            for child in &object.children {
                draw_semantic_object(
                    ui,
                    child,
                    matches,
                    document_version,
                    selected,
                    requested,
                    budget,
                );
            }
        });
    if matches.directly_matches(&object.id) && response.header_response.clicked() {
        *requested = Some(object.reference(document_version));
    }
    response.header_response.on_hover_text(format!(
        "{} · {}",
        object.id.qualified_name(),
        object.detail.as_deref().unwrap_or(object.kind.label())
    ));
}

fn semantic_object_matches_lowered(object: &SemanticObject, filter: &str) -> bool {
    contains_case_insensitive(&object.name, filter)
        || contains_case_insensitive(&object.id.qualified_name(), filter)
        || contains_case_insensitive(&object.id.document.to_string_lossy(), filter)
        || contains_case_insensitive(object.kind.label(), filter)
        || object
            .detail
            .as_deref()
            .is_some_and(|detail| contains_case_insensitive(detail, filter))
        || object
            .attributes
            .iter()
            .any(|attribute| contains_case_insensitive(attribute, filter))
}

fn contains_case_insensitive(value: &str, lowered_query: &str) -> bool {
    if lowered_query.is_ascii() && value.is_ascii() {
        return value
            .as_bytes()
            .windows(lowered_query.len())
            .any(|candidate| candidate.eq_ignore_ascii_case(lowered_query.as_bytes()));
    }
    value.to_lowercase().contains(lowered_query)
}

#[cfg(test)]
pub(super) fn semantic_tree_matches(object: &SemanticObject, filter: &str) -> bool {
    SemanticFilterMatches::build(std::slice::from_ref(object), filter).is_visible(&object.id)
}

pub(super) fn draw_semantic_members(
    ui: &mut egui::Ui,
    members: &[SemanticObject],
    version: u64,
    selected: Option<&SemanticObjectRef>,
    requested: &mut Option<SemanticObjectRef>,
) {
    for member in members {
        let reference = member.reference(version);
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    selected == Some(&reference),
                    format!("{}  {}", member.kind.label(), member.name),
                )
                .clicked()
            {
                *requested = Some(reference);
            }
            if let Some(detail) = &member.detail {
                ui.weak(detail);
            }
        });
        if !member.children.is_empty() {
            egui::CollapsingHeader::new(format!("{} nested members", member.children.len()))
                .id_salt(("semantic-inspector-members", &member.id))
                .show(ui, |ui| {
                    ui.indent(("semantic-inspector-depth", &member.id), |ui| {
                        draw_semantic_members(ui, &member.children, version, selected, requested);
                    });
                });
        }
    }
}

pub(super) fn semantic_convention_status(object: &SemanticObject) -> Option<String> {
    let marker = match object.kind {
        SemanticObjectKind::Component => "Component",
        SemanticObjectKind::Resource => "Resource",
        SemanticObjectKind::Event => "Event",
        _ => return None,
    };
    let explicit = object.attributes.iter().find(|attribute| {
        if *attribute == "#[component]" {
            return marker == "Component";
        }
        let Some(derives) = attribute
            .strip_prefix("#[derive(")
            .and_then(|attribute| attribute.strip_suffix(")]"))
        else {
            return false;
        };
        derives.split(',').any(|derive| {
            derive
                .trim()
                .rsplit("::")
                .next()
                .is_some_and(|name| name.trim() == marker)
        })
    });
    Some(explicit.map_or_else(
        || format!("Classified as {marker} by Rust syntax metadata"),
        |attribute| format!("Detected from {attribute}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        SemanticFilterMatches, SemanticMatchCache, SemanticRenderBudget, semantic_explorer_guidance,
    };
    use std::path::Path;
    use vapor_gui_core::semantic::{
        SemanticObject, SemanticObjectId, SemanticObjectKind, SemanticRange, SemanticWorkspaceIndex,
    };

    fn object(name: &str, children: Vec<SemanticObject>) -> SemanticObject {
        SemanticObject {
            id: SemanticObjectId::new("rust", "src/components.rs", vec![name.to_owned()]),
            kind: SemanticObjectKind::Component,
            name: name.to_owned(),
            detail: None,
            range: SemanticRange::default(),
            selection_range: SemanticRange::default(),
            attributes: Vec::new(),
            children,
        }
    }

    #[test]
    fn semantic_explorer_guidance_distinguishes_open_file_outlines_from_workspace_search() {
        let connected = semantic_explorer_guidance(2, true);
        assert!(connected.contains("2 open documents"));
        assert!(connected.contains("workspace-wide Rust declarations"));
        assert!(connected.contains("open-file outlines"));

        let no_open_files = semantic_explorer_guidance(0, true);
        assert!(no_open_files.contains("Open a supported file"));
        assert!(no_open_files.contains("workspace-wide Rust declarations"));

        let disconnected = semantic_explorer_guidance(1, false);
        assert!(disconnected.contains("1 open document"));
        assert!(disconnected.contains("Rust Analyzer is not connected"));
    }

    #[test]
    fn semantic_filter_indexes_matching_nodes_and_only_their_ancestor_path() {
        let matching_field = object("velocity", Vec::new());
        let unrelated_field = object("health", Vec::new());
        let ship = object(
            "Ship",
            vec![matching_field.clone(), unrelated_field.clone()],
        );
        let other = object("Station", Vec::new());

        let matches = SemanticFilterMatches::build(&[ship.clone(), other.clone()], "VELO");
        assert!(matches.is_visible(&ship.id));
        assert!(matches.directly_matches(&matching_field.id));
        assert!(!matches.directly_matches(&ship.id));
        assert!(!matches.is_visible(&other.id));

        let all = SemanticFilterMatches::build(&[ship.clone(), other.clone()], "");
        assert!(all.is_visible(&unrelated_field.id));
        assert!(all.directly_matches(&ship.id));
    }

    #[test]
    fn semantic_match_cache_reuses_results_until_query_or_document_revision_changes() {
        let mut index = SemanticWorkspaceIndex::default();
        index.replace_document(
            "src/components.rs",
            1,
            vec![object("Ship", vec![object("velocity", Vec::new())])],
        );
        let mut cache = SemanticMatchCache::default();
        cache.prepare(&index, "velocity");
        assert_eq!(cache.build_count, 1);
        cache.prepare(&index, "velocity");
        assert_eq!(cache.build_count, 1);
        assert!(
            cache
                .get(Path::new("src/components.rs"))
                .unwrap()
                .is_visible(&SemanticObjectId::new(
                    "rust",
                    "src/components.rs",
                    vec!["Ship".to_owned()]
                ))
        );

        index.replace_document(
            "src/components.rs",
            2,
            vec![object("Ship", vec![object("rotation", Vec::new())])],
        );
        cache.prepare(&index, "velocity");
        assert_eq!(cache.build_count, 2);
        assert!(
            !cache
                .get(Path::new("src/components.rs"))
                .unwrap()
                .is_visible(&SemanticObjectId::new(
                    "rust",
                    "src/components.rs",
                    vec!["Ship".to_owned()]
                ))
        );

        index.invalidate_document(Path::new("src/components.rs"));
        cache.prepare(&index, "velocity");
        assert!(cache.get(Path::new("src/components.rs")).is_none());
    }

    #[test]
    fn semantic_outline_render_budget_stops_tree_expansion_at_a_global_limit() {
        let mut budget = SemanticRenderBudget::with_limit(2);
        assert!(budget.take());
        assert!(budget.take());
        assert!(!budget.take());
        assert!(budget.was_truncated());
    }

    #[test]
    fn empty_semantic_documents_do_not_claim_to_have_an_outline() {
        assert!(!super::has_semantic_outline(&[]));
        assert!(super::has_semantic_outline(&[semantic_object()]));
    }

    fn semantic_object() -> vapor_gui_core::semantic::SemanticObject {
        use vapor_gui_core::semantic::{
            SemanticObject, SemanticObjectId, SemanticObjectKind, SemanticRange,
        };
        SemanticObject {
            id: SemanticObjectId::new("toml", "Cargo.toml", vec!["package".to_owned()]),
            kind: SemanticObjectKind::Configuration,
            name: "package".to_owned(),
            detail: Some("table".to_owned()),
            range: SemanticRange::default(),
            selection_range: SemanticRange::default(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }
}
