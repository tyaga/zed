# Git panel: repository tree and merge-commit filter

## Goal

A project can contain several git repositories: several worktree roots that
are repositories, a folder added to the project that is a repository, or
repositories nested inside a folder. The git panel works with one active
repository at a time, and the only way to change it is a dropdown in the panel
footer, next to the branch button and Fetch.

This change puts all repositories of the project in a tree at the top of the
git panel. Clicking a repository makes it active; the Changes and History tabs
below show that repository. The History tab also gets a toggle that hides
merge commits.

## Behavior

### Repository tree

- One tree for the whole panel, above the Changes / History tab bar. Both tabs
  already follow the project's active repository, so a single selection serves
  both.
- Rows are repositories only. A repository nested inside another one's work
  directory is indented under it. Repositories are grouped under the worktree
  root that contains them; folders without a repository are not rows, the row
  shows the repository path relative to its parent row instead.
- Each row shows:
  - the repository name (`display_name`) and, when nested, its relative path in
    muted text;
  - the current branch, or the short HEAD sha when detached;
  - `↑N ↓M` against the upstream when the branch tracks one and N or M is
    non-zero;
  - the number of changed files when non-zero.
- The active repository row is highlighted. Clicking another row calls
  `Repository::set_as_active_repository`.
- Parked repositories (nested repositories that the git store has found but
  not loaded) are listed too, in muted text and without branch information.
  Clicking one activates it in the git store and then makes it the active
  repository.
- The tree is hidden when the project has at most one repository, counting
  parked ones. Otherwise it takes at most a third of the panel height and
  scrolls beyond that. Its header row ("Repositories" and the count) has a
  disclosure arrow that collapses the tree; the collapsed state is stored with
  the panel's serialized state.
- The repository dropdown in the panel footer is removed. The branch button
  and the remote buttons stay.

### Merge-commit filter

- New setting `git_panel.hide_merge_commits`, default `false`.
- A toggle at the top of the History tab flips the setting; the list hides
  commits with more than one parent while it is on.
- The "unpushed" marker keeps marking the commits that are ahead of upstream.
  Each entry stores its position in the unfiltered list and the marker is
  computed from it, because hiding merges shifts row indices.

## Design

### `project::git_store`

- The existing `GitStore::parked_repositories()` provides the parked work
  directories.
- `GitStore::activate_parked_repository(&mut self, work_directory: &Path, cx)`
  registers the parked repository at that path (the same path that
  `activate_parked_repositories` uses) and makes it the active repository.
- The store emits `GitStoreEvent::ParkedRepositoriesChanged` when the parked
  list changes, so the tree refreshes when repositories are parked or found.
- Remote and collab projects have no parked repositories; the tree lists
  `repositories()` only.

### `git_ui::repository_tree` (new module)

- Pure function `build_repository_tree(repositories, parked, worktree_roots)
  -> Vec<RepositoryTreeRow>`. A row holds depth, work directory, display name,
  relative path, whether it is parked, and the repository entity when loaded.
  Ordering: by worktree root, then depth-first by path. Unit tested without
  GPUI.
- `RepositoryTreeRow` details (branch, ahead/behind, changed-file count) are
  read from the repository snapshot at render time, not stored in the row.
- The GPUI part renders the header and rows inside a scrollable container with
  a max height of a third of the panel. `GitPanel` owns the collapsed flag and
  re-renders on `GitStoreEvent` repository events, which it already observes.

### `git_ui::git_panel`

- Render the tree above `render_tab_bar`.
- Remove the repository dropdown from `PanelRepoFooter`. `RepositorySelector`
  stays: the `git: select repo` action opens it as a modal.
- `CommitHistoryEntry` gains `is_merge: bool` (from `parents.len() > 1`) and
  `position: usize`, its index in the unfiltered list. The loaded history keeps the full list; the rendered
  list is the full list filtered by the setting, rebuilt when the setting
  changes. Keyboard navigation, the active entry, and the context menu work on
  the filtered list.
- The History toggle writes `git_panel.hide_merge_commits` through
  `update_settings_file`, the same way other git panel view options do.

### Settings

- `git_panel.hide_merge_commits` in `settings_content`, `GitPanelSettings`,
  `assets/settings/default.json`, and the settings UI Git Panel section.

## Testing

- `build_repository_tree`: flat roots, a repository nested in another, a
  repository nested in a non-repository folder, parked repositories, sorting.
- `GitStore`: `activate_parked_repository` registers a parked nested
  repository, makes it active, and emits `ParkedRepositoriesChanged`.
- Git panel: clicking a tree row changes the active repository and the History
  list; the tree is hidden with one repository; the footer has no repository
  dropdown.
- History filter: with the setting on, merge commits are not rendered and
  unpushed markers stay on the right commits; turning it off restores the full
  list.

## Out of scope

- Grouping by intermediate folders that are not repositories.
- Showing per-repository status in the tree beyond branch, ahead/behind, and
  the changed-file count.
- Changing which repositories the git store parks.
