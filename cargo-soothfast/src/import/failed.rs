//! `import --failed`: the file an import gate that failed or errored leaves
//! for the docs job. It holds no numbers, only a `gate-failed` entry for the
//! prefix, naming the items of HEAD's results when there are any.

use serde_json::{Value, json};

use super::{Format, read};
use crate::baseline_merge::IMPORTED_KEY;
use crate::unmeasured::{Reason, Unmeasured};

/// The stub for results `file` of `format` under `prefix`.
pub fn stub(format: &str, file: &str, prefix: Option<&str>) -> Result<Value, String> {
    let prefix = prefix
        .unwrap_or(Format::parse(format)?.default_prefix())
        .to_string();
    let items: Vec<String> = read(format, file, Some(&prefix))
        .map(|(run, _)| run.items.into_keys().collect())
        .unwrap_or_default();
    let entry = Unmeasured {
        detail: format!("the import gate failed, so {prefix} saved no numbers"),
        all_items: items.is_empty(),
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

    #[test]
    fn a_gate_with_results_names_their_items() {
        let dir =
            std::env::temp_dir().join(format!("soothfast-import-failed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("jmh.json");
        let jmh = r#"[{"benchmark":"a.B.run","mode":"avgt","primaryMetric":{"score":2.0,"scoreUnit":"ns/op","rawData":[[2.0]]}}]"#;
        std::fs::write(&file, jmh).unwrap();
        let doc = stub("jmh", file.to_str().unwrap(), None).unwrap();
        let entry = &unmeasured::read(&doc).unwrap()[0];
        assert_eq!(entry.items.len(), 1);
        assert!(entry.items[0].starts_with("jmh::a.B.run"));
        assert!(!entry.all_items);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gate_without_results_covers_its_prefix() {
        let doc = stub("jmh", "/nonexistent/results.json", Some("app")).unwrap();
        let entry = &unmeasured::read(&doc).unwrap()[0];
        assert!(entry.all_items && entry.items.is_empty());
        assert_eq!(entry.package, "app");
        assert_eq!(doc[IMPORTED_KEY], "app");
    }
}
