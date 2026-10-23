use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use vapor_client_protocol::semantic::*;

/// Maximum source size for local semantic projections. Large files stay
/// editable, but domain adapters avoid unbounded UI-thread parsing.
pub const MAX_SEMANTIC_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

/// Upper bound shared by domain adapters for selectable objects in one file.
pub const MAX_SEMANTIC_OBJECTS: usize = 4_096;

/// Maximum nesting supported by local semantic projections.
pub const MAX_SEMANTIC_DEPTH: usize = 64;

/// Mark the first visible node when a local adapter publishes a partial tree.
pub fn mark_semantic_outline_truncated(objects: &mut [SemanticObject]) {
    if let Some(root) = objects.first_mut() {
        root.attributes.push(format!(
            "Semantic outline truncated at {} objects or {} levels",
            MAX_SEMANTIC_OBJECTS, MAX_SEMANTIC_DEPTH,
        ));
    }
}

/// Versioned per-document index. Replacing a document invalidates every prior
/// source anchor for that document before publishing the new semantic tree.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticWorkspaceIndex {
    documents: BTreeMap<PathBuf, SemanticDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticDocument {
    pub path: PathBuf,
    pub version: u64,
    pub objects: Vec<SemanticObject>,
}

impl SemanticWorkspaceIndex {
    pub fn replace_document(
        &mut self,
        path: impl Into<PathBuf>,
        version: u64,
        mut objects: Vec<SemanticObject>,
    ) -> bool {
        let path = path.into();
        if self
            .documents
            .get(&path)
            .is_some_and(|current| current.version > version)
        {
            return false;
        }
        let mut object_count = 0;
        let truncated = bound_semantic_outline(&mut objects, &mut object_count, 0);
        if !objects_belong_to_document(&objects, &path) {
            return false;
        }
        if truncated {
            mark_semantic_outline_truncated(&mut objects);
        }
        self.documents.insert(
            path.clone(),
            SemanticDocument {
                path,
                version,
                objects,
            },
        );
        true
    }

    pub fn invalidate_document(&mut self, path: &Path) {
        self.documents.remove(path);
    }

    pub fn document(&self, path: &Path) -> Option<&SemanticDocument> {
        self.documents.get(path)
    }

    pub fn documents(&self) -> impl Iterator<Item = &SemanticDocument> {
        self.documents.values()
    }

    pub fn resolve(&self, reference: &SemanticObjectRef) -> Option<&SemanticObject> {
        let document = self.documents.get(&reference.id.document)?;
        if document.version != reference.document_version {
            return None;
        }
        find_object(&document.objects, &reference.id)
    }
}

fn bound_semantic_outline(
    objects: &mut Vec<SemanticObject>,
    object_count: &mut usize,
    depth: usize,
) -> bool {
    let mut truncated = false;
    let mut index = 0;
    while index < objects.len() {
        if *object_count >= MAX_SEMANTIC_OBJECTS {
            objects.truncate(index);
            truncated = true;
            break;
        }
        *object_count += 1;
        let object = &mut objects[index];
        if depth + 1 >= MAX_SEMANTIC_DEPTH {
            if !object.children.is_empty() {
                object.children.clear();
                truncated = true;
            }
        } else if bound_semantic_outline(&mut object.children, object_count, depth + 1) {
            truncated = true;
        }
        index += 1;
    }
    truncated
}

fn objects_belong_to_document(objects: &[SemanticObject], path: &Path) -> bool {
    let mut pending = objects.iter().collect::<Vec<_>>();
    while let Some(object) = pending.pop() {
        if object.id.document != path {
            return false;
        }
        pending.extend(object.children.iter());
    }
    true
}

fn find_object<'a>(
    objects: &'a [SemanticObject],
    id: &SemanticObjectId,
) -> Option<&'a SemanticObject> {
    let mut pending = objects.iter().collect::<Vec<_>>();
    while let Some(object) = pending.pop() {
        if &object.id == id {
            return Some(object);
        }
        pending.extend(object.children.iter());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(document: &str, name: &str, child: Option<SemanticObject>) -> SemanticObject {
        SemanticObject {
            id: SemanticObjectId::new("rust", document, vec![name.to_owned()]),
            kind: SemanticObjectKind::Component,
            name: name.to_owned(),
            detail: None,
            range: SemanticRange::default(),
            selection_range: SemanticRange::default(),
            attributes: vec!["derive(Component)".to_owned()],
            children: child.into_iter().collect(),
        }
    }

    #[test]
    fn semantic_index_round_trips_workspace_identity_and_source_revision() {
        let mut index = SemanticWorkspaceIndex::default();
        index.replace_document(
            "src/components.rs",
            7,
            vec![object("src/components.rs", "Ship", None)],
        );

        let encoded = serde_json::to_string(&index).expect("serialize semantic index");
        let decoded: SemanticWorkspaceIndex =
            serde_json::from_str(&encoded).expect("deserialize semantic index");

        assert_eq!(decoded, index);
        assert_eq!(
            decoded
                .document(Path::new("src/components.rs"))
                .unwrap()
                .version,
            7
        );
    }

    #[test]
    fn semantic_ids_are_domain_and_document_scoped() {
        let a = SemanticObjectId::new("rust", "src/components.rs", vec!["Ship".into()]);
        let b = SemanticObjectId::new("rhai", "src/components.rs", vec!["Ship".into()]);
        assert_ne!(a, b);
        assert_eq!(a.qualified_name(), "Ship");
    }

    #[test]
    fn qualified_names_use_each_domain_syntax() {
        let rust = SemanticObjectId::new("rust", "src/lib.rs", vec!["game".into(), "Ship".into()]);
        let toml = SemanticObjectId::new(
            "vapor-workspace",
            "Workspace.vapor.toml",
            vec![
                "workspace".into(),
                "projects".into(),
                "[0]".into(),
                "path".into(),
            ],
        );

        assert_eq!(rust.qualified_name(), "game::Ship");
        assert_eq!(toml.qualified_name(), "workspace.projects[0].path");
    }

    #[test]
    fn same_named_symbols_can_have_distinct_semantic_instances() {
        let first = SemanticObjectId::with_discriminator(
            "rust",
            "src/lib.rs",
            vec!["impl Ship".to_owned(), "tick".to_owned()],
            "line:4:method",
        );
        let second = SemanticObjectId::with_discriminator(
            "rust",
            "src/lib.rs",
            vec!["impl Ship".to_owned(), "tick".to_owned()],
            "line:12:method",
        );
        assert_ne!(first, second);
        assert_eq!(first.qualified_name(), "impl Ship::tick");
    }

    #[test]
    fn document_replacement_invalidates_old_source_anchors() {
        let mut index = SemanticWorkspaceIndex::default();
        let first = object("src/components.rs", "Ship", None);
        let old_ref = first.reference(3);
        index.replace_document("src/components.rs", 3, vec![first]);
        assert!(index.resolve(&old_ref).is_some());

        index.invalidate_document(Path::new("src/components.rs"));
        assert!(index.resolve(&old_ref).is_none());
    }

    #[test]
    fn stale_document_replacement_cannot_overwrite_a_newer_index() {
        let mut index = SemanticWorkspaceIndex::default();
        assert!(index.replace_document(
            "src/components.rs",
            8,
            vec![object("src/components.rs", "Current", None)],
        ));
        assert!(!index.replace_document(
            "src/components.rs",
            7,
            vec![object("src/components.rs", "Stale", None)],
        ));

        let document = index.document(Path::new("src/components.rs")).unwrap();
        assert_eq!(document.version, 8);
        assert_eq!(document.objects[0].name, "Current");
    }

    #[test]
    fn semantic_index_enforces_object_and_depth_limits_at_its_boundary() {
        let mut many = (0..=MAX_SEMANTIC_OBJECTS)
            .map(|index| object("src/components.rs", &format!("C{index}"), None))
            .collect::<Vec<_>>();
        let mut index = SemanticWorkspaceIndex::default();
        assert!(index.replace_document("src/components.rs", 1, std::mem::take(&mut many)));
        let document = index.document(Path::new("src/components.rs")).unwrap();
        assert_eq!(document.objects.len(), MAX_SEMANTIC_OBJECTS);
        assert!(
            document.objects[0]
                .attributes
                .iter()
                .any(|attribute| attribute.contains("truncated"))
        );

        let mut nested = object("src/components.rs", "depth-0", None);
        let mut cursor = &mut nested;
        for depth in 1..=MAX_SEMANTIC_DEPTH + 2 {
            cursor
                .children
                .push(object("src/components.rs", &format!("depth-{depth}"), None));
            cursor = cursor.children.last_mut().unwrap();
        }
        assert!(index.replace_document("src/components.rs", 2, vec![nested]));
        let document = index.document(Path::new("src/components.rs")).unwrap();
        let mut current = &document.objects[0];
        let mut depth = 1;
        while let Some(child) = current.children.first() {
            depth += 1;
            current = child;
        }
        assert!(depth <= MAX_SEMANTIC_DEPTH);
        assert!(
            document.objects[0]
                .attributes
                .iter()
                .any(|attribute| attribute.contains("truncated"))
        );
    }

    #[test]
    fn semantic_index_rejects_objects_anchored_to_another_document() {
        let mut index = SemanticWorkspaceIndex::default();
        let wrong_document = object("src/other.rs", "Ship", None);
        assert!(!index.replace_document("src/components.rs", 1, vec![wrong_document],));
        assert!(index.document(Path::new("src/components.rs")).is_none());
    }

    #[test]
    fn nested_objects_resolve_only_at_the_current_document_version() {
        let field = object("src/components.rs", "velocity", None);
        let component = object("src/components.rs", "Ship", Some(field.clone()));
        let field_ref = field.reference(4);
        let mut index = SemanticWorkspaceIndex::default();
        index.replace_document("src/components.rs", 4, vec![component]);
        assert_eq!(
            index.resolve(&field_ref).map(|item| item.name.as_str()),
            Some("velocity")
        );

        index.replace_document("src/components.rs", 5, Vec::new());
        assert!(index.resolve(&field_ref).is_none());
    }
}
