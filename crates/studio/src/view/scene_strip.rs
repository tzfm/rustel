use super::*;

/// How many chips are off each end of the strip, and where the ones on it
/// land. Rendering and hit-testing both read this, so a chip is clickable
/// exactly where it is drawn.
pub struct SceneStripLayout {
    pub hits: Vec<Rect>,
    /// Chips before the first one drawn, and after the last.
    pub before: usize,
    pub after: usize,
}

/// Columns kept at each end for the `‹2` and `3›` counters.
const STRIP_MARKER: u16 = 3;

/// One dim separator cell between adjacent chips.
const STRIP_SEPARATOR: u16 = 1;

/// Where each chip of the strip lands, computed the same way it is drawn.
/// A chip is `▶1 name ♪c1/10 ●` with a space of padding either side.
///
/// Sixteen scenes need over two hundred columns, so on any ordinary
/// terminal the strip cannot show them all. It scrolls rather than
/// clipping, and what it scrolls to is the chip you are on: the whole
/// point of the strip is to say which scene is lit, and a lit chip that
/// has fallen off the end says nothing.
///
/// The window is derived from the chips themselves rather than kept as
/// state, so there is nothing to invalidate when a scene is added, closed
/// or renamed - and a rename that grows a chip cannot leave the strip
/// scrolled somewhere stale.
pub fn scene_strip_layout(area: Rect, chips: &[SceneChip]) -> SceneStripLayout {
    let empty = || SceneStripLayout {
        hits: vec![Rect::default(); chips.len()],
        before: 0,
        after: 0,
    };
    if area.is_empty() || chips.is_empty() {
        return empty();
    }
    let widths: Vec<u16> = chips
        .iter()
        .enumerate()
        .map(|(index, chip)| UnicodeWidthStr::width(chip_text(index, chip, None).as_str()) as u16)
        .collect();
    let current = chips.iter().position(|chip| chip.current).unwrap_or(0);

    // Seat as many as fit from `first`, and say whether `current` is among
    // them. Padding precedes the first chip; separators sit between chips.
    let seat = |first: usize, left: u16, right: u16| {
        let mut hits = vec![Rect::default(); chips.len()];
        let mut x = area.x.saturating_add(1).saturating_add(left);
        let mut last = first;
        for (index, width) in widths.iter().enumerate().skip(first) {
            if x.saturating_add(*width) > area.right().saturating_sub(right) {
                break;
            }
            hits[index] = Rect::new(x, area.y, *width, 1);
            x = x.saturating_add(*width).saturating_add(STRIP_SEPARATOR);
            last = index;
        }
        (hits, last)
    };

    // Everything at once, if it fits.
    let (hits, last) = seat(0, 0, 0);
    if last + 1 == chips.len() && !hits[chips.len() - 1].is_empty() {
        return SceneStripLayout {
            hits,
            before: 0,
            after: 0,
        };
    }

    // Otherwise scroll the least that keeps the current chip on screen,
    // so the strip only moves when it has to and the chips before the
    // current one stay put as long as they can.
    let mut first = 0usize;
    loop {
        let left = if first == 0 { 0 } else { STRIP_MARKER };
        let (hits, last) = seat(first, left, STRIP_MARKER);
        let seated_all = last + 1 == chips.len() && !hits[chips.len() - 1].is_empty();
        if !hits[current].is_empty() || first >= current {
            let right = if seated_all { 0 } else { STRIP_MARKER };
            // Re-seat without reserving the right marker when nothing is
            // after the last chip, so the final chip is not dropped to
            // make room for a counter of zero.
            let (hits, last) = if seated_all {
                (hits, last)
            } else {
                seat(first, left, right)
            };
            return SceneStripLayout {
                hits,
                before: first,
                after: chips.len().saturating_sub(last + 1),
            };
        }
        first += 1;
    }
}

/// The chip rectangles alone, for callers that only hit-test.
pub fn scene_strip_hits(area: Rect, chips: &[SceneChip]) -> Vec<Rect> {
    scene_strip_layout(area, chips).hits
}

fn chip_text(index: usize, chip: &SceneChip, renaming: Option<&str>) -> String {
    // A prebake wears its own mark and no number: it is not one of the
    // sixteen, and there is no key that plays it.
    let (marker, number) = match chip.prebake {
        Some(_) => (PREBAKE_GLYPH.to_string(), String::new()),
        None if chip.replay => (REPLAY_GLYPH.to_string(), String::new()),
        None => {
            let marker = if chip.armed.is_some() {
                "»"
            } else if chip.playing {
                "▶"
            } else {
                " "
            };
            (marker.to_owned(), (index + 1).to_string())
        }
    };
    let name = match renaming {
        Some(draft) => format!("{draft}{}", crate::terminal::symbol("▏")),
        None => chip.name.clone(),
    };
    let pad = chip
        .pad
        .as_deref()
        .map(|pad| format!(" ♪{pad}"))
        .unwrap_or_default();
    let dirty = if chip.dirty {
        format!(" {}", crate::terminal::symbol("●"))
    } else {
        String::new()
    };
    let errors = if chip.errors { " ✗" } else { "" };
    let rewind = if chip.rewind {
        format!(" {}", crate::terminal::symbol("⟲"))
    } else {
        String::new()
    };
    let armed = chip
        .armed
        .map(|cycles| format!(" in {cycles:.1}"))
        .unwrap_or_default();
    format!("{marker}{number} {name}{rewind}{pad}{dirty}{errors}{armed} ")
}

/// The scene strip: one chip per scene, the current one lit, the sounding
/// one marked, and the chords that drive it at the right.
pub(super) struct SceneStrip<'a> {
    pub keybinds: &'a crate::keybinds::Keybinds,
    pub(super) chips: &'a [SceneChip],
    pub(super) mode: &'a SceneStripMode,
    pub(super) split: bool,
    pub(super) capabilities: KeyboardCapabilities,
    pub(super) theme: &'a Theme,
}

impl Widget for SceneStrip<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        clear_surface(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.muted),
        );
        // A split gives the strip a second row: a dim rule that ends the
        // tabs, so the pane titles under it read as titles rather than as
        // more tabs. The chips keep the first row, and every hit test is
        // measured against it.
        if area.height > 1 {
            let rule = "\u{2500}".repeat(usize::from(area.width));
            buffer.set_stringn(
                area.x,
                area.bottom() - 1,
                rule,
                usize::from(area.width),
                Style::default().fg(theme.rule).bg(theme.surface),
            );
        }
        let area = Rect::new(area.x, area.y, area.width, 1);
        let SceneStripLayout {
            hits,
            before,
            after,
        } = scene_strip_layout(area, self.chips);
        // What is off each end, said rather than left to be discovered.
        // The counters sit in the muted colour so they read as chrome
        // beside the chips rather than as another scene.
        let counter = Style::default().fg(theme.muted).bg(theme.surface);
        if before > 0 {
            buffer.set_stringn(area.x, area.y, format!("‹{before}"), 3, counter);
        }
        if after > 0 {
            let text = format!("{after}›");
            let at = area
                .right()
                .saturating_sub(UnicodeWidthStr::width(text.as_str()) as u16);
            buffer.set_stringn(at, area.y, text, 3, counter);
        }
        let mut preceding_chip = false;
        for (index, (chip, hit)) in self.chips.iter().zip(&hits).enumerate() {
            if hit.is_empty() {
                continue;
            }
            if preceding_chip {
                buffer.set_string(hit.x - STRIP_SEPARATOR, hit.y, "│", counter);
            }
            preceding_chip = true;
            let renaming = match self.mode {
                SceneStripMode::Renaming(draft) if chip.current => Some(draft.as_str()),
                _ => None,
            };
            let text = chip_text(index, chip, renaming);
            // Setup reads in its own colour wherever it appears, so a
            // glance at the strip never mistakes it for a scene.
            let style = match (chip.current, chip.prebake.is_some(), chip.replay) {
                (true, true, _) => Style::default()
                    .fg(theme.fill_text())
                    .bg(theme.mini)
                    .add_modifier(Modifier::BOLD),
                (true, false, true) => Style::default()
                    .fg(theme.fill_text())
                    .bg(theme.replay_colour())
                    .add_modifier(Modifier::BOLD),
                (true, false, false) => Style::default()
                    .fg(theme.fill_text())
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD),
                (false, true, _) => Style::default().fg(theme.mini),
                (false, false, true) => Style::default().fg(theme.replay_colour()),
                (false, false, false) if chip.playing => Style::default().fg(theme.ok),
                // Quieter than the text under it. A tab nobody is on is
                // somewhere to go, not something to read. The strip sits
                // one row above a pane's own title, which names the scene
                // you are on, so the muted colour keeps the two rows apart.
                (false, false, false) => Style::default().fg(theme.muted),
            };
            // A rename draft may be wider than the chip the hits were
            // measured for; it simply runs on to the right.
            let width = UnicodeWidthStr::width(text.as_str()) as u16;
            buffer.set_stringn(
                hit.x,
                hit.y,
                text,
                usize::from(area.right().saturating_sub(hit.x).min(width)),
                style,
            );
        }
        let used = hits
            .iter()
            .filter(|hit| !hit.is_empty())
            .map(|hit| hit.right())
            .max()
            .unwrap_or(area.x);

        // The hint shrinks before it disappears: the full form on a wide
        // terminal, the chords alone on a narrower one.
        use crate::keybinds::BindAction;
        let previous_scene = menu::scene_shortcut_hint(
            self.keybinds,
            self.capabilities,
            BindAction::PreviousScene,
            self.split,
        );
        let next_scene = menu::scene_shortcut_hint(
            self.keybinds,
            self.capabilities,
            BindAction::NextScene,
            self.split,
        );
        let update = self.keybinds.hint(BindAction::Evaluate);
        let close = self.keybinds.hint(BindAction::CloseScene);
        let settings = self.keybinds.hint(BindAction::Settings);
        let hints: Vec<String> = match self.mode {
            SceneStripMode::Renaming(_) => vec!["Enter renames · Esc cancels".to_owned()],
            SceneStripMode::Learning => {
                vec!["hit a pad on your controller · Esc cancels".to_owned()]
            }
            // On a prebake tab the scene chords are the wrong advice:
            // there is nothing here to rename, launch or learn.
            SceneStripMode::Idle
                if self
                    .chips
                    .iter()
                    .any(|chip| chip.current && chip.prebake.is_some()) =>
            {
                let switch = [previous_scene.as_str(), next_scene.as_str()]
                    .into_iter()
                    .filter(|hint| !hint.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
                let keys = [
                    switch.as_str(),
                    update.as_str(),
                    close.as_str(),
                    settings.as_str(),
                ];
                let row = |labels: [&str; 4]| {
                    keys.into_iter()
                        .zip(labels)
                        .filter(|(key, _)| !key.is_empty())
                        .map(|(key, label)| {
                            if label.is_empty() {
                                key.to_owned()
                            } else {
                                format!("{key} {label}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" · ")
                };
                [
                    row(["switch", "applies the prebake", "closes it", "reopens it"]),
                    row(["", "applies", "closes", "reopens"]),
                    row(["", "", "", ""]),
                ]
                .into_iter()
                .filter(|hint| !hint.is_empty())
                .collect()
            }
            // Nothing at rest: the Scene menu holds every scene chord, with
            // its accelerator beside it. What stays here is what the menu
            // cannot say: the modes above, where the keyboard means
            // something it does not otherwise mean.
            SceneStripMode::Idle => Vec::new(),
        };
        let Some(hint) = hints.into_iter().find(|hint| {
            let width = UnicodeWidthStr::width(hint.as_str()) as u16;
            area.right().saturating_sub(width + 1) > used + 2
        }) else {
            return;
        };
        let hint_width = UnicodeWidthStr::width(hint.as_str()) as u16;
        let hint_x = area.right().saturating_sub(hint_width + 1);
        {
            buffer.set_stringn(
                hint_x,
                area.y,
                hint,
                usize::from(hint_width),
                Style::default().fg(match self.mode {
                    SceneStripMode::Idle => theme.muted,
                    _ => theme.warn,
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::tests::*;

    #[test]
    fn a_rewind_scene_chip_uses_a_conhost_safe_marker() {
        let mut chip = strip_chip("score", true);
        chip.rewind = true;
        chip.dirty = true;
        let fancy = {
            let _symbols = crate::terminal::ForceSymbolsForTest::set(true);
            chip_text(0, &chip, None)
        };
        let plain = {
            let _symbols = crate::terminal::ForceSymbolsForTest::set(false);
            chip_text(0, &chip, None)
        };
        assert!(fancy.contains(" ⟲"));
        assert!(fancy.contains(" ●"));
        assert!(plain.contains(" <"));
        assert!(plain.contains(" ·"));
        assert!(!plain.contains('⟲'));
        assert!(!plain.contains('●'));
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(fancy.as_str()),
            unicode_width::UnicodeWidthStr::width(plain.as_str())
        );
    }

    fn strip_chip(name: &str, current: bool) -> SceneChip {
        SceneChip {
            name: name.to_owned(),
            current,
            playing: false,
            dirty: false,
            rewind: false,
            pad: None,
            errors: false,
            armed: None,
            prebake: None,
            replay: false,
        }
    }

    /// Sixteen scenes need over two hundred columns, so the strip scrolls,
    /// and it always keeps the current chip on screen.
    #[test]
    fn the_scene_strip_keeps_the_current_chip_on_screen() {
        let area = Rect::new(0, 0, 80, 1);
        for current in 0..16usize {
            let chips: Vec<SceneChip> = (0..16)
                .map(|index| strip_chip(&format!("scene {index}"), index == current))
                .collect();
            let layout = scene_strip_layout(area, &chips);
            assert!(
                !layout.hits[current].is_empty(),
                "chip {current} of 16 fell off the strip"
            );
            assert!(
                layout.hits[current].right() <= area.right(),
                "chip {current} runs past the edge"
            );
            // Everything drawn is inside the area and in order.
            let drawn: Vec<usize> = (0..chips.len())
                .filter(|index| !layout.hits[*index].is_empty())
                .collect();
            assert!(!drawn.is_empty());
            assert!(
                drawn.windows(2).all(|pair| pair[1] == pair[0] + 1),
                "the drawn chips are a run, not a scatter: {drawn:?}"
            );
            assert_eq!(layout.before, drawn[0], "the left counter counts the rest");
            assert_eq!(
                layout.after,
                chips.len() - 1 - drawn[drawn.len() - 1],
                "the right counter counts the rest"
            );
        }
    }

    /// A strip that fits says nothing at its edges and starts at the first
    /// chip, so the counters never appear when there is nothing behind
    /// them.
    #[test]
    fn a_strip_that_fits_scrolls_nowhere() {
        let area = Rect::new(0, 0, 80, 1);
        let chips = vec![strip_chip("a", true), strip_chip("b", false)];
        let layout = scene_strip_layout(area, &chips);
        assert_eq!((layout.before, layout.after), (0, 0));
        assert!(layout.hits.iter().all(|hit| !hit.is_empty()));
        // And hit-testing lands on the chip that was drawn.
        for (index, hit) in layout.hits.iter().enumerate() {
            let found = scene_strip_hits(area, &chips);
            assert_eq!(found[index], *hit, "hit-testing reads the same layout");
        }
    }

    #[test]
    fn the_scene_strip_draws_chips_where_it_hit_tests_them() {
        let theme = theme();
        let chips = vec![
            SceneChip {
                replay: false,
                prebake: None,
                name: "intro".into(),
                current: false,
                playing: true,
                dirty: false,
                rewind: false,
                pad: Some("c1/10".into()),
                errors: false,
                armed: None,
            },
            SceneChip {
                replay: false,
                prebake: None,
                name: "drop".into(),
                current: true,
                playing: false,
                dirty: true,
                rewind: false,
                pad: None,
                errors: true,
                armed: None,
            },
        ];
        let area = Rect::new(0, 1, 120, 1);
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &crate::keybinds::Keybinds::default(),
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let hits = scene_strip_hits(area, &chips);
        let read = |rect: Rect| {
            (rect.x..rect.right())
                .filter_map(|x| buffer.cell((x, rect.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert_eq!(read(hits[0]).trim(), "▶1 intro ♪c1/10");
        assert_eq!(read(hits[1]).trim(), "2 drop ● ✗");
        assert!(!hits[0].intersects(hits[1]));
        assert_eq!(buffer.cell((hits[1].x, 1)).unwrap().bg, theme.accent);
        // At rest the strip advertises nothing: those chords live in the
        // Scene menu, each with its accelerator beside it.
        let text = text_of(&buffer);
        assert!(!text.contains("^N new"), "{text}");
        assert!(!text.contains("^[/^]"), "{text}");

        let mut renaming = Buffer::empty(area);
        SceneStrip {
            keybinds: &crate::keybinds::Keybinds::default(),
            chips: &chips,
            mode: &SceneStripMode::Renaming("dr".into()),
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut renaming);
        let text = text_of(&renaming);
        assert!(text.contains("2 dr▏"), "{text}");
        assert!(text.contains("Enter renames"), "{text}");
    }
    #[cfg(test)]
    mod scene_separator_tests {
        use super::*;

        #[test]
        fn scene_markers_are_separated_without_moving_clicks_into_the_gap() {
            let chips: Vec<_> = ["intro", "breakdown"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| SceneChip {
                    name: name.into(),
                    current: index == 1,
                    playing: index == 0,
                    dirty: true,
                    errors: true,
                    rewind: false,
                    pad: None,
                    armed: None,
                    prebake: None,
                    replay: false,
                })
                .collect();
            let theme = Theme::built_in_default();
            let area = Rect::new(3, 2, 80, 1);
            let mut buffer = Buffer::empty(area);
            SceneStrip {
                keybinds: &crate::keybinds::Keybinds::default(),
                chips: &chips,
                mode: &SceneStripMode::Idle,
                split: false,
                capabilities: KeyboardCapabilities::enhanced(),
                theme: &theme,
            }
            .render(area, &mut buffer);
            let hits = scene_strip_hits(area, &chips);
            let separator = (hits[0].right(), area.y);
            assert_eq!(hits[1].x, separator.0 + 1);
            let cell = buffer.cell(separator).expect("separator cell");
            assert_eq!(cell.symbol(), "│");
            assert_eq!(cell.fg, theme.muted);
            assert!(!hits.iter().any(|hit| hit.contains(separator.into())));
            for (index, hit) in hits.iter().enumerate() {
                for x in hit.x..hit.right() {
                    assert_eq!(
                        hits.iter()
                            .position(|rect| rect.contains((x, area.y).into())),
                        Some(index)
                    );
                }
            }
        }
    }
    /// A prebake chip says what it is, wears no number, and takes its own
    /// colour - and the strip still draws chips where it hit-tests them.
    #[test]
    fn a_prebake_chip_is_marked_numberless_and_in_its_own_colour() {
        let theme = theme();
        let chips = vec![
            SceneChip {
                replay: false,
                prebake: None,
                name: "intro".into(),
                current: false,
                playing: true,
                dirty: false,
                rewind: false,
                pad: None,
                errors: false,
                armed: None,
            },
            SceneChip {
                replay: false,
                prebake: Some(PrebakeScope::Global),
                name: PrebakeScope::Global.tab_name(),
                current: false,
                playing: false,
                dirty: true,
                rewind: false,
                pad: None,
                errors: false,
                armed: None,
            },
            SceneChip {
                replay: false,
                prebake: Some(PrebakeScope::Local),
                name: PrebakeScope::Local.tab_name(),
                current: true,
                playing: false,
                dirty: false,
                rewind: false,
                pad: None,
                errors: false,
                armed: None,
            },
        ];
        let area = Rect::new(0, 1, 120, 1);
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &crate::keybinds::Keybinds::default(),
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);

        let hits = scene_strip_hits(area, &chips);
        let read = |rect: Rect| {
            (rect.x..rect.right())
                .filter_map(|x| buffer.cell((x, rect.y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert_eq!(read(hits[0]).trim(), "▶1 intro");
        assert_eq!(read(hits[1]).trim(), "⚙ prebake (global) ●");
        assert_eq!(read(hits[2]).trim(), "⚙ prebake (local)");
        assert!(!hits[1].intersects(hits[2]), "the chips overlap");
        assert_eq!(
            buffer.cell((hits[1].x, 1)).unwrap().fg,
            theme.mini,
            "a prebake chip is not in its own colour"
        );
        assert_eq!(buffer.cell((hits[2].x, 1)).unwrap().bg, theme.mini);

        let text = text_of(&buffer);
        assert!(text.contains("^Enter applies"), "{text}");
    }

    #[test]
    fn prebake_strip_uses_active_update_close_and_settings_bindings() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let theme = Theme::built_in_default();
        let chips = [SceneChip {
            replay: false,
            prebake: Some(PrebakeScope::Local),
            name: "prebake".into(),
            current: true,
            playing: false,
            dirty: false,
            rewind: false,
            pad: None,
            errors: false,
            armed: None,
        }];
        let mut bindings = Keybinds::default();
        for (action, chord) in [
            (BindAction::Evaluate, "f2"),
            (BindAction::CloseScene, "f3"),
            (BindAction::Settings, "f4"),
        ] {
            bindings.learn(action, KeyCombo::parse(chord));
        }
        let area = Rect::new(0, 0, 200, 1);
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &bindings,
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("F2 applies"), "{text}");
        assert!(text.contains("F3 closes"), "{text}");
        assert!(text.contains("F4 reopens"), "{text}");
        for retired in ["^S", "^W", "^B"] {
            assert!(!text.contains(retired), "{text}");
        }

        for action in [
            BindAction::PreviousScene,
            BindAction::NextScene,
            BindAction::Evaluate,
            BindAction::CloseScene,
            BindAction::Settings,
        ] {
            bindings.unbind(action);
        }
        let mut buffer = Buffer::empty(area);
        SceneStrip {
            keybinds: &bindings,
            chips: &chips,
            mode: &SceneStripMode::Idle,
            split: false,
            capabilities: KeyboardCapabilities::enhanced(),
            theme: &theme,
        }
        .render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for absent in ["switch", "applies", "closes", "reopens", "/", " · "] {
            assert!(
                !text.contains(absent),
                "unbound action advertised in {text}"
            );
        }
    }
}
