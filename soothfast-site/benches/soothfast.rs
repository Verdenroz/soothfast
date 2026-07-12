//! The site engine measured by soothfast: every page of every docs build
//! goes through the markdown renderer, and every fenced block through the
//! highlighter.

use soothfast::{bench, fixture, keep};
use soothfast_site::md::{self, Options};

soothfast::bench_main!();

/// Synthetic docs page: headings, prose, links, and fenced Rust — the mix
/// the real pages under `docs/` are made of.
#[fixture]
fn page_n(n: usize) -> String {
    let mut doc = String::with_capacity(n * 40);
    for i in 0..n {
        match i % 8 {
            0 => doc.push_str("## A section heading\n\n"),
            1 => doc.push_str("Prose with `code`, *emphasis*, and a [link](measuring.md).\n\n"),
            2 => doc.push_str("```rust\n"),
            3 => doc.push_str("fn measured(input: &[u8]) -> u64 { fnv1a(input) }\n"),
            4 => doc.push_str("```\n\n"),
            5 => doc.push_str("- a list item\n- another one\n\n"),
            6 => doc.push_str("> A quoted line.\n\n"),
            _ => doc.push_str("More prose between the interesting blocks.\n\n"),
        }
    }
    doc
}

/// Rust source of roughly `n` tokens, for the highlighter alone.
#[fixture]
fn rust_n(n: usize) -> String {
    let mut code = String::with_capacity(n * 12);
    for i in 0..n {
        code.push_str(&format!(
            "let value_{i}: u64 = compute(&input[{i}]); // a trailing comment\n"
        ));
    }
    code
}

/// The markdown renderer: once per page, on every build and every deploy.
#[bench(
    group = "self",
    setup_sized = page_n,
    sizes(256, 1024, 4096),
    complexity = "n",
    covers = "soothfast_site::md::render"
)]
fn bench_md_render(doc: &str) {
    let opts = Options::default();
    keep(md::render(keep(doc), &opts).expect("synthetic page is in the subset"));
}

/// Build-time syntax highlighting: once per fenced block, and a docs page
/// is mostly fenced blocks.
#[bench(
    group = "self",
    setup_sized = rust_n,
    sizes(128, 512, 2048),
    complexity = "n",
    covers = "soothfast_site::highlight::highlight"
)]
fn bench_highlight(code: &str) {
    keep(soothfast_site::highlight::highlight(
        keep("rust"),
        keep(code),
    ));
}
