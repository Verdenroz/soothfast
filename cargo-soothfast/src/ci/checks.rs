//! The check jobs a repository's configuration calls for, which `plan` turns
//! into the workflow's matrix.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::env::CiEnv;
use super::{capture, coverage, proto, site, spec};
use crate::lanes::graph::Graph;

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
/// `spec` the spec packages whose builds it stages. A group of spec checks
/// whose packages share workspace crates is one entry, which falls back to
/// the cache of its first member's own job through `legacy`; the other kinds
/// share one. A check whose kind or record name is in `separate` keeps an
/// entry of its own.
pub fn matrix<'a>(
    checks: impl Iterator<Item = &'a Check>,
    graph: Option<&Graph>,
    separate: &[String],
) -> Value {
    let checks: Vec<&Check> = checks.collect();
    let alone = |c: &Check| separate.iter().any(|s| s == c.kind || *s == c.name());
    let specs: Vec<&Check> = checks
        .iter()
        .copied()
        .filter(|c| c.kind == "spec" && !alone(c))
        .collect();
    let others: Vec<&Check> = checks
        .iter()
        .copied()
        .filter(|c| c.kind != "spec" && !alone(c))
        .collect();
    let groups = spec_groups(&specs, graph);
    let include: Vec<Value> = checks
        .iter()
        .filter_map(|c| {
            if alone(c) {
                return Some(single(c));
            }
            if c.kind == "spec" {
                let group = groups.iter().find(|g| std::ptr::eq(g[0], *c))?;
                return Some(spec_entry(group, groups.len() == 1));
            }
            std::ptr::eq(others[0], *c).then(|| match others.as_slice() {
                [only] => single(only),
                _ => entry("checks".into(), String::new(), &others),
            })
        })
        .collect();
    json!({ "include": include })
}

fn single(check: &Check) -> Value {
    entry(check.name(), String::new(), &[check])
}

fn entry(slug: String, legacy: String, members: &[&Check]) -> Value {
    let runs: Vec<String> = members.iter().map(|c| c.token()).collect();
    let spec: Vec<&str> = members
        .iter()
        .filter(|c| c.kind == "spec")
        .filter_map(|c| c.package.as_deref())
        .collect();
    json!({
        "slug": slug,
        "runs": runs.join(" "),
        "spec": spec.join(" "),
        "legacy": legacy,
        "rustdoc": members.iter().any(|c| c.rustdoc),
    })
}

fn spec_entry(group: &[&Check], sole: bool) -> Value {
    let first = group[0];
    if group.len() == 1 {
        return single(first);
    }
    let slug = if sole {
        "spec-shared".to_string()
    } else {
        format!("{}-group", first.name())
    };
    entry(slug, first.name(), group)
}

/// The spec checks split into groups whose packages reach no workspace crate
/// in common with another group, each in the order its first member came.
/// Without a graph every check is one group.
fn spec_groups<'a>(specs: &[&'a Check], graph: Option<&Graph>) -> Vec<Vec<&'a Check>> {
    if specs.is_empty() {
        return Vec::new();
    }
    let Some(graph) = graph else {
        return vec![specs.to_vec()];
    };
    let reach: Vec<_> = specs
        .iter()
        .map(|c| graph.closure(c.package.as_deref().unwrap_or_default()))
        .collect();
    let mut parent: Vec<usize> = (0..specs.len()).collect();
    for i in 0..specs.len() {
        for j in i + 1..specs.len() {
            if !reach[i].is_disjoint(&reach[j]) {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<&Check>> = BTreeMap::new();
    for (i, check) in specs.iter().enumerate() {
        groups.entry(root(&mut parent, i)).or_default().push(check);
    }
    groups.into_values().collect()
}

fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
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
            matrix(checks.iter(), None, &[])["include"][0],
            json!({"slug": "spec-server", "runs": "spec:server", "spec": "server", "legacy": "", "rustdoc": true})
        );
        assert_eq!(
            matrix(checks.iter(), None, &[])["include"][1],
            json!({"slug": "checks", "runs": "coverage:core coverage:server site", "spec": "", "legacy": "", "rustdoc": true})
        );
        assert_eq!(matrix(checks.iter(), None, &[])["include"][2], Value::Null);
    }

    #[test]
    fn a_lone_small_check_keeps_its_own_entry() {
        let checks = collect(true, &["coverage".into()], found, || Ok(true)).unwrap();
        assert_eq!(
            slugs(&matrix(checks.iter(), None, &[])),
            ["spec-server", "site"]
        );
    }

    #[test]
    fn separate_names_a_kind_or_a_record_to_keep_apart() {
        let checks = collect(true, &[], found, || Ok(true)).unwrap();
        let by_kind = matrix(checks.iter(), None, &tokens(&["coverage", "spec"]));
        assert_eq!(
            slugs(&by_kind),
            ["spec-server", "coverage-core", "coverage-server", "site"]
        );
        let by_record = matrix(checks.iter(), None, &tokens(&["coverage-core"]));
        assert_eq!(
            slugs(&by_record),
            ["spec-server", "coverage-core", "checks"]
        );
        assert_eq!(by_record["include"][2]["runs"], "coverage:server site");
    }

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn workspace(members: &[(&str, &[&str])]) -> Graph {
        let packages: Vec<Value> = members
            .iter()
            .map(|(name, deps)| {
                let deps: Vec<Value> = deps.iter().map(|d| json!({ "name": d })).collect();
                json!({
                    "name": name,
                    "manifest_path": format!("/ws/{name}/Cargo.toml"),
                    "dependencies": deps,
                })
            })
            .collect();
        Graph::from_metadata(&json!({ "packages": packages }), Path::new("/ws")).unwrap()
    }

    fn spec(package: &str) -> Check {
        Check {
            kind: "spec",
            package: Some(package.into()),
            rustdoc: true,
        }
    }

    fn slugs(matrix: &Value) -> Vec<&str> {
        let entries = matrix["include"].as_array().unwrap();
        entries
            .iter()
            .map(|e| e["slug"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn spec_packages_sharing_a_crate_build_in_one_entry() {
        let graph = workspace(&[
            ("api", &["core"]),
            ("admin", &["core"]),
            ("core", &[]),
            ("lone", &[]),
        ]);
        let checks = [spec("api"), spec("admin")];
        let m = matrix(checks.iter(), Some(&graph), &[]);
        assert_eq!(slugs(&m), ["spec-shared"]);
        let shared = &m["include"][0];
        assert_eq!(shared["runs"], "spec:api spec:admin");
        assert_eq!(shared["spec"], "api admin");
        assert_eq!(shared["legacy"], "spec-api");
    }

    #[test]
    fn disjoint_spec_packages_keep_their_own_entries_beside_a_group() {
        let graph = workspace(&[
            ("api", &["core"]),
            ("admin", &["core"]),
            ("core", &[]),
            ("lone", &[]),
        ]);
        let checks = [spec("api"), spec("lone"), spec("admin")];
        let m = matrix(checks.iter(), Some(&graph), &[]);
        assert_eq!(slugs(&m), ["spec-api-group", "spec-lone"]);
    }

    #[test]
    fn without_a_graph_every_spec_check_is_one_group() {
        let checks = [spec("a"), spec("b")];
        assert_eq!(slugs(&matrix(checks.iter(), None, &[])), ["spec-shared"]);
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
