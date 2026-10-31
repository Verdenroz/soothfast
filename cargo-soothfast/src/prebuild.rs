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

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::invoke::{self, Build, CommonArgs, HostEnv, Shard, Visibility};
use crate::itemreuse::{self, Bin};
use crate::runcache::Runs;
use crate::{
    benchexe, binstore, buildstamp, gate_config, headrun, linkmap, runcache, seed, spec_config,
    spec_gen, workspace,
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

impl Item {
    /// The item without what does not tell two requests for it apart.
    fn identity(&self) -> Item {
        match self {
            Item::Rustdoc {
                pkg,
                private,
                features,
                ..
            } => Item::Rustdoc {
                pkg: pkg.clone(),
                private: *private,
                features: features.clone(),
                aux: false,
            },
            other => other.clone(),
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
    /// The `--features` the caller gave, which is all `docs check` uses: it
    /// never reads the `[gate]` table.
    docs_features: Option<String>,
    against_ref: Option<String>,
    reuse_base: bool,
    stage: Option<PathBuf>,
    /// Legs the gate is split into, which decide the run-cache keys a stored
    /// merge-base run is looked up under.
    shards: u64,
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
                .map(|pkg| rustdoc(pkg, false, wanted.docs_features.clone(), false)),
        );
        for s in &wanted.spec {
            let Some(aux) = state.generated_from(&s.pkg) else {
                continue;
            };
            items.push(rustdoc(&s.pkg, true, wanted.features.clone(), false));
            items.extend(aux.iter().map(|pkg| rustdoc(pkg, true, None, true)));
        }
    }
    // One item per crate, visibility and features; it is an aux crate only if
    // every requester wanted it as one.
    let mut seen: BTreeMap<Item, usize> = BTreeMap::new();
    let mut unique: Vec<Item> = Vec::new();
    for item in items {
        match seen.get(&item.identity()) {
            Some(&at) => {
                if let (Item::Rustdoc { aux, .. }, Item::Rustdoc { aux: wanted, .. }) =
                    (&mut unique[at], &item)
                {
                    *aux &= *wanted;
                }
            }
            None => {
                seen.insert(item.identity(), unique.len());
                unique.push(item);
            }
        }
    }
    unique.into_iter().map(|item| step(item, state)).collect()
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
    shards: u64,
    memo: &'a Memo,
}

/// What one run works out once and both sides read.
#[derive(Default)]
struct Memo {
    /// What HEAD's bench binaries resolved.
    heads: RefCell<BTreeMap<String, Head>>,
    rustdoc_keys: RefCell<BTreeMap<Item, Option<String>>>,
    /// The run files that cover a package's merge-base, when they do.
    references: RefCell<BTreeMap<String, Option<Vec<PathBuf>>>>,
}

struct Head {
    digest: Option<String>,
    env: Option<HostEnv>,
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

    /// The run files that cover the merge-base of `pkg` for every gate leg,
    /// or `None` when some leg would not find one.
    fn reference_files(&self, pkg: &str) -> Option<Vec<PathBuf>> {
        if let Some(known) = self.memo.references.borrow().get(pkg) {
            return known.clone();
        }
        let found = self.find_reference(pkg);
        self.memo
            .references
            .borrow_mut()
            .insert(pkg.to_string(), found.clone());
        found
    }

    fn find_reference(&self, pkg: &str) -> Option<Vec<PathBuf>> {
        let one = self.one(pkg, None);
        let stamp = buildstamp::capture(one.codegen_units_stamp().as_deref(), None);
        let env = self
            .memo
            .heads
            .borrow()
            .get(pkg)
            .and_then(|h| h.env.clone());
        let file = |shard: Option<Shard>| {
            let common = CommonArgs {
                shard,
                ..one.clone()
            };
            Runs::new(&stamp, &common, env.clone()).reference_file(&self.base_sha)
        };
        let legs: Vec<Option<Shard>> = match self.shards {
            0 | 1 => vec![None],
            count => (1..=count)
                .map(|index| Some(Shard { index, count }))
                .collect(),
        };
        let mut all = Vec::new();
        for leg in legs {
            all.push(file(leg)?);
        }
        all.sort();
        all.dedup();
        Some(all)
    }

    /// Hand the stored merge-base runs to the gate legs along with the
    /// executables, so a leg finds the same runs this decision did.
    fn stage_reference(&self, pkg: &str) {
        let (Some(stage), Some(files)) = (self.stage, self.reference_files(pkg)) else {
            return;
        };
        let to = stage.join("runs");
        let _ = std::fs::create_dir_all(&to);
        for file in files {
            if let Some(name) = file.file_name() {
                let _ = std::fs::copy(&file, to.join(name));
            }
        }
    }

    /// The store key of a rustdoc item, worked out once.
    fn rustdoc_key(&self, item: &Item) -> Option<String> {
        let Item::Rustdoc {
            pkg,
            private,
            features,
            ..
        } = item
        else {
            return None;
        };
        self.memo
            .rustdoc_keys
            .borrow_mut()
            .entry(item.clone())
            .or_insert_with(|| invoke::rustdoc_key(pkg, features.as_deref(), visibility(*private)))
            .clone()
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
            Item::Rustdoc { .. } => self
                .rustdoc_key(item)
                .is_some_and(|key| invoke::rustdoc_holds(&key)),
        }
    }

    fn reference_stored(&self, pkg: &str) -> bool {
        self.reference_files(pkg).is_some()
    }

    fn generated_from(&self, pkg: &str) -> Option<Vec<String>> {
        let cfg = spec_config::generating(pkg)?;
        Some(spec_gen::aux_crates(pkg, None, &cfg))
    }
}

/// Copy the entry `entry` of the store in `store` into `stage`, with the
/// manifest and group of its `key` and the files kept beside it.
fn stage_entry(store: &Path, entry: &Path, key: &str, stage: &Path) {
    let (Some(name), Some(entry_name)) = (store.file_name(), entry.file_name()) else {
        return;
    };
    let to = stage.join(name);
    let _ = std::fs::create_dir_all(&to);
    let beside = entry_name.to_string_lossy().into_owned();
    for file in std::fs::read_dir(store).into_iter().flatten().flatten() {
        let file_name = file.file_name();
        let wanted = file_name == entry_name
            || file_name
                .to_string_lossy()
                .starts_with(&format!("{beside}."))
            || file_name == format!("{key}.manifest").as_str()
            || file_name == format!("{key}.group").as_str();
        if wanted {
            let _ = std::fs::copy(file.path(), to.join(file_name));
        }
    }
}

/// The key of the entry holding `exe`, which is named `<key>-<digest>.bin`.
fn executable_entry(exe: &Path) -> Option<(PathBuf, String)> {
    let entry = binstore::entry_of(exe)?;
    let key = entry.file_name()?.to_str()?.split_once('-')?.0.to_string();
    Some((entry, key))
}

/// Copy what a later job reads for `item` into `stage`: its entry, the
/// manifest naming its inputs, and the digest and extents kept beside it.
fn stage_item(item: &Item, built: Option<PathBuf>, live: &Live, stage: &Path) {
    let from_exe = |exe: Option<PathBuf>| {
        if let (Some((entry, key)), Some(store)) =
            (exe.as_deref().and_then(executable_entry), binstore::dir())
        {
            stage_entry(&store, &entry, &key, stage);
        }
    };
    match item {
        Item::Bench { .. } => from_exe(built),
        Item::Routes { pkg, target } => from_exe(built.or_else(|| {
            benchexe::executable(&live.one(pkg, Some(target)), None, None, Build::Discover)
        })),
        Item::Rustdoc { .. } => {
            let (Some(key), Some(store), Ok(root)) = (
                live.rustdoc_key(item),
                runcache::state_dir("rustdoc"),
                invoke::workspace_root(),
            ) else {
                return;
            };
            if let Some(entry) = crate::store::lookup(&store, &key, &root, "json") {
                stage_entry(&store, &entry, &key, stage);
            }
        }
    }
}

/// Link the bench target a second time for its map, when the gate would read
/// one for this binary.
fn link_map(
    common: &CommonArgs,
    exe: &Path,
    env: Option<&HostEnv>,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) {
    let Some(env) = env else {
        return;
    };
    if !itemreuse::reads_maps(env, common) {
        return;
    }
    let digest = headrun::binary_digest(exe);
    let bin = Bin {
        exe,
        digest: digest.as_deref(),
        dir,
        target_dir,
        env: Some(env),
    };
    linkmap::extents("prebuild", common, &bin);
}

/// Builds what `step` names. The executable of a bench or routes step, which
/// staging then reads.
fn build_item(step: &Step, live: &Live) -> Result<Option<PathBuf>, String> {
    let missing = |pkg: &str| format!("{pkg}: the executable did not build");
    match &step.item {
        Item::Bench { pkg, side } => {
            let (dir, target_dir) = live.tree(*side);
            let one = live.one(pkg, None);
            let exe = benchexe::executable(&one, dir, target_dir, Build::Measure)
                .ok_or_else(|| missing(pkg))?;
            let digest = headrun::binary_digest(&exe);
            let env = invoke::host_env(&exe, one.backend.as_deref()).ok();
            // Identical binaries need no map of the merge-base: the gate
            // measures the one pass.
            let same_as_head = *side == Side::Base
                && digest.is_some()
                && live.memo.heads.borrow().get(pkg).map(|h| &h.digest) == Some(&digest);
            if *side == Side::Head {
                live.memo.heads.borrow_mut().insert(
                    pkg.clone(),
                    Head {
                        digest,
                        env: env.clone(),
                    },
                );
            }
            if !same_as_head {
                link_map(&one, &exe, env.as_ref(), dir, target_dir);
            }
            Ok(Some(exe))
        }
        Item::Routes { pkg, target } => {
            benchexe::executable(&live.one(pkg, Some(target)), None, None, Build::Discover)
                .map(Some)
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
                Ok(_) => Ok(None),
                Err(e) if *aux => {
                    eprintln!("prebuild: cannot document workspace crate {pkg} ({e}); dropped");
                    Ok(None)
                }
                Err(e) => Err(format!("{pkg}: {e}")),
            }
        }
    }
}

/// Run `steps`, building the benches of one side together first so cargo
/// shares their dependency graph.
fn execute(steps: &[Step], live: &Live) -> Vec<Outcome> {
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
                live.stage_reference(s.item.pkg());
                return Outcome::Covered;
            }
            let wants_build = s.action == Action::Build || side_of(s).is_some();
            let label = format!("prebuild {}", s.item);
            let built = if wants_build {
                invoke::timed(&label, || build_item(s, live))
            } else {
                Ok(None)
            };
            let outcome = match &built {
                Err(e) => Outcome::Failed(e.clone()),
                Ok(_) if s.action == Action::Stored => Outcome::Stored,
                Ok(_) if live.stored(&s.item) => Outcome::Built,
                Ok(_) => Outcome::Unstored,
            };
            if let Some(stage) = live.stage
                && matches!(outcome, Outcome::Built | Outcome::Stored)
            {
                stage_item(&s.item, built.ok().flatten(), live, stage);
            }
            outcome
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
        docs_features: None,
        against_ref: None,
        reuse_base: true,
        stage: None,
        shards: 1,
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
            "--shards" => {
                wanted.shards = take(&mut it, a)?
                    .parse()
                    .map_err(|_| "--shards wants a count".to_string())?
            }
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
    wanted.docs_features = common.features.clone();
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
    let memo = Memo::default();
    let base_sha = wanted
        .against_ref
        .as_deref()
        .filter(|_| wanted.lanes.contains(&Lane::Bench))
        .and_then(|r| invoke::git(&["merge-base", "HEAD", r]).ok())
        .unwrap_or_default();
    let live = Live {
        common: &common,
        base: None,
        base_sha: base_sha.clone(),
        stage: wanted.stage.as_deref(),
        shards: wanted.shards,
        memo: &memo,
    };
    let steps = select(&wanted, &live);
    let outcomes = execute(&steps, &live);
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
        match base_side(&live, &wanted, refname) {
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
fn base_side(head: &Live, wanted: &Wanted, refname: &str) -> Result<Vec<(Step, Outcome)>, String> {
    let covered = |pkg: &String| Step {
        item: Item::Bench {
            pkg: pkg.clone(),
            side: Side::Base,
        },
        action: Action::Covered,
    };
    // The worktree is only made when some package needs the merge-base built.
    if wanted.packages.is_empty()
        || (wanted.reuse_base && wanted.packages.iter().all(|p| head.reference_stored(p)))
    {
        wanted.packages.iter().for_each(|p| head.stage_reference(p));
        return Ok(wanted
            .packages
            .iter()
            .map(|p| (covered(p), Outcome::Covered))
            .collect());
    }
    let target = invoke::worktree_target_dir().map_err(|e| e.to_string())?;
    let bench = head.common.target.as_deref().unwrap_or("soothfast");
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
            common: head.common,
            base: Some(Tree {
                wt: wt.to_path_buf(),
                target,
            }),
            base_sha: head.base_sha.clone(),
            stage: head.stage,
            shards: head.shards,
            memo: head.memo,
        };
        let wanted = Wanted {
            packages,
            ..wanted.clone()
        };
        let steps = select_base(&wanted, &live);
        let outcomes = execute(&steps, &live);
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
        stage_entry(&store, &store.join("k1-d.bin"), "k1", &stage);
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
}
