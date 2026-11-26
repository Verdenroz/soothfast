//! Router drift for generated specs: the routes an axum-style router
//! registers, read from source, against the operations the spec declares.
//!
//! Advisory by design. A generated spec cannot disagree with its `#[route]`
//! markers, but the markers are written by hand and can disagree with the
//! router that actually serves requests.

use crate::DeclaredOp;

const VERBS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

/// One `.route("path", verb(handler))` registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterRoute {
    /// Upper-case HTTP verb.
    pub method: String,
    /// The path literal as written, relative to any `.nest` prefix.
    pub path: String,
}

/// What the router and the spec disagree on.
#[derive(Debug, Default)]
pub struct RouterDrift {
    /// Registered by the router; no operation in the spec serves it.
    pub unspecified: Vec<RouterRoute>,
    /// Declared by the spec; no registration in the router serves it.
    pub unserved: Vec<DeclaredOp>,
}

/// Every `.route("<literal>", verb(..)[.verb(..)])` registration in `source`,
/// one entry per verb. Code after the first `#[cfg(test)]` is skipped.
pub fn scan_routes(source: &str) -> Vec<RouterRoute> {
    let live = source.split("#[cfg(test)]").next().unwrap_or(source);
    let bytes = live.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = live[at..].find(".route(") {
        let open = at + found + ".route(".len();
        at = open;
        let Some(close) = matching_paren(bytes, open - 1) else {
            continue;
        };
        let args = &live[open..close];
        let Some((path, rest)) = leading_literal(args) else {
            continue;
        };
        for verb in verbs(rest) {
            out.push(RouterRoute {
                method: verb.to_uppercase(),
                path: path.to_string(),
            });
        }
    }
    out
}

/// Compare registrations with declared operations. Paths match on shape
/// (`{id}`, `:id` and `{symbol}` are the same segment) and by suffix, since
/// a `.nest("/v2", ..)` prefix is applied away from the registration.
pub fn compare(routes: &[RouterRoute], declared: &[DeclaredOp]) -> RouterDrift {
    let served = |d: &DeclaredOp| {
        routes
            .iter()
            .any(|r| r.method.eq_ignore_ascii_case(&d.method) && suffix_of(&d.path, &r.path))
    };
    RouterDrift {
        unspecified: routes
            .iter()
            .filter(|r| {
                !declared.iter().any(|d| {
                    r.method.eq_ignore_ascii_case(&d.method) && suffix_of(&d.path, &r.path)
                })
            })
            .cloned()
            .collect(),
        unserved: declared.iter().filter(|d| !served(d)).cloned().collect(),
    }
}

fn suffix_of(spec_path: &str, route_path: &str) -> bool {
    let spec = segments(spec_path);
    let route = segments(route_path);
    spec.len() >= route.len() && spec[spec.len() - route.len()..] == route[..]
}

fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with(':') || (s.starts_with('{') && s.ends_with('}')) {
                "{}".to_string()
            } else {
                s.to_string()
            }
        })
        .collect()
}

/// The index of the `)` closing the `(` at `open`, skipping string literals.
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = string_end(bytes, i)?,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index of the `"` closing the literal opened at `open`.
fn string_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'"' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The leading `"literal"` of a call's arguments and what follows its comma.
fn leading_literal(args: &str) -> Option<(&str, &str)> {
    let args = args.trim_start().strip_prefix('"')?;
    let end = args.find('"')?;
    let rest = args[end + 1..].trim_start().strip_prefix(',')?;
    Some((&args[..end], rest))
}

/// The verbs called at the top level of a method-router expression, such as
/// `get(a).post(b)` or `routing::get(a)`.
fn verbs(expr: &str) -> Vec<&'static str> {
    let bytes = expr.as_bytes();
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => match string_end(bytes, i) {
                Some(end) => i = end,
                None => break,
            },
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            c if depth == 0 && c.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &expr[start..i];
                let boundary =
                    start == 0 || !matches!(bytes[start - 1], b'a'..=b'z' | b'A'..=b'Z' | b'_');
                if boundary
                    && bytes.get(i) == Some(&b'(')
                    && let Some(verb) = VERBS.iter().find(|v| **v == word)
                {
                    found.push(*verb);
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(method: &str, path: &str) -> DeclaredOp {
        DeclaredOp {
            operation: format!("{method}{path}"),
            method: method.into(),
            path: path.into(),
        }
    }

    fn route(method: &str, path: &str) -> RouterRoute {
        RouterRoute {
            method: method.into(),
            path: path.into(),
        }
    }

    #[test]
    fn reads_one_route_per_verb() {
        let src =
            r#"Router::new().route("/graphql", post(run).get(playground)).route("/h", get(h))"#;
        assert_eq!(
            scan_routes(src),
            [
                route("POST", "/graphql"),
                route("GET", "/graphql"),
                route("GET", "/h")
            ]
        );
    }

    #[test]
    fn reads_multiline_registrations_and_qualified_verbs() {
        let src = "r.route(\n    \"/a/{id}\",\n    routing::get(handlers::a).layer(L::new()),\n)";
        assert_eq!(scan_routes(src), [route("GET", "/a/{id}")]);
    }

    #[test]
    fn ignores_handler_names_that_contain_a_verb() {
        let src = r#"r.route("/x", get(forget_all))"#;
        assert_eq!(scan_routes(src), [route("GET", "/x")]);
    }

    #[test]
    fn skips_test_modules_and_non_literal_paths() {
        let src = "r.route(PATH, get(a)).route(\"/live\", get(b))\n#[cfg(test)]\nmod t { fn f() { r.route(\"/t\", get(c)); } }";
        assert_eq!(scan_routes(src), [route("GET", "/live")]);
    }

    #[test]
    fn finds_nothing_in_another_framework() {
        assert!(scan_routes("#[get(\"/x\")] async fn x() {}").is_empty());
    }

    #[test]
    fn matches_across_nest_prefix_and_param_spelling() {
        let routes = [route("GET", "/analysis/{type}"), route("GET", "/health")];
        let declared = [
            op("GET", "/v2/analysis/{analysis_type}"),
            op("GET", "/v2/health"),
        ];
        let drift = compare(&routes, &declared);
        assert!(drift.unspecified.is_empty() && drift.unserved.is_empty());
    }

    #[test]
    fn reports_both_directions() {
        let routes = [route("GET", "/a"), route("POST", "/b")];
        let declared = [op("GET", "/v2/a"), op("GET", "/v2/c")];
        let drift = compare(&routes, &declared);
        assert_eq!(drift.unspecified, [route("POST", "/b")]);
        assert_eq!(drift.unserved, [op("GET", "/v2/c")]);
    }

    #[test]
    fn a_shorter_spec_path_is_not_served_by_a_longer_route() {
        let drift = compare(&[route("GET", "/v2/a")], &[op("GET", "/a")]);
        assert_eq!(drift.unserved.len(), 1);
        assert_eq!(drift.unspecified.len(), 1);
    }
}
