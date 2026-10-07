//! Value choices and the chord, scale, and tuning catalogues.

use super::*;

/// Every colour a score can name in `.color(…)`, in the picker's order.
pub fn color_vocabulary() -> Vec<String> {
    super::super::theme::COLOR_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// Suggested equal divisions of the octave. This list is only for
/// completion; `edo` accepts any division from 1 to 65,536.
pub fn edo_vocabulary() -> Vec<String> {
    // Start with 12-tone equal temperament; 1200 divides the octave into cents.
    [12, 19, 22, 24, 31, 41, 53, 72, 5, 7, 10, 15, 17, 1200]
        .iter()
        .map(|divisions| format!("{divisions}edo"))
        .collect()
}

/// A tuning has no tonic of its own, so its preview starts from the same
/// C the scales start from and the ear can compare them.
const TUNING_PREVIEW_TONIC_HZ: f64 = 261.625_565_3;
/// Enough degrees to hear the tuning's character. `Tune::note` wraps and
/// octave-shifts past the end, so this is a rising run whatever the
/// tuning's length.
const TUNING_PREVIEW_STEPS: u8 = 8;

/// The first degrees of a tuning as fractional MIDI.
///
/// Fractional on purpose: a tuning's degrees do not land on semitones, and
/// `note_name` rounds, so naming them would print a lie. The ear is the
/// only honest reading, which is the whole reason the list is playable.
pub fn tuning_notes(name: &str) -> Vec<f64> {
    let mut tune = rustel_core::tune::Tune::new();
    tune.tonicize(TUNING_PREVIEW_TONIC_HZ);
    if tune
        .load_scale(&rustel_core::tune::ScaleSpec::name(name))
        .is_err()
    {
        return Vec::new();
    }
    (0..TUNING_PREVIEW_STEPS)
        .map(|step| {
            let hertz = tune.note(f64::from(step), None);
            69.0 + 12.0 * (hertz / 440.0).log2()
        })
        .filter(|midi| midi.is_finite())
        .collect()
}

/// The tunings worth meeting by name.
///
/// The dictionary behind `xen` and `tune` holds 3,304 of them, and a list
/// that long would be no use even if it fitted: fewer than four hundred of
/// those names are words, and the rest read `efg777` and `temp19ebmt`. A
/// reader cannot pick from names that say nothing.
///
/// So this is the handful anyone might come looking for, grouped the way
/// they are learnt - the keyboard temperaments, the theorists, the
/// twentieth century's inventions, and the tunings people actually play -
/// and everything else is still typed in full. As with the divisions, the
/// completion offers rather than restricts.
pub fn tuning_vocabulary() -> Vec<String> {
    [
        // Historical keyboard temperaments.
        "pyth_12",
        "meanquar",
        "werck3",
        "kirnberger3",
        "vallotti",
        "young",
        // Just intonation, after the people who wrote it down.
        "ptolemy",
        "zarlino",
        "partch-ur",
        "harmonical",
        "silver",
        "diamond7",
        // Invented, and recently.
        "carlos_alpha",
        "carlos_beta",
        "carlos_gamma",
        "bohlen-p",
        "lucy_31",
        // Played, and for a long time.
        "slendro",
        "pelog1",
        "indian",
        "segah",
        "iraq",
        "turkish_bagl",
        "santur2",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}

/// Preferred order for common scales. Other dictionary names follow
/// alphabetically in [`scale_vocabulary`].
pub const SCALE_ORDER: &[&str] = &[
    "major",
    "minor",
    "major:pentatonic",
    "minor:pentatonic",
    "minor:blues",
    "major:blues",
    "dorian",
    "mixolydian",
    "lydian",
    "phrygian",
    "locrian",
    "harmonic:minor",
    "melodic:minor",
    "harmonic:major",
    "whole:tone",
    "chromatic",
    "bebop:major",
    "bebop:minor",
    "phrygian:dominant",
    "lydian:dominant",
    "altered",
    "diminished",
    "egyptian",
    "hirajoshi",
    "iwato",
    "in:sen",
    "flamenco",
    "hungarian:minor",
];

/// Every accepted scale name and alias, with common scales first and the
/// rest alphabetical. Spaces become colons so a name such as
/// `major:pentatonic` stays one step in mini-notation.
pub fn scale_vocabulary() -> Vec<String> {
    let mut names: Vec<String> = rustel_core::tonaljs_scales::SCALE_DICTIONARY
        .iter()
        .flat_map(|(name, aliases, _)| std::iter::once(*name).chain(aliases.iter().copied()))
        .map(|name| name.replace(' ', ":"))
        .collect();
    names.sort();
    names.dedup();
    // The common ones first, in their own order; the rest keep the
    // alphabet they already have.
    names.sort_by_key(|name| {
        SCALE_ORDER
            .iter()
            .position(|common| common == name)
            .unwrap_or(SCALE_ORDER.len())
    });
    names
}

/// Every chord symbol any voicing dictionary knows, sorted.
pub fn chord_vocabulary() -> Vec<String> {
    let mut names: Vec<String> =
        serde_json::from_str::<serde_json::Value>(rustel_core::voicings::registry_json())
            .ok()
            .and_then(|registry| {
                let dictionaries = registry.get("registry")?.as_object()?.clone();
                let mut symbols = Vec::new();
                for dictionary in dictionaries.values() {
                    if let Some(entries) = dictionary.get("dictionary").and_then(|d| d.as_object())
                    {
                        symbols.extend(entries.keys().cloned());
                    }
                }
                Some(symbols)
            })
            .unwrap_or_default();
    names.retain(|name| !name.is_empty());
    names.sort();
    names.dedup();
    names
}

/// The chord qualities in the order they are offered: the ones that carry
/// most modern and popular music first, the jazz extensions after them,
/// and whatever the dictionary has that this list does not name last.
pub const CHORD_ORDER: &[ChordQuality] = &[
    ChordQuality {
        symbol: "^",
        name: "major",
        common: "C",
    },
    ChordQuality {
        symbol: "-",
        name: "minor",
        common: "Cm",
    },
    ChordQuality {
        symbol: "7",
        name: "dominant 7",
        common: "C7",
    },
    ChordQuality {
        symbol: "^7",
        name: "major 7",
        common: "Cmaj7",
    },
    ChordQuality {
        symbol: "-7",
        name: "minor 7",
        common: "Cm7",
    },
    ChordQuality {
        symbol: "sus",
        name: "suspended 4th",
        common: "Csus4",
    },
    ChordQuality {
        symbol: "2",
        name: "suspended 2nd",
        common: "Csus2",
    },
    ChordQuality {
        symbol: "5",
        name: "power (no third)",
        common: "C5",
    },
    ChordQuality {
        symbol: "add9",
        name: "added 9th",
        common: "Cadd9",
    },
    ChordQuality {
        symbol: "6",
        name: "major 6",
        common: "C6",
    },
    ChordQuality {
        symbol: "-6",
        name: "minor 6",
        common: "Cm6",
    },
    ChordQuality {
        symbol: "9",
        name: "dominant 9",
        common: "C9",
    },
    ChordQuality {
        symbol: "^9",
        name: "major 9",
        common: "Cmaj9",
    },
    ChordQuality {
        symbol: "-9",
        name: "minor 9",
        common: "Cm9",
    },
    ChordQuality {
        symbol: "69",
        name: "6 add 9",
        common: "C6/9",
    },
    ChordQuality {
        symbol: "7sus",
        name: "dominant 7 suspended",
        common: "C7sus4",
    },
    ChordQuality {
        symbol: "11",
        name: "dominant 11",
        common: "C11",
    },
    ChordQuality {
        symbol: "13",
        name: "dominant 13",
        common: "C13",
    },
    ChordQuality {
        symbol: "^13",
        name: "major 13",
        common: "Cmaj13",
    },
    ChordQuality {
        symbol: "-11",
        name: "minor 11",
        common: "Cm11",
    },
    ChordQuality {
        symbol: "+",
        name: "augmented",
        common: "Caug",
    },
    ChordQuality {
        symbol: "o",
        name: "diminished",
        common: "Cdim",
    },
    ChordQuality {
        symbol: "h",
        name: "half-diminished",
        common: "Cm7b5",
    },
    ChordQuality {
        symbol: "o7",
        name: "diminished 7",
        common: "Cdim7",
    },
    ChordQuality {
        symbol: "h7",
        name: "half-diminished 7",
        common: "Cm7b5",
    },
    ChordQuality {
        symbol: "-^7",
        name: "minor major 7",
        common: "CmMaj7",
    },
    ChordQuality {
        symbol: "7b9",
        name: "dominant 7 flat 9",
        common: "C7b9",
    },
    ChordQuality {
        symbol: "7#9",
        name: "dominant 7 sharp 9",
        common: "C7#9",
    },
    ChordQuality {
        symbol: "7#11",
        name: "dominant 7 sharp 11",
        common: "C7#11",
    },
    ChordQuality {
        symbol: "7b13",
        name: "dominant 7 flat 13",
        common: "C7b13",
    },
    ChordQuality {
        symbol: "7b5",
        name: "dominant 7 flat 5",
        common: "C7b5",
    },
    ChordQuality {
        symbol: "^7#11",
        name: "major 7 sharp 11",
        common: "Cmaj7#11",
    },
    ChordQuality {
        symbol: "^7#5",
        name: "major 7 sharp 5",
        common: "Cmaj7#5",
    },
    ChordQuality {
        symbol: "-b6",
        name: "minor flat 6",
        common: "Cmb6",
    },
    ChordQuality {
        symbol: "-add9",
        name: "minor added 9th",
        common: "Cmadd9",
    },
    ChordQuality {
        symbol: "-69",
        name: "minor 6 add 9",
        common: "Cm6/9",
    },
];

/// A name as a musician reads it: `b` after a letter or digit is a flat,
/// `#` is a sharp. Display only - a score is written with the plain
/// letters, and every insertion uses those.
pub fn pretty_notation(text: &str) -> String {
    let letters: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (index, letter) in letters.iter().enumerate() {
        match letter {
            '#' => out.push('♯'),
            'b' if index > 0
                && letters[index - 1].is_ascii_alphanumeric()
                && letters
                    .get(index + 1)
                    .is_none_or(|next| next.is_ascii_digit() || *next == ':' || *next == ' ') =>
            {
                out.push('♭')
            }
            other => out.push(*other),
        }
    }
    out
}

/// The twelve roots, spelled as a score writes them.
pub const CHORD_ROOTS: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

/// The tonic a written-out scale names, as its index into
/// [`CHORD_ROOTS`]: `Gb1` is `Gb`, and `F#` reads as its flat spelling.
/// The octave - `C-1`, `Bb2` - is not a choice the list offers, so it is
/// read and dropped.
pub(crate) fn tonic_root(tonic: &str) -> Option<usize> {
    let root = tonic.trim().trim_end_matches(|c: char| c.is_ascii_digit());
    let root = root.trim_end_matches('-');
    if root.is_empty() {
        return None;
    }
    // The list spells every black key as a flat; a score may write its
    // enharmonic sharp, and both name the same row.
    let flat = match root.to_ascii_lowercase().as_str() {
        "c#" => "db",
        "d#" => "eb",
        "f#" => "gb",
        "g#" => "ab",
        "a#" => "bb",
        _ => root,
    };
    CHORD_ROOTS
        .iter()
        .position(|spelled| spelled.eq_ignore_ascii_case(flat))
}

/// Octave used for chord previews.
pub(super) const CHORD_PREVIEW_OCTAVE: i32 = 3;
/// A scale is heard an octave above a chord, so a run stays clear of one.
pub const SCALE_PREVIEW_OCTAVE: i32 = 4;

/// The notes of a scale written as a score writes it, `C:major`, with its
/// octave on the end so the run lands where it started.
pub fn scale_notes(scale: &str) -> Vec<f64> {
    let Ok(props) = rustel_core::tonaljs::get_scale(scale) else {
        return Vec::new();
    };
    // Lift each pitch class by whole octaves until it no longer falls
    // below the previous degree. Keeping every name in one octave would
    // make C follow Bb downward in Db major.
    let mut notes: Vec<f64> = Vec::with_capacity(props.notes.len() + 1);
    for note in &props.notes {
        let Ok(mut midi) = rustel_core::util::note_to_midi(note, SCALE_PREVIEW_OCTAVE) else {
            continue;
        };
        if let Some(previous) = notes.last().copied() {
            while midi < previous {
                midi += 12.0;
            }
        }
        notes.push(midi);
    }
    // The tonic again on top, so the run lands where it started - above
    // the last degree, however far the scale climbed to reach it.
    if let Some(first) = notes.first().copied() {
        let mut home = first + 12.0;
        while notes.last().is_some_and(|last| home < *last) {
            home += 12.0;
        }
        notes.push(home);
    }
    notes
}

impl ReferencePanel {
    /// The chord qualities on offer, in order: the named ones the
    /// dictionary actually has, then anything else it knows, so nothing is
    /// hidden. Filtered by the search box, which matches the name, the
    /// symbol, or a whole chord like `Cm7`.
    pub fn chord_qualities(&self) -> Vec<ChordQuality> {
        let symbols = rustel_core::voicings::dictionary_symbols(None);
        let has = |symbol: &str| symbols.iter().any(|known| known == symbol);
        let mut qualities: Vec<ChordQuality> = CHORD_ORDER
            .iter()
            .copied()
            .filter(|quality| has(quality.symbol))
            .collect();
        for symbol in &symbols {
            if symbol.is_empty() || qualities.iter().any(|quality| quality.symbol == symbol) {
                continue;
            }
            // A symbol this list does not name keeps its own spelling: the
            // dictionary is the authority on what can be played.
            let leaked: &'static str = Box::leak(symbol.clone().into_boxed_str());
            qualities.push(ChordQuality {
                symbol: leaked,
                name: leaked,
                // Nothing to teach a reader about a symbol this list does
                // not name: the dictionary's spelling is all there is.
                common: leaked,
            });
        }
        let query = self.chord_query.trim();
        if query.is_empty() {
            return qualities;
        }
        // A chord is asked for by its name, by its symbol, or by writing
        // one out - `Ab-7` - and the nearest answer comes first.
        let mut scored = qualities
            .into_iter()
            .filter_map(|quality| {
                let written = CHORD_ROOTS
                    .iter()
                    .map(|root| format!("{root}{}", quality.symbol))
                    .collect::<Vec<_>>();
                let score =
                    super::super::fuzzy::score(query, quality.name, &written, quality.symbol)
                        .or_else(|| super::super::fuzzy::name_score(query, quality.symbol))?;
                Some((score, quality))
            })
            .collect::<Vec<_>>();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.into_iter().map(|(_, quality)| quality).collect()
    }

    /// The scales on offer, common ones first, filtered by the search
    /// box - which matches the name, or a scale written out with its
    /// tonic, `C:major`.
    pub fn scale_names(&self) -> Vec<String> {
        let names = scale_vocabulary();
        let query = self.scale_query.trim();
        if query.is_empty() {
            return names;
        }
        // `C:maj` asks for major on C: match the written form too, and
        // keep the list's own order among equals.
        let mut scored = names
            .into_iter()
            .filter_map(|name| {
                let written = CHORD_ROOTS
                    .iter()
                    .map(|tonic| format!("{tonic}:{name}"))
                    .collect::<Vec<_>>();
                let score = super::super::fuzzy::score(query, &name, &written, "")?;
                Some((score, name))
            })
            .collect::<Vec<_>>();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.into_iter().map(|(_, name)| name).collect()
    }

    /// The scales list as drawn: a scale, and under the open one its
    /// twelve tonics.
    pub fn scale_rows(&self) -> Vec<ScaleRow> {
        let names = self.scale_names();
        let mut rows = Vec::with_capacity(names.len() + CHORD_ROOTS.len());
        for (index, name) in names.iter().enumerate() {
            rows.push(ScaleRow::Scale(index));
            if self.open_scale.as_deref() == Some(name.as_str()) {
                for tonic in 0..CHORD_ROOTS.len() {
                    rows.push(ScaleRow::Tonic(index, tonic));
                }
            }
        }
        rows
    }

    /// The scale a row names, as a score writes it: `C:major`. A scale
    /// row is that scale on C, which is what the ear expects to hear.
    pub fn scale_of(&self, row: ScaleRow) -> Option<String> {
        let names = self.scale_names();
        match row {
            ScaleRow::Scale(index) => names
                .get(index)
                .map(|name| format!("{}:{name}", CHORD_ROOTS[0])),
            ScaleRow::Tonic(index, tonic) => names
                .get(index)
                .map(|name| format!("{}:{name}", CHORD_ROOTS[tonic])),
        }
    }

    /// The scale under the cursor, written as a score writes it.
    pub fn selected_scale(&self) -> Option<String> {
        if self.tab != Tab::Scales {
            return None;
        }
        self.scale_rows()
            .get(self.scale_selected)
            .copied()
            .and_then(|row| self.scale_of(row))
    }

    /// The notes of the scale under the cursor, for the row, the keyboard
    /// and the preview.
    pub fn selected_scale_notes(&self) -> Vec<f64> {
        self.selected_scale()
            .map(|scale| scale_notes(&scale))
            .unwrap_or_default()
    }

    /// Open a scale onto its tonics, or fold it, staying on the row.
    pub(super) fn toggle_scale(&mut self, index: usize) -> bool {
        let names = self.scale_names();
        let Some(name) = names.get(index).cloned() else {
            return false;
        };
        let standing_on = self.selected_scale();
        if self.open_scale.as_deref() == Some(name.as_str()) {
            self.open_scale = None;
        } else {
            self.open_scale = Some(name);
        }
        if let Some(scale) = standing_on
            && let Some(row) = self
                .scale_rows()
                .iter()
                .position(|row| self.scale_of(*row).as_deref() == Some(scale.as_str()))
        {
            self.scale_selected = row;
        }
        self.scale_selected = self
            .scale_selected
            .min(self.scale_rows().len().saturating_sub(1));
        true
    }

    /// The chords list as drawn: a quality, and under the open one its
    /// twelve roots.
    pub fn chord_rows(&self) -> Vec<ChordRow> {
        let qualities = self.chord_qualities();
        let mut rows = Vec::with_capacity(qualities.len() + CHORD_ROOTS.len());
        for (index, quality) in qualities.iter().enumerate() {
            rows.push(ChordRow::Quality(index));
            if self.open_quality.as_deref() == Some(quality.symbol) {
                for root in 0..CHORD_ROOTS.len() {
                    rows.push(ChordRow::Chord(index, root));
                }
            }
        }
        rows
    }

    /// The chord a row names, as a score writes it: `C-7`, `Ab^9`.
    pub fn chord_of(&self, row: ChordRow) -> Option<String> {
        let qualities = self.chord_qualities();
        match row {
            ChordRow::Quality(index) => qualities
                .get(index)
                .map(|quality| format!("{}{}", CHORD_ROOTS[0], quality.symbol)),
            ChordRow::Chord(index, root) => qualities
                .get(index)
                .map(|quality| format!("{}{}", CHORD_ROOTS[root], quality.symbol)),
        }
    }

    /// The chord under the cursor, if the chords tab is the one showing.
    pub fn selected_chord(&self) -> Option<String> {
        if self.tab != Tab::Chords {
            return None;
        }
        self.chord_rows()
            .get(self.chord_selected)
            .copied()
            .and_then(|row| self.chord_of(row))
    }

    /// The notes of the chord under the cursor, for the keyboard and the
    /// preview. Empty when the dictionary cannot voice it.
    pub fn selected_chord_notes(&self) -> Vec<f64> {
        self.selected_chord()
            .and_then(|chord| {
                rustel_core::voicings::chord_notes(&chord, None, CHORD_PREVIEW_OCTAVE).ok()
            })
            .unwrap_or_default()
    }

    /// Open the quality under the cursor onto its roots, or fold it.
    pub(super) fn toggle_quality(&mut self, index: usize) -> bool {
        let Some(quality) = self.chord_qualities().get(index).copied() else {
            return false;
        };
        let standing_on = self.selected_chord();
        if self.open_quality.as_deref() == Some(quality.symbol) {
            self.open_quality = None;
        } else {
            self.open_quality = Some(quality.symbol.to_owned());
        }
        // Stay on the row that was pressed, whatever opened or closed.
        if let Some(chord) = standing_on
            && let Some(row) = self
                .chord_rows()
                .iter()
                .position(|row| self.chord_of(*row).as_deref() == Some(chord.as_str()))
        {
            self.chord_selected = row;
        }
        self.chord_selected = self
            .chord_selected
            .min(self.chord_rows().len().saturating_sub(1));
        true
    }
}
