//! Hand-emitted SVG trend charts from the `.soothfast/trend.jsonl` series:
//! one chart per metric, one polyline per item, normalized to the first
//! point (relative drift is the story, not absolute magnitudes).

use serde_json::Value;

const W: f64 = 720.0;
const H: f64 = 240.0;
const PAD: f64 = 40.0;
const COLORS: &[&str] = &[
    "#4c78a8", "#f58518", "#54a24b", "#e45756", "#72b7b2", "#b279a2",
];

/// (metric key path in baseline items, display name)
pub const METRICS: &[(&[&str; 2], &str)] = &[
    (&["perfcnt", "instructions"], "instructions"),
    (&["walltime", "median_ns"], "walltime_median_ns"),
    (&["alloc", "allocs"], "allocs"),
];

/// Render one metric's chart; None when fewer than 2 points exist.
pub fn render(points: &[Value], key: &[&str; 2], title: &str) -> Option<String> {
    let mut ids: Vec<String> = points
        .iter()
        .flat_map(|p| {
            p["items"]
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect();
    ids.sort();
    ids.dedup();

    let mut series: Vec<(String, Vec<f64>)> = Vec::new();
    for id in ids {
        let values: Vec<f64> = points
            .iter()
            .filter_map(|p| p["items"][&id][key[0]][key[1]].as_f64())
            .collect();
        if values.len() >= 2 && values[0] > 0.0 {
            let normalized = values.iter().map(|v| v / values[0] * 100.0).collect();
            series.push((id, normalized));
        }
    }
    if series.is_empty() {
        return None;
    }

    let max_len = series.iter().map(|(_, v)| v.len()).max()? as f64;
    let (lo, hi) = series
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let (lo, hi) = (lo.min(95.0), hi.max(105.0));
    let x = |i: f64| PAD + i / (max_len - 1.0).max(1.0) * (W - 2.0 * PAD);
    let y = |v: f64| H - PAD - (v - lo) / (hi - lo) * (H - 2.0 * PAD);

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" font-family="monospace" font-size="11">
<text x="{PAD}" y="16" font-size="13">{title} (first point = 100%)</text>
<line x1="{PAD}" y1="{y100}" x2="{x2}" y2="{y100}" stroke="#999" stroke-dasharray="4 3"/>
<text x="4" y="{y100t}" fill="#999">100%</text>
"##,
        y100 = y(100.0),
        y100t = y(100.0) + 4.0,
        x2 = W - PAD,
    );
    for (n, (id, values)) in series.iter().enumerate() {
        let color = COLORS[n % COLORS.len()];
        let pts: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{:.1},{:.1}", x(i as f64), y(*v)))
            .collect();
        svg.push_str(&format!(
            r##"<polyline points="{}" fill="none" stroke="{color}" stroke-width="1.5"/>
<text x="{x2}" y="{ly}" fill="{color}">{short} {last:+.1}%</text>
"##,
            pts.join(" "),
            x2 = W - PAD + 4.0,
            ly = 20.0 + n as f64 * 14.0,
            short = id.rsplit("::").next().unwrap_or(id),
            last = values.last().unwrap() - 100.0,
        ));
    }
    svg.push_str("</svg>\n");
    Some(svg)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn renders_two_point_series() {
        let points = vec![
            json!({ "items": { "demo::f": { "perfcnt": { "instructions": 100 } } } }),
            json!({ "items": { "demo::f": { "perfcnt": { "instructions": 110 } } } }),
        ];
        let svg = super::render(&points, &["perfcnt", "instructions"], "instructions").unwrap();
        assert!(svg.contains("<polyline"));
        assert!(svg.contains("+10.0%"));
    }

    #[test]
    fn single_point_yields_none() {
        let points = vec![json!({ "items": { "demo::f": { "alloc": { "allocs": 1 } } } })];
        assert!(super::render(&points, &["alloc", "allocs"], "allocs").is_none());
    }
}
