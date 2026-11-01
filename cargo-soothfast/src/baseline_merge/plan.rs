//! `baseline plan`: the items each package's gate leg will measure, listed
//! where the bench binaries are built, so the docs merge can name the items
//! of a leg that uploaded nothing.

use std::path::Path;

use serde_json::{Value, json};

use super::fail;
use crate::{gate_config, invoke};

/// The key marking a plan file, `{"plan": true, "listed": {pkg: [ids]}}`.
pub const PLAN_KEY: &str = "plan";

/// `baseline plan --out FILE -p PKG`: add PKG's listing to plan FILE.
pub fn cmd(args: &[String]) -> i32 {
    let mut common = invoke::CommonArgs::default();
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if common.try_parse(a, &mut it) {
            continue;
        }
        match a.as_str() {
            "--out" => out = it.next().cloned(),
            other => return fail(&format!("unknown baseline plan arg {other:?}")),
        }
    }
    let (Some(out), Some(pkg)) = (out, common.pkg.clone()) else {
        return fail("baseline plan needs --out FILE and -p PKG");
    };
    if let Err(e) = gate_config::apply(&mut common) {
        return fail(&e);
    }
    let listed = invoke::bench_executable(&common, None, None)
        .and_then(|exe| invoke::listed_ids(&exe, common.filter.as_deref()));
    let Some(ids) = listed else {
        return fail(&format!("cannot list {pkg}'s items"));
    };
    match add(Path::new(&out), &pkg, &ids) {
        Ok(()) => 0,
        Err(e) => fail(&format!("{out}: {e}")),
    }
}

/// Record `pkg`'s listed `ids` in plan file `path`.
pub fn add(path: &Path, pkg: &str, ids: &[String]) -> Result<(), String> {
    let mut doc = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string())?,
        Err(_) => json!({ "version": 1, PLAN_KEY: true, "listed": {} }),
    };
    doc["listed"][pkg] = json!(ids);
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// Whether `doc` is a plan file.
pub fn is_plan(doc: &Value) -> bool {
    doc[PLAN_KEY] == true
}
