//! TOML palette + true-color downgrade matrix (issue #85).
//!
//! Today the renderer uses crossterm's hardcoded colour choices and only
//! `BorderStyle` is themed. This module ships a declarative palette with a
//! deterministic downgrade path so a single theme file looks consistent on
//! true-colour, 256-colour, and 16-colour terminals.
//!
//! Scope of *this* commit (worktree-isolated):
//! - Pure data + parser. Renderer wiring lives in `render.rs` and is the
//!   parent-side responsibility — kept off-limits per the task brief.
//! - Five built-in themes embedded via `include_str!`.
//! - Hex parsing, contrast helpers, and a 24-bit -> 256/16 quantizer.
//!
//! ```toml
//! [theme]
//! name           = "ezpn-dark"
//! fg             = "#e6e1cf"
//! bg             = "#1f2430"
//! border         = "#5c6370"
//! border_active  = "#73d0ff"
//! status_bg      = "#1c1e26"
//! status_fg      = "#9da5b4"
//! tab_active_bg  = "#73d0ff"
//! tab_active_fg  = "#1c1e26"
//! tab_inactive_fg = "#5c6370"
//! selection      = "#3a3d4a"
//! search_match   = "#ffd866"
//! broadcast_indicator = "#ff6188"
//! copy_mode_indicator = "#a9dc76"
//! ```

use serde::Deserialize;
use std::fmt;

/// 24-bit RGB colour. Lossless source-of-truth — downgrade happens at the
/// last possible moment in the renderer via [`Theme::resolve`] /
/// [`RgbColor::downgrade_to`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl RgbColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#rgb`, `#rrggbb`, or `rrggbb` (with or without leading `#`).
    /// Case-insensitive. Anything else returns `Err`.
    pub fn parse_hex(s: &str) -> Result<Self, ThemeError> {
        let raw = s.trim();
        let stripped = raw.strip_prefix('#').unwrap_or(raw);
        let bytes = stripped.as_bytes();
        match bytes.len() {
            3 => {
                let r = parse_nibble(bytes[0])?;
                let g = parse_nibble(bytes[1])?;
                let b = parse_nibble(bytes[2])?;
                Ok(Self::new(r * 17, g * 17, b * 17))
            }
            6 => Ok(Self::new(
                parse_byte(bytes[0], bytes[1])?,
                parse_byte(bytes[2], bytes[3])?,
                parse_byte(bytes[4], bytes[5])?,
            )),
            _ => Err(ThemeError::BadHex(s.to_string())),
        }
    }

    /// Relative luminance per WCAG 2.1 §1.4.3.
    pub fn relative_luminance(self) -> f64 {
        fn channel(c: u8) -> f64 {
            let v = c as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// WCAG 2.1 contrast ratio. Returns a value in `[1.0, 21.0]`.
    pub fn contrast_ratio(self, other: Self) -> f64 {
        let a = self.relative_luminance();
        let b = other.relative_luminance();
        let (light, dark) = if a > b { (a, b) } else { (b, a) };
        (light + 0.05) / (dark + 0.05)
    }

    /// Downgrade to the requested palette depth.
    ///
    /// - `ColorDepth::TrueColor` returns `Resolved::Rgb`.
    /// - `ColorDepth::Palette256` returns the closest entry of the standard
    ///   xterm 256-colour cube (16..=231) plus the 24-step grayscale ramp
    ///   (232..=255).
    /// - `ColorDepth::Palette16` returns the closest of the 16 ANSI colours.
    pub fn downgrade_to(self, depth: ColorDepth) -> Resolved {
        match depth {
            ColorDepth::TrueColor => Resolved::Rgb(self),
            ColorDepth::Palette256 => Resolved::Indexed(rgb_to_xterm256(self)),
            ColorDepth::Palette16 => Resolved::Indexed(rgb_to_ansi16(self)),
        }
    }
}

impl fmt::Display for RgbColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

fn parse_nibble(c: u8) -> Result<u8, ThemeError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(ThemeError::BadHex(format!("non-hex digit {:?}", c as char))),
    }
}

fn parse_byte(hi: u8, lo: u8) -> Result<u8, ThemeError> {
    Ok(parse_nibble(hi)? * 16 + parse_nibble(lo)?)
}

/// Terminal palette depth, surfaced by callers based on `$COLORTERM`,
/// `$TERM`, or a DA1/CSI 0c probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    /// 24-bit. `$COLORTERM` is `truecolor` or `24bit`, or DA1 reports it.
    TrueColor,
    /// 256-colour palette (8-bit indexed, e.g. `xterm-256color`).
    Palette256,
    /// 16-colour ANSI fallback. The safe minimum.
    Palette16,
}

impl ColorDepth {
    /// Best-effort detection from environment variables. Callers that hold
    /// a live terminal handle should prefer a DA1 probe and pass the result
    /// in directly.
    pub fn detect() -> Self {
        if let Ok(ct) = std::env::var("COLORTERM") {
            let lower = ct.to_ascii_lowercase();
            if lower == "truecolor" || lower == "24bit" {
                return ColorDepth::TrueColor;
            }
        }
        if let Ok(term) = std::env::var("TERM") {
            let lower = term.to_ascii_lowercase();
            if lower.contains("256color") || lower.contains("direct") {
                if lower.contains("direct") {
                    return ColorDepth::TrueColor;
                }
                return ColorDepth::Palette256;
            }
        }
        ColorDepth::Palette16
    }
}

/// A colour after downgrading to a specific terminal capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    Rgb(RgbColor),
    /// Palette index. For `Palette256` this is the full 0..=255 xterm range;
    /// for `Palette16` it's 0..=15 (ANSI).
    Indexed(u8),
}

// ─── Quantizers ───────────────────────────────────────────

/// Map an RGB triplet to the closest xterm 256-colour entry.
///
/// xterm 256 = 16 system colours + 6×6×6 cube (16..=231) + 24-step grayscale
/// ramp (232..=255). We only emit cube + grayscale entries because the
/// system 0..=15 vary by emulator configuration and would defeat the
/// purpose of a deterministic theme.
pub fn rgb_to_xterm256(rgb: RgbColor) -> u8 {
    // Grayscale check first: if r ≈ g ≈ b, the grayscale ramp is closer
    // (it has 24 steps vs the cube's 6, so ~4× the resolution on the
    // achromatic axis).
    let max = rgb.r.max(rgb.g).max(rgb.b) as i32;
    let min = rgb.r.min(rgb.g).min(rgb.b) as i32;
    let chroma = max - min;

    let (cube_idx, cube_dist) = nearest_cube(rgb);
    if chroma < 8 {
        let (gray_idx, gray_dist) = nearest_grayscale(rgb);
        if gray_dist <= cube_dist {
            return gray_idx;
        }
    }
    cube_idx
}

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn nearest_cube(rgb: RgbColor) -> (u8, u32) {
    let r = nearest_cube_axis(rgb.r);
    let g = nearest_cube_axis(rgb.g);
    let b = nearest_cube_axis(rgb.b);
    let idx = 16 + 36 * (r as u8) + 6 * (g as u8) + b as u8;
    let actual = RgbColor::new(CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b]);
    (idx, dist_sq(rgb, actual))
}

fn nearest_cube_axis(v: u8) -> usize {
    let mut best = 0usize;
    let mut best_d = u32::MAX;
    for (i, level) in CUBE_LEVELS.iter().enumerate() {
        let d = (v as i32 - *level as i32).unsigned_abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

fn nearest_grayscale(rgb: RgbColor) -> (u8, u32) {
    // The xterm grayscale ramp is `8 + 10*i` for i in 0..24.
    let avg = ((rgb.r as u32 + rgb.g as u32 + rgb.b as u32) / 3) as i32;
    let mut best_idx = 232u8;
    let mut best_d = u32::MAX;
    for i in 0..24u8 {
        let level = 8 + 10 * i as i32;
        let d = (avg - level).unsigned_abs();
        if d < best_d {
            best_d = d;
            best_idx = 232 + i;
        }
    }
    let level = 8 + 10 * (best_idx - 232);
    let actual = RgbColor::new(level, level, level);
    (best_idx, dist_sq(rgb, actual))
}

/// Map an RGB triplet to the closest of the 16 ANSI colours.
///
/// ANSI doesn't have a fixed mapping (palette 0..=15 vary by emulator) so
/// we use the xterm default values, which match VT100 reference and are
/// the de-facto standard.
pub fn rgb_to_ansi16(rgb: RgbColor) -> u8 {
    const ANSI16: [(u8, u8, u8); 16] = [
        (0, 0, 0),       // 0  black
        (170, 0, 0),     // 1  red
        (0, 170, 0),     // 2  green
        (170, 85, 0),    // 3  yellow (xterm yellow ≠ pure yellow)
        (0, 0, 170),     // 4  blue
        (170, 0, 170),   // 5  magenta
        (0, 170, 170),   // 6  cyan
        (170, 170, 170), // 7  white (light grey)
        (85, 85, 85),    // 8  bright black
        (255, 85, 85),   // 9
        (85, 255, 85),   // 10
        (255, 255, 85),  // 11
        (85, 85, 255),   // 12
        (255, 85, 255),  // 13
        (85, 255, 255),  // 14
        (255, 255, 255), // 15
    ];
    let mut best = 0u8;
    let mut best_d = u32::MAX;
    for (i, (r, g, b)) in ANSI16.iter().enumerate() {
        let d = dist_sq(rgb, RgbColor::new(*r, *g, *b));
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

fn dist_sq(a: RgbColor, b: RgbColor) -> u32 {
    let dr = a.r as i32 - b.r as i32;
    let dg = a.g as i32 - b.g as i32;
    let db = a.b as i32 - b.b as i32;
    (dr * dr + dg * dg + db * db) as u32
}

// ─── Theme schema ─────────────────────────────────────────

/// Resolved palette. All fields are 24-bit RGB; the renderer downgrades at
/// emit time via [`RgbColor::downgrade_to`] using the live `ColorDepth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub fg: RgbColor,
    pub bg: RgbColor,
    pub border: RgbColor,
    pub border_active: RgbColor,
    pub status_bg: RgbColor,
    pub status_fg: RgbColor,
    pub tab_active_bg: RgbColor,
    pub tab_active_fg: RgbColor,
    pub tab_inactive_fg: RgbColor,
    pub selection: RgbColor,
    pub search_match: RgbColor,
    pub broadcast_indicator: RgbColor,
    pub copy_mode_indicator: RgbColor,
}

impl Theme {
    /// Resolve every palette entry to a single colour depth in one pass.
    /// Useful when the renderer caches a per-depth palette.
    pub fn resolve(&self, depth: ColorDepth) -> ResolvedPalette {
        ResolvedPalette {
            fg: self.fg.downgrade_to(depth),
            bg: self.bg.downgrade_to(depth),
            border: self.border.downgrade_to(depth),
            border_active: self.border_active.downgrade_to(depth),
            status_bg: self.status_bg.downgrade_to(depth),
            status_fg: self.status_fg.downgrade_to(depth),
            tab_active_bg: self.tab_active_bg.downgrade_to(depth),
            tab_active_fg: self.tab_active_fg.downgrade_to(depth),
            tab_inactive_fg: self.tab_inactive_fg.downgrade_to(depth),
            selection: self.selection.downgrade_to(depth),
            search_match: self.search_match.downgrade_to(depth),
            broadcast_indicator: self.broadcast_indicator.downgrade_to(depth),
            copy_mode_indicator: self.copy_mode_indicator.downgrade_to(depth),
        }
    }
}

/// Per-depth resolved palette. Cheap to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPalette {
    pub fg: Resolved,
    pub bg: Resolved,
    pub border: Resolved,
    pub border_active: Resolved,
    pub status_bg: Resolved,
    pub status_fg: Resolved,
    pub tab_active_bg: Resolved,
    pub tab_active_fg: Resolved,
    pub tab_inactive_fg: Resolved,
    pub selection: Resolved,
    pub search_match: Resolved,
    pub broadcast_indicator: Resolved,
    pub copy_mode_indicator: Resolved,
}

#[derive(Debug, Deserialize)]
struct RawThemeFile {
    theme: RawTheme,
}

#[derive(Debug, Deserialize)]
struct RawTheme {
    name: String,
    fg: String,
    bg: String,
    border: String,
    border_active: String,
    status_bg: String,
    status_fg: String,
    tab_active_bg: String,
    tab_active_fg: String,
    tab_inactive_fg: String,
    selection: String,
    search_match: String,
    broadcast_indicator: String,
    copy_mode_indicator: String,
}

impl Theme {
    /// Parse a theme TOML document. Expects a `[theme]` section with all
    /// palette fields populated. Unknown fields are tolerated (forward
    /// compatibility); missing fields are a structured error.
    pub fn from_toml(src: &str) -> Result<Self, ThemeError> {
        let raw: RawThemeFile =
            toml::from_str(src).map_err(|e| ThemeError::Toml(e.message().to_string()))?;
        let r = raw.theme;
        Ok(Self {
            name: r.name,
            fg: RgbColor::parse_hex(&r.fg)?,
            bg: RgbColor::parse_hex(&r.bg)?,
            border: RgbColor::parse_hex(&r.border)?,
            border_active: RgbColor::parse_hex(&r.border_active)?,
            status_bg: RgbColor::parse_hex(&r.status_bg)?,
            status_fg: RgbColor::parse_hex(&r.status_fg)?,
            tab_active_bg: RgbColor::parse_hex(&r.tab_active_bg)?,
            tab_active_fg: RgbColor::parse_hex(&r.tab_active_fg)?,
            tab_inactive_fg: RgbColor::parse_hex(&r.tab_inactive_fg)?,
            selection: RgbColor::parse_hex(&r.selection)?,
            search_match: RgbColor::parse_hex(&r.search_match)?,
            broadcast_indicator: RgbColor::parse_hex(&r.broadcast_indicator)?,
            copy_mode_indicator: RgbColor::parse_hex(&r.copy_mode_indicator)?,
        })
    }

    /// Look up a built-in theme by name. Returns `None` for unknown names —
    /// callers should treat this as a structured error and fall back to
    /// the default theme rather than panicking.
    pub fn builtin(name: &str) -> Option<Theme> {
        let src = builtin_source(name)?;
        // Built-ins are version-controlled and tested; a parse failure here
        // is a programmer error, but we never panic — surface as `None` so
        // the caller can fall back gracefully and log.
        Theme::from_toml(src).ok()
    }

    /// Names of every built-in theme, in the canonical order used by docs.
    pub fn builtin_names() -> &'static [&'static str] {
        &[
            "ezpn-dark",
            "ezpn-light",
            "solarized-dark",
            "gruvbox-dark",
            "nord",
        ]
    }

    /// Default theme, used when no `theme = "..."` is set and no inline
    /// `[theme]` section is present in the user config.
    pub fn default_theme() -> Theme {
        // Fall back to a hardcoded copy of `ezpn-dark` so we never panic
        // even if asset embedding ever breaks at compile time.
        Theme::builtin("ezpn-dark").unwrap_or_else(|| Theme {
            name: "ezpn-dark".into(),
            fg: RgbColor::new(0xe6, 0xe1, 0xcf),
            bg: RgbColor::new(0x1f, 0x24, 0x30),
            border: RgbColor::new(0x5c, 0x63, 0x70),
            border_active: RgbColor::new(0x73, 0xd0, 0xff),
            status_bg: RgbColor::new(0x1c, 0x1e, 0x26),
            status_fg: RgbColor::new(0x9d, 0xa5, 0xb4),
            tab_active_bg: RgbColor::new(0x73, 0xd0, 0xff),
            tab_active_fg: RgbColor::new(0x1c, 0x1e, 0x26),
            tab_inactive_fg: RgbColor::new(0x5c, 0x63, 0x70),
            selection: RgbColor::new(0x3a, 0x3d, 0x4a),
            search_match: RgbColor::new(0xff, 0xd8, 0x66),
            broadcast_indicator: RgbColor::new(0xff, 0x61, 0x88),
            copy_mode_indicator: RgbColor::new(0xa9, 0xdc, 0x76),
        })
    }
}

fn builtin_source(name: &str) -> Option<&'static str> {
    match name {
        "ezpn-dark" => Some(include_str!("../assets/themes/ezpn-dark.toml")),
        "ezpn-light" => Some(include_str!("../assets/themes/ezpn-light.toml")),
        "solarized-dark" => Some(include_str!("../assets/themes/solarized-dark.toml")),
        "gruvbox-dark" => Some(include_str!("../assets/themes/gruvbox-dark.toml")),
        "nord" => Some(include_str!("../assets/themes/nord.toml")),
        _ => None,
    }
}

// ─── Errors ───────────────────────────────────────────────

#[derive(Debug)]
pub enum ThemeError {
    BadHex(String),
    Toml(String),
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadHex(s) => write!(f, "invalid hex colour: {s}"),
            Self::Toml(s) => write!(f, "theme TOML error: {s}"),
        }
    }
}

impl std::error::Error for ThemeError {}

// ─── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_digit_hex() {
        let c = RgbColor::parse_hex("#1f2430").unwrap();
        assert_eq!(c, RgbColor::new(0x1f, 0x24, 0x30));
        assert_eq!(RgbColor::parse_hex("1f2430").unwrap(), c);
        assert_eq!(RgbColor::parse_hex("#1F2430").unwrap(), c);
    }

    #[test]
    fn parses_three_digit_hex() {
        // #abc -> #aabbcc
        assert_eq!(
            RgbColor::parse_hex("#abc").unwrap(),
            RgbColor::new(0xaa, 0xbb, 0xcc)
        );
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(matches!(
            RgbColor::parse_hex("#zzz"),
            Err(ThemeError::BadHex(_))
        ));
        assert!(RgbColor::parse_hex("#12345").is_err());
        assert!(RgbColor::parse_hex("").is_err());
    }

    #[test]
    fn truecolor_roundtrips_unchanged() {
        let c = RgbColor::new(0x73, 0xd0, 0xff);
        assert_eq!(c.downgrade_to(ColorDepth::TrueColor), Resolved::Rgb(c));
    }

    #[test]
    fn xterm256_picks_grayscale_for_achromatic_input() {
        // Mid-grey should land on the 24-step grayscale ramp, not the cube.
        let g = RgbColor::new(120, 120, 120);
        match g.downgrade_to(ColorDepth::Palette256) {
            Resolved::Indexed(i) => {
                assert!((232..=255).contains(&i), "expected grayscale ramp, got {i}")
            }
            other => panic!("expected indexed, got {other:?}"),
        }
    }

    #[test]
    fn xterm256_picks_cube_for_chromatic_input() {
        let c = RgbColor::new(0x73, 0xd0, 0xff);
        match c.downgrade_to(ColorDepth::Palette256) {
            Resolved::Indexed(i) => {
                assert!((16..=231).contains(&i), "expected cube colour, got {i}")
            }
            other => panic!("expected indexed, got {other:?}"),
        }
    }

    #[test]
    fn ansi16_maps_pure_red_to_red() {
        let r = RgbColor::new(255, 0, 0);
        match r.downgrade_to(ColorDepth::Palette16) {
            Resolved::Indexed(i) => assert!(i == 1 || i == 9, "got {i}"),
            other => panic!("expected indexed, got {other:?}"),
        }
    }

    #[test]
    fn xterm256_index_of_pure_red_is_cube_corner() {
        // (255,0,0) -> cube axes (5,0,0) -> 16 + 5*36 = 196.
        assert_eq!(rgb_to_xterm256(RgbColor::new(255, 0, 0)), 196);
    }

    #[test]
    fn xterm256_index_of_pure_white_is_cube_corner() {
        // (255,255,255) achromatic — but white sits at the cube corner.
        // On the grayscale ramp, the closest level is 248 (offset 23 = 238)
        // vs cube level 255. Cube is closer; expect 16 + 5*43 = 231.
        assert_eq!(rgb_to_xterm256(RgbColor::new(255, 255, 255)), 231);
    }

    #[test]
    fn contrast_ratio_white_on_black_is_21() {
        let w = RgbColor::new(255, 255, 255);
        let k = RgbColor::new(0, 0, 0);
        let ratio = w.contrast_ratio(k);
        assert!((ratio - 21.0).abs() < 0.01, "expected ~21, got {ratio}");
    }

    #[test]
    fn each_builtin_loads_and_passes_aa_status_contrast() {
        // WCAG AA for normal text: contrast ratio >= 4.5.
        for name in Theme::builtin_names() {
            let t = Theme::builtin(name).unwrap_or_else(|| panic!("missing builtin: {name}"));
            let ratio = t.status_fg.contrast_ratio(t.status_bg);
            assert!(
                ratio >= 4.5,
                "theme {name}: status fg/bg contrast {ratio:.2} < 4.5 (WCAG AA)"
            );
        }
    }

    #[test]
    fn unknown_builtin_returns_none() {
        assert!(Theme::builtin("hot-pink-2099").is_none());
    }

    #[test]
    fn from_toml_rejects_missing_field() {
        // No `selection`.
        let src = r##"
            [theme]
            name = "x"
            fg = "#ffffff"
            bg = "#000000"
            border = "#888888"
            border_active = "#aaaaaa"
            status_bg = "#111111"
            status_fg = "#eeeeee"
            tab_active_bg = "#222222"
            tab_active_fg = "#ffffff"
            tab_inactive_fg = "#777777"
            search_match = "#ffd700"
            broadcast_indicator = "#ff0000"
            copy_mode_indicator = "#00ff00"
        "##;
        assert!(matches!(Theme::from_toml(src), Err(ThemeError::Toml(_))));
    }

    #[test]
    fn from_toml_rejects_bad_hex() {
        let src = r##"
            [theme]
            name = "x"
            fg = "not a colour"
            bg = "#000000"
            border = "#888888"
            border_active = "#aaaaaa"
            status_bg = "#111111"
            status_fg = "#eeeeee"
            tab_active_bg = "#222222"
            tab_active_fg = "#ffffff"
            tab_inactive_fg = "#777777"
            selection = "#333333"
            search_match = "#ffd700"
            broadcast_indicator = "#ff0000"
            copy_mode_indicator = "#00ff00"
        "##;
        assert!(matches!(Theme::from_toml(src), Err(ThemeError::BadHex(_))));
    }

    #[test]
    fn default_theme_never_panics() {
        let t = Theme::default_theme();
        assert_eq!(t.name, "ezpn-dark");
    }

    #[test]
    fn resolve_runs_for_every_depth() {
        let t = Theme::default_theme();
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Palette256,
            ColorDepth::Palette16,
        ] {
            let r = t.resolve(depth);
            // Sanity: at least the bg and fg are populated.
            match (r.fg, r.bg) {
                (
                    Resolved::Rgb(_) | Resolved::Indexed(_),
                    Resolved::Rgb(_) | Resolved::Indexed(_),
                ) => {}
            }
        }
    }
}
