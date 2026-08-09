//! `cargo soothfast spec html -p PKG` — render every generate-mode spec file
//! (OpenAPI / AsyncAPI / MCP tools) as a themed, nav-integrated docs-site
//! page.
//!
//! No CDN, no Node, no live-server round-trip: this renders the exact same
//! [`soothfast_spec::dialect::Document`] values `spec gen` writes to disk,
//! as markdown + raw HTML fragments for `soothfast-site` to theme,
//! search-index, and slot into the TOC — the same recipe `docs routes`
//! already uses for the reconciliation report. The shape deliberately
//! mirrors a standardized API reference (Swagger UI / the AsyncAPI
//! generator / ReDoc): operations, channels, tools and schemas are
//! collapsed one-line rows grouped by resource; a parameter's available
//! values, default, and description are always visible in the reference
//! table (never gated behind "Try it out"); enum parameters get a real
//! `<select>`, not free text; and request/response bodies show a
//! synthesized example alongside their schema, one click apart. GraphQL is
//! skipped: it has no generate-mode consumer in practice (served live,
//! e.g. a Playground), and its document is a type graph rather than the
//! JSON-Schema-flavoured shapes the renderers below share.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;
use soothfast_spec::SpecKind;
use soothfast_spec::dialect::Info;

use crate::invoke::{self, CommonArgs};
use crate::spec::escape;
use crate::spec_config;
use crate::spec_gen;

/// Path-item verb keys in the order a reader expects them, not alphabetical.
const ORDERED_VERBS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "options", "head", "trace",
];

/// A row's disclosure chevron — the exact mark `docs routes`' `.route-group`
/// already uses, so a collapsed spec row and a collapsed route group read
/// as the same interaction, not two different widgets.
const CHEVRON: &str = "<svg class=\"spec-row-chevron\" width=\"11\" height=\"11\" viewBox=\"0 0 16 16\" fill=\"none\" aria-hidden=\"true\"><path d=\"M5 2.5 L11 8 L5 13.5\" stroke=\"currentColor\" stroke-width=\"1.6\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>";

pub fn run(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut out: Option<std::path::PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--out" {
            out = it.next().map(std::path::PathBuf::from);
        } else if !common.try_parse(a, &mut it) {
            eprintln!("soothfast: unknown spec html arg {a:?}");
            return 2;
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: spec html requires -p PKG");
        return 2;
    };
    match generate(&pkg, &common, out.as_deref()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

fn generate(pkg: &str, common: &CommonArgs, out: Option<&Path>) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let cfg = spec_config::load(&meta.dir)?;
    let built = spec_gen::build(pkg, common, None, &cfg, &meta)?;

    if built.docs.is_empty() {
        println!(
            "spec html: nothing to render — no [[spec]] entry in soothfast.toml \
             sets mode = \"generate\" for a spec file any #[route] declares"
        );
        return Ok(0);
    }
    if !built.conflicts.is_empty() {
        for c in &built.conflicts {
            println!("FAIL  {c}");
        }
        println!(
            "spec html: FAILED ({} conflict(s)) — fix with `cargo soothfast spec gen -p {pkg}` first",
            built.conflicts.len()
        );
        return Ok(1);
    }

    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let out_dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join("docs/api"));
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;

    let mut skipped = 0u32;
    for (spec_file, value) in &built.docs {
        let kind = cfg.dialect_of(spec_file);
        let entry = cfg.for_path(spec_file);
        let info = spec_gen::info_for(entry, pkg, &meta);
        let gaps = built.gaps.get(spec_file).map(Vec::as_slice).unwrap_or(&[]);
        let notes = built.notes.get(spec_file).map(Vec::as_slice).unwrap_or(&[]);

        let md = match kind {
            SpecKind::OpenApi => render_openapi(spec_file, &info, value, gaps, notes),
            SpecKind::AsyncApi => render_asyncapi(spec_file, &info, value, gaps, notes),
            SpecKind::McpTools => render_mcp(spec_file, pkg, value, gaps, notes),
            SpecKind::GraphQl => {
                skipped += 1;
                println!(
                    "spec html: {spec_file} [graphql] — HTML rendering is not supported for the \
                     GraphQL dialect yet, skipping"
                );
                continue;
            }
        };

        let (operations, schemas) = kind.summarize(value);
        let path = out_dir.join(format!("{}.md", page_stem(spec_file)));
        std::fs::write(&path, md).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!(
            "spec html: {spec_file} [{}] -> {} — {operations} operation(s), {schemas} schema(s)",
            kind.name(),
            path.display(),
        );
    }
    Ok(if skipped > 0 && built.docs.len() == skipped as usize {
        1
    } else {
        0
    })
}

/// A filesystem-friendly page name for a spec file, distinct from the spec
/// file's own extension (`openapi.yaml` -> `openapi`).
fn page_stem(spec_file: &str) -> String {
    let base = spec_file.rsplit('/').next().unwrap_or(spec_file);
    base.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(base)
        .to_string()
}

/// Gap/note admonitions shared by every dialect: what the extractor could
/// not derive from the code (a shortfall in the *source*), and what the
/// target dialect cannot express as precisely as the code states it (a
/// shortfall in the *target*) — reported apart so the remedy isn't confused.
fn push_gaps_notes(out: &mut String, gaps: &[String], notes: &[String]) {
    if !gaps.is_empty() {
        out.push_str(&format!(
            "!!! warning \"{} gap(s) the extractor could not derive\"\n",
            gaps.len()
        ));
        for g in gaps {
            out.push_str(&format!("    - {g}\n"));
        }
        out.push('\n');
    }
    if !notes.is_empty() {
        out.push_str(&format!(
            "!!! note \"{} note(s): shapes this dialect cannot express as precisely as the code states them\"\n",
            notes.len()
        ));
        for n in notes {
            out.push_str(&format!("    - {n}\n"));
        }
        out.push('\n');
    }
}

/// A method/action badge, colored by what it actually costs a caller —
/// read, create, mutate, destroy — reusing the pass/warn/fail evidence
/// palette rather than an arbitrary per-verb rainbow, but still solid-filled
/// and immediately legible the way Swagger UI's own method badges are.
fn method_class(verb: &str) -> &'static str {
    match verb.to_ascii_lowercase().as_str() {
        "get" | "receive" | "query" => "spec-method-get",
        "post" | "send" => "spec-method-post",
        "put" | "patch" | "mutation" => "spec-method-put",
        "delete" => "spec-method-delete",
        _ => "spec-method-options",
    }
}

fn method_badge(verb: &str) -> String {
    format!(
        "<span class=\"spec-method {}\">{}</span>",
        method_class(verb),
        escape(&verb.to_ascii_uppercase()),
    )
}

/// The resource a path belongs to, standing in for an OpenAPI `tags` list
/// the extractor doesn't produce (routes carry no tag annotation today): a
/// version-shaped first segment (`v2`, `v1`) is skipped so the group is the
/// resource name a reader actually recognizes (`/v2/quote/{symbol}` ->
/// `quote`), matching how Swagger UI groups operations by tag.
fn group_key(path: &str) -> String {
    let mut parts = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty());
    let first = parts.next().unwrap_or("");
    let looks_versioned = first.len() >= 2
        && first.starts_with('v')
        && first[1..].chars().all(|c| c.is_ascii_digit());
    let key = if looks_versioned {
        parts.next().unwrap_or(first)
    } else {
        first
    };
    if key.is_empty() {
        "root".to_string()
    } else {
        key.to_string()
    }
}

/// `capital-gains` -> `Capital gains` — a group heading, not a slug.
fn group_title(key: &str) -> String {
    let mut chars = key.chars();
    let capitalized = match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    };
    capitalized.replace(['-', '_'], " ")
}

/// A JSON value as a caller would type it — the string itself, unquoted,
/// or the literal for anything else — for use in "Available values" /
/// "Default value" prose and as an HTML attribute value, not as embedded
/// JSON.
fn json_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ---------- OpenAPI ----------

fn render_openapi(
    spec_file: &str,
    info: &Info,
    value: &Value,
    gaps: &[String],
    notes: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", info.title));
    out.push_str(&format!(
        "> Generated by `cargo soothfast spec html` from `{spec_file}` (OpenAPI {}). \
         Regenerate with `cargo soothfast spec gen` + `spec html`; do not hand-edit.\n\n",
        value["openapi"].as_str().unwrap_or("3.1.0"),
    ));
    if !info.version.is_empty() {
        out.push_str(&format!("**Version:** `{}`\n\n", info.version));
    }
    if let Some(d) = &info.description {
        out.push_str(&format!("{d}\n\n"));
    }
    if !info.servers.is_empty() {
        out.push_str("**Servers:**\n\n");
        for s in &info.servers {
            out.push_str(&format!("- `{s}`\n"));
        }
        out.push('\n');
    }
    push_gaps_notes(&mut out, gaps, notes);

    let components = value["components"]["schemas"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let paths = value["paths"].as_object().cloned().unwrap_or_default();
    let mut groups: BTreeMap<String, Vec<(String, &str, Value)>> = BTreeMap::new();
    let mut total = 0usize;
    for (path, item) in &paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for verb in ORDERED_VERBS.iter().filter(|v| item.contains_key(**v)) {
            groups.entry(group_key(path)).or_default().push((
                path.clone(),
                verb,
                item[*verb].clone(),
            ));
            total += 1;
        }
    }
    out.push_str(&format!(
        "\n## Endpoints <sup>{total} endpoint(s), {} group(s)</sup>\n",
        groups.len()
    ));
    for (key, ops) in &groups {
        out.push_str(&format!(
            "\n### {} <sup>{} endpoint(s)</sup>\n\n",
            group_title(key),
            ops.len()
        ));
        out.push_str("<div class=\"spec-rowlist\">");
        for (path, verb, op) in ops {
            out.push_str(&operation_row_html(
                verb,
                path,
                op,
                &info.servers,
                &components,
            ));
        }
        out.push_str("</div>\n");
    }

    out.push_str(&schemas_section(&value["components"]["schemas"]));
    out
}

/// One collapsed operation row: method badge, path, summary — expands to
/// parameters (with an inline "Try it out" value column), the request
/// body, and responses.
fn operation_row_html(
    verb: &str,
    path: &str,
    op: &Value,
    servers: &[String],
    defs: &serde_json::Map<String, Value>,
) -> String {
    let op_id = op["operationId"].as_str().unwrap_or(path);
    let summary = op["summary"].as_str().unwrap_or("");
    let request_schema = op["requestBody"]["content"]
        .as_object()
        .and_then(|content| content.values().next())
        .map(|media| media["schema"].clone());

    let mut body = String::new();
    body.push_str(&parameters_and_try_html(
        op["parameters"].as_array(),
        request_schema.as_ref(),
        servers,
        defs,
    ));
    if let Some(rb) = op.get("requestBody") {
        body.push_str(&request_body_html(rb, defs));
    }
    if let Some(responses) = op["responses"].as_object() {
        body.push_str(&responses_html(responses, defs));
    }

    format!(
        "<details class=\"spec-row\" data-method=\"{}\" data-path=\"{}\"><summary>{}<code class=\"spec-row-path\" title=\"{}\">{}</code><span class=\"spec-row-desc\">{}</span>{CHEVRON}</summary><div class=\"spec-body\">{body}</div></details>",
        escape(&verb.to_ascii_uppercase()),
        escape(path),
        method_badge(verb),
        escape(op_id),
        escape(path),
        escape(summary),
    )
}

// ---------- AsyncAPI ----------

fn render_asyncapi(
    spec_file: &str,
    info: &Info,
    value: &Value,
    gaps: &[String],
    notes: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", info.title));
    out.push_str(&format!(
        "> Generated by `cargo soothfast spec html` from `{spec_file}` (AsyncAPI {}). \
         Regenerate with `cargo soothfast spec gen` + `spec html`; do not hand-edit.\n\n",
        value["asyncapi"].as_str().unwrap_or("3.0.0"),
    ));
    if !info.version.is_empty() {
        out.push_str(&format!("**Version:** `{}`\n\n", info.version));
    }
    if let Some(d) = &info.description {
        out.push_str(&format!("{d}\n\n"));
    }
    if let Some(servers) = value["servers"].as_object().filter(|s| !s.is_empty()) {
        out.push_str("**Servers:**\n\n");
        for (name, s) in servers {
            out.push_str(&format!(
                "- `{name}` — `{}://{}{}`\n",
                s["protocol"].as_str().unwrap_or(""),
                s["host"].as_str().unwrap_or(""),
                s["pathname"].as_str().unwrap_or(""),
            ));
        }
        out.push('\n');
    }
    push_gaps_notes(&mut out, gaps, notes);

    let components = value["components"]["schemas"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let messages = value["components"]["messages"]
        .as_object()
        .cloned()
        .unwrap_or_default();

    let channels = value["channels"].as_object().cloned().unwrap_or_default();
    out.push_str(&format!(
        "\n## Channels <sup>{} channel(s)</sup>\n\n",
        channels.len()
    ));
    out.push_str("<div class=\"spec-rowlist\">");
    for (key, ch) in &channels {
        let mut body = String::new();
        if let Some(params) = ch["parameters"].as_object().filter(|p| !p.is_empty()) {
            body.push_str("<p class=\"spec-subhead\">Parameters</p>");
            let mut rows = String::new();
            for (name, p) in params {
                let desc = p["description"].as_str().unwrap_or("");
                rows.push_str(&format!(
                    "<tr><td><code>{}</code></td><td>{}</td></tr>",
                    escape(name),
                    escape(desc),
                ));
            }
            body.push_str(&format!(
                "<div class=\"tablewrap\"><table><thead><tr><th>name</th><th>description</th></tr></thead><tbody>{rows}</tbody></table></div>",
            ));
        }
        let msg_names: Vec<&str> = ch["messages"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.keys().map(String::as_str))
            .collect();
        if !msg_names.is_empty() {
            let links: Vec<String> = msg_names
                .iter()
                .map(|n| format!("<code>{}</code>", escape(n)))
                .collect();
            body.push_str(&format!(
                "<p class=\"spec-subhead\">Messages</p><p>{}</p>",
                links.join(", "),
            ));
        }
        out.push_str(&format!(
            "<details class=\"spec-row\" open><summary><code class=\"spec-row-name\">{}</code><span class=\"spec-row-desc\">{}</span>{CHEVRON}</summary><div class=\"spec-body\">{body}</div></details>",
            escape(ch["address"].as_str().unwrap_or(key)),
            escape(&msg_names.join(", ")),
        ));
    }
    out.push_str("</div>\n");

    let operations = value["operations"].as_object().cloned().unwrap_or_default();
    out.push_str(&format!(
        "\n## Operations <sup>{} operation(s)</sup>\n\n",
        operations.len()
    ));
    out.push_str("<div class=\"spec-rowlist\">");
    for (op_id, op) in &operations {
        let action = op["action"].as_str().unwrap_or("");
        let channel_ref = op["channel"]["$ref"].as_str().unwrap_or("");
        let channel_key = channel_ref.rsplit('/').next().unwrap_or(channel_ref);
        let summary = op["summary"].as_str().unwrap_or("");

        let mut body = String::new();
        if let Some(msg_ref) = op["messages"][0]["$ref"].as_str() {
            let msg_name = msg_ref.rsplit('/').next().unwrap_or(msg_ref);
            if let Some(msg) = messages.get(msg_name) {
                body.push_str(&format!(
                    "<p class=\"spec-subhead\">Payload · <code>{}</code></p>",
                    escape(msg_name),
                ));
                body.push_str(&schema_with_example_html(&msg["payload"], &components));
            }
        }
        out.push_str(&format!(
            "<details class=\"spec-row\" open><summary>{}<code class=\"spec-row-path\" title=\"{}\">{}</code><span class=\"spec-row-desc\">{}</span>{CHEVRON}</summary><div class=\"spec-body\">{body}</div></details>",
            method_badge(action),
            escape(op_id),
            escape(channel_key),
            escape(summary),
        ));
    }
    out.push_str("</div>\n");

    out.push_str(&schemas_section(&value["components"]["schemas"]));
    out
}

// ---------- MCP tools ----------

fn render_mcp(
    spec_file: &str,
    pkg: &str,
    value: &Value,
    gaps: &[String],
    notes: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# `{pkg}` MCP tools\n\n"));
    out.push_str(&format!(
        "> Generated by `cargo soothfast spec html` from `{spec_file}` (MCP tool manifest). \
         Regenerate with `cargo soothfast spec gen` + `spec html`; do not hand-edit.\n\n"
    ));
    let tools = value["tools"].as_array().cloned().unwrap_or_default();
    out.push_str(&format!("**{} tool(s).**\n\n", tools.len()));
    push_gaps_notes(&mut out, gaps, notes);

    out.push_str("<div class=\"spec-rowlist\">");
    for tool in &tools {
        let name = tool["name"].as_str().unwrap_or("tool");
        let desc = tool["description"].as_str().unwrap_or("");

        // Each tool carries its own inlined $defs (see soothfast-spec's mcp
        // dialect doc comment) — the resolution scope for this tool's refs
        // is the union of its input and output schemas' own $defs, not a
        // document-wide components map (MCP has none).
        let mut defs: serde_json::Map<String, Value> = serde_json::Map::new();
        for schema_key in ["inputSchema", "outputSchema"] {
            if let Some(d) = tool[schema_key]["$defs"].as_object() {
                defs.extend(d.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }

        let mut body = String::new();
        body.push_str("<p class=\"spec-subhead\">Arguments</p>");
        body.push_str(&schema_with_example_html(&tool["inputSchema"], &defs));
        if let Some(output) = tool.get("outputSchema") {
            body.push_str("<p class=\"spec-subhead\">Result</p>");
            body.push_str(&schema_with_example_html(output, &defs));
        }
        if !defs.is_empty() {
            body.push_str("<p class=\"spec-subhead\">Types</p>");
            for (dname, dschema) in &defs {
                body.push_str(&format!("<p><code>{}</code></p>", escape(dname)));
                body.push_str(&schema_html(dschema));
            }
        }

        out.push_str(&format!(
            "<details class=\"spec-row\"><summary><span class=\"spec-row-name\">{}</span><span class=\"spec-row-desc\">{}</span>{CHEVRON}</summary><div class=\"spec-body\">{body}</div></details>",
            escape(name),
            escape(desc),
        ));
    }
    out.push_str("</div>\n");
    out
}

// ---------- shared: parameters + inline "Try it out" ----------

/// The parameter reference table and, when a server is known to try
/// against, an inline value column (a real `<select>` for an enum
/// parameter, not free text) plus a request-body editor and Execute
/// button — all hidden until "Try it out" is clicked, but built into the
/// same table the reference (available values, defaults, descriptions)
/// already lives in, so trying an endpoint never requires re-deriving
/// values that were already shown.
fn parameters_and_try_html(
    params: Option<&Vec<Value>>,
    request_schema: Option<&Value>,
    servers: &[String],
    defs: &serde_json::Map<String, Value>,
) -> String {
    let params: &[Value] = params.map(Vec::as_slice).unwrap_or(&[]);
    let can_try = !servers.is_empty();
    if params.is_empty() && !can_try {
        return String::new();
    }

    let mut out = String::new();
    let toggle = if can_try {
        " <button type=\"button\" class=\"spec-try-toggle\">Try it out</button>"
    } else {
        ""
    };
    out.push_str(&format!("<p class=\"spec-subhead\">Parameters{toggle}</p>"));

    if !params.is_empty() {
        let mut rows = String::new();
        for p in params {
            let name = p["name"].as_str().unwrap_or("");
            let loc = p["in"].as_str().unwrap_or("query");
            let schema = &p["schema"];
            // A parameter's schema is commonly just `{ description, $ref }`
            // — description overriding/adding to the referenced schema,
            // per 2020-12's ref-as-applicator semantics — so `enum` and
            // `default` most often live on the *referenced* schema
            // (`Region`, say), not this one.
            let ref_target = resolve_ref(schema, defs);
            let ty = type_str(schema);
            let req_badge = if p["required"].as_bool().unwrap_or(false) {
                "<span class=\"spec-required\">required</span>"
            } else {
                "<span class=\"spec-optional\">optional</span>"
            };
            let desc = schema["description"]
                .as_str()
                .or_else(|| ref_target.and_then(|r| r["description"].as_str()))
                .unwrap_or("");
            let enum_vals: Vec<Value> = schema["enum"]
                .as_array()
                .cloned()
                .filter(|v| !v.is_empty())
                .or_else(|| {
                    ref_target
                        .and_then(|r| r["enum"].as_array().cloned())
                        .filter(|v| !v.is_empty())
                })
                .unwrap_or_default();
            let default_val: Value = {
                let own = schema["default"].clone();
                if !own.is_null() {
                    own
                } else {
                    ref_target
                        .map(|r| r["default"].clone())
                        .unwrap_or(Value::Null)
                }
            };

            let mut meta = String::new();
            if !enum_vals.is_empty() {
                let items: Vec<String> = enum_vals.iter().map(json_plain).collect();
                meta.push_str(&format!(
                    "<p class=\"spec-param-meta\"><em>Available values</em>: {}</p>",
                    escape(&items.join(", ")),
                ));
            }
            if !default_val.is_null() {
                meta.push_str(&format!(
                    "<p class=\"spec-param-meta\"><em>Default value</em>: <code>{}</code></p>",
                    escape(&json_plain(&default_val)),
                ));
            }

            let try_cell = if can_try && (loc == "path" || loc == "query") {
                let field = if !enum_vals.is_empty() {
                    let default_str = json_plain(&default_val);
                    let opts: String = enum_vals
                        .iter()
                        .map(|v| {
                            let s = json_plain(v);
                            let sel = if s == default_str { " selected" } else { "" };
                            format!(
                                "<option value=\"{}\"{sel}>{}</option>",
                                escape(&s),
                                escape(&s)
                            )
                        })
                        .collect();
                    format!("<select data-name=\"{}\">{opts}</select>", escape(name))
                } else {
                    let val_attr = if default_val.is_null() {
                        String::new()
                    } else {
                        format!(" value=\"{}\"", escape(&json_plain(&default_val)))
                    };
                    format!(
                        "<input type=\"text\" data-name=\"{}\"{val_attr}>",
                        escape(name)
                    )
                };
                format!("<td class=\"spec-try-col\" data-param=\"{loc}\" hidden>{field}</td>")
            } else if can_try {
                "<td class=\"spec-try-col\" hidden></td>".to_string()
            } else {
                String::new()
            };

            rows.push_str(&format!(
                "<tr><td><code>{}</code> {req_badge}<div class=\"spec-param-meta-type\">{} ({})</div></td><td>{}{meta}</td>{try_cell}</tr>",
                escape(name),
                escape(&ty),
                escape(loc),
                escape(desc),
            ));
        }
        let try_th = if can_try {
            "<th class=\"spec-try-col\" hidden>value</th>"
        } else {
            ""
        };
        out.push_str(&format!(
            "<div class=\"tablewrap\"><table><thead><tr><th>name</th><th>description</th>{try_th}</tr></thead><tbody>{rows}</tbody></table></div>",
        ));
    }

    if can_try {
        if let Some(schema) = request_schema {
            let example = example_value(schema, defs, &mut BTreeSet::new());
            let example_json = serde_json::to_string_pretty(&example).unwrap_or_default();
            out.push_str(&format!(
                "<div class=\"spec-try-body\" hidden><p class=\"spec-subhead\">Request body</p><textarea class=\"spec-try-bodytext\">{}</textarea></div>",
                escape(&example_json),
            ));
        }
        let options: String = servers
            .iter()
            .map(|s| format!("<option value=\"{}\">{}</option>", escape(s), escape(s)))
            .collect();
        out.push_str(&format!(
            "<div class=\"spec-try-actions\" hidden><select class=\"spec-try-server\">{options}</select><button type=\"button\" class=\"spec-try-send\">Execute</button></div>\
<div class=\"spec-try-result\" hidden><div class=\"spec-try-status\"></div><pre class=\"spec-try-out\"></pre></div>",
        ));
    }

    out
}

fn request_body_html(rb: &Value, defs: &serde_json::Map<String, Value>) -> String {
    let mut out = String::from("<p class=\"spec-subhead\">Request body</p>");
    for (ct, media) in rb["content"].as_object().into_iter().flatten() {
        out.push_str(&format!("<p class=\"spec-ct\">{}</p>", escape(ct)));
        out.push_str(&schema_with_example_html(&media["schema"], defs));
    }
    out
}

fn responses_html(
    responses: &serde_json::Map<String, Value>,
    defs: &serde_json::Map<String, Value>,
) -> String {
    let mut out = String::from("<p class=\"spec-subhead\">Responses</p>");
    for (code, resp) in responses {
        let desc = resp["description"].as_str().unwrap_or("");
        out.push_str(&format!(
            "<div class=\"spec-status-line\"><code class=\"spec-status-code {}\">{}</code><span class=\"spec-status-desc\">{}</span></div>",
            status_class(code),
            escape(code),
            escape(desc),
        ));
        for (ct, media) in resp["content"].as_object().into_iter().flatten() {
            out.push_str(&format!("<p class=\"spec-ct\">{}</p>", escape(ct)));
            out.push_str(&schema_with_example_html(&media["schema"], defs));
        }
    }
    out
}

/// Which status-code bucket a response's badge is colored by. `default` and
/// any other non-numeric code fall into the same "something went wrong"
/// bucket a 4xx gets — a reader scanning for the unhappy paths shouldn't
/// have to know OpenAPI's `default` keyword to find them.
fn status_class(code: &str) -> &'static str {
    match code.as_bytes().first() {
        Some(b'2') => "spec-status-2xx",
        Some(b'3') => "spec-status-3xx",
        Some(b'4') => "spec-status-4xx",
        Some(b'5') => "spec-status-5xx",
        _ => "spec-status-4xx",
    }
}

/// One collapsed row per named schema, grouped under a single list — 180
/// always-open cards would be a wall the reader has to scroll past, not a
/// reference they can scan, so each schema opens on demand like an
/// operation row does.
fn schemas_section(schemas: &Value) -> String {
    let Some(schemas) = schemas.as_object().filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let mut out = format!("\n## Schemas <sup>{} type(s)</sup>\n\n", schemas.len());
    out.push_str("<div class=\"spec-rowlist\">");
    for (name, schema) in schemas {
        let mut body = String::new();
        if let Some(d) = schema["description"].as_str() {
            body.push_str(&format!("<p>{}</p>", escape(d)));
        }
        body.push_str(&schema_html(schema));
        out.push_str(&format!(
            "<details class=\"spec-row\"><summary><span class=\"spec-row-name\">{}</span><span class=\"spec-row-desc\">{}</span>{CHEVRON}</summary><div class=\"spec-body\">{body}</div></details>",
            escape(name),
            escape(&schema_hint(schema)),
        ));
    }
    out.push_str("</div>\n");
    out
}

/// The short "object · 4 fields" line shown next to a schema's name while
/// its row is collapsed — enough to judge relevance without opening it.
fn schema_hint(schema: &Value) -> String {
    if let Some(props) = schema
        .get("properties")
        .and_then(Value::as_object)
        .filter(|p| !p.is_empty())
    {
        let n = props.len();
        return format!("object · {n} field{}", if n == 1 { "" } else { "s" });
    }
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        let n = variants.len();
        return format!("oneOf · {n} variant{}", if n == 1 { "" } else { "s" });
    }
    if schema.get("enum").is_some() {
        return "enum".to_string();
    }
    type_str(schema)
}

// ---------- shared: JSON Schema rendering ----------

/// `<table>` of an object schema's properties (field / type / required /
/// description), or a one-line type/enum summary when there is nothing to
/// tabulate (an array, a bare string, a `oneOf` union).
fn schema_html(schema: &Value) -> String {
    let props = schema.get("properties").and_then(Value::as_object);
    let Some(props) = props.filter(|p| !p.is_empty()) else {
        if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
            return oneof_html(variants);
        }
        if let Some(vals) = schema
            .get("enum")
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty())
        {
            let items: Vec<String> = vals
                .iter()
                .map(|v| format!("<code>{}</code>", escape(&v.to_string())))
                .collect();
            return format!("<p>one of: {}</p>", items.join(", "));
        }
        return format!("<p><code>{}</code></p>", escape(&type_str(schema)));
    };
    let required: std::collections::BTreeSet<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut rows = String::new();
    for (name, prop) in props {
        let req = if required.contains(name.as_str()) {
            "<span class=\"spec-required\">required</span>"
        } else {
            "<span class=\"spec-optional\">optional</span>"
        };
        let desc = prop["description"].as_str().unwrap_or("");
        let enum_html = prop["enum"]
            .as_array()
            .filter(|v| !v.is_empty())
            .map(|vals| {
                let items: Vec<String> = vals
                    .iter()
                    .map(|v| format!("<code>{}</code>", escape(&v.to_string())))
                    .collect();
                format!(" · one of {}", items.join(", "))
            })
            .unwrap_or_default();
        rows.push_str(&format!(
            "<tr><td><code>{}</code></td><td><code>{}</code></td><td>{req}</td><td>{}{enum_html}</td></tr>",
            escape(name),
            escape(&type_str(prop)),
            escape(desc),
        ));
    }
    format!(
        "<div class=\"tablewrap\"><table><thead><tr><th>field</th><th>type</th><th>required</th><th>description</th></tr></thead><tbody>{rows}</tbody></table></div>",
    )
}

fn oneof_html(variants: &[Value]) -> String {
    let items: Vec<String> = variants
        .iter()
        .map(|v| format!("<li>{}</li>", schema_html(v)))
        .collect();
    format!(
        "<p>one of:</p><ul class=\"spec-oneof\">{}</ul>",
        items.join("")
    )
}

/// A short, code-formatted type description for a JSON Schema fragment —
/// shared by all three dialects, since request/response/tool schemas are
/// all draft 2020-12 JSON Schema underneath. A `$ref` (to `#/components/
/// schemas/…` or an MCP tool's own inlined `#/$defs/…`) resolves to the
/// name it points at rather than the unhelpful "any" a reader would
/// otherwise see — the type is still findable, just not re-expanded here.
fn type_str(schema: &Value) -> String {
    if let Some(name) = ref_name(schema) {
        return name;
    }
    if let Some(variants) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        return variants
            .iter()
            .map(type_str)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        return all.iter().map(type_str).collect::<Vec<_>>().join(" & ");
    }
    if let Some(c) = schema.get("const") {
        return c.to_string();
    }
    let base = match schema.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" | "),
        _ if schema.get("properties").is_some() => "object".to_string(),
        _ => "any".to_string(),
    };
    if base == "array" {
        let items = schema
            .get("items")
            .map(type_str)
            .unwrap_or_else(|| "any".into());
        return format!("{items}[]");
    }
    base
}

fn ref_name(schema: &Value) -> Option<String> {
    let r = schema.get("$ref")?.as_str()?;
    r.rsplit('/').next().map(String::from)
}

/// The schema a `$ref` points at, when `schema` is one — JSON Schema
/// 2020-12 (what OpenAPI 3.1 targets) treats a `$ref` as an applicator like
/// any other, so sibling keywords (commonly just `description`, overriding
/// or adding to the referenced schema's own) coexist with it rather than
/// replacing it; a parameter's `enum`/`default` most often live on the
/// *referenced* schema, not the parameter's own.
fn resolve_ref<'a>(schema: &Value, defs: &'a serde_json::Map<String, Value>) -> Option<&'a Value> {
    ref_name(schema).and_then(|name| defs.get(&name))
}

// ---------- shared: synthesized example values ----------

static TAB_ID: AtomicU32 = AtomicU32::new(0);

fn next_tab_id() -> u32 {
    TAB_ID.fetch_add(1, Ordering::Relaxed)
}

/// A schema resolved into a plausible instance — the literal `example` or
/// `default` when the schema carries one, else the first enum value or
/// `oneOf` variant, else a recursively-built object/array of placeholder
/// leaves — the way Swagger UI synthesizes its own "Example Value" tab when
/// the spec has no literal example. `defs` is whatever map `$ref`s in this
/// schema resolve against: `components.schemas` for OpenAPI/AsyncAPI, or an
/// MCP tool's own inlined `$defs`.
///
/// Cycle-tracked, not depth-capped: `seen` names every component currently
/// being expanded on the path from the root to here, so a genuinely
/// self-referential schema still terminates (re-entering a name already on
/// the path returns `null` instead of recursing forever) without an
/// arbitrary depth limit cutting off legitimately deep-but-finite shapes —
/// a paginated batch response nests several such $refs (batch → page →
/// edge → node) well past what a small fixed cap would allow.
fn example_value(
    schema: &Value,
    defs: &serde_json::Map<String, Value>,
    seen: &mut BTreeSet<String>,
) -> Value {
    if let Some(name) = ref_name(schema) {
        if !seen.insert(name.clone()) {
            return Value::Null;
        }
        let result = match defs.get(&name) {
            Some(resolved) => example_value(resolved, defs, seen),
            None => Value::Null,
        };
        seen.remove(&name);
        return result;
    }
    if let Some(ex) = schema.get("example") {
        return ex.clone();
    }
    if let Some(default) = schema.get("default") {
        return default.clone();
    }
    if let Some(vals) = schema.get("enum").and_then(Value::as_array) {
        return vals.first().cloned().unwrap_or(Value::Null);
    }
    if let Some(variants) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        return variants
            .first()
            .map(|v| example_value(v, defs, seen))
            .unwrap_or(Value::Null);
    }
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        let mut merged = serde_json::Map::new();
        for member in all {
            if let Value::Object(obj) = example_value(member, defs, seen) {
                merged.extend(obj);
            }
        }
        return Value::Object(merged);
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        let mut obj = serde_json::Map::new();
        for (k, v) in props {
            obj.insert(k.clone(), example_value(v, defs, seen));
        }
        return Value::Object(obj);
    }
    // `"type"` is usually a bare string, but a nullable field (`["string",
    // "null"]`) makes it an array — the first non-null entry is still a
    // real example, where defaulting straight to an empty object (as a bare
    // `None` does below) would understate a field that does have a type.
    let ty = match schema.get("type") {
        Some(Value::String(t)) => Some(t.as_str()),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .or_else(|| types.first().and_then(Value::as_str)),
        _ => None,
    };
    match ty {
        Some("string") => Value::String("string".into()),
        Some("integer") => serde_json::json!(0),
        Some("number") => serde_json::json!(0.0),
        Some("boolean") => Value::Bool(true),
        Some("array") => {
            let item = schema
                .get("items")
                .map(|i| example_value(i, defs, seen))
                .unwrap_or(Value::Null);
            Value::Array(vec![item])
        }
        Some("null") => Value::Null,
        Some("object") | None => Value::Object(serde_json::Map::new()),
        _ => Value::Null,
    }
}

/// A schema shown two ways, one click apart: a synthesized instance
/// ("Example Value") and its property table ("Schema") — the same duality
/// Swagger UI's own request/response bodies show, via the site's existing
/// CSS-only `.tabs` component.
fn schema_with_example_html(schema: &Value, defs: &serde_json::Map<String, Value>) -> String {
    let example = example_value(schema, defs, &mut BTreeSet::new());
    let example_json = serde_json::to_string_pretty(&example).unwrap_or_default();
    let id = next_tab_id();
    format!(
        "<div class=\"tabs\"><input type=\"radio\" name=\"tabs-{id}\" id=\"tab-{id}-0\" checked>\
<label for=\"tab-{id}-0\">Example Value</label>\
<input type=\"radio\" name=\"tabs-{id}\" id=\"tab-{id}-1\"><label for=\"tab-{id}-1\">Schema</label>\
<section class=\"tab-panel\"><pre class=\"spec-example\">{}</pre></section>\
<section class=\"tab-panel\">{}</section></div>",
        escape(&example_json),
        schema_html(schema),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_defs() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    #[test]
    fn page_stem_drops_the_extension_and_directory() {
        assert_eq!(page_stem("openapi.yaml"), "openapi");
        assert_eq!(page_stem("server/mcp-tools.json"), "mcp-tools");
        assert_eq!(page_stem("no-extension"), "no-extension");
    }

    #[test]
    fn type_str_resolves_a_ref_to_its_bare_name() {
        let schema = json!({ "$ref": "#/components/schemas/Item" });
        assert_eq!(type_str(&schema), "Item");
        let mcp_ref = json!({ "$ref": "#/$defs/AnalysisType" });
        assert_eq!(type_str(&mcp_ref), "AnalysisType");
    }

    #[test]
    fn type_str_renders_arrays_and_unions() {
        assert_eq!(
            type_str(&json!({ "type": "array", "items": { "type": "string" } })),
            "string[]"
        );
        assert_eq!(
            type_str(&json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] })),
            "string | integer"
        );
    }

    #[test]
    fn type_str_falls_back_to_object_when_properties_imply_it() {
        let schema = json!({ "properties": { "a": { "type": "string" } } });
        assert_eq!(type_str(&schema), "object");
    }

    #[test]
    fn schema_html_tabulates_properties_with_required_marked() {
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" }, "qty": { "type": "integer" } },
            "required": ["name"],
        });
        let html = schema_html(&schema);
        assert!(html.contains("spec-required"), "{html}");
        assert!(html.contains("<code>name</code>"), "{html}");
        assert!(html.contains("<code>qty</code>"), "{html}");
    }

    #[test]
    fn schema_html_falls_back_to_a_type_line_for_a_non_object_schema() {
        let html = schema_html(&json!({ "type": "array", "items": { "type": "string" } }));
        assert!(html.contains("string[]"), "{html}");
        assert!(!html.contains("<table>"), "{html}");
    }

    #[test]
    fn schema_html_lists_a_bare_enum() {
        let html = schema_html(&json!({ "type": "string", "enum": ["a", "b"] }));
        assert!(html.contains("one of"), "{html}");
        assert!(html.contains("&quot;a&quot;"), "{html}");
    }

    #[test]
    fn method_badge_colors_by_mutation_risk_not_an_arbitrary_hue() {
        assert!(method_badge("GET").contains("spec-method-get"));
        assert!(method_badge("post").contains("spec-method-post"));
        assert!(method_badge("DELETE").contains("spec-method-delete"));
    }

    #[test]
    fn status_class_buckets_default_with_the_error_responses() {
        assert_eq!(status_class("200"), "spec-status-2xx");
        assert_eq!(status_class("404"), "spec-status-4xx");
        assert_eq!(status_class("default"), "spec-status-4xx");
    }

    #[test]
    fn group_key_skips_a_version_prefix_and_names_the_resource() {
        assert_eq!(group_key("/v2/quote/{symbol}"), "quote");
        assert_eq!(group_key("/v1/analysis/{symbol}/{type}"), "analysis");
        assert_eq!(group_key("/health"), "health");
        assert_eq!(group_key("/"), "root");
    }

    #[test]
    fn group_title_capitalizes_and_despaces() {
        assert_eq!(group_title("capital-gains"), "Capital gains");
        assert_eq!(group_title("quote"), "Quote");
    }

    #[test]
    fn schema_hint_summarizes_shape_without_expanding_it() {
        assert_eq!(
            schema_hint(&json!({ "type": "object", "properties": { "a": {}, "b": {} } })),
            "object · 2 fields"
        );
        assert_eq!(
            schema_hint(&json!({ "oneOf": [{}, {}, {}] })),
            "oneOf · 3 variants"
        );
        assert_eq!(schema_hint(&json!({ "enum": ["a"] })), "enum");
        assert_eq!(schema_hint(&json!({ "type": "string" })), "string");
    }

    #[test]
    fn example_value_prefers_a_literal_example_over_synthesis() {
        let schema = json!({ "type": "string", "example": "AAPL" });
        assert_eq!(
            example_value(&schema, &no_defs(), &mut BTreeSet::new()),
            json!("AAPL")
        );
    }

    #[test]
    fn example_value_falls_back_to_default_then_first_enum_value() {
        assert_eq!(
            example_value(
                &json!({ "type": "string", "default": "US" }),
                &no_defs(),
                &mut BTreeSet::new()
            ),
            json!("US")
        );
        assert_eq!(
            example_value(
                &json!({ "type": "string", "enum": ["AR", "US"] }),
                &no_defs(),
                &mut BTreeSet::new()
            ),
            json!("AR")
        );
    }

    #[test]
    fn example_value_builds_an_object_from_properties_recursively() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "count": { "type": "integer" },
                "active": { "type": "boolean" },
            },
        });
        let example = example_value(&schema, &no_defs(), &mut BTreeSet::new());
        assert_eq!(example["name"], json!("string"));
        assert_eq!(example["count"], json!(0));
        assert_eq!(example["active"], json!(true));
    }

    #[test]
    fn example_value_picks_the_non_null_arm_of_a_nullable_type_array() {
        // Regression: `"type": ["string", "null"]` (a nullable field) used
        // to fall through to the bare-object default instead of the type
        // that's actually still there.
        let schema = json!({ "type": ["string", "null"] });
        assert_eq!(
            example_value(&schema, &no_defs(), &mut BTreeSet::new()),
            json!("string")
        );
    }

    #[test]
    fn example_value_resolves_a_ref_against_the_supplied_defs() {
        let mut defs = serde_json::Map::new();
        defs.insert(
            "Item".into(),
            json!({ "type": "object", "properties": { "id": { "type": "string" } } }),
        );
        let schema = json!({ "$ref": "#/components/schemas/Item" });
        let example = example_value(&schema, &defs, &mut BTreeSet::new());
        assert_eq!(example["id"], json!("string"));
    }

    #[test]
    fn example_value_terminates_on_a_self_referential_schema() {
        let mut defs = serde_json::Map::new();
        defs.insert(
            "Node".into(),
            json!({ "type": "object", "properties": { "child": { "$ref": "#/$defs/Node" } } }),
        );
        let schema = json!({ "$ref": "#/$defs/Node" });
        // Must return, not overflow the stack.
        let _ = example_value(&schema, &defs, &mut BTreeSet::new());
    }

    #[test]
    fn example_value_resolves_a_deep_but_finite_chain_of_distinct_refs() {
        // Regression: an earlier depth-capped version cut this off with
        // `null` a few levels early, even though it isn't a cycle — no name
        // repeats on the path, it's just several distinct types deep (the
        // shape a paginated batch response — batch -> page -> edge -> node
        // — actually produces).
        let mut defs = serde_json::Map::new();
        defs.insert(
            "A".into(),
            json!({ "type": "object", "properties": { "b": { "$ref": "#/$defs/B" } } }),
        );
        defs.insert(
            "B".into(),
            json!({ "type": "object", "properties": { "c": { "$ref": "#/$defs/C" } } }),
        );
        defs.insert(
            "C".into(),
            json!({ "type": "object", "properties": { "d": { "$ref": "#/$defs/D" } } }),
        );
        defs.insert(
            "D".into(),
            json!({ "type": "object", "properties": { "leaf": { "type": "string" } } }),
        );
        let example = example_value(&json!({ "$ref": "#/$defs/A" }), &defs, &mut BTreeSet::new());
        assert_eq!(
            example["b"]["c"]["d"]["leaf"],
            json!("string"),
            "got {example}"
        );
    }

    #[test]
    fn schema_with_example_html_renders_both_tabs() {
        let html = schema_with_example_html(&json!({ "type": "string" }), &no_defs());
        assert!(html.contains("Example Value"), "{html}");
        assert!(html.contains("Schema"), "{html}");
        assert!(html.contains("class=\"tabs\""), "{html}");
    }

    #[test]
    fn parameters_and_try_out_shows_available_values_and_default_unconditionally() {
        let params = vec![json!({
            "name": "region", "in": "query", "required": false,
            "schema": { "type": "string", "enum": ["US", "CA"], "default": "US" },
        })];
        let html = parameters_and_try_html(
            Some(&params),
            None,
            &["https://a.example".to_string()],
            &no_defs(),
        );
        assert!(html.contains("Available values"), "{html}");
        assert!(html.contains(">US<") || html.contains("US, CA"), "{html}");
        assert!(html.contains("Default value"), "{html}");
        // The value column is a real dropdown, not a text box, for an enum param.
        assert!(html.contains("<select data-name=\"region\">"), "{html}");
        assert!(html.contains("<option value=\"US\" selected>"), "{html}");
    }

    #[test]
    fn parameters_and_try_out_uses_a_text_input_for_a_non_enum_param() {
        let params = vec![json!({
            "name": "symbol", "in": "path", "required": true,
            "schema": { "type": "string" },
        })];
        let html = parameters_and_try_html(
            Some(&params),
            None,
            &["https://a.example".to_string()],
            &no_defs(),
        );
        assert!(
            html.contains("<input type=\"text\" data-name=\"symbol\">"),
            "{html}"
        );
    }

    #[test]
    fn parameters_and_try_out_resolves_enum_default_and_description_through_a_ref() {
        // Regression: a parameter's own schema is often just
        // `{ description, $ref }` (description overriding/adding to the
        // referenced schema, per 2020-12 ref-as-applicator semantics) —
        // the enum and default live on the *referenced* schema, and the
        // description can live on either.
        let mut defs = serde_json::Map::new();
        defs.insert(
            "Region".into(),
            json!({
                "description": "Supported regions",
                "type": "string",
                "enum": ["US", "JP", "GB"],
            }),
        );
        let params = vec![json!({
            "name": "region", "in": "query", "required": false,
            "schema": {
                "description": "Region code, defaults to US if not specified.",
                "$ref": "#/components/schemas/Region",
            },
        })];
        let html = parameters_and_try_html(
            Some(&params),
            None,
            &["https://a.example".to_string()],
            &defs,
        );
        assert!(html.contains("Region code, defaults to US"), "{html}");
        assert!(html.contains("Available values"), "{html}");
        assert!(html.contains("US, JP, GB"), "{html}");
        assert!(html.contains("<select data-name=\"region\">"), "{html}");
        assert!(html.contains("<option value=\"US\">US</option>"), "{html}");
    }

    #[test]
    fn parameters_and_try_out_is_omitted_entirely_with_no_params_and_no_server() {
        assert_eq!(parameters_and_try_html(None, None, &[], &no_defs()), "");
    }

    #[test]
    fn parameters_and_try_out_still_shows_the_toggle_with_zero_params() {
        let html =
            parameters_and_try_html(None, None, &["https://a.example".to_string()], &no_defs());
        assert!(html.contains("spec-try-toggle"), "{html}");
        assert!(html.contains("spec-try-actions"), "{html}");
    }

    #[test]
    fn request_body_prefills_from_the_schema_default_and_examples() {
        let html = parameters_and_try_html(
            None,
            Some(&json!({ "type": "object", "properties": { "a": { "type": "string" } } })),
            &["https://a.example".to_string()],
            &no_defs(),
        );
        assert!(html.contains("spec-try-bodytext"), "{html}");
        assert!(html.contains("&quot;a&quot;"), "{html}");
    }

    #[test]
    fn render_mcp_shows_the_ref_name_instead_of_any() {
        let value = json!({
            "tools": [{
                "name": "get_analysis",
                "description": "Get analysis.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "kind": { "$ref": "#/$defs/AnalysisType" } },
                    "required": ["kind"],
                    "$defs": { "AnalysisType": { "type": "string", "enum": ["a", "b"] } },
                },
            }],
        });
        let md = render_mcp("mcp-tools.json", "acme", &value, &[], &[]);
        assert!(md.contains("get_analysis"), "{md}");
        assert!(md.contains("<code>AnalysisType</code>"), "{md}");
        assert!(
            !md.contains(">any<"),
            "the ref name must not fall back to any:\n{md}"
        );
    }

    #[test]
    fn render_openapi_groups_endpoints_by_resource_and_orders_verbs_readably() {
        let value = json!({
            "openapi": "3.1.0",
            "paths": {
                "/v2/items": {
                    "post": { "operationId": "createItem", "responses": {} },
                    "get": { "operationId": "listItems", "responses": {} },
                },
                "/v2/health": {
                    "get": { "operationId": "getHealth", "responses": {} },
                },
            },
        });
        let info = Info {
            title: "Items API".into(),
            version: "1.0".into(),
            description: None,
            servers: Vec::new(),
        };
        let md = render_openapi("openapi.yaml", &info, &value, &[], &[]);
        assert!(md.contains("### Items"), "{md}");
        assert!(md.contains("### Health"), "{md}");
        let get_at = md.find("listItems").unwrap();
        let post_at = md.find("createItem").unwrap();
        assert!(get_at < post_at, "GET should render before POST:\n{md}");
    }

    #[test]
    fn gaps_and_notes_render_as_admonitions() {
        let value = json!({ "openapi": "3.1.0", "paths": {} });
        let info = Info::default();
        let md = render_openapi(
            "openapi.yaml",
            &info,
            &value,
            &["a gap".to_string()],
            &["a note".to_string()],
        );
        assert!(md.contains("!!! warning"), "{md}");
        assert!(md.contains("a gap"), "{md}");
        assert!(md.contains("!!! note"), "{md}");
        assert!(md.contains("a note"), "{md}");
    }
}
