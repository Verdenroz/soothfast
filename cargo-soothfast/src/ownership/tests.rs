use super::*;
use crate::invoke::ItemMetrics;

fn bench() -> Vec<String> {
    vec!["core".into(), "my-app".into()]
}

fn listed(id: &str, pkg: Option<&str>) -> Listed {
    (id.to_string(), pkg.map(str::to_string))
}

#[test]
fn a_linked_lib_item_belongs_to_its_own_package_alone() {
    let listing = [
        listed("core::parse", Some("core")),
        listed("my_app::bench_decode", Some("my-app")),
    ];
    assert_eq!(
        owned_ids(&listing, "my-app", &bench()),
        ["my_app::bench_decode"]
    );
    assert_eq!(owned_ids(&listing, "core", &bench()), ["core::parse"]);
}

#[test]
fn an_item_with_no_package_or_one_without_a_bench_belongs_to_its_lister() {
    let listing = [
        listed("core::parse", None),
        listed("util::hash", Some("util")),
    ];
    assert_eq!(
        owned_ids(&listing, "my-app", &bench()),
        ["core::parse", "util::hash"]
    );
}

#[test]
fn a_save_keeps_only_the_items_its_package_owns() {
    let item = |pkg: &str| ItemMetrics {
        pkg: Some(pkg.into()),
        median_ns: Some(1.0),
        ..ItemMetrics::default()
    };
    let mut run = Run::default();
    run.items.insert("core::parse".into(), item("core"));
    run.items.insert("my_app::decode".into(), item("my-app"));
    let owned = owned_run(&run, "my_app", &bench());
    assert_eq!(owned.items.keys().collect::<Vec<_>>(), ["my_app::decode"]);
}

#[test]
fn a_lib_item_both_bench_binaries_measure_is_owned_once_and_not_ambiguous() {
    let root = std::env::temp_dir().join(format!("soothfast-linked-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let item = |pkg: &str| ItemMetrics {
        pkg: Some(pkg.into()),
        median_ns: Some(1.0),
        ..ItemMetrics::default()
    };
    let mut core = Run::default();
    core.items.insert("core::parse".into(), item("core"));
    let mut app = Run::default();
    app.items.insert("core::parse".into(), item("core"));
    app.items.insert("my_app::decode".into(), item("my-app"));
    for (pkg, run) in [("core", &core), ("my-app", &app)] {
        let scope = invoke::SaveScope::BenchFull(Some(pkg.into()));
        invoke::save_baseline_in(&root, "head", &owned_run(run, pkg, &bench()), scope).unwrap();
    }
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(crate::unmeasured::owned_by(&doc, "core"), ["core::parse"]);
    assert_eq!(
        crate::unmeasured::owned_by(&doc, "my-app"),
        ["my_app::decode"]
    );
    assert!(crate::unmeasured::read(&doc).unwrap().is_empty());
    assert!(doc["items"]["core::parse"]["walltime"]["median_ns"].is_number());
    let _ = std::fs::remove_dir_all(&root);
}
