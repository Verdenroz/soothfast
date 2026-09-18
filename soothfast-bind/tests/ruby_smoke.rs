//! Builds the Ruby golden into a real gem extension and exercises it.
//!
//! Ignored by default: it needs a Ruby toolchain (`ruby`, `bundler`,
//! `rb_sys`) reachable, which the rest of the suite does not require.
//! Unlike the other smoke tests, it skips rather than fails when `ruby` or
//! `bundle` is missing, since that is this machine's normal state, not a
//! broken one. Run with:
//! `cargo test -p soothfast-bind --test ruby_smoke -- --ignored`
//!
//! The golden's `Cargo.toml` depends on `acme` at `path = ".."`, so this
//! copies `tests/fixture_crate` one directory above the copied golden, the
//! same layout `bind gen` produces for a real package.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("makes a directory");
    for entry in std::fs::read_dir(src).expect("reads the golden") {
        let entry = entry.expect("reads a directory entry");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies a golden file");
        }
    }
}

const SMOKE_SCRIPT: &str = r#"
require "acme_core"

counter = AcmeCore::Counter.new(10)
raise "value" unless counter.value == 10
raise "bump" unless counter.bump(5) == 15
raise "bump_all" unless counter.bump_all([1, 2, 3]) == 16
raise "at low" unless counter.at(:low) == 10
raise "at high" unless counter.at(:high) == 20

begin
  AcmeCore::Counter.new(9223372036854775807).bump(1)
  raise "expected an error"
rescue AcmeCore::Error => e
  raise "message" unless e.message == "overflow"
end

raise "digest" unless AcmeCore.digest("abc".b) == "bcd".b
raise "normalize" unless AcmeCore.normalize([1.0, 2.0], 2.0) == [2.0, 4.0]

scaled = [0.0, 0.0, 0.0]
AcmeCore.scale_into([1.0, 2.0, 3.0], 2.0, scaled)
raise "scale_into" unless scaled == [2.0, 4.0, 6.0]

raise "greet" unless AcmeCore.greet("world") == "hello, world"

raise "peak_level" unless AcmeCore.peak_level([0.1, 0.9, 0.3]) == :high

found = AcmeCore.find_counter(5)
raise "find_counter" unless found.value == 5
raise "find_counter absent" unless AcmeCore.find_counter(-1).nil?

raise "describe" unless AcmeCore.describe("world") == "label=world"
raise "describe absent" unless AcmeCore.describe(nil).nil?

raise "describe_owned" unless AcmeCore.describe_owned("world") == "owned=world"
raise "describe_owned absent" unless AcmeCore.describe_owned(nil).nil?

raise "maybe_ratio" unless AcmeCore.maybe_ratio(4) == 0.25
raise "maybe_ratio absent" unless AcmeCore.maybe_ratio(-1).nil?

puts "ok"
"#;

#[test]
#[ignore = "shells out to bundle/rake/ruby; run with --ignored"]
fn the_ruby_golden_builds_and_runs() {
    for (tool, hint) in [
        (
            "ruby",
            "https://www.ruby-lang.org/en/documentation/installation/",
        ),
        ("bundle", "gem install bundler"),
    ] {
        let available = Command::new(tool).arg("--version").output().is_ok();
        if !support::require_toolchain(available, tool, hint) {
            return;
        }
    }

    let root = build_scratch();
    let glue = root.join("glue");

    bundle_install(&glue);
    rake_compile(&glue);

    let output = run_smoke_script(&glue);
    assert!(
        output.status.success(),
        "ruby exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ok"));

    let _ = std::fs::remove_dir_all(&root);
}

fn build_scratch() -> PathBuf {
    let root = std::env::temp_dir().join(format!("soothfast-ruby-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &root);
    copy_dir(
        &manifest_dir().join("tests/goldens/ruby"),
        &root.join("glue"),
    );
    root
}

fn bundle_install(glue: &Path) {
    let local_install = Command::new("bundle")
        .args(["install", "--local"])
        .current_dir(glue)
        .status()
        .expect("runs bundle install --local");
    if !local_install.success() {
        let status = Command::new("bundle")
            .arg("install")
            .current_dir(glue)
            .status()
            .expect("runs bundle install");
        assert!(status.success(), "bundle install failed");
    }
}

fn rake_compile(glue: &Path) {
    let compile = Command::new("bundle")
        .args(["exec", "rake", "compile"])
        .current_dir(glue)
        .status()
        .expect("runs bundle exec rake compile");
    assert!(compile.success(), "rake compile failed");
}

fn run_smoke_script(glue: &Path) -> Output {
    let script = glue.join("smoke.rb");
    std::fs::write(&script, SMOKE_SCRIPT).expect("writes smoke script");
    Command::new("ruby")
        .arg("-Ilib")
        .arg(&script)
        .current_dir(glue)
        .output()
        .expect("runs ruby")
}
