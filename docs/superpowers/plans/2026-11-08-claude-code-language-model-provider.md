# Claude Code Language Model Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `claude-code` language model provider that streams completions from the local `claude` CLI, so inline assist and commit messages use the user's Claude Code sign-in.

**Architecture:** One new module `crates/language_models/src/provider/claude_code.rs`: pure request/response helpers (unit tested) and a provider that spawns `claude -p --output-format stream-json` per request and maps stdout lines to completion events. A `language_models.claude_code.path` setting overrides binary lookup.

**Tech Stack:** Rust, GPUI, `language_model` traits, `util::command::new_command`.

**Spec:** `docs/superpowers/specs/2026-11-08-claude-code-language-model-provider-design.md`

## Global Constraints

- Provider id `claude-code`, name `Claude Code`.
- Models (ids, in this order): `sonnet`, `opus`, `haiku`, `fable`; default and default fast model `sonnet`; `supports_tools: false`, `supports_images: false`.
- CLI arguments exactly: `-p --output-format stream-json --include-partial-messages --verbose --model <model> --tools "" --no-session-persistence --setting-sources "" --strict-mcp-config`, plus `--system-prompt <text>` only when there is system text. `--tools` and `--setting-sources` take an empty-string argument.
- Never `--bare`.
- Process cwd: a fresh empty temporary directory; prompt via stdin, then stdin closed; kill on drop.
- Binary lookup when `path` is unset: `PATH`, then `~/.local/bin/claude`, then `~/.claude/local/claude`.
- Rust rules from `CLAUDE.md`: no `unwrap()` outside tests, no `let _ =` on fallible ops, full-word names, no `mod.rs`.
- No personal data in code/tests; comments explain non-obvious why only; no history words.
- Commits: plain `crate: Imperative summary`, no AI attribution; never push.

## Review Focus

- A response with no `result` line (process killed or crashed): stream must end with an error, not hang. Test in Task 2.
- A `result` line with `is_error: true` whose text mentions "login" / "/login" / "sign in" / "not logged in" / "authenticat": the sign-in error. Test in Task 1.
- Very long prompts: passed via stdin, never as an argument (argument length limits). Covered by Task 1 argument test asserting the prompt is not in the arguments.
- `claude` binary missing: `is_authenticated` false and `stream_completion` returns `NoApiKey`-style error immediately, no spawn. Test in Task 2.
- Request with only system messages and no user message: prompt is empty string; CLI still runs (no panic). Test in Task 1.

---

### Task 1: Pure helpers and settings

**Files:**
- Create: `crates/language_models/src/provider/claude_code.rs` (helpers + tests only in this task)
- Modify: `crates/language_models/src/provider.rs` (`pub mod claude_code;`)
- Modify: `crates/settings_content/src/language_model.rs` (`claude_code: Option<ClaudeCodeSettingsContent>` in `AllLanguageModelSettingsContent`, struct `ClaudeCodeSettingsContent { path: Option<String> }` with the same derives as `OllamaSettingsContent`)
- Modify: `crates/language_models/src/settings.rs` (`claude_code: ClaudeCodeSettings` with `path: Option<PathBuf>`)
- Modify: `assets/settings/default.json` (`"claude_code": { "path": null }` under `language_models`, with a comment)

**Interfaces (produces):**
```rust
pub const PROVIDER_ID: LanguageModelProviderId;      // "claude-code"
pub const PROVIDER_NAME: LanguageModelProviderName;  // "Claude Code"
pub const MODEL_IDS: [&str; 4];                      // ["sonnet", "opus", "haiku", "fable"]
pub(crate) fn claude_code_arguments(model: &str, system_prompt: Option<&str>) -> Vec<String>;
pub(crate) struct ClaudeCodePrompt { pub system_prompt: Option<String>, pub prompt: String }
pub(crate) fn claude_code_prompt(request: &LanguageModelRequest) -> ClaudeCodePrompt;
pub(crate) enum ClaudeCodeLine { Event(LanguageModelCompletionEvent), Finished, Failed(ClaudeCodeFailure), Ignored }
pub(crate) enum ClaudeCodeFailure { SignInRequired(String), Error(String) }
pub(crate) fn parse_claude_code_line(line: &str) -> ClaudeCodeLine;
pub struct ClaudeCodeSettings { pub path: Option<PathBuf> }
```
`Finished` is emitted for a successful `result`; the caller then emits `Stop(EndTurn)`.

- [ ] **Step 1: Write failing tests** in `claude_code.rs`:
  - `test_arguments_without_system_prompt`: equals the exact Global Constraints list with `sonnet`.
  - `test_arguments_with_system_prompt`: ends with `["--system-prompt", "Be terse."]`.
  - `test_single_user_message_prompt`: one user text → `prompt == text`, `system_prompt == None`.
  - `test_system_messages_joined`: two system messages `"A"`, `"B"` → `Some("A\n\nB")`.
  - `test_transcript_prompt`: user/assistant/user → `"User:\nq1\n\nAssistant:\na1\n\nUser:\nq2"`.
  - `test_non_text_content_dropped`: a user message with an image and text → only the text.
  - `test_only_system_messages`: prompt is `""`.
  - Line parsing: text_delta → `Event(Text("hi"))`; thinking_delta → `Event(Thinking{text, signature: None})`; message_start → `Event(Started)`; `message_delta` with `usage.output_tokens` → `Event(UsageUpdate(..))`; `{"type":"result","is_error":false}` → `Finished`; `{"type":"result","is_error":true,"result":"Invalid API key · Please run /login"}` → `Failed(SignInRequired(..))`; `{"type":"result","is_error":true,"result":"Overloaded"}` → `Failed(Error("Overloaded"))`; `"not json"` and `{"type":"system"}` → `Ignored`.
- [ ] **Step 2: Run** `cargo test -p language_models --lib claude_code` — expected FAIL (missing items).
- [ ] **Step 3: Implement** the helpers and settings plumbing. Parse with `serde_json::Value` (no new structs needed beyond small private ones if clearer).
- [ ] **Step 4: Run** `cargo test -p language_models --lib claude_code` and `cargo test -p settings --lib` — PASS.
- [ ] **Step 5: Commit** `language_models: Add Claude Code request and response helpers`.

### Task 2: Provider and registration

**Files:**
- Modify: `crates/language_models/src/provider/claude_code.rs`
- Modify: `crates/language_models/src/language_models.rs` (register after the last provider)

**Interfaces:**
- Consumes Task 1 items.
- Produces: `pub struct ClaudeCodeProvider` with `pub fn new(cx: &mut App) -> Self`; implements `LanguageModelProvider`, `LanguageModelClient`, `LanguageModelProviderState` (`ObservableEntity = ()`-like empty or no entity: `observable_entity` returns `None`); `fn resolve_claude_binary(configured: Option<&Path>, path_env: Option<&OsStr>, home: &Path) -> Option<PathBuf>` (pure, tested).

- [ ] **Step 1: Write failing tests:**
  - `test_resolve_binary_prefers_configured_path`, `test_resolve_binary_from_path_env`, `test_resolve_binary_falls_back_to_local_bin` (use a temp dir with an executable file).
  - `#[gpui::test] test_streams_events_from_cli`: write an executable shell script to a temp dir that ignores args, reads stdin, and prints three recorded lines (message_start, text_delta "Hello", result success); set `language_models.claude_code.path` to it; `stream_completion` for `sonnet` yields `Started`, `Text("Hello")`, `Stop(EndTurn)` in order.
  - `#[gpui::test] test_stream_without_result_is_an_error`: script prints one text_delta then exits 0; the stream yields the text and then `Err(StreamEndedUnexpectedly)`.
  - `#[gpui::test] test_missing_binary`: `path` pointing to a nonexistent file → `is_authenticated` false; `stream_completion` returns an error without spawning.
- [ ] **Step 2: Run** `cargo test -p language_models --lib claude_code` — FAIL.
- [ ] **Step 3: Implement.**
  - Models built with `LanguageModel::new(LanguageModelId(id), LanguageModelName(display name e.g. "Claude Sonnet (Claude Code)"), PROVIDER_ID, PROVIDER_NAME, telemetry id "claude-code/<id>", max_token_count 200_000)` with `supports_tools: false, supports_images: false`.
  - `is_authenticated`: binary resolves. `authenticate`: `Ok` when it resolves, else `Err(AuthenticateError::CredentialsNotFound)`.
  - `stream_completion`: resolve binary (error `LanguageModelCompletionError::NoApiKey { provider: PROVIDER_NAME }` if missing); build prompt and args; create a temp dir (`tempfile::tempdir()` kept alive for the stream's lifetime); `util::command::new_command(binary).args(args).current_dir(temp).stdin/stdout/stderr piped().kill_on_drop(true).spawn()`; write prompt to stdin and drop it; map stdout lines through `parse_claude_code_line`: `Event` → yield, `Finished` → yield `Stop(EndTurn)` and end, `Failed(SignInRequired)` → `ProviderRejection` with `category: Authentication` and message "Claude Code is not signed in. Run `claude` in a terminal and sign in.", `Failed(Error(text))` → `Other(anyhow!(text))`; EOF before Finished → `StreamEndedUnexpectedly` (log the last stderr lines with `log::warn!`). Keep the child and temp dir owned by the stream so dropping the stream kills the process.
  - `settings_view`: `ProviderSettingsView::Inline` (or `SubPage`) with title "Claude Code" and a description showing the resolved path or "`claude` not found — install Claude Code or set language_models.claude_code.path".
  - Register in `language_models.rs`; add `tempfile` to `language_models` deps if not present (workspace dep).
- [ ] **Step 4: Run** `cargo test -p language_models --lib` and `cargo check -p language_models -p zed` — PASS, no warnings.
- [ ] **Step 5: Commit** `language_models: Add a Claude Code provider backed by the claude CLI`.

### Task 3: Finish

- [ ] `cargo fmt`; `cargo clippy -p language_models --tests` — no new warnings.
- [ ] User settings (controller, not code): `agent.inline_assistant_model` and `agent.commit_message_model` = `{ "provider": "claude-code", "model": "sonnet" }`.
- [ ] Rebuild with `~/.claude/scripts/zed-dev-update.sh` when the user asks.
