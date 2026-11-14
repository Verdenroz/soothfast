//! Declarations for items whose span points at a macro call: rebuilt from
//! the rustdoc JSON item, since the call site no longer names them.

use serde_json::{Map, Value};

type Index = Map<String, Value>;

/// Whether `declaration` (the span-derived text up to the first `{` or `;`)
/// is a macro invocation that does not declare an item named `name` of `kind`.
pub(super) fn is_macro_call(declaration: &str, name: &str, kind: &str) -> bool {
    keyword(kind).is_some()
        && invokes_macro(declaration)
        && !declaration
            .split(|c: char| !is_ident_char(c))
            .any(|word| word == name)
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn invokes_macro(text: &str) -> bool {
    let name_len = text
        .find(|c: char| !(is_ident_char(c) || c == ':'))
        .unwrap_or(text.len());
    let Some(rest) = text[name_len..].strip_prefix('!') else {
        return false;
    };
    let rest = rest.trim_start();
    name_len > 0 && (rest.is_empty() || rest.starts_with(['(', '[', '{']))
}

fn keyword(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "function" => "fn",
        "struct" => "struct",
        "enum" => "enum",
        "trait" => "trait",
        "constant" => "const",
        "type_alias" => "type",
        "module" => "mod",
        "static" => "static",
        "union" => "union",
        _ => return None,
    })
}

/// Declaration rebuilt from the JSON item. With `public_only`, a struct or
/// union lists only its `pub` fields, matching the span-derived signature.
pub(super) fn declaration(index: &Index, item: &Value, kind: &str, public_only: bool) -> String {
    let (Some(keyword), Some(name)) = (keyword(kind), item["name"].as_str()) else {
        return String::new();
    };
    let inner = &item["inner"][kind];
    let generics = generics(&inner["generics"]);
    match kind {
        "function" => function(inner, name, &generics),
        "struct" | "union" => format!(
            "pub {keyword} {name}{generics}{}",
            fields(index, inner, kind, public_only)
        ),
        "enum" => {
            let variants: Vec<&str> = ids(&inner["variants"])
                .filter_map(|v| index.get(&v)?["name"].as_str())
                .collect();
            format!("pub enum {name}{generics} {{ {} }}", variants.join(", "))
        }
        _ => format!("pub {keyword} {name}{generics}"),
    }
}

/// FNV-1a over a generated item's full declaration and docs. JSON ids never
/// reach the text, so the value is stable across builds.
pub(super) fn fingerprint(declaration: &str, docs: &str) -> u64 {
    let text = super::normalize(&format!("{declaration} {docs}"));
    soothfast_registry::fnv1a(text.as_bytes())
}

fn ids(list: &Value) -> impl Iterator<Item = String> + '_ {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| Some(id.as_u64()?.to_string()))
}

fn generics(generics: &Value) -> String {
    let params: Vec<String> = generics["params"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["kind"]["type"]["is_synthetic"] != true)
        .filter_map(|p| {
            let name = p["name"].as_str()?;
            Some(match p["kind"]["const"].as_object() {
                Some(c) => format!("const {name}: {}", render_type(&c["type"])),
                None => name.to_string(),
            })
        })
        .collect();
    if params.is_empty() {
        String::new()
    } else {
        format!("<{}>", params.join(", "))
    }
}

fn function(inner: &Value, name: &str, generics: &str) -> String {
    let header = &inner["header"];
    let mut out = String::from("pub ");
    for (flag, word) in [
        ("is_const", "const "),
        ("is_async", "async "),
        ("is_unsafe", "unsafe "),
    ] {
        if header[flag] == true {
            out.push_str(word);
        }
    }
    let params: Vec<String> = inner["sig"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|input| {
            let param = input[0].as_str().unwrap_or("_");
            format!("{param}: {}", render_type(&input[1]))
        })
        .collect();
    out.push_str(&format!("fn {name}{generics}({})", params.join(", ")));
    if inner["sig"]["output"].is_object() {
        out.push_str(&format!(" -> {}", render_type(&inner["sig"]["output"])));
    }
    out
}

fn fields(index: &Index, inner: &Value, kind: &str, public_only: bool) -> String {
    let shape = if kind == "struct" {
        &inner["kind"]
    } else {
        &inner["fields"]
    };
    let tuple = shape["tuple"].is_array();
    let field_ids: Vec<String> = if tuple {
        ids(&shape["tuple"]).collect()
    } else if shape.is_array() {
        ids(shape).collect()
    } else if shape["plain"].is_object() {
        ids(&shape["plain"]["fields"]).collect()
    } else {
        return String::new();
    };
    let rendered: Vec<String> = field_ids
        .iter()
        .filter_map(|id| index.get(id))
        .filter(|f| !public_only || f["visibility"] == "public")
        .map(|f| {
            let public = if f["visibility"] == "public" {
                "pub "
            } else {
                ""
            };
            let ty = render_type(&f["inner"]["struct_field"]);
            match f["name"].as_str() {
                Some(name) if !tuple => format!("{public}{name}: {ty}"),
                _ => format!("{public}{ty}"),
            }
        })
        .collect();
    if tuple {
        format!("({})", rendered.join(", "))
    } else {
        format!(" {{ {} }}", rendered.join(", "))
    }
}

/// Renders the type nodes a signature is built from; anything else is `_`.
fn render_type(ty: &Value) -> String {
    if let Some(name) = ty["primitive"].as_str().or(ty["generic"].as_str()) {
        return name.to_string();
    }
    if let Some(path) = ty["resolved_path"]["path"].as_str() {
        let args: Vec<String> = ty["resolved_path"]["args"]["angle_bracketed"]["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|arg| match arg["lifetime"].as_str() {
                Some(lifetime) => lifetime.to_string(),
                None => render_type(&arg["type"]),
            })
            .collect();
        return if args.is_empty() {
            path.to_string()
        } else {
            format!("{path}<{}>", args.join(", "))
        };
    }
    if let Some(r) = ty.get("borrowed_ref") {
        let lifetime = r["lifetime"]
            .as_str()
            .map(|l| format!("{l} "))
            .unwrap_or_default();
        let mutable = if r["is_mutable"] == true { "mut " } else { "" };
        return format!("&{lifetime}{mutable}{}", render_type(&r["type"]));
    }
    if let Some(items) = ty["tuple"].as_array() {
        let parts: Vec<String> = items.iter().map(render_type).collect();
        return format!("({})", parts.join(", "));
    }
    if ty.get("slice").is_some() {
        return format!("[{}]", render_type(&ty["slice"]));
    }
    if ty.get("array").is_some() {
        let len = ty["array"]["len"].as_str().unwrap_or("_");
        return format!("[{}; {len}]", render_type(&ty["array"]["type"]));
    }
    "_".to_string()
}
