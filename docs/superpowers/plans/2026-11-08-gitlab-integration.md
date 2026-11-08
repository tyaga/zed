# GitLab Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the current branch's GitLab merge request and pipeline in the status bar, highlight MR changes in the gutter, give links to MR/pipeline/file-in-MR, and list the project's open MRs in a picker.

**Architecture:** New crate `crates/gitlab` with a REST client over Zed's `HttpClient`, a per-project `GitlabStore` entity that follows the active repository and polls GitLab, a status bar item, actions and a picker. Gutter highlighting reuses `git.diff_base: "default_branch"` through a new per-repository base ref override in `GitStore`.

**Tech Stack:** Rust, GPUI, `http_client`, `serde_json`, `sha1`, `git`/`project` crates, GitLab REST API v4.

**Spec:** `docs/superpowers/specs/2026-11-08-gitlab-integration-design.md`

## Global Constraints

- Tasks 1–7 are iteration 1; Task 8 is iteration 2. Iteration 1 ships without Task 8.
- Token sources, in order: `GITLAB_TOKEN`, then `glab config get token --host <host>`. Token only in memory, header `PRIVATE-TOKEN`.
- Poll interval: 15 s while pipeline status is `created`, `pending`, `running`, `preparing`, `waiting_for_resource`; 120 s otherwise. No requests while no Zed window is active.
- Not a GitLab project: no status bar item; actions fail with `Active repository is not hosted on GitLab`.
- No token: status item text `GitLab: no token`.
- Override applies only with `diff_base: default_branch`.
- Follow `CLAUDE.md`: no `unwrap()` outside tests, no `let _ =` on fallible calls, no `mod.rs`, full-word names, comments only for non-obvious "why".
- No company, host or user names in code, tests or fixtures: use `gitlab.example.com`, `group/subgroup/project`, `jdoe`.
- Commits only when the user asks (user rule); "Commit" steps below mean: stage and stop for the user.
- Build check: `cargo check -p <crate>`; lint: `./script/clippy -p <crate>`.

## Review Focus

- Branch names with `/`, `#`, `+` and project paths with subgroups: every path segment and query value URL-encoded (`group%2Fsubgroup%2Fproject`). Test in Task 3.
- Self-hosted instance under a sub-path (`https://gitlab.example.com/gitlab`): API URL keeps the sub-path. Test in Task 3.
- Branch switched while a refresh is in flight: the stale response must not overwrite the new branch's state. Test in Task 5.
- MR target branch has no local remote-tracking ref: gutter falls back to the default branch, tooltip says so. Test in Task 5.
- Remote URL in SSH form with port (`ssh://git@gitlab.example.com:2222/group/project.git`) is detected as GitLab and maps to `https://gitlab.example.com`. Test in Task 3.

---

### Task 1: Base ref override in `GitStore`

**Files:**
- Modify: `crates/project/src/git_store.rs` (`GitStore` struct ~line 105, `refresh_diff_base_for_repo` ~line 5100, repository removal paths that already call `display_diffs.remove`)
- Test: `crates/project/tests/integration/git_store.rs`

**Interfaces:**
- Produces:
  - `GitStore::set_diff_base_override(&mut self, repo_id: RepositoryId, base_ref: Option<SharedString>, cx: &mut Context<Self>)`
  - `GitStore::diff_base_override(&self, repo_id: RepositoryId) -> Option<&SharedString>`

- [ ] **Step 1: Write the failing tests**

`test_diff_base_override_replaces_default_branch`: fake fs repo with `default_branch_ref = "refs/remotes/origin/main"` and branches `origin/main`, `origin/release`; settings `git.diff_base = default_branch`; after `run_until_parked`, `display_diff_for_repo(id).diff_base()` is `Merge { base_ref: "origin/main" }`. Call `set_diff_base_override(id, Some("origin/release"))`, park: `Merge { base_ref: "origin/release" }` and a `GitStoreEvent::DiffBaseChanged(Some(id))` was emitted. Set `None`, park: back to `origin/main`.

`test_diff_base_override_ignored_for_head`: settings `diff_base = head`, set override: `display_diff_for_repo(id)` is `None`.

- [ ] **Step 2: Run, expect compile failure** — `cargo test -p project --test integration diff_base_override`

- [ ] **Step 3: Implement**

Field `diff_base_overrides: HashMap<RepositoryId, SharedString>`. `set_diff_base_override` stores/removes the value; when it changed and `diff_base == DefaultBranch`, call `refresh_diff_base_for_repo(repo_id, cx)`. In `refresh_diff_base_for_repo`, when an override exists, resolve to it directly (`Ok(Some(override))`) instead of `repo.default_branch(true)`. Remove the entry where `display_diffs.remove(&id)` runs on repository removal.

- [ ] **Step 4: Run tests, expect PASS** — same command, plus `cargo test -p project --test integration git_store`.

- [ ] **Step 5: Stage** `crates/project/src/git_store.rs crates/project/tests/integration/git_store.rs`

### Task 2: Public branch diff entry point

**Files:**
- Modify: `crates/git_ui/src/branch_diff.rs:197` and `crates/git_ui/src/git_ui.rs`

**Interfaces:**
- Produces: `git_ui::open_branch_diff(workspace: &mut Workspace, base_ref: SharedString, window: &mut Window, cx: &mut Context<Workspace>) -> anyhow::Result<()>` — opens or focuses `BranchDiff` for the active repository with `base_ref`; `Err("No active repository")` without one.

- [ ] **Step 1: Implement** as a thin wrapper over `BranchDiff::deploy_branch_diff_with_base_ref(workspace, project, repo, base_ref, None, None, window, cx)`; re-export from `git_ui.rs`.
- [ ] **Step 2: Verify** — `cargo check -p git_ui`; existing tests `cargo test -p git_ui branch_diff` pass.
- [ ] **Step 3: Stage** both files.

### Task 3: Crate scaffold, project detection, links, token

**Files:**
- Create: `crates/gitlab/Cargo.toml` (`[lib] path = "src/gitlab.rs"`; deps: `anyhow, collections, git, gpui, http_client, log, project, serde, serde_json, settings, sha1, ui, url, urlencoding, util, workspace, zed_actions`; dev: `gpui/test-support, http_client/test-support, project/test-support, fs/test-support, settings/test-support`)
- Create: `crates/gitlab/src/gitlab.rs`, `crates/gitlab/src/links.rs`, `crates/gitlab/src/token.rs`
- Modify: root `Cargo.toml` (member `crates/gitlab`, `gitlab = { path = "crates/gitlab" }`, `sha1 = "0.10"`)
- Modify: `README.md` — prepend the two `> [!IMPORTANT]` lines if absent (already present at time of writing).

**Interfaces:**
- Produces (in `links.rs`):
  - `pub struct GitlabProject { pub instance: Url, pub path: Arc<str> }` — `instance` is the web base URL (may include a sub-path), `path` like `group/subgroup/project`.
  - `impl GitlabProject { pub fn api_url(&self, endpoint: &str) -> Result<Url>; pub fn encoded_path(&self) -> String; pub fn host(&self) -> &str }` — `api_url("merge_requests")` → `<instance>/api/v4/projects/<encoded_path>/merge_requests`.
  - `pub fn gitlab_project_from_remote(registry: &GitHostingProviderRegistry, remote_url: &str) -> Option<GitlabProject>` — uses `git::parse_git_remote_url`; accepts providers whose `name()` starts with `"GitLab"`; `instance = provider.base_url()`, `path = format!("{owner}/{repo}")`.
  - `pub fn merge_request_file_url(merge_request_web_url: &Url, repo_relative_path: &str, line: Option<(Option<u32>, u32)>) -> Url` — fragment `<sha1_hex(path)>` or `<sha1_hex(path)>_<old>_<new>` with empty `old` when `None`; lines are 1-based.
- Produces (in `token.rs`): `pub async fn lookup_token(host: &str) -> Option<String>` — env `GITLAB_TOKEN`, then `util::command::new_command("glab")` with `config get token --host <host>`, trimmed, empty → `None`.

- [ ] **Step 1: Write failing tests in `links.rs`**
  - `gitlab_com_https` — `https://gitlab.com/group/project.git` → instance `https://gitlab.com/`, path `group/project`.
  - `self_hosted_ssh_with_subgroups` — `git@gitlab.example.com:group/subgroup/project.git` → `https://gitlab.example.com/`, `group/subgroup/project`, `encoded_path() == "group%2Fsubgroup%2Fproject"`.
  - `ssh_with_port` — `ssh://git@gitlab.example.com:2222/group/project.git` → instance host `gitlab.example.com`.
  - `instance_sub_path` — `GitlabProject { instance: "https://gitlab.example.com/gitlab/", path: "group/project" }.api_url("merge_requests")` == `https://gitlab.example.com/gitlab/api/v4/projects/group%2Fproject/merge_requests`.
  - `github_remote_is_not_gitlab` — `https://github.com/owner/repo.git` → `None`.
  - `file_url_without_line` — fragment equals the sha1 hex of `src/main.rs`, computed in the test with `sha1::Sha1`.
  - `file_url_added_line` — `line = Some((None, 12))` → fragment ends with `__12`.
  - `file_url_unchanged_line` — `Some((Some(10), 12))` → ends with `_10_12`.
- [ ] **Step 2: Run, expect failure** — `cargo test -p gitlab links`
- [ ] **Step 3: Implement** `links.rs`, `token.rs`; `gitlab.rs` declares modules and `pub fn init(cx: &mut App)` (empty for now).
- [ ] **Step 4: Run, expect PASS**; `cargo check -p gitlab`.
- [ ] **Step 5: Stage** new crate, `Cargo.toml`, `Cargo.lock`.

### Task 4: REST client

**Files:**
- Create: `crates/gitlab/src/api.rs`

**Interfaces:**
- Consumes: `GitlabProject` (Task 3).
- Produces:
  - `pub struct GitlabClient { http: Arc<dyn HttpClient>, project: GitlabProject, token: String }`, `GitlabClient::new(http, project, token)`.
  - `#[derive(Deserialize, Clone, Debug, PartialEq)] pub struct MergeRequest { iid: u64, title: String, state: MergeRequestState, draft: bool, source_branch: String, target_branch: String, web_url: Url, author: User, updated_at: String, head_pipeline: Option<Pipeline> }` (all `pub`).
  - `pub enum MergeRequestState { Opened, Merged, Closed, Locked }` (`serde(rename_all = "snake_case")`).
  - `pub struct Pipeline { id: u64, status: PipelineStatus, web_url: Url, duration: Option<u64>, updated_at: Option<String> }`.
  - `pub enum PipelineStatus { Created, WaitingForResource, Preparing, Pending, Running, Success, Failed, Canceled, Skipped, Manual, Scheduled, #[serde(other)] Unknown }`, `impl PipelineStatus { pub fn is_active(&self) -> bool }` — true for the five active statuses from Global Constraints.
  - `pub struct User { id: u64, username: String, name: String }`.
  - `pub enum ApiError { Unauthorized, NotFound, Other(anyhow::Error) }` implementing `std::error::Error`.
  - `impl GitlabClient`:
    - `pub async fn merge_requests_for_branch(&self, branch: &str, state: MergeRequestState) -> Result<Vec<MergeRequest>, ApiError>` — `merge_requests?source_branch=..&state=..&order_by=updated_at&sort=desc&per_page=20`.
    - `pub async fn latest_pipeline_for_ref(&self, branch: &str) -> Result<Option<Pipeline>, ApiError>` — `pipelines?ref=..&per_page=1`.
    - `pub async fn open_merge_requests(&self, filter: MergeRequestFilter, current_user: Option<u64>) -> Result<Vec<MergeRequest>, ApiError>` — `state=opened&scope=all&per_page=100` plus `assignee_id`/`reviewer_id`/`author_id`.
    - `pub async fn current_user(&self) -> Result<User, ApiError>` — `GET <instance>/api/v4/user`.
  - `pub enum MergeRequestFilter { AssignedToMe, ReviewRequested, CreatedByMe, AllOpen }`.

- [ ] **Step 1: Write failing tests** with `FakeHttpClient::create`, asserting on the request and parsed result:
  - `merge_requests_query` — branch `feature/a+b`: request URL is `https://gitlab.example.com/api/v4/projects/group%2Fproject/merge_requests?source_branch=feature%2Fa%2Bb&state=opened&order_by=updated_at&sort=desc&per_page=20`, header `PRIVATE-TOKEN: token`; response fixture with one MR with `head_pipeline.status = "running"` parses to `PipelineStatus::Running`.
  - `unknown_pipeline_status` — `"status": "something_new"` → `Unknown`.
  - `unauthorized` — 401 → `ApiError::Unauthorized`; 404 → `ApiError::NotFound`; 500 → `Other` with the status in the message.
  - `open_merge_requests_review_filter` — `ReviewRequested` with user 7 → query contains `reviewer_id=7`.
- [ ] **Step 2: Run, expect failure** — `cargo test -p gitlab api`
- [ ] **Step 3: Implement.** One private `async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T, ApiError>`.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Stage.**

### Task 5: `GitlabStore`

**Files:**
- Create: `crates/gitlab/src/merge_request_store.rs`

**Interfaces:**
- Consumes: Task 1 override API, Task 3 `gitlab_project_from_remote`, `lookup_token`, Task 4 client.
- Produces:
  - `pub struct GitlabStore` — one per `Project`. `pub fn for_project(project: &Entity<Project>, cx: &mut App) -> Entity<GitlabStore>` (global `HashMap<EntityId, WeakEntity<GitlabStore>>`, entries dropped when upgrade fails).
  - `pub enum BranchState { NotGitlab, NoToken, Loading, Ready(BranchInfo) }`.
  - `pub struct BranchInfo { pub project: GitlabProject, pub branch: SharedString, pub merge_request: Option<MergeRequest>, pub pipeline: Option<Pipeline>, pub last_refreshed: std::time::Instant, pub last_error: Option<SharedString>, pub target_ref_missing: bool }`.
  - `impl GitlabStore { pub fn state(&self) -> &BranchState; pub fn refresh(&mut self, cx: &mut Context<Self>); pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>); pub fn client(&self) -> Option<GitlabClient> }`
  - `impl EventEmitter<GitlabStoreEvent>`, `pub enum GitlabStoreEvent { Changed }`.

Behavior:
- Subscribes to the project's `GitStore`: `ActiveRepositoryChanged`, `RepositoryUpdated(_, HeadChanged | BranchListChanged, true)` → `refresh`.
- `refresh`: resolve active repo → remote (`branch.upstream.remote_name()` else `"origin"`) → URL via `repo.remote_urls()` → `GitlabProject`; token cached per host, else `lookup_token`. Lookup order: opened, merged, closed; pipeline = `head_pipeline` or `latest_pipeline_for_ref`. Each refresh increments `generation: u64`; results whose generation is stale are dropped.
- Override: opened MR and `refs/remotes/<remote>/<target>` exists in `repo.branch_list` (`Branch::is_remote` with name `<remote>/<target>`) → `set_diff_base_override(repo_id, Some("<remote>/<target>"))`; target missing → `None` and `target_ref_missing = true`; no opened MR or branch changed → `None`.
- Errors: `Unauthorized` → drop cached token, state `NoToken`; other errors keep previous `Ready` info with `last_error` set.
- Timer: after each refresh, schedule next in 15 s / 120 s (`PipelineStatus::is_active`) with `cx.background_executor().timer`. When it fires and `cx.active_window().is_none()`, mark `stale = true` and stop; `refresh_if_stale` restarts.

- [ ] **Step 1: Write failing tests** (`#[gpui::test]`, `FakeFs` repo with remote `origin = git@gitlab.example.com:group/project.git`, branch `feature` tracking `origin/feature`, `origin/main` present; fake HTTP routes by path; token injected via a test-only `GitlabStore::set_token_for_tests(host, token)`):
  - `finds_opened_merge_request` — state `Ready` with MR iid 5 and pipeline status from `head_pipeline`.
  - `falls_back_to_merged` — opened empty, merged has iid 4 → iid 4; no override set.
  - `branch_pipeline_without_merge_request` — all MR queries empty → pipeline from `pipelines?ref=feature`.
  - `stale_response_dropped` — first response delayed on a channel; switch branch to `other`; release first response → state branch is `other`.
  - `sets_and_clears_override` — opened MR targeting `main` → `diff_base_override == Some("origin/main")`; switch to a branch without MR → `None`.
  - `missing_target_ref` — MR targets `release` with no `origin/release` → override `None`, `target_ref_missing`.
  - `error_keeps_last_state` — second refresh returns 500 → MR still iid 5, `last_error` is `Some`.
  - `unauthorized_drops_token` — 401 → `NoToken`.
  - `poll_interval_follows_pipeline` — running pipeline: advancing clock 15 s triggers a request; success: no request at 15 s, one at 120 s.
  - `not_gitlab_remote` — remote on github.com → `NotGitlab`, no HTTP requests.
- [ ] **Step 2: Run, expect failure** — `cargo test -p gitlab merge_request_store`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Stage.**

### Task 6: Actions and links

**Files:**
- Create: `crates/gitlab/src/actions.rs`
- Modify: `crates/gitlab/src/gitlab.rs` (`init` registers actions on new workspaces via `cx.observe_new::<Workspace>`)

**Interfaces:**
- Consumes: Task 2 `git_ui::open_branch_diff`, Task 3 `merge_request_file_url`, Task 5 store.
- Produces: `actions!(gitlab, [OpenMergeRequest, CopyMergeRequestUrl, OpenPipeline, CopyPipelineUrl, CopyFileLinkInMergeRequest, OpenMergeRequestDiff, Refresh])` with doc comments shown in the command palette.

Behavior:
- Missing data errors (via `detach_and_notify_err` / `Toast`): `Active repository is not hosted on GitLab`, `No GitLab token for <host>: set GITLAB_TOKEN or run glab auth login`, `No merge request for branch <branch>`, `No pipeline for branch <branch>`.
- `CopyFileLinkInMergeRequest`: active `Editor` singleton buffer; repo-relative path via `GitStore::repository_and_path_for_buffer_id`; line = newest selection head row + 1; old line from the buffer's display diff (`project.git_store().open_display_diff`) — row inside a hunk → `None`, else map through the diff base by subtracting accumulated hunk line deltas above the row.
- `OpenMergeRequestDiff`: `git_ui::open_branch_diff(workspace, "<remote>/<target>")`.

- [ ] **Step 1: Write failing test** `old_line_mapping` for a pure helper `fn base_line_for(row: u32, hunks: &[(Range<u32>, Range<u32>)]) -> Option<u32>` (buffer range, base range, 0-based): row before any hunk → same; row inside an added range → `None`; row after a hunk that added 3 lines → row − 3.
- [ ] **Step 2: Run, expect failure** — `cargo test -p gitlab actions`
- [ ] **Step 3: Implement actions and helper.**
- [ ] **Step 4: Run, expect PASS**; `cargo check -p gitlab`.
- [ ] **Step 5: Stage.**

### Task 7: Status bar item and wiring

**Files:**
- Create: `crates/gitlab/src/status_item.rs`
- Modify: `crates/zed/Cargo.toml`, `crates/zed/src/main.rs` (call `gitlab::init(cx)` next to `git_ui::init`), `crates/zed/src/zed.rs:636-641` (`status_bar.add_left_item(gitlab_status, ...)` after `git_blame_status`)

**Interfaces:**
- Consumes: Task 5 store, Task 6 actions.
- Produces: `pub struct GitlabStatusItem`, `GitlabStatusItem::new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self`, `impl StatusItemView` (no-op `set_active_pane_item`).

Behavior:
- Renders nothing for `NotGitlab`; `GitLab: no token` for `NoToken`; for `Ready`: `!<iid>` with MR state icon then pipeline icon; pipeline icon only without MR; hidden with neither.
- Icon colors: success → `Color::Success`, failed → `Color::Error`, running/pending/created/preparing/waiting → `Color::Info`, others → `Color::Muted`; `last_error` → `Color::Warning` on the whole item.
- Tooltip lines: title, `source → target`, author, pipeline status and duration, `Updated <n>s ago`, `Gutter compares with default branch: <target> is not fetched` when `target_ref_missing`, last error.
- Click: `ContextMenu` with the Task 6 actions (file link entry only when the active item is an editor), dispatching the actions.
- `window.observe_window_activation` → on activation `store.refresh_if_stale`.

- [ ] **Step 1: Implement** the item and wiring.
- [ ] **Step 2: Verify** — `cargo check -p zed`, `./script/clippy -p gitlab -p project -p git_ui`, `cargo test -p gitlab -p git_ui`, `cargo test -p project --test integration git_store`.
- [ ] **Step 3: Manual check** in a built Zed on a self-hosted project branch with an open MR and running pipeline: item appears, pipeline icon turns green after completion within 15 s, gutter (with default-branch mode on) matches MR changes, every menu entry opens/copies the right URL, file link lands on the line in the MR diff.
- [ ] **Step 4: Stage.**

### Task 8: Merge request picker (iteration 2)

**Files:**
- Create: `crates/gitlab/src/merge_request_picker.rs`
- Modify: `crates/gitlab/src/actions.rs` (`MergeRequests` action), `crates/gitlab/Cargo.toml` (`picker`, `fuzzy`, `menu`)

**Interfaces:**
- Consumes: Task 4 `open_merge_requests`, `current_user`, `MergeRequestFilter`; Task 5 `GitlabStore::client`.
- Produces: `pub struct MergeRequestPickerDelegate` implementing `PickerDelegate`; `gitlab::MergeRequests` action opening it as a modal.

Behavior:
- Filter row (`ToggleButtonGroup`) with the four filters, default Assigned to me; switching refetches; `current_user` cached in `GitlabStore` per host.
- Match string: `!iid title source_branch author.username`.
- Row: `!iid`, draft badge, title, `source → target`, author, pipeline icon (same colors as Task 7), relative updated time.
- `menu::Confirm` → open `web_url`; `menu::SecondaryConfirm` → checkout: `repo.fetch(FetchOptions::Remote(remote))`, then `change_branch(source)` if a local branch exists, else `create_branch(source, Some("<remote>/<source>"))`; errors shown as notifications. Copy URL: `cmd-c` / `ctrl-c`, listed in the footer.

- [ ] **Step 1: Write failing test** `picker_lists_and_filters`: fake HTTP returns two MRs for `reviewer_id=7` (`/user` returns id 7); open picker, switch to Review requested, type `fix` → one match.
- [ ] **Step 2: Run, expect failure** — `cargo test -p gitlab merge_request_picker`
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS**; clippy; manual check of checkout on a branch not present locally.
- [ ] **Step 5: Stage.**
