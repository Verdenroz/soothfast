//! `cargo soothfast verdict`: one result per gate job (`record`), folded into
//! the single pull request comment and check (`merge`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const TAIL_LINES: usize = 40;
const REUSE_LINE: &str = "items a stored measurement covers";
const REUSE_ONE_SIDED: &str = "items run code a stored measurement covers";
const NOTHING_TO_COMPARE: &str = "nothing to compare";
const CLAIMS_SKIPPED: &str = "claims not evaluated";
const KINDS: [&str; 5] = ["rust", "import", "docs", "test", "check"];
const UNREADABLE: &str = "result";
/// The exit code a `check` job ends with when it found only advisory
/// findings. The job's record reads it as `advisory`; the caller's own exit
/// status is 0. Only `ci spec` exits with it, so a check job must not pass
/// on the exit code of a command that is not `cargo soothfast`.
pub(crate) const ADVISORY_EXIT: i32 = 75;
/// GitHub rejects a comment over 65,536 characters; the rest is the marker.
const SHORT_SHA: usize = 7;
const NOTE_SHA: usize = 12;
const RECORD_LIMIT: u64 = 4 << 20;
const BODY_LIMIT: usize = 65_000;
const TRUNCATED: &str = "output truncated";
const LANDING_NOTE: &str = "\nNo lane ran: this pull request is a soothfast-bot landing, which holds only regenerated files.\n";

/// Declared worst first: sorting ascending lists the sections to read first,
/// and the minimum of two verdicts is the one that decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Errored,
    Regressed,
    Advisory,
    Pass,
    NothingToCompare,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Verdict::Errored => "errored",
            Verdict::Regressed => "regressed",
            Verdict::Advisory => "advisory",
            Verdict::Pass => "pass",
            Verdict::NothingToCompare => "nothing-to-compare",
        }
    }

    fn parse(name: &str) -> Option<Verdict> {
        [
            Verdict::Errored,
            Verdict::Regressed,
            Verdict::Advisory,
            Verdict::Pass,
            Verdict::NothingToCompare,
        ]
        .into_iter()
        .find(|v| v.name() == name)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reuse {
    covered: u64,
    total: u64,
    measuring: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    name: String,
    kind: String,
    verdict: Verdict,
    tail: String,
    reuse: Option<Reuse>,
    seconds: u64,
    flaky: bool,
    unevaluated: bool,
    /// The commit whose pass this lane carries, when it did not run.
    carried: Option<String>,
    /// A coverage check's figure for each dimension it could tell.
    coverage: Vec<Figure>,
}

/// One coverage dimension's tally and the minimum it was held to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Figure {
    dimension: String,
    covered: u64,
    total: u64,
    pct: u64,
    min: Option<u64>,
    /// The figure is not exact; the report's note gives the direction.
    partial: bool,
}

impl Figure {
    fn to_json(&self) -> Value {
        json!({
            "dimension": self.dimension,
            "covered": self.covered,
            "total": self.total,
            "pct": self.pct,
            "min": self.min,
            "partial": self.partial,
        })
    }

    fn from_json(v: &Value) -> Option<Figure> {
        Some(Figure {
            dimension: v.get("dimension")?.as_str()?.to_string(),
            covered: v.get("covered")?.as_u64()?,
            total: v.get("total")?.as_u64()?,
            pct: v.get("pct")?.as_u64()?,
            min: v.get("min").and_then(Value::as_u64),
            partial: v["partial"].as_bool().unwrap_or(false),
        })
    }
}

/// The test runner's verdict words, which are not the gate's.
fn test_verdict(word: &str) -> Option<(Verdict, bool)> {
    match word {
        "reused" | "skipped" => Some((Verdict::Pass, false)),
        "flaky" => Some((Verdict::Pass, true)),
        "failed" => Some((Verdict::Errored, false)),
        _ => None,
    }
}

impl Record {
    /// A test record uses the test runner's field names and words, so one
    /// reader serves both producers.
    fn to_json(&self) -> Value {
        let mut v = if self.kind == "test" {
            json!({
                "name": self.name,
                "kind": self.kind,
                "verdict": if self.verdict == Verdict::Errored { "failed" } else { "pass" },
                "output": self.tail,
                "wall_s": self.seconds,
            })
        } else {
            json!({
                "name": self.name,
                "kind": self.kind,
                "verdict": self.verdict.name(),
                "tail": self.tail,
                "seconds": self.seconds,
            })
        };
        if let Some(r) = self.reuse {
            v["reuse"] = json!({"covered": r.covered, "total": r.total, "measuring": r.measuring});
        }
        if self.flaky {
            v["flaky"] = json!(true);
        }
        if self.unevaluated {
            v["unevaluated"] = json!(true);
        }
        if let Some(commit) = &self.carried {
            v["carried"] = json!(commit);
        }
        if !self.coverage.is_empty() {
            v["coverage"] = self.coverage.iter().map(Figure::to_json).collect();
        }
        v
    }

    /// Reads a gate record, or a test record: `output` for `tail`, `wall_s`
    /// for `seconds`, and the test runner's verdict words.
    fn from_json(v: &Value) -> Option<Record> {
        let reuse = v.get("reuse").and_then(|r| {
            Some(Reuse {
                covered: r.get("covered")?.as_u64()?,
                total: r.get("total")?.as_u64()?,
                measuring: r.get("measuring")?.as_u64()?,
            })
        });
        let word = v.get("verdict")?.as_str()?;
        let (verdict, flaky) = match Verdict::parse(word) {
            Some(verdict) => (verdict, v["flaky"].as_bool().unwrap_or(false)),
            None => test_verdict(word)?,
        };
        let tail = match v.get("tail") {
            Some(t) => strip_ansi(t.as_str()?),
            None => tail(&strip_ansi(v.get("output")?.as_str()?)),
        };
        let seconds = v.get("seconds").or(v.get("wall_s"))?;
        Some(Record {
            name: v.get("name")?.as_str()?.to_string(),
            kind: v.get("kind")?.as_str()?.to_string(),
            verdict,
            tail,
            reuse,
            seconds: seconds
                .as_u64()
                .or_else(|| seconds.as_f64().map(|s| s.round() as u64))?,
            flaky,
            unevaluated: v["unevaluated"].as_bool().unwrap_or(false),
            carried: v["carried"].as_str().map(String::from),
            coverage: v["coverage"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Figure::from_json)
                .collect(),
        })
    }

    /// Folds another shard of the same project into this one: the worse
    /// verdict wins and brings its own output (a worse record with none
    /// leaves the existing output), equal verdicts' outputs are joined, reuse
    /// adds up, and shards run side by side so the slowest sets the time.
    fn absorb(&mut self, other: Record) {
        // The output of a worse verdict replaces that of a better one, so a
        // regressed section shows its regression; equals are kept together.
        match other.verdict.cmp(&self.verdict) {
            std::cmp::Ordering::Less if !other.tail.is_empty() => self.tail = other.tail,
            std::cmp::Ordering::Equal if !other.tail.is_empty() => {
                if !self.tail.is_empty() {
                    self.tail.push('\n');
                }
                self.tail.push_str(&other.tail);
                self.tail = tail(&self.tail);
            }
            _ => {}
        }
        self.verdict = self.verdict.min(other.verdict);
        self.reuse = match (self.reuse, other.reuse) {
            (Some(a), Some(b)) => Some(Reuse {
                covered: a.covered.saturating_add(b.covered),
                total: a.total.saturating_add(b.total),
                measuring: a.measuring.saturating_add(b.measuring),
            }),
            (a, b) => a.or(b),
        };
        self.seconds = self.seconds.max(other.seconds);
        self.flaky |= other.flaky;
        self.unevaluated |= other.unevaluated;
        self.carried = self.carried.take().filter(|_| other.carried.is_some());
        self.coverage.extend(other.coverage);
    }
}

pub fn run(args: &[String]) -> i32 {
    let result = match args.first().map(String::as_str) {
        Some("record") => record(&args[1..]),
        Some("merge") => merge(&args[1..]),
        _ => Err("usage: cargo soothfast verdict record|merge".to_string()),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: verdict: {e}");
            2
        }
    }
}

fn value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String, String> {
    it.next().cloned().ok_or(format!("{flag} needs a value"))
}

fn record(args: &[String]) -> Result<i32, String> {
    let (mut name, mut kind, mut code, mut output) = (None, None, None, None);
    let mut seconds = 0;
    let mut out = PathBuf::from("result.json");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => name = Some(value(&mut it, a)?),
            "--kind" => kind = Some(value(&mut it, a)?),
            "--exit" => code = Some(value(&mut it, a)?),
            "--output" => output = Some(value(&mut it, a)?),
            "--seconds" => {
                seconds = value(&mut it, a)?
                    .parse()
                    .map_err(|_| "--seconds is not a number")?
            }
            "--out" => out = PathBuf::from(value(&mut it, a)?),
            other => return Err(format!("unknown record arg {other:?}")),
        }
    }
    let name = name.ok_or("record needs --name")?;
    let kind = kind.ok_or("record needs --kind")?;
    let code: i32 = code
        .ok_or("record needs --exit")?
        .parse()
        .map_err(|_| "--exit is not a number")?;
    let output = output.ok_or("record needs --output")?;
    write_record(name, kind, code, Path::new(&output), seconds, &out)?;
    Ok(0)
}

/// Writes one job's record to `out`, read from the job's exit code and the
/// output saved in `output`.
pub(crate) fn write_record(
    name: String,
    kind: String,
    code: i32,
    output: &Path,
    seconds: u64,
    out: &Path,
) -> Result<(), String> {
    if !KINDS.contains(&kind.as_str()) {
        return Err(format!("--kind is one of {}", KINDS.join(", ")));
    }
    // A job that died before the gate wrote anything still records an error.
    let output = fs::read_to_string(output).unwrap_or_default();
    store(&build(name, kind, code, &output, seconds), out)
}

/// Writes to `out` the record of a lane that did not run because `commit`
/// passed it with the same inputs.
pub(crate) fn write_carried(
    name: String,
    kind: String,
    commit: &str,
    out: &Path,
) -> Result<(), String> {
    let tail = format!(
        "inputs unchanged since {} passed this lane; nothing ran",
        short(commit, NOTE_SHA)
    );
    let rec = Record {
        name,
        kind,
        verdict: Verdict::Pass,
        tail,
        reuse: None,
        seconds: 0,
        flaky: false,
        unevaluated: false,
        carried: Some(commit.to_string()),
        coverage: Vec::new(),
    };
    store(&rec, out)
}

fn store(rec: &Record, out: &Path) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&rec.to_json()).map_err(|e| e.to_string())?;
    fs::write(out, text).map_err(|e| format!("{}: {e}", out.display()))
}

fn short(sha: &str, len: usize) -> String {
    sha.chars().take(len).collect()
}

fn build(name: String, kind: String, code: i32, output: &str, seconds: u64) -> Record {
    let (output, coverage) = split_coverage(&strip_ansi(output));
    let verdict = classify(code, &kind, &output);
    Record {
        name,
        tail: if matches!(verdict, Verdict::Errored | Verdict::Advisory) {
            tail(&output)
        } else {
            tail(&drop_warnings(&output))
        },
        reuse: parse_reuse(&output),
        unevaluated: kind == "docs" && output.contains(CLAIMS_SKIPPED),
        carried: None,
        coverage,
        kind,
        verdict,
        seconds,
        flaky: false,
    }
}

/// The output without the summary line `coverage report --check` ends with,
/// and the figures that line carries.
fn split_coverage(output: &str) -> (String, Vec<Figure>) {
    let mut figures = Vec::new();
    let mut kept = Vec::new();
    for line in output.lines() {
        match line.strip_prefix(crate::coverage_report::SUMMARY_PREFIX) {
            Some(json) => {
                let parsed: Option<Value> = serde_json::from_str(json).ok();
                let rows = parsed.as_ref().and_then(Value::as_array);
                figures.extend(rows.into_iter().flatten().filter_map(Figure::from_json));
            }
            None => kept.push(line),
        }
    }
    (kept.join("\n"), figures)
}

/// A docs or check failure is drift or breakage, not a perf regression, so
/// it is never one the perf label may waive.
fn classify(code: i32, kind: &str, output: &str) -> Verdict {
    match code {
        0 if output.contains(NOTHING_TO_COMPARE) => Verdict::NothingToCompare,
        0 => Verdict::Pass,
        1 if matches!(kind, "rust" | "import") => Verdict::Regressed,
        ADVISORY_EXIT if kind == "check" => Verdict::Advisory,
        _ => Verdict::Errored,
    }
}

/// Removes CSI and OSC escape sequences; cargo colours its stderr even when
/// it is piped.
pub(crate) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if c == '\x07' || (prev == '\x1b' && c == '\\') {
                        break;
                    }
                    prev = c;
                }
            }
            _ => {}
        }
    }
    out
}

/// Drops each `warning:` line with the indented lines under it.
fn drop_warnings(output: &str) -> String {
    let mut in_warning = false;
    let mut kept = Vec::new();
    for line in output.lines() {
        if line.starts_with("warning:") {
            in_warning = true;
        } else if !(in_warning && line.starts_with([' ', '\t'])) {
            in_warning = false;
            kept.push(line);
        }
    }
    kept.join("\n")
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

fn parse_reuse(output: &str) -> Option<Reuse> {
    output
        .lines()
        .find_map(|l| two_sided(l).or_else(|| one_sided(l)))
}

/// `3/5 items run code a stored measurement covers, measuring 2`.
fn one_sided(line: &str) -> Option<Reuse> {
    let (before, rest) = line.split_once(REUSE_ONE_SIDED)?;
    let (covered, total) = before.split_whitespace().last()?.split_once('/')?;
    let measuring = rest
        .trim_start_matches(',')
        .trim()
        .strip_prefix("measuring ")?;
    Some(Reuse {
        covered: covered.parse().ok()?,
        total: total.parse().ok()?,
        measuring: measuring.split_whitespace().next()?.parse().ok()?,
    })
}

/// `items a stored measurement covers: head 14/16, base 16/16, measuring 2`,
/// counted over both sides.
fn two_sided(line: &str) -> Option<Reuse> {
    let rest = line.split_once(REUSE_LINE)?.1.trim_start_matches(':');
    let mut reuse = Reuse::default();
    for part in rest.split(',').map(str::trim) {
        if let Some(n) = part.strip_prefix("measuring ") {
            reuse.measuring = n.split_whitespace().next()?.parse().ok()?;
        } else {
            let (covered, total) = part.split_once(' ')?.1.split_once('/')?;
            reuse.covered = reuse.covered.saturating_add(covered.parse().ok()?);
            reuse.total = reuse.total.saturating_add(total.parse().ok()?);
        }
    }
    Some(reuse)
}

fn merge(args: &[String]) -> Result<i32, String> {
    let mut dir = None;
    let mut out = None;
    let mut accepted = None;
    let mut expect = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = Some(value(&mut it, a)?),
            "--accepted" => accepted = Some(value(&mut it, a)?),
            "--expect" => expect.push(expected(&value(&mut it, a)?)?),
            other if !other.starts_with("--") && dir.is_none() => dir = Some(PathBuf::from(other)),
            other => return Err(format!("unknown merge arg {other:?}")),
        }
    }
    let dir = dir.ok_or("merge needs a results directory")?;
    let out = out.ok_or("merge needs --out")?;
    merge_dir(&dir, Path::new(&out), accepted.as_deref(), &expect, false)
}

/// Folds the records under `dir` into the comment body at `out`. Each
/// `expect` entry is a `(name, kind)` that must have a record, and a
/// regression is waived when `accepted` names the label that waived it.
/// `landing` says the plan ran no lane because the pull request is a bot
/// landing. Returns 1 when the run fails.
pub(crate) fn merge_dir(
    dir: &Path,
    out: &Path,
    accepted: Option<&str>,
    expect: &[(String, String)],
    landing: bool,
) -> Result<i32, String> {
    let raw = read_records(dir);
    let seen: BTreeSet<String> = raw.iter().map(|r| r.name.clone()).collect();
    let mut records = fold(raw);
    for (name, kind) in expect {
        if !seen.contains(name) {
            let tail = "the job recorded no result".into();
            records.push(errored(name.clone(), kind.clone(), tail));
        }
    }
    records.sort_by(|a, b| (a.verdict, &a.name).cmp(&(b.verdict, &b.name)));
    let mut body = fit(&records, accepted);
    if landing {
        body.push_str(LANDING_NOTE);
    }
    fs::write(out, body).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(i32::from(fails(&records, accepted.is_some())))
}

fn errored(name: String, kind: String, tail: String) -> Record {
    Record {
        name,
        kind,
        verdict: Verdict::Errored,
        tail,
        reuse: None,
        seconds: 0,
        flaky: false,
        unevaluated: false,
        carried: None,
        coverage: Vec::new(),
    }
}

/// `NAME:KIND`, the kind a missing result is shown with.
pub(crate) fn expected(arg: &str) -> Result<(String, String), String> {
    match arg.split_once(':') {
        Some((name, kind)) if KINDS.contains(&kind) => Ok((name.to_string(), kind.to_string())),
        _ => Err(format!(
            "--expect is NAME:KIND with KIND one of {}",
            KINDS.join(", ")
        )),
    }
}

fn read_capped(path: &Path) -> Result<String, String> {
    let len = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > RECORD_LIMIT {
        return Err(format!("{len} bytes is over the {RECORD_LIMIT} byte limit"));
    }
    fs::read_to_string(path).map_err(|e| e.to_string())
}

/// `text` with every character `keep` rejects replaced by `?`.
fn only(text: &str, keep: impl Fn(char) -> bool) -> String {
    text.chars()
        .map(|c| if keep(c) { c } else { '?' })
        .collect()
}

/// A result's name is shown outside the code fence, in a code span, which
/// GitHub does not autolink. Only characters that cannot end the span or the
/// line survive.
fn plain(text: &str) -> String {
    only(text, |c| c.is_ascii_alphanumeric() || "._-+/:#".contains(c))
}

/// A kind is shown outside the code fence as it is, so only the kinds a
/// record is written with survive.
fn known(kind: &str) -> &str {
    if KINDS.contains(&kind) || kind == UNREADABLE {
        kind
    } else {
        "?"
    }
}

/// Every result under `dir`. A directory that is missing is empty, and a
/// file that is unreadable or not a record becomes an errored section named
/// after it, so a bad artifact cannot stop the comment.
fn read_records(dir: &Path) -> Vec<Record> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "json") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .iter()
        .map(|f| {
            let text = read_capped(f);
            let record = text.and_then(|t| {
                let v: Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                Record::from_json(&v).ok_or("not a verdict record".to_string())
            });
            record.unwrap_or_else(|e| {
                let name = f.strip_prefix(dir).unwrap_or(f).display().to_string();
                errored(name, UNREADABLE.into(), format!("unreadable result: {e}"))
            })
        })
        .collect()
}

/// The names of the records under `dir` that are a plain pass: not carried,
/// not flaky, and with every claim evaluated.
pub(crate) fn plain_passes(dir: &Path) -> BTreeSet<String> {
    read_records(dir)
        .into_iter()
        .filter(|r| r.verdict == Verdict::Pass && !r.flaky && !r.unevaluated)
        .filter(|r| r.carried.is_none())
        .map(|r| r.name)
        .collect()
}

/// One record per name. A name with `#` after it (`pkg#1/3`, a shard) folds
/// into the one before it, so shards share a section and each is still
/// expected on its own.
fn fold(records: Vec<Record>) -> Vec<Record> {
    let mut by_name: BTreeMap<String, Record> = BTreeMap::new();
    for mut rec in records {
        let (name, shard) = match rec.name.split_once('#') {
            Some((name, shard)) => (name.to_string(), Some(shard.to_string())),
            None => (rec.name.clone(), None),
        };
        if let (Some(shard), false) = (shard, rec.tail.is_empty()) {
            rec.tail = format!("-- shard {shard} --\n{}", rec.tail);
        }
        rec.name.clone_from(&name);
        match by_name.get_mut(&name) {
            Some(seen) => seen.absorb(rec),
            None => {
                by_name.insert(name, rec);
            }
        }
    }
    by_name.into_values().collect()
}

/// Regressions fail unless a writer accepted them; an error never waives.
/// An advisory finding never fails.
fn fails(records: &[Record], accepted: bool) -> bool {
    records.iter().any(|r| match r.verdict {
        Verdict::Errored => true,
        Verdict::Regressed => !accepted,
        _ => false,
    })
}

/// The comment body under GitHub's size limit: sections are emptied from the
/// end of the sort order, so passing output goes first and errors keep theirs.
fn fit(records: &[Record], accepted: Option<&str>) -> String {
    let mut records = records.to_vec();
    let mut body = render(&records, accepted);
    for i in (0..records.len()).rev() {
        if body.chars().count() <= BODY_LIMIT {
            break;
        }
        records[i].tail = TRUNCATED.into();
        body = render(&records, accepted);
    }
    body
}

impl Figure {
    fn cell(&self) -> String {
        let bound = if self.partial { " (partial)" } else { "" };
        match self.min {
            Some(min) if self.pct < min => format!("{}%{bound} (min {min}, not met)", self.pct),
            Some(min) => format!("{}%{bound} (min {min})", self.pct),
            None => format!("{}%{bound}", self.pct),
        }
    }
}

/// Package by dimension, from the figures coverage checks recorded. Empty
/// when no record carries any.
fn coverage_table(records: &[Record]) -> String {
    let known = |f: &&Figure| crate::coverage_report::is_dimension(&f.dimension);
    let mut rows: Vec<&Record> = records.iter().filter(|r| !r.coverage.is_empty()).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    let mut columns: Vec<&str> = Vec::new();
    for f in rows.iter().flat_map(|r| &r.coverage).filter(known) {
        if !columns.contains(&f.dimension.as_str()) {
            columns.push(&f.dimension);
        }
    }
    if columns.is_empty() {
        return String::new();
    }
    let mut out = format!("\n| package | {} |\n", columns.join(" | "));
    out.push_str(&format!("|{}\n", " --- |".repeat(columns.len() + 1)));
    for r in rows {
        let cells: Vec<String> = columns
            .iter()
            .map(|c| {
                let figure = r.coverage.iter().find(|f| f.dimension == *c);
                figure.map_or("n/a".to_string(), Figure::cell)
            })
            .collect();
        let name = plain(r.name.strip_prefix("coverage-").unwrap_or(&r.name));
        out.push_str(&format!("| {name} | {} |\n", cells.join(" | ")));
    }
    out
}

fn render(records: &[Record], accepted: Option<&str>) -> String {
    let count = |v: Verdict| records.iter().filter(|r| r.verdict == v).count();
    let advisory = match count(Verdict::Advisory) {
        0 => String::new(),
        n => format!(" {n} advisory,"),
    };
    let mut out = format!(
        "## soothfast verdict\n{} projects: {} regressed, {} errored,{advisory} {} pass\n",
        records.len(),
        count(Verdict::Regressed),
        count(Verdict::Errored),
        count(Verdict::Pass) + count(Verdict::NothingToCompare),
    );
    if !advisory.is_empty() {
        out.push_str("\nAdvisory findings are reported and do not fail the run.\n");
    }
    if let Some(label) = accepted.filter(|_| count(Verdict::Regressed) > 0) {
        out.push_str(&format!("\nRegressions accepted by the `{label}` label.\n"));
    }
    out.push_str(&coverage_table(records));
    for r in records {
        let carried = r
            .carried
            .as_deref()
            .map(|c| only(&short(c, SHORT_SHA), |c| c.is_ascii_hexdigit()))
            .map(|sha| format!(" (carried from {sha})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n### `{}`\n{}{carried}{}{} | {} | {}s",
            plain(&r.name),
            r.verdict.name(),
            if r.flaky { " (flaky)" } else { "" },
            if r.unevaluated {
                " (claims not evaluated)"
            } else {
                ""
            },
            known(&r.kind),
            r.seconds
        ));
        if let Some(u) = r.reuse {
            out.push_str(&format!(
                " | {}/{} items covered, measuring {}",
                u.covered, u.total, u.measuring
            ));
        }
        // The tail is the pull request's own output under a write-access
        // author, so nothing in it may close the fence.
        out.push_str(&format!("\n```\n{}\n```\n", r.tail.replace("```", "` ` `")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempDir;

    fn rec(name: &str, verdict: Verdict, tail: &str) -> Record {
        Record {
            name: name.into(),
            kind: "rust".into(),
            verdict,
            tail: tail.into(),
            reuse: None,
            seconds: 10,
            flaky: false,
            unevaluated: false,
            carried: None,
            coverage: Vec::new(),
        }
    }

    fn reuse(covered: u64, total: u64, measuring: u64) -> Option<Reuse> {
        Some(Reuse {
            covered,
            total,
            measuring,
        })
    }

    const SUMMARY: &str = "soothfast-coverage [{\"dimension\":\"documented\",\"covered\":3,\"total\":4,\"pct\":75,\"min\":80},{\"dimension\":\"measured_declared\",\"covered\":0,\"total\":2,\"pct\":0,\"min\":null}]";

    #[test]
    fn a_coverage_summary_line_becomes_the_records_figures() {
        let out = format!("coverage: documented is 75%, below the minimum 80%\n{SUMMARY}\n");
        let r = build("coverage-a".into(), "check".into(), 1, &out, 2);
        assert_eq!(r.verdict, Verdict::Errored);
        assert_eq!(r.tail, "coverage: documented is 75%, below the minimum 80%");
        let figures: Vec<_> = r
            .coverage
            .iter()
            .map(|f| (f.dimension.as_str(), f.pct, f.min))
            .collect();
        assert_eq!(
            figures,
            [("documented", 75, Some(80)), ("measured_declared", 0, None)]
        );
        assert_eq!(Record::from_json(&r.to_json()), Some(r));
    }

    fn figure(dimension: &str, covered: u64, total: u64, min: Option<u64>) -> Figure {
        Figure {
            dimension: dimension.into(),
            covered,
            total,
            pct: covered * 100 / total,
            min,
            partial: false,
        }
    }

    #[test]
    fn coverage_figures_render_as_a_package_by_dimension_table() {
        let with = |name: &str, coverage: Vec<Figure>| Record {
            coverage,
            kind: "check".into(),
            ..rec(name, Verdict::Pass, "")
        };
        let records = [
            with(
                "coverage-b",
                vec![
                    figure("documented", 3, 4, Some(80)),
                    figure("documented_strict", 1, 4, None),
                ],
            ),
            with("coverage-a", vec![figure("documented", 4, 4, Some(80))]),
            rec("spec-a", Verdict::Pass, ""),
        ];
        let body = render(&records, None);
        let table = "| package | documented | documented_strict |\n| --- | --- | --- |\n| a | 100% (min 80) | n/a |\n| b | 75% (min 80, not met) | 25% |\n";
        assert!(body.contains(table), "{body}");
        assert!(!render(&records[2..], None).contains("| package |"));
    }

    #[test]
    fn a_partial_figure_is_marked_partial() {
        let partial = Figure {
            partial: true,
            ..figure("documented_strict", 1, 4, None)
        };
        assert_eq!(partial.cell(), "25% (partial)");
        assert_eq!(Figure::from_json(&partial.to_json()), Some(partial));
    }

    #[test]
    fn a_dimension_the_record_invents_is_not_a_column() {
        let r = Record {
            coverage: vec![figure("x|y", 1, 1, None)],
            ..rec("coverage-a", Verdict::Pass, "")
        };
        assert!(!render(&[r], None).contains("| package |"));
    }

    #[test]
    fn a_record_with_no_summary_has_no_figures() {
        let r = build("a".into(), "check".into(), 2, "could not read config", 1);
        assert!(r.coverage.is_empty());
        assert!(r.to_json().get("coverage").is_none());
    }

    #[test]
    fn exit_codes_map_to_verdicts() {
        assert_eq!(classify(0, "rust", "ok"), Verdict::Pass);
        assert_eq!(classify(1, "rust", ""), Verdict::Regressed);
        assert_eq!(classify(1, "import", ""), Verdict::Regressed);
        assert_eq!(classify(2, "rust", ""), Verdict::Errored);
        assert_eq!(classify(3, "import", ""), Verdict::Errored);
        assert_eq!(classify(1, "docs", ""), Verdict::Errored);
    }

    #[test]
    fn the_advisory_exit_is_a_check_finding_that_does_not_fail() {
        assert_eq!(
            classify(ADVISORY_EXIT, "check", "BREAKING"),
            Verdict::Advisory
        );
        assert_eq!(classify(ADVISORY_EXIT, "docs", ""), Verdict::Errored);
        let advisory = [rec("a", Verdict::Advisory, "")];
        assert!(!fails(&advisory, false));
        assert!(fails(
            &[advisory[0].clone(), rec("b", Verdict::Errored, "")],
            false
        ));
        assert_eq!(Verdict::parse("advisory"), Some(Verdict::Advisory));
    }

    #[test]
    fn an_advisory_record_is_not_a_pass_a_lane_may_carry() {
        let dir = TempDir::new("advisory-lane");
        let write = |name: &str, code: i32| {
            let out = dir.join(format!("{name}.json"));
            let log = dir.join("none.log");
            write_record(name.into(), "check".into(), code, &log, 1, &out).unwrap();
        };
        write("spec-clean", 0);
        write("spec-found", ADVISORY_EXIT);
        assert_eq!(
            plain_passes(&dir),
            BTreeSet::from(["spec-clean".to_string()])
        );
    }

    #[test]
    fn an_advisory_section_is_counted_apart_and_sorts_after_the_failures() {
        let mut records = vec![
            rec("a", Verdict::Pass, ""),
            rec("b", Verdict::Advisory, "BREAKING  GET /x"),
            rec("c", Verdict::Errored, ""),
        ];
        records.sort_by(|a, b| (a.verdict, &a.name).cmp(&(b.verdict, &b.name)));
        let body = render(&records, None);
        assert!(body.contains("3 projects: 0 regressed, 1 errored, 1 advisory, 1 pass"));
        assert!(body.contains("do not fail the run"));
        assert!(body.contains("### `b`\nadvisory | rust"), "{body}");
        let order = |name: &str| body.find(&format!("### `{name}`")).unwrap();
        assert!(order("c") < order("b") && order("b") < order("a"));
        assert!(!render(&[rec("a", Verdict::Pass, "")], None).contains("advisory"));
    }

    #[test]
    fn a_check_fails_on_any_non_zero_exit_and_is_never_waivable() {
        assert_eq!(classify(0, "check", "ok"), Verdict::Pass);
        assert_eq!(classify(1, "check", ""), Verdict::Errored);
        assert_eq!(classify(2, "check", ""), Verdict::Errored);
        let failed = [build("spec".into(), "check".into(), 1, "drift", 1)];
        assert!(fails(&failed, true));
        assert_eq!(expected("spec:check"), Ok(("spec".into(), "check".into())));
    }

    #[test]
    fn a_notice_in_the_output_means_nothing_to_compare() {
        let out = "merge-base has no benchmark results; nothing to compare";
        assert_eq!(classify(0, "import", out), Verdict::NothingToCompare);
        assert_eq!(classify(2, "import", out), Verdict::Errored);
    }

    #[test]
    fn the_reuse_line_sums_both_sides() {
        let out = "building\ngate: items a stored measurement covers: head 14/16, base 16/16, measuring 2 (2 code changed)\n";
        assert_eq!(parse_reuse(out), reuse(30, 32, 2));
        assert_eq!(parse_reuse("no reuse here"), None);
    }

    #[test]
    fn test_records_read_with_their_own_field_names_and_words() {
        let read = |verdict: &str| {
            Record::from_json(&json!({
                "name": "test-api", "kind": "test", "verdict": verdict,
                "output": "ran", "wall_s": 12.4,
            }))
        };
        let pass = read("pass").unwrap();
        assert_eq!(
            (pass.verdict, pass.tail.as_str(), pass.seconds),
            (Verdict::Pass, "ran", 12)
        );
        for word in ["reused", "skipped"] {
            assert_eq!(read(word).unwrap().verdict, Verdict::Pass);
        }
        let flaky = read("flaky").unwrap();
        assert!(flaky.flaky && flaky.verdict == Verdict::Pass);
        assert!(render(&[flaky], None).contains("pass (flaky) | test | 12s"));
        assert_eq!(read("failed").unwrap().verdict, Verdict::Errored);
        assert_eq!(read("exploded"), None);
    }

    #[test]
    fn escapes_are_stripped_from_recorded_and_stored_tails() {
        let raw = "\x1b[1m\x1b[94mnote\x1b[0m \x1b]8;;http://x\x07link\x1b]8;;\x1b\\ done";
        assert_eq!(strip_ansi(raw), "note link done");
        let r = build("a".into(), "rust".into(), 0, raw, 1);
        assert_eq!(r.tail, "note link done");
        let old = json!({
            "name": "a", "kind": "docs", "verdict": "pass", "tail": raw, "seconds": 1,
        });
        assert_eq!(Record::from_json(&old).unwrap().tail, "note link done");
    }

    #[test]
    fn cargo_warnings_are_dropped_unless_the_job_errored() {
        let out = "building\nwarning: unused dep\n  --> Cargo.toml\n   |\nwarning: `x` generated 1 warning\nchecked";
        let docs = |code| build("a".into(), "docs".into(), code, out, 1);
        assert_eq!(docs(0).tail, "building\nchecked");
        assert_eq!(docs(2).tail, out);
    }

    #[test]
    fn skipped_claims_show_in_the_header_and_count_as_pass() {
        let out = "claims not evaluated: no measurement\n".to_string() + &"x\n".repeat(60);
        let r = build("docs".into(), "docs".into(), 0, &out, 5);
        assert!(r.unevaluated && r.verdict == Verdict::Pass);
        assert_eq!(Record::from_json(&r.to_json()), Some(r.clone()));
        let body = render(&[r], None);
        assert!(body.contains("pass (claims not evaluated) | docs | 5s"));
        assert!(body.contains("1 projects: 0 regressed, 0 errored, 1 pass"));
        assert!(!build("rust".into(), "rust".into(), 0, &out, 5).unevaluated);
    }

    #[test]
    fn the_one_sided_reuse_line_is_read() {
        let out = "gate: 2/3 items run code a stored measurement covers, measuring 1";
        assert_eq!(parse_reuse(out), reuse(2, 3, 1));
    }

    #[test]
    fn a_missing_directory_has_no_records() {
        assert!(read_records(Path::new("/nonexistent/soothfast-results")).is_empty());
    }

    #[test]
    fn a_directory_of_settled_test_results_is_read_whole() {
        let dir =
            std::env::temp_dir().join(format!("soothfast-settled-read-{}", std::process::id()));
        let settled = dir.join("soothfast-result-test-settled");
        fs::create_dir_all(&settled).unwrap();
        for (name, verdict) in [("test-node", "reused"), ("test-python", "skipped")] {
            let record = json!({
                "name": name, "project": "x", "kind": "test", "verdict": verdict,
                "output": "", "wall_s": 0.0,
            });
            fs::write(settled.join(format!("{name}.json")), record.to_string()).unwrap();
        }
        let records = read_records(&dir);
        fs::remove_dir_all(&dir).unwrap();
        let seen: Vec<_> = records
            .iter()
            .map(|r| (r.name.as_str(), r.verdict))
            .collect();
        assert_eq!(
            seen,
            [("test-node", Verdict::Pass), ("test-python", Verdict::Pass)]
        );
    }

    #[test]
    fn a_bad_file_becomes_an_errored_section_named_after_it() {
        let dir = std::env::temp_dir().join(format!("soothfast-verdict-{}", std::process::id()));
        fs::create_dir_all(dir.join("soothfast-result-x")).unwrap();
        fs::write(dir.join("soothfast-result-x/result.json"), "{not json").unwrap();
        let records = read_records(&dir);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "soothfast-result-x/result.json");
        assert_eq!(records[0].verdict, Verdict::Errored);
    }

    fn scratch(tag: &str) -> TempDir {
        let dir = TempDir::new(&format!("hostile-{tag}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_name_and_kind_cannot_start_markdown_outside_the_fence() {
        let dir = scratch("names");
        let record = json!({
            "name": "a\n## forged\n[x](http://evil) @someone", "kind": "k\n```\n<b>",
            "verdict": "pass", "tail": "", "seconds": 1,
        });
        fs::write(dir.join("one.json"), record.to_string()).unwrap();
        fs::write(dir.join("two\n# forged.json"), "{}").unwrap();
        let out = dir.join("body.md");
        merge_dir(&dir, &out, None, &[], false).unwrap();
        let body = fs::read_to_string(&out).unwrap();
        assert!(
            !body.lines().any(|l| l.starts_with("##")
                && !l.starts_with("### ")
                && l != "## soothfast verdict"),
            "{body}"
        );
        assert!(
            !body.contains('<') && !body.contains("](") && !body.contains('@'),
            "{body}"
        );
        assert_eq!(
            body.lines().filter(|l| l.starts_with("### ")).count(),
            2,
            "{body}"
        );
    }

    #[test]
    fn a_record_cannot_link_mention_or_leave_its_fence() {
        let dir = scratch("inert");
        let records = [
            json!({
                "name": "www.evil.example", "kind": "https://evil.example", "verdict": "pass",
                "tail": "## forged\n@someone", "seconds": 1, "carried": "\n```",
            }),
            json!({
                "name": "GH-1", "kind": "a/b#2", "verdict": "pass",
                "tail": "t", "seconds": 1, "carried": "@harvey",
            }),
        ];
        for (i, record) in records.iter().enumerate() {
            fs::write(dir.join(format!("{i}.json")), record.to_string()).unwrap();
        }
        let out = dir.join("body.md");
        merge_dir(&dir, &out, None, &[], false).unwrap();
        let body = fs::read_to_string(&out).unwrap();
        assert_eq!(
            body,
            "## soothfast verdict\n2 projects: 0 regressed, 0 errored, 2 pass\n\
             \n### `GH-1`\npass (carried from ??a??e?) | ? | 1s\n```\nt\n```\n\
             \n### `www.evil.example`\npass (carried from ????) | ? | 1s\n```\n## forged\n@someone\n```\n"
        );
    }

    #[test]
    fn reuse_counts_saturate() {
        let shard = |name: &str| Record {
            reuse: reuse(u64::MAX, u64::MAX, u64::MAX),
            ..rec(name, Verdict::Pass, "")
        };
        let folded = fold(vec![shard("core#1/2"), shard("core#2/2")]);
        assert_eq!(folded[0].reuse, reuse(u64::MAX, u64::MAX, u64::MAX));
        let line = format!(
            "{REUSE_LINE}: head {0}/{0}, base {0}/{0}, measuring 1",
            u64::MAX
        );
        assert_eq!(parse_reuse(&line), reuse(u64::MAX, u64::MAX, 1));
    }

    #[test]
    fn symlinks_and_oversized_files_are_not_followed_or_read() {
        let dir = scratch("links");
        fs::write(dir.join("big.json"), vec![b' '; RECORD_LIMIT as usize + 1]).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", dir.join("file.json")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();
        let records = read_records(&dir);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "big.json");
        assert!(records[0].tail.contains("over the"), "{}", records[0].tail);
    }

    #[test]
    fn deeply_nested_and_wrongly_shaped_json_is_an_errored_section() {
        let dir = scratch("deep");
        fs::write(dir.join("deep.json"), "[".repeat(100_000)).unwrap();
        let shapes = [
            r#"{"name":"x","kind":"rust","verdict":"pass","tail":"","seconds":1e999}"#,
            r#"{"name":1,"kind":[],"verdict":{},"tail":null,"seconds":"s"}"#,
        ];
        for (i, shape) in shapes.iter().enumerate() {
            fs::write(dir.join(format!("shape{i}.json")), shape).unwrap();
        }
        let records = read_records(&dir);
        assert_eq!(records.len(), 3);
        assert!(
            records
                .iter()
                .any(|r| r.name == "deep.json" && r.verdict == Verdict::Errored)
        );
    }

    #[test]
    fn an_expected_result_names_its_kind() {
        assert_eq!(
            expected("docs:docs"),
            Ok(("docs".to_string(), "docs".to_string()))
        );
        assert!(expected("docs").is_err());
        assert!(expected("a:other").is_err());
    }

    #[test]
    fn a_failed_test_run_is_errored_even_on_exit_one() {
        assert_eq!(classify(1, "test", ""), Verdict::Errored);
        let mut r = rec("test-rust", Verdict::Errored, "boom");
        r.kind = "test".into();
        assert_eq!(r.to_json()["verdict"], "failed");
        assert_eq!(Record::from_json(&r.to_json()), Some(r));
    }

    #[test]
    fn folded_tails_are_trimmed_to_forty_lines() {
        let lines = |p: &str| {
            (0..30)
                .map(|i| format!("{p}{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let folded = fold(vec![
            rec("a", Verdict::Pass, &lines("x")),
            rec("a", Verdict::Pass, &lines("y")),
        ]);
        assert_eq!(folded[0].tail.lines().count(), 40);
        assert!(folded[0].tail.ends_with("y29"));
    }

    #[test]
    fn shard_tails_keep_their_shard_and_a_regression_is_not_buried() {
        let folded = fold(vec![
            rec("core#1/2", Verdict::Pass, "fine"),
            rec("core#2/2", Verdict::Regressed, "slower"),
        ]);
        assert_eq!(folded[0].tail, "-- shard 2/2 --\nslower");
        let both = fold(vec![
            rec("core#1/2", Verdict::Pass, "a"),
            rec("core#2/2", Verdict::Pass, "b"),
        ]);
        assert_eq!(both[0].tail, "-- shard 1/2 --\na\n-- shard 2/2 --\nb");
    }

    #[test]
    fn a_worse_record_with_no_output_keeps_the_existing_output() {
        let folded = fold(vec![
            rec("core#1/2", Verdict::Pass, "fine"),
            rec("core#2/2", Verdict::Regressed, ""),
        ]);
        assert_eq!(folded[0].verdict, Verdict::Regressed);
        assert_eq!(folded[0].tail, "-- shard 1/2 --\nfine");
    }

    #[test]
    fn an_oversized_body_loses_passing_output_first() {
        let big = "x".repeat(40_000);
        let records = [
            rec("a", Verdict::Errored, "kept"),
            rec("b", Verdict::Pass, &big),
            rec("c", Verdict::Pass, &big),
        ];
        let body = fit(&records, None);
        assert!(body.chars().count() <= BODY_LIMIT);
        assert!(body.contains("kept") && body.contains(TRUNCATED));
    }

    #[test]
    fn the_tail_keeps_the_last_forty_lines() {
        let out: String = (0..50).map(|i| format!("l{i}\n")).collect();
        let t = tail(&out);
        assert_eq!(t.lines().count(), 40);
        assert!(t.starts_with("l10") && t.ends_with("l49"));
    }

    #[test]
    fn a_record_survives_its_json() {
        let mut r = rec("rust", Verdict::Pass, "t");
        r.reuse = reuse(1, 2, 1);
        assert_eq!(Record::from_json(&r.to_json()), Some(r));
    }

    #[test]
    fn shards_of_one_project_fold_into_one_section() {
        let mut a = rec("core", Verdict::Pass, "a");
        a.reuse = reuse(1, 2, 1);
        let mut b = rec("core", Verdict::Regressed, "b");
        b.reuse = reuse(3, 4, 1);
        b.seconds = 30;
        let folded = fold(vec![a, b, rec("other", Verdict::Pass, "")]);
        assert_eq!(folded.len(), 2);
        let core = &folded[0];
        assert_eq!(core.verdict, Verdict::Regressed);
        assert_eq!(core.tail, "b");
        assert_eq!(core.reuse, reuse(4, 6, 2));
        assert_eq!(core.seconds, 30);
    }

    #[test]
    fn a_shard_that_recorded_nothing_is_an_errored_section() {
        let dir = std::env::temp_dir().join(format!("soothfast-shards-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let record = json!({
            "name": "core#1/2", "kind": "rust", "verdict": "pass", "tail": "ok", "seconds": 3
        });
        fs::write(dir.join("one.json"), record.to_string()).unwrap();
        let out = dir.join("body.md");
        let args: Vec<String> = [
            dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--expect",
            "core#1/2:rust",
            "--expect",
            "core#2/2:rust",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(merge(&args), Ok(1));
        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("core#2/2"), "{body}");
        assert!(!body.contains("core#1/2"), "{body}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn regressed_and_errored_sections_come_first() {
        let mut records = vec![
            rec("a", Verdict::Pass, ""),
            rec("b", Verdict::Regressed, ""),
            rec("c", Verdict::Errored, ""),
            rec("d", Verdict::NothingToCompare, ""),
        ];
        records.sort_by(|a, b| (a.verdict, &a.name).cmp(&(b.verdict, &b.name)));
        let body = render(&records, None);
        let pos = |n: &str| body.find(&format!("### `{n}`\n")).unwrap();
        assert!(pos("c") < pos("b") && pos("b") < pos("a") && pos("a") < pos("d"));
        assert!(body.contains("4 projects: 1 regressed, 1 errored, 2 pass"));
    }

    #[test]
    fn fences_in_the_output_cannot_close_the_block() {
        let body = render(&[rec("a", Verdict::Pass, "x\n```\n@everyone")], None);
        assert_eq!(body.matches("```").count(), 2);
    }

    #[test]
    fn the_comment_names_the_label_that_waived() {
        let reg = [rec("a", Verdict::Regressed, "")];
        let body = render(&reg, Some("ship-it"));
        assert!(body.contains("Regressions accepted by the `ship-it` label."));
        assert!(!render(&[rec("a", Verdict::Pass, "")], Some("ship-it")).contains("accepted by"));
    }

    #[test]
    fn only_regressions_are_waived() {
        let reg = [rec("a", Verdict::Regressed, "")];
        assert!(fails(&reg, false));
        assert!(!fails(&reg, true));
        let both = [
            rec("a", Verdict::Regressed, ""),
            rec("b", Verdict::Errored, ""),
        ];
        assert!(fails(&both, true));
        assert!(!fails(&[rec("a", Verdict::NothingToCompare, "")], false));
    }

    fn carried_in(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-carried-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_carried(
            "core".into(),
            "rust".into(),
            &"ab".repeat(20),
            &dir.join("core.json"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_carried_record_survives_a_round_trip() {
        let dir = carried_in("json");
        let text = fs::read_to_string(dir.join("core.json")).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        let rec = Record::from_json(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(rec.carried.as_deref(), Some("ab".repeat(20).as_str()));
        assert_eq!((rec.verdict, rec.seconds), (Verdict::Pass, 0));
        assert_eq!(
            rec.tail,
            "inputs unchanged since abababababab passed this lane; nothing ran"
        );
        assert_eq!(Record::from_json(&rec.to_json()), Some(rec));
        assert_eq!(
            Record::from_json(
                &json!({"name": "a", "kind": "rust", "verdict": "pass", "tail": "", "seconds": 1})
            )
            .unwrap()
            .carried,
            None
        );
    }

    #[test]
    fn a_carried_pass_is_shown_with_the_commit_it_came_from() {
        let dir = carried_in("render");
        let body = render(&read_records(&dir), None);
        fs::remove_dir_all(&dir).unwrap();
        assert!(body.contains("### `core`\npass (carried from abababa) | rust | 0s"));
        assert!(body.contains("1 projects: 0 regressed, 0 errored, 1 pass"));
    }

    #[test]
    fn a_carried_record_satisfies_the_expectation_of_its_lane() {
        let dir = carried_in("expect");
        let out = dir.join("comment.md");
        let expect = [("core".to_string(), "rust".to_string())];
        assert_eq!(merge_dir(&dir, &out, None, &expect, false), Ok(0));
        let missing = [("other".to_string(), "rust".to_string())];
        assert_eq!(merge_dir(&dir, &out, None, &missing, false), Ok(1));
        fs::remove_dir_all(&dir).unwrap();
    }
}
