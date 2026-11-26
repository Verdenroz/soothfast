//! `docs stats`: per page, how many generated doc blocks execute and how many
//! only compile, and which pages carry no `bind` or `claim` marker.

use serde_json::json;
use soothfast_docs::gentests::PageCounts;

/// The report for `pages` (path, counts): a JSON document or one line per page
/// followed by a total. Advisory: a flagged page fails nothing.
pub fn render(pages: &[(&str, PageCounts)], as_json: bool) -> String {
    if as_json {
        return json_report(pages);
    }
    let mut out = String::new();
    for (page, c) in pages {
        let flag = if c.unanchored() {
            "  no bind or claim"
        } else {
            ""
        };
        out.push_str(&format!(
            "{page}: {} executed, {} compile-only, {} bind(s), {} claim(s){flag}\n",
            c.executed, c.compile_only, c.binds, c.claims
        ));
    }
    let total = |f: fn(&PageCounts) -> usize| pages.iter().map(|(_, c)| f(c)).sum::<usize>();
    let unanchored = pages.iter().filter(|(_, c)| c.unanchored()).count();
    out.push_str(&format!(
        "docs stats: {} of {} doc block(s) execute; {unanchored} of {} page(s) have no bind or claim\n",
        total(|c| c.executed),
        total(|c| c.executed + c.compile_only),
        pages.len()
    ));
    out
}

fn json_report(pages: &[(&str, PageCounts)]) -> String {
    let pages: Vec<_> = pages
        .iter()
        .map(|(page, c)| {
            json!({
                "page": page,
                "executed": c.executed,
                "compile_only": c.compile_only,
                "binds": c.binds,
                "claims": c.claims,
                "unanchored": c.unanchored(),
            })
        })
        .collect();
    json!({ "pages": pages }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(executed: usize, compile_only: usize, binds: usize, claims: usize) -> PageCounts {
        PageCounts {
            executed,
            compile_only,
            binds,
            claims,
        }
    }

    #[test]
    fn a_page_with_no_markers_is_flagged_and_totals_follow() {
        let pages = [("a.md", counts(1, 3, 0, 0)), ("b.md", counts(2, 0, 1, 1))];
        assert_eq!(
            render(&pages, false),
            "a.md: 1 executed, 3 compile-only, 0 bind(s), 0 claim(s)  no bind or claim\n\
             b.md: 2 executed, 0 compile-only, 1 bind(s), 1 claim(s)\n\
             docs stats: 3 of 6 doc block(s) execute; 1 of 2 page(s) have no bind or claim\n"
        );
    }

    #[test]
    fn json_carries_the_counts_and_the_flag() {
        let pages = [("a.md", counts(0, 2, 0, 0))];
        let v: serde_json::Value = serde_json::from_str(&render(&pages, true)).unwrap();
        assert_eq!(
            v,
            json!({ "pages": [{
                "page": "a.md", "executed": 0, "compile_only": 2,
                "binds": 0, "claims": 0, "unanchored": true
            }] })
        );
    }
}
