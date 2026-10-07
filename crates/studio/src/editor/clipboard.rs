use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardError(pub String);

impl fmt::Display for ClipboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ClipboardError {}

/// Text-only clipboard seam.  Tests never touch the desktop clipboard, and a
/// terminal running over SSH can retain an in-process fallback even when no
/// X11/Wayland/Win32 pasteboard is available.
pub trait Clipboard {
    fn get_text(&mut self) -> Result<String, ClipboardError>;
    fn set_text(&mut self, text: String) -> Result<(), ClipboardError>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryClipboard {
    text: Option<String>,
}

impl MemoryClipboard {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
        }
    }

    pub fn contents(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

impl Clipboard for MemoryClipboard {
    fn get_text(&mut self) -> Result<String, ClipboardError> {
        self.text
            .clone()
            .ok_or_else(|| ClipboardError("clipboard contains no text".into()))
    }

    fn set_text(&mut self, text: String) -> Result<(), ClipboardError> {
        self.text = Some(text);
        Ok(())
    }
}

/// System clipboard with an always-available process-local fallback.
///
/// The arboard handle deliberately lives for the whole UI session: on Linux
/// the process that owns clipboard text may need to remain alive to serve it.
pub struct PlatformClipboard {
    system: Option<arboard::Clipboard>,
    fallback: MemoryClipboard,
}

impl fmt::Debug for PlatformClipboard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformClipboard")
            .field("system_available", &self.system.is_some())
            .field("fallback", &self.fallback)
            .finish()
    }
}

impl Default for PlatformClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformClipboard {
    pub fn new() -> Self {
        Self {
            system: arboard::Clipboard::new().ok(),
            fallback: MemoryClipboard::default(),
        }
    }

    pub fn system_available(&self) -> bool {
        self.system.is_some()
    }
}

impl Clipboard for PlatformClipboard {
    fn get_text(&mut self) -> Result<String, ClipboardError> {
        if let Some(system) = &mut self.system {
            match system.get_text() {
                Ok(text) => {
                    self.fallback.set_text(text.clone())?;
                    return Ok(text);
                }
                // A busy clipboard is worth a patient retry - Windows
                // clipboard opens fail while another process holds it. A
                // terminal that reports no clipboard at all is not:
                // fall through to the process-local fallback.
                Err(arboard::Error::ClipboardOccupied) => {
                    let mut text = None;
                    for attempt in 0..5 {
                        match system.get_text() {
                            Ok(found) => {
                                text = Some(found);
                                break;
                            }
                            Err(arboard::Error::ClipboardOccupied) if attempt < 4 => {
                                std::thread::sleep(std::time::Duration::from_millis(
                                    25 * (attempt as u64 + 1),
                                ));
                            }
                            Err(_) => break,
                        }
                    }
                    if let Some(found) = text {
                        self.fallback.set_text(found.clone())?;
                        return Ok(found);
                    }
                }
                _ => {}
            }
        }
        self.fallback.get_text()
    }

    fn set_text(&mut self, text: String) -> Result<(), ClipboardError> {
        // The in-process copy always lands: the ordinary editor operation
        // remains useful in a headless terminal even when the desktop
        // clipboard is unavailable or temporarily busy.
        self.fallback.set_text(text.clone())?;
        if let Some(system) = &mut self.system {
            // A busy clipboard is worth a patient retry: Windows clipboard
            // opens fail while another process holds it, for example a
            // clipboard manager or another job on a shared runner, and one
            // immediate retry is not enough. A few short waits, about a
            // quarter second in all, are. Anything still busy, or any other
            // error, is reported, so the status line never says "copied"
            // when the OS clipboard did not change.
            let mut busy: Option<arboard::Error> = None;
            for attempt in 0..5 {
                match system.set_text(text.clone()) {
                    Ok(()) => {
                        busy = None;
                        break;
                    }
                    Err(error @ arboard::Error::ClipboardOccupied) => {
                        busy = Some(error);
                        if attempt < 4 {
                            std::thread::sleep(std::time::Duration::from_millis(
                                25 * (attempt as u64 + 1),
                            ));
                        }
                    }
                    Err(other) => return Err(ClipboardError(other.to_string())),
                }
            }
            if let Some(error) = busy {
                return Err(ClipboardError(format!("the clipboard is busy ({error})")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_clipboard_round_trips_unicode() {
        let mut clipboard = MemoryClipboard::default();
        clipboard.set_text("界🥁".into()).unwrap();
        assert_eq!(clipboard.get_text().unwrap(), "界🥁");
    }
}
