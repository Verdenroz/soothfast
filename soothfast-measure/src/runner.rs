//! In-bench-binary runner: parses argv, measures every registered item with
//! the selected backends, evaluates checked claims (alloc/p99/complexity),
//! and emits results (JSONL for the CLI, plain lines for humans).
//!
//! The runner always exits 0 on a completed run; assertion verdicts travel
//! as records and the CLI decides the exit code.

use soothfast_registry::{Bencher, MeasuredItem, fixture_items, measured_items, route_items};

use crate::json::esc;
use crate::{alloc, asyncexec, callgrind, sweep, walltime};

#[derive(PartialEq, Clone, Copy)]
enum Backend {
    Auto,
    All,
    Walltime,
    Alloc,
    Perfcnt,
    Callgrind,
}

struct Args {
    json: bool,
    list: bool,
    list_routes: bool,
    backend: Backend,
    filter: Option<String>,
    samples: u32,
    callgrind_exec: Option<String>,
    triage: Option<String>,
    iters: u64,
    skip_gating_counters: bool,
}

fn parse_args() -> Args {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(mut it: impl Iterator<Item = String>) -> Args {
    let mut args = Args {
        json: false,
        list: false,
        list_routes: false,
        backend: Backend::Auto,
        filter: None,
        samples: walltime::DEFAULT_SAMPLES,
        callgrind_exec: None,
        triage: None,
        iters: 1,
        skip_gating_counters: false,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            // `cargo bench` appends --bench to harness=false binaries; ignore.
            "--bench" => {}
            "--json" => args.json = true,
            "--list" => args.list = true,
            "--list-routes" => args.list_routes = true,
            "--backend" => {
                args.backend = match it.next().as_deref() {
                    Some("auto") => Backend::Auto,
                    Some("all") => Backend::All,
                    Some("walltime") => Backend::Walltime,
                    Some("alloc") => Backend::Alloc,
                    Some("perfcnt") => Backend::Perfcnt,
                    Some("callgrind") => Backend::Callgrind,
                    other => die(&format!("unknown --backend {other:?}")),
                }
            }
            "--filter" => match it.next() {
                Some(f) => args.filter = Some(f),
                None => die("--filter needs a value"),
            },
            "--samples" => match it.next().and_then(|s| s.parse().ok()) {
                Some(n) if n > 0 => args.samples = n,
                _ => die("--samples needs a positive integer"),
            },
            "--callgrind-exec" => match it.next() {
                Some(id) => args.callgrind_exec = Some(id),
                None => die("--callgrind-exec needs an item id"),
            },
            "--triage" => match it.next() {
                Some(id) => args.triage = Some(id),
                None => die("--triage needs an item id"),
            },
            "--iters" => match it.next().and_then(|s| s.parse().ok()) {
                Some(n) if n > 0 => args.iters = n,
                _ => die("--iters needs a positive integer"),
            },
            "--skip-gating-counters" => args.skip_gating_counters = true,
            other => die(&format!("unknown runner arg {other:?}")),
        }
    }
    args
}

fn die(msg: &str) -> ! {
    eprintln!("soothfast runner: {msg}");
    std::process::exit(2);
}

/// Whether resolving backends for this request could possibly need
/// callgrind, so the caller can skip probing it (a real valgrind
/// subprocess) when the answer can't matter.
fn might_need_callgrind(backend: Backend, perf_available: bool) -> bool {
    matches!(backend, Backend::Callgrind)
        || (matches!(backend, Backend::Auto | Backend::All) && !perf_available)
}

/// Which backends actually run, given what was requested and whether perf
/// counters and callgrind are available on this host. Auto/All degrade
/// gracefully — perfcnt, else callgrind, else neither — so a PMU-less CI VM
/// still gets a real run instead of silently measuring nothing. An explicit
/// choice that isn't available is the caller's job to reject before this
/// runs; here it just reads as "not used".
fn resolve_backends(backend: Backend, perf_available: bool, cg_available: bool) -> (bool, bool) {
    let use_perf =
        matches!(backend, Backend::Perfcnt | Backend::Auto | Backend::All) && perf_available;
    let use_cg = match backend {
        Backend::Callgrind => cg_available,
        Backend::Auto | Backend::All if !use_perf => cg_available,
        _ => false,
    };
    (use_perf, use_cg)
}

/// One registered item with its package-qualified id and effective
/// fingerprint (the setup fixture's fingerprint folded in, so an input
/// change can never masquerade as unchanged code).
struct Entry {
    id: String,
    fingerprint: u64,
    item: &'static MeasuredItem,
}

fn entry_for(item: &'static MeasuredItem) -> Entry {
    let mut fingerprint = item.fingerprint;
    if let Some(setup) = item.setup {
        for f in fixture_items() {
            if f.id == setup || f.id.ends_with(&format!("::{setup}")) {
                fingerprint ^= f.fingerprint;
            }
        }
    }
    Entry {
        id: item.full_id(),
        fingerprint,
        item,
    }
}

fn find_item(id: &str) -> &'static MeasuredItem {
    measured_items()
        .iter()
        .find(|i| i.full_id() == id)
        .unwrap_or_else(|| die(&format!("no measured item with id {id:?}")))
}

#[cfg(target_os = "linux")]
fn perf_probe() -> Result<(), String> {
    crate::perfcnt::probe()
}
#[cfg(not(target_os = "linux"))]
fn perf_probe() -> Result<(), String> {
    Err("perfcnt backend is Linux-only".into())
}

/// Entry point installed by `soothfast::bench_main!`.
pub fn main() {
    let args = parse_args();

    // Hidden mode: execute one item's body under valgrind (see callgrind.rs).
    if let Some(id) = &args.callgrind_exec {
        let item = find_item(id);
        let mut collector = |body: &mut dyn FnMut()| body();
        (item.runner)(&mut Bencher::__new(args.iters, &mut collector));
        return;
    }
    if let Some(id) = &args.triage {
        find_item(id); // fail fast on typos
        match callgrind::annotate(id) {
            Ok(report) => print!("{report}"),
            Err(e) => die(&format!("triage failed: {e}")),
        }
        return;
    }

    if args.list_routes {
        let mut routes: Vec<_> = route_items().iter().collect();
        routes.sort_by_key(|r| (r.spec, r.operation));
        for r in routes {
            if args.json {
                println!("{}", route_json(r));
            } else {
                println!(
                    "{} {} {} -> {} [{}]",
                    r.method, r.path, r.operation, r.id, r.spec
                );
            }
        }
        return;
    }

    let mut entries: Vec<Entry> = measured_items().iter().map(entry_for).collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    // Duplicate ids would silently merge distinct benchmarks downstream
    // (baselines are keyed by id) — fail loudly instead.
    for pair in entries.windows(2) {
        if pair[0].id == pair[1].id {
            die(&format!(
                "duplicate measured item id {:?} — rename one of the functions \
                 (ids are package + module path + fn name)",
                pair[0].id
            ));
        }
    }
    if let Some(f) = &args.filter {
        entries.retain(|e| e.id.contains(f.as_str()));
    }

    if args.list {
        if entries.is_empty() {
            eprintln!("soothfast runner: warning: no measured items registered/matched");
        }
        for e in &entries {
            if args.json {
                println!(
                    "{{\"type\":\"item\",\"id\":\"{}\",\"group\":\"{}\",\"fingerprint\":\"{:016x}\",\"covers\":\"{}\"}}",
                    esc(&e.id),
                    esc(e.item.group),
                    e.fingerprint,
                    esc(e.item.covers),
                );
            } else {
                println!("{} [{}]", e.id, e.item.group);
            }
        }
        return;
    }

    // Measuring nothing is a setup bug, not a result. The classic footgun: a
    // bench file with only `bench_main!()` never *names* the lib crate, so
    // the linker drops its registrations without any error.
    if entries.is_empty() {
        if measured_items().is_empty() {
            die(
                "no measured items registered — if your #[measured] items live in the \
                 library, add `use <yourcrate> as _;` to benches/soothfast.rs so their \
                 registrations reach the linker",
            );
        } else {
            die(&format!(
                "no measured items match --filter {:?}",
                args.filter.as_deref().unwrap_or("")
            ));
        }
    }

    // Resolve backends: explicit choices fail hard when unavailable; auto/all
    // degrade gracefully but say so (no silent downgrade). callgrind::probe
    // spawns a real valgrind subprocess, so it's only called when the
    // outcome could actually matter.
    let perf = perf_probe();
    let perf_available = perf.is_ok();
    if args.backend == Backend::Perfcnt
        && let Err(e) = &perf
    {
        die(&format!("--backend perfcnt unavailable: {e}"));
    }
    let cg_probe =
        if !args.skip_gating_counters && might_need_callgrind(args.backend, perf_available) {
            Some(callgrind::probe())
        } else {
            None
        };
    if args.backend == Backend::Callgrind
        && let Some(Err(e)) = &cg_probe
    {
        die(&format!("--backend callgrind unavailable: {e}"));
    }
    let cg_available = matches!(cg_probe, Some(Ok(())));
    // Gating counters (perfcnt/callgrind) are deterministic: when the caller
    // already has them from another pass, re-collecting only costs time.
    let (use_perf, use_cg) = if args.skip_gating_counters {
        (false, false)
    } else {
        resolve_backends(args.backend, perf_available, cg_available)
    };
    let use_wall = matches!(
        args.backend,
        Backend::Auto | Backend::All | Backend::Walltime
    );
    let use_alloc = matches!(args.backend, Backend::Auto | Backend::All | Backend::Alloc);
    let gating = if use_perf {
        "perfcnt"
    } else if use_cg {
        "callgrind"
    } else if use_wall {
        "walltime"
    } else {
        "alloc"
    };

    if args.json {
        println!(
            "{{\"type\":\"env\",\"perfcnt\":{},\"perfcnt_detail\":\"{}\",\"gating_backend\":\"{gating}\"}}",
            perf.is_ok(),
            esc(perf.as_ref().err().map(String::as_str).unwrap_or("ok")),
        );
    } else {
        match &perf {
            Ok(()) => println!("env: perfcnt available; gating backend = {gating}"),
            Err(e) => println!("env: perfcnt unavailable ({e}); gating backend = {gating}"),
        }
    }

    if use_wall {
        let noise_pct = walltime::calibrate(args.samples);
        if args.json {
            println!(
                "{{\"type\":\"calibration\",\"backend\":\"walltime\",\"noise_pct\":{noise_pct:.4}}}"
            );
        } else {
            println!("calibration: walltime A/A noise floor {noise_pct:.2}%");
        }
    }

    // Only Ir collection is safe to parallelize: it counts instructions, not
    // time, so concurrent valgrind children cannot disturb each other's
    // numbers. Everything else below stays serial.
    let cg_irs: Vec<Result<u64, String>> = if use_cg {
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        callgrind::measure_all(&ids)
    } else {
        Vec::new()
    };

    for (idx, e) in entries.iter().enumerate() {
        let item = e.item;
        let mut wall: Option<walltime::WallMeasurement> = None;
        let mut allocs: Option<alloc::AllocMeasurement> = None;

        if use_wall {
            let m = walltime::measure(item.runner, args.samples);
            emit_result(
                e,
                "walltime",
                &format!(
                    "\"median_ns\":{},\"mad_ns\":{},\"min_ns\":{},\"max_ns\":{},\"p99_ns\":{}",
                    m.summary.median, m.summary.mad, m.summary.min, m.summary.max, m.p99_ns
                ),
                &format!(
                    "walltime  median={:>12.1}ns  mad={:>8.1}  p99={:.1}ns  iters={}",
                    m.summary.median, m.summary.mad, m.p99_ns, m.iters
                ),
                args.json,
            );
            wall = Some(m);
        }
        #[cfg(target_os = "linux")]
        if use_perf {
            let m = crate::perfcnt::measure(item.runner);
            emit_result(
                e,
                "perfcnt",
                &format!(
                    "\"instructions\":{},\"cycles\":{},\"cache_refs\":{}",
                    m.instructions, m.cycles, m.cache_refs
                ),
                &format!(
                    "perfcnt   instructions={} cycles={} cache_refs={}",
                    m.instructions, m.cycles, m.cache_refs
                ),
                args.json,
            );
        }
        if use_cg {
            match &cg_irs[idx] {
                Ok(ir) => {
                    emit_result(
                        e,
                        "callgrind",
                        &format!("\"ir\":{ir}"),
                        &format!("callgrind ir={ir}"),
                        args.json,
                    );
                    // Standing in for perfcnt (no PMU access)
                    if !use_perf {
                        emit_result(
                            e,
                            "perfcnt",
                            &format!("\"instructions\":{ir},\"cycles\":0,\"cache_refs\":0"),
                            &format!(
                                "perfcnt   instructions={ir} cycles=0 cache_refs=0 (via callgrind)"
                            ),
                            args.json,
                        );
                    }
                }
                Err(err) => die(&format!("callgrind measurement failed for {}: {err}", e.id)),
            }
        }
        if use_alloc {
            let m = alloc::measure(item.runner);
            emit_result(
                e,
                "alloc",
                &format!("\"allocs\":{},\"bytes\":{}", m.allocs, m.bytes),
                &format!("alloc     allocs={} bytes={}", m.allocs, m.bytes),
                args.json,
            );
            allocs = Some(m);
        }
        // Async behavior rides along automatically for async items — counts,
        // deterministic, cheap.
        if item.is_async && matches!(args.backend, Backend::Auto | Backend::All) {
            let m = asyncexec::measure(item.runner);
            emit_result(
                e,
                "asyncexec",
                &format!("\"polls\":{},\"wakes\":{}", m.polls, m.wakes),
                &format!("asyncexec polls={} wakes={}", m.polls, m.wakes),
                args.json,
            );
        }

        evaluate_assertions(e, wall.as_ref(), allocs.as_ref(), use_perf, &args);
    }
}

fn emit_result(e: &Entry, backend: &str, metrics_json: &str, human: &str, json: bool) {
    if json {
        let tolerance = match e.item.tolerance_pct {
            Some(t) => format!(",\"tolerance_pct\":{t}"),
            None => String::new(),
        };
        println!(
            "{{\"type\":\"result\",\"id\":\"{}\",\"group\":\"{}\",\"fingerprint\":\"{:016x}\",\"covers\":\"{}\"{tolerance},\"backend\":\"{backend}\",\"metrics\":{{{metrics_json}}}}}",
            esc(&e.id),
            esc(e.item.group),
            e.fingerprint,
            esc(e.item.covers),
        );
    } else {
        println!("{:<44} {human}", e.id);
    }
}

fn emit_assertion(id: &str, kind: &str, ok: bool, detail: &str, json: bool) {
    if json {
        println!(
            "{{\"type\":\"assertion\",\"id\":\"{}\",\"kind\":\"{kind}\",\"ok\":{ok},\"detail\":\"{}\"}}",
            esc(id),
            esc(detail),
        );
    } else {
        let mark = if ok { "assert ok  " } else { "assert FAIL" };
        println!("{mark} {id} {kind}: {detail}");
    }
}

fn evaluate_assertions(
    e: &Entry,
    wall: Option<&walltime::WallMeasurement>,
    allocs: Option<&alloc::AllocMeasurement>,
    use_perf: bool,
    args: &Args,
) {
    let item = e.item;
    let a = &item.assertions;
    // Declared claims are always evaluated: --backend selects what gets
    // measured/gated, never which claims count. Measure on demand if needed.
    let alloc_on_demand = match (a.max_allocs, allocs) {
        (Some(_), None) => Some(alloc::measure(item.runner)),
        _ => None,
    };
    let allocs = allocs.or(alloc_on_demand.as_ref());
    let wall_on_demand = match (a.p99_ns, wall) {
        (Some(_), None) => Some(walltime::measure(item.runner, args.samples)),
        _ => None,
    };
    let wall = wall.or(wall_on_demand.as_ref());
    if let (Some(max), Some(m)) = (a.max_allocs, allocs) {
        emit_assertion(
            &e.id,
            "alloc",
            m.allocs <= max,
            &format!("allocs {} <= {max}", m.allocs),
            args.json,
        );
    }
    if let (Some(bound), Some(m)) = (a.p99_ns, wall) {
        emit_assertion(
            &e.id,
            "p99",
            m.p99_ns <= bound as f64,
            &format!("p99 {:.0}ns <= {bound}ns", m.p99_ns),
            args.json,
        );
    }
    if let (Some(class), Some(sized)) = (a.complexity, item.sized_runner) {
        // Sweep on the most deterministic available metric.
        let values: Vec<f64> = item
            .sizes
            .iter()
            .map(|&n| sweep_value(sized, n, use_perf, args.samples))
            .collect();
        let outcome = sweep::evaluate(class, item.sizes, &values);
        let sizes_str = format!("{:?}", item.sizes);
        let values_str = values
            .iter()
            .map(|v| format!("{v:.0}"))
            .collect::<Vec<_>>()
            .join(",");
        if args.json {
            println!(
                "{{\"type\":\"sweep\",\"id\":\"{}\",\"claimed\":\"{}\",\"sizes\":{},\"values\":[{}],\"drift\":{:.3},\"ok\":{}}}",
                esc(&e.id),
                esc(class),
                sizes_str.replace(' ', ""),
                values_str,
                outcome.drift,
                outcome.ok,
            );
        }
        emit_assertion(
            &e.id,
            "complexity",
            outcome.ok,
            &format!(
                "claimed O({class}); growth drift x{:.2} over sizes {sizes_str} (limit x{})",
                outcome.drift,
                sweep::DRIFT_LIMIT
            ),
            args.json,
        );
    }
}

#[cfg(target_os = "linux")]
fn sweep_value(sized: fn(&mut Bencher, usize), n: usize, use_perf: bool, samples: u32) -> f64 {
    if use_perf {
        crate::perfcnt::measure_instructions(move |b| sized(b, n)) as f64
    } else {
        walltime::measure(move |b| sized(b, n), samples)
            .summary
            .median
    }
}

#[cfg(not(target_os = "linux"))]
fn sweep_value(sized: fn(&mut Bencher, usize), n: usize, _use_perf: bool, samples: u32) -> f64 {
    walltime::measure(move |b| sized(b, n), samples)
        .summary
        .median
}

/// One `--list-routes --json` record.
///
/// Shape overrides are absent far more often than present, so they serialize
/// as `null` rather than an empty-string sentinel — the consumer can tell
/// "not set" from "set to empty" without a second convention.
fn route_json(r: &soothfast_registry::RouteItem) -> String {
    let opt = |v: Option<&str>| match v {
        Some(s) => format!("\"{}\"", esc(s)),
        None => "null".to_string(),
    };
    let status = match r.status {
        Some(s) => s.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"type\":\"route\",\"id\":\"{}\",\"spec\":\"{}\",\"operation\":\"{}\",\
         \"method\":\"{}\",\"path\":\"{}\",\"request\":{},\"response\":{},\"status\":{},\
         \"params\":{},\"path_params\":{},\"summary\":{}}}",
        esc(r.id),
        esc(r.spec),
        esc(r.operation),
        esc(r.method),
        esc(r.path),
        opt(r.request),
        opt(r.response),
        status,
        opt(r.params),
        opt(r.path_params),
        opt(r.summary),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use soothfast_registry::RouteItem;

    const PLAIN: RouteItem = RouteItem::new(
        "app::get_item",
        "specs/openapi.yaml",
        "getItem",
        "GET",
        "/items/{id}",
    );

    #[test]
    fn skip_gating_counters_flag_parses() {
        let argv = ["--json", "--skip-gating-counters"].map(String::from);
        let args = parse_args_from(argv.into_iter());
        assert!(args.json);
        assert!(args.skip_gating_counters);
        assert!(!parse_args_from(std::iter::empty()).skip_gating_counters);
    }

    #[test]
    fn auto_prefers_perfcnt_when_available() {
        assert_eq!(resolve_backends(Backend::Auto, true, true), (true, false));
    }

    #[test]
    fn auto_falls_back_to_callgrind_on_pmu_less_hosts() {
        assert_eq!(resolve_backends(Backend::Auto, false, true), (false, true));
    }

    #[test]
    fn auto_runs_neither_when_nothing_is_available() {
        assert_eq!(
            resolve_backends(Backend::Auto, false, false),
            (false, false)
        );
    }

    #[test]
    fn all_degrades_the_same_way_as_auto() {
        assert_eq!(resolve_backends(Backend::All, true, true), (true, false));
        assert_eq!(resolve_backends(Backend::All, false, true), (false, true));
        assert_eq!(resolve_backends(Backend::All, false, false), (false, false));
    }

    #[test]
    fn explicit_choices_never_cross_degrade() {
        // Explicit --backend perfcnt never falls back to callgrind, and
        // vice versa — an unavailable explicit choice is the caller's job
        // to reject (die), not silently substitute.
        assert_eq!(
            resolve_backends(Backend::Perfcnt, false, true),
            (false, false)
        );
        assert_eq!(
            resolve_backends(Backend::Callgrind, true, false),
            (false, false)
        );
        assert_eq!(
            resolve_backends(Backend::Walltime, true, true),
            (false, false)
        );
        assert_eq!(resolve_backends(Backend::Alloc, true, true), (false, false));
    }

    #[test]
    fn callgrind_is_only_probed_when_it_could_matter() {
        assert!(might_need_callgrind(Backend::Callgrind, true));
        assert!(might_need_callgrind(Backend::Auto, false));
        assert!(might_need_callgrind(Backend::All, false));
        assert!(!might_need_callgrind(Backend::Auto, true));
        assert!(!might_need_callgrind(Backend::Walltime, false));
        assert!(!might_need_callgrind(Backend::Perfcnt, false));
    }

    #[test]
    fn absent_overrides_serialize_as_null() {
        let json = route_json(&PLAIN);
        assert!(json.contains("\"request\":null"), "{json}");
        assert!(json.contains("\"response\":null"), "{json}");
        assert!(json.contains("\"status\":null"), "{json}");
        assert!(json.contains("\"params\":null"), "{json}");
        assert!(json.contains("\"path_params\":null"), "{json}");
        assert!(json.contains("\"summary\":null"), "{json}");
    }

    #[test]
    fn present_overrides_carry_through() {
        let item = PLAIN.with_shape(Some("NewItem"), Some("Item"), Some(201));
        let json = route_json(&item);
        assert!(json.contains("\"request\":\"NewItem\""), "{json}");
        assert!(json.contains("\"response\":\"Item\""), "{json}");
        assert!(json.contains("\"status\":201"), "{json}");
    }

    #[test]
    fn params_and_summary_carry_through() {
        let item = PLAIN
            .with_params(Some("ItemQuery"))
            .with_path_params(Some("id: ItemId"))
            .with_summary(Some("Get one item."));
        let json = route_json(&item);
        assert!(json.contains("\"params\":\"ItemQuery\""), "{json}");
        assert!(json.contains("\"path_params\":\"id: ItemId\""), "{json}");
        assert!(json.contains("\"summary\":\"Get one item.\""), "{json}");
    }

    #[test]
    fn identity_fields_are_escaped() {
        let item = RouteItem::new("a\"b", "s", "op", "GET", "/x");
        assert!(route_json(&item).contains("a\\\"b"));
    }
}
