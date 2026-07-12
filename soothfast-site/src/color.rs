//! Hand-rolled color math for deriving a full Material-Design-3-shaped role
//! set (base/on/container/on-container, light + dark) from a single seed hex
//! color. No color-science dependency — sRGB/HSL only, consistent with the
//! project's zero-new-dependency policy and `tokens.css`'s existing avoidance
//! of `color-mix()`/`oklch()` for older-browser support.

/// One Material-style color role, resolved to concrete hex values for a
/// single scheme (light or dark).
#[derive(Debug, Clone, PartialEq)]
pub struct RoleTones {
    pub base: String,
    pub on: String,
    pub container: String,
    pub on_container: String,
}

/// A role's light and dark tones together, as derived from one seed color.
#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    pub light: RoleTones,
    pub dark: RoleTones,
}

/// Parse a `#rgb`, `#rrggbb`, or bare `rrggbb`/`rgb` hex string into `(r, g, b)` bytes.
pub fn parse_hex(s: &str) -> Result<(u8, u8, u8), String> {
    let s = s.trim().trim_start_matches('#');
    let expand = |c: char| -> Result<u8, String> {
        let d = c
            .to_digit(16)
            .ok_or_else(|| format!("invalid hex color: {s}"))? as u8;
        Ok(d * 16 + d)
    };
    match s.len() {
        3 => {
            let mut chars = s.chars();
            let r = expand(chars.next().unwrap())?;
            let g = expand(chars.next().unwrap())?;
            let b = expand(chars.next().unwrap())?;
            Ok((r, g, b))
        }
        6 => {
            let byte = |i: usize| -> Result<u8, String> {
                u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("invalid hex color: {s}"))
            };
            Ok((byte(0)?, byte(2)?, byte(4)?))
        }
        _ => Err(format!("hex color must be 3 or 6 digits: {s}")),
    }
}

pub fn to_hex(rgb: (u8, u8, u8)) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb.0, rgb.1, rgb.2)
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance, 0.0 (black) .. 1.0 (white).
pub fn relative_luminance(rgb: (u8, u8, u8)) -> f64 {
    0.2126 * srgb_to_linear(rgb.0) + 0.7152 * srgb_to_linear(rgb.1) + 0.0722 * srgb_to_linear(rgb.2)
}

/// WCAG contrast ratio between two colors, >= 1.0.
pub fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whichever of white/near-black gives the higher contrast against `bg`.
fn readable_on(bg: (u8, u8, u8), dark_ink: (u8, u8, u8)) -> (u8, u8, u8) {
    let white = (255, 255, 255);
    if contrast(white, bg) >= contrast(dark_ink, bg) {
        white
    } else {
        dark_ink
    }
}

fn rgb_to_hsl(rgb: (u8, u8, u8)) -> (f64, f64, f64) {
    let (r, g, b) = (
        rgb.0 as f64 / 255.0,
        rgb.1 as f64 / 255.0,
        rgb.2 as f64 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (h, s, l)
}

fn hue_to_rgb(p: f64, q: f64, t: f64) -> f64 {
    let t = if t < 0.0 {
        t + 1.0
    } else if t > 1.0 {
        t - 1.0
    } else {
        t
    };
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    if s.abs() < 1e-9 {
        let v = (l.clamp(0.0, 1.0) * 255.0).round() as u8;
        return (v, v, v);
    }
    let h = h.rem_euclid(360.0) / 360.0;
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let to_byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (
        to_byte(hue_to_rgb(p, q, h + 1.0 / 3.0)),
        to_byte(hue_to_rgb(p, q, h)),
        to_byte(hue_to_rgb(p, q, h - 1.0 / 3.0)),
    )
}

/// Same hue/saturation, lightness set to an absolute target in `[0, 1]`.
fn with_lightness(rgb: (u8, u8, u8), l: f64) -> (u8, u8, u8) {
    let (h, s, _) = rgb_to_hsl(rgb);
    hsl_to_rgb(h, s, l.clamp(0.0, 1.0))
}

/// Nudge lightness toward a target, used to guarantee text-level contrast
/// against a known background without discarding the seed's hue/chroma.
fn ensure_contrast(
    rgb: (u8, u8, u8),
    bg: (u8, u8, u8),
    min_ratio: f64,
    darken: bool,
) -> (u8, u8, u8) {
    let (h, s, mut l) = rgb_to_hsl(rgb);
    let step = if darken { -0.02 } else { 0.02 };
    for _ in 0..40 {
        let candidate = hsl_to_rgb(h, s, l);
        if contrast(candidate, bg) >= min_ratio {
            return candidate;
        }
        l = (l + step).clamp(0.0, 1.0);
    }
    hsl_to_rgb(h, s, l.clamp(0.0, 1.0))
}

const DARK_INK: (u8, u8, u8) = (0x10, 0x0E, 0x08);

/// Derive a full light+dark Material role from one seed hex color, given the
/// page's light and dark background tones (see `tones_for_scheme` for how a
/// single scheme's tones are computed).
pub fn role_from_seed(seed: (u8, u8, u8), bg_light: (u8, u8, u8), bg_dark: (u8, u8, u8)) -> Role {
    Role {
        light: tones_for_scheme(seed, bg_light, 0.88, true),
        dark: tones_for_scheme(with_lightness(seed, 0.72), bg_dark, 0.78, false),
    }
}

/// One scheme's role tones: `base` is nudged to >= 4.5:1 against `bg` (used
/// inline in prose as link/text color); `container` is a soft tint of `base`
/// toward `bg`; `on_container` starts as `base` itself (the bind-chip
/// convention of accent-on-accent-soft) and is then nudged to >= 4.5:1
/// against `container`, since the tint and base can land close enough in
/// lightness on their own to fail AA. `darken` picks the nudge direction for
/// both `base` and `on_container` in this scheme.
fn tones_for_scheme(
    seed: (u8, u8, u8),
    bg: (u8, u8, u8),
    container_ratio: f64,
    darken: bool,
) -> RoleTones {
    let base = ensure_contrast(seed, bg, 4.5, darken);
    let on = readable_on(base, DARK_INK);
    let container = mix(base, bg, container_ratio);
    let on_container = ensure_contrast(base, container, 4.5, darken);
    RoleTones {
        base: to_hex(base),
        on: to_hex(on),
        container: to_hex(container),
        on_container: to_hex(on_container),
    }
}

/// Linear-RGB mix of `a` toward `b` by `t` (0 = pure `a`, 1 = pure `b`).
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let lerp = |x: u8, y: u8| -> u8 { (x as f64 + (y as f64 - x as f64) * t).round() as u8 };
    (lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

const BG_LIGHT: (u8, u8, u8) = (0xFA, 0xF9, 0xF6);
const BG_DARK: (u8, u8, u8) = (0x12, 0x14, 0x19);

/// Background/surface tones for one scheme, derived from a background seed.
struct GroundTones {
    background: (u8, u8, u8),
    on_background: (u8, u8, u8),
    surface: (u8, u8, u8),
    surface_variant: (u8, u8, u8),
    on_surface_variant: (u8, u8, u8),
    outline: (u8, u8, u8),
}

fn ground_from_seed(seed: (u8, u8, u8), dark_scheme: bool) -> GroundTones {
    let on_background = readable_on(seed, DARK_INK);
    // Surface is a touch further from `on_background` than background is (a
    // faintly "raised" tone); surface-variant a bit further still.
    let (h, s, l) = rgb_to_hsl(seed);
    let step = if dark_scheme { 0.03 } else { -0.03 };
    let surface = hsl_to_rgb(h, s, (l + step).clamp(0.0, 1.0));
    let surface_variant = hsl_to_rgb(h, s, (l + step * 2.0).clamp(0.0, 1.0));
    let on_surface_variant = mix(on_background, seed, 0.35);
    let outline = mix(on_background, seed, 0.62);
    GroundTones {
        background: seed,
        on_background,
        surface,
        surface_variant,
        on_surface_variant,
        outline,
    }
}

fn role_css_lines(role_name: &str, tones: &RoleTones) -> String {
    format!(
        "  --{role_name}: {b};\n  --on-{role_name}: {o};\n  --{role_name}-container: {c};\n  --on-{role_name}-container: {oc};\n",
        b = tones.base,
        o = tones.on,
        c = tones.container,
        oc = tones.on_container,
    )
}

fn ground_css_lines(g: &GroundTones) -> String {
    // `--on-surface` has no field of its own on `GroundTones` — it's always
    // identical to `--on-background`, so it's aliased rather than recomputed.
    format!(
        "  --background: {bg};\n  --on-background: {ob};\n  --surface: {sf};\n  --on-surface: var(--on-background);\n  --surface-variant: {sv};\n  --on-surface-variant: {osv};\n  --outline: {ol};\n  --bg: var(--background);\n  --panel: var(--surface);\n  --ink: var(--on-background);\n  --muted: var(--on-surface-variant);\n  --line-strong: var(--outline);\n",
        bg = to_hex(g.background),
        ob = to_hex(g.on_background),
        sf = to_hex(g.surface),
        sv = to_hex(g.surface_variant),
        osv = to_hex(g.on_surface_variant),
        ol = to_hex(g.outline),
    )
}

/// Legacy `--accent*` variables, pointed at `--primary` so pre-MD3-roles
/// `theme_dir` overrides keep working.
const ACCENT_ALIAS_LINES: &str = "  --accent: var(--primary);\n  --accent-soft: var(--primary-container);\n  --accent-ink: var(--on-primary);\n";

/// Generate a `theme-vars.css` override for whichever `[site.theme]` seeds
/// are set, covering `:root`, the OS-dark media query, and the explicit
/// `[data-theme]` toggle — mirroring `tokens.css`'s own structure so it can
/// simply be linked after it.
pub fn generate_theme_css(
    primary: Option<&str>,
    secondary: Option<&str>,
    tertiary: Option<&str>,
    background: Option<&str>,
) -> Result<String, String> {
    let bg_seed = background.map(parse_hex).transpose()?;
    let bg_light = bg_seed.unwrap_or(BG_LIGHT);
    // A custom light seed still needs a dark counterpart; darken it the
    // same way the built-in dark scheme relates to the light one.
    let bg_dark = bg_seed.map_or(BG_DARK, |s| with_lightness(s, 0.10));

    let mut light = String::from(":root {\n");
    let mut dark = String::new();

    if let Some(seed) = bg_seed {
        light.push_str(&ground_css_lines(&ground_from_seed(seed, false)));
        dark.push_str(&ground_css_lines(&ground_from_seed(
            with_lightness(seed, 0.12),
            true,
        )));
    }
    for (name, seed_hex) in [
        ("primary", primary),
        ("secondary", secondary),
        ("tertiary", tertiary),
    ] {
        if let Some(hex) = seed_hex {
            let role = role_from_seed(parse_hex(hex)?, bg_light, bg_dark);
            light.push_str(&role_css_lines(name, &role.light));
            dark.push_str(&role_css_lines(name, &role.dark));
            if name == "primary" {
                light.push_str(ACCENT_ALIAS_LINES);
                dark.push_str(ACCENT_ALIAS_LINES);
            }
        }
    }
    light.push_str("}\n");

    if dark.is_empty() {
        return Ok(light);
    }
    Ok(format!(
        "{light}\n@media (prefers-color-scheme: dark) {{\n  :root:not([data-theme=\"light\"]) {{\n{dark}  }}\n}}\n\n:root[data-theme=\"dark\"] {{\n{dark}}}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_and_long_hex() {
        assert_eq!(parse_hex("#fff").unwrap(), (255, 255, 255));
        assert_eq!(parse_hex("#BF360C").unwrap(), (0xBF, 0x36, 0x0C));
        assert_eq!(parse_hex("bf360c").unwrap(), (0xBF, 0x36, 0x0C));
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(parse_hex("#12345").is_err());
        assert!(parse_hex("zzzzzz").is_err());
    }

    #[test]
    fn contrast_of_black_on_white_is_maximal() {
        let ratio = contrast((0, 0, 0), (255, 255, 255));
        assert!((ratio - 21.0).abs() < 0.01);
    }

    #[test]
    fn role_from_seed_meets_text_contrast_both_schemes() {
        let bg_light = parse_hex("#FAF9F6").unwrap();
        let bg_dark = parse_hex("#121419").unwrap();
        let seed = parse_hex("#FF9800").unwrap(); // a light, low-contrast orange
        let role = role_from_seed(seed, bg_light, bg_dark);

        let base_light = parse_hex(&role.light.base).unwrap();
        assert!(contrast(base_light, bg_light) >= 4.5, "{:?}", role.light);

        let base_dark = parse_hex(&role.dark.base).unwrap();
        assert!(contrast(base_dark, bg_dark) >= 4.5, "{:?}", role.dark);

        let on_primary_light = parse_hex(&role.light.on).unwrap();
        assert!(contrast(on_primary_light, base_light) >= 4.5);
        let on_primary_dark = parse_hex(&role.dark.on).unwrap();
        assert!(contrast(on_primary_dark, base_dark) >= 4.5);

        let container_light = parse_hex(&role.light.container).unwrap();
        let on_container_light = parse_hex(&role.light.on_container).unwrap();
        assert!(
            contrast(on_container_light, container_light) >= 4.5,
            "{:?}",
            role.light
        );
        let container_dark = parse_hex(&role.dark.container).unwrap();
        let on_container_dark = parse_hex(&role.dark.on_container).unwrap();
        assert!(
            contrast(on_container_dark, container_dark) >= 4.5,
            "{:?}",
            role.dark
        );
    }

    #[test]
    fn regression_ff5722_seed_keeps_on_container_readable() {
        // The exact mkdocs "deep orange" hex used for finance-query's
        // primary color — its default container tint used to leave
        // on_container at ~4.05:1 against the container background.
        let bg_light = parse_hex("#FAF9F6").unwrap();
        let bg_dark = parse_hex("#121419").unwrap();
        let role = role_from_seed(parse_hex("#FF5722").unwrap(), bg_light, bg_dark);

        let container_light = parse_hex(&role.light.container).unwrap();
        let on_container_light = parse_hex(&role.light.on_container).unwrap();
        assert!(
            contrast(on_container_light, container_light) >= 4.5,
            "{:?}",
            role.light
        );
    }

    #[test]
    fn generate_theme_css_with_no_seeds_is_empty_root() {
        let css = generate_theme_css(None, None, None, None).unwrap();
        assert_eq!(css, ":root {\n}\n");
    }

    #[test]
    fn generate_theme_css_with_primary_only_emits_light_and_dark() {
        let css = generate_theme_css(Some("#BF360C"), None, None, None).unwrap();
        assert!(css.contains("--primary: #BF360C;"));
        assert!(css.contains("--accent: var(--primary);"));
        assert!(css.contains("@media (prefers-color-scheme: dark)"));
        assert!(css.contains(":root[data-theme=\"dark\"]"));
        // Untouched roles are absent — they fall through to tokens.css.
        assert!(!css.contains("--secondary:"));
    }

    #[test]
    fn generate_theme_css_rejects_bad_seed() {
        assert!(generate_theme_css(Some("nope"), None, None, None).is_err());
    }

    #[test]
    fn role_from_seed_preserves_already_accessible_seed() {
        // A seed that's already dark enough shouldn't be needlessly altered.
        let bg_light = parse_hex("#FAF9F6").unwrap();
        let bg_dark = parse_hex("#121419").unwrap();
        let seed = parse_hex("#3A56C5").unwrap();
        let role = role_from_seed(seed, bg_light, bg_dark);
        assert_eq!(role.light.base, "#3A56C5");
    }
}
