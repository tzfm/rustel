//! Background jobs: a header chip and a small list sheet.
//!
//! Sample downloads, exports, and anything else that runs off the UI thread
//! show up here so progress stays visible after Settings is closed. Click the
//! chip or open the sheet to see every running task with a name and percent.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::Widget,
};
use unicode_width::UnicodeWidthStr;

use super::devices::draw_border;
use super::theme::Theme;

/// One background task as the list and chip read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundJob {
    /// Short name: pack cache, cache all, export, …
    pub name: String,
    /// How far along, when known. Missing means the job has no total yet.
    pub percent: Option<u8>,
}

impl BackgroundJob {
    /// One line for the list: `name · 42%` or `name · …`.
    pub fn line(&self) -> String {
        match self.percent {
            Some(percent) => format!("{} · {percent}%", self.name),
            None => format!("{} · …", self.name),
        }
    }

    /// Compact chip text for a single job.
    pub fn chip(&self) -> String {
        match self.percent {
            Some(percent) => format!("{} {} {percent}%", jobs_mark(), self.name),
            None => format!("{} {}…", jobs_mark(), self.name),
        }
    }
}

/// The chip's leading mark: a spinner while animation is on, so the
/// header itself says work is moving; ⇣ when motion is off.
pub fn jobs_mark() -> &'static str {
    jobs_mark_at(super::settings::animation(), std::time::SystemTime::now())
}

fn jobs_mark_at(animate: bool, now: std::time::SystemTime) -> &'static str {
    if !animate {
        return "⇣";
    }
    const FRAMES: [&str; 4] = ["⠋", "⠙", "⠸", "⠴"];
    let frame = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| (age.as_millis() / 120) as usize)
        .unwrap_or(0)
        % FRAMES.len();
    FRAMES[frame]
}

/// Summary for the header when any jobs are running.
pub fn jobs_chip(jobs: &[BackgroundJob]) -> Option<String> {
    let mark = jobs_mark();
    match jobs {
        [] => None,
        [only] => Some(only.chip()),
        many => Some(format!("{mark} {} jobs", many.len())),
    }
}

/// Percent from a done/total count, capped at 100.
pub fn percent_of(done: usize, total: usize) -> u8 {
    if total == 0 {
        return 0;
    }
    ((done.saturating_mul(100)) / total).min(100) as u8
}

/// The jobs sheet: a short list of running background work.
#[derive(Clone, Debug, Default)]
pub struct JobsPanel {
    pub selected: usize,
    scroll: std::cell::Cell<usize>,
}

impl JobsPanel {
    pub fn opened() -> Self {
        Self::default()
    }

    pub fn clamp(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(len - 1);
        }
    }

    pub fn step(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        let at = self.selected as isize + delta;
        self.selected = at.rem_euclid(len as isize) as usize;
    }

    /// Page keys stop at the list ends instead of wrapping to another job.
    pub fn page(&mut self, forwards: bool, rows: usize, len: usize) {
        self.clamp(len);
        self.selected = if forwards {
            self.selected
                .saturating_add(rows.max(1))
                .min(len.saturating_sub(1))
        } else {
            self.selected.saturating_sub(rows.max(1))
        };
    }

    fn first_row(&self, rows: usize, len: usize) -> usize {
        let rows = rows.max(1);
        let first = super::scroll::follow(
            self.scroll.get(),
            self.selected.min(len.saturating_sub(1)),
            rows,
            len,
            super::scroll::margin(rows),
        );
        self.scroll.set(first);
        first
    }
}

/// Where the header drew its jobs chip this frame, packed x/y/width.
static JOBS_CHIP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn set_jobs_chip(rect: Option<(u16, u16, u16)>) {
    let packed = rect.map_or(0, |(x, y, width)| {
        (u64::from(x) << 32) | (u64::from(y) << 16) | u64::from(width)
    });
    JOBS_CHIP.store(packed, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a pointer position is on the header's jobs chip.
pub fn jobs_chip_at(x: u16, y: u16) -> bool {
    let packed = JOBS_CHIP.load(std::sync::atomic::Ordering::Relaxed);
    if packed == 0 {
        return false;
    }
    let chip_x = (packed >> 32) as u16;
    let chip_y = (packed >> 16) as u16;
    let width = packed as u16;
    y == chip_y && x >= chip_x && x < chip_x.saturating_add(width)
}

pub struct JobsPanelView<'a> {
    pub panel: &'a JobsPanel,
    pub jobs: &'a [BackgroundJob],
    pub theme: &'a Theme,
    pub focused: bool,
}

impl JobsPanelView<'_> {
    /// A compact sheet near the top, under the header.
    pub fn geometry(available: Rect, job_count: usize) -> Option<(Rect, Rect)> {
        if available.height < 8 || available.width < 28 {
            return None;
        }
        let rows = (job_count.max(1) as u16).saturating_add(3).clamp(5, 14);
        let height = rows.min(available.height.saturating_sub(2));
        let width = available.width.saturating_sub(4).clamp(28, 56);
        let area = Rect::new(
            available.x + 2,
            available.y.saturating_add(2),
            width,
            height,
        );
        let list = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(3),
        );
        Some((area, list))
    }

    pub fn sheet_area(available: Rect, job_count: usize) -> Option<Rect> {
        Self::geometry(available, job_count).map(|(sheet, _)| sheet)
    }
}

impl Widget for JobsPanelView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some((panel, list)) = Self::geometry(area, self.jobs.len()) else {
            return;
        };
        let theme = self.theme;
        super::view::clear_overlay(
            buffer,
            panel,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        draw_border(buffer, panel, theme);
        let title = if self.jobs.is_empty() {
            " background jobs - none running ".to_owned()
        } else {
            format!(" {} background jobs ", jobs_mark())
        };
        buffer.set_stringn(
            panel.x + 2,
            panel.y,
            title,
            usize::from(panel.width.saturating_sub(4)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        if self.jobs.is_empty() {
            buffer.set_stringn(
                list.x,
                list.y,
                "nothing in the background right now",
                usize::from(list.width),
                Style::default().fg(theme.muted),
            );
        } else {
            let first = self
                .panel
                .first_row(usize::from(list.height), self.jobs.len());
            for (index, job) in self
                .jobs
                .iter()
                .enumerate()
                .skip(first)
                .take(usize::from(list.height))
            {
                let selected = self.focused && index == self.panel.selected;
                let marker = if selected {
                    format!("{} ", crate::terminal::symbol("▸"))
                } else {
                    "  ".to_owned()
                };
                let text = format!("{marker}{}", job.line());
                let style = if selected {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.foreground)
                };
                buffer.set_stringn(
                    list.x,
                    list.y + (index - first) as u16,
                    &text,
                    usize::from(list.width),
                    style,
                );
            }
        }
        let hint = if self.jobs.len() > usize::from(list.height) {
            "PgUp/PgDn page · Esc closes"
        } else {
            "Esc closes"
        };
        let hint_width = UnicodeWidthStr::width(hint) as u16;
        buffer.set_stringn(
            panel.x + 2,
            panel.bottom().saturating_sub(1),
            hint,
            usize::from(hint_width.min(panel.width.saturating_sub(4))),
            Style::default().fg(theme.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_summarises_one_or_many() {
        let one = [BackgroundJob {
            name: "drums".into(),
            percent: Some(40),
        }];
        let chip = jobs_chip(&one).expect("one job");
        assert!(chip.contains("drums 40%"), "chip carries the job: {chip}");
        let many = [
            BackgroundJob {
                name: "a".into(),
                percent: Some(10),
            },
            BackgroundJob {
                name: "b".into(),
                percent: None,
            },
        ];
        let chip = jobs_chip(&many).expect("two jobs");
        assert!(chip.contains("2 jobs"), "chip counts them: {chip}");
        assert_eq!(jobs_chip(&[]), None);
    }

    #[test]
    fn the_jobs_mark_spins_when_animation_is_on() {
        assert_eq!(jobs_mark_at(false, std::time::UNIX_EPOCH), "⇣");
        assert_eq!(jobs_mark_at(true, std::time::UNIX_EPOCH), "⠋");
        let later = std::time::UNIX_EPOCH + std::time::Duration::from_millis(120);
        assert_eq!(jobs_mark_at(true, later), "⠙");
    }

    #[test]
    fn jobs_pages_clamp_and_render_the_selected_job() {
        let jobs: Vec<_> = (0..35)
            .map(|index| BackgroundJob {
                name: format!("job-{index:02}"),
                percent: None,
            })
            .collect();
        let theme = Theme::built_in_default();
        for frame in [Rect::new(0, 0, 100, 30), Rect::new(0, 0, 100, 10)] {
            let (_, list) = JobsPanelView::geometry(frame, jobs.len()).unwrap();
            let rows = usize::from(list.height);
            let mut panel = JobsPanel::opened();
            for _ in 0..20 {
                panel.page(true, rows, jobs.len());
                let first = panel.first_row(rows, jobs.len());
                assert!(panel.selected >= first && panel.selected < first + rows);
                let mut buffer = Buffer::empty(frame);
                JobsPanelView {
                    panel: &panel,
                    jobs: &jobs,
                    theme: &theme,
                    focused: true,
                }
                .render(frame, &mut buffer);
                let text = buffer
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(
                    text.contains(&format!("job-{:02}", panel.selected)),
                    "{text}"
                );
            }
            assert_eq!(panel.selected, jobs.len() - 1);
            for _ in 0..20 {
                panel.page(false, rows, jobs.len());
            }
            assert_eq!(panel.selected, 0);
            assert_eq!(panel.first_row(rows, jobs.len()), 0);
            panel.page(true, rows, 0);
            assert_eq!(panel.selected, 0);
        }
    }

    #[test]
    fn percent_of_caps_and_guards_zero() {
        assert_eq!(percent_of(0, 0), 0);
        assert_eq!(percent_of(1, 4), 25);
        assert_eq!(percent_of(5, 4), 100);
    }
}
