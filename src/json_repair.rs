//! Shared almost-JSON repair pipeline. Both consumers of model-generated
//! JSON — tool arguments on the wire (`chat_client`) and the tool runtime's
//! argument repair — go through the same string-literal-aware passes, so a
//! comma inside a string can never be "repaired" away by one of them.

use serde_json::Value;

/// Починить почти-JSON текст: сырой парс, затем экранирование control-chars
/// в строках, срез висячих запятых и докрытие незакрытых скобок (bounded).
/// Возвращает распарсенное значение; `None` — текст не спасти.
pub fn repair_json_text(raw: &str) -> Option<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Some(value);
    }
    let mut text = escape_control_chars_in_strings(raw);
    text = strip_trailing_commas(&text);
    let closers = missing_json_closers(&text);
    // Больше 8 недостающих закрывашек — это не обрезанный стримом вызов,
    // а мусор; чинить его дальше рискованнее, чем вернуть как есть.
    if closers.len() > 8 {
        return None;
    }
    text.push_str(&closers);
    serde_json::from_str::<Value>(&text).ok()
}

/// Закрывашки для незакрытых `{`/`[` (и висячей кавычки), вычисленные одним
/// проходом с учётом строковых литералов и экранирования.
pub fn missing_json_closers(text: &str) -> String {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    let mut out = String::new();
    if in_string {
        out.push('"');
    }
    while let Some(closer) = stack.pop() {
        out.push(closer);
    }
    out
}

/// `\n`/`\t`/`\r` живьём внутри строковых литералов -> экранированные
/// последовательности (аналог питоновского `json.loads(strict=False)`).
pub fn escape_control_chars_in_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
                out.push(ch);
                continue;
            }
            match ch {
                '\\' => {
                    escaped = true;
                    out.push(ch);
                }
                '"' => {
                    in_string = false;
                    out.push(ch);
                }
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        }
        out.push(ch);
    }
    out
}

/// Запятая перед `}`/`]` (вне строк) выбрасывается — частый артефакт
/// генерации у GLM/llama.cpp-стека. Строковые литералы отслеживаются, так
/// что запятая внутри строки никогда не срезается.
pub fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for (index, &ch) in chars.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == ',' {
            let next_meaning = chars[index + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next_meaning, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(ch);
    }
    out
}
