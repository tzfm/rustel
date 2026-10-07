//! The studio's ratatui backend.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::queue;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// Crossterm's backend, with the terminal's cursor visible on the caret
/// only. A frame's cells go out with the cursor hidden, and Show follows
/// the move to the caret with no flush between them, so no terminal can
/// paint the cursor on a cell the frame passes through.
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
        self.inner.draw(content)
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
