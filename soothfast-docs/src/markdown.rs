//! Line-based markdown scanning: fenced code blocks with info tags, and
//! `<!-- soothfast:... -->` markers. Deliberately not a markdown AST — we need
//! fences and comment markers only (dependency policy: no pulldown-cmark).

/// One fenced code block.
#[derive(Debug, Clone)]
pub struct CodeBlock {
    /// 0-based ordinal among ALL fenced blocks in the file.
    pub index: usize,
    /// First token of the info string ("rust", "text", "console", ...).
    pub lang: String,
    /// Remaining info-string tokens ("no_run", "ignore", "capture-output", ...).
    pub tags: Vec<String>,
    pub code: String,
    /// 1-based line of the opening fence.
    pub line: usize,
    /// 1-based line of the closing fence.
    pub close_line: usize,
    /// Backtick count of the opening fence.
    pub ticks: usize,
}

impl CodeBlock {
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
    /// A rust block that should become a generated test.
    pub fn is_testable_rust(&self) -> bool {
        self.lang == "rust" && !self.has_tag("ignore") && !self.has_tag("capture-output")
    }
    pub fn is_capture(&self) -> bool {
        self.lang == "rust" && self.has_tag("capture-output")
    }
    /// Raw value of a `feature=NAME` tag: the cargo feature the generated test
    /// is gated behind (for blocks exercising feature-gated API). Callers that
    /// want the individual features want [`Self::feature_list`] instead.
    pub fn feature_tag(&self) -> Option<&str> {
        self.tags.iter().find_map(|t| t.strip_prefix("feature="))
    }
    /// Individual features of a `feature=` tag — one, or several
    /// comma-separated with **no spaces** (`feature=risk,polygon`; a space
    /// would split into a separate fence tag) when a block needs more than one
    /// feature to compile. Empty when the block has no `feature=` tag; `scan`
    /// rejects a tag whose value names no feature.
    pub fn feature_list(&self) -> Vec<&str> {
        self.feature_tag()
            .into_iter()
            .flat_map(|raw| raw.split(','))
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .collect()
    }
    /// The `cfg` predicate gating this block, or None when it has no
    /// `feature=` tag. Several features become an `all(...)` — every listed
    /// feature must be on for the block to compile.
    pub fn feature_cfg(&self) -> Option<String> {
        match self.feature_list().as_slice() {
            [] => None,
            [one] => Some(format!("feature = \"{one}\"")),
            many => Some(format!(
                "all({})",
                many.iter()
                    .map(|f| format!("feature = \"{f}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    /// `(name, arg)` from a `mock=NAME` or `mock=NAME(ARG)` tag — the name of
    /// a `#[soothfast::mock_seam]` fn this block calls via
    /// `soothfast::mock::activate(name, arg)`, and its arg (`""` when
    /// omitted). Requires an accompanying `feature=` tag; enforced in `scan`.
    pub fn mock_tag(&self) -> Option<(&str, &str)> {
        let raw = self.tags.iter().find_map(|t| t.strip_prefix("mock="))?;
        match raw.strip_suffix(')').and_then(|s| s.split_once('(')) {
            Some((name, arg)) => Some((name, arg)),
            None => Some((raw, "")),
        }
    }
    /// Raw value of a `covers=path::to::item` tag — one item, or several
    /// comma-separated with **no spaces** (`covers=a::b,a::c`; a space
    /// would split into a separate fence tag) when a single example
    /// demonstrates more than one measured/bound item. A claim or bind for
    /// any listed item renders attached to this block instead of as a
    /// standalone chip. Callers that need the individual items should split
    /// the value on `,` and trim.
    pub fn covers_tag(&self) -> Option<&str> {
        self.tags.iter().find_map(|t| t.strip_prefix("covers="))
    }
}

/// `<!-- soothfast:bind path::to::item -->`
#[derive(Debug, Clone)]
pub struct Bind {
    pub item: String,
    pub line: usize,
}

/// `<!-- soothfast:claim item.backend.metric < value -->`
#[derive(Debug, Clone)]
pub struct ClaimMarker {
    pub expr: String,
    pub line: usize,
}

/// Everything [`scan`] extracts from one markdown file: fenced code blocks
/// plus `soothfast:bind` / `soothfast:claim` markers, in document order.
#[derive(Debug, Default)]
pub struct Doc {
    /// Fenced code blocks, with language and tags.
    pub blocks: Vec<CodeBlock>,
    /// `<!-- soothfast:bind ... -->` markers.
    pub binds: Vec<Bind>,
    /// `<!-- soothfast:claim ... -->` markers.
    pub claims: Vec<ClaimMarker>,
}

/// Scan one markdown text. Closing markers (`<!-- /soothfast:... -->`) are
/// structural for readers and ignored here. Errs on an unclosed fence:
/// dropping it would silently swallow the block and every later marker.
pub fn scan(text: &str) -> Result<Doc, String> {
    let mut doc = Doc::default();
    let mut fence: Option<(usize, String, Vec<String>, String, usize)> = None; // (ticks, lang, tags, code, line)

    for (i, line) in text.lines().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim_start();

        if let Some((ticks, lang, tags, code, open_line)) = fence.take() {
            let closes = trimmed.chars().take_while(|&c| c == '`').count() >= ticks
                && trimmed.trim_end().chars().all(|c| c == '`');
            if closes {
                doc.blocks.push(CodeBlock {
                    index: doc.blocks.len(),
                    lang,
                    tags,
                    code,
                    line: open_line,
                    close_line: lineno,
                    ticks,
                });
            } else {
                let mut code = code;
                code.push_str(line);
                code.push('\n');
                fence = Some((ticks, lang, tags, code, open_line));
            }
            continue;
        }

        let ticks = trimmed.chars().take_while(|&c| c == '`').count();
        if ticks >= 3 {
            let info = trimmed[ticks..].trim();
            let mut tokens = info.split_whitespace().map(str::to_string);
            let lang = tokens.next().unwrap_or_default();
            fence = Some((ticks, lang, tokens.collect(), String::new(), lineno));
            continue;
        }

        // A marker the scanner declines to read is a claim nothing gates, so
        // every failure below is an error rather than a `continue`.
        if let Some(rest) = trimmed.trim_end().strip_prefix("<!-- soothfast:") {
            let body = rest
                .strip_suffix("-->")
                .map(str::trim)
                .ok_or_else(|| format!("unterminated soothfast marker at line {lineno}"))?;
            if let Some(item) = body.strip_prefix("bind ") {
                doc.binds.push(Bind {
                    item: item.trim().to_string(),
                    line: lineno,
                });
            } else if let Some(expr) = body.strip_prefix("claim ") {
                doc.claims.push(ClaimMarker {
                    expr: expr.trim().to_string(),
                    line: lineno,
                });
            } else {
                return Err(format!(
                    "unknown soothfast marker at line {lineno}: expected `bind <item>` or `claim <expr>`"
                ));
            }
        }
    }
    if let Some((_, _, _, _, open_line)) = fence {
        return Err(format!("unclosed code fence opened at line {open_line}"));
    }
    for block in &doc.blocks {
        if block.mock_tag().is_some() && block.feature_tag().is_none() {
            return Err(format!(
                "mock=NAME requires an accompanying feature=NAME tag (block opened at line {})",
                block.line
            ));
        }
        // A tag that names no feature would silently generate an ungated
        // block, which is exactly what the tag was written to prevent.
        if block.feature_tag().is_some() && block.feature_list().is_empty() {
            return Err(format!(
                "feature= tag names no feature (block opened at line {})",
                block.line
            ));
        }
    }
    Ok(doc)
}

/// Info string of the generated output fence (fence length varies).
const OUTPUT_INFO: &str = "text soothfast-output";

/// Replace (or insert) the `text soothfast-output` fence that follows the
/// `nth` capture-output block (0-based among capture blocks). Ok(None) when
/// the capture block wasn't found; Err on malformed markdown.
pub fn splice_output(
    text: &str,
    nth_capture: usize,
    output: &str,
) -> Result<Option<String>, String> {
    // Consuming scan() keeps capture indices in lockstep with gen-tests;
    // a second ad-hoc fence parser here desynced them in the past.
    let doc = scan(text)?;
    let Some(block) = doc
        .blocks
        .iter()
        .filter(|b| b.is_capture())
        .nth(nth_capture)
    else {
        return Ok(None);
    };
    let lines: Vec<&str> = text.lines().collect();
    Ok(Some(splice_after(&lines, block.close_line - 1, output)))
}

fn splice_after(lines: &[&str], close_idx: usize, output: &str) -> String {
    // The fence must outrun any backtick run in the output, or the block
    // would close early and every re-splice would duplicate its tail.
    let max_run = output
        .lines()
        .map(|l| l.trim_start().chars().take_while(|&c| c == '`').count())
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((max_run + 1).max(3));

    let mut out: Vec<String> = lines[..=close_idx].iter().map(|s| s.to_string()).collect();

    // Skip an existing output fence (allowing one blank line between),
    // honoring its exact opening length so removal is exact.
    let mut rest_start = close_idx + 1;
    let mut peek = rest_start;
    if peek < lines.len() && lines[peek].trim().is_empty() {
        peek += 1;
    }
    if peek < lines.len() {
        let t = lines[peek].trim_start();
        let open_ticks = t.chars().take_while(|&c| c == '`').count();
        if open_ticks >= 3 && t[open_ticks..].trim() == OUTPUT_INFO {
            let mut k = peek + 1;
            while k < lines.len() {
                let c = lines[k].trim_start();
                if c.chars().take_while(|&ch| ch == '`').count() >= open_ticks
                    && c.trim_end().chars().all(|ch| ch == '`')
                {
                    break;
                }
                k += 1;
            }
            rest_start = k + 1;
        }
    }

    out.push(String::new());
    out.push(format!("{fence}{OUTPUT_INFO}"));
    for l in output.trim_end().lines() {
        out.push(l.to_string());
    }
    out.push(fence);
    for l in &lines[rest_start.min(lines.len())..] {
        out.push(l.to_string());
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "\
# T

<!-- soothfast:bind demo::sorted -->
prose
<!-- /soothfast:bind -->

```rust
let x = 1;
```

<!-- soothfast:claim demo::f.perfcnt.instructions < 100 -->

```rust no_run
loop {}
```

```rust capture-output
println!(\"hi\");
```

```text
not rust
```
";

    #[test]
    fn scans_blocks_binds_claims() {
        let doc = scan(MD).unwrap();
        assert_eq!(doc.blocks.len(), 4);
        assert_eq!(doc.binds.len(), 1);
        assert_eq!(doc.binds[0].item, "demo::sorted");
        assert_eq!(doc.claims.len(), 1);
        assert!(doc.blocks[0].is_testable_rust());
        assert!(doc.blocks[1].has_tag("no_run"));
        assert!(doc.blocks[2].is_capture());
        assert_eq!(doc.blocks[3].lang, "text");
    }

    #[test]
    fn trailing_whitespace_still_reads_the_marker() {
        let doc = scan("<!-- soothfast:claim demo::f.alloc == 0 -->  \n").unwrap();
        assert_eq!(doc.claims.len(), 1);
        assert_eq!(doc.claims[0].expr, "demo::f.alloc == 0");
    }

    #[test]
    fn scan_errors_on_a_marker_it_cannot_read() {
        assert!(scan("<!-- soothfast:claim demo::f.alloc == 0\n").is_err());
        assert!(scan("<!-- soothfast:clam demo::f.alloc == 0 -->\n").is_err());
    }

    #[test]
    fn splice_inserts_then_replaces() {
        let once = splice_output(MD, 0, "hi").unwrap().unwrap();
        assert!(once.contains("```text soothfast-output\nhi\n```"));
        // Re-splicing with new output replaces, not duplicates.
        let twice = splice_output(&once, 0, "bye").unwrap().unwrap();
        assert_eq!(twice.matches("soothfast-output").count(), 1);
        assert!(twice.contains("bye"));
        assert!(!twice.contains("\nhi\n"));
    }

    #[test]
    fn scan_errors_on_unclosed_fence() {
        let err = scan("# T\n\n```rust\nlet x = 1;\n").unwrap_err();
        assert!(err.contains("line 3"), "{err}");
        // Markers after the open fence must not be silently consumed.
        let err = scan("```text\n<!-- soothfast:bind demo::f -->\n").unwrap_err();
        assert!(err.contains("unclosed"), "{err}");
    }

    #[test]
    fn splice_errors_on_unclosed_fence_instead_of_panicking() {
        let md = "```rust capture-output\nprintln!(\"hi\");\n";
        let err = splice_output(md, 0, "hi").unwrap_err();
        assert!(err.contains("unclosed"), "{err}");
    }

    #[test]
    fn mock_tag_parses_name_and_optional_arg() {
        let md =
            "```rust capture-output feature=test-utils mock=fmp_profile(AAPL)\nlet x = 1;\n```\n";
        let doc = scan(md).unwrap();
        assert_eq!(doc.blocks[0].mock_tag(), Some(("fmp_profile", "AAPL")));

        let md_no_arg =
            "```rust capture-output feature=test-utils mock=fmp_profile\nlet x = 1;\n```\n";
        let doc_no_arg = scan(md_no_arg).unwrap();
        assert_eq!(doc_no_arg.blocks[0].mock_tag(), Some(("fmp_profile", "")));
    }

    #[test]
    fn scan_errors_when_mock_tag_lacks_feature_tag() {
        let md = "```rust capture-output mock=fmp_profile\nlet x = 1;\n```\n";
        let err = scan(md).unwrap_err();
        assert!(err.contains("feature=NAME"), "{err}");
    }

    #[test]
    fn feature_list_splits_on_commas() {
        let doc = scan("```rust feature=risk,polygon\nlet x = 1;\n```\n").unwrap();
        assert_eq!(doc.blocks[0].feature_list(), ["risk", "polygon"]);
        let doc = scan("```rust feature=risk\nlet x = 1;\n```\n").unwrap();
        assert_eq!(doc.blocks[0].feature_list(), ["risk"]);
        assert!(
            scan("```rust\nlet x = 1;\n```\n").unwrap().blocks[0]
                .feature_list()
                .is_empty()
        );
    }

    #[test]
    fn scan_errors_when_feature_tag_names_no_feature() {
        // Silently generating an ungated block would defeat the tag.
        let err = scan("```rust feature=\nlet x = 1;\n```\n").unwrap_err();
        assert!(err.contains("names no feature"), "{err}");
        let err = scan("```rust feature=,,\nlet x = 1;\n```\n").unwrap_err();
        assert!(err.contains("names no feature"), "{err}");
    }

    #[test]
    fn splice_missing_capture_block_is_none() {
        assert!(splice_output("prose only\n", 0, "hi").unwrap().is_none());
    }

    #[test]
    fn splice_is_idempotent_when_output_contains_backtick_fences() {
        let md = "```rust capture-output\nprintln!(\"```\");\n```\n\ntail prose\n";
        let out = "before\n```\nafter";
        let once = splice_output(md, 0, out).unwrap().unwrap();
        // Output containing ``` forces a longer surrounding fence.
        assert!(once.contains("````text soothfast-output\nbefore\n```\nafter\n````"));
        let twice = splice_output(&once, 0, out).unwrap().unwrap();
        assert_eq!(once, twice, "second splice must be a no-op");
        // Replacing with new output removes the old block exactly once.
        let replaced = splice_output(&once, 0, "plain").unwrap().unwrap();
        assert_eq!(replaced.matches("soothfast-output").count(), 1);
        assert_eq!(replaced.matches("tail prose").count(), 1);
        assert!(!replaced.contains("before"));
    }
}
