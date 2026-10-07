//! The DVD logo, bouncing off the edges and changing colour when it hits.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct DvdBounce {
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) secondary: Color,
    pub(super) speed: u8,
}

pub(super) const DVD_VIDEO_ART: &str = include_str!("../themes/dvd-video.txt");

impl DvdBounce {
    pub(super) fn new(active: &ActiveVisual) -> Self {
        Self {
            background: active.background,
            foreground: active.foreground,
            accent: active.accent,
            secondary: active.secondary,
            speed: active.config.speed,
        }
    }

    pub(super) fn art_dimensions() -> (u16, u16) {
        let height = DVD_VIDEO_ART.lines().count() as u16;
        let width = DVD_VIDEO_ART
            .lines()
            .map(str::len)
            .max()
            .unwrap_or_default() as u16;
        (width, height)
    }

    /// Keep the supplied dot art byte-for-byte whenever it fits. Small
    /// terminals sample it uniformly instead of substituting another logo.
    pub(super) fn raster(area: Rect) -> (u16, u16, u16, u16) {
        let (source_width, source_height) = Self::art_dimensions();
        let target_width = area.width.saturating_sub(2).max(1);
        let target_height = area.height.saturating_sub(1).max(1);
        let step_x = source_width.div_ceil(target_width).max(1);
        let step_y = source_height.div_ceil(target_height).max(1);
        (
            step_x,
            step_y,
            source_width.div_ceil(step_x),
            source_height.div_ceil(step_y),
        )
    }

    pub(super) fn position(&self, elapsed_ms: u64, area: Rect) -> (u16, u16, u64) {
        let (_, _, logo_width, logo_height) = Self::raster(area);
        let max_x = area.width.saturating_sub(logo_width);
        let max_y = area.height.saturating_sub(logo_height);
        let seconds = elapsed_ms as f32 / 1000.0;
        // Cell terminals have no sub-pixel motion. Crossing roughly twenty
        // columns a second reads as movement; the old six-column pace read
        // as an occasional jump.
        let pace = 10.0 + f32::from(self.speed) * 0.2;
        let x_distance = seconds * pace;
        let y_distance = seconds * pace * 0.47;
        let (x, x_bounces) = bounced_axis(x_distance, max_x);
        let (y, y_bounces) = bounced_axis(y_distance, max_y);
        (area.x + x, area.y + y, x_bounces + y_bounces)
    }
}

impl NativeScene for DvdBounce {
    fn name(&self) -> &'static str {
        "rustel_dvd_bounce"
    }

    fn render(&self, elapsed_ms: u64, _audio: ReactiveAudio, buffer: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        let (step_x, step_y, logo_width, logo_height) = Self::raster(area);
        let (left, top, bounces) = self.position(elapsed_ms, area);
        let color = cycle_color(
            self.accent,
            self.secondary,
            self.foreground,
            bounces as f32 * 0.173,
        );
        for output_y in 0..logo_height {
            let source_y = usize::from(output_y * step_y);
            let Some(line) = DVD_VIDEO_ART.lines().nth(source_y) else {
                continue;
            };
            for output_x in 0..logo_width {
                let source_x = usize::from(output_x * step_x);
                if line.as_bytes().get(source_x) != Some(&b'.') {
                    continue;
                }
                let Some(cell) = buffer.cell_mut((left + output_x, top + output_y)) else {
                    continue;
                };
                if cell.bg != self.background
                    || (cell.symbol() != " " && cell.symbol().width() != 1)
                {
                    continue;
                }
                if cell.symbol() == " " {
                    cell.set_symbol(".");
                }
                cell.set_fg(color);
                cell.modifier.insert(Modifier::BOLD);
            }
        }
    }
}

pub(super) fn bounced_axis(distance: f32, extent: u16) -> (u16, u64) {
    if extent == 0 {
        return (0, 0);
    }
    let extent_f = f32::from(extent);
    let leg = (distance / extent_f).floor() as u64;
    let offset = distance.rem_euclid(extent_f);
    let position = if leg & 1 == 0 {
        offset
    } else {
        extent_f - offset
    };
    (position.round().clamp(0.0, extent_f) as u16, leg)
}
