//! Hand-emitted SVG trend charts from the `.soothfast/trend.jsonl` series.
//!
//! One chart per metric. The fleet of items renders as a p10-p90 band plus a
//! median line, and only items that drifted past [`MOVER_THRESHOLD_PCT`] get
//! their own colored line, capped at the palette size with the overflow
//! counted in the footer. A flat fleet is the common case, and a gray band
//! that says so beats 176 overlapping polylines that say nothing.

use serde_json::Value;

const W: f64 = 720.0;
const H: f64 = 260.0;
const PAD_L: f64 = 46.0;
const PAD_R: f64 = 190.0;
const PAD_T: f64 = 30.0;
const PAD_B: f64 = 34.0;
/// Mid-gray ink and band tones, legible on both site themes; a static SVG
/// cannot switch palettes with the page.
const INK: &str = "#8a8a8a";
const BAND: &str = "#808080";
/// Categorical slots in fixed order, validated for CVD separation and
/// contrast against both light and dark chart surfaces.
const COLORS: &[&str] = &[
    "#3987e5", "#d95926", "#199e70", "#c98500", "#d55181", "#008300",
];

/// (metric key path in baseline items, display name, mover threshold in
/// percent drift, fleet_relative). Walltime is fleet-relative: runner-speed
/// shifts move every item together, so each series divides out the fleet
/// median before anything is called a mover. Counter metrics are
/// deterministic and compare directly against their first point.
pub const METRICS: &[(&[&str; 2], &str, f64, bool)] = &[
    (&["perfcnt", "instructions"], "instructions", 2.0, false),
    (&["walltime", "median_ns"], "walltime_median_ns", 10.0, true),
    (&["alloc", "allocs"], "allocs", 2.0, false),
];

struct Series {
    id: String,
    values: Vec<Option<f64>>,
    /// How far the last point sits from the first, in percent. Transient
    /// spikes stay in the band; a line is for drift that stuck.
    end_drift: f64,
}

/// Render one metric's chart; None when fewer than 2 points exist.
pub fn render(
    points: &[Value],
    key: &[&str; 2],
    title: &str,
    threshold_pct: f64,
    fleet_relative: bool,
) -> Option<String> {
    if points.len() < 2 {
        return None;
    }
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

    let mut series: Vec<Series> = Vec::new();
    for id in ids {
        let raw: Vec<Option<f64>> = points
            .iter()
            .map(|p| p["items"][&id][key[0]][key[1]].as_f64())
            .collect();
        let Some(first) = raw.iter().flatten().copied().find(|v| *v > 0.0) else {
            continue;
        };
        let values: Vec<Option<f64>> = raw.iter().map(|v| v.map(|v| v / first * 100.0)).collect();
        if values.iter().flatten().count() < 2 {
            continue;
        }
        let end_drift = values
            .iter()
            .flatten()
            .last()
            .map(|v| (v - 100.0).abs())
            .unwrap_or(0.0);
        series.push(Series {
            id,
            values,
            end_drift,
        });
    }
    if series.is_empty() {
        return None;
    }

    let n_points = points.len();
    if fleet_relative {
        let fleet_median: Vec<Option<f64>> = (0..n_points)
            .map(|i| {
                let mut at: Vec<f64> = series.iter().filter_map(|s| s.values[i]).collect();
                if at.is_empty() {
                    return None;
                }
                let mid = (at.len() - 1) / 2;
                Some(*at.select_nth_unstable_by(mid, |a, b| a.total_cmp(b)).1)
            })
            .collect();
        for s in &mut series {
            for (v, m) in s.values.iter_mut().zip(&fleet_median) {
                if let (Some(v), Some(m)) = (v.as_mut(), m)
                    && *m > 0.0
                {
                    *v = *v / m * 100.0;
                }
            }
            s.end_drift = s
                .values
                .iter()
                .flatten()
                .last()
                .map(|v| (v - 100.0).abs())
                .unwrap_or(0.0);
        }
    }
    let quantile = |xs: &mut Vec<f64>, q: f64| -> f64 {
        let idx = ((xs.len() - 1) as f64 * q).round() as usize;
        *xs.select_nth_unstable_by(idx, |a, b| a.total_cmp(b)).1
    };
    let mut band: Vec<(f64, f64, f64)> = Vec::new();
    for i in 0..n_points {
        let mut at: Vec<f64> = series.iter().filter_map(|s| s.values[i]).collect();
        if at.is_empty() {
            continue;
        }
        let (p10, p50, p90) = (
            quantile(&mut at, 0.1),
            quantile(&mut at, 0.5),
            quantile(&mut at, 0.9),
        );
        band.push((p10, p50, p90));
    }

    let mut movers: Vec<&Series> = series
        .iter()
        .filter(|s| s.end_drift >= threshold_pct)
        .collect();
    movers.sort_by(|a, b| b.end_drift.total_cmp(&a.end_drift));
    let total_movers = movers.len();
    movers.truncate(COLORS.len());

    let (lo, hi) = movers
        .iter()
        .flat_map(|s| s.values.iter().flatten().copied())
        .chain(band.iter().flat_map(|(a, _, c)| [*a, *c]))
        .fold((95.0_f64, 105.0_f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let x = |i: f64| PAD_L + i / (n_points as f64 - 1.0).max(1.0) * (W - PAD_L - PAD_R);
    let y = |v: f64| H - PAD_B - (v - lo) / (hi - lo) * (H - PAD_T - PAD_B);

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" font-family="monospace" font-size="11">
<text x="{PAD_L}" y="16" font-size="13" fill="{INK}">{title} ({frame})</text>
<line x1="{PAD_L}" y1="{y100}" x2="{xr}" y2="{y100}" stroke="{INK}" stroke-opacity="0.7" stroke-dasharray="4 3"/>
<text x="4" y="{y100t}" fill="{INK}">100%</text>
"##,
        y100 = y(100.0),
        y100t = y(100.0) + 4.0,
        xr = W - PAD_R,
        frame = if fleet_relative {
            "vs fleet median, first point = 100%"
        } else {
            "first point = 100%"
        },
    );

    let upper: Vec<String> = band
        .iter()
        .enumerate()
        .map(|(i, (_, _, p90))| format!("{:.1},{:.1}", x(i as f64), y(*p90)))
        .collect();
    let lower: Vec<String> = band
        .iter()
        .enumerate()
        .rev()
        .map(|(i, (p10, _, _))| format!("{:.1},{:.1}", x(i as f64), y(*p10)))
        .collect();
    svg.push_str(&format!(
        "<polygon points=\"{} {}\" fill=\"{BAND}\" fill-opacity=\"0.18\"/>\n",
        upper.join(" "),
        lower.join(" "),
    ));
    let median: Vec<String> = band
        .iter()
        .enumerate()
        .map(|(i, (_, p50, _))| format!("{:.1},{:.1}", x(i as f64), y(*p50)))
        .collect();
    svg.push_str(&format!(
        "<polyline points=\"{}\" fill=\"none\" stroke=\"{BAND}\" stroke-width=\"1.5\" stroke-opacity=\"0.8\"/>\n",
        median.join(" "),
    ));

    // Labels sit in the right gutter, ordered by where each line ends so
    // neighbors never overlap.
    let mut label_order: Vec<(usize, f64)> = movers
        .iter()
        .enumerate()
        .map(|(n, s)| {
            (
                n,
                s.values.iter().flatten().last().copied().unwrap_or(100.0),
            )
        })
        .collect();
    label_order.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut label_y: Vec<f64> = vec![0.0; movers.len()];
    let mut next_free = PAD_T;
    for (n, last) in &label_order {
        let ideal = y(*last).max(next_free);
        label_y[*n] = ideal;
        next_free = ideal + 13.0;
    }

    for (n, s) in movers.iter().enumerate() {
        let color = COLORS[n];
        let pts: Vec<String> = s
            .values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.map(|v| format!("{:.1},{:.1}", x(i as f64), y(v))))
            .collect();
        let last = s.values.iter().flatten().last().copied().unwrap_or(100.0);
        let name = s.id.rsplit("::").next().unwrap_or(&s.id);
        let short: String = if name.chars().count() > 17 {
            name.chars().take(16).chain("\u{2026}".chars()).collect()
        } else {
            name.to_string()
        };
        let ly = label_y[n];
        svg.push_str(&format!(
            r##"<polyline points="{}" fill="none" stroke="{color}" stroke-width="2"/>
<line x1="{lx}" y1="{lyl}" x2="{lx2}" y2="{lyl}" stroke="{color}" stroke-width="2"/>
<text x="{tx}" y="{ty}" fill="{INK}" font-size="10">{short} {delta:+.1}%</text>
"##,
            pts.join(" "),
            lx = W - PAD_R + 4.0,
            lx2 = W - PAD_R + 14.0,
            lyl = ly - 3.5,
            tx = W - PAD_R + 18.0,
            ty = ly,
            delta = last - 100.0,
        ));
    }

    let short_sha = |p: &Value| {
        p["commit"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(7)
            .collect::<String>()
    };
    svg.push_str(&format!(
        "<text x=\"{PAD_L}\" y=\"{ay}\" fill=\"{INK}\" font-size=\"10\">{}</text>\n<text x=\"{ax}\" y=\"{ay}\" fill=\"{INK}\" font-size=\"10\" text-anchor=\"end\">{}</text>\n",
        short_sha(&points[0]),
        short_sha(&points[n_points - 1]),
        ax = W - PAD_R,
        ay = H - PAD_B + 14.0,
    ));

    let n_items = series.len();
    let footer = if total_movers == 0 {
        format!(
            "all {n_items} items ended within \u{b1}{threshold_pct:.0}% of their first point; band spans p10-p90"
        )
    } else if total_movers > movers.len() {
        format!(
            "{total_movers} of {n_items} items ended \u{2265}{threshold_pct:.0}% from start; {} largest shown, the rest stay in the p10-p90 band",
            movers.len(),
        )
    } else {
        format!(
            "{total_movers} of {n_items} items ended \u{2265}{threshold_pct:.0}% from start; band spans p10-p90"
        )
    };
    svg.push_str(&format!(
        "<text x=\"{PAD_L}\" y=\"{fy}\" fill=\"{INK}\" font-size=\"10\">{footer}</text>\n",
        fy = H - 6.0,
    ));
    svg.push_str("</svg>\n");
    Some(svg)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    fn point(id_vals: &[(&str, u64)]) -> serde_json::Value {
        let items: serde_json::Map<String, serde_json::Value> = id_vals
            .iter()
            .map(|(id, v)| (id.to_string(), json!({ "perfcnt": { "instructions": v } })))
            .collect();
        json!({ "commit": "abc1234", "items": items })
    }

    #[test]
    fn a_drifting_item_gets_its_own_labeled_line() {
        let points = vec![
            point(&[("demo::f", 100), ("demo::g", 100)]),
            point(&[("demo::f", 110), ("demo::g", 100)]),
        ];
        let svg = super::render(
            &points,
            &["perfcnt", "instructions"],
            "instructions",
            2.0,
            false,
        )
        .unwrap();
        assert!(svg.contains("stroke=\"#3987e5\""), "{svg}");
        assert!(svg.contains("f +10.0%"), "{svg}");
        assert!(svg.contains("1 of 2 items ended"), "{svg}");
    }

    #[test]
    fn a_flat_fleet_is_a_band_and_a_footer_not_lines() {
        let points = vec![
            point(&[("demo::f", 100), ("demo::g", 200)]),
            point(&[("demo::f", 101), ("demo::g", 201)]),
        ];
        let svg = super::render(
            &points,
            &["perfcnt", "instructions"],
            "instructions",
            2.0,
            false,
        )
        .unwrap();
        assert!(!svg.contains("stroke=\"#3987e5\""), "{svg}");
        assert!(svg.contains("all 2 items ended within"), "{svg}");
        assert!(svg.contains("<polygon"), "{svg}");
    }

    #[test]
    fn movers_beyond_the_palette_fold_into_the_band_with_a_count() {
        let first: Vec<(String, u64)> = (0..9).map(|i| (format!("demo::m{i}"), 100)).collect();
        let second: Vec<(String, u64)> = (0..9).map(|i| (format!("demo::m{i}"), 110 + i)).collect();
        fn to_refs(v: &[(String, u64)]) -> Vec<(&str, u64)> {
            v.iter().map(|(s, n)| (s.as_str(), *n)).collect()
        }
        let points = vec![point(&to_refs(&first)), point(&to_refs(&second))];
        let svg = super::render(
            &points,
            &["perfcnt", "instructions"],
            "instructions",
            2.0,
            false,
        )
        .unwrap();
        assert!(svg.contains("9 of 9 items ended"), "{svg}");
        assert!(svg.contains("6 largest shown"), "{svg}");
    }

    #[test]
    fn single_point_yields_none() {
        let points = vec![point(&[("demo::f", 1)])];
        assert!(super::render(&points, &["perfcnt", "instructions"], "x", 2.0, false).is_none());
    }
}

#[cfg(test)]
mod eyeball {
    #[test]
    #[ignore = "dev harness: renders real trend data to files"]
    fn render_real_data() {
        let src = std::env::var("TREND_SRC").unwrap();
        let out = std::env::var("TREND_OUT").unwrap();
        let points: Vec<serde_json::Value> = std::fs::read_to_string(src)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        for (key, name, threshold, rel) in super::METRICS {
            if let Some(svg) = super::render(&points, key, name, *threshold, *rel) {
                std::fs::write(format!("{out}/trend-{name}.svg"), svg).unwrap();
            }
        }
    }
}
