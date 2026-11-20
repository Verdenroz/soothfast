//! The check jobs a repository's configuration calls for, which `plan` turns
//! into the workflow's matrix.

use std::path::Path;

use serde_json::{Value, json};

use super::{capture, coverage, proto, site, spec};

/// A per-package check kind: where its packages come from, and whether its
/// command reads rustdoc JSON and so needs the pinned nightly.
struct Kind {
    name: &'static str,
    rustdoc: bool,
    packages: fn(&Path) -> Result<Vec<String>, String>,
}

const KINDS: &[Kind] = &[
    Kind {
        name: "spec",
        rustdoc: true,
        packages: spec::configured_at,
    },
    Kind {
        name: "proto",
        rustdoc: false,
        packages: proto::configured_at,
    },
    Kind {
        name: "coverage",
        rustdoc: true,
        packages: coverage::configured_at,
    },
    Kind {
        name: "capture",
        rustdoc: false,
        packages: capture::configured_at,
    },
];

const SITE: &str = "site";

/// One job: `cargo soothfast ci KIND [-p PACKAGE]`.
#[derive(Debug, PartialEq)]
pub struct Check {
    pub kind: &'static str,
    pub package: Option<String>,
    pub rustdoc: bool,
}

impl Check {
    /// The name of the record the job leaves, which the verdict expects.
    pub fn name(&self) -> String {
        match &self.package {
            Some(package) => format!("{}-{package}", self.kind),
            None => self.kind.to_string(),
        }
    }
}

/// The checks this repository runs, leaving out the kinds `skip` names, and
/// capture when the default branch regenerates it after merge. The
/// per-package kinds need a cargo workspace.
pub fn planned(
    root: &Path,
    cargo: bool,
    skip: &[String],
    capture_regenerated: bool,
) -> Result<Vec<Check>, String> {
    collect(
        cargo,
        skip,
        capture_regenerated,
        |kind| (kind.packages)(root),
        || Ok(site::configured_at(root)),
    )
}

fn collect(
    cargo: bool,
    skip: &[String],
    capture_regenerated: bool,
    packages: impl Fn(&Kind) -> Result<Vec<String>, String>,
    site: impl FnOnce() -> Result<bool, String>,
) -> Result<Vec<Check>, String> {
    let runs =
        |name: &str| !skip.iter().any(|s| s == name) && !(capture_regenerated && name == "capture");
    let mut checks = Vec::new();
    for kind in KINDS.iter().filter(|k| cargo && runs(k.name)) {
        checks.extend(packages(kind)?.into_iter().map(|package| Check {
            kind: kind.name,
            package: Some(package),
            rustdoc: kind.rustdoc,
        }));
    }
    if runs(SITE) && site()? {
        checks.push(Check {
            kind: SITE,
            package: None,
            rustdoc: true,
        });
    }
    Ok(checks)
}

/// The matrix the workflow's check job runs over.
pub fn matrix<'a>(checks: impl Iterator<Item = &'a Check>) -> Value {
    let include: Vec<Value> = checks
        .map(|c| {
            json!({
                "name": c.name(),
                "kind": c.kind,
                "package": c.package.clone().unwrap_or_default(),
                "rustdoc": c.rustdoc,
            })
        })
        .collect();
    json!({ "include": include })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(kind: &Kind) -> Result<Vec<String>, String> {
        Ok(match kind.name {
            "spec" => vec!["server".into()],
            "coverage" => vec!["core".into(), "server".into()],
            "capture" => vec!["docs".into()],
            _ => Vec::new(),
        })
    }

    fn names(checks: &[Check]) -> Vec<String> {
        checks.iter().map(Check::name).collect()
    }

    #[test]
    fn each_configured_package_and_the_site_get_a_job() {
        let checks = collect(true, &[], false, found, || Ok(true)).unwrap();
        assert_eq!(
            names(&checks),
            [
                "spec-server",
                "coverage-core",
                "coverage-server",
                "capture-docs",
                "site"
            ]
        );
        assert_eq!(
            matrix(checks.iter())["include"][0],
            json!({"name": "spec-server", "kind": "spec", "package": "server", "rustdoc": true})
        );
        assert_eq!(matrix(checks.iter())["include"][4]["package"], "");
    }

    #[test]
    fn a_skipped_kind_gets_none() {
        let skip = ["coverage".to_string(), "site".to_string()];
        let checks = collect(true, &skip, false, found, || Ok(true)).unwrap();
        assert_eq!(names(&checks), ["spec-server", "capture-docs"]);
    }

    #[test]
    fn capture_is_not_planned_when_the_bot_regenerates_it() {
        let skip = ["coverage".to_string()];
        let on = collect(true, &skip, true, found, || Ok(true)).unwrap();
        assert_eq!(names(&on), ["spec-server", "site"]);
        let off = collect(true, &skip, false, found, || Ok(true)).unwrap();
        assert_eq!(names(&off), ["spec-server", "capture-docs", "site"]);
    }

    #[test]
    fn without_a_cargo_workspace_only_the_site_is_checked() {
        let checks = collect(false, &[], false, found, || Ok(true)).unwrap();
        assert_eq!(names(&checks), ["site"]);
        assert!(
            collect(false, &[], false, found, || Ok(false))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_lookup_error_is_reported() {
        let failing = |_: &Kind| Err("bad table".to_string());
        assert_eq!(
            collect(true, &[], false, failing, || Ok(false)),
            Err("bad table".into())
        );
    }
}
