# Компактная лента агента: группы активности

## Цель

Между сообщением пользователя и ответом агента лента показывает одну серую
строку-сводку вместо всех карточек команд, вызовов инструментов, правок и
блоков Thinking. Образец — вывод Claude Code CLI:

```
❯ проверь сейчас за то время которое прошло с прошлой проверки
  Ran 3 shell commands
⏺ С 11:23 до 12:26 ошибок нет.
```

Клик по сводке разворачивает группу в обычный вид ленты. Отдельная карточка
инструмента внутри развёрнутой группы имеет три состояния: одна строка,
полный ввод, ввод и результат.

Поведение общее для всех агентов панели (внешние ACP-агенты и встроенный
агент Zed).

## Подход

Группировка делается при рендере. Строки `ListState` остаются один к одному
с `AgentThreadEntry`: синхронизация списка (`splice_focusable` на
`NewEntry`/`EntriesRemoved`), `scroll_to_entry`, поиск по треду и вложенные
субагенты не меняются. Первая запись группы рисует сводку, остальные записи
свёрнутой группы рисуют `Empty`.

## Группа активности

Группа — максимальная последовательность подряд идущих записей активности:

- `AgentThreadEntry::ToolCall` любого `ToolKind`, включая субагентов;
- `AgentThreadEntry::SessionMessage`, для которого `acp_subagent_card`
  возвращает карточку;
- `AgentThreadEntry::AssistantMessage` без видимых `Message`-чанков
  (только `Thought`).

Группу завершают `UserMessage`, `AssistantMessage` с видимым текстом,
`Elicitation`, `ContextCompaction` и `SessionMessage` без карточки
субагента.

`Thought`-чанки в начале текстового `AssistantMessage` учитываются в
предыдущей группе: в свёрнутом виде они скрыты, в развёрнутом показываются
как сейчас. Если предыдущей группы нет, они образуют группу из одних
Thinking, сводка которой стоит перед этим сообщением.

Запись в группе рисуется полностью, даже если группа свёрнута, когда она
ждёт решения пользователя: `ToolCallStatus::WaitingForConfirmation` или
есть `render_generic_tool_permissions`. Сводка при этом остаётся над ней.

Группа из одной записи тоже сворачивается в сводку.

## Сводка

Одна строка: иконка-стрелка, текст, шрифт `tool_name_font_size()`, цвет
`Color::Muted`, без рамки и фона. Наведение подсвечивает строку, клик
переключает группу.

Части текста идут в фиксированном порядке, через запятую, пустые части
пропускаются; первая буква строки заглавная, как в Claude Code CLI
(`Called mcp-builds, mcp-dns, mcp-search 3 times, called 1 tool,
ran 4 shell commands`):

1. MCP-вызовы: `called <server>, <server> N time(s)` — до трёх серверов в
   порядке первого появления, дальше `and K more`; N — число MCP-вызовов
   всего.
2. Прочие `Other` и `Fetch`-вызовы без сервера: `called N tool(s)`.
3. `Execute`: `ran N shell command(s)`.
4. `Read`: `read N file(s)`.
5. `Search`: `searched N time(s)`.
6. `Edit`, `Delete`, `Move`: `edited N file(s)` — уникальные пути из
   `locations`, без путей — число вызовов.
7. Субагенты: `ran N subagent(s)`.
8. `Think` и Thought-чанки: `thought N time(s)`.

MCP-сервер записи определяется по `ToolCall::meta`: сначала
`claudeCode.mcpServer.name`, затем префикс `mcp__<server>__` в
`claudeCode.toolName`, затем тот же префикс в `ToolCall::tool_name`.
Сервер не найден — запись считается в п. 2.

Записи `Canceled` без видимого содержимого не считаются (их и сейчас не
видно). `Failed`/`Rejected` считаются; при наличии таких в конце строки
добавляется ` · N failed` цветом `Color::Error`.

Пока тред генерирует и группа последняя в ленте, справа от текста —
текущий шаг: спиннер и первая строка метки последней записи группы со
статусом `Pending`/`InProgress`, обрезанная по ширине с `…`. Счётчики
включают и выполняющиеся записи.

## Три состояния карточки инструмента

`EntryViewState::expanded_tool_calls: HashSet<ToolCallId>` заменяется на
`tool_call_expansion: HashMap<ToolCallId, ToolCallExpansion>`:

```rust
enum ToolCallExpansion {
    Collapsed,
    Input,
    Output,
}
```

Отсутствие ключа — `Collapsed`. Стрелка в заголовке переключает по кругу
`Collapsed → Input → Output → Collapsed`.

- `Collapsed`: заголовок в одну строку — иконка и первая строка
  команды или метки, обрезанная с `…`.
- `Input`: для `Execute` — команда целиком (`render_collapsible_command`);
  для правок — список файлов; для остальных — `raw_input`.
- `Output`: `Input` и результат — вывод терминала, содержимое ответа, diff.

Существующие вызовы `expand_tool_call` (раскрытие при запросе разрешения,
`ConversationView::expand_tool_call`) ставят `Output`.
`is_tool_call_content_visible` возвращает истину для `Output` и для
записей, ждущих разрешения. Настройки `expand_terminal_card` и
`expand_edit_card` задают начальное состояние `Output` для терминальных и
правочных карточек, как и раньше задавали раскрытие.

Карточки субагентов сохраняют свою логику раскрытия.

## Настройка

`agent.collapse_activity: bool`, по умолчанию `true`:

- `crates/settings_content/src/agent.rs` — поле `collapse_activity:
  Option<bool>` с doc-комментарием;
- `crates/agent_settings/src/agent_settings.rs` — поле в `AgentSettings`;
- `assets/settings/default.json` — `"collapse_activity": true` рядом с
  `expand_terminal_card`;
- `crates/settings_ui/src/page_data.rs` — переключатель рядом с
  `expand_terminal_card`.

При `false` группы не строятся, лента выглядит как без этой функции;
трёхсостоянные карточки работают в обоих режимах.

## Состояние развёрнутых групп

`EntryViewState::expanded_activity_groups: HashSet<usize>` — индексы первых
записей развёрнутых групп, по образцу `expanded_compactions`. При удалении
записей индексы пересчитываются через `reindex_after_removal`, как
`expanded_compactions` и `expanded_thinking_blocks`. Если после
пересчёта индекс указывает не на первую запись группы, разворот этой
группы теряется — допустимо, удаление записей бывает только при откате
треда.

## Компоненты

- `crates/agent_ui/src/conversation_view/activity_group.rs` (новый):
  - чистые функции без GPUI: `GroupRole`, `ActivityGroup`,
    `activity_group_at`, `ActivityItem`, `ActivitySummary`,
    `mcp_server_name`, `first_line_of_label`;
  - функции поверх `AcpThread`: `group_role`, `activity_items`,
    `activity_summary`, `current_step_label`,
    `tool_call_has_visible_content`.
- `crates/agent_ui/src/entry_view_state.rs` — `ToolCallExpansion`,
  `tool_call_expansion`, `expanded_activity_groups` и методы
  `is_activity_group_expanded`, `toggle_activity_group`,
  `tool_call_expansion`, `cycle_tool_call_expansion`.
- `crates/agent_ui/src/conversation_view/thread_view.rs`:
  - `render_entry`: при `collapse_activity` вычисляет группу; первая
    запись свёрнутой группы — `render_activity_summary`, остальные —
    `Empty` (кроме ждущих разрешения);
  - `render_activity_summary` — новая функция;
  - `render_tool_call`, `render_terminal_tool_call` — три состояния.

## Производительность

`activity_group_range` идёт от записи назад и вперёд до границ — O(длина
группы) на запись, O(сумма квадратов длин групп) на кадр по видимым
строкам. `list` рендерит только видимые строки, а свёрнутые записи имеют
нулевую высоту, так что в кадре участвует одна-две группы. Кэш не нужен,
пока замеры не покажут обратное.

## Тесты

Юнит-тесты в `activity_group.rs`:

- границы: `[User, Tool, Tool, Text]`, `[User, Thought-only, Tool, Text]`,
  `[Tool, Elicitation, Tool]`, `[Text-with-leading-thought]` без
  предыдущей группы, одна запись, конец ленты без текста;
- текст: единственное и множественное число, порядок частей, группировка
  MCP по серверу, `and N more`, уникальные пути правок, ` · N failed`;
- отменённые записи без содержимого не считаются.

`thread_view` (GPUI-тесты, `TestAppContext`):

- при `collapse_activity: true` группа из трёх tool call'ов рендерит одну
  сводку и разворачивается по клику;
- запись `WaitingForConfirmation` видна в свёрнутой группе;
- цикл стрелки `Collapsed → Input → Output → Collapsed`;
- существующие тесты `agent_ui` проходят.

## Вне рамок

- Отдельная панель фоновых задач (`/tasks`).
- Изменения в карточке субагента и её полноэкранном режиме.
- Сохранение состояния разворота между перезапусками.
