use super::*;
use crate::SymbolKind;

const SRC: &str = r#"# module note
@decorator
async def top(x):
    """Docstring."""
    def inner():
        pass
    return x


class Outer:
    """Outer docs."""

    @property
    def value(self):
        # inline note
        return 1

    class Inner:
        def deep(self):
            pass
"#;

#[test]
fn names_follow_containers_in_source_order() {
    assert_eq!(
        names(Lang::Python, SRC),
        [
            "top",
            "Outer",
            "Outer.value",
            "Outer.Inner",
            "Outer.Inner.deep"
        ]
    );
}

#[test]
fn kinds_and_decorated_span() {
    let all = syms(Lang::Python, SRC);
    assert_eq!(all[0].kind, SymbolKind::Function);
    assert_eq!((all[0].start_line, all[0].end_line), (2, 7));
    assert_eq!(all[1].kind, SymbolKind::Class);
    assert_eq!(all[2].kind, SymbolKind::Method);
    assert_eq!((all[2].start_line, all[2].end_line), (13, 16));
}

#[test]
fn ordinary_comments_and_whitespace_keep_fingerprint() {
    let before = fp(Lang::Python, SRC, "Outer.value").unwrap();
    let edited = SRC
        .replace("# inline note", "#   different   note")
        .replace("return 1", "return   1");
    assert_eq!(fp(Lang::Python, &edited, "Outer.value").unwrap(), before);
    assert_eq!(before.len(), 16);
}

#[test]
fn docstring_and_body_edits_change_fingerprint() {
    let top = fp(Lang::Python, SRC, "top").unwrap();
    let doc = SRC.replace("Docstring.", "Changed.");
    assert_ne!(fp(Lang::Python, &doc, "top").unwrap(), top);
    let body = SRC.replace("return x", "return x + 1");
    assert_ne!(fp(Lang::Python, &body, "top").unwrap(), top);
}

#[test]
fn unknown_name_is_none() {
    assert_eq!(fp(Lang::Python, SRC, "missing"), None);
}

#[test]
fn syntax_errors_keep_parsed_symbols() {
    let broken = "def ok():\n    pass\n\ndef broken(:\n";
    assert!(names(Lang::Python, broken).contains(&"ok".to_string()));
}
