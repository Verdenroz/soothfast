//! Golden tests: the Python emitter's full file tree, byte-for-byte.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p soothfast-sdk`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use soothfast_sdk::{SdkKind, SdkOptions};
use soothfast_spec::dialect::{Info, Operation};
use soothfast_spec::schema::RouteShape;
use soothfast_spec::schema::route_sig::{Parameter, RequestBody, Response};

fn item_components() -> BTreeMap<String, serde_json::Value> {
    let mut c = BTreeMap::new();
    c.insert(
        "Item".into(),
        json!({
            "type": "object",
            "description": "One item.",
            "properties": {
                "from": { "type": "string" },
                "id": { "type": "integer" },
                "logoUrl": { "type": "string" },
                "logo_url": { "type": "string" },
                "note": { "type": "string" },
            },
            "required": ["id"],
        }),
    );
    c
}

fn param(name: &str, location: &str, required: bool, schema: serde_json::Value) -> Parameter {
    Parameter {
        name: name.into(),
        location: location.into(),
        required,
        schema,
    }
}

fn op(operation_id: &str, method: &str, path: &str, summary: &str, shape: RouteShape) -> Operation {
    Operation {
        operation_id: operation_id.into(),
        method: method.into(),
        path: path.into(),
        summary: (!summary.is_empty()).then(|| summary.to_string()),
        shape,
    }
}

fn fixture() -> Vec<Operation> {
    let get_item = {
        let mut shape = RouteShape {
            components: item_components(),
            ..RouteShape::default()
        };
        shape
            .parameters
            .push(param("fields", "query", false, json!({ "type": "string" })));
        shape
            .parameters
            .push(param("id", "path", true, json!({ "type": "string" })));
        shape.responses.insert(
            "200".into(),
            Response::json(json!({ "$ref": "#/components/schemas/Item" })),
        );
        op("getItem", "GET", "/v1/items/{id}", "Get one item.", shape)
    };

    let create_item = {
        let mut components = item_components();
        components.insert(
            "NewItem".into(),
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"],
            }),
        );
        let mut shape = RouteShape {
            components,
            ..RouteShape::default()
        };
        shape.request = Some(RequestBody {
            content_type: "application/json".into(),
            schema: json!({ "$ref": "#/components/schemas/NewItem" }),
            required: true,
        });
        shape.responses.insert(
            "201".into(),
            Response::json(json!({ "$ref": "#/components/schemas/Item" })),
        );
        op("createItem", "POST", "/v1/items", "Create an item.", shape)
    };

    let list_items = {
        let mut shape = RouteShape {
            components: item_components(),
            ..RouteShape::default()
        };
        shape
            .parameters
            .push(param("cursor", "query", false, json!({ "type": "string" })));
        shape
            .parameters
            .push(param("limit", "query", false, json!({ "type": "integer" })));
        shape
            .parameters
            .push(param("tag", "query", false, json!({ "type": "string" })));
        shape.responses.insert(
            "200".into(),
            Response::json(json!({
                "type": "array",
                "items": { "$ref": "#/components/schemas/Item" },
            })),
        );
        op("listItems", "GET", "/v1/items", "List items.", shape)
    };

    let get_stats = {
        let mut components = item_components();
        components.insert(
            "ApiResponse_Item".into(),
            json!({
                "type": "object",
                "properties": {
                    "data": { "oneOf": [
                        { "$ref": "#/components/schemas/Item" },
                        { "type": "string" },
                    ]},
                    "meta": {},
                    "status": { "type": "string", "enum": ["ok", "error"] },
                },
                "required": ["status"],
            }),
        );
        let mut shape = RouteShape {
            components,
            ..RouteShape::default()
        };
        shape.responses.insert(
            "200".into(),
            Response::json(json!({ "$ref": "#/components/schemas/ApiResponse_Item" })),
        );
        op("getStats", "GET", "/v1/stats", "Get stats.", shape)
    };

    let delete_item = {
        let mut shape = RouteShape::default();
        shape
            .parameters
            .push(param("id", "path", true, json!({ "type": "string" })));
        shape.responses.insert("204".into(), Response::empty());
        op(
            "deleteItem",
            "DELETE",
            "/v1/items/{id}",
            "Delete an item.",
            shape,
        )
    };

    vec![get_item, create_item, list_items, get_stats, delete_item]
}

fn options() -> SdkOptions {
    SdkOptions {
        package: "acme-items".into(),
        module: "acme_items".into(),
        version: "1.2.3".into(),
        base_url: Some("https://api.acme.test".into()),
        description: Some("Acme items client".into()),
        repository: Some("https://github.com/acme/items".into()),
        paginated: vec!["listItems".into()],
        ..SdkOptions::default()
    }
}

fn info() -> Info {
    Info {
        title: "Acme Items API".into(),
        version: "1.2.3".into(),
        description: None,
        servers: vec![],
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/python")
}

fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Python tooling run against the goldens (pytest, mypy, uv) drops
        // artifacts next to them; only the emitted files are the fixture.
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "__pycache__" || name == "uv.lock" {
            continue;
        }
        if path.is_dir() {
            walk(&path, root, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, std::fs::read_to_string(&path).expect("golden file"));
        }
    }
}

#[test]
fn python_emission_matches_the_goldens() {
    let files = SdkKind::Python
        .emit(&info(), &fixture(), &options())
        .expect("emits")
        .files;

    let dir = golden_dir();
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, content) in &files {
            let target = dir.join(rel);
            std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
            std::fs::write(&target, content).expect("write golden");
        }
        // Keep the guard against committing Python tool artifacts.
        std::fs::write(
            dir.join(".gitignore"),
            ".venv/\n.mypy_cache/\n__pycache__/\nuv.lock\ndist/\n",
        )
        .expect("write gitignore");
        return;
    }

    let mut expected = BTreeMap::new();
    walk(&dir, &dir, &mut expected);
    assert!(
        !expected.is_empty(),
        "no goldens found — run UPDATE_GOLDENS=1 cargo test -p soothfast-sdk"
    );
    let got: Vec<&String> = files.keys().collect();
    let want: Vec<&String> = expected.keys().collect();
    assert_eq!(got, want, "file set changed");
    for (rel, content) in &files {
        assert_eq!(content, &expected[rel], "content of {rel} changed");
    }
}

#[test]
fn emission_is_deterministic() {
    let a = SdkKind::Python
        .emit(&info(), &fixture(), &options())
        .expect("emits");
    let b = SdkKind::Python
        .emit(&info(), &fixture(), &options())
        .expect("emits");
    assert_eq!(a.files, b.files);
    assert_eq!(a.notes, b.notes);
}

#[test]
fn the_name_collision_is_disambiguated_not_silently_merged() {
    let out = SdkKind::Python
        .emit(&info(), &fixture(), &options())
        .expect("emits");
    let models = &out.files["src/acme_items/models.py"];
    assert!(models.contains("logo_url:"), "{models}");
    assert!(models.contains("logo_url_:"), "{models}");
    assert!(
        out.notes.iter().any(|n| n.contains("logo_url")),
        "collision is reported: {:?}",
        out.notes
    );
}

#[test]
fn conflicting_component_schemas_are_refused() {
    let mut ops = fixture();
    ops[1]
        .shape
        .components
        .insert("Item".into(), json!({ "type": "object", "properties": {} }));
    let err = SdkKind::Python
        .emit(&info(), &ops, &options())
        .expect_err("conflict");
    assert!(err.contains("Item"), "{err}");
}

#[test]
fn components_unreachable_from_any_method_are_pruned() {
    let mut ops = fixture();
    ops[0].shape.components.insert(
        "OrphanQuery".into(),
        json!({
            "type": "object",
            "properties": { "q": { "type": "string" } },
        }),
    );
    let out = SdkKind::Python
        .emit(&info(), &ops, &options())
        .expect("emits");
    let models = &out.files["src/acme_items/models.py"];
    assert!(!models.contains("OrphanQuery"), "orphan pruned: {models}");
    assert!(models.contains("class Item"), "reachable models stay");
}
