//! The target runner cargo calls for each test binary in the run step.
//!
//! The run step is one `cargo test` invocation matching the build, with this
//! shim set as `target.<triple>.runner`. Cargo calls it as `<exe> [args...]`
//! with the test environment already in place. The plan says which
//! executables have a stored pass and where to record how the rest exited.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::runcache::hashed;

/// The variable naming the plan file.
pub const PLAN_ENV: &str = "SOOTHFAST_TEST_PLAN";
/// The subcommand cargo calls the shim as.
pub const SUBCOMMAND: &str = "__test-shim";

/// What the run step does with each executable.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Where exit statuses are recorded, one file per executable.
    pub results: PathBuf,
    /// The runner configured before soothfast's, called in its place.
    pub runner: Vec<String>,
    /// Keyed executables, by the path cargo reported for them.
    pub entries: BTreeMap<PathBuf, Entry>,
}

/// One keyed executable.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub label: String,
    pub cached: bool,
}

#[allow(dead_code)] // `write` and `status` are the run step's half of the plan.
impl Plan {
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let entries: serde_json::Map<String, Value> = self
            .entries
            .iter()
            .map(|(exe, e)| {
                let entry = json!({ "label": e.label, "cached": e.cached });
                (exe.to_string_lossy().into_owned(), entry)
            })
            .collect();
        let plan = json!({ "results": self.results, "runner": self.runner, "entries": entries });
        std::fs::write(path, plan.to_string())
    }

    pub fn read(path: &Path) -> io::Result<Plan> {
        let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let malformed = || io::Error::other(format!("malformed test plan {}", path.display()));
        let runner = v["runner"]
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .map(|a| a.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(malformed)?;
        let entries = v["entries"]
            .as_object()
            .ok_or_else(malformed)?
            .iter()
            .map(|(exe, e)| {
                let entry = Entry {
                    label: e["label"].as_str()?.to_string(),
                    cached: e["cached"].as_bool()?,
                };
                Some((PathBuf::from(exe), entry))
            })
            .collect::<Option<BTreeMap<_, _>>>()
            .ok_or_else(malformed)?;
        Ok(Plan {
            results: PathBuf::from(v["results"].as_str().ok_or_else(malformed)?),
            runner,
            entries,
        })
    }

    /// How `exe` exited, or `None` when it never finished.
    pub fn status(&self, exe: &Path) -> Option<i32> {
        std::fs::read_to_string(record_path(&self.results, exe))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn entry(&self, exe: &Path) -> Option<&Entry> {
        self.entries.get(exe).or_else(|| {
            let canonical = exe.canonicalize().ok()?;
            self.entries
                .iter()
                .find(|(path, _)| path.canonicalize().is_ok_and(|p| p == canonical))
                .map(|(_, e)| e)
        })
    }
}

fn record_path(results: &Path, exe: &Path) -> PathBuf {
    results.join(hashed(&[exe.to_string_lossy().into_owned()]))
}

/// Written to a temporary name and renamed, so a killed run leaves no record
/// rather than a partial one.
fn record(results: &Path, exe: &Path, code: i32) -> io::Result<()> {
    let path = record_path(results, exe);
    let partial = path.with_extension("partial");
    std::fs::write(&partial, code.to_string())?;
    std::fs::rename(partial, path)
}

/// Entry point for `cargo soothfast __test-shim <exe> [args...]`. Without a
/// readable plan every executable runs and nothing is recorded, which the run
/// step reads as unfinished.
pub fn run(args: &[String]) -> i32 {
    let plan = std::env::var_os(PLAN_ENV)
        .and_then(|p| Plan::read(Path::new(&p)).ok())
        .unwrap_or_default();
    run_with(&plan, args)
}

fn run_with(plan: &Plan, args: &[String]) -> i32 {
    let Some((exe, rest)) = args.split_first() else {
        eprintln!("soothfast: {SUBCOMMAND} needs an executable");
        return 2;
    };
    let exe = Path::new(exe);
    let entry = plan.entry(exe);
    if let Some(e) = entry.filter(|e| e.cached) {
        eprintln!("test: skip {} (cached pass)", e.label);
        return 0;
    }
    let code = match spawn(&plan.runner, exe, rest) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: cannot run {}: {e}", exe.display());
            return 101;
        }
    };
    if entry.is_some()
        && let Err(e) = record(&plan.results, exe, code)
    {
        eprintln!("soothfast: cannot record {}: {e}", exe.display());
    }
    code
}

/// A binary killed by a signal has no code and counts as a failure.
fn spawn(runner: &[String], exe: &Path, args: &[String]) -> io::Result<i32> {
    let mut cmd = match runner.split_first() {
        Some((program, lead)) => {
            let mut c = Command::new(program);
            c.args(lead).arg(exe);
            c
        }
        None => Command::new(exe),
    };
    Ok(cmd.args(args).status()?.code().unwrap_or(101))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            let dir =
                std::env::temp_dir().join(format!("soothfast-shim-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn plan(results: &Path, entries: &[(&str, bool)]) -> Plan {
        Plan {
            results: results.to_path_buf(),
            runner: Vec::new(),
            entries: entries
                .iter()
                .map(|(exe, cached)| {
                    let entry = Entry {
                        label: format!("p {exe} (test)"),
                        cached: *cached,
                    };
                    (PathBuf::from(exe), entry)
                })
                .collect(),
        }
    }

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_plan_reads_back_as_written() {
        let dir = Dir::new("roundtrip");
        let mut written = plan(&dir.0, &[("/t/a-1", true), ("/t/b-2", false)]);
        written.runner = argv(&["qemu-x86_64", "-L", "/sysroot"]);
        let file = dir.0.join("plan.json");
        written.write(&file).unwrap();
        assert_eq!(Plan::read(&file).unwrap(), written);
    }

    #[test]
    fn a_cached_executable_exits_zero_without_running() {
        let dir = Dir::new("cached");
        let plan = plan(&dir.0, &[("/nonexistent/test-1", true)]);
        assert_eq!(run_with(&plan, &argv(&["/nonexistent/test-1"])), 0);
        assert_eq!(plan.status(Path::new("/nonexistent/test-1")), None);
    }

    #[cfg(unix)]
    #[test]
    fn an_uncached_executable_runs_and_records_its_status() {
        let dir = Dir::new("uncached");
        let plan = plan(&dir.0, &[("/bin/sh", false)]);
        assert_eq!(run_with(&plan, &argv(&["/bin/sh", "-c", "exit 3"])), 3);
        assert_eq!(plan.status(Path::new("/bin/sh")), Some(3));
        assert_eq!(run_with(&plan, &argv(&["/bin/sh", "-c", "exit 0"])), 0);
        assert_eq!(plan.status(Path::new("/bin/sh")), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_outside_the_plan_runs_and_records_nothing() {
        let dir = Dir::new("unknown");
        let plan = plan(&dir.0, &[]);
        assert_eq!(run_with(&plan, &argv(&["/bin/sh", "-c", "exit 4"])), 4);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_configured_runner_is_called_with_the_executable() {
        let dir = Dir::new("runner");
        let mut plan = plan(&dir.0, &[("/bin/true", false)]);
        plan.runner = argv(&["/bin/sh", "-c", "[ \"$1\" = /bin/true ] && exit 7", "sh"]);
        assert_eq!(run_with(&plan, &argv(&["/bin/true"])), 7);
        assert_eq!(plan.status(Path::new("/bin/true")), Some(7));
    }

    #[test]
    fn a_partial_record_reads_as_unfinished() {
        let dir = Dir::new("partial");
        let plan = plan(&dir.0, &[("/t/a-1", false)]);
        let path = record_path(&dir.0, Path::new("/t/a-1")).with_extension("partial");
        std::fs::write(path, "0").unwrap();
        assert_eq!(plan.status(Path::new("/t/a-1")), None);
    }
}
