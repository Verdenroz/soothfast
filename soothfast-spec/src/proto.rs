//! `.proto` message field reconciliation, parallel to [`crate::reconcile`]:
//! matches struct fields against a proto message by tag, for consumed (not
//! served) wire formats with no handler to hang `#[route]` off of. Both
//! sides are read straight from source, so neither can drift.

use std::collections::BTreeMap;

/// One field, from either a `.proto` message or a struct's `#[prost(..)]`
/// attributes. `ty` is the bare type token (`"string"`, `"sint64"`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct ProtoField {
    pub name: String,
    pub ty: String,
    pub tag: u32,
}

/// Parse the scalar fields of one `message NAME { ... }` block from `.proto`
/// text. Nested message/oneof/enum bodies are skipped by brace depth, not
/// descended into — this is a flat wire-format check, not a full parser.
pub fn parse_proto_message(text: &str, message: &str) -> Result<Vec<ProtoField>, String> {
    let stripped = strip_comments(text);
    let needle = format!("message {message}");
    let start = stripped
        .find(&needle)
        .ok_or_else(|| format!("no `message {message}` in proto text"))?;
    let brace_start = stripped[start..]
        .find('{')
        .map(|i| start + i)
        .ok_or_else(|| format!("message {message} has no body"))?;

    let end = matching_brace(&stripped, brace_start)
        .ok_or_else(|| format!("message {message} body never closes"))?;
    let body = &stripped[brace_start + 1..end];

    let mut fields = Vec::new();
    for stmt in body.split(';') {
        let stmt = stmt.trim();
        // Skip nested message/oneof/enum bodies rather than misparse them.
        if stmt.is_empty() || stmt.contains('{') || stmt.contains('}') {
            continue;
        }
        let Some((decl, rest)) = stmt.split_once('=') else {
            continue; // not a `type name = tag` field (e.g. a reserved range)
        };
        let mut tokens: Vec<&str> = decl.split_whitespace().collect();
        tokens.retain(|t| !matches!(*t, "repeated" | "optional" | "required"));
        let Some(name) = tokens.pop() else { continue };
        let ty = tokens.join(" ");
        if ty.is_empty() {
            continue;
        }
        let tag_str = rest.split('[').next().unwrap_or("").trim();
        let Ok(tag) = tag_str.parse::<u32>() else {
            continue;
        };
        fields.push(ProtoField {
            name: name.to_string(),
            ty,
            tag,
        });
    }
    if fields.is_empty() {
        return Err(format!("message {message} declared no scalar fields"));
    }
    Ok(fields)
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut prev = ' ';
            for c in chars.by_ref() {
                if prev == '*' && c == '/' {
                    break;
                }
                prev = c;
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Extract `#[prost(..)]` field declarations from one struct in Rust source
/// text, by scanning forward from each `#[prost(..)]` occurrence within the
/// struct's brace-matched body to the field name it decorates. Deliberately
/// not a full Rust parser (no `syn`) — same tradeoff as [`parse_proto_message`].
pub fn parse_struct_prost_fields(
    source: &str,
    struct_name: &str,
) -> Result<Vec<ProtoField>, String> {
    let stripped = strip_comments(source);
    let needle = format!("struct {struct_name}");
    let start = stripped
        .find(&needle)
        .ok_or_else(|| format!("no `struct {struct_name}` found"))?;
    let brace_start = stripped[start..]
        .find('{')
        .map(|i| start + i)
        .ok_or_else(|| format!("struct {struct_name} has no body"))?;
    let end = matching_brace(&stripped, brace_start)
        .ok_or_else(|| format!("struct {struct_name} body never closes"))?;
    let body = &stripped[brace_start + 1..end];

    let mut fields = Vec::new();
    let mut pos = 0;
    while let Some(rel) = body[pos..].find("#[prost(") {
        let args_start = pos + rel + "#[prost(".len();
        let args_end = matching_paren(body, args_start)
            .ok_or_else(|| format!("unterminated #[prost(..)] in struct {struct_name}"))?;
        let after_attr = body[args_end..]
            .find(']')
            .map(|i| args_end + i + 1)
            .ok_or_else(|| format!("unterminated #[prost(..)] in struct {struct_name}"))?;
        if let (Some(ty), Some(tag)) = parse_prost_args(&body[args_start..args_end])
            && let Some(name) = next_field_name(&body[after_attr..])
        {
            fields.push(ProtoField { name, ty, tag });
        }
        pos = after_attr;
    }
    Ok(fields)
}

/// Index of the `)` matching the `(` implied by `after_open` (the position
/// right after it), or `None` if unbalanced.
fn matching_paren(text: &str, after_open: usize) -> Option<usize> {
    let mut depth = 1i32;
    for (i, c) in text[after_open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after_open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Index of the `}` matching the `{` at `brace_start`.
fn matching_brace(text: &str, brace_start: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in text[brace_start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(brace_start + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse `#[prost(..)]`'s parenthesized args (e.g. `sint64, tag = "20"` or
/// `enumeration = "X", tag = "6"`) into (type, tag).
fn parse_prost_args(args: &str) -> (Option<String>, Option<u32>) {
    let mut ty = None;
    let mut tag = None;
    for token in args.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        match token.split_once('=') {
            Some((key, val)) if key.trim() == "tag" => {
                tag = val.trim().trim_matches('"').parse::<u32>().ok();
            }
            Some((key, _)) => ty = Some(key.trim().to_string()),
            None => ty = Some(token.to_string()),
        }
    }
    (ty, tag)
}

/// The identifier before the next `:`, skipping visibility keywords and any
/// further `#[..]` attributes — i.e. the field name a `#[prost(..)]`
/// attribute decorates.
fn next_field_name(rest: &str) -> Option<String> {
    let mut s = rest.trim_start();
    loop {
        if let Some(after) = s.strip_prefix("#[") {
            let close = after.find(']')?;
            s = after[close + 1..].trim_start();
        } else if let Some(after) = s.strip_prefix("pub(") {
            let close = after.find(')')?;
            s = after[close + 1..].trim_start();
        } else if s.starts_with("pub") && s[3..].starts_with(char::is_whitespace) {
            s = s[3..].trim_start();
        } else {
            break;
        }
    }
    let name = &s[..s.find(':')?];
    let name = name.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| name.to_string())
}

/// prost's `enumeration` is wire-identical to plain `int32`/`uint32`, never
/// zigzag — anything else must match textually.
fn types_compatible(proto_ty: &str, code_ty: &str) -> bool {
    proto_ty == code_ty || (code_ty == "enumeration" && matches!(proto_ty, "int32" | "uint32"))
}

/// Outcome of matching struct fields to a `.proto` message by tag number.
#[derive(Debug, Default)]
pub struct ProtoReconciliation {
    /// Tag declared in the `.proto` message; no struct field carries it.
    pub missing_code: Vec<ProtoField>,
    /// Struct field's tag; the `.proto` message doesn't declare it.
    pub missing_spec: Vec<ProtoField>,
    /// Same tag on both sides, but type and/or name disagree: (spec field,
    /// code field, what).
    pub mismatched: Vec<(ProtoField, ProtoField, String)>,
    /// Tags present on both sides with a compatible type.
    pub matched: usize,
}

impl ProtoReconciliation {
    /// True when every tag matched: nothing missing on either side and no
    /// type/name disagreements.
    pub fn is_clean(&self) -> bool {
        self.missing_code.is_empty() && self.missing_spec.is_empty() && self.mismatched.is_empty()
    }
}

/// Match struct fields to declared `.proto` fields by tag number, then
/// check that type (wire-compatibly) and name agree.
pub fn reconcile_proto(
    spec_fields: &[ProtoField],
    code_fields: &[ProtoField],
) -> ProtoReconciliation {
    let mut rec = ProtoReconciliation::default();
    let by_tag: BTreeMap<u32, &ProtoField> = code_fields.iter().map(|f| (f.tag, f)).collect();

    for spec in spec_fields {
        match by_tag.get(&spec.tag) {
            None => rec.missing_code.push(spec.clone()),
            Some(code) => {
                let mut problems = Vec::new();
                if !types_compatible(&spec.ty, &code.ty) {
                    problems.push(format!("type {} != spec {}", code.ty, spec.ty));
                }
                if spec.name != code.name {
                    problems.push(format!("name {} != spec {}", code.name, spec.name));
                }
                if problems.is_empty() {
                    rec.matched += 1;
                } else {
                    rec.mismatched
                        .push((spec.clone(), (*code).clone(), problems.join("; ")));
                }
            }
        }
    }

    let declared_tags: std::collections::BTreeSet<u32> =
        spec_fields.iter().map(|f| f.tag).collect();
    for code in code_fields {
        if !declared_tags.contains(&code.tag) {
            rec.missing_spec.push(code.clone());
        }
    }
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROTO: &str = r#"
        syntax = "proto3";
        package yahoo;

        message PricingData {
            string id = 1;
            float price = 2;
            sint64 time = 3;
            int32 quote_type = 6;
            sint64 options_type = 20;
        }
    "#;

    const SOURCE: &str = r#"
        #[derive(Clone, PartialEq, Message)]
        pub(crate) struct PricingData {
            #[prost(string, tag = "1")]
            pub id: String,

            #[prost(float, tag = "2")]
            pub price: f32,

            #[prost(sint64, tag = "3")]
            pub time: i64,

            #[prost(enumeration = "QuoteTypeProto", tag = "6")]
            pub quote_type: i32,

            #[prost(enumeration = "OptionTypeProto", tag = "20")]
            pub options_type: i32,
        }
    "#;

    #[test]
    fn parses_proto_message_fields_by_tag() {
        let fields = parse_proto_message(PROTO, "PricingData").unwrap();
        assert_eq!(fields.len(), 5);
        assert_eq!(
            fields[4],
            ProtoField {
                name: "options_type".into(),
                ty: "sint64".into(),
                tag: 20
            }
        );
    }

    #[test]
    fn parses_struct_prost_fields() {
        let fields = parse_struct_prost_fields(SOURCE, "PricingData").unwrap();
        assert_eq!(fields.len(), 5);
        assert_eq!(
            fields[3],
            ProtoField {
                name: "quote_type".into(),
                ty: "enumeration".into(),
                tag: 6
            }
        );
    }

    #[test]
    fn enumeration_is_compatible_with_plain_int32_but_not_sint64() {
        let spec = parse_proto_message(PROTO, "PricingData").unwrap();
        let code = parse_struct_prost_fields(SOURCE, "PricingData").unwrap();
        let rec = reconcile_proto(&spec, &code);
        // quote_type (int32 vs enumeration) matches; options_type (sint64
        // vs enumeration) does not — this is the real finance-query drift.
        assert_eq!(rec.matched, 4);
        assert_eq!(rec.mismatched.len(), 1);
        assert_eq!(rec.mismatched[0].0.tag, 20);
        assert!(rec.mismatched[0].2.contains("sint64"));
    }

    #[test]
    fn flags_missing_tags_on_either_side() {
        let spec = vec![
            ProtoField {
                name: "a".into(),
                ty: "string".into(),
                tag: 1,
            },
            ProtoField {
                name: "b".into(),
                ty: "int32".into(),
                tag: 2,
            },
        ];
        let code = vec![
            ProtoField {
                name: "a".into(),
                ty: "string".into(),
                tag: 1,
            },
            ProtoField {
                name: "c".into(),
                ty: "bool".into(),
                tag: 3,
            },
        ];
        let rec = reconcile_proto(&spec, &code);
        assert_eq!(rec.matched, 1);
        assert_eq!(rec.missing_code.len(), 1); // tag 2, spec only
        assert_eq!(rec.missing_spec.len(), 1); // tag 3, code only
    }

    #[test]
    fn is_clean_when_everything_lines_up() {
        let fields = vec![ProtoField {
            name: "a".into(),
            ty: "string".into(),
            tag: 1,
        }];
        assert!(reconcile_proto(&fields, &fields).is_clean());
    }
}
