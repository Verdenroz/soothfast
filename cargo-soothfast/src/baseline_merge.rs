//! `cargo soothfast baseline merge`: one baseline built from exactly the
//! baseline files of one CI run, the shard baselines of a sharded gate plus
//! any imported ones. Nothing else is read, so no other commit's numbers can
//! reach it. A shard that saved nothing leaves its items `unmeasured`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use soothfast_docs::baseline::AMBIGUOUS_KEY;

use crate::invoke::{self, Shard};
use crate::unmeasured::{self, Reason, Unmeasured};
use crate::{ambiguity, benchexe, gate_config, gate_save, import};

mod collect;

mod plan;

/// The shard a gate's baseline holds the items of, as `I/N`.
const SHARD_KEY: &str = "shard";
/// Every item each package's bench binary lists, whichever shard owns it.
pub const LISTED_KEY: &str = "listed";
/// The import prefix of an import gate's file, which is no shard of the gate.
pub const IMPORTED_KEY: &str = "imported";

/// Refuse to save shard `shard` into baseline `name` when it already holds
/// another shard's run, or an unsharded one.
pub fn check_shard_in(root: &Path, name: &str, shard: Shard) -> Result<(), String> {
    let Some(doc) = invoke::load_baseline_in(root, name).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    match doc.get(SHARD_KEY).and_then(Value::as_str) {
        Some(s) if s == shard.to_string() => Ok(()),
        Some(s) => Err(format!(
            "baseline {name:?} holds shard {s}; save shard {shard} into its own baseline"
        )),
        None if is_rust(&doc) => Err(format!(
            "baseline {name:?} holds an unsharded run; save shard {shard} into its own baseline"
        )),
        None => Ok(()),
    }
}

/// Stamp baseline `name` as shard `shard`, recording that `package`'s bench
/// binary lists `ids`.
pub fn stamp_shard_in(
    root: &Path,
    name: &str,
    shard: Shard,
    package: &str,
    ids: &[String],
) -> io::Result<()> {
    let mut doc = invoke::load_baseline_in(root, name)?
        .unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    doc[SHARD_KEY] = json!(shard.to_string());
    if !doc[LISTED_KEY].is_object() {
        doc[LISTED_KEY] = json!({});
    }
    doc[LISTED_KEY][unmeasured::norm(package)] = json!(ids);
    invoke::write_baseline_in(root, name, &doc)
}

/// Whether `doc` holds a gate's run, as opposed to imported items only.
fn is_rust(doc: &Value) -> bool {
    doc.get(IMPORTED_KEY).is_none()
        && (doc.get(SHARD_KEY).is_some()
            || doc.get("unmeasured").is_some()
            || doc.get(unmeasured::OWNERS_KEY).is_some()
            || items(doc).is_some_and(|m| m.values().any(|v| !v["imported"].is_string())))
}

fn items(doc: &Value) -> Option<&Map<String, Value>> {
    doc.get("items").and_then(Value::as_object)
}

/// Why a merge failed: a file that cannot be trusted, or one from a gate
/// split another way than `--expect` says.
#[derive(Debug, PartialEq)]
enum MergeError {
    Corrupt(String),
    Count(String),
}

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("merge") => merge_cmd(&args[1..]),
        Some("mark") => mark_cmd(&args[1..]),
        Some("plan") => plan::cmd(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast baseline merge --into NAME [--expect N] [--packages \"A B\"] [--not-gated EVENT] [--plan FILE]... FILE...\n       \
                 cargo soothfast baseline mark --into NAME -p PKG --reason REASON [--detail TEXT] [--features F]\n       \
                 cargo soothfast baseline plan --out FILE -p PKG [--features F]"
            );
            2
        }
    }
}

/// `baseline mark`: mark `-p PKG` unmeasured in baseline NAME, for a leg that
/// ran no gate to save it, naming the items its bench binary lists when it
/// builds and lists them.
fn mark_cmd(args: &[String]) -> i32 {
    let mut common = invoke::CommonArgs::default();
    let (mut into, mut reason, mut detail) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if common.try_parse(a, &mut it) {
            continue;
        }
        match a.as_str() {
            "--into" => into = it.next().cloned(),
            "--reason" => reason = it.next().and_then(|r| Reason::parse(r)),
            "--detail" => detail = it.next().cloned(),
            other => return fail(&format!("unknown baseline mark arg {other:?}")),
        }
    }
    let (Some(into), Some(_), Some(reason)) = (into, &common.pkg, reason) else {
        return fail("baseline mark needs --into NAME, -p PKG and a known --reason");
    };
    let listed = gate_config::apply(&mut common).and_then(|_| {
        let exe = invoke::bench_executable(&common, None, None);
        benchexe::listing(&common, exe.as_deref())
    });
    let detail = detail.unwrap_or_else(|| reason.as_str().to_string());
    match invoke::workspace_root() {
        Ok(root) => mark_listed(&root, &into, &common, reason, &detail, listed),
        Err(e) => fail(&format!("cannot find the workspace root: {e}")),
    }
}

/// Mark `common`'s package in baseline `into` under `root`. When the items
/// could not be `listed`, the entry names only the package's prior owners
/// and the command fails, so the caller warns.
fn mark_listed(
    root: &Path,
    into: &str,
    common: &invoke::CommonArgs,
    reason: Reason,
    detail: &str,
    listed: Result<Vec<String>, String>,
) -> i32 {
    let ids = listed.as_ref().ok().map(Vec::as_slice);
    if let Err(e) = gate_save::mark_package(root, into, common, reason, detail, ids) {
        return fail(&format!("cannot write baseline {into:?}: {e}"));
    }
    match listed {
        Ok(_) => 0,
        Err(e) => fail(&format!(
            "{e}: no items could be listed, so the entry names only what {} owned before, and claims on the rest will fail",
            common.pkg.as_deref().unwrap_or("the package")
        )),
    }
}

fn merge_cmd(args: &[String]) -> i32 {
    let mut into = None;
    let mut expect = 1u64;
    let mut packages = Vec::new();
    let mut not_gated = None;
    let mut planned = plan::Planned::new();
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--into" => into = it.next().cloned(),
            "--expect" => match it.next().map(|n| n.parse::<u64>()) {
                Some(Ok(n)) if n > 0 => expect = n,
                _ => return fail("--expect needs a shard count of at least 1"),
            },
            "--packages" => match it.next() {
                Some(p) => packages = p.split_whitespace().map(str::to_string).collect(),
                None => return fail("--packages needs a space-separated list"),
            },
            "--not-gated" => match it.next() {
                Some(event) => not_gated = Some(event.clone()),
                None => return fail("--not-gated needs the event name"),
            },
            "--plan" => match it.next() {
                Some(f) => {
                    if let Err(e) = plan::read_into(Path::new(f), &mut planned) {
                        return fail(&format!("plan file {f}: {e}"));
                    }
                }
                None => return fail("--plan needs the plan file"),
            },
            other if !other.starts_with('-') => files.push(PathBuf::from(other)),
            other => return fail(&format!("unknown baseline merge arg {other:?}")),
        }
    }
    let Some(into) = into else {
        return fail("baseline merge needs --into NAME");
    };
    let mut docs = Vec::new();
    for file in &files {
        let read = std::fs::read_to_string(file)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
        match read {
            Ok(doc) if plan::is_plan(&doc) => {
                return fail(&format!(
                    "{} is a plan file; pass it with --plan",
                    file.display()
                ));
            }
            Ok(doc) => {
                let label = file.display().to_string();
                if let Some(note) = unmeasured::legacy_note(&doc, &label) {
                    println!("baseline merge: {note}");
                }
                docs.push((label, doc));
            }
            Err(e) => return fail(&format!("corrupt baseline {}: {e}", file.display())),
        }
    }
    let imports = if not_gated.is_some() {
        match import::configured_prefixes(&invoke::project_root()) {
            Ok(prefixes) => prefixes,
            Err(e) => return fail(&format!("cannot read the import prefixes: {e}")),
        }
    } else {
        Vec::new()
    };
    let ungated = not_gated.as_deref().map(|event| Ungated {
        event,
        imports: &imports,
    });
    let merged = match merge_on(&docs, expect, &packages, &planned, ungated.as_ref()) {
        Ok(doc) => doc,
        Err(MergeError::Corrupt(e)) => return fail(&e),
        Err(MergeError::Count(e)) => {
            eprintln!("soothfast: {e}");
            return 3;
        }
    };
    if let Err(e) = invoke::write_baseline_in(&invoke::project_root(), &into, &merged) {
        return fail(&format!("cannot write baseline {into:?}: {e}"));
    }
    report(&into, docs.len(), &merged);
    0
}

fn report(into: &str, files: usize, merged: &Value) {
    let count = items(merged).map_or(0, Map::len);
    println!("baseline merge: {files} file(s) into {into}: {count} item(s)");
    for u in unmeasured::read(merged).unwrap_or_default() {
        let ids = if u.items.is_empty() {
            "items unknown".to_string()
        } else {
            u.items.join(", ")
        };
        println!(
            "baseline merge: not measured ({}): {}: {ids}",
            u.reason.as_str(),
            u.detail
        );
    }
}

fn fail(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    2
}

/// One baseline from `files` (label, document) of a gate split into
/// `expect` shards, accounting for every one of `packages`.
#[cfg(test)]
fn merge(files: &[(String, Value)], expect: u64, packages: &[String]) -> Result<Value, MergeError> {
    merge_on(files, expect, packages, &plan::Planned::new(), None)
}

/// A CI event the cargo gate does not measure on, and the configured import
/// prefixes, whose items are measured on it all the same.
pub struct Ungated<'a> {
    pub event: &'a str,
    pub imports: &'a [String],
}

/// [`merge`] for a CI event the cargo gate measured on, or, with
/// `not_gated`, one it did not: then one entry covers every item outside the
/// import prefixes, and only packages some file accounts for are checked for
/// a missing shard.
fn merge_on(
    files: &[(String, Value)],
    expect: u64,
    packages: &[String],
    planned: &plan::Planned,
    not_gated: Option<&Ungated>,
) -> Result<Value, MergeError> {
    let files: Vec<(String, Value)> = (files.iter())
        .map(|(label, doc)| {
            let mut doc = doc.clone();
            unmeasured::normalize(&mut doc);
            (label.clone(), doc)
        })
        .collect();
    let packages: Vec<String> = packages.iter().map(|p| unmeasured::norm(p)).collect();
    let packages = packages.as_slice();
    let corrupt = |label: &str, why: String| MergeError::Corrupt(format!("{label}: {why}"));
    let mut out = json!({ "version": 1 });
    let mut collected = collect::Collected::default();
    let mut assertions = Vec::new();
    let mut entries: Vec<Unmeasured> = Vec::new();
    let mut shards: BTreeMap<u64, Saved> = BTreeMap::new();
    let no_items = Map::new();
    let mut owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut registered: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut ambiguous = Map::new();
    for (label, doc) in &files {
        let file_entries = unmeasured::read(doc).map_err(|e| corrupt(label, e))?;
        let file_items = match doc.get("items") {
            None => &no_items,
            Some(v) => v
                .as_object()
                .ok_or_else(|| corrupt(label, "`items` is not an object".into()))?,
        };
        if is_rust(doc) {
            let shard = match doc.get(SHARD_KEY) {
                None => Shard { index: 1, count: 1 },
                Some(v) => v
                    .as_str()
                    .and_then(|s| Shard::parse(s).ok())
                    .ok_or_else(|| corrupt(label, format!("bad shard stamp {v}")))?,
            };
            if shard.count != expect {
                return Err(MergeError::Count(format!(
                    "{label} is shard {shard}, but the gate was split into {expect}"
                )));
            }
            let saved = Saved {
                doc,
                entries: file_entries.clone(),
            };
            if shards.insert(shard.index, saved).is_some() {
                return Err(corrupt(label, format!("a second file for shard {shard}")));
            }
            if let Some(id) = file_items
                .iter()
                .find(|(id, v)| !v["imported"].is_string() && !shard.owns(id))
                .map(|(id, _)| id)
            {
                return Err(corrupt(
                    label,
                    format!("shard {shard} holds {id}, which it does not own"),
                ));
            }
            for key in ["noise_pct", "build"] {
                if out.get(key).is_none()
                    && let Some(v) = doc.get(key)
                {
                    out[key] = v.clone();
                }
            }
        }
        collected
            .take(label, doc, file_items)
            .map_err(MergeError::Corrupt)?;
        for a in doc["assertions"].as_array().into_iter().flatten() {
            let idless = |a: &Value| a["id"].as_str().is_none_or(str::is_empty);
            let same = |b: &&Value| {
                idless(a) && idless(b) && b["kind"] == a["kind"] && b["detail"] == a["detail"]
            };
            match assertions.iter().position(|b| same(&b)) {
                Some(i) if assertions[i]["ok"] == true && a["ok"] != true => {
                    assertions[i] = a.clone();
                }
                Some(_) => {}
                None => assertions.push(a.clone()),
            }
        }
        union(&mut owners, &doc[unmeasured::OWNERS_KEY]);
        union(&mut registered, &doc[ambiguity::REGISTERED_KEY]);
        for (id, entry) in doc[AMBIGUOUS_KEY].as_object().into_iter().flatten() {
            ambiguous.entry(id.clone()).or_insert_with(|| entry.clone());
        }
        entries.extend(file_entries);
    }
    for pkg in packages {
        if not_gated.is_none() || shards.values().any(|s| s.covers(pkg)) {
            let plan = planned.get(pkg.as_str());
            entries.extend(unaccounted(pkg, &shards, expect, plan));
        }
    }
    if let Some(ungated) = not_gated {
        let mut imports: BTreeSet<String> = ungated.imports.iter().cloned().collect();
        imports.extend(
            collected
                .items
                .values()
                .filter_map(|v| v["imported"].as_str().map(str::to_string)),
        );
        entries.push(Unmeasured {
            package: "cargo".into(),
            reason: Reason::NotGatedOnEvent,
            detail: format!(
                "the cargo gate does not measure on a {} event; only imported items are",
                ungated.event
            ),
            except_imported: Some(imports.into_iter().collect()),
            ..Unmeasured::default()
        });
    }
    out["created_unix"] = json!(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    out["items"] = Value::Object(collected.items);
    out["assertions"] = Value::Array(assertions);
    out["unmeasured"] = entries.iter().map(Unmeasured::to_json).collect();
    if !owners.is_empty() {
        out[unmeasured::OWNERS_KEY] = json!(owners);
    }
    if !registered.is_empty() {
        out[ambiguity::REGISTERED_KEY] = json!(registered);
    }
    if !ambiguous.is_empty() {
        out[AMBIGUOUS_KEY] = Value::Object(ambiguous);
    }
    let gated = gated_listing(packages, &shards);
    ambiguity::settle(&mut out, &gated);
    Ok(out)
}

/// Add a `{package: [ids]}` map of one file to `into`.
fn union(into: &mut BTreeMap<String, BTreeSet<String>>, map: &Value) {
    for (package, ids) in unmeasured::by_package(map) {
        into.entry(package).or_default().extend(ids);
    }
}

/// What each of `packages` listed in the shards of this run.
fn gated_listing(packages: &[String], shards: &BTreeMap<u64, Saved>) -> ambiguity::Gated {
    let mut gated = ambiguity::Gated::new();
    for pkg in packages {
        let listed = shards.values().flat_map(|s| {
            let ids = s.doc[LISTED_KEY][pkg].as_array().into_iter().flatten();
            ids.filter_map(|id| id.as_str().map(str::to_string))
        });
        gated.insert(pkg.clone(), listed.collect());
    }
    gated
}

/// The entries for the shards of `pkg` that saved nothing of it. Their items
/// are named from the other shards' listings and owners of `pkg`, and from
/// the `plan` the bench build listed; with none to read, no item is.
fn unaccounted(
    pkg: &str,
    shards: &BTreeMap<u64, Saved>,
    expect: u64,
    plan: Option<&BTreeSet<String>>,
) -> Vec<Unmeasured> {
    let missing: Vec<Shard> = (1..=expect)
        .map(|index| Shard {
            index,
            count: expect,
        })
        .filter(|s| !shards.get(&s.index).is_some_and(|saved| saved.covers(pkg)))
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    let known: BTreeSet<String> = shards
        .values()
        .flat_map(|Saved { doc, .. }| {
            let listed = doc[LISTED_KEY][pkg].as_array().into_iter().flatten();
            listed
                .filter_map(|id| id.as_str().map(str::to_string))
                .chain(unmeasured::owned_by(doc, pkg))
        })
        .chain(plan.into_iter().flatten().cloned())
        .collect();
    let unsaved = !shards.values().any(|saved| saved.covers(pkg));
    if known.is_empty() {
        let which = if expect == 1 {
            "the gate".to_string()
        } else {
            let names: Vec<String> = missing.iter().map(Shard::to_string).collect();
            format!("shard {}", names.join(", "))
        };
        return vec![Unmeasured {
            detail: format!("{which} saved no measurement of {pkg}"),
            package: pkg.to_string(),
            reason: Reason::NoMeasurement,
            ..Unmeasured::default()
        }];
    }
    missing
        .into_iter()
        .map(|shard| {
            let nameless = shards.get(&shard.index).and_then(|saved| {
                (saved.entries.iter()).find(|e| unmeasured::norm(&e.package) == pkg && e.items.is_empty())
            });
            let (reason, detail) = match nameless {
                Some(e) => (e.reason, format!("{} (shard {shard})", e.detail)),
                None if unsaved => (
                    Reason::NoMeasurement,
                    format!("no gate leg saved a measurement of {pkg} (shard {shard}); the bench build listed these items"),
                ),
                None => (
                    Reason::ShardMissing,
                    format!("shard {shard} saved no measurement of {pkg}"),
                ),
            };
            Unmeasured {
                detail,
                package: pkg.to_string(),
                reason,
                items: known.iter().filter(|id| shard.owns(id)).cloned().collect(),
                ..Unmeasured::default()
            }
        })
        .filter(|u| !u.items.is_empty())
        .collect()
}

/// One shard's file, with its entries read once.
struct Saved<'a> {
    doc: &'a Value,
    entries: Vec<Unmeasured>,
}

impl Saved<'_> {
    /// Whether the file accounts for `pkg`, by its own record of the
    /// package: its owners, even none, a non-empty listing, or an entry
    /// naming items.
    fn covers(&self, pkg: &str) -> bool {
        let named = |v: &Value| v.as_array().is_some_and(|ids| !ids.is_empty());
        self.doc[unmeasured::OWNERS_KEY][pkg].is_array()
            || named(&self.doc[LISTED_KEY][pkg])
            || (self.entries.iter())
                .any(|e| unmeasured::norm(&e.package) == pkg && !e.items.is_empty())
    }
}

#[cfg(test)]
mod tests;
