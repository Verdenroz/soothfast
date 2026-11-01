use super::*;
use crate::invoke::{self, ItemMetrics, Run, SaveScope};
use soothfast_docs::baseline::{Number, number};

fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("soothfast-amb-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// Save, as `pkg`'s full save, items `(id, registrar, allocs)`.
fn save(root: &std::path::Path, pkg: &str, items: &[(&str, Option<&str>, u64)]) {
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
    invoke::save_baseline_in(root, "head", &run, scope).unwrap();
}

fn head(root: &std::path::Path) -> Value {
    invoke::load_baseline_in(root, "head").unwrap().unwrap()
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
        assert!(unmeasured::read(&doc).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn a_gated_registrar_that_listed_the_id_takes_it_from_the_listing_packages() {
    let mut doc = json!({
        "items": {},
        "owners": { "app": ["core::parse"], "web": ["core::parse"] },
        "ambiguous": { "core::parse": {
            "app": { "pkg": "core", "alloc": { "allocs": 9 } },
            "web": { "pkg": "core", "alloc": { "allocs": 8 } },
        } },
    });
    let gated = Gated::from([(
        "core".to_string(),
        BTreeSet::from(["core::parse".to_string()]),
    )]);
    settle(&mut doc, None, &gated).unwrap();
    assert!(unmeasured::owned_by(&doc, "app").is_empty());
    assert!(unmeasured::owned_by(&doc, "web").is_empty());
    assert_eq!(allocs(&doc, "core::parse"), Number::Missing);
}

#[test]
fn a_registrar_that_did_not_list_the_id_leaves_it_with_the_listing_package() {
    let root = scratch("feature-gated");
    save(&root, "core", &[("core::other", Some("core"), 1)]);
    save(&root, "app", &[("core::gated", Some("core"), 4)]);
    let doc = head(&root);
    assert_eq!(unmeasured::owned_by(&doc, "app"), ["core::gated"]);
    assert_eq!(allocs(&doc, "core::gated"), Number::Measured(4.0));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_lib_two_packages_link_with_no_bench_of_its_own_is_ambiguous_to_every_reader() {
    let root = scratch("unbenched-lib");
    save(&root, "a", &[("util::hash", Some("util"), 2)]);
    save(&root, "b", &[("util::hash", Some("util"), 3)]);
    let doc = head(&root);
    assert_eq!(
        allocs(&doc, "util::hash"),
        Number::Ambiguous(vec!["a".into(), "b".into()])
    );
    assert!(
        doc["items"]["util::hash"].is_null(),
        "an older reader sees no number"
    );
    assert_eq!(doc[AMBIGUOUS_KEY]["util::hash"]["a"]["alloc"]["allocs"], 2);
    assert_eq!(doc[AMBIGUOUS_KEY]["util::hash"]["b"]["alloc"]["allocs"], 3);
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].reason, Reason::AmbiguousId);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_id_one_package_stops_owning_gets_the_other_s_number_back() {
    let root = scratch("moved");
    save(&root, "a", &[("util::hash", None, 2)]);
    save(&root, "b", &[("util::hash", None, 3)]);
    save(&root, "a", &[]);
    let doc = head(&root);
    assert_eq!(allocs(&doc, "util::hash"), Number::Measured(3.0));
    assert!(doc.get(AMBIGUOUS_KEY).is_none());
    assert!(unmeasured::read(&doc).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_malformed_entry_fails_the_settle_and_changes_nothing() {
    let mut doc = json!({
        "items": { "x": { "alloc": { "allocs": 1 } } },
        "owners": { "a": ["x"], "b": ["x"] },
        "unmeasured": [{ "package": "a", "reason": "no-such-reason" }],
    });
    let before = doc.clone();
    assert!(settle(&mut doc, None, &Gated::new()).is_err());
    assert_eq!(doc, before);
}

#[test]
fn the_gate_reads_an_ambiguous_item_as_having_no_baseline_number() {
    let mut baseline = json!({
        "items": { "util::hash": { "alloc": { "allocs": 2 } } },
        "owners": { "a": ["util::hash"], "b": ["util::hash"] },
        "unmeasured": [{ "package": "a", "reason": "ambiguous-id", "detail": "", "items": ["util::hash"] }],
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
