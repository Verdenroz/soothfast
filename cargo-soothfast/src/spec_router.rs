//! Advisory router check for `mode = "generate"` OpenAPI specs: what the
//! package's axum-style router serves against what the spec declares.

use std::path::{Path, PathBuf};

use soothfast_spec::router::{compare, scan_routes};
use soothfast_spec::{DeclaredOp, SpecKind};

use crate::spec_config::{Mode, SpecConfig};

/// `note:` lines for one spec file; empty when the spec is not a generated
/// OpenAPI file or the router matches it.
pub(crate) fn notes(
    pkg_dir: &Path,
    config: &SpecConfig,
    spec_file: &str,
    kind: SpecKind,
    declared: &[DeclaredOp],
) -> Vec<String> {
    if kind != SpecKind::OpenApi || config.mode_of(spec_file) != Mode::Generate {
        return Vec::new();
    }
    let sources = rust_sources(&pkg_dir.join("src"));
    drift_notes(spec_file, &sources, declared)
}

fn drift_notes(spec_file: &str, sources: &[String], declared: &[DeclaredOp]) -> Vec<String> {
    let routes: Vec<_> = sources.iter().flat_map(|s| scan_routes(s)).collect();
    if routes.is_empty() {
        return vec![format!(
            "note  {spec_file}: no axum-style `.route(\"..\", verb(..))` registrations found, router check skipped"
        )];
    }
    let drift = compare(&routes, declared);
    let unspecified = drift.unspecified.iter().map(|r| {
        format!(
            "note  {spec_file}: router serves {} {}, the spec lacks it",
            r.method, r.path
        )
    });
    let unserved = drift.unserved.iter().map(|d| {
        format!(
            "note  {spec_file}: spec declares `{}` ({} {}), no router registration serves it",
            d.operation, d.method, d.path
        )
    });
    unspecified.chain(unserved).collect()
}

fn rust_sources(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect(dir, &mut files);
    files.sort();
    files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .collect()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(method: &str, path: &str) -> DeclaredOp {
        DeclaredOp {
            operation: "op".into(),
            method: method.into(),
            path: path.into(),
        }
    }

    #[test]
    fn a_matching_router_has_nothing_to_say() {
        let src = vec![r#"r.route("/a", get(a))"#.to_string()];
        assert!(drift_notes("o.yaml", &src, &[op("GET", "/v2/a")]).is_empty());
    }

    #[test]
    fn drift_is_reported_both_ways_as_notes() {
        let src = vec![r#"r.route("/a", get(a))"#.to_string()];
        let notes = drift_notes("o.yaml", &src, &[op("GET", "/v2/b")]);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|n| n.starts_with("note  o.yaml")));
    }

    #[test]
    fn another_framework_is_skipped_with_a_note() {
        let src = vec!["#[get(\"/a\")] async fn a() {}".to_string()];
        let notes = drift_notes("o.yaml", &src, &[op("GET", "/a")]);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("router check skipped"));
    }
}
