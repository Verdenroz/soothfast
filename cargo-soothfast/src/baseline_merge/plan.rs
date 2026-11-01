//! `baseline plan`: the items each package's gate leg will measure, listed
//! where the bench binaries are built, so the docs merge can name the items
//! of a leg that uploaded nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use super::fail;
use crate::{benchexe, gate_config, invoke};

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
    let exe = if benchexe::storable(&common, invoke::Build::Measure) {
        benchexe::stored(&common, None, None, invoke::Build::Measure)
    } else {
        benchexe::already_built(&common)
    };
    let ids = match benchexe::listing(&common, exe.as_deref()) {
        Ok(ids) => ids,
        Err(e) => return fail(&e),
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

/// Each package's planned ids.
pub type Planned = BTreeMap<String, BTreeSet<String>>;

/// Add the plan file at `path` to `planned`.
pub fn read_into(path: &Path, planned: &mut Planned) -> Result<(), String> {
    for (pkg, ids) in read(path)? {
        planned.entry(pkg).or_default().extend(ids);
    }
    Ok(())
}

/// The plan file at `path`.
pub fn read(path: &Path) -> Result<Planned, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if !is_plan(&doc) {
        return Err("not a baseline plan file".into());
    }
    let listed = doc["listed"].as_object().into_iter().flatten();
    let ids = |v: &Value| -> BTreeSet<String> {
        let ids = v.as_array().into_iter().flatten().filter_map(Value::as_str);
        ids.map(str::to_string).collect()
    };
    Ok(listed.map(|(pkg, v)| (pkg.clone(), ids(v))).collect())
}

/// Whether `doc` is a plan file.
pub fn is_plan(doc: &Value) -> bool {
    doc[PLAN_KEY] == true
}
