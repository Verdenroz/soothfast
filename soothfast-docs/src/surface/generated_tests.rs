use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::generated::{declaration, is_macro_call};
use super::{Surface, from_rustdoc};

const CALL: &str = "define_batch_response! {\n    /// Response containing quotes.\n    BatchQuotesResponse => quotes: Quote\n}\n";

fn call_site(tag: &str, source: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("soothfast-generated-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), source).unwrap();
    dir
}

fn batch_doc(id_shift: u64, field_type: Value, docs: &str) -> Value {
    let (s, f) = (2 + id_shift, 3 + id_shift);
    let span = json!({ "filename": "src/lib.rs", "begin": [1, 0], "end": [4, 0] });
    json!({
        "index": {
            "1": { "visibility": "public", "inner": { "module": { "items": [s] } } },
            s.to_string(): {
                "name": "BatchQuotesResponse",
                "visibility": "public",
                "docs": docs,
                "span": span,
                "inner": { "struct": {
                    "generics": { "params": [{ "name": "'a", "kind": { "lifetime": {} } }] },
                    "kind": { "plain": { "fields": [f] } },
                } },
            },
            f.to_string(): {
                "name": "quotes",
                "visibility": "public",
                "span": span,
                "inner": { "struct_field": field_type },
            },
        },
        "paths": {
            "1": { "path": ["fq"], "kind": "module" },
            s.to_string(): { "path": ["fq", "BatchQuotesResponse"], "kind": "struct" },
        },
    })
}

fn quotes() -> Value {
    json!({ "resolved_path": { "path": "Vec", "id": 9, "args": { "angle_bracketed": {
        "args": [{ "type": { "resolved_path": { "path": "Quote", "id": 8, "args": null } } }],
    } } } })
}

fn surface(tag: &str, source: &str, doc: &Value) -> Surface {
    let dir = call_site(tag, source);
    let surface = from_rustdoc(doc, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    surface
}

fn batch_fingerprint(tag: &str, doc: &Value) -> u64 {
    surface(tag, CALL, doc).items["fq::BatchQuotesResponse"].fingerprint
}

#[test]
fn a_macro_generated_struct_is_listed_under_its_real_declaration() {
    let got = surface(
        "issue",
        CALL,
        &batch_doc(0, quotes(), "Response containing quotes."),
    );
    let info = &got.items["fq::BatchQuotesResponse"];
    assert_eq!(
        info.signature,
        "pub struct BatchQuotesResponse<'a> { pub quotes: Vec<Quote> }"
    );
    assert!(info.has_docs);
    assert_ne!(info.fingerprint, 0);
}

#[test]
fn json_ids_do_not_reach_the_fingerprint() {
    let a = batch_fingerprint("ids-a", &batch_doc(0, quotes(), "Docs."));
    let b = batch_fingerprint("ids-b", &batch_doc(40, quotes(), "Docs."));
    assert_eq!(a, b);
}

#[test]
fn changing_a_generated_field_type_moves_the_fingerprint() {
    let before = batch_fingerprint("ty-a", &batch_doc(0, quotes(), "Docs."));
    let after = batch_fingerprint("ty-b", &batch_doc(0, json!({ "primitive": "u8" }), "Docs."));
    assert_ne!(before, after);
}

#[test]
fn changing_generated_docs_moves_the_fingerprint() {
    let before = batch_fingerprint("doc-a", &batch_doc(0, quotes(), "One."));
    let after = batch_fingerprint("doc-b", &batch_doc(0, quotes(), "Two."));
    assert_ne!(before, after);
}

#[test]
fn an_ordinary_item_keeps_its_span_fingerprint_and_signature() {
    let source = "pub struct S {\n    pub a: u8,\n    b: u8,\n}\n";
    let mut doc = batch_doc(0, quotes(), "Docs.");
    doc["index"]["2"]["name"] = json!("S");
    doc["paths"]["2"]["path"] = json!(["fq", "S"]);
    let got = surface("ordinary", source, &doc);

    let info = &got.items["fq::S"];
    assert_eq!(info.signature, "pub struct S { pub a: u8 }");
    let text = "pub struct S { pub a: u8, b: u8, }";
    assert_eq!(info.fingerprint, soothfast_registry::fnv1a(text.as_bytes()));
}

#[test]
fn macro_invocations_are_told_from_declarations() {
    let calls = |decl, name| is_macro_call(decl, name, "struct");
    assert!(calls("define_batch_response!", "BatchQuotesResponse"));
    assert!(calls("crate::define!", "Thing"));
    assert!(!calls(
        "pub struct BatchQuotesResponse",
        "BatchQuotesResponse"
    ));
    assert!(!calls("make!(BatchQuotesResponse)", "BatchQuotesResponse"));
    assert!(!calls("pub struct Wrapper", "Other"));
    assert!(!calls("not_a_call! tokens", "Thing"));
    assert!(!is_macro_call("m!", "n", "macro"));
}

#[test]
fn functions_render_params_return_and_header() {
    let item = json!({ "name": "load", "inner": { "function": {
        "generics": { "params": [{ "name": "T", "kind": { "type": {} } }] },
        "header": { "is_async": true },
        "sig": {
            "inputs": [
                ["key", { "borrowed_ref": { "lifetime": null, "is_mutable": false,
                    "type": { "slice": { "primitive": "u8" } } } }],
                ["pair", { "tuple": [{ "generic": "T" }, { "dyn_trait": {} }] }],
            ],
            "output": { "array": { "type": { "primitive": "u8" }, "len": "4" } },
        },
    } } });
    assert_eq!(
        declaration(&Map::new(), &item, "function", true),
        "pub async fn load<T>(key: &[u8], pair: (T, _)) -> [u8; 4]"
    );
}

#[test]
fn enums_list_variant_names() {
    let index = json!({ "5": { "name": "A" }, "6": { "name": "B" } });
    let item = json!({ "name": "E", "inner": { "enum": { "variants": [5, 6] } } });
    assert_eq!(
        declaration(index.as_object().unwrap(), &item, "enum", true),
        "pub enum E { A, B }"
    );
}
