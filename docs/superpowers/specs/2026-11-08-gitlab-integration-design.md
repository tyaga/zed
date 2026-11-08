# GitLab integration: merge requests, pipelines, MR-aware gutter

## Goal

Working on a branch of a GitLab-hosted repository, the user wants to see from
the editor, without switching to the browser:

- whether the current branch has a merge request, its state and the status of
  its latest pipeline;
- which lines the merge request changes, highlighted in the gutter of every
  open file;
- links to the merge request, its pipeline and to a file or line inside the
  merge request diff, opened in the browser or copied;
- the list of open merge requests of the repository, to open one or check out
  its branch.

Zed extensions run in WASM and cannot add status bar items, pickers or gutter
decorations, so this is a native crate in the fork.

Delivery is split in two iterations:

1. Current-branch merge request and pipeline status, MR-aware gutter, links.
2. Merge request list picker.

Out of scope: job list and job logs, MR discussions in the editor, approve,
merge, creating merge requests (the git panel already opens the create-MR
page after push).

## Behavior

### Detecting the GitLab project

- For the active repository (`GitStore::active_repository`) the remote is the
  one the current branch tracks (`Branch::upstream`, remote part of the
  upstream ref); without an upstream, `origin`.
- The remote URL is parsed with the existing hosting provider registry
  (`git::parse_git_remote_url`). The repository is a GitLab project when the
  provider is the GitLab provider: `gitlab.com`, a self-hosted host detected by
  `Gitlab::from_remote_url`, or a `git_hosting_providers` entry with
  `provider: "gitlab"`. The provider's base URL gives the instance, and
  `owner/repo` of `ParsedGitRemote` gives the project path (subgroups are part
  of `owner`; an instance sub-path such as `/gitlab` is not part of it).
- Not a GitLab project: no status bar item, actions report
  "Active repository is not hosted on GitLab".

### Token

Looked up per instance host, first hit wins:

1. `GITLAB_TOKEN` environment variable (the same variable `glab` reads), only
   for the host in `GITLAB_HOST`, or for `gitlab.com` when it is unset: the
   remote of a cloned repository picks the host, and the token must not go
   to a host it was not issued for;
2. `glab config get token --host <host>`, when `glab` is on `PATH`.

The token is kept in memory only and sent as `PRIVATE-TOKEN`. Without a
token the status bar item shows `GitLab: no token`, its tooltip names both
sources. A 401 response drops the cached token and shows the same state; the
next refresh looks it up again.

### Current-branch merge request and pipeline (iteration 1)

- Lookup: `GET /projects/:path/merge_requests?source_branch=<branch>&state=opened`,
  newest first. No opened MR: the same query with `state=merged`, then
  `state=closed`, taking the most recent. Merge requests from forks
  (`source_project_id` differs from `target_project_id`) are skipped. The
  branch name is the upstream's branch on the remote. Detached HEAD: no lookup.
- The list endpoint has no `head_pipeline`; when it is missing, the single
  merge request is fetched.
- Pipeline: `head_pipeline` of the MR when present. Without an MR, the latest
  pipeline of the branch: `GET /projects/:path/pipelines?ref=<branch>&per_page=1`.
- Refresh:
  - when the active repository, its branch or its HEAD changes;
  - by timer: every 15 s while the pipeline is `created`, `pending`,
    `running`, `preparing`, `waiting_for_resource`; every 120 s otherwise;
  - only while a Zed window is active; the timer pauses otherwise and the
    first activation refreshes immediately;
  - on the `gitlab: refresh` action.
- Status bar item, left side next to the git blame status:
  - `!<iid>` with an icon for the MR state (opened, draft, merged, closed)
    followed by a pipeline status icon (success, failed, running, pending,
    canceled, skipped, manual). Without an MR, only the pipeline icon and the
    branch pipeline. Neither: the item is hidden.
  - Tooltip: MR title, `source → target`, author, pipeline status and
    duration, time of last refresh, last error if the latest refresh failed.
  - Click opens a context menu: Open Merge Request, Open Pipeline, Copy Merge
    Request URL, Copy File Link in Merge Request (when an editor with a file
    is active), Refresh.
- Errors (network, 5xx, 404 on the project) keep the last good state on
  screen and show it in the tooltip; the icon gets a warning color.

### MR-aware gutter (iteration 1)

Zed already has `git.diff_base: "default_branch"`: buffers diff against the
merge base of `HEAD` and the repository's default branch, and project panel
and tab statuses follow it. The quick action bar toggles it.

- `GitStore` gains a per-repository base ref override. While an override is
  set and `diff_base` is `default_branch`, `refresh_diff_base_for_repo` uses
  the override instead of `Repository::default_branch`.
- The GitLab crate sets the override to `<remote>/<target_branch>` when the
  current branch has an opened MR, and clears it when the MR disappears or
  the branch changes. A missing remote-tracking ref (target branch never
  fetched) falls back to the default branch and the tooltip says so.
- `gitlab: open merge request diff` opens the existing branch diff view
  (`BranchDiff`) with the MR target as its base, through
  `git_ui::branch_diff::open_branch_diff`. When the target is not fetched it
  reports that instead.

With `diff_base: "head"` the override has no effect: the user chose to see
uncommitted changes, and the gutter keeps doing that.

### Links (iteration 1)

Actions in the `gitlab` namespace, also in the status bar menu:

- `open merge request`, `copy merge request url`;
- `open pipeline`, `copy pipeline url`;
- `copy file link in merge request`: link to the first selected line (or the
  cursor line) of the active file in the MR diff,
  `<mr web_url>/diffs#<sha1(path)>_<old>_<new>`, GitLab's line code: `old` is
  the line in the base version for an unchanged line and the next base line
  for an added one. It comes from the hunks of the MR-aware display diff;
  when the display diff is not against the MR target, the link is to the
  file only, `#<sha1(path)>`. The path is relative to the repository root.

Permalinks to a file or line on a commit stay with the existing
`editor: copy permalink to line`, which already supports GitLab.

### Merge request list (iteration 2)

- `gitlab: merge requests` opens a picker over the active repository's
  project. A segmented filter at the top: Assigned to me, Review requested,
  Created by me, All open.
  - `GET /projects/:path/merge_requests?state=opened&scope=all` with
    `assignee_id`, `reviewer_id` or `author_id` of the current user
    (`GET /user`, cached per instance).
- Row: `!iid`, draft badge, title, `source → target`, author, pipeline
  status icon, updated time. Fuzzy match on iid, title, branch and author.
- Confirm opens the MR in the browser. Secondary confirm checks out the
  source branch: fetch the remote, then switch to the branch, creating a
  local tracking branch when needed. Copy URL is a third keybinding shown in
  the picker footer.

## Architecture

New crate `crates/gitlab` (`[lib] path = "src/gitlab.rs"`), registered from
`zed`'s `init`.

- `api.rs` — `GitlabClient` over `Arc<dyn HttpClient>`: instance base URL,
  token, typed requests for the endpoints above, serde types for merge
  request, pipeline and user. Pagination is not needed for the iteration 1
  queries; the picker reads the first 100.
- `token.rs` — token lookup for a host: environment (scoped by `GITLAB_HOST`),
  then `glab`.
- `merge_request_store.rs` — global `Entity<GitlabStore>`, one per app. It
  subscribes to each project's `GitStore` events, keeps
  `HashMap<RepositoryId, RepoGitlabState>` (project ref, branch, MR, pipeline,
  last error, refresh task), runs the refresh timer and sets the `GitStore`
  base ref override. Emits `GitlabStoreEvent::Changed`.
- `status_item.rs` — `StatusItemView` showing the active workspace's
  repository state, the context menu and the tooltip. It reports window
  activation to the store, which pauses polling while the window is inactive.
- `links.rs` — GitLab project detection from a remote URL (registered
  providers, `git_hosting_providers` entries of kind `gitlab`, self-hosted
  detection) and in-MR file URLs.
- `actions.rs` — action definitions and workspace registration.
- `merge_request_picker.rs` — iteration 2.

Changes outside the crate:

- `project::git_store`: `set_diff_base_override(repo_id, Option<SharedString>)`
  and its use in `refresh_diff_base_for_repo`.
- `git_ui::branch_diff`: `open_branch_diff`, which opens the branch diff with
  a given base ref.
- `zed`: crate init and the status bar item.

## Testing

- `api`: requests and parsing against `FakeHttpClient` with recorded GitLab
  responses; 401 drops the token.
- `links`: URL building for plain, subgroup and self-hosted projects, path
  hashing, line anchors for unchanged and added lines.
- `merge_request_store`: with a fake repository and fake HTTP client, branch
  change triggers a lookup, opened/merged/closed fallback order, timer
  interval follows pipeline status, error keeps the last good state, override
  set and cleared on `GitStore`.
- `git_store`: override wins over the default branch with
  `diff_base: default_branch` and is ignored with `head`.
- Manual check in the built Zed against a self-hosted GitLab project with an
  open MR and a running pipeline.
