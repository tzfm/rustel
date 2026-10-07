//! Small musical decisions shared by the recipes, independent of sound design.

use super::Rng;

const MODES: [[i32; 7]; 5] = [
    [0, 2, 3, 5, 7, 9, 10], // Dorian
    [0, 2, 3, 5, 7, 8, 10], // natural minor
    [0, 2, 4, 5, 7, 9, 10], // Mixolydian
    [0, 2, 4, 5, 7, 9, 11], // major
    [0, 2, 3, 5, 7, 8, 11], // harmonic minor
];
const PROGRESSIONS: [[i32; 4]; 6] = [
    [0, 5, 3, 4],
    [0, 3, 5, 4],
    [0, 6, 3, 4],
    [0, 2, 5, 3],
    [0, 4, 5, 3],
    [0, 3, 0, 6],
];
// Zero is an anchor, one/two add activity, three stays a rest.
const RHYTHMS: [[u8; 8]; 6] = [
    [0, 3, 1, 0, 3, 0, 2, 1],
    [0, 1, 3, 0, 2, 3, 0, 1],
    [0, 2, 1, 3, 1, 0, 3, 2],
    [0, 3, 0, 1, 3, 2, 0, 1],
    [0, 1, 2, 3, 0, 3, 1, 0],
    [0, 1, 2, 0, 1, 0, 1, 3],
];

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Music {
    mode: usize,
    pub notes: [i32; 8],
    rhythm: usize,
    pub response: usize,
    progression: usize,
    voicing: usize,
    pub groove: usize,
}

impl Music {
    pub fn new(rng: &mut Rng) -> Self {
        let mut notes = [0; 8];
        notes[0] = [0, 2, 4][rng.index(3)];
        // A short contour, then an answering contour, rather than eight unrelated pitches.
        let contour = [[1, 1, -1], [2, -1, -1], [-1, 2, 1], [0, 2, -1], [2, 1, -2]][rng.index(5)];
        for i in 1..4 {
            notes[i] = (notes[i - 1] + contour[i - 1]).clamp(-2, 8);
        }
        let offset = [-2, -1, 1, 2][rng.index(4)];
        for i in 4..7 {
            notes[i] = (notes[i - 4] + offset).clamp(-2, 8);
        }
        notes[7] = [0, 2, 4][rng.index(3)];
        Self {
            mode: rng.index(MODES.len()),
            notes,
            rhythm: rng.index(RHYTHMS.len()),
            response: rng.index(24),
            progression: rng.index(PROGRESSIONS.len()),
            voicing: rng.index(4),
            groove: rng.index(4),
        }
    }

    pub fn pitch(&self, root: i32, degree: i32, octave: i32) -> i32 {
        root + MODES[self.mode][degree.rem_euclid(7) as usize]
            + 12 * (octave + degree.div_euclid(7))
    }

    pub fn similar(&mut self, rng: &mut Rng) {
        // Preserve the opening hook, mode, harmony and rhythmic anchors. Explore
        // the answering half with a nearby scale step and a chord-tone resolution.
        let at = 4 + rng.index(3);
        let step = if rng.index(2) == 0 { -1 } else { 1 };
        self.notes[at] = (self.notes[at] + step).clamp(-2, 8);
        self.notes[7] = [0, 2, 4]
            .into_iter()
            .filter(|note| *note != self.notes[7])
            .nth(rng.index(2))
            .unwrap();
        // Recipes use response palettes of different sizes. Avoid choosing a
        // new index that aliases to the same bass, phrase or ghost-note pattern.
        let choices: Vec<_> = (0..24)
            .filter(|next| {
                [4, 6, 7]
                    .iter()
                    .all(|size| next % size != self.response % size)
            })
            .collect();
        self.response = choices[rng.index(choices.len())];
        self.voicing = (self.voicing + 1 + rng.index(3)) % 4;
    }

    pub fn vary(&mut self, amount: u8) {
        // Reversible, local edits. The phrase's first half remains the hook.
        self.notes[6] += i32::from(amount / 25) - 2;
        self.notes[7] = [0, 2, 4][(self.notes[7] as usize / 2 + usize::from(amount / 40) + 2) % 3];
        self.response = (self.response + usize::from(amount / 20) + 22) % 24;
    }

    pub fn phrase(&self, root: i32, octave: i32, activity: f64) -> String {
        let level = u8::from(activity >= 0.34) + u8::from(activity >= 0.67);
        (0..8)
            .map(|i| {
                let rhythm = if i < 4 {
                    self.rhythm
                } else {
                    self.response % RHYTHMS.len()
                };
                // Give even a sparse response a deliberate chord-tone landing.
                let rank = if i == 7 { 0 } else { RHYTHMS[rhythm][i] };
                if rank > level {
                    return "~".to_owned();
                }
                let note = self.pitch(root, self.notes[i], octave);
                if level == 2 && (rank == 2 || i == 0 || i == 4) {
                    let next = self.pitch(root, self.notes[(i + 1) % 8], octave);
                    format!("[{note} {next}]")
                } else {
                    note.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn bass(&self, root: i32, activity: f64) -> String {
        let fifth = root + 7;
        let follow = self.pitch(root, self.notes[7], 0);
        let response = match self.response % 4 {
            0 => format!("{fifth} ~ {follow} ~"),
            1 => format!("~ {follow} ~ {root}"),
            2 => format!("{follow} ~ ~ {fifth}"),
            _ => format!("~ {fifth} {follow} ~"),
        };
        let hook = match (self.groove, activity >= 0.67) {
            (0, false) => format!("{root} ~ ~ {root}"),
            (1, false) => format!("{root} ~ {fifth} ~"),
            (2, false) => format!("{root} {root} ~ ~"),
            (_, false) => format!("{root} ~ ~ {fifth}"),
            (0 | 2, true) => format!("{root} ~ [{root} {}] {root}", root + 12),
            (_, true) => format!("{root} [{root} {fifth}] ~ {}", root + 12),
        };
        format!("{hook} {response}")
    }

    pub fn chord_notes(&self, root: i32, octave: i32, extended: bool) -> Vec<Vec<i32>> {
        let count = if extended { 4 } else { 3 };
        let centre = root + octave * 12 + 7 + self.voicing as i32 * 2;
        let mut previous: Vec<i32> = (0..count).map(|i| centre - 4 + i * 3).collect();
        PROGRESSIONS[self.progression]
            .iter()
            .map(|degree| {
                let mut best = Vec::new();
                let mut cost = i32::MAX;
                for inversion in 0..count {
                    for register in -1..=1 {
                        let mut candidate: Vec<_> = (0..count)
                            .map(|i| {
                                self.pitch(root, degree + i * 2, octave + register)
                                    + if i < inversion { 12 } else { 0 }
                            })
                            .collect();
                        candidate.sort_unstable();
                        if candidate[0] < root + octave * 12 - 5
                            || candidate[count as usize - 1] > root + octave * 12 + 26
                        {
                            continue;
                        }
                        let movement: i32 = candidate
                            .iter()
                            .zip(&previous)
                            .map(|(a, b)| (a - b).abs())
                            .sum();
                        let distance = (candidate.iter().sum::<i32>() / count - centre).abs();
                        let score = movement * 3 + distance;
                        if score < cost {
                            cost = score;
                            best = candidate;
                        }
                    }
                }
                debug_assert!(!best.is_empty());
                previous = best.clone();
                best
            })
            .collect()
    }

    pub fn chords(&self, root: i32, octave: i32, extended: bool) -> String {
        let chords = self
            .chord_notes(root, octave, extended)
            .iter()
            .map(|chord| {
                format!(
                    "[{}]",
                    chord
                        .iter()
                        .map(i32::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("<{chords}>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_keep_their_hook_and_scale_through_long_similar_paths() {
        for seed in 0..32 {
            let mut rng = Rng(seed);
            let mut music = Music::new(&mut rng);
            let initial = music.clone();
            for _ in 0..96 {
                music.similar(&mut rng);
                assert_eq!(music.mode, initial.mode);
                assert_eq!(music.progression, initial.progression);
                assert_eq!(music.groove, initial.groove);
                assert_eq!(music.notes[..4], initial.notes[..4]);
                for activity in [0.0, 0.5, 1.0] {
                    let phrase = music.phrase(36, 2, activity);
                    let hook = initial.phrase(36, 2, activity);
                    // Compare rhythmic tokens, including bracketed subdivisions.
                    assert_eq!(
                        phrase.split(' ').take(4).collect::<Vec<_>>(),
                        hook.split(' ').take(4).collect::<Vec<_>>()
                    );
                }
                for degree in music.notes {
                    let pitch = music.pitch(36, degree, 2);
                    assert!((56..=74).contains(&pitch));
                    assert!(MODES[music.mode].contains(&(pitch - 36).rem_euclid(12)));
                }
            }
        }
    }

    #[test]
    fn chord_voices_are_ordered_in_key_and_move_smoothly() {
        for seed in 0..256 {
            let music = Music::new(&mut Rng(seed));
            for extended in [false, true] {
                let chords = music.chord_notes(36, 1, extended);
                for chord in &chords {
                    assert_eq!(chord.len(), if extended { 4 } else { 3 });
                    assert!(chord.windows(2).all(|pair| pair[0] < pair[1]));
                    assert!(
                        chord
                            .iter()
                            .all(|note| MODES[music.mode].contains(&(note - 36).rem_euclid(12)))
                    );
                }
                for (chord, next) in chords.iter().zip(chords.iter().cycle().skip(1)) {
                    for (a, b) in chord.iter().zip(next) {
                        assert!((a - b).abs() <= 7, "large voice jump: {chords:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn activity_adds_events_without_erasing_anchors() {
        for seed in 0..128 {
            let music = Music::new(&mut Rng(seed));
            let sparse = music.phrase(36, 1, 0.0);
            let medium = music.phrase(36, 1, 0.5);
            let dense = music.phrase(36, 1, 1.0);
            assert_ne!(sparse, medium);
            assert_ne!(medium, dense);
            assert_ne!(sparse.split_whitespace().next(), Some("~"));
        }
    }
}
