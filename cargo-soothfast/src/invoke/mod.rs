//! Subprocess plumbing: run the user's soothfast bench binary via `cargo bench`,
//! parse its JSONL, and read/write baseline files.

use std::time::Instant;

mod args;
mod baseline;
mod bench;
mod git;
mod harness;
mod hostenv;
mod root;
mod run;
mod rustdoc;
mod worktree;

pub use args::{CommonArgs, Shard};
pub use baseline::{
    SaveScope, baseline_path, id_pkg, load_baseline, load_baseline_in, save_baseline,
    save_baseline_in, write_baseline_in,
};
pub(crate) use bench::{Build, bench_artifact, bench_build_command, bench_toolchain};
pub use bench::{
    bench_executable, build_link_map, cargo_messages, listed_ids, prebuild_benches, run_bench,
    run_bench_dir, run_bench_in, run_bench_raw, run_discovery, run_discovery_in,
};
pub use git::{git, git_in};
pub(crate) use harness::lock_packages;
pub use harness::{HarnessSync, harness_versions, sync_harness_versions, tree_is_clean};
#[cfg(test)]
pub use hostenv::Guest;
pub use hostenv::{HostEnv, host_env, list_items};
pub use root::{
    PkgMeta, buildcost_base_target_dir, buildcost_target_dir, pkg_dir, pkg_meta, project_root,
    target_dir, workspace_root, worktree_target_dir,
};
use run::saved_items;
pub use run::{
    AssertionOutcome, ItemMetrics, Reused, Run, collect, pair, run_from_items_value,
    run_to_items_value,
};
pub(crate) use rustdoc::{RUSTDOC_FORMAT_VERSION, crate_dirs};
pub use rustdoc::{Visibility, rustdoc_holds, rustdoc_json_in, rustdoc_key};
pub use worktree::{
    forget_missing_worktree, sync_untracked_cargo_config, with_merge_base_worktree,
};

/// Run `f`, printing its wall time to stderr when `SOOTHFAST_TIMINGS` is set.
pub fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    if std::env::var_os("SOOTHFAST_TIMINGS").is_none() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    eprintln!(
        "soothfast-timing: {label} {:.1}s",
        start.elapsed().as_secs_f64()
    );
    out
}
