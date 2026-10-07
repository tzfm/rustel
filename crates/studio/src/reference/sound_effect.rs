//! What a chained call does to its receiver's sounds, read from
//! declarations: the control-table key a name writes, parameter types, and
//! the `combiners` and `selectors` tags. The only names here are the `s`
//! and `n` keys; no function is listed.

use rustel_core::controls::canonical_control_name;

use super::Reference;

/// What a call declares about the sounds of its receiver. The caller weighs
/// these facts against the argument actually given.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SoundEffect {
    /// Writes the sound (`s`, `sound`).
    pub(crate) source: bool,
    /// Writes the sample number (`n`).
    pub(crate) renumbers: bool,
    /// Its arguments decide what plays: a parameter takes a function, or
    /// the entry is tagged `selectors`.
    pub(crate) replaces: bool,
    /// Can bring in another pattern: tagged `combiners`, or a parameter typed
    /// `Pattern` or `any`.
    pub(crate) joins: bool,
    /// Documents no parameters and is not a control.
    pub(crate) undocumented: bool,
}

impl Reference {
    /// What `callee` declares about its receiver's sounds, or `None` when
    /// nothing declares it. A control hidden from the reference is still
    /// declared by its control-table row.
    pub(crate) fn sound_effect(&self, callee: &str) -> Option<SoundEffect> {
        let control = canonical_control_name(callee);
        if control == Some("s") {
            return Some(SoundEffect {
                source: true,
                ..SoundEffect::default()
            });
        }
        let renumbers = control == Some("n");
        let Some(entry) = self.resolve(callee).and_then(|index| self.entry(index)) else {
            return control.map(|_| SoundEffect {
                renumbers,
                ..SoundEffect::default()
            });
        };
        let tagged = |word: &str| entry.tags.iter().any(|tag| tag == word);
        Some(SoundEffect {
            source: false,
            renumbers,
            replaces: tagged("selectors")
                || entry
                    .params
                    .iter()
                    .any(|param| param.r#type.to_ascii_lowercase().contains("function")),
            joins: tagged("combiners")
                || entry.params.iter().any(|param| {
                    let kind = param.r#type.trim();
                    kind == "Pattern" || kind.contains("any")
                }),
            undocumented: entry.params.is_empty() && control.is_none(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> &'static Reference {
        static REFERENCE: std::sync::OnceLock<Reference> = std::sync::OnceLock::new();
        REFERENCE.get_or_init(|| Reference::load(|_| true))
    }

    fn effect(name: &str) -> SoundEffect {
        reference()
            .sound_effect(name)
            .unwrap_or_else(|| panic!("{name} is declared"))
    }

    /// `sound_effect` answers from the declarations as tabled here.
    #[test]
    fn every_effect_is_read_off_a_declaration() {
        for name in ["s", "sound"] {
            let source = SoundEffect {
                source: true,
                ..SoundEffect::default()
            };
            assert_eq!(effect(name), source, "{name}");
        }
        let renumbers = SoundEffect {
            renumbers: true,
            ..SoundEffect::default()
        };
        assert_eq!(effect("n"), renumbers);
        for name in ["vel", "velocity", "dec", "lpf", "fast", "note"] {
            assert_eq!(effect(name), SoundEffect::default(), "{name}");
        }
        for name in [
            "sometimes",
            "every",
            "withValue",
            "fmap",
            "apply",
            "source",
            "pick",
            "pickF",
            "pickSqueeze",
            "pickmodSqueeze",
            "inhabit",
            "inhabitmod",
            "squeeze",
        ] {
            assert!(effect(name).replaces, "{name}: {:?}", effect(name));
        }
        for name in ["layer", "superimpose"] {
            let effect = effect(name);
            assert!(effect.undocumented && effect.joins, "{name}: {effect:?}");
        }
        assert!(effect("stack").undocumented, "{:?}", effect("stack"));
        for name in ["cat", "slowcat", "set", "appLeft", "appBoth"] {
            let effect = effect(name);
            assert!(effect.joins && !effect.replaces, "{name}: {effect:?}");
        }
        assert_eq!(effect("chorus"), SoundEffect::default());
        for name in ["customTransform", "map"] {
            assert_eq!(reference().sound_effect(name), None, "{name}");
        }
    }

    /// Every `pick*` name is declared a selector.
    #[test]
    fn every_pick_is_declared_a_selector() {
        let reference = reference();
        let picks = (0..reference.len())
            .filter_map(|index| reference.entry(index))
            .flat_map(|entry| std::iter::once(&entry.name).chain(&entry.synonyms))
            .filter(|name| name.starts_with("pick"))
            .collect::<Vec<_>>();
        assert!(picks.len() >= 12, "{picks:?}");
        for name in picks {
            assert!(effect(name).replaces, "{name}");
        }
    }

    /// Every combinator registered with `takes_function` declares it, so its own
    /// entry reads as replacing the sounds.
    #[test]
    fn a_combinator_that_takes_a_function_declares_it() {
        let registries = [
            rustel_core::register::default_registry(),
            #[cfg(feature = "extensions")]
            rustel_ext::default_registry(),
        ];
        for registry in &registries {
            for entry in registry.reference_entries() {
                let Some(registration) = registry.get(entry.name) else {
                    continue;
                };
                if !registration.takes_function || registration.reference.name != entry.name {
                    continue;
                }
                let alone = Reference::default().with_entry(super::super::Entry::from(entry));
                assert!(
                    alone
                        .sound_effect(entry.name)
                        .is_some_and(|effect| effect.replaces),
                    "{} takes a function and does not declare it: {:?}",
                    entry.name,
                    entry.params
                );
            }
        }
    }
}
