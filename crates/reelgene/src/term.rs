//! Terminal output: a status line that redraws in place.
//!
//! The status line is cleared with spaces, not ANSI escapes, so it also works
//! in the old Windows console.

use std::io::{IsTerminal, Write};

pub struct Line {
    tty: bool,
    /// Columns the line currently on screen takes.
    shown: usize,
}

impl Line {
    pub fn new() -> Self {
        Self {
            tty: std::io::stderr().is_terminal(),
            shown: 0,
        }
    }

    /// A line that stays, printed above the status line.
    pub fn say(&mut self, msg: &str) {
        self.clear();
        println!("{msg}");
    }

    pub fn draw(&mut self, msg: &str) {
        if !self.tty {
            return;
        }
        let width = display_width(msg);
        let pad = self.shown.saturating_sub(width);
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r{msg}{}", " ".repeat(pad));
        let _ = err.flush();
        self.shown = width;
    }

    pub fn clear(&mut self) {
        if self.shown > 0 {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{}\r", " ".repeat(self.shown));
            let _ = err.flush();
            self.shown = 0;
        }
    }
}

/// Columns a string takes; CJK and other wide characters count as two.
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            if (c as u32) >= 0x1100 && !('\u{2000}'..='\u{2bff}').contains(&c) {
                2
            } else {
                1
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_take_two_columns() {
        assert_eq!(display_width("ep01"), 4);
        assert_eq!(display_width("第1集"), 5);
        assert_eq!(display_width("a · b"), 5, "a middle dot is narrow");
    }
}
