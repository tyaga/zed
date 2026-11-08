use std::path::PathBuf;

use language_model::{
    LanguageModelCompletionEvent, LanguageModelProviderId, LanguageModelProviderName,
    LanguageModelRequest, MessageContent, Role, TokenUsage,
};
use serde_json::Value;

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

#[cfg(test)]
mod tests {
    use super::*;
    use language_model::{
        LanguageModelImage, LanguageModelRequestMessage, LanguageModelRequestMessage as Message,
    };

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
