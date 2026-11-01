//! The claims half of `docs check`. A claim is checked against the
//! baseline's numbers, or reported "not evaluated" when a metric it reads is
//! listed `unmeasured` there and the claim is unchanged since the merge-base.
//! A new or edited claim, or one naming an item no entry lists, still fails
//! when its measurement is missing.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;
use soothfast_docs::{claims, markdown};

use crate::invoke;
use crate::unmeasured::{self, Reason, Unmeasured};

/// What one claim came to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Holds(f64),
    Violated(f64),
    /// An unchanged claim whose metrics went unmeasured, for the reason.
    NotEvaluated(Reason, String),
    Failed(String),
}

/// A claim read against the baseline, before anyone asks whether it changed.
#[derive(Debug, PartialEq)]
pub enum Reading {
    Decided(Outcome),
    /// Every missing metric is listed `unmeasured`, and some for a reason a
    /// measurement was possible under; the first such reason.
    Unmeasured(Reason, String),
}

/// The `(path, expr)` claims the merge-base made.
pub type Claims = BTreeSet<(String, String)>;

/// Check every claim in `docs` (path, scanned doc) against `baseline`, or
/// report each one skipped when `baseline` is `None`. `unchanged` gives the
/// merge-base's claims, asked for only once some claim reads unmeasured.
/// Returns the failures.
pub fn check(
    docs: &[(&str, &markdown::Doc)],
    baseline: Option<&Value>,
    unchanged: impl FnOnce() -> Result<Claims, String>,
) -> Result<u32, String> {
    if let Some(note) = baseline.and_then(|b| unmeasured::legacy_note(b, "the docs baseline")) {
        println!("docs check: {note}");
    }
    let against = baseline.map(Against::new).transpose()?;
    let mut unchanged = Some(unchanged);
    let mut base: Option<Claims> = None;
    let mut failures = 0u32;
    let mut skipped = 0u32;
    for (file, doc) in docs {
        for cm in &doc.claims {
            let at = format!("{file}:{} claim `{}`", cm.line, cm.expr);
            let reading = match &against {
                Some(a) => a.evaluate(&cm.expr),
                None => Reading::Decided(Outcome::NotEvaluated(
                    Reason::ClaimsSkipped,
                    "docs check ran with --skip-claims".into(),
                )),
            };
            let outcome = match reading {
                Reading::Decided(outcome) => outcome,
                Reading::Unmeasured(reason, why) => {
                    if base.is_none()
                        && let Some(f) = unchanged.take()
                    {
                        base = Some(f()?);
                    }
                    let key = (file.to_string(), cm.expr.clone());
                    if base.as_ref().is_some_and(|b| b.contains(&key)) {
                        Outcome::NotEvaluated(reason, why)
                    } else {
                        Outcome::Failed(format!(
                            "new or edited claim with no measurement ({}: {why})",
                            reason.as_str()
                        ))
                    }
                }
            };
            match outcome {
                Outcome::Holds(actual) => println!("ok    {at} (actual {actual:.1})"),
                Outcome::Violated(actual) => {
                    failures += 1;
                    println!("FAIL  {at} VIOLATED (actual {actual:.1})");
                }
                Outcome::NotEvaluated(reason, why) => {
                    skipped += 1;
                    println!("skip  {at}: not evaluated ({}): {why}", reason.as_str());
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

/// A baseline claims are read against, with its entries and owners read once.
pub struct Against<'a> {
    baseline: &'a Value,
    entries: Vec<Unmeasured>,
    owned: BTreeSet<String>,
}

impl<'a> Against<'a> {
    pub fn new(baseline: &'a Value) -> Result<Against<'a>, String> {
        Ok(Against {
            baseline,
            entries: unmeasured::read(baseline)?,
            owned: unmeasured::owned(baseline),
        })
    }

    /// One claim. An item two packages own fails, unchanged or not.
    pub fn evaluate(&self, expr: &str) -> Reading {
        let claim = match claims::parse(expr) {
            Ok(c) => c,
            Err(e) => return Reading::Decided(Outcome::Failed(e)),
        };
        let operands: Vec<(&String, &String, &String)> =
            std::iter::once((&claim.item, &claim.backend, &claim.metric))
                .chain(claim.minus.iter().map(|m| (&m.item, &m.backend, &m.metric)))
                .collect();
        let e = match claims::evaluate(&claim, self.baseline) {
            Ok((true, actual)) => return Reading::Decided(Outcome::Holds(actual)),
            Ok((false, actual)) => return Reading::Decided(Outcome::Violated(actual)),
            Err(e @ claims::Unread::Ambiguous { .. }) => {
                return Reading::Decided(Outcome::Failed(e.to_string()));
            }
            Err(claims::Unread::Other(e)) => e,
        };
        let mut first: Option<Reason> = None;
        let mut measurable: Option<Reason> = None;
        let mut reasons = Vec::new();
        for (item, backend, metric) in operands {
            if self.baseline["items"][item][backend][metric].is_number() {
                continue;
            }
            let covering: Vec<&Unmeasured> = (self.entries.iter())
                .filter(|u| u.covers(item, backend, metric))
                .collect();
            let owned = self.owned.contains(item.as_str());
            if covering.is_empty() {
                let e = match owned {
                    true => e,
                    false => with_unnamed(e, &self.entries),
                };
                return Reading::Decided(Outcome::Failed(e));
            }
            for u in covering {
                first.get_or_insert(u.reason);
                if u.reason.measurable_here() {
                    measurable.get_or_insert(u.reason);
                }
                reasons.push(format!("{item}: {} ({})", u.detail, u.reason.as_str()));
            }
        }
        let why = reasons.join("; ");
        match (measurable, first) {
            (Some(reason), _) => Reading::Unmeasured(reason, why),
            (None, Some(reason)) => Reading::Decided(Outcome::NotEvaluated(reason, why)),
            (None, None) => Reading::Decided(Outcome::Failed(e)),
        }
    }
}

/// `e` for a claim on an item no package owns, with every entry that names
/// no item said beside it as possibly related: a gate that failed with no
/// listing may be why the item has no number, but nothing proves it.
fn with_unnamed(e: String, entries: &[Unmeasured]) -> String {
    let unnamed: Vec<String> = entries
        .iter()
        .filter(|u| u.items.is_empty() && u.except_imported.is_none())
        .map(|u| format!("{} ({}): {}", u.package, u.reason.as_str(), u.detail))
        .collect();
    if unnamed.is_empty() {
        e
    } else {
        format!("{e}; possibly related: {}", unnamed.join("; "))
    }
}

/// The `(path, expr)` claims each of `files` made at the merge-base of HEAD
/// and `against`. A file the merge-base lacks made none.
pub fn unchanged_since(root: &Path, against: &str, files: &[&str]) -> Result<Claims, String> {
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
mod tests;
