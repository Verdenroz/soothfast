//! The `prebuild` command line.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::select::{Lane, SpecPackage, Wanted};
use crate::gate_config;
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

fn spec_package(arg: &str) -> SpecPackage {
    let (pkg, target) = arg.split_once(':').unwrap_or((arg, "soothfast"));
    SpecPackage {
        pkg: pkg.to_string(),
        target: target.to_string(),
    }
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
        features: None,
        docs_features: None,
        docs_paths: Vec::new(),
        against_ref: None,
        reuse_base: true,
        stage: None,
        shards: 1,
    };
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => wanted.packages.push(take(&mut it, a)?),
            "--docs" => wanted.docs.push(take(&mut it, a)?),
            "--docs-features" => wanted.docs_features = Some(take(&mut it, a)?),
            "--docs-paths" => {
                while let Some(path) = it.next_if(|p| !p.starts_with('-')) {
                    wanted.docs_paths.push(PathBuf::from(path));
                }
            }
            "--spec" => wanted.spec.push(spec_package(&take(&mut it, a)?)),
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
    gate_config::apply(&mut common)?;
    wanted.features = common.features.clone();
    wanted.lanes = if lanes.is_empty() {
        BTreeSet::from([Lane::Bench, Lane::Routes, Lane::Rustdoc])
    } else {
        lanes
    };
    Ok(Args { common, wanted })
}
