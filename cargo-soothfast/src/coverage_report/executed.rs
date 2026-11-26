//! The `tested` and `measured` columns: which public items the package's
//! tests and benches execute, from a fresh instrumented run or a stored one.

use std::collections::BTreeSet;
use std::path::Path;

use soothfast_docs::surface::Surface;

use super::Row;
use crate::coverage_executed::{self, Cached, Regions};
use crate::invoke::CommonArgs;
use crate::workspace;

/// Whether an executed run may happen while building a report.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Want {
    Run,
    Stored,
    Skip,
}

/// What each executed column may do while building a report.
#[derive(Clone, Copy)]
pub(super) struct Wants {
    pub tests: Want,
    pub benches: Want,
}

/// Where a column's items came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Origin {
    NotRun,
    /// A check with no minimum on `tested` or `measured`.
    Skipped,
    Run,
    /// The run failed, so the column is a lower bound.
    Partial,
    /// A stored run, finished at this many seconds since the epoch.
    Stored(u64),
}

/// The origin of each executed column. `None` for a column the package
/// cannot have, such as benches of a package with no bench target.
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Provenance {
    pub tests: Option<Origin>,
    pub benches: Option<Origin>,
}

impl Provenance {
    pub(super) fn of(&self, name: &str) -> Option<Origin> {
        match name {
            "tested" => self.tests,
            _ => self.benches,
        }
    }
}

/// The executed columns of one package: the items they can tell, the ones
/// each run reached, and where each run came from.
pub(super) struct Columns {
    package: String,
    declared: BTreeSet<String>,
    tested: Option<BTreeSet<String>>,
    measured: Option<BTreeSet<String>>,
    provenance: Provenance,
}

type Column = (Option<Regions>, Origin);

/// The executed columns of `pkg`. `bench` names its bench target and features.
pub(super) fn gather(
    pkg: &str,
    surf: &Surface,
    root: &Path,
    bench: &CommonArgs,
    wants: Wants,
) -> Result<Columns, String> {
    let features = bench.features.as_deref();
    let tests = column(
        wants.tests,
        || coverage_executed::cached_tests(pkg, features),
        || coverage_executed::tests(pkg, features),
    )?;
    let target = bench.target.as_deref().unwrap_or("soothfast");
    let benches = if workspace::has_bench_target(pkg, target, None).map_err(|e| e.to_string())? {
        Some(column(
            wants.benches,
            || coverage_executed::cached_benches(bench),
            || coverage_executed::benches(bench),
        )?)
    } else {
        None
    };
    let hit = |column: &Column| {
        column
            .0
            .as_ref()
            .map(|regions| coverage_executed::reach(surf, root, regions))
    };
    Ok(Columns {
        package: pkg.to_string(),
        tested: hit(&tests),
        measured: benches.as_ref().and_then(hit),
        provenance: Provenance {
            tests: Some(tests.1),
            benches: benches.map(|b| b.1),
        },
        declared: coverage_executed::declared_paths(surf),
    })
}

fn column(
    want: Want,
    stored: impl FnOnce() -> Option<Cached>,
    run: impl FnOnce() -> Result<Regions, String>,
) -> Result<Column, String> {
    Ok(match want {
        Want::Skip => (None, Origin::Skipped),
        Want::Stored => match stored() {
            Some(cached) => (Some(cached.regions), Origin::Stored(cached.unix)),
            None => (None, Origin::NotRun),
        },
        Want::Run => {
            let regions = run()?;
            let origin = if regions.partial {
                Origin::Partial
            } else {
                Origin::Run
            };
            (Some(regions), origin)
        }
    })
}

/// `rows` with the columns filled in for the items a run can tell.
pub(super) fn attach(rows: Vec<Row>, columns: &Columns) -> Vec<Row> {
    let judge = |hit: &Option<BTreeSet<String>>, path: &str| {
        hit.as_ref()
            .filter(|_| columns.declared.contains(path))
            .map(|hit| hit.contains(path))
    };
    rows.into_iter()
        .map(|row| Row {
            tested: judge(&columns.tested, &row.path),
            measured: judge(&columns.measured, &row.path),
            executed: columns.provenance,
            package: columns.package.clone(),
            ..row
        })
        .collect()
}

/// The note a summary line carries for `name`: for a figure its run's age,
/// bound and the packages left out, and for a dimension no row could tell,
/// why not.
pub(super) fn note(rows: &[Row], name: &str, told: bool) -> Option<String> {
    let origins: Vec<Origin> = rows.iter().filter_map(|r| r.executed.of(name)).collect();
    if !told {
        return if origins.contains(&Origin::NotRun) {
            Some("not run: pass --executed".to_string())
        } else {
            origins
                .contains(&Origin::Skipped)
                .then(|| "not run: no tested or measured minimum".to_string())
        };
    }
    let mut parts = Vec::new();
    if origins.contains(&Origin::Partial) {
        parts.push("lower bound: the bench run failed".to_string());
    }
    match unrun_packages(rows, name) {
        0 => {}
        n => parts.push(format!("{n} package(s) not run")),
    }
    let oldest = origins
        .iter()
        .filter_map(|o| match o {
            Origin::Stored(unix) => Some(*unix),
            _ => None,
        })
        .min();
    parts.extend(oldest.map(|unix| format!("cached run from {}", stamp(unix))));
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn unrun_packages(rows: &[Row], name: &str) -> usize {
    rows.iter()
        .filter(|r| matches!(r.executed.of(name), Some(Origin::NotRun | Origin::Skipped)))
        .map(|r| r.package.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

/// Whether the figure for `name` is a lower bound: a bench run failed, or a
/// package it would count was not run.
pub(super) fn partial(rows: &[Row], name: &str) -> bool {
    matches!(name, "tested" | "measured")
        && (rows
            .iter()
            .any(|r| r.executed.of(name) == Some(Origin::Partial))
            || unrun_packages(rows, name) > 0)
}

/// `unix` as `YYYY-MM-DD HH:MM UTC`.
fn stamp(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let minutes = unix % 86_400 / 60;
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minutes / 60,
        minutes % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, executed: Provenance) -> Row {
        Row {
            path: path.into(),
            package: path.split("::").next().unwrap_or_default().into(),
            kind: "function".into(),
            executed,
            ..Row::default()
        }
    }

    #[test]
    fn a_stamp_reads_as_utc_minutes() {
        assert_eq!(stamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(stamp(1_795_707_600), "2026-11-26 15:40 UTC");
        assert_eq!(stamp(951_782_400 + 86_399), "2000-02-29 23:59 UTC");
    }

    #[test]
    fn only_declared_items_are_judged() {
        let columns = Columns {
            package: "d".into(),
            declared: BTreeSet::from(["d::f".to_string(), "d::g".to_string()]),
            tested: Some(BTreeSet::from(["d::f".to_string()])),
            measured: None,
            provenance: Provenance {
                tests: Some(Origin::Run),
                benches: Some(Origin::NotRun),
            },
        };
        let rows = attach(
            vec![
                row("d::f", Provenance::default()),
                row("d::g", Provenance::default()),
                row("d::C", Provenance::default()),
            ],
            &columns,
        );
        let tested: Vec<_> = rows.iter().map(|r| r.tested).collect();
        assert_eq!(tested, [Some(true), Some(false), None]);
        assert!(rows.iter().all(|r| r.measured.is_none()));
    }

    #[test]
    fn a_dimension_nobody_told_says_why_it_did_not_run() {
        let unrun = Provenance {
            tests: Some(Origin::NotRun),
            benches: None,
        };
        let rows = [row("d::f", unrun)];
        assert_eq!(
            note(&rows, "tested", false).as_deref(),
            Some("not run: pass --executed")
        );
        assert_eq!(note(&rows, "measured", false), None);
        let skipped = Provenance {
            tests: Some(Origin::Skipped),
            benches: None,
        };
        assert_eq!(
            note(&[row("d::f", skipped)], "tested", false).as_deref(),
            Some("not run: no tested or measured minimum")
        );
    }

    #[test]
    fn a_stored_run_dates_the_figure_and_a_failed_bench_run_bounds_it() {
        let stored = Provenance {
            tests: Some(Origin::Stored(1_795_707_600)),
            benches: Some(Origin::Partial),
        };
        let rows = [row("d::f", stored)];
        assert_eq!(
            note(&rows, "tested", true).as_deref(),
            Some("cached run from 2026-11-26 15:40 UTC")
        );
        assert_eq!(
            note(&rows, "measured", true).as_deref(),
            Some("lower bound: the bench run failed")
        );
        assert!(partial(&rows, "measured") && !partial(&rows, "tested"));
    }

    #[test]
    fn a_package_without_a_run_leaves_the_figure_a_lower_bound() {
        let ran = Provenance {
            tests: Some(Origin::Run),
            benches: None,
        };
        let idle = Provenance {
            tests: Some(Origin::NotRun),
            benches: None,
        };
        let rows = [row("a::f", ran), row("b::f", idle), row("b::g", idle)];
        assert!(partial(&rows, "tested"));
        assert_eq!(
            note(&rows, "tested", true).as_deref(),
            Some("1 package(s) not run")
        );
        assert!(!partial(&rows[..1], "tested"));
    }
}
