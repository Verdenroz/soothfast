//! `cargo soothfast verdict`: one result per gate job (`record`), folded into
//! the single pull request comment and check (`merge`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const TAIL_LINES: usize = 40;
const REUSE_LINE: &str = "items a stored measurement covers";
const NOTHING_TO_COMPARE: &str = "nothing to compare";
const KINDS: [&str; 3] = ["rust", "import", "docs"];

/// Declared worst first: sorting ascending lists the sections to read first,
/// and the minimum of two verdicts is the one that decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Errored,
    Regressed,
    Pass,
    NothingToCompare,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Verdict::Errored => "errored",
            Verdict::Regressed => "regressed",
            Verdict::Pass => "pass",
            Verdict::NothingToCompare => "nothing-to-compare",
        }
    }

    fn parse(name: &str) -> Option<Verdict> {
        [
            Verdict::Errored,
            Verdict::Regressed,
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
}

impl Record {
    fn to_json(&self) -> Value {
        let mut v = json!({
            "name": self.name,
            "kind": self.kind,
            "verdict": self.verdict.name(),
            "tail": self.tail,
            "seconds": self.seconds,
        });
        if let Some(r) = self.reuse {
            v["reuse"] = json!({"covered": r.covered, "total": r.total, "measuring": r.measuring});
        }
        v
    }

    fn from_json(v: &Value) -> Option<Record> {
        let reuse = v.get("reuse").and_then(|r| {
            Some(Reuse {
                covered: r.get("covered")?.as_u64()?,
                total: r.get("total")?.as_u64()?,
                measuring: r.get("measuring")?.as_u64()?,
            })
        });
        Some(Record {
            name: v.get("name")?.as_str()?.to_string(),
            kind: v.get("kind")?.as_str()?.to_string(),
            verdict: Verdict::parse(v.get("verdict")?.as_str()?)?,
            tail: v.get("tail")?.as_str()?.to_string(),
            reuse,
            seconds: v.get("seconds")?.as_u64()?,
        })
    }

    /// Folds another shard of the same project into this one: the worse
    /// verdict wins, tails and reuse add up, and shards run side by side so
    /// the slowest sets the time.
    fn absorb(&mut self, other: Record) {
        self.verdict = self.verdict.min(other.verdict);
        if !other.tail.is_empty() {
            if !self.tail.is_empty() {
                self.tail.push('\n');
            }
            self.tail.push_str(&other.tail);
        }
        self.reuse = match (self.reuse, other.reuse) {
            (Some(a), Some(b)) => Some(Reuse {
                covered: a.covered + b.covered,
                total: a.total + b.total,
                measuring: a.measuring + b.measuring,
            }),
            (a, b) => a.or(b),
        };
        self.seconds = self.seconds.max(other.seconds);
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
    if !KINDS.contains(&kind.as_str()) {
        return Err(format!("--kind is one of {}", KINDS.join(", ")));
    }
    let code: i32 = code
        .ok_or("record needs --exit")?
        .parse()
        .map_err(|_| "--exit is not a number")?;
    let output = output.ok_or("record needs --output")?;
    // A job that died before the gate wrote anything still records an error.
    let output = fs::read_to_string(&output).unwrap_or_default();
    let rec = Record {
        name,
        verdict: classify(code, &kind, &output),
        kind,
        tail: tail(&output),
        reuse: parse_reuse(&output),
        seconds,
    };
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&rec.to_json()).map_err(|e| e.to_string())?;
    fs::write(&out, text).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(0)
}

/// A docs failure is drift, not a perf regression, so it is never one the
/// perf label may waive.
fn classify(code: i32, kind: &str, output: &str) -> Verdict {
    match code {
        0 if output.contains(NOTHING_TO_COMPARE) => Verdict::NothingToCompare,
        0 => Verdict::Pass,
        1 if kind != "docs" => Verdict::Regressed,
        _ => Verdict::Errored,
    }
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

/// Reads `items a stored measurement covers: head 14/16, base 16/16,
/// measuring 2` into one count over both sides.
fn parse_reuse(output: &str) -> Option<Reuse> {
    let line = output.lines().find(|l| l.contains(REUSE_LINE))?;
    let rest = line.split_once(REUSE_LINE)?.1.trim_start_matches(':');
    let mut reuse = Reuse::default();
    for part in rest.split(',').map(str::trim) {
        if let Some(n) = part.strip_prefix("measuring ") {
            reuse.measuring = n.split_whitespace().next()?.parse().ok()?;
        } else {
            let (covered, total) = part.split_once(' ')?.1.split_once('/')?;
            reuse.covered += covered.parse::<u64>().ok()?;
            reuse.total += total.parse::<u64>().ok()?;
        }
    }
    Some(reuse)
}

fn merge(args: &[String]) -> Result<i32, String> {
    let mut dir = None;
    let mut out = None;
    let mut accepted = false;
    let mut expect = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = Some(value(&mut it, a)?),
            "--accepted" => accepted = true,
            "--expect" => expect.push(value(&mut it, a)?),
            other if !other.starts_with("--") && dir.is_none() => dir = Some(PathBuf::from(other)),
            other => return Err(format!("unknown merge arg {other:?}")),
        }
    }
    let dir = dir.ok_or("merge needs a results directory")?;
    let out = out.ok_or("merge needs --out")?;
    let mut records = fold(read_records(&dir)?);
    for name in expect {
        if !records.iter().any(|r| r.name == name) {
            records.push(Record {
                name,
                kind: "rust".into(),
                verdict: Verdict::Errored,
                tail: "the job recorded no result".into(),
                reuse: None,
                seconds: 0,
            });
        }
    }
    records.sort_by(|a, b| (a.verdict, &a.name).cmp(&(b.verdict, &b.name)));
    fs::write(&out, render(&records, accepted)).map_err(|e| format!("{out}: {e}"))?;
    Ok(i32::from(fails(&records, accepted)))
}

fn read_records(dir: &Path) -> Result<Vec<Record>, String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for entry in fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "json") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .iter()
        .map(|f| {
            let text = fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
            let v: Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", f.display()))?;
            Record::from_json(&v).ok_or(format!("{}: not a verdict record", f.display()))
        })
        .collect()
}

fn fold(records: Vec<Record>) -> Vec<Record> {
    let mut by_name: BTreeMap<String, Record> = BTreeMap::new();
    for rec in records {
        match by_name.get_mut(&rec.name) {
            Some(seen) => seen.absorb(rec),
            None => {
                by_name.insert(rec.name.clone(), rec);
            }
        }
    }
    by_name.into_values().collect()
}

/// Regressions fail unless a writer accepted them; an error never waives.
fn fails(records: &[Record], accepted: bool) -> bool {
    records.iter().any(|r| match r.verdict {
        Verdict::Errored => true,
        Verdict::Regressed => !accepted,
        _ => false,
    })
}

fn render(records: &[Record], accepted: bool) -> String {
    let count = |v: Verdict| records.iter().filter(|r| r.verdict == v).count();
    let mut out = format!(
        "## soothfast verdict\n{} projects: {} regressed, {} errored, {} pass\n",
        records.len(),
        count(Verdict::Regressed),
        count(Verdict::Errored),
        count(Verdict::Pass) + count(Verdict::NothingToCompare),
    );
    if accepted && count(Verdict::Regressed) > 0 {
        out.push_str("\nRegressions accepted by the `soothfast:perf-accepted` label.\n");
    }
    for r in records {
        out.push_str(&format!(
            "\n### {}\n{} | {} | {}s",
            r.name,
            r.verdict.name(),
            r.kind,
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

    fn rec(name: &str, verdict: Verdict, tail: &str) -> Record {
        Record {
            name: name.into(),
            kind: "rust".into(),
            verdict,
            tail: tail.into(),
            reuse: None,
            seconds: 10,
        }
    }

    fn reuse(covered: u64, total: u64, measuring: u64) -> Option<Reuse> {
        Some(Reuse {
            covered,
            total,
            measuring,
        })
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
        assert_eq!(core.tail, "a\nb");
        assert_eq!(core.reuse, reuse(4, 6, 2));
        assert_eq!(core.seconds, 30);
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
        let body = render(&records, false);
        let pos = |n: &str| body.find(&format!("### {n}\n")).unwrap();
        assert!(pos("c") < pos("b") && pos("b") < pos("a") && pos("a") < pos("d"));
        assert!(body.contains("4 projects: 1 regressed, 1 errored, 2 pass"));
    }

    #[test]
    fn fences_in_the_output_cannot_close_the_block() {
        let body = render(&[rec("a", Verdict::Pass, "x\n```\n@everyone")], false);
        assert_eq!(body.matches("```").count(), 2);
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
}
