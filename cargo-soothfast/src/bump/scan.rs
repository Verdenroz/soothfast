//! The soothfast pins in a repository's files: where each sits, what it
//! says, and what it becomes under a target release.

use std::collections::BTreeSet;
use std::ops::{Range, RangeInclusive};

use crate::invoke;

/// What replaces a pin's text.
pub enum Fix {
    Text(String),
    /// The commit the release tag names in the repository `slug`, followed
    /// by the version comment when `comment`.
    Commit {
        slug: String,
        comment: bool,
    },
}

/// One version a file pins: the bytes to replace, what they say now, and
/// whether that already is the target.
pub struct Pin {
    pub line: usize,
    pub span: Range<usize>,
    pub found: String,
    pub current: bool,
    pub fix: Fix,
    pub package: Option<String>,
}

/// A line that names `cargo-soothfast` with a version this scanner cannot
/// read.
pub struct Unrecognised {
    pub line: usize,
    pub text: String,
}

#[derive(Default)]
pub struct Scan {
    pub pins: Vec<Pin>,
    pub unrecognised: Vec<Unrecognised>,
}

fn offset(whole: &str, part: &str) -> usize {
    part.as_ptr() as usize - whole.as_ptr() as usize
}

fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split('\n').enumerate().map(|(i, l)| (i + 1, l))
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn versioned(s: &str, parts: RangeInclusive<usize>) -> bool {
    let (core, suffix) = match s.find(['-', '+']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, "0"),
    };
    let nums: Vec<&str> = core.split('.').collect();
    parts.contains(&nums.len())
        && nums
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && !suffix.is_empty()
        && suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
}

/// Whether `s` is a full `X.Y.Z` release version, pre-release suffix allowed.
pub fn is_version(s: &str) -> bool {
    versioned(s, 3..=3)
}

fn is_soothfast(name: &str) -> bool {
    name == "soothfast" || name.starts_with("soothfast-")
}

enum Table {
    Other,
    Deps,
    Dep(String),
}

impl Table {
    fn of(header: &str) -> Table {
        let inner = header.trim_matches(['[', ']', ' ']);
        let segments: Vec<&str> = inner.split('.').collect();
        match segments
            .iter()
            .rposition(|s| s.trim().ends_with("dependencies"))
        {
            None => Table::Other,
            Some(i) => match segments.get(i + 1) {
                None => Table::Deps,
                Some(name) => Table::Dep(name.trim().trim_matches(['"', '\'']).to_string()),
            },
        }
    }
}

fn quoted(s: &str) -> Option<&str> {
    Some(s.trim_start().strip_prefix('"')?.split_once('"')?.0)
}

fn inline_version(table: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(i) = table[from..].find("version") {
        let at = from + i;
        let before = table[..at].chars().next_back();
        let after = table[at + "version".len()..].trim_start();
        let word = before.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
        if !word && let Some(value) = after.strip_prefix('=') {
            return quoted(value);
        }
        from = at + "version".len();
    }
    None
}

/// The `soothfast*` dependency requirements in a Cargo.toml.
pub fn cargo(text: &str, target: &str) -> Scan {
    let mut scan = Scan::default();
    let mut table = Table::Other;
    for (line, row) in lines(text) {
        let row = row.trim();
        if row.starts_with('[') {
            table = Table::of(row);
            continue;
        }
        let Some((key, value)) = row.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']);
        let (name, literal) = match &table {
            Table::Deps if is_soothfast(key) => {
                let value = value.trim_start();
                let literal = if value.starts_with('{') {
                    inline_version(value)
                } else {
                    quoted(value)
                };
                (key, literal)
            }
            Table::Dep(name) if key == "version" && is_soothfast(name) => {
                (name.as_str(), quoted(value))
            }
            _ => continue,
        };
        let Some(literal) = literal else { continue };
        let prefix = literal.len() - literal.trim_start_matches(['^', '~', '=', ' ']).len();
        let version = &literal[prefix..];
        if !versioned(version, 1..=3) {
            continue;
        }
        let start = offset(text, literal) + prefix;
        scan.pins.push(Pin {
            line,
            span: start..start + version.len(),
            found: version.to_string(),
            current: version == target,
            fix: Fix::Text(target.to_string()),
            package: Some(name.to_string()),
        });
    }
    scan
}

/// The registry entries of `packages` in a Cargo.lock.
pub fn lock(text: &str, packages: &BTreeSet<String>, target: &str) -> Vec<Pin> {
    invoke::lock_packages(text)
        .into_iter()
        .filter(|p| p.registry && packages.contains(&p.name))
        .filter_map(|p| {
            let head = format!("name = \"{}\"\nversion = \"", p.name);
            let at = text.find(&format!("{head}{}\"", p.version))? + head.len();
            Some(Pin {
                line: text[..at].matches('\n').count() + 1,
                span: at..at + p.version.len(),
                current: p.version == target,
                found: p.version,
                fix: Fix::Text(target.to_string()),
                package: Some(p.name),
            })
        })
        .collect()
}

struct Action<'a> {
    pin: Pin,
    slug: String,
    commit: Option<&'a str>,
}

fn action<'a>(text: &'a str, line: usize, uses: &'a str, target: &str) -> Option<Action<'a>> {
    let (path, rest) = uses.split_once('@')?;
    let mut parts = path.split('/');
    let owner = parts
        .next()
        .filter(|o| !o.is_empty() && !o.starts_with('.'))?;
    if parts.next()? != "soothfast" {
        return None;
    }
    let slug = format!("{owner}/soothfast");
    let reference = rest.split_whitespace().next()?;
    let comment = rest[reference.len()..].trim();
    let start = offset(text, reference);
    if is_sha(reference) {
        let seen = comment
            .strip_prefix('#')
            .and_then(|c| c.trim().strip_prefix('v'))
            .filter(|v| is_version(v));
        let pin = Pin {
            line,
            span: start..offset(text, uses) + uses.len(),
            found: seen.unwrap_or(&reference[..7]).to_string(),
            current: seen == Some(target),
            fix: Fix::Commit {
                slug: slug.clone(),
                comment: true,
            },
            package: None,
        };
        return Some(Action {
            commit: pin.current.then_some(reference),
            pin,
            slug,
        });
    }
    let version = reference.strip_prefix('v').filter(|v| is_version(v))?;
    let pin = Pin {
        line,
        span: start..start + reference.len(),
        found: version.to_string(),
        current: version == target,
        fix: Fix::Text(format!("v{target}")),
        package: None,
    };
    Some(Action {
        pin,
        slug,
        commit: None,
    })
}

fn input_pin(
    text: &str,
    line: usize,
    value: &str,
    slug: &str,
    released: &[&str],
    target: &str,
) -> Option<Pin> {
    let start = offset(text, value);
    let span = start..start + value.len();
    if is_sha(value) {
        return Some(Pin {
            line,
            span,
            found: value[..7].to_string(),
            current: released.contains(&value),
            fix: Fix::Commit {
                slug: slug.to_string(),
                comment: false,
            },
            package: None,
        });
    }
    let version = value.strip_prefix('v').filter(|v| is_version(v))?;
    Some(Pin {
        line,
        span,
        found: version.to_string(),
        current: version == target,
        fix: Fix::Text(format!("v{target}")),
        package: None,
    })
}

fn version_token(s: &str) -> &str {
    let s = s.trim_start_matches(['=', ' ', '"', '\'']);
    s.split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .next()
        .unwrap_or("")
}

fn install(text: &str, line: usize, row: &str, target: &str, scan: &mut Scan) {
    for (flag, is_flag) in [("cargo-soothfast@", false), ("--version", true)] {
        let Some(at) = row.find(flag) else { continue };
        let after = &row[at + flag.len()..];
        if is_flag && !after.starts_with(['=', ' ']) {
            continue;
        }
        let token = version_token(after);
        if !is_version(token) {
            scan.unrecognised.push(Unrecognised {
                line,
                text: row.trim().to_string(),
            });
            continue;
        }
        let start = offset(text, token);
        scan.pins.push(Pin {
            line,
            span: start..start + token.len(),
            found: token.to_string(),
            current: token == target,
            fix: Fix::Text(target.to_string()),
            package: None,
        });
    }
}

/// The soothfast pins in a workflow file: `uses:` refs of any
/// `<owner>/soothfast`, the `soothfast_ref` input, and literal
/// `cargo-soothfast` install versions.
pub fn workflow(text: &str, target: &str) -> Scan {
    let mut scan = Scan::default();
    let mut slug: Option<String> = None;
    let mut released: Vec<&str> = Vec::new();
    let mut inputs: Vec<(usize, &str)> = Vec::new();
    for (line, row) in lines(text) {
        let body = row.trim_start();
        let body = body.strip_prefix("- ").unwrap_or(body).trim_start();
        if let Some(uses) = body.strip_prefix("uses:") {
            if let Some(a) = action(text, line, uses.trim(), target) {
                slug.get_or_insert(a.slug);
                released.extend(a.commit);
                scan.pins.push(a.pin);
            }
        } else if let Some(value) = body.strip_prefix("soothfast_ref:") {
            let token = value.split_whitespace().next().unwrap_or("");
            inputs.push((line, token.trim_matches(['"', '\''])));
        } else if body.contains("cargo-soothfast") && body.contains("install") {
            install(text, line, row, target, &mut scan);
        }
    }
    if let Some(slug) = slug {
        for (line, value) in inputs {
            scan.pins
                .extend(input_pin(text, line, value, &slug, &released, target));
        }
    }
    scan
}
