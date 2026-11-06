# Компактная лента агента: план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** между сообщением пользователя и ответом агента — одна строка-сводка активности вместо карточек; карточка инструмента с тремя состояниями.

**Architecture:** группы вычисляются при рендере записи; строки `ListState` остаются один к одному с `AgentThreadEntry`. Чистая логика (границы групп, текст сводки, разбор имени MCP) — в новом модуле `activity_group.rs`; состояние разворота — в `EntryViewState`; отрисовка — в `ThreadView::render_entry`.

**Tech Stack:** Rust, GPUI, `acp_thread`, `agent_settings`.

**Spec:** `docs/superpowers/specs/2026-11-06-agent-activity-groups-design.md`

## Global Constraints

- Отдельная ветка от ветки с патчем feature flags.
- `.rules` (он же `CLAUDE.md`): перед первой правкой исходников первые две строки `README.md` — `> [!IMPORTANT]` и `> Remove this line to confirm you've reviewed this PR before submitting.`; не удалять.
- `.rules`: без `unwrap()` и паники на индексах, без `let _ =` на fallible-операциях, полные имена переменных, комментарии только «почему», без `mod.rs`.
- Clippy — `./script/clippy`, не `cargo clippy`.
- Настройка: `agent.collapse_activity`, по умолчанию `true`.
- Текст сводки — английский, в формате Claude Code CLI: `Called mcp-builds, mcp-dns 3 times, called 1 tool, ran 4 shell commands`.
- Коммиты — только когда пользователь разрешил; без строк атрибуции ИИ.
- Тулчейн: `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` перед `cargo`.

## Review Focus

- Последняя запись ленты скрыта в свёрнутой группе: кнопки треда (`render_thread_controls`) у последней записи должны остаться видны — скрытая запись заменяет только `primary`, а не делает ранний `return`.
- Разворот/сворот группы меняет высоту нескольких строк сразу: без `list_state.remeasure_items` на диапазон группы строки перекрываются или оставляют пустоты.
- Запрос разрешения внутри свёрнутой группы: карточка с кнопками Allow/Deny видна, иначе агент висит без видимой причины.
- Переключение `collapse_activity` в настройках при открытом треде: высоты всех строк пересчитываются.
- Поиск по треду (`thread_search_bar`) находит текст в свёрнутой записи: при переходе к совпадению группа разворачивается.

---

### Task 1: Настройка `collapse_activity`

**Files:**
- Modify: `README.md:1` (две строки из Global Constraints)
- Modify: `crates/settings_content/src/agent.rs:360` (после `expand_terminal_card`)
- Modify: `crates/agent_settings/src/agent_settings.rs:243`, `:837`
- Modify: `assets/settings/default.json:1389`
- Modify: `crates/settings_ui/src/page_data.rs:9270` (новый `SettingItem` после `Expand Terminal Card`)
- Modify: `crates/agent_ui/src/agent_ui.rs:1100` (тестовые `AgentSettings`)

**Interfaces:**
- Produces: `AgentSettings::collapse_activity: bool`; `AgentSettingsContent::collapse_activity: Option<bool>`.

- [ ] **Step 1:** Создать ветку; добавить две строки в начало `README.md`.
- [ ] **Step 2:** Поле `pub collapse_activity: Option<bool>` в `AgentSettingsContent` с doc-комментарием `/// Whether to collapse the agent's tool calls, edits and thinking between messages into a one-line summary.` и `/// Default: true`.
- [ ] **Step 3:** `pub collapse_activity: bool` в `AgentSettings`, заполнение `agent.collapse_activity.unwrap()`; в `default.json` — `"collapse_activity": true` с тем же комментарием после `expand_terminal_card`; `collapse_activity: true` в `agent_ui.rs:1100`.
- [ ] **Step 4:** `SettingItem` в `page_data.rs`: title `"Collapse Activity"`, description из Step 2, `json_path: Some("agent.collapse_activity")`, `pick`/`write` по образцу `expand_terminal_card`.
- [ ] **Step 5:** Run: `cargo test -p agent_settings && cargo check -p settings_ui -p agent_ui`. Expected: PASS, без ошибок.
- [ ] **Step 6: Commit** — `agent: Add collapse_activity setting`.

### Task 2: Чистая логика групп и сводки

**Files:**
- Create: `crates/agent_ui/src/conversation_view/activity_group.rs`
- Modify: `crates/agent_ui/src/conversation_view.rs` (объявление `mod activity_group;` рядом с `mod subagent_card;`)

**Interfaces:**
- Produces:
  ```rust
  pub(crate) enum GroupRole { Member, Boundary, TextWithLeadingThoughts }
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub(crate) struct ActivityGroup { pub members: Range<usize>, pub trailing_thoughts: Option<usize> }
  impl ActivityGroup {
      /// Entry that draws the summary and keys the expansion state.
      pub fn key(&self) -> usize; // members.start, or trailing_thoughts when members is empty
      pub fn entries(&self) -> Range<usize>; // members.start..(trailing_thoughts + 1 or members.end)
  }
  pub(crate) fn activity_group_at(entry_count: usize, entry_ix: usize, role_of: impl Fn(usize) -> GroupRole) -> Option<ActivityGroup>;
  pub(crate) enum ActivityItem { McpCall { server: SharedString }, ToolCall, ShellCommand, Read, Search, Edit { paths: Vec<PathBuf> }, Subagent, Thought }
  #[derive(Default)] pub(crate) struct ActivitySummary { .. }
  impl ActivitySummary {
      pub fn add(&mut self, item: ActivityItem, failed: bool);
      pub fn text(&self) -> String;
      pub fn failed_count(&self) -> usize;
      pub fn is_empty(&self) -> bool;
  }
  pub(crate) fn mcp_server_name(meta: Option<&acp_v2::Meta>, tool_name: Option<&str>) -> Option<SharedString>;
  pub(crate) fn first_line_of_label(source: &str) -> String;
  ```

`activity_group_at`: для `Member` — идти назад и вперёд по `Member`; если следующая за последним `Member` запись — `TextWithLeadingThoughts`, она становится `trailing_thoughts`. Для `TextWithLeadingThoughts` — если предыдущая запись `Member`, вернуть группу этой записи; иначе `members: entry_ix..entry_ix`, `trailing_thoughts: Some(entry_ix)`. Для `Boundary` — `None`.

`mcp_server_name`: порядок источников из спеки — `meta["claudeCode"]["mcpServer"]["name"]`, затем `mcp__<server>__` в `meta["claudeCode"]["toolName"]`, затем в `tool_name`. Сервер — часть между `mcp__` и следующим `__`.

`first_line_of_label`: отбросить строки-ограждения (начинаются с три обратных кавычки), взять первую непустую строку, `trim`; если строк с содержимым больше одной — добавить `…`.

- [ ] **Step 1: Write the failing tests** (`#[cfg(test)] mod tests` в том же файле, `#[test]`):

```rust
use GroupRole::{Boundary as B, Member as M, TextWithLeadingThoughts as T};
fn group(roles: &[GroupRole], ix: usize) -> Option<(Range<usize>, Option<usize>)> {
    activity_group_at(roles.len(), ix, |i| roles[i]).map(|g| (g.members, g.trailing_thoughts))
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
fn summary_text_matches_cli_format() {
    let mut summary = ActivitySummary::default();
    for server in ["mcp-builds", "mcp-dns", "mcp-search"] {
        summary.add(ActivityItem::McpCall { server: server.into() }, false);
    }
    summary.add(ActivityItem::ToolCall, false);
    for _ in 0..4 { summary.add(ActivityItem::ShellCommand, false); }
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
    assert_eq!(summary.text(), "Ran 1 shell command, read 1 file, searched 1 time, ran 1 subagent, thought 1 time");
}

#[test]
fn summary_more_than_three_mcp_servers() {
    let mut summary = ActivitySummary::default();
    for server in ["a", "b", "a", "c", "d", "e"] {
        summary.add(ActivityItem::McpCall { server: server.into() }, false);
    }
    assert_eq!(summary.text(), "Called a, b, c and 2 more 6 times");
}

#[test]
fn summary_counts_unique_edited_paths() {
    let mut summary = ActivitySummary::default();
    summary.add(ActivityItem::Edit { paths: vec!["/a.rs".into()] }, false);
    summary.add(ActivityItem::Edit { paths: vec!["/a.rs".into(), "/b.rs".into()] }, false);
    summary.add(ActivityItem::Edit { paths: vec![] }, false);
    assert_eq!(summary.text(), "Edited 3 files");
}

#[test]
fn summary_counts_failures() {
    let mut summary = ActivitySummary::default();
    summary.add(ActivityItem::ShellCommand, true);
    summary.add(ActivityItem::ShellCommand, false);
    assert_eq!(summary.failed_count(), 1);
    assert_eq!(summary.text(), "Ran 2 shell commands");
}

#[test]
fn mcp_server_name_sources() {
    let meta: acp_v2::Meta = serde_json::from_value(json!({
        "claudeCode": {"toolName": "mcp__mcp-builds__get_build", "mcpServer": {"name": "builds", "source": "user"}}
    })).expect("meta");
    assert_eq!(mcp_server_name(Some(&meta), None).as_deref(), Some("builds"));
    let meta: acp_v2::Meta = serde_json::from_value(json!({
        "claudeCode": {"toolName": "mcp__mcp-builds__get_build"}
    })).expect("meta");
    assert_eq!(mcp_server_name(Some(&meta), None).as_deref(), Some("mcp-builds"));
    assert_eq!(mcp_server_name(None, Some("mcp__search__search_code")).as_deref(), Some("search"));
    assert_eq!(mcp_server_name(None, Some("Bash")), None);
}

#[test]
fn first_line_of_fenced_label() {
    assert_eq!(first_line_of_label("```\ngit status --short\n```"), "git status --short");
    assert_eq!(first_line_of_label("```sh\ncd /tmp\nls -la\n```"), "cd /tmp…");
    assert_eq!(first_line_of_label("Read file"), "Read file");
}
```

- [ ] **Step 2:** Run: `cargo test -p agent_ui activity_group`. Expected: FAIL — `activity_group_at` not found.
- [ ] **Step 3:** Реализовать интерфейсы из блока Interfaces.
- [ ] **Step 4:** Run: `cargo test -p agent_ui activity_group`. Expected: PASS (12 tests).
- [ ] **Step 5: Commit** — `agent_ui: Add activity group summary logic`.

### Task 3: Состояние разворота в `EntryViewState`

**Files:**
- Modify: `crates/agent_ui/src/entry_view_state.rs:56-128` (поля, методы), `:612-640` (`remove`), тесты в `mod tests` (`:927`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
  pub(crate) enum ToolCallExpansion { #[default] Collapsed, Input, Output }
  impl ToolCallExpansion { pub fn next(self) -> Self } // Collapsed→Input→Output→Collapsed
  impl EntryViewState {
      pub(crate) fn tool_call_expansion(&self, id: &acp_v2::ToolCallId) -> ToolCallExpansion;
      pub(crate) fn cycle_tool_call_expansion(&mut self, id: &acp_v2::ToolCallId);
      pub(crate) fn is_activity_group_expanded(&self, key: usize) -> bool;
      pub(crate) fn toggle_activity_group(&mut self, key: usize);
  }
  ```
- Сохраняются с прежней семантикой: `is_tool_call_expanded` (истина при `Output`), `expand_tool_call` (ставит `Output`), `collapse_tool_call` (удаляет ключ), `toggle_tool_call_expansion` (`Output` ↔ `Collapsed`, для карточек субагентов), `is_tool_call_content_visible`.

`expanded_tool_calls: HashSet<ToolCallId>` → `tool_call_expansions: HashMap<ToolCallId, ToolCallExpansion>`; новое поле `expanded_activity_groups: HashSet<usize>`, в `remove` пересчитывается через `reindex_after_removal`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn tool_call_expansion_cycles() {
    assert_eq!(ToolCallExpansion::Collapsed.next(), ToolCallExpansion::Input);
    assert_eq!(ToolCallExpansion::Input.next(), ToolCallExpansion::Output);
    assert_eq!(ToolCallExpansion::Output.next(), ToolCallExpansion::Collapsed);
}
```

`#[gpui::test] async fn activity_group_expansion_reindexes_on_remove` — по образцу существующего gpui-теста в этом модуле (`:969`) создать `EntryViewState`, `toggle_activity_group(2)` и `toggle_activity_group(5)`, вызвать `remove(1..3)`; assert `!is_activity_group_expanded(2)`, `is_activity_group_expanded(3)`.

`#[gpui::test] async fn expand_tool_call_sets_output` — `expand_tool_call(id)`; assert `tool_call_expansion(&id) == Output` и `is_tool_call_expanded(&id)`; `cycle_tool_call_expansion(&id)`; assert `Collapsed` и `!is_tool_call_expanded(&id)`.

- [ ] **Step 2:** Run: `cargo test -p agent_ui entry_view_state`. Expected: FAIL — `ToolCallExpansion` not found.
- [ ] **Step 3:** Реализовать.
- [ ] **Step 4:** Run: `cargo test -p agent_ui entry_view_state && cargo test -p agent_ui conversation_view::tests`. Expected: PASS, включая существующие тесты на `expand_tool_call`/`collapse_tool_call`.
- [ ] **Step 5: Commit** — `agent_ui: Track three-state tool call and activity group expansion`.

### Task 4: Классификация записей треда

**Files:**
- Modify: `crates/agent_ui/src/conversation_view/activity_group.rs`
- Modify: `crates/agent_ui/src/conversation_view/thread_view.rs:7598-7625` (проверка видимости отменённого tool call выносится в функцию)
- Test: `crates/agent_ui/src/conversation_view.rs` (`mod tests`)

**Interfaces:**
- Consumes: `GroupRole`, `ActivityItem` (Task 2); `acp_subagent_card` (`subagent_card.rs:48`).
- Produces:
  ```rust
  pub(crate) fn tool_call_has_visible_content(tool_call: &ToolCall, cx: &App) -> bool;
  pub(crate) fn group_role(thread: &AcpThread, entry_ix: usize, cx: &App) -> GroupRole;
  pub(crate) fn activity_items(thread: &AcpThread, entry_ix: usize, cx: &App) -> Vec<(ActivityItem, bool)>;
  pub(crate) fn activity_summary(thread: &AcpThread, group: &ActivityGroup, cx: &App) -> ActivitySummary;
  pub(crate) fn current_step_label(thread: &AcpThread, group: &ActivityGroup, cx: &App) -> Option<String>;
  ```

`group_role` — правила раздела «Группа активности» спеки. `activity_items`: `ToolCall` → по `kind()` и `is_subagent()` (`Edit`/`Delete`/`Move` — пути из `locations`), `Canceled` без видимого содержимого — пусто, `failed` для `Failed`/`Rejected`; `AssistantMessage` — по `Thought` на каждый видимый Thought-чанк до первого видимого `Message`-чанка; карточка субагента в `SessionMessage` — `Subagent`. `current_step_label` — последняя запись группы со статусом `Pending`/`InProgress`, `first_line_of_label` от `label.read(cx).source()`.

- [ ] **Step 1: Write the failing test** `#[gpui::test] async fn test_activity_group_classification` в `conversation_view.rs`. Хелпер в тестах:

```rust
fn push_tool_call(thread: &Entity<AcpThread>, id: &str, kind: &str, title: &str, status: &str, meta: Option<serde_json::Value>, cx: &mut VisualTestContext) {
    let mut update = json!({"toolCallId": id, "title": title, "kind": kind, "status": status});
    if let Some(meta) = meta { update["_meta"] = meta; }
    let update: acp_v2::ToolCallUpdate = serde_json::from_value(update).expect("tool call");
    thread.update(cx, |thread, cx| thread.upsert_wire_tool_call(update, cx)).expect("tool call applies");
}
```

Последовательность: user-текст; thought-only сообщение (`push_assistant_content_block(.., true, ..)`); `push_tool_call("t1", "execute", "```\nls\n```", "completed", None)`; `push_tool_call("t2", "other", "get_build", "failed", Some(json!({"claudeCode": {"toolName": "mcp__mcp-builds__get_build"}})))`; assistant-текст. Assert: `group_role` = `[Boundary, Member, Member, Member, Boundary]`; `activity_summary` группы `1..4` — `text() == "Called mcp-builds 1 time, ran 1 shell command, thought 1 time"`, `failed_count() == 1`; `current_step_label == None`. Затем `push_tool_call("t3", "execute", "```\ncargo build\n```", "in_progress", None)` после текста; assert `current_step_label` новой группы `== Some("cargo build")`.

- [ ] **Step 2:** Run: `cargo test -p agent_ui test_activity_group_classification`. Expected: FAIL — `group_role` not found.
- [ ] **Step 3:** Реализовать; в `render_entry` заменить встроенную проверку отменённого tool call вызовом `tool_call_has_visible_content`.
- [ ] **Step 4:** Run: `cargo test -p agent_ui test_activity_group_classification`. Expected: PASS. Если `meta` из `_meta` не доходит до `ToolCall::meta` — проверить `upsert_wire_tool_call` и протянуть, тест должен пройти без изменения ассертов.
- [ ] **Step 5: Commit** — `agent_ui: Classify thread entries into activity groups`.

### Task 5: Сводка и сворачивание в ленте

**Files:**
- Modify: `crates/agent_ui/src/conversation_view/thread_view.rs` — `render_entry` (`:7343`), новая `render_activity_summary`, ветка `AssistantMessage` (`:7533-7575`), конструктор `ThreadView` (наблюдатель настроек)
- Test: `crates/agent_ui/src/conversation_view.rs`

**Interfaces:**
- Consumes: Task 2–4.
- Produces: `fn render_activity_summary(&self, group: &ActivityGroup, is_expanded: bool, cx: &Context<Self>) -> AnyElement`; `fn toggle_activity_group(&mut self, group: ActivityGroup, window: &mut Window, cx: &mut Context<Self>)`; debug selector сводки — `activity-summary-{key}`.

Правила в `render_entry` при `AgentSettings::get_global(cx).collapse_activity` и `Some(group) = activity_group_at(..)`:
- запись `group.key()` — сводка над `primary`;
- группа свёрнута, запись — член группы, не ждёт разрешения (`ToolCallStatus::WaitingForConfirmation` или `render_generic_tool_permissions(..).is_some()`) → `primary` = `Empty`, дальше обычный код обёрток (без раннего `return`), обёртка отступа (`is_indented`) не применяется;
- запись `trailing_thoughts` в свёрнутой группе — `AssistantMessage` без Thought-чанков, предшествующих первому видимому `Message`-чанку.

`render_activity_summary`: `h_flex().id(("activity-summary", key)).debug_selector(..)`, `px_5().py_1().gap_1p5()`, иконка `ChevronRight`/`ChevronDown` `IconSize::XSmall` `Color::Muted`, `Label` текста размера `LabelSize::Custom(self.tool_name_font_size())` `Color::Muted`, при `failed_count() > 0` — `Label` `" · {n} failed"` `Color::Error`; при `ThreadStatus::Generating` и последней группе ленты — `SpinnerLabel` и `Label(current_step).truncate()`; hover — `bg(cx.theme().colors().element_hover)`; `on_click` → `toggle_activity_group`.

`toggle_activity_group`: `entry_view_state.toggle_activity_group(group.key())`; `materialize_tool_call_views(ix, ..)` для каждой записи `group.entries()` при разворачивании; `list_state.remeasure_items(group.entries())`; `refresh_thread_search`; `cx.notify()`.

Наблюдатель: в конструкторе `ThreadView` — `cx.observe_global::<SettingsStore>` (подписка в существующий вектор подписок вида), хранит последнее `collapse_activity`, при изменении — `list_state.remeasure_items(0..list_state.item_count())` и `cx.notify()`.

Поиск: `scroll_to_entry(entry_ix)` (`:8260`, через него идёт переход к совпадению в `thread_search_bar`) разворачивает свёрнутую группу, в которую входит `entry_ix`, тем же путём, что `toggle_activity_group`.

- [ ] **Step 1: Write the failing tests** в `conversation_view.rs` (`add_to_workspace`, `run_until_parked`, `cx.debug_bounds`, как в `test_receipt_submission_waits_for_echo_and_reported_idle`):
  - `test_activity_group_collapses_to_summary`: user-текст, три `execute` tool call'а `t1..t3` `completed`, assistant-текст. Assert `debug_bounds("activity-summary-1").is_some()`, `debug_bounds("tool-call-t1-standalone").is_none()`, `…t3…is_none()`. Клик: `thread_view.update_in(cx, |view, window, cx| view.toggle_activity_group(group, window, cx))`; assert `tool-call-t1-standalone` и `…t3…` `is_some()`.
  - `test_activity_group_shows_pending_permission`: tool call `t1` `completed` и `t2` с `authorization` (по образцу существующих permission-тестов в модуле); assert `tool-call-t2-standalone` `is_some()` при свёрнутой группе.
  - `test_activity_group_disabled_by_setting`: группа как в первом тесте; `AgentSettings::override_global` с `collapse_activity: false`, `run_until_parked`; assert `activity-summary-1` `is_none()`, `tool-call-t1-standalone` `is_some()`; вернуть `true` — assert снова `activity-summary-1` `is_some()`, `tool-call-t1-standalone` `is_none()`.
  - `test_activity_group_expands_on_scroll_to_entry`: группа как в первом тесте, `view.scroll_to_entry(2, cx)`; assert `entry_view_state.is_activity_group_expanded(1)` и `tool-call-t2-standalone` `is_some()`.
  - `test_activity_group_hides_last_entry_keeps_controls`: группа — последняя запись ленты, тред не генерирует; assert карточка скрыта, а селектор кнопок треда (тот, что использует `render_thread_controls`; найти его `debug_selector`/`id`) присутствует.
- [ ] **Step 2:** Run: `cargo test -p agent_ui test_activity_group_`. Expected: FAIL — `activity-summary-1` отсутствует.
- [ ] **Step 3:** Реализовать.
- [ ] **Step 4:** Run: `cargo test -p agent_ui`. Expected: PASS — новые и все существующие тесты (существующие идут с `collapse_activity` из `default.json`; если какие-то полагаются на видимость карточек — в их `override_global` выставить `collapse_activity: false`, не меняя ассертов).
- [ ] **Step 5: Commit** — `agent_ui: Collapse agent activity into a summary row`.

### Task 6: Три состояния карточки инструмента

**Files:**
- Modify: `crates/agent_ui/src/conversation_view/thread_view.rs` — `render_terminal_tool_call` (`:9238`, заголовок `:9300-9370`), `render_tool_call` (`:9605`, `Disclosure` `:9935-9985`, раздел raw input `:9720-9790`), `handle_view_event` (`:1585-1605`)
- Modify: `crates/agent_ui/src/ui/terminal_tool_header.rs` (иконка состояния вместо `is_expanded: bool`)
- Test: `crates/agent_ui/src/conversation_view.rs`

**Interfaces:**
- Consumes: `ToolCallExpansion`, `tool_call_expansion`, `cycle_tool_call_expansion` (Task 3); `first_line_of_label` (Task 2).
- Produces: `TerminalToolHeader::new(id, group, working_dir, expansion: ToolCallExpansion)`; debug selectors `tool-call-input-{id}`, `tool-call-output-{id}`.

Состояния (иконки: `Collapsed` — `ChevronRight`, `Input` — `ChevronDown`, `Output` — `ChevronUp`; тултипы `"Show Full Command"`/`"Show Input"`, `"Show Output"`, `"Collapse"`):
- терминал: `Collapsed` — `Label(first_line_of_label(..)).buffer_font(cx).truncate()` вместо `render_collapsible_command`; `Input` — `render_collapsible_command` (`tool-call-input-{id}`); `Output` — команда и вывод терминала (`tool-call-output-{id}`) — как сейчас при `is_expanded`;
- прочие инструменты: `Collapsed` — заголовок `render_tool_call_label` без содержимого; `Input` — блок raw input (раскрытый, без кнопки «View Raw Input»), для правок — список путей `locations`; `Output` — содержимое как сейчас при `is_open`.

Стрелка вызывает `cycle_tool_call_expansion` вместо `toggle_tool_call_expansion`; кнопка `ChevronUp` внизу развёрнутой карточки (`:9875`) — `collapse_tool_call`. `expand_terminal_card`/`expand_edit_card` в `handle_view_event` по-прежнему вызывают `expand_tool_call` (→ `Output`). Карточки субагентов не меняются.

- [ ] **Step 1: Write the failing test** `test_tool_call_three_state_disclosure`: `collapse_activity: false`, `expand_terminal_card: false`; один `execute` tool call `t1` с меткой из двух строк. Assert `tool-call-input-t1` и `tool-call-output-t1` `is_none()`; `cycle_tool_call_expansion` → `input` `is_some()`, `output` `is_none()`; ещё раз → `output` `is_some()`; ещё раз → оба `is_none()`.
- [ ] **Step 2:** Run: `cargo test -p agent_ui test_tool_call_three_state_disclosure`. Expected: FAIL — `tool-call-input-t1` отсутствует.
- [ ] **Step 3:** Реализовать.
- [ ] **Step 4:** Run: `cargo test -p agent_ui && ./script/clippy`. Expected: PASS, clippy без предупреждений.
- [ ] **Step 5: Commit** — `agent_ui: Add three-state tool call disclosure`.

### Task 7: Сборка и ручная проверка

- [ ] **Step 1:** Run: `script/bundle-mac` в фоне, лог в `/tmp/zed-build.log`. Expected: `EXIT=0`, `target/aarch64-apple-darwin/release/Zed-aarch64.dmg`.
- [ ] **Step 2:** Заменить `/Applications/Zed Dev.app` содержимым DMG (`hdiutil attach -nobrowse -readonly`, `rm -rf` старого бандла, `cp -R`, `hdiutil detach`). Запуск — пользователем, `zed-dev`.
- [ ] **Step 3:** Пользователь проверяет в треде Claude: вопрос, требующий нескольких shell-команд и MCP-вызова → одна строка `Called …, ran N shell commands`; клик разворачивает; стрелка карточки — три состояния; запрос разрешения виден в свёрнутой группе; `"collapse_activity": false` возвращает обычный вид.
