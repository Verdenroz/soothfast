use std::collections::BTreeSet;
use std::path::Path;

use super::{Ctx, Row, State};
use crate::invoke::load_baseline_in;

/// A saved baseline for each package with a bench target.
pub(super) fn check(ctx: &Ctx) -> Vec<Row> {
    let saved = saved_packages(&ctx.root);
    ctx.packages
        .iter()
        .map(|pkg| row(pkg, saved.contains(pkg)))
        .collect()
}

fn row(pkg: &str, saved: bool) -> Row {
    let subject = format!("baseline {pkg}");
    if saved {
        return Row::ok(&subject, "saved under .soothfast/baselines");
    }
    Row::needs(
        State::Missing,
        &subject,
        "no saved baseline",
        format!("cargo soothfast measure -p {pkg} --save-baseline main"),
    )
}

/// The packages any baseline file under `root` holds items saved by.
fn saved_packages(root: &Path) -> BTreeSet<String> {
    let names: Vec<String> = std::fs::read_dir(root.join(".soothfast/baselines"))
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            name.strip_suffix(".json").map(String::from)
        })
        .collect();
    names
        .iter()
        .filter_map(|name| load_baseline_in(root, name).ok().flatten())
        .flat_map(|doc| {
            doc["items"]
                .as_object()
                .into_iter()
                .flat_map(|items| items.values())
                .filter_map(|item| item["saved_by"].as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::scratch;

    fn ctx(tag: &str, packages: &[&str]) -> Ctx {
        Ctx {
            root: scratch(tag),
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn a_package_with_a_saved_item_is_ok() {
        let ctx = ctx("baseline-ok", &["a"]);
        let dir = ctx.root.join(".soothfast/baselines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.json"),
            r#"{"items":{"a::f":{"saved_by":"a"}}}"#,
        )
        .unwrap();
        let rows = check(&ctx);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, State::Ok);
    }

    #[test]
    fn a_package_without_a_baseline_is_missing_with_the_measure_command() {
        let ctx = ctx("baseline-missing", &["a", "b"]);
        let dir = ctx.root.join(".soothfast/baselines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.json"),
            r#"{"items":{"a::f":{"saved_by":"a"}}}"#,
        )
        .unwrap();
        let rows = check(&ctx);
        assert_eq!(rows[0].state, State::Ok);
        assert_eq!(rows[1].state, State::Missing);
        assert_eq!(
            rows[1].fix.as_deref(),
            Some("cargo soothfast measure -p b --save-baseline main")
        );
    }

    #[test]
    fn no_bench_packages_means_no_rows() {
        assert!(check(&ctx("baseline-none", &[])).is_empty());
    }
}
