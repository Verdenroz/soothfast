//! Gating result files imported from another harness instead of measuring.

use super::args::GateArgs;
use super::compare::{CompareCtx, compare};
use super::outcome::err;
use crate::config::Config;
use crate::gate_lock;
use crate::import;
use crate::invoke;

/// Gate an imported run against a baseline or another imported run. Nothing
/// is built or measured, so this works outside a cargo workspace; `root`
/// holds the baselines and `soothfast.toml`.
pub(super) fn run_imported(g: &GateArgs, root: &std::path::Path) -> i32 {
    let Some((format, file)) = &g.import.current else {
        return err("gate --import needs a format and a file");
    };
    if g.ratchet.is_some() || g.against_ref.is_some() || g.save_baseline.is_some() || g.deps {
        return err("--import gates against --baseline or --reference-import only");
    }
    if g.import.baseline_given && g.import.reference.is_some() {
        return err("--baseline and --reference-import both name the reference; pass one");
    }
    if g.common.shard.is_some() || g.common.pkg.is_some() {
        return err("--import gates a result file whole; --shard and -p do not apply");
    }
    let walltime_advisory = match (
        &g.import.walltime,
        Config::load(root).and_then(|c| c.gate()),
    ) {
        (Some(mode), _) => mode == "advisory",
        (None, Ok(cfg)) => cfg.walltime_advisory,
        (None, Err(e)) => return err(&e),
    };
    let prefix = g.import.prefix.as_deref();
    let (mut current, prefix) = match import::read(format, file, prefix) {
        Ok(read) => read,
        Err(e) => return err(&e),
    };
    if let Err(e) = import::check_prefixes(root, std::slice::from_ref(&prefix)) {
        return err(&e);
    }
    if let Some(f) = &g.common.filter {
        current.items.retain(|id, _| id.contains(f.as_str()));
        if current.items.is_empty() {
            return err("--filter matches no imported item; refusing to gate");
        }
    }
    let reference = match &g.import.reference {
        Some((format, file)) => {
            import::read(format, file, Some(&prefix)).map(|(run, _)| import::to_doc(&run))
        }
        None => match invoke::load_baseline_in(root, &g.baseline) {
            Ok(Some(mut doc)) => {
                // A baseline can hold other prefixes and Rust items; theirs are not missing here.
                if let Some(items) = doc["items"].as_object_mut() {
                    items.retain(|_, v| v["imported"].as_str() == Some(prefix.as_str()));
                }
                Ok(doc)
            }
            Ok(None) => Err(format!("no baseline named {:?}", g.baseline)),
            Err(e) => Err(format!("failed to load baseline: {e}")),
        },
    };
    let reference = match reference {
        Ok(doc) => doc,
        Err(e) => return err(&e),
    };

    let ctx = CompareCtx {
        deps_mode: false,
        allow_gone: g.allow_gone,
        filter: g.common.filter.clone(),
        shard: None,
        pkg: None,
        buildcost_mode: false,
        accepted: gate_lock::read(root).unwrap_or_default(),
        headroom_pct: None,
        walltime_advisory,
    };
    let mut failing_ids = Vec::new();
    let failures = compare(
        &reference,
        &current,
        "",
        &ctx,
        &mut failing_ids,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    if failures > 0 {
        println!("gate: FAILED ({failures} regression(s))");
        1
    } else {
        println!("gate: passed ({} item(s))", current.items.len());
        0
    }
}

#[cfg(test)]
mod tests {

    use crate::gate::test_support::{imported_gate_with, pyperf_spread};

    use crate::invoke;

    fn imported_gate(name: &str, current: f64, config: &str, baseline: bool) -> i32 {
        imported_gate_with(name, current, config, baseline, |_| {})
    }

    #[test]
    fn an_import_filter_matching_nothing_is_refused() {
        let code = imported_gate_with("filter-none", 0.0013, "", false, |g| {
            g.common.filter = Some("nothing".into());
        });
        assert_eq!(code, 2);
    }

    #[test]
    fn an_import_filter_gates_the_matching_items() {
        let code = imported_gate_with("filter-some", 0.0013, "", false, |g| {
            g.common.filter = Some("b".into());
        });
        assert_eq!(code, 1);
    }

    #[test]
    fn an_import_rejects_shard_and_package() {
        let shard = imported_gate_with("shard", 0.001, "", false, |g| {
            g.common.shard = Some(invoke::Shard::parse("1/2").unwrap());
        });
        let pkg = imported_gate_with("pkg", 0.001, "", false, |g| {
            g.common.pkg = Some("mylib".into());
        });
        assert_eq!((shard, pkg), (2, 2));
    }

    #[test]
    fn an_import_rejects_a_baseline_beside_a_reference_import() {
        let code = imported_gate_with("both", 0.001, "", false, |g| {
            g.baseline = "base".into();
            g.import.baseline_given = true;
        });
        assert_eq!(code, 2);
    }

    #[test]
    fn an_imported_run_30_percent_slower_fails() {
        assert_eq!(imported_gate("slow", 0.0013, "", false), 1);
    }

    fn spread_gate(name: &str, spread: f64, current: f64) -> i32 {
        imported_gate_with(name, current, "", false, |g| {
            let paths = [&g.import.current, &g.import.reference];
            for (path, seconds) in paths.into_iter().zip([current, 0.001]) {
                let path = &path.as_ref().unwrap().1;
                std::fs::write(path, pyperf_spread(seconds, spread)).unwrap();
            }
        })
    }

    #[test]
    fn imports_with_4_percent_dispersion_pass_at_9_percent() {
        assert_eq!(spread_gate("noisy-9", 0.04, 0.00109), 0);
    }

    #[test]
    fn dispersion_widens_the_walltime_limit_past_the_fixed_10_percent() {
        assert_eq!(spread_gate("noisy-11", 0.04, 0.00111), 0);
        assert_eq!(spread_gate("quiet-11", 0.0001, 0.00111), 1);
    }

    #[test]
    fn imports_with_4_percent_dispersion_still_fail_at_30_percent() {
        assert_eq!(spread_gate("noisy-30", 0.04, 0.0013), 1);
    }

    #[test]
    fn imports_with_near_zero_dispersion_still_fail_at_30_percent() {
        assert_eq!(spread_gate("quiet-30", 0.0001, 0.0013), 1);
    }

    #[test]
    fn an_imported_run_2_percent_slower_passes() {
        assert_eq!(imported_gate("flat", 0.00102, "", false), 0);
    }

    #[test]
    fn an_imported_run_gates_against_a_stored_baseline_of_its_own_prefix() {
        assert_eq!(imported_gate("base-slow", 0.0013, "", true), 1);
        assert_eq!(imported_gate("base-flat", 0.00102, "", true), 0);
    }

    #[test]
    fn advisory_walltime_lets_an_imported_regression_through() {
        let advisory = "[gate]\nwalltime = \"advisory\"\n";
        assert_eq!(imported_gate("advisory", 0.0013, advisory, false), 0);
    }
}
