//! Error and partial exits, and the triage artifact written on failure.

use serde_json::Value;

use super::args::GateArgs;
use crate::gate_save::save_errored;
use crate::invoke::{self, CommonArgs};

/// On failure, produce a callgrind function-level report per regressed item
/// (best effort). No `valgrind --version` pre-check: it reports success on
/// AVX-512 hosts where callgrind SIGILLs — only running the real binary
/// (which the runner's own probe does) tells the truth. One failed attempt
/// disables triage for the rest of the run instead of failing per item.
pub(super) fn triage(common: &CommonArgs, failing_ids: &[String]) {
    let Ok(root) = invoke::workspace_root() else {
        return;
    };
    let dir = root.join(".soothfast").join("triage");
    let _ = std::fs::create_dir_all(&dir);
    for id in failing_ids.iter().take(3) {
        match invoke::run_bench_raw(common, &["--triage", id]) {
            Ok(report) => {
                let path = dir.join(format!("{}.txt", id.replace("::", "_")));
                if std::fs::write(&path, report).is_ok() {
                    println!("triage: {}", path.display());
                }
            }
            Err(e) => {
                println!("triage: skipped (callgrind unavailable here: {e})");
                return;
            }
        }
    }
}

pub(super) fn err(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    2
}

/// `err` for a gate that never reached a verdict. Its `--save-baseline`
/// still names the package's items as gate-failed, so no claim on them reads
/// as measured.
pub(super) fn errored(g: &GateArgs, exe: Option<&std::path::Path>, msg: &str) -> i32 {
    if let (Some(name), Ok(root)) = (&g.save_baseline, invoke::workspace_root())
        && g.common.backend.as_deref() != Some("buildcost")
    {
        let built = exe
            .map(std::path::Path::to_path_buf)
            .or_else(|| crate::benchexe::stored(&g.common, None, None, invoke::Build::Measure));
        let listed = crate::benchexe::listing(&g.common, built.as_deref());
        if let Err(e) = &listed {
            println!("gate: {e}, so its gate-failed entry names only the items it owned before");
        }
        errored_at(&root, name, &g.common, listed.ok().as_deref(), msg);
    }
    err(msg)
}

/// `errored` for a gate whose config did not apply: a build without that
/// config is not HEAD's binary, so nothing is listed and every item is.
pub(super) fn errored_unbuilt(g: &GateArgs, msg: &str) -> i32 {
    if let (Some(name), Ok(root)) = (&g.save_baseline, invoke::workspace_root())
        && g.common.backend.as_deref() != Some("buildcost")
    {
        errored_at(&root, name, &g.common, None, msg);
    }
    err(msg)
}

/// The gate-failed stub `errored` writes into baseline `name` under `root`.
fn errored_at(
    root: &std::path::Path,
    name: &str,
    common: &CommonArgs,
    listed: Option<&[String]>,
    msg: &str,
) {
    if let Err(e) = save_errored(root, name, common, listed, msg) {
        eprintln!("soothfast: failed to save baseline: {e}");
    }
}

/// The ratchet baseline `name` under `root`.
pub(super) fn ratchet_in(root: &std::path::Path, name: &str) -> Result<Value, String> {
    match invoke::load_baseline_in(root, name) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(format!("no ratchet baseline named {name:?}")),
        Err(e) => Err(format!("failed to load ratchet baseline: {e}")),
    }
}

/// Like `err`, but for a `gate accept --only` invocation that recorded
/// nothing: distinct from `2` (a usage/config error) so a CI step doesn't
/// mistake "named benches, none eligible" for full success or bad usage.
pub(super) fn partial(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    3
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{errored_at, ratchet_in};
    use crate::gate::test_support::scratch;
    use crate::invoke::{self, CommonArgs};

    #[test]
    fn a_gate_whose_bench_did_not_build_covers_its_package_for_unchanged_claims_only() {
        let root = scratch("bench-unbuilt");
        let stale = json!({
            "version": 1,
            "items": { "pkg::old": { "alloc": { "allocs": 1, "bytes": 8 } } },
            "owners": { "pkg": ["pkg::old"] },
        });
        invoke::write_baseline_in(&root, "head", &stale).unwrap();
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        errored_at(&root, "head", &common, None, "cargo bench failed");
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(saved["items"], json!({}));
        let expr = "pkg::old.alloc.allocs <= 1";
        let doc =
            soothfast_docs::markdown::scan(&format!("<!-- soothfast:claim {expr} -->\n")).unwrap();
        let docs = [("a.md", &doc)];
        let unchanged = || {
            Ok(std::collections::BTreeSet::from([(
                "a.md".to_string(),
                expr.to_string(),
            )]))
        };
        assert_eq!(
            crate::docs_claims::check(&docs, Some(&saved), unchanged),
            Ok(0)
        );
        let edited = || Ok(std::collections::BTreeSet::new());
        assert_eq!(
            crate::docs_claims::check(&docs, Some(&saved), edited),
            Ok(1)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_ratchet_error_replaces_the_package_s_saved_numbers_with_a_failed_entry() {
        let root = scratch("ratchet-error");
        let stale = json!({
            "version": 1,
            "items": { "pkg::old": { "alloc": { "allocs": 1, "bytes": 8 } } },
            "owners": { "pkg": ["pkg::old"] },
        });
        invoke::write_baseline_in(&root, "head", &stale).unwrap();
        let e = ratchet_in(&root, "release").unwrap_err();
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        let listed = ["pkg::new".to_string()];
        errored_at(&root, "head", &common, Some(&listed), &e);
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(saved["items"], json!({}));
        assert_eq!(saved["unmeasured"][0]["reason"], "gate-failed");
        assert_eq!(saved["unmeasured"][0]["items"], json!(["pkg::new"]));
        let _ = std::fs::remove_dir_all(&root);
    }
}
