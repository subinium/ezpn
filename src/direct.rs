//! `--no-daemon` entry point.
//!
//! [`run_direct`] sets up the alternate screen, mouse capture, and
//! keyboard enhancements, then hands off to [`crate::app::event_loop::run`]
//! and tears it all down on exit. Kept tiny on purpose so the terminal
//! teardown sequence is auditable in one place.

use std::io;

use crossterm::{
    cursor, event, execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::app::event_loop::run;
use crate::cli::parse::Config;

pub(crate) fn run_direct(config: &Config) -> anyhow::Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        event::EnableMouseCapture,
        event::EnableFocusChange,
        event::PushKeyboardEnhancementFlags(
            event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ),
        cursor::Hide
    )?;

    let result = run(&mut stdout, config);

    let _ = execute!(
        io::stdout(),
        event::PopKeyboardEnhancementFlags,
        event::DisableFocusChange,
        cursor::Show,
        event::DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = terminal::disable_raw_mode();

    result
}
