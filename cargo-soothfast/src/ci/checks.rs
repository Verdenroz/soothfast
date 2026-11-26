//! The check jobs a repository's configuration calls for, which `plan` turns
//! into the workflow's matrix.

use std::path::Path;

use serde_json::{Value, json};

use super::env::CiEnv;
use super::{capture, coverage, proto, site, spec};

type Run = fn(&CiEnv, &[String]) -> i32;

/// A per-package check kind: where its packages come from, and whether its
/// command reads rustdoc JSON and so needs the pinned nightly.
struct Kind {
    name: &'static str,
    rustdoc: bool,
    packages: fn(&Path) -> Result<Vec<String>, String>,
    run: Run,
}

const KINDS: &[Kind] = &[
    Kind {
        name: "spec",
        rustdoc: true,
        packages: spec::configured_at,
        run: spec::run,
    },
    Kind {
        name: "proto",
        rustdoc: false,
        packages: proto::configured_at,
        run: proto::run,
    },
    Kind {
        name: "coverage",
        rustdoc: true,
        packages: coverage::configured_at,
        run: coverage::run,
    },
    Kind {
        name: "capture",
        rustdoc: false,
        packages: capture::configured_at,
        run: capture::run,
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

    /// The `KIND[:PACKAGE]` token `ci checks` runs it by.
    pub fn token(&self) -> String {
        match &self.package {
            Some(package) => format!("{}:{package}", self.kind),
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

/// The matrix the workflow's check job runs over. An entry's `slug` names
/// its job, caches and artifacts, `runs` the tokens `ci checks` takes, and
/// `spec` the spec packages whose builds it stages.
pub fn matrix<'a>(checks: impl Iterator<Item = &'a Check>) -> Value {
    let include: Vec<Value> = checks
        .map(|c| {
            let spec = if c.kind == "spec" {
                c.package.clone()
            } else {
                None
            };
            json!({
                "slug": c.name(),
                "runs": c.token(),
                "spec": spec.unwrap_or_default(),
                "cache": c.kind,
                "rustdoc": c.rustdoc,
            })
        })
        .collect();
    json!({ "include": include })
}

/// The kinds `tokens` name, in order of first mention, each with its
/// packages.
fn grouped(tokens: &[String]) -> Result<Vec<(&str, Vec<&str>)>, String> {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for token in tokens {
        let (kind, package) = match token.split_once(':') {
            Some((kind, package)) => (kind, Some(package)),
            None => (token.as_str(), None),
        };
        if kind != SITE && !KINDS.iter().any(|k| k.name == kind) {
            return Err(format!("unknown check kind {kind:?}"));
        }
        let at = match groups.iter().position(|(k, _)| *k == kind) {
            Some(at) => at,
            None => {
                groups.push((kind, Vec::new()));
                groups.len() - 1
            }
        };
        groups[at].1.extend(package);
    }
    Ok(groups)
}

/// Entry point for `ci checks KIND[:PACKAGE]...`: runs each kind once over
/// its packages and returns the worst exit code.
pub fn run(env: &CiEnv, tokens: &[String]) -> i32 {
    let groups = match grouped(tokens) {
        Ok(groups) => groups,
        Err(e) => return crate::arg_err(&e),
    };
    groups
        .iter()
        .map(|(kind, packages)| {
            let run: Run = KINDS
                .iter()
                .find(|k| k.name == *kind)
                .map_or(site::run, |k| k.run);
            let args: Vec<String> = packages
                .iter()
                .flat_map(|p| ["-p".to_string(), p.to_string()])
                .collect();
            run(env, &args)
        })
        .max()
        .unwrap_or(0)
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
            matrix(checks.iter())["include"][0],
            json!({"slug": "spec-server", "runs": "spec:server", "spec": "server", "cache": "spec", "rustdoc": true})
        );
        assert_eq!(
            matrix(checks.iter())["include"][3],
            json!({"slug": "site", "runs": "site", "spec": "", "cache": "site", "rustdoc": true})
        );
    }

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tokens_group_by_kind_in_order() {
        let given = tokens(&["spec:a", "proto:w", "spec:b", "site"]);
        let groups = grouped(&given).unwrap();
        assert_eq!(
            groups,
            [
                ("spec", vec!["a", "b"]),
                ("proto", vec!["w"]),
                ("site", vec![])
            ]
        );
        assert!(grouped(&tokens(&["nope:a"])).is_err());
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
