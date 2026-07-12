//! `cargo soothfast mcp -p PKG` — agent-facing living docs.
//!
//! Hand-rolled MCP stdio transport (newline-delimited JSON-RPC 2.0; no MCP
//! framework crate per the dependency policy). Read-only tools over data the
//! other engines already produce: the current API surface, measurements,
//! and the trend series. An agent asking "what is the current signature and
//! measured cost of X?" gets the *produced* answer, never a stale README.

use std::collections::BTreeMap;
use std::io::BufRead;

use serde_json::{Value, json};
use soothfast_report::llms::SurfaceEntry;

use crate::{docs_support, invoke};

struct State {
    pkg: String,
    entries: Vec<SurfaceEntry>,
    baseline: Value,
    trend: Vec<Value>,
}

pub fn run(args: &[String]) -> i32 {
    let mut pkg: Option<String> = None;
    let mut baseline_name = String::from("base");
    let mut features: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => pkg = it.next().cloned(),
            "--baseline" => match it.next() {
                Some(n) => baseline_name = n.clone(),
                None => return err("--baseline needs a name"),
            },
            "--features" => features = it.next().cloned(),
            other => return err(&format!("unknown mcp arg {other:?}")),
        }
    }
    let Some(pkg) = pkg else {
        return err("mcp requires -p PKG");
    };

    // Build state once at startup; the server is a read-only view.
    let entries = match docs_support::surface_entries(&pkg, features.as_deref()) {
        Ok(e) => e,
        Err(e) => return err(&format!("cannot build surface: {e}")),
    };
    let baseline = invoke::load_baseline(&baseline_name)
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({ "items": {} }));
    let trend = invoke::workspace_root()
        .ok()
        .and_then(|r| std::fs::read_to_string(r.join(".soothfast/trend.jsonl")).ok())
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default();
    let state = State {
        pkg,
        entries,
        baseline,
        trend,
    };

    eprintln!(
        "soothfast mcp: serving {} public items on stdio",
        state.entries.len()
    );
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            // JSON-RPC: unparseable requests get a -32700 with null id;
            // silence would leave a well-behaved client hanging forever.
            println!(
                "{}",
                json!({ "jsonrpc": "2.0", "id": null,
                        "error": { "code": -32700, "message": "parse error" } })
            );
            continue;
        };
        if let Some(resp) = dispatch(&state, &req) {
            println!("{resp}");
        }
    }
    0
}

fn dispatch(state: &State, req: &Value) -> Option<Value> {
    let id = req.get("id")?.clone(); // notifications (no id) need no response
    let result = match req["method"].as_str() {
        Some("initialize") => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "soothfast", "version": env!("CARGO_PKG_VERSION") },
        }),
        Some("ping") => json!({}),
        Some("tools/list") => tools_list(),
        Some("tools/call") => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = &req["params"]["arguments"];
            match tool_call(state, name, args) {
                Ok(text) => {
                    json!({ "content": [ { "type": "text", "text": text } ], "isError": false })
                }
                // Unknown/missing tool name is a protocol error, not a tool
                // answer — agents must not read it as a successful result.
                Err(msg) => {
                    return Some(json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32602, "message": msg },
                    }));
                }
            }
        }
        _ => {
            return Some(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": "method not found" },
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tools_list() -> Value {
    let str_arg = |name: &str, desc: &str| {
        json!({ "type": "object",
                "properties": { name: { "type": "string", "description": desc } },
                "required": [name] })
    };
    json!({ "tools": [
        { "name": "surface_summary",
          "description": "Current public API of the crate: every item with signature and measured cost. Derived from the code — always current.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "get_item",
          "description": "Current signature, docs, and measurements of one public item.",
          "inputSchema": str_arg("path", "full item path, e.g. demo::sorted") },
        { "name": "search_surface",
          "description": "Find public items whose path or docs match a query.",
          "inputSchema": str_arg("query", "substring to search for") },
        { "name": "perf_trend",
          "description": "Recorded measurement history of one item across merges.",
          "inputSchema": str_arg("item", "measured item id") },
    ] })
}

fn measurements_for(state: &State, path: &str) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(items) = state.baseline["items"].as_object() {
        for (id, m) in items {
            let target = m["covers"].as_str().filter(|c| !c.is_empty()).unwrap_or(id);
            if target != path && id != path {
                continue;
            }
            for (label, keys) in [
                ("instructions/iter", ["perfcnt", "instructions"]),
                ("median ns", ["walltime", "median_ns"]),
                ("allocs/iter", ["alloc", "allocs"]),
                ("polls/iter", ["asyncexec", "polls"]),
            ] {
                if let Some(v) = m[keys[0]][keys[1]].as_f64() {
                    facts.push(format!("{v:.0} {label} (as `{id}`)"));
                }
            }
        }
    }
    facts
}

fn tool_call(state: &State, name: &str, args: &Value) -> Result<String, String> {
    Ok(match name {
        "surface_summary" => {
            soothfast_report::llms::render(&state.pkg, &state.entries, &state.baseline)
        }
        "get_item" => {
            let path = args["path"].as_str().unwrap_or("");
            match state.entries.iter().find(|e| e.path == path) {
                None => format!("no public item `{path}` in {}", state.pkg),
                Some(e) => {
                    let mut out = format!("{} ({})\n", e.path, e.kind);
                    if !e.signature.is_empty() {
                        out.push_str(&format!("signature: {}\n", e.signature));
                    }
                    if !e.docs.is_empty() {
                        out.push_str(&format!("docs: {}\n", e.docs));
                    }
                    let facts = measurements_for(state, path);
                    if facts.is_empty() {
                        out.push_str("measured: no (see `cargo soothfast coverage measure`)\n");
                    } else {
                        for f in facts {
                            out.push_str(&format!("measured: {f}\n"));
                        }
                    }
                    out
                }
            }
        }
        "search_surface" => {
            let query = args["query"].as_str().unwrap_or("").to_lowercase();
            let hits: Vec<&SurfaceEntry> = state
                .entries
                .iter()
                .filter(|e| {
                    e.path.to_lowercase().contains(&query) || e.docs.to_lowercase().contains(&query)
                })
                .collect();
            if hits.is_empty() {
                format!("no public items matching {query:?}")
            } else {
                hits.iter()
                    .map(|e| format!("{} ({}) — {}", e.path, e.kind, e.summary))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "perf_trend" => {
            let item = args["item"].as_str().unwrap_or("");
            let mut lines: Vec<String> = Vec::new();
            for p in &state.trend {
                let m = &p["items"][item];
                if m.is_null() {
                    continue;
                }
                let mut point: BTreeMap<&str, f64> = BTreeMap::new();
                for (label, keys) in [
                    ("instructions", ["perfcnt", "instructions"]),
                    ("median_ns", ["walltime", "median_ns"]),
                    ("allocs", ["alloc", "allocs"]),
                ] {
                    if let Some(v) = m[keys[0]][keys[1]].as_f64() {
                        point.insert(label, v);
                    }
                }
                lines.push(format!(
                    "{} {}: {}",
                    p["unix"].as_u64().unwrap_or(0),
                    p["commit"].as_str().unwrap_or("?"),
                    point
                        .iter()
                        .map(|(k, v)| format!("{k}={v:.0}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            if lines.is_empty() {
                format!("no trend data for `{item}` — run `cargo soothfast trend append`")
            } else {
                lines.join("\n")
            }
        }
        other => return Err(format!("unknown tool {other:?}")),
    })
}

fn err(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    2
}
