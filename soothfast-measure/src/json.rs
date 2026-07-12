//! Minimal JSON string escaping for the runner's JSONL output.
//! (Dependency policy: serde lives only in `cargo-soothfast`.)

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_specials() {
        assert_eq!(super::esc(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(super::esc("x\ny"), "x\\ny");
    }
}
