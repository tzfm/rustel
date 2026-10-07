//! Computer-keyboard piano note state, independent of the score and transport.
//!
//! Slots follow semitone order, so a key's identity remains stable while its
//! sounding pitch is retained across octave changes. The event loop owns note
//! lifetimes and direct audition audio; this module never edits the score.

const KEYS: [char; 15] = [
    'a', 'w', 's', 'e', 'd', 'f', 't', 'g', 'y', 'h', 'u', 'j', 'k', 'o', 'l',
];
const SHARPS: [&str; 12] = [
    "c", "c#", "d", "d#", "e", "f", "f#", "g", "g#", "a", "a#", "b",
];
const FLATS: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

/// Lightweight oscillators for the keyboard, independent of score sample banks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PianoSound {
    Sine,
    #[default]
    Triangle,
    Square,
}

impl PianoSound {
    pub fn key(self) -> &'static str {
        match self {
            Self::Sine => "sine",
            Self::Triangle => "triangle",
            Self::Square => "square",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "sine" => Self::Sine,
            "triangle" => Self::Triangle,
            "square" => Self::Square,
            // Older preferences selected a sampled piano. Opening the keyboard
            // must never need that bank to load before the first note.
            _ => Self::default(),
        }
    }

    pub fn step(self, forwards: bool) -> Self {
        let sounds = [Self::Sine, Self::Triangle, Self::Square];
        let index = sounds.iter().position(|sound| *sound == self).unwrap();
        sounds[(index + if forwards { 1 } else { sounds.len() - 1 }) % sounds.len()]
    }
}

/// Percentage of the original keyboard gain, before velocity and master gain.
pub const DEFAULT_PIANO_VOLUME: u16 = 130;
pub const MAX_PIANO_VOLUME: u16 = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PianoNote {
    pub key: u8,
    pub note: u8,
}

#[derive(Clone, Debug)]
pub(super) struct Piano {
    base: u8,
    held: [Option<PianoNote>; KEYS.len()],
    last_notes: Vec<u8>,
}

impl Default for Piano {
    fn default() -> Self {
        Self {
            // MIDI 60 is c4 in Rustel, regardless of a DAW's labels.
            base: 60,
            held: [None; KEYS.len()],
            last_notes: Vec::new(),
        }
    }
}

impl Piano {
    pub fn key_for_char(character: char) -> Option<u8> {
        KEYS.iter()
            .position(|&key| key == character.to_ascii_lowercase())
            .map(|slot| slot as u8)
    }

    /// Repeats never retrigger a held note or add another copy of its voice.
    pub fn press(&mut self, character: char) -> Option<PianoNote> {
        let key = Self::key_for_char(character)?;
        if self.held[usize::from(key)].is_some() {
            return None;
        }
        let note = PianoNote {
            key,
            note: self.base + key,
        };
        self.held[usize::from(key)] = Some(note);
        self.last_notes = self.held_notes().map(|note| note.note).collect();
        Some(note)
    }

    /// Also used when a terminal without key-up events expires a timed note.
    pub fn release_key(&mut self, key: u8) -> Option<u8> {
        self.held.get_mut(usize::from(key))?.take().map(|_| key)
    }

    pub fn release_all(&mut self) -> Vec<u8> {
        self.held
            .iter_mut()
            .filter_map(|note| note.take().map(|note| note.key))
            .collect()
    }

    /// Shift future notes only: releasing an old key still releases its slot.
    pub fn octave(&mut self, delta: i8) {
        self.base = (i16::from(self.base) + i16::from(delta) * 12).clamp(0, 108) as u8;
    }

    /// The pitch assigned to A for the next press.
    pub fn base_note(&self) -> u8 {
        self.base
    }

    pub fn held_notes(&self) -> impl Iterator<Item = PianoNote> + '_ {
        self.held.iter().filter_map(|note| *note)
    }

    /// Leave the most recent chord readable after the fingers are lifted.
    pub fn notes(&self) -> Vec<u8> {
        self.last_notes.clone()
    }

    pub fn forget_notes(&mut self) {
        self.last_notes.clear();
    }
}

/// Rustel uses MIDI 60 = c4, including exact ASCII accidentals.
pub(super) fn note_name(note: u8) -> String {
    format!(
        "{}{}",
        SHARPS[usize::from(note % 12)],
        i16::from(note / 12) - 1
    )
}

pub(super) fn note_label(note: u8) -> String {
    let sharp = note_name(note).to_ascii_uppercase();
    let semitone = usize::from(note % 12);
    if SHARPS[semitone].contains('#') {
        format!("{sharp} / {}{}", FLATS[semitone], i16::from(note / 12) - 1)
    } else {
        sharp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ableton_layout_covers_fifteen_semitones_and_supports_chords() {
        let mut piano = Piano::default();
        for (key, character) in KEYS.into_iter().enumerate() {
            assert_eq!(
                piano.press(character),
                Some(PianoNote {
                    key: key as u8,
                    note: 60 + key as u8,
                })
            );
        }
        assert_eq!(piano.notes(), (60..75).collect::<Vec<_>>());
        assert_eq!(piano.held_notes().count(), 15);
        assert_eq!(piano.press('a'), None);
        assert_eq!(piano.press('A'), None);
        assert_eq!(piano.press('q'), None);
        for character in ['z', 'x', 'c', 'v'] {
            assert_eq!(piano.press(character), None);
        }
    }

    #[test]
    fn release_retains_pitch_identity_after_octave_changes() {
        let mut piano = Piano::default();
        piano.press('a').unwrap();
        piano.octave(1);
        assert_eq!(piano.base_note(), 72);
        assert_eq!(piano.held_notes().next().unwrap().note, 60);
        assert_eq!(piano.press('s').unwrap().note, 74);
        assert_eq!(
            piano.release_key(Piano::key_for_char('A').unwrap()),
            Some(0)
        );
        assert_eq!(piano.release_key(Piano::key_for_char('A').unwrap()), None);
        assert_eq!(piano.release_key(255), None);
        assert_eq!(piano.release_all(), vec![2]);
        assert_eq!(piano.press('a').unwrap().note, 72);
    }

    #[test]
    fn final_chord_stays_readable_until_the_next_note() {
        let mut piano = Piano::default();
        assert!(piano.notes().is_empty());
        piano.press('a');
        piano.press('d');
        piano.press('g');
        piano.release_key(0);
        assert_eq!(piano.notes(), vec![60, 64, 67]);
        piano.release_key(4);
        piano.release_key(7);
        assert_eq!(piano.notes(), vec![60, 64, 67]);
        piano.press('w');
        assert_eq!(piano.notes(), vec![61]);
        assert_eq!(note_label(61), "C#4 / Db4");
        piano.release_all();
        assert_eq!(piano.notes(), vec![61]);
    }

    #[test]
    fn octave_keeps_every_key_in_midi_range() {
        let mut piano = Piano::default();
        piano.octave(i8::MIN);
        assert_eq!(piano.base_note(), 0);
        assert_eq!(piano.press('a').unwrap().note, 0);
        assert_eq!(piano.press('l').unwrap().note, 14);
        piano.release_all();
        piano.octave(i8::MAX);
        assert_eq!(piano.base_note(), 108);
        assert_eq!(piano.press('a').unwrap().note, 108);
        assert_eq!(piano.press('l').unwrap().note, 122);
    }
}
