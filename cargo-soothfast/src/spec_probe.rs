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
//!
//! `--accept-passing` locks the probes that passed and leaves the rest at
//! their existing values, for a manifest whose live upstreams fail often
//! enough that a whole-run accept rarely lands. It still reports every
//! failure and still exits non-zero.

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
    /// Request verb, upper-cased at parse time. Defaults to `GET`.
    method: String,
    /// Request body, sent as `application/json`. Absent means no body.
    body: Option<String>,
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

/// Statuses whose responses carry no body, so an empty one is the contract
/// rather than a missing answer.
const NO_CONTENT_STATUSES: [u16; 3] = [204, 205, 304];

pub fn run(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut accept = false;
    let mut accept_passing = false;
    let mut allow_gone = false;
    let mut base_url = None;
    let mut filter = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--accept" => accept = true,
            "--accept-passing" => accept_passing = true,
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
        accept_passing,
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
    accept_passing: bool,
    allow_gone: bool,
    base_url: Option<&str>,
    filter: Option<&str>,
) -> Result<i32, String> {
    let pkg_dir = invoke::pkg_dir(pkg).map_err(|e| e.to_string())?;
    probe_in(
        &pkg_dir,
        accept,
        accept_passing,
        allow_gone,
        base_url,
        filter,
    )
}

fn probe_in(
    pkg_dir: &Path,
    accept: bool,
    accept_passing: bool,
    allow_gone: bool,
    base_url: Option<&str>,
    filter: Option<&str>,
) -> Result<i32, String> {
    let manifest_path = pkg_dir.join("probes.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest = parse_manifest(&manifest_text)?;

    let mut routes = Vec::new();
    let spec = match &manifest.header.spec {
        Some(file) => {
            let path = pkg_dir.join(file);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read spec {}: {e}", path.display()))?;
            if let Some(kind) = soothfast_spec::sniff_kind(file, &text) {
                routes = soothfast_spec::providers::parse(kind, &text).unwrap_or_default();
            }
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
            let launched = launch(&manifest.header, pkg_dir)?;
            let url = launched.base_url.clone();
            _server = launched;
            url
        }
    };

    let accepting = accept || accept_passing;
    let declared: Vec<String> = manifest.probes.iter().map(|p| p.name.clone()).collect();
    let gone: Vec<String> = baseline
        .probes
        .keys()
        .filter(|name| !declared.contains(name))
        .cloned()
        .collect();
    // A partial write must not delete a locked probe as a side effect of an
    // unrelated failure, so only a full accept or --allow-gone sweeps them.
    let drop_gone = allow_gone || accept;
    if drop_gone {
        baseline.retain_probes(&declared);
    }
    let mut failures = 0u32;
    if !gone.is_empty() && !drop_gone {
        for name in &gone {
            failures += 1;
            println!(
                "FAIL  probe `{name}` is locked but no longer in probes.toml (--allow-gone to drop)"
            );
        }
    }

    let mut probed = 0usize;
    let mut accepted = 0usize;
    for entry in &manifest.probes {
        if let Some(f) = filter
            && !entry.name.contains(f)
        {
            continue;
        }
        probed += 1;
        let (status, body) = match request(&base, &entry.method, &entry.path, entry.body.as_deref())
        {
            Ok(response) => response,
            Err(e) => {
                // One unreachable endpoint must not discard the rest of
                // the run the way an early return would.
                failures += 1;
                println!("FAIL  {}: {e}", entry.name);
                continue;
            }
        };
        if status != entry.status {
            failures += 1;
            println!(
                "FAIL  {}: {} answered {status}, expected {}",
                entry.name, entry.path, entry.status
            );
            continue;
        }
        // A no-content answer has nothing to parse. It still reaches the
        // lock, so an endpoint that stops returning data reads as a
        // regression rather than a pass.
        //
        // Only a status that cannot carry content may answer empty. Anywhere
        // else an empty body would skip every declared check in silence, and
        // on a first accept would lock a broken endpoint as the baseline.
        let value: Option<Value> = if body.iter().all(u8::is_ascii_whitespace) {
            None
        } else {
            match serde_json::from_slice(&body) {
                Ok(v) => Some(v),
                Err(e) => {
                    failures += 1;
                    println!("FAIL  {}: {} body is not JSON: {e}", entry.name, entry.path);
                    continue;
                }
            }
        };

        let mut problems = Vec::new();
        let mut declared = None;
        if let Some(value) = &value {
            if let Some(spec) = spec.as_ref().filter(|_| entry.shape) {
                match shape::response_schema(spec, &entry.method, &entry.path, entry.status) {
                    Some(schema) => {
                        for v in shape::validate(value, schema, spec) {
                            problems.push(format!("shape: {} — {}", v.path, v.message));
                        }
                        declared = Some(coverage::declared_paths(schema, spec));
                    }
                    None => problems.push("shape: no response schema in spec".to_string()),
                }
            }
            for assertion in &entry.asserts {
                if let Err(reason) = assertion.check(value) {
                    problems.push(format!("assert: {reason}"));
                }
            }
        } else if !NO_CONTENT_STATUSES.contains(&entry.status)
            && (!entry.asserts.is_empty() || (spec.is_some() && entry.shape))
        {
            problems.push("empty body: declared expectations could not be evaluated".to_string());
        }

        let observed = value.as_ref().map(population::populate).unwrap_or_default();
        if accepting {
            if problems.is_empty() {
                accepted += 1;
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
    if !routes.is_empty() {
        let covered = routes
            .iter()
            .filter(|op| manifest.probes.iter().any(|p| probe_covers(p, op)))
            .count();
        println!(
            "spec probe: {covered} of {} spec route(s) covered by a probe",
            routes.len()
        );
    }
    if accepting {
        if failures > 0 && !accept_passing {
            println!("spec probe: NOT accepted ({failures} problem(s))");
            return Ok(1);
        }
        std::fs::write(&lock_path, baseline.render())
            .map_err(|e| format!("cannot write {}: {e}", lock_path.display()))?;
        if failures > 0 {
            println!(
                "spec probe: accepted {accepted} of {probed} probe(s), {failures} left as they were"
            );
            return Ok(1);
        }
        println!("spec probe: accepted {accepted} probe(s) into probes.lock");
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

/// Whether `entry` exercises `op`: same verb, and a path the route's
/// template accepts.
fn probe_covers(entry: &Probe, op: &soothfast_spec::DeclaredOp) -> bool {
    let path = entry.path.split('?').next().unwrap_or(&entry.path);
    let concrete: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    entry.method.eq_ignore_ascii_case(&op.method) && shape::template_matches(&op.path, &concrete)
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
                method: "GET".to_string(),
                body: None,
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
        ("method", TomlValue::Str(s)) => entry.method = s.to_ascii_uppercase(),
        // A body spans more lines than the mini-TOML parser joins, so an
        // array of fragments is accepted alongside a single-line string.
        ("body", TomlValue::Str(s)) => entry.body = Some(s),
        ("body", TomlValue::StrArray(a)) => entry.body = Some(a.join("\n")),
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

/// One HTTP/1.1 request against a loopback server. Hand-rolled over
/// `TcpStream` in keeping with the dependency budget — no TLS, no
/// keep-alive, `Connection: close` and read to completion.
fn request(
    base: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, Vec<u8>), String> {
    let authority = base
        .strip_prefix("http://")
        .ok_or_else(|| format!("only http:// base URLs are probed, got {base}"))?;
    let authority = authority.split('/').next().unwrap_or(authority);
    let stream = TcpStream::connect(authority).map_err(|e| format!("connect {authority}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| e.to_string())?;
    let mut stream = stream;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\n"
    );
    if let Some(body) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|e| format!("send {path}: {e}"))?;
    if let Some(body) = body {
        stream
            .write_all(body.as_bytes())
            .map_err(|e| format!("send {path}: {e}"))?;
    }

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

    use std::io::BufWriter;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    struct Fake {
        base: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    /// Serve `replies` (path -> status, body) over `connections` requests,
    /// then stop listening so a later probe sees a refused connection.
    fn fake_server(replies: Vec<(&'static str, u16, &'static str)>, connections: usize) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        std::thread::spawn(move || {
            for _ in 0..connections {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if let Some((name, value)) = line.trim_end().split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    if line.trim_end().is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                head.push_str(std::str::from_utf8(&body).unwrap_or(""));

                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                recorder.lock().unwrap().push(head);

                let (status, payload) = replies
                    .iter()
                    .find(|(p, _, _)| *p == path)
                    .map(|(_, s, b)| (*s, *b))
                    .unwrap_or((404, ""));
                let mut out = BufWriter::new(stream);
                let _ = write!(
                    out,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = out.flush();
            }
        });
        Fake { base, seen }
    }

    fn pkg_with(manifest: &str, lock: Option<&str>) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("soothfast-probe-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("probes.toml"), manifest).unwrap();
        if let Some(lock) = lock {
            std::fs::write(dir.join("probes.lock"), lock).unwrap();
        }
        dir
    }

    #[test]
    fn a_post_probe_sends_its_verb_and_body() {
        let server = fake_server(vec![("/items", 201, "{\"id\":7}")], 1);
        let dir = pkg_with(
            "[[probe]]\nname = \"create\"\npath = \"/items\"\nmethod = \"post\"\nbody = \"{\\\"n\\\": 1}\"\nstatus = 201\n",
            None,
        );
        let code = probe_in(&dir, true, false, false, Some(&server.base), None).unwrap();
        assert_eq!(code, 0);

        let seen = server.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with("POST /items HTTP/1.1"), "{}", seen[0]);
        assert!(seen[0].contains("Content-Type: application/json"));
        assert!(seen[0].contains("Content-Length: 8"));
        assert!(seen[0].ends_with("{\"n\": 1}"), "{}", seen[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_no_content_response_is_probed_rather_than_failing_to_parse() {
        let server = fake_server(vec![("/items/1", 204, "")], 1);
        let dir = pkg_with(
            "[[probe]]\nname = \"remove\"\npath = \"/items/1\"\nmethod = \"DELETE\"\nstatus = 204\n",
            None,
        );
        let code = probe_in(&dir, true, false, false, Some(&server.base), None).unwrap();
        assert_eq!(code, 0);
        let lock = std::fs::read_to_string(dir.join("probes.lock")).unwrap();
        assert!(lock.contains("remove"), "{lock}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_body_fails_a_probe_that_declared_expectations() {
        let server = fake_server(vec![("/items", 200, "")], 1);
        let dir = pkg_with(
            "[[probe]]\nname = \"list\"\npath = \"/items\"\nassert = [\"total > 0\"]\n",
            None,
        );
        let code = probe_in(&dir, true, false, false, Some(&server.base), None).unwrap();
        assert_eq!(code, 1);
        assert!(
            !dir.join("probes.lock").exists(),
            "a broken endpoint was locked"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const TWO_PROBES: &str = "\
[[probe]]
name = \"good\"
path = \"/good\"

[[probe]]
name = \"flaky\"
path = \"/flaky\"
";

    #[test]
    fn accept_passing_locks_the_probes_that_passed() {
        let server = fake_server(vec![("/good", 200, "{\"a\":1}"), ("/flaky", 500, "{}")], 2);
        let dir = pkg_with(TWO_PROBES, None);
        let code = probe_in(&dir, false, true, false, Some(&server.base), None).unwrap();
        assert_eq!(code, 1);
        let lock = std::fs::read_to_string(dir.join("probes.lock")).unwrap();
        assert!(lock.contains("good"), "{lock}");
        assert!(!lock.contains("flaky"), "{lock}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plain_accept_still_writes_nothing_when_a_probe_fails() {
        let server = fake_server(vec![("/good", 200, "{\"a\":1}"), ("/flaky", 500, "{}")], 2);
        let dir = pkg_with(TWO_PROBES, None);
        let code = probe_in(&dir, true, false, false, Some(&server.base), None).unwrap();
        assert_eq!(code, 1);
        assert!(!dir.join("probes.lock").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partial_accept_does_not_silently_drop_a_locked_probe() {
        let server = fake_server(vec![("/good", 200, "{\"a\":1}"), ("/flaky", 500, "{}")], 2);
        let existing = "{\n  \"version\": 1,\n  \"probes\": {\n    \"retired\": { \"fields\": { \"z\": \"always\" } }\n  }\n}\n";
        let dir = pkg_with(TWO_PROBES, Some(existing));
        let code = probe_in(&dir, false, true, false, Some(&server.base), None).unwrap();
        assert_eq!(code, 1);
        let lock = std::fs::read_to_string(dir.join("probes.lock")).unwrap();
        assert!(
            lock.contains("retired"),
            "a locked probe was deleted: {lock}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreachable_probe_fails_alone_instead_of_aborting_the_run() {
        let server = fake_server(vec![("/good", 200, "{\"a\":1}")], 1);
        let dir = pkg_with(TWO_PROBES, None);
        let code = probe_in(&dir, false, false, false, Some(&server.base), None).unwrap();
        assert_eq!(code, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
