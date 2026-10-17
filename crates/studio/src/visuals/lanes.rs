//! The lanes view: the score's lanes as rows on a time axis that scrolls
//! past a fixed playhead. What has played runs off to the left, dimmed, and
//! the engine's lookahead fills the right with what is coming. A Rustel
//! extension, drawn by `_lanes()` under a pattern, by `lanes()` behind the
//! score, and by the Lanes panel.
//!
//! ```text
//!            ▼
//! kick ·▮···▮┊··▮···▮···▮···
//! hats ¦·¦·¦·┊¦·¦·¦·¦·¦·¦·¦·
//! bass ▃▃▃·▅▅┊▅▅·▂▂▂▂·▃▃▃·▅▅
//!       12.4 ◆
//!            ▲
//! ```
//!
//! A row is one sound of one lane, and a lane the score named that plays
//! more than one sound heads its rows with its label. A lane is a labeled
//! statement of the score, found from the source ranges an event carries. An event whose
//! ranges all lie outside the labeled statements, as a pattern kept in a
//! variable does, still gets a row for its sound, without a lane label.
//! The rows of a lane share its colour, and a sound keeps its mark in every
//! lane. Behind the score the rows keep to a strip along the bottom of the
//! stage.

use std::ops::Range;
use std::sync::Arc;

use super::*;

/// The marks of the sounds without a pitch, chosen by the sound's name.
const HITS: [char; 4] = ['▮', '¦', '×', '▪'];
/// A pitched row draws each note as a block whose height is its pitch
/// within the row's range.
const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// The most cells a row's label takes from the past.
const MAX_LABEL: usize = 16;

pub(super) struct Lanes<'a, 'b> {
    request: &'a VisualRequest<'b>,
    cycles: f64,
    playhead: f64,
    labels: bool,
}

impl<'a, 'b> Lanes<'a, 'b> {
    /// Three cycles with the playhead a quarter of the way in: the engine
    /// looks 2.5 cycles ahead, so the right edge stays filled.
    pub(super) fn new(request: &'a VisualRequest<'b>) -> Self {
        let options = request.options;
        Self {
            request,
            cycles: options.number("cycles").unwrap_or(3.0).clamp(0.25, 64.0),
            playhead: options.number("playhead").unwrap_or(0.25).clamp(0.0, 1.0),
            labels: options.flag("labels").unwrap_or(true),
        }
    }
}

/// One row: the events of one sound in one lane.
struct Row<'e> {
    /// The labeled statement, by its place in the score. `None` outside
    /// every one.
    lane: Option<usize>,
    /// The `s` the events name, without its `:n`. Empty when they name none.
    sound: &'e str,
    /// The lowest and highest pitch, in MIDI notes, when the events have one.
    pitch: Option<(f32, f32)>,
}

/// What one line of the view holds.
#[derive(Clone, Copy)]
enum Line {
    /// The heading of a lane the score named that plays more than one
    /// sound, by the index of its first row.
    Lane(usize),
    /// A row of events, by its index.
    Row(usize),
}

impl Widget for Lanes<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let request = self.request;
        let state = request.state;
        let theme = request.theme;
        let Some((now_seconds, cycle_now, cps)) = state.current_clock() else {
            note(request, buffer, area, "waiting for the first beat");
            return;
        };
        if area.width < 4 || area.height == 0 {
            return;
        }
        let begin = cycle_now - self.cycles * self.playhead;
        let end = begin + self.cycles;
        let placed = placed_events(state, request.slot, begin, end, now_seconds, cps);

        // Each event's row, and its pitch when it has one.
        let mut rows: Vec<Row<'_>> = Vec::new();
        let mut marks = Vec::with_capacity(placed.len());
        for entry in &placed {
            let lane = lane_of(&state.lanes, &entry.event.context);
            let sound = sound_of(entry.event);
            let index = rows
                .iter()
                .position(|row| row.lane == lane && row.sound == sound)
                .unwrap_or_else(|| {
                    rows.push(Row {
                        lane,
                        sound,
                        pitch: None,
                    });
                    rows.len() - 1
                });
            let midi = event_pitch(entry.event);
            if let Some(midi) = midi {
                let row = &mut rows[index];
                row.pitch = Some(
                    row.pitch
                        .map_or((midi, midi), |(low, high)| (low.min(midi), high.max(midi))),
                );
            }
            marks.push((index, midi));
        }
        // Lanes in score order with the unlabeled last, and sounds by name
        // within a lane.
        let mut order = (0..rows.len()).collect::<Vec<_>>();
        order.sort_by_key(|&index| {
            (
                rows[index].lane.is_none(),
                rows[index].lane,
                rows[index].sound,
            )
        });
        // The lines, top to bottom: each row, under a heading for a lane the
        // score named that plays more than one sound.
        let grouped = |row: &Row<'_>| {
            lane_name(row, &state.lanes).is_some()
                && rows.iter().filter(|other| other.lane == row.lane).count() > 1
        };
        let mut lines = Vec::with_capacity(rows.len() + 4);
        let mut line_of = vec![0; rows.len()];
        for (rank, &index) in order.iter().enumerate() {
            let row = &rows[index];
            let first = rank == 0 || rows[order[rank - 1]].lane != row.lane;
            if first && grouped(row) {
                lines.push(Line::Lane(index));
            }
            line_of[index] = lines.len();
            lines.push(Line::Row(index));
        }
        // The theme's colours, each once. A theme of one colour keeps it.
        let mut palette: Vec<Color> = Vec::with_capacity(9);
        for color in [
            theme.event,
            theme.accent,
            theme.syntax.string,
            theme.syntax.punctuation,
            theme.error,
        ]
        .into_iter()
        .chain(theme.syntax.function)
        .chain(theme.syntax.keyword)
        .chain([theme.syntax.number, theme.mini])
        {
            if !palette.contains(&color) {
                palette.push(color);
            }
        }
        // A row's colour follows its lane's place in the score, and its mark
        // the sound's name, so neither changes as other rows come and go. A
        // sound whose name chooses a mark another sound here already has
        // takes the next free one.
        let mut colors = vec![theme.event; rows.len()];
        let mut hits = vec![HITS[0]; rows.len()];
        let mut chosen: Vec<(&str, usize)> = Vec::new();
        for &index in &order {
            let row = &rows[index];
            let place = row.lane.unwrap_or_else(|| name_hash(row.sound));
            colors[index] = palette[place % palette.len()];
            let mark = match chosen.iter().find(|(sound, _)| *sound == row.sound) {
                Some(&(_, mark)) => mark,
                None => {
                    let first = name_hash(row.sound) % HITS.len();
                    let mark = (0..HITS.len())
                        .map(|step| (first + step) % HITS.len())
                        .find(|mark| chosen.iter().all(|(_, taken)| taken != mark))
                        .unwrap_or(first);
                    chosen.push((row.sound, mark));
                    mark
                }
            };
            hits[index] = HITS[mark];
        }

        // Behind the score the rows keep to a strip along the bottom of the
        // stage, a blank line between them, clear of the code at the top.
        let area = if request.inline {
            area
        } else {
            let height = (lines.len().max(1) * 2 + 3).min(usize::from(area.height)) as u16;
            Rect::new(area.x, area.bottom() - height, area.width, height)
        };

        // The caps take the first and last lines when the lines leave room
        // for them. The lines share what is left, each on the middle line of
        // its share.
        let caps = usize::from(area.height) >= lines.len().max(1) + 2;
        let inner = if caps {
            Rect::new(area.x, area.y + 1, area.width, area.height - 2)
        } else {
            area
        };
        let shown = lines.len().clamp(1, usize::from(inner.height));
        let share = inner.height / shown as u16;
        let line = |rank: usize| inner.y + rank as u16 * share + (share - 1) / 2;
        let width = f64::from(area.width);
        // A cell column, one past either edge at most, so an event that
        // started long ago still has a start to draw from.
        let column = |cycle: f64| {
            ((cycle - begin) / self.cycles * width)
                .floor()
                .clamp(-1.0, width + 1.0) as i64
        };
        let playhead_x = area.x + ((self.playhead * width) as u16).min(area.width - 1);

        // A dotted line under each row, with a colon where a cycle starts. A
        // view with nothing to show yet still has one.
        for rank in 0..shown {
            if matches!(lines.get(rank), Some(Line::Lane(_))) {
                continue;
            }
            let y = line(rank);
            for x in area.x..area.right() {
                put(buffer, x, y, '·', theme.grid);
            }
            let mut cycle = begin.ceil();
            while cycle < end {
                if let Ok(x) = u16::try_from(column(cycle))
                    && x < area.width
                {
                    put(buffer, area.x + x, y, ':', theme.grid);
                }
                cycle += 1.0;
            }
        }
        for y in area.y..area.bottom() {
            put(buffer, playhead_x, y, '┊', theme.playhead);
        }

        // What sounds now is drawn last, over anything it shares a cell
        // with.
        for active in [false, true] {
            for (entry, &(index, midi)) in placed.iter().zip(&marks) {
                let rank = line_of[index];
                if entry.active != active || rank >= shown {
                    continue;
                }
                let base = request.event_color(entry.event).unwrap_or(colors[index]);
                let base = mix(request.background, base, gain_alpha(entry.event));
                let color = if entry.active {
                    brighten(base)
                } else if entry.end <= cycle_now {
                    dim(base, theme)
                } else {
                    base
                };
                let from = column(entry.begin);
                let (glyph, to) = match (midi, rows[index].pitch) {
                    (Some(midi), Some((low, high))) => {
                        let level = if high - low < 1.0 {
                            3
                        } else {
                            (((midi - low) / (high - low)) * 7.0).round() as usize
                        };
                        // A held note fills its cells, less one at its end
                        // so repeated notes stay apart.
                        let to = column(entry.end).max(from + 1);
                        (
                            LEVELS[level.min(7)],
                            if to - from >= 3 { to - 1 } else { to },
                        )
                    }
                    _ => (hits[index], from + 1),
                };
                for x in from.max(0)..to.min(i64::from(area.width)) {
                    put(buffer, area.x + x as u16, line(rank), glyph, color);
                }
            }
        }

        // A heading names its lane, and the lane's rows hang under it by a
        // branch, each with its sound. A lane of one sound names its row,
        // and a row of a lane with no name says its sound.
        if self.labels {
            let room = usize::from(playhead_x - area.x)
                .saturating_sub(1)
                .min(MAX_LABEL);
            for (rank, &item) in lines.iter().take(shown).enumerate() {
                let (Line::Lane(index) | Line::Row(index)) = item;
                let row = &rows[index];
                let color = colors[index];
                let (branch, label) = match item {
                    Line::Lane(_) => (None, lane_name(row, &state.lanes).unwrap_or_default()),
                    Line::Row(_) if grouped(row) => {
                        let last = !matches!(
                            lines.get(rank + 1),
                            Some(Line::Row(next)) if rows[*next].lane == row.lane
                        );
                        (Some(if last { " └ " } else { " ├ " }), row.sound)
                    }
                    Line::Row(_) => (None, lane_name(row, &state.lanes).unwrap_or(row.sound)),
                };
                let y = line(rank);
                let mut x = area.x;
                let mut left = room;
                if let Some(branch) = branch {
                    if left < 3 {
                        continue;
                    }
                    buffer.set_stringn(x, y, branch, 3, Style::default().fg(dim(color, theme)));
                    x += 3;
                    left -= 3;
                }
                if label.is_empty() || left == 0 {
                    continue;
                }
                buffer.set_stringn(x, y, format!("{label} "), left, Style::default().fg(color));
            }
        }

        // The cycle the playhead is on, beside it, half way down: on a line
        // of its own, else on a heading, whose label keeps to the left, else
        // over the middle row.
        if inner.height >= 3 {
            let middle = inner.y + (inner.height - 1) / 2;
            let held = |y: u16| {
                (0..shown).find(|&rank| line(rank) == y).map_or(0, |rank| {
                    if matches!(lines.get(rank), Some(Line::Lane(_))) {
                        1
                    } else {
                        2
                    }
                })
            };
            let y = (inner.y..inner.bottom())
                .map(|y| (held(y), y.abs_diff(middle), y))
                .filter(|(held, _, _)| *held < 2)
                .min()
                .map_or(middle, |(_, _, y)| y);
            let text = format!("{cycle_now:.1} ");
            let from = playhead_x.saturating_sub(text.len() as u16).max(area.x);
            buffer.set_stringn(
                from,
                y,
                &text,
                usize::from(playhead_x - from),
                Style::default().fg(theme.accent),
            );
            let (diamond, color) = if state.running() {
                ('◆', theme.ok)
            } else {
                ('◇', theme.muted)
            };
            put(buffer, playhead_x, y, diamond, color);
        }
        if caps {
            put(buffer, playhead_x, area.y, '▼', theme.playhead);
            put(buffer, playhead_x, area.bottom() - 1, '▲', theme.playhead);
        }
    }
}

fn put(buffer: &mut Buffer, x: u16, y: u16, symbol: char, color: Color) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.set_char(symbol).set_fg(color);
    }
}

/// A name's place in a short list, the same in every picture.
fn name_hash(name: &str) -> usize {
    name.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    }) as usize
}

/// The labeled statement an event comes from: the first of its source
/// ranges that starts inside one.
fn lane_of(lanes: &[(Range<usize>, Arc<str>)], context: &[(usize, usize)]) -> Option<usize> {
    context.iter().find_map(|&(from, _)| {
        let index = lanes
            .partition_point(|(span, _)| span.start <= from)
            .checked_sub(1)?;
        lanes[index].0.contains(&from).then_some(index)
    })
}

/// The sound an event names: `bd` for `s:bd n:3` and for `s:bd:3`.
fn sound_of(event: &UiScheduledEvent) -> &str {
    event
        .value
        .as_deref()
        .and_then(|value| {
            value
                .split_whitespace()
                .find_map(|field| field.strip_prefix("s:"))
        })
        .map_or("", |sound| sound.split(':').next().unwrap_or(sound))
}

/// The label of a row's lane, when the score named the lane. `$` lanes have
/// no name of their own.
fn lane_name<'l>(row: &Row<'_>, lanes: &'l [(Range<usize>, Arc<str>)]) -> Option<&'l str> {
    row.lane
        .map(|lane| &*lanes[lane].1)
        .filter(|label| !label.contains('$'))
}
