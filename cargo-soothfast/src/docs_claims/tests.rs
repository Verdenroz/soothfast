use super::*;
use serde_json::json;

fn entry(reason: Reason, items: &[&str], metrics: &[&str]) -> Unmeasured {
    Unmeasured {
        package: "core".into(),
        reason,
        detail: format!("{} detail", reason.as_str()),
        items: items.iter().map(|s| s.to_string()).collect(),
        metrics: metrics.iter().map(|s| s.to_string()).collect(),
        except_imported: None,
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
    Against::new(b).unwrap().evaluate(expr)
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
fn a_leg_that_saved_nothing_covers_only_its_own_package_s_items() {
    let mut b = baseline(&[entry(Reason::NoMeasurement, &[], &[])]);
    b["owners"] = json!({ "core": ["core::a"] });
    let outcome = read("core::a.alloc.allocs <= 0", &b);
    assert!(matches!(
        outcome,
        Reading::Unmeasured(Reason::NoMeasurement, _)
    ));
    for unclaimed in [
        "core::typo.alloc.allocs <= 0",
        "jmh::x.walltime.median_ns < 1ms",
    ] {
        let outcome = read(unclaimed, &b);
        assert!(
            matches!(outcome, Reading::Decided(Outcome::Failed(_))),
            "{unclaimed}"
        );
    }
}

#[test]
fn a_nameless_failed_gate_covers_no_item() {
    let b = baseline(&[entry(Reason::GateFailed, &[], &[])]);
    let outcome = read("core::a.alloc.allocs <= 0", &b);
    assert!(matches!(outcome, Reading::Decided(Outcome::Failed(_))));
}

#[test]
fn every_covering_entry_counts_whatever_its_order() {
    let ungated = entry(Reason::NotGatedOnEvent, &["core::same"], &[]);
    let failed = entry(Reason::GateFailed, &["core::same"], &[]);
    for order in [[ungated.clone(), failed.clone()], [failed, ungated]] {
        let b = baseline(&order);
        let Reading::Unmeasured(reason, why) = read("core::same.alloc.allocs <= 0", &b) else {
            panic!("a measurable reason keeps the edited-claim rule");
        };
        assert_eq!(reason, Reason::GateFailed);
        assert!(why.contains("not-gated-on-event") && why.contains("gate-failed"));
    }
}

#[test]
fn a_failed_package_with_no_listing_has_its_claims_fail_saying_so() {
    let mut unnamed = entry(Reason::GateFailed, &[], &[]);
    unnamed.detail = "the gate failed, and no listing was available to name core's items".into();
    let b = baseline(&[unnamed]);
    let Reading::Decided(Outcome::Failed(why)) = read("soothfast::x.alloc.allocs <= 0", &b) else {
        panic!("a claim on an unnamed item must fail");
    };
    assert!(why.contains("gate-failed") && why.contains("no listing"));
}

#[test]
fn a_nameless_entry_is_said_as_possibly_related_only_for_an_unowned_item() {
    let mut b = baseline(&[entry(Reason::GateFailed, &[], &[])]);
    b["owners"] = json!({ "core": ["core::owned"] });
    let Reading::Decided(Outcome::Failed(unowned)) = read("core::x.alloc.allocs <= 0", &b) else {
        panic!("a claim on an unowned item fails");
    };
    assert!(unowned.contains("possibly related: core (gate-failed)"));
    let Reading::Decided(Outcome::Failed(owned)) = read("core::owned.alloc.allocs <= 0", &b) else {
        panic!("a claim on an owned item with no number fails");
    };
    assert!(!owned.contains("possibly related"));
}

/// A merged baseline of an ungated event, holding one measured import.
fn ungated_baseline() -> Value {
    let mut ungated = entry(Reason::NotGatedOnEvent, &[], &[]);
    ungated.except_imported = Some(vec!["jmh".into()]);
    let mut b = baseline(&[ungated]);
    b["items"]["jmh::Parse.run"] = json!({ "imported": "jmh", "walltime": { "median_ns": 5.0 } });
    b
}

#[test]
fn on_an_ungated_event_rust_claims_are_not_evaluated_and_imported_ones_are() {
    let b = ungated_baseline();
    let doc = doc_with("soothfast::parse.alloc.allocs <= 0");
    let never_made = || Ok(Claims::new());
    assert_eq!(check(&[("a.md", &doc)], Some(&b), never_made), Ok(0));
    let holds = read("jmh::Parse.run.walltime.median_ns < 10", &b);
    assert_eq!(holds, Reading::Decided(Outcome::Holds(5.0)));
    let missing = read("jmh::Prase.run.walltime.median_ns < 10", &b);
    assert!(matches!(missing, Reading::Decided(Outcome::Failed(_))));
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
    git(&["config", "commit.gpgsign", "false"]);
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
