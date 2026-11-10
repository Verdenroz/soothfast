use std::collections::BTreeSet;
use std::path::Path;

use super::{Ctx, Row};
use crate::invoke::load_baseline_in;

/// Which packages with a bench target have a baseline saved locally. Never
/// needs action: CI measures the baselines a pull request is gated on.
pub(super) fn check(ctx: &Ctx) -> Vec<Row> {
    let saved = saved_packages(&ctx.root);
    ctx.packages
        .iter()
        .map(|pkg| row(pkg, saved.contains(pkg)))
        .collect()
}

fn row(pkg: &str, saved: bool) -> Row {
    let detail = if saved {
        "saved under .soothfast/baselines"
    } else {
        "no baseline saved locally"
    };
    Row::ok(&format!("baseline {pkg}"), detail)
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
    use crate::status::{State, scratch};

    fn ctx(tag: &str, packages: &[&str]) -> Ctx {
        Ctx {
            root: scratch(tag),
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn saved_main(ctx: &Ctx) {
        let dir = ctx.root.join(".soothfast/baselines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.json"),
            r#"{"items":{"a::f":{"saved_by":"a"}}}"#,
        )
        .unwrap();
    }

    #[test]
    fn a_package_with_a_saved_item_reports_it() {
        let ctx = ctx("baseline-saved", &["a"]);
        saved_main(&ctx);
        let rows = check(&ctx);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].detail, "saved under .soothfast/baselines");
    }

    #[test]
    fn a_package_without_a_baseline_is_ok_and_says_none_is_saved() {
        let ctx = ctx("baseline-none-saved", &["a", "b"]);
        saved_main(&ctx);
        let rows = check(&ctx);
        assert_eq!(rows[1].state, State::Ok);
        assert!(rows[1].fix.is_none());
        assert_eq!(rows[1].detail, "no baseline saved locally");
    }

    #[test]
    fn no_bench_packages_means_no_rows() {
        assert!(check(&ctx("baseline-none", &[])).is_empty());
    }
}
