use std::{path::Path, sync::Arc};

use collections::{HashMap, HashSet};
use gpui::SharedString;
use project::git_store::RepositoryId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepositoryTreeSource {
    Loaded(RepositoryId),
    Parked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepositoryTreeEntry {
    Folder,
    Repository(RepositoryTreeSource),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepositoryTreeRow {
    pub depth: usize,
    pub path: Arc<Path>,
    pub name: SharedString,
    /// The part of the path between the row and its parent row that no folder row shows.
    pub relative_path: Option<SharedString>,
    /// Path from the enclosing repository or worktree root, for labels shown outside the tree.
    pub location: Option<SharedString>,
    pub entry: RepositoryTreeEntry,
    pub has_children: bool,
}

struct Node {
    path: Arc<Path>,
    entry: RepositoryTreeEntry,
    location: Option<SharedString>,
    children: Vec<usize>,
}

pub(crate) fn build_repository_tree(
    loaded: &[(RepositoryId, Arc<Path>)],
    parked: &[Arc<Path>],
    worktree_roots: &[Arc<Path>],
) -> Vec<RepositoryTreeRow> {
    let mut entries: Vec<(Arc<Path>, RepositoryTreeSource)> = loaded
        .iter()
        .map(|(id, path)| (path.clone(), RepositoryTreeSource::Loaded(*id)))
        .collect();
    for path in parked {
        if !entries.iter().any(|(existing, _)| existing == path) {
            entries.push((path.clone(), RepositoryTreeSource::Parked));
        }
    }
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    entries.dedup_by(|(right, _), (left, _)| left == right);

    // Longest matching root wins so a root nested in another root claims its repositories.
    let containing_root = |path: &Path| -> Option<usize> {
        worktree_roots
            .iter()
            .enumerate()
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(index, _)| index)
    };

    let mut nodes: Vec<Node> = Vec::new();
    let mut top_level: Vec<(Option<usize>, usize)> = Vec::new();
    let mut folder_nodes: HashMap<Arc<Path>, usize> = HashMap::default();
    let mut entry_nodes: Vec<usize> = Vec::with_capacity(entries.len());
    let mut entry_roots: Vec<Option<usize>> = Vec::with_capacity(entries.len());

    let mut attach =
        |nodes: &mut Vec<Node>, parent: Option<usize>, root: Option<usize>, node: Node| {
            let index = nodes.len();
            nodes.push(node);
            match parent {
                Some(parent) => nodes[parent].children.push(index),
                None => top_level.push((root, index)),
            }
            index
        };

    // Entries are sorted by path, so every ancestor precedes its descendants.
    for (index, (path, source)) in entries.iter().enumerate() {
        let parent_index = (0..index)
            .filter(|&candidate| path.starts_with(&entries[candidate].0))
            .max_by_key(|&candidate| entries[candidate].0.components().count());

        let (base, mut parent, root) = match parent_index {
            Some(parent_index) => (
                Some(entries[parent_index].0.clone()),
                Some(entry_nodes[parent_index]),
                entry_roots[parent_index],
            ),
            None => {
                let root = containing_root(path);
                (root.map(|root| worktree_roots[root].clone()), None, root)
            }
        };
        entry_roots.push(root);

        let relative = base
            .as_deref()
            .and_then(|base| Some((base, path.strip_prefix(base).ok()?)));
        if let Some((base, relative)) = relative {
            let components = relative.components().collect::<Vec<_>>();
            let mut folder = base.to_path_buf();
            for component in components.iter().take(components.len().saturating_sub(1)) {
                folder.push(component);
                let folder_path: Arc<Path> = Arc::from(folder.as_path());
                let folder_node = match folder_nodes.get(&folder_path) {
                    Some(folder_node) => *folder_node,
                    None => {
                        let folder_node = attach(
                            &mut nodes,
                            parent,
                            root,
                            Node {
                                path: folder_path.clone(),
                                entry: RepositoryTreeEntry::Folder,
                                location: None,
                                children: Vec::new(),
                            },
                        );
                        folder_nodes.insert(folder_path, folder_node);
                        folder_node
                    }
                };
                parent = Some(folder_node);
            }
        }

        let location = relative
            .map(|(_, relative)| join_components(relative))
            .filter(|location| {
                !location.is_empty() && Some(location.as_str()) != file_name(path).as_deref()
            })
            .map(SharedString::from);
        let node = attach(
            &mut nodes,
            parent,
            root,
            Node {
                path: path.clone(),
                entry: RepositoryTreeEntry::Repository(*source),
                location,
                children: Vec::new(),
            },
        );
        entry_nodes.push(node);
    }

    // Repositories outside every worktree root sort after those inside one.
    top_level.sort_by(|(left_root, left), (right_root, right)| {
        left_root
            .unwrap_or(usize::MAX)
            .cmp(&right_root.unwrap_or(usize::MAX))
            .then_with(|| nodes[*left].path.cmp(&nodes[*right].path))
    });
    let mut rows = Vec::with_capacity(nodes.len());
    for (root, node) in top_level {
        let base = root.map(|root| worktree_roots[root].clone());
        push_rows(&nodes, node, 0, base.as_deref(), &mut rows);
    }
    rows
}

/// A folder with a single child adds nothing to the tree, so the child takes its place and
/// shows the skipped path next to its name.
fn push_rows(
    nodes: &[Node],
    index: usize,
    depth: usize,
    base: Option<&Path>,
    rows: &mut Vec<RepositoryTreeRow>,
) {
    let node = &nodes[index];
    if let ([only_child], RepositoryTreeEntry::Folder) = (node.children.as_slice(), node.entry) {
        push_rows(nodes, *only_child, depth, base, rows);
        return;
    }

    let relative = base
        .and_then(|base| node.path.strip_prefix(base).ok())
        .map(join_components)
        .filter(|relative| !relative.is_empty());
    let file_name = file_name(&node.path);
    let (name, relative_path) = match node.entry {
        RepositoryTreeEntry::Folder => (relative.or(file_name), None),
        RepositoryTreeEntry::Repository(_) => {
            let relative_path = relative.filter(|relative| Some(relative) != file_name.as_ref());
            (file_name, relative_path)
        }
    };
    let name = name.unwrap_or_else(|| node.path.to_string_lossy().into_owned());

    rows.push(RepositoryTreeRow {
        depth,
        path: node.path.clone(),
        name: name.into(),
        relative_path: relative_path.map(SharedString::from),
        location: node.location.clone(),
        entry: node.entry,
        has_children: !node.children.is_empty(),
    });

    let mut children = node.children.clone();
    children.sort_by(|left, right| nodes[*left].path.cmp(&nodes[*right].path));
    for child in children {
        push_rows(nodes, child, depth + 1, Some(&node.path), rows);
    }
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn join_components(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Rows left after hiding the descendants of collapsed rows.
pub(crate) fn visible_repository_tree_rows(
    rows: Vec<RepositoryTreeRow>,
    collapsed: &HashSet<Arc<Path>>,
) -> Vec<RepositoryTreeRow> {
    let mut hidden_below: Option<usize> = None;
    rows.into_iter()
        .filter(|row| {
            if let Some(depth) = hidden_below {
                if row.depth > depth {
                    return false;
                }
                hidden_below = None;
            }
            if row.has_children && collapsed.contains(&row.path) {
                hidden_below = Some(row.depth);
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(values: &[&str]) -> Vec<Arc<Path>> {
        values
            .iter()
            .map(|value| Arc::from(Path::new(value)))
            .collect()
    }

    fn loaded(values: &[&str]) -> Vec<(RepositoryId, Arc<Path>)> {
        paths(values)
            .into_iter()
            .enumerate()
            .map(|(index, path)| (RepositoryId(index as u64), path))
            .collect()
    }

    /// `(depth, name, relative_path, kind)`, kind being `'f'` for folders, `'p'` for parked
    /// and `'r'` for loaded repositories.
    fn summary(rows: &[RepositoryTreeRow]) -> Vec<(usize, &str, Option<&str>, char)> {
        rows.iter()
            .map(|row| {
                let kind = match row.entry {
                    RepositoryTreeEntry::Folder => 'f',
                    RepositoryTreeEntry::Repository(RepositoryTreeSource::Parked) => 'p',
                    RepositoryTreeEntry::Repository(RepositoryTreeSource::Loaded(_)) => 'r',
                };
                (
                    row.depth,
                    row.name.as_ref(),
                    row.relative_path.as_deref(),
                    kind,
                )
            })
            .collect()
    }

    #[test]
    fn test_flat_roots() {
        let rows = build_repository_tree(&loaded(&["/a", "/b"]), &[], &paths(&["/a", "/b"]));
        assert_eq!(
            summary(&rows),
            vec![(0, "a", None, 'r'), (0, "b", None, 'r')]
        );
    }

    #[test]
    fn test_repository_nested_in_repository() {
        let rows = build_repository_tree(
            &loaded(&["/root", "/root/libs/inner"]),
            &[],
            &paths(&["/root"]),
        );
        assert_eq!(
            summary(&rows),
            vec![
                (0, "root", None, 'r'),
                (1, "inner", Some("libs/inner"), 'r')
            ]
        );
    }

    #[test]
    fn test_repository_in_plain_folder() {
        let rows = build_repository_tree(
            &loaded(&["/root/nested/deep/repo2"]),
            &[],
            &paths(&["/root"]),
        );
        assert_eq!(
            summary(&rows),
            vec![(0, "repo2", Some("nested/deep/repo2"), 'r')]
        );
    }

    #[test]
    fn test_parked_rows() {
        let rows = build_repository_tree(
            &loaded(&["/root"]),
            &paths(&["/root/x/y"]),
            &paths(&["/root"]),
        );
        assert_eq!(
            summary(&rows),
            vec![(0, "root", None, 'r'), (1, "y", Some("x/y"), 'p')]
        );
    }

    #[test]
    fn test_sibling_root_prefix_does_not_nest() {
        let rows = build_repository_tree(
            &loaded(&["/root/app", "/root/app-old"]),
            &[],
            &paths(&["/root/app", "/root/app-old"]),
        );
        assert_eq!(
            summary(&rows),
            vec![(0, "app", None, 'r'), (0, "app-old", None, 'r')]
        );
    }

    #[test]
    fn test_duplicate_loaded_and_parked_is_loaded() {
        let rows = build_repository_tree(&loaded(&["/a"]), &paths(&["/a"]), &paths(&["/a"]));
        assert_eq!(summary(&rows), vec![(0, "a", None, 'r')]);
        assert_eq!(
            rows[0].entry,
            RepositoryTreeEntry::Repository(RepositoryTreeSource::Loaded(RepositoryId(0)))
        );
    }

    #[test]
    fn test_folders_group_sibling_repositories() {
        let rows = build_repository_tree(
            &loaded(&[
                "/root",
                "/root/env",
                "/root/services/alpha",
                "/root/services/beta",
                "/root/tools/gamma",
            ]),
            &[],
            &paths(&["/root"]),
        );
        assert_eq!(
            summary(&rows),
            vec![
                (0, "root", None, 'r'),
                (1, "env", None, 'r'),
                (1, "services", None, 'f'),
                (2, "alpha", None, 'r'),
                (2, "beta", None, 'r'),
                (1, "gamma", Some("tools/gamma"), 'r'),
            ]
        );
        assert!(rows[0].has_children);
        assert!(rows[2].has_children);
        assert!(!rows[3].has_children);
        assert_eq!(rows[3].location.as_deref(), Some("services/alpha"));
    }

    #[test]
    fn test_single_child_folder_chain_is_merged() {
        let rows = build_repository_tree(
            &loaded(&["/root/a/b/x", "/root/a/b/y"]),
            &[],
            &paths(&["/root"]),
        );
        assert_eq!(
            summary(&rows),
            vec![
                (0, "a/b", None, 'f'),
                (1, "x", None, 'r'),
                (1, "y", None, 'r'),
            ]
        );
    }

    #[test]
    fn test_collapsed_rows_hide_descendants() {
        let rows = build_repository_tree(
            &loaded(&[
                "/root",
                "/root/services/alpha",
                "/root/services/alpha/vendor/inner",
                "/root/services/beta",
                "/root/tools/gamma",
            ]),
            &[],
            &paths(&["/root"]),
        );
        let mut collapsed = HashSet::default();
        collapsed.insert(Arc::from(Path::new("/root/services")));
        let visible = visible_repository_tree_rows(rows.clone(), &collapsed);
        assert_eq!(
            summary(&visible),
            vec![
                (0, "root", None, 'r'),
                (1, "services", None, 'f'),
                (1, "gamma", Some("tools/gamma"), 'r'),
            ]
        );

        collapsed.insert(Arc::from(Path::new("/root")));
        let visible = visible_repository_tree_rows(rows, &collapsed);
        assert_eq!(summary(&visible), vec![(0, "root", None, 'r')]);
    }
}
