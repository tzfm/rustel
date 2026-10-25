//! The studio's ratatui backend.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::queue;
use crossterm::style::Colored;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, IntoCrossterm, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::Color;

/// Crossterm's backend, with the terminal's cursor visible on the caret
/// only. A frame's cells go out with the cursor hidden, and Show follows
/// the move to the caret with no flush between them, so no terminal can
/// paint the cursor on a cell the frame passes through. Underline colours
/// go out in the standard colon form.
pub struct StudioBackend<W: Write> {
    inner: CrosstermBackend<W>,
    /// Show goes out with the next move or flush.
    show_pending: bool,
}

impl<W: Write> StudioBackend<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            show_pending: false,
        }
    }

    fn queue_pending_show(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.show_pending) {
            queue!(self.inner, Show)?;
        }
        Ok(())
    }
}

impl<W: Write> Write for StudioBackend<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.queue_pending_show()?;
        Write::flush(&mut self.inner)
    }
}

impl<W: Write> Backend for StudioBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut content = content.peekable();
        if content.peek().is_some() {
            queue!(self.inner, Hide)?;
        }
        // Crossterm sends an underline colour as `58;2;r;g;b`. A terminal
        // that reads 58 only in its colon form, as Windows Terminal does,
        // takes the rest as separate codes: `2` dims and `107` paints the
        // background white. Cells with an underline colour go out in runs,
        // each led by the colon form, with the colour hidden from crossterm.
        loop {
            self.inner.draw(std::iter::from_fn(|| {
                content.next_if(|(_, _, cell)| cell.underline_color == Color::Reset)
            }))?;
            let Some(&(_, _, cell)) = content.peek() else {
                return Ok(());
            };
            let colour = cell.underline_color;
            let run: Vec<_> = std::iter::from_fn(|| {
                content.next_if(|(_, _, cell)| cell.underline_color == colour)
            })
            .map(|(x, y, cell)| {
                let mut cell = cell.clone();
                cell.underline_color = Color::Reset;
                (x, y, cell)
            })
            .collect();
            write!(self.inner, "\x1b[{}m", colon_underline(colour))?;
            self.inner
                .draw(run.iter().map(|(x, y, cell)| (*x, *y, cell)))?;
        }
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.show_pending = false;
        queue!(self.inner, Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.show_pending = true;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let Position { x, y } = position.into();
        queue!(self.inner, MoveTo(x, y))?;
        self.queue_pending_show()
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        // With no controlling terminal, the inner backend asks descriptor
        // 1. That is the null device here: see `rustel_runtime::stdio`.
        #[cfg(unix)]
        if let Some((columns, rows)) = super::size_now() {
            return Ok(Size::new(columns, rows));
        }
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.queue_pending_show()?;
        Backend::flush(&mut self.inner)
    }
}

/// SGR 58 in its colon form, `58:2::r:g:b` or `58:5:n`, made from
/// crossterm's own so both name a colour the same way.
fn colon_underline(colour: Color) -> String {
    Colored::UnderlineColor(colour.into_crossterm())
        .to_string()
        .replacen("58;2;", "58:2::", 1)
        .replace(';', ":")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::cursor_trace::Capture;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// Windows Terminal reads an underline colour only in the colon form.
    /// With semicolons, `58;2;255;107;107` makes the background white.
    #[test]
    fn underline_colours_go_out_in_the_colon_form() {
        let area = Rect::new(0, 0, 4, 1);
        let mut frame = Buffer::empty(area);
        frame[(1, 0)].set_symbol("x").underline_color = Color::Rgb(255, 107, 107);
        frame[(2, 0)].set_symbol("y").underline_color = Color::LightRed;
        frame[(3, 0)].set_symbol("z");
        let capture = Capture::default();
        let mut backend = StudioBackend::new(capture.clone());
        backend
            .draw(Buffer::empty(area).diff(&frame).into_iter())
            .unwrap();
        let sent = String::from_utf8(capture.take()).unwrap();
        assert!(!sent.contains("58;"), "{sent:?}");
        for (sgr, symbol) in [("58:2::255:107:107", 'x'), ("58:5:9", 'y')] {
            let run = sent
                .split(&format!("\x1b[{sgr}m"))
                .nth(1)
                .and_then(|after| after.split("\x1b[59m").next());
            assert!(run.is_some_and(|run| run.contains(symbol)), "{sent:?}");
        }
    }
}
