//! Shields.io endpoint-badge JSON (`https://img.shields.io/endpoint?url=...`)
//! and a self-hosted SVG rendering of the same badge, so a README needs no
//! third-party badge service at all.

use serde_json::{Value, json};

/// Raw shields.io endpoint JSON: `{schemaVersion, label, message, color}`.
pub fn badge(label: &str, message: &str, color: &str) -> Value {
    json!({ "schemaVersion": 1, "label": label, "message": message, "color": color })
}

/// Green ≥ 90, yellow ≥ 70, red below.
pub fn coverage_badge(label: &str, pct: u32) -> Value {
    let color = if pct >= 90 {
        "brightgreen"
    } else if pct >= 70 {
        "yellow"
    } else {
        "red"
    };
    badge(label, &format!("{pct}%"), color)
}

/// `None` means no recorded gate verdict — say so rather than guess green.
pub fn gate_badge(passed: Option<bool>) -> Value {
    match passed {
        Some(true) => badge("soothfast gate", "passing", "brightgreen"),
        Some(false) => badge("soothfast gate", "failing", "red"),
        None => badge("soothfast gate", "unknown", "lightgrey"),
    }
}

/// Shields color names → the theme's evidence palette (color is evidence,
/// so the badge speaks the same pass/warn/fail vocabulary as the site).
fn color_hex(name: &str) -> &'static str {
    match name {
        "brightgreen" | "green" => "#1D7A3E",
        "yellow" => "#855D0A",
        "red" => "#B3362A",
        _ => "#5B6270",
    }
}

/// Render endpoint JSON (as produced by the builders above) as a flat SVG
/// badge: dark instrument-panel label segment, evidence-colored message.
pub fn svg_from(value: &Value) -> String {
    let label = value["label"].as_str().unwrap_or("");
    let message = value["message"].as_str().unwrap_or("");
    let color = color_hex(value["color"].as_str().unwrap_or(""));

    // Monospace at 11px: ~6.62px advance. Widths in tenths avoid floats.
    let text_w = |s: &str| s.chars().count() as u32 * 66 / 10;
    let pad = 8u32;
    let (lw, mw) = (text_w(label) + 2 * pad, text_w(message) + 2 * pad);
    let w = lw + mw;
    let font = "ui-monospace,SFMono-Regular,Menlo,Consolas,monospace";

    format!(
        concat!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="20" "##,
            r##"role="img" aria-label="{label}: {message}">"##,
            r##"<title>{label}: {message}</title>"##,
            r##"<clipPath id="r"><rect width="{w}" height="20" rx="3"/></clipPath>"##,
            r##"<g clip-path="url(#r)">"##,
            r##"<rect width="{lw}" height="20" fill="#14171F"/>"##,
            r##"<rect x="{lw}" width="{mw}" height="20" fill="{color}"/>"##,
            r##"</g>"##,
            r##"<g fill="#FFFFFF" font-family="{font}" font-size="11" text-anchor="middle">"##,
            r##"<text x="{lx}" y="14" fill="#D9DEE9">{label}</text>"##,
            r##"<text x="{mx}" y="14">{message}</text>"##,
            r##"</g></svg>"##,
        ),
        w = w,
        lw = lw,
        mw = mw,
        lx = lw / 2,
        mx = lw + mw / 2,
        color = color,
        font = font,
        label = xml_escape(label),
        message = xml_escape(message),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{coverage_badge, gate_badge, svg_from};

    #[test]
    fn svg_carries_label_message_and_evidence_color() {
        let svg = svg_from(&coverage_badge("soothfast docs", 100));
        assert!(svg.starts_with("<svg "));
        assert!(svg.contains(">soothfast docs</text>"));
        assert!(svg.contains(">100%</text>"));
        assert!(svg.contains("#1D7A3E"));
    }

    #[test]
    fn svg_maps_fail_and_unknown_colors() {
        assert!(svg_from(&gate_badge(Some(false))).contains("#B3362A"));
        assert!(svg_from(&gate_badge(None)).contains("#5B6270"));
    }

    #[test]
    fn svg_escapes_markup_in_text() {
        let svg = svg_from(&super::badge("a<b", "x & y", "red"));
        assert!(svg.contains("a&lt;b"));
        assert!(svg.contains("x &amp; y"));
        assert!(!svg.contains("<b"));
    }
}
