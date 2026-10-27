//! The knobs panel's hands: the reading of the call at the caret, the
//! writes that move a rail, and the panel's keys. The panel itself - its
//! rows, its rail cells, its sheets - is `knobs_panel.rs`. This file is
//! what the App does with it.
//!
//! The score is the master. A rail moves only what the score already
//! holds: the number of a parameter, or the value argument of the
//! `slider(...)` the parameter is. The panel's own state is what the hand
//! is doing - the selection, the sheet that is open, and the entries the
//! hand took out at the far-left stop, which stay until the caret leaves
//! the call so a move right can write them back.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use rustel_runtime::lint::{code_only, scan};
use rustel_runtime::ui_events::literal_sliders;
#[cfg(feature = "vst")]
use rustel_runtime::vst::{self, ParamInfo, ParamRole};

use super::args::{members, parameter_edit, plain_key};
use super::list_keys::page_selection;
use super::slider::{SliderTrack, Travel, pointer_value, rail_stepped, snap_value};
use super::smart_action::guess_range;
use super::*;
use crate::knobs_panel::{
    AddList, CallAt, Fader, HeldOff, HeldText, Knob, KnobEdit, KnobPlace, KnobPresets, KnobState,
    KnobsPanel, add_row_at, add_sheet, add_shown, knob_at, knob_row, parts, presets_rows, shown,
};
use crate::number_form::{Field, SliderForm, format_value, short_number};
use crate::prefs::KnobRange;
use crate::reference::score_key;
use rustel_core::controls::canonical_control_name;
use rustel_transpiler::is_frequency_control;

/// The call the caret is in or on the name of, read once per change of
/// the score or the caret. Everything the rows need from the text is
/// here, so no write has to re-read what a frame just read.
pub(super) struct KnobsParse {
    scene: SceneId,
    revision: Revision,
    caret: usize,
    call: Option<CallParse>,
    /// The keys the reference documents for the call's options object,
    /// when the call is no plugin and no control.
    options: Vec<DocOption>,
    /// The named plugin's parameters, once the host has them: a plugin
    /// still loading is asked again each frame until it answers, then
    /// its list stays with the parse. A frame costs no ask.
    #[cfg(feature = "vst")]
    params: Option<Vec<ParamInfo>>,
}

/// One call at the caret: its name, its `(`, its numeric parameters, and
/// the members of its options object for the writes that remove one.
pub(super) struct CallParse {
    name: String,
    open: usize,
    /// The call's `)`, when the score has it.
    close: Option<usize>,
    /// The plugin the call names, when it is a `vst` or `vsti` call.
    plugin: Option<String>,
    /// The call is an audio control, such as `lpf` or `gain`: it takes a
    /// number, and its name says what the number is.
    control: bool,
    /// The `preset` the call's object names, when a quoted string gives
    /// it: the preset the score plays.
    preset: Option<String>,
    knobs: Vec<Knob>,
    entries: Vec<Range<usize>>,
    /// Every key the options object holds, a rail or not: `attack: "0.1
    /// 0.2"` holds the attack as a pattern, which no rail moves.
    keys: Vec<String>,
    /// Whether the options object's closing brace and the call's closing
    /// paren are in the score. An open object's last member reaches the
    /// end of the file, so its entries cannot be taken out one at a time.
    entries_closed: bool,
}

impl CallParse {
    /// The control a rail's number sets, when the score names one: an
    /// entry's key, or the control a call's first argument is. A plugin
    /// names its own parameters, so none of them is a control.
    fn control_name<'a>(&'a self, place: &'a KnobPlace) -> Option<&'a str> {
        match place {
            KnobPlace::Entry(key) if self.plugin.is_none() => Some(key),
            KnobPlace::Argument(0) if self.control => Some(&self.name),
            _ => None,
        }
    }
}

/// The rows the painter draws, assembled from the parse, the panel's held
/// off-entries and the remembered plugin parameters.
pub(super) struct KnobsRows {
    pub name: Option<String>,
    /// The call has a list for `a` to add from: a plugin's parameters,
    /// or the options its reference documents.
    pub listed: bool,
    /// The `preset` the call's object names, when it names one: the mark
    /// beside the plugin's presets.
    pub preset: Option<String>,
    pub knobs: Vec<Knob>,
}

/// The call the caret is in or stands on the name of: the innermost one
/// that has a name, because a fader's own numbers - `slider(200, 100,
/// 2000)` - are the fader's business, not four knobs.
fn call_at(code: &str, caret: usize) -> Option<(Range<usize>, usize)> {
    let bytes = code.as_bytes();
    let typed = scan::name_ending_at(code, caret);
    let name = scan::name_starting_at(code, typed.start);
    let on_call = (!name.is_empty())
        .then(|| name.end + (code[name.end..].len() - code[name.end..].trim_start().len()))
        .filter(|open| bytes.get(*open) == Some(&b'('));
    on_call
        .into_iter()
        .chain(scan::open_parens(code, caret))
        .filter_map(|open| Some((scan::name_before(code, open)?, open)))
        .find(|(name, _)| &code[name.clone()] != "slider")
}

/// A range's own bytes, trimmed of the spaces and the blanked comments
/// around it. Both ends move, because the trim is of the blanked text -
/// a trailing comment is not part of the value.
fn trimmed(code: &str, range: Range<usize>) -> Option<Range<usize>> {
    let text = code.get(range.clone())?;
    let start = range.start + text.len() - text.trim_start().len();
    Some(start..start + text.trim().len())
}

/// The number a byte range of code holds, when it holds one outright:
/// number characters only, so an identifier or a note never passes, and
/// the parse decides.
fn number_literal(code: &str, range: Range<usize>) -> Option<(Range<usize>, f64)> {
    let literal = trimmed(code, range.clone())?;
    let text = code.get(literal.clone())?;
    let number = text
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        .then(|| text.parse::<f64>().ok().filter(|n| n.is_finite()))
        .flatten()?;
    Some((literal, number))
}

/// Where an entry of an object divides: its key and its value, split at
/// the first colon the object's own depth. A colon inside a nested value
/// - `{a: {b: 1}}`, `{a: c ? 1 : 2}` - belongs to the value.
fn entry_parts(code: &str, entry: Range<usize>) -> Option<(Range<usize>, Range<usize>)> {
    let mut depth = 0usize;
    for (at, byte) in code.as_bytes()[entry.clone()].iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => {
                let colon = entry.start + at;
                return Some((entry.start..colon, colon + 1..entry.end));
            }
            _ => {}
        }
    }
    None
}

/// The rows in the order the hand has seen them. A rail it has seen keeps
/// its row. A new one takes the row after the rail it follows in `knobs`.
fn steady(knobs: Vec<Knob>, order: &[KnobPlace]) -> Vec<Knob> {
    let mut slots: Vec<Option<Knob>> = order.iter().map(|_| None).collect();
    // New rails, by the slot they follow: the first list is before all.
    let mut new: Vec<Vec<Knob>> = (0..=order.len()).map(|_| Vec::new()).collect();
    let mut previous: Option<usize> = None;
    for knob in knobs {
        let seen = order
            .iter()
            .enumerate()
            .position(|(at, place)| *place == knob.place && slots[at].is_none());
        match seen {
            Some(at) => {
                slots[at] = Some(knob);
                previous = Some(at);
            }
            None => new[previous.map_or(0, |at| at + 1)].push(knob),
        }
    }
    let mut new = new.into_iter();
    let mut rows: Vec<Knob> = new.next().unwrap_or_default();
    for (slot, after) in slots.into_iter().zip(new) {
        rows.extend(slot);
        rows.extend(after);
    }
    rows
}

/// Whether the call `now` reads is the call `held` was read as: the same
/// scene and name, with its `(` where the edits since then moved it.
/// Nothing held and nothing read is the same nothing.
fn same_call(editor: Option<&Editor>, held: Option<&CallAt>, now: Option<&CallAt>) -> bool {
    match (held, now) {
        (None, None) => true,
        (Some(held), Some(now)) => {
            held.scene == now.scene
                && held.name == now.name
                && if held.revision == now.revision {
                    held.open == now.open
                } else {
                    editor
                        .and_then(|editor| {
                            editor.map_range_since(held.revision, held.open..held.open + 1)
                        })
                        .is_some_and(|open| open.start == now.open)
                }
        }
        _ => false,
    }
}

/// The bytes that take entry `at` out of an object: the entry's own code
/// and one comma beside it, so the object stays a list. Comments stay
/// where they are, and a line the entry had to itself goes with it.
/// Ranges of `source`, in order, none overlapping.
fn entry_cuts(
    source: &str,
    code: &str,
    entries: &[Range<usize>],
    at: usize,
) -> Option<Vec<Range<usize>>> {
    // The entry's code: from its first byte that is no space and no
    // comment - a quoted key is code, though the checked text blanks it -
    // to the end of its value.
    let member = entries.get(at)?.clone();
    let mut start = member.start;
    loop {
        let rest = &source[start..member.end];
        let skipped = rest.len() - rest.trim_start().len();
        start += skipped;
        let rest = &source[start..member.end];
        start += if rest.starts_with("//") {
            rest.find('\n').unwrap_or(rest.len())
        } else if rest.starts_with("/*") {
            rest.find("*/").map_or(rest.len(), |end| end + 2)
        } else if skipped == 0 {
            break;
        } else {
            0
        };
    }
    let entry = start..trimmed(code, member)?.end;
    let spaces = |text: &str| text.len() - text.trim_start_matches([' ', '\t']).len();
    let spaces_back = |text: &str| text.len() - text.trim_end_matches([' ', '\t']).len();
    let line_start = |at: usize| source[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let line_end = |at: usize| source[at..].find('\n').map_or(source.len(), |end| at + end);
    let blank = |range: Range<usize>| source[range].trim().is_empty();
    let mut cuts = Vec::new();
    if at + 1 < entries.len() {
        // The entry, its comma and the spaces after it - or its whole
        // line, when the line holds nothing else. A comment between the
        // value and its comma stays where it is.
        let comma = entries[at].end;
        let end = comma + 1 + spaces(&source[comma + 1..]);
        let start = line_start(entry.start);
        if !blank(entry.end..comma) {
            let after = entry.end + spaces(&source[entry.end..]);
            cuts.push(entry.start..after);
            cuts.push(comma..comma + 1);
        } else if blank(start..entry.start) && blank(end..line_end(end)) {
            cuts.push(start..(line_end(end) + 1).min(source.len()));
        } else {
            cuts.push(entry.start..end);
        }
    } else {
        // The last entry: the comma before it, then the entry with the
        // spaces before it - or with the line break before it, when the
        // entry had its line to itself.
        if at > 0 {
            let comma = entries[at - 1].end;
            cuts.push(comma..comma + 1);
        }
        let start = entry.start - spaces_back(&source[..entry.start]);
        let own_line = start > 0
            && source.as_bytes()[start - 1] == b'\n'
            && blank(entry.end..line_end(entry.end));
        // The line break before it, CR and all.
        let start = if own_line {
            start - 1 - usize::from(source[..start - 1].ends_with('\r'))
        } else {
            start
        };
        match cuts.last_mut() {
            Some(comma) if comma.end >= start => comma.end = entry.end,
            _ => cuts.push(start..entry.end),
        }
    }
    Some(cuts)
}

/// The call at the caret and its numeric parameters: one knob per
/// positional number and per numeric entry of the options object, with
/// the bounds the score itself declares - the fader's, or a guess.
pub(super) fn parse_call(source: &str, caret: usize) -> Option<CallParse> {
    let code = code_only(source);
    let (name_range, open) = call_at(&code, caret)?;
    let name = code[name_range].to_owned();
    let args = members(&code, open, true)?;
    let close = members(&code, open, false).and_then(|closed| closed.last().map(|last| last.end));
    let plugin = matches!(name.as_str(), "vst" | "vsti")
        .then(|| rustel_runtime::sounds::string_argument(&source[open + 1..]))
        .flatten()
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.into_owned());
    let control = plugin.is_none() && canonical_control_name(&name).is_some();
    let faders = literal_sliders(source);
    // An audio control reads an object as its value and refuses the
    // voice: `lpf({ cutoff: 800 })` is no filter. Its entries hold no
    // rails, because no move of them is heard.
    let object = (!control)
        .then(|| {
            args.iter()
                .position(|arg| code[arg.clone()].trim_start().starts_with('{'))
        })
        .flatten();
    let mut knobs = Vec::new();
    let mut entries = Vec::new();
    let mut keys = Vec::new();
    let mut preset = None;
    let mut entries_closed = close.is_some();
    for (at, arg) in args.iter().enumerate() {
        if Some(at) == object {
            let object_open = code[arg.clone()]
                .find('{')
                .expect("the member starts with a brace")
                + arg.start;
            // A still-open object: its last member runs to the end of
            // the file, so the panel takes no entry out of it until it
            // closes.
            entries_closed &= members(&code, object_open, false).is_some();
            let Some(members) = members(&code, object_open, true) else {
                continue;
            };
            entries = members.clone();
            for entry in members {
                let Some((key, value)) = entry_parts(&code, entry.clone()) else {
                    continue;
                };
                // A computed key, `[name]: 1`, names no parameter the
                // panel could write back.
                let Some(label) =
                    plain_key(&source[key.clone()]).filter(|label| !label.starts_with('['))
                else {
                    continue;
                };
                keys.push(label.clone());
                // The preset the object names, for the mark beside the
                // plugin's presets.
                if label == "preset" {
                    preset = rustel_runtime::sounds::string_argument(&source[value.clone()])
                        .filter(|name| !name.trim().is_empty())
                        .map(|name| name.into_owned());
                }
                // An entry that holds neither a number nor a whole fader -
                // a chord, an expression - is no rail, but it must not take
                // the call's other numbers with it.
                let Some(state) = entry_state(&code, &faders, value) else {
                    continue;
                };
                knobs.push(make_knob(label.clone(), KnobPlace::Entry(label), state));
            }
        } else if let Some(state) = entry_state(&code, &faders, arg.clone()) {
            // A number, or a whole fader: `lpf(slider(800, 20, 20000))`
            // is the score's own rail for the cutoff.
            knobs.push(make_knob(format!("#{at}"), KnobPlace::Argument(at), state));
        }
    }
    // A fader's bounds are its own. A bare number's come with the
    // assembly, which knows what was remembered and what the plugin says.
    for knob in &mut knobs {
        let Some(state) = knob.state.as_ref() else {
            continue;
        };
        if let Some(fader) = state.fader {
            (knob.min, knob.max, knob.step) = (fader.min, fader.max, fader.step);
        }
        knob.default = state.value;
    }
    Some(CallParse {
        name,
        open,
        close,
        plugin,
        control,
        preset,
        knobs,
        entries,
        keys,
        entries_closed,
    })
}

/// One rail before its bounds are resolved. Every entry can be off. A
/// positional argument cannot, because the call would not parse without
/// it.
fn make_knob(label: String, place: KnobPlace, state: KnobState) -> Knob {
    Knob {
        label,
        optional: matches!(place, KnobPlace::Entry(_)),
        place,
        state: Some(state),
        min: 0.0,
        max: 1.0,
        step: 0.01,
        default: 0.0,
        log: false,
    }
}

/// What a parameter's value expression holds: a number outright, or a
/// fader - one whole `slider(...)` call, the transpiler's own reading, so
/// what the panel says is a fader is what the engine says is a control.
/// Anything else holds no rail: rewriting half an expression would write
/// a score that does not mean what it did.
fn entry_state(
    code: &str,
    faders: &[rustel_runtime::ui_events::LiveSlider],
    value: Range<usize>,
) -> Option<KnobState> {
    if let Some((literal, number)) = number_literal(code, value.clone()) {
        return Some(KnobState {
            literal,
            fader: None,
            value: number,
        });
    }
    let value = trimmed(code, value)?;
    let live = faders
        .iter()
        .find(|live| (live.call.0..live.call.1) == value)?;
    let slider = &live.slider;
    Some(KnobState {
        literal: slider.from..slider.to,
        fader: Some(Fader {
            min: slider.min,
            max: slider.max,
            step: slider.step,
            frequency: live.frequency,
            call: live.call,
        }),
        value: slider.value,
    })
}

/// The preferences' name for a knob's remembered bounds: the call and
/// the parameter it stands at. A plugin call names the plugin, so two
/// plugins' same-named parameters keep their own bounds.
fn range_key(callee: &str, plugin: Option<&str>, place: &KnobPlace) -> String {
    let owner = plugin.unwrap_or(callee);
    match place {
        KnobPlace::Entry(key) => format!("{owner}:{key}"),
        KnobPlace::Argument(at) => format!("{owner}:#{at}"),
    }
}

/// The bounds the preferences remember, when they are bounds.
fn remembered_range(range: &KnobRange) -> Option<(f64, f64, f64)> {
    use std::cmp::Ordering;
    let number = |text: &str| text.trim().parse::<f64>().ok().filter(|n| n.is_finite());
    let min = number(&range.min)?;
    let max = number(&range.max)?;
    if min.partial_cmp(&max) != Some(Ordering::Less) {
        return None;
    }
    let step = number(&range.step)
        .filter(|step| *step > 0.0)
        .unwrap_or((max - min) / 1000.0);
    Some((min, max, step))
}

/// The travel and step the reference gives a rail's parameter: a call's
/// positional argument, or a control an options object names, as in
/// `set({ cutoff: 800 })`. `seg(16)` travels whole segments this way, and
/// `lpf(800)` the ear's 20 to 20000 Hz. A plugin's parameters are its own.
fn documented_number(
    reference: &crate::reference::Reference,
    call: &CallParse,
    place: &KnobPlace,
) -> Option<(f64, f64, f64)> {
    if call.plugin.is_some() {
        return None;
    }
    let (name, at) = match place {
        KnobPlace::Argument(at) => (call.name.as_str(), *at),
        KnobPlace::Entry(key) if canonical_control_name(key).is_some() => (key.as_str(), 0),
        KnobPlace::Entry(_) => return None,
    };
    let entry = reference
        .lookup_in(name, crate::reference::ReferenceScope::Pattern)
        .and_then(|index| reference.entry(index))?;
    let parameter = &entry.params.get(at)?.name;
    let number = rustel_core::reference::reference_number(&entry.name, parameter);
    #[cfg(feature = "extensions")]
    let number = number.or_else(|| rustel_ext::reference_number(&entry.name, parameter));
    number.map(|number| (number.min, number.max, number.step))
}

/// A documented travel that takes the score's own value: a number past an
/// end moves that end out to it. `seg(128)` is right, though the travel
/// stops at 64. A travel above zero stays above zero: `fast(0.125)` floors
/// at its own value, on a step fine enough to reach it that still stops on
/// whole numbers, so no move writes the `fast(0)` that silences a pattern.
fn widened((min, max, step): (f64, f64, f64), value: f64) -> (f64, f64, f64) {
    let (min, step) = if value >= min {
        (min, step)
    } else if min > 0.0 && value > 0.0 {
        let whole = |number: f64| (number - number.round()).abs() < 1e-9;
        let fine = (1..=1000)
            .map(f64::from)
            .find(|parts| whole(value * parts))
            .map_or(step, |parts| step.min(1.0 / parts));
        (value, fine)
    } else {
        ((value / step).floor() * step, step)
    };
    let max = if value > max {
        guess_range(value).1.max((value / step).ceil() * step)
    } else {
        max
    };
    (min, max, step)
}

/// One key of an options object the reference documents, such as
/// `_pianoroll`'s `cycles` or `lfo`'s `rate`: the short names the engine
/// also reads, the bounds the documentation gives, and the value the call
/// takes without the key.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DocOption {
    key: String,
    aliases: Vec<String>,
    range: (f64, f64, f64),
    default: Option<f64>,
}

impl DocOption {
    /// Whether the score's key is this option, by its name or an alias:
    /// `env({ a: 0.1 })` sets the attack. The engine and the painters read
    /// a key in any case, so `fliptime` is `flipTime`.
    fn names(&self, key: &str) -> bool {
        self.key.eq_ignore_ascii_case(key)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(key))
    }

    /// The value a rail starts at when it comes on: the default, which
    /// changes nothing the call does, or, for a key with no default, the
    /// middle of its travel. The low end of a rate or a depth can be the
    /// value that stops the call.
    fn start(&self) -> f64 {
        let (min, max, step) = self.range;
        let middle = min + ((max - min) / 2.0 / step).round() * step;
        self.default.unwrap_or(middle).clamp(min, max)
    }
}

/// The numeric keys of the options object a call's reference documents,
/// in the reference's order. The object is the `options` or `config`
/// parameter. Its keys are the parameters after it, or, when the
/// reference names them `config.rate`, the ones that carry its name. A
/// key that takes text, such as a colour, is no rail, and neither is a key
/// the terminal ignores: a move would change nothing on the screen.
fn documented_options(entry: &crate::reference::Entry) -> Vec<DocOption> {
    let Some(object) = entry.params.iter().position(|param| {
        param.r#type.eq_ignore_ascii_case("object")
            && matches!(param.name.as_str(), "options" | "config")
    }) else {
        return Vec::new();
    };
    let prefix = format!("{}.", entry.params[object].name);
    let keys = &entry.params[object + 1..];
    let prefixed = keys.iter().any(|param| param.name.starts_with(&prefix));
    keys.iter()
        .filter_map(|param| {
            let key = if prefixed {
                param.name.strip_prefix(&prefix)?
            } else {
                &param.name
            };
            if crate::visuals::painter_terminal_option(&entry.name, key).starts_with("ignored") {
                return None;
            }
            let terminal = crate::visuals::painter_terminal_number(&entry.name, key)
                .map(|number| (number.default, number.min, number.max));
            documented_option(key, &param.r#type, &param.description, terminal)
        })
        .collect()
}

/// One documented key as a rail, when its type is a number. Its default
/// and bounds are the terminal's, when the terminal reads the key in its
/// own way. Otherwise they are what the text says - `0 to 1`, `-1 =
/// relaxed, 1 = snappy`, or a list of numbered choices - then what its
/// type and default say.
fn documented_option(
    key: &str,
    kind: &str,
    description: &str,
    terminal: Option<(f64, f64, f64)>,
) -> Option<DocOption> {
    use std::sync::LazyLock;

    use regex::Regex;
    static DEFAULT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)(?:defaults? to |\(default )(-?\d+(?:\.\d+)?)|(-?\d+(?:\.\d+)?) by default",
        )
        .expect("the default pattern is valid")
    });
    static SPAN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[^\w.-])(-?\d+(?:\.\d+)?) to (-?\d+(?:\.\d+)?)")
            .expect("the span pattern is valid")
    });
    static ENDS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(-?\d+(?:\.\d+)?) = [^,()]+, (-?\d+(?:\.\d+)?) = ")
            .expect("the ends pattern is valid")
    });
    static CHOICE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\((\d+)\)").expect("the choice pattern is valid"));
    static ALIASES: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"Alias(?:es)?: ([\w, ]+)").expect("the alias pattern is valid")
    });
    let kinds: Vec<String> = kind
        .split('|')
        .map(|kind| kind.trim().to_ascii_lowercase())
        .collect();
    let has = |name: &str| kinds.iter().any(|kind| kind == name);
    let (boolean, integer) = (has("boolean"), has("integer"));
    // A colour takes text. An FX stage index, `fxi`, routes a modulator
    // to a stage rather than moving it: a move would send it to another
    // stage, or to none.
    if !(boolean || integer || has("number"))
        || key.to_ascii_lowercase().ends_with("color")
        || key.eq_ignore_ascii_case("fxi")
    {
        return None;
    }
    let number = |text: &str| text.parse::<f64>().ok().filter(|n| n.is_finite());
    let default = terminal.map(|(default, _, _)| default).or_else(|| {
        DEFAULT
            .captures(description)
            .and_then(|found| number(found.get(1).or_else(|| found.get(2))?.as_str()))
    });
    let pair = |found: regex::Captures| Some((number(&found[1])?, number(&found[2])?));
    let choices: Vec<f64> = CHOICE
        .captures_iter(description)
        .filter_map(|found| number(&found[1]))
        .collect();
    let span = terminal
        .map(|(_, min, max)| (min, max))
        .or_else(|| SPAN.captures(description).and_then(pair))
        .or_else(|| ENDS.captures(description).and_then(pair))
        .map(|(low, high)| (low.min(high), low.max(high)))
        .or_else(|| {
            let low = choices.iter().copied().reduce(f64::min)?;
            let high = choices.iter().copied().reduce(f64::max)?;
            (choices.len() > 1).then_some((low, high))
        })
        .filter(|(low, high)| low < high);
    let whole = boolean || integer || (span.is_some() && choices.len() > 1);
    let range = if boolean {
        (0.0, 1.0, 1.0)
    } else if let Some((min, max)) = span {
        let step = if whole || max - min > 100.0 {
            1.0
        } else if max - min > 10.0 {
            0.1
        } else {
            0.01
        };
        (min, max, step)
    } else if key.to_ascii_lowercase().contains("midi") {
        // A MIDI note.
        (0.0, 127.0, 1.0)
    } else {
        let value = default.unwrap_or(0.0);
        let (min, max, step) = guess_range(value);
        // A default at the top of its guess gets room above it: an
        // LFO's rate of 1 travels to 5, not to 1.
        let (min, max, step) = if value > 0.0 && value >= max {
            guess_range(value * 2.0)
        } else {
            (min, max, step)
        };
        if !integer {
            (min, max, step)
        } else if value >= 1.0 {
            (1.0, max, 1.0)
        } else if value == 0.0 {
            (0.0, 10.0, 1.0)
        } else {
            (min, max, 1.0)
        }
    };
    let aliases = ALIASES
        .captures(description)
        .map(|found| {
            found[1]
                .split(',')
                .map(str::trim)
                .filter(|alias| !alias.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(DocOption {
        key: key.to_owned(),
        aliases,
        range,
        default,
    })
}

/// The bounds of a number nothing declares, as best the reference says
/// them. A plugin's parameters travel 0 to 1, the numbers its scores
/// write. A key of a documented options object travels its documented
/// bounds while the score's value is inside them. A parameter the
/// reference gives numbers travels them, widened to take the score's
/// value. Anything else gets the round guess, in whole steps for a whole
/// number of 2 or more: a count nothing documents stays a count.
fn deduced_range(
    call: &CallParse,
    value: f64,
    option: Option<&DocOption>,
    number: Option<(f64, f64, f64)>,
) -> (f64, f64, f64) {
    if call.plugin.is_some() {
        return (0.0, 1.0, 0.001);
    }
    if let Some(option) = option {
        let (min, max, _) = option.range;
        if (min..=max).contains(&value) {
            return option.range;
        }
    }
    if let Some(number) = number {
        return widened(number, value);
    }
    let (min, max, step) = guess_range(value);
    // A value at the top of its guess gets room above it: `cps(1)` can
    // still go up.
    let (min, max, step) = if value > 0.0 && value >= max {
        guess_range(value * 2.0)
    } else {
        (min, max, step)
    };
    let step = if value.fract() == 0.0 && value.abs() >= 2.0 {
        1.0
    } else {
        step
    };
    // A guess past the largest float is no bound: the value is.
    let bound = |end: f64| if end.is_finite() { end } else { value };
    (bound(min), bound(max), step)
}

/// The bounds a rail travels, in their order of authority: the score's
/// own fader first - nothing outranks the declaration the score stands
/// behind - then what the form remembered, then what the plugin says,
/// and the deduction last. The default is what the rail comes back to
/// when it leaves the off stop: the plugin's own start value for a plugin
/// parameter, the value the score holds for everything else.
fn resolve_bounds(
    knob: &mut Knob,
    remembered: Option<&KnobRange>,
    param: Option<ParamDefaults>,
    deduced: (f64, f64, f64),
    frequency: bool,
    log_sliders: bool,
) {
    let Some(state) = knob.state.as_ref() else {
        return;
    };
    if let Some(fader) = state.fader {
        // A fader's bounds are its own: the panel never second-guesses
        // the declaration the score stands behind. Its travel follows
        // the transpiler's marking, the way the pill over the call does.
        knob.min = fader.min;
        knob.max = fader.max;
        knob.step = fader.step;
        knob.log = Travel::frequency(fader.frequency && log_sliders, fader.min, fader.max)
            == Travel::Logarithmic;
        return;
    }
    let (min, max, step) = if let Some(range) = remembered.and_then(remembered_range) {
        range
    } else if let Some(param) = param {
        knob.default = param.default;
        (0.0, 1.0, param.step)
    } else {
        deduced
    };
    knob.min = min;
    knob.max = max;
    knob.step = step;
    knob.log = Travel::frequency(frequency && log_sliders, min, max) == Travel::Logarithmic;
}

/// What a plugin parameter tells its rail: the value the parameter
/// starts at, and the step of its normalised 0-to-1 travel. The vst
/// code fills one from the host's parameter. A studio built without
/// the vst feature has no plugin to fill one.
#[derive(Clone, Copy)]
struct ParamDefaults {
    default: f64,
    step: f64,
}

#[cfg(feature = "vst")]
impl ParamDefaults {
    fn of(param: &ParamInfo) -> Self {
        // A step count of 0 is a continuous parameter. One step is a
        // switch, off at 0 and on at 1: its rail has two stops.
        Self {
            default: param.default,
            step: if param.steps > 0 {
                1.0 / param.steps as f64
            } else {
                0.001
            },
        }
    }
}

/// The parameter a score key names, as the plugin spells it.
#[cfg(feature = "vst")]
fn param_for<'a>(params: &'a [ParamInfo], key: &str) -> Option<&'a ParamInfo> {
    params
        .iter()
        .find(|param| param.key == key || score_key(&param.key) == key)
}

/// The plugin's parameters as the host has them now. A plugin still
/// loading has none yet, and the panel says what it has: the next change
/// of the score or the caret asks again.
#[cfg(feature = "vst")]
fn plugin_params(name: &str) -> Option<Vec<ParamInfo>> {
    match vst::host().resolve(name, false) {
        vst::Resolved::Ready(_, plugin) => Some(plugin.params().to_vec()),
        _ => None,
    }
}

/// A rail the score does not hold, beside the rails it does: a
/// remembered plugin parameter lands beside its own knob only.
#[cfg(feature = "vst")]
fn push_off(knob: Knob, knobs: &mut Vec<Knob>) {
    if !knobs.iter().any(|shown| shown.place == knob.place) {
        knobs.push(knob);
    }
}

/// How many rails a plugin call shows before the hand has chosen its own,
/// and how many documented keys a built-in call shows.
const SUGGESTED_KNOBS: usize = 10;

/// The parameters a plugin call shows before the hand has chosen any, as a
/// host can tell them without the plugin's window: the dry/wet mix first
/// when the plugin names one, then the plugin's sound - a continuous
/// parameter whose start value reads as a number, `0.0 dB` or `100 %` -
/// then its switches, lists and modes, whose start values read as words:
/// `OFF`, `Bell`, `--`. A bypass, a program change, and the stand-ins some
/// plugins give a host for MIDI controllers are no knobs.
#[cfg(feature = "vst")]
fn suggested_params(params: &[ParamInfo]) -> Vec<&ParamInfo> {
    const STAND_INS: [&str; 5] = [
        "MIDI CC",
        "Pitch Bend",
        "Channel Pressure",
        "Aftertouch",
        "Internal",
    ];
    let mut suggested: Vec<&ParamInfo> = params
        .iter()
        .filter(|param| {
            !matches!(param.role, ParamRole::Bypass | ParamRole::Program)
                && !param.name.to_lowercase().contains("bypass")
                && !STAND_INS.contains(&param.name.trim())
        })
        .collect();
    let sound = |param: &ParamInfo| {
        let text = param.default_text.trim();
        param.steps == 0 && (text.is_empty() || text.chars().any(|c| c.is_ascii_digit()))
    };
    // A stable sort: the plugin's own order stays within each kind.
    suggested.sort_by_key(|param| (param.role != ParamRole::Mix, !sound(param)));
    suggested.truncate(SUGGESTED_KNOBS);
    suggested
}

impl App {
    /// Rebuild the panel's rows for this frame. Cheap when nothing
    /// moved: the parse of the score is kept until the scene, the text
    /// or the caret changes, and only the assembly - the bounds, the
    /// remembered parameters, the held off-entries - is remade.
    pub(super) fn refresh_knobs(&mut self, area: Rect) {
        if self.knobs_panel.is_none() {
            self.knobs_rows = None;
            return;
        }
        let scene = self.scenes.current().id;
        let caret = self.editor().primary_selection().head.0;
        let revision = self.editor().revision();
        if self.knobs_parse.as_ref().is_none_or(|parse| {
            parse.scene != scene || parse.revision != revision || parse.caret != caret
        }) {
            let call = parse_call(&self.editor().source(), caret);
            let options = call
                .as_ref()
                .filter(|call| call.plugin.is_none() && !call.control)
                .and_then(|call| {
                    self.reference
                        .lookup_in(&call.name, crate::reference::ReferenceScope::Pattern)
                })
                .and_then(|index| self.reference.entry(index))
                .map(documented_options)
                .unwrap_or_default();
            self.knobs_parse = Some(KnobsParse {
                scene,
                revision,
                caret,
                call,
                options,
                #[cfg(feature = "vst")]
                params: None,
            });
        }
        // The plugin the call names: its parameters arrive once it has
        // finished loading, and until then each frame asks again.
        #[cfg(feature = "vst")]
        {
            let loading = self
                .knobs_parse
                .as_ref()
                .filter(|parse| parse.params.is_none())
                .and_then(|parse| parse.call.as_ref())
                .and_then(|call| call.plugin.clone());
            if let Some(plugin) = loading
                && let Some(params) = plugin_params(&plugin)
                && let Some(parse) = self.knobs_parse.as_mut()
            {
                parse.params = Some(params);
            }
        }
        self.assemble_knobs();
        // The presets of the plugin call, read for the section under the
        // rails: a new call asks for its own names, and a new read of the
        // preset folders brings them again. The names stay empty for a
        // plugin the host is still finding, so the ask goes on while its
        // parameters have not arrived.
        #[cfg(feature = "vst")]
        {
            let plugin = self
                .knobs_parse
                .as_ref()
                .and_then(|parse| parse.call.as_ref())
                .and_then(|call| call.plugin.clone());
            let loading = self
                .knobs_parse
                .as_ref()
                .is_some_and(|parse| parse.params.is_none());
            if let Some(plugin) = plugin {
                let version = self.plugin_presets_version;
                let stale = self
                    .knobs_panel
                    .as_ref()
                    .and_then(|panel| panel.presets.as_ref())
                    .is_none_or(|presets| {
                        presets.version != version || (loading && presets.names.is_empty())
                    });
                if stale
                    && let Some(names) = vst::host().presets(&plugin)
                    && let Some(panel) = self.knobs_panel.as_mut()
                {
                    let open = panel.presets.as_ref().is_some_and(|presets| presets.open);
                    panel.presets = Some(KnobPresets {
                        names,
                        version,
                        open,
                    });
                }
            }
        }
        let count = self.knobs_row_count();
        let Some(panel) = self.knobs_panel.as_mut() else {
            return;
        };
        panel.clamp(count);
        if let Some(parts) = parts(area) {
            panel.follow(count, shown(parts.body));
        }
    }

    /// The rows the column holds: the rails, and the presets section under
    /// them, heading and names, while the call is one of a plugin.
    fn knobs_row_count(&self) -> usize {
        let knobs = self.knobs_rows.as_ref().map_or(0, |rows| rows.knobs.len());
        let presets = self
            .knobs_panel
            .as_ref()
            .and_then(|panel| panel.presets.as_ref())
            .filter(|presets| !presets.names.is_empty())
            .map_or(0, presets_rows);
        knobs + presets
    }

    /// Put the rows together: the score's knobs with their bounds
    /// resolved, then the plugin parameters chosen before that the score
    /// does not hold, then the entries the hand took out of this call.
    fn assemble_knobs(&mut self) {
        let cache = self.knobs_parse.as_ref();
        let parse = cache.and_then(|cache| cache.call.as_ref());
        let callee = parse.map(|call| call.name.clone());
        let mut knobs = parse.map_or_else(Vec::new, |call| call.knobs.clone());
        let log_sliders = self.ui_settings.frequency_slider_log;
        let plugin = parse.and_then(|call| call.plugin.as_deref());
        let options = cache.map_or(&[][..], |cache| cache.options.as_slice());
        #[cfg(feature = "vst")]
        let params = cache.and_then(|cache| cache.params.as_deref());
        // What the hand holds - entries taken off, bounds deduced -
        // belongs to the one call it worked: another call, even of the
        // same name, or another document empties the hand. An edit above
        // the call moves the call, not the hand.
        let call_at = cache.zip(parse).map(|(cache, call)| CallAt {
            scene: cache.scene,
            open: call.open,
            revision: cache.revision,
            name: call.name.clone(),
        });
        let editor = cache
            .and_then(|cache| self.scenes.get(cache.scene))
            .map(|scene| &scene.editor);
        let Some(panel) = self.knobs_panel.as_mut() else {
            self.knobs_rows = None;
            return;
        };
        if !same_call(editor, panel.off_call.as_ref(), call_at.as_ref()) {
            panel.off.clear();
            panel.ranges.clear();
            panel.order.clear();
            panel.order_by_plugin = false;
            panel.presets = None;
        }
        panel.off_call = call_at;
        for knob in &mut knobs {
            let (Some(call), Some(state)) = (parse, knob.state.as_ref()) else {
                continue;
            };
            // A positional number goes by the name the reference gives the
            // argument: `lpf`'s first is its frequency.
            if let KnobPlace::Argument(at) = knob.place
                && let Some(name) = self
                    .reference
                    .lookup_in(&call.name, crate::reference::ReferenceScope::Pattern)
                    .and_then(|index| self.reference.entry(index))
                    .and_then(|entry| entry.params.get(at))
                    .map(|param| param.name.trim())
                    .filter(|name| !name.is_empty())
            {
                knob.label = name.to_owned();
            }
            // A documented key goes by the reference's name for it, so
            // `env({ a: 0.1 })` reads `attack`.
            let option = match &knob.place {
                KnobPlace::Entry(key) => options.iter().find(|option| option.names(key)),
                KnobPlace::Argument(_) => None,
            };
            if let Some(option) = option {
                knob.label = option.key.clone();
            }
            let remembered = self
                .prefs
                .knob_ranges
                .as_ref()
                .and_then(|ranges| ranges.get(&range_key(&call.name, plugin, &knob.place)));
            #[cfg(feature = "vst")]
            let param = params.and_then(|params| match &knob.place {
                KnobPlace::Entry(key) => param_for(params, key).map(ParamDefaults::of),
                KnobPlace::Argument(_) => None,
            });
            #[cfg(not(feature = "vst"))]
            let param = None;
            // Deduced once, then held while the value stays inside: a
            // number moved to the top of its rail does not widen it.
            let held = panel
                .ranges
                .iter()
                .position(|(place, _)| *place == knob.place);
            let deduced = match held.map(|at| panel.ranges[at].1) {
                Some(range) if (range.0..=range.1).contains(&state.value) => range,
                _ => {
                    let number = documented_number(&self.reference, call, &knob.place);
                    let range = deduced_range(call, state.value, option, number);
                    match held {
                        Some(at) => panel.ranges[at].1 = range,
                        None => panel.ranges.push((knob.place.clone(), range)),
                    }
                    range
                }
            };
            let frequency = option.is_none()
                && call
                    .control_name(&knob.place)
                    .is_some_and(is_frequency_control);
            resolve_bounds(knob, remembered, param, deduced, frequency, log_sliders);
        }
        // A remembered parameter or a documented key the hand is holding
        // stays in the hand: the rail the hand holds carries the value it
        // left with, where the other would carry its default.
        let held = self.knobs_panel.as_ref().map_or_else(Vec::new, |panel| {
            panel
                .off
                .iter()
                .map(|off| off.knob.place.clone())
                .collect::<Vec<_>>()
        });
        // The plugin parameters chosen before, in the order they were
        // chosen, as rails at their off stop until the score holds them.
        #[cfg(feature = "vst")]
        if let (Some(plugin), Some(params)) = (plugin, params) {
            // The parameters the hand chose for this plugin, or, before it
            // chose any, the suggestions, up to a full column beside the
            // entries the score holds.
            let chosen = self
                .prefs
                .vst_knobs
                .as_ref()
                .and_then(|knobs| knobs.get(plugin));
            let shown: Vec<&ParamInfo> = match chosen {
                Some(keys) => keys
                    .iter()
                    .filter_map(|key| param_for(params, key))
                    .collect(),
                None => suggested_params(params),
            };
            for param in shown {
                let defaults = ParamDefaults::of(param);
                let place = KnobPlace::Entry(param.key.clone());
                if held.contains(&place) {
                    continue;
                }
                push_off(
                    Knob {
                        label: score_key(&param.key),
                        place,
                        state: None,
                        min: 0.0,
                        max: 1.0,
                        step: defaults.step,
                        default: defaults.default,
                        optional: true,
                        log: false,
                    },
                    &mut knobs,
                );
            }
            // A plugin's rails stand in the plugin's own order, as its
            // window lists them, whether the score holds them or not. A
            // key the plugin does not know keeps its place after them.
            let order = |knob: &Knob| match &knob.place {
                KnobPlace::Entry(key) => params
                    .iter()
                    .position(|param| param.key == *key || score_key(&param.key) == *key)
                    .unwrap_or(usize::MAX),
                KnobPlace::Argument(_) => usize::MAX,
            };
            knobs.sort_by_key(order);
            // The rows taken before the plugin's list arrived were the
            // score's order: they give way to the plugin's, once. A rail the
            // hand holds off takes its row in the plugin's order too.
            if let Some(panel) = self.knobs_panel.as_mut()
                && !panel.order_by_plugin
            {
                panel.order.clear();
                panel.order_by_plugin = true;
                let held: Vec<usize> = panel.off.iter().map(|off| order(&off.knob)).collect();
                for (off, place) in panel.off.iter_mut().zip(&held) {
                    let before = knobs.iter().filter(|knob| order(knob) < *place).count();
                    off.at = before + held.iter().filter(|other| *other < place).count();
                }
            }
        }
        // The keys the reference documents for the call's object, up to a
        // column of them, as rails at their off stop until the score holds
        // them. A key with a default comes on at the value the call already
        // takes, so a key with none - `sync` replaces the rate - waits in
        // the add list. Every rail stands in the reference's order, and a
        // key the reference does not list keeps its place after them.
        if !options.is_empty() {
            let named = |place: &KnobPlace, option: &DocOption| matches!(place, KnobPlace::Entry(key) if option.names(key));
            let written = parse.map_or(&[][..], |call| call.keys.as_slice());
            let suggested = options.iter().filter(|option| option.default.is_some());
            for option in suggested.take(SUGGESTED_KNOBS) {
                if knobs.iter().any(|knob| named(&knob.place, option))
                    || held.iter().any(|place| named(place, option))
                    || written.iter().any(|key| option.names(key))
                {
                    continue;
                }
                let place = KnobPlace::Entry(option.key.clone());
                let (min, max, step) = callee
                    .as_deref()
                    .and_then(|callee| {
                        self.prefs
                            .knob_ranges
                            .as_ref()?
                            .get(&range_key(callee, None, &place))
                    })
                    .and_then(remembered_range)
                    .unwrap_or(option.range);
                knobs.push(Knob {
                    label: option.key.clone(),
                    place,
                    state: None,
                    min,
                    max,
                    step,
                    default: option.start().clamp(min, max),
                    optional: true,
                    log: false,
                });
            }
            knobs.sort_by_key(|knob| {
                options
                    .iter()
                    .position(|option| named(&knob.place, option))
                    .unwrap_or(usize::MAX)
            });
        }
        if let Some(panel) = self.knobs_panel.as_mut() {
            // The entries the hand took out go back at the rows they sat
            // at, each where it was taken from, so a rail never slides
            // away under the hand that is holding it.
            let mut held = panel.off.clone();
            held.sort_by_key(|held| held.at);
            for off in held {
                if knobs.iter().any(|shown| shown.place == off.knob.place) {
                    continue;
                }
                let at = off.at.min(knobs.len());
                knobs.insert(at, off.knob);
            }
            // And every rail keeps the row it had: an entry that comes
            // back at the front of the object, or a suggestion the score
            // takes in, does not push the rails around it.
            knobs = steady(knobs, &panel.order);
            panel.order = knobs.iter().map(|knob| knob.place.clone()).collect();
        }
        self.knobs_rows = Some(KnobsRows {
            name: callee,
            listed: plugin.is_some() || !options.is_empty(),
            preset: parse.and_then(|call| call.preset.clone()),
            knobs,
        });
    }

    /// Show the column at the score's right edge. A fixture like the
    /// mixer: Esc hands the keyboard back and leaves it standing, and it
    /// comes back the next launch the way it was left - which the
    /// keyless restore is, so it must not take the keyboard either.
    pub(super) fn open_knobs_panel(&mut self, focus: bool) {
        if self.ui_settings.zen {
            self.status = "knobs: zen has no docked furniture - leave zen for the column".into();
            self.dirty_frame = true;
            return;
        }
        self.knobs_panel = Some(KnobsPanel::default());
        self.knobs_unlaid = true;
        self.prefs.knobs_panel = Some(true);
        self.save_prefs_soon();
        self.invalidate_maps();
        if focus {
            self.focus_panel(PanelKind::Knobs);
            // The column ships with no key: View ▸ Knobs is its door
            // until one is learnt.
            let hint = self.keybinds.hint(BindAction::Knobs);
            let hides = if hint.is_empty() {
                "View ▸ Knobs".to_owned()
            } else {
                hint
            };
            self.status = format!(
                "knobs: ↑/↓ rail · ←/→ move · ⌫ off · Enter edit · a add · r reset list · Esc back to the score · {hides} hides"
            );
        }
        self.dirty_frame = true;
    }

    /// Hide the column, whether or not it has the keyboard.
    pub(super) fn close_knobs_panel(&mut self) {
        self.knobs_panel = None;
        self.knobs_parse = None;
        self.knobs_rows = None;
        self.knobs_unlaid = false;
        self.prefs.knobs_panel = Some(false);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    pub(super) fn toggle_knobs_panel(&mut self) {
        if self.knobs_panel.is_some() {
            // The panel close every panel takes: the status says so, and
            // the keyboard goes back to the score with it.
            self.close_panel(PanelKind::Knobs);
        } else {
            self.open_knobs_panel(true);
        }
    }

    /// The column's keys. Returns true when the panel took the key, the
    /// way every focused panel answers.
    pub(super) fn handle_knobs_key(
        &mut self,
        code: KeyCode,
        primary: bool,
        shift: bool,
        alt: bool,
    ) -> bool {
        if self.knobs_panel.is_none() {
            return false;
        }
        // A key can arrive before the first paint after opening, and every
        // key reads the rows - so the rows are made here, not only in the
        // layout. The parse is kept until the score or the caret changes,
        // so this is the assembly alone for every key but the first.
        self.refresh_knobs(Rect::default());
        // A sheet that is open owns the keyboard first.
        if let Some(mut adding) = self
            .knobs_panel
            .as_mut()
            .and_then(|panel| panel.adding.take())
        {
            let open = self.knobs_add_key(&mut adding, code, primary, alt);
            if let Some(panel) = self.knobs_panel.as_mut() {
                panel.adding = open.then_some(adding);
            }
            self.dirty_frame = true;
            return true;
        }
        if let Some(mut editing) = self
            .knobs_panel
            .as_mut()
            .and_then(|panel| panel.editing.take())
        {
            let open = self.knobs_edit_key(&mut editing, code, primary, shift, alt);
            if let Some(panel) = self.knobs_panel.as_mut() {
                panel.editing = open.then_some(editing);
            }
            self.dirty_frame = true;
            return true;
        }
        let count = self.knobs_row_count();
        let selected = self.knobs_panel.as_ref().map_or(0, |panel| panel.selected);
        let takes = |code: KeyCode| {
            matches!(
                code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Esc
                    | KeyCode::Enter
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Delete
                    | KeyCode::Backspace
                    | KeyCode::Char('e' | 'E' | 'a' | 'A' | 'r' | 'R')
            )
        };
        // Any key but a second `r` keeps the rails a first `r` offered to
        // reset.
        if !matches!(code, KeyCode::Char('r' | 'R'))
            && let Some(panel) = self.knobs_panel.as_mut()
        {
            panel.reset_armed = false;
        }
        if !primary && !alt && takes(code) {
            if let Some(panel) = self.knobs_panel.as_mut() {
                match code {
                    KeyCode::Up => panel.move_by(-1, count),
                    KeyCode::Down => panel.move_by(1, count),
                    KeyCode::PageUp => panel.selected = page_selection(selected, count, 8, false),
                    KeyCode::PageDown => panel.selected = page_selection(selected, count, 8, true),
                    _ => {}
                }
            }
            // The presets section under the rails: → and ← are its chords,
            // and Enter takes the preset under the hand into the call.
            if self.knobs_presets_key(code) {
                self.dirty_frame = true;
                return true;
            }
            match code {
                KeyCode::Esc => {
                    self.focus = Focus::Editor;
                    self.status = "knobs: back to the score - the column stays".into();
                }
                KeyCode::Char('e' | 'E') | KeyCode::Enter => self.open_knobs_form(),
                KeyCode::Char('a' | 'A') => self.open_knobs_add(),
                KeyCode::Char('r' | 'R') => self.knobs_reset_key(),
                KeyCode::Delete | KeyCode::Backspace => self.knobs_delete(),
                KeyCode::Left => self.nudge_knob(-1, shift),
                KeyCode::Right => self.nudge_knob(1, shift),
                KeyCode::Home => self.end_knob(false),
                KeyCode::End => self.end_knob(true),
                _ => {}
            }
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// The keys of the presets section, when the hand is on one of its
    /// rows: → and Enter open the heading, ← closes it wherever the hand
    /// is in the section, and Enter on a name takes the preset into the
    /// call. False leaves the key to the rails.
    fn knobs_presets_key(&mut self, code: KeyCode) -> bool {
        let knobs = self.knobs_rows.as_ref().map_or(0, |rows| rows.knobs.len());
        let Some(panel) = self.knobs_panel.as_mut() else {
            return false;
        };
        if panel
            .presets
            .as_ref()
            .is_none_or(|presets| presets.names.is_empty())
        {
            return false;
        }
        let Some(offset) = panel.selected.checked_sub(knobs) else {
            return false;
        };
        let name = match code {
            KeyCode::Right | KeyCode::Enter if offset == 0 => {
                panel.presets.as_mut().expect("the section has names").open = true;
                return true;
            }
            KeyCode::Left => {
                panel.presets.as_mut().expect("the section has names").open = false;
                panel.selected = knobs;
                return true;
            }
            // Enter on a name: the preset goes into the call, and the
            // evaluation plays it.
            KeyCode::Enter => panel
                .presets
                .as_ref()
                .and_then(|presets| presets.names.get(offset - 1))
                .cloned(),
            _ => return false,
        };
        let Some(name) = name else {
            return false;
        };
        self.knob_preset_write(&name);
        true
    }

    /// Write `preset: name` into the call the column reads. A preset the
    /// call holds already gives way to the chosen one, and the score
    /// changes, so the evaluation plays the chosen preset.
    fn knob_preset_write(&mut self, name: &str) {
        let Some(open) = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
            .map(|call| call.open)
        else {
            return;
        };
        let text = format!("preset: {name:?}");
        let source = self.editor().source();
        let Some((range, kept)) = parameter_edit(&source, open, &text, None) else {
            self.status = "knobs: the call needs a closed object for a preset".into();
            self.dirty_frame = true;
            return;
        };
        // The call holds a preset already: the chosen one takes its place.
        let replacement = if kept == source[range.clone()] {
            text
        } else {
            kept
        };
        if self.knobs_write(range, &replacement) {
            self.status = format!("knobs: preset {name}");
        }
    }

    /// The add list's keys: letters search, Enter adds and remembers,
    /// Delete forgets, Esc closes.
    fn knobs_add_key(
        &mut self,
        adding: &mut AddList,
        code: KeyCode,
        primary: bool,
        alt: bool,
    ) -> bool {
        let rows = adding.rows();
        match code {
            KeyCode::Esc => {
                self.status = "add cancelled".into();
                false
            }
            KeyCode::Up if !primary => {
                if !rows.is_empty() {
                    adding.selected = adding.selected.saturating_sub(1);
                    adding.follow(rows.len(), self.add_list_shown(rows.len()));
                }
                true
            }
            KeyCode::Down if !primary => {
                if !rows.is_empty() {
                    adding.selected = (adding.selected + 1).min(rows.len() - 1);
                    adding.follow(rows.len(), self.add_list_shown(rows.len()));
                }
                true
            }
            KeyCode::Backspace if !primary => {
                adding.query.pop();
                adding.rank();
                true
            }
            KeyCode::Enter if !primary => {
                self.knobs_add_chosen(adding);
                false
            }
            KeyCode::Delete if !primary => {
                self.knobs_forget_chosen(adding);
                false
            }
            KeyCode::Char(character) if !primary && !alt => {
                adding.query.push(character);
                adding.rank();
                true
            }
            _ => true,
        }
    }

    /// `a`: the call's list to search, the way the visual fx panel adds
    /// its widgets - a plugin's parameters, or the keys the reference
    /// documents for a built-in call's options. Any other call has no
    /// list: a guess here would write a wrong number.
    fn open_knobs_add(&mut self) {
        let Some(parse) = self.knobs_parse.as_ref() else {
            return;
        };
        let Some(call) = parse.call.as_ref() else {
            self.status = "knobs: put the caret in a call first".into();
            self.dirty_frame = true;
            return;
        };
        let Some(plugin) = call.plugin.clone() else {
            if parse.options.is_empty() {
                self.status = format!(
                    "knobs: {} has no list - add works on a vst call or documented options",
                    call.name
                );
                self.dirty_frame = true;
                return;
            }
            let (names, details) = parse
                .options
                .iter()
                .map(|option| {
                    let (min, max, _) = option.range;
                    let span = format!("{} to {}", short_number(min), short_number(max));
                    let detail = match option.default {
                        Some(default) => format!("{} · {span}", short_number(default)),
                        None => span,
                    };
                    (option.key.clone(), detail)
                })
                .unzip();
            // The reference's own search, for the reference's own names.
            self.show_add_list(names, details, |query, names| {
                crate::fuzzy::rank(query, names.iter().map(String::as_str))
            });
            return;
        };
        #[cfg(feature = "vst")]
        {
            // The list the column reads from, or the host's when the
            // column has none yet.
            let params = parse.params.clone().or_else(|| plugin_params(&plugin));
            let Some(params) = params else {
                self.status = format!("knobs: {plugin} is still loading - try again in a moment");
                self.dirty_frame = true;
                return;
            };
            let (names, details) = params
                .iter()
                .map(|param| (score_key(&param.key), param.default_shown()))
                .unzip();
            self.show_add_list(names, details, crate::reference::rank_names);
        }
        #[cfg(not(feature = "vst"))]
        {
            // This studio was built with no plugin host: the call names
            // a plugin, but no host exists to ask for its parameters.
            self.status =
                format!("knobs: this studio was built without plugins - {plugin} has no list");
            self.dirty_frame = true;
        }
    }

    /// The add list over the column, with its names to search, the detail
    /// beside each, and the search that ranks them.
    fn show_add_list(
        &mut self,
        names: Vec<String>,
        details: Vec<String>,
        ranker: fn(&str, &[String]) -> Vec<usize>,
    ) {
        self.status = "knobs: type to search · Enter adds the rail · Esc back".into();
        if let Some(panel) = self.knobs_panel.as_mut() {
            panel.adding = Some(AddList {
                query: String::new(),
                ranked: Vec::new(),
                names,
                details,
                ranker,
                selected: 0,
                scroll: 0,
            });
        }
        self.dirty_frame = true;
    }

    /// Enter in the list: the parameter goes into the call's object at
    /// its default. A plugin remembers the choice - the same rails every
    /// time the plugin is used, in any set. A documented key is in the
    /// column already, so the score is all it changes.
    fn knobs_add_chosen(&mut self, adding: &AddList) {
        let rows = adding.rows();
        let Some(at) = rows.get(adding.selected) else {
            return;
        };
        let name = adding.names[*at].clone();
        let Some(parse) = self.knobs_parse.as_ref() else {
            return;
        };
        let Some(call) = parse.call.as_ref() else {
            return;
        };
        let open = call.open;
        let plugin = call.plugin.clone();
        let documented = parse
            .options
            .iter()
            .find(|option| option.key == name)
            .cloned();
        let default = if let Some(plugin) = &plugin {
            #[cfg(feature = "vst")]
            let default = parse
                .params
                .clone()
                .or_else(|| plugin_params(plugin))
                .as_deref()
                .and_then(|params| param_for(params, &name))
                .map_or(0.0, |param| param.default);
            #[cfg(not(feature = "vst"))]
            let default = {
                let _ = plugin;
                0.0
            };
            default
        } else if let Some(option) = &documented {
            option.start()
        } else {
            return;
        };
        // A documented key the score holds already, maybe by an alias or
        // as a pattern, stays as it is: a second spelling of it would set
        // the same thing twice. Its rail, when it has one, takes the keys.
        let holding = documented
            .as_ref()
            .is_some_and(|option| call.keys.iter().any(|key| option.names(key)));
        if holding {
            let rail = documented.as_ref().and_then(|option| {
                self.knobs_rows.as_ref()?.knobs.iter().position(|knob| {
                    knob.state.is_some()
                        && matches!(&knob.place, KnobPlace::Entry(key) if option.names(key))
                })
            });
            if let Some(at) = rail
                && let Some(panel) = self.knobs_panel.as_mut()
            {
                panel.selected = at;
            }
            self.status = format!("knobs: {name} is in the score already");
            self.dirty_frame = true;
            return;
        }
        let source = self.editor().source();
        let text = format!("{name}: {}", self.knob_text(default));
        let Some((range, insert)) = parameter_edit(&source, open, &text, None) else {
            self.status = "knobs: could not add the parameter here - fix the call first".into();
            self.dirty_frame = true;
            return;
        };
        // The plugin's list, started from the suggestions while the column
        // still holds them: the write below clears the reading they live in.
        if let Some(plugin) = &plugin {
            self.chosen_knobs(plugin);
        }
        if !self.knobs_write(range, &insert) {
            return;
        }
        if let Some(plugin) = &plugin {
            let chosen = self.chosen_knobs(plugin);
            if !chosen.contains(&name) {
                chosen.push(name.clone());
            }
            self.save_prefs_soon();
        }
        // The rail just added is the rail under the keys: ← and → move it
        // at once.
        self.refresh_knobs(Rect::default());
        let place = KnobPlace::Entry(plain_key(&name).unwrap_or_else(|| name.clone()));
        if let Some(at) = self
            .knobs_rows
            .as_ref()
            .and_then(|rows| rows.knobs.iter().rposition(|row| row.place == place))
            && let Some(panel) = self.knobs_panel.as_mut()
        {
            panel.selected = at;
        }
        self.status = match plugin {
            Some(plugin) => format!("knobs: added {name} - {plugin} keeps it from now on"),
            None => format!("knobs: added {name}"),
        };
        self.dirty_frame = true;
    }

    /// Delete in the list: the parameter stops being remembered. The
    /// score is not touched - what it holds stays until the score itself
    /// says otherwise.
    fn knobs_forget_chosen(&mut self, adding: &AddList) {
        let rows = adding.rows();
        let Some(at) = rows.get(adding.selected) else {
            return;
        };
        let name = adding.names[*at].clone();
        let Some(plugin) = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
            .and_then(|call| call.plugin.clone())
        else {
            return;
        };
        forget_param(self.chosen_knobs(&plugin), &name);
        self.save_prefs_soon();
        self.knobs_parse = None;
        self.status = format!("knobs: {plugin} no longer shows {name}");
        self.dirty_frame = true;
    }

    /// `r` on a plugin call: its rails go back to the suggestions, and the
    /// list the hand chose for the plugin is forgotten. Neither the score
    /// nor a remembered bound changes.
    /// `r` asks before it resets: the first press says what the second
    /// one does, and any other key keeps the rails as they are.
    fn knobs_reset_key(&mut self) {
        let plugin = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
            .and_then(|call| call.plugin.clone());
        let armed = self
            .knobs_panel
            .as_ref()
            .is_some_and(|panel| panel.reset_armed);
        match plugin {
            Some(_) if armed => self.knobs_reset(),
            Some(plugin) => {
                if let Some(panel) = self.knobs_panel.as_mut() {
                    panel.reset_armed = true;
                }
                self.status = format!(
                    "knobs: r again resets {plugin} to its suggested rails - any other key keeps them"
                );
                self.dirty_frame = true;
            }
            None => self.knobs_reset(),
        }
    }

    fn knobs_reset(&mut self) {
        let Some(plugin) = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
            .and_then(|call| call.plugin.clone())
        else {
            self.status = "knobs: r resets a plugin's rails - this call names no plugin".into();
            self.dirty_frame = true;
            return;
        };
        let forgot = self
            .prefs
            .vst_knobs
            .as_mut()
            .and_then(|knobs| knobs.remove(&plugin))
            .is_some();
        if forgot {
            self.save_prefs_soon();
        }
        if let Some(panel) = self.knobs_panel.as_mut() {
            panel.off.clear();
            panel.order.clear();
            panel.order_by_plugin = false;
            panel.selected = 0;
            panel.reset_armed = false;
        }
        self.status = format!("knobs: {plugin} shows its suggested rails again");
        self.dirty_frame = true;
    }

    /// `e`, or Enter: the number form on the rail under the keys - the
    /// value first, then the bounds. What the form shows is what the
    /// rail travels once Enter applies it.
    fn open_knobs_form(&mut self) {
        let Some(knob) = self.selected_knob() else {
            return;
        };
        let value = knob
            .state
            .as_ref()
            .map_or(knob.default, |state| state.value);
        let numbers = [value, knob.min, knob.max, knob.step];
        let mut form = SliderForm {
            fields: numbers.map(format_value),
            cursor: 0,
            selected: true,
            field: 0,
            error: None,
            written: false,
        };
        form.focus(0);
        let original = self.editor().source();
        let place = knob.place;
        let call = self.knobs_call_at();
        if let Some(panel) = self.knobs_panel.as_mut() {
            panel.editing = Some(KnobEdit {
                place,
                call,
                original,
                form,
            });
        }
        self.status =
            "knobs: Tab or the arrows between the fields · Enter applies · Esc back".into();
        self.dirty_frame = true;
    }

    /// The form's keys: the field's own keys from the shared form, Tab
    /// between the fields, Enter applies, Esc backs out. Returns whether
    /// the form stays open.
    fn knobs_edit_key(
        &mut self,
        editing: &mut KnobEdit,
        code: KeyCode,
        primary: bool,
        shift: bool,
        alt: bool,
    ) -> bool {
        let mut modifiers = KeyModifiers::empty();
        if primary {
            modifiers |= KeyModifiers::CONTROL;
        }
        if shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        if alt {
            modifiers |= KeyModifiers::ALT;
        }
        let key = KeyEvent::new(code, modifiers);
        match code {
            KeyCode::Esc => {
                self.status = "the knob keeps its value".into();
                false
            }
            // The fields of the form walk with the same keys as the smart
            // action's form: Tab, Shift+Tab and the vertical arrows.
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                editing.form.navigate(&key);
                true
            }
            KeyCode::Enter if !primary => !self.apply_knobs_form(editing),
            // The field's keys only: a key the form does not know stays
            // with the form, the way the quick action keeps its popup
            // open through a key it has no use for.
            _ => {
                editing.form.key(&key);
                true
            }
        }
    }

    /// Write the form into the score: nothing until every field is a
    /// number and min is below max, a value past a bound moves the bound,
    /// and a score that changed under the form is refused rather than
    /// miswritten. Returns whether the write landed.
    fn apply_knobs_form(&mut self, editing: &mut KnobEdit) -> bool {
        let current = self.editor().source();
        if current != editing.original {
            editing.form.error = Some("the score changed - Esc and open the form again".into());
            self.dirty_frame = true;
            return false;
        }
        let now = self.knobs_call_at();
        let editor = now
            .as_ref()
            .and_then(|now| self.scenes.get(now.scene))
            .map(|scene| &scene.editor);
        if !same_call(editor, editing.call.as_ref(), now.as_ref()) {
            editing.form.error =
                Some("the caret is in another call - Esc and open the form again".into());
            self.dirty_frame = true;
            return false;
        }
        let [value, min, max, step] = match editing.form.parse() {
            Ok(numbers) => numbers,
            Err(problem) => {
                editing.form.error = Some(problem);
                self.dirty_frame = true;
                return false;
            }
        };
        // A value past a bound takes the bound with it: the number typed is
        // the number meant, and the rail grows to hold it.
        let (min, max) = (min.min(value), max.max(value));
        if min >= max {
            editing.form.error = Some("min has to be below max".into());
            self.dirty_frame = true;
            return false;
        }
        let step = if step > 0.0 {
            step
        } else {
            (max - min) / 1000.0
        };
        editing.form.set_field(Field::Step, format_value(step));
        let Some(knob) = self
            .knobs_rows
            .as_ref()
            // Two entries can share a name. The score reads the last,
            // so the last is the one the form edits.
            .and_then(|rows| rows.knobs.iter().rfind(|knob| knob.place == editing.place))
            .cloned()
        else {
            editing.form.error = Some("the knob is gone from the call - Esc and back".into());
            self.dirty_frame = true;
            return false;
        };
        // The call's name and plugin, kept before the write: the write
        // clears the parse the store below would read.
        let call_named = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
            .map(|call| (call.name.clone(), call.plugin.clone()));
        let bounds_moved = (knob.min - min).abs() > f64::EPSILON
            || (knob.max - max).abs() > f64::EPSILON
            || (knob.step - step).abs() > f64::EPSILON;
        let fader = knob.state.as_ref().and_then(|state| state.fader);
        // An entry the hand took off from a fader is a fader still: its
        // bounds go back into its own call, not into the settings.
        let declared = fader.is_some() || (knob.state.is_none() && self.held_fader(&knob.place));
        // The rail with the form's bounds: a value the form let through
        // must not be pulled back into the bounds it replaced.
        let mut target = knob.clone();
        (target.min, target.max, target.step) = (min, max, step);
        let wrote = match (&knob.state, fader) {
            // The score declares its own bounds: the form's bounds go
            // into the call's own arguments, where they live.
            (Some(state), Some(_)) if bounds_moved => {
                self.write_fader_bounds(state, value, min, max, step)
            }
            (Some(_), _) => self.set_knob(&target, value),
            // An absent entry comes back with the form's value.
            (None, _) => self.knob_on(&target, value),
        };
        if !wrote {
            editing.form.error = Some("could not write - the call may have moved".into());
            self.dirty_frame = true;
            return false;
        }
        if bounds_moved && !declared {
            let Some((name, plugin)) = call_named else {
                return true;
            };
            self.prefs
                .knob_ranges
                .get_or_insert_with(Default::default)
                .insert(
                    range_key(&name, plugin.as_deref(), &knob.place),
                    KnobRange {
                        min: format_value(min),
                        max: format_value(max),
                        step: format_value(step),
                    },
                );
            self.save_prefs_soon();
        }
        self.status = format!("knobs: {} is {}", knob.label, short_number(value));
        self.dirty_frame = true;
        true
    }

    /// The bounds of a fader the score declares, written into the call's
    /// own arguments: the value too, in the one write, because the
    /// engine hears a control's bounds only through an update.
    fn write_fader_bounds(
        &mut self,
        state: &KnobState,
        value: f64,
        min: f64,
        max: f64,
        step: f64,
    ) -> bool {
        let source = self.editor().source();
        let Some(live) = literal_sliders(&source).into_iter().find(|live| {
            live.slider.from <= state.literal.start && state.literal.end <= live.slider.to
        }) else {
            return false;
        };
        let call = live.call.0..live.call.1;
        let code = code_only(&source);
        let open = source[call.clone()]
            .find('(')
            .is_some_and(|at| members(&code, call.start + at, true).is_some());
        if !open {
            return false;
        }
        let text = format!(
            "slider({}, {}, {}, {})",
            format_value(value),
            format_value(min),
            format_value(max),
            format_value(step)
        );
        self.knobs_write(call, &text)
    }

    /// Move the rail under the keys the way the score's own sliders move:
    /// a notch, or one step exactly with Shift. A notch is one step on a
    /// rail of 200 steps or fewer, else a hundredth of the travel in whole
    /// steps, and a hundredth of a log rail. The left end of an optional
    /// rail is the off stop: a move left from the bottom takes the entry
    /// out.
    fn nudge_knob(&mut self, direction: isize, shift: bool) {
        let Some(knob) = self.selected_knob() else {
            return;
        };
        let Some(state) = knob.state.as_ref() else {
            // Off: right is the only way, at the rail's default.
            if direction > 0 {
                self.knob_on(&knob, knob.default);
            }
            return;
        };
        if direction < 0 && knob.optional && state.value <= knob.min {
            self.knob_off(&knob);
            return;
        }
        let notches = direction as f64;
        let target = if shift {
            state.value + notches * knob.step
        } else {
            let travel = if knob.log {
                Travel::Logarithmic
            } else {
                Travel::Linear
            };
            rail_stepped(travel, state.value, knob.min, knob.max, knob.step, notches)
        };
        let moved = snap_value(target, knob.min, knob.max, knob.step);
        self.set_knob(&knob, moved);
        self.knob_status(&knob, moved);
    }

    /// Home and End: the rail's ends, in value. Home at an optional rail
    /// that is on is its off stop, the way ← at the left end is. At an
    /// off rail, End turns it back on at its top, the way a move right
    /// turns it on at its default.
    fn end_knob(&mut self, top: bool) {
        let Some(knob) = self.selected_knob() else {
            return;
        };
        if !top && knob.optional {
            if knob.state.is_some() {
                self.knob_off(&knob);
            }
            return;
        }
        // On the step grid: a step that does not divide the range ends
        // its top a step short of the max, as the score's sliders do.
        let value = snap_value(
            if top { knob.max } else { knob.min },
            knob.min,
            knob.max,
            knob.step,
        );
        if knob.state.is_none() {
            self.knob_on(&knob, value);
            return;
        }
        self.set_knob(&knob, value);
        self.knob_status(&knob, value);
    }

    /// The call the panel reads now, as the hand keys what it holds.
    fn knobs_call_at(&self) -> Option<CallAt> {
        let parse = self.knobs_parse.as_ref()?;
        let call = parse.call.as_ref()?;
        Some(CallAt {
            scene: parse.scene,
            open: call.open,
            revision: parse.revision,
            name: call.name.clone(),
        })
    }

    /// The rails the hand has chosen for a plugin. The first choice starts
    /// from the suggestions the column showed, so adding one rail or
    /// taking one off leaves the others where they were.
    fn chosen_knobs(&mut self, plugin: &str) -> &mut Vec<String> {
        #[cfg(feature = "vst")]
        let suggested: Vec<String> = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.params.as_deref())
            .map(|params| {
                suggested_params(params)
                    .into_iter()
                    .map(|param| score_key(&param.key))
                    .collect()
            })
            .unwrap_or_default();
        #[cfg(not(feature = "vst"))]
        let suggested = Vec::new();
        self.prefs
            .vst_knobs
            .get_or_insert_with(Default::default)
            .entry(plugin.to_owned())
            .or_insert(suggested)
    }

    /// Whether the hand holds this place off a fader.
    fn held_fader(&self, place: &KnobPlace) -> bool {
        self.knobs_panel.as_ref().is_some_and(|panel| {
            panel
                .off
                .iter()
                .any(|off| off.knob.place == *place && off.fader)
        })
    }

    /// The rail under the keys, as it stands this frame.
    fn selected_knob(&self) -> Option<Knob> {
        self.knobs_rows.as_ref().and_then(|rows| {
            rows.knobs
                .get(self.knobs_panel.as_ref().map_or(0, |panel| panel.selected))
                .cloned()
        })
    }

    /// Say what a move did, the way the inline sliders do: the value and
    /// the step, so the next move is a known quantity.
    fn knob_status(&mut self, knob: &Knob, value: f64) {
        self.status = format!(
            "knobs: {} {} · step {}",
            knob.label,
            short_number(value),
            short_number(knob.step)
        );
        self.dirty_frame = true;
    }

    /// Set a rail's value: through the live control when the score holds
    /// a fader there - the engine hears it without a re-evaluation - or
    /// as one control edit of the text, which the next update or auto
    /// evaluate picks up.
    fn set_knob(&mut self, knob: &Knob, value: f64) -> bool {
        let scene = self.scenes.current().id;
        let value = value.clamp(knob.min, knob.max);
        let Some(state) = knob.state.as_ref() else {
            return false;
        };
        if let Some(chip) = self
            .live_chips(scene)
            .into_iter()
            .find(|chip| chip.span.from <= state.literal.start && state.literal.end <= chip.span.to)
        {
            self.set_slider_with_smoothing(scene, &chip, value, false);
            return true;
        }
        // The number is already there: a drag that stays on one cell is
        // no edit, no undo step and no update.
        if value == state.value {
            return true;
        }
        let literal = state.literal.clone();
        let text = self.knob_text(value);
        self.knobs_write(literal, &text)
    }

    /// A rail's value as the score writes it: a plugin parameter to three
    /// decimals at most, so a step of a third writes `0.333`, any other
    /// number as it is.
    fn knob_text(&self, value: f64) -> String {
        let plugin = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
            .is_some_and(|call| call.plugin.is_some());
        if plugin {
            short_number(value)
        } else {
            format_value(value)
        }
    }

    /// The off stop: the entry comes out of the score, and the rail
    /// stays in the panel's hand so a move right writes it back.
    fn knob_off(&mut self, knob: &Knob) {
        let Some(state) = knob.state.as_ref() else {
            return;
        };
        let literal = state.literal.clone();
        let Some(call) = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
        else {
            return;
        };
        let Some(at) = call
            .entries
            .iter()
            .position(|entry| entry.contains(&literal.start))
        else {
            return;
        };
        let Some(close) = call.close.filter(|_| call.entries_closed) else {
            self.status = "knobs: the call is still open - close it first".into();
            self.dirty_frame = true;
            return;
        };
        let open = call.open;
        let source = self.editor().source();
        let code = code_only(&source);
        let Some(cuts) = entry_cuts(&source, &code, &call.entries, at) else {
            return;
        };
        // What goes back, and where it goes back in the call as the cuts
        // leave it.
        let mut pieces = Vec::with_capacity(cuts.len());
        let mut removed = 0;
        for cut in &cuts {
            pieces.push((cut.start - open - removed, source[cut.clone()].to_owned()));
            removed += cut.len();
        }
        let Some(entry) = cuts.iter().position(|cut| cut.contains(&literal.start)) else {
            return;
        };
        let held_text = HeldText {
            after: String::new(),
            literal: literal.start - cuts[entry].start..literal.end - cuts[entry].start,
            fader: state
                .fader
                .map(|fader| fader.call.0 - cuts[entry].start..fader.call.1 - cuts[entry].start),
            pieces,
            entry,
        };
        if !self.knobs_edit(cuts.into_iter().map(|cut| (cut, String::new())).collect()) {
            return;
        }
        let after = self
            .editor()
            .source()
            .get(open..=close - removed)
            .map(str::to_owned);
        let mut held = knob.clone();
        let fader = state.fader.is_some();
        held.state = None;
        held.default = state.value;
        if let Some(panel) = self.knobs_panel.as_mut() {
            panel.off.retain(|off| off.knob.place != held.place);
            // The row the rail sat at is its place in the column: the
            // rows above and below it are what they were, so the
            // assembly puts it back exactly there.
            let at = panel.selected;
            panel.off.push(HeldOff {
                at,
                knob: held,
                fader,
                text: after.map(|after| HeldText { after, ..held_text }),
            });
        }
        // The rows answer at once, the rail held off at its row.
        self.refresh_knobs(Rect::default());
        self.status = format!("knobs: {} off - the score keeps the rest", knob.label);
        self.dirty_frame = true;
    }

    /// Back from the off stop: the entry goes into the call's object at
    /// the value it is given. One the hand took off goes back exactly as
    /// it was - its place, its comments, its fader - while the call reads
    /// as the hand left it, and the bounds the rail is given go into its
    /// fader when they are new.
    fn knob_on(&mut self, knob: &Knob, value: f64) -> bool {
        let KnobPlace::Entry(key) = &knob.place else {
            return false;
        };
        let Some(call) = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
        else {
            return false;
        };
        let source = self.editor().source();
        let held = self.knobs_panel.as_ref().and_then(|panel| {
            panel
                .off
                .iter()
                .find(|off| off.knob.place == knob.place)
                .cloned()
        });
        let written = self.knob_text(value);
        let slider = || {
            format!(
                "slider({}, {}, {}, {})",
                format_value(value),
                format_value(knob.min),
                format_value(knob.max),
                format_value(knob.step)
            )
        };
        let unchanged = |text: &HeldText| {
            call.close
                .and_then(|close| source.get(call.open..=close))
                .is_some_and(|now| now == text.after)
        };
        let edits = match held.as_ref().and_then(|held| held.text.as_ref()) {
            Some(text) if unchanged(text) => {
                let off = held.as_ref().map(|held| &held.knob);
                let bounds_moved = off.is_some_and(|off| {
                    (off.min, off.max, off.step) != (knob.min, knob.max, knob.step)
                });
                // The value it left with goes back as the score spelt it:
                // `.5` stays `.5`.
                let moved = bounds_moved || off.is_none_or(|off| off.default != value);
                text.pieces
                    .iter()
                    .enumerate()
                    .map(|(at, (place, piece))| {
                        let mut piece = piece.clone();
                        if at == text.entry && moved {
                            match text.fader.clone().filter(|_| bounds_moved) {
                                Some(fader) => piece.replace_range(fader, &slider()),
                                None => piece.replace_range(text.literal.clone(), &written),
                            }
                        }
                        let at = call.open + place;
                        (at..at, piece)
                    })
                    .collect()
            }
            _ => {
                // An entry the hand took off from a fader comes back as
                // the fader it was, so the score keeps the declaration it
                // stood behind. The key goes back as the score writes it,
                // quoted when the plain name is not one.
                let text = if held.as_ref().is_some_and(|held| held.fader) {
                    format!("{}: {}", score_key(key), slider())
                } else {
                    format!("{}: {}", score_key(key), written)
                };
                let Some((range, insert)) = parameter_edit(&source, call.open, &text, None) else {
                    self.status =
                        "knobs: could not add the parameter here - fix the call first".into();
                    self.dirty_frame = true;
                    return false;
                };
                vec![(range, insert)]
            }
        };
        if !self.knobs_edit(edits) {
            return false;
        }
        if let Some(panel) = self.knobs_panel.as_mut() {
            panel.off.retain(|off| off.knob.place != knob.place);
        }
        // The written entry lands inside the object, and the hand keeps
        // the rail it just turned back on.
        self.refresh_knobs(Rect::default());
        if let Some(rows) = self.knobs_rows.as_ref()
            // Two entries can share a name. The score reads the last,
            // so the last is the one the hand keeps.
            && let Some(at) = rows.knobs.iter().rposition(|row| row.place == knob.place)
            && let Some(panel) = self.knobs_panel.as_mut()
        {
            panel.selected = at;
        }
        self.status = format!("knobs: {} on at {}", knob.label, short_number(value));
        self.dirty_frame = true;
        true
    }

    /// Delete or Backspace: an entry the score holds goes to the off
    /// stop. A rail that is already off, and is a remembered plugin
    /// parameter, is forgotten outright, with the key the visual fx
    /// panel removes its widgets with.
    fn knobs_delete(&mut self) {
        let Some(knob) = self.selected_knob() else {
            return;
        };
        if !knob.optional {
            self.status = format!("knobs: the call needs {} - it has no off stop", knob.label);
            self.dirty_frame = true;
            return;
        }
        if knob.state.is_some() {
            self.knob_off(&knob);
            return;
        }
        let KnobPlace::Entry(key) = &knob.place else {
            return;
        };
        let Some(plugin) = self
            .knobs_parse
            .as_ref()
            .and_then(|parse| parse.call.as_ref())
            .and_then(|call| call.plugin.clone())
        else {
            // No plugin, nothing remembered: a rail the hand holds is
            // let go, and its row leaves the column.
            if let Some(panel) = self.knobs_panel.as_mut() {
                let before = panel.off.len();
                panel.off.retain(|off| off.knob.place != knob.place);
                if before != panel.off.len() {
                    self.status = format!("knobs: {} leaves the column", knob.label);
                    self.dirty_frame = true;
                }
            }
            return;
        };
        forget_param(self.chosen_knobs(&plugin), key);
        if let Some(panel) = self.knobs_panel.as_mut() {
            panel.off.retain(|off| off.knob.place != knob.place);
        }
        self.save_prefs_soon();
        self.knobs_parse = None;
        self.status = format!("knobs: {plugin} no longer shows {key}");
        self.dirty_frame = true;
    }

    /// One edit of the score the panel owns: a control edit, undoable,
    /// keeping the viewport - a rail that moves must not scroll the text
    /// under the pointer - setting off what an edit always sets off, and
    /// applied at once: a rail that moves sounds while it moves, the way
    /// the score's own sliders do, instead of after the typing pause. A
    /// stopped set, or auto evaluate off, keeps the pause.
    fn knobs_write(&mut self, range: Range<usize>, text: &str) -> bool {
        self.knobs_edit(vec![(range, text.to_owned())])
    }

    /// [`Self::knobs_write`] for edits that land together: ranges of the
    /// score as it stands, none overlapping, as one undo step.
    fn knobs_edit(&mut self, edits: Vec<(Range<usize>, String)>) -> bool {
        let scene = self.scenes.current().id;
        let moment = self.moment();
        let Some(entry) = self.scenes.get_mut(scene) else {
            return false;
        };
        let viewport = entry.editor.viewport();
        let before = entry.editor.revision();
        let transaction = edits.into_iter().fold(
            Transaction::new(EditOrigin::Control),
            |transaction, (range, text)| {
                transaction.replace(Edit::new(
                    ByteOffset(range.start)..ByteOffset(range.end),
                    text,
                ))
            },
        );
        match entry.editor.apply_transaction(transaction, moment) {
            Ok(()) => {
                entry.editor.set_viewport(viewport);
                self.clear_error(ErrorOwner::Editor);
            }
            Err(error) => {
                self.set_error(ErrorOwner::Editor, error.to_string());
                return false;
            }
        }
        self.after_edit(scene, before, false);
        self.auto_evaluate_due = false;
        self.auto_evaluate();
        self.knobs_parse = None;
        self.dirty_frame = true;
        true
    }

    /// A press on the panel: a rail sets its value where it is clicked,
    /// and keeps the pointer for the drag that follows, a name selects its
    /// rail, and a sheet that is up closes. Returns whether the press was
    /// the panel's.
    pub(super) fn press_knobs(&mut self, area: Rect, x: u16, y: u16) -> bool {
        // The add list reaches over the score: a press there is the
        // list's, not the score's.
        let sheet = self.add_list_sheet();
        let on_sheet = sheet.is_some_and(|sheet| within(sheet, x, y));
        if self.knobs_panel.is_none()
            || !(on_sheet || area.contains(ratatui::layout::Position::new(x, y)))
        {
            return false;
        }
        // The panel takes the pointer and the keyboard with the press,
        // as the desk does. The rows are made here too: a press can be
        // the first thing the panel sees after opening.
        self.pointer = Some(Pointer::Knobs { rail: None });
        self.focus_panel(PanelKind::Knobs);
        self.refresh_knobs(Rect::default());
        // A press on a name adds it, the way Enter does. Elsewhere on the
        // list it does nothing, and anywhere else it closes a sheet.
        if let Some(sheet) = sheet.filter(|_| on_sheet) {
            let picked = self
                .knobs_panel
                .as_mut()
                .and_then(|panel| panel.adding.as_mut())
                .and_then(|adding| {
                    let at = add_row_at(sheet, adding.scroll, x, y)?;
                    (at < adding.rows().len()).then(|| {
                        adding.selected = at;
                    })
                });
            if picked.is_some()
                && let Some(adding) = self
                    .knobs_panel
                    .as_mut()
                    .and_then(|panel| panel.adding.take())
            {
                self.knobs_add_chosen(&adding);
            }
            self.dirty_frame = true;
            return true;
        }
        if let Some(panel) = self.knobs_panel.as_mut()
            && (panel.adding.is_some() || panel.editing.is_some())
        {
            panel.adding = None;
            panel.editing = None;
            self.dirty_frame = true;
            return true;
        }
        let Some(parts) = parts(area) else {
            return true;
        };
        let scroll = self.knobs_panel.as_ref().map_or(0, |panel| panel.scroll);
        let Some((at, on_rail)) = knob_at(parts.body, scroll, x, y) else {
            return true;
        };
        if let Some(panel) = self.knobs_panel.as_mut() {
            panel.selected = at;
        }
        // The presets section under the rails: the heading opens and
        // closes by the click, and a name writes its preset into the call.
        let knobs = self.knobs_rows.as_ref().map_or(0, |rows| rows.knobs.len());
        if at >= knobs {
            match at - knobs {
                0 => {
                    if let Some(presets) = self
                        .knobs_panel
                        .as_mut()
                        .and_then(|panel| panel.presets.as_mut())
                    {
                        presets.open = !presets.open;
                    }
                }
                offset => {
                    let name = self
                        .knobs_panel
                        .as_ref()
                        .and_then(|panel| panel.presets.as_ref())
                        .and_then(|presets| presets.names.get(offset - 1))
                        .cloned();
                    if let Some(name) = name {
                        self.knob_preset_write(&name);
                    }
                }
            }
            self.dirty_frame = true;
            return true;
        }
        let rail = knob_row(parts.body, at - scroll).map(|(_, rail)| rail);
        let Some(rail) = rail.filter(|_| on_rail) else {
            self.dirty_frame = true;
            return true;
        };
        // The rail the press landed on is the drag's, wherever the
        // pointer goes next: off the column, over the score, past the
        // screen's edge.
        self.pointer = Some(Pointer::Knobs {
            rail: Some((at, rail)),
        });
        self.drag_knob(at, rail, x);
        true
    }

    /// A drag of a rail the press took: the value follows the pointer's
    /// column along the rail, the way the score's own sliders and the
    /// desk's faders follow it, to the pixel where the terminal reports
    /// one, and held at the rail's ends past them. The far left of an
    /// optional rail is the off stop.
    pub(super) fn drag_knob(&mut self, at: usize, rail: Rect, x: u16) {
        // A write moves the text under the next event's rail, so the rows
        // are remade from the score as it now stands - the parse cache
        // makes this the assembly alone when the drag wrote nothing.
        self.refresh_knobs(Rect::default());
        let Some(knob) = self
            .knobs_rows
            .as_ref()
            .and_then(|rows| rows.knobs.get(at).cloned())
        else {
            return;
        };
        if rail.width < 3 {
            return;
        }
        if knob.optional && x <= rail.x {
            if knob.state.is_some() {
                self.knob_off(&knob);
            }
            return;
        }
        // An optional rail travels the cells right of its off stop.
        let track = if knob.optional {
            SliderTrack {
                x: rail.x + 1,
                width: rail.width - 1,
            }
        } else {
            SliderTrack {
                x: rail.x,
                width: rail.width,
            }
        };
        let cell_width = self
            .features
            .pixel_mouse
            .then_some(self.features.cell_pixels)
            .flatten()
            .map(|(width, _)| width);
        let ratio = track.ratio(x, self.pointer_subcell, cell_width);
        let travel = if knob.log {
            Travel::Logarithmic
        } else {
            Travel::Linear
        };
        let resolution = track.resolution(cell_width);
        let value = pointer_value(travel, ratio, resolution, knob.min, knob.max, knob.step);
        if knob.state.is_none() {
            self.knob_on(&knob, value);
            return;
        }
        self.set_knob(&knob, value);
        self.knob_status(&knob, value);
    }

    /// The wheel over the panel walks its rails.
    pub(super) fn wheel_knobs(&mut self, down: bool) -> bool {
        if self.knobs_panel.is_none() {
            return false;
        }
        self.refresh_knobs(Rect::default());
        let count = self.knobs_rows.as_ref().map_or(0, |rows| rows.knobs.len());
        let Some(panel) = self.knobs_panel.as_mut() else {
            return false;
        };
        let step = if down { 1 } else { -1 };
        panel.selected =
            ((panel.selected as isize + step).rem_euclid(count.max(1) as isize)) as usize;
        self.dirty_frame = true;
        true
    }

    /// The wheel over the column, as over every other panel's rows: the
    /// selection walks the knobs.
    pub(super) fn wheel_knobs_over(&mut self, x: u16, y: u16, direction: f32) -> bool {
        if let Some(sheet) = self.add_list_sheet()
            && within(sheet, x, y)
        {
            self.wheel_add_list(direction < 0.0);
            return true;
        }
        self.knobs_panel.is_some()
            && within(self.regions.knobs, x, y)
            && self.wheel_knobs(direction < 0.0)
    }

    /// Where the add list stands, while it is open.
    fn add_list_sheet(&self) -> Option<Rect> {
        let adding = self.knobs_panel.as_ref()?.adding.as_ref()?;
        Some(add_sheet(self.regions.knobs, adding.rows().len()))
    }

    /// The names the add list shows at once, with `rows` to list.
    fn add_list_shown(&self, rows: usize) -> usize {
        add_shown(add_sheet(self.regions.knobs, rows))
    }

    /// The wheel over the add list, from a mouse or a trackpad alike: each
    /// report scrolls the list as many rows as it scrolls the score, and
    /// the chosen name stays among the rows in view.
    fn wheel_add_list(&mut self, down: bool) {
        let shown = self.add_list_sheet().map_or(1, add_shown);
        let Some(adding) = self
            .knobs_panel
            .as_mut()
            .and_then(|panel| panel.adding.as_mut())
        else {
            return;
        };
        let count = adding.rows().len();
        if count == 0 {
            return;
        }
        let rows = crate::editor::DEFAULT_WHEEL_ROWS;
        let last = count.saturating_sub(shown);
        adding.scroll = if down {
            (adding.scroll + rows).min(last)
        } else {
            adding.scroll.saturating_sub(rows)
        };
        let bottom = (adding.scroll + shown).min(count) - 1;
        adding.selected = adding.selected.clamp(adding.scroll, bottom);
        self.dirty_frame = true;
    }
}

/// Take a parameter off a plugin's chosen rails. A list the hand emptied
/// stays, empty: the plugin then shows no rails of its own, where no list
/// at all would show the suggestions again.
fn forget_param(chosen: &mut Vec<String>, key: &str) {
    // The list keeps a key as the score writes it, quoted when it must be.
    chosen.retain(|remembered| remembered != key && *remembered != score_key(key));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_at_the_caret_yields_its_numbers() {
        let source = "fx(440, { cutoff: 800, note: \"c4\", q: 0.5 })";
        let parse = parse_call(source, 8).expect("the caret is in the call");
        assert_eq!(parse.name, "fx");
        let places: Vec<String> = parse.knobs.iter().map(|knob| knob.label.clone()).collect();
        assert_eq!(places, ["#0", "cutoff", "q"], "numbers only");
        let cutoff = parse
            .knobs
            .iter()
            .find(|knob| knob.label == "cutoff")
            .expect("the entry");
        let state = cutoff.state.as_ref().expect("the score holds it");
        assert_eq!(state.value, 800.0);
        assert_eq!(&source[state.literal.clone()], "800");
        assert!(cutoff.optional, "an entry can be off");
        let argument = parse
            .knobs
            .iter()
            .find(|knob| knob.label == "#0")
            .expect("the positional number");
        assert!(!argument.optional, "a positional argument cannot be off");
    }

    #[test]
    fn strings_and_comments_hold_no_knobs() {
        // A number inside a pattern string, and one in a comment, are
        // blanked by the checker, and a rail that would rewrite them
        // would rewrite the wrong thing.
        let source = "s(\"bd 440\")";
        let parse = parse_call(source, 5).expect("the call is there");
        assert!(parse.knobs.is_empty(), "the string holds no number");
        let source = "lpf(440) // 800";
        let parse = parse_call(source, 2).expect("the call is there");
        assert_eq!(parse.knobs.len(), 1, "the comment holds no second knob");
        assert!(parse_call("bd hh cp", 2).is_none(), "no call at all");
    }

    #[test]
    fn a_fader_argument_keeps_its_call() {
        let source = "set({ cutoff: slider(200, 100, 2000) })";
        let parse = parse_call(source, 12).expect("the caret is in the call");
        let cutoff = parse
            .knobs
            .iter()
            .find(|knob| knob.label == "cutoff")
            .expect("the entry");
        let state = cutoff.state.as_ref().expect("the fader is the value");
        let fader = state.fader.expect("the score declares bounds");
        assert_eq!(fader.min, 100.0);
        assert_eq!(fader.max, 2000.0);
        assert_eq!(state.value, 200.0);
        assert_eq!(&source[state.literal.clone()], "200");
        assert_eq!(cutoff.min, 100.0, "the fader's bounds are the knob's");
        assert_eq!(cutoff.max, 2000.0);
    }

    #[test]
    fn the_call_behind_a_fader_wins_over_the_fader_itself() {
        // The caret inside the fader's own numbers: the panel offers the
        // set's cutoff, not four knobs for the fader's own arguments.
        let source = "set({ cutoff: slider(200, 100, 2000) })";
        let parse = parse_call(source, 25).expect("a call behind the fader");
        assert_eq!(parse.name, "set");
        assert_eq!(parse.knobs.len(), 1, "only the cutoff");
        assert_eq!(parse.knobs[0].label, "cutoff");
    }

    #[test]
    fn half_an_expression_is_no_rail() {
        let source = "set({ cutoff: 1 + slider(0.5) })";
        let parse = parse_call(source, 6).expect("the call");
        assert!(
            parse.knobs.is_empty(),
            "rewriting part of an expression would change what the score means"
        );
    }

    #[test]
    fn bounds_follow_their_order_of_authority() {
        let mut knob = Knob {
            label: "cutoff".into(),
            place: KnobPlace::Entry("cutoff".into()),
            state: Some(KnobState {
                literal: 0..3,
                fader: None,
                value: 800.0,
            }),
            min: 0.0,
            max: 1.0,
            step: 0.01,
            default: 800.0,
            optional: true,
            log: false,
        };
        let param = ParamDefaults {
            default: 0.25,
            step: 0.001,
        };
        let deduced = (20.0, 20000.0, 1.0);
        // Nothing remembered, no plugin: the deduction, on the ear's scale
        // for a frequency.
        resolve_bounds(&mut knob, None, None, deduced, true, true);
        assert_eq!((knob.min, knob.max, knob.step), deduced);
        assert!(
            knob.log,
            "a positive range on a frequency control is a log rail"
        );
        // The plugin's parameter outranks the deduction.
        resolve_bounds(&mut knob, None, Some(param), deduced, false, true);
        assert_eq!((knob.min, knob.max, knob.step), (0.0, 1.0, 0.001));
        assert_eq!(knob.default, 0.25, "the plugin's own start value");
        assert!(!knob.log, "a log rail needs a positive floor");
        // What the form remembered outranks the plugin.
        let remembered = KnobRange {
            min: "100".into(),
            max: "18000".into(),
            step: "10".into(),
        };
        resolve_bounds(
            &mut knob,
            Some(&remembered),
            Some(param),
            deduced,
            true,
            true,
        );
        assert_eq!((knob.min, knob.max, knob.step), (100.0, 18000.0, 10.0));
        // The score's own fader outranks everything, and travels as the
        // transpiler marks it, the way the pill over it does.
        let fader = |min, max, frequency| Fader {
            min,
            max,
            step: 1.0,
            frequency,
            call: (0, 0),
        };
        knob.state.as_mut().expect("set above").fader = Some(fader(100.0, 1000.0, false));
        resolve_bounds(
            &mut knob,
            Some(&remembered),
            Some(param),
            deduced,
            true,
            true,
        );
        assert_eq!((knob.min, knob.max), (100.0, 1000.0));
        assert!(
            !knob.log,
            "a fader the transpiler did not mark travels linearly"
        );
        knob.state.as_mut().expect("set above").fader = Some(fader(100.0, 16000.0, true));
        resolve_bounds(
            &mut knob,
            Some(&remembered),
            Some(param),
            deduced,
            true,
            true,
        );
        assert!(knob.log, "a marked fader on a positive range is a log rail");
        knob.state.as_mut().expect("set above").fader = Some(fader(0.0, 1.0, true));
        resolve_bounds(
            &mut knob,
            Some(&remembered),
            Some(param),
            deduced,
            true,
            true,
        );
        assert!(
            !knob.log,
            "a log rail needs a positive floor, or its maths turns to NaN"
        );
    }

    /// A number nothing declares travels what the reference gives its
    /// parameter: whole numbers for a count, quarters for a speed, the
    /// ear's range for a frequency, and never a value the engine refuses. A
    /// number past the travel widens it. A whole number nothing documents
    /// keeps to whole steps, and a plugin's parameter is the 0 to 1 its
    /// scores write, whatever its name.
    #[test]
    fn a_bare_number_travels_what_the_reference_gives_it() {
        let reference = crate::reference::Reference::load_all();
        let rail = |source: &str| {
            let caret = source.find('(').expect("a call") + 1;
            let call = parse_call(source, caret).expect("a call");
            let knob = call.knobs.first().expect("a rail");
            let value = knob.state.as_ref().expect("a number").value;
            let number = documented_number(&reference, &call, &knob.place);
            deduced_range(&call, value, None, number)
        };
        assert_eq!(rail("seg(16)"), (1.0, 64.0, 1.0), "a segment is whole");
        assert_eq!(
            rail("segment(128)"),
            (1.0, 500.0, 1.0),
            "widened to take it"
        );
        assert_eq!(rail("ply(3)"), (1.0, 16.0, 1.0));
        assert_eq!(rail("euclid(3, 8)"), (0.0, 16.0, 1.0));
        assert_eq!(rail("fast(1.5)"), (0.25, 8.0, 0.25));
        assert_eq!(rail("off(0.125)"), (0.0, 1.0, 0.0625));
        assert_eq!(rail("setcps(0.5)"), (0.1, 2.0, 0.01));
        assert_eq!(
            rail("lpf(800)"),
            (20.0, 20000.0, 1.0),
            "an alias names the control"
        );
        assert_eq!(rail("unison(16)"), (1.0, 32.0, 1.0), "the voice refuses 33");
        assert_eq!(rail("delayfeedback(0.6)"), (0.0, 0.98, 0.01));
        assert_eq!(rail("set({ resonance: 5 })"), (0.0, 50.0, 0.1));
        assert_eq!(rail("begin(0.5)"), (0.0, 0.99, 0.01));
        assert_eq!(rail("gain(0.8)"), (0.0, 2.0, 0.01));
        assert_eq!(
            rail("swingBy(0.3, 4)"),
            (0.0, 1.0, 0.01),
            "the amount comes first"
        );
        assert_eq!(rail("vst(\"x\", { cutoff: 0.5 })"), (0.0, 1.0, 0.001));
        assert_eq!(rail("nothingKnown(12)"), (0.0, 25.0, 1.0));
        assert_eq!(rail("nothingKnown(0.3)"), (0.0, 1.0, 0.01));
        assert_eq!(rail("nothingKnown(1)"), (0.0, 5.0, 0.01), "room above");
        // A speed below the travel floors at itself, never at 0.
        assert_eq!(rail("fast(0.125)"), (0.125, 8.0, 0.125));
        assert_eq!(rail("slow(0.1)"), (0.1, 8.0, 0.1));
        assert_eq!(rail("inside(1.5, rev)"), (0.25, 8.0, 0.25));
        assert_eq!(rail("octave(-1)"), (-4.0, 4.0, 1.0));
        assert_eq!(rail("cps(1)"), (0.1, 2.0, 0.01));
        assert_eq!(rail("bpq(1)"), (0.0, 50.0, 0.1));
    }

    /// Every number the reference gives names a parameter it documents,
    /// the core's own and the extensions' alike: a row with a misspelt
    /// name would never reach a rail.
    #[test]
    fn every_documented_number_names_a_parameter() {
        let reference = crate::reference::Reference::load_all();
        let numbers = rustel_core::reference::REFERENCE_NUMBERS.iter();
        #[cfg(feature = "extensions")]
        let numbers = numbers.chain(rustel_ext::reference_numbers());
        for number in numbers {
            let entry = reference
                .lookup_in(number.entry, crate::reference::ReferenceScope::Pattern)
                .and_then(|index| reference.entry(index))
                .unwrap_or_else(|| panic!("{} is documented", number.entry));
            assert_eq!(entry.name, number.entry, "the entry's own name");
            assert!(
                entry
                    .params
                    .iter()
                    .any(|param| param.name == number.parameter),
                "{} documents {}",
                number.entry,
                number.parameter
            );
        }
    }

    /// An audio control reads an object as its value, and the engine
    /// refuses the voice: its entries are no rails. `set` takes them.
    #[test]
    fn an_audio_control_takes_a_number_not_an_object() {
        let parse = parse_call("lpf({ cutoff: 800 })", 6).expect("the call");
        assert!(parse.knobs.is_empty(), "lpf({{ cutoff }}) is no filter");
        let parse = parse_call("set({ cutoff: 800 })", 6).expect("the call");
        assert_eq!(parse.knobs.len(), 1);
        // A fader as an argument is the score's own rail for it.
        let source = "lpf(slider(800, 20, 20000))";
        let parse = parse_call(source, 4).expect("the call");
        let state = parse.knobs[0].state.as_ref().expect("the fader");
        assert!(state.fader.is_some());
        assert_eq!(&source[state.literal.clone()], "800");
    }

    /// Taking an entry out keeps the comments around it, and putting it
    /// back gives the score it had, in its own spelling.
    #[test]
    fn the_off_stop_keeps_comments_and_comes_back_exactly() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let score = "$: s(\"bd\").set({\n  cutoff: 800, // bright\n  // q: 2,\n  q: .0\n})\n";
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(20)));
        app.open_knobs_panel(true);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        // q sits at the bottom of its rail: a move left is its off stop.
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            app.scenes.current().editor.source(),
            "$: s(\"bd\").set({\n  cutoff: 800 // bright\n  // q: 2,\n})\n"
        );
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.scenes.current().editor.source(), score);
    }

    /// A comment between a value and its comma, and line breaks of either
    /// kind, stay with the object when an entry comes out.
    #[test]
    fn an_entry_comes_out_around_its_comments() {
        let cut = |source: &str, at: usize| {
            let code = code_only(source);
            let open = source.find('{').expect("an object");
            let entries = members(&code, open, false).expect("closed");
            let mut out = source.to_owned();
            for range in entry_cuts(source, &code, &entries, at)
                .expect("cuts")
                .iter()
                .rev()
            {
                out.replace_range(range.clone(), "");
            }
            out
        };
        assert_eq!(cut("{ a: 1 /* one */, b: 2 }", 0), "{ /* one */ b: 2 }");
        assert_eq!(
            cut("{ b: 2, 'a b': 1 }", 1),
            "{ b: 2 }",
            "a quoted key is code"
        );
        assert_eq!(cut("{\r\n  a: 1,\r\n  b: 2\r\n}", 1), "{\r\n  a: 1\r\n}");
        assert_eq!(cut("{\r\n  a: 1,\r\n  b: 2\r\n}", 0), "{\r\n  b: 2\r\n}");
    }

    /// The hand holds a rail's bounds while it works the call: a number
    /// moved to the top does not widen the rail under it, not even when an
    /// edit above the call moves the call along the text.
    #[test]
    fn a_rail_holds_its_bounds_while_it_moves() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        app.scenes.current_mut().editor =
            Editor::new("$: s(\"hh\").gain(0.5)\n$: s(\"bd\").gain(1.2)\n").expect("score");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(37)));
        app.open_knobs_panel(true);
        press(&mut app, KeyCode::End, KeyModifiers::NONE);
        type_over(&mut app, "gain(0.5)", "gain(0.75)");
        press(&mut app, KeyCode::End, KeyModifiers::NONE);
        let top = app.scenes.current().editor.revision();
        press(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(
            app.scenes.current().editor.source(),
            "$: s(\"hh\").gain(0.75)\n$: s(\"bd\").gain(2)\n"
        );
        assert_eq!(
            app.scenes.current().editor.revision(),
            top,
            "a move that changes nothing is no edit"
        );
        // 200 steps make a notch of one step, and Shift moves one step.
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        assert!(
            app.scenes.current().editor.source().contains("gain(1.98)"),
            "{}",
            app.scenes.current().editor.source()
        );
    }

    /// A count stays whole: the arrows move `seg(16)` one segment at a
    /// time and back to 16, and a drag across its rail writes whole
    /// segments only.
    #[test]
    fn a_count_stays_whole_under_the_keys_and_the_pointer() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        let score = "$: n(\"0 1\").seg(16)\n";
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let caret = score.find("16").expect("the count");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(caret)));
        app.open_knobs_panel(true);
        let count = |app: &App| {
            let source = app.scenes.current().editor.source();
            let open = source.rfind('(').expect("the call") + 1;
            let close = source.rfind(')').expect("the call");
            source[open..close].to_owned()
        };
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(count(&app), "17");
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(count(&app), "16");
        let rail = Rect::new(0, 0, 24, 1);
        for x in 0..24 {
            app.drag_knob(0, rail, x);
            let written = count(&app);
            assert!(!written.contains('.'), "the drag wrote {written}");
        }
    }

    /// A rail's move leaves with the write, not after the typing pause, so
    /// a plain number is heard as the rail moves, the way a score slider
    /// is. Nothing leaves while the set is stopped: the write alone is
    /// owed, and the press that plays carries it.
    #[test]
    fn a_rail_move_is_applied_at_once_while_the_set_plays() {
        use super::super::tests::support::{app_over, sounding};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        app.ui_settings.auto_evaluate = true;
        let score = "$: s(\"bd\").gain(0.5)\n";
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let caret = score.find("0.5").expect("the value");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(caret)));
        app.open_knobs_panel(true);
        app.drag_knob(0, Rect::new(0, 0, 24, 1), 20);
        assert!(
            app.pending_evaluation.is_none() && app.inflight.is_empty(),
            "a stopped set is asked for nothing"
        );
        app.snapshot = Some(sounding(true, false));
        app.drag_knob(0, Rect::new(0, 0, 24, 1), 4);
        assert!(
            app.pending_evaluation.is_some() || !app.inflight.is_empty(),
            "the update left with the write"
        );
    }

    /// What the hand holds belongs to one call: an entry taken off one
    /// `set` never lands in another, and a form opened on one call does
    /// not write into the call the caret went to.
    #[test]
    fn the_hand_keeps_to_its_own_call() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let score = "$: s(\"bd\").set({ q: 0.2, cutoff: 800 })\n$: s(\"hh\").set({ q: 0.5 })\n";
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let caret = |app: &mut App, line: usize| {
            let source = app.scenes.current().editor.source();
            let at = source.match_indices("q:").nth(line).expect("a q").0;
            let _ = app
                .scenes
                .current_mut()
                .editor
                .set_selection(Selection::caret(ByteOffset(at)));
        };
        caret(&mut app, 0);
        app.open_knobs_panel(true);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        caret(&mut app, 1);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        let moved = "$: s(\"bd\").set({ q: 0.2 })\n$: s(\"hh\").set({ q: 0.51 })\n";
        assert_eq!(
            app.scenes.current().editor.source(),
            moved,
            "the second call moves its own q"
        );
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        caret(&mut app, 0);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.scenes.current().editor.source(),
            moved,
            "the form refuses the other call's q"
        );
    }

    /// Before the hand chooses, a plugin shows the parameters a host can
    /// tell are its sound: its named mix first, its continuous parameters
    /// in its own order, its switches after them, and no bypass, program
    /// change or MIDI stand-in. A full column is ten rails.
    #[cfg(feature = "vst")]
    #[test]
    fn a_plugin_suggests_its_sound_first() {
        let param = |key: &str, name: &str, steps: u32, role: ParamRole| ParamInfo {
            id: 0,
            name: name.into(),
            key: key.into(),
            group: String::new(),
            units: String::new(),
            default: 0.5,
            steps,
            default_text: "0.0 dB".into(),
            role,
        };
        // A switch the plugin calls continuous still reads as a word.
        let mut cleanxov = param("cleanxov", "Clean XOV", 0, ParamRole::Plain);
        cleanxov.default_text = "OFF".into();
        let params = [
            param("power", "Power", 1, ParamRole::Plain),
            param("bypass", "Bypass", 1, ParamRole::Bypass),
            cleanxov,
            param("hostbypass", "Host Bypass", 1, ParamRole::Plain),
            param("inputgain", "Input Gain", 0, ParamRole::Plain),
            param("midicc", "MIDI CC", 0, ParamRole::Plain),
            param("drive", "Drive", 0, ParamRole::Plain),
            param("mix", "Mix", 0, ParamRole::Mix),
            param("program", "Program", 127, ParamRole::Program),
        ];
        let keys: Vec<&str> = suggested_params(&params)
            .iter()
            .map(|param| param.key.as_str())
            .collect();
        assert_eq!(keys, ["mix", "inputgain", "drive", "power", "cleanxov"]);
        let many: Vec<ParamInfo> = (0..40)
            .map(|at| param(&format!("p{at}"), &format!("P {at}"), 0, ParamRole::Plain))
            .collect();
        assert_eq!(suggested_params(&many).len(), 10);
    }

    /// The first choice keeps the suggestions the column showed: taking
    /// one off leaves the others, and the plugin remembers the rest.
    #[cfg(feature = "vst")]
    #[test]
    fn the_first_choice_starts_from_the_suggestions() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        app.scenes.current_mut().editor =
            Editor::new("$: s(\"bd\").vst(\"FX\", { })\n").expect("score");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(22)));
        app.open_knobs_panel(true);
        app.refresh_knobs(Rect::default());
        let param = |key: &str| ParamInfo {
            id: 0,
            name: key.into(),
            key: key.into(),
            group: String::new(),
            units: String::new(),
            default: 0.5,
            steps: 0,
            default_text: String::new(),
            role: ParamRole::Plain,
        };
        // The host has the plugin's parameters, as it would once loaded.
        if let Some(parse) = app.knobs_parse.as_mut() {
            parse.params = Some(vec![param("drive"), param("tone"), param("mix")]);
        }
        app.refresh_knobs(Rect::default());
        let rows: Vec<String> = app
            .knobs_rows
            .as_ref()
            .expect("the rows")
            .knobs
            .iter()
            .map(|knob| knob.label.clone())
            .collect();
        assert_eq!(rows, ["drive", "tone", "mix"], "the suggestions, off");
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        let chosen = app
            .prefs
            .vst_knobs
            .as_ref()
            .and_then(|knobs| knobs.get("FX"))
            .cloned();
        assert_eq!(chosen, Some(vec!["tone".to_owned(), "mix".to_owned()]));
        // r asks first: another key keeps the list, a second r resets.
        let kept = |app: &App| {
            app.prefs
                .vst_knobs
                .as_ref()
                .is_some_and(|knobs| knobs.contains_key("FX"))
        };
        press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(kept(&app), "one r only asks");
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(kept(&app), "a key between keeps the list");
        press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(!kept(&app), "the second r gives the suggestions back");
    }

    /// A plugin call opened with forty parameters the host knows.
    #[cfg(feature = "vst")]
    fn plugin_app(directory: &std::path::Path, score: &str, params: Vec<ParamInfo>) -> App {
        use super::super::tests::support::app_over;
        use crate::editor::{ByteOffset, Selection};
        let mut app = app_over(directory);
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let caret = score.find('{').expect("an object") + 1;
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(caret)));
        app.open_knobs_panel(true);
        app.refresh_knobs(Rect::default());
        if let Some(parse) = app.knobs_parse.as_mut() {
            parse.params = Some(params);
        }
        app.refresh_knobs(Rect::default());
        app
    }

    #[cfg(feature = "vst")]
    fn plugin_param(key: &str, steps: u32) -> ParamInfo {
        ParamInfo {
            id: 0,
            name: key.into(),
            key: key.into(),
            group: String::new(),
            units: String::new(),
            default: 0.5,
            steps,
            default_text: String::new(),
            role: ParamRole::Plain,
        }
    }

    /// The add list is wider than the column and as tall as it. It scrolls
    /// under the wheel - a trackpad's burst of reports as much as a
    /// mouse's notches - over the part of it above the score too, and a
    /// press on a name adds it.
    #[cfg(feature = "vst")]
    #[test]
    fn the_add_list_is_roomy_scrolls_and_takes_a_click() {
        use super::super::tests::support::press;
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let directory = tempfile::tempdir().expect("tempdir");
        let params = (0..40)
            .map(|at| plugin_param(&format!("p{at}"), 0))
            .collect();
        let mut app = plugin_app(directory.path(), "$: s(\"bd\").vst(\"FX\", { })\n", params);
        app.regions.knobs = Rect::new(100, 1, 26, 30);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        let sheet = app.add_list_sheet().expect("the list is open");
        assert_eq!((sheet.width, sheet.height), (48, 30));
        assert!(sheet.x < 100, "the list reaches over the score");
        assert_eq!(add_shown(sheet), 27, "27 names at once, not 10");

        let mouse = |app: &mut App, kind, column, row| {
            app.handle_mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
            .expect("the mouse");
        };
        let list = |app: &App| {
            let adding = app.knobs_panel.as_ref()?.adding.as_ref()?;
            Some((adding.selected, adding.scroll))
        };
        // A mouse's notches: three rows each, as the score scrolls.
        for _ in 0..2 {
            mouse(&mut app, MouseEventKind::ScrollDown, sheet.x + 4, 10);
        }
        assert_eq!(list(&app), Some((6, 6)), "the chosen name stays in view");
        // A trackpad's burst runs to the end of the list and stops there.
        for _ in 0..30 {
            mouse(&mut app, MouseEventKind::ScrollDown, sheet.x + 4, 10);
        }
        assert_eq!(list(&app), Some((13, 13)), "40 names, 27 in view");
        mouse(&mut app, MouseEventKind::ScrollUp, sheet.x + 4, 10);
        assert_eq!(list(&app), Some((13, 10)));

        let (_, scroll) = list(&app).expect("still open");
        let first = sheet.y + 2;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            sheet.x + 3,
            first,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            sheet.x + 3,
            first,
        );
        let source = app.scenes.current().editor.source();
        assert!(
            source.contains(&format!("p{scroll}: 0.5")),
            "the name under the press:\n{source}"
        );
        assert!(list(&app).is_none(), "the list closes with the add");
    }

    /// A plugin's column is its main page, in its own order: the ten
    /// suggestions, the ones the score holds on and the rest off, the
    /// score's own order no matter.
    #[cfg(feature = "vst")]
    #[test]
    fn a_plugin_column_follows_the_plugin_order() {
        let directory = tempfile::tempdir().expect("tempdir");
        let word = |key: &str, text: &str| {
            let mut param = plugin_param(key, 0);
            param.default_text = text.into();
            param
        };
        let mut params: Vec<ParamInfo> = ["depth", "time", "ingain", "outgain"]
            .into_iter()
            .map(|key| word(key, "100 %"))
            .collect();
        params.push(word("cleanxov", "OFF"));
        params.extend(
            ["threshl", "threshm", "threshh", "gainl", "gainm", "gainh"]
                .into_iter()
                .map(|key| word(key, "0.0 dB")),
        );
        params.push(word("bypupwdl", "--"));
        let app = plugin_app(
            directory.path(),
            "$: s(\"bd\").vst(\"OTT\", { threshl: 0.857, outgain: 0.714, depth: 0.476 })\n",
            params,
        );
        let rows: Vec<(String, bool)> = app
            .knobs_rows
            .as_ref()
            .expect("the rows")
            .knobs
            .iter()
            .map(|knob| (knob.label.clone(), knob.state.is_some()))
            .collect();
        let on = |key: &str| (key.to_owned(), true);
        let off = |key: &str| (key.to_owned(), false);
        assert_eq!(
            rows,
            [
                on("depth"),
                off("time"),
                off("ingain"),
                on("outgain"),
                on("threshl"),
                off("threshm"),
                off("threshh"),
                off("gainl"),
                off("gainm"),
                off("gainh"),
            ]
        );
    }

    /// A plugin parameter with steps of a third writes and shows three
    /// decimals, not every digit of the float.
    #[cfg(feature = "vst")]
    #[test]
    fn a_stepped_plugin_value_reads_short() {
        use super::super::tests::support::press;
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = plugin_app(
            directory.path(),
            "$: s(\"bd\").vst(\"FX\", { mode: 0 })\n",
            vec![plugin_param("mode", 3)],
        );
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        let source = app.scenes.current().editor.source();
        assert!(source.contains("mode: 0.333 "), "{source}");
        assert_eq!(app.status, "knobs: mode 0.333 · step 0.333");
    }

    /// A parameter added from the list is the rail under the keys at once,
    /// and the plugin's list keeps the suggestions the column showed.
    #[cfg(feature = "vst")]
    #[test]
    fn an_added_parameter_takes_the_keys() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        app.scenes.current_mut().editor =
            Editor::new("$: s(\"bd\").vst(\"FX\", { })\n").expect("score");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(22)));
        app.open_knobs_panel(true);
        app.refresh_knobs(Rect::default());
        let param = |key: &str, steps: u32| ParamInfo {
            id: 0,
            name: key.into(),
            key: key.into(),
            group: String::new(),
            units: String::new(),
            default: 0.5,
            steps,
            default_text: String::new(),
            role: ParamRole::Plain,
        };
        // Two continuous suggestions, and a switch that comes after them.
        if let Some(parse) = app.knobs_parse.as_mut() {
            parse.params = Some(vec![param("drive", 0), param("tone", 0), param("mode", 3)]);
        }
        app.refresh_knobs(Rect::default());
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        for character in "mode".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        let source = app.scenes.current().editor.source();
        // Added at its default, 0.5, and moved off it by the arrow.
        assert!(
            source.contains("{ mode: ") && !source.contains("mode: 0.5 "),
            "→ moved the added rail:\n{source}"
        );
        let chosen = app
            .prefs
            .vst_knobs
            .as_ref()
            .and_then(|knobs| knobs.get("FX"))
            .cloned();
        assert_eq!(
            chosen,
            Some(vec![
                "drive".to_owned(),
                "tone".to_owned(),
                "mode".to_owned()
            ])
        );
    }

    /// A rail keeps its row while the hand works the call: an entry that
    /// moves in the object, or a suggestion the score takes in at the
    /// front, does not push the rails around it. A new rail lands after
    /// the one it follows.
    #[test]
    fn rails_keep_their_rows() {
        let rail = |label: &str| {
            make_knob(
                label.into(),
                KnobPlace::Entry(label.into()),
                KnobState {
                    literal: 0..1,
                    fader: None,
                    value: 0.5,
                },
            )
        };
        let order = ["drive", "tone", "mix"].map(|key| KnobPlace::Entry(key.into()));
        let labels = |rows: Vec<Knob>| rows.into_iter().map(|knob| knob.label).collect::<Vec<_>>();
        let moved = vec![rail("mix"), rail("drive"), rail("tone")];
        assert_eq!(labels(steady(moved, &order)), ["drive", "tone", "mix"]);
        let grown = vec![rail("mix"), rail("gain"), rail("drive"), rail("tone")];
        assert_eq!(
            labels(steady(grown, &order)),
            ["drive", "tone", "mix", "gain"]
        );
    }

    /// A hand edit of the score, the way typing lands.
    fn type_over(app: &mut App, from: &str, to: &str) {
        let source = app.scenes.current().editor.source();
        let at = source.find(from).expect("the text to replace");
        let moment = app.moment();
        app.scenes
            .current_mut()
            .editor
            .apply_transaction(
                Transaction::new(EditOrigin::Typing).replace(Edit::new(
                    ByteOffset(at)..ByteOffset(at + from.len()),
                    to.to_owned(),
                )),
                moment,
            )
            .expect("the edit applies");
    }

    /// A plugin parameter with steps of its own travels them: the rail
    /// takes the plugin's step, not the thousandth a plain parameter
    /// falls back to.
    #[cfg(feature = "vst")]
    #[test]
    fn a_stepped_plugin_parameter_takes_its_own_step() {
        let mut param = ParamInfo {
            id: 3,
            name: "Mix".into(),
            key: "mix".into(),
            group: String::new(),
            units: String::new(),
            default: 0.5,
            steps: 100,
            default_text: "50 %".into(),
            role: Default::default(),
        };
        assert_eq!(ParamDefaults::of(&param).step, 0.01);
        assert_eq!(ParamDefaults::of(&param).default, 0.5);
        // A switch has two stops, and a parameter with no steps of its
        // own keeps the thousandth.
        param.steps = 1;
        assert_eq!(ParamDefaults::of(&param).step, 1.0);
        param.steps = 0;
        assert_eq!(ParamDefaults::of(&param).step, 0.001);
    }

    #[test]
    fn an_entry_parts_at_its_own_colon() {
        let code = "cutoff: { a: 1 } ? 2 : 3";
        let (key, value) = entry_parts(code, 0..code.len()).expect("one entry");
        assert_eq!(&code[key], "cutoff");
        assert_eq!(code[value].trim(), "{ a: 1 } ? 2 : 3");
    }

    #[test]
    fn a_number_literal_is_only_a_number() {
        let code = "c4 0.5 1e3 -12 +7";
        assert_eq!(number_literal(code, 0..2), None, "a note is not a number");
        let (range, value) = number_literal(code, 3..6).expect("a number");
        assert_eq!(&code[range], "0.5");
        assert_eq!(value, 0.5);
        let (range, _) = number_literal(code, 7..10).expect("an exponent");
        assert_eq!(&code[range], "1e3");
        let (range, value) = number_literal(code, 11..15).expect("a negative number");
        assert_eq!(&code[range], "-12");
        assert_eq!(value, -12.0);
    }

    /// An entry put back into a call that changed while it was off comes
    /// back as what it was: a fader as a fader, a quoted key quoted.
    #[test]
    fn an_entry_comes_back_as_it_was_into_a_changed_call() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        for (score, edited, expected) in [
            (
                "$: s(\"bd\").set({ cutoff: slider(200, 100, 2000) })\n",
                "$: s(\"bd\").set({ q: 1 })\n",
                "cutoff: slider(200, 100, 2000",
            ),
            (
                "$: s(\"bd\").set({ 'my key': 1 })\n",
                "$: s(\"bd\").set({ q: 1 })\n",
                "'my key': 1",
            ),
        ] {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new(score).expect("score");
            let _ = app
                .scenes
                .current_mut()
                .editor
                .set_selection(Selection::caret(ByteOffset(20)));
            app.open_knobs_panel(true);
            press(&mut app, KeyCode::Home, KeyModifiers::NONE);
            // A hand edit inside the call while the entry is off.
            type_over(&mut app, "{ }", "{ q: 1 }");
            assert_eq!(app.scenes.current().editor.source(), edited);
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            let source = app.scenes.current().editor.source();
            assert!(source.contains(expected), "{source}");
            assert!(source.contains("q: 1"), "{source}");
        }
    }

    /// An object between the call's own arguments holds its rails the
    /// same way: an entry taken out goes back where it came from.
    #[test]
    fn an_object_between_arguments_takes_entries_out_and_back() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        app.scenes.current_mut().editor =
            Editor::new("$: s(\"bd\").fx({ cutoff: 800 }, 440)\n").expect("score");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(27)));
        app.open_knobs_panel(true);

        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        let source = app.scenes.current().editor.source();
        assert_eq!(source, "$: s(\"bd\").fx({ }, 440)\n");
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        let source = app.scenes.current().editor.source();
        assert_eq!(source, "$: s(\"bd\").fx({ cutoff: 800 }, 440)\n");
    }

    /// The reference's own pages give each documented key its bounds and
    /// default: a count in whole steps, a switch, a span the text names,
    /// a list of numbered choices, a MIDI note, and the short names the
    /// engine reads in any case. A painter's key is read the terminal's
    /// way where the terminal differs, and a key it ignores is no rail. A
    /// colour, a font, a node name and an FX stage index are no rails, and
    /// an object that holds no options is no list.
    #[test]
    fn documented_options_read_the_reference() {
        let reference = crate::reference::Reference::load_all();
        let options = |name: &str| {
            reference
                .lookup_in(name, crate::reference::ReferenceScope::Pattern)
                .and_then(|index| reference.entry(index))
                .map(documented_options)
                .unwrap_or_default()
        };
        let find = |options: &[DocOption], key: &str| {
            options
                .iter()
                .find(|option| option.key == key)
                .cloned()
                .unwrap_or_else(|| panic!("{key} is documented"))
        };
        let keys = |options: &[DocOption]| -> Vec<String> {
            options.iter().map(|option| option.key.clone()).collect()
        };
        let roll = options("_pianoroll");
        assert_eq!(
            keys(&roll)[..4],
            ["cycles", "playhead", "vertical", "labels"]
        );
        for missing in [
            "active",
            "playheadColor",
            "fontFamily",
            "overscan",
            "smear",
            "fill",
        ] {
            assert!(!keys(&roll).iter().any(|key| key == missing), "{missing}");
        }
        let cycles = find(&roll, "cycles");
        assert_eq!(
            (cycles.range, cycles.default),
            ((1.0, 10.0, 1.0), Some(4.0))
        );
        let playhead = find(&roll, "playhead");
        assert_eq!(
            (playhead.range, playhead.default),
            ((0.0, 1.0, 0.01), Some(0.5))
        );
        let colorize = find(&roll, "colorizeInactive");
        assert_eq!(
            (colorize.range, colorize.default),
            ((0.0, 1.0, 1.0), Some(1.0))
        );
        assert_eq!(find(&roll, "fold").default, Some(1.0), "the terminal folds");
        assert_eq!(find(&roll, "maxMidi").range, (0.0, 127.0, 1.0));
        assert!(find(&roll, "flipTime").names("fliptime"));
        let lfo = options("lfo");
        assert_eq!(
            keys(&lfo),
            [
                "rate", "sync", "depth", "depthabs", "dcoffset", "shape", "skew", "curve", "retrig"
            ]
        );
        let rate = find(&lfo, "rate");
        assert_eq!((rate.range, rate.default), ((0.0, 5.0, 0.01), Some(1.0)));
        assert!(rate.names("r") && rate.names("R") && !rate.names("rt"));
        assert_eq!(find(&lfo, "shape").range, (0.0, 4.0, 1.0));
        assert_eq!(find(&lfo, "dcoffset").range, (-1.0, 0.0, 0.01));
        let sync = find(&lfo, "sync");
        assert_eq!((sync.default, sync.start()), (None, 0.5), "no rate of 0");
        let env = options("env");
        assert_eq!(find(&env, "acurve").range, (-1.0, 1.0, 0.01));
        assert_eq!(find(&env, "attack").aliases, ["att", "a"]);
        assert_eq!(keys(&options("bmod")), ["depth", "depthabs", "dc"]);
        assert_eq!(find(&options("bmod"), "depth").default, Some(1.0));
        let scope = options("scope");
        let pos = find(&scope, "pos");
        assert_eq!((pos.range, pos.default), ((0.0, 1.0, 0.01), Some(0.5)));
        assert_eq!(find(&scope, "scale").range, (0.01, 8.0, 0.01));
        let spiral = options("spiral");
        assert_eq!(find(&spiral, "inset").range, (0.5, 12.0, 0.1));
        assert_eq!(find(&spiral, "steady").default, Some(1.0));
        for missing in ["activeColor", "size", "thickness"] {
            assert!(!keys(&spiral).iter().any(|key| key == missing), "{missing}");
        }
        let max = find(&options("spectrum"), "max");
        assert_eq!((max.range, max.default), ((-120.0, 0.0, 1.0), Some(0.0)));
        for plain in ["lpf", "set", "getFrequencyFromValue", "vst"] {
            assert!(options(plain).is_empty(), "{plain} has no options to list");
        }
    }

    /// A documented call shows its keys as a column of rails in the
    /// reference's order: the ones the score holds on, the rest at their
    /// off stop, each coming on at the value the call already takes. A key
    /// with no default waits in the add list. A key the score writes by its
    /// alias or in another case is that key, by the reference's name, and a
    /// key it holds as a pattern is neither a rail nor a suggestion.
    #[test]
    fn a_documented_call_shows_its_keys_in_order() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        let at = |app: &mut App, score: &str| {
            app.scenes.current_mut().editor = Editor::new(score).expect("score");
            let caret = score.find('{').expect("an object") + 1;
            let _ = app
                .scenes
                .current_mut()
                .editor
                .set_selection(Selection::caret(ByteOffset(caret)));
            app.knobs_parse = None;
            app.refresh_knobs(Rect::default());
        };
        let rows = |app: &App| {
            app.knobs_rows
                .as_ref()
                .expect("rows")
                .knobs
                .iter()
                .map(|knob| {
                    (
                        knob.label.clone(),
                        knob.state.as_ref().map(|state| state.value),
                    )
                })
                .collect::<Vec<_>>()
        };
        let labels = |app: &App| {
            rows(app)
                .into_iter()
                .map(|(label, _)| label)
                .collect::<Vec<_>>()
        };
        app.open_knobs_panel(true);
        at(
            &mut app,
            "$: note(\"c3\")._pianoroll({ fold: 1, cycles: 8 })\n",
        );
        assert_eq!(
            labels(&app),
            [
                "cycles",
                "playhead",
                "vertical",
                "labels",
                "flipTime",
                "flipValues",
                "fold",
                "hideInactive",
                "colorizeInactive",
                "minMidi"
            ]
        );
        let shown = rows(&app);
        assert_eq!(
            (shown[0].1, shown[1].1, shown[6].1),
            (Some(8.0), None, Some(1.0))
        );
        assert!(app.knobs_rows.as_ref().is_some_and(|rows| rows.listed));
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            app.scenes.current().editor.source(),
            "$: note(\"c3\")._pianoroll({ playhead: 0.5, fold: 1, cycles: 8 })\n"
        );
        assert_eq!(rows(&app)[1], ("playhead".to_owned(), Some(0.5)));
        at(&mut app, "$: note(\"c3\")._pianoroll({ fliptime: 1 })\n");
        let flip: Vec<_> = rows(&app)
            .into_iter()
            .filter(|(label, _)| label == "flipTime")
            .collect();
        assert_eq!(flip, [("flipTime".to_owned(), Some(1.0))]);
        at(&mut app, "$: s(\"saw\").lpf(400).lfo({ r: 2 })\n");
        assert_eq!(
            labels(&app),
            [
                "rate", "depth", "dcoffset", "shape", "skew", "curve", "retrig"
            ]
        );
        assert_eq!(rows(&app)[0].1, Some(2.0));
        at(&mut app, "$: s(\"saw\").lpf(400).env({ a: 0.2 })\n");
        let attack: Vec<_> = rows(&app)
            .into_iter()
            .filter(|(label, _)| label == "attack")
            .collect();
        assert_eq!(attack, [("attack".to_owned(), Some(0.2))]);
        at(&mut app, "$: s(\"saw\").lpf(400).env({ a: \"0.1 0.2\" })\n");
        assert!(!labels(&app).iter().any(|label| label == "attack"));
    }

    /// `a` on a documented call lists its keys, and a call with nothing in
    /// it takes the object: the key goes in at its default, under the
    /// keys. A key the score holds already, as a pattern too, is left as
    /// it is. Any other built-in call has no list.
    #[test]
    fn a_documented_key_adds_into_a_bare_call() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        let at = |app: &mut App, score: &str, caret: usize| {
            app.scenes.current_mut().editor = Editor::new(score).expect("score");
            let _ = app
                .scenes
                .current_mut()
                .editor
                .set_selection(Selection::caret(ByteOffset(caret)));
            app.knobs_parse = None;
        };
        let add = |app: &mut App, name: &str| {
            press(app, KeyCode::Char('a'), KeyModifiers::NONE);
            for key in name.chars() {
                press(app, KeyCode::Char(key), KeyModifiers::NONE);
            }
            press(app, KeyCode::Enter, KeyModifiers::NONE);
        };
        app.open_knobs_panel(true);
        let score = "$: note(\"c3\")._pianoroll()\n";
        at(&mut app, score, score.find("()").expect("the call") + 1);
        add(&mut app, "maxmidi");
        assert_eq!(
            app.scenes.current().editor.source(),
            "$: note(\"c3\")._pianoroll({ maxMidi: 90 })\n"
        );
        let selected = app.selected_knob().expect("a rail under the keys");
        assert_eq!(selected.label, "maxMidi");
        assert!(selected.state.is_some());
        let score = "$: s(\"saw\").lpf(400).env({ a: \"0.1 0.2\" })\n";
        at(&mut app, score, score.find('{').expect("an object") + 1);
        add(&mut app, "attack");
        assert_eq!(app.scenes.current().editor.source(), score);
        assert!(app.status.contains("already"), "{}", app.status);
        let score = "$: s(\"bd\").set({ cutoff: 800 })\n";
        at(&mut app, score, score.find('{').expect("an object"));
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(
            app.knobs_panel
                .as_ref()
                .is_some_and(|panel| panel.adding.is_none())
        );
        assert!(app.status.contains("set has no list"), "{}", app.status);
    }

    /// A rail the hand took off before the plugin's list arrived takes its
    /// row in the plugin's order with the rest, not the row it had in the
    /// score's.
    #[cfg(feature = "vst")]
    #[test]
    fn a_rail_held_off_follows_the_plugin_order_when_it_arrives() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        let score = "$: s(\"bd\").vst(\"Late FX\", { mix: 0.2, drive: 0.4, tone: 0.6 })\n";
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let caret = score.find('{').expect("an object") + 1;
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(caret)));
        app.open_knobs_panel(true);
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        if let Some(parse) = app.knobs_parse.as_mut() {
            parse.params = Some(vec![
                plugin_param("drive", 0),
                plugin_param("tone", 0),
                plugin_param("mix", 0),
            ]);
        }
        app.refresh_knobs(Rect::default());
        let rows: Vec<(String, bool)> = app
            .knobs_rows
            .as_ref()
            .expect("rows")
            .knobs
            .iter()
            .map(|knob| (knob.label.clone(), knob.state.is_some()))
            .collect();
        assert_eq!(
            rows,
            [
                ("drive".to_owned(), true),
                ("tone".to_owned(), true),
                ("mix".to_owned(), false)
            ]
        );
    }

    #[test]
    fn the_parse_names_the_preset_the_call_holds() {
        let source = "$: s(\"bd\").vst(\"FX\", { preset: \"Dark Bass\", gain: 1 })";
        let caret = source.find("gain").expect("the entry");
        let parse = parse_call(source, caret).expect("the caret is in the call");
        assert_eq!(parse.preset.as_deref(), Some("Dark Bass"));
        let source = "$: s(\"bd\").vst(\"FX\", { gain: 1 })";
        let caret = source.find("gain").expect("the entry");
        let parse = parse_call(source, caret).expect("the caret is in the call");
        assert_eq!(parse.preset, None);
    }

    /// The presets of a plugin call sit under the rails as a section of
    /// their own, closed until the hand opens it: → opens the heading, ↓
    /// walks the names, Enter takes a name into the call - a preset the
    /// call holds gives way to it - and ← closes the section.
    #[cfg(feature = "vst")]
    #[test]
    fn the_presets_of_a_plugin_call_write_the_score() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        let score = "$: s(\"bd\").vst(\"FX\", { gain: 1 })\n";
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(score.len() - 3)));
        app.open_knobs_panel(true);
        app.refresh_knobs(Rect::default());
        if let Some(panel) = app.knobs_panel.as_mut() {
            panel.presets = Some(KnobPresets {
                names: vec!["Dark Bass".into(), "Lead".into()],
                version: 0,
                open: false,
            });
        }
        let knobs = app.knobs_rows.as_ref().expect("rows").knobs.len();
        assert_eq!(
            app.knobs_row_count(),
            knobs + 1,
            "the heading alone while the section is closed"
        );
        if let Some(panel) = app.knobs_panel.as_mut() {
            panel.selected = knobs;
        }
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            app.knobs_row_count(),
            knobs + 3,
            "the names under the heading"
        );
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            app.editor().source().contains("preset: \"Dark Bass\""),
            "the pick wrote the preset:\n{}",
            app.editor().source()
        );
        // The second pick replaces the first: one preset member, with the
        // name it picked.
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let source = app.editor().source();
        assert!(
            source.contains("preset: \"Lead\""),
            "the second pick replaced the first:\n{source}"
        );
        assert!(
            !source.contains("Dark Bass"),
            "one preset member, the one picked:\n{source}"
        );
        assert_eq!(
            source.matches("preset:").count(),
            1,
            "no second spelling of the preset:\n{source}"
        );
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        let panel = app.knobs_panel.as_ref().expect("the panel");
        assert!(!panel.presets.as_ref().expect("the section").open);
        assert_eq!(panel.selected, knobs, "the hand stands on the heading");
        assert_eq!(app.knobs_row_count(), knobs + 1);
    }

    /// The value form of a rail walks its fields with the same keys as the
    /// smart action's form: ↓ and Tab forward, ↑ and Shift+Tab back, and
    /// ← and → stay with the field's own caret.
    #[test]
    fn the_knobs_form_walks_its_fields_with_the_arrows() {
        use super::super::tests::support::{app_over, press};
        use crate::editor::{ByteOffset, Selection};
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = app_over(directory.path());
        let score = "$: note(\"c3\").lpf(800)";
        app.scenes.current_mut().editor = Editor::new(score).expect("score");
        let _ = app
            .scenes
            .current_mut()
            .editor
            .set_selection(Selection::caret(ByteOffset(score.len() - 1)));
        app.open_knobs_panel(true);
        app.refresh_knobs(Rect::default());
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        let field = |app: &App| {
            app.knobs_panel
                .as_ref()
                .and_then(|panel| panel.editing.as_ref())
                .map(|editing| editing.form.field)
        };
        assert_eq!(field(&app), Some(0), "the form opens on the value");

        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(field(&app), Some(1), "↓ walks to the next field");
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(field(&app), Some(0), "↑ walks back");
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(field(&app), Some(1), "Tab walks forward");
        press(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(field(&app), Some(0), "Shift+Tab walks back");
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(field(&app), Some(3), "↑ wraps around to the last field");
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            field(&app),
            Some(3),
            "→ is the field's own caret, not a field walk"
        );
    }
}
