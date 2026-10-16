//! Session-only address and token entries in the remote-control sheet.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteField {
    Address,
    Token,
}

pub(super) struct RemoteEdit {
    pub(super) field: RemoteField,
    text: String,
    cursor: usize,
    pub(super) selected: bool,
    pub(super) error: Option<String>,
}

impl RemoteEdit {
    pub(super) fn visible_text(&self, width: usize, revealed: bool) -> String {
        let start = self.start(width);
        let end = (start + width).min(self.text.len());
        if self.field == RemoteField::Token && !revealed {
            "•".repeat(end - start)
        } else {
            self.text[start..end].to_owned()
        }
    }

    fn start(&self, width: usize) -> usize {
        self.cursor.saturating_sub(width.saturating_sub(1))
    }

    pub(super) fn cursor_column(&self, width: usize) -> u16 {
        (self.cursor - self.start(width)) as u16
    }

    fn insert(&mut self, text: &str) {
        let limit = if self.field == RemoteField::Token {
            256
        } else {
            64
        };
        let kept = if self.selected { 0 } else { self.text.len() };
        if text.len() > limit - kept || !text.bytes().all(|byte| byte.is_ascii_graphic()) {
            self.error = Some(format!("Use up to {limit} ASCII characters without spaces"));
            return;
        }
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.error = None;
    }
}

impl App {
    pub(super) fn begin_remote_edit(&mut self, field: RemoteField) {
        if self.remote.is_some() {
            self.status = "disable remote control before editing its address or token".into();
            self.toast(self.status.clone());
        } else {
            let text = match field {
                RemoteField::Address => self.remote_bind_address().to_string(),
                RemoteField::Token => self.options.auth_token.clone().unwrap_or_default(),
            };
            let mut edit = RemoteEdit {
                field,
                cursor: 0,
                text: String::new(),
                selected: false,
                error: None,
            };
            edit.insert(&text);
            edit.selected = true;
            self.remote_edit = Some(Box::new(edit));
            self.remote_token_revealed = false;
        }
        self.dirty_frame = true;
    }

    fn save_remote_edit(&mut self, edit: &RemoteEdit) -> Result<(), String> {
        // Stop the listener before changing its settings.
        if self.remote.is_some() {
            return Err("Disable remote control before editing".into());
        }
        match edit.field {
            RemoteField::Address => {
                self.options.remote_control = Some(crate::remote::parse_bind(&edit.text)?);
            }
            RemoteField::Token => {
                self.options.auth_token = if edit.text.is_empty() {
                    None
                } else {
                    Some(crate::remote::parse_auth_token(&edit.text)?)
                };
            }
        }
        self.remote_token_revealed = false;
        Ok(())
    }

    pub(super) fn remote_edit_event(&mut self, event: &Event) -> bool {
        let Some(mut edit) = self.remote_edit.take() else {
            return false;
        };
        match event {
            Event::Paste(text) => edit.insert(text),
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let primary = key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                match key.code {
                    KeyCode::Esc if key.kind == KeyEventKind::Press => {
                        self.remote_token_revealed = false;
                        self.dirty_frame = true;
                        return true;
                    }
                    KeyCode::Enter
                        if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                    {
                        match self.save_remote_edit(&edit) {
                            Ok(()) => {
                                self.dirty_frame = true;
                                return true;
                            }
                            Err(error) => edit.error = Some(error),
                        }
                    }
                    KeyCode::Char('r' | 'R')
                        if alt && !primary && key.kind == KeyEventKind::Press =>
                    {
                        if edit.field == RemoteField::Token {
                            self.remote_token_revealed = !self.remote_token_revealed;
                        }
                    }
                    KeyCode::Char('a' | 'A') if primary && !alt => edit.selected = true,
                    KeyCode::Char('v' | 'V')
                        if primary && !alt && key.kind == KeyEventKind::Press =>
                    {
                        match self.clipboard.get_text() {
                            Ok(text) => edit.insert(&text),
                            Err(_) => edit.error = Some("Could not read clipboard".into()),
                        }
                    }
                    KeyCode::Char(ch) if !primary && !alt => edit.insert(&ch.to_string()),
                    KeyCode::Backspace | KeyCode::Delete if !primary && !alt => {
                        if edit.selected {
                            edit.text.clear();
                            edit.cursor = 0;
                            edit.selected = false;
                        } else if key.code == KeyCode::Backspace && edit.cursor > 0 {
                            edit.cursor -= 1;
                            edit.text.remove(edit.cursor);
                        } else if key.code == KeyCode::Delete && edit.cursor < edit.text.len() {
                            edit.text.remove(edit.cursor);
                        }
                        edit.error = None;
                    }
                    KeyCode::Left if !primary && !alt => {
                        edit.cursor = if edit.selected {
                            0
                        } else {
                            edit.cursor.saturating_sub(1)
                        };
                        edit.selected = false;
                    }
                    KeyCode::Right if !primary && !alt => {
                        edit.cursor = if edit.selected {
                            edit.text.len()
                        } else {
                            (edit.cursor + 1).min(edit.text.len())
                        };
                        edit.selected = false;
                    }
                    KeyCode::Home if !primary && !alt => {
                        edit.cursor = 0;
                        edit.selected = false;
                    }
                    KeyCode::End if !primary && !alt => {
                        edit.cursor = edit.text.len();
                        edit.selected = false;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        self.remote_edit = Some(edit);
        self.dirty_frame = true;
        true
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "remote-control")]
    mod remote_update {
        //! A remote update evaluates and saves the text from the same input drain.

        use crate::app::tests::support::app_over;
        use crate::app::*;
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;

        fn expect_ok(reader: &mut BufReader<TcpStream>, command: &str) {
            let mut reply = String::new();
            reader.read_line(&mut reply).expect("command reply");
            assert_eq!(reply, "ok\n", "reply to {command}");
        }

        /// Request IDs increase, so the highest ID holds the latest sent source.
        fn newest(app: &App) -> Option<&Arc<str>> {
            app.pending_evaluation
                .as_ref()
                .map(|pending| &pending.source)
                .or_else(|| {
                    app.inflight
                        .iter()
                        .max_by_key(|(id, _)| **id)
                        .map(|(_, sent)| &sent.source)
                })
        }

        #[test]
        fn an_update_chord_in_the_same_drain_as_its_text_installs_that_text() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.remote = Some(Box::new(
                crate::remote::listen("127.0.0.1:0".parse().unwrap(), Some("gig1"))
                    .expect("listener"),
            ));
            let address = app.remote.as_ref().unwrap().address();
            let mut client = TcpStream::connect(address).expect("connect client");
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut reader = BufReader::new(client.try_clone().expect("clone client"));
            writeln!(client, "auth gig1").expect("authenticate");
            expect_ok(&mut reader, "auth gig1");

            writeln!(client, "key ctrl-s").expect("send update");
            expect_ok(&mut reader, "key ctrl-s");
            app.pump_turn();
            app.poll_remote().expect("poll first update");
            assert!(newest(&app).is_some(), "the first update was not queued");

            let scene = app.scenes.current().id;
            let starter: Arc<str> = Arc::from(app.editor().source().as_str());
            let starter_revision = app.editor().revision();

            // The trailing space puts the last edit and update in the fourth
            // eight-event drain. All 26 events fit in the socket queue.
            let replacement = "setcps(0.25)\n$: s(\"bd\") ";
            let mut burst = String::from("key ctrl-a\n");
            for character in replacement.chars() {
                match character {
                    '\n' => burst.push_str("key enter\n"),
                    ' ' => burst.push_str("key space\n"),
                    other => burst.push_str(&format!("key {other}\n")),
                }
            }
            burst.push_str("key ctrl-s\n");
            assert_eq!(burst.lines().count(), 26);
            client.write_all(burst.as_bytes()).expect("send burst");

            // An acknowledgement confirms that the command is in the queue.
            for command in burst.lines() {
                expect_ok(&mut reader, command);
            }
            for _ in 0..3 {
                app.pump_turn();
                app.poll_remote().expect("poll");
            }
            app.pump_turn();
            assert_eq!(app.editor().source(), replacement.trim_end());

            // Hold a prior evaluation open while the installed score plays.
            // ID zero is older than every request that the app sends.
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                ..Default::default()
            });
            app.audible_scene = Some(scene);
            app.installed_revision = Some(source_revision(starter.as_ref()).to_owned());
            app.inflight.insert(
                0,
                PendingEvaluation {
                    request_id: 0,
                    scene,
                    editor_revision: starter_revision,
                    source: starter,
                    launch: Launch::Now,
                    rewind: false,
                    record: false,
                },
            );
            app.poll_remote().expect("poll final edit and update");
            assert_eq!(
                app.editor().source(),
                replacement,
                "the burst never fully landed in the editor"
            );

            assert_eq!(
                newest(&app).map(|source| source.as_ref()),
                Some(replacement),
                "the update answered ok but never queued the burst's text; status: {:?}",
                app.status
            );
            app.wait_for_saves();
            assert!(app.save_worker.idle(), "the update save did not complete");
            let on_disk = std::fs::read_to_string(directory.path().join("first.strudel"))
                .expect("the score file");
            assert_eq!(
                on_disk, replacement,
                "the update saved the text from before the burst"
            );
        }
    }
}
