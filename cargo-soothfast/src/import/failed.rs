//! `import --failed`: the file an import gate that failed or errored leaves
//! for the docs job. It holds no numbers, only a `gate-failed` entry for the
//! prefix, naming the items of HEAD's results, else of the merge-base's,
//! else the prefix's ids in a prior baseline. With none of them it names
//! nothing, and a claim on the prefix's items fails, unchanged or not.

use std::path::Path;

use serde_json::{Value, json};

use super::{Format, read};
use crate::baseline_merge::IMPORTED_KEY;
use crate::invoke;
use crate::unmeasured::{Reason, Unmeasured};

/// Where else a failed import's item names may come from.
#[derive(Default)]
pub struct Fallbacks<'a> {
    /// The merge-base's results file, same format.
    pub base_results: Option<&'a str>,
    /// A baseline under `root` whose items carry the prefix.
    pub prior: Option<(&'a Path, &'a str)>,
}

/// The stub for results `file` of `format` under `prefix`.
pub fn stub(
    format: &str,
    file: &str,
    prefix: Option<&str>,
    fallbacks: &Fallbacks,
) -> Result<Value, String> {
    let prefix = prefix
        .unwrap_or(Format::parse(format)?.default_prefix())
        .to_string();
    let results = |file: &str| -> Option<Vec<String>> {
        let (run, _) = read(format, file, Some(&prefix)).ok()?;
        Some(run.items.into_keys().collect())
    };
    let named = results(file)
        .map(|ids| (ids, "HEAD's results"))
        .or_else(|| {
            fallbacks
                .base_results
                .and_then(results)
                .map(|ids| (ids, "the merge-base's results"))
        })
        .or_else(|| {
            let (root, name) = fallbacks.prior?;
            let doc = invoke::load_baseline_in(root, name).ok()??;
            let items = doc["items"].as_object()?;
            let ids: Vec<String> = items
                .iter()
                .filter(|(_, v)| v["imported"].as_str() == Some(prefix.as_str()))
                .map(|(id, _)| id.clone())
                .collect();
            (!ids.is_empty()).then_some((ids, "the prior baseline"))
        });
    let (items, detail) = match named {
        Some((ids, source)) => (
            ids,
            format!(
                "the import gate failed, so {prefix} saved no numbers; items named from {source}"
            ),
        ),
        None => (
            Vec::new(),
            format!(
                "the import gate failed, and no results were available to name {prefix}'s items"
            ),
        ),
    };
    let entry = Unmeasured {
        detail,
        package: prefix.clone(),
        reason: Reason::GateFailed,
        items,
        ..Unmeasured::default()
    };
    Ok(json!({
        "version": 1,
        IMPORTED_KEY: prefix,
        "items": {},
        "unmeasured": [entry.to_json()],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unmeasured;

    const JMH: &str = r#"[{"benchmark":"a.B.run","mode":"avgt","primaryMetric":{"score":2.0,"scoreUnit":"ns/op","rawData":[[2.0]]}}]"#;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_gate_with_results_names_their_items() {
        let dir = scratch("failed-head");
        let file = dir.join("jmh.json");
        std::fs::write(&file, JMH).unwrap();
        let doc = stub("jmh", file.to_str().unwrap(), None, &Fallbacks::default()).unwrap();
        let entry = &unmeasured::read(&doc).unwrap()[0];
        assert_eq!(entry.items.len(), 1);
        assert!(entry.items[0].starts_with("jmh::a.B.run"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_no_head_results_the_merge_base_s_name_the_items() {
        let dir = scratch("failed-base");
        let base = dir.join("base.json");
        std::fs::write(&base, JMH).unwrap();
        let fallbacks = Fallbacks {
            base_results: base.to_str(),
            prior: None,
        };
        let doc = stub("jmh", "/nonexistent/results.json", None, &fallbacks).unwrap();
        let entry = &unmeasured::read(&doc).unwrap()[0];
        assert!(entry.items[0].starts_with("jmh::a.B.run"));
        assert!(entry.detail.contains("merge-base"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_no_results_at_all_a_prior_baseline_names_the_prefix_s_items() {
        let dir = scratch("failed-prior");
        let prior = json!({ "items": {
            "app::x": { "imported": "app" },
            "core::y": { "alloc": { "allocs": 0 } },
        } });
        invoke::write_baseline_in(&dir, "head", &prior).unwrap();
        let fallbacks = Fallbacks {
            base_results: None,
            prior: Some((&dir, "head")),
        };
        let doc = stub("jmh", "/nonexistent/results.json", Some("app"), &fallbacks).unwrap();
        let entry = &unmeasured::read(&doc).unwrap()[0];
        assert_eq!(entry.items, ["app::x"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gate_without_any_results_names_nothing_and_says_so() {
        let doc = stub(
            "jmh",
            "/nonexistent/results.json",
            Some("app"),
            &Fallbacks::default(),
        )
        .unwrap();
        let entry = &unmeasured::read(&doc).unwrap()[0];
        assert!(entry.items.is_empty() && entry.detail.contains("no results"));
        assert_eq!(entry.package, "app");
        assert_eq!(doc[IMPORTED_KEY], "app");
    }
}
