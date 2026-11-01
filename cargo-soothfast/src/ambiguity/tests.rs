use super::*;
use crate::invoke::{self, ItemMetrics, Run, SaveScope};
use crate::unmeasured::{self, Reason};
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

/// Mark `pkg`'s gate failed, its listing naming `ids`.
fn fail(root: &Scratch, pkg: &str, ids: &[&str]) {
    let ids = ids.iter().map(|id| id.to_string()).collect();
    let failed = unmeasured::entry(pkg, Reason::GateFailed, "failed", &[], ids);
    unmeasured::mark_in(&root.0, "head", &[failed], true).unwrap();
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
fn a_failed_lister_never_costs_the_registrar_its_number_in_either_order() {
    let root = scratch("failed-lister-after");
    save(&root, "util", &[("util::hash", Some("util"), 3)]);
    fail(&root, "app", &["util::hash"]);
    assert_eq!(allocs(&head(&root), "util::hash"), Number::Measured(3.0));

    let root = scratch("failed-lister-before");
    fail(&root, "app", &["util::hash"]);
    save(&root, "util", &[("util::hash", Some("util"), 3)]);
    assert_eq!(allocs(&head(&root), "util::hash"), Number::Measured(3.0));
}

#[test]
fn a_baseline_from_before_the_side_map_keeps_its_number() {
    let root = scratch("pre-side-map");
    let old = json!({
        "version": 1,
        "items": { "util::hash": { "alloc": { "allocs": 4, "bytes": 0 } } },
        "owners": { "a": ["util::hash"], "b": ["util::hash"] },
    });
    invoke::write_baseline_in(&root.0, "head", &old).unwrap();
    save(&root, "c", &[("c::x", None, 1)]);
    let doc = head(&root);
    assert!(matches!(allocs(&doc, "util::hash"), Number::Ambiguous(_)));
    assert!(
        doc["items"]["util::hash"].is_null(),
        "an older reader sees no number"
    );
    save(&root, "b", &[]);
    assert_eq!(allocs(&head(&root), "util::hash"), Number::Measured(4.0));
}

#[test]
fn a_gated_registrar_that_listed_the_id_takes_it_from_the_listing_packages() {
    let mut doc = json!({
        "items": {},
        "owners": { "app": ["core::parse"], "web": ["core::parse"] },
        "ambiguous": { "core::parse": [
            { "package": "app", "item": { "pkg": "core", "alloc": { "allocs": 9 } } },
            { "package": "web", "item": { "pkg": "core", "alloc": { "allocs": 8 } } },
        ] },
    });
    let gated = Gated::from([(
        "core".to_string(),
        BTreeSet::from(["core::parse".to_string()]),
    )]);
    settle(&mut doc, None, &gated);
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
    let saved = doc[AMBIGUOUS_KEY]["util::hash"].as_array().unwrap();
    let by = |p: &str| {
        saved.iter().find(|s| s["package"] == p).unwrap()["item"]["alloc"]["allocs"].clone()
    };
    assert_eq!((by("a"), by("b")), (json!(2), json!(3)));
    assert!(
        unmeasured::read(&doc).unwrap().is_empty(),
        "nothing an older CLI cannot read"
    );
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
}

#[test]
fn the_gate_reads_an_ambiguous_item_as_having_no_baseline_number() {
    let mut baseline = json!({
        "items": { "util::hash": { "alloc": { "allocs": 2 } } },
        "owners": { "a": ["util::hash"], "b": ["util::hash"] },
        "ambiguous": { "util::hash": [{ "package": "a", "item": {} }] },
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
