use super::*;

pub(super) struct FileTreeView<'a> {
    pub(super) files: &'a [PathBuf],
    pub(super) documents: &'a BTreeMap<PathBuf, Document>,
    pub(super) selected: Option<&'a PathBuf>,
    pub(super) enabled: bool,
    pub(super) filter: &'a str,
    pub(super) rust_crate_roots: &'a BTreeSet<PathBuf>,
}

pub(super) fn draw_file_tree(
    ui: &mut egui::Ui,
    tree: &WorkspaceFileTree,
    parent: &Path,
    depth: usize,
    view: &FileTreeView<'_>,
    requested: &mut Option<PathBuf>,
) {
    let ordering = workspace_tree_ordering(parent, view.rust_crate_roots);
    for (name, entry) in tree.ordered_entries(ordering) {
        match entry {
            WorkspaceTreeEntry::Directory(directory) => {
                if !directory.contains_matching_file(view.filter) {
                    continue;
                }
                let path = parent.join(name);
                egui::CollapsingHeader::new(name)
                    .id_salt(("ide-tree-folder", &path, !view.filter.is_empty()))
                    .default_open(depth == 0 || !view.filter.is_empty())
                    .show(ui, |ui| {
                        draw_file_tree(ui, directory, &path, depth + 1, view, requested);
                    });
            }
            WorkspaceTreeEntry::File { index, .. } => {
                if !entry.matches_filter(view.filter) {
                    continue;
                }
                let Some(path) = view.files.get(*index) else {
                    continue;
                };
                let dirty = view.documents.get(path).is_some_and(Document::is_dirty);
                let response = ui.add_enabled(
                    view.enabled,
                    egui::Button::selectable(
                        view.selected == Some(path),
                        format!(
                            "{}{}",
                            file_display_label(path, view.files),
                            if dirty { " *" } else { "" }
                        ),
                    ),
                );
                if response.clicked() {
                    *requested = Some(path.clone());
                }
                response.on_hover_text(path.display().to_string());
            }
        }
    }
}

pub(super) fn reference_range(
    index: &SemanticWorkspaceIndex,
    reference: &SemanticObjectRef,
) -> Option<SemanticRange> {
    index
        .resolve(reference)
        .map(|object| object.selection_range)
}

pub(super) fn same_existing_workspace_path(left: &Path, right: &Path) -> bool {
    matches!(
        (left.canonicalize(), right.canonicalize()),
        (Ok(left), Ok(right)) if left == right
    )
}

pub(super) fn file_display_label(path: &Path, all_files: &[PathBuf]) -> String {
    if path.extension().is_some_and(|extension| extension == "rs") {
        shortest_rust_label(path, all_files)
    } else {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

pub(super) fn rust_label_parts(path: &Path) -> Vec<String> {
    let Some(file_name) = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return vec![path.display().to_string()];
    };
    let mut directories = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if let Some(src_index) = directories.iter().rposition(|part| part == "src") {
        directories = directories.split_off(src_index + 1);
    }
    if file_name == "mod.rs" {
        if directories.is_empty() {
            vec!["crate".to_owned()]
        } else {
            directories
        }
    } else {
        let stem = Path::new(&file_name).file_stem().map_or_else(
            || file_name.clone(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        directories.push(stem);
        directories
    }
}

fn rust_crate_label(path: &Path) -> Option<String> {
    let directories = path
        .parent()?
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let src_index = directories.iter().rposition(|part| part == "src")?;
    if src_index >= 2 && directories[src_index - 2] == "crates" {
        Some(directories[src_index - 1].clone())
    } else {
        directories.get(src_index.checked_sub(1)?).cloned()
    }
}

pub(super) fn canonical_rust_item_kind(
    document: &Path,
    crate_roots: &BTreeSet<PathBuf>,
) -> Option<rust_semantics::CanonicalRustItemKind> {
    let is_cargo_source = crate_roots.iter().any(|root| {
        document
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| component.as_os_str() == "src")
    });
    is_cargo_source
        .then(|| rust_semantics::CanonicalRustItemKind::for_document(document))
        .flatten()
}

pub(super) fn rust_crate_roots(files: &[PathBuf]) -> BTreeSet<PathBuf> {
    let manifests = files
        .iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "Cargo.toml"))
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    let mut roots = BTreeSet::new();
    for source in files
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
    {
        let nearest_manifest = manifests
            .iter()
            .filter(|root| source.starts_with(root))
            .max_by_key(|root| root.components().count());
        if let Some(root) = nearest_manifest
            && let Ok(relative) = source.strip_prefix(root)
        {
            let first = relative
                .components()
                .next()
                .and_then(|component| match component {
                    std::path::Component::Normal(name) => name.to_str(),
                    _ => None,
                });
            let cargo_source = matches!(first, Some("src" | "tests" | "benches" | "examples"))
                || relative == Path::new("build.rs");
            if cargo_source {
                roots.insert(root.clone());
            }
        }
    }
    roots
}

pub(super) fn workspace_tree_ordering(
    parent: &Path,
    rust_crate_roots: &BTreeSet<PathBuf>,
) -> WorkspaceTreeOrdering {
    if rust_crate_roots
        .iter()
        .any(|root| parent == root || parent.starts_with(root))
    {
        WorkspaceTreeOrdering::Alphabetical
    } else {
        WorkspaceTreeOrdering::DirectoriesFirst
    }
}

pub(super) fn shortest_rust_label(path: &Path, candidates: &[PathBuf]) -> String {
    let parts = rust_label_parts(path);
    for suffix_len in 1..=parts.len() {
        let suffix = parts[parts.len() - suffix_len..].join("/");
        let ambiguous = candidates
            .iter()
            .filter(|other| *other != path)
            .any(|other| {
                if other.extension().is_none_or(|extension| extension != "rs") {
                    return false;
                }
                let other_parts = rust_label_parts(other);
                other_parts.len() >= suffix_len
                    && other_parts[other_parts.len() - suffix_len..].join("/") == suffix
            });
        if !ambiguous {
            return suffix;
        }
    }
    let mut qualified = rust_crate_label(path).into_iter().collect::<Vec<_>>();
    qualified.extend(parts);
    let label = qualified.join("/");
    let fallback_still_collides = candidates
        .iter()
        .filter(|other| *other != path)
        .any(|other| {
            let mut other_qualified = rust_crate_label(other).into_iter().collect::<Vec<_>>();
            other_qualified.extend(rust_label_parts(other));
            other_qualified.join("/") == label
        });
    if path.file_name().is_some_and(|name| name == "mod.rs") && fallback_still_collides {
        qualified.push("mod".to_owned());
    }
    qualified.join("/")
}

pub(super) fn document_tab_label(
    path: &Path,
    open_documents: &BTreeMap<PathBuf, Document>,
) -> String {
    if path.extension().is_some_and(|extension| extension == "rs") {
        return shortest_rust_label(path, &open_documents.keys().cloned().collect::<Vec<_>>());
    }
    let parts = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for suffix_len in 1..=parts.len() {
        let suffix = parts[parts.len() - suffix_len..].join("/");
        let is_ambiguous = open_documents.keys().any(|other| {
            if other == path {
                return false;
            }
            let other_parts = other
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            other_parts.len() >= suffix_len
                && other_parts[other_parts.len() - suffix_len..].join("/") == suffix
        });
        if !is_ambiguous {
            return suffix;
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::same_existing_workspace_path;
    use std::path::Path;

    #[test]
    fn workspace_identity_requires_both_paths_to_exist() {
        let missing = std::env::temp_dir().join(format!(
            "vapor-missing-workspace-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        assert!(!same_existing_workspace_path(&missing, &missing));
    }

    #[test]
    fn workspace_identity_matches_canonical_existing_paths() {
        let root =
            std::env::temp_dir().join(format!("vapor-workspace-identity-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let alias = root.join(".");
        assert!(same_existing_workspace_path(&root, &alias));
        assert!(same_existing_workspace_path(&root, Path::new(&root)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
