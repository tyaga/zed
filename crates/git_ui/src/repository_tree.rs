use std::{path::Path, sync::Arc};

use gpui::SharedString;
use project::git_store::RepositoryId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepositoryTreeSource {
    Loaded(RepositoryId),
    Parked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepositoryTreeRow {
    pub depth: usize,
    pub work_directory: Arc<Path>,
    pub name: SharedString,
    pub relative_path: Option<SharedString>,
    pub source: RepositoryTreeSource,
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

    // Entries are sorted by path, so every ancestor precedes its descendants.
    let mut rows: Vec<RepositoryTreeRow> = Vec::with_capacity(entries.len());
    let mut group_keys: Vec<usize> = Vec::with_capacity(entries.len());
    for (index, (path, source)) in entries.iter().enumerate() {
        let parent_index = (0..index)
            .filter(|&candidate| path.starts_with(&entries[candidate].0))
            .max_by_key(|&candidate| entries[candidate].0.components().count());

        let (depth, base, group_key) = match parent_index {
            Some(parent_index) => (
                rows[parent_index].depth + 1,
                Some(entries[parent_index].0.clone()),
                group_keys[parent_index],
            ),
            None => {
                let root_index = containing_root(path);
                (
                    0,
                    root_index.map(|root_index| worktree_roots[root_index].clone()),
                    root_index.unwrap_or(usize::MAX),
                )
            }
        };
        group_keys.push(group_key);

        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let relative_path = base
            .as_deref()
            .and_then(|base| path.strip_prefix(base).ok())
            .map(|relative| {
                relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .filter(|relative| !relative.is_empty() && *relative != name)
            .map(SharedString::from);

        rows.push(RepositoryTreeRow {
            depth,
            work_directory: path.clone(),
            name: name.into(),
            relative_path,
            source: *source,
        });
    }

    let mut ordered: Vec<(usize, RepositoryTreeRow)> = group_keys.into_iter().zip(rows).collect();
    ordered.sort_by(|(left_key, left), (right_key, right)| {
        left_key
            .cmp(right_key)
            .then_with(|| left.work_directory.cmp(&right.work_directory))
    });
    ordered.into_iter().map(|(_, row)| row).collect()
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

    fn summary(rows: &[RepositoryTreeRow]) -> Vec<(usize, &str, Option<&str>, bool)> {
        rows.iter()
            .map(|row| {
                (
                    row.depth,
                    row.name.as_ref(),
                    row.relative_path.as_deref(),
                    row.source == RepositoryTreeSource::Parked,
                )
            })
            .collect()
    }

    #[test]
    fn test_flat_roots() {
        let rows = build_repository_tree(&loaded(&["/a", "/b"]), &[], &paths(&["/a", "/b"]));
        assert_eq!(
            summary(&rows),
            vec![(0, "a", None, false), (0, "b", None, false)]
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
                (0, "root", None, false),
                (1, "inner", Some("libs/inner"), false)
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
            vec![(0, "repo2", Some("nested/deep/repo2"), false)]
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
            vec![(0, "root", None, false), (1, "y", Some("x/y"), true)]
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
            vec![(0, "app", None, false), (0, "app-old", None, false)]
        );
    }

    #[test]
    fn test_duplicate_loaded_and_parked_is_loaded() {
        let rows = build_repository_tree(&loaded(&["/a"]), &paths(&["/a"]), &paths(&["/a"]));
        assert_eq!(summary(&rows), vec![(0, "a", None, false)]);
        assert_eq!(
            rows[0].source,
            RepositoryTreeSource::Loaded(RepositoryId(0))
        );
    }
}
