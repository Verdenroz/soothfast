//! `cargo soothfast spec probe` — data quality against the live surface.
//!
//! `spec check` proves every operation exists; nothing proves an endpoint
//! still *populates* what it answers. This command launches the package's
//! embedded server (or targets `--base-url`), fires the requests declared
//! in `probes.toml`, and holds each response to four checks from
//! `soothfast_spec::probe`: field population against the committed
//! `probes.lock`, spec-declared coverage of fields never populated,
//! structure against the spec's own response schema, and the manifest's
//! sanity assertions.
//!
//! `--accept` folds the observed population back into the lock — the same
//! accept discipline as living docs, with the same rule that a run with
//! failing shape checks or assertions is never accepted.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use soothfast_site::toml::{TomlValue, logical_lines, parse_value};
use soothfast_spec::probe::assert::Assertion;
use soothfast_spec::probe::baseline::Baseline;
use soothfast_spec::probe::{coverage, population, shape};
use soothfast_spec::serialize;

use crate::invoke::{self, CommonArgs};

/// The `[probes]` header: how to reach a server and which spec file the
/// responses are validated against.
#[derive(Debug, Default)]
struct Header {
    /// Spec file (relative to the package dir) for shape validation; no
    /// key, no shape checks.
    spec: Option<String>,
    /// Server binary to launch, relative to the package dir. Absent means
    /// `--base-url` is required.
    command: Option<String>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    ready_timeout_secs: u64,
}

/// One `[[probe]]` entry.
#[derive(Debug)]
struct Probe {
    name: String,
    path: String,
    status: u16,
    /// Fields pinned intermittent by the operator; `*` suffix matches a
    /// prefix.
    sometimes: Vec<String>,
    asserts: Vec<Assertion>,
    /// `shape = false` skips schema validation for this probe — the
    /// escape hatch for a spec known to misdescribe the endpoint.
    shape: bool,
}

struct Manifest {
    header: Header,
    probes: Vec<Probe>,
}

pub fn run(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut accept = false;
    let mut allow_gone = false;
    let mut base_url = None;
    let mut filter = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--accept" => accept = true,
            "--allow-gone" => allow_gone = true,
            "--base-url" => match it.next() {
                Some(u) => base_url = Some(u.clone()),
                None => {
                    eprintln!("soothfast: --base-url needs a URL");
                    return 2;
                }
            },
            "--filter" => match it.next() {
                Some(f) => filter = Some(f.clone()),
                None => {
                    eprintln!("soothfast: --filter needs a substring");
                    return 2;
                }
            },
            _ => {
                if !common.try_parse(a, &mut it) {
                    eprintln!("soothfast: unknown spec probe arg {a:?}");
                    return 2;
                }
            }
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: spec probe requires -p PKG");
        return 2;
    };
    match probe(
        &pkg,
        accept,
        allow_gone,
        base_url.as_deref(),
        filter.as_deref(),
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

fn probe(
    pkg: &str,
    accept: bool,
    allow_gone: bool,
    base_url: Option<&str>,
    filter: Option<&str>,
) -> Result<i32, String> {
    let pkg_dir = invoke::pkg_dir(pkg).map_err(|e| e.to_string())?;
    let manifest_path = pkg_dir.join("probes.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest = parse_manifest(&manifest_text)?;

    let spec = match &manifest.header.spec {
        Some(file) => {
            let path = pkg_dir.join(file);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read spec {}: {e}", path.display()))?;
            Some(serialize::from_text(&text).map_err(|e| format!("{file}: {e}"))?)
        }
        None => None,
    };

    let lock_path = pkg_dir.join("probes.lock");
    let mut baseline = match std::fs::read_to_string(&lock_path) {
        Ok(text) => Baseline::parse(&text)?,
        Err(_) => Baseline::default(),
    };

    let _server;
    let base = match base_url {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => {
            let launched = launch(&manifest.header, &pkg_dir)?;
            let url = launched.base_url.clone();
            _server = launched;
            url
        }
    };

    let declared: Vec<String> = manifest.probes.iter().map(|p| p.name.clone()).collect();
    let gone = baseline.retain_probes(&declared);
    let mut failures = 0u32;
    if !gone.is_empty() && !accept && !allow_gone {
        for name in &gone {
            failures += 1;
            println!(
                "FAIL  probe `{name}` is locked but no longer in probes.toml (--allow-gone to drop)"
            );
        }
    }

    let mut probed = 0usize;
    for entry in &manifest.probes {
        if let Some(f) = filter
            && !entry.name.contains(f)
        {
            continue;
        }
        probed += 1;
        let (status, body) = get(&base, &entry.path)?;
        if status != entry.status {
            failures += 1;
            println!(
                "FAIL  {}: {} answered {status}, expected {}",
                entry.name, entry.path, entry.status
            );
            continue;
        }
        let value: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                failures += 1;
                println!("FAIL  {}: {} body is not JSON: {e}", entry.name, entry.path);
                continue;
            }
        };

        let mut problems = Vec::new();
        let mut declared = None;
        if let Some(spec) = spec.as_ref().filter(|_| entry.shape) {
            match shape::response_schema(spec, "GET", &entry.path, entry.status) {
                Some(schema) => {
                    for v in shape::validate(&value, schema, spec) {
                        problems.push(format!("shape: {} — {}", v.path, v.message));
                    }
                    declared = Some(coverage::declared_paths(schema, spec));
                }
                None => problems.push("shape: no response schema in spec".to_string()),
            }
        }
        for assertion in &entry.asserts {
            if let Err(reason) = assertion.check(&value) {
                problems.push(format!("assert: {reason}"));
            }
        }

        let observed = population::populate(&value);
        if accept {
            if problems.is_empty() {
                baseline.accept(&entry.name, &observed, declared.as_ref());
                println!(
                    "probe: {} — accepted ({} fields{})",
                    entry.name,
                    observed.len(),
                    uncovered_note(&baseline, &entry.name),
                );
            } else {
                // A run that fails its own checks must not ratify a lock.
                failures += 1;
                println!("FAIL  {}: not accepted:", entry.name);
                for p in &problems {
                    println!("      {p}");
                }
            }
            continue;
        }

        let findings = baseline.gate(&entry.name, &observed, &entry.sometimes, declared.as_ref());
        for path in &findings.regressed {
            problems.push(format!(
                "population: `{path}` was always populated, now empty"
            ));
        }
        for path in &findings.new_fields {
            problems.push(format!(
                "population: `{path}` is new — run spec probe --accept"
            ));
        }
        for path in &findings.uncovered {
            problems.push(format!(
                "coverage: `{path}` is declared but never populated — wire it or accept it"
            ));
        }
        if problems.is_empty() {
            println!("probe: {} — ok ({} fields)", entry.name, observed.len());
        } else {
            failures += 1;
            println!("FAIL  {}:", entry.name);
            for p in &problems {
                println!("      {p}");
            }
        }
    }

    if probed == 0 {
        return Err("no probes matched".into());
    }
    if accept {
        if failures > 0 {
            println!("spec probe: NOT accepted ({failures} problem(s))");
            return Ok(1);
        }
        std::fs::write(&lock_path, baseline.render())
            .map_err(|e| format!("cannot write {}: {e}", lock_path.display()))?;
        println!("spec probe: accepted {probed} probe(s) into probes.lock");
        return Ok(0);
    }
    if failures > 0 {
        println!("spec probe: FAILED ({failures} problem(s))");
        Ok(1)
    } else {
        println!("spec probe: passed ({probed} probe(s))");
        Ok(0)
    }
}

fn uncovered_note(baseline: &Baseline, probe: &str) -> String {
    match baseline.probes.get(probe).map(|l| l.uncovered.len()) {
        Some(0) | None => String::new(),
        Some(n) => format!(", {n} uncovered"),
    }
}

fn parse_manifest(text: &str) -> Result<Manifest, String> {
    #[derive(PartialEq)]
    enum Section {
        None,
        Header,
        Probe,
    }
    let mut header = Header {
        ready_timeout_secs: 30,
        ..Header::default()
    };
    let mut probes: Vec<Probe> = Vec::new();
    let mut section = Section::None;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            if inner.trim() != "probe" {
                return Err(format!("line {lineno}: unknown table [[{}]]", inner.trim()));
            }
            section = Section::Probe;
            probes.push(Probe {
                name: String::new(),
                path: String::new(),
                status: 200,
                sometimes: Vec::new(),
                asserts: Vec::new(),
                shape: true,
            });
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = match inner.trim() {
                "probes" => Section::Header,
                other => return Err(format!("line {lineno}: unknown table [{other}]")),
            };
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(raw.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        match section {
            Section::None => return Err(format!("line {lineno}: key outside any table")),
            Section::Header => set_header(&mut header, key.trim(), value)
                .map_err(|e| format!("line {lineno}: {e}"))?,
            Section::Probe => {
                let entry = probes.last_mut().expect("section implies an entry");
                set_probe(entry, key.trim(), value).map_err(|e| format!("line {lineno}: {e}"))?;
            }
        }
    }

    for (i, entry) in probes.iter().enumerate() {
        if entry.name.is_empty() {
            return Err(format!("probe #{} has no name", i + 1));
        }
        if entry.path.is_empty() {
            return Err(format!("probe `{}` has no path", entry.name));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for entry in &probes {
        if !seen.insert(&entry.name) {
            return Err(format!("duplicate probe name `{}`", entry.name));
        }
    }
    Ok(Manifest { header, probes })
}

fn set_header(header: &mut Header, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("spec", TomlValue::Str(s)) => header.spec = Some(s),
        ("command", TomlValue::Str(s)) => header.command = Some(s),
        ("args", TomlValue::StrArray(a)) => header.args = a,
        ("env", TomlValue::Table(t)) => {
            for (name, v) in t {
                let TomlValue::Str(s) = v else {
                    return Err(format!("env `{name}` must be a string"));
                };
                header.env.insert(name, s);
            }
        }
        ("ready_timeout_secs", TomlValue::Int(n)) if n > 0 => {
            header.ready_timeout_secs = n as u64;
        }
        (key, _) => return Err(format!("unknown or mistyped [probes] key `{key}`")),
    }
    Ok(())
}

fn set_probe(entry: &mut Probe, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("name", TomlValue::Str(s)) => entry.name = s,
        ("path", TomlValue::Str(s)) => entry.path = s,
        ("status", TomlValue::Int(n)) if (100..=599).contains(&n) => entry.status = n as u16,
        ("sometimes", TomlValue::StrArray(a)) => entry.sometimes = a,
        ("shape", TomlValue::Bool(b)) => entry.shape = b,
        ("assert", TomlValue::StrArray(a)) => {
            for line in a {
                entry.asserts.push(Assertion::parse(&line)?);
            }
        }
        (key, _) => return Err(format!("unknown or mistyped [[probe]] key `{key}`")),
    }
    Ok(())
}

/// A launched embedded server, killed on drop.
struct Server {
    child: std::process::Child,
    base_url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn launch(header: &Header, pkg_dir: &Path) -> Result<Server, String> {
    let Some(command) = &header.command else {
        return Err("probes.toml has no [probes] command — pass --base-url".into());
    };
    let mut child = std::process::Command::new(pkg_dir.join(command))
        .args(&header.args)
        .envs(&header.env)
        .current_dir(pkg_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot launch {command}: {e}"))?;

    // Scan on a thread so a server that never announces its ready line
    // hits the timeout instead of blocking forever.
    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(rest) = line.strip_prefix("soothfast-ready ") {
                        let _ = tx.send(rest.trim().to_string());
                        break;
                    }
                }
            }
        }
        // Keep draining so the server never blocks on a full pipe.
        std::io::copy(&mut reader, &mut std::io::sink()).ok();
    });

    let announced = rx
        .recv_timeout(Duration::from_secs(header.ready_timeout_secs))
        .map_err(|_| {
            let _ = child.kill();
            format!(
                "{command} did not announce soothfast-ready within {}s",
                header.ready_timeout_secs
            )
        })?;
    let base_url = serde_json::from_str::<Value>(&announced)
        .ok()
        .and_then(|v| v.get("base_url").and_then(Value::as_str).map(String::from))
        .ok_or_else(|| format!("bad ready line: {announced}"))?;
    Ok(Server { child, base_url })
}

/// One HTTP/1.1 GET against a loopback server. Hand-rolled over
/// `TcpStream` in keeping with the dependency budget — no TLS, no
/// keep-alive, `Connection: close` and read to completion.
fn get(base: &str, path: &str) -> Result<(u16, Vec<u8>), String> {
    let authority = base
        .strip_prefix("http://")
        .ok_or_else(|| format!("only http:// base URLs are probed, got {base}"))?;
    let authority = authority.split('/').next().unwrap_or(authority);
    let stream = TcpStream::connect(authority).map_err(|e| format!("connect {authority}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| e.to_string())?;
    let mut stream = stream;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| format!("send {path}: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("read {path}: {e}"))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line for {path}: {status_line:?}"))?;

    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, val)) = line.split_once(':') {
            let val = val.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = val.parse::<usize>().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && val.eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }

    let body = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(len) = content_length {
        let mut body = vec![0u8; len];
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("read body: {e}"))?;
        body
    } else {
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|e| format!("read body: {e}"))?;
        body
    };
    Ok((status, body))
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .map_err(|e| e.to_string())?;
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| format!("bad chunk size {size_line:?}"))?;
        if size == 0 {
            break;
        }
        let mut chunk = vec![0u8; size + 2];
        reader
            .read_exact(&mut chunk)
            .map_err(|e| format!("read chunk: {e}"))?;
        chunk.truncate(size);
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "\
[probes]
spec = \"openapi.yaml\"
command = \"../target/release/server\"
env = { PORT = \"0\" }

[[probe]]
name = \"quote\"
path = \"/v2/quote/AAPL\"
sometimes = [\"preMarket*\"]
assert = [\"regularMarketPrice > 0\"]

[[probe]]
name = \"missing\"
path = \"/v2/nope\"
status = 404
";

    #[test]
    fn a_manifest_parses_with_defaults_applied() {
        let manifest = parse_manifest(MANIFEST).unwrap();
        assert_eq!(manifest.header.spec.as_deref(), Some("openapi.yaml"));
        assert_eq!(manifest.header.env["PORT"], "0");
        assert_eq!(manifest.header.ready_timeout_secs, 30);
        assert_eq!(manifest.probes.len(), 2);
        assert_eq!(manifest.probes[0].status, 200);
        assert_eq!(manifest.probes[0].sometimes, ["preMarket*"]);
        assert_eq!(manifest.probes[0].asserts.len(), 1);
        assert_eq!(manifest.probes[1].status, 404);
    }

    #[test]
    fn nameless_pathless_and_duplicate_probes_are_rejected() {
        assert!(parse_manifest("[[probe]]\npath = \"/x\"\n").is_err());
        assert!(parse_manifest("[[probe]]\nname = \"a\"\n").is_err());
        let dup =
            "[[probe]]\nname = \"a\"\npath = \"/x\"\n[[probe]]\nname = \"a\"\npath = \"/y\"\n";
        assert!(parse_manifest(dup).is_err());
    }

    #[test]
    fn unknown_tables_and_keys_are_errors_not_silence() {
        assert!(parse_manifest("[site]\nname = \"x\"\n").is_err());
        assert!(parse_manifest("[probes]\nbogus = 1\n").is_err());
        assert!(parse_manifest("[[probe]]\nname = \"a\"\npath = \"/x\"\nbogus = 1\n").is_err());
    }
}
