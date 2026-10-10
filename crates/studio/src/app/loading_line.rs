//! The header's loading line: a thin line that grows along the bottom of
//! the header row while the sounds and the plugins the playing or the
//! waiting score needs come in, shown only once a load has lasted, and after
//! a while its words in the tempo's place.

use super::*;

/// A load shorter than this never shows, so a quick load does not flash.
pub(super) const LOADING_LINE_AFTER: Duration = Duration::from_millis(250);

/// After this long the line also says what it is loading.
pub(super) const LOADING_LABEL_AFTER: Duration = Duration::from_millis(1_500);

impl App {
    /// Follow the engine's loading cue: when the current load began, and a
    /// repaint when the line or its words become due or the load ends.
    pub(super) fn follow_loading_line(&mut self, now: Instant) {
        let loading = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.loading.is_some());
        match (loading, self.loading_since) {
            (true, None) => self.loading_since = Some(now),
            (false, Some(_)) => {
                self.loading_since = None;
                self.dirty_frame = true;
            }
            _ => {}
        }
        let due = self.loading_since.map_or((false, false), |since| {
            let lasted = now.saturating_duration_since(since);
            (lasted >= LOADING_LINE_AFTER, lasted >= LOADING_LABEL_AFTER)
        });
        if due != self.loading_due {
            self.loading_due = due;
            self.dirty_frame = true;
        }
    }

    /// The line as the header draws it at `now`, if it is due. A terminal
    /// that reports 24-bit colour draws a coloured underline (SGR 58, which
    /// the same terminals implement); one that does not gets a faint tint,
    /// since an underline colour it does not understand is at best ignored.
    pub(super) fn loading_line(&self, now: Instant) -> Option<view::LoadingLine> {
        let cue = self.snapshot.as_ref()?.loading.as_ref()?;
        let lasted = now.saturating_duration_since(self.loading_since?);
        if lasted < LOADING_LINE_AFTER || cue.total == 0 {
            return None;
        }
        let label = (lasted >= LOADING_LABEL_AFTER).then(|| {
            let words = if cue.waiting {
                "waiting for"
            } else {
                "loading"
            };
            // A plugin wait is named as a sound wait is.
            let what = if cue.plugins == 0 {
                "sounds"
            } else if cue.plugins < cue.total - cue.settled {
                "sounds and plugins"
            } else {
                "plugins"
            };
            let percent = cue.settled * 100 / cue.total;
            let mut label = format!(
                "{words} {what} · {}/{} · {percent}%",
                cue.settled, cue.total
            );
            if let Some(name) = &cue.loading {
                label.push_str(" · ");
                label.push_str(name);
            }
            label
        });
        Some(view::LoadingLine {
            reach: cue.settled as f64 / cue.total as f64,
            label,
            underline: self.features.truecolor,
        })
    }
}
