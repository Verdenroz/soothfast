//! The `prebuild` command line.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::select::{Lane, SpecPackage, Wanted};
use crate::config::{self, Group};
use crate::invoke::CommonArgs;

pub(super) struct Args {
    pub(super) common: CommonArgs,
    pub(super) wanted: Wanted,
}

pub(super) fn take<'a>(
    it: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<String, String> {
    it.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

/// `PKG[:TARGET]`. What the argument and `--features` leave unset comes from
/// the package's `spec` defaults in `soothfast.toml`.
fn spec_package(arg: &str, features: Option<&str>) -> Result<SpecPackage, String> {
    let (pkg, target) = match arg.split_once(':') {
        Some((pkg, target)) => (pkg, Some(target)),
        None => (arg, None),
    };
    let defaults = config::defaults(&[pkg.to_string()], Group::Spec)?;
    Ok(SpecPackage {
        pkg: pkg.to_string(),
        target: target
            .map(String::from)
            .or(defaults.target)
            .unwrap_or_else(|| "soothfast".into()),
        features: features.map(String::from).or(defaults.features),
    })
}

fn lane(name: &str) -> Result<Lane, String> {
    match name {
        "bench" => Ok(Lane::Bench),
        "routes" => Ok(Lane::Routes),
        "rustdoc" => Ok(Lane::Rustdoc),
        other => Err(format!(
            "--only wants bench, routes or rustdoc, got {other:?}"
        )),
    }
}

pub(super) fn parse(args: &[String]) -> Result<Args, String> {
    let mut common = CommonArgs::default();
    let mut lanes = BTreeSet::new();
    let mut wanted = Wanted {
        lanes: BTreeSet::new(),
        packages: Vec::new(),
        docs: Vec::new(),
        spec: Vec::new(),
        docs_features: None,
        docs_defaults: BTreeMap::new(),
        docs_paths: Vec::new(),
        against_ref: None,
        reuse_base: true,
        stage: None,
        shards: 1,
    };
    let mut spec = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => wanted.packages.push(take(&mut it, a)?),
            "--docs" => wanted.docs.push(take(&mut it, a)?),
            "--docs-features" => wanted.docs_features = Some(take(&mut it, a)?),
            "--docs-paths" => {
                let given = wanted.docs_paths.len();
                while let Some(path) = it.next_if(|p| !p.starts_with('-')) {
                    wanted.docs_paths.push(PathBuf::from(path));
                }
                if wanted.docs_paths.len() == given {
                    return Err(format!("{a} needs a value"));
                }
            }
            "--spec" => spec.push(take(&mut it, a)?),
            "--against-ref" => wanted.against_ref = Some(take(&mut it, a)?),
            "--no-reuse-base" => wanted.reuse_base = false,
            "--stage" => wanted.stage = Some(PathBuf::from(take(&mut it, a)?)),
            "--shards" => {
                wanted.shards = take(&mut it, a)?
                    .parse()
                    .map_err(|_| "--shards wants a count".to_string())?
            }
            "--only" => {
                lanes.insert(lane(&take(&mut it, a)?)?);
            }
            _ => {
                if !common.try_parse(a, &mut it) {
                    return Err(format!("unknown prebuild arg {a:?}"));
                }
            }
        }
    }
    for arg in &spec {
        wanted
            .spec
            .push(spec_package(arg, common.features.as_deref())?);
    }
    if wanted.docs_features.is_none() {
        for pkg in &wanted.docs {
            let defaults = config::defaults(std::slice::from_ref(pkg), Group::Docs)?;
            wanted
                .docs_defaults
                .extend(defaults.features.map(|f| (pkg.clone(), f)));
        }
    }
    config::apply_to(&mut common, Group::Gate, &wanted.packages)?;
    wanted.lanes = if lanes.is_empty() {
        BTreeSet::from([Lane::Bench, Lane::Routes, Lane::Rustdoc])
    } else {
        lanes
    };
    Ok(Args { common, wanted })
}
