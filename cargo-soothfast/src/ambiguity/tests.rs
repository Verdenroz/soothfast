use super::*;
use crate::invoke::{self, ItemMetrics, Run, SaveScope};
use crate::unmeasured;
use serde_json::json;
use soothfast_docs::baseline::{Number, number};

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!("soothfast-amb-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    Scratch(root)
}

/// Save, as `pkg`'s full save, items `(id, registrar, allocs)`.
fn save(root: &Scratch, pkg: &str, items: &[(&str, Option<&str>, u64)]) {
    let mut run = Run::default();
    for (id, registrar, allocs) in items {
        let m = ItemMetrics {
            pkg: registrar.map(str::to_string),
            allocs: Some(*allocs),
            bytes: Some(0),
            ..ItemMetrics::default()
        };
        run.items.insert(id.to_string(), m);
    }
    let scope = SaveScope::BenchFull(Some(pkg.into()));
    invoke::save_baseline_in(&root.0, "head", &run, scope).unwrap();
}

fn head(root: &Scratch) -> Value {
    invoke::load_baseline_in(&root.0, "head").unwrap().unwrap()
}

fn allocs(doc: &Value, id: &str) -> Number {
    number(doc, id, "alloc", "allocs")
}

#[test]
fn the_registrar_owning_the_id_takes_it_whichever_saves_first() {
    for order in [["core", "app"], ["app", "core"]] {
        let root = scratch(&format!("registrar-{}", order[0]));
        for pkg in order {
            let allocs = if pkg == "core" { 1 } else { 9 };
            save(&root, pkg, &[("core::parse", Some("core"), allocs)]);
        }
        let doc = head(&root);
        assert_eq!(unmeasured::owned_by(&doc, "core"), ["core::parse"]);
        assert!(unmeasured::owned_by(&doc, "app").is_empty());
        assert_eq!(
            allocs(&doc, "core::parse"),
            Number::Measured(1.0),
            "{order:?}"
        );
    }
}

#[test]
fn two_packages_registering_one_id_leave_it_ambiguous_with_no_number() {
    let root = scratch("clash");
    save(&root, "a", &[("dup::f", Some("a"), 2)]);
    save(&root, "b", &[("dup::f", Some("b"), 3)]);
    let doc = head(&root);
    assert!(
        doc["items"]["dup::f"].is_null(),
        "an older reader sees no number"
    );
    assert_eq!(
        allocs(&doc, "dup::f"),
        Number::Ambiguous(vec!["a".into(), "b".into()])
    );
    assert!(
        unmeasured::read(&doc).unwrap().is_empty(),
        "nothing an older CLI cannot read"
    );
}

#[test]
fn a_rename_that_ends_the_clash_lets_the_other_package_s_next_save_restore_its_number() {
    let root = scratch("renamed");
    save(&root, "a", &[("dup::f", Some("a"), 2)]);
    save(&root, "b", &[("dup::f", Some("b"), 3)]);
    save(&root, "a", &[("dup::renamed", Some("a"), 2)]);
    assert!(matches!(allocs(&head(&root), "dup::f"), Number::Missing));
    save(&root, "b", &[("dup::f", Some("b"), 3)]);
    assert_eq!(allocs(&head(&root), "dup::f"), Number::Measured(3.0));
}

#[test]
fn a_gated_registrar_that_listed_the_id_takes_it_from_the_listing_packages() {
    let mut doc = json!({
        "items": { "core::parse": { "pkg": "core", "alloc": { "allocs": 9 } } },
        "owners": { "app": ["core::parse"], "web": ["core::parse"] },
    });
    let gated = Gated::from([(
        "core".to_string(),
        BTreeSet::from(["core::parse".to_string()]),
    )]);
    settle(&mut doc, &gated);
    assert!(unmeasured::owned_by(&doc, "app").is_empty());
    assert!(unmeasured::owned_by(&doc, "web").is_empty());
}

#[test]
fn a_registrar_that_did_not_list_the_id_leaves_it_with_the_listing_package() {
    let root = scratch("feature-gated");
    save(&root, "core", &[("core::other", Some("core"), 1)]);
    save(&root, "app", &[("core::gated", Some("core"), 4)]);
    let doc = head(&root);
    assert_eq!(unmeasured::owned_by(&doc, "app"), ["core::gated"]);
    assert_eq!(allocs(&doc, "core::gated"), Number::Measured(4.0));
}

#[test]
fn the_gate_reads_an_ambiguous_item_as_having_no_baseline_number() {
    let mut baseline = json!({
        "items": { "util::hash": { "alloc": { "allocs": 2 } } },
        "owners": { "a": ["util::hash"], "b": ["util::hash"] },
    });
    let mut run = Run::default();
    run.items
        .insert("util::hash".into(), ItemMetrics::default());
    let notes = unbaselined(&mut baseline, &run);
    assert!(baseline["items"]["util::hash"].is_null());
    assert_eq!(
        notes,
        ["gate: util::hash has no baseline number: it is ambiguous between a and b"]
    );
}
