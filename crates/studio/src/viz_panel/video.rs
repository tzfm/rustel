//! The video widget: one clip of the set, its picture where its sound is,
//! with ←/→ choosing which clip feeds it. The painter's own lane is not
//! involved - any video the set sounds can feed the widget.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use super::super::theme::Theme;
use super::super::visuals::{Canvas, VideoEntry, VisualState};

/// The video widget: the chosen clip's frame, drawn like the painter
/// draws it - small, on the dock's own tier - with the feed named in the
/// header and its state said in place of a picture it does not have.
pub struct VideoView<'a> {
    pub state: &'a VisualState,
    pub feed: Option<&'a str>,
    /// The clips playing in detached windows: this widget folds to a note
    /// while its clip plays elsewhere.
    pub windows: &'a [String],
    pub theme: &'a Theme,
}

impl VideoView<'_> {
    /// The feed the widget draws: the one the widget chose, else the
    /// first the set has.
    fn chosen<'b>(&'b self, feeds: &'b [String]) -> Option<&'b str> {
        let fallback = feeds.first().map(String::as_str)?;
        let chosen = self
            .feed
            .filter(|feed| feeds.iter().any(|known| known == *feed));
        Some(chosen.unwrap_or(fallback))
    }
}

impl Widget for VideoView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let feeds = self.state.video_feeds();
        if feeds.is_empty() {
            self.say(
                buffer,
                area,
                "no video clips yet - the set's sounds answer as they play",
            );
            return;
        }
        let Some(feed) = self.chosen(&feeds) else {
            return;
        };
        if self.windows.iter().any(|detached| detached == feed) {
            self.say(
                buffer,
                area,
                &format!("{feed} plays in the window - Enter takes it back"),
            );
            return;
        }
        let Some((position, strip)) = self.state.video_feed_frame(feed) else {
            match self.state.video_for(feed) {
                Some(VideoEntry::Failed(message)) => {
                    self.say(buffer, area, &format!("{feed}: {message}"));
                }
                _ => self.say(buffer, area, &format!("waiting for {feed}")),
            }
            return;
        };
        let Some(frame) = strip.frame_at(position as f32) else {
            self.say(buffer, area, &format!("{feed} decoded to no frames"));
            return;
        };
        let mut canvas = Canvas::bars(area);
        let (canvas_width, canvas_height) = (canvas.width(), canvas.height());
        let (frame_width, frame_height) = (strip.width() as usize, strip.height() as usize);
        // The frame keeps its shape inside the canvas points and sits
        // centred, however much larger or smaller it is than the slot.
        let scale = (canvas_width as f64 / frame_width.max(1) as f64)
            .min(canvas_height as f64 / frame_height.max(1) as f64);
        let fit = |frame_side: usize, canvas_side: usize| {
            (((frame_side as f64 * scale).round().max(1.0)) as usize).min(canvas_side)
        };
        let (draw_width, draw_height) = (
            fit(frame_width, canvas_width),
            fit(frame_height, canvas_height),
        );
        let (x0, y0) = (
            (canvas_width - draw_width) / 2,
            (canvas_height - draw_height) / 2,
        );
        for y in 0..draw_height {
            let source_row = (y * frame_height / draw_height) * frame_width;
            for x in 0..draw_width {
                let at = (source_row + x * frame_width / draw_width) * 4;
                if let [red, green, blue, ..] = frame.get(at..at + 3).unwrap_or_default() {
                    canvas.set(x0 + x, y0 + y, Color::Rgb(*red, *green, *blue));
                }
            }
        }
        canvas.paint(buffer, self.theme.background);
    }
}

impl VideoView<'_> {
    /// Say in the widget's slot why it drew no picture.
    fn say(&self, buffer: &mut Buffer, area: Rect, message: &str) {
        buffer.set_stringn(
            area.x,
            area.y,
            message,
            usize::from(area.width),
            Style::default().fg(self.theme.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    //! The video widget: what it says while no picture is there, and
    //! what it draws when one is.

    use super::super::super::theme::Theme;
    use super::*;
    use crate::visuals::VideoEntry;
    use std::sync::Arc;

    fn strip() -> rustel_runtime::video::VideoThumbnails {
        rustel_runtime::video::VideoThumbnails::from_frames(
            2,
            2,
            vec![(0.0, [255, 0, 0, 255].repeat(4))],
        )
        .expect("strip")
    }

    fn view<'a>(
        state: &'a VisualState,
        feed: Option<&'a str>,
        windows: &'a [String],
    ) -> VideoView<'a> {
        static THEME: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
        VideoView {
            state,
            feed,
            windows,
            theme: THEME.get_or_init(|| Theme::built_in("terminal").expect("terminal ships")),
        }
    }

    fn drawn(state: &VisualState, feed: Option<&str>, windows: &[String]) -> String {
        let area = Rect::new(0, 0, 40, 10);
        let mut buffer = Buffer::empty(area);
        view(state, feed, windows).render(area, &mut buffer);
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn a_dock_with_no_clips_says_so() {
        let state = VisualState::default();
        let text = drawn(&state, None, &[]);
        assert!(text.contains("no video clips"), "{text}");
    }

    #[test]
    fn a_clip_the_window_took_folds_the_widget_to_a_note() {
        let mut state = VisualState::default();
        assert!(state.install_video("clip.webm", VideoEntry::Asking(1)));
        let windows = vec!["clip.webm".to_owned()];
        let text = drawn(&state, Some("clip.webm"), &windows);
        assert!(
            text.contains("plays in the window"),
            "the widget collapsed: {text}"
        );
        assert!(
            !text.contains("waiting"),
            "no waiting note while it is detached"
        );
        // Another clip's window leaves this widget's own picture alone.
        let other = vec!["other.webm".to_owned()];
        let text = drawn(&state, Some("clip.webm"), &other);
        assert!(text.contains("waiting for clip.webm"), "{text}");
    }

    #[test]
    fn a_feed_without_a_strip_says_it_is_waiting() {
        let state = VisualState::default();
        // Nothing asked: no feeds at all. Asked and answered: the first
        // feed draws while its strip has not landed.
        let mut state = state;
        assert!(state.install_video("clip.webm", VideoEntry::Asking(1)));
        let text = drawn(&state, None, &[]);
        assert!(text.contains("waiting for clip.webm"), "{text}");
        // A refused clip names its reason.
        assert!(state.install_video(
            "clip.webm",
            VideoEntry::Failed("the picture is H.264".to_owned())
        ));
        let text = drawn(&state, None, &[]);
        assert!(text.contains("H.264"), "{text}");
    }

    #[test]
    fn a_landed_strip_draws_the_widget_s_poster_frame() {
        let mut state = VisualState::default();
        assert!(state.install_video("clip.webm", VideoEntry::Ready(Arc::new(strip()))));
        let area = Rect::new(0, 0, 20, 8);
        let mut buffer = Buffer::empty(area);
        view(&state, None, &[]).render(area, &mut buffer);
        let red = buffer
            .content
            .iter()
            .filter(|cell| cell.fg == ratatui::style::Color::Rgb(255, 0, 0))
            .count();
        assert!(red > 50, "the poster frame filled the slot: {red} cells");
    }

    #[test]
    fn the_feed_stepper_wraps_and_clamps_to_what_the_set_has() {
        let feeds: Vec<String> = ["a.webm", "b.webm", "c.webm"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let mut spec = super::super::WidgetSpec::new(super::super::WidgetKind::Video);
        assert!(spec.step_feed(true, &feeds));
        assert_eq!(spec.feed.as_deref(), Some("b.webm"));
        assert!(spec.step_feed(true, &feeds));
        assert!(spec.step_feed(true, &feeds));
        assert_eq!(spec.feed.as_deref(), Some("a.webm"), "the step wraps");
        spec.step_feed(false, &feeds);
        assert_eq!(spec.feed.as_deref(), Some("c.webm"), "back the other way");
        // One feed: nothing to step, and the choice is pinned to it.
        let one = vec!["only.webm".to_owned()];
        assert!(!spec.step_feed(true, &one));
        assert_eq!(spec.feed.as_deref(), Some("only.webm"));
        // A feed the set no longer sounds steps from the first again.
        spec.feed = Some("gone.webm".to_owned());
        assert!(spec.step_feed(true, &feeds));
        assert_eq!(spec.feed.as_deref(), Some("b.webm"), "from the first");
    }
}
