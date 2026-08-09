//! `cargo soothfast` — CLI entry point.
//!
//! Phase 2 subcommands: `measure` (bench or buildcost backends, baseline
//! save), `gate` (baseline / merge-base worktree / ratchet comparison with
//! thresholds + triage artifacts), `trend append|render`, `coverage measure`.
//! Fully synchronous; everything is subprocess + file I/O.

mod buildcost;
mod coverage;
mod docs;
mod docs_support;
mod gate;
mod invoke;
mod mcp;
mod report;
mod sdk;
mod sdk_build;
mod sdk_config;
mod site;
mod spec;
mod spec_config;
mod spec_gate;
mod spec_gen;
mod spec_html;
mod trend;
mod workspace;

use invoke::CommonArgs;

const USAGE: &str = "\
usage: cargo soothfast <command>

commands:
  measure  [-p PKG] [--filter S] [--backend auto|all|walltime|alloc|perfcnt|callgrind|buildcost]
           [--samples N] [--features F] [--features-matrix \"default;full\"]
           [--target NAME] [--save-baseline NAME]
  gate     [-p PKG] [--filter S] [--backend B] [--samples N] [--features F]
           [--baseline NAME] [--ratchet NAME] [--against-ref REF] [--deps]
           [--features-matrix M] [--target NAME]
  trend    append|render [-p PKG]
  docs     check|accept|gen-tests -p PKG [--baseline NAME] [--features F] [PATHS...]
  docs     capture -p PKG [--check] [PATHS...]
  docs     diff -p PKG --against-ref REF [--features F]
  docs     reference -p PKG [--baseline NAME] [--features F] [--out DIR]
  docs     routes -p PKG [--baseline NAME] [--features F] [--target NAME] [--out DIR]
  docs     build [--baseline NAME] [--out DIR]   (static site from soothfast.toml [site])
  docs     serve [--baseline NAME] [--addr HOST:PORT]   (dev server + live reload)
  docs     gh-deploy [--baseline NAME] [--branch NAME] [--remote NAME] [--message MSG]
           (build + publish to a gh-pages-style branch, replaces `mkdocs gh-deploy`)
  coverage measure|docs -p PKG [--min PCT] [--features F]
  spec     gen -p PKG [--features F] [--check]
  spec     gate -p PKG [--base REF] [--allow-breaking]
  spec     check -p PKG [--features F] [--target NAME]
  spec     check-proto -p PKG --proto FILE --message NAME --source FILE --struct NAME
           (reconciles a .proto message's fields against a Rust struct's
           #[prost(..)] fields by tag — for consumed, not served, wire formats)
  spec     html -p PKG [--features F] [--target NAME] [--out DIR]
           (renders every generate-mode spec file — OpenAPI/AsyncAPI/MCP —
           as a themed docs-site page; default --out is docs/api)
  sdk      gen -p PKG [--features F] [--target NAME] [--check]
  sdk      build -p PKG [--target TRIPLE].. [--bench NAME] [--debug]
           (cross-compiles the embedded server and stages it per ecosystem;
           note --target is a Rust triple here, so name the bench --bench)
  sdk      gate -p PKG [--base REF] [--allow-breaking]   (alias of spec gate:
           the SDK surface is the linked generate-mode spec surface)
  sdk      publish -p PKG [--only LANG] [--dry-run]   (refuses stale output;
           python publishes via `uv build` + `uv publish`)
  report   render -p PKG [--out DIR] [--baseline NAME] [--features F]
  report   changelog -p PKG [-p PKG ...] [--against-ref REF] [--features F]
           (no --against-ref: first release, lists the surface it ships)
  mcp      -p PKG [--baseline NAME] [--features F]   (agent-facing server on stdio)

`--features F` builds the bench binary / rustdoc surface with those cargo
features enabled (feature-gated items are otherwise invisible).

`--bench NAME` (also spelled `--target NAME` everywhere except `sdk build`,
where cargo's meaning of `--target` wins) picks the `[[bench]]` target to
link the registry from (default \"soothfast\"). Lets `spec check` read
`#[route]` markers from a target separate from the one `measure`/`gate` use
for perf benches, e.g. `benches/soothfast-routes.rs` (target
\"soothfast-routes\") instead of overloading a single `benches/soothfast.rs`.
";

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    // Invoked as `cargo soothfast ...` cargo passes "soothfast" as argv[1].
    if argv.first().map(String::as_str) == Some("soothfast") {
        argv.remove(0);
    }
    let Some(cmd) = argv.first().cloned() else {
        eprint!("{USAGE}");
        return 2;
    };
    let rest = &argv[1..];
    match cmd.as_str() {
        "measure" => cmd_measure(rest),
        "gate" => gate::run(rest),
        "trend" => trend::run(rest),
        "docs" => docs::run(rest),
        "coverage" => coverage::run(rest),
        "spec" => spec::run(rest),
        "sdk" => sdk::run(rest),
        "report" => report::run(rest),
        "mcp" => mcp::run(rest),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            0
        }
        other => {
            eprintln!("unknown command {other:?}\n{USAGE}");
            2
        }
    }
}

fn cmd_measure(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut save: Option<String> = None;
    let mut matrix = String::from("default");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if common.try_parse(a, &mut it) {
            continue;
        }
        match a.as_str() {
            "--save-baseline" => match it.next() {
                Some(n) => save = Some(n.clone()),
                None => return arg_err("--save-baseline needs a name"),
            },
            "--features-matrix" => match it.next() {
                Some(m) => matrix = m.clone(),
                None => return arg_err("--features-matrix needs combos like \"default;full\""),
            },
            other => return arg_err(&format!("unknown measure arg {other:?}")),
        }
    }

    // buildcost is CLI-side (no bench binary); everything else runs the runner.
    let run = if common.backend.as_deref() == Some("buildcost") {
        let Some(pkg) = common.pkg.clone() else {
            return arg_err("--backend buildcost requires -p PKG");
        };
        match buildcost::measure(&pkg, &matrix) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 1;
            }
        }
    } else {
        let records = match invoke::run_bench(&common, &[]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 1;
            }
        };
        let run = invoke::collect(&records);
        if let Some(b) = &run.gating_backend {
            println!("env: gating backend = {b}");
        }
        if let Some(noise) = run.noise_pct {
            println!("calibration: walltime A/A noise floor {noise:.2}%");
        }
        for (id, item) in &run.items {
            if let Some(i) = item.instructions {
                println!(
                    "{id:<44} perfcnt   instructions={i} cycles={} cache_refs={}",
                    item.cycles.unwrap_or(0),
                    item.cache_refs.unwrap_or(0)
                );
            }
            if let Some(ir) = item.ir {
                println!("{id:<44} callgrind ir={ir}");
            }
            if let Some(m) = item.median_ns {
                println!(
                    "{id:<44} walltime  median={m:>12.1}ns  mad={:>8.1}  p99={:.1}ns",
                    item.mad_ns.unwrap_or(0.0),
                    item.p99_ns.unwrap_or(0.0)
                );
            }
            if let (Some(a), Some(b)) = (item.allocs, item.bytes) {
                println!("{id:<44} alloc     allocs={a} bytes={b}");
            }
            if let (Some(p), Some(w)) = (item.polls, item.wakes) {
                println!("{id:<44} asyncexec polls={p} wakes={w}");
            }
        }
        run
    };

    let mut assertion_failures = 0;
    for a in &run.assertions {
        let verdict = if a.ok { "assert ok  " } else { "assert FAIL" };
        if !a.ok {
            assertion_failures += 1;
        }
        println!("{verdict} {} {}: {}", a.id, a.kind, a.detail);
    }
    println!("measured {} item(s)", run.items.len());

    if assertion_failures > 0 {
        // Never persist a run that violates its own claims: a saved baseline
        // would ratify the regression and the next gate would pass on it.
        if save.is_some() {
            eprintln!(
                "soothfast: refusing to save baseline: {assertion_failures} assertion failure(s)"
            );
        }
        println!("measure: {assertion_failures} assertion failure(s)");
        return 1;
    }
    if let Some(name) = save {
        let scope = if common.backend.as_deref() == Some("buildcost") {
            invoke::SaveScope::Buildcost(common.pkg.clone())
        } else if common.filter.is_some() {
            invoke::SaveScope::BenchFiltered
        } else {
            invoke::SaveScope::BenchFull(common.pkg.clone())
        };
        match invoke::save_baseline(&name, &run, scope) {
            Ok(path) => println!("baseline saved: {}", path.display()),
            Err(e) => {
                eprintln!("soothfast: failed to save baseline: {e}");
                return 1;
            }
        }
    }
    0
}

fn arg_err(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}\n{USAGE}");
    2
}
