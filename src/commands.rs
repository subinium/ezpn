//! Command-palette vocabulary parser.
//!
//! Maps tmux-compatible command strings (typed after `Ctrl+B :`) to a typed
//! [`Command`] enum that the server dispatches against existing actions.
//!
//! Scope (issue #58): a deliberately minimal parity floor. Tab completion,
//! fuzzy matching, and full tmux quoting rules are deferred (#86).

use std::fmt;

/// Direction for `select-pane` / `resize-pane` / `swap-pane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    fn parse(flag: &str) -> Option<Self> {
        // Accept the tmux-style `-U`/`-D`/`-L`/`-R` flags. Lower-case forms
        // (`-u`, `-d`, ...) are accepted for ergonomics; tmux is case-sensitive
        // here but the current vocabulary is small enough to be permissive.
        match flag {
            "-U" | "-u" => Some(Dir::Up),
            "-D" | "-d" => Some(Dir::Down),
            "-L" | "-l" => Some(Dir::Left),
            "-R" | "-r" => Some(Dir::Right),
            _ => None,
        }
    }
}

/// Parsed command-palette command.
///
/// Each variant maps onto an existing server action; see `server::execute_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `split-window -h` / `splitw -h` — split active pane horizontally.
    SplitHorizontal,
    /// `split-window -v` / `splitw -v` — split active pane vertically.
    SplitVertical,
    /// `kill-pane` / `killp` — close current pane.
    KillPane,
    /// `kill-window` — close current tab.
    KillWindow,
    /// `new-window [-n NAME]` — open a new tab, optionally named.
    NewWindow { name: Option<String> },
    /// `rename-window NAME` — rename the current tab.
    RenameWindow { name: String },
    /// `select-pane -[UDLR]` — focus an adjacent pane.
    SelectPane { dir: Dir },
    /// `resize-pane -[UDLR] [N]` — resize active pane by `n` cells (default 1).
    ResizePane { dir: Dir, amount: u16 },
    /// `swap-pane -[UD]` — swap active pane with its tree-order neighbour.
    /// Only `-U` (previous) and `-D` (next) are accepted; tmux's `-L`/`-R`
    /// forms are not implemented yet.
    SwapPane { up: bool },
    /// `select-layout NAME` — apply a layout preset (e.g. `ide`, `dev`, `1:1`).
    SelectLayout { name: String },
    /// `set-option KEY VALUE` — session-scoped option.
    SetOption { key: String, value: String },
    /// `display-message TEXT` — flash a message in the status bar.
    DisplayMessage { text: String },
}

/// Parser failure modes. Carries enough context for the UI to report
/// `unknown command: <name> (try ?)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Empty input (no command typed).
    Empty,
    /// First token did not match any known command name.
    UnknownCommand(String),
    /// A required argument was missing.
    MissingArgument {
        command: &'static str,
        argument: &'static str,
    },
    /// An argument failed to parse (e.g. bad direction flag).
    InvalidArgument {
        command: &'static str,
        argument: String,
    },
    /// Input contained an unterminated quoted string.
    UnterminatedQuote,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => f.write_str("empty command"),
            ParseError::UnknownCommand(name) => write!(f, "unknown command: {name} (try ?)"),
            ParseError::MissingArgument { command, argument } => {
                write!(f, "{command}: missing argument <{argument}>")
            }
            ParseError::InvalidArgument { command, argument } => {
                write!(f, "{command}: invalid argument: {argument}")
            }
            ParseError::UnterminatedQuote => f.write_str("unterminated quote"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse a command-palette string into a typed [`Command`].
pub fn parse(input: &str) -> Result<Command, ParseError> {
    let tokens = tokenize(input)?;
    let mut iter = tokens.iter().map(String::as_str);
    let head = match iter.next() {
        Some(h) => h,
        None => return Err(ParseError::Empty),
    };
    let rest: Vec<&str> = iter.collect();

    match head {
        // ── splits ──
        "split-window" | "splitw" | "split" => parse_split(&rest),

        // ── pane lifecycle ──
        "kill-pane" | "killp" | "close-pane" => Ok(Command::KillPane),

        // ── window/tab lifecycle ──
        "kill-window" | "close-tab" => Ok(Command::KillWindow),
        "new-window" | "new-tab" | "neww" => parse_new_window(&rest),
        "rename-window" | "rename-tab" | "renamew" => {
            let name = join_remaining(&rest).ok_or(ParseError::MissingArgument {
                command: "rename-window",
                argument: "NAME",
            })?;
            Ok(Command::RenameWindow { name })
        }

        // ── pane navigation / sizing ──
        "select-pane" | "selectp" => {
            parse_dir_only(&rest, "select-pane").map(|dir| Command::SelectPane { dir })
        }
        "resize-pane" | "resizep" => parse_resize_pane(&rest),
        "swap-pane" | "swapp" => parse_swap_pane(&rest),

        // ── layout ──
        "select-layout" | "selectl" | "layout" => {
            let name = rest
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument {
                    command: "select-layout",
                    argument: "NAME",
                })?
                .to_string();
            Ok(Command::SelectLayout { name })
        }

        // ── options / messaging ──
        "set-option" | "setw" | "set" => parse_set_option(&rest),
        "display-message" | "display" | "displaym" => {
            let text = join_remaining(&rest).ok_or(ParseError::MissingArgument {
                command: "display-message",
                argument: "TEXT",
            })?;
            Ok(Command::DisplayMessage { text })
        }

        other => Err(ParseError::UnknownCommand(other.to_string())),
    }
}

// ── helpers ──────────────────────────────────────────────────────────────

fn parse_split(rest: &[&str]) -> Result<Command, ParseError> {
    // Default to horizontal if no flag is given (matches tmux default for `splitw`).
    match rest.first().copied() {
        Some("-h") => Ok(Command::SplitHorizontal),
        Some("-v") => Ok(Command::SplitVertical),
        Some(other) if other.starts_with('-') => Err(ParseError::InvalidArgument {
            command: "split-window",
            argument: other.to_string(),
        }),
        _ => Ok(Command::SplitHorizontal),
    }
}

fn parse_new_window(rest: &[&str]) -> Result<Command, ParseError> {
    // `new-window` (no args) or `new-window -n NAME`.
    let mut name: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-n" => {
                i += 1;
                let v = rest.get(i).copied().ok_or(ParseError::MissingArgument {
                    command: "new-window",
                    argument: "NAME",
                })?;
                name = Some(v.to_string());
            }
            other if other.starts_with('-') => {
                return Err(ParseError::InvalidArgument {
                    command: "new-window",
                    argument: other.to_string(),
                });
            }
            _ => {
                // Trailing positional args are silently ignored; tmux treats
                // them as a shell command which we don't implement.
            }
        }
        i += 1;
    }
    Ok(Command::NewWindow { name })
}

fn parse_dir_only(rest: &[&str], cmd: &'static str) -> Result<Dir, ParseError> {
    let flag = rest.first().copied().ok_or(ParseError::MissingArgument {
        command: cmd,
        argument: "-U|-D|-L|-R",
    })?;
    Dir::parse(flag).ok_or(ParseError::InvalidArgument {
        command: cmd,
        argument: flag.to_string(),
    })
}

fn parse_resize_pane(rest: &[&str]) -> Result<Command, ParseError> {
    let dir = parse_dir_only(rest, "resize-pane")?;
    let amount: u16 = match rest.get(1).copied() {
        Some(s) => s.parse().map_err(|_| ParseError::InvalidArgument {
            command: "resize-pane",
            argument: s.to_string(),
        })?,
        None => 1,
    };
    Ok(Command::ResizePane { dir, amount })
}

fn parse_swap_pane(rest: &[&str]) -> Result<Command, ParseError> {
    let flag = rest.first().copied().ok_or(ParseError::MissingArgument {
        command: "swap-pane",
        argument: "-U|-D",
    })?;
    match flag {
        "-U" | "-u" => Ok(Command::SwapPane { up: true }),
        "-D" | "-d" => Ok(Command::SwapPane { up: false }),
        other => Err(ParseError::InvalidArgument {
            command: "swap-pane",
            argument: other.to_string(),
        }),
    }
}

fn parse_set_option(rest: &[&str]) -> Result<Command, ParseError> {
    let key = rest
        .first()
        .copied()
        .ok_or(ParseError::MissingArgument {
            command: "set-option",
            argument: "KEY",
        })?
        .to_string();
    let value = if rest.len() >= 2 {
        join_remaining(&rest[1..]).unwrap_or_default()
    } else {
        return Err(ParseError::MissingArgument {
            command: "set-option",
            argument: "VALUE",
        });
    };
    Ok(Command::SetOption { key, value })
}

/// Join the remaining tokens into a single space-separated string. Returns
/// `None` if there are no tokens.
fn join_remaining(tokens: &[&str]) -> Option<String> {
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

/// Tokenize an input string using a minimal shell-style splitter:
/// whitespace separates tokens, single and double quotes group whitespace
/// inside, and `\` escapes the next character.
///
/// This is intentionally simpler than tmux's full quoting rules. The full
/// rules are deferred to #86 along with fuzzy completion.
fn tokenize(input: &str) -> Result<Vec<String>, ParseError> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match (quote, c) {
            // Closing the active quote.
            (Some(q), c) if c == q => {
                quote = None;
                // Empty quoted strings still count as a token.
                in_token = true;
            }
            // Inside a quote — take the character literally except for `\`
            // inside double quotes (single quotes are fully literal).
            (Some('"'), '\\') => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    current.push(next);
                }
            }
            (Some(_), c) => {
                current.push(c);
                in_token = true;
            }
            // Start of a quoted region.
            (None, c) if c == '"' || c == '\'' => {
                quote = Some(c);
                in_token = true;
            }
            // Backslash escape outside quotes.
            (None, '\\') => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    current.push(next);
                    in_token = true;
                }
            }
            // Whitespace splits tokens.
            (None, c) if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            // Anything else is part of the current token.
            (None, c) => {
                current.push(c);
                in_token = true;
            }
        }
    }

    if quote.is_some() {
        return Err(ParseError::UnterminatedQuote);
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

// ── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_error() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert_eq!(parse("   "), Err(ParseError::Empty));
    }

    #[test]
    fn unknown_command_is_structured_error() {
        let err = parse("frobnicate").unwrap_err();
        assert_eq!(err, ParseError::UnknownCommand("frobnicate".to_string()));
        // Display is what the status bar shows.
        assert_eq!(err.to_string(), "unknown command: frobnicate (try ?)");
    }

    #[test]
    fn split_horizontal_long_and_short_alias() {
        assert_eq!(parse("split-window -h"), Ok(Command::SplitHorizontal));
        assert_eq!(parse("splitw -h"), Ok(Command::SplitHorizontal));
        // Default (no flag) maps to horizontal.
        assert_eq!(parse("split-window"), Ok(Command::SplitHorizontal));
    }

    #[test]
    fn split_vertical_long_and_short_alias() {
        assert_eq!(parse("split-window -v"), Ok(Command::SplitVertical));
        assert_eq!(parse("splitw -v"), Ok(Command::SplitVertical));
    }

    #[test]
    fn kill_pane_aliases() {
        assert_eq!(parse("kill-pane"), Ok(Command::KillPane));
        assert_eq!(parse("killp"), Ok(Command::KillPane));
    }

    #[test]
    fn kill_window_parses() {
        assert_eq!(parse("kill-window"), Ok(Command::KillWindow));
    }

    #[test]
    fn new_window_with_and_without_name() {
        assert_eq!(parse("new-window"), Ok(Command::NewWindow { name: None }));
        assert_eq!(
            parse("new-window -n logs"),
            Ok(Command::NewWindow {
                name: Some("logs".to_string())
            })
        );
    }

    #[test]
    fn new_window_missing_name_after_flag_errors() {
        assert_eq!(
            parse("new-window -n"),
            Err(ParseError::MissingArgument {
                command: "new-window",
                argument: "NAME"
            })
        );
    }

    #[test]
    fn rename_window_with_simple_name() {
        assert_eq!(
            parse("rename-window editor"),
            Ok(Command::RenameWindow {
                name: "editor".to_string()
            })
        );
    }

    #[test]
    fn rename_window_quoted_name_preserves_spaces() {
        assert_eq!(
            parse("rename-window \"my tab\""),
            Ok(Command::RenameWindow {
                name: "my tab".to_string()
            })
        );
        assert_eq!(
            parse("rename-window 'with spaces'"),
            Ok(Command::RenameWindow {
                name: "with spaces".to_string()
            })
        );
    }

    #[test]
    fn rename_window_requires_name() {
        assert_eq!(
            parse("rename-window"),
            Err(ParseError::MissingArgument {
                command: "rename-window",
                argument: "NAME"
            })
        );
    }

    #[test]
    fn select_pane_each_direction() {
        assert_eq!(
            parse("select-pane -U"),
            Ok(Command::SelectPane { dir: Dir::Up })
        );
        assert_eq!(
            parse("select-pane -D"),
            Ok(Command::SelectPane { dir: Dir::Down })
        );
        assert_eq!(
            parse("select-pane -L"),
            Ok(Command::SelectPane { dir: Dir::Left })
        );
        assert_eq!(
            parse("select-pane -R"),
            Ok(Command::SelectPane { dir: Dir::Right })
        );
    }

    #[test]
    fn select_pane_missing_dir_errors() {
        assert!(matches!(
            parse("select-pane"),
            Err(ParseError::MissingArgument {
                command: "select-pane",
                ..
            })
        ));
    }

    #[test]
    fn select_pane_invalid_flag_errors() {
        assert!(matches!(
            parse("select-pane -X"),
            Err(ParseError::InvalidArgument {
                command: "select-pane",
                ..
            })
        ));
    }

    #[test]
    fn resize_pane_with_and_without_amount() {
        assert_eq!(
            parse("resize-pane -L"),
            Ok(Command::ResizePane {
                dir: Dir::Left,
                amount: 1,
            })
        );
        assert_eq!(
            parse("resize-pane -R 5"),
            Ok(Command::ResizePane {
                dir: Dir::Right,
                amount: 5,
            })
        );
    }

    #[test]
    fn resize_pane_invalid_amount_errors() {
        assert!(matches!(
            parse("resize-pane -L abc"),
            Err(ParseError::InvalidArgument {
                command: "resize-pane",
                ..
            })
        ));
    }

    #[test]
    fn swap_pane_up_and_down() {
        assert_eq!(parse("swap-pane -U"), Ok(Command::SwapPane { up: true }));
        assert_eq!(parse("swap-pane -D"), Ok(Command::SwapPane { up: false }));
    }

    #[test]
    fn select_layout_with_name() {
        assert_eq!(
            parse("select-layout ide"),
            Ok(Command::SelectLayout {
                name: "ide".to_string()
            })
        );
        assert_eq!(
            parse("layout dev"),
            Ok(Command::SelectLayout {
                name: "dev".to_string()
            })
        );
    }

    #[test]
    fn select_layout_requires_name() {
        assert_eq!(
            parse("select-layout"),
            Err(ParseError::MissingArgument {
                command: "select-layout",
                argument: "NAME"
            })
        );
    }

    #[test]
    fn set_option_key_value() {
        assert_eq!(
            parse("set-option border rounded"),
            Ok(Command::SetOption {
                key: "border".to_string(),
                value: "rounded".to_string(),
            })
        );
    }

    #[test]
    fn set_option_quoted_value() {
        assert_eq!(
            parse("set-option status-left \"hello world\""),
            Ok(Command::SetOption {
                key: "status-left".to_string(),
                value: "hello world".to_string(),
            })
        );
    }

    #[test]
    fn set_option_requires_value() {
        assert_eq!(
            parse("set-option border"),
            Err(ParseError::MissingArgument {
                command: "set-option",
                argument: "VALUE"
            })
        );
    }

    #[test]
    fn display_message_passes_text_through() {
        assert_eq!(
            parse("display-message hello"),
            Ok(Command::DisplayMessage {
                text: "hello".to_string()
            })
        );
        assert_eq!(
            parse("display-message \"hello, world\""),
            Ok(Command::DisplayMessage {
                text: "hello, world".to_string()
            })
        );
    }

    #[test]
    fn display_message_requires_text() {
        assert_eq!(
            parse("display-message"),
            Err(ParseError::MissingArgument {
                command: "display-message",
                argument: "TEXT"
            })
        );
    }

    #[test]
    fn quoted_args_with_internal_spaces() {
        // Tokenizer correctness: quoted args preserve embedded whitespace.
        let toks = tokenize("foo \"bar baz\" 'qux quux'").unwrap();
        assert_eq!(toks, vec!["foo", "bar baz", "qux quux"]);
    }

    #[test]
    fn unterminated_quote_errors() {
        assert_eq!(
            parse("rename-window \"unterminated"),
            Err(ParseError::UnterminatedQuote)
        );
    }

    #[test]
    fn backslash_escapes_outside_quotes() {
        let toks = tokenize("a\\ b c").unwrap();
        assert_eq!(toks, vec!["a b", "c"]);
    }

    #[test]
    fn vocabulary_smoke_test_every_command_parses() {
        // One assertion per row of the spec table — ensures none silently
        // disappear from the dispatch table during refactors.
        let cases: &[(&str, Command)] = &[
            ("split-window -h", Command::SplitHorizontal),
            ("splitw -h", Command::SplitHorizontal),
            ("split-window -v", Command::SplitVertical),
            ("splitw -v", Command::SplitVertical),
            ("kill-pane", Command::KillPane),
            ("killp", Command::KillPane),
            ("kill-window", Command::KillWindow),
            ("new-window", Command::NewWindow { name: None }),
            (
                "new-window -n side",
                Command::NewWindow {
                    name: Some("side".to_string()),
                },
            ),
            (
                "rename-window editor",
                Command::RenameWindow {
                    name: "editor".to_string(),
                },
            ),
            ("select-pane -U", Command::SelectPane { dir: Dir::Up }),
            ("select-pane -D", Command::SelectPane { dir: Dir::Down }),
            ("select-pane -L", Command::SelectPane { dir: Dir::Left }),
            ("select-pane -R", Command::SelectPane { dir: Dir::Right }),
            (
                "resize-pane -U",
                Command::ResizePane {
                    dir: Dir::Up,
                    amount: 1,
                },
            ),
            (
                "resize-pane -L 3",
                Command::ResizePane {
                    dir: Dir::Left,
                    amount: 3,
                },
            ),
            ("swap-pane -U", Command::SwapPane { up: true }),
            ("swap-pane -D", Command::SwapPane { up: false }),
            (
                "select-layout ide",
                Command::SelectLayout {
                    name: "ide".to_string(),
                },
            ),
            (
                "set-option border heavy",
                Command::SetOption {
                    key: "border".to_string(),
                    value: "heavy".to_string(),
                },
            ),
            (
                "display-message hi",
                Command::DisplayMessage {
                    text: "hi".to_string(),
                },
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(parse(input).as_ref(), Ok(expected), "input: {input:?}");
        }
    }

    /// Placeholder for the integration-style test from the issue:
    /// "send `:kill-pane` via send-keys to a 4-pane session, assert 3 panes
    /// remain". The end-to-end harness is part of #62 and not yet wired up;
    /// this test asserts the parser side of that contract so the link from
    /// `:kill-pane` -> `Command::KillPane` cannot regress.
    #[test]
    fn kill_pane_dispatches_to_kill_pane_command() {
        assert_eq!(parse("kill-pane"), Ok(Command::KillPane));
        assert_eq!(
            parse(":kill-pane".trim_start_matches(':')),
            Ok(Command::KillPane)
        );
    }
}
