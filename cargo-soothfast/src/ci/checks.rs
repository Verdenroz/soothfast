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

/// The checks this repository runs, leaving out the kinds `skip` names. The
/// per-package kinds need a cargo workspace.
pub fn planned(root: &Path, cargo: bool, skip: &[String]) -> Result<Vec<Check>, String> {
    collect(
        cargo,
        skip,
        |kind| (kind.packages)(root),
        || Ok(site::configured_at(root)),
    )
}

fn collect(
    cargo: bool,
    skip: &[String],
    packages: impl Fn(&Kind) -> Result<Vec<String>, String>,
    site: impl FnOnce() -> Result<bool, String>,
) -> Result<Vec<Check>, String> {
    let runs = |name: &str| !skip.iter().any(|s| s == name);
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
pub fn matrix(checks: &[Check]) -> Value {
    let include: Vec<Value> = checks
        .iter()
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
            _ => Vec::new(),
        })
    }

    fn names(checks: &[Check]) -> Vec<String> {
        checks.iter().map(Check::name).collect()
    }

    #[test]
    fn each_configured_package_and_the_site_get_a_job() {
        let checks = collect(true, &[], found, || Ok(true)).unwrap();
        assert_eq!(
            names(&checks),
            ["spec-server", "coverage-core", "coverage-server", "site"]
        );
        assert_eq!(
            matrix(&checks)["include"][0],
            json!({"name": "spec-server", "kind": "spec", "package": "server", "rustdoc": true})
        );
        assert_eq!(matrix(&checks)["include"][3]["package"], "");
    }

    #[test]
    fn a_skipped_kind_gets_none() {
        let skip = ["coverage".to_string(), "site".to_string()];
        let checks = collect(true, &skip, found, || Ok(true)).unwrap();
        assert_eq!(names(&checks), ["spec-server"]);
    }

    #[test]
    fn without_a_cargo_workspace_only_the_site_is_checked() {
        let checks = collect(false, &[], found, || Ok(true)).unwrap();
        assert_eq!(names(&checks), ["site"]);
        assert!(collect(false, &[], found, || Ok(false)).unwrap().is_empty());
    }

    #[test]
    fn a_lookup_error_is_reported() {
        let failing = |_: &Kind| Err("bad table".to_string());
        assert_eq!(
            collect(true, &[], failing, || Ok(false)),
            Err("bad table".into())
        );
    }
}
