//! Comment stripping for source-span fingerprints.
//!
//! Doc comments (`///`, `//!`, `/** */`, `/*! */`) survive: they state the
//! contract the prose is bound to. Ordinary comments are dropped, so editing
//! one leaves the fingerprint untouched.

/// Remove ordinary comments, replacing each with a space so neighbouring
/// tokens can't fuse. String, raw-string and char literals pass through
/// verbatim, so a `//` inside `"http://host"` is not mistaken for a comment.
pub fn strip(source: &str) -> String {
    let src: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < src.len() {
        let end = match kind_at(&src, i) {
            Some(Token::Doc(end)) | Some(Token::Literal(end)) => {
                out.extend(&src[i..end]);
                end
            }
            Some(Token::Comment(end)) => {
                out.push(' ');
                end
            }
            None => {
                out.push(src[i]);
                i + 1
            }
        };
        i = end;
    }
    out
}

enum Token {
    Doc(usize),
    Comment(usize),
    Literal(usize),
}

fn kind_at(src: &[char], i: usize) -> Option<Token> {
    match src[i] {
        '/' if src.get(i + 1) == Some(&'/') => {
            let end = src[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(src.len(), |n| i + n);
            match src.get(i + 2) {
                Some('/') | Some('!') => Some(Token::Doc(end)),
                _ => Some(Token::Comment(end)),
            }
        }
        '/' if src.get(i + 1) == Some(&'*') => {
            let end = block_end(src, i);
            // `/**/` is an empty ordinary comment, not the start of a doc one.
            let doc = src.get(i + 2) == Some(&'!')
                || (src.get(i + 2) == Some(&'*') && src.get(i + 3) != Some(&'/'));
            Some(if doc {
                Token::Doc(end)
            } else {
                Token::Comment(end)
            })
        }
        '"' => Some(Token::Literal(quoted_end(src, i + 1))),
        '\'' => char_literal_end(src, i).map(Token::Literal),
        'r' | 'b' if starts_token(src, i) => raw_end(src, i).map(Token::Literal),
        _ => None,
    }
}

fn starts_token(src: &[char], i: usize) -> bool {
    i == 0 || !(src[i - 1].is_alphanumeric() || src[i - 1] == '_')
}

fn block_end(src: &[char], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < src.len() {
        if src[i] == '/' && src.get(i + 1) == Some(&'*') {
            depth += 1;
            i += 2;
        } else if src[i] == '*' && src.get(i + 1) == Some(&'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    src.len()
}

fn quoted_end(src: &[char], mut i: usize) -> usize {
    while i < src.len() {
        match src[i] {
            '\\' => i += 2,
            '"' => return i + 1,
            _ => i += 1,
        }
    }
    src.len()
}

/// `'a'` and `'\n'` are literals; `'a` is a lifetime and carries no text to
/// protect, so it reports `None` and is copied one char at a time.
fn char_literal_end(src: &[char], i: usize) -> Option<usize> {
    if src.get(i + 1) == Some(&'\\') {
        let mut j = i + 3;
        while j < src.len() && src[j] != '\'' {
            j += 1;
        }
        return Some((j + 1).min(src.len()));
    }
    (src.get(i + 2) == Some(&'\'')).then_some(i + 3)
}

fn raw_end(src: &[char], i: usize) -> Option<usize> {
    let mut j = i;
    if src[j] == 'b' {
        j += 1;
    }
    if src.get(j) == Some(&'"') {
        return Some(quoted_end(src, j + 1));
    }
    if src.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let hashes = src[j..].iter().take_while(|c| **c == '#').count();
    j += hashes;
    if src.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    let close: Vec<char> = std::iter::once('"')
        .chain(std::iter::repeat_n('#', hashes))
        .collect();
    while j < src.len() {
        if src[j..].starts_with(&close) {
            return Some(j + close.len());
        }
        j += 1;
    }
    Some(src.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_without_comments_is_returned_unchanged() {
        let src = "pub fn keep<T>(value: T) -> T { value }";
        assert_eq!(strip(src), src);
    }

    #[test]
    fn line_and_block_comments_go_but_doc_comments_stay() {
        let src =
            "/// contract\npub struct S {\n  // note\n  /** kept */\n  /*! kept */\n  a: u8,\n}";
        let out = strip(src);
        assert!(out.contains("/// contract"));
        assert!(out.contains("/** kept */"));
        assert!(out.contains("/*! kept */"));
        assert!(!out.contains("note"));
    }

    #[test]
    fn a_slash_slash_inside_a_string_is_not_a_comment() {
        let src = r#"let u = "http://host/path"; // drop"#;
        let out = strip(src);
        assert!(out.contains(r#""http://host/path""#));
        assert!(!out.contains("drop"));
    }

    #[test]
    fn escaped_quote_does_not_end_the_string() {
        let src = r#"let s = "a\"// b"; // drop"#;
        let out = strip(src);
        assert!(out.contains(r#""a\"// b""#));
        assert!(!out.contains("drop"));
    }

    #[test]
    fn raw_strings_keep_their_hashes_and_contents() {
        let src = "let s = r#\"// not a comment \"# ; // drop";
        let out = strip(src);
        assert!(out.contains("r#\"// not a comment \"#"));
        assert!(!out.contains("drop"));
    }

    #[test]
    fn a_quote_char_literal_does_not_open_a_string() {
        let src = "let q = '\"'; // drop\nlet k = 1;";
        let out = strip(src);
        assert!(out.contains("let k = 1;"));
        assert!(!out.contains("drop"));
    }

    #[test]
    fn an_escaped_quote_char_literal_does_not_swallow_the_code_after_it() {
        let src = "match c { '\\'' => 1, '\\\\' => 2 } // drop\nlet k = 1;";
        let out = strip(src);
        assert!(out.contains("'\\'' => 1"), "{out}");
        assert!(out.contains("let k = 1;"), "{out}");
        assert!(!out.contains("drop"));
    }

    #[test]
    fn lifetimes_are_not_char_literals() {
        let src = "fn f<'a>(x: &'a str) -> &'a str { x } // drop";
        let out = strip(src);
        assert!(out.contains("fn f<'a>(x: &'a str) -> &'a str { x }"));
        assert!(!out.contains("drop"));
    }

    #[test]
    fn nested_block_comments_close_at_the_outer_terminator() {
        let src = "a /* x /* y */ z */ b";
        assert_eq!(
            strip(src).split_whitespace().collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn a_stripped_comment_cannot_fuse_its_neighbours() {
        assert_eq!(strip("foo/*x*/bar").trim(), "foo bar");
    }

    #[test]
    fn an_empty_block_comment_is_not_a_doc_comment() {
        assert_eq!(strip("a/**/b").trim(), "a b");
    }

    #[test]
    fn stripping_real_sources_twice_changes_nothing_the_second_time() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;
        for entry in std::fs::read_dir(src_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let once = strip(&text);
            assert_eq!(strip(&once), once, "not idempotent on {}", path.display());
            checked += 1;
        }
        assert!(checked > 0);
    }
}
