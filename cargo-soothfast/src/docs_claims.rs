//! The claims half of `docs check`. A claim is checked against the
//! baseline's numbers, or reported "not evaluated" when an item it reads is
//! listed `unmeasured` there and the claim is unchanged since the merge-base.
//! A new or edited claim, or one naming an item no entry lists, still fails
//! when its measurement is missing.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;
use soothfast_docs::{claims, markdown};

use crate::invoke;
use crate::unmeasured::{self, Unmeasured};

/// What one claim came to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Holds(f64),
    Violated(f64),
    /// An unchanged claim whose items went unmeasured, with the reasons.
    NotEvaluated(String),
    Failed(String),
}

/// Check every claim in `docs` (path, scanned doc) against `baseline`, or
/// report each one not evaluated when `baseline` is `None`. `unchanged` holds
/// the `(path, expr)` claims the merge-base made. Returns the failures.
pub fn check(
    docs: &[(&str, &markdown::Doc)],
    baseline: Option<&Value>,
    unchanged: &BTreeSet<(String, String)>,
) -> Result<u32, String> {
    let entries = baseline
        .map(unmeasured::read)
        .transpose()?
        .unwrap_or_default();
    let mut failures = 0u32;
    let mut skipped = 0u32;
    for (file, doc) in docs {
        for cm in &doc.claims {
            let at = format!("{file}:{} claim `{}`", cm.line, cm.expr);
            let outcome = match baseline {
                Some(b) => {
                    let same = unchanged.contains(&(file.to_string(), cm.expr.clone()));
                    evaluate(&cm.expr, b, &entries, same)
                }
                None => Outcome::NotEvaluated("no measurement".into()),
            };
            match outcome {
                Outcome::Holds(actual) => println!("ok    {at} (actual {actual:.1})"),
                Outcome::Violated(actual) => {
                    failures += 1;
                    println!("FAIL  {at} VIOLATED (actual {actual:.1})");
                }
                Outcome::NotEvaluated(why) => {
                    skipped += 1;
                    println!("skip  {at}: not evaluated: {why}");
                }
                Outcome::Failed(why) => {
                    failures += 1;
                    println!("FAIL  {at}: {why}");
                }
            }
        }
    }
    if skipped > 0 {
        println!("docs check: {skipped} claim(s) not evaluated");
    }
    Ok(failures)
}

/// One claim against `baseline`. `unchanged` says the merge-base made the
/// same claim in the same file.
pub fn evaluate(expr: &str, baseline: &Value, entries: &[Unmeasured], unchanged: bool) -> Outcome {
    let claim = match claims::parse(expr) {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(e),
    };
    let e = match claims::evaluate(&claim, baseline) {
        Ok((true, actual)) => return Outcome::Holds(actual),
        Ok((false, actual)) => return Outcome::Violated(actual),
        Err(e) => e,
    };
    let operands = std::iter::once((&claim.item, &claim.backend, &claim.metric))
        .chain(claim.minus.iter().map(|m| (&m.item, &m.backend, &m.metric)));
    let missing: Vec<&String> = operands
        .filter(|(item, backend, metric)| !baseline["items"][*item][*backend][*metric].is_number())
        .map(|(item, _, _)| item)
        .collect();
    let mut reasons = Vec::new();
    for item in &missing {
        match entries.iter().find(|u| u.items.contains(item)) {
            Some(u) => reasons.push(format!("{item}: {}", u.detail)),
            None => {
                let package = entries
                    .iter()
                    .find(|u| u.items.is_empty() && u.package == invoke::id_pkg(item));
                return Outcome::Failed(match package {
                    Some(u) => format!("{e} ({})", u.detail),
                    None => e,
                });
            }
        }
    }
    let reasons = reasons.join("; ");
    if unchanged {
        Outcome::NotEvaluated(reasons)
    } else {
        Outcome::Failed(format!(
            "new or edited claim with no measurement ({reasons})"
        ))
    }
}

/// The `(path, expr)` claims each of `files` made at the merge-base of HEAD
/// and `against`. A file the merge-base lacks made none.
pub fn unchanged_since(
    root: &Path,
    against: &str,
    files: &[&str],
) -> Result<BTreeSet<(String, String)>, String> {
    let base = invoke::git_in(root, &["merge-base", "HEAD", against])
        .map_err(|e| format!("no merge-base with {against} to compare claims with: {e}"))?;
    let mut out = BTreeSet::new();
    for file in files {
        let Ok(text) = invoke::git_in(root, &["show", &format!("{base}:./{file}")]) else {
            continue;
        };
        if let Ok(doc) = markdown::scan(&text) {
            out.extend(doc.claims.into_iter().map(|c| (file.to_string(), c.expr)));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unmeasured::Reason;
    use serde_json::json;

    fn baseline() -> Value {
        let entry = Unmeasured {
            package: "core".into(),
            reason: Reason::IdenticalBinaries,
            detail: "bench binaries identical".into(),
            items: vec!["core::same".into()],
        };
        json!({
            "items": { "core::measured": { "alloc": { "allocs": 0, "bytes": 0 } } },
            "unmeasured": [entry.to_json()],
        })
    }

    fn entries(doc: &Value) -> Vec<Unmeasured> {
        unmeasured::read(doc).unwrap()
    }

    #[test]
    fn a_measured_claim_is_checked() {
        let b = baseline();
        let holds = evaluate("core::measured.alloc.allocs <= 0", &b, &entries(&b), true);
        assert_eq!(holds, Outcome::Holds(0.0));
        let violated = evaluate("core::measured.alloc.allocs < 0", &b, &entries(&b), true);
        assert_eq!(violated, Outcome::Violated(0.0));
    }

    #[test]
    fn an_unchanged_claim_on_an_unmeasured_item_is_not_evaluated_by_name() {
        let b = baseline();
        let outcome = evaluate("core::same.alloc.allocs <= 0", &b, &entries(&b), true);
        assert_eq!(
            outcome,
            Outcome::NotEvaluated("core::same: bench binaries identical".into())
        );
    }

    #[test]
    fn a_new_or_edited_claim_on_an_unmeasured_item_still_fails() {
        let b = baseline();
        let outcome = evaluate("core::same.alloc.allocs <= 0", &b, &entries(&b), false);
        assert!(matches!(outcome, Outcome::Failed(_)));
    }

    #[test]
    fn a_misspelled_item_fails_even_in_an_unmeasured_package() {
        let b = baseline();
        let outcome = evaluate("core::sme.alloc.allocs <= 0", &b, &entries(&b), true);
        assert!(matches!(outcome, Outcome::Failed(_)));
    }

    #[test]
    fn a_package_with_no_named_items_fails_with_its_reason() {
        let entry = Unmeasured {
            package: "core".into(),
            reason: Reason::NoMeasurement,
            detail: "the gate saved no measurement of core".into(),
            items: Vec::new(),
        };
        let b = json!({ "items": {}, "unmeasured": [entry.to_json()] });
        let Outcome::Failed(why) = evaluate("core::a.alloc.allocs <= 0", &b, &entries(&b), true)
        else {
            panic!("expected a failure");
        };
        assert!(why.contains("the gate saved no measurement of core"));
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
        assert_eq!(unchanged, BTreeSet::from([("a.md".to_string(), expr)]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_difference_needs_both_items_unmeasured_to_be_skipped() {
        let b = baseline();
        let outcome = evaluate(
            "core::same.alloc.allocs - core::gone.alloc.allocs <= 0",
            &b,
            &entries(&b),
            true,
        );
        assert!(matches!(outcome, Outcome::Failed(_)));
    }
}
