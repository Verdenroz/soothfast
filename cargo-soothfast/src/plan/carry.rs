//! The lanes a pull request leaves alone: `plan` drops their jobs and shows
//! each in the verdict as a pass carried from a stored run.
//!
//! A lane is carried when a pass is stored under its key: the fingerprint of
//! its inputs at head and the delta, the merge-base side of the changed files
//! it reads. A lane the pull request did not touch has an empty delta, so a
//! pass of the default branch matches; a touched lane matches only a pass of
//! a pull-request head whose touched inputs began from the same base blobs.
//! The delta is what stops a pull request that reverts a lane's inputs to an
//! older state from matching an old pass while regressing against the base.

use std::collections::BTreeSet;
use std::path::Path;

use soothfast_docs::claims::Claim;

use serde_json::{Value, json};

use super::{Plan, names};
use crate::baseline_merge::CARRIED_KEY;
use crate::config::Config;
use crate::invoke;
use crate::lanes::delta::base_delta;
use crate::lanes::graph::Graph;
use crate::lanes::inputs::{Inputs, Lane, Reads, Setup};
use crate::lanes::store::{Pass, Store, key};
use crate::lanes::tool;
use crate::test::project::Files;
use crate::verdict::write_carried;

/// The two facts about the toolchain and the tool that `plan` cannot see.
pub(super) struct Stamps {
    pub rev: String,
    pub rust_stable: String,
    /// The pull request's base, which the deltas are read against.
    pub base: Option<String>,
}

/// The delta of a lane, or none when it cannot be read.
type Delta<'a> = &'a dyn Fn(&Lane) -> Option<String>;

/// A result a lane's job leaves: its name and the kind it is shown with.
type Shown = (String, &'static str);

/// One lane, with the results that say it passed.
struct Unit {
    lane: Lane,
    key: String,
    kind: &'static str,
    saved: Vec<String>,
    shown: Vec<Shown>,
}

/// The lanes of a plan that have a fingerprint.
struct Units {
    all: Vec<Unit>,
}

/// A lane whose job the plan drops, the commit that passed it, and, for a
/// gate the docs job still reads, what it measured.
#[derive(Debug, PartialEq)]
pub(super) struct Carried {
    pub lane: String,
    pub commit: String,
    shown: Vec<Shown>,
    baseline: Option<Value>,
}

/// A lane's fingerprint key and the results a default-branch run must leave
/// for the pass to be stored.
#[derive(Debug, PartialEq)]
pub(super) struct Recorded {
    lane: String,
    key: String,
    records: Vec<String>,
    kind: &'static str,
}

/// What the site build reads beyond the docs.
pub(super) struct Site {
    reads: Vec<Reads>,
    hook: bool,
}

impl Site {
    /// `None` when the `[site]` table does not parse.
    fn read(root: &Path, config: &Config) -> Option<Site> {
        let text = std::fs::read_to_string(root.join("soothfast.toml")).ok()?;
        let cfg = soothfast_site::config::parse(&text).ok()?;
        let docs = cfg
            .docs_dir
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        let theme = cfg
            .theme_dir
            .iter()
            .map(|d| d.to_string_lossy().to_string());
        let extras = cfg.extra_css.iter().chain(&cfg.extra_js);
        let files: BTreeSet<String> = extras
            .flat_map(|f| [f.clone(), format!("{docs}/{f}")])
            .collect();
        let reads = [Reads::Under(docs), Reads::Files(files)]
            .into_iter()
            .chain(theme.map(Reads::Under))
            .collect();
        Some(Site {
            reads,
            hook: config.site_run().ok()?.is_some(),
        })
    }
}

impl Plan {
    /// Whether the plan drops the job of `lane`.
    pub(super) fn carries(&self, lane: &str) -> bool {
        self.carried.iter().any(|c| c.lane == lane)
    }

    /// Reads the lanes' inputs, then records which lanes are carried and
    /// what each lane would leave. A lane without a fingerprint runs.
    pub(super) fn settle_lanes(&mut self, root: &Path, stamps: &Stamps) {
        let Ok(files) = Files::read(root) else { return };
        let Ok(config) = Config::load(root) else {
            return;
        };
        let graph = self.rust.as_ref().and_then(|_| Graph::read(root));
        let tool = tool::token(root, &stamps.rev, files.ids(), graph.as_ref());
        let setup = Setup {
            tool,
            rust_stable: stamps.rust_stable.clone(),
            runner: self.ci.runs_on_json(),
            shards: self.shards,
            pins: self.pins.clone(),
        };
        let (Ok(test), Ok(ci_inputs)) = (config.test(), config.ci_inputs()) else {
            return;
        };
        let Some(inputs) = Inputs::new(files.ids(), graph, test, ci_inputs, setup) else {
            return;
        };
        let site = Site::read(root, &config);
        let claims = self
            .docs
            .then(|| coverage::read(root, files.ids()))
            .flatten();
        let merge_base = stamps
            .base
            .as_deref()
            .and_then(|b| invoke::git_in(root, &["merge-base", b, "HEAD"]).ok());
        let changed = self.changed.clone();
        let delta = |lane: &Lane| {
            let touched = changed.iter().flatten();
            let paths: Vec<&str> = touched
                .filter(|f| inputs.reads(lane, f))
                .map(String::as_str)
                .collect();
            if paths.is_empty() {
                return Some(String::new());
            }
            base_delta(root, merge_base.as_deref()?, &paths)
        };
        self.decide(
            &inputs,
            site.as_ref(),
            Store::open().as_ref(),
            &delta,
            claims.as_deref(),
        );
    }

    /// A gate lane carries on its own key. While the docs job still runs, it
    /// also needs a stored baseline that measures every claim of its items.
    fn decide(
        &mut self,
        inputs: &Inputs,
        site: Option<&Site>,
        store: Option<&Store>,
        delta: Delta,
        claims: Option<&[Claim]>,
    ) {
        let units = self.units(inputs, site, delta);
        self.recorded = units
            .all
            .iter()
            .map(|u| Recorded {
                lane: u.lane.id.clone(),
                key: u.key.clone(),
                records: u.saved.clone(),
                kind: u.kind,
            })
            .collect();
        let (Some(_), Some(store)) = (&self.changed, store) else {
            return;
        };
        if self.landing || self.test_all() {
            return;
        }
        let found: Vec<(&Unit, Option<Pass>)> = units
            .all
            .iter()
            .map(|u| (u, store.lookup(&u.lane.id, &u.key)))
            .collect();
        let docs_runs = self.docs
            && !found
                .iter()
                .any(|(u, pass)| u.kind == "docs" && pass.is_some());
        self.carried = found
            .into_iter()
            .filter_map(|(u, pass)| {
                let pass = pass?;
                let gate = matches!(u.kind, "rust" | "import");
                let baseline = match (docs_runs && gate, pass.baseline) {
                    (false, _) => None,
                    (true, Some(b))
                        if claims.is_some_and(|c| coverage::resolves(&u.lane.id, &b, c)) =>
                    {
                        Some(b)
                    }
                    (true, _) => return None,
                };
                Some(Carried {
                    lane: u.lane.id.clone(),
                    commit: pass.commit,
                    shown: u.shown.clone(),
                    baseline,
                })
            })
            .collect();
    }

    fn units(&self, inputs: &Inputs, site: Option<&Site>, delta: Delta) -> Units {
        let rust = self.rust.as_ref();
        let packages = rust.map(|r| r.packages.as_slice()).unwrap_or_default();
        let docs_packages = rust.map(|r| r.docs_packages.as_slice()).unwrap_or_default();
        let gates: Vec<Option<Unit>> = packages
            .iter()
            .map(|p| {
                let lane = inputs.rust(&format!("rust:{p}"), p)?;
                let shown = vec![(p.clone(), "rust")];
                unit(inputs, delta, lane, "rust", self.gate_records(p), shown)
            })
            .collect();
        let imports: Vec<Option<Unit>> = self
            .projects
            .iter()
            .zip(names(&self.projects))
            .filter(|(p, _)| p.bench.is_some())
            .map(|(p, name)| {
                let lane = inputs.project(&format!("import:{name}"), p)?;
                let shown = vec![(name.clone(), "import")];
                unit(inputs, delta, lane, "import", vec![name], shown)
            })
            .collect();
        let documented: Option<Vec<Lane>> = docs_packages
            .iter()
            .map(|p| inputs.rust(&format!("docs:{p}"), p))
            .collect();
        let gated_ok = gates.iter().chain(&imports).all(Option::is_some);
        let feeds: Vec<&Lane> = gates
            .iter()
            .chain(&imports)
            .flatten()
            .map(|u| &u.lane)
            .collect();
        let base = documented.filter(|_| gated_ok).map(|documented| {
            let parts: Vec<&Lane> = feeds.iter().copied().chain(&documented).collect();
            Lane::union("docs", &parts, vec![Reads::Markdown])
        });
        let docs = self.docs_unit(inputs, delta, base.as_ref(), site);
        let mut all: Vec<Unit> = gates.into_iter().chain(imports).flatten().collect();
        all.extend(docs);
        all.extend(
            self.checks
                .iter()
                .filter(|c| !(self.site_in_docs() && c.kind == "site"))
                .filter_map(|c| self.check_unit(inputs, delta, c, base.as_ref(), site)),
        );
        Units { all }
    }

    /// The docs job, and the site it builds when there is one.
    fn docs_unit(
        &self,
        inputs: &Inputs,
        delta: Delta,
        base: Option<&Lane>,
        site: Option<&Site>,
    ) -> Option<Unit> {
        if !self.docs {
            return None;
        }
        let base = base?;
        let mut shown = vec![("docs".to_string(), "docs")];
        let mut reads = Vec::new();
        if self.site_in_docs() {
            let site = site.filter(|s| !s.hook)?;
            reads.clone_from(&site.reads);
            shown.push(("site".into(), "check"));
        }
        let lane = Lane::union("docs", &[base], reads);
        let saved = shown.iter().map(|(name, _)| name.clone()).collect();
        unit(inputs, delta, lane, "docs", saved, shown)
    }

    fn check_unit(
        &self,
        inputs: &Inputs,
        delta: Delta,
        check: &crate::ci::checks::Check,
        base: Option<&Lane>,
        site: Option<&Site>,
    ) -> Option<Unit> {
        let name = check.name();
        let id = format!("check:{name}");
        let lane = match (check.kind, &check.package) {
            ("site", _) => {
                let site = site.filter(|s| !s.hook)?;
                Lane::union(&id, &[base?], site.reads.clone())
            }
            ("capture", _) => Lane::union(&id, &[base?], Vec::new()),
            (_, Some(package)) => inputs.rust(&id, package)?,
            _ => return None,
        };
        let shown = vec![(name.clone(), "check")];
        unit(inputs, delta, lane, "check", vec![name], shown)
    }

    /// The results a gate job leaves for `package`: one per leg of the shard.
    fn gate_records(&self, package: &str) -> Vec<String> {
        let legs = self.shard_legs();
        let shards = legs["include"].as_array().into_iter().flatten();
        shards
            .map(|leg| match leg["shard"].as_str() {
                Some(shard) if !shard.is_empty() => format!("{package}#{shard}"),
                _ => package.to_string(),
            })
            .collect()
    }

    /// A result file in `dir` for each carried lane.
    pub(super) fn write_carried(&self, dir: &Path) -> Result<(), String> {
        for carried in &self.carried {
            for (name, kind) in &carried.shown {
                let out = dir.join(format!("carried-{name}.json"));
                write_carried(name.clone(), kind.to_string(), &carried.commit, &out)?;
            }
        }
        Ok(())
    }

    /// A baseline file in `dir` for each carried gate the docs job reads, which
    /// `baseline merge` takes beside the legs of this run.
    pub(super) fn write_baselines(&self, dir: &Path) -> Result<(), String> {
        for carried in &self.carried {
            let (Some(baseline), Some((kind, name))) =
                (&carried.baseline, carried.lane.split_once(':'))
            else {
                continue;
            };
            let mut doc = baseline.clone();
            if kind == "rust" {
                doc[CARRIED_KEY] = json!([name]);
            }
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = dir.join(format!("{kind}-{name}.json"));
            std::fs::write(&path, format!("{doc}\n"))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    }

    /// `lanes.json` in `dir`: each lane's key and the results that must pass
    /// for the default branch to store it.
    pub(super) fn write_lanes(&self, dir: &Path) -> Result<(), String> {
        let lanes: Vec<Value> = self
            .recorded
            .iter()
            .map(|r| json!({"lane": r.lane, "key": r.key, "records": r.records, "kind": r.kind}))
            .collect();
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("lanes.json");
        std::fs::write(&path, format!("{}\n", Value::Array(lanes)))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn unit(
    inputs: &Inputs,
    delta: Delta,
    lane: Lane,
    kind: &'static str,
    saved: Vec<String>,
    shown: Vec<Shown>,
) -> Option<Unit> {
    let key = key(&lane.id, &inputs.fingerprint(&lane), &delta(&lane)?);
    Some(Unit {
        key,
        lane,
        kind,
        saved,
        shown,
    })
}

mod coverage;

#[cfg(test)]
mod tests;
