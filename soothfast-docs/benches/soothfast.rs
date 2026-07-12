//! The docs engine measured by soothfast: the scanner and claim parser run
//! over every doc file on every `docs check`.

use soothfast::{bench, fixture, keep};

soothfast::bench_main!();

/// Synthetic markdown: prose, fences, and bind/claim markers, n lines.
#[fixture]
fn markdown_n(n: usize) -> String {
    let mut doc = String::with_capacity(n * 32);
    for i in 0..n {
        match i % 8 {
            0 => doc.push_str("<!-- soothfast:bind mylib::sorted -->\n"),
            1 => doc.push_str("Prose describing the bound item in detail.\n"),
            2 => doc.push_str("<!-- /soothfast:bind -->\n"),
            3 => doc.push_str("```rust\n"),
            4 => doc.push_str("let x = 1 + 1;\n"),
            5 => doc.push_str("```\n"),
            _ => doc.push_str("More prose between the interesting blocks.\n"),
        }
    }
    doc
}

/// The markdown scanner every docs command runs over every doc file.
#[bench(
    group = "self",
    setup_sized = markdown_n,
    sizes(512, 2048, 8192),
    complexity = "n",
    covers = "soothfast_docs::markdown::scan"
)]
fn bench_markdown_scan(doc: &str) {
    keep(soothfast_docs::markdown::scan(keep(doc)).expect("synthetic doc is well-formed"));
}

/// Claim-expression parsing (`docs check` runs one per soothfast:claim).
#[bench(group = "self", p99 = "50us", covers = "soothfast_docs::claims::parse")]
fn bench_claim_parse() {
    keep(
        soothfast_docs::claims::parse(keep("mylib::checksum.perfcnt.instructions < 25000"))
            .expect("valid claim"),
    );
}
