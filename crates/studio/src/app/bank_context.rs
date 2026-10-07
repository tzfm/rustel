//! Bank inference for completion: the literal sounds that reach a `.bank(…)`
//! call, the bank that applies at the caret, and the library machines to
//! offer.

use rustel_core::controls::canonical_control_name;

use super::*;
use crate::reference::SoundEffect;
use rustel_runtime::lint::scan;

/// The machines named by the bank that applies to a sound at `at`, each
/// once, in the order written. That bank is [`rustel_runtime::lint::applied_bank`].
/// It names the machines of the string its argument opens with, read as
/// preload reads it with [`rustel_runtime::sounds::string_argument`]: a template's
/// `${…}` is left out, so `` .bank(`Metal${x}`) `` names `Metal`. A bank
/// whose argument opens with anything else names nothing.
pub(super) fn chain_banks(source: &str, at: usize) -> Vec<String> {
    let code = rustel_runtime::lint::code_only(source);
    rustel_runtime::lint::applied_bank(&code, at, None)
        .and_then(|bank| rustel_runtime::sounds::string_argument(&source[bank.open + 1..]))
        .map(|pattern| rustel_runtime::sounds::bank_pattern_machines(&pattern))
        .unwrap_or_default()
}

/// The literal sounds that reach the `.bank(…)` call at `caret` through its
/// receiver chain, or none when any link might change them.
///
/// Each link is judged by [`Reference::sound_effect`] and its argument; an
/// undeclared link returns none.
pub(super) fn bank_receiver_sounds(
    source: &str,
    caret: usize,
    reference: &Reference,
) -> Vec<String> {
    receiver_sounds(source, caret, reference).unwrap_or_default()
}

fn receiver_sounds(source: &str, caret: usize, reference: &Reference) -> Option<Vec<String>> {
    let code = rustel_runtime::lint::code_only(source.get(..caret)?);
    let open = scan::open_parens(&code, code.len()).next()?;
    let bank = scan::name_before(&code, open)?;
    if canonical_control_name(&code[bank.clone()]) != Some("bank") {
        return None;
    }
    let mut renumbered = false;
    let mut from = bank.start;
    loop {
        let link = scan::link_before(&code, from)?;
        let argument = link.open + 1..link.close?;
        let effect = reference.sound_effect(&code[link.name.clone()])?;
        if effect.source {
            return Some(literal_bank_sounds(&source[argument], !renumbered));
        }
        if !lets_through(effect, &code[argument.clone()], &source[argument]) {
            return None;
        }
        renumbered |= effect.renumbers;
        from = link.name.start;
    }
}

/// Whether the receiver's sounds pass through a link with `effect`, given
/// its argument as blanked `code` and as source `text`. An inline function
/// or a nested sound call always blocks them.
fn lets_through(effect: SoundEffect, code: &str, text: &str) -> bool {
    let builds = scan::holds_function(code)
        || code.match_indices('(').any(|(at, _)| {
            scan::name_before(code, at)
                .is_some_and(|name| canonical_control_name(&code[name]) == Some("s"))
        });
    !(builds
        || effect.replaces
        || (effect.undocumented && !code.trim().is_empty())
        || (effect.joins && !plain_literal(text)))
}

/// The body of a string literal that is the whole of `text`, when nothing
/// in it is computed: no escape, no interpolation, no second string.
fn literal_body(text: &str) -> Option<&str> {
    let quote = text
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let body = text[1..].strip_suffix(quote)?;
    (!body.contains(quote) && !body.contains('\\') && !(quote == '`' && body.contains("${")))
        .then_some(body)
}

/// An argument that is a lone string or number, or nothing: a value with
/// no controls, so no sound, behind it.
fn plain_literal(argument: &str) -> bool {
    let text = argument.trim();
    // `inf` and `nan` parse as numbers too, but in a score they are names.
    let number = text
        .strip_prefix('-')
        .unwrap_or(text)
        .starts_with(|c: char| c.is_ascii_digit() || c == '.')
        && text.parse::<f64>().is_ok();
    text.is_empty() || number || literal_body(text).is_some()
}

/// The sounds named by `argument` when it is a whole string literal, each
/// once, for bank matching. Native synths are skipped, a single-atom sample
/// number stays (`bd:3`), a patterned one (`bd:<0 3>`) is dropped, and a
/// range yields none. Words come from the lint's
/// [`mini_words_of`](rustel_runtime::lint::mini_words_of). With `keep_index` false,
/// every sample number is dropped.
fn literal_bank_sounds(argument: &str, keep_index: bool) -> Vec<String> {
    let Some(words) = literal_body(argument.trim()).and_then(rustel_runtime::lint::mini_words_of)
    else {
        return Vec::new();
    };
    // A range may generate names not present as literal atoms.
    if words.iter().any(|word| word.ranged) {
        return Vec::new();
    }
    let mut names = Vec::new();
    for word in &words {
        let mut parts = word.text.split(':');
        let head = parts.next().unwrap_or_default().trim();
        // The engine plays a native synth by its own name under any bank,
        // and the lint lets it pass, so it asks nothing of the bank.
        if !head.chars().any(char::is_alphabetic) || rustel_voice::is_native_synth_sound(head) {
            continue;
        }
        let index = parts
            .next()
            .filter(|_| keep_index && word.written_index)
            .and_then(|index| index.trim().parse::<usize>().ok());
        let name = index.map_or_else(|| head.to_owned(), |index| format!("{head}:{index}"));
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The library's sample entries, each name beside its lowercase form, and
/// the most variants any entry of a lowercase name holds. Bank matching
/// ignores case, and a bank list asks about every entry for every machine:
/// lowercased inside those loops, that was an allocation per pair, on the
/// UI thread, each time the list opened or the library changed under it.
/// The input monitor and the engine's own voices are left out: a channel
/// count and an oscillator are reached by their own names, and no
/// `.bank()` can bank them.
struct BankNames<'a> {
    entries: Vec<(&'a str, String)>,
    variants: HashMap<String, usize>,
}

impl<'a> BankNames<'a> {
    fn new(entries: &'a [rustel_runtime::samples::SoundEntry]) -> Self {
        use rustel_runtime::samples::SoundOrigin;
        let mut names = Vec::with_capacity(entries.len());
        let mut variants = HashMap::with_capacity(entries.len());
        for entry in entries {
            if matches!(entry.origin, SoundOrigin::Synth | SoundOrigin::Input) {
                continue;
            }
            let lower = entry.name.to_lowercase();
            let most = variants.entry(lower.clone()).or_insert(0);
            *most = entry.variants.max(*most);
            names.push((entry.name.as_str(), lower));
        }
        Self {
            entries: names,
            variants,
        }
    }
}

/// The machines a loaded library answers to, for completing a `.bank(…)`
/// argument: the prefix each bank name shares - `Metal_cymbal`'s machine
/// is `Metal`. Deduped and alphabetised - the catalogue's own order is a
/// map's, which is no order at all. An absent or empty library lists
/// nothing: the offer is exactly what would play.
fn bank_machines(names: &BankNames<'_>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut machines: Vec<String> = Vec::new();
    for (name, _) in &names.entries {
        let Some(machine) = name.split_once('_').map(|(prefix, _)| prefix) else {
            continue;
        };
        if seen.insert(machine) {
            machines.push(machine.to_owned());
        }
    }
    machines.sort();
    machines
}

/// The machines that can play every sound the bank call's receiver names,
/// with every explicit variant: `bd:3` needs a machine with four kicks.
///
/// Bank and sound names may both contain underscores, so splitting at the
/// first underscore loses names such as `wt_digital`.  The sound already
/// written in the score gives us the unambiguous boundary instead:
/// `wt_digital_basique` minus `_basique` is `wt_digital`.
fn bank_machines_for_sounds(names: &BankNames<'_>, sounds: &[String]) -> Vec<String> {
    if sounds.is_empty() {
        return bank_machines(names);
    }
    let requirements = sounds
        .iter()
        .map(|sound| {
            let mut parts = sound.split(':');
            let name = parts.next().unwrap_or(sound).to_lowercase();
            let index = parts.next().and_then(|index| index.parse::<usize>().ok());
            (name, index)
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut machines = Vec::new();
    for (name, lower) in &names.entries {
        for (sound, _) in &requirements {
            // Only a name that ends in the sound can hold it; the rest need
            // no closer look.
            if !lower.ends_with(sound.as_str()) {
                continue;
            }
            // Match at original character boundaries: lowercasing Unicode
            // can change a name's byte length, but the inserted spelling
            // must stay the catalogue's own.
            for (at, character) in name.char_indices() {
                if character != '_' || name[at + 1..].to_lowercase() != *sound {
                    continue;
                }
                let machine = &name[..at];
                if !machine.is_empty() && seen.insert(machine.to_lowercase()) {
                    machines.push(machine.to_owned());
                }
            }
        }
    }
    machines.retain(|machine| {
        let machine = machine.to_lowercase();
        requirements.iter().all(|(sound, index)| {
            names
                .variants
                .get(&format!("{machine}_{sound}"))
                .is_some_and(|most| index.is_none_or(|index| *most > index))
        })
    });
    machines.sort();
    machines
}

/// The machines a `.bank(…)` completion offers: the ones that can play
/// every sound the bank call's receiver names leading, every other machine
/// the library holds behind them.
///
/// Return the compatible prefix length as well, so the picker can hide
/// the remaining names until Backspace explicitly opens them. An entry a
/// known machine already speaks for is no evidence of a shorter one:
/// `wt_digital_basique` stays `wt_digital`'s, and the naive first-underscore
/// split - which would also offer `wt` - never runs over it.
pub(super) fn bank_machines_ranked(
    entries: &[rustel_runtime::samples::SoundEntry],
    sounds: &[String],
) -> (Vec<String>, usize) {
    let names = BankNames::new(entries);
    let mut ranked = bank_machines_for_sounds(&names, sounds);
    let compatible_count = ranked.len();
    let mut known: HashSet<String> = ranked
        .iter()
        .map(|machine| machine.to_lowercase())
        .collect();
    let mut rest: Vec<String> = Vec::new();
    // A missing variant or second sound does not change a bank's spelling.
    // Preserve contextual boundaries for incompatible banks too, so showing
    // all banks still offers `wt_digital` when `basique:3` is unavailable.
    for sound in sounds {
        let name = sound.split(':').next().unwrap_or(sound).to_owned();
        for machine in bank_machines_for_sounds(&names, &[name]) {
            if known.insert(machine.to_lowercase()) {
                rest.push(machine);
            }
        }
    }
    for (name, lower) in &names.entries {
        if lower
            .match_indices('_')
            .any(|(at, _)| known.contains(&lower[..at]))
        {
            continue;
        }
        let Some(machine) = name.split_once('_').map(|(prefix, _)| prefix) else {
            continue;
        };
        if known.insert(machine.to_lowercase()) {
            rest.push(machine.to_owned());
        }
    }
    rest.sort();
    ranked.extend(rest);
    (ranked, compatible_count)
}

impl App {
    /// Literal sound requirements in the receiver chain of the bank call
    /// at the caret.
    pub(super) fn bank_receiver_sounds_at_caret(&self) -> Vec<String> {
        let editor = self.editor();
        let caret = editor.primary_selection().head;
        bank_receiver_sounds(&editor.source(), caret.0, &self.reference)
    }

    /// The machines of the bank that applies at the caret, keeping only
    /// those the library holds.
    pub(super) fn banks_around_caret(&self) -> Vec<String> {
        let editor = self.editor();
        let caret = editor.primary_selection().head;
        let named = chain_banks(&editor.source(), caret.0);
        if named.is_empty() {
            return Vec::new();
        }
        let catalogue = self.worker.catalogue();
        named
            .into_iter()
            .filter(|bank| {
                let prefix = format!("{bank}_");
                catalogue
                    .sounds
                    .iter()
                    .any(|entry| entry.name.starts_with(&prefix))
            })
            .collect()
    }
}
