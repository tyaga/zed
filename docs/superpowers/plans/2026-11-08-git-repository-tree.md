# Git Panel Repository Tree Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show every git repository of the project as a tree at the top of the git panel, switch the active repository by clicking a row, and add a History toggle that hides merge commits.

**Architecture:** `project::git_store` gains a public way to activate one parked repository and an event for parked-list changes. `git_ui` gets a pure tree builder in a new `repository_tree.rs` and renders the tree in `GitPanel` above the tab bar; the footer repository dropdown goes away. History keeps the full commit list and derives a visible list filtered by a new `git_panel.hide_merge_commits` setting.

**Tech Stack:** Rust, GPUI, Zed settings (`settings_content`, `settings_ui`).

**Spec:** `docs/superpowers/specs/2026-11-08-git-repository-tree-design.md`

## Global Constraints

- Setting name: `git_panel.hide_merge_commits`, default `false`.
- Tree is hidden when loaded + parked repositories number at most 1.
- Tree max height: a third of the panel height, scrolls beyond.
- `RepositorySelector` and the `git: select repo` modal stay.
- Rust rules from `CLAUDE.md`: no `unwrap()` in non-test code, no `let _ =` on fallible calls, full-word names, no `mod.rs`.
- `README.md` keeps its first two `[!IMPORTANT]` lines.
- Commits only when the user asks; no AI attribution lines; author email is the noreply one already configured.

## Review Focus

- Two worktree roots where one root path is a prefix string of another (`/root/app` and `/root/app-old`): the second must not nest under the first. Test in Task 2.
- A repository whose work directory equals a worktree root: depth 0, no relative path. Test in Task 2.
- Clicking a parked row after it has been activated by another path (row is stale): must not panic, just activate the loaded repository. Test in Task 1 (`activate_parked_repository` on a non-parked path is a no-op that still selects a loaded repo at that path).
- Hiding merges when the active history entry is a merge commit: focused index must stay in range. Test in Task 4.
- History of a repository with only merge commits and the setting on: shows the "No commits yet" placeholder, no crash. Test in Task 4.

---

### Task 1: Activate one parked repository from the git store

**Files:**
- Modify: `crates/project/src/git_store.rs` (`GitStoreEvent` ~871, parked handling ~2620-2915)
- Test: `crates/project/tests/integration/git_store.rs` (parking tests module ~2030)

**Interfaces:**
- Produces: `GitStore::activate_parked_repository(&mut self, work_directory_abs_path: &Path, cx: &mut Context<Self>)`; `GitStoreEvent::ParkedRepositoriesChanged`. Existing `GitStore::parked_repositories(&self) -> &[ParkedRepository]` and `ParkedRepository::work_directory_abs_path(&self) -> &Path` are used as the parked list.

- [ ] **Step 1: Write failing tests** in the parking module, using `build_project` and `assert_repositories` with the `test_deep_repositories_park_in_unmanaged_root` tree:
  - `test_activate_parked_repository_registers_and_selects_it`: call `activate_parked_repository(path!("/root/nested/deep/repo2"))`, run until parked; `assert_repositories(.., &[repo1, repo2], &[])`; the active repository's `work_directory_abs_path` is `/root/nested/deep/repo2`.
  - `test_activate_parked_repository_on_loaded_path_selects_it`: activate repo2 twice; the second call leaves the repository set unchanged and repo2 active.
  - `test_parked_repositories_changed_event`: subscribe to `GitStoreEvent`; activating repo2 emits `ParkedRepositoriesChanged`.
- [ ] **Step 2: Run** `cargo test -p project --test integration parked` — expected: compile error, method missing.
- [ ] **Step 3: Implement.** Add the enum variant. `activate_parked_repository` calls `activate_parked_repositories(|parked| parked.work_directory_abs_path() == work_directory_abs_path, cx)`, then selects the repository whose `work_directory_abs_path` equals the argument through `set_active_repo_id`. Emit `ParkedRepositoriesChanged` wherever `parked_repositories` gains or loses entries (push in `update_repositories_from_worktree`, `activate_parked_repositories`, `retarget_parked_repositories`, worktree removal), once per call that changed it.
- [ ] **Step 4: Run** the same command — expected: PASS; then `cargo test -p project --test integration git_store` — PASS.
- [ ] **Step 5: Commit** `project: Allow activating a single parked repository`.

### Task 2: Pure repository tree builder

**Files:**
- Create: `crates/git_ui/src/repository_tree.rs`
- Modify: `crates/git_ui/src/git_ui.rs` (add `mod repository_tree;`)

**Interfaces:**
- Produces:
  ```rust
  pub(crate) enum RepositoryTreeSource { Loaded(RepositoryId), Parked }
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
  ) -> Vec<RepositoryTreeRow>
  ```
- Rules: parent row = the nearest other repository whose work directory is a strict ancestor (compare with `Path::starts_with`, never string prefixes); depth = parent depth + 1, else 0. `relative_path` = path relative to the parent row, or to the containing worktree root when there is no parent; `None` when it is a single component equal to `name` or empty. Order: by index of the containing worktree root (longest matching root; repositories outside every root go last), then depth-first by path. A path present in both lists is treated as loaded.

- [ ] **Step 1: Write failing tests** (`#[cfg(test)] mod tests` in the new file), each asserting `(depth, name, relative_path, is_parked)` tuples in order:
  - `test_flat_roots`: roots `/a`, `/b`, both repositories → `(0,"a",None)`, `(0,"b",None)`.
  - `test_repository_nested_in_repository`: `/root` repo with `/root/libs/inner` repo → `(0,"root",None)`, `(1,"inner",Some("libs/inner"))`.
  - `test_repository_in_plain_folder`: root `/root` not a repo, `/root/nested/deep/repo2` → `(0,"repo2",Some("nested/deep/repo2"))`.
  - `test_parked_rows`: parked `/root/x/y` under loaded `/root` → `(1,"y",Some("x/y"), parked)`.
  - `test_sibling_root_prefix_does_not_nest`: roots `/root/app`, `/root/app-old`, both repos → both depth 0.
  - `test_duplicate_loaded_and_parked_is_loaded`.
- [ ] **Step 2: Run** `cargo test -p git_ui --lib repository_tree` — expected: FAIL (function missing).
- [ ] **Step 3: Implement** `build_repository_tree` per the rules above.
- [ ] **Step 4: Run** the same command — expected: PASS.
- [ ] **Step 5: Commit** `git_ui: Add repository tree builder`.

### Task 3: Render the tree in the git panel

**Files:**
- Modify: `crates/git_ui/src/git_panel.rs` (`GitPanel` fields ~1319, `SerializedGitPanel` ~664, `GitStoreEvent` subscription ~1667, panel render ~10064, `PanelRepoFooter::render` ~10350)
- Modify: `crates/git_ui/src/repository_tree.rs` (row rendering helper if kept with the builder)

**Interfaces:**
- Consumes: Task 1 `activate_parked_repository`, `ParkedRepositoriesChanged`; Task 2 `build_repository_tree`, `RepositoryTreeRow`, `RepositoryTreeSource`.
- Produces on `GitPanel`: `fn repository_tree_rows(&self, cx: &App) -> Vec<RepositoryTreeRow>`, `fn activate_repository_tree_row(&mut self, row: &RepositoryTreeRow, cx: &mut Context<Self>)`, `fn toggle_repository_tree(&mut self, cx: &mut Context<Self>)`, field `repository_tree_collapsed: bool`, serialized as `#[serde(default)] repository_tree_collapsed: bool`.

- [ ] **Step 1: Write failing panel tests** (git panel `tests` module; build a `FakeFs` tree with two repositories `/project/one/.git`, `/project/two/.git` and one parked repository `/project/nested/deep/three/.git`):
  - `test_repository_tree_lists_all_repositories`: `repository_tree_rows` names are `["one", "three", "two"]` in tree order with `three` parked.
  - `test_repository_tree_row_click_switches_active_repository`: activating the `two` row makes the project's active repository `/project/two`; activating `three` loads it and makes it active.
  - `test_repository_tree_hidden_for_single_repository`: with one repository, the panel renders no element with `debug_selector("repository-tree")`.
  - `test_repository_tree_collapsed_state_is_serialized`: toggle, serialize, reload panel → still collapsed.
- [ ] **Step 2: Run** `cargo test -p git_ui --lib repository_tree` — expected: FAIL.
- [ ] **Step 3: Implement.**
  - `repository_tree_rows` collects `(repository.id, work_directory_abs_path)` from `git_store.repositories()`, parked paths from `parked_repositories()`, worktree roots from `project.visible_worktrees(cx)` abs paths, and calls `build_repository_tree`.
  - Render above `render_tab_bar` when rows > 1: header row `Disclosure` + "Repositories" + count, `debug_selector("repository-tree")`; body (unless collapsed) in a `div().id("repository-tree-rows").max_h(relative(1. / 3.)).overflow_y_scroll()`. Row: indent by `depth`, name, muted `relative_path`, then for loaded rows branch name (or 7-char HEAD sha), `↑N ↓M` from `branch.upstream.tracking.status()` when non-zero, changed-file count from `statuses_by_path.summary().count` when non-zero. Parked rows muted with no details. Active row uses `element_selected` background. Click calls `activate_repository_tree_row`.
  - `activate_repository_tree_row`: loaded → `repository.update(cx, |repository, cx| repository.set_as_active_repository(cx))`; parked → `git_store.update(cx, |store, cx| store.activate_parked_repository(&row.work_directory, cx))`.
  - Notify on `GitStoreEvent::ParkedRepositoriesChanged` alongside `RepositoryAdded`/`RepositoryRemoved`.
  - Remove `repo_selector` and its separator from `PanelRepoFooter::render`; drop the now-unused import.
- [ ] **Step 4: Run** `cargo test -p git_ui --lib` — expected: PASS (all).
- [ ] **Step 5: Commit** `git_ui: Show project repositories as a tree in the git panel`.

### Task 4: Hide merge commits in History

**Files:**
- Modify: `crates/settings_content/src/settings_content.rs` (`GitPanelSettingsContent`, after `generate_commit_message_button`)
- Modify: `crates/git_ui/src/git_panel_settings.rs`, `assets/settings/default.json` (`git_panel`), `crates/settings_ui/src/page_data.rs` (`git_panel_section`, bump array size)
- Modify: `crates/git_ui/src/git_panel.rs` (`CommitHistoryEntry` ~1407, `set_commit_history` ~8091, `commit_history_entries` ~7863, `render_history_tab` ~7832, `render_commit_history` ~8132)

**Interfaces:**
- Produces: `CommitHistoryEntry { sha, tag_names, is_merge: bool, position: usize }` where `position` is the index in the unfiltered list; `fn visible_commit_history(entries: &[CommitHistoryEntry], hide_merge_commits: bool) -> Rc<[CommitHistoryEntry]>`; `GitPanel.visible_commit_history: Rc<[CommitHistoryEntry]>`; `GitPanelSettings.hide_merge_commits: bool`.

- [ ] **Step 1: Write failing tests:**
  - `test_visible_commit_history_hides_merges`: entries `[a(merge), b, c(merge), d]` → hidden list `[b, d]` with positions `[1, 3]`; with `false` → all four.
  - `test_unpushed_marker_uses_unfiltered_position`: with `ahead_count = 2` and `[a(merge), b, c]`, the visible `b` is unpushed (`position 1 < 2`), `c` is not.
  - `test_hide_merge_commits_keeps_focus_in_range`: panel with history `[m(merge), x]`, focused entry 1; set the setting on → focused entry is 0 and points at `x`.
  - `test_only_merge_commits_shows_placeholder`: history of merges only with the setting on → `commit_history_entries()` is empty and render does not panic.
- [ ] **Step 2: Run** `cargo test -p git_ui --lib commit_history` — expected: FAIL.
- [ ] **Step 3: Implement.**
  - Setting plumbing as for `generate_commit_message_button` (doc: "Whether to hide merge commits in the History tab.", default `false`).
  - `From<&Arc<InitialGraphCommitData>>` sets `is_merge = parents.len() > 1`; positions are assigned where the list is built (~8080).
  - `set_commit_history` and a settings observer recompute `visible_commit_history`; `commit_history_entries()` returns it; the focused-entry clamp runs after the recompute. Render and keyboard navigation use the visible list; the unpushed check becomes `entry.position < ahead_count`. Empty visible list with a loaded history shows "No commits yet".
  - A `Checkbox::new("hide-merge-commits", ..).label("Hide merge commits")` row at the top of the History tab writes the setting through `update_settings_file`, as `set_sort_by_path` does.
- [ ] **Step 4: Run** `cargo test -p git_ui --lib`, `cargo test -p settings -p settings_ui --lib` — expected: PASS.
- [ ] **Step 5: Commit** `git_ui: Add a setting to hide merge commits in History`.

### Task 5: Finish

- [ ] `cargo fmt` on touched crates; `./script/clippy -p git_ui -p project` — no new warnings.
- [ ] Build and install with `~/.claude/scripts/zed-dev-update.sh` once the user asks to update.
