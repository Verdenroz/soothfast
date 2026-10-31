use std::collections::{BTreeMap, BTreeSet};

use super::select::{
    Action, Item, Lane, Side, SpecPackage, State, Step, Wanted, select, select_base,
};
use super::stage::stage_entry;
use super::*;
use crate::gate_config;

#[derive(Default)]
struct Fake {
    stored: BTreeSet<Item>,
    references: BTreeSet<String>,
    generated: BTreeMap<String, Vec<String>>,
}

impl State for Fake {
    fn stored(&self, item: &Item) -> bool {
        self.stored.contains(item)
    }

    fn reference_stored(&self, pkg: &str) -> bool {
        self.references.contains(pkg)
    }

    fn generated_from(&self, pkg: &str) -> Option<Vec<String>> {
        self.generated.get(pkg).cloned()
    }
}

fn bench(pkg: &str, side: Side) -> Item {
    Item::Bench {
        pkg: pkg.into(),
        side,
    }
}

fn routes(pkg: &str, target: &str) -> Item {
    Item::Routes {
        pkg: pkg.into(),
        target: target.into(),
    }
}

fn docs(pkg: &str, private: bool) -> Item {
    Item::Rustdoc {
        pkg: pkg.into(),
        private,
        features: None,
        aux: false,
    }
}

fn aux_docs(pkg: &str) -> Item {
    Item::Rustdoc {
        pkg: pkg.into(),
        private: true,
        features: None,
        aux: true,
    }
}

fn wanted() -> Wanted {
    Wanted {
        lanes: BTreeSet::from([Lane::Bench, Lane::Routes, Lane::Rustdoc]),
        packages: vec!["core".into(), "server".into()],
        docs: vec!["core".into()],
        spec: vec![SpecPackage {
            pkg: "server".into(),
            target: "routes".into(),
        }],
        features: None,
        docs_features: None,
        against_ref: Some("origin/main".into()),
        reuse_base: true,
        stage: None,
        shards: 1,
    }
}

fn actions(steps: &[Step]) -> Vec<(&Item, &Action)> {
    steps.iter().map(|s| (&s.item, &s.action)).collect()
}

#[test]
fn a_cold_store_builds_every_head_item() {
    let fake = Fake::default();
    let steps = select(&wanted(), &fake);
    assert_eq!(
        actions(&steps),
        [
            (&bench("core", Side::Head), &Action::Build),
            (&bench("server", Side::Head), &Action::Build),
            (&routes("server", "routes"), &Action::Build),
            (&docs("core", false), &Action::Build),
        ]
    );
}

#[test]
fn what_is_stored_is_not_built_again() {
    let fake = Fake {
        stored: BTreeSet::from([bench("core", Side::Head), docs("core", false)]),
        ..Fake::default()
    };
    let steps = select(&wanted(), &fake);
    let built: Vec<_> = steps
        .iter()
        .filter(|s| s.action == Action::Build)
        .map(|s| &s.item)
        .collect();
    assert_eq!(
        built,
        [&bench("server", Side::Head), &routes("server", "routes")]
    );
}

#[test]
fn a_lane_builds_only_its_own_items() {
    let fake = Fake::default();
    for (lane, expected) in [
        (Lane::Routes, vec![routes("server", "routes")]),
        (Lane::Rustdoc, vec![docs("core", false)]),
    ] {
        let wanted = Wanted {
            lanes: BTreeSet::from([lane]),
            ..wanted()
        };
        let items: Vec<_> = select(&wanted, &fake).into_iter().map(|s| s.item).collect();
        assert_eq!(items, expected);
    }
}

#[test]
fn a_generated_spec_adds_the_private_rustdoc_of_its_package_and_crates() {
    let fake = Fake {
        generated: BTreeMap::from([(
            "server".to_string(),
            vec!["core".to_string(), "types".to_string()],
        )]),
        ..Fake::default()
    };
    let wanted = Wanted {
        lanes: BTreeSet::from([Lane::Rustdoc]),
        ..wanted()
    };
    let items: Vec<_> = select(&wanted, &fake).into_iter().map(|s| s.item).collect();
    assert_eq!(
        items,
        [
            docs("core", false),
            docs("server", true),
            aux_docs("core"),
            aux_docs("types"),
        ]
    );
}

#[test]
fn a_crate_two_specs_share_is_built_once() {
    let fake = Fake {
        generated: BTreeMap::from([
            ("a".to_string(), vec!["shared".to_string()]),
            ("b".to_string(), vec!["shared".to_string()]),
        ]),
        ..Fake::default()
    };
    let spec = |pkg: &str| SpecPackage {
        pkg: pkg.into(),
        target: "soothfast".into(),
    };
    let wanted = Wanted {
        lanes: BTreeSet::from([Lane::Rustdoc]),
        docs: Vec::new(),
        spec: vec![spec("a"), spec("b")],
        ..wanted()
    };
    let shared = aux_docs("shared");
    let count = select(&wanted, &fake)
        .iter()
        .filter(|s| s.item == shared)
        .count();
    assert_eq!(count, 1);
}

#[test]
fn only_a_failed_head_bench_stops_the_merge_base_side() {
    let failed = |item| {
        (
            Step {
                item,
                action: Action::Build,
            },
            Outcome::Failed("x".into()),
        )
    };
    assert!(head_benches_built(&[failed(routes("server", "routes"))]));
    assert!(head_benches_built(&[failed(docs("core", false))]));
    assert!(!head_benches_built(&[failed(bench("core", Side::Head))]));
}

#[test]
fn staging_copies_the_resolved_entry_and_what_belongs_to_it() {
    let root = std::env::temp_dir().join(format!("soothfast-stage-{}", std::process::id()));
    let (store, stage) = (root.join("bins"), root.join("out"));
    std::fs::create_dir_all(&store).unwrap();
    for name in [
        "k1-d.bin",
        "k1-d.bin.extents",
        "k1.manifest",
        "k1.group",
        "k1-old.bin",
        "k2-d.bin",
    ] {
        std::fs::write(store.join(name), name).unwrap();
    }
    stage_entry(&store.join("k1-d.bin"), &stage);
    let mut names: Vec<_> = std::fs::read_dir(stage.join("bins"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["k1-d.bin", "k1-d.bin.extents", "k1.group", "k1.manifest"]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_docs_lane_keys_rustdoc_with_the_features_docs_check_uses() {
    let wanted = Wanted {
        lanes: BTreeSet::from([Lane::Rustdoc]),
        docs_features: Some("full".into()),
        ..wanted()
    };
    let items: Vec<_> = select(&wanted, &Fake::default())
        .into_iter()
        .map(|s| s.item)
        .collect();
    assert!(matches!(
        &items[0],
        Item::Rustdoc { features: Some(f), private: false, .. } if f == "full"
    ));
}

#[test]
fn a_gate_table_feature_keys_the_spec_rustdoc_but_not_the_docs_check_one() {
    let gate = gate_config::parse("[gate]\nfeatures = \"bench-gate\"\n").unwrap();
    let fake = Fake {
        generated: BTreeMap::from([("server".to_string(), Vec::new())]),
        ..Fake::default()
    };
    let wanted = Wanted {
        lanes: BTreeSet::from([Lane::Rustdoc]),
        features: gate.features,
        ..wanted()
    };
    let features: Vec<_> = select(&wanted, &fake)
        .into_iter()
        .map(|s| match s.item {
            Item::Rustdoc { features, .. } => features,
            other => panic!("{other}"),
        })
        .collect();
    assert_eq!(features, [None, Some("bench-gate".to_string())]);
}

#[test]
fn a_crate_wanted_whole_by_one_spec_and_as_aux_by_another_is_not_aux() {
    let fake = Fake {
        generated: BTreeMap::from([
            ("a".to_string(), vec!["b".to_string()]),
            ("b".to_string(), Vec::new()),
        ]),
        ..Fake::default()
    };
    let spec = |pkg: &str| SpecPackage {
        pkg: pkg.into(),
        target: "soothfast".into(),
    };
    let wanted = Wanted {
        lanes: BTreeSet::from([Lane::Rustdoc]),
        docs: Vec::new(),
        spec: vec![spec("a"), spec("b")],
        ..wanted()
    };
    let items: Vec<_> = select(&wanted, &fake).into_iter().map(|s| s.item).collect();
    assert_eq!(items, [docs("a", true), docs("b", true)]);
}

#[test]
fn a_stored_merge_base_run_leaves_that_side_unbuilt() {
    let fake = Fake {
        references: BTreeSet::from(["core".to_string()]),
        stored: BTreeSet::new(),
        ..Fake::default()
    };
    let steps = select_base(&wanted(), &fake);
    assert_eq!(
        actions(&steps),
        [
            (&bench("core", Side::Base), &Action::Covered),
            (&bench("server", Side::Base), &Action::Build),
        ]
    );
}

#[test]
fn a_stored_merge_base_executable_is_not_built_again() {
    let fake = Fake {
        stored: BTreeSet::from([bench("server", Side::Base)]),
        ..Fake::default()
    };
    let steps = select_base(&wanted(), &fake);
    assert_eq!(steps[1].action, Action::Stored);
    assert_eq!(steps[0].action, Action::Build);
}

#[test]
fn declining_to_reuse_the_base_builds_it_whatever_the_run_cache_holds() {
    let fake = Fake {
        references: BTreeSet::from(["core".to_string(), "server".to_string()]),
        ..Fake::default()
    };
    let wanted = Wanted {
        reuse_base: false,
        ..wanted()
    };
    assert!(
        select_base(&wanted, &fake)
            .iter()
            .all(|s| s.action == Action::Build)
    );
}

#[test]
fn no_ref_or_no_bench_lane_has_no_merge_base_side() {
    let fake = Fake::default();
    let no_ref = Wanted {
        against_ref: None,
        ..wanted()
    };
    let no_bench = Wanted {
        lanes: BTreeSet::from([Lane::Rustdoc]),
        ..wanted()
    };
    assert!(select_base(&no_ref, &fake).is_empty());
    assert!(select_base(&no_bench, &fake).is_empty());
}

fn argv(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn arguments_name_packages_lanes_and_the_routes_target() {
    let args = parse(&argv(&[
        "-p",
        "core",
        "--docs",
        "core",
        "--spec",
        "server:routes",
        "--spec",
        "other",
        "--only",
        "routes",
        "--against-ref",
        "origin/main",
        "--no-reuse-base",
    ]))
    .unwrap();
    let w = args.wanted;
    assert_eq!(w.packages, ["core"]);
    assert_eq!(w.docs, ["core"]);
    assert_eq!(
        w.spec.iter().map(|s| s.target.as_str()).collect::<Vec<_>>(),
        ["routes", "soothfast"]
    );
    assert_eq!(w.lanes, BTreeSet::from([Lane::Routes]));
    assert_eq!(w.against_ref.as_deref(), Some("origin/main"));
    assert!(!w.reuse_base);
}

#[test]
fn no_lane_named_means_every_lane() {
    let w = parse(&argv(&["-p", "core"])).unwrap().wanted;
    assert_eq!(w.lanes.len(), 3);
    assert!(parse(&argv(&["--only", "everything"])).is_err());
    assert!(parse(&argv(&["--nope"])).is_err());
}
