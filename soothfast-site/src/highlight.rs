//! Build-time syntax highlighting: small lexers emitting `<span class=
//! "tok-*">` HTML, so the site ships zero highlighting JavaScript. Unknown
//! languages fall back to escaped plain text — highlighting is best-effort
//! by design; correctness of the escape is the only hard requirement.

use crate::template::escape;

/// Highlight `code` for `lang`. Always returns escaped HTML.
pub fn highlight(lang: &str, code: &str) -> String {
    match lang {
        "rust" => rust(code),
        "toml" => toml(code),
        "json" => json(code),
        "bash" | "sh" | "console" | "shell" => shell(code),
        _ => escape(code),
    }
}

fn span(class: &str, text: &str, out: &mut String) {
    out.push_str("<span class=\"tok-");
    out.push_str(class);
    out.push_str("\">");
    out.push_str(&escape(text));
    out.push_str("</span>");
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const RUST_PRIMITIVES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64",
];

fn rust(code: &str) -> String {
    let mut out = String::with_capacity(code.len() * 2);
    let b: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // Line and block comments (block comments nest in Rust).
        if c == '/' && b.get(i + 1) == Some(&'/') {
            let end = seek(&b, i, |c| c == '\n');
            span("com", &collect(&b, i, end), &mut out);
            i = end;
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                if b[j] == '/' && b.get(j + 1) == Some(&'*') {
                    depth += 1;
                    j += 2;
                } else if b[j] == '*' && b.get(j + 1) == Some(&'/') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            span("com", &collect(&b, i, j), &mut out);
            i = j;
        } else if c == '#' && b.get(i + 1) == Some(&'[')
            || c == '#' && b.get(i + 1) == Some(&'!') && b.get(i + 2) == Some(&'[')
        {
            // Attribute: consume to the matching bracket.
            let mut depth = 0i32;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            span("mac", &collect(&b, i, j), &mut out);
            i = j;
        } else if c == '"' {
            let j = string_end(&b, i);
            span("str", &collect(&b, i, j), &mut out);
            i = j;
        } else if c == 'r' && matches!(b.get(i + 1), Some('"' | '#')) {
            // Raw string r"..." / r#"..."#.
            let mut hashes = 0;
            let mut j = i + 1;
            while b.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&'"') {
                j += 1;
                'outer: while j < b.len() {
                    if b[j] == '"' {
                        let mut k = j + 1;
                        let mut n = 0;
                        while n < hashes && b.get(k) == Some(&'#') {
                            n += 1;
                            k += 1;
                        }
                        if n == hashes {
                            j = k;
                            break 'outer;
                        }
                    }
                    j += 1;
                }
                span("str", &collect(&b, i, j), &mut out);
                i = j;
            } else {
                i = ident(&b, i, &mut out);
            }
        } else if c == '\''
            && b.get(i + 1).is_some_and(|c| c.is_alphabetic() || *c == '_')
            && b.get(i + 2) != Some(&'\'')
        {
            // Lifetime 'a (vs char literal 'a').
            let end = seek(&b, i + 1, |c| !c.is_alphanumeric() && c != '_');
            span("kw", &collect(&b, i, end), &mut out);
            i = end;
        } else if c == '\'' {
            // Char literal, escapes included.
            let mut j = i + 1;
            while j < b.len() && b[j] != '\'' {
                j += if b[j] == '\\' { 2 } else { 1 };
            }
            span("str", &collect(&b, i, (j + 1).min(b.len())), &mut out);
            i = (j + 1).min(b.len());
        } else if c.is_ascii_digit() {
            let end = seek(&b, i, |c| {
                !c.is_ascii_alphanumeric() && c != '_' && c != '.'
            });
            span("num", &collect(&b, i, end), &mut out);
            i = end;
        } else if c.is_alphabetic() || c == '_' {
            i = ident(&b, i, &mut out);
        } else {
            out.push_str(&escape(&c.to_string()));
            i += 1;
        }
    }
    out
}

/// Classify one identifier: keyword, primitive/type, macro!, or call.
fn ident(b: &[char], start: usize, out: &mut String) -> usize {
    let end = seek(b, start, |c| !c.is_alphanumeric() && c != '_');
    let word = collect(b, start, end);
    if RUST_KEYWORDS.contains(&word.as_str()) {
        span("kw", &word, out);
    } else if RUST_PRIMITIVES.contains(&word.as_str())
        || word.chars().next().is_some_and(char::is_uppercase)
    {
        span("ty", &word, out);
    } else if b.get(end) == Some(&'!') {
        span("mac", &format!("{word}!"), out);
        return end + 1;
    } else if b.get(end) == Some(&'(') {
        span("fn", &word, out);
    } else {
        out.push_str(&escape(&word));
    }
    end
}

/// End index (exclusive) of a `"…"` string starting at `start`.
fn string_end(b: &[char], start: usize) -> usize {
    let mut j = start + 1;
    while j < b.len() && b[j] != '"' {
        j += if b[j] == '\\' { 2 } else { 1 };
    }
    (j + 1).min(b.len())
}

fn seek(b: &[char], from: usize, stop: impl Fn(char) -> bool) -> usize {
    let mut j = from;
    while j < b.len() && !stop(b[j]) {
        j += 1;
    }
    j
}

fn collect(b: &[char], from: usize, to: usize) -> String {
    b[from..to.min(b.len())].iter().collect()
}

fn toml(code: &str) -> String {
    let mut out = String::new();
    for (n, line) in code.lines().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let t = line.trim_start();
        if t.starts_with('#') {
            span("com", line, &mut out);
        } else if t.starts_with('[') {
            span("kw", line, &mut out);
        } else if let Some((k, v)) = line.split_once('=') {
            span("ty", k, &mut out);
            out.push('=');
            toml_value(v, &mut out);
        } else {
            out.push_str(&escape(line));
        }
    }
    out
}

fn toml_value(v: &str, out: &mut String) {
    let t = v.trim();
    if t.starts_with('"') || t.starts_with('[') {
        span("str", v, out);
    } else if t == "true" || t == "false" {
        span("kw", v, out);
    } else if t.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        span("num", v, out);
    } else {
        out.push_str(&escape(v));
    }
}

fn json(code: &str) -> String {
    let b: Vec<char> = code.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '"' {
            let j = string_end(&b, i);
            // Key if the next non-space char is a colon.
            let mut k = j;
            while k < b.len() && b[k] == ' ' {
                k += 1;
            }
            let class = if b.get(k) == Some(&':') { "ty" } else { "str" };
            span(class, &collect(&b, i, j), &mut out);
            i = j;
        } else if c.is_ascii_digit() || c == '-' && b.get(i + 1).is_some_and(char::is_ascii_digit) {
            let end = seek(&b, i + 1, |c| {
                !c.is_ascii_alphanumeric() && c != '.' && c != '+' && c != '-'
            });
            span("num", &collect(&b, i, end), &mut out);
            i = end;
        } else if c.is_alphabetic() {
            let end = seek(&b, i, |c| !c.is_alphanumeric());
            span("kw", &collect(&b, i, end), &mut out);
            i = end;
        } else {
            out.push_str(&escape(&c.to_string()));
            i += 1;
        }
    }
    out
}

fn shell(code: &str) -> String {
    let mut out = String::new();
    for (n, line) in code.lines().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let t = line.trim_start();
        if t.starts_with('#') {
            span("com", line, &mut out);
        } else if let Some(rest) = t.strip_prefix("$ ") {
            let indent = &line[..line.len() - t.len()];
            out.push_str(&escape(indent));
            span("kw", "$ ", &mut out);
            out.push_str(&escape(rest));
        } else {
            out.push_str(&escape(line));
        }
    }
    // lines() drops a trailing newline; code blocks keep theirs.
    if code.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_tokens_are_classified_and_escaped() {
        let html = highlight("rust", "fn main() { let s = \"a<b\"; }\n// done");
        assert!(html.contains("<span class=\"tok-kw\">fn</span>"));
        assert!(html.contains("<span class=\"tok-fn\">main</span>"));
        assert!(html.contains("<span class=\"tok-str\">&quot;a&lt;b&quot;</span>"));
        assert!(html.contains("<span class=\"tok-com\">// done</span>"));
        assert!(!html.contains("a<b"));
    }

    #[test]
    fn rust_attributes_macros_types() {
        let html = highlight(
            "rust",
            "#[soothfast::measured(alloc = 0)]\nprintln!(\"x\");\nlet v: Vec<u8> = vec![];",
        );
        assert!(html.contains("tok-mac\">#[soothfast::measured(alloc = 0)]"));
        assert!(html.contains("tok-mac\">println!"));
        assert!(html.contains("tok-ty\">Vec"));
        assert!(html.contains("tok-ty\">u8"));
    }

    #[test]
    fn rust_raw_strings_and_lifetimes() {
        let html = highlight(
            "rust",
            "let r = r#\"raw \" inside\"#; fn f<'a>(x: &'a str) {}",
        );
        assert!(html.contains("tok-str\">r#&quot;raw &quot; inside&quot;#</span>"));
        assert!(html.contains("tok-kw\">&#39;a</span>"));
    }

    #[test]
    fn unknown_language_is_escaped_verbatim() {
        assert_eq!(highlight("brainfuck", "<+>"), "&lt;+&gt;");
    }

    #[test]
    fn toml_and_shell_line_forms() {
        let html = highlight("toml", "[site]\nname = \"a\" # c");
        assert!(html.contains("tok-kw\">[site]"));
        let html = highlight("console", "$ cargo soothfast docs build\nwrote site/");
        assert!(html.contains("tok-kw\">$ </span>"));
    }
}
