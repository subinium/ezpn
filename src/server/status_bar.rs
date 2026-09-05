//! Bounded text-only rendering for the declarative `[status_bar]` schema.

#[cfg(test)]
use crate::vt100;

use std::io::Write;

use crossterm::{
    cursor, queue,
    style::{Attribute, Print, ResetColor, SetAttribute},
    terminal::{Clear, ClearType},
};
use unicode_width::UnicodeWidthChar;

use crate::config::{SegmentKind, StatusBarConfig};
use crate::render::{resolved_to_crossterm, AnsiBackground, AnsiForeground};
use crate::theme::ResolvedPalette;

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    buf: &mut impl Write,
    config: &StatusBarConfig,
    session: &str,
    tab_count: usize,
    mode: &str,
    tw: u16,
    th: u16,
    palette: &ResolvedPalette,
) -> anyhow::Result<bool> {
    if config.left.is_empty() && config.right.is_empty() {
        return Ok(false);
    }
    if tw == 0 || th == 0 {
        return Ok(true);
    }

    let context = Context {
        session,
        tab_count,
        mode,
        time: now_hhmm(),
    };
    // Preserve the right edge (clock/hints) first; the left side uses the
    // remainder with one separating cell. Never draw overlapping strings.
    let right = side(&config.right, config, &context, usize::from(tw));
    let left_limit =
        usize::from(tw).saturating_sub(right.columns + usize::from(!right.text.is_empty()));
    let left = side(&config.left, config, &context, left_limit);
    let row = th - 1;
    queue!(
        buf,
        cursor::MoveTo(0, row),
        SetAttribute(Attribute::Reset),
        AnsiBackground(resolved_to_crossterm(&palette.status_bg)),
        AnsiForeground(resolved_to_crossterm(&palette.status_fg)),
        Clear(ClearType::CurrentLine),
        Print(left.text),
    )?;
    if !right.text.is_empty() {
        queue!(
            buf,
            cursor::MoveTo(tw - right.columns as u16, row),
            Print(right.text)
        )?;
    }
    // Cancel a terminal's pending wrap when the final glyph fills the row.
    queue!(
        buf,
        cursor::MoveTo(0, row),
        ResetColor,
        SetAttribute(Attribute::Reset)
    )?;
    Ok(true)
}

struct Context<'a> {
    session: &'a str,
    tab_count: usize,
    mode: &'a str,
    time: String,
}

struct Text {
    text: String,
    columns: usize,
    limit: usize,
}

impl Text {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            columns: 0,
            limit,
        }
    }

    fn append(&mut self, value: &str) {
        if self.limit == 0 {
            return;
        }
        // Width alone does not bound zero-width marks/control strings.
        let byte_limit = self.limit.saturating_mul(16).max(64);
        for ch in value.chars().take(byte_limit) {
            if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                continue;
            }
            let width = ch.width().unwrap_or(0);
            if width == 0 && self.text.is_empty() {
                continue;
            }
            if self.columns + width > self.limit || self.text.len() + ch.len_utf8() > byte_limit {
                break;
            }
            self.text.push(ch);
            self.columns += width;
        }
    }

    fn item(&mut self, item: &Text) {
        if item.text.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.append(" ");
        }
        self.append(&item.text);
    }
}

fn side(names: &[String], config: &StatusBarConfig, context: &Context<'_>, limit: usize) -> Text {
    let mut out = Text::new(limit);
    for name in names.iter().take(128) {
        if out.columns >= limit {
            break;
        }
        let remaining = limit.saturating_sub(out.columns + usize::from(!out.text.is_empty()));
        let mut item = Text::new(remaining);
        match config.segment(name).map(|segment| &segment.kind) {
            Some(SegmentKind::Literal(text)) => item.append(text),
            Some(SegmentKind::Builtin(name)) => builtin(&mut item, name, context),
            Some(SegmentKind::KeyHints {
                max_width, keys, ..
            }) => {
                item.limit = item.limit.min(usize::from(*max_width));
                // Explicit arrays are static cheatsheets. Do not guess labels
                // for user-remapped keybindings or execute dynamic providers.
                for hint in keys.iter().take(128) {
                    if item.columns >= item.limit {
                        break;
                    }
                    let remaining = item
                        .limit
                        .saturating_sub(item.columns + usize::from(!item.text.is_empty()));
                    let mut pair = Text::new(remaining);
                    pair.append(&hint.key);
                    if !pair.text.is_empty() && !hint.label.is_empty() {
                        pair.append(" ");
                    }
                    pair.append(&hint.label);
                    item.item(&pair);
                }
            }
            None => builtin(&mut item, name, context),
        }
        out.item(&item);
    }
    out
}

fn builtin(out: &mut Text, name: &str, context: &Context<'_>) {
    match name {
        "session" => out.append(context.session),
        "tab_count" => out.append(&context.tab_count.to_string()),
        "mode" => out.append(if context.mode.is_empty() {
            "NORMAL"
        } else {
            context.mode
        }),
        "time" => out.append(&context.time),
        _ => {}
    }
}

fn now_hhmm() -> String {
    #[cfg(unix)]
    {
        let mut now: libc::time_t = 0;
        let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
        // Both pointers are valid owned storage; localtime_r initializes tm
        // only on success and does not share libc's static time buffer.
        unsafe {
            if libc::time(&mut now) == -1 || libc::localtime_r(&now, tm.as_mut_ptr()).is_null() {
                return "--:--".into();
            }
            let tm = tm.assume_init();
            format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
        }
    }
    #[cfg(not(unix))]
    {
        "--:--".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HintMode, KeyHint, StatusBarSegment};
    use crate::theme::{ColorDepth, Theme};

    fn context() -> Context<'static> {
        Context {
            session: "work",
            tab_count: 3,
            mode: "PREFIX",
            time: "12:34".into(),
        }
    }

    #[test]
    fn empty_config_keeps_legacy_bar() {
        let mut out = Vec::new();
        assert!(!draw(
            &mut out,
            &StatusBarConfig::default(),
            "s",
            1,
            "",
            80,
            24,
            &Theme::default_theme().resolve(ColorDepth::Palette16)
        )
        .unwrap());
        assert!(out.is_empty());
    }

    #[test]
    fn builtins_are_text_only_and_unknown_names_are_ignored() {
        let cfg = StatusBarConfig {
            left: vec![
                "session".into(),
                "tab_count".into(),
                "mode".into(),
                "time".into(),
                "$(echo nope)".into(),
            ],
            ..Default::default()
        };
        assert_eq!(
            side(&cfg.left, &cfg, &context(), 80).text,
            "work 3 PREFIX 12:34"
        );
    }

    #[test]
    fn literal_and_key_hints_obey_widths() {
        let cfg = StatusBarConfig {
            left: vec!["brand".into(), "hints".into()],
            right: vec![],
            segments: vec![
                StatusBarSegment {
                    name: "brand".into(),
                    kind: SegmentKind::Literal("한글".into()),
                },
                StatusBarSegment {
                    name: "hints".into(),
                    kind: SegmentKind::KeyHints {
                        mode: HintMode::Auto,
                        max_width: 7,
                        keys: vec![
                            KeyHint {
                                key: "F1".into(),
                                label: "settings".into(),
                            },
                            KeyHint {
                                key: "F2".into(),
                                label: "equalize".into(),
                            },
                        ],
                    },
                },
            ],
        };
        let line = side(&cfg.left, &cfg, &context(), 80);
        assert_eq!(line.text, "한글 F1 sett");
        assert_eq!(line.columns, 12);
    }

    #[test]
    fn unicode_truncation_and_zero_width_input_are_bounded() {
        let mut out = Text::new(3);
        out.append("한e\u{301}글");
        assert_eq!(out.text, "한e\u{301}");
        assert_eq!(out.columns, 3);
        let mut out = Text::new(2);
        out.append(&format!("a{}", "\u{301}".repeat(100_000)));
        assert!(out.text.len() <= 64);
        let mut out = Text::new(2);
        out.append("\u{301}x\x1b\n\r\u{202e}y");
        assert_eq!(out.text, "xy");
    }

    #[test]
    fn left_and_right_never_overlap_or_scroll_the_terminal() {
        let cfg = StatusBarConfig {
            left: vec!["session".into()],
            right: vec!["time".into()],
            ..Default::default()
        };
        for width in 1..=40 {
            let palette = Theme::default_theme().resolve(ColorDepth::Palette16);
            let mut out = Vec::new();
            assert!(draw(
                &mut out,
                &cfg,
                "한글 e\u{301} work\x1b[2J",
                3,
                "PREFIX",
                width,
                3,
                &palette
            )
            .unwrap());
            let wire = String::from_utf8(out.clone()).unwrap();
            assert!(!wire.contains("38;2;") && !wire.contains("38;5;"));
            let mut parser = vt100::Parser::new(3, width, 0);
            parser.process(b"top");
            parser.process(&out);
            let rows: Vec<_> = parser.screen().rows(0, width).collect();
            assert!(rows[0].starts_with(&"top"[..usize::from(width.min(3))]));
            assert_eq!(parser.screen().cursor_position(), (2, 0));
        }
    }

    #[test]
    fn zero_dimensions_emit_nothing() {
        let cfg = StatusBarConfig {
            left: vec!["session".into()],
            ..Default::default()
        };
        for (width, height) in [(0, 0), (0, 10), (10, 0)] {
            let mut out = Vec::new();
            assert!(draw(
                &mut out,
                &cfg,
                "s",
                1,
                "",
                width,
                height,
                &Theme::default_theme().resolve(ColorDepth::Palette16)
            )
            .unwrap());
            assert!(out.is_empty());
        }
    }
}
