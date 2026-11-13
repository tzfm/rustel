//! The Link session's side of the app: the checkbox that joins it, and
//! the transport that follows what the session plays.

use super::*;

/// Whether this studio is in the Link session, and what the session's
/// transport last said.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum LinkFollow {
    /// Not in a session. The snapshot's link status is not followed.
    #[default]
    Off,
    /// In the session. `seen_playing` is what its transport last said,
    /// once a stand was taken on it.
    Joined { seen_playing: Option<bool> },
}

impl LinkFollow {
    pub(super) fn is_joined(self) -> bool {
        matches!(self, Self::Joined { .. })
    }
}

impl App {
    /// Join or leave the Link session. Never kept between runs: joining
    /// puts this studio on a grid other machines run, so it is a thing
    /// done on purpose, each time.
    pub(crate) fn set_link(&mut self, on: bool) {
        if on == self.link.is_joined() {
            return;
        }
        if self.worker.try_link(on) {
            self.link = if on {
                LinkFollow::Joined { seen_playing: None }
            } else {
                LinkFollow::Off
            };
            self.status = if on {
                "link session joined - tempo and transport follow it"
            } else {
                "link session left"
            }
            .into();
        }
    }

    /// The Link session once a turn: a session that starts playing
    /// starts this studio too, and one that stops stops it. What this
    /// studio plays is already told to the session by the engine, so
    /// the echoes of our own decisions arrive agreeing with us and
    /// nothing happens. Out of the session nothing is followed, though the
    /// last snapshot can still carry the session it left.
    pub(super) fn pump_link(&mut self) {
        let LinkFollow::Joined { seen_playing } = &mut self.link else {
            return;
        };
        let Some(link) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.clock.link)
        else {
            *seen_playing = None;
            return;
        };
        let seen = seen_playing.replace(link.playing);
        if seen == Some(link.playing) {
            return;
        }
        if link.playing && !self.is_playing() {
            let _ = self.dispatch_editor(Command::Evaluate);
        } else if seen == Some(true) && !link.playing && self.is_playing() && !self.is_stopping() {
            // A stop is the session going quiet after it was heard
            // playing. The turns before our own start echoes back must
            // not read as that.
            let _ = self.dispatch_editor(Command::Stop);
        }
    }
}

#[cfg(test)]
mod tests {
    mod link {
        //! Tests for `app/link.rs`: the checkbox that joins the Link
        //! session, and the transport that follows what it plays.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::devices::DeviceKind;
        use rustel_runtime::link::LinkStatus;

        /// A snapshot whose session stands where the test wants it.
        fn session(playing: bool) -> StudioSnapshot {
            StudioSnapshot {
                clock: crate::engine::ClockStatus {
                    link: Some(LinkStatus {
                        peers: 1,
                        bpm: 120.0,
                        playing,
                        cycle: 4.0,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }
        }

        #[test]
        fn the_link_tab_joins_on_space_and_leaves_on_space_again() {
            // The checkbox is what is tested here, not the wire: the
            // worker behind it is real, so the session stays off it,
            // else a plain test run asks the OS for its consent.
            rustel_runtime::link::keep_the_session_off_the_wire();
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.panel = Some(crate::devices::DevicePanel {
                kind: DeviceKind::Link,
                ..crate::devices::DevicePanel::default()
            });

            app.handle_panel_key(KeyCode::Char(' '), false)
                .expect("handled");
            assert!(app.link.is_joined(), "joined");
            assert!(app.panel.is_some(), "the list stays up to watch it");
            assert!(app.status.contains("joined"), "{}", app.status);

            app.handle_panel_key(KeyCode::Char(' '), false)
                .expect("handled");
            assert!(!app.link.is_joined(), "left");
            assert!(app.status.contains("left"), "{}", app.status);
        }

        #[test]
        fn a_refused_link_clears_the_checkbox() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.link = LinkFollow::Joined { seen_playing: None };
            app.handle_control(StudioControlEvent::LinkRefused);
            assert!(!app.link.is_joined(), "the box shows what the engine did");
            assert!(app.status.contains("not joined"), "{}", app.status);
        }

        #[test]
        fn a_session_that_starts_playing_starts_this_studio_too() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.link = LinkFollow::Joined { seen_playing: None };
            // The session stands idle first, so the start is a change.
            app.snapshot = Some(session(false));
            app.pump_link();

            app.snapshot = Some(session(true));
            app.pump_link();
            let last = app.log.lines().back().expect("something was done");
            assert!(
                last.text.contains("update -"),
                "the score plays: {}",
                last.text
            );

            // The same state again is our own echo, or a steady session:
            // nothing is sent a second time.
            let sent = last.text.to_owned();
            app.pump_link();
            let still = app.log.lines().back().expect("something was done");
            assert_eq!(still.text, sent, "one start, one update");
        }

        #[test]
        fn a_session_that_stops_playing_stops_this_studio_too() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.link = LinkFollow::Joined { seen_playing: None };
            // A studio already playing, as one standing in a session is.
            let mut steady = session(true);
            steady.playing = true;
            app.snapshot = Some(steady);
            app.pump_link();

            // The engine has not answered the stop yet, so the studio still
            // reads as playing when the session's stop arrives.
            let mut stopped = session(false);
            stopped.playing = true;
            app.snapshot = Some(stopped);
            app.pump_link();
            let last = app.log.lines().back().expect("something was done");
            assert!(
                last.text.contains("stop -"),
                "the set goes quiet: {}",
                last.text
            );
        }

        /// Leaving the session ends the follow. The last snapshot still
        /// carries the session's transport until the engine has left it,
        /// and that word does not start this studio.
        #[test]
        fn leaving_the_session_does_not_start_this_studio() {
            rustel_runtime::link::keep_the_session_off_the_wire();
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            // The engine's queue can be full for a moment after the start,
            // so the box is pressed until the engine takes the word.
            let link = |app: &mut App, on: bool, said: &str| {
                for _ in 0..400 {
                    app.set_link(on);
                    if app.status.contains(said) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                panic!("the engine never took the link word: {}", app.status);
            };
            link(&mut app, true, "joined");
            // The session plays and this studio does not, as when the
            // update that followed the session was refused.
            app.snapshot = Some(session(true));
            app.pump_link();
            app.pending_evaluation = None;
            app.inflight.clear();

            link(&mut app, false, "left");
            let next = app.next_request_id;
            app.pump_link();
            assert_eq!(
                app.next_request_id, next,
                "leaving the session started the score: {}",
                app.status
            );
        }

        #[test]
        fn with_no_session_to_stand_in_nothing_is_sent() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.link = LinkFollow::Joined { seen_playing: None };
            app.snapshot = Some(StudioSnapshot::default());
            app.pump_link();
            assert!(
                app.log
                    .lines()
                    .back()
                    .is_none_or(|line| !line.text.contains("update -")),
                "no session, no start"
            );
        }
    }
}
