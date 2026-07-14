# Аудит улучшений harness-cli — июль 2026

Дата: 2026-07-02. Метод: многоагентный аудит (6 направлений: толерантность тулов,
кросс-платформенность, производительность/RAM ядра, агентный цикл, TUI, качество кода),
каждая находка адверсарно верифицирована отдельным агентом по реальному коду, затем
критик полноты добрал 4 пропущенных области (SSE-обрывы, config, секреты, тестовые
пробелы). Из 79 находок 2 опровергнуты, 76 подтверждены (из них 20 — с уточнениями
к рекомендации). Дубликаты между направлениями ниже слиты; строки указаны на момент аудита.

Приоритеты, по которым выставлена важность:

1. Скорость, RAM (меньше — лучше), качество tool_calls, отсутствие багов и зацикливаний модели.
2. Тулы корректно работают на Windows/Linux/macOS, без повторных вызовов, лояльны к ошибкам
   LLM (null/пустое поле чинится кодом, а не отдаётся ошибкой).

---

## Топ-10 по влиянию

| # | Находка | Где | Важность |
|---|---------|-----|----------|
| 1 | Таймаут shell не убивает дерево процессов; `join` ридеров блокируется навсегда — агент виснет | `src/tools/shell.rs:59` | critical |
| 2 | Паника tool-воркера вешает `execute_batch` навсегда (дефолт — без batch-таймаута) | `src/runtime.rs:1092` | critical |
| 3 | Общий 60-сек. ureq-дедлайн убивает любой стрим длиннее таймаута с потерей всего накопленного | `src/chat_client.rs:197` | critical |
| 4 | `max_tool_rounds` убивает ран ошибкой — модели не дают финального хода без тулов | `src/agent.rs:168` | high |
| 5 | Нет детекции зацикливания (идентичные повторные tool calls) и нет ретрая при 429/5xx | `src/agent.rs:96`, `src/chat_client.rs:143` | high |
| 6 | `file.replace` падает `TextNotFound` на CRLF-файлах — главный источник петель на Windows | `src/tools/files.rs:307` | high |
| 7 | EOF без `[DONE]` / `finish_reason: length` → «тихий успех» с усечёнными tool-аргументами, которые исполняются | `src/chat_client.rs:1237`, `:527` | high |
| 8 | Новый ureq-агент на каждый запрос: TLS-хендшейк на каждом раунде цикла | `src/chat_client.rs:131` | high |
| 9 | UI-поток блокируется на весь прогон: Ctrl+C/Esc не работают, зациклившуюся модель нельзя прервать | `src/repl.rs:450` | high |
| 10 | macOS фактически не поддержан: тесты не компилируются, clipboard и RSS-диагностика молча мертвы | `tests/*`, `src/clipboard.rs:247`, `src/diagnostics.rs:132` | high |

---

## 1. Зависания и потеря работы

### 1.1. Таймаут shell не убивает дерево процессов; join ридеров вечный — `src/tools/shell.rs:59` [critical/large]

По таймауту убивается только прямой потомок (powershell/bash), не его дети. Внуки
наследуют write-handle пайпа, ридеры не получают EOF, а `join_pipe_reader`
(безлимитный `JoinHandle::join`) блокируется до выхода внука. Для `ping -t`,
dev-сервера, `cargo watch` — навсегда: tool call не возвращается, агент завис,
осиротевший процесс ест CPU.

**Сделать:**
- Убивать дерево: Windows — Job Object через рукописный FFI
  (`CreateJobObjectW`/`AssignProcessToJobObject`/`TerminateJobObject`, в стиле FFI из
  `diagnostics.rs`) или fallback `taskkill /T /F /PID`; Unix —
  `CommandExt::process_group(0)` при spawn и `kill(-pgid, SIGKILL)` (однострочный
  `unsafe extern "C"`, libc уже слинкован — новых зависимостей нет).
- Защита в глубину: заменить блокирующий `join` на ожидание через mpsc с
  `recv_timeout(~2s)`; не дождались — вернуть частичный вывод с пометкой.

### 1.2. Паника tool-воркера вешает `execute_batch` навсегда — `src/runtime.rs:1092` [critical/small]

`spawn_tool_call` не ловит паники. Если `executor.execute` паникует, поток умирает без
`send`, но оригинальный `tx` жив в `execute_batch` — `rx.recv()` блокируется навечно
(ветка `panic!("tool worker terminated…")` недостижима). Дефолт `tool_batch_timeout: None`
(`agent.rs:39`) — значит виснет весь агент и REPL.

**Сделать:** обернуть `executor.execute` в `std::panic::catch_unwind(AssertUnwindSafe(...))`;
при панике слать `ToolBatchResult{ok:false, error:"tool panicked: …", hint:"internal tool
failure; try a different tool or arguments"}`. Тест — в `tests/tool_scheduler.rs` (там уже
есть паттерн мок-executor'а `ObservedExecutor`).

### 1.3. Общий ureq-таймаут режет длинные стримы — `src/chat_client.rs:197` [critical/small]

`.timeout(self.timeout)` в ureq 2.x — общий дедлайн на весь вызов, включая чтение тела.
Дефолт 60 с; чат-TUI всегда стримит (`repl.rs:439`), значит любая генерация
thinking-модели дольше 60 с гарантированно падает с потерей всего накопленного.

**Сделать:** для стриминговых путей заменить на `.timeout_connect(min(timeout,10s))` +
`.timeout_read(self.timeout)` — per-read таймаут работает как stall-детектор между
SSE-чанками, не ограничивая общую длительность. Тест: mock шлёт 3 события с паузами
1.5 с при клиентском таймауте 2 с → сейчас `Err(Io)`, после фикса `Ok`.

### 1.4. UI-поток заблокирован на весь прогон агента — `src/repl.rs:450` [high/large]

`run_with_events` выполняется синхронно внутри event-loop TUI: Ctrl+C/Esc не читаются,
скролл не работает, спиннер заморожен (тикает только на событиях агента), зациклившуюся
модель можно прервать только убийством процесса.

**Сделать:** вынести прогон на `std::thread` (клонировать `AgentRunner`, события через
`mpsc::Sender<AgentEvent>`); UI-loop перевести на `event::poll(50ms)` + `rx.try_recv()`:
на poll-timeout тикать спиннер, на входные события обрабатывать Esc/Ctrl+C/скролл.
Отмена — `AgentRunner::with_cancel_flag(Arc<AtomicBool>)`, проверяемый в начале каждой
итерации цикла и в стрим-коллбеке. Всё в рамках std::thread/mpsc, без tokio. Бонус:
коалесинг кадров (п. 6.2) получается бесплатно — за итерацию дренируются все события
и рисуется один кадр.

---

## 2. Агентный цикл: анти-зацикливание и устойчивость

### 2.1. `max_tool_rounds` убивает ран вместо финального хода — `src/agent.rs:168` [high/medium]

При достижении лимита возвращается `Err(MaxToolRoundsExceeded)`: вся работа (выполненные
тулы, контекст) выбрасывается. Дефолт 4 раунда — реальные задачи легко упираются.

**Сделать:** (1) добавить в `messages` pending tool_calls (сейчас push на строке 187
происходит ПОСЛЕ проверки лимита на 163) и на каждый — синтетический результат
`{ok:false, error:"tool budget exhausted", hint:"answer now from what you already know;
no more tool calls will be executed"}`; (2) ОДИН финальный запрос с тем же envelope
(tools оставить — `cache_prefix_key` не включает messages, префикс стабилен);
(3) если модель снова вернула tool_calls — тогда уже `MaxToolRoundsExceeded`.

### 2.2. Нет детекции повторных идентичных tool calls — `src/agent.rs:96` [high/medium]

Модель, повторяющая тот же вызов с теми же аргументами (типичный паттерн слабых моделей),
молча исполняется каждый раз до упора в лимит раундов.

**Сделать:** хранить сигнатуры вызовов предыдущего раунда (имя + `serde_json::Value`
аргументов, сравнение через `==`, blake3 не нужен). Повтор — ВСЕГДА исполнять
(кэшировать результат нельзя: side-effect в том же батче мог изменить файл — модель
получила бы stale data), но проставлять `hint: "identical call already executed in
round N — reuse the result you already have"`. На 3-м подряд идентичном раунде —
завершать ран финальным ходом по механике п. 2.1.

### 2.3. Текст, пришедший вместе с tool_calls, теряется целиком — `src/agent.rs:187` [high/small]

Штатное поведение Anthropic/DeepSeek/GLM — текст-план перед tool_use. Сейчас
`response.content` в ветке с tool_calls нигде не читается: не попадает ни в историю
(`assistant_tool_calls` с пустым content), ни в события, ни в trace. Модель на следующем
раунде не видит собственного плана — переформулирует его заново (лишние токены, дрейф).

**Сделать:** `ChatMessage::assistant_tool_calls_with_content(content, tool_calls)` в
`request.rs`; в `openai_message_body` ставить content вместо безусловного `Null`;
в `anthropic_message_body` — text-блок перед tool_use; в `openai_responses_input_items` —
message-item перед function_call. В `agent.rs` эмитить текст событием и писать в trace.

### 2.4. Нет ретрая при 429/5xx; тело HTTP-ошибки выбрасывается — `src/chat_client.rs:143`, `:448` [high/small]

Один сетевой сбой посреди многораундового рана уничтожает всё. Тело ошибки провайдера
(«context length exceeded» и т.п.) не читается — пользователь видит голый `status code 400`.

**Сделать:**
- Retry-цикл в `ProviderChatClient::send/stream_chat`: до 3 попыток при
  `Status(429|500..=599)` и `Transport`, backoff 1s/2s/4s через `thread::sleep`,
  уважать `Retry-After` (ограничив сверху ~30 с). НЕ ретраить `ChatClientError::Io`
  (mid-stream обрыв повторно эмитил бы уже показанные дельты). Задержки — инжектируемые,
  чтобы тесты не спали.
- В `impl From<ureq::Error> for ChatClientError` при `Error::Status` читать тело
  ограниченно (`into_reader().take(2048)`) в новый вариант `Api{status, body}` — все
  call-sites с `?` починятся автоматически. То же в `model_client.rs:29`.

### 2.5. `finish_reason`/`stop_reason` не читается — обрезанные tool-аргументы исполняются — `src/chat_client.rs:527` [high/medium]

Grep по `finish_reason|stop_reason` в src/ — 0 совпадений. При `finish_reason:"length"`
обрезанный JSON аргументов уходит через lossy-парсер на исполнение (риск: `file.write`
с половиной контента), а усечённый текст выдаётся как полный ответ.

**Сделать:** `#[serde(default)] finish_reason` в `OpenAiChoice`/`OpenAiStreamChoice`,
`stop_reason` в `AnthropicMessagesResponse`, для Responses-форматов — верхнеуровневый
`status == "incomplete"` + `incomplete_details.reason`. Пробросить `truncated: bool` в
`ChatResponse`; в `agent.rs` при `truncated` + tool_calls — НЕ исполнять, а вернуть
синтетический результат с hint «output truncated, re-issue the call».

### 2.6. Anthropic: `max_tokens` захардкожен в 4096 — `src/chat_client.rs:905` [medium/small]

**Сделать:** константа `ANTHROPIC_DEFAULT_MAX_TOKENS = 8192` + переопределение
per-provider (поле вне `cache_prefix_key`); парсить `stop_reason == "max_tokens"` →
`truncated` (см. п. 2.5).

### 2.7. История REPL не передаётся агенту — `src/agent.rs:86` [high/medium]

Каждый Submit создаёт нового `AgentRunner` с единственным user-сообщением: «исправь его»
на втором ходе не имеет контекста — модель заново перечитывает файлы или отвечает невпопад.

**Сделать:** `AgentRunner::with_history(Vec<ChatMessage>)` (builder в стиле существующих
`with_*`); добавить `ChatMessage::assistant(content)` в `request.rs` (сейчас финальный
ответ ассистента нечем представить); в `repl.rs` (оба пути Submit) накапливать переписку
с капом по суммарному размеру. История идёт только в messages — `cache_prefix_key`
не затронут.

---

## 3. Толерантность тулов к ошибкам LLM

### 3.1. `file.replace`: CRLF/whitespace ломают поиск — `src/tools/files.rs:307` [high/medium]

`replace_limited` ищет `old_text` побайтово. На Windows файлы в CRLF, модель передаёт
`\n` → `TextNotFound` → петля ретраев или затирание файла через `file.write`.
Основной edit-инструмент, основная платформа.

**Сделать:** при 0 совпадений — ступенчатый fallback: (1) нормализовать CRLF→LF для
поиска (для `new_text` порядок обязателен: сначала `\r\n`→`\n`, потом при CRLF-файле
развернуть обратно — иначе `\r\r\n`); (2) сравнение с trim хвостовых пробелов построчно.
Запись — с исходным стилем перевода строк файла. Успех через fallback → `repaired=true`
со специфичным hint («old_text matched only after newline normalization; file uses CRLF»),
generic `repair_hint` тут не объяснит причину.

### 3.2. Абсолютный путь внутри workspace отвергается — `src/tools/files.rs:511` [high/small]

`normalize_relative_path` возвращает `OutsideWorkspace` для ЛЮБОГО абсолютного пути, даже
лежащего внутри workspace. Модели, обученные на Claude Code, передают абсолютные пути
постоянно. Отягчающее: harness САМ отдаёт модели абсолютный путь (`"wrote {path}"` в
`runtime.rs:244` при workspace = `current_dir`) — модель, переиспользующая путь из
результата тула, получает отказ с дезинформирующим текстом «outside workspace».

**Сделать:** для абсолютного пути — канонизировать root, `strip_prefix`, при успехе
продолжить с относительным остатком (`repaired=true`). Windows-нюанс: `canonicalize()`
даёт `\\?\`-verbatim — нормализовать префикс перед сравнением; для write цель может не
существовать — релятивизировать лексически, границу по-прежнему проверяет canonicalize
родителя. Плюс trim кавычек/пробелов вокруг пути (паттерн уже есть в
`clean_attachment_reference`). И привести вывод `file.write` к относительному пути.

### 3.3. `null`/число/массив в обязательном поле — hard error — `src/runtime.rs:950` [high/small]

`repair_tool_arguments` вычищает null-ключи (опциональные поля чинятся — уже хорошо),
но для обязательных полей после вычистки остаётся голый `MissingArgument`. `string_arg`
отвергает `Number`/`Bool` (`{"path": 123}` → `InvalidArgument`), массив
(`{"command": ["git","status"]}` — Codex-модели обучены argv-массиву) и объект
(`{"content": {...}}` для JSON-файла).

**Сделать:** в `string_arg` — коэрция `Number`/`Bool` → `to_string()`; `""` трактовать
как отсутствующий ключ и продолжать перебор алиасов; массив строк — только для
`command` (с квотированием элементов с пробелами, иначе `join(" ")` исказит
`["git","commit","-m","two words"]`); объект/массив для `content` →
`to_string_pretty`. `content: null` в `file.write`/`file.append` → `""` с
`repaired=true`. Для `path`/`old_text` null оставить ошибкой, но с hint (п. 3.5).
Возвращать признак coerced → `repaired=true`.

### 3.4. Обёртки `{"arguments": {...}}` и дважды-JSON не разворачиваются — `src/runtime.rs:837` [medium/small]

**Сделать:** (1) объект с единственным ключом из `{arguments, input, params, parameters,
args}` со значением-объектом — развернуть (`repaired=true`); (2) в начале
`raw_tool_arguments` попробовать `serde_json::from_str` — дважды-кодированный JSON
сейчас уходит в key:value-парсер и порождает мусор.

### 3.5. Error-результаты никогда не несут hint — `src/runtime.rs:73-82` [medium/small]

Памятка строится только в Ok-ветке `from_execution`, а больше всего она нужна именно
провалившимся вызовам. «unknown tool: bash» не перечисляет доступные тулы — модель
повторяет несуществующее имя до упора.

**Сделать:** в Err-ветке — hint по типу ошибки: `MissingArgument`/`InvalidArgument` →
«Call '<wire>' with arguments like <canonical_tool_usage>» (canonical через
`ToolResolution::from_name`, wire = точки→подчёркивания); `UnknownTool` → список
wire-имён из `tool_specs()` (не хардкодить второй раз). Для File/Shell-ошибок
(аргументы корректны по форме) hint не добавлять — шум.

### 3.6. Нет алиасов bash/read/write/edit/… — `src/runtime.rs:802` [high/small]

**Сделать:** добавить в `ToolResolution::from_name`:
`bash|sh|zsh|terminal|exec|execute|execute_command|run_shell_command|run_terminal_cmd`
→ shell.exec; `read|cat|view|open_file` → file.read; `write|create_file|save_file` →
file.write; `edit|str_replace` → file.replace; `glob|find_files` → file.list.
НЕ алиасить вслепую `apply_patch` (аргумент — patch-документ) и `str_replace_editor`
(мультиплексируется полем `command`) — для них корректен только UnknownTool + hint.

### 3.7. `{"timeout": 60}` трактуется как 60 мс — `src/runtime.rs:442` [medium/small]

**Сделать:** `timeout_ms` — всегда мс (тест `tool_runtime.rs:611` не меняется);
`timeout_seconds|timeout_sec|timeout_s` — всегда секунды; голый `timeout` — эвристика
(≤600 → секунды, иначе мс), `repaired=true` + `effective_timeout_ms` в metadata.
Описание ToolSpec не трогать (входит в `cache_prefix_key`).

### 3.8. Таймаут shell выбрасывает частичный вывод — `src/runtime.rs:445` [medium/small]

`ShellError::TimedOut` несёт захваченные stdout/stderr, но модель видит только строку
«timed out; captured stdout=40960 bytes» — и повторяет ту же долгую команду.

**Сделать:** в `execute_shell` матчить `TimedOut` ДО конверсии в `RuntimeError` и
возвращать `Ok(ToolCallResult{ok:false, content: output.stdout, metadata:
{timed_out:true, timeout_ms, stderr, …}})` — как уже сделано для ненулевого exit_code.
Обновить тест `runtime_shell_exec_accepts_per_call_timeout_ms` (сейчас ассертит Err).

### 3.9. Пустые/отсутствующие id tool calls — `src/chat_client.rs:544`, `:1284` [medium/small]

Отсутствующий `id` в OpenAI-chat валит десериализацию ВСЕГО ответа; в
Responses/стриме/Anthropic пустой id молча уходит в follow-up (провайдер вернёт 400,
несколько вызовов неразличимы).

**Сделать:** `#[serde(default)]` на id; общая `normalize_tool_call_ids(&mut Vec<ChatToolCall>)`
— пустые → `call_<index>`, дубли → суффикс `-2`; вызвать во всех ЧЕТЫРЁХ парсерах
(OpenAI chat, Responses, Anthropic, стрим).

### 3.10. Отменённые по batch-дедлайну тулы: нет предупреждения о двойном исполнении — `src/runtime.rs:1026` [low/small]

Detach-нутый поток доработает ПОСЛЕ того, как модели сказали «timed out» — повтор
side-effectful команды даёт двойное исполнение.

**Сделать:** различать spawned-but-unfinished («side effects may still land; verify
workspace state before re-running») и never-started («safe to retry») в hint.

---

## 4. Кросс-платформенность (Windows / Linux / macOS)

### 4.1. Тесты не компилируются на macOS — `tests/shell_tool.rs:89-137`, `tests/tool_runtime.rs:695-723` [medium/small]

Хелперы `native_echo_command` и пр. объявлены только под `cfg(windows)` /
`cfg(target_os = "linux")` — на macOS `cargo test` падает на компиляции, вся
TDD-верификация невозможна, остальные macOS-баги невидимы.

**Сделать:** заменить на `cfg(unix)` (команды POSIX-совместимы); в
`tests/platform_prompt.rs` добавить macOS-ветку (сейчас тест проходит вакуумно и ломает
`clippy -D warnings` неиспользуемым импортом). Добавить macos-runner в CI, когда он появится.

### 4.2. Clipboard на macOS молча мёртв — `src/clipboard.rs:245-247` [high/medium]

Unix-ветка перебирает только `wl-paste`/`xclip`; `NotFound` глотается → «буфер пуст»
при любом содержимом.

**Сделать:** ветка `#[cfg(target_os = "macos")]`: текст — `pbpaste`; PNG — osascript
с `try`-блоком и `close access` (`write (the clipboard as «class PNGf») to f`), код
ошибки -1700 (не-PNG буфер) маппить в `Ok(None)`, по аналогии с `exit 3` Windows-ветки.
Linux-ветку сузить до `all(unix, not(target_os = "macos"))`.

### 4.3. RSS-диагностика на macOS всегда «ок» — `src/diagnostics.rs:132` [medium/medium]

`/proc/self/status` на macOS нет → `rss_bytes: null`, `within_limits: true` при любом
потреблении — проверка лимита RAM молча деградирует.

**Сделать:** `#[cfg(target_os = "macos")]` FFI к `proc_pidinfo(PROC_PIDTASKINFO)` →
`pti_resident_size` (точный аналог существующего FFI `GetProcessMemoryInfo` в этом же
файле, зависимостей ноль). Procfs-ветку — под `all(unix, not(target_os = "macos")))`.
Существующий `tests/cli.rs:183` покроет фикс.

### 4.4. macOS-шелл — `sh -lc` (bash 3.2 в POSIX-режиме) — `src/platform.rs:32` [medium/small]

Башизмы (`[[ ]]`, `pipefail`, массивы) падают; несогласованность с Linux (`bash`).

**Сделать:** заменить `sh` на `bash` (есть на всех macOS), оставив `-lc` на macOS
осознанно: там `-l` — дешёвый `path_helper`, дающий `/opt/homebrew/bin` при запуске вне
login-контекста. Компромисс: `-l` пере-сорсит профили (латентность, echo в stdout,
ре-экспорт секретов из `~/.profile` — см. п. 7.4); если изоляция секретов станет
приоритетом, убирать `-l` на обеих Unix-ОС и документировать требование к PATH.
Тест в `tests/platform_prompt.rs`.

### 4.5. PowerShell-вывод в OEM-кодировке — U+FFFD-мусор — `src/tools/shell.rs:198` [high/small]

Проверено на живой системе: `Write-Output привет` → CP866-байты → шесть U+FFFD. Любой
русский текст в выводе git/cargo/ошибках PowerShell нечитаем для модели → повторные
вызовы.

**Сделать:** поле `command_prelude` в `ShellProfile` (Windows:
`[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $OutputEncoding=…; `; Unix —
пусто) и `format!("{prelude}{command}")` в `ShellTool::run`. Проверено: с прелюдией
приходит валидный UTF-8. Тест под `cfg(windows)`.

### 4.6. Модель не знает, какой шелл за `shell.exec` — `src/runtime.rs:176` [high/small]

`echo a && echo b` в PowerShell 5.1 — ParserError (проверено). Модели по умолчанию
пишут bash.

**Сделать:** платформозависимое описание тула, выведенное из
`ShellProfile::native().program()` (не хардкод): Windows — «Windows PowerShell 5.1:
separate commands with ';', no bash syntax»; unix — фактический шелл. Строка стабильна
per-install → `cache_prefix_key` стабилен. Тест: `tool_specs()` содержит `program()`.

### 4.7. `repl.rs` не фильтрует `KeyEventKind::Release` — двойной ввод на Windows — `src/repl.rs:581` [high/small]

`tui.rs` фильтрует везде, `repl.rs` — нигде: в line-mode дублируются символы, в
коалесинге Release-события искажают paste-детекцию.

**Сделать:** отбрасывать не-Press в `is_text_key`, `key_to_repl_event` и ветках
`Event::Key` внутри `coalesce_chat_events` (консистентно с tui.rs). Тест на пары
Press/Release.

### 4.8. Windows-путь на Unix молча создаёт каталог `C:/…` в workspace — `src/tools/files.rs:509` [low/small]

**Сделать:** в `normalize_relative_path` распознавать `[A-Za-z]:/` и `[A-Za-z]:` в
начале → `OutsideWorkspace`; ведущий `~/` — отдельная понятная ошибка либо обрезка
с `repaired` на уровне executor'ов (у `normalize_relative_path` нет канала repaired).

### 4.9. `file.write` пишет сквозь symlink наружу — `src/tools/files.rs:138` [medium/small]

Граница проверяется только для родителя; symlink внутри workspace на файл снаружи
перезаписывается. Читающие тулы канонизируют target — асимметрия именно в записи.

**Сделать:** до `fs::read` проверять `fs::symlink_metadata(&target)`: symlink →
canonicalize + `starts_with(root)`, висячая ссылка → `OutsideWorkspace`. Тест под
`cfg(unix)`.

---

## 5. Скорость и RAM ядра

### 5.1. Новый ureq-агент на каждый запрос — `src/chat_client.rs:131,165,197,252,290` [high/small]

Пул соединений живёт в агенте → TLS-хендшейк на каждом раунде: ~0.5–1.5 с накладных
на run из 4 раундов, на каждом Submit REPL.

**Сделать:** один `ureq::Agent` в `ProviderChatClient::new(timeout)`, клонировать в
адаптеры (Agent дёшево клонируется, пул общий). Обязательное дополнение для стрим-пути:
в `read_openai_stream*` после `break` на `[DONE]` дочитать reader до EOF — иначе
соединение с недочитанным телом не вернётся в пул. Тест: mock читает два запроса из
одного TCP-стрима.

### 5.2. История клонируется целиком каждый раунд — `src/agent.rs:101` [medium/medium]

`messages.clone()` в envelope + копия в JSON-дерево + `body.to_string()` — до 4
одновременных копий истории на пике; `tool_specs()` и системный промпт аллоцируются
заново каждый раунд. За run из N раундов — O(N²) по объёму истории.

**Сделать:** простой вариант без лайфтаймов — `std::mem::take(&mut messages)` в envelope
перед send и возврат Vec обратно после (envelope живёт только на время запроса); либо
`Cow<'a, [ChatMessage]>`. `tool_specs()`/промпт — вынести из цикла. Хэши не меняются.

### 5.3. Каждый tool result — в 3 долгоживущих копиях; trace не ограничен — `src/agent.rs:209-216` [medium/medium]

messages (полный JSON) + trace + `tool_results` + клоны на события; `AgentRunResult`
содержит и `tool_results`, и trace с теми же данными.

**Сделать:** `tool_results` вычислять из trace (по образцу `tool_errors()`); формат
JSON-вывода `agent run` сохранить ручным `impl Serialize` либо осознанно поменять
(обновив `tests/agent_loop.rs:93,168-171` и `tests/cli.rs:1084`). В trace хранить
усечённый content (8–16 КБ + `content_len`) — полный текст модель всё равно получает
через messages.

### 5.4. stdout уходит модели дважды — `src/runtime.rs:453` [medium/small]

`content` = stdout, и тот же stdout внутри сериализованного `metadata` — до 64 КБ × 2
в каждом сообщении истории, переклонируется каждый раунд.

**Сделать:** строить metadata вручную без поля stdout (`exit_code`, `stderr`,
`*_truncated`, `max_output_bytes`).

### 5.5. Мелкое (низкий приоритет, попутно)

- `ToolScheduler` клонирует полные `ToolCall` дважды ради id/name (`runtime.rs:638`,
  `:1093`) → хранить `(id, name)`, stubs строить только при `batch_timeout.is_some()`.
- `cache_prefix_key` пересчитывается на каждый запрос даже для политик без заголовка
  (`chat_client.rs:136`) → проверять политику до вычисления / кэшировать на run.
- `parse_tool_arguments_lossy` жадно строит кандидатов до первой попытки парсинга
  (`chat_client.rs:1053`): fast path `serde_json::from_str` первым; в
  `remove_trailing_commas` — `char_indices` + `Cow<str>` вместо `Vec<char>`
  (для 500 КБ file.write — мегабайты мусорных аллокаций).
- SSE-парсер аллоцирует Vec по `index` из ответа провайдера без границы
  (`chat_client.rs:1262`): `MAX_STREAM_TOOL_SLOTS = 128`, фрагменты выше — молча
  игнорировать (НЕ класть в последний слот — испортит чужие аргументы).

---

## 6. Устойчивость SSE-стрима

### 6.1. EOF без `[DONE]` — тихий «успех» с усечёнными аргументами — `src/chat_client.rs:1237` [high/small]

Обрыв соединения → `Ok(ChatResponse)` с тем, что накопилось; обрезанный JSON аргументов
уходит в lossy-парсер → `_raw_arguments` → repair извлекает частичный content →
`file.write` молча пишет половину файла.

**Сделать:** флаг `saw_done` (и/или `finish_reason` как альтернативный терминальный
сигнал); EOF без маркера → `ChatClientError::StreamTruncated{partial: Box<ChatResponse>}`.
Плюс `#[serde(default)] error: Option<Value>` в чанке — при наличии сразу ошибка с
текстом провайдера.

### 6.2. Не-JSON SSE-событие валит весь стрим — `src/chat_client.rs:1241` [medium/small]

`data: ping` от прокси → `?` на `from_str` → потеря 500 уже полученных токенов.

**Сделать:** пропускать нераспарсенные события (счётчик; >N подряд — ошибка).

### 6.3. Тестовый пробел: SSE покрыт только happy-path — `tests/chat_client.rs:818` [low/small]

**Сделать:** варианты `respond_sse_no_done` / `respond_sse_and_drop`; тесты: EOF без
`[DONE]` → `StreamTruncated`; мусорное событие → Ok; `data:{"error":…}` → Err;
медленный стрим с `timeout_read` → Ok. Писать вместе с фиксами (TDD).

---

## 7. Конфигурация и секреты

### 7.1. Битый `providers.json` необратимо брикает CLI — `src/config.rs:30` [high/small]

`save_provider` первой строкой делает `load()?` — поверх битого файла нельзя даже
пересохранить; `resolve_default_launch` падает ДО TUI — онбординг недостижим.

**Сделать:** (1) в `save_provider` при `ConfigError::Json` — переименовать битый файл в
`providers.json.corrupt-<ts>` и продолжить с дефолтом (с предупреждением); (2) в
`resolve_default_launch` при `Json` — маршрут в `DefaultLaunch::Setup` с warning;
(3) путь файла в Display ошибки (`Json{path, source}`, правки в двух местах —
`config.rs:23,39` — и в `Error::source`).

### 7.2. Запись конфига не атомарна — `src/config.rs:40` [medium/small]

**Сделать:** tmp-файл (с pid в имени) в той же директории + `sync_all()` + `fs::rename`
(атомарен на Unix и Windows). Не решает lost-update двух процессов — зафиксировать
отдельно.

### 7.3. Незнакомый enum-вариант валит загрузку всего конфига — `src/providers.rs:189` [medium/medium]

Откат версии harness → старый бинарник отказывается грузить ВСЕХ провайдеров.

**Сделать:** двухэтапная загрузка (providers как `BTreeMap<String, Value>`, per-entry
`from_value`, невалидные — в `unrecognized: BTreeMap<String, Value>` с `#[serde(skip)]`);
в `save_provider` мержить `unrecognized` обратно, чтобы не уничтожать чужие записи.

### 7.4. Секреты: env-наследование, plaintext 0644, git — [medium]

- `shell.exec` наследует всё окружение, включая все `key_env`-ключи (`shell.rs:39`):
  `with_env_removals` — все `BuiltinProvider::profile().key_env` + key_env всех
  провайдеров из конфига + активного. `HARNESS_CONFIG` не удалять (путь, не секрет).
  Помнить: `-l`-шелл пере-сорсит профили и может вернуть переменные (п. 4.4).
- `providers.json` c ключом создаётся 0644 на Unix (`config.rs:39`): открывать через
  `OpenOptionsExt::mode(0o600)` + `set_permissions` для уже существующих файлов.
- `.harness/` (ключ + скриншоты буфера) попадает под `git add .`: генерировать
  `.harness/.gitignore` с `*` в `AttachmentStore::save` и в `save_provider` (когда
  родитель — `.harness`); добавить `/.harness/` в `.gitignore` репозитория; в README
  продвигать `--key-env` первым.

---

## 8. TUI

### 8.1. O(вся история) пересборка + повторный markdown-парсинг на каждую SSE-дельту — `src/tui.rs:1370` [high/medium]

`render_chat_body` пересобирает styled-строки всех записей на каждый кадр, кадр — на
каждую дельту. Стримящийся ответ перепарсивается целиком: O(n²) по ответу.

**Сделать:** кэш `Vec<Vec<Line<'static>>>` по-записно; инвалидировать только мутируемую
запись (последнюю при дельтах; tool-карточку по id — `apply_tool_result` уже находит её;
учесть push «memo»-записи). Ручные `Clone`/`PartialEq` для `ChatTuiApp` без кэша.
`#[cfg(test)]`-хуки не сработают (тесты интеграционные) — тестировать через TestBackend.

### 8.2. Кадр на каждую дельту — `src/repl.rs:453` [medium/small]

**Сделать:** минимально — `last_draw: Instant`, рисовать не чаще ~33 мс (форсировать на
ToolResult); при выносе агента в поток (п. 1.4) — бесплатно.

### 8.3. Окно транскрипта в логических строках до wrap — новьё обрезается — `src/tui.rs:1380` [high/medium]

**Сделать:** считать окно в визуальных строках (`Line::width()` / inner_width), идти с
конца; помнить, что оценка `ceil(width/inner)` занижает при word-wrap — консервативный
запас или точный повтор алгоритма переноса; верхнюю строку компенсировать смещением.

### 8.4. Остальное по TUI

- Потолок скролла `len*4` не пускает к началу длинного ответа (`tui.rs:867`) — клампить
  по фактическому числу строк из кэша; добавить `ratatui::widgets::Scrollbar`.
- Каретка в chars: CJK/эмодзи уезжают; длинный ввод редактируется вслепую
  (`tui.rs:1424`) — колонка через `Line::width()`, видимость каретки через
  `Paragraph::scroll((row_off, col_off))`.
- Esc мгновенно убивает сессию (`tui.rs:918`) — первый Esc чистит input / прерывает
  прогон; выход — двойной Esc за ~1.5 с с подсказкой.
- Каждое событие агента сбрасывает scroll в 0 (`tui.rs:1208`) — sticky-bottom только
  если уже внизу; реально полезно после п. 1.4.
- Транскрипт/история не ограничены (`tui.rs:775`) — history через VecDeque cap 200
  (как ReplSession), transcript cap ~2000 с «older output trimmed»; Thinking НЕ усекать
  «потому что есть в trace» — trace выбрасывается (`repl.rs:462`), transcript —
  единственная копия.
- `/model` без аргументов — overlay-список провайдеров/моделей (инфраструктура
  `list_models`/`ModelDiscovery` уже есть, подключена только в CLI).
- SSE для Anthropic (`content_block_delta`) по образцу `read_openai_stream_full`;
  Responses — вторым шагом (`chat_client.rs:94`).
- Vision-payload: картинка уходит путём, не байтами (`repl.rs:601`) — это
  задокументированный gap из goal.md; честная быстрая правка — system-line «model can
  only reference the path», полная — контент-части в `ChatMessage` (text +
  image/base64) и сериализация в адаптерах.
- `tui.rs` 2277 строк: разбить на `tui/{mod,setup,chat,render,markdown,commands}.rs`;
  удалить мёртвый `SetupTuiApp`+`run_setup_tui` (ноль вызовов в проде); таблиц команд
  фактически ЧЕТЫРЕ и они уже разошлись — единый `CommandRegistry`.

---

## 9. Качество кода и схемы тулов

- **Пустые schema у тулов** (`chat_client.rs:751,808,942`): `{"type":"object",
  additionalProperties:true}` без properties/required — модель угадывает имена
  аргументов, отсюда часть repair-раундов. Добавить статические `parameters` в
  `ToolSpec` (properties + required, `additionalProperties:true` оставить для
  прощающих алиасов). Схема статична — префикс меняется один раз при деплое, допустимо.
- **cli.rs: девять одинаковых ручных циклов парсинга флагов (~550 строк)**
  (`cli.rs:876`): мини-`ArgCursor` без clap; сохранить дословные тексты ошибок
  (тесты ассертят их), семантику `required_value` и два режима (строгий / с
  позиционными аргументами).
- **`required_value` отвергает значения, начинающиеся с `--`** (`cli.rs:1091`):
  `agent run --message "--fix the bug"` падает. Убрать фильтр (защита «забыли значение»
  сохраняется через `required_flag`); `--flag=value` — на уровне диспетчеризации.
- **Список builtin-провайдеров продублирован** (`providers.rs:334`, `cli.rs:451`):
  `BuiltinProvider::ALL` + переписать на него оба места И тесты
  (`provider_models.rs` уже дрейфанул — DeepSeek отсутствует).
- **Негативные сценарии конфига не покрыты** (`tests/config_store.rs`): битый JSON,
  усечённый файл, незнакомый вариант, save поверх битого — писать первыми, до фиксов.

---

## 10. Дорожная карта

**Волна 1 — «не виснуть и не терять работу» (все small, ~1–2 дня):**
catch_unwind в воркере (1.2); split-таймауты ureq (1.3); saw_done + skip не-JSON +
error-событие (6.1, 6.2); finish_reason/truncated (2.5); retry + тело ошибки (2.4);
один ureq::Agent + дочитывание до EOF (5.1); id-fallback (3.9); index-cap (5.5);
Release-фильтр (4.7); частичный вывод при таймауте (3.8).

**Волна 2 — «качество tool_calls и анти-зацикливание» (small/medium):**
CRLF-fallback в replace (3.1); абсолютные пути (3.2); коэрция null/типов (3.3);
обёртки arguments (3.4); hint на ошибках + алиасы (3.5, 3.6); timeout-единицы (3.7);
финальный ход при max_tool_rounds (2.1); детекция повторов (2.2); сохранение текста
при tool_calls (2.3); schema тулов (9); описание шелла (4.6); UTF-8 prelude (4.5);
история REPL (2.7); stdout один раз (5.4).

**Волна 3 — «macOS + конфиг»:**
cfg(unix) в тестах (4.1); pbpaste/osascript (4.2); proc_pidinfo (4.3); bash на macOS
(4.4); Windows-пути на Unix (4.8); symlink-write (4.9); config recovery + атомарная
запись + tolerant enums + 0600 + .gitignore (7.1–7.4).

**Волна 4 — «большие куски»:**
убийство дерева процессов (1.1); агент в поток + отмена + отзывчивый UI (1.4);
кэш рендера + визуальные строки + bounded transcript (8.1–8.4); Anthropic SSE;
разбиение tui.rs; envelope без клона истории (5.2, 5.3); ArgCursor (9);
vision-payload (8.4, последним — единственный, кто меняет форму ChatMessage).

Каждый пункт тестируем в существующем стиле (mock TcpListener / tempdir / TestBackend);
конкретные тест-сценарии указаны в пунктах.

---

## 11. Что проверено и в порядке (не трогать)

- null в **опциональных** полях уже чинится (`repair_tool_arguments` вычищает
  null-ключи, есть тест); wire-имена (`file_write`) корректно не считаются repair'ом.
- `\\?\`-префикс canonicalize: root и target канонизируются консистентно — ложных
  `OutsideWorkspace` нет; обход границы через `..` невозможен.
- CRLF в `file.search`/`file.tail` обрабатывается (`str::lines` срезает `\r`);
  не-UTF8 контент даёт аккуратный `InvalidUtf8`, паник нет.
- SSE-ридер структурно хорош: типизированный serde-парс, push_str-аккумуляторы,
  без посимвольных аллокаций; providers.json читается один раз на команду;
  `full_request_key` лишний раз не хэшируется.
- Пустой ответ модели НЕ зацикливает (немедленный return); paste-коалесинг и
  терминальные guard'ы корректны; idle-CPU нет (`event::read` блокирует).
- unwrap/expect в library-коде защищены проверками — единственная реальная паника
  была в ToolScheduler (п. 1.2).

## 12. Опровергнутые находки

Две находки об «Anthropic: top-level `cache_control` отвергается API» не подтверждены:
верификаторы сверили код (`chat_client.rs:907-914`, тест `tests/chat_client.rs:587-622`)
с актуальной документацией API и не нашли основания считать текущее поведение
неработоспособным. Поведение закреплено тестом; при живой интеграции с Anthropic стоит
перепроверить на реальном запросе.
