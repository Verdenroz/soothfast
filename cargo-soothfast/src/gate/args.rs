//! Parsed `gate` and `gate accept` arguments, and helpers that read raw argv.

use crate::invoke::{self, CommonArgs};

pub(super) struct GateArgs {
    pub(super) common: CommonArgs,
    pub(super) baseline: String,
    pub(super) ratchet: Option<String>,
    pub(super) against_ref: Option<String>,
    pub(super) deps: bool,
    pub(super) allow_gone: bool,
    pub(super) allow_harness_change: bool,
    pub(super) matrix: String,
    pub(super) save_baseline: Option<String>,
    pub(super) reuse_base: bool,
    /// Whether a crate the merge-base has no bench target for is measured;
    /// `gate accept` has nothing to accept for one.
    pub(super) measure_new: bool,
    pub(super) import: ImportArgs,
}

/// `--import`: gate result files from another harness instead of measuring.
/// Each side is a `(format, file)` pair.
#[derive(Default)]
pub(super) struct ImportArgs {
    pub(super) current: Option<(String, String)>,
    pub(super) reference: Option<(String, String)>,
    pub(super) prefix: Option<String>,
    pub(super) walltime: Option<String>,
    pub(super) baseline_given: bool,
}

pub(super) fn against_ref_in(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--against-ref" {
            return it.next().cloned();
        }
    }
    None
}

/// Packages named by repeated `-p`, in the order given.
pub(super) fn packages_in(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if matches!(a.as_str(), "-p" | "--package")
            && let Some(p) = it.next()
        {
            out.push(p.clone());
        }
    }
    out
}

/// `args` with every `-p` collapsed to the single named package.
pub(super) fn with_package(args: &[String], pkg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if matches!(a.as_str(), "-p" | "--package") {
            it.next();
            continue;
        }
        out.push(a.clone());
    }
    out.push("-p".into());
    out.push(pkg.into());
    out
}

pub(super) struct AcceptArgs {
    pub(super) common: CommonArgs,
    pub(super) baseline: Option<String>,
    pub(super) against_ref: Option<String>,
    pub(super) matrix: String,
    pub(super) justification: Option<String>,
    pub(super) only: Vec<String>,
    pub(super) headroom: Option<f64>,
}

/// `gate accept`'s arguments.
pub(super) fn accept_args(args: &[String]) -> Result<AcceptArgs, String> {
    let mut a = AcceptArgs {
        common: CommonArgs::default(),
        baseline: None,
        against_ref: None,
        matrix: "default".into(),
        justification: None,
        only: Vec::new(),
        headroom: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if a.common.try_parse(arg, &mut it) {
            continue;
        }
        match arg.as_str() {
            "--baseline" => match it.next() {
                Some(n) => a.baseline = Some(n.clone()),
                None => return Err("--baseline needs a name".into()),
            },
            "--against-ref" => match it.next() {
                Some(r) => a.against_ref = Some(r.clone()),
                None => return Err("--against-ref needs a git ref".into()),
            },
            "--features-matrix" => match it.next() {
                Some(m) => a.matrix = m.clone(),
                None => return Err("--features-matrix needs combos like \"default;full\"".into()),
            },
            "--justification" => match it.next() {
                Some(j) => a.justification = Some(j.clone()),
                None => return Err("--justification needs text".into()),
            },
            "--only" => match it.next() {
                Some(id) => a.only.push(id.clone()),
                None => return Err("--only needs a bench id".into()),
            },
            "--headroom" => match it.next().map(|v| v.parse::<f64>()) {
                Some(Ok(pct)) => a.headroom = Some(pct),
                _ => return Err("--headroom needs a percentage number".into()),
            },
            "--shard" => match it.next().map(|s| invoke::Shard::parse(s)) {
                Some(Ok(shard)) => a.common.shard = Some(shard),
                Some(Err(e)) => return Err(e),
                None => return Err("--shard needs I/N".into()),
            },
            other => return Err(format!("unknown gate accept arg {other:?}")),
        }
    }
    Ok(a)
}
