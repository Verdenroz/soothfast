//! Minimal template engine for theme files. Supports `{{ path }}` (HTML-
//! escaped), `{{ path | raw }}`, `{% if path %}…{% else %}…{% endif %}`,
//! `{% for x in path %}…{% endfor %}` and `{% include "file" %}` resolved
//! through the theme, so users can override any partial without recompiling.

use serde_json::Value;

/// Resolves `{% include %}` names to template text (backed by the theme).
pub trait Includes {
    /// Template text for `name`, or None if the theme has no such file.
    fn template(&self, name: &str) -> Option<String>;
}

/// Render `source` against `ctx`. Values are looked up by dot path
/// (`page.title`); a bare `.` names the current `for` item.
pub fn render(source: &str, ctx: &Value, includes: &dyn Includes) -> Result<String, String> {
    let nodes = parse(source)?;
    let mut out = String::with_capacity(source.len());
    let scope = vec![ctx.clone()];
    render_nodes(&nodes, &scope, includes, &mut out, 0)?;
    Ok(out)
}

enum Node {
    Text(String),
    /// (path, raw): `{{ path }}` or `{{ path | raw }}`.
    Var(String, bool),
    If(String, Vec<Node>, Vec<Node>),
    /// (binding, path, body)
    For(String, String, Vec<Node>),
    Include(String),
}

fn parse(source: &str) -> Result<Vec<Node>, String> {
    let mut toks = lex(source)?;
    let (nodes, end) = parse_block(&mut toks, None)?;
    if let Some(t) = end {
        return Err(format!("unexpected {{% {t} %}} at top level"));
    }
    Ok(nodes)
}

enum Tok {
    Text(String),
    Expr(String),
    Stmt(String),
}

fn lex(source: &str) -> Result<std::collections::VecDeque<Tok>, String> {
    let mut toks = std::collections::VecDeque::new();
    let mut rest = source;
    while !rest.is_empty() {
        let expr = rest.find("{{");
        let stmt = rest.find("{%");
        let (at, is_expr) = match (expr, stmt) {
            (Some(e), Some(s)) if e < s => (e, true),
            (Some(e), None) => (e, true),
            (_, Some(s)) => (s, false),
            (None, None) => {
                toks.push_back(Tok::Text(rest.to_string()));
                break;
            }
        };
        if at > 0 {
            toks.push_back(Tok::Text(rest[..at].to_string()));
        }
        let (open, close) = if is_expr { ("{{", "}}") } else { ("{%", "%}") };
        let body_start = at + open.len();
        let end = rest[body_start..].find(close).ok_or_else(|| {
            let near: String = rest[at..].chars().take(20).collect();
            format!("unclosed {open} near {near:?}")
        })?;
        let body = rest[body_start..body_start + end].trim().to_string();
        toks.push_back(if is_expr {
            Tok::Expr(body)
        } else {
            Tok::Stmt(body)
        });
        rest = &rest[body_start + end + close.len()..];
    }
    Ok(toks)
}

/// Parse until a closing statement (`endif`/`else`/`endfor`); returns the
/// nodes and which closer stopped the block (None at end of input).
fn parse_block(
    toks: &mut std::collections::VecDeque<Tok>,
    inside: Option<&str>,
) -> Result<(Vec<Node>, Option<String>), String> {
    let mut nodes = Vec::new();
    while let Some(tok) = toks.pop_front() {
        match tok {
            Tok::Text(t) => nodes.push(Node::Text(t)),
            Tok::Expr(body) => {
                let (path, raw) = match body.split_once('|') {
                    Some((p, f)) if f.trim() == "raw" => (p.trim().to_string(), true),
                    Some((_, f)) => return Err(format!("unknown filter {:?}", f.trim())),
                    None => (body, false),
                };
                nodes.push(Node::Var(path, raw));
            }
            Tok::Stmt(body) => {
                let mut words = body.split_whitespace();
                match words.next() {
                    Some("if") => {
                        let path = words.next().ok_or("if needs a path")?.to_string();
                        let (then, closer) = parse_block(toks, Some("if"))?;
                        let (els, closer) = if closer.as_deref() == Some("else") {
                            let (e, c) = parse_block(toks, Some("if"))?;
                            (e, c)
                        } else {
                            (Vec::new(), closer)
                        };
                        if closer.as_deref() != Some("endif") {
                            return Err("unclosed {% if %}".into());
                        }
                        nodes.push(Node::If(path, then, els));
                    }
                    Some("for") => {
                        let var = words.next().ok_or("for needs a binding")?.to_string();
                        if words.next() != Some("in") {
                            return Err("for syntax is {% for x in path %}".into());
                        }
                        let path = words.next().ok_or("for needs a path")?.to_string();
                        let (body, closer) = parse_block(toks, Some("for"))?;
                        if closer.as_deref() != Some("endfor") {
                            return Err("unclosed {% for %}".into());
                        }
                        nodes.push(Node::For(var, path, body));
                    }
                    Some("include") => {
                        let name = body["include".len()..].trim();
                        let name = name
                            .strip_prefix('"')
                            .and_then(|n| n.strip_suffix('"'))
                            .ok_or_else(|| format!("include needs a quoted name, got {name:?}"))?;
                        nodes.push(Node::Include(name.to_string()));
                    }
                    Some(t @ ("endif" | "else" | "endfor")) => {
                        if inside.is_none() {
                            return Err(format!("unexpected {{% {t} %}}"));
                        }
                        return Ok((nodes, Some(t.to_string())));
                    }
                    other => return Err(format!("unknown statement {other:?}")),
                }
            }
        }
    }
    Ok((nodes, None))
}

const MAX_INCLUDE_DEPTH: usize = 16;

fn render_nodes(
    nodes: &[Node],
    scope: &[Value],
    includes: &dyn Includes,
    out: &mut String,
    depth: usize,
) -> Result<(), String> {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Var(path, raw) => {
                let v = lookup(scope, path);
                let s = to_text(&v);
                if *raw {
                    out.push_str(&s);
                } else {
                    out.push_str(&escape(&s));
                }
            }
            Node::If(path, then, els) => {
                let branch = if truthy(&lookup(scope, path)) {
                    then
                } else {
                    els
                };
                render_nodes(branch, scope, includes, out, depth)?;
            }
            Node::For(var, path, body) => {
                if let Value::Array(items) = lookup(scope, path) {
                    for item in items {
                        let mut layer = serde_json::Map::new();
                        layer.insert(var.clone(), item);
                        let mut inner = scope.to_vec();
                        inner.push(Value::Object(layer));
                        render_nodes(body, &inner, includes, out, depth)?;
                    }
                }
            }
            Node::Include(name) => {
                if depth >= MAX_INCLUDE_DEPTH {
                    return Err(format!("include depth exceeded at {name:?}"));
                }
                let src = includes
                    .template(name)
                    .ok_or_else(|| format!("include {name:?}: no such theme file"))?;
                let nodes = parse(&src).map_err(|e| format!("{name}: {e}"))?;
                render_nodes(&nodes, scope, includes, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

/// Innermost scope layer that has the path's first segment wins, so `for`
/// bindings shadow page context which shadows site context.
fn lookup(scope: &[Value], path: &str) -> Value {
    let mut segs = path.split('.');
    let Some(first) = segs.next() else {
        return Value::Null;
    };
    for layer in scope.iter().rev() {
        let mut v = &layer[first];
        if v.is_null() {
            continue;
        }
        for seg in segs.by_ref() {
            v = &v[seg];
        }
        return v.clone();
    }
    Value::Null
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    }
}

fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Escape text for HTML element and attribute contexts.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct NoIncludes;
    impl Includes for NoIncludes {
        fn template(&self, _: &str) -> Option<String> {
            None
        }
    }

    struct OneInclude;
    impl Includes for OneInclude {
        fn template(&self, name: &str) -> Option<String> {
            (name == "part.html").then(|| "[{{ page.title }}]".to_string())
        }
    }

    #[test]
    fn variables_escape_by_default() {
        let ctx = json!({ "page": { "title": "a<b" }, "body": "<p>x</p>" });
        let out = render("{{ page.title }}|{{ body | raw }}", &ctx, &NoIncludes).unwrap();
        assert_eq!(out, "a&lt;b|<p>x</p>");
    }

    #[test]
    fn if_else_and_truthiness() {
        let ctx = json!({ "repo": "", "name": "soothfast" });
        let t = "{% if repo %}R{% else %}-{% endif %}{% if name %}N{% endif %}";
        assert_eq!(render(t, &ctx, &NoIncludes).unwrap(), "-N");
    }

    #[test]
    fn for_binds_and_shadows() {
        let ctx = json!({ "x": "outer", "items": [{"x": "a"}, {"x": "b"}] });
        let t = "{% for it in items %}{{ it.x }}{% endfor %}{{ x }}";
        assert_eq!(render(t, &ctx, &NoIncludes).unwrap(), "abouter");
    }

    #[test]
    fn nested_for_over_nav() {
        let ctx = json!({ "nav": [
            { "title": "G", "pages": [{"title": "p1"}, {"title": "p2"}] }
        ]});
        let t = "{% for g in nav %}{{ g.title }}:{% for p in g.pages %}{{ p.title }},{% endfor %}{% endfor %}";
        assert_eq!(render(t, &ctx, &NoIncludes).unwrap(), "G:p1,p2,");
    }

    #[test]
    fn include_renders_with_context() {
        let ctx = json!({ "page": { "title": "T" } });
        let out = render("a{% include \"part.html\" %}b", &ctx, &OneInclude).unwrap();
        assert_eq!(out, "a[T]b");
        assert!(render("{% include \"nope.html\" %}", &ctx, &OneInclude).is_err());
    }

    #[test]
    fn errors_on_malformed_templates() {
        let ctx = json!({});
        assert!(render("{% if x %}unclosed", &ctx, &NoIncludes).is_err());
        assert!(render("{{ unclosed", &ctx, &NoIncludes).is_err());
        assert!(render("{% endfor %}", &ctx, &NoIncludes).is_err());
        assert!(render("{{ x | upper }}", &ctx, &NoIncludes).is_err());
    }
}
