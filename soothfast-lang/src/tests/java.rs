use super::*;
use crate::SymbolKind;

const SRC: &str = r#"package demo;

/** Widget docs. */
@Deprecated
public class Widget {
    // ordinary note
    public Widget() {}

    public int size(int a) {
        return a;
    }

    public int size(String s) {
        return s.length();
    }

    interface Hook {
        void fire();
    }

    enum Mode {
        ON;

        void toggle() {}
    }

    record Point(int x, int y) {
        int sum() { return x + y; }
    }
}
"#;

#[test]
fn names_follow_containers_in_source_order() {
    assert_eq!(
        names(Lang::Java, SRC),
        [
            "Widget",
            "Widget.Widget",
            "Widget.size",
            "Widget.size",
            "Widget.Hook",
            "Widget.Hook.fire",
            "Widget.Mode",
            "Widget.Mode.toggle",
            "Widget.Point",
            "Widget.Point.sum",
        ]
    );
}

#[test]
fn kinds_and_spans() {
    let all = syms(Lang::Java, SRC);
    assert_eq!(all[0].kind, SymbolKind::Class);
    assert_eq!((all[0].start_line, all[0].end_line), (4, 30));
    assert_eq!(all[1].kind, SymbolKind::Method);
    assert_eq!(all[4].kind, SymbolKind::Interface);
    assert_eq!(all[6].kind, SymbolKind::Enum);
}

#[test]
fn ordinary_comments_and_whitespace_keep_fingerprint() {
    let before = fp(Lang::Java, SRC, "Widget.size").unwrap();
    let edited = SRC
        .replace("return a;", "return   a; // trailing")
        .replace("// ordinary note", "/* other */");
    assert_eq!(fp(Lang::Java, &edited, "Widget.size").unwrap(), before);
}

#[test]
fn javadoc_and_body_edits_change_fingerprint() {
    let class = fp(Lang::Java, SRC, "Widget").unwrap();
    let doc = SRC.replace("Widget docs.", "Changed docs.");
    assert_ne!(fp(Lang::Java, &doc, "Widget").unwrap(), class);
    let body = SRC.replace("return a;", "return a + 1;");
    assert_ne!(
        fp(Lang::Java, &body, "Widget.size").unwrap(),
        fp(Lang::Java, SRC, "Widget.size").unwrap()
    );
}

#[test]
fn overloads_hash_together_in_source_order() {
    let swapped = SRC.replace("size(int a)", "size(long a)");
    assert_ne!(
        fp(Lang::Java, &swapped, "Widget.size"),
        fp(Lang::Java, SRC, "Widget.size")
    );
}

#[test]
fn unknown_name_is_none() {
    assert_eq!(fp(Lang::Java, SRC, "Widget.missing"), None);
}
