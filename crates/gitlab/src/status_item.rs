use gpui::{
    Action as _, Anchor, App, Context, Entity, IntoElement, Render, Subscription, Window, prelude::*,
};
use ui::{Button, ContextMenu, Icon, IconName, IconSize, LabelSize, PopoverMenu, Tooltip, prelude::*};
use workspace::{HideStatusItem, ItemHandle, StatusItemView, Workspace};

use crate::{
    BranchInfo, BranchState, CopyFileLinkInMergeRequest, CopyMergeRequestUrl, CopyPipelineUrl,
    GitlabStore, MergeRequestState, OpenMergeRequest, OpenMergeRequestDiff, OpenPipeline,
    PipelineStatus, Refresh, TOKEN_ENV_VAR,
};

pub struct GitlabStatusItem {
    store: Entity<GitlabStore>,
    active_item_is_editor: bool,
    _subscriptions: Vec<Subscription>,
}

impl GitlabStatusItem {
    pub fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = GitlabStore::for_project(workspace.project(), cx);
        let is_active = window.is_window_active();
        store.update(cx, |store, cx| store.set_window_active(is_active, cx));
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |this, window, cx| {
                let is_active = window.is_window_active();
                this.store
                    .update(cx, |store, cx| store.set_window_active(is_active, cx));
            }),
        ];
        Self {
            store,
            active_item_is_editor: false,
            _subscriptions: subscriptions,
        }
    }
}

impl StatusItemView for GitlabStatusItem {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_item_is_editor = active_pane_item
            .and_then(|item| item.act_as::<editor::Editor>(cx))
            .is_some();
        cx.notify();
    }

    /// Shown only for repositories hosted on GitLab.
    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        None
    }
}

fn pipeline_icon(status: PipelineStatus) -> (IconName, Color) {
    match status {
        PipelineStatus::Success => (IconName::Check, Color::Success),
        PipelineStatus::Failed => (IconName::XCircle, Color::Error),
        PipelineStatus::Running => (IconName::ArrowCircle, Color::Info),
        PipelineStatus::Created
        | PipelineStatus::Pending
        | PipelineStatus::Preparing
        | PipelineStatus::WaitingForResource
        | PipelineStatus::Scheduled => (IconName::Clock, Color::Info),
        PipelineStatus::Canceled
        | PipelineStatus::Skipped
        | PipelineStatus::Manual
        | PipelineStatus::Unknown => (IconName::Circle, Color::Muted),
    }
}

fn label(info: &BranchInfo) -> Option<SharedString> {
    match &info.merge_request {
        Some(merge_request) => {
            let mut label = format!("!{}", merge_request.iid);
            if merge_request.draft {
                label = format!("Draft {label}");
            }
            match merge_request.state {
                MergeRequestState::Merged => label.push_str(" merged"),
                MergeRequestState::Closed => label.push_str(" closed"),
                MergeRequestState::Locked => label.push_str(" locked"),
                MergeRequestState::Opened => {}
            }
            Some(label.into())
        }
        None if info.pipeline.is_some() => Some("Pipeline".into()),
        None if info.last_error.is_some() => Some("GitLab".into()),
        None => None,
    }
}

fn format_duration(seconds: u64) -> String {
    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn tooltip_text(info: &BranchInfo) -> String {
    let mut lines = Vec::new();
    if let Some(merge_request) = &info.merge_request {
        lines.push(merge_request.title.clone());
        lines.push(format!(
            "{} → {}",
            merge_request.source_branch, merge_request.target_branch
        ));
        lines.push(format!("Author: {}", merge_request.author.name));
    } else {
        lines.push(format!("No merge request for {}", info.branch));
    }
    if let Some(pipeline) = &info.pipeline {
        let mut line = format!("Pipeline #{}: {}", pipeline.id, pipeline.status.label());
        if let Some(duration) = pipeline.duration {
            line.push_str(&format!(", {}", format_duration(duration)));
        }
        lines.push(line);
    }
    if info.target_ref_missing
        && let Some(merge_request) = &info.merge_request
    {
        lines.push(format!(
            "Gutter compares with the default branch: {}/{} is not fetched",
            info.remote, merge_request.target_branch
        ));
    }
    lines.push(format!(
        "Updated {} ago",
        format_duration(info.last_refreshed.elapsed().as_secs())
    ));
    if let Some(error) = &info.last_error {
        lines.push(format!("Last refresh failed: {error}"));
    }
    lines.join("\n")
}

impl Render for GitlabStatusItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.store.read(cx).state().clone();
        let info = match state {
            BranchState::Ready(info) => info,
            BranchState::NoToken { host } => {
                return h_flex()
                    .child(
                        Button::new("gitlab-status-no-token", "GitLab: no token")
                            .label_size(LabelSize::Small)
                            .start_icon(
                                Icon::new(IconName::Gitlab)
                                    .size(IconSize::Small)
                                    .color(Color::Warning),
                            )
                            .tooltip(Tooltip::text(format!(
                                "No token for {host}: set {TOKEN_ENV_VAR} or run glab auth login"
                            )))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Refresh.boxed_clone(), cx)
                            }),
                    )
                    .into_any_element();
            }
            BranchState::NotGitlab | BranchState::NoBranch | BranchState::Loading => {
                return div().into_any_element();
            }
        };
        let Some(label) = label(&info) else {
            return div().into_any_element();
        };

        let (icon, mut icon_color) = info
            .pipeline
            .as_ref()
            .map(|pipeline| pipeline_icon(pipeline.status))
            .unwrap_or((IconName::Gitlab, Color::Muted));
        if info.last_error.is_some() {
            icon_color = Color::Warning;
        }
        let tooltip = tooltip_text(&info);
        let has_merge_request = info.merge_request.is_some();
        let has_open_merge_request = info.fetched_target_ref().is_some();
        let has_pipeline = info.pipeline.is_some();
        let active_item_is_editor = self.active_item_is_editor;

        h_flex()
            .child(
                PopoverMenu::new("gitlab-status-menu")
                    .anchor(Anchor::BottomLeft)
                    .trigger_with_tooltip(
                        Button::new("gitlab-status-trigger", label)
                            .label_size(LabelSize::Small)
                            .start_icon(Icon::new(icon).size(IconSize::Small).color(icon_color)),
                        Tooltip::text(tooltip),
                    )
                    .menu(move |window, cx| {
                        Some(ContextMenu::build(window, cx, |menu, _, _| {
                            menu.when(has_merge_request, |menu| {
                                menu.action("Open Merge Request", OpenMergeRequest.boxed_clone())
                                    .action(
                                        "Copy Merge Request URL",
                                        CopyMergeRequestUrl.boxed_clone(),
                                    )
                            })
                            .when(has_merge_request && active_item_is_editor, |menu| {
                                menu.action(
                                    "Copy File Link in Merge Request",
                                    CopyFileLinkInMergeRequest.boxed_clone(),
                                )
                            })
                            .when(has_open_merge_request, |menu| {
                                menu.action(
                                    "Open Merge Request Diff",
                                    OpenMergeRequestDiff.boxed_clone(),
                                )
                            })
                            .when(has_pipeline, |menu| {
                                menu.separator()
                                    .action("Open Pipeline", OpenPipeline.boxed_clone())
                                    .action("Copy Pipeline URL", CopyPipelineUrl.boxed_clone())
                            })
                            .separator()
                            .action("Refresh", Refresh.boxed_clone())
                        }))
                    }),
            )
            .into_any_element()
    }
}
