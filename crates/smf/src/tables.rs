/*
tables.rs - General MIDI program and drum tables
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! General MIDI names used by import and export.
//!
//! The program table follows the order of `crates/runtime/assets/gm-fonts.json`.
//! Programs 0 through 3 all use the first font. The runtime test in `sounds.rs`
//! keeps the table aligned with the asset.

/// The General MIDI program names, indexed by program number.
pub const GM_PROGRAMS: [&str; 128] = [
    "gm_piano",
    "gm_piano",
    "gm_piano",
    "gm_piano",
    "gm_epiano1",
    "gm_epiano2",
    "gm_harpsichord",
    "gm_clavinet",
    "gm_celesta",
    "gm_glockenspiel",
    "gm_music_box",
    "gm_vibraphone",
    "gm_marimba",
    "gm_xylophone",
    "gm_tubular_bells",
    "gm_dulcimer",
    "gm_drawbar_organ",
    "gm_percussive_organ",
    "gm_rock_organ",
    "gm_church_organ",
    "gm_reed_organ",
    "gm_accordion",
    "gm_harmonica",
    "gm_bandoneon",
    "gm_acoustic_guitar_nylon",
    "gm_acoustic_guitar_steel",
    "gm_electric_guitar_jazz",
    "gm_electric_guitar_clean",
    "gm_electric_guitar_muted",
    "gm_overdriven_guitar",
    "gm_distortion_guitar",
    "gm_guitar_harmonics",
    "gm_acoustic_bass",
    "gm_electric_bass_finger",
    "gm_electric_bass_pick",
    "gm_fretless_bass",
    "gm_slap_bass_1",
    "gm_slap_bass_2",
    "gm_synth_bass_1",
    "gm_synth_bass_2",
    "gm_violin",
    "gm_viola",
    "gm_cello",
    "gm_contrabass",
    "gm_tremolo_strings",
    "gm_pizzicato_strings",
    "gm_orchestral_harp",
    "gm_timpani",
    "gm_string_ensemble_1",
    "gm_string_ensemble_2",
    "gm_synth_strings_1",
    "gm_synth_strings_2",
    "gm_choir_aahs",
    "gm_voice_oohs",
    "gm_synth_choir",
    "gm_orchestra_hit",
    "gm_trumpet",
    "gm_trombone",
    "gm_tuba",
    "gm_muted_trumpet",
    "gm_french_horn",
    "gm_brass_section",
    "gm_synth_brass_1",
    "gm_synth_brass_2",
    "gm_soprano_sax",
    "gm_alto_sax",
    "gm_tenor_sax",
    "gm_baritone_sax",
    "gm_oboe",
    "gm_english_horn",
    "gm_bassoon",
    "gm_clarinet",
    "gm_piccolo",
    "gm_flute",
    "gm_recorder",
    "gm_pan_flute",
    "gm_blown_bottle",
    "gm_shakuhachi",
    "gm_whistle",
    "gm_ocarina",
    "gm_lead_1_square",
    "gm_lead_2_sawtooth",
    "gm_lead_3_calliope",
    "gm_lead_4_chiff",
    "gm_lead_5_charang",
    "gm_lead_6_voice",
    "gm_lead_7_fifths",
    "gm_lead_8_bass_lead",
    "gm_pad_new_age",
    "gm_pad_warm",
    "gm_pad_poly",
    "gm_pad_choir",
    "gm_pad_bowed",
    "gm_pad_metallic",
    "gm_pad_halo",
    "gm_pad_sweep",
    "gm_fx_rain",
    "gm_fx_soundtrack",
    "gm_fx_crystal",
    "gm_fx_atmosphere",
    "gm_fx_brightness",
    "gm_fx_goblins",
    "gm_fx_echoes",
    "gm_fx_sci_fi",
    "gm_sitar",
    "gm_banjo",
    "gm_shamisen",
    "gm_koto",
    "gm_kalimba",
    "gm_bagpipe",
    "gm_fiddle",
    "gm_shanai",
    "gm_tinkle_bell",
    "gm_agogo",
    "gm_steel_drums",
    "gm_woodblock",
    "gm_taiko_drum",
    "gm_melodic_tom",
    "gm_synth_drum",
    "gm_reverse_cymbal",
    "gm_guitar_fret_noise",
    "gm_breath_noise",
    "gm_seashore",
    "gm_bird_tweet",
    "gm_telephone",
    "gm_helicopter",
    "gm_applause",
    "gm_gunshot",
];

/// General MIDI drum notes that have a name the runtime plays.
///
/// Notes outside this table are dropped on import and never guessed.
pub const GM_DRUMS: [(u8, &str); 24] = [
    (35, "bd"),
    (36, "bd"),
    (37, "rim"),
    (38, "sd"),
    (39, "cp"),
    (40, "sd"),
    (41, "lt"),
    (42, "hh"),
    (43, "lt"),
    (44, "hh"),
    (45, "mt"),
    (46, "oh"),
    (47, "mt"),
    (48, "ht"),
    (49, "cr"),
    (50, "ht"),
    (51, "rd"),
    (53, "rd"),
    (55, "cr"),
    (56, "cb"),
    (57, "cr"),
    (59, "rd"),
    (69, "sh"),
    (70, "sh"),
];

/// The sound before a `:variant` suffix.
pub fn base_sound_name(name: &str) -> &str {
    name.split_once(':').map_or(name, |(base, _)| base)
}

/// The General MIDI program for a sound name, or `None`.
pub fn program_for_sound(sound: &str) -> Option<u8> {
    let base = base_sound_name(sound);
    GM_PROGRAMS
        .iter()
        .position(|name| *name == base)
        .and_then(|program| u8::try_from(program).ok())
}

/// The General MIDI sound for a program, or `None`.
pub fn sound_for_program(program: u8) -> Option<&'static str> {
    GM_PROGRAMS.get(usize::from(program)).copied()
}

/// The General MIDI drum note for a sound name, or `None`.
pub fn drum_note_for_name(name: &str) -> Option<u8> {
    let base = base_sound_name(name);
    GM_DRUMS
        .iter()
        .find(|(_, drum)| *drum == base)
        .map(|(note, _)| *note)
}

/// The drum name for a General MIDI note, or `None`.
pub fn drum_name_for_note(note: u8) -> Option<&'static str> {
    GM_DRUMS
        .iter()
        .find(|(candidate, _)| *candidate == note)
        .map(|(_, drum)| *drum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_table_has_every_general_midi_program() {
        assert_eq!(GM_PROGRAMS.len(), 128);
        assert_eq!(&GM_PROGRAMS[..4], &["gm_piano"; 4]);
        assert_eq!(GM_PROGRAMS[127], "gm_gunshot");
    }

    #[test]
    fn known_programs_land_on_their_general_midi_names() {
        assert_eq!(sound_for_program(40), Some("gm_violin"));
        assert_eq!(sound_for_program(56), Some("gm_trumpet"));
        assert_eq!(sound_for_program(72), Some("gm_piccolo"));
        assert_eq!(sound_for_program(127), Some("gm_gunshot"));
        assert_eq!(sound_for_program(128), None);
    }

    #[test]
    fn sound_names_resolve_with_or_without_a_variant() {
        assert_eq!(program_for_sound("gm_piano"), Some(0));
        assert_eq!(program_for_sound("gm_violin"), Some(40));
        assert_eq!(program_for_sound("gm_violin:2"), Some(40));
        assert_eq!(program_for_sound("my_gm_violin"), None);
        assert_eq!(base_sound_name("gm_piano:2"), "gm_piano");
        assert_eq!(base_sound_name("bd"), "bd");
    }

    #[test]
    fn the_drum_table_covers_the_named_notes_and_never_guesses() {
        assert_eq!(drum_note_for_name("bd"), Some(35));
        assert_eq!(drum_note_for_name("bd:3"), Some(35));
        assert_eq!(drum_name_for_note(36), Some("bd"));
        assert_eq!(drum_name_for_note(38), Some("sd"));
        assert_eq!(drum_name_for_note(52), None);
        assert_eq!(drum_note_for_name("tambourine"), None);
        for (note, name) in GM_DRUMS {
            assert_eq!(drum_name_for_note(note), Some(name), "note {note}");
        }
    }
}
