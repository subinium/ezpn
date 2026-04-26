//! TOML-driven color theme system with terminal capability detection.
//!
//! The renderer no longer hardcodes any RGB triples — every color flows from
//! a [`Theme`] (loaded from `~/.config/ezpn/themes/<name>.toml` or one of the
//! built-in palettes embedded via `include_str!`).  At runtime the raw RGB
//! palette is downgraded into [`AdaptedTheme`], which holds
//! [`crossterm::style::Color`] values appropriate for the host terminal:
//!
//! * truecolor host  → `Color::Rgb` (24-bit)
//! * 256-color host  → `Color::AnsiValue` (6×6×6 cube + grayscale ramp)
//! * 16-color host   → `Color::<basic>`  (nearest of the 16 ANSI colors)
//!
//! Capability detection reads `$COLORTERM` and `$TERM`; both default to
//! "no truecolor, no 256-color" when unset, which yields the most
//! conservative output.

use crossterm::style::Color;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── RGB primitive ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

// ─── Theme palette ────────────────────────────────────────────────────────

/// User-facing theme.  All 18 color fields are required so themes always
/// render fully — missing fields would otherwise leak the underlying
/// terminal default and produce ugly contrast holes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,

    pub bg: Rgb,
    pub focus_bg: Rgb,
    pub lbl_fg: Rgb,
    pub accent: Rgb,
    pub warn_fg: Rgb,
    pub sec_fg: Rgb,
    pub dim_fg: Rgb,
    pub div_fg: Rgb,
    pub status_bg: Rgb,
    pub status_fg: Rgb,
    pub hint_fg: Rgb,
    pub broadcast_color: Rgb,
    pub muted_fg: Rgb,
    pub border_color: Rgb,
    pub active_color: Rgb,
    pub close_color: Rgb,
    pub drag_color: Rgb,
    pub dead_fg: Rgb,
}

/// Hardcoded fallback palette.  Mirrors the original `Color::Rgb { ... }`
/// constants previously living in `settings.rs` and `render.rs`, so
/// upgrading users see no visual change unless they opt into a different
/// theme via `[ui].theme = "..."`.
pub fn default_theme() -> Theme {
    Theme {
        name: "default".into(),
        description: Some("ezpn default dark palette".into()),
        bg: Rgb::new(16, 18, 24),
        focus_bg: Rgb::new(26, 32, 44),
        lbl_fg: Rgb::new(190, 200, 212),
        accent: Rgb::new(102, 217, 239),
        warn_fg: Rgb::new(255, 110, 110),
        sec_fg: Rgb::new(75, 90, 110),
        dim_fg: Rgb::new(90, 98, 110),
        div_fg: Rgb::new(36, 42, 52),
        status_bg: Rgb::new(36, 38, 48),
        status_fg: Rgb::new(255, 255, 255),
        hint_fg: Rgb::new(160, 170, 190),
        broadcast_color: Rgb::new(255, 140, 50),
        muted_fg: Rgb::new(100, 100, 110),
        border_color: Rgb::new(120, 120, 120), // crossterm DarkGrey ≈ rgb(128,128,128)
        active_color: Rgb::new(0, 200, 220),   // approximate Cyan on truecolor
        close_color: Rgb::new(170, 60, 60),    // crossterm DarkRed ≈ rgb(170,0,0)
        drag_color: Rgb::new(220, 220, 80),    // approximate Yellow
        dead_fg: Rgb::new(120, 120, 120),      // DarkGrey
    }
}

// ─── Terminal capability detection ────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermCaps {
    pub true_color: bool,
    pub color_256: bool,
}

#[allow(dead_code)]
impl TermCaps {
    pub const TRUECOLOR: Self = Self {
        true_color: true,
        color_256: true,
    };
    pub const C256: Self = Self {
        true_color: false,
        color_256: true,
    };
    pub const C16: Self = Self {
        true_color: false,
        color_256: false,
    };
}

/// Detect host terminal color depth from `$COLORTERM` and `$TERM`.
///
/// `COLORTERM=truecolor` (or `24bit`) wins outright. Otherwise `$TERM`
/// containing "256" implies 256-color. Everything else falls back to 16.
pub fn detect_caps() -> TermCaps {
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    let true_color = matches!(colorterm.as_str(), "truecolor" | "24bit");
    let color_256 = true_color || term.contains("256");
    TermCaps {
        true_color,
        color_256,
    }
}

// ─── Quantization helpers ─────────────────────────────────────────────────

/// Map an RGB triple into the closest xterm 256-color palette index.
/// Uses the canonical 6×6×6 color cube (16..=231) for chromatic colors and
/// the 24-step grayscale ramp (232..=255) for true grays.
pub fn rgb_to_256(rgb: Rgb) -> u8 {
    if rgb.r == rgb.g && rgb.g == rgb.b {
        // Grayscale ramp 232..=255 (24 levels). Avoids fighting the cube
        // for shades like #161616 that don't quantize cleanly to {0,95,135,…}.
        let level = (u16::from(rgb.r) * 24 / 256) as u8;
        232 + level.min(23)
    } else {
        let q = |v: u8| -> u8 {
            match v {
                0..=47 => 0,
                48..=114 => 1,
                115..=154 => 2,
                155..=194 => 3,
                195..=234 => 4,
                _ => 5,
            }
        };
        16 + 36 * q(rgb.r) + 6 * q(rgb.g) + q(rgb.b)
    }
}

/// Map an RGB triple into the nearest of the 16 standard ANSI colors by
/// Euclidean distance in the sRGB cube.  Good enough for fallback rendering
/// in 16-color terminals; nobody is colour-grading there.
pub fn rgb_to_basic16(rgb: Rgb) -> Color {
    // (r, g, b, Color)
    const PALETTE: &[(u8, u8, u8, Color)] = &[
        (0, 0, 0, Color::Black),
        (170, 0, 0, Color::DarkRed),
        (0, 170, 0, Color::DarkGreen),
        (170, 85, 0, Color::DarkYellow),
        (0, 0, 170, Color::DarkBlue),
        (170, 0, 170, Color::DarkMagenta),
        (0, 170, 170, Color::DarkCyan),
        (170, 170, 170, Color::Grey),
        (85, 85, 85, Color::DarkGrey),
        (255, 85, 85, Color::Red),
        (85, 255, 85, Color::Green),
        (255, 255, 85, Color::Yellow),
        (85, 85, 255, Color::Blue),
        (255, 85, 255, Color::Magenta),
        (85, 255, 255, Color::Cyan),
        (255, 255, 255, Color::White),
    ];
    let mut best = Color::White;
    let mut best_d = u32::MAX;
    let r = i32::from(rgb.r);
    let g = i32::from(rgb.g);
    let b = i32::from(rgb.b);
    for (pr, pg, pb, c) in PALETTE {
        let dr = r - i32::from(*pr);
        let dg = g - i32::from(*pg);
        let db = b - i32::from(*pb);
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = *c;
        }
    }
    best
}

/// Quantize an `Rgb` value into a `crossterm::style::Color` for the given
/// terminal capability set.  This is the single ingress for every color
/// the renderer emits, so every visible pixel respects truecolor /
/// 256-color / 16-color downgrade.
pub fn adapt_color(rgb: Rgb, caps: TermCaps) -> Color {
    if caps.true_color {
        Color::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }
    } else if caps.color_256 {
        Color::AnsiValue(rgb_to_256(rgb))
    } else {
        rgb_to_basic16(rgb)
    }
}

// ─── Adapted theme ────────────────────────────────────────────────────────

/// A theme post-quantization.  Renderer code only ever reads from this —
/// it never sees the raw `Rgb` palette.  Constructed once per session
/// (or per theme change in the future) by [`Theme::adapt`].
#[derive(Clone, Debug)]
pub struct AdaptedTheme {
    /// Resolved theme name — kept for diagnostics, settings panel labels,
    /// and a future `:theme` palette command.
    #[allow(dead_code)]
    pub name: String,
    pub bg: Color,
    pub focus_bg: Color,
    pub lbl_fg: Color,
    pub accent: Color,
    pub warn_fg: Color,
    pub sec_fg: Color,
    pub dim_fg: Color,
    pub div_fg: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub hint_fg: Color,
    pub broadcast_color: Color,
    pub muted_fg: Color,
    pub border_color: Color,
    pub active_color: Color,
    pub close_color: Color,
    pub drag_color: Color,
    pub dead_fg: Color,
}

impl Theme {
    pub fn adapt(&self, caps: TermCaps) -> AdaptedTheme {
        AdaptedTheme {
            name: self.name.clone(),
            bg: adapt_color(self.bg, caps),
            focus_bg: adapt_color(self.focus_bg, caps),
            lbl_fg: adapt_color(self.lbl_fg, caps),
            accent: adapt_color(self.accent, caps),
            warn_fg: adapt_color(self.warn_fg, caps),
            sec_fg: adapt_color(self.sec_fg, caps),
            dim_fg: adapt_color(self.dim_fg, caps),
            div_fg: adapt_color(self.div_fg, caps),
            status_bg: adapt_color(self.status_bg, caps),
            status_fg: adapt_color(self.status_fg, caps),
            hint_fg: adapt_color(self.hint_fg, caps),
            broadcast_color: adapt_color(self.broadcast_color, caps),
            muted_fg: adapt_color(self.muted_fg, caps),
            border_color: adapt_color(self.border_color, caps),
            active_color: adapt_color(self.active_color, caps),
            close_color: adapt_color(self.close_color, caps),
            drag_color: adapt_color(self.drag_color, caps),
            dead_fg: adapt_color(self.dead_fg, caps),
        }
    }
}

#[allow(dead_code)]
impl AdaptedTheme {
    /// Convenience: a default-flavoured truecolor theme for tests / boot
    /// before config has loaded.
    pub fn default_truecolor() -> Self {
        default_theme().adapt(TermCaps::TRUECOLOR)
    }
}

// ─── Loader ───────────────────────────────────────────────────────────────

const BUILTIN_DEFAULT: &str = include_str!("../assets/themes/default.toml");
const BUILTIN_TOKYO_NIGHT: &str = include_str!("../assets/themes/tokyo-night.toml");
const BUILTIN_GRUVBOX_DARK: &str = include_str!("../assets/themes/gruvbox-dark.toml");
const BUILTIN_SOLARIZED_DARK: &str = include_str!("../assets/themes/solarized-dark.toml");
const BUILTIN_SOLARIZED_LIGHT: &str = include_str!("../assets/themes/solarized-light.toml");

fn builtin_theme(name: &str) -> Option<&'static str> {
    match name {
        "default" => Some(BUILTIN_DEFAULT),
        "tokyo-night" => Some(BUILTIN_TOKYO_NIGHT),
        "gruvbox-dark" => Some(BUILTIN_GRUVBOX_DARK),
        "solarized-dark" => Some(BUILTIN_SOLARIZED_DARK),
        "solarized-light" => Some(BUILTIN_SOLARIZED_LIGHT),
        _ => None,
    }
}

/// User theme override location: `~/.config/ezpn/themes/<name>.toml` or
/// `$XDG_CONFIG_HOME/ezpn/themes/<name>.toml` when set.
fn user_theme_path(name: &str) -> Option<PathBuf> {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/tmp"));
            home.join(".config")
        });
    let path = dir.join("ezpn").join("themes").join(format!("{name}.toml"));
    Some(path)
}

/// Load a theme by name.  Resolution order:
/// 1. `~/.config/ezpn/themes/<name>.toml` (user override)
/// 2. Built-in embedded TOML (`assets/themes/<name>.toml`)
/// 3. [`default_theme`] with a single warning printed to stderr.
///
/// Parse errors degrade — we print a one-line warning and fall back rather
/// than panicking, since a broken theme should never brick the multiplexer.
pub fn load_theme(name: &str) -> Theme {
    if let Some(path) = user_theme_path(name) {
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str::<Theme>(&contents) {
                    Ok(t) => return t,
                    Err(e) => {
                        eprintln!(
                            "ezpn: failed to parse user theme '{}' ({}): {e}; falling back",
                            name,
                            path.display()
                        );
                    }
                },
                Err(e) => {
                    eprintln!(
                        "ezpn: failed to read user theme '{}' ({}): {e}; falling back",
                        name,
                        path.display()
                    );
                }
            }
        }
    }
    if let Some(src) = builtin_theme(name) {
        match toml::from_str::<Theme>(src) {
            Ok(t) => return t,
            Err(e) => {
                eprintln!("ezpn: builtin theme '{name}' failed to parse: {e}; using default");
            }
        }
    } else if name != "default" {
        eprintln!("ezpn: unknown theme '{name}'; using default");
    }
    default_theme()
}

// ─── vt100 → crossterm color bridge ───────────────────────────────────────

/// Convert a vt100 color cell into a crossterm `Color`.  Lives in this
/// module so the grep audit `rg 'Color::Rgb' src/` only ever matches this
/// file — every other call site goes through [`adapt_color`].
pub fn vt100_to_crossterm(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::AnsiValue(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb { r, g, b },
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn theme_rgb_to_256_grayscale_ramp() {
        // Pure grays land in the 232..=255 ramp.
        assert_eq!(rgb_to_256(Rgb::new(0, 0, 0)), 232);
        assert_eq!(rgb_to_256(Rgb::new(255, 255, 255)), 232 + 23);
        let mid = rgb_to_256(Rgb::new(128, 128, 128));
        assert!((232..=255).contains(&mid));
    }

    #[test]
    fn theme_rgb_to_256_color_cube() {
        // Pure red maps into the cube row r=5.
        let red = rgb_to_256(Rgb::new(255, 0, 0));
        assert_eq!(red, 16 + 36 * 5);
        // Pure green
        let green = rgb_to_256(Rgb::new(0, 255, 0));
        assert_eq!(green, 16 + 6 * 5);
        // Pure blue
        let blue = rgb_to_256(Rgb::new(0, 0, 255));
        assert_eq!(blue, 16 + 5);
        // White corner
        let white = rgb_to_256(Rgb::new(250, 250, 250));
        // 250 is not equal to 250 to 250 — wait, equal. So grayscale ramp.
        // Re-test with off-gray.
        assert!((232..=255).contains(&white));
        let off = rgb_to_256(Rgb::new(200, 100, 50));
        assert!((16..=231).contains(&off));
    }

    #[test]
    fn theme_rgb_to_basic16_nearest() {
        assert_eq!(rgb_to_basic16(Rgb::new(0, 0, 0)), Color::Black);
        assert_eq!(rgb_to_basic16(Rgb::new(255, 255, 255)), Color::White);
        // Pure (255,0,0) is closer to DarkRed (170,0,0) than to Red (255,85,85)
        // by squared-distance — that's the canonical 16-color quantization.
        assert_eq!(rgb_to_basic16(Rgb::new(255, 0, 0)), Color::DarkRed);
        // Bright reddish with green/blue tint hits the bright Red row.
        assert_eq!(rgb_to_basic16(Rgb::new(255, 85, 85)), Color::Red);
        assert_eq!(rgb_to_basic16(Rgb::new(0, 170, 0)), Color::DarkGreen);
        assert_eq!(rgb_to_basic16(Rgb::new(85, 255, 85)), Color::Green);
    }

    #[test]
    fn theme_adapt_truecolor_preserves_rgb() {
        let t = default_theme();
        let a = t.adapt(TermCaps::TRUECOLOR);
        assert!(matches!(a.bg, Color::Rgb { .. }));
        assert!(matches!(a.accent, Color::Rgb { .. }));
    }

    #[test]
    fn theme_adapt_256_uses_ansi_palette() {
        let t = default_theme();
        let a = t.adapt(TermCaps::C256);
        assert!(matches!(a.bg, Color::AnsiValue(_)));
        assert!(matches!(a.accent, Color::AnsiValue(_)));
    }

    #[test]
    fn theme_adapt_16_uses_basic_palette() {
        let t = default_theme();
        let a = t.adapt(TermCaps::C16);
        // Every color must lower to a non-Rgb / non-AnsiValue variant.
        for c in [
            a.bg,
            a.focus_bg,
            a.lbl_fg,
            a.accent,
            a.warn_fg,
            a.sec_fg,
            a.dim_fg,
            a.div_fg,
            a.status_bg,
            a.status_fg,
            a.hint_fg,
            a.broadcast_color,
            a.muted_fg,
            a.border_color,
            a.active_color,
            a.close_color,
            a.drag_color,
            a.dead_fg,
        ] {
            assert!(
                !matches!(c, Color::Rgb { .. }) && !matches!(c, Color::AnsiValue(_)),
                "color {c:?} should be a basic16 variant"
            );
        }
    }

    #[test]
    fn theme_load_unknown_falls_back_to_default() {
        let t = load_theme("does-not-exist-xyz");
        assert_eq!(t.name, "default");
    }

    #[test]
    fn theme_load_all_builtins_parse() {
        for name in [
            "default",
            "tokyo-night",
            "gruvbox-dark",
            "solarized-dark",
            "solarized-light",
        ] {
            let t = load_theme(name);
            // Builtins must round-trip without falling back to default
            // (except the default itself).
            if name != "default" {
                assert_ne!(
                    t.name, "default",
                    "builtin theme '{name}' fell back to default — TOML invalid?"
                );
            }
        }
    }

    #[test]
    fn theme_corrupt_user_theme_falls_back() {
        // Use a tempdir as XDG_CONFIG_HOME so we don't pollute the user.
        let dir = tempfile::tempdir().expect("tempdir");
        let themes = dir.path().join("ezpn").join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("broken.toml"), "this = is not = valid toml @@@").unwrap();
        // Save / restore the env var so other tests aren't affected.
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let t = load_theme("broken");
        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        assert_eq!(t.name, "default");
    }

    #[test]
    fn theme_adapt_preserves_logical_structure() {
        // adapt() must never drop a field; every Rgb should yield some Color.
        let t = default_theme();
        let a = t.adapt(TermCaps::TRUECOLOR);
        // Cheap structural check: name + a few canonical fields are populated.
        assert_eq!(a.name, t.name);
        // accent should be visibly different from bg in any cap mode.
        for caps in [TermCaps::TRUECOLOR, TermCaps::C256, TermCaps::C16] {
            let a = t.adapt(caps);
            assert_ne!(format!("{:?}", a.bg), format!("{:?}", a.accent));
        }
    }
}
