//! What a call's string argument stands for, which list completes it, and
//! how the status line names that list or the lack of one.

use super::*;
use crate::reference::ReferenceScope;

/// What a string handed to a function stands for, and so what list
/// completes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StringRole {
    Sound,
    /// The argument of `.bank(…)` itself: the machine a sound is asked
    /// from, not a sound. `s("cymbal").bank("Metal")` plays what
    /// `Metal_cymbal` names, so the string is completed by the machines
    /// the library holds - its banks' shared prefixes - and never by a
    /// whole sound name, which a bank argument would double up.
    Bank,
    Scale,
    Chord,
    Colour,
    /// Equal divisions of the octave. Its own word rather than a tuning,
    /// because `edo` refuses every tuning name that is not one.
    Edo,
    /// Named tunings, which `xen` and `tune` both read from the same
    /// dictionary.
    Tuning,
    /// The MIDI ports this machine offers, which is a list only this
    /// machine can know.
    MidiOut,
    MidiIn,
    /// A VST3 plugin: the first argument of `.vst()` and `.vsti()`. The
    /// plugin host has the list.
    #[cfg(feature = "vst")]
    Plugin,
    /// A preset file of the plugin of the call: the string of `preset`.
    #[cfg(feature = "vst")]
    Preset,
}

impl StringRole {
    /// True for a name with spaces: a choice replaces the whole string,
    /// not one word of the string.
    pub(super) fn whole_string(self) -> bool {
        match self {
            Self::MidiIn | Self::MidiOut => true,
            #[cfg(feature = "vst")]
            Self::Plugin | Self::Preset => true,
            _ => false,
        }
    }
}

impl App {
    /// What the function's current string argument stands for.
    ///
    /// The core vocabulary is known by name. Everything else says so in its
    /// own reference entry, in one of two ways. A parameter can declare a
    /// vocabulary word as its type - `scale`, `chord` - which is the way
    /// that holds however the parameter is spelled and wherever it sits in
    /// the argument list, so `xen("…")` offers tunings although its
    /// parameter is called `scaleNameOrRatios`. Only the parameter at the
    /// caret is consulted: `inspire("C:major", "…")` takes a density at
    /// that position, not another scale. Older entries instead name
    /// their first parameter after the vocabulary, which is why
    /// `inspire("ab:major", …)` has always worked; that reading stays
    /// first-parameter-only, because a name is a coincidence where a
    /// declared type is a decision.
    ///
    /// One word in one entry is the whole cost of teaching the editor about
    /// a new function. Nothing here is a second catalogue to keep in sync.
    pub(super) fn string_role(&self, callee: &str) -> Option<StringRole> {
        // Graph nodes share names with pattern functions. Their string
        // arguments do not inherit those functions' sound or scale lists.
        let scope = self.string_callee_scope(callee);
        if scope.is_kabelsalat() {
            return None;
        }
        let by_name = |name: &str| match name {
            "s" | "sound" => Some(StringRole::Sound),
            // `.bank(…)` completes by machine, not by sound: its string
            // names the machine, and the sound before it says what to
            // take from that machine.
            "bank" => Some(StringRole::Bank),
            "scale" => Some(StringRole::Scale),
            "chord" | "voicing" => Some(StringRole::Chord),
            "color" | "colour" => Some(StringRole::Colour),
            _ => None,
        };
        // The vocabulary is a closed set of words. A word with no list yet
        // is still a decision, and silences the older reading of the
        // parameter's name - `tune`'s parameter is called `scale` and its
        // scales are not the ones `.scale()` means, so guessing from the
        // name offers names that fail at query time.
        let declared = |word: &str| match word {
            "sound" => Some(Some(StringRole::Sound)),
            "scale" => Some(Some(StringRole::Scale)),
            "chord" => Some(Some(StringRole::Chord)),
            "colour" | "color" => Some(Some(StringRole::Colour)),
            // `edo` takes divisions of the octave and nothing else, so
            // it gets its own word: the tuning names would every one of
            // them be refused.
            "edo" => Some(Some(StringRole::Edo)),
            // The ports this machine has right now. Nothing compiled in
            // can know them, which is the whole reason to read them off
            // the dock rather than ask the reader to.
            "midiout" => Some(Some(StringRole::MidiOut)),
            "midiin" => Some(Some(StringRole::MidiIn)),
            // `xen` takes the divisions, the presets and these; `tune`
            // takes these alone. So a list of named tunings is right for
            // both - exactly right for `tune`, and short of the divisions
            // for `xen`, which offers less than it accepts rather than
            // more, and `edo` has those under its own word anyway.
            "tuning" => Some(Some(StringRole::Tuning)),
            _ => None,
        };
        // A type is written to be read - `scale | number[]`, `(string |
        // number[] )` - so the word is one alternative among several
        // rather than the whole string.
        let by_type = |written: &str| {
            written
                .split('|')
                .map(|word| word.trim().trim_matches(['(', ')', ' ']))
                .find_map(declared)
        };
        let argument = self.completion_argument_index(callee);
        // A plugin call has 2 strings with a list: the plugin name, and the
        // preset name in its object. Each other string is a pattern.
        #[cfg(feature = "vst")]
        if matches!(callee, "vst" | "vsti") {
            let key = self.object_key_before_string_at_caret();
            return match (argument, key.as_deref()) {
                (0, None) => Some(StringRole::Plugin),
                (1, Some("preset")) => Some(StringRole::Preset),
                _ => None,
            };
        }
        let entry = self
            .reference
            .lookup_in(callee, scope)
            .and_then(|index| self.reference.entry(index));
        // Historical core names describe their first argument only. In
        // particular, a second string in `s("bd", "…")` must not borrow
        // the first argument's list, nor may an unrelated object field.
        if argument == 0 && self.object_key_before_string_at_caret().is_none() {
            let canonical = entry.map_or(callee, |entry| entry.name.as_str());
            if let Some(role) = by_name(canonical) {
                return Some(role);
            }
        }
        let entry = entry?;
        let param = self.string_parameter_at_argument(entry, argument)?;
        match by_type(param.r#type.trim_start_matches("...")) {
            Some(role) => role,
            None if argument == 0 && !param.name.contains('.') => by_name(&param.name),
            None => None,
        }
    }

    /// Metadata queries away from this call retain the first-argument
    /// answer; completion within it always uses the actual position.
    fn completion_argument_index(&self, callee: &str) -> usize {
        let scope = self.string_callee_scope(callee);
        self.enclosing_call_argument_at_caret()
            .filter(|(active, _, _)| {
                active == callee
                    || self
                        .reference
                        .lookup_in(active, scope)
                        .is_some_and(|index| Some(index) == self.reference.lookup_in(callee, scope))
            })
            .map_or(0, |(_, _, argument)| argument)
    }

    fn string_callee_scope(&self, callee: &str) -> ReferenceScope {
        self.enclosing_call_at_caret()
            .filter(|(active, _)| active == callee)
            .map_or_else(
                || self.reference_scope_at_caret(),
                |(_, range)| self.reference_scope_at(range.start),
            )
    }

    /// Dotted documentation rows belong to an object argument rather than
    /// taking another positional slot. Select such a row only for its own
    /// field, so `lfo({ control: "…", shape: "…" }, "id")` completes
    /// the shape alone. A declared rest parameter keeps its later slots.
    fn string_parameter_at_argument<'a>(
        &self,
        entry: &'a super::super::reference::Entry,
        argument: usize,
    ) -> Option<&'a super::super::reference::Param> {
        let mut positional = entry
            .params
            .iter()
            .filter(|param| !param.name.trim_start_matches("...").contains('.'))
            .enumerate();
        let param = positional.find_map(|(index, param)| {
            (index == argument
                || (index < argument
                    && (param.name.starts_with("...") || param.r#type.starts_with("..."))))
            .then_some(param)
        })?;
        let Some(key) = self.object_key_before_string_at_caret() else {
            return Some(param);
        };
        entry.params.iter().find(|field| {
            let Some((parent, name)) = field.name.rsplit_once('.') else {
                return false;
            };
            parent == param.name
                && (name == key
                    || matches!(
                        (entry.name.as_str(), field.name.as_str(), key.as_str()),
                        ("lfo", "config.shape", "sh")
                            | ("talkit", "config.personality", "v")
                            | ("talkit", "config.personality", "voice")
                            | ("talkit", "config.quality", "q")
                            | ("talkit", "config.effort", "e")
                            | ("talkit", "config.language", "l")
                            | ("talkit", "config.language", "lang")
                    ))
        })
    }

    /// A documented finite list that can safely replace a word in this
    /// call's string. The entry is resolved first so aliases such as
    /// `wavetableWarpMode` inherit the canonical function's choices.
    pub(super) fn string_choice_set(
        &self,
        callee: &str,
    ) -> Option<&'static rustel_core::reference::ReferenceChoiceSet> {
        let scope = self.string_callee_scope(callee);
        if scope.is_kabelsalat() {
            return None;
        }
        let entry = self
            .reference
            .entry(self.reference.lookup_in(callee, scope)?)?;
        let param =
            self.string_parameter_at_argument(entry, self.completion_argument_index(callee))?;
        rustel_core::reference::reference_choices(&entry.name, &param.name)
            .filter(|set| set.completes_string)
    }

    /// The list that completes the string under the caret, named the way
    /// the status line says it: "the sounds", "the scales".
    pub(super) fn string_completion_panel_kind(&self) -> Option<&'static str> {
        let (callee, _, _) = self.string_completion_at_caret()?;
        if self.string_choice_set(&callee).is_some() {
            return Some("the documented choices");
        }
        match self.string_role(&callee)? {
            StringRole::Sound => Some("the sounds"),
            StringRole::Bank => Some("the sample banks"),
            StringRole::Scale => Some("the scales"),
            StringRole::Chord => Some("the chords"),
            StringRole::Colour => Some("the colours"),
            StringRole::Edo => Some("the equal divisions"),
            StringRole::Tuning => Some("the tunings"),
            StringRole::MidiOut => Some("the MIDI outputs"),
            StringRole::MidiIn => Some("the MIDI inputs"),
            #[cfg(feature = "vst")]
            StringRole::Plugin => Some("the plugins"),
            #[cfg(feature = "vst")]
            StringRole::Preset => Some("the presets"),
        }
    }

    /// What the string under the caret is asking for, when nothing can
    /// answer it: a name the score itself defines, or a string this
    /// engine has no list for. The words a chord says instead of opening
    /// a panel that would search functions for `myfunction`.
    pub(super) fn string_completion_refusal(&self) -> Option<String> {
        let (callee, _, _) = self.string_completion_at_caret()?;
        // `register('myfunction', …)` names a function of the score's own;
        // so do `registerControl` and `registerSound`. There is nothing to
        // complete, and the reference has nothing to say about a name that
        // does not exist yet.
        let refusal = match callee.as_str() {
            "register" | "registerControl" | "registerSound" | "registerSynthSounds" => {
                "this string names something of your own - nothing to complete"
            }
            _ => "nothing to complete inside this string",
        };
        Some(refusal.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Editor, Selection};

    #[test]
    fn talkit_option_values_have_choices() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = crate::app::tests::support::app_over(directory.path());
        for (marked, expected) in [
            ("talkit(\"hi\", { personality: \"ro|\" })", true),
            ("talkit(\"hi\", { v: \"ro|\" })", true),
            ("talkit(\"hi\", { voice: \"ro|\" })", true),
            ("talkit(\"hi\", { quality: \"mo|\" })", true),
            ("talkit(\"hi\", { q: \"mo|\" })", true),
            ("talkit(\"hi\", { effort: \"wh|\" })", true),
            ("talkit(\"hi\", { language: \"sp|\" })", true),
            ("talkit(\"hi\", { l: \"sp|\" })", true),
            ("talkit(\"hi\", { pitch: \"1|\" })", false),
            ("talkit(\"hi|\", { v: \"ro\" })", false),
        ] {
            let at = marked.find('|').expect("caret marker");
            app.scenes.current_mut().editor =
                Editor::new(&marked.replace('|', "")).expect("editor");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            assert_eq!(
                app.string_choice_set("talkit").is_some(),
                expected,
                "{marked}"
            );
        }
    }

    #[test]
    fn graph_calls_do_not_borrow_pattern_string_choices() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = crate::app::tests::support::app_over(directory.path());
        for (marked, callee, expected) in [
            ("s(\"bd|\")", "s", Some(StringRole::Sound)),
            ("K(() => s(\"bd|\"))", "s", None),
            ("K(() => S(\"bd|\"))", "S", None),
            ("K(() => S(s(\"bd|\")))", "s", Some(StringRole::Sound)),
            ("K(() => scale(\"c:major|\"))", "scale", None),
        ] {
            let at = marked.find('|').expect("caret marker");
            app.scenes.current_mut().editor =
                Editor::new(&marked.replace('|', "")).expect("editor");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            assert_eq!(app.string_role(callee), expected, "{marked}");
        }
        for (marked, choices) in [
            ("s(\"bd\").warpmode(\"asym|\")", true),
            ("K(() => warpmode(\"asym|\"))", false),
            ("K(() => S(s(\"bd\").warpmode(\"asym|\")))", true),
        ] {
            let at = marked.find('|').expect("caret marker");
            app.scenes.current_mut().editor =
                Editor::new(&marked.replace('|', "")).expect("editor");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            assert_eq!(
                app.string_choice_set("warpmode").is_some(),
                choices,
                "{marked}"
            );
        }
    }

    mod string_roles {
        //! Tests for `app/string_roles.rs`.

        use crate::app::string_roles::StringRole;
        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::{MemoryClipboard, Selection};

        #[test]
        fn wtshape_uses_the_documented_waveform_names() {
            let set = rustel_core::reference::reference_choices("wtshape", "shape")
                .expect("wtshape choices");
            assert_eq!(
                set.choices
                    .iter()
                    .map(|choice| choice.value)
                    .collect::<Vec<_>>(),
                ["triangle", "sine", "ramp", "saw", "square"]
            );
        }

        #[test]
        fn an_object_choice_only_opens_for_its_own_key() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let score = r#"$: s("sine").lfo({ control: "gain" })
"#;
            app.scenes.current_mut().editor = crate::editor::Editor::new(score).expect("editor");
            let at = score.find("gain").expect("control") + 2;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");

            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(
                app.reference_panel
                    .as_ref()
                    .is_none_or(|panel| panel.vocabulary.is_none()),
                "an LFO control name must not open the shape choices"
            );

            let score = "$: s(\"sine\").lfo({\n  \"shape\":\n    \"square\"\n})\n";
            app.scenes.current_mut().editor = crate::editor::Editor::new(score).expect("editor");
            let at = score.find("square").expect("shape") + 3;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            app.reference_panel = None;
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            let panel = app.reference_panel.as_ref().expect("shape choices");
            let vocabulary = panel.vocabulary.as_ref().expect("shape vocabulary");
            assert_eq!(
                panel
                    .selected_result()
                    .and_then(|index| vocabulary.names.get(index))
                    .map(String::as_str),
                Some("square"),
                "a multiline quoted object key still selects its current choice"
            );
        }

        /// A function that says its first parameter is a scale gets the
        /// scales for its first string, the way `.scale()` does: `inspire`
        /// from the extensions names it so in the reference.
        #[cfg(feature = "extensions")]
        #[test]
        fn a_function_whose_first_parameter_is_a_scale_offers_the_scales() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"piano\").inspire(\"c:mi".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            assert_eq!(app.string_completion_panel_kind(), Some("the scales"));
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("panel open");
            assert_eq!(panel.tab, crate::reference::Tab::Scales);
            assert_eq!(panel.scale_query, "c:mi");
        }

        /// A parameter that declares a vocabulary word as its type is offered
        /// that vocabulary, however the parameter is spelled. `xen` asks for a
        /// scale in a parameter called `scaleNameOrRatios`, which no reading of
        /// the name could have found.
        #[test]
        fn a_declared_vocabulary_offers_its_list_whatever_the_parameter_is_called() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: n(\"0 2 4\").xen(\"31e".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let (callee, _, _) = app
                .string_completion_at_caret()
                .expect("the caret stands in a string");
            assert_eq!(callee.as_str(), "xen");
            // `xen` and `tune` resolve against the EDO, preset and tune.js
            // dictionaries - never the tonaljs one `scale` offers, where
            // `xen("c:major")` is an error. They declare `tuning`, and get the
            // tunings rather than the scales.
            assert_eq!(app.string_role(&callee), Some(StringRole::Tuning));
            assert_eq!(app.string_role("tune"), Some(StringRole::Tuning));
            // The name path still answers for the entries that rely on it.
            assert_eq!(app.string_role("scale"), Some(StringRole::Scale));
            // `inspire` is one of the extensions, and a lean build has no
            // reference entry for it to read a vocabulary off.
            #[cfg(feature = "extensions")]
            assert_eq!(app.string_role("inspire"), Some(StringRole::Scale));
            assert_eq!(app.string_role("chord"), Some(StringRole::Chord));
            // `chord` is answered by the callee's name here, so this is a
            // regression guard rather than proof of the type path. The chord
            // word is declared on the control's own `symbols` parameter, which
            // is what a future chord-taking function will match on without any
            // change to this function.
            //
            // And a name with no vocabulary anywhere still answers nothing, so
            // the reference cannot start offering scales to everything.
            assert_eq!(app.string_role("room"), None);
            // `edo` was invisible to all of this until it was given an entry:
            // installed straight onto the globals, it had no reference at all.
            // It resolves now, and declares the tuning vocabulary rather than
            // borrowing the scales.
            let edo = app
                .reference
                .resolve("edo")
                .and_then(|found| app.reference.entry(found))
                .expect("edo has a reference entry");
            assert_eq!(
                edo.params.first().map(|param| param.r#type.as_str()),
                Some("edo")
            );
            assert_eq!(app.string_role("edo"), Some(StringRole::Edo));
            // And it offers the divisions rather than the tuning names, every
            // one of which `edo` refuses.
            let names = crate::reference::edo_vocabulary();
            assert!(names.contains(&"31edo".to_owned()), "{names:?}");
            assert!(
                names.iter().all(|name| rustel_core::xen::edo(name).is_ok()),
                "every offered division must be one the engine accepts"
            );
        }

        /// `register('myfunction', …)` names something of the score's own:
        /// there is nothing to complete, and searching the function list for
        /// a name that does not exist yet answers a question nobody asked.
        #[test]
        fn a_string_that_names_something_of_your_own_completes_nothing() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("register('myfunction".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            assert!(
                app.reference_panel.is_none(),
                "no list is opened for a name the score invents"
            );
            assert!(app.status.contains("nothing to complete"), "{}", app.status);

            // Ctrl+D still explains the enclosing function's arguments.
            app.toggle_reference_at_caret();
            let panel = app.reference_panel.as_ref().expect("register arguments");
            assert_eq!(panel.tab, crate::reference::Tab::Reference);
            let crate::reference::ReferenceMode::Entry { index, .. } = panel.mode else {
                panic!("expected the enclosing function's arguments")
            };
            assert_eq!(app.reference.entry(index).unwrap().name, "register");
            assert!(panel.vocabulary.is_none());
            assert!(app.reference_anchor.is_none());

            // A string that DOES have a list still says which one.
            app.set_reference_panel(None);
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("', (x, p) => p.decay(x))\n$: s('bd".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("a sound");
            app.toggle_reference_at_caret();
            assert!(app.status.contains("the sounds"), "{}", app.status);
        }
    }
}
