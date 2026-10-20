//! Path globs for `soothfast.toml` lists: `*` and `?` stay inside one path
//! segment, `**` crosses them.

/// Whether `path` (forward slashes, relative to the workspace root) matches
/// `pattern`.
pub fn matches(pattern: &str, path: &str) -> bool {
    go(pattern.as_bytes(), path.as_bytes())
}

fn go(pat: &[u8], text: &[u8]) -> bool {
    match pat.first() {
        None => text.is_empty(),
        Some(b'*') if pat.get(1) == Some(&b'*') => {
            let rest = pat[2..].strip_prefix(b"/").unwrap_or(&pat[2..]);
            (0..=text.len()).any(|i| go(rest, &text[i..]))
        }
        Some(b'*') => {
            let rest = &pat[1..];
            let end = text.iter().position(|&c| c == b'/').unwrap_or(text.len());
            (0..=end).any(|i| go(rest, &text[i..]))
        }
        Some(b'?') => text.first().is_some_and(|&c| c != b'/') && go(&pat[1..], &text[1..]),
        Some(&c) => text.first() == Some(&c) && go(&pat[1..], &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn star_stays_inside_a_segment() {
        assert!(matches("docs/*.md", "docs/gating.md"));
        assert!(!matches("docs/*.md", "docs/sub/gating.md"));
    }

    #[test]
    fn double_star_crosses_segments_and_matches_none() {
        assert!(matches("docs/**", "docs/a/b.md"));
        assert!(matches("**/*.md", "README.md"));
        assert!(matches("**/*.md", "a/b/README.md"));
        assert!(!matches("docs/**", "src/a.md"));
    }

    #[test]
    fn question_mark_is_one_non_separator_char() {
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "a/c"));
    }

    #[test]
    fn a_literal_must_match_whole() {
        assert!(matches("LICENSE", "LICENSE"));
        assert!(!matches("LICENSE", "LICENSE.md"));
    }
}
