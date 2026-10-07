//! The events widget: every event as it is queued to sound, read as a
//! chain of what it carries.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use super::super::theme::{Theme, hsv, mix, rgb_parts};
use super::super::visuals::VisualState;

/// The events as they are queued to sound, newest at the bottom, each a
/// chain of what it carries - the sound first, then its bank, then the
/// controls, abbreviated - wrapped onto the next row when the column is
/// too narrow. The newest event stays visible even in a short viewport:
/// what has sounded dim, what is sounding lit,
/// what is coming fainter still.
pub struct EventsView<'a> {
    pub state: &'a VisualState,
    pub theme: &'a Theme,
}

/// One event's chain, in cells: the head, then the rest as links.
fn event_chain(event: &rustel_runtime::ui_events::UiScheduledEvent) -> (String, Vec<String>) {
    // The controls in the order the pipeline set them - the value keeps
    // its keys as they were added, so `s("bd").bank("tr808").dec(.4)`
    // reads bd ▸ tr808 ▸ dec .4 and a chain written the other way round
    // reads the other way round: the order is the point, it says why a
    // chain does what it does.
    let links: Vec<String> = event
        .value
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|token| token.split_once(':'))
        .filter_map(|(key, value)| match key {
            // The desk's own bookkeeping is nothing to look at, and the
            // label heads the chain.
            "label" | "activeLabel" | "ui_visuals" | "analyze" => None,
            key if key.starts_with('_') => None,
            "s" | "bank" => Some(value.to_owned()),
            "note" | "n" => Some(format!("{key} {}", trim_number(value))),
            _ => Some(format!("{} {}", short_key(key), trim_number(value))),
        })
        .collect();
    let label = event
        .active_label
        .as_deref()
        .or(event.label.as_deref())
        .map(str::to_owned);
    match label {
        Some(label) => (label, links),
        None => {
            let mut links = links.into_iter();
            let head = links.next().unwrap_or_else(|| "·".to_owned());
            (head, links.collect())
        }
    }
}

/// A control's name the way a desk labels it.
fn short_key(key: &str) -> &str {
    match key {
        "decay" => "dec",
        "release" => "rel",
        "attack" => "atk",
        "sustain" => "sus",
        "gain" => "g",
        "speed" => "spd",
        "velocity" => "vel",
        "cutoff" => "lpf",
        "hcutoff" => "hpf",
        "resonance" => "res",
        "delay" => "dly",
        "delaytime" => "dlyt",
        "delayfeedback" => "dlyfb",
        "distort" => "dist",
        "orbit" => "o",
        "begin" => "beg",
        "legato" => "leg",
        other => other,
    }
}

/// `0.4` as `.4`, `1.0` as `1`, `0.250000` as `.25`; words as they are.
fn trim_number(value: &str) -> String {
    let Ok(number) = value.parse::<f64>() else {
        return value.to_owned();
    };
    if number.fract() == 0.0 && number.abs() < 1e9 {
        return format!("{}", number as i64);
    }
    let text = format!("{number:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
    match text.strip_prefix("0.") {
        Some(rest) => format!(".{rest}"),
        None => match text.strip_prefix("-0.") {
            Some(rest) => format!("-.{rest}"),
            None => text,
        },
    }
}

/// The chain as rows of `width` cells: the head and as many links as fit
/// on the first row, the rest on rows under it, marked as a turn.
fn wrap_chain(head: &str, links: &[String], width: usize) -> Vec<String> {
    // Not a `const`: what this spells depends on the terminal, and a
    // console that cannot draw the arrow would print a row of boxes
    // between every link of the chain.
    let arrow = format!(" {} ", crate::terminal::symbol("▸"));
    let arrow = arrow.as_str();
    let turn = format!("  {} ", crate::terminal::symbol("↳"));
    let width = width.max(8);
    let mut rows = vec![head.chars().take(width).collect::<String>()];
    for link in links {
        let last = rows.last_mut().expect("a row");
        let joined = format!("{last}{arrow}{link}");
        if joined.chars().count() <= width {
            *last = joined;
        } else {
            let start = format!("{turn}{link}");
            rows.push(start.chars().take(width).collect());
        }
    }
    rows
}

impl Widget for EventsView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        let Some((now, _, _)) = self.state.current_clock() else {
            buffer.set_stringn(
                area.x,
                area.y,
                "…",
                usize::from(area.width),
                Style::default().fg(theme.muted),
            );
            return;
        };
        let mut events: Vec<_> = self
            .state
            .events()
            .filter(|event| event.target_time <= now + 0.25)
            .collect();
        events.sort_by(|a, b| {
            a.target_time
                .partial_cmp(&b.target_time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let width = usize::from(area.width);
        let rows = usize::from(area.height);
        // Prefer whole events. The newest event must remain visible even
        // when its controls wrap beyond the entire viewport.
        let mut lines: Vec<(String, Style)> = Vec::new();
        for event in events.iter().rev() {
            let (head, links) = event_chain(event);
            let mut chain = wrap_chain(&head, &links, width);
            if lines.len() + chain.len() > rows {
                if !lines.is_empty() {
                    break;
                }
                chain.truncate(rows);
                if let Some(last) = chain.last_mut() {
                    *last = format!(
                        "{}…",
                        last.chars()
                            .take(width.saturating_sub(1))
                            .collect::<String>()
                    );
                }
            }
            let colour = event
                .color
                .as_deref()
                .and_then(super::super::theme::parse_color)
                .unwrap_or_else(|| label_colour(head.as_str(), theme));
            let sounding = event.target_time <= now
                && now < event.target_time + event.duration_seconds.max(0.05);
            let style = if sounding {
                Style::default().fg(colour).add_modifier(Modifier::BOLD)
            } else if event.target_time > now {
                Style::default().fg(mix(theme.background, colour, 0.45))
            } else {
                Style::default().fg(mix(theme.background, colour, 0.7))
            };
            for row in chain.into_iter().rev() {
                lines.push((row, style));
            }
        }
        for (offset, (text, style)) in lines.iter().enumerate() {
            let y = area.bottom() - 1 - offset as u16;
            buffer.set_stringn(area.x, y, text, width, *style);
        }
    }
}

/// A steady colour for a sound's name, so `bd` is always the same one.
fn label_colour(name: &str, theme: &Theme) -> Color {
    let hash = name.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    let hue = (hash % 360) as f32;
    let (r, g, b) = rgb_parts(hsv(hue, 0.7, 0.95));
    let (tr, tg, tb) = rgb_parts(theme.foreground);
    // Toward the theme's foreground a little, so it reads on the surface.
    Color::Rgb(
        ((u16::from(r) * 3 + u16::from(tr)) / 4) as u8,
        ((u16::from(g) * 3 + u16::from(tg)) / 4) as u8,
        ((u16::from(b) * 3 + u16::from(tb)) / 4) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_with(value: &str) -> rustel_runtime::ui_events::UiScheduledEvent {
        rustel_runtime::ui_events::UiScheduledEvent {
            onset_id: 1,
            generation: 1,
            whole_begin: "0".into(),
            whole_end: "1".into(),
            part_begin: "0".into(),
            part_end: "1".into(),
            target_time: 0.0,
            duration_seconds: 0.5,
            value: Some(value.to_owned()),
            color: None,
            label: None,
            active_label: None,
            scale: None,
            frequency_hz: None,
            gain: None,
            ui_visuals: 0,
            context: Vec::new(),
        }
    }

    #[test]
    fn long_event_controls_remain_visible_in_a_vertical_viewport() {
        use rustel_runtime::ui_events::{UiEventBatch, UiScheduledEvent};
        let source = r#"$: note(`< [e5@2 d5 e5 g5@2 e5 d5] [c5 d5 e5 g5 a5@2 g5 e5] [e5 g5 a5@2 g5 e5 d5 c5] [g5@2 e5 d5 c5@2 d5 e5] >`)
            .s("supersaw").unison(6).detune(0.28).spread(0.9)
            .lpf(sine.range(1800,5200).slow(16))
            .attack(0.02).decay(0.25).sustain(0.45).release(0.35)
            .gain(0.38).delay(0.5).delaytime(0.375).delayfeedback(0.38)
            .room(0.55).vib(4).vibmod(0.12).orbit(4)._pitchwheel()"#;
        let mut session = rustel_runtime::Session::new().unwrap();
        session.evaluate(source).unwrap();
        let generation = session.generation();
        let envelope = rustel_runtime::ui_events::visual_layout(source, generation).unwrap();
        let revision = envelope.ui_layout.source_revision.clone();
        let events = session
            .preview_traces(0.0, 1.0, generation)
            .unwrap()
            .iter()
            .map(|trace| UiScheduledEvent::from_trace(trace, session.cps()).unwrap())
            .collect();
        let mut state = VisualState::default();
        state.install_layout(envelope);
        state.start();
        assert!(state.install_batch(
            UiEventBatch::new(0.0, 0.0, session.cps(), generation, revision, events, 0).unwrap()
        ));
        state.stop();
        for (width, height) in [(24, 36), (24, 5), (120, 6)] {
            let area = Rect::new(3, 2, width, height);
            let mut buffer = Buffer::empty(area);
            EventsView {
                state: &state,
                theme: &Theme::default(),
            }
            .render(area, &mut buffer);
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("supersaw"), "{width}x{height}: {text}");
            if height == 5 {
                assert!(text.contains('…'));
            }
            if height == 36 {
                assert!(text.contains("o 4"), "the whole event fits: {text}");
            }
        }
    }

    #[test]
    fn wrapped_events_use_same_width_ascii_arrows_on_conhost() {
        use rustel_runtime::ui_events::UiEventBatch;

        let envelope = rustel_runtime::ui_events::visual_layout("$: s(\"bd\")", 1).unwrap();
        let revision = envelope.ui_layout.source_revision.clone();
        let event = event_with("s:bd bank:tr808 decay:0.4 gain:1.0 orbit:0 cutoff:1200.5");
        let (head, links) = event_chain(&event);
        let modern = wrap_chain(&head, &links, 22);
        let _symbols = crate::terminal::ForceSymbolsForTest::set(false);
        let plain = wrap_chain(&head, &links, 22);
        assert_eq!(
            plain,
            ["bd > tr808 > dec .4", "  > g 1 > o 0", "  > lpf 1200.5"]
        );
        assert_eq!(
            plain
                .iter()
                .map(|row| row.chars().count())
                .collect::<Vec<_>>(),
            modern
                .iter()
                .map(|row| row.chars().count())
                .collect::<Vec<_>>()
        );

        let mut state = VisualState::default();
        state.install_layout(envelope);
        state.start();
        assert!(
            state.install_batch(
                UiEventBatch::new(0.0, 0.0, 1.0, 1, revision, vec![event], 0).unwrap()
            )
        );
        state.stop();
        let area = Rect::new(3, 2, 22, 3);
        let mut buffer = Buffer::empty(area);
        EventsView {
            state: &state,
            theme: &Theme::default(),
        }
        .render(area, &mut buffer);
        let rendered: Vec<String> = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        assert_eq!(rendered, plain);
        assert!(rendered.iter().all(|row| row.is_ascii()));
    }

    /// An event's value text becomes a chain: the sound first, the bank
    /// next, the controls abbreviated with their numbers trimmed, and the
    /// desk's own keys left out; the chain wraps at the arrows and never
    /// cuts a link.
    #[test]
    fn an_event_reads_as_a_wrapped_chain() {
        let mut event =
            event_with("s:bd bank:tr808 decay:0.4 gain:1.0 _pianoroll:1 orbit:0 cutoff:1200.5");
        let (head, links) = event_chain(&event);
        assert_eq!(head, "bd");
        assert_eq!(links, ["tr808", "dec .4", "g 1", "o 0", "lpf 1200.5"]);
        let rows = wrap_chain(&head, &links, 22);
        assert_eq!(
            rows,
            ["bd ▸ tr808 ▸ dec .4", "  ↳ g 1 ▸ o 0", "  ↳ lpf 1200.5"]
        );
        // The pipeline's order is kept: a sound set after its controls
        // comes after them, which is what the chain is for.
        event.value = Some("decay:0.4 note:c3 s:sawtooth release:0.25".to_owned());
        let (head, links) = event_chain(&event);
        assert_eq!(head, "dec .4");
        assert_eq!(links, ["note c3", "sawtooth", "rel .25"]);
        // A label the score gave heads the chain instead.
        event.label = Some("lead".to_owned());
        let (head, links) = event_chain(&event);
        assert_eq!(head, "lead");
        assert_eq!(links.len(), 4);
        assert_eq!(trim_number("0.250000"), ".25");
        assert_eq!(trim_number("-0.5"), "-.5");
        assert_eq!(trim_number("bd"), "bd");
    }
}
