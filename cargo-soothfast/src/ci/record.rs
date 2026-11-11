//! Runs a job's command and records its verdict.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use super::env::CiEnv;
use crate::verdict;

/// Where the job `name` leaves its record under `dir`.
pub fn result_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.json", file_stem(name)))
}

/// Where the job `name`'s output is saved under `dir`.
pub fn log_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.log", file_stem(name)))
}

/// A shard's name (`pkg#1/2`) holds a slash, which must not become a path.
fn file_stem(name: &str) -> String {
    name.replace('/', "-")
}

/// Runs `cmd` and returns its exit code. With a results directory the output
/// is also saved, and a record named `name` of kind `kind` is written unless
/// the command wrote one at [`result_path`]. Without one nothing is saved.
pub fn run_recorded(env: &CiEnv, name: &str, kind: &str, cmd: &mut Command) -> i32 {
    let Some(dir) = &env.results else {
        return exit_code(cmd.status());
    };
    let started = Instant::now();
    let log = log_path(dir, name);
    let result = result_path(dir, name);
    if let Err(e) = fs::create_dir_all(dir) {
        eprintln!("soothfast: {}: {e}", dir.display());
        return 2;
    }
    let _ = fs::remove_file(&result);
    let code = tee(cmd, &log);
    if fs::metadata(&result).is_ok_and(|m| m.len() > 0) {
        return code;
    }
    let seconds = started.elapsed().as_secs();
    if let Err(e) = verdict::write_record(name.into(), kind.into(), code, &log, seconds, &result) {
        eprintln!("soothfast: {e}");
        return 2;
    }
    code
}

fn exit_code(status: std::io::Result<std::process::ExitStatus>) -> i32 {
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

/// Streams the command's stdout and stderr to ours and into `log`.
fn tee(cmd: &mut Command, log: &Path) -> i32 {
    let file = match File::create(log) {
        Ok(f) => Mutex::new(f),
        Err(e) => {
            eprintln!("soothfast: {}: {e}", log.display());
            return 2;
        }
    };
    let child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let (out, err) = (child.stdout.take(), child.stderr.take());
    std::thread::scope(|s| {
        if let Some(out) = out {
            s.spawn(|| pump(out, std::io::stdout(), &file));
        }
        if let Some(err) = err {
            s.spawn(|| pump(err, std::io::stderr(), &file));
        }
    });
    exit_code(child.wait())
}

fn pump(mut from: impl Read, mut to: impl Write, log: &Mutex<File>) {
    let mut buf = [0u8; 8192];
    while let Ok(n) = from.read(&mut buf) {
        if n == 0 {
            break;
        }
        let _ = to.write_all(&buf[..n]).and_then(|()| to.flush());
        if let Ok(mut log) = log.lock() {
            let _ = log.write_all(&buf[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn ci_env(results: Option<&Path>) -> CiEnv {
        let flags = Flags {
            results: results.map(Path::to_path_buf),
            ..Flags::default()
        };
        CiEnv::parse(&BTreeMap::new(), None, &flags)
    }

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("soothfast-ci-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn record(dir: &Path, name: &str) -> Value {
        let text = fs::read_to_string(result_path(dir, name)).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn records_when_a_results_directory_is_set() {
        let dir = scratch("records");
        let env = ci_env(Some(&dir));
        let code = run_recorded(&env, "job", "test", &mut sh("echo out; echo err >&2"));
        assert_eq!(code, 0);
        let rec = record(&dir, "job");
        assert_eq!(rec["name"], "job");
        assert_eq!(rec["kind"], "test");
        assert_eq!(rec["verdict"], "pass");
        let log = fs::read_to_string(dir.join("job.log")).unwrap();
        assert!(log.contains("out") && log.contains("err"), "{log}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn records_nothing_without_a_results_directory() {
        let dir = scratch("none");
        let code = run_recorded(&ci_env(None), "job", "test", &mut sh("exit 0"));
        assert_eq!(code, 0);
        assert!(!dir.exists());
    }

    #[test]
    fn the_exit_code_is_the_commands_own() {
        let dir = scratch("exit");
        let env = ci_env(Some(&dir));
        assert_eq!(
            run_recorded(&env, "job", "test", &mut sh("echo boom; exit 7")),
            7
        );
        let rec = record(&dir, "job");
        assert_eq!(rec["verdict"], "failed");
        assert!(rec["output"].as_str().unwrap().contains("boom"));
        assert_eq!(
            run_recorded(&ci_env(None), "job", "test", &mut sh("exit 5")),
            5
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_shard_name_stays_in_the_results_directory() {
        let dir = scratch("shard");
        let env = ci_env(Some(&dir));
        run_recorded(&env, "core#1/2", "rust", &mut sh("echo hi"));
        assert_eq!(record(&dir, "core#1/2")["name"], "core#1/2");
        assert!(log_path(&dir, "core#1/2").is_file());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_record_the_command_wrote_is_kept() {
        let dir = scratch("kept");
        let env = ci_env(Some(&dir));
        let script = format!(
            "echo '{{\"own\": true}}' > {}",
            result_path(&dir, "job").display()
        );
        run_recorded(&env, "job", "test", &mut sh(&script));
        assert_eq!(record(&dir, "job")["own"], true);
        fs::remove_dir_all(dir).unwrap();
    }
}
