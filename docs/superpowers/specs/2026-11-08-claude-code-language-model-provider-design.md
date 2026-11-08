# Claude Code language model provider

## Goal

Inline assist (editor and terminal), commit message generation, and other
features that take a model from Zed's language model registry need an API key
or a Zed account. The user already works with Claude through Claude Code and
its own sign-in. This provider runs the local `claude` CLI in headless mode so
those features use the existing Claude Code sign-in, with no API key.

## Behavior

- A provider with id `claude-code` and name "Claude Code" appears in every
  model picker that uses the language model registry.
- Models: `sonnet` (default and default fast model), `opus`, `haiku`, `fable`.
  The id is passed to the CLI as `--model <id>`; Claude Code resolves the alias
  to the current version.
- Models report no tool support and no image support, so the agent panel does
  not offer them for agent threads.
- Each completion request starts one `claude` process and streams its output.
  Cancelling the request (dropping the stream) kills the process.
- The provider is authenticated when the `claude` binary is found. A request
  that fails because Claude Code is not signed in returns an error telling the
  user to run `claude` and sign in.
- The provider settings view shows the resolved binary path and whether it was
  found; there is nothing to enter.

## Design

### Settings

`language_models.claude_code.path`: path to the `claude` binary, default
`null`. When unset, the provider looks for `claude` on `PATH`, then
`~/.local/bin/claude`, then `~/.claude/local/claude`.

### Command line

```
claude -p --output-format stream-json --include-partial-messages --verbose
       --model <model> --tools "" --no-session-persistence
       --setting-sources "" --strict-mcp-config
       [--system-prompt <system text>]
```

- `--setting-sources ""` and `--strict-mcp-config` keep the user's Claude Code
  hooks, MCP servers, and settings out of the request: they slow it down and
  change the answer (for example, a response style set by a hook).
- `--bare` is not used: it ignores the OAuth sign-in and requires an API key.
- The working directory is a fresh empty temporary directory, so no project
  `CLAUDE.md` is loaded.
- The prompt is written to stdin, which is then closed.

### Request conversion

- `Role::System` messages are joined with blank lines into `--system-prompt`.
- When the remaining messages are a single user message, its text is the
  prompt.
- Otherwise the messages are rendered as a transcript, each block headed by
  `User:` or `Assistant:`, ending with the last user message.
- `MessageContent::Text` and `Thinking` text are used; images, tool uses, and
  tool results are dropped (the models declare no support for them).
- `temperature`, `stop`, and `max_output_tokens` are not supported by the CLI
  and are ignored.

### Output parsing

Each stdout line is a JSON object:

- `{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":…}}}`
  → `LanguageModelCompletionEvent::Text`.
- `thinking_delta` → `Thinking { text, signature: None }`.
- `message_start` → `Started`.
- `message_delta` with usage, and `result.usage` → `UsageUpdate`.
- `{"type":"result","is_error":false}` → `Stop(StopReason::EndTurn)`, then
  the stream ends.
- `{"type":"result","is_error":true,"result":…}` → an error with that text;
  text that mentions signing in or `/login` becomes the sign-in error.
- Other lines and lines that are not JSON are ignored.
- If the process exits before a `result` line, the stream ends with
  `StreamEndedUnexpectedly`, including the last stderr lines in the log.

### Code layout

- `crates/language_models/src/provider/claude_code.rs`:
  - pure functions `claude_code_arguments`, `claude_code_prompt`,
    `parse_claude_code_line`, unit tested;
  - `ClaudeCodeProvider` implementing `LanguageModelProvider`,
    `LanguageModelClient`, `LanguageModelProviderState` (no observable state);
  - a small settings view.
- Registration in `language_models.rs`; settings in `settings_content`
  (`ClaudeCodeSettingsContent`), `AllLanguageModelSettings`, and
  `assets/settings/default.json`.

## Testing

- Unit tests: argument building (system prompt present or absent), prompt
  building (single message, transcript, dropped non-text content), line
  parsing (text, thinking, usage, success result, error result, sign-in error,
  garbage).
- Provider test: point `path` at a test script that prints a recorded stream
  and exits; assert the provider yields the expected events. A second script
  exits without a `result` line; assert the error.

## Out of scope

- Tool calls and images.
- Reusing a long-running CLI process between requests.
- Selecting effort or thinking through the CLI.
