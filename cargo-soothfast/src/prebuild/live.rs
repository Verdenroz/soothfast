//! The machine's stores and run cache, seen from HEAD's checkout or the
//! merge-base's worktree.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::select::{Item, Side, State};
use crate::invoke::{self, Build, CommonArgs, HostEnv, Shard, Visibility};
use crate::runcache::Runs;
use crate::{benchexe, buildstamp, docs, spec_config, spec_gen};

pub(super) struct Tree {
    pub(super) wt: PathBuf,
    pub(super) target: PathBuf,
}

/// The stores and run cache of this machine, seen from HEAD's checkout or,
/// with `base`, from the merge-base's worktree.
pub(super) struct Live<'a> {
    pub(super) common: &'a CommonArgs,
    pub(super) base: Option<Tree>,
    pub(super) base_sha: String,
    /// Where the entries this run needs are copied to, when asked.
    pub(super) stage: Option<&'a Path>,
    pub(super) shards: u64,
    /// Whether the gate may reuse a stored merge-base run.
    pub(super) reuse_base: bool,
    pub(super) memo: &'a Memo,
}

/// What one run works out once and both sides read.
#[derive(Default)]
pub(super) struct Memo {
    /// What HEAD's bench binaries resolved.
    pub(super) heads: RefCell<BTreeMap<String, Head>>,
    pub(super) rustdoc_keys: RefCell<BTreeMap<Item, Option<String>>>,
    /// The run files that cover a package's merge-base, when they do.
    pub(super) references: RefCell<BTreeMap<String, Option<Vec<PathBuf>>>>,
}

pub(super) struct Head {
    pub(super) digest: Option<String>,
    pub(super) env: Option<HostEnv>,
}

impl Live<'_> {
    pub(super) fn one(&self, pkg: &str, target: Option<&str>) -> CommonArgs {
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
    pub(super) fn reference_files(&self, pkg: &str) -> Option<Vec<PathBuf>> {
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

    pub(super) fn find_reference(&self, pkg: &str) -> Option<Vec<PathBuf>> {
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
            Runs::new(&stamp, &common, env.clone())
                .reusable_reference(self.reuse_base, &self.base_sha)
                .map(|(path, _)| path)
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
    pub(super) fn stage_reference(&self, pkg: &str) {
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
    pub(super) fn rustdoc_key(&self, item: &Item) -> Option<String> {
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

    pub(super) fn tree(&self, side: Side) -> (Option<&Path>, Option<&Path>) {
        match (&self.base, side) {
            (Some(t), Side::Base) => (Some(&t.wt), Some(&t.target)),
            _ => (None, None),
        }
    }
}

pub(super) fn visibility(private: bool) -> Visibility {
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

    /// Docs that fail to scan are built for anyway; `docs check` reports the scan.
    fn docs_bind_rust(&self) -> bool {
        docs::binds_rust_at(invoke::project_root()).unwrap_or(true)
    }
}
