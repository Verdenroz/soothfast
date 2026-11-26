//! The `tested` and `measured` columns: which public items the package's
//! tests and benches execute, from a fresh instrumented run or a stored one.

use std::collections::BTreeSet;
use std::path::Path;

use soothfast_docs::surface::Surface;

use super::Row;
use crate::coverage_executed::{self, Cached, Regions};
use crate::invoke::{CommonArgs, harness_predating_setup_exclusion, workspace_root};
use crate::workspace;

/// Whether an executed run may happen while building a report.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    Run,
    Stored,
    Skip,
}

/// What each executed column may do while building a report.
#[derive(Clone, Copy)]
pub(crate) struct Wants {
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
    /// The bench harness credits code that runs only in setup, so the column
    /// is an upper bound on what is measured. Carries the finish time of a
    /// stored run.
    SetupCredited(Option<u64>),
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
pub(crate) struct Columns {
    package: String,
    pub declared: BTreeSet<String>,
    pub tested: Option<BTreeSet<String>>,
    pub measured: Option<BTreeSet<String>>,
    provenance: Provenance,
}

type Column = (Option<Regions>, Origin);

/// The executed columns of `pkg`, for `coverage report` and `coverage
/// executed` alike. Tests build with `features`, the ones the
/// surface was read under; `bench` names the bench target and its features.
pub(crate) fn gather(
    pkg: &str,
    surf: &Surface,
    root: &Path,
    features: Option<&str>,
    bench: &CommonArgs,
    wants: Wants,
) -> Result<Columns, String> {
    let members = match wants.tests {
        Want::Skip => Vec::new(),
        _ => coverage_executed::defining_members(surf, root, pkg)?,
    };
    let tests = column(
        wants.tests,
        || coverage_executed::cached_tests(pkg, features, &members),
        || coverage_executed::tests(pkg, features, &members),
    )?;
    let target = bench.target.as_deref().unwrap_or("soothfast");
    let benches = if workspace::has_bench_target(pkg, target, None).map_err(|e| e.to_string())? {
        Some(
            column(
                wants.benches,
                || coverage_executed::cached_benches(bench),
                || coverage_executed::benches(bench),
            )
            .map(|column| credited_setup(column, pkg, &lock()))?,
        )
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

fn lock() -> String {
    workspace_root()
        .and_then(|root| std::fs::read_to_string(root.join("Cargo.lock")))
        .unwrap_or_default()
}

/// `column` marked when `lock` pins a bench harness that counts setup code as
/// measured, with a warning.
fn credited_setup(column: Column, pkg: &str, lock: &str) -> Column {
    let (regions, origin) = column;
    let Some(version) = harness_predating_setup_exclusion(lock).filter(|_| regions.is_some())
    else {
        return (regions, origin);
    };
    eprintln!(
        "{pkg}: bench harness soothfast-measure {version} predates setup exclusion; measured includes code that runs only in bench setup (upgrade soothfast to fix)"
    );
    let origin = match origin {
        Origin::Partial => origin,
        Origin::Stored(unix) => Origin::SetupCredited(Some(unix)),
        _ => Origin::SetupCredited(None),
    };
    (regions, origin)
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
    if origins
        .iter()
        .any(|o| matches!(o, Origin::SetupCredited(_)))
    {
        parts.push("upper bound: the bench harness predates setup exclusion".to_string());
    }
    match unrun_packages(rows, name) {
        0 => {}
        n => parts.push(format!("{n} package(s) not run")),
    }
    let oldest = origins
        .iter()
        .filter_map(|o| match o {
            Origin::Stored(unix) | Origin::SetupCredited(Some(unix)) => Some(*unix),
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

/// Whether the figure for `name` is only a bound: a bench run failed, a
/// harness credits setup, or a package it would count was not run.
pub(super) fn partial(rows: &[Row], name: &str) -> bool {
    matches!(name, "tested" | "measured")
        && (rows.iter().any(|r| {
            matches!(
                r.executed.of(name),
                Some(Origin::Partial | Origin::SetupCredited(_))
            )
        }) || unrun_packages(rows, name) > 0)
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
    fn the_report_judges_the_items_the_standalone_run_lists() {
        let mut surf = Surface::default();
        let spans = vec![soothfast_docs::surface::Span {
            file: "src/lib.rs".into(),
            begin: 1,
            end: 2,
        }];
        for (path, kind) in [
            ("d::f", "function"),
            ("d::C", "constant"),
            ("d::M", "module"),
        ] {
            surf.items.insert(
                path.into(),
                soothfast_docs::surface::ItemInfo {
                    kind: kind.into(),
                    fingerprint: 1 + path.len() as u64,
                    signature: String::new(),
                    has_docs: false,
                    example: false,
                    spans: spans.clone(),
                },
            );
        }
        let columns = Columns {
            package: "d".into(),
            declared: coverage_executed::declared_paths(&surf),
            tested: Some(BTreeSet::new()),
            measured: None,
            provenance: Provenance::default(),
        };
        let rows: Vec<Row> = surf
            .counted()
            .iter()
            .map(|c| row(&c.path, Provenance::default()))
            .collect();
        let judged: BTreeSet<_> = attach(rows, &columns)
            .into_iter()
            .filter(|r| r.tested.is_some())
            .map(|r| r.path)
            .collect();
        assert_eq!(judged, columns.declared);
        assert_eq!(judged, BTreeSet::from(["d::f".to_string()]));
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
    fn an_older_registry_harness_marks_the_measured_column_partial() {
        let lock = "[[package]]\nname = \"soothfast-measure\"\nversion = \"0.8.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let ran = || (Some(Regions::default()), Origin::Run);
        assert_eq!(
            credited_setup(ran(), "demo", lock).1,
            Origin::SetupCredited(None)
        );
        let stored = (Some(Regions::default()), Origin::Stored(7));
        assert_eq!(
            credited_setup(stored, "demo", lock).1,
            Origin::SetupCredited(Some(7))
        );
        assert_eq!(credited_setup(ran(), "demo", "").1, Origin::Run);
        let path = "[[package]]\nname = \"soothfast-measure\"\nversion = \"0.8.0\"\n";
        assert_eq!(credited_setup(ran(), "demo", path).1, Origin::Run);
        let rows = [row(
            "d::f",
            Provenance {
                tests: None,
                benches: Some(Origin::SetupCredited(Some(1_795_707_600))),
            },
        )];
        assert!(partial(&rows, "measured"));
        assert_eq!(
            note(&rows, "measured", true).as_deref(),
            Some(
                "upper bound: the bench harness predates setup exclusion, cached run from 2026-11-26 15:40 UTC"
            )
        );
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
