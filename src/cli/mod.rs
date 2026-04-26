//! CLI surface: argument parsing and `--help` text.
//!
//! Split out of `main.rs` so the dispatcher stays small. Two siblings:
//! - [`parse`]: argv → [`parse::Config`] for both foreground + daemon entries.
//! - [`help`]: `--help` body, kept verbatim.

pub(crate) mod help;
pub(crate) mod parse;

pub(crate) use help::print_help;
pub(crate) use parse::parse_args;
