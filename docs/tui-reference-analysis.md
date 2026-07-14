# TUI reference analysis: Qwen Code, OpenCode, Pi

Дата: 2026-06-26

## Что скачано

Репозитории положены в `external/tui-reference/`:

- `external/tui-reference/qwen-code` - Qwen Code, официальный репозиторий `QwenLM/qwen-code`: https://github.com/QwenLM/qwen-code
- `external/tui-reference/opencode` - OpenCode, официальный репозиторий `anomalyco/opencode`, ссылка с `opencode.ai`: https://github.com/anomalyco/opencode
- `external/tui-reference/pi` - Pi, официальный репозиторий `earendil-works/pi`, ссылка с `pi.dev`: https://github.com/earendil-works/pi

## Краткий вывод

Для `harness` лучший путь - не копировать полностью ни Qwen, ни OpenCode, ни Pi. Нужно оставить Rust-стек `ratatui + crossterm`, но взять архитектурные идеи:

- из Pi: маленькая компонентная модель, явный focus/overlay stack, простые stateful selectors, TTY-vs-non-TTY разделение;
- из OpenCode: центральный registry команд и keybindings, command palette/help, provider setup как TUI dialog, события `tui.*`;
- из Qwen Code: полноценный setup-flow провайдера внутри интерфейса, виртуализация/статическая история, внимание к redraw-performance и resize.

Практически: `harness` должен быть ближе к Pi по простоте, к OpenCode по command/keymap слою, и к Qwen по provider/setup UX.

## Qwen Code

Стек:

- Node/TypeScript.
- `ink` + `react`.
- Много React context/provider слоев.
- Дополнительные UI-библиотеки: `ink-gradient`, `ink-link`, `ink-spinner`, `string-width`, `wrap-ansi`, `strip-ansi`, `highlight.js`.

Ключевые файлы:

- `packages/cli/src/ui/startInteractiveUI.tsx`
- `packages/cli/src/ui/AppContainer.tsx`
- `packages/cli/src/ui/App.tsx`
- `packages/cli/src/ui/components/shared/VirtualizedList.tsx`
- `packages/cli/src/ui/utils/terminalRedrawOptimizer.ts`
- `packages/cli/src/ui/auth/ProviderSetupSteps.tsx`
- `packages/cli/src/ui/auth/useProviderSetupFlow.ts`

Как устроен TUI:

- `startInteractiveUI()` готовит runtime, terminal title, remote input, dual output, cleanup, затем вызывает `ink.render(<AppWrapper />)`.
- `AppWrapper` собирает большое дерево контекстов: settings, keypress, session stats, vim mode, agent view, background tasks.
- `AppContainer` держит основную интерактивную логику: история, slash commands, model/auth/theme/settings commands, streaming, focus, resize, render mode.
- `App` почти только выбирает layout: screen reader layout или default layout.
- История рендерится не тупым списком. Есть `VirtualizedList`, измерение высот элементов, sticky-to-bottom, scrollbar, static rendering.
- Для производительности есть `terminalRedrawOptimizer`: он перехватывает `stdout.write` и схлопывает дорогие multiline erase sequences, которые генерирует Ink.
- Provider setup сделан как многошаговый flow: `protocol -> baseUrl -> apiKey -> models -> advancedConfig -> review`. Состояние wizard вынесено в `useProviderSetupFlow`, визуальные шаги - в `ProviderSetupSteps`.

Что взять для harness:

- Provider setup должен быть экраном/модалкой внутри TUI, а не до запуска интерфейса.
- Нужен отдельный state object для wizard, а не линейный prompt в CLI.
- Историю чата нельзя бесконечно перерисовывать целиком; нужен bounded/virtualized transcript.
- Resize/redraw должны иметь тесты и отдельную логику, иначе TUI быстро начнет мигать при streaming.

Что не брать:

- React/Ink-подход целиком. Для Rust это лишняя архитектурная тяжесть и плохое соответствие текущему стеку.
- Огромное количество context layers на раннем этапе. Для `harness` сейчас это YAGNI.

## OpenCode

Стек:

- TypeScript.
- `@opentui/core` + `@opentui/solid`.
- `@opentui/keymap`.
- `solid-js`.
- Плагинный TUI runtime.

Ключевые файлы:

- `packages/tui/src/app.tsx`
- `packages/tui/src/keymap.tsx`
- `packages/tui/src/config/keybind.ts`
- `packages/tui/src/component/prompt/index.tsx`
- `packages/tui/src/ui/dialog.tsx`
- `packages/tui/src/ui/dialog-select.tsx`
- `packages/tui/src/component/dialog-provider.tsx`
- `packages/tui/src/component/command-palette.tsx`
- `packages/tui/src/feature-plugins/system/which-key.tsx`
- `packages/opencode/src/server/routes/instance/httpapi/groups/tui.ts`

Как устроен TUI:

- `run()` создает `createCliRenderer()` с `targetFps: 60`, mouse support, Kitty keyboard, external output passthrough.
- После renderer создается OpenTUI keymap, затем через Solid `render()` собирается дерево providers.
- `App` создает TUI API adapters, plugin runtime, command registry, routes, dialogs, toast, theme, sync state.
- Provider setup открывается прямо внутри TUI:
  - если providers пустые, `dialog.replace(() => <DialogProviderList />)`;
  - команда `provider.connect` делает то же самое;
  - `DialogProvider` показывает `DialogSelect`, потом OAuth/API-key steps.
- Keybindings описаны централизованно в `config/keybind.ts`: имя, дефолтная клавиша, описание.
- Команды регистрируются отдельно от клавиш. Потом keymap dispatch вызывает команду по имени.
- Command palette строится из registered/reachable commands и показывает shortcuts.
- Dialog layer держит stack, modal mode, close on Escape/Ctrl+C, restore focus.
- Есть HTTP/API поверхность для управления TUI: append prompt, open help/sessions/themes/models, submit/clear prompt, execute command, show toast, select session, publish event.

Что взять для harness:

- Ввести `CommandRegistry`: имя команды, описание, aliases/slash name, keybinding, handler.
- Сделать `/provider add` и `provider.connect` как одну команду, которая открывает TUI dialog.
- Сделать `DialogStack`: help, provider setup, model select, command palette.
- Команды и keybindings держать в одном месте, а не размазывать по `match` внутри каждого экрана.
- В будущем можно добавить локальный control/event слой, но сейчас это не первый приоритет.

Что не брать:

- OpenTUI/Solid runtime: это не Rust и не нужен при выбранном `ratatui + crossterm`.
- Plugin TUI API сейчас рано. Сначала нужен стабильный базовый интерфейс и provider wizard.

## Pi

Стек:

- TypeScript.
- Своя библиотека `@earendil-works/pi-tui`.
- Минимальные runtime зависимости: `get-east-asian-width`, `marked`.
- В coding-agent используется `@earendil-works/pi-tui` как отдельная TUI-библиотека.

Ключевые файлы:

- `packages/tui/src/tui.ts`
- `packages/tui/src/terminal.ts`
- `packages/tui/src/stdin-buffer.ts`
- `packages/tui/src/components/input.ts`
- `packages/tui/src/components/select-list.ts`
- `packages/coding-agent/src/main.ts`
- `packages/coding-agent/src/core/slash-commands.ts`
- `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
- `packages/coding-agent/src/modes/interactive/components/model-selector.ts`
- `packages/coding-agent/src/modes/interactive/components/oauth-selector.ts`
- `packages/coding-agent/src/modes/interactive/components/login-dialog.ts`
- `packages/coding-agent/src/modes/interactive/components/tool-execution.ts`

Как устроен TUI:

- Базовый интерфейс компонента очень маленький:
  - `render(width): string[]`
  - `handleInput?(data: string): void`
  - `invalidate(): void`
- `Container` просто агрегирует children и рендерит их строки.
- `TUI` держит:
  - `previousLines`;
  - focus;
  - input listeners;
  - overlay stack;
  - render throttling примерно до 60 fps через `MIN_RENDER_INTERVAL_MS = 16`;
  - differential rendering;
  - full redraw на width/height changes;
  - hardware cursor через marker;
  - Kitty keyboard, bracketed paste, Windows VT input.
- `ProcessTerminal` отвечает за raw mode, stdin/stdout, bracketed paste, Kitty protocol negotiation, resize, cursor, clear, progress.
- `main.ts` явно выбирает режим:
  - если `--rpc` -> rpc;
  - если `--json` -> json;
  - если `--print` или stdin/stdout не TTY -> print;
  - иначе -> interactive.
- `InteractiveMode` создает `new TUI(new ProcessTerminal())`, добавляет header/chat/status/editor/footer containers, ставит focus на editor и запускает `ui.start()`.
- Slash commands описаны списком `BUILTIN_SLASH_COMMANDS`.
- В `setupEditorSubmitHandler()` команды пока обрабатываются большим `if`-списком: `/settings`, `/model`, `/login`, `/logout`, `/resume`, `/quit` и т.д.
- Selectors вроде `ModelSelectorComponent` - stateful components с search input, filtered list, selected index и `requestRender()` после async load.
- Tool rendering отдельным компонентом: `ToolExecutionComponent` умеет partial args, execution started, result/error, custom renderer, images, expanded state.

Что взять для harness:

- Это самая полезная модель для Rust: маленькие stateful экраны и явный event loop.
- Сохранить TTY/non-TTY split: TUI только в реальном терминале, fallback для pipe/test.
- Сделать app state плоским и явным: screen, dialog, prompt, transcript, selected index, active provider/model.
- Для каждого экрана иметь `handle_event -> Action`, а не прямые side effects внутри render.
- Overlay/focus stack нужен уже на provider wizard/model select/help.
- Для transcript/tool calls нужен отдельный компонент, который умеет обновляться частично.

Что не брать:

- Собственный ANSI diff renderer. В Rust это уже дает Ratatui/Crossterm достаточно хорошо.
- Большой `if`-список для команд из `InteractiveMode`. Лучше сразу сделать registry как в OpenCode.

## Рекомендованная архитектура для harness

Оставить стек:

- `ratatui`
- `crossterm`
- без `tokio` на первом этапе

Предлагаемый модульный слой:

```text
src/tui/
  mod.rs
  app.rs            # AppState, Screen, Dialog, ProviderWizardState
  event.rs          # UiEvent -> Action
  commands.rs       # CommandRegistry, slash aliases, help labels
  keymap.rs         # KeyBinding -> CommandId / Action
  render.rs         # root layout
  widgets/
    prompt.rs
    transcript.rs
    select_list.rs
    dialog.rs
    status_bar.rs
    provider_wizard.rs
```

Главные типы:

```rust
enum Screen {
    Setup,
    Chat,
}

enum Dialog {
    ProviderWizard(ProviderWizardState),
    ModelSelect(ModelSelectState),
    Help,
    CommandPalette,
}

enum Action {
    None,
    Quit,
    SubmitPrompt(String),
    OpenDialog(Dialog),
    CloseDialog,
    RunCommand(CommandId),
    SaveProvider(ProviderDraft),
}
```

Сразу сделать:

1. `harness` всегда открывает нормальный TUI в TTY, даже без провайдера.
2. Если провайдера нет, первый экран показывает setup panel и предлагает `/provider add`, но не выходит в line prompt.
3. `/provider add` открывает TUI wizard:
   - provider select;
   - base URL;
   - API key;
   - model;
   - save.
4. `/providers`, `/help`, `/exit` проходят через `CommandRegistry`.
5. Non-TTY fallback оставить для тестов и pipe usage.
6. Добавить render tests через Ratatui `TestBackend`.

## Приоритеты

P0:

- Перенести provider add из line prompt в Ratatui wizard.
- Ввести `AppState`, `Dialog`, `Action`.
- Ввести command registry хотя бы для `/provider add`, `/providers`, `/help`, `/exit`.

P1:

- Prompt/editor widget с history.
- Transcript widget.
- Model selector.
- Help/command palette.
- Tests for key handling and rendered screens.

P2:

- Tool-call transcript view.
- Trace viewer/export controls.
- Redraw/resize tests.
- Optional local TUI event/control API.

## Итоговая рекомендация

Для `harness` целевой дизайн:

- визуально и по UX ближе к Claude/Codex/OpenCode;
- внутренне проще, ближе к Pi;
- с командным слоем как у OpenCode;
- с provider setup flow как у Qwen/OpenCode;
- без тяжелого async runtime и без своего renderer на текущем этапе.

Это соответствует DRY и YAGNI: мы не добавляем новый UI framework сверх Ratatui, не тащим plugin API раньше времени, но сразу отделяем state/actions/commands, чтобы TUI не превратился в один большой `match`.
