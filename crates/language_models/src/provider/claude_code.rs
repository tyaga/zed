use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use futures::future::BoxFuture;
use futures::io::BufReader;
use futures::stream::BoxStream;
use futures::{AsyncBufReadExt as _, AsyncWriteExt as _, FutureExt as _, StreamExt as _};
use gpui::{
    AnyView, App, AppContext as _, AsyncApp, BackgroundExecutor, Render, Task, Window, div,
};
use language_model::{
    AuthenticateError, InlineDescription, InlineProviderSettings, LanguageModel,
    LanguageModelClient, LanguageModelCompletionError, LanguageModelCompletionEvent,
    LanguageModelCompletionStream, LanguageModelId, LanguageModelName, LanguageModelProvider,
    LanguageModelProviderId, LanguageModelProviderName, LanguageModelProviderState,
    LanguageModelRequest, MessageContent, ProviderErrorCategory, ProviderSettingsView, Role,
    StopReason, TokenUsage,
};
use serde_json::Value;
use settings::Settings as _;
use util::command::{Child, Stdio};

use crate::AllLanguageModelSettings;

pub const PROVIDER_ID: LanguageModelProviderId = LanguageModelProviderId::new("claude-code");
pub const PROVIDER_NAME: LanguageModelProviderName = LanguageModelProviderName::new("Claude Code");

pub const MODEL_IDS: [&str; 4] = ["sonnet", "opus", "haiku", "fable"];

#[derive(Default, Clone, Debug)]
pub struct ClaudeCodeSettings {
    pub path: Option<PathBuf>,
}

const SIGN_IN_MARKERS: [&str; 5] = ["login", "log in", "sign in", "not logged in", "authenticat"];

/// Arguments that run the CLI as a plain text model: no tools, no session
/// files and no user, project or MCP configuration.
pub(crate) fn claude_code_arguments(model: &str, system_prompt: Option<&str>) -> Vec<String> {
    let mut arguments: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        "--model",
        model,
        "--tools",
        "",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--strict-mcp-config",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    if let Some(system_prompt) = system_prompt {
        arguments.push("--system-prompt".to_string());
        arguments.push(system_prompt.to_string());
    }
    arguments
}

pub(crate) struct ClaudeCodePrompt {
    pub system_prompt: Option<String>,
    pub prompt: String,
}

fn message_text(content: &[MessageContent]) -> String {
    content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn claude_code_prompt(request: &LanguageModelRequest) -> ClaudeCodePrompt {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();
    for message in &request.messages {
        let text = message_text(&message.content);
        if text.is_empty() {
            continue;
        }
        match message.role {
            Role::System => system_parts.push(text),
            Role::User => turns.push(("User", text)),
            Role::Assistant => turns.push(("Assistant", text)),
        }
    }

    let system_prompt = (!system_parts.is_empty()).then(|| system_parts.join("\n\n"));
    let prompt = match turns.as_slice() {
        [("User", text)] => text.clone(),
        _ => turns
            .iter()
            .map(|(role, text)| format!("{role}:\n{text}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    };
    ClaudeCodePrompt {
        system_prompt,
        prompt,
    }
}

#[derive(Debug)]
pub(crate) enum ClaudeCodeFailure {
    SignInRequired(String),
    Error(String),
}

pub(crate) enum ClaudeCodeLine {
    Event(LanguageModelCompletionEvent),
    Finished,
    Failed(ClaudeCodeFailure),
    Ignored,
}

fn token_usage(usage: &Value) -> TokenUsage {
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    TokenUsage {
        input_tokens: field("input_tokens"),
        output_tokens: field("output_tokens"),
        cache_creation_input_tokens: field("cache_creation_input_tokens"),
        cache_read_input_tokens: field("cache_read_input_tokens"),
    }
}

fn parse_stream_event(event: &Value) -> ClaudeCodeLine {
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => ClaudeCodeLine::Event(LanguageModelCompletionEvent::Started),
        Some("content_block_delta") => {
            let Some(delta) = event.get("delta") else {
                return ClaudeCodeLine::Ignored;
            };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => delta
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| {
                        ClaudeCodeLine::Event(LanguageModelCompletionEvent::Text(text.to_string()))
                    })
                    .unwrap_or(ClaudeCodeLine::Ignored),
                Some("thinking_delta") => delta
                    .get("thinking")
                    .and_then(Value::as_str)
                    .map(|text| {
                        ClaudeCodeLine::Event(LanguageModelCompletionEvent::Thinking {
                            text: text.to_string(),
                            signature: None,
                        })
                    })
                    .unwrap_or(ClaudeCodeLine::Ignored),
                _ => ClaudeCodeLine::Ignored,
            }
        }
        Some("message_delta") => event
            .get("usage")
            .map(|usage| {
                ClaudeCodeLine::Event(LanguageModelCompletionEvent::UsageUpdate(token_usage(
                    usage,
                )))
            })
            .unwrap_or(ClaudeCodeLine::Ignored),
        _ => ClaudeCodeLine::Ignored,
    }
}

fn parse_result(value: &Value) -> ClaudeCodeLine {
    if !value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ClaudeCodeLine::Finished;
    }
    let message = value
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("Claude Code reported an error")
        .to_string();
    let lowercase = message.to_lowercase();
    if SIGN_IN_MARKERS
        .iter()
        .any(|marker| lowercase.contains(marker))
    {
        ClaudeCodeLine::Failed(ClaudeCodeFailure::SignInRequired(message))
    } else {
        ClaudeCodeLine::Failed(ClaudeCodeFailure::Error(message))
    }
}

pub(crate) fn parse_claude_code_line(line: &str) -> ClaudeCodeLine {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return ClaudeCodeLine::Ignored;
    };
    match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => value
            .get("event")
            .map(parse_stream_event)
            .unwrap_or(ClaudeCodeLine::Ignored),
        Some("result") => parse_result(&value),
        _ => ClaudeCodeLine::Ignored,
    }
}

const MAX_TOKEN_COUNT: u64 = 200_000;
const STDERR_TAIL_LINES: usize = 20;
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const SIGN_IN_MESSAGE: &str =
    "Claude Code is not signed in. Run `claude` in a terminal and sign in.";

/// An explicitly configured path is never substituted with a discovered one,
/// so a wrong setting surfaces as a missing binary instead of silently
/// running a different `claude`.
fn resolve_claude_binary(
    configured: Option<&Path>,
    path_env: Option<&OsStr>,
    home: &Path,
) -> Option<PathBuf> {
    if let Some(configured) = configured {
        return configured.is_file().then(|| configured.to_path_buf());
    }
    path_env
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("claude"))
        .chain([
            home.join(".local/bin/claude"),
            home.join(".claude/local/claude"),
        ])
        .find(|candidate| candidate.is_file())
}

fn claude_binary(cx: &App) -> Option<PathBuf> {
    resolve_claude_binary(
        AllLanguageModelSettings::get_global(cx)
            .claude_code
            .path
            .as_deref(),
        std::env::var_os("PATH").as_deref(),
        paths::home_dir(),
    )
}

fn model_display_name(id: &str) -> String {
    match id {
        "sonnet" => "Claude Sonnet".to_string(),
        "opus" => "Claude Opus".to_string(),
        "haiku" => "Claude Haiku".to_string(),
        "fable" => "Claude Fable".to_string(),
        other => other.to_string(),
    }
}

fn claude_code_model(id: &'static str) -> LanguageModel {
    LanguageModel {
        supports_tools: false,
        supports_images: false,
        ..LanguageModel::new(
            LanguageModelId(id.into()),
            LanguageModelName(model_display_name(id).into()),
            PROVIDER_ID,
            PROVIDER_NAME,
            format!("claude-code/{id}"),
            MAX_TOKEN_COUNT,
        )
    }
}

pub struct ClaudeCodeProvider;

impl ClaudeCodeProvider {
    pub fn new(_cx: &mut App) -> Self {
        Self
    }
}

impl LanguageModelProviderState for ClaudeCodeProvider {
    type ObservableEntity = ();

    fn observable_entity(&self) -> Option<gpui::Entity<Self::ObservableEntity>> {
        None
    }
}

impl LanguageModelProvider for ClaudeCodeProvider {
    fn id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }

    fn name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }

    fn default_model(&self, _cx: &App) -> Option<LanguageModel> {
        Some(claude_code_model(MODEL_IDS[0]))
    }

    fn default_fast_model(&self, _cx: &App) -> Option<LanguageModel> {
        Some(claude_code_model(MODEL_IDS[0]))
    }

    fn provided_models(&self, _cx: &App) -> Vec<LanguageModel> {
        MODEL_IDS.into_iter().map(claude_code_model).collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        claude_binary(cx).is_some()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        Task::ready(if claude_binary(cx).is_some() {
            Ok(())
        } else {
            Err(AuthenticateError::CredentialsNotFound)
        })
    }

    fn settings_view(&self, cx: &mut App) -> Option<ProviderSettingsView> {
        let description = match claude_binary(cx) {
            Some(binary) => format!("Uses {}", binary.display()),
            None => {
                "`claude` not found — install Claude Code or set language_models.claude_code.path"
                    .to_string()
            }
        };
        Some(ProviderSettingsView::Inline(InlineProviderSettings {
            title: Some(PROVIDER_NAME.0),
            description: Some(InlineDescription::Text(description.into())),
            create_view: Arc::new(|_window: &mut Window, cx: &mut App| -> AnyView {
                cx.new(|_| ConfigurationView).into()
            }),
        }))
    }
}

struct ConfigurationView;

impl Render for ConfigurationView {
    fn render(
        &mut self,
        _window: &mut Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        div()
    }
}

impl LanguageModelClient for ClaudeCodeProvider {
    fn stream_completion(
        &self,
        model: &LanguageModel,
        request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<'static, Result<LanguageModelCompletionStream, LanguageModelCompletionError>>
    {
        let Some(binary) = cx.update(|cx| claude_binary(cx)) else {
            return futures::future::ready(Err(LanguageModelCompletionError::NoApiKey {
                provider: PROVIDER_NAME,
            }))
            .boxed();
        };
        let prompt = claude_code_prompt(&request);
        let arguments = claude_code_arguments(&model.id.0, prompt.system_prompt.as_deref());
        let executor = cx.background_executor().clone();
        async move {
            let process =
                ClaudeCodeProcess::spawn(&binary, &arguments, &prompt.prompt, executor).await?;
            Ok(process.into_events())
        }
        .boxed()
    }
}

/// Owns everything a running completion depends on, so dropping the
/// completion stream kills the CLI and removes its working directory.
struct ClaudeCodeProcess {
    lines: BoxStream<'static, std::io::Result<String>>,
    stderr_tail: Task<VecDeque<String>>,
    executor: BackgroundExecutor,
    _child: Child,
    _working_directory: tempfile::TempDir,
}

impl ClaudeCodeProcess {
    async fn spawn(
        binary: &Path,
        arguments: &[String],
        prompt: &str,
        executor: BackgroundExecutor,
    ) -> anyhow::Result<Self> {
        // An empty working directory keeps the CLI from picking up project
        // files or instructions from wherever Zed was launched.
        let working_directory =
            tempfile::tempdir().context("failed to create a working directory for Claude Code")?;
        let mut child = util::command::new_command(binary)
            .args(arguments)
            .current_dir(working_directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to start {}", binary.display()))?;
        let mut stdin = child
            .stdin
            .take()
            .context("Claude Code stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("Claude Code stdout unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("Claude Code stderr unavailable")?;

        // Stderr is drained concurrently so a chatty CLI cannot block on a full
        // pipe while stdout is being read.
        let stderr_tail = executor.spawn(async move {
            let mut tail = VecDeque::with_capacity(STDERR_TAIL_LINES);
            let mut lines = BufReader::new(stderr).lines();
            while let Some(Ok(line)) = lines.next().await {
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            tail
        });

        stdin
            .write_all(prompt.as_bytes())
            .await
            .context("failed to send the prompt to Claude Code")?;
        drop(stdin);

        Ok(Self {
            lines: BufReader::new(stdout).lines().boxed(),
            stderr_tail,
            executor,
            _child: child,
            _working_directory: working_directory,
        })
    }

    async fn stderr_tail(self) -> Vec<String> {
        let timeout = self.executor.timer(STDERR_DRAIN_TIMEOUT);
        futures::select_biased! {
            tail = self.stderr_tail.fuse() => tail.into(),
            _ = timeout.fuse() => Vec::new(),
        }
    }

    fn into_events(self) -> LanguageModelCompletionStream {
        futures::stream::unfold(Some(self), |process| async move {
            let mut process = process?;
            loop {
                let event = match process.lines.next().await {
                    Some(Ok(line)) => match parse_claude_code_line(&line) {
                        ClaudeCodeLine::Event(event) => return Some((Ok(event), Some(process))),
                        ClaudeCodeLine::Ignored => continue,
                        ClaudeCodeLine::Finished => {
                            Ok(LanguageModelCompletionEvent::Stop(StopReason::EndTurn))
                        }
                        ClaudeCodeLine::Failed(ClaudeCodeFailure::SignInRequired(message)) => {
                            log::warn!("Claude Code sign-in required: {message}");
                            Err(LanguageModelCompletionError::ProviderRejection {
                                provider: PROVIDER_NAME,
                                status: None,
                                code: None,
                                message: SIGN_IN_MESSAGE.to_string(),
                                retry_after: None,
                                category: ProviderErrorCategory::Authentication,
                            })
                        }
                        ClaudeCodeLine::Failed(ClaudeCodeFailure::Error(message)) => {
                            Err(LanguageModelCompletionError::Other(anyhow!(message)))
                        }
                    },
                    Some(Err(error)) => Err(LanguageModelCompletionError::Other(
                        anyhow::Error::new(error).context("failed to read Claude Code output"),
                    )),
                    None => {
                        let tail = process.stderr_tail().await;
                        log::warn!(
                            "Claude Code exited without a result. Last stderr lines:\n{}",
                            tail.join("\n")
                        );
                        return Some((
                            Err(LanguageModelCompletionError::StreamEndedUnexpectedly {
                                provider: PROVIDER_NAME,
                            }),
                            None,
                        ));
                    }
                };
                return Some((event, None));
            }
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{BorrowAppContext as _, TestAppContext};
    use language_model::{
        LanguageModelImage, LanguageModelRequestMessage, LanguageModelRequestMessage as Message,
    };
    use settings::SettingsStore;

    fn message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![MessageContent::Text(text.to_string())],
            cache: false,
            reasoning_details: None,
        }
    }

    fn request(messages: Vec<LanguageModelRequestMessage>) -> LanguageModelRequest {
        LanguageModelRequest {
            messages,
            ..Default::default()
        }
    }

    #[test]
    fn test_arguments_without_system_prompt() {
        assert_eq!(
            claude_code_arguments("sonnet", None),
            [
                "-p",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--model",
                "sonnet",
                "--tools",
                "",
                "--no-session-persistence",
                "--setting-sources",
                "",
                "--strict-mcp-config",
            ]
        );
    }

    #[test]
    fn test_arguments_with_system_prompt() {
        let arguments = claude_code_arguments("opus", Some("Be terse."));
        assert_eq!(
            arguments[arguments.len() - 2..],
            ["--system-prompt", "Be terse."]
        );
        assert_eq!(arguments[6], "opus");
    }

    #[test]
    fn test_single_user_message_prompt() {
        let prompt = claude_code_prompt(&request(vec![message(Role::User, "hello")]));
        assert_eq!(prompt.prompt, "hello");
        assert_eq!(prompt.system_prompt, None);
    }

    #[test]
    fn test_system_messages_joined() {
        let prompt = claude_code_prompt(&request(vec![
            message(Role::System, "A"),
            message(Role::System, "B"),
            message(Role::User, "q"),
        ]));
        assert_eq!(prompt.system_prompt.as_deref(), Some("A\n\nB"));
        assert_eq!(prompt.prompt, "q");
    }

    #[test]
    fn test_transcript_prompt() {
        let prompt = claude_code_prompt(&request(vec![
            message(Role::User, "q1"),
            message(Role::Assistant, "a1"),
            message(Role::User, "q2"),
        ]));
        assert_eq!(prompt.prompt, "User:\nq1\n\nAssistant:\na1\n\nUser:\nq2");
    }

    #[test]
    fn test_non_text_content_dropped() {
        let prompt = claude_code_prompt(&request(vec![LanguageModelRequestMessage {
            role: Role::User,
            content: vec![
                MessageContent::Image(LanguageModelImage::empty()),
                MessageContent::Text("look".to_string()),
            ],
            cache: false,
            reasoning_details: None,
        }]));
        assert_eq!(prompt.prompt, "look");
    }

    #[test]
    fn test_only_system_messages() {
        let prompt = claude_code_prompt(&request(vec![message(Role::System, "A")]));
        assert_eq!(prompt.prompt, "");
        assert_eq!(prompt.system_prompt.as_deref(), Some("A"));
    }

    #[test]
    fn test_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}},"session_id":"s"}"#;
        assert!(matches!(
            parse_claude_code_line(line),
            ClaudeCodeLine::Event(LanguageModelCompletionEvent::Text(text)) if text == "hi"
        ));
    }

    #[test]
    fn test_thinking_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}}"#;
        assert!(matches!(
            parse_claude_code_line(line),
            ClaudeCodeLine::Event(LanguageModelCompletionEvent::Thinking { text, signature: None })
                if text == "hmm"
        ));
    }

    #[test]
    fn test_message_start() {
        let line = r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-sonnet-5-5","id":"msg_x","type":"message","role":"assistant","content":[]}},"session_id":"s"}"#;
        assert!(matches!(
            parse_claude_code_line(line),
            ClaudeCodeLine::Event(LanguageModelCompletionEvent::Started)
        ));
    }

    #[test]
    fn test_message_delta_usage() {
        let line = r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}}"#;
        match parse_claude_code_line(line) {
            ClaudeCodeLine::Event(LanguageModelCompletionEvent::UsageUpdate(usage)) => {
                assert_eq!(
                    usage,
                    TokenUsage {
                        output_tokens: 7,
                        ..Default::default()
                    }
                );
            }
            _ => panic!("expected usage update"),
        }
    }

    #[test]
    fn test_successful_result() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"stop_reason":"end_turn","result":"ok","usage":{"input_tokens":2,"output_tokens":4}}"#;
        assert!(matches!(
            parse_claude_code_line(line),
            ClaudeCodeLine::Finished
        ));
        assert!(matches!(
            parse_claude_code_line(r#"{"type":"result","is_error":false}"#),
            ClaudeCodeLine::Finished
        ));
    }

    #[test]
    fn test_sign_in_error() {
        let line =
            r#"{"type":"result","is_error":true,"result":"Invalid API key · Please run /login"}"#;
        assert!(matches!(
            parse_claude_code_line(line),
            ClaudeCodeLine::Failed(ClaudeCodeFailure::SignInRequired(_))
        ));
    }

    #[test]
    fn test_other_error() {
        let line = r#"{"type":"result","is_error":true,"result":"Overloaded"}"#;
        assert!(matches!(
            parse_claude_code_line(line),
            ClaudeCodeLine::Failed(ClaudeCodeFailure::Error(message)) if message == "Overloaded"
        ));
    }

    fn create_file(path: &Path) -> PathBuf {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "").unwrap();
        path.to_path_buf()
    }

    #[test]
    fn test_resolve_binary_prefers_configured_path() {
        let directory = tempfile::tempdir().unwrap();
        let configured = create_file(&directory.path().join("custom/claude"));
        let on_path = directory.path().join("bin");
        create_file(&on_path.join("claude"));
        let path_env = std::env::join_paths([&on_path]).unwrap();

        assert_eq!(
            resolve_claude_binary(Some(&configured), Some(&path_env), directory.path()),
            Some(configured)
        );
        assert_eq!(
            resolve_claude_binary(
                Some(&directory.path().join("missing/claude")),
                Some(&path_env),
                directory.path()
            ),
            None
        );
    }

    #[test]
    fn test_resolve_binary_from_path_env() {
        let directory = tempfile::tempdir().unwrap();
        let empty = directory.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let bin = directory.path().join("bin");
        let expected = create_file(&bin.join("claude"));
        create_file(&directory.path().join(".local/bin/claude"));
        let path_env = std::env::join_paths([&empty, &bin]).unwrap();

        assert_eq!(
            resolve_claude_binary(None, Some(&path_env), directory.path()),
            Some(expected)
        );
    }

    #[test]
    fn test_resolve_binary_falls_back_to_local_bin() {
        let home = tempfile::tempdir().unwrap();
        let path_env = std::env::join_paths([home.path().join("empty")]).unwrap();
        let fallback = create_file(&home.path().join(".claude/local/claude"));

        assert_eq!(
            resolve_claude_binary(None, Some(&path_env), home.path()),
            Some(fallback)
        );

        let local_bin = create_file(&home.path().join(".local/bin/claude"));
        assert_eq!(
            resolve_claude_binary(None, None, home.path()),
            Some(local_bin)
        );
        assert_eq!(
            resolve_claude_binary(None, None, &home.path().join("elsewhere")),
            None
        );
    }

    fn init_provider(path: &Path, cx: &mut TestAppContext) -> ClaudeCodeProvider {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .language_models
                        .get_or_insert_default()
                        .claude_code
                        .get_or_insert_default()
                        .path = Some(path.to_string_lossy().into_owned());
                });
            });
            ClaudeCodeProvider::new(cx)
        })
    }

    #[cfg(unix)]
    fn fake_cli(directory: &Path, lines: &[&str]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let mut script = String::from("#!/bin/sh\ncat > /dev/null\n");
        for line in lines {
            script.push_str(&format!("printf '%s\\n' '{line}'\n"));
        }
        let path = directory.join("claude");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn stream_events(
        provider: &ClaudeCodeProvider,
        cx: &mut TestAppContext,
    ) -> Result<
        Vec<Result<LanguageModelCompletionEvent, LanguageModelCompletionError>>,
        LanguageModelCompletionError,
    > {
        let model = cx.update(|cx| provider.default_model(cx)).unwrap();
        let future = provider.stream_completion(
            &model,
            request(vec![message(Role::User, "hello")]),
            &cx.to_async(),
        );
        Ok(future.await?.collect().await)
    }

    const MESSAGE_START_LINE: &str =
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"content":[]}}}"#;
    const HELLO_LINE: &str = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}}"#;
    const RESULT_LINE: &str =
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello"}"#;

    #[cfg(unix)]
    #[gpui::test]
    async fn test_streams_events_from_cli(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let binary = fake_cli(
            directory.path(),
            &[MESSAGE_START_LINE, HELLO_LINE, RESULT_LINE],
        );
        let provider = init_provider(&binary, cx);

        let events = stream_events(&provider, cx).await.unwrap();
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(matches!(
            events[0],
            Ok(LanguageModelCompletionEvent::Started)
        ));
        assert!(matches!(
            &events[1],
            Ok(LanguageModelCompletionEvent::Text(text)) if text == "Hello"
        ));
        assert!(matches!(
            events[2],
            Ok(LanguageModelCompletionEvent::Stop(StopReason::EndTurn))
        ));
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn test_stream_without_result_is_an_error(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let binary = fake_cli(directory.path(), &[HELLO_LINE]);
        let provider = init_provider(&binary, cx);

        let events = stream_events(&provider, cx).await.unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(
            &events[0],
            Ok(LanguageModelCompletionEvent::Text(text)) if text == "Hello"
        ));
        assert!(matches!(
            events[1],
            Err(LanguageModelCompletionError::StreamEndedUnexpectedly { .. })
        ));
    }

    #[gpui::test]
    async fn test_missing_binary(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let provider = init_provider(&directory.path().join("missing/claude"), cx);

        assert!(!cx.update(|cx| provider.is_authenticated(cx)));
        assert!(matches!(
            stream_events(&provider, cx).await,
            Err(LanguageModelCompletionError::NoApiKey { .. })
        ));
    }

    #[test]
    fn test_ignored_lines() {
        assert!(matches!(
            parse_claude_code_line("not json"),
            ClaudeCodeLine::Ignored
        ));
        assert!(matches!(
            parse_claude_code_line(r#"{"type":"system","subtype":"init"}"#),
            ClaudeCodeLine::Ignored
        ));
        assert!(matches!(
            parse_claude_code_line(r#"{"type":"rate_limit_event"}"#),
            ClaudeCodeLine::Ignored
        ));
    }
}
