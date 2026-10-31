//! `cargo soothfast prebuild`: build into the stores what a later job reads,
//! and nothing else, so that job compiles nothing.
//!
//! What a job reads is named by its inputs: the bench packages and the
//! merge-base to gate against (`-p`, `--against-ref`), the package the docs
//! check documents (`--docs`), and the packages `spec` commands run on
//! (`--spec PKG[:TARGET]`). Each is looked up in the store first and built
//! only on a miss. The merge-base side is also left alone when the run cache
//! already holds a run for it, which is the gate's own test for not building
//! that side.
//!
//! `--only bench|routes|rustdoc` restricts the run to one lane, so the lanes
//! can run on separate machines: each fills a different part of the stores.
//! `--stage DIR` copies just the entries the run built or found, so the
//! machine hands on those and not everything it restored.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::invoke::{self, Build, CommonArgs, Visibility};
use crate::itemreuse::{self, Bin};
use crate::{
    benchexe, binstore, buildstamp, gate, gate_config, headrun, linkmap, runcache, seed,
    spec_config, spec_gen, workspace,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Lane {
    Bench,
    Routes,
    Rustdoc,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Side {
    Head,
    Base,
}

/// One thing a later job reads from a store.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Item {
    Bench {
        pkg: String,
        side: Side,
    },
    Routes {
        pkg: String,
        target: String,
    },
    Rustdoc {
        pkg: String,
        private: bool,
        features: Option<String>,
        /// A workspace crate documented beside a spec's own package, whose
        /// failure costs that crate's types their schemas and nothing else.
        aux: bool,
    },
}

impl Item {
    fn pkg(&self) -> &str {
        match self {
            Item::Bench { pkg, .. } | Item::Routes { pkg, .. } | Item::Rustdoc { pkg, .. } => pkg,
        }
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Bench { pkg, side } => {
                let side = if *side == Side::Head {
                    "head"
                } else {
                    "merge-base"
                };
                write!(f, "bench executable {pkg} ({side})")
            }
            Item::Routes { pkg, target } => write!(f, "routes executable {pkg} ({target})"),
            Item::Rustdoc { pkg, private, .. } => {
                let scope = if *private { "private" } else { "public" };
                write!(f, "rustdoc json {pkg} ({scope} items)")
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Action {
    Build,
    Stored,
    /// Nothing is needed: the gate would not build it.
    Covered,
}

#[derive(Debug, PartialEq)]
struct Step {
    item: Item,
    action: Action,
}

#[derive(Clone, Debug)]
struct SpecPackage {
    pkg: String,
    target: String,
}

/// What the caller asked to have ready.
#[derive(Clone)]
struct Wanted {
    lanes: BTreeSet<Lane>,
    packages: Vec<String>,
    docs: Vec<String>,
    spec: Vec<SpecPackage>,
    features: Option<String>,
    against_ref: Option<String>,
    reuse_base: bool,
    stage: Option<PathBuf>,
}

/// What the stores and the run cache hold.
trait State {
    fn stored(&self, item: &Item) -> bool;
    /// Whether the run cache holds the merge-base run the gate would reuse
    /// for `pkg`.
    fn reference_stored(&self, pkg: &str) -> bool;
    /// The workspace crates to document with `pkg`, or `None` when none of
    /// its specs is generated from the code.
    fn generated_from(&self, pkg: &str) -> Option<Vec<String>>;
}

fn step(item: Item, state: &impl State) -> Step {
    let action = if state.stored(&item) {
        Action::Stored
    } else {
        Action::Build
    };
    Step { item, action }
}

/// The steps for HEAD's tree, in the order they run.
fn select(wanted: &Wanted, state: &impl State) -> Vec<Step> {
    let mut items = Vec::new();
    if wanted.lanes.contains(&Lane::Bench) {
        items.extend(wanted.packages.iter().map(|pkg| Item::Bench {
            pkg: pkg.clone(),
            side: Side::Head,
        }));
    }
    if wanted.lanes.contains(&Lane::Routes) {
        items.extend(wanted.spec.iter().map(|s| Item::Routes {
            pkg: s.pkg.clone(),
            target: s.target.clone(),
        }));
    }
    if wanted.lanes.contains(&Lane::Rustdoc) {
        let rustdoc =
            |pkg: &str, private: bool, features: Option<String>, aux: bool| Item::Rustdoc {
                pkg: pkg.to_string(),
                private,
                features,
                aux,
            };
        items.extend(
            wanted
                .docs
                .iter()
                .map(|pkg| rustdoc(pkg, false, None, false)),
        );
        for s in &wanted.spec {
            let Some(aux) = state.generated_from(&s.pkg) else {
                continue;
            };
            items.push(rustdoc(&s.pkg, true, wanted.features.clone(), false));
            items.extend(aux.iter().map(|pkg| rustdoc(pkg, true, None, true)));
        }
    }
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .map(|item| step(item, state))
        .collect()
}

/// The steps for the merge-base's tree. A package whose merge-base run is
/// stored needs no executable of that side.
fn select_base(wanted: &Wanted, state: &impl State) -> Vec<Step> {
    if !wanted.lanes.contains(&Lane::Bench) || wanted.against_ref.is_none() {
        return Vec::new();
    }
    wanted
        .packages
        .iter()
        .map(|pkg| {
            let item = Item::Bench {
                pkg: pkg.clone(),
                side: Side::Base,
            };
            if wanted.reuse_base && state.reference_stored(pkg) {
                return Step {
                    item,
                    action: Action::Covered,
                };
            }
            step(item, state)
        })
        .collect()
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Built,
    Stored,
    Covered,
    /// Built, but the store did not keep it; the consumer builds it again.
    Unstored,
    Failed(String),
}

struct Tree {
    wt: PathBuf,
    target: PathBuf,
}

/// The stores and run cache of this machine, seen from HEAD's checkout or,
/// with `base`, from the merge-base's worktree.
struct Live<'a> {
    common: &'a CommonArgs,
    base: Option<Tree>,
    base_sha: String,
    /// Where the entries this run needs are copied to, when asked.
    stage: Option<&'a Path>,
}

impl Live<'_> {
    fn one(&self, pkg: &str, target: Option<&str>) -> CommonArgs {
        CommonArgs {
            pkg: Some(pkg.to_string()),
            target: target
                .map(str::to_string)
                .or_else(|| self.common.target.clone()),
            ..self.common.clone()
        }
    }

    fn tree(&self, side: Side) -> (Option<&Path>, Option<&Path>) {
        match (&self.base, side) {
            (Some(t), Side::Base) => (Some(&t.wt), Some(&t.target)),
            _ => (None, None),
        }
    }
}

fn visibility(private: bool) -> Visibility {
    if private {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

impl State for Live<'_> {
    fn stored(&self, item: &Item) -> bool {
        match item {
            Item::Bench { pkg, side } => {
                let (dir, target_dir) = self.tree(*side);
                benchexe::is_stored(&self.one(pkg, None), dir, target_dir, Build::Measure)
            }
            Item::Routes { pkg, target } => {
                benchexe::is_stored(&self.one(pkg, Some(target)), None, None, Build::Discover)
            }
            Item::Rustdoc {
                pkg,
                private,
                features,
                ..
            } => invoke::rustdoc_stored(pkg, features.as_deref(), visibility(*private)),
        }
    }

    fn reference_stored(&self, pkg: &str) -> bool {
        let one = self.one(pkg, None);
        let stamp = buildstamp::capture(one.codegen_units_stamp().as_deref(), None);
        let (_, runs, _) = headrun::probe("prebuild", &one, &stamp);
        gate::stored_reference(&runs, true, &self.base_sha).is_some()
    }

    fn generated_from(&self, pkg: &str) -> Option<Vec<String>> {
        let cfg = spec_config::generating(pkg)?;
        Some(spec_gen::aux_crates(pkg, None, &cfg))
    }
}

/// Copy the files of the store entry named `key` in `store` into `stage`.
fn stage_entry(store: &Path, key: &str, stage: &Path) {
    let Some(name) = store.file_name() else {
        return;
    };
    let to = stage.join(name);
    let Ok(files) = std::fs::read_dir(store) else {
        return;
    };
    let _ = std::fs::create_dir_all(&to);
    for file in files.flatten() {
        if file.file_name().to_string_lossy().starts_with(key) {
            let _ = std::fs::copy(file.path(), to.join(file.file_name()));
        }
    }
}

/// The key of the entry holding `exe`, which is named `<key>-<digest>.bin`.
fn executable_key(exe: &Path) -> Option<String> {
    let entry = binstore::entry_of(exe)?;
    let name = entry.file_name()?.to_str()?;
    Some(name.split_once('-')?.0.to_string())
}

/// Copy what a later job reads for `item` into `stage`: its entry, the
/// manifest naming its inputs, and the digest and extents kept beside it.
fn stage_item(item: &Item, live: &Live, stage: &Path) {
    match item {
        Item::Bench { pkg, side } => {
            let (dir, target_dir) = live.tree(*side);
            let exe = benchexe::executable(&live.one(pkg, None), dir, target_dir, Build::Measure);
            stage_from(
                exe.as_deref().and_then(executable_key),
                binstore::dir(),
                stage,
            );
        }
        Item::Routes { pkg, target } => {
            let exe =
                benchexe::executable(&live.one(pkg, Some(target)), None, None, Build::Discover);
            stage_from(
                exe.as_deref().and_then(executable_key),
                binstore::dir(),
                stage,
            );
        }
        Item::Rustdoc {
            pkg,
            private,
            features,
            ..
        } => stage_from(
            invoke::rustdoc_key(pkg, features.as_deref(), visibility(*private)),
            runcache::state_dir("rustdoc"),
            stage,
        ),
    }
}

fn stage_from(key: Option<String>, store: Option<PathBuf>, stage: &Path) {
    if let (Some(key), Some(store)) = (key, store) {
        stage_entry(&store, &key, stage);
    }
}

/// Link the bench target a second time for its map, when the gate would read
/// one for this binary.
fn link_map(common: &CommonArgs, exe: &Path, dir: Option<&Path>, target_dir: Option<&Path>) {
    let Ok(env) = invoke::host_env(exe, common.backend.as_deref()) else {
        return;
    };
    if !itemreuse::reads_maps(&env, common) {
        return;
    }
    let digest = headrun::binary_digest(exe);
    let bin = Bin {
        exe,
        digest: digest.as_deref(),
        dir,
        target_dir,
        env: Some(&env),
    };
    linkmap::extents("prebuild", common, &bin);
}

fn build_item(
    step: &Step,
    live: &Live,
    heads: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let missing = |pkg: &str| format!("{pkg}: the executable did not build");
    match &step.item {
        Item::Bench { pkg, side } => {
            let (dir, target_dir) = live.tree(*side);
            let one = live.one(pkg, None);
            let exe = benchexe::executable(&one, dir, target_dir, Build::Measure)
                .ok_or_else(|| missing(pkg))?;
            let digest = headrun::binary_digest(&exe);
            // Identical binaries need no map of the merge-base: the gate
            // measures the one pass.
            let same_as_head =
                *side == Side::Base && digest.is_some() && heads.get(pkg) == digest.as_ref();
            if *side == Side::Head
                && let Some(digest) = digest
            {
                heads.insert(pkg.clone(), digest);
            }
            if !same_as_head {
                link_map(&one, &exe, dir, target_dir);
            }
            Ok(())
        }
        Item::Routes { pkg, target } => {
            benchexe::executable(&live.one(pkg, Some(target)), None, None, Build::Discover)
                .map(drop)
                .ok_or_else(|| missing(pkg))
        }
        Item::Rustdoc {
            pkg,
            private,
            features,
            aux,
        } => {
            let built =
                invoke::rustdoc_json_in(pkg, None, features.as_deref(), visibility(*private));
            match built {
                Ok(_) => Ok(()),
                Err(e) if *aux => {
                    eprintln!("prebuild: cannot document workspace crate {pkg} ({e}); dropped");
                    Ok(())
                }
                Err(e) => Err(format!("{pkg}: {e}")),
            }
        }
    }
}

/// Run `steps`, building the benches of one side together first so cargo
/// shares their dependency graph.
fn execute(steps: &[Step], live: &Live, heads: &mut BTreeMap<String, String>) -> Vec<Outcome> {
    let side_of = |s: &Step| match &s.item {
        Item::Bench { side, .. } => Some(*side),
        _ => None,
    };
    for side in [Side::Head, Side::Base] {
        let missing: Vec<String> = steps
            .iter()
            .filter(|s| s.action == Action::Build && side_of(s) == Some(side))
            .map(|s| s.item.pkg().to_string())
            .collect();
        // `--features` names one package's features, so across several cargo
        // wants `pkg/feature`; those builds are left to the one-by-one pass.
        if !missing.is_empty() && live.common.features.is_none() {
            let (dir, target_dir) = live.tree(side);
            let label = format!("cargo bench build {side:?}");
            let _ = invoke::timed(&label, || {
                invoke::prebuild_benches(&missing, live.common, dir, target_dir)
            });
        }
    }
    steps
        .iter()
        .map(|s| {
            if s.action == Action::Covered {
                return Outcome::Covered;
            }
            let wants_build = s.action == Action::Build || side_of(s).is_some();
            let label = format!("prebuild {}", s.item);
            let built = if wants_build {
                invoke::timed(&label, || build_item(s, live, heads))
            } else {
                Ok(())
            };
            if let Some(stage) = live.stage
                && matches!(s.action, Action::Build | Action::Stored)
                && built.is_ok()
            {
                stage_item(&s.item, live, stage);
            }
            match built {
                Err(e) => Outcome::Failed(e),
                Ok(()) if s.action == Action::Stored => Outcome::Stored,
                Ok(()) if live.stored(&s.item) => Outcome::Built,
                Ok(()) => Outcome::Unstored,
            }
        })
        .collect()
}

struct Args {
    common: CommonArgs,
    wanted: Wanted,
}

fn take<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String, String> {
    it.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut common = CommonArgs::default();
    let mut lanes = BTreeSet::new();
    let mut wanted = Wanted {
        lanes: BTreeSet::new(),
        packages: Vec::new(),
        docs: Vec::new(),
        spec: Vec::new(),
        features: None,
        against_ref: None,
        reuse_base: true,
        stage: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => wanted.packages.push(take(&mut it, a)?),
            "--docs" => wanted.docs.push(take(&mut it, a)?),
            "--spec" => {
                let v = take(&mut it, a)?;
                let (pkg, target) = v.split_once(':').unwrap_or((&v, "soothfast"));
                wanted.spec.push(SpecPackage {
                    pkg: pkg.to_string(),
                    target: target.to_string(),
                });
            }
            "--against-ref" => wanted.against_ref = Some(take(&mut it, a)?),
            "--no-reuse-base" => wanted.reuse_base = false,
            "--stage" => wanted.stage = Some(PathBuf::from(take(&mut it, a)?)),
            "--only" => {
                lanes.insert(match take(&mut it, a)?.as_str() {
                    "bench" => Lane::Bench,
                    "routes" => Lane::Routes,
                    "rustdoc" => Lane::Rustdoc,
                    other => {
                        return Err(format!(
                            "--only wants bench, routes or rustdoc, got {other:?}"
                        ));
                    }
                });
            }
            _ => {
                if !common.try_parse(a, &mut it) {
                    return Err(format!("unknown prebuild arg {a:?}"));
                }
            }
        }
    }
    gate_config::apply(&mut common)?;
    wanted.features = common.features.clone();
    wanted.lanes = if lanes.is_empty() {
        BTreeSet::from([Lane::Bench, Lane::Routes, Lane::Rustdoc])
    } else {
        lanes
    };
    Ok(Args { common, wanted })
}

pub fn run(args: &[String]) -> i32 {
    let Args { common, wanted } = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let mut heads = BTreeMap::new();
    let base_sha = wanted
        .against_ref
        .as_deref()
        .and_then(|r| invoke::git(&["merge-base", "HEAD", r]).ok())
        .unwrap_or_default();
    let live = Live {
        common: &common,
        base: None,
        base_sha: base_sha.clone(),
        stage: wanted.stage.as_deref(),
    };
    let steps = select(&wanted, &live);
    let outcomes = execute(&steps, &live, &mut heads);
    let mut report: Vec<(Step, Outcome)> = steps.into_iter().zip(outcomes).collect();

    let head_built = head_benches_built(&report);
    if !head_built {
        println!("prebuild: no merge-base side: HEAD's bench executables did not all build");
    }
    if head_built
        && let Some(refname) = wanted
            .against_ref
            .as_deref()
            .filter(|_| wanted.lanes.contains(&Lane::Bench))
    {
        match base_side(&common, &wanted, refname, &base_sha, &mut heads) {
            Ok(done) => report.extend(done),
            Err(e) => eprintln!("prebuild: no merge-base side: {e}"),
        }
    }
    finish(&report)
}

/// Whether no bench executable of HEAD failed to build. Only those stop the
/// merge-base side, since the gate has nothing to compare without them.
fn head_benches_built(report: &[(Step, Outcome)]) -> bool {
    report.iter().all(|(step, outcome)| {
        let head_bench = matches!(
            step.item,
            Item::Bench {
                side: Side::Head,
                ..
            }
        );
        !(head_bench && matches!(outcome, Outcome::Failed(_)))
    })
}

/// The merge-base's executables, built in its worktree the way the gate
/// builds them.
fn base_side(
    common: &CommonArgs,
    wanted: &Wanted,
    refname: &str,
    base_sha: &str,
    heads: &mut BTreeMap<String, String>,
) -> Result<Vec<(Step, Outcome)>, String> {
    let target = invoke::worktree_target_dir().map_err(|e| e.to_string())?;
    let bench = common.target.as_deref().unwrap_or("soothfast");
    invoke::with_merge_base_worktree(refname, |wt| {
        let packages = wanted
            .packages
            .iter()
            .filter(|p| workspace::has_bench_target(p, bench, Some(wt)).unwrap_or(false))
            .cloned()
            .collect();
        invoke::sync_untracked_cargo_config(wt)?;
        invoke::sync_harness_versions(wt)?;
        seed::seed_worktree(wt, &target);
        let live = Live {
            common,
            base: Some(Tree {
                wt: wt.to_path_buf(),
                target,
            }),
            base_sha: base_sha.to_string(),
            stage: wanted.stage.as_deref(),
        };
        let wanted = Wanted {
            packages,
            ..wanted.clone()
        };
        let steps = select_base(&wanted, &live);
        let outcomes = execute(&steps, &live, heads);
        Ok(steps.into_iter().zip(outcomes).collect())
    })
}

/// Print what happened to each step and a total. A merge-base side that did
/// not build is no failure here: the gate reports it with its own output.
fn finish(report: &[(Step, Outcome)]) -> i32 {
    let mut counts = BTreeMap::new();
    let mut failed = false;
    for (step, outcome) in report {
        let label = match outcome {
            Outcome::Built => "built",
            Outcome::Stored => "stored",
            Outcome::Covered => "skipped",
            Outcome::Unstored => "unstored",
            Outcome::Failed(_) => "FAILED",
        };
        *counts.entry(label).or_insert(0) += 1;
        println!("prebuild: {label:<8} {}", step.item);
        if let Outcome::Failed(why) = outcome {
            eprintln!("prebuild: {why}");
            failed |= !matches!(
                step.item,
                Item::Bench {
                    side: Side::Base,
                    ..
                }
            );
        }
    }
    let total: Vec<String> = counts.iter().map(|(l, n)| format!("{n} {l}")).collect();
    println!(
        "prebuild: {}",
        if total.is_empty() {
            "nothing to build".to_string()
        } else {
            total.join(", ")
        }
    );
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            against_ref: Some("origin/main".into()),
            reuse_base: true,
            stage: None,
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
    fn staging_copies_the_files_of_one_key_only() {
        let root = std::env::temp_dir().join(format!("soothfast-stage-{}", std::process::id()));
        let (store, stage) = (root.join("bins"), root.join("out"));
        std::fs::create_dir_all(&store).unwrap();
        for name in ["k1-d.bin", "k1-d.bin.extents", "k1.manifest", "k2-d.bin"] {
            std::fs::write(store.join(name), name).unwrap();
        }
        stage_entry(&store, "k1", &stage);
        let mut names: Vec<_> = std::fs::read_dir(stage.join("bins"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["k1-d.bin", "k1-d.bin.extents", "k1.manifest"]);
        std::fs::remove_dir_all(root).unwrap();
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
}
