use std::{
    collections::BTreeMap,
    path::{Component, PathBuf},
};

/// A stable, searchable hierarchy over paths returned by the Vapor workspace service.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTreeOrdering {
    #[default]
    DirectoriesFirst,
    Alphabetical,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkspaceFileTree {
    entries: BTreeMap<String, WorkspaceTreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTreeEntry {
    Directory(WorkspaceFileTree),
    File { index: usize, search_key: String },
}

impl WorkspaceTreeEntry {
    pub fn matches_filter(&self, filter: &str) -> bool {
        self.matches_filter_normalized(&filter.to_lowercase())
    }

    fn matches_filter_normalized(&self, filter: &str) -> bool {
        match self {
            Self::File { search_key, .. } => filter.is_empty() || search_key.contains(filter),
            Self::Directory(directory) => directory.contains_matching_file_normalized(filter),
        }
    }
}

impl WorkspaceFileTree {
    pub fn new(files: &[PathBuf]) -> Self {
        let mut tree = Self::default();
        for (index, path) in files.iter().enumerate() {
            let search_key = path.to_string_lossy().to_lowercase();
            let mut parts = Vec::new();
            let mut safe_relative_path = true;
            for part in path.components() {
                match part {
                    Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
                    _ => safe_relative_path = false,
                }
            }
            if !safe_relative_path {
                continue;
            }
            tree.insert(&parts, index, &search_key);
        }
        tree
    }

    fn insert(&mut self, parts: &[String], index: usize, search_key: &str) {
        let Some((name, rest)) = parts.split_first() else {
            return;
        };
        if rest.is_empty() {
            self.entries.insert(
                name.clone(),
                WorkspaceTreeEntry::File {
                    index,
                    search_key: search_key.to_owned(),
                },
            );
            return;
        }
        let entry = self
            .entries
            .entry(name.clone())
            .or_insert_with(|| WorkspaceTreeEntry::Directory(WorkspaceFileTree::default()));
        if let WorkspaceTreeEntry::Directory(directory) = entry {
            directory.insert(rest, index, search_key);
        }
    }

    pub fn entries(&self) -> &BTreeMap<String, WorkspaceTreeEntry> {
        &self.entries
    }

    /// Return a display order without changing the tree's canonical keyed storage.
    pub fn ordered_entries(&self, ordering: WorkspaceTreeOrdering) -> Vec<(&str, &WorkspaceTreeEntry)> {
        let mut entries = self.entries.iter().map(|(name, entry)| (name.as_str(), entry)).collect::<Vec<_>>();
        entries.sort_by(|(left_name, left), (right_name, right)| match ordering {
            WorkspaceTreeOrdering::Alphabetical => left_name.cmp(right_name),
            WorkspaceTreeOrdering::DirectoriesFirst => {
                let left_is_file = matches!(left, WorkspaceTreeEntry::File { .. });
                let right_is_file = matches!(right, WorkspaceTreeEntry::File { .. });
                left_is_file.cmp(&right_is_file).then_with(|| left_name.cmp(right_name))
            }
        });
        entries
    }

    pub fn contains_matching_file(&self, filter: &str) -> bool {
        self.contains_matching_file_normalized(&filter.to_lowercase())
    }

    fn contains_matching_file_normalized(&self, filter: &str) -> bool {
        self.entries.values().any(|entry| match entry {
            WorkspaceTreeEntry::Directory(directory) => {
                directory.contains_matching_file_normalized(filter)
            }
            WorkspaceTreeEntry::File { search_key, .. } => {
                filter.is_empty() || search_key.contains(filter)
            }
        })
    }

    pub fn matching_file_count(&self, filter: &str) -> usize {
        self.matching_file_count_normalized(&filter.to_lowercase())
    }

    fn matching_file_count_normalized(&self, filter: &str) -> usize {
        self.entries
            .values()
            .map(|entry| match entry {
                WorkspaceTreeEntry::Directory(directory) => {
                    directory.matching_file_count_normalized(filter)
                }
                WorkspaceTreeEntry::File { search_key, .. } => {
                    usize::from(filter.is_empty() || search_key.contains(filter))
                }
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkspaceFileTree, WorkspaceTreeEntry};
    use std::path::{Path, PathBuf};

    #[test]
    fn builds_sorted_nested_paths_and_searches_case_insensitively() {
        let files = [
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/game/player.rs"),
            PathBuf::from("Cargo.toml"),
        ];
        let tree = WorkspaceFileTree::new(&files);

        assert_eq!(tree.matching_file_count("PLAYER"), 1);
        assert_eq!(tree.matching_file_count("src"), 2);
        assert_eq!(tree.matching_file_count("missing"), 0);
        assert!(matches!(
            tree.entries().get("src"),
            Some(WorkspaceTreeEntry::Directory(_))
        ));
    }

    #[test]
    fn directory_first_and_rust_alphabetical_orders_are_explicit() {
        let files = [
            PathBuf::from("z_file.rs"),
            PathBuf::from("a_folder/mod.rs"),
            PathBuf::from("b_file.rs"),
            PathBuf::from("m_folder/mod.rs"),
        ];
        let tree = WorkspaceFileTree::new(&files);
        let folder_first = tree.ordered_entries(super::WorkspaceTreeOrdering::DirectoriesFirst)
            .into_iter().map(|(name, _)| name.to_owned()).collect::<Vec<_>>();
        assert_eq!(folder_first, ["a_folder", "m_folder", "b_file.rs", "z_file.rs"]);
        let alphabetical = tree.ordered_entries(super::WorkspaceTreeOrdering::Alphabetical)
            .into_iter().map(|(name, _)| name.to_owned()).collect::<Vec<_>>();
        assert_eq!(alphabetical, ["a_folder", "b_file.rs", "m_folder", "z_file.rs"]);
    }

    #[test]
    fn ignores_non_relative_components_in_service_paths() {
        let files = [
            Path::new("../secret.rs").to_path_buf(),
            PathBuf::from("src/lib.rs"),
        ];
        let tree = WorkspaceFileTree::new(&files);

        assert_eq!(tree.matching_file_count("lib.rs"), 1);
        assert_eq!(tree.matching_file_count("secret.rs"), 0);
    }
}
