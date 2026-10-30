use super::*;
use crate::SymbolKind;

const SRC: &str = r#"/** Foo docs. */
@Suppress("x")
class Foo {
    // ordinary note
    fun bar(a: Int): Int {
        return a
    }

    fun bar(s: String): Int {
        return s.length
    }

    companion object {
        fun make(): Foo = Foo()
    }

    object Registry {
        fun size(): Int = 0
    }
}

interface Shape {
    fun area(): Int
}

enum class Mode { ON, OFF }

typealias Id = Int

fun top() {}
"#;

#[test]
fn names_follow_containers_in_source_order() {
    assert_eq!(
        names(Lang::Kotlin, SRC),
        [
            "Foo",
            "Foo.bar",
            "Foo.bar",
            "Foo.Companion",
            "Foo.Companion.make",
            "Foo.Registry",
            "Foo.Registry.size",
            "Shape",
            "Shape.area",
            "Mode",
            "Id",
            "top",
        ]
    );
}

#[test]
fn kinds_and_spans() {
    let all = syms(Lang::Kotlin, SRC);
    assert_eq!(all[0].kind, SymbolKind::Class);
    assert_eq!((all[0].start_line, all[0].end_line), (2, 20));
    assert_eq!(all[1].kind, SymbolKind::Method);
    assert_eq!(all[3].kind, SymbolKind::Object);
    assert_eq!(all[7].kind, SymbolKind::Interface);
    assert_eq!(all[9].kind, SymbolKind::Enum);
    assert_eq!(all[10].kind, SymbolKind::TypeAlias);
    assert_eq!(all[11].kind, SymbolKind::Function);
}

#[test]
fn ordinary_comments_and_whitespace_keep_fingerprint() {
    let before = fp(Lang::Kotlin, SRC, "Foo.bar").unwrap();
    let edited = SRC
        .replace("return a\n", "return   a // trailing\n")
        .replace("// ordinary note", "/* other */");
    assert_eq!(fp(Lang::Kotlin, &edited, "Foo.bar").unwrap(), before);
}

#[test]
fn kdoc_and_body_edits_change_fingerprint() {
    let class = fp(Lang::Kotlin, SRC, "Foo").unwrap();
    let doc = SRC.replace("Foo docs.", "Changed docs.");
    assert_ne!(fp(Lang::Kotlin, &doc, "Foo").unwrap(), class);
    let body = SRC.replace("return a\n", "return a + 1\n");
    let bar = fp(Lang::Kotlin, SRC, "Foo.bar").unwrap();
    assert_ne!(fp(Lang::Kotlin, &body, "Foo.bar").unwrap(), bar);
}

#[test]
fn unknown_name_is_none() {
    assert_eq!(fp(Lang::Kotlin, SRC, "Foo.missing"), None);
}

#[test]
fn misparsed_members_are_missing_not_misnamed() {
    let src = "class Foo {\n    fun bar() {}\n    companion object { fun c() {} }\n}\n";
    assert!(!names(Lang::Kotlin, src).contains(&"c".to_string()));
    assert_eq!(fp(Lang::Kotlin, src, "Foo.Companion.c"), None);
}
