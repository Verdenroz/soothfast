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
    let mut run = Run {
        listed: true,
        ..Run::default()
    };
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
        doc["items"].get("dup::f").is_none(),
        "an older reader sees no item"
    );
    assert_eq!(doc["ambiguous"]["dup::f"]["packages"], json!(["a", "b"]));
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
    let mut run = Run {
        listed: true,
        ..Run::default()
    };
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
    let baseline = json!({
        "items": {},
        "ambiguous": { "util::hash": { "packages": ["a", "b"] } },
    });
    let mut run = Run::default();
    run.items
        .insert("util::hash".into(), ItemMetrics::default());
    assert_eq!(
        unbaselined(&baseline, &run),
        ["util::hash has no baseline number: ambiguous between a, b"]
    );
}

#[test]
fn an_ambiguous_id_keeps_what_it_covers_in_the_index_and_nothing_in_items() {
    let mut doc = json!({
        "items": { "util::hash": { "covers": "util::hash_bytes", "alloc": { "allocs": 2 } } },
        "owners": { "a": ["util::hash"], "b": ["util::hash"] },
        "assertions": [{ "id": "util::hash", "kind": "alloc", "ok": true, "detail": "" }],
    });
    settle(&mut doc, &Gated::new());
    assert!(doc["items"].get("util::hash").is_none());
    assert_eq!(doc["assertions"], json!([]));
    assert_eq!(
        doc["ambiguous"]["util::hash"],
        json!({ "packages": ["a", "b"], "covers": "util::hash_bytes" })
    );
    settle(&mut doc, &Gated::new());
    assert_eq!(doc["ambiguous"]["util::hash"]["covers"], "util::hash_bytes");
}

#[test]
fn settle_never_touches_an_imported_item() {
    let mut doc = json!({
        "items": { "jmh::a": { "imported": "jmh", "walltime": { "median_ns": 5.0 } } },
        "owners": { "a": ["jmh::a"], "b": ["jmh::a"] },
    });
    settle(&mut doc, &Gated::new());
    assert_eq!(doc["items"]["jmh::a"]["walltime"]["median_ns"], 5.0);
    assert!(doc.get("ambiguous").is_none());
}

#[test]
fn a_registration_its_package_no_longer_owns_is_pruned_and_blocks_no_save() {
    let mut doc = json!({
        "owners": { "core": [], "app": ["core::parse"] },
        "registered": { "core": ["core::parse"], "app": [] },
    });
    let mut run = Run::default();
    run.items
        .insert("core::parse".into(), ItemMetrics::default());
    assert!(foreign(&doc, "app", &run).is_empty());
    settle(&mut doc, &Gated::new());
    assert_eq!(doc["registered"]["core"], json!([]));
}

#[test]
fn a_full_save_whose_listing_names_nothing_of_its_own_records_none() {
    let mut doc = json!({ "registered": { "app": ["app::x"] } });
    let run = Run {
        listed: true,
        ..Run::default()
    };
    record_registered(&mut doc, "app", &run, true);
    assert_eq!(doc["registered"]["app"], json!([]));
}

#[test]
fn a_package_spelled_with_a_dash_is_the_one_spelled_with_an_underscore() {
    let mut doc = json!({
        "owners": { "my-core": ["my_core::parse"], "app": ["my_core::parse"] },
        "registered": { "my_core": ["my_core::parse"] },
    });
    unmeasured::normalize(&mut doc);
    let mut run = Run::default();
    let m = ItemMetrics {
        pkg: Some("my-core".into()),
        ..ItemMetrics::default()
    };
    run.items.insert("my_core::parse".into(), m);
    assert!(foreign(&doc, "my_core", &run).is_empty());
    assert_eq!(foreign(&doc, "app", &run).len(), 1);
    settle(&mut doc, &Gated::new());
    assert_eq!(unmeasured::owned_by(&doc, "my_core"), ["my_core::parse"]);
    assert!(unmeasured::owned_by(&doc, "app").is_empty());
}

#[test]
fn an_indexed_id_keeps_its_registrar_so_a_later_settle_resolves_it() {
    let mut doc = json!({
        "items": { "core::parse": { "pkg": "core", "covers": "core::parse" } },
        "owners": { "app": ["core::parse"], "web": ["core::parse"] },
    });
    settle(&mut doc, &Gated::new());
    assert_eq!(doc["ambiguous"]["core::parse"]["pkg"], "core");
    let gated = Gated::from([(
        "core".to_string(),
        BTreeSet::from(["core::parse".to_string()]),
    )]);
    settle(&mut doc, &gated);
    assert!(doc.get("ambiguous").is_none());
    assert!(unmeasured::owned_by(&doc, "app").is_empty());
}
