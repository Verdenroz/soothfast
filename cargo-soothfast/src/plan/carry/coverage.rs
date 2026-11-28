//! Whether a stored baseline still answers the claims the docs make of its
//! lane's items.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use soothfast_docs::claims::{self, Claim};
use soothfast_docs::markdown;

use crate::unmeasured;

/// Every claim of the markdown files in `files`, or none when one cannot be
/// read, since a claim unseen could be the one the baseline lacks. A claim
/// that does not parse is left to the docs check, which fails it either way.
pub(super) fn read(root: &Path, files: &BTreeMap<String, String>) -> Option<Vec<Claim>> {
    let mut all = Vec::new();
    for path in files.keys().filter(|f| f.ends_with(".md")) {
        let text = std::fs::read_to_string(root.join(path)).ok()?;
        let doc = markdown::scan(&text).ok()?;
        all.extend(
            doc.claims
                .iter()
                .filter_map(|c| claims::parse(&c.expr).ok()),
        );
    }
    Some(all)
}

/// Whether `baseline` holds every metric the `claims` read of the items the
/// lane owns: a Rust lane's `owners`, or an import lane's items.
pub(super) fn resolves(lane: &str, baseline: &Value, claims: &[Claim]) -> bool {
    let owns = |item: &str| match lane.strip_prefix("rust:") {
        Some(package) => unmeasured::owned_by(baseline, package)
            .iter()
            .any(|i| i == item),
        None => baseline["items"].get(item).is_some(),
    };
    claims.iter().all(|claim| {
        let minus = claim.minus.iter().map(|m| (&m.item, &m.backend, &m.metric));
        let operands = std::iter::once((&claim.item, &claim.backend, &claim.metric)).chain(minus);
        operands
            .filter(|(item, ..)| owns(item))
            .all(|(item, backend, metric)| {
                let single = Claim {
                    item: item.clone(),
                    backend: backend.clone(),
                    metric: metric.clone(),
                    minus: None,
                    op: claim.op,
                    bound: claim.bound,
                };
                claims::values(&single, baseline).is_ok()
            })
    })
}
