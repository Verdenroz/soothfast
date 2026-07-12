//! `cargo soothfast spec gen` — write spec files from the code.
//!
//! The inverse of `spec check`: instead of proving the code matches a
//! hand-authored spec, the spec is derived from the code, so the two cannot
//! disagree. Only files a `[[spec]]` entry marks `mode = "generate"` are
//! written; everything else stays on the reconcile path, because a contract
//! someone else serves can't be generated from our source.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use soothfast_spec::dialect::{Info, Operation};
use soothfast_spec::schema::{self, TypeTable, route_sig};

use crate::invoke::{self, CommonArgs, Visibility};
use crate::spec_config::{self, Mode, SpecEntry};

/// One `#[route]` registration, including the shape overrides.
pub(crate) struct Route {
    pub id: String,
    pub operation: String,
    pub method: String,
    pub path: String,
    /// Expansion-time doc summary of the marker fn, when it has one.
    pub summary: Option<String>,
    pub overrides: route_sig::Overrides,
}

fn route_from(r: &Value) -> Route {
    let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
    Route {
        id: s("id"),
        operation: s("operation"),
        method: s("method"),
        path: s("path"),
        summary: r["summary"].as_str().map(String::from),
        overrides: route_sig::Overrides {
            request: r["request"].as_str().map(String::from),
            response: r["response"].as_str().map(String::from),
            status: r["status"].as_u64().map(|n| n as u16),
            params: r["params"].as_str().map(String::from),
        },
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut check_only = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--check" {
            check_only = true;
        } else if !common.try_parse(a, &mut it) {
            eprintln!("soothfast: unknown spec gen arg {a:?}");
            return 2;
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: spec gen requires -p PKG");
        return 2;
    };
    match generate(&pkg, &common, check_only) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

/// The documents generated for one tree, keyed by spec file.
pub(crate) struct Built {
    pub docs: BTreeMap<String, Value>,
    /// Shapes the extractor could not derive from the code.
    pub gaps: BTreeMap<String, Vec<String>>,
    /// Shapes the target dialect cannot express as precisely as the code
    /// states them — a different shortfall from a gap, and reported apart
    /// from one so the remedy is not confused.
    pub notes: BTreeMap<String, Vec<String>>,
    pub conflicts: Vec<String>,
}

/// Discover every `#[route]` registration in one tree, keyed by spec file.
///
/// Shells out to `cargo bench`, so callers must check their config has
/// something to generate first — a crate with no bench target fails
/// outright on the bench run.
pub(crate) fn discover_routes(
    common: &CommonArgs,
    dir: Option<&Path>,
) -> Result<BTreeMap<String, Vec<Route>>, String> {
    let records =
        invoke::run_bench_in(common, &["--list-routes"], dir).map_err(|e| e.to_string())?;
    let mut by_spec: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    for r in records.iter().filter(|r| r["type"] == "route") {
        by_spec
            .entry(r["spec"].as_str().unwrap_or("").to_string())
            .or_default()
            .push(route_from(r));
    }
    Ok(by_spec)
}

/// Build rustdoc JSON for the package, private items included.
///
/// The expensive step, so generators call it once for all their files —
/// and only after route discovery has proven there is work to do.
/// Private items are mandatory here: route handlers are commonly not
/// `pub`, and rustdoc omits them entirely by default.
pub(crate) fn rustdoc_for(
    pkg: &str,
    common: &CommonArgs,
    dir: Option<&Path>,
) -> Result<Value, String> {
    let (doc, _root) =
        invoke::rustdoc_json_in(pkg, dir, common.features.as_deref(), Visibility::Private)
            .map_err(|e| e.to_string())?;
    Ok(doc)
}

/// Infer the operations one spec entry's routes declare.
///
/// Returns the operations (whose shapes still carry structured gaps) plus
/// the deduplicated human-readable gap list — one shared type reached from
/// several operations reports the same gap each time; the reader needs it
/// once.
pub(crate) fn operations_for(
    doc: &Value,
    entry: Option<&SpecEntry>,
    routes: &[Route],
) -> Result<(Vec<Operation>, Vec<String>), String> {
    let extractors = route_sig::Extractors::builtin();
    // `[spec.types]` extends the built-in table; user entries win, so a
    // crate can correct a default it knows to be wrong for its own wire.
    let mut table = TypeTable::builtin();
    if let Some(e) = entry {
        for (path, schema) in &e.types {
            table.insert(path.clone(), schema.clone());
        }
    }
    let mut ops = Vec::new();
    let mut gaps = Vec::new();
    for route in routes {
        let shape = route_sig::infer(
            doc,
            &table,
            &extractors,
            &route.id,
            &route.path,
            &route.overrides,
        )?;
        gaps.extend(shape.gaps.iter().map(|g| g.explain()));
        ops.push(Operation {
            operation_id: route.operation.clone(),
            method: route.method.clone(),
            path: route.path.clone(),
            summary: shape.summary.clone().or_else(|| route.summary.clone()),
            shape,
        });
    }
    if let Some(e) = entry {
        merge_common_responses(doc, &table, e, &mut ops, &mut gaps)?;
    }
    gaps.sort();
    gaps.dedup();
    Ok((ops, gaps))
}

/// Merge each `[spec.responses]` entry into every operation that doesn't
/// already declare its status code.
fn merge_common_responses(
    doc: &Value,
    table: &TypeTable,
    entry: &SpecEntry,
    ops: &mut [Operation],
    gaps: &mut Vec<String>,
) -> Result<(), String> {
    for (code, common) in &entry.responses {
        let (schema, components) = match &common.type_name {
            Some(name) => {
                let ex = schema::extract_named(doc, table, name)
                    .map_err(|e| format!("[spec.responses] {code}: {e}"))?;
                gaps.extend(ex.gaps.iter().map(|g| g.explain()));
                (ex.schema, ex.components)
            }
            None => (Value::Null, BTreeMap::new()),
        };
        let response = route_sig::Response {
            content_type: if schema.is_null() {
                String::new()
            } else {
                "application/json".into()
            },
            schema,
            headers: common.headers.clone(),
            description: common.description.clone(),
        };
        for op in ops.iter_mut() {
            op.shape.components.extend(components.clone());
            op.shape
                .responses
                .entry(code.clone())
                .or_insert_with(|| response.clone());
        }
    }
    Ok(())
}

/// Build every generate-mode document for one working tree.
///
/// `dir` points at another checkout (the merge-base worktree) so the gate can
/// build the same specs from two revisions. Which files are generated, and
/// their metadata, always come from the current tree's config: the question
/// is how today's API compares, not how the base was configured.
pub(crate) fn build(
    pkg: &str,
    common: &CommonArgs,
    dir: Option<&Path>,
    cfg: &spec_config::SpecConfig,
    meta: &invoke::PkgMeta,
) -> Result<Built, String> {
    let mut built = Built {
        docs: BTreeMap::new(),
        gaps: BTreeMap::new(),
        notes: BTreeMap::new(),
        conflicts: Vec::new(),
    };
    // No generate-mode entry means nothing can be generated whatever the code
    // declares, so route discovery is not just wasted work — it shells out to
    // `cargo bench`, which fails outright in a package with no bench target.
    // A crate that generates no specs must not be a crate that cannot be run
    // through the command.
    if !cfg.entries.iter().any(|e| e.mode == Mode::Generate) {
        return Ok(built);
    }

    let mut by_spec = discover_routes(common, dir)?;
    by_spec.retain(|s, _| cfg.mode_of(s) == Mode::Generate);
    if by_spec.is_empty() {
        return Ok(built);
    }
    let doc = rustdoc_for(pkg, common, dir)?;

    for (spec_file, routes) in &by_spec {
        let entry = cfg.for_path(spec_file);
        let (ops, gaps) = operations_for(&doc, entry, routes)?;
        let document = cfg
            .dialect_of(spec_file)
            .document(&info_for(entry, pkg, meta), &ops);
        built.conflicts.extend(
            document
                .conflicts
                .iter()
                .map(|c| format!("{spec_file}: {c}")),
        );
        built.gaps.insert(spec_file.clone(), gaps);
        let mut notes = document.notes;
        notes.sort();
        notes.dedup();
        built.notes.insert(spec_file.clone(), notes);
        built.docs.insert(spec_file.clone(), document.value);
    }
    Ok(built)
}

fn generate(pkg: &str, common: &CommonArgs, check_only: bool) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let cfg = spec_config::load(&meta.dir)?;
    let built = build(pkg, common, None, &cfg, &meta)?;

    if built.docs.is_empty() {
        println!(
            "spec gen: nothing to generate — no [[spec]] entry in soothfast.toml \
             sets mode = \"generate\" for a spec file any #[route] declares"
        );
        return Ok(0);
    }

    let mut stale = 0u32;
    for c in &built.conflicts {
        println!("FAIL  {c}");
    }

    for (spec_file, value) in &built.docs {
        let kind = cfg.dialect_of(spec_file);
        let target = meta.dir.join(spec_file);
        let rendered = kind.render(spec_file, value);

        if check_only {
            let current = std::fs::read_to_string(&target).unwrap_or_default();
            if current == rendered {
                println!("spec gen --check: {spec_file} is up to date");
            } else {
                stale += 1;
                println!(
                    "STALE {spec_file}: regenerating would change it — run \
                     `cargo soothfast spec gen -p {pkg}` and commit the result"
                );
            }
        } else {
            write_if_changed(&target, &rendered)?;
        }

        let gaps = built.gaps.get(spec_file).map(Vec::as_slice).unwrap_or(&[]);
        let notes = built.notes.get(spec_file).map(Vec::as_slice).unwrap_or(&[]);
        let (operations, schemas) = kind.summarize(value);
        println!(
            "spec gen: {spec_file} [{}] — {operations} operation(s), \
             {schemas} schema(s), {} gap(s), {} note(s)",
            kind.name(),
            gaps.len(),
            notes.len(),
        );
        // Neither is fatal: an open schema is imprecise, not wrong, and
        // generation that failed on ordinary Rust would be useless.
        for g in gaps {
            println!("  gap: {g}");
        }
        for n in notes {
            println!("  note: {n}");
        }
    }

    if !built.conflicts.is_empty() {
        println!("spec gen: FAILED ({} conflict(s))", built.conflicts.len());
        return Ok(1);
    }
    if stale > 0 {
        println!("spec gen --check: FAILED ({stale} stale file(s))");
        return Ok(1);
    }
    Ok(0)
}

/// Document metadata: `[[spec]]` first, then the package manifest.
pub(crate) fn info_for(entry: Option<&SpecEntry>, pkg: &str, meta: &invoke::PkgMeta) -> Info {
    Info {
        title: entry
            .and_then(|e| e.title.clone())
            .unwrap_or_else(|| pkg.to_string()),
        version: entry
            .and_then(|e| e.version.clone())
            .unwrap_or_else(|| meta.version.clone()),
        description: entry
            .and_then(|e| e.description.clone())
            .or_else(|| meta.description.clone()),
        servers: entry.map(|e| e.servers.clone()).unwrap_or_default(),
    }
}

/// Write only when the content differs, so a no-op run leaves the file's
/// mtime alone and the bot-commit workflow has nothing to commit.
pub(crate) fn write_if_changed(target: &Path, rendered: &str) -> Result<(), String> {
    if std::fs::read_to_string(target).is_ok_and(|c| c == rendered) {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(target, rendered).map_err(|e| format!("cannot write {}: {e}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_are_read_off_the_route_record() {
        let r = json!({
            "type": "route", "id": "app::create", "spec": "s.yaml",
            "operation": "createItem", "method": "POST", "path": "/items",
            "request": "NewItem", "response": "Item", "status": 201,
        });
        let route = route_from(&r);
        assert_eq!(route.overrides.request.as_deref(), Some("NewItem"));
        assert_eq!(route.overrides.status, Some(201));
    }

    #[test]
    fn absent_overrides_stay_absent() {
        let r = json!({
            "type": "route", "id": "app::get", "spec": "s.yaml",
            "operation": "getItem", "method": "GET", "path": "/items",
            "request": Value::Null, "response": Value::Null, "status": Value::Null,
        });
        let route = route_from(&r);
        assert_eq!(route.overrides.request, None);
        assert_eq!(route.overrides.status, None);
    }

    #[test]
    fn spec_metadata_falls_back_to_the_package_manifest() {
        let meta = invoke::PkgMeta {
            dir: std::path::PathBuf::from("/tmp"),
            version: "2.3.4".into(),
            description: Some("from Cargo.toml".into()),
        };
        let info = info_for(None, "my-api", &meta);
        assert_eq!(info.title, "my-api");
        assert_eq!(info.version, "2.3.4");
        assert_eq!(info.description.as_deref(), Some("from Cargo.toml"));
    }

    #[test]
    fn config_metadata_wins_over_the_manifest() {
        let meta = invoke::PkgMeta {
            dir: std::path::PathBuf::from("/tmp"),
            version: "2.3.4".into(),
            description: None,
        };
        let entry = SpecEntry {
            path: "specs/openapi.yaml".into(),
            mode: Mode::Generate,
            dialect: None,
            title: Some("Items API".into()),
            version: Some("2.1".into()),
            description: None,
            servers: vec!["https://api.example.com".into()],
            types: BTreeMap::new(),
            responses: BTreeMap::new(),
        };
        let info = info_for(Some(&entry), "my-api", &meta);
        assert_eq!(info.title, "Items API");
        assert_eq!(info.version, "2.1");
        assert_eq!(info.servers, vec!["https://api.example.com".to_string()]);
    }

    #[test]
    fn a_package_with_nothing_to_generate_never_reaches_the_bench_run() {
        // The regression this guards: `spec gen` used to discover routes
        // first, so pointing it at a crate that generates no specs failed on
        // the missing `[[bench]]` target instead of reporting nothing to do.
        // `run_bench_in` would panic-or-error here if it were reached, since
        // there is no such package.
        let meta = invoke::PkgMeta {
            dir: std::path::PathBuf::from("/nonexistent"),
            version: "0.0.0".into(),
            description: None,
        };
        let cfg = spec_config::SpecConfig::default();
        let built = build("no-such-package", &CommonArgs::default(), None, &cfg, &meta)
            .expect("reports nothing to do rather than failing");
        assert!(built.docs.is_empty());
        assert!(built.conflicts.is_empty());
    }

    #[test]
    fn a_check_only_config_is_also_nothing_to_generate() {
        let meta = invoke::PkgMeta {
            dir: std::path::PathBuf::from("/nonexistent"),
            version: "0.0.0".into(),
            description: None,
        };
        let cfg = spec_config::parse("[[spec]]\npath = \"vendor/stripe.yaml\"\n").expect("parses");
        let built = build("no-such-package", &CommonArgs::default(), None, &cfg, &meta)
            .expect("a check-mode entry generates nothing");
        assert!(built.docs.is_empty());
    }

    #[test]
    fn common_responses_merge_without_clobbering_declared_ones() {
        use soothfast_spec::dialect::Operation;
        use soothfast_spec::schema::route_sig::{Response, RouteShape};

        let mut declared = RouteShape::default();
        declared
            .responses
            .insert("429".into(), Response::json(json!({ "type": "string" })));
        let mut ops = vec![
            Operation {
                operation_id: "getItem".into(),
                method: "GET".into(),
                path: "/items".into(),
                summary: None,
                shape: RouteShape::default(),
            },
            Operation {
                operation_id: "getOther".into(),
                method: "GET".into(),
                path: "/other".into(),
                summary: None,
                shape: declared,
            },
        ];
        let entry = spec_config::parse(
            "[[spec]]\npath = \"openapi.yaml\"\nmode = \"generate\"\n\n[spec.responses]\n\
             \"429\" = { description = \"Rate limited\", headers = [\"Retry-After\"] }\n",
        )
        .expect("parses")
        .entries
        .remove(0);
        let doc = json!({ "index": {}, "paths": {} });
        let mut gaps = Vec::new();
        merge_common_responses(&doc, &TypeTable::builtin(), &entry, &mut ops, &mut gaps)
            .expect("merges");

        let merged = &ops[0].shape.responses["429"];
        assert_eq!(merged.description.as_deref(), Some("Rate limited"));
        assert_eq!(merged.headers, vec!["Retry-After".to_string()]);
        assert!(merged.content_type.is_empty(), "no type means no body");

        let kept = &ops[1].shape.responses["429"];
        assert!(kept.description.is_none(), "a declared status is kept");
        assert_eq!(kept.schema, json!({ "type": "string" }));
    }

    #[test]
    fn writing_is_skipped_when_the_content_is_unchanged() {
        let dir = std::env::temp_dir().join("soothfast-spec-gen-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let target = dir.join("spec.yaml");
        std::fs::write(&target, "openapi: 3.1.0\n").expect("seed");
        let before = std::fs::metadata(&target).and_then(|m| m.modified()).ok();

        write_if_changed(&target, "openapi: 3.1.0\n").expect("no-op write");
        let after = std::fs::metadata(&target).and_then(|m| m.modified()).ok();
        assert_eq!(before, after, "an unchanged file is not rewritten");

        write_if_changed(&target, "openapi: 3.1.0\ninfo: {}\n").expect("real write");
        let content = std::fs::read_to_string(&target).expect("read back");
        assert!(content.contains("info"), "a changed file is written");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
