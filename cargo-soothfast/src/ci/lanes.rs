//! `ci lanes save`: stores the lanes a default-branch run passed, for the
//! pull requests after it to carry.

use std::path::PathBuf;

use super::docs::baseline_files;
use super::env::CiEnv;
use crate::lanes::save::{Baseline, save};
use crate::lanes::store::{Pass, Store};
use crate::verdict::plain_passes;

/// Entry point for `ci lanes`: `save --lanes DIR [--baselines DIR]`, where
/// `DIR` holds the plan's `lanes.json` and the second the gate legs'
/// baselines, with the results directory the run's global `--results`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    match parse(args).and_then(|(dir, baselines)| stored(env, dir, baselines)) {
        Ok(()) => 0,
        Err(e) => crate::arg_err(&e),
    }
}

type Parsed = (PathBuf, Option<PathBuf>);

fn parse(args: &[String]) -> Result<Parsed, String> {
    match args {
        [save, flag, dir] if save == "save" && flag == "--lanes" => Ok((PathBuf::from(dir), None)),
        [save, flag, dir, more, baselines]
            if save == "save" && flag == "--lanes" && more == "--baselines" =>
        {
            Ok((PathBuf::from(dir), Some(PathBuf::from(baselines))))
        }
        _ => Err("usage: cargo soothfast ci lanes save --lanes DIR [--baselines DIR]".into()),
    }
}

fn stored(env: &CiEnv, dir: PathBuf, baselines: Option<PathBuf>) -> Result<(), String> {
    let results = env
        .results
        .as_ref()
        .ok_or("lanes needs a results directory")?;
    let path = dir.join("lanes.json");
    let Some(listing) = read_listing(&path)? else {
        println!("lanes: no lanes.json; nothing to store");
        return Ok(());
    };
    let store = Store::open().ok_or("cannot open .soothfast/lanes")?;
    let measured = baselines.as_deref().map(read_baselines).unwrap_or_default();
    let saved = save(
        &listing,
        &plain_passes(results),
        &measured,
        &store,
        &pass(env)?,
    )?;
    println!("lanes: stored {saved} passed lane(s)");
    Ok(())
}

/// Where the run's lanes passed: a pull request's head, not its merge commit.
fn pass(env: &CiEnv) -> Result<Pass, String> {
    let commit = env.head.clone().ok_or("lanes needs the run's commit")?;
    let run = std::env::var("GITHUB_RUN_ID").unwrap_or_default();
    Ok(Pass {
        commit,
        run,
        baseline: None,
    })
}

/// The baselines under `dir` that parse. One that does not leaves its lane
/// stored without a baseline, which only costs a run.
fn read_baselines(dir: &std::path::Path) -> Vec<Baseline> {
    baseline_files(dir)
        .into_iter()
        .filter_map(|file| {
            let doc = serde_json::from_str(&std::fs::read_to_string(&file).ok()?).ok()?;
            let stem = file.file_stem()?.to_string_lossy().into_owned();
            Some((stem, doc))
        })
        .collect()
}

/// The plan's listing, or none when the plan wrote nothing.
fn read_listing(path: &std::path::Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_save_with_a_lanes_directory_is_accepted() {
        let argv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse(&argv(&["save", "--lanes", "d"])),
            Ok((PathBuf::from("d"), None))
        );
        assert_eq!(
            parse(&argv(&["save", "--lanes", "d", "--baselines", "b"])),
            Ok((PathBuf::from("d"), Some(PathBuf::from("b"))))
        );
        assert!(parse(&argv(&["save"])).is_err());
        assert!(parse(&argv(&["load", "--lanes", "d"])).is_err());
    }

    #[test]
    fn a_missing_listing_is_none_and_an_unreadable_one_is_an_error() {
        let dir = std::env::temp_dir().join(format!("soothfast-lanes-ci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_listing(&dir.join("lanes.json")), Ok(None));
        std::fs::write(dir.join("lanes.json"), "[]").unwrap();
        assert_eq!(read_listing(&dir.join("lanes.json")), Ok(Some("[]".into())));
        assert!(read_listing(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pull_requests_lanes_pass_at_its_head_not_its_merge_commit() {
        use crate::ci::env::Flags;
        use std::collections::BTreeMap;
        let vars = BTreeMap::from([
            ("GITHUB_EVENT_NAME".to_string(), "pull_request".to_string()),
            ("GITHUB_SHA".to_string(), "merge".to_string()),
        ]);
        let payload = r#"{"pull_request":{"head":{"sha":"headsha"},"base":{"sha":"b"}}}"#;
        let env = CiEnv::parse(&vars, Some(payload), &Flags::default());
        assert_eq!(pass(&env).unwrap().commit, "headsha");
        let none = CiEnv::parse(&BTreeMap::new(), None, &Flags::default());
        assert!(pass(&none).is_err());
    }
}
