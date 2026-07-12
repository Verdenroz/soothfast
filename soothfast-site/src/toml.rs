//! The workspace's shared TOML subset: tables, array-of-tables, strings,
//! booleans, numbers, string arrays and inline tables.
//!
//! Hand-rolled rather than depending on a TOML crate, in keeping with the
//! dependency budget. Site config was the first consumer; spec config is the
//! second, so the primitives live here rather than inside either one.

/// A parsed TOML scalar, string array, or inline table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TomlValue {
    Str(String),
    Bool(bool),
    /// Integers and floats are kept apart so a JSON Schema `minimum` emits
    /// as a number rather than a quoted string.
    Int(i64),
    Float(String),
    StrArray(Vec<String>),
    /// `{ type = "string", format = "uuid" }`. Values are scalars only:
    /// nesting inline tables is legal TOML but nothing here needs it.
    Table(std::collections::BTreeMap<String, TomlValue>),
}

/// Parse a scalar value: string, bool, or string array.
pub fn parse_value(s: &str) -> Result<TomlValue, String> {
    match s {
        "true" => return Ok(TomlValue::Bool(true)),
        "false" => return Ok(TomlValue::Bool(false)),
        _ => {}
    }
    if s.starts_with('"') {
        return Ok(TomlValue::Str(parse_string(s)?.0));
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok(TomlValue::Int(n));
    }
    if s.parse::<f64>().is_ok() {
        return Ok(TomlValue::Float(s.to_string()));
    }
    if let Some(inner) = s.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let mut items = Vec::new();
        let mut rest = inner.trim();
        while !rest.is_empty() {
            let (item, len) = parse_string(rest)?;
            items.push(item);
            rest = rest[len..].trim_start();
            if let Some(r) = rest.strip_prefix(',') {
                rest = r.trim_start();
            } else if !rest.is_empty() {
                return Err(format!("expected `,` in array near {rest:?}"));
            }
        }
        return Ok(TomlValue::StrArray(items));
    }
    if let Some(inner) = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        let mut table = std::collections::BTreeMap::new();
        for item in split_top_level(inner, ',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (key, value) = item
                .split_once('=')
                .ok_or_else(|| format!("expected `key = value` in inline table near {item:?}"))?;
            let key = key.trim().trim_matches('"').to_string();
            table.insert(key, parse_value(value.trim())?);
        }
        return Ok(TomlValue::Table(table));
    }
    Err(format!(
        "unsupported value {s:?} (string, bool, number, [\"..\"] array, or {{ .. }} table)"
    ))
}

/// Split on a separator that is outside quotes, brackets and braces.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let (mut out, mut depth, mut in_str, mut escaped, mut start) =
        (Vec::new(), 0i32, false, false, 0);
    for (i, c) in s.char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Parse a leading double-quoted string; returns (content, bytes consumed).
pub fn parse_string(s: &str) -> Result<(String, usize), String> {
    let mut chars = s.char_indices();
    if chars.next().map(|(_, c)| c) != Some('"') {
        return Err(format!("expected string, got {s:?}"));
    }
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, i + 1)),
            '\\' => match chars.next().map(|(_, c)| c) {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                other => return Err(format!("unsupported escape \\{other:?}")),
            },
            c => out.push(c),
        }
    }
    Err(format!("unterminated string {s:?}"))
}

/// Comment-stripped, non-empty logical lines with their 1-based numbers.
/// A line whose `[` array is still open pulls in following lines, so
/// multi-line arrays parse as one `key = value`.
pub fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open = false;
    for (i, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        // `open` implies a previous line exists (it set the flag).
        match out.last_mut() {
            Some((_, prev)) if open => {
                prev.push(' ');
                prev.push_str(&line);
            }
            _ => out.push((i + 1, line)),
        }
        open = out
            .last()
            .map(|(_, l)| array_still_open(l))
            .unwrap_or(false);
    }
    out
}

/// True when the line contains `= [` with no matching `]` outside strings.
fn array_still_open(line: &str) -> bool {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for c in line.chars() {
        match c {
            '\\' if in_str => escaped = !escaped,
            '"' if !escaped => in_str = !in_str,
            '[' if !in_str => depth += 1,
            ']' if !in_str => depth -= 1,
            _ => escaped = false,
        }
    }
    depth > 0 && line.contains('=')
}

/// Strip a `#` comment, respecting `#` inside quoted strings.
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match c {
            '\\' if in_str => escaped = !escaped,
            '"' if !escaped => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => escaped = false,
        }
    }
    line
}
