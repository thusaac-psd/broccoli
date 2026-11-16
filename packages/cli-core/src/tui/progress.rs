//! A one-line progress indicator for commands that wait on the server.

use std::fmt::Display;
use std::io::{IsTerminal, Write};

use super::spinner::Spinner;

/// A status line redrawn in place while it is shown on a terminal. On
/// anything else (a pipe, a file, a CI log) it writes nothing, so captured
/// output carries no carriage returns or escape sequences.
pub struct Progress<W: Write> {
    out: W,
    live: bool,
    spinner: Spinner,
    drawn: bool,
}

impl Progress<std::io::Stderr> {
    /// A status line on stderr, drawn only when stderr is a terminal.
    pub fn stderr() -> Self {
        let out = std::io::stderr();
        let live = out.is_terminal();
        Self::new(out, live)
    }
}

impl Progress<std::io::Stdout> {
    /// A status line on stdout, drawn only when stdout is a terminal.
    pub fn stdout() -> Self {
        let out = std::io::stdout();
        let live = out.is_terminal();
        Self::new(out, live)
    }
}

impl<W: Write> Progress<W> {
    pub fn new(out: W, live: bool) -> Self {
        Self {
            out,
            live,
            spinner: Spinner::new(),
            drawn: false,
        }
    }

    /// Replace the line with the next spinner frame and `message`.
    pub fn tick(&mut self, message: impl Display) {
        if !self.live {
            return;
        }
        let frame = self.spinner.next().to_string();
        let _ = write!(self.out, "\r\x1b[K  {frame} {message}");
        let _ = self.out.flush();
        self.drawn = true;
    }

    /// Remove the line, so what follows starts on a clean line.
    pub fn clear(&mut self) {
        if self.drawn {
            let _ = write!(self.out, "\r\x1b[K");
            let _ = self.out.flush();
            self.drawn = false;
        }
    }
}

impl<W: Write> Drop for Progress<W> {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_written_off_a_terminal() {
        let mut out = Vec::new();
        {
            let mut progress = Progress::new(&mut out, false);
            progress.tick("Judging...");
            progress.clear();
        }
        assert!(out.is_empty());
    }

    #[test]
    fn a_terminal_line_is_redrawn_and_cleared() {
        let mut out = Vec::new();
        {
            let mut progress = Progress::new(&mut out, true);
            progress.tick("Judging...");
        }
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\r\x1b[K  "), "{text:?}");
        assert!(text.contains("Judging..."));
        assert!(text.ends_with("\r\x1b[K"), "{text:?}");
    }
}
