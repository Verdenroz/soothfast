use super::*;
use serde_json::json;

fn entry(reason: Reason, items: &[&str], metrics: &[&str]) -> Unmeasured {
    Unmeasured {
        package: "core".into(),
        reason,
        detail: format!("{} detail", reason.as_str()),
        items: items.iter().map(|s| s.to_string()).collect(),
        metrics: metrics.iter().map(|s| s.to_string()).collect(),
        all_items: false,
        shard: None,
    }
}

fn baseline(entries: &[Unmeasured]) -> Value {
    json!({
        "items": {
            "core::measured": { "alloc": { "allocs": 0, "bytes": 0 } },
            "core::cached": { "callgrind": { "ir": 900 } },
        },
        "unmeasured": entries.iter().map(Unmeasured::to_json).collect::<Vec<_>>(),
    })
}

fn read(expr: &str, b: &Value) -> Reading {
    evaluate(expr, b, &unmeasured::read(b).unwrap())
}

#[test]
fn a_measured_claim_is_checked() {
    let b = baseline(&[]);
    let holds = read("core::measured.alloc.allocs <= 0", &b);
    assert_eq!(holds, Reading::Decided(Outcome::Holds(0.0)));
    let violated = read("core::measured.alloc.allocs < 0", &b);
    assert_eq!(violated, Reading::Decided(Outcome::Violated(0.0)));
}

#[test]
fn reused_counters_are_checked_and_their_walltime_reads_unmeasured() {
    let b = baseline(&[entry(
        Reason::ReusedWalltime,
        &["core::cached"],
        &["walltime"],
    )]);
    let ir = read("core::cached.callgrind.ir < 1000", &b);
    assert_eq!(ir, Reading::Decided(Outcome::Holds(900.0)));
    let wall = read("core::cached.walltime.median_ns < 1ms", &b);
    assert!(matches!(
        wall,
        Reading::Unmeasured(Reason::ReusedWalltime, _)
    ));
}

#[test]
fn a_misspelled_item_fails_even_in_an_unmeasured_package() {
    let b = baseline(&[entry(Reason::GateFailed, &["core::same"], &[])]);
    let outcome = read("core::sme.alloc.allocs <= 0", &b);
    assert!(matches!(outcome, Reading::Decided(Outcome::Failed(_))));
}

#[test]
fn a_package_with_no_named_items_fails_with_its_reason() {
    let b = baseline(&[entry(Reason::NoMeasurement, &[], &[])]);
    let Reading::Decided(Outcome::Failed(why)) = read("core::a.alloc.allocs <= 0", &b) else {
        panic!("expected a failure");
    };
    assert!(why.contains("no-measurement"));
}

#[test]
fn a_difference_needs_every_missing_side_listed() {
    let b = baseline(&[entry(Reason::GateFailed, &["core::same"], &[])]);
    let both = read("core::same.alloc.allocs - core::gone.alloc.allocs <= 0", &b);
    assert!(matches!(both, Reading::Decided(Outcome::Failed(_))));
}

fn doc_with(expr: &str) -> markdown::Doc {
    markdown::scan(&format!("<!-- soothfast:claim {expr} -->\n")).unwrap()
}

#[test]
fn only_an_unchanged_claim_on_an_unmeasured_item_is_not_evaluated() {
    let b = baseline(&[entry(Reason::GateFailed, &["core::same"], &[])]);
    let expr = "core::same.alloc.allocs <= 0";
    let doc = doc_with(expr);
    let docs = [("a.md", &doc)];
    let base = || Ok(Claims::from([("a.md".to_string(), expr.to_string())]));
    assert_eq!(check(&docs, Some(&b), base).unwrap(), 0);
    assert_eq!(check(&docs, Some(&b), || Ok(Claims::new())).unwrap(), 1);
}

#[test]
fn the_merge_base_is_read_only_for_an_unmeasured_claim() {
    let b = baseline(&[]);
    let doc = doc_with("core::measured.alloc.allocs <= 0");
    let never = || -> Result<Claims, String> { panic!("read the merge-base") };
    assert_eq!(check(&[("a.md", &doc)], Some(&b), never).unwrap(), 0);
}

#[test]
fn skipped_claims_are_reported_with_a_typed_reason() {
    let doc = doc_with("core::anything.alloc.allocs <= 0");
    let never = || -> Result<Claims, String> { panic!("read the merge-base") };
    assert_eq!(check(&[("a.md", &doc)], None, never).unwrap(), 0);
}

#[test]
fn only_a_claim_the_merge_base_made_counts_as_unchanged() {
    let dir = std::env::temp_dir().join(format!("soothfast-claims-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| invoke::git_in(&dir, args).unwrap();
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    let old = "<!-- soothfast:claim core::a.alloc.allocs <= 0 -->\n";
    std::fs::write(dir.join("a.md"), old).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    let new = format!("{old}<!-- soothfast:claim core::b.alloc.allocs <= 0 -->\n");
    std::fs::write(dir.join("a.md"), new).unwrap();
    std::fs::write(dir.join("b.md"), old).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "head"]);
    let unchanged = unchanged_since(&dir, "HEAD~1", &["a.md", "b.md"]).unwrap();
    let expr = "core::a.alloc.allocs <= 0".to_string();
    assert_eq!(unchanged, Claims::from([("a.md".to_string(), expr)]));
    let _ = std::fs::remove_dir_all(&dir);
}
