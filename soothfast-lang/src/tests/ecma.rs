use super::*;
use crate::SymbolKind;

const SRC: &str = r#"/** Adds. */
export function add(a: number, b: number): number {
    return a + b; // trailing
}

export const twice = (x: number) => x * 2;
const named = function (x: number) { return x; };
const answer = 42;

/** Box docs. */
export class Box {
    // ordinary note
    constructor(private v: number) {}

    get(): number {
        return this.v;
    }

    static of(v: number): Box {
        return new Box(v);
    }
}

export interface Shape {
    area(): number;
}

export type Id = string;

enum Mode { On, Off }

declare function over(a: string): void;
function over(a: number): void;
function over(a: any) {}

namespace Util {
    export function help() {}
}
"#;

const NAMES: [&str; 14] = [
    "add",
    "twice",
    "named",
    "Box",
    "Box.constructor",
    "Box.get",
    "Box.of",
    "Shape",
    "Id",
    "Mode",
    "over",
    "over",
    "over",
    "Util.help",
];

#[test]
fn names_follow_containers_in_source_order() {
    assert_eq!(names(Lang::TypeScript, SRC), NAMES);
    assert_eq!(names(Lang::Tsx, SRC), NAMES);
}

#[test]
fn kinds_and_spans() {
    let all = syms(Lang::TypeScript, SRC);
    assert_eq!(all[0].kind, SymbolKind::Function);
    assert_eq!((all[0].start_line, all[0].end_line), (2, 4));
    assert_eq!(all[1].kind, SymbolKind::Function);
    assert_eq!(all[3].kind, SymbolKind::Class);
    assert_eq!(all[5].kind, SymbolKind::Method);
    assert_eq!(all[7].kind, SymbolKind::Interface);
    assert_eq!(all[8].kind, SymbolKind::TypeAlias);
    assert_eq!(all[9].kind, SymbolKind::Enum);
}

#[test]
fn ordinary_comments_and_whitespace_keep_fingerprint() {
    let before = fp(Lang::TypeScript, SRC, "add").unwrap();
    let edited = SRC
        .replace("// trailing", "/* other */")
        .replace("a + b", "a  +  b");
    assert_eq!(fp(Lang::TypeScript, &edited, "add").unwrap(), before);
}

#[test]
fn jsdoc_and_body_edits_change_fingerprint() {
    let before = fp(Lang::TypeScript, SRC, "add").unwrap();
    let doc = SRC.replace("Adds.", "Sums.");
    assert_ne!(fp(Lang::TypeScript, &doc, "add").unwrap(), before);
    let body = SRC.replace("a + b", "a - b");
    assert_ne!(fp(Lang::TypeScript, &body, "add").unwrap(), before);
}

#[test]
fn overloads_hash_together() {
    let before = fp(Lang::TypeScript, SRC, "over").unwrap();
    let edited = SRC.replace("over(a: number): void;", "over(a: bigint): void;");
    assert_ne!(fp(Lang::TypeScript, &edited, "over").unwrap(), before);
}

#[test]
fn unknown_name_is_none() {
    assert_eq!(fp(Lang::TypeScript, SRC, "missing"), None);
}

const JS: &str = r#"/** Greets. */
export function greet(name) {
  return `hi ${name}`;
}

const shout = (s) => s.toUpperCase();
const limit = 3;

class Counter {
  // note
  inc() { return 1; }
  static zero() { return 0; }
}
"#;

#[test]
fn javascript_symbols_and_fingerprint() {
    assert_eq!(
        names(Lang::JavaScript, JS),
        ["greet", "shout", "Counter", "Counter.inc", "Counter.zero"]
    );
    let before = fp(Lang::JavaScript, JS, "Counter.inc").unwrap();
    let edited = JS.replace("// note", "// other note");
    assert_eq!(
        fp(Lang::JavaScript, &edited, "Counter.inc").unwrap(),
        before
    );
    let changed = JS.replace("inc() { return 1; }", "inc() { return 2; }");
    assert_ne!(
        fp(Lang::JavaScript, &changed, "Counter.inc").unwrap(),
        before
    );
    assert_eq!(fp(Lang::JavaScript, JS, "limit"), None);
}

#[test]
fn tsx_parses_jsx_components() {
    let src = "export const App = () => <div>hi</div>;\nexport function View() { return <b/>; }\n";
    assert_eq!(names(Lang::Tsx, src), ["App", "View"]);
}
