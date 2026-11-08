use std::ops::Range;

use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use gpui::{App, ClipboardItem, Context, Window, actions};
use language::Point;
use settings::GitDiffBaseSetting;
use workspace::{
    Toast, Workspace,
    notifications::{NotificationId, NotifyTaskExt as _},
};

use crate::{BranchInfo, BranchState, GitlabStore, TOKEN_ENV_VAR, merge_request_file_url};

actions!(
    gitlab,
    [
        /// Opens the merge request of the current branch in the browser.
        OpenMergeRequest,
        /// Copies the link to the merge request of the current branch.
        CopyMergeRequestUrl,
        /// Opens the latest pipeline of the current branch in the browser.
        OpenPipeline,
        /// Copies the link to the latest pipeline of the current branch.
        CopyPipelineUrl,
        /// Copies the link to the current file and line on the merge request's Changes tab.
        CopyFileLinkInMergeRequest,
        /// Opens the diff of the current branch against the merge request's target branch.
        OpenMergeRequestDiff,
        /// Reloads the merge request and pipeline of the current branch from GitLab.
        Refresh,
    ]
);

pub const NOT_GITLAB_MESSAGE: &str = "Active repository is not hosted on GitLab";

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| register(workspace))
        .detach();
}

fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &OpenMergeRequest, _, cx| {
        let result = branch_info(workspace, cx).and_then(|info| {
            let merge_request = require_merge_request(&info)?;
            cx.open_url(merge_request.web_url.as_str());
            Ok(())
        });
        report(workspace, result, cx);
    });
    workspace.register_action(|workspace, _: &CopyMergeRequestUrl, _, cx| {
        let result = branch_info(workspace, cx).and_then(|info| {
            let merge_request = require_merge_request(&info)?;
            cx.write_to_clipboard(ClipboardItem::new_string(merge_request.web_url.to_string()));
            Ok(())
        });
        report(workspace, result, cx);
    });
    workspace.register_action(|workspace, _: &OpenPipeline, _, cx| {
        let result = branch_info(workspace, cx).and_then(|info| {
            let pipeline = require_pipeline(&info)?;
            cx.open_url(pipeline.web_url.as_str());
            Ok(())
        });
        report(workspace, result, cx);
    });
    workspace.register_action(|workspace, _: &CopyPipelineUrl, _, cx| {
        let result = branch_info(workspace, cx).and_then(|info| {
            let pipeline = require_pipeline(&info)?;
            cx.write_to_clipboard(ClipboardItem::new_string(pipeline.web_url.to_string()));
            Ok(())
        });
        report(workspace, result, cx);
    });
    workspace.register_action(|workspace, _: &OpenMergeRequestDiff, window, cx| {
        let result = branch_info(workspace, cx).and_then(|info| {
            let target_ref = info.target_ref().ok_or_else(|| {
                anyhow!("No open merge request for branch {}", info.branch)
            })?;
            if info.fetched_target_ref().is_none() {
                return Err(anyhow!("{target_ref} is not fetched: fetch it to see the diff"));
            }
            git_ui::branch_diff::open_branch_diff(workspace, target_ref, window, cx)
        });
        report(workspace, result, cx);
    });
    workspace.register_action(|workspace, _: &Refresh, _, cx| {
        GitlabStore::for_project(workspace.project(), cx)
            .update(cx, |store, cx| store.refresh(cx));
    });
    workspace.register_action(|workspace, _: &CopyFileLinkInMergeRequest, window, cx| {
        if let Err(error) = copy_file_link_in_merge_request(workspace, window, cx) {
            report(workspace, Err(error), cx);
        }
    });
}

fn branch_info(workspace: &Workspace, cx: &mut App) -> Result<BranchInfo> {
    let store = GitlabStore::for_project(workspace.project(), cx);
    let state = store.read(cx).state().clone();
    match state {
        BranchState::Ready(info) => Ok(info),
        BranchState::NotGitlab => Err(anyhow!(NOT_GITLAB_MESSAGE)),
        BranchState::NoToken { host } => Err(anyhow!(
            "No GitLab token for {host}: set {TOKEN_ENV_VAR} or run glab auth login"
        )),
        BranchState::NoBranch => Err(anyhow!("No branch is checked out in the active repository")),
        BranchState::Loading => Err(anyhow!("GitLab data for the current branch is still loading")),
    }
}

fn require_merge_request(info: &BranchInfo) -> Result<&crate::MergeRequest> {
    info.merge_request
        .as_ref()
        .ok_or_else(|| anyhow!("No merge request for branch {}", info.branch))
}

fn require_pipeline(info: &BranchInfo) -> Result<&crate::Pipeline> {
    info.pipeline
        .as_ref()
        .ok_or_else(|| anyhow!("No pipeline for branch {}", info.branch))
}

fn report(workspace: &mut Workspace, result: Result<()>, cx: &mut Context<Workspace>) {
    if let Err(error) = result {
        struct GitlabActionError;
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<GitlabActionError>(),
                error.to_string(),
            )
            .autohide(),
            cx,
        );
    }
}

fn copy_file_link_in_merge_request(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    let info = branch_info(workspace, cx)?;
    let web_url = require_merge_request(&info)?.web_url.clone();
    let editor = workspace
        .active_item_as::<Editor>(cx)
        .context("No active editor")?;
    let (buffer, row) = editor
        .update(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton()?;
            let display_snapshot = editor.display_snapshot(cx);
            let selection = editor.selections.newest::<Point>(&display_snapshot);
            Some((buffer, selection.start.row))
        })
        .context("The active editor does not show a single file")?;

    let git_store = workspace.project().read(cx).git_store().clone();
    let (repository, repo_path) = git_store
        .read(cx)
        .repository_and_path_for_buffer_id(buffer.read(cx).remote_id(), cx)
        .context("The file is not in a git repository")?;
    if repository.read(cx).id != info.repository_id {
        return Err(anyhow!("The file is not in the merge request's repository"));
    }
    let path = repo_path.as_unix_str().to_string();

    // Old line numbers are only correct when the display diff is the one
    // against the merge request target.
    let diff_is_against_target = {
        let git_store = git_store.read(cx);
        git_store.diff_base() == GitDiffBaseSetting::DefaultBranch
            && git_store.diff_base_override(info.repository_id) == info.target_ref().as_ref()
            && info.target_ref().is_some()
    };
    if !diff_is_against_target {
        let url = merge_request_file_url(&web_url, &path, None);
        cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
        return Ok(());
    }

    let diff_task = git_store.update(cx, |git_store, cx| {
        git_store.open_display_diff(buffer.clone(), cx)
    });
    let task = cx.spawn_in(window, async move |_, cx| {
        let diff = diff_task.await?;
        cx.update(|_, cx| {
            let buffer_snapshot = buffer.read(cx).text_snapshot();
            let diff_snapshot = diff.read(cx).snapshot(cx);
            let base_text = diff_snapshot.base_text();
            let hunks = diff_snapshot
                .hunks(&buffer_snapshot)
                .map(|hunk| {
                    let base_range = base_text.offset_to_point(hunk.diff_base_byte_range.start)
                        ..base_text.offset_to_point(hunk.diff_base_byte_range.end);
                    (row_range(hunk.range), row_range(base_range))
                })
                .collect::<Vec<_>>();
            let base_line = base_line_for(row, &hunks) + 1;
            let url = merge_request_file_url(&web_url, &path, Some((base_line, row + 1)));
            cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
        })
    });
    task.detach_and_notify_err(workspace.weak_handle(), window, cx);
    Ok(())
}

fn row_range(range: Range<Point>) -> Range<u32> {
    let end = if range.end.column > 0 {
        range.end.row + 1
    } else {
        range.end.row
    };
    range.start.row..end
}

/// Maps a 0-based buffer row to the 0-based base row GitLab pairs with it in
/// line codes: the row itself when unchanged, the base row after the hunk's
/// deletions when added. `hunks` are `(buffer rows, base rows)` in buffer order.
fn base_line_for(row: u32, hunks: &[(Range<u32>, Range<u32>)]) -> u32 {
    let mut mapping = (0, 0);
    for (buffer_rows, base_rows) in hunks {
        if buffer_rows.contains(&row) {
            return base_rows.end;
        }
        if buffer_rows.end <= row {
            mapping = (buffer_rows.end, base_rows.end);
        }
    }
    mapping.1 + (row - mapping.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_line_mapping() {
        // Rows 2..5 of the buffer replace row 2 of the base (two lines added),
        // and base rows 6..8 are deleted just before buffer row 8.
        let hunks = [(2..5, 2..3), (8..8, 6..8)];
        assert_eq!(base_line_for(0, &hunks), 0);
        assert_eq!(base_line_for(1, &hunks), 1);
        // Added rows carry the position of the next base row, as in GitLab's
        // line codes.
        assert_eq!(base_line_for(2, &hunks), 3);
        assert_eq!(base_line_for(4, &hunks), 3);
        assert_eq!(base_line_for(5, &hunks), 3);
        assert_eq!(base_line_for(7, &hunks), 5);
        assert_eq!(base_line_for(8, &hunks), 8);
        assert_eq!(base_line_for(10, &hunks), 10);
    }
}
