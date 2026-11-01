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
        doc["items"]["dup::f"]["alloc"].is_null(),
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
    assert_eq!(
        allocs(&doc, "core::parse"),
        Number::Missing,
        "core saved none of it"
    );
}

#[test]
fn an_id_handed_to_its_registrar_keeps_only_a_number_the_registrar_saved() {
    for (saver, kept) in [("core", true), ("app", false)] {
        let mut doc = json!({
            "items": { "core::parse": { "pkg": "core", "saved_by": saver, "alloc": { "allocs": 1 } } },
            "owners": { "app": ["core::parse"], "core": ["core::parse"] },
            "registered": { "core": ["core::parse"] },
        });
        settle(&mut doc, &Gated::new());
        assert_eq!(unmeasured::owned_by(&doc, "core"), ["core::parse"]);
        assert_eq!(
            matches!(allocs(&doc, "core::parse"), Number::Measured(_)),
            kept,
            "{saver}"
        );
    }
}

#[test]
fn a_filtered_save_drops_ids_its_listing_shows_another_package_registered() {
    let mut doc = json!({ "registered": { "app": ["core::parse", "app::x"] } });
    let mut run = Run::default();
    let item = |pkg: &str| ItemMetrics {
        pkg: Some(pkg.into()),
        ..ItemMetrics::default()
    };
    run.items.insert("core::parse".into(), item("core"));
    run.items.insert("app::y".into(), item("app"));
    record_registered(&mut doc, "app", &run, false);
    assert_eq!(doc["registered"]["app"], json!(["app::x", "app::y"]));
}

#[test]
fn a_full_save_with_no_listing_keeps_what_was_registered() {
    let mut doc = json!({ "registered": { "app": ["app::x"] } });
    let mut run = Run::default();
    run.items.insert("app::x".into(), ItemMetrics::default());
    record_registered(&mut doc, "app", &run, true);
    assert_eq!(doc["registered"]["app"], json!(["app::x"]));
}

#[test]
fn a_save_never_writes_an_id_another_package_registered() {
    let root = scratch("never-write");
    save(&root, "core", &[("core::parse", Some("core"), 1)]);
    save(
        &root,
        "app",
        &[("core::parse", None, 9), ("app::x", Some("app"), 2)],
    );
    let doc = head(&root);
    assert_eq!(allocs(&doc, "core::parse"), Number::Measured(1.0));
    assert_eq!(doc["items"]["core::parse"]["saved_by"], "core");
    assert!(
        unmeasured::owned_by(&doc, "app")
            .iter()
            .all(|id| id != "core::parse")
    );
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
