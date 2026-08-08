//! Golden tests: each emitter's full file tree, byte-for-byte.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p soothfast-sdk`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use soothfast_sdk::target::Target;
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

/// A server's own dotenv template, the file an SDK reads its configuration
/// surface out of rather than restating it.
const ENV_TEMPLATE: &str = "\
# Where the server binds. The launcher overrides both.
PORT=8000

# Echoed back by /v1/stats, so a test can see the environment arrive.
# EMBED_SERVER_NOTE=hello
";

/// The same fixture, but bundling its server instead of pointing at a host.
fn embed_options() -> SdkOptions {
    SdkOptions {
        embed: Some("embed_server".into()),
        base_url: None,
        embed_env: BTreeMap::from([("PORT".into(), "0".into())]),
        embed_env_vars: soothfast_sdk::envtemplate::parse(ENV_TEMPLATE),
        ..options()
    }
}

fn golden_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(name)
}

/// Language tooling run against the goldens (pytest, mypy, uv, npm, tsc)
/// drops artifacts next to them, and `sdk build` stages compiled binaries
/// into them; only the emitted files are the fixture.
fn is_artifact(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "__pycache__" | "uv.lock" | "node_modules" | "package-lock.json" | "dist" | "bin"
        )
}

fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if is_artifact(&name.to_string_lossy()) {
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

/// Ignore rules for tool artifacts, so a `npm install` or `pytest` run
/// against a golden never shows up as a diff.
fn gitignore(kind: SdkKind) -> &'static str {
    match kind {
        // `sdk build` stages compiled binaries into these trees; the
        // manifests around them are the golden, the binaries never are.
        SdkKind::Python => ".venv/\n.mypy_cache/\n__pycache__/\nuv.lock\ndist/\n",
        SdkKind::TypeScript => "node_modules/\npackage-lock.json\ndist/\nplatforms/*/bin/\n",
    }
}

fn check_goldens(kind: SdkKind, name: &str, opts: &SdkOptions) {
    let files = SdkKind::emit(kind, &info(), &fixture(), opts)
        .expect("emits")
        .files;

    let dir = golden_dir(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, content) in &files {
            let target = dir.join(rel);
            std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
            std::fs::write(&target, content).expect("write golden");
        }
        std::fs::write(dir.join(".gitignore"), gitignore(kind)).expect("write gitignore");
        return;
    }

    let mut expected = BTreeMap::new();
    walk(&dir, &dir, &mut expected);
    assert!(
        !expected.is_empty(),
        "no {name} goldens found — run UPDATE_GOLDENS=1 cargo test -p soothfast-sdk"
    );
    let got: Vec<&String> = files.keys().collect();
    let want: Vec<&String> = expected.keys().collect();
    assert_eq!(got, want, "file set changed");
    for (rel, content) in &files {
        assert_eq!(content, &expected[rel], "content of {rel} changed");
    }
}

#[test]
fn python_emission_matches_the_goldens() {
    check_goldens(SdkKind::Python, "python", &options());
}

#[test]
fn typescript_emission_matches_the_goldens() {
    check_goldens(SdkKind::TypeScript, "typescript", &options());
}

#[test]
fn python_embed_emission_matches_the_goldens() {
    check_goldens(SdkKind::Python, "python-embed", &embed_options());
}

#[test]
fn typescript_embed_emission_matches_the_goldens() {
    check_goldens(SdkKind::TypeScript, "typescript-embed", &embed_options());
}

#[test]
fn emission_is_deterministic() {
    for kind in SdkKind::ALL {
        for opts in [options(), embed_options()] {
            let a = kind.emit(&info(), &fixture(), &opts).expect("emits");
            let b = kind.emit(&info(), &fixture(), &opts).expect("emits");
            assert_eq!(a.files, b.files, "{}", kind.name());
            assert_eq!(a.notes, b.notes, "{}", kind.name());
        }
    }
}

/// Embedding only ever *adds* — a launcher plus that language's binary
/// packaging. Nothing about the client surface may move.
#[test]
fn embedding_only_adds_a_launcher_and_its_packaging() {
    for kind in SdkKind::ALL {
        let plain = kind.emit(&info(), &fixture(), &options()).expect("emits");
        let embedded = kind
            .emit(&info(), &fixture(), &embed_options())
            .expect("emits");

        let dropped: Vec<&String> = plain
            .files
            .keys()
            .filter(|k| !embedded.files.contains_key(*k))
            .collect();
        assert!(dropped.is_empty(), "{}: dropped {dropped:?}", kind.name());

        let added: Vec<&String> = embedded
            .files
            .keys()
            .filter(|k| !plain.files.contains_key(*k))
            .collect();
        for path in &added {
            assert!(
                path.contains("server")
                    || path.starts_with("platforms/")
                    || *path == "hatch_build.py",
                "{}: unexpected addition {path}",
                kind.name()
            );
        }
        // One launcher, plus one manifest per target for npm and one
        // wheel hook for Python.
        let expected = match kind {
            SdkKind::TypeScript => 1 + Target::DEFAULT.len(),
            SdkKind::Python => 2,
        };
        assert_eq!(added.len(), expected, "{}: {added:?}", kind.name());
    }
}

/// The server's own template is the configuration surface, so a client
/// cannot offer knobs the server does not read, or miss ones it does.
#[test]
fn the_server_template_becomes_a_typed_configuration_surface() {
    let python = SdkKind::Python
        .emit(&info(), &fixture(), &embed_options())
        .expect("emits");
    let client = &python.files["src/acme_items/client.py"];
    assert!(
        client.contains("class ServerEnv(typing.TypedDict, total=False):"),
        "{client}"
    );
    assert!(client.contains("    EMBED_SERVER_NOTE: str"), "{client}");
    assert!(
        client.contains("embedded_base_url(EMBED, server_env)"),
        "the caller's environment reaches the launch: {client}"
    );
    assert!(
        client.contains("(\"PORT\", \"0\")"),
        "the launch settings ride on the config: {client}"
    );

    let typescript = SdkKind::TypeScript
        .emit(&info(), &fixture(), &embed_options())
        .expect("emits");
    let client = &typescript.files["src/client.ts"];
    assert!(client.contains("EMBED_SERVER_NOTE?: string;"), "{client}");
    assert!(client.contains("  serverEnv?: ServerEnv;"), "{client}");
    assert!(
        client.contains("embeddedBaseUrl(EMBED, options.serverEnv)"),
        "{client}"
    );
}

/// A commented-out entry is a knob the server reads but ships unset — the
/// distinction the README has to keep, since those are the ones a caller
/// actually has to supply.
#[test]
fn the_readme_separates_what_is_defaulted_from_what_is_unset() {
    let out = SdkKind::Python
        .emit(&info(), &fixture(), &embed_options())
        .expect("emits");
    let readme = &out.files["README.md"];
    assert!(readme.contains("| `PORT` | `8000` |"), "{readme}");
    assert!(
        readme.contains("| `EMBED_SERVER_NOTE` | _unset_ |"),
        "{readme}"
    );
    assert!(
        readme.contains("server_env={\"EMBED_SERVER_NOTE\": \"...\"}"),
        "the example leads with what the caller must supply: {readme}"
    );
}

/// A server with no template documented still has an environment; the
/// untyped map is the surface, not an absent feature.
#[test]
fn a_server_without_a_template_still_takes_an_environment() {
    let opts = SdkOptions {
        embed_env_vars: Vec::new(),
        ..embed_options()
    };
    let python = SdkKind::Python
        .emit(&info(), &fixture(), &opts)
        .expect("emits");
    let client = &python.files["src/acme_items/client.py"];
    assert!(!client.contains("class ServerEnv"), "nothing to type");
    assert!(
        client.contains("server_env: typing.Mapping[str, str] | None = None,"),
        "{client}"
    );

    let typescript = SdkKind::TypeScript
        .emit(&info(), &fixture(), &opts)
        .expect("emits");
    let client = &typescript.files["src/client.ts"];
    assert!(
        client.contains("[key: string]: string | undefined;"),
        "the index signature is the whole surface: {client}"
    );
}

/// A narrowed matrix must reach both the manifests on disk and the
/// `optionalDependencies` that resolve them, or npm would try to install a
/// package that was never published.
#[test]
fn the_target_matrix_drives_the_manifests_and_the_optional_deps_together() {
    let opts = SdkOptions {
        targets: vec![
            "x86_64-unknown-linux-gnu".into(),
            "aarch64-apple-darwin".into(),
        ],
        ..embed_options()
    };
    let out = SdkKind::TypeScript
        .emit(&info(), &fixture(), &opts)
        .expect("emits");

    let manifests: Vec<&String> = out
        .files
        .keys()
        .filter(|k| k.starts_with("platforms/"))
        .collect();
    assert_eq!(
        manifests,
        [
            "platforms/acme-items-darwin-arm64/package.json",
            "platforms/acme-items-linux-x64/package.json",
        ]
    );

    let package_json = &out.files["package.json"];
    assert!(
        package_json.contains("\"acme-items-linux-x64\": \"1.2.3\""),
        "{package_json}"
    );
    assert!(
        package_json.contains("\"acme-items-darwin-arm64\": \"1.2.3\""),
        "{package_json}"
    );
    assert!(
        !package_json.contains("win32"),
        "an unbuilt target must not be advertised: {package_json}"
    );
}

#[test]
fn an_unknown_target_is_refused_before_anything_is_emitted() {
    let opts = SdkOptions {
        targets: vec!["sparc-unknown-none".into()],
        ..embed_options()
    };
    let err = SdkKind::TypeScript
        .emit(&info(), &fixture(), &opts)
        .expect_err("rejected");
    assert!(err.contains("sparc"), "{err}");
}

/// Without a host *or* a bundled server there is nowhere to send a request,
/// and that is the only configuration the emitters refuse.
#[test]
fn embedding_removes_the_need_for_a_base_url() {
    for kind in SdkKind::ALL {
        kind.emit(&info(), &fixture(), &embed_options())
            .expect("a bundled server is a base URL");

        let neither = SdkOptions {
            base_url: None,
            ..options()
        };
        let err = kind
            .emit(&info(), &fixture(), &neither)
            .expect_err("nowhere to send a request");
        assert!(err.contains("embed"), "{}: {err}", kind.name());
    }
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

/// The pair that collides under snake_case is two distinct keys in
/// TypeScript, so there is nothing to disambiguate and nothing to report.
#[test]
fn typescript_keeps_both_wire_spellings_without_a_collision() {
    let out = SdkKind::TypeScript
        .emit(&info(), &fixture(), &options())
        .expect("emits");
    let models = &out.files["src/models.ts"];
    assert!(models.contains("logoUrl?: string;"), "{models}");
    assert!(models.contains("logo_url?: string;"), "{models}");
    assert!(
        !out.notes.iter().any(|n| n.contains("collides")),
        "no collision to report: {:?}",
        out.notes
    );
}

/// Reserved words and other unquotable keys have to survive as-is on the
/// wire, so they are emitted quoted rather than renamed.
#[test]
fn typescript_quotes_the_keys_it_cannot_spell_bare() {
    let mut ops = fixture();
    ops[0].shape.components.insert(
        "Item".into(),
        json!({
            "type": "object",
            "properties": {
                "50DayAverage": { "type": "number" },
                "from": { "type": "string" },
                "id": { "type": "integer" },
            },
            "required": ["id"],
        }),
    );
    ops.truncate(1);
    let out = SdkKind::TypeScript
        .emit(&info(), &ops, &options())
        .expect("emits");
    let models = &out.files["src/models.ts"];
    assert!(models.contains("\"50DayAverage\"?: number;"), "{models}");
    // `from` is reserved as a binding but legal as a property key.
    assert!(models.contains("  from?: string;"), "{models}");
}

#[test]
fn conflicting_component_schemas_are_refused() {
    let mut ops = fixture();
    ops[1]
        .shape
        .components
        .insert("Item".into(), json!({ "type": "object", "properties": {} }));
    for kind in SdkKind::ALL {
        let err = kind.emit(&info(), &ops, &options()).expect_err("conflict");
        assert!(err.contains("Item"), "{}: {err}", kind.name());
    }
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

    let out = SdkKind::TypeScript
        .emit(&info(), &ops, &options())
        .expect("emits");
    let models = &out.files["src/models.ts"];
    assert!(!models.contains("OrphanQuery"), "orphan pruned: {models}");
    assert!(
        models.contains("export interface Item"),
        "reachable models stay"
    );
}
