//! Built-in plugin: measured evidence. Rewrites `soothfast:claim` /
//! `soothfast:bind` markers into verified chips carrying live baseline
//! numbers, and merges `capture-output` blocks with their recorded output
//! into run panels. The site's whole identity — prose that proves itself —
//! lives here, and it goes through the same plugin seam users get.

use std::collections::BTreeMap;

use serde_json::Value;
use soothfast_docs::{claims, lockfile, markdown};

use crate::highlight;
use crate::plugin::{PageRef, SitePlugin};
use crate::template::escape;

/// Evidence renderer over one baseline + the bind lockfile.
pub struct Evidence {
    baseline: Value,
    binds: lockfile::Binds,
}

impl Evidence {
    /// `baseline` may be `Value::Null`: chips then render as unverified
    /// rather than failing the build (measurement is a separate step).
    pub fn new(baseline: Value, binds: lockfile::Binds) -> Evidence {
        Evidence { baseline, binds }
    }

    /// Parse + evaluate a claim once; both the compact chip and the ledger
    /// row render from this so pass/fail/warn logic lives in one place.
    fn evaluate_claim(&self, expr: &str) -> ClaimEval {
        let parsed = match claims::parse(expr) {
            Ok(c) => c,
            Err(e) => {
                return ClaimEval {
                    state: "fail",
                    mark: "✗",
                    item: expr.to_string(),
                    metric: String::new(),
                    actual: None,
                    raw_actual: None,
                    op: "",
                    bound: String::new(),
                    raw_bound: 0.0,
                    error: Some(format!("bad claim `{expr}`: {e}")),
                };
            }
        };
        let short = parsed
            .item
            .rsplit("::")
            .next()
            .unwrap_or(&parsed.item)
            .to_string();
        let op = match parsed.op {
            claims::Op::Lt => "<",
            claims::Op::Le => "≤",
            claims::Op::Gt => ">",
            claims::Op::Ge => "≥",
        };
        let metric = metric_phrase(&parsed.backend, &parsed.metric);
        let bound = fmt_metric(&parsed.metric, parsed.bound);
        match claims::evaluate(&parsed, &self.baseline) {
            Ok((true, actual)) => ClaimEval {
                state: "pass",
                mark: "✓",
                item: short,
                metric,
                actual: Some(fmt_metric(&parsed.metric, actual)),
                raw_actual: Some(actual),
                op,
                bound,
                raw_bound: parsed.bound,
                error: None,
            },
            Ok((false, actual)) => ClaimEval {
                state: "fail",
                mark: "✗",
                item: short,
                metric,
                actual: Some(fmt_metric(&parsed.metric, actual)),
                raw_actual: Some(actual),
                op,
                bound,
                raw_bound: parsed.bound,
                error: None,
            },
            Err(_) => ClaimEval {
                state: "warn",
                mark: "?",
                item: short,
                metric,
                actual: None,
                raw_actual: None,
                op,
                bound,
                raw_bound: parsed.bound,
                error: None,
            },
        }
    }

    fn claim_chip(&self, expr: &str) -> String {
        let e = self.evaluate_claim(expr);
        if let Some(err) = &e.error {
            return chip("fail", "✗", &[("chip-item", err)], None, expr, "claim");
        }
        let detail = match &e.actual {
            Some(actual) => format!("{} {actual}", e.metric),
            None => format!("{} not yet measured", e.metric),
        };
        let limit = format!("(limit {} {})", e.op, e.bound);
        let segments = [
            ("chip-item", e.item.as_str()),
            ("chip-sep", "—"),
            ("chip-detail", detail.as_str()),
            ("chip-limit", limit.as_str()),
        ];
        let gauge = e
            .raw_actual
            .map(|actual| scale_svg(actual, e.raw_bound, &e.bound));
        chip(e.state, e.mark, &segments, gauge.as_deref(), expr, "claim")
    }

    /// Richer form of a claim, attached to the code block it covers instead
    /// of standing alone: item, metric, measured value and bound all in
    /// their own cells rather than packed into one line of chip text.
    fn claim_row(&self, expr: &str) -> String {
        let e = self.evaluate_claim(expr);
        if let Some(err) = &e.error {
            return format!(
                "<div class=\"ev-row ev-row-fail\"><span class=\"ev-mark\">✗</span>\
<span class=\"ev-item\">{}</span></div>",
                escape(err)
            );
        }
        let actual_html = match &e.actual {
            Some(a) => format!("<span class=\"ev-actual\">{}</span>", escape(a)),
            None => "<span class=\"ev-actual ev-unmeasured\">unmeasured</span>".to_string(),
        };
        format!(
            "<div class=\"ev-row ev-row-{state}\" data-soothfast-claim title=\"{raw}\">\
<span class=\"ev-mark\">{mark}</span><span class=\"ev-item\">{item}</span>\
<span class=\"ev-metric\">{metric}</span>{actual}\
<span class=\"ev-op\">{op}</span><span class=\"ev-bound\">{bound}</span></div>",
            state = e.state,
            raw = escape(expr),
            mark = e.mark,
            item = escape(&e.item),
            metric = escape(&e.metric),
            actual = actual_html,
            op = e.op,
            bound = escape(&e.bound),
        )
    }

    /// The only thing a reader needs from a bind: does this prose still
    /// match the code right now? "Fingerprint" is our implementation detail,
    /// not theirs — CI keeps this row honest regardless of what it's called.
    fn bind_state(&self, item: &str) -> (&'static str, &'static str, &'static str) {
        if self.binds.contains_key(item) {
            ("bind", "⌖", "verified current")
        } else {
            ("warn", "⌖", "not yet verified")
        }
    }

    fn bind_chip(&self, item: &str) -> String {
        let (state, mark, detail) = self.bind_state(item);
        let short = item.rsplit("::").next().unwrap_or(item);
        let segments = [
            ("chip-item", short),
            ("chip-sep", "·"),
            ("chip-detail", detail),
        ];
        chip(state, mark, &segments, None, item, "bind")
    }

    /// Richer form of a bind, attached to the code block it covers.
    fn bind_row(&self, item: &str) -> String {
        let (state, mark, detail) = self.bind_state(item);
        let short = item.rsplit("::").next().unwrap_or(item);
        format!(
            "<div class=\"ev-row ev-row-{state}\" data-soothfast-bind title=\"{}\">\
<span class=\"ev-mark\">{mark}</span><span class=\"ev-item\">{}</span>\
<span class=\"ev-detail\">{}</span></div>",
            escape(item),
            escape(short),
            escape(detail)
        )
    }
}

/// One claim, parsed and evaluated against the baseline.
struct ClaimEval {
    state: &'static str,
    mark: &'static str,
    item: String,
    /// Human phrase for `backend.metric` ("median time", "allocations",
    /// "CPU instructions") — bare metric names like "median" don't say what
    /// they're the median *of*, so the backend has to be folded in.
    metric: String,
    /// Formatted measured value; `None` when there is no baseline to check
    /// against (`state` is then `"warn"`).
    actual: Option<String>,
    /// Raw measured value, for the calibrated-scale geometry — `None` when
    /// unmeasured (nothing to plot).
    raw_actual: Option<f64>,
    op: &'static str,
    bound: String,
    /// Raw bound value, for the calibrated-scale geometry.
    raw_bound: f64,
    /// Set instead of everything else when the claim expression itself
    /// fails to parse.
    error: Option<String>,
}

impl SitePlugin for Evidence {
    fn name(&self) -> &'static str {
        "evidence"
    }

    fn on_page_markdown(&self, page: &PageRef, md: String) -> Result<String, String> {
        let doc = markdown::scan(&md).map_err(|e| format!("{}: {e}", page.src))?;
        // (1-based inclusive line range) → replacement HTML, one line.
        let mut edits: Vec<(usize, usize, String)> = Vec::new();

        // Any rust block tagged `covers=item` (or `covers=item,item,...`
        // when one example demonstrates several measured/bound items) —
        // capture-output or a plain testable example — claims that item's
        // ledger row for itself; the marker then renders nothing standalone.
        let mut covers_index: BTreeMap<&str, usize> = BTreeMap::new();
        for (n, block) in doc.blocks.iter().enumerate() {
            if block.lang == "rust" {
                if let Some(tag) = block.covers_tag() {
                    for item in tag.split(',').map(str::trim) {
                        covers_index.insert(item, n);
                    }
                }
            }
        }
        let mut ledger_rows: BTreeMap<usize, Vec<String>> = BTreeMap::new();

        for cm in &doc.claims {
            let covering = claims::parse(&cm.expr)
                .ok()
                .and_then(|p| covers_index.get(p.item.as_str()).copied());
            match covering {
                Some(n) => ledger_rows
                    .entry(n)
                    .or_default()
                    .push(self.claim_row(&cm.expr)),
                None => edits.push((cm.line, cm.line, self.claim_chip(&cm.expr))),
            }
        }
        for bind in &doc.binds {
            match covers_index.get(bind.item.as_str()).copied() {
                Some(n) => ledger_rows
                    .entry(n)
                    .or_default()
                    .push(self.bind_row(&bind.item)),
                None => edits.push((bind.line, bind.line, self.bind_chip(&bind.item))),
            }
        }

        for (n, block) in doc.blocks.iter().enumerate() {
            if block.is_capture() {
                // Recorded output follows within a blank line, if captured.
                let output = doc.blocks.get(n + 1).filter(|next| {
                    next.lang == "text"
                        && next.has_tag("soothfast-output")
                        && next.line <= block.close_line + 2
                });
                let end = output.map_or(block.close_line, |o| o.close_line);
                let rows = ledger_rows.remove(&n).unwrap_or_default();
                edits.push((
                    block.line,
                    end,
                    run_panel(&block.code, output.map(|o| o.code.as_str()), &rows),
                ));
            } else if block.lang == "rust" && block.covers_tag().is_some() {
                // A plain (non-capturing) block that still names what it
                // covers: same ledger, no recorded-output section to show.
                let rows = ledger_rows.remove(&n).unwrap_or_default();
                edits.push((block.line, block.close_line, code_panel(block, &rows)));
            }
        }

        if edits.is_empty() {
            return Ok(md);
        }

        edits.sort_by_key(|&(start, _, _)| start);
        let lines: Vec<&str> = md.lines().collect();
        let mut out = String::with_capacity(md.len());
        let mut next = 0usize; // 0-based index of the next line to copy
        for (start, end, html) in &edits {
            // A marker inside another edit's consumed range (e.g. between a
            // capture fence and its output fence) is already gone: skip it.
            if start - 1 < next {
                continue;
            }
            for line in &lines[next..start - 1] {
                out.push_str(line);
                out.push('\n');
            }
            // Blank lines around each insert: raw-HTML passthrough must not
            // absorb the chip into adjacent prose (either direction).
            out.push('\n');
            out.push_str(html);
            out.push_str("\n\n");
            next = *end;
        }
        for line in &lines[next..] {
            out.push_str(line);
            out.push('\n');
        }
        Ok(out)
    }
}

/// One chip line, built from ordered (css-class, text) segments — each
/// escaped and wrapped in its own span, so the item name, measured detail,
/// and limit clause can carry distinct visual weight (a gauge reading, not
/// one flat run of text). `gauge` is a pre-built HTML fragment (already
/// escaped/trusted, from `scale_svg`) appended below the text row — `None`
/// for bind chips and error/unmeasured claims, which have nothing to plot.
/// `data-*` attributes carry the raw expression so the dev server (and
/// user JS) can enrich chips without re-parsing prose.
fn chip(
    state: &str,
    mark: &str,
    segments: &[(&str, &str)],
    gauge: Option<&str>,
    raw: &str,
    kind: &str,
) -> String {
    let body: String = segments
        .iter()
        .map(|(class, text)| format!("<span class=\"{class}\">{}</span>", escape(text)))
        .collect();
    let gauge = gauge.unwrap_or_default();
    format!(
        "<div class=\"claimline\"><span class=\"chip chip-{state}\" data-soothfast-{kind} data-state=\"{state}\" title=\"{}\">\
<span class=\"chip-head\"><span class=\"chip-mark\">{mark}</span>{body}</span>{gauge}</span></div>",
        escape(raw),
    )
}

/// A calibrated scale: a rail from 0 to a headroom-padded max, filled to
/// the measured value, with a tick at the limit and a marker dot — the
/// real actual-vs-bound relationship, drawn to scale rather than implied
/// by a pass/fail color alone. Purely decorative geometry layered over
/// numbers already stated in the chip's text row, so it's `aria-hidden`.
fn scale_svg(actual: f64, bound: f64, bound_label: &str) -> String {
    let scale_max = (actual.max(bound) * 1.18).max(1e-9);
    let (x0, x1) = (8.0_f64, 212.0_f64);
    let px = |v: f64| x0 + (v / scale_max).clamp(0.0, 1.0) * (x1 - x0);
    let marker_x = round2(px(actual));
    let limit_x = round2(px(bound));
    let fill_len = round2(marker_x - x0);
    format!(
        "<span class=\"chip-gauge\">\
<svg class=\"chip-scale\" viewBox=\"0 0 220 22\" aria-hidden=\"true\" focusable=\"false\">\
<line x1=\"{x0}\" y1=\"11\" x2=\"{x1}\" y2=\"11\" class=\"chip-scale-rail\"/>\
<line x1=\"{x0}\" y1=\"7\" x2=\"{x0}\" y2=\"15\" class=\"chip-scale-cap\"/>\
<line x1=\"{x1}\" y1=\"7\" x2=\"{x1}\" y2=\"15\" class=\"chip-scale-cap\"/>\
<line x1=\"{limit_x}\" y1=\"3\" x2=\"{limit_x}\" y2=\"19\" class=\"chip-scale-limit\"/>\
<line x1=\"{x0}\" y1=\"11\" x2=\"{marker_x}\" y2=\"11\" class=\"chip-scale-fill\" style=\"--fill-len:{fill_len}\"/>\
<circle cx=\"{marker_x}\" cy=\"11\" r=\"3.2\" class=\"chip-scale-marker\" style=\"--mx:{marker_x}\"/>\
</svg>\
<span class=\"chip-scale-axis\"><span>0</span><span>limit {}</span></span>\
</span>",
        escape(bound_label),
    )
}

/// Runnable block + its recorded output as one instrument panel. Emitted as
/// a single line: raw-HTML passthrough ends at blank lines, and recorded
/// output may contain them (`&#10;` renders as a newline inside `<pre>`).
/// `ledger` holds the rows of any claim/bind this block declared
/// `covers=` for — the code, its real recorded output, and what it's
/// checked against all read as one instrument.
fn run_panel(code: &str, output: Option<&str>, ledger: &[String]) -> String {
    let code_html = highlight::highlight("rust", code).replace('\n', "&#10;");
    // Collapsed by default (a page can carry a hundred captured blocks); no
    // expandable panel when there's nothing recorded to pull down into.
    let out_html = match output.filter(|s| !s.trim().is_empty()) {
        Some(out) => format!(
            "<details class=\"run-out\"><summary class=\"run-out-head\">\
<span class=\"run-out-label\"><svg class=\"run-out-chevron\" width=\"11\" height=\"11\" viewBox=\"0 0 16 16\" fill=\"none\" aria-hidden=\"true\"><path d=\"M5 2.5 L11 8 L5 13.5\" stroke=\"currentColor\" stroke-width=\"1.6\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>recorded output</span>\
<span class=\"run-out-src\">cargo soothfast docs capture</span></summary>\
<pre>{}</pre></details>",
            escape(out.trim_end()).replace('\n', "&#10;"),
        ),
        None if output.is_some() => String::from(
            "<div class=\"run-out run-out-static\"><div class=\"run-out-head\"><span>ran — printed nothing</span>\
<span class=\"run-out-src\">cargo soothfast docs capture</span></div></div>",
        ),
        None => String::from(
            "<div class=\"run-out run-out-static\"><div class=\"run-out-head\"><span>no recorded output</span>\
<span class=\"run-out-src\">run cargo soothfast docs capture</span></div></div>",
        ),
    };
    format!(
        "<figure class=\"code runpanel\"><figcaption class=\"code-bar\">\
<span class=\"code-lang\">rust · runnable</span>\
<button class=\"code-copy\" type=\"button\" data-copy>Copy</button></figcaption>\
<pre><code>{code_html}</code></pre>{out_html}{}</figure>",
        ledger_html(ledger)
    )
}

/// Plain block that names what it covers but isn't a capture-output block:
/// same ledger, no recorded-output section — the block was never wired to
/// print anything, so there is nothing to invite a reader to go capture.
fn code_panel(block: &markdown::CodeBlock, ledger: &[String]) -> String {
    let visible_tags: Vec<&str> = block
        .tags
        .iter()
        .filter(|t| !t.starts_with("covers="))
        .map(String::as_str)
        .collect();
    let label = if visible_tags.is_empty() {
        block.lang.clone()
    } else {
        format!("{} · {}", block.lang, visible_tags.join(" "))
    };
    let code_html = highlight::highlight(&block.lang, &block.code).replace('\n', "&#10;");
    format!(
        "<figure class=\"code\"><figcaption class=\"code-bar\">\
<span class=\"code-lang\">{}</span>\
<button class=\"code-copy\" type=\"button\" data-copy>Copy</button></figcaption>\
<pre><code>{code_html}</code></pre>{}</figure>",
        escape(&label),
        ledger_html(ledger)
    )
}

/// Shared "checked claims" section under a code panel; empty when nothing
/// on the page named this block with `covers=`.
fn ledger_html(ledger: &[String]) -> String {
    if ledger.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"ev-ledger\"><div class=\"ev-ledger-head\">checked claims</div>{}</div>",
            ledger.concat()
        )
    }
}

/// Human units for a metric value ("median_ns" 41700 → "41.7 µs").
fn fmt_metric(metric: &str, v: f64) -> String {
    if metric.ends_with("_ns") || metric == "p99" || metric == "median" {
        return fmt_ns(v);
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return group_thousands(v as i64);
    }
    format!("{v:.1}")
}

/// Round to 2 decimal places — SVG coordinates don't need f64's full
/// precision cluttering the markup.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn fmt_ns(ns: f64) -> String {
    if ns < 1e3 {
        format!("{ns:.0} ns")
    } else if ns < 1e6 {
        format!("{:.1} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.1} ms", ns / 1e6)
    } else {
        format!("{:.2} s", ns / 1e9)
    }
}

fn group_thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Short label for a metric key ("median_ns" → "median").
fn metric_label(metric: &str) -> &str {
    metric.strip_suffix("_ns").unwrap_or(metric)
}

/// Human-readable "what is this a measurement of" phrase for a
/// `backend.metric` pair. A bare metric like "median" doesn't say median
/// *of what* — folding the backend in ("median time", "allocations") is
/// what makes a chip readable without cross-referencing the bench source.
fn metric_phrase(backend: &str, metric: &str) -> String {
    match (backend, metric_label(metric)) {
        ("walltime", "median") => "median time".into(),
        ("walltime", "p99") => "p99 time".into(),
        ("alloc", "allocs") => "allocations".into(),
        ("alloc", "bytes") => "bytes allocated".into(),
        ("perfcnt", "instructions") => "CPU instructions".into(),
        ("asyncexec", "polls") => "poll count".into(),
        ("asyncexec", "wakes") => "wake count".into(),
        (backend, metric) => format!("{backend} {metric}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plugin() -> Evidence {
        let baseline = json!({ "items": {
            "demo::f": { "walltime": { "median_ns": 41_700.0 }, "alloc": { "allocs": 0.0 } }
        }});
        let mut binds = lockfile::Binds::new();
        binds.insert("demo::f".into(), "abc".into());
        Evidence::new(baseline, binds)
    }

    fn page() -> PageRef<'static> {
        PageRef {
            src: "t.md",
            route: "t/",
        }
    }

    #[test]
    fn claim_markers_become_verified_chips() {
        let md = "<!-- soothfast:claim demo::f.walltime.median_ns < 50us -->\nprose\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("chip-pass"));
        assert!(out.contains("41.7 µs"));
        assert!(out.contains("median time"));
        assert!(out.contains("limit &lt; 50.0 µs") || out.contains("limit < 50.0 µs"));
        assert!(out.contains("\nprose\n"));
    }

    #[test]
    fn violated_and_unmeasured_claims_are_marked() {
        let md = "<!-- soothfast:claim demo::f.walltime.median_ns < 1us -->\n\
                  <!-- soothfast:claim demo::g.alloc.allocs <= 0 -->\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("chip-fail"));
        assert!(out.contains("chip-warn"));
        assert!(out.contains("not yet measured"));
    }

    #[test]
    fn bind_markers_show_verification_state() {
        let md = "<!-- soothfast:bind demo::f -->\n<!-- soothfast:bind demo::ghost -->\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("verified current"));
        assert!(out.contains("not yet verified"));
    }

    #[test]
    fn capture_block_merges_with_recorded_output() {
        let md = "```rust capture-output\nprintln!(\"hi\");\n```\n\n\
                  ```text soothfast-output\nhi\nthere\n```\n\ntail\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("runpanel"));
        assert!(out.contains("recorded output"));
        assert!(out.contains("hi&#10;there"));
        assert!(!out.contains("soothfast-output")); // consumed, not duplicated
        assert!(out.contains("\ntail\n"));
        // Panel is a single line so raw-HTML passthrough can't truncate it.
        let panel = out.lines().find(|l| l.contains("runpanel")).unwrap();
        assert!(panel.contains("</figure>"));
    }

    #[test]
    fn capture_without_output_gets_placeholder() {
        let md = "```rust capture-output\nprintln!(\"hi\");\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("no recorded output"));
        assert!(!out.contains("<details"));
    }

    #[test]
    fn capture_with_blank_recorded_output_has_no_expandable_panel() {
        // A block that ran successfully via `docs capture` but printed
        // nothing still gets a `soothfast-output` block spliced in — empty.
        // That must not render as an expandable-but-empty <details>.
        let md = "```rust capture-output\nlet _ = 1;\n```\n\n```text soothfast-output\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("ran — printed nothing"));
        assert!(!out.contains("<details"));
    }

    #[test]
    fn capture_with_real_output_is_an_expandable_details() {
        let md =
            "```rust capture-output\nprintln!(\"hi\");\n```\n\n```text soothfast-output\nhi\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("<details class=\"run-out\">"));
        assert!(out.contains("<summary class=\"run-out-head\">"));
    }

    #[test]
    fn covers_tag_attaches_claim_and_bind_ledger_to_the_block() {
        let md = "<!-- soothfast:bind demo::f -->\n\
                  <!-- soothfast:claim demo::f.walltime.median_ns < 50us -->\n\
                  Some explanatory prose in between, far from the fence.\n\n\
                  ```rust capture-output covers=demo::f\nprintln!(\"hi\");\n```\n\n\
                  ```text soothfast-output\nhi\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();

        // No standalone chip for either marker — both routed into the panel.
        assert!(!out.contains("class=\"claimline\""));

        let panel = out.lines().find(|l| l.contains("runpanel")).unwrap();
        assert!(panel.contains("ev-ledger"));
        assert!(panel.contains("checked claims"));
        assert!(panel.contains("ev-row-pass"));
        assert!(panel.contains("41.7"));
        assert!(panel.contains("ev-row-bind"));
        assert!(panel.contains("verified current"));
        // Still ordered after the recorded output, inside the same figure.
        assert!(panel.find("run-out").unwrap() < panel.find("ev-ledger").unwrap());
        assert!(panel.contains("</figure>"));
    }

    #[test]
    fn covers_tag_with_no_matching_marker_renders_no_ledger() {
        let md = "```rust capture-output covers=demo::nothing\nprintln!(\"hi\");\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(!out.contains("ev-ledger"));
    }

    #[test]
    fn covers_tag_attaches_to_a_plain_non_capture_block_too() {
        // No `capture-output`: a testable example that never prints
        // anything can still carry a ledger — just no run-out section.
        let md = "<!-- soothfast:bind demo::f -->\n\n\
                  ```rust covers=demo::f\nassert_eq!(1 + 1, 2);\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(!out.contains("class=\"claimline\""));
        assert!(out.contains("ev-ledger"));
        assert!(out.contains("ev-row-bind"));
        assert!(!out.contains("run-out"));
        assert!(!out.contains("runpanel"));
        // The internal `covers=` tag never leaks into the visible label.
        assert!(!out.contains("covers="));
        assert!(out.contains("<span class=\"code-lang\">rust</span>"));
    }

    #[test]
    fn covers_tag_accepts_a_comma_separated_list_of_items() {
        // One example can exercise several measured/bound items at once
        // (e.g. a function that calls five sibling metrics) — all of their
        // markers should land in the same ledger, not float separately.
        let md = "<!-- soothfast:claim demo::f.walltime.median_ns < 50us -->\n\
                  <!-- soothfast:bind demo::g -->\n\
                  ```rust covers=demo::f,demo::g\nlet _ = 1;\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(!out.contains("class=\"claimline\""));
        assert!(out.contains("ev-row-pass"));
        assert!(out.contains("ev-row-warn")); // demo::g isn't in the lockfile
        let rows = out.matches("class=\"ev-row").count();
        assert_eq!(rows, 2);
    }

    #[test]
    fn uncovered_markers_keep_rendering_as_standalone_chips() {
        // A `covers=` block for a different item must not swallow this one.
        let md = "<!-- soothfast:claim demo::f.walltime.median_ns < 50us -->\n\
                  ```rust capture-output covers=demo::other\nprintln!(\"hi\");\n```\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("class=\"claimline\""));
        assert!(out.contains("chip-pass"));
    }

    #[test]
    fn marker_inside_capture_range_is_dropped_not_a_panic() {
        // A bind marker between the capture fence and its output fence falls
        // inside the panel's consumed range; it must not reverse the slice.
        let md = "```rust capture-output\nprintln!(\"hi\");\n```\n\
                  <!-- soothfast:bind demo::f -->\n\
                  ```text soothfast-output\nhi\n```\n\ntail\n";
        let out = plugin().on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("runpanel"));
        assert!(out.contains("\ntail\n"));
    }

    #[test]
    fn missing_baseline_renders_claims_as_unmeasured() {
        let p = Evidence::new(Value::Null, lockfile::Binds::new());
        let md = "<!-- soothfast:claim demo::f.alloc.allocs <= 0 -->\n";
        let out = p.on_page_markdown(&page(), md.into()).unwrap();
        assert!(out.contains("not yet measured"));
    }

    #[test]
    fn chip_directly_after_prose_renders_outside_the_paragraph() {
        let md = "Some prose\n<!-- soothfast:claim demo::f.alloc.allocs <= 0 -->\nmore prose\n";
        let rewritten = plugin().on_page_markdown(&page(), md.into()).unwrap();
        let html = crate::md::render(&rewritten, &crate::md::Options::default())
            .unwrap()
            .html;
        assert!(html.contains("chip-pass"));
        assert!(!html.contains("&amp;lt;"), "double-escaped: {html}");
        assert!(html.contains("<p>more prose</p>"), "{html}");
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(fmt_ns(950.0), "950 ns");
        assert_eq!(fmt_ns(41_700.0), "41.7 µs");
        assert_eq!(fmt_ns(3.2e6), "3.2 ms");
        assert_eq!(group_thousands(61_204), "61,204");
        assert_eq!(fmt_metric("allocs", 0.0), "0");
    }
}
