//! `coverage executed`: the public items tests and benches run.

use std::collections::BTreeSet;

use serde_json::json;

use super::regions::{self, Declared};
use crate::config::{Config, Group};
use crate::coverage_report::executed::{Want, Wants, gather};
use crate::invoke::{self, CommonArgs};

/// Items are `tested` and `measured`; a column that was not run is absent.
pub fn run(args: &[String]) -> i32 {
    match execute(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

struct Flags {
    /// The bench build: `--bench`, and `--bench-features`.
    common: CommonArgs,
    /// The features of the surface and the test build.
    features: Option<String>,
    tests: bool,
    benches: bool,
    json: bool,
}

fn parse(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        common: CommonArgs::default(),
        features: None,
        tests: false,
        benches: false,
        json: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tests" => flags.tests = true,
            "--benches" => flags.benches = true,
            "--json" => flags.json = true,
            "--features" => flags.features = it.next().cloned(),
            "--bench-features" => flags.common.features = it.next().cloned(),
            other if flags.common.try_parse(other, &mut it) => {}
            other => return Err(format!("unknown coverage executed arg {other:?}")),
        }
    }
    if !flags.tests && !flags.benches {
        flags.tests = true;
        flags.benches = true;
    }
    Ok(flags)
}

/// Fill the flags the way `coverage report` resolves them, so a run stored
/// here is the one the report looks up: the surface and tests take the
/// coverage features, the bench build the gate's.
fn resolve(flags: &mut Flags, config: &Config) -> Result<(), String> {
    let pkgs: Vec<String> = flags.common.pkg.iter().cloned().collect();
    if flags.features.is_none() {
        flags.features = config.resolve_coverage(&pkgs)?.features;
    }
    config
        .fill(&mut flags.common, Group::Gate, &pkgs)
        .map(|_| ())
}

fn execute(args: &[String]) -> Result<(), String> {
    let mut flags = parse(args)?;
    resolve(&mut flags, &Config::load(&invoke::project_root())?)?;
    let pkg = flags
        .common
        .pkg
        .as_deref()
        .ok_or("coverage executed requires -p PKG")?;
    let features = flags.features.as_deref();
    let (surface, _, root) = crate::docs_support::surface_in(pkg, None, features)?;
    let want = |on: bool| if on { Want::Run } else { Want::Skip };
    let wants = Wants {
        tests: want(flags.tests),
        benches: want(flags.benches),
    };
    let executed = gather(pkg, &surface, &root, features, &flags.common, wants)?;
    let declared = regions::declared(&surface);
    let columns = Columns {
        tested: executed.tested.as_ref(),
        measured: executed.measured.as_ref(),
    };
    if flags.json {
        println!("{}", columns.json(pkg, &declared));
    } else {
        columns.print(&declared);
    }
    Ok(())
}

struct Columns<'a> {
    tested: Option<&'a BTreeSet<String>>,
    measured: Option<&'a BTreeSet<String>>,
}

impl Columns<'_> {
    fn json(&self, pkg: &str, declared: &[Declared<'_>]) -> serde_json::Value {
        let items: Vec<_> = declared
            .iter()
            .map(|d| {
                let mut item = json!({ "path": d.path, "kind": d.kind });
                for (name, set) in [("tested", self.tested), ("measured", self.measured)] {
                    if let Some(set) = set {
                        item[name] = json!(set.contains(&d.path));
                    }
                }
                item
            })
            .collect();
        json!({ "package": pkg, "items": items })
    }

    fn print(&self, declared: &[Declared<'_>]) {
        let mark = |set: Option<&BTreeSet<String>>, path: &str, word: &str| match set {
            Some(set) if set.contains(path) => word.to_string(),
            Some(_) => " ".repeat(word.len()),
            None => String::new(),
        };
        for d in declared {
            let tested = mark(self.tested, &d.path, "tested");
            let measured = mark(self.measured, &d.path, "measured");
            println!("{tested} {measured} {} ({})", d.path, d.kind);
        }
        for (name, set) in [("tested", self.tested), ("measured", self.measured)] {
            if let Some(set) = set {
                println!(
                    "coverage executed: {}/{} public item(s) {name}",
                    set.len(),
                    declared.len()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> Config {
        let dir = std::env::temp_dir().join(format!(
            "soothfast-cov-cli-{}-{}",
            std::process::id(),
            toml.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("soothfast.toml"), toml).unwrap();
        let config = Config::load(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        config
    }

    fn flags(args: &[&str]) -> Result<Flags, String> {
        parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn no_column_flag_runs_both() {
        let f = flags(&["-p", "demo"]).unwrap();
        assert!(f.tests && f.benches);
        assert_eq!(f.common.pkg.as_deref(), Some("demo"));
    }

    #[test]
    fn a_column_flag_runs_only_that_column() {
        let f = flags(&["-p", "demo", "--benches", "--json"]).unwrap();
        assert!(!f.tests && f.benches && f.json);
    }

    #[test]
    fn the_configured_features_fill_what_a_flag_leaves_unset() {
        let config = config(
            "[package.demo.gate]\nfeatures = \"bench-gate\"\n[package.demo.docs]\nfeatures = \"full\"\n",
        );
        let mut bare = flags(&["-p", "demo"]).unwrap();
        resolve(&mut bare, &config).unwrap();
        assert_eq!(bare.features.as_deref(), Some("full"));
        assert_eq!(bare.common.features.as_deref(), Some("bench-gate"));
        let mut given =
            flags(&["-p", "demo", "--features", "lite", "--bench-features", "b"]).unwrap();
        resolve(&mut given, &config).unwrap();
        assert_eq!(given.features.as_deref(), Some("lite"));
        assert_eq!(given.common.features.as_deref(), Some("b"));
    }

    #[test]
    fn a_features_flag_leaves_the_bench_build_to_the_gate() {
        let config = config("[package.demo.gate]\nfeatures = \"bench-gate\"\n");
        let mut f = flags(&["-p", "demo", "--features", "lite"]).unwrap();
        resolve(&mut f, &config).unwrap();
        assert_eq!(f.features.as_deref(), Some("lite"));
        assert_eq!(f.common.features.as_deref(), Some("bench-gate"));
    }

    #[test]
    fn an_unknown_flag_is_refused() {
        assert!(flags(&["--nope"]).is_err());
    }

    #[test]
    fn json_omits_a_column_that_did_not_run() {
        let declared = [Declared {
            path: "demo::run".into(),
            kind: "function",
            spans: &[],
        }];
        let tested = BTreeSet::from(["demo::run".to_string()]);
        let columns = Columns {
            tested: Some(&tested),
            measured: None,
        };
        assert_eq!(
            columns.json("demo", &declared),
            json!({ "package": "demo", "items": [
                { "path": "demo::run", "kind": "function", "tested": true }
            ] })
        );
    }
}
