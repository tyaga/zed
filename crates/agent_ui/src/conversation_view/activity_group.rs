use std::ops::Range;
use std::path::PathBuf;

use acp_thread::{
    AcpThread, AgentThreadEntry, AssistantMessage, AssistantMessageChunk, ToolCall,
    ToolCallContent, ToolCallStatus,
};
use agent_client_protocol::v2::schema as acp_v2;
use gpui::{App, SharedString};

use super::subagent_card::acp_subagent_card;

const MAX_LISTED_MCP_SERVERS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupRole {
    Member,
    Boundary,
    /// An assistant message with visible text whose leading thought chunks
    /// are folded into the preceding activity group.
    TextWithLeadingThoughts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityGroup {
    pub members: Range<usize>,
    pub trailing_thoughts: Option<usize>,
}

impl ActivityGroup {
    /// Entry that draws the summary and keys the expansion state.
    pub fn key(&self) -> usize {
        self.members.start
    }

    pub fn entries(&self) -> Range<usize> {
        let end = self
            .trailing_thoughts
            .map_or(self.members.end, |entry_ix| entry_ix + 1);
        self.members.start..end
    }
}

pub(crate) fn activity_group_at(
    entry_count: usize,
    entry_ix: usize,
    role_of: impl Fn(usize) -> GroupRole,
) -> Option<ActivityGroup> {
    if entry_ix >= entry_count {
        return None;
    }
    match role_of(entry_ix) {
        GroupRole::Boundary => None,
        GroupRole::Member => Some(group_around_member(entry_count, entry_ix, &role_of)),
        GroupRole::TextWithLeadingThoughts => {
            if entry_ix > 0 && role_of(entry_ix - 1) == GroupRole::Member {
                Some(group_around_member(entry_count, entry_ix - 1, &role_of))
            } else {
                Some(ActivityGroup {
                    members: entry_ix..entry_ix,
                    trailing_thoughts: Some(entry_ix),
                })
            }
        }
    }
}

fn group_around_member(
    entry_count: usize,
    member_ix: usize,
    role_of: &impl Fn(usize) -> GroupRole,
) -> ActivityGroup {
    let mut start = member_ix;
    while start > 0 && role_of(start - 1) == GroupRole::Member {
        start -= 1;
    }
    let mut end = member_ix + 1;
    while end < entry_count && role_of(end) == GroupRole::Member {
        end += 1;
    }
    let trailing_thoughts =
        (end < entry_count && role_of(end) == GroupRole::TextWithLeadingThoughts).then_some(end);
    ActivityGroup {
        members: start..end,
        trailing_thoughts,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActivityItem {
    McpCall { server: SharedString },
    ToolCall,
    ShellCommand,
    Read,
    Search,
    Edit { paths: Vec<PathBuf> },
    Subagent,
    Thought,
}

#[derive(Debug, Default)]
pub(crate) struct ActivitySummary {
    mcp_servers: Vec<SharedString>,
    mcp_calls: usize,
    tool_calls: usize,
    shell_commands: usize,
    reads: usize,
    searches: usize,
    edited_paths: Vec<PathBuf>,
    edits_without_paths: usize,
    subagents: usize,
    thoughts: usize,
    failed: usize,
    total: usize,
}

impl ActivitySummary {
    pub fn add(&mut self, item: ActivityItem, failed: bool) {
        self.total += 1;
        if failed {
            self.failed += 1;
        }
        match item {
            ActivityItem::McpCall { server } => {
                self.mcp_calls += 1;
                if !self.mcp_servers.contains(&server) {
                    self.mcp_servers.push(server);
                }
            }
            ActivityItem::ToolCall => self.tool_calls += 1,
            ActivityItem::ShellCommand => self.shell_commands += 1,
            ActivityItem::Read => self.reads += 1,
            ActivityItem::Search => self.searches += 1,
            ActivityItem::Edit { paths } => {
                if paths.is_empty() {
                    self.edits_without_paths += 1;
                }
                for path in paths {
                    if !self.edited_paths.contains(&path) {
                        self.edited_paths.push(path);
                    }
                }
            }
            ActivityItem::Subagent => self.subagents += 1,
            ActivityItem::Thought => self.thoughts += 1,
        }
    }

    pub fn failed_count(&self) -> usize {
        self.failed
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        if self.mcp_calls > 0 {
            let mut servers = self
                .mcp_servers
                .iter()
                .take(MAX_LISTED_MCP_SERVERS)
                .map(|server| server.as_ref())
                .collect::<Vec<_>>()
                .join(", ");
            let unlisted = self
                .mcp_servers
                .len()
                .saturating_sub(MAX_LISTED_MCP_SERVERS);
            if unlisted > 0 {
                servers.push_str(&format!(" and {unlisted} more"));
            }
            parts.push(format!(
                "called {servers} {}",
                pluralize(self.mcp_calls, "time", "times")
            ));
        }
        if self.tool_calls > 0 {
            parts.push(format!(
                "called {}",
                pluralize(self.tool_calls, "tool", "tools")
            ));
        }
        if self.shell_commands > 0 {
            parts.push(format!(
                "ran {}",
                pluralize(self.shell_commands, "shell command", "shell commands")
            ));
        }
        if self.reads > 0 {
            parts.push(format!("read {}", pluralize(self.reads, "file", "files")));
        }
        if self.searches > 0 {
            parts.push(format!(
                "searched {}",
                pluralize(self.searches, "time", "times")
            ));
        }
        let edited_files = self.edited_paths.len() + self.edits_without_paths;
        if edited_files > 0 {
            parts.push(format!(
                "edited {}",
                pluralize(edited_files, "file", "files")
            ));
        }
        if self.subagents > 0 {
            parts.push(format!(
                "ran {}",
                pluralize(self.subagents, "subagent", "subagents")
            ));
        }
        if self.thoughts > 0 {
            parts.push(format!(
                "thought {}",
                pluralize(self.thoughts, "time", "times")
            ));
        }
        capitalize_first(&parts.join(", "))
    }
}

fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Claude Code reports the MCP server in `_meta.claudeCode`; other agents only
/// expose the `mcp__<server>__<tool>` naming convention.
pub(crate) fn mcp_server_name(
    meta: Option<&acp_v2::Meta>,
    tool_name: Option<&str>,
) -> Option<SharedString> {
    let claude_code = meta.and_then(|meta| meta.get("claudeCode"));
    let declared_server = claude_code
        .and_then(|claude_code| claude_code.get("mcpServer"))
        .and_then(|server| server.get("name"))
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty());
    if let Some(server) = declared_server {
        return Some(SharedString::from(server.to_owned()));
    }
    claude_code
        .and_then(|claude_code| claude_code.get("toolName"))
        .and_then(|name| name.as_str())
        .and_then(server_from_tool_name)
        .or_else(|| tool_name.and_then(server_from_tool_name))
}

fn server_from_tool_name(tool_name: &str) -> Option<SharedString> {
    let (server, _) = tool_name.strip_prefix("mcp__")?.split_once("__")?;
    (!server.is_empty()).then(|| SharedString::from(server.to_owned()))
}

pub(crate) fn first_line_of_label(source: &str) -> String {
    let mut lines = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some(first_line) = lines.next() else {
        return String::new();
    };
    if lines.next().is_some() {
        format!("{first_line}…")
    } else {
        first_line.to_owned()
    }
}

/// A canceled tool call that produced nothing is not drawn at all.
pub(crate) fn tool_call_has_visible_content(tool_call: &ToolCall, cx: &App) -> bool {
    tool_call.content().iter().any(|content| match content {
        ToolCallContent::ContentBlock { block, .. } => block.visible_content(cx),
        ToolCallContent::Diff(_)
        | ToolCallContent::LegacyDiff { .. }
        | ToolCallContent::Terminal { .. }
        | ToolCallContent::SessionReference(_) => true,
        ToolCallContent::DiffPatch { render, .. } => {
            !render.files.is_empty()
                || render
                    .fallback
                    .as_ref()
                    .is_some_and(|markdown| !markdown.read(cx).source().is_empty())
        }
        ToolCallContent::Other { markdown, .. } => !markdown.read(cx).source().is_empty(),
    })
}

fn is_hidden_tool_call(tool_call: &ToolCall, cx: &App) -> bool {
    matches!(tool_call.status(), ToolCallStatus::Canceled)
        && !tool_call_has_visible_content(tool_call, cx)
}

/// Thought chunks drawn before the first visible text chunk of the message.
pub(crate) fn leading_thought_count(message: &AssistantMessage, cx: &App) -> usize {
    let mut count = 0;
    for chunk in &message.chunks {
        match chunk {
            AssistantMessageChunk::Message { block, .. } => {
                if block.visible_content(cx) {
                    break;
                }
            }
            AssistantMessageChunk::Thought { block, .. } => {
                if block.visible_content(cx) {
                    count += 1;
                }
            }
        }
    }
    count
}

fn has_visible_text(message: &AssistantMessage, cx: &App) -> bool {
    message.chunks.iter().any(|chunk| match chunk {
        AssistantMessageChunk::Message { block, .. } => block.visible_content(cx),
        AssistantMessageChunk::Thought { .. } => false,
    })
}

pub(crate) fn group_role(thread: &AcpThread, entry_ix: usize, cx: &App) -> GroupRole {
    let Some(entry) = thread.entries().get(entry_ix) else {
        return GroupRole::Boundary;
    };
    match entry {
        AgentThreadEntry::ToolCall(_) => GroupRole::Member,
        AgentThreadEntry::SessionMessage(message) => {
            if acp_subagent_card(thread, entry_ix, message, cx).is_some() {
                GroupRole::Member
            } else {
                GroupRole::Boundary
            }
        }
        AgentThreadEntry::AssistantMessage(message) => {
            if !has_visible_text(message, cx) {
                GroupRole::Member
            } else if leading_thought_count(message, cx) > 0 {
                GroupRole::TextWithLeadingThoughts
            } else {
                GroupRole::Boundary
            }
        }
        AgentThreadEntry::UserMessage(_)
        | AgentThreadEntry::Elicitation(_)
        | AgentThreadEntry::ContextCompaction(_) => GroupRole::Boundary,
    }
}

fn tool_call_item(tool_call: &ToolCall) -> ActivityItem {
    if tool_call.is_subagent() {
        return ActivityItem::Subagent;
    }
    match tool_call.kind() {
        acp_v2::ToolKind::Execute => ActivityItem::ShellCommand,
        acp_v2::ToolKind::Read => ActivityItem::Read,
        acp_v2::ToolKind::Search => ActivityItem::Search,
        acp_v2::ToolKind::Edit | acp_v2::ToolKind::Delete | acp_v2::ToolKind::Move => {
            ActivityItem::Edit {
                paths: tool_call
                    .locations
                    .iter()
                    .map(|location| location.path.clone())
                    .collect(),
            }
        }
        acp_v2::ToolKind::Think => ActivityItem::Thought,
        _ => match mcp_server_name(tool_call.meta.as_ref(), tool_call.tool_name.as_deref()) {
            Some(server) => ActivityItem::McpCall { server },
            None => ActivityItem::ToolCall,
        },
    }
}

/// Items with whether each one failed.
pub(crate) fn activity_items(
    thread: &AcpThread,
    entry_ix: usize,
    cx: &App,
) -> Vec<(ActivityItem, bool)> {
    let Some(entry) = thread.entries().get(entry_ix) else {
        return Vec::new();
    };
    match entry {
        AgentThreadEntry::ToolCall(tool_call) => {
            if is_hidden_tool_call(tool_call, cx) {
                return Vec::new();
            }
            let failed = matches!(
                tool_call.status(),
                ToolCallStatus::Failed | ToolCallStatus::Rejected
            );
            vec![(tool_call_item(tool_call), failed)]
        }
        AgentThreadEntry::SessionMessage(message) => {
            if acp_subagent_card(thread, entry_ix, message, cx).is_some() {
                vec![(ActivityItem::Subagent, false)]
            } else {
                Vec::new()
            }
        }
        AgentThreadEntry::AssistantMessage(message) => (0..leading_thought_count(message, cx))
            .map(|_| (ActivityItem::Thought, false))
            .collect(),
        AgentThreadEntry::UserMessage(_)
        | AgentThreadEntry::Elicitation(_)
        | AgentThreadEntry::ContextCompaction(_) => Vec::new(),
    }
}

pub(crate) fn activity_summary(
    thread: &AcpThread,
    group: &ActivityGroup,
    cx: &App,
) -> ActivitySummary {
    let mut summary = ActivitySummary::default();
    for entry_ix in group.entries() {
        for (item, failed) in activity_items(thread, entry_ix, cx) {
            summary.add(item, failed);
        }
    }
    summary
}

pub(crate) fn current_step_label(
    thread: &AcpThread,
    group: &ActivityGroup,
    cx: &App,
) -> Option<String> {
    group.members.clone().rev().find_map(|entry_ix| {
        let AgentThreadEntry::ToolCall(tool_call) = thread.entries().get(entry_ix)? else {
            return None;
        };
        matches!(
            tool_call.status(),
            ToolCallStatus::Pending | ToolCallStatus::InProgress
        )
        .then(|| first_line_of_label(&tool_call.label.read(cx).source()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v2::schema as acp_v2;
    use serde_json::json;
    use std::ops::Range;

    use GroupRole::{Boundary as B, Member as M, TextWithLeadingThoughts as T};

    fn group(roles: &[GroupRole], ix: usize) -> Option<(Range<usize>, Option<usize>)> {
        activity_group_at(roles.len(), ix, |i| roles[i])
            .map(|group| (group.members, group.trailing_thoughts))
    }

    #[test]
    fn group_spans_consecutive_members() {
        let roles = [B, M, M, B];
        assert_eq!(group(&roles, 1), Some((1..3, None)));
        assert_eq!(group(&roles, 2), Some((1..3, None)));
        assert_eq!(group(&roles, 0), None);
        assert_eq!(group(&roles, 3), None);
    }

    #[test]
    fn text_with_leading_thoughts_joins_previous_group() {
        let roles = [B, M, T];
        assert_eq!(group(&roles, 1), Some((1..2, Some(2))));
        assert_eq!(group(&roles, 2), Some((1..2, Some(2))));
    }

    #[test]
    fn text_with_leading_thoughts_without_previous_group() {
        let roles = [B, T];
        assert_eq!(group(&roles, 1), Some((1..1, Some(1))));
    }

    #[test]
    fn boundary_splits_groups() {
        let roles = [M, B, M];
        assert_eq!(group(&roles, 0), Some((0..1, None)));
        assert_eq!(group(&roles, 2), Some((2..3, None)));
    }

    #[test]
    fn group_open_at_end_of_thread() {
        assert_eq!(group(&[B, M, M], 2), Some((1..3, None)));
    }

    #[test]
    fn group_key_and_entries() {
        let with_members = ActivityGroup {
            members: 1..3,
            trailing_thoughts: Some(3),
        };
        assert_eq!(with_members.key(), 1);
        assert_eq!(with_members.entries(), 1..4);
        let thoughts_only = ActivityGroup {
            members: 4..4,
            trailing_thoughts: Some(4),
        };
        assert_eq!(thoughts_only.key(), 4);
        assert_eq!(thoughts_only.entries(), 4..5);
    }

    #[test]
    fn summary_text_matches_cli_format() {
        let mut summary = ActivitySummary::default();
        for server in ["mcp-builds", "mcp-dns", "mcp-search"] {
            summary.add(
                ActivityItem::McpCall {
                    server: server.into(),
                },
                false,
            );
        }
        summary.add(ActivityItem::ToolCall, false);
        for _ in 0..4 {
            summary.add(ActivityItem::ShellCommand, false);
        }
        assert_eq!(
            summary.text(),
            "Called mcp-builds, mcp-dns, mcp-search 3 times, called 1 tool, ran 4 shell commands"
        );
    }

    #[test]
    fn summary_singular_and_order() {
        let mut summary = ActivitySummary::default();
        summary.add(ActivityItem::Thought, false);
        summary.add(ActivityItem::Read, false);
        summary.add(ActivityItem::ShellCommand, false);
        summary.add(ActivityItem::Search, false);
        summary.add(ActivityItem::Subagent, false);
        assert_eq!(
            summary.text(),
            "Ran 1 shell command, read 1 file, searched 1 time, ran 1 subagent, thought 1 time"
        );
    }

    #[test]
    fn summary_more_than_three_mcp_servers() {
        let mut summary = ActivitySummary::default();
        for server in ["a", "b", "a", "c", "d", "e"] {
            summary.add(
                ActivityItem::McpCall {
                    server: server.into(),
                },
                false,
            );
        }
        assert_eq!(summary.text(), "Called a, b, c and 2 more 6 times");
    }

    #[test]
    fn summary_counts_unique_edited_paths() {
        let mut summary = ActivitySummary::default();
        summary.add(
            ActivityItem::Edit {
                paths: vec!["/a.rs".into()],
            },
            false,
        );
        summary.add(
            ActivityItem::Edit {
                paths: vec!["/a.rs".into(), "/b.rs".into()],
            },
            false,
        );
        summary.add(ActivityItem::Edit { paths: vec![] }, false);
        assert_eq!(summary.text(), "Edited 3 files");
    }

    #[test]
    fn summary_counts_failures() {
        let mut summary = ActivitySummary::default();
        assert!(summary.is_empty());
        summary.add(ActivityItem::ShellCommand, true);
        summary.add(ActivityItem::ShellCommand, false);
        assert!(!summary.is_empty());
        assert_eq!(summary.failed_count(), 1);
        assert_eq!(summary.text(), "Ran 2 shell commands");
    }

    #[test]
    fn mcp_server_name_sources() {
        let meta: acp_v2::Meta = serde_json::from_value(json!({
            "claudeCode": {
                "toolName": "mcp__mcp-builds__get_build",
                "mcpServer": {"name": "builds", "source": "user"}
            }
        }))
        .expect("meta");
        assert_eq!(
            mcp_server_name(Some(&meta), None).as_deref(),
            Some("builds")
        );
        let meta: acp_v2::Meta = serde_json::from_value(json!({
            "claudeCode": {"toolName": "mcp__mcp-builds__get_build"}
        }))
        .expect("meta");
        assert_eq!(
            mcp_server_name(Some(&meta), None).as_deref(),
            Some("mcp-builds")
        );
        assert_eq!(
            mcp_server_name(None, Some("mcp__search__search_code")).as_deref(),
            Some("search")
        );
        assert_eq!(mcp_server_name(None, Some("Bash")), None);
    }

    #[test]
    fn first_line_of_fenced_label() {
        assert_eq!(
            first_line_of_label("```\ngit status --short\n```"),
            "git status --short"
        );
        assert_eq!(
            first_line_of_label("```sh\ncd /tmp\nls -la\n```"),
            "cd /tmp…"
        );
        assert_eq!(first_line_of_label("Read file"), "Read file");
    }
}
