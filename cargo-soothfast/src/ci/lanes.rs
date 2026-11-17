//! `ci lanes save`: stores the lanes a default-branch run passed, for the
//! pull requests after it to carry.

use std::path::PathBuf;

use super::env::CiEnv;
use crate::lanes::save::save;
use crate::lanes::store::{Pass, Store};
use crate::verdict::plain_passes;

/// Entry point for `ci lanes`: `save --lanes DIR`, where `DIR` holds the
/// plan's `lanes.json`, with the results directory the run's global
/// `--results`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    match parse(args).and_then(|dir| stored(env, dir)) {
        Ok(()) => 0,
        Err(e) => crate::arg_err(&e),
    }
}

fn parse(args: &[String]) -> Result<PathBuf, String> {
    match args {
        [save, flag, dir] if save == "save" && flag == "--lanes" => Ok(PathBuf::from(dir)),
        _ => Err("usage: cargo soothfast ci lanes save --lanes DIR".into()),
    }
}

fn stored(env: &CiEnv, dir: PathBuf) -> Result<(), String> {
    let results = env
        .results
        .as_ref()
        .ok_or("lanes needs a results directory")?;
    let commit = env.head.clone().ok_or("lanes needs the run's commit")?;
    let run = std::env::var("GITHUB_RUN_ID").unwrap_or_default();
    let path = dir.join("lanes.json");
    let Some(listing) = read_listing(&path)? else {
        println!("lanes: no lanes.json; nothing to store");
        return Ok(());
    };
    let store = Store::open().ok_or("cannot open .soothfast/lanes")?;
    let saved = save(
        &listing,
        &plain_passes(results),
        &store,
        &Pass { commit, run },
    )?;
    println!("lanes: stored {saved} passed lane(s)");
    Ok(())
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
            Ok(PathBuf::from("d"))
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
}
