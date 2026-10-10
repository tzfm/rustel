//! Checking a score before it plays.
//!
//! Two levels of mistake end a set. A *syntax* error - a missing bracket, a
//! mini-notation string that does not parse - is refused by the engine, so
//! at least the last good score keeps playing. A *value* error - a sample
//! that does not exist, a scale or chord with a typo, `z1` for a note - is
//! accepted and plays silence, which on stage is worse: nothing says what
//! went wrong. This module finds both before an update, so the studio can
//! refuse the update and a watched set can refuse the save, exactly as they
//! already refuse a syntax error.
//!
//! Everything here is pure Rust and runs the same code the engine runs: the
//! transpiler, the mini-notation parser, `tonaljs::get_scale`, the voicing
//! dictionaries and the sample library's own knowledge of its banks. It
//! never evaluates the score, so it is safe from any thread and has nothing
//! to contend for - with one exception: the set of known function names is
//! built once, lazily, by booting a throwaway [`rustel_jsruntime::JsRuntime`]
//! and reading its globals (a heavier, permanently cached first call). What
//! it cannot know it does not guess: a score that
//! registers its own samples or voicings is checked only for what is still
//! certain.
//!
//! Setup files are checked with the same rules plus their own. What a setup
//! defined is accepted in the scores that follow it, so a helper library is
//! not eight unknown functions. What a setup may not do (set tempo, name a
//! MIDI input, open visuals) is refused in it, in the engine's own words.
//! And a top-level declaration there is a note: it reads like a definition
//! and reaches no score.

use rustel_transpiler::{TranspileOptions, transpile};

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::samples::SampleLibrary;

mod input_channels;
pub mod scan;

use scan::{matching_pair, matching_paren};

/// Diagnostics reported per check; a score with more problems than this
/// has one problem, and it is the first one.
const MAX_DIAGNOSTICS: usize = 8;

/// Which kind of mistake a finding is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Level {
    /// The score cannot be evaluated at all.
    Syntax,
    /// The score evaluates, and names something that does not exist.
    Value,
    /// Nothing is wrong: the score plays. This is something worth knowing
    /// about it. A note never refuses a score.
    Note,
}

/// One problem, at a byte range of the checked text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub level: Level,
    pub message: String,
    pub from: usize,
    pub to: usize,
}

/// What the checked text is.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LintMode {
    /// A score: it plays, and may set tempo, name an input, open visuals.
    #[default]
    Score,
    /// A setup file: it defines things for the scores that follow it.
    Setup,
}

/// What the checker may take for granted beyond a fresh engine.
///
/// The engine's own name list is built from a runtime that has run nothing,
/// so a score calling a helper a prebake defined is an unknown function
/// unless the names come with it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LintContext {
    pub mode: LintMode,
    /// Names the setup that ran before this text defined.
    pub setup_names: Vec<String>,
    /// That setup defined names this could not read. Nothing is then judged
    /// unknown, for the same reason a dynamic `register()` stops the check:
    /// there is no sound basis for refusing a call.
    pub setup_has_dynamic_names: bool,
    /// Verified channels on the active audio input device. Without a verified
    /// device, native input references still obey the engine's channel limit.
    pub input_channels: Option<usize>,
    /// The MIDI ports this machine last offered, so a score that names one
    /// that is not there can be told before it is evaluated in front of an
    /// audience. `None` is an unprobed machine, which judges nothing.
    pub midi_inputs: Option<Vec<String>>,
    pub midi_outputs: Option<Vec<String>>,
}

impl LintContext {
    /// The context a text creates for itself and for whatever follows it.
    pub fn from_setup_sources<'a>(sources: impl IntoIterator<Item = &'a str>) -> Self {
        let mut context = Self::default();
        for source in sources {
            let definitions = rustel_transpiler::setup_definitions(source);
            context.setup_names.extend(definitions.names);
            context.setup_has_dynamic_names |= definitions.has_dynamic_names;
        }
        context
    }

    /// The same names, checking a setup file rather than a score.
    pub fn as_setup(mut self) -> Self {
        self.mode = LintMode::Setup;
        self
    }
}

/// Check one score. `library` lets sample and bank names be checked against
/// what the engine can actually play; without it, or while its manifests
/// are still arriving, sound names are not judged.
pub fn lint(source: &str, mini: bool, library: Option<&SampleLibrary>) -> Vec<Diagnostic> {
    lint_with(source, mini, library, &LintContext::default())
}

/// Report sample imports that the library could not read.
pub fn failed_samples_imports<'a>(
    imports: &'a [crate::sounds::SamplesImport],
    library: &'a SampleLibrary,
) -> impl Iterator<Item = Diagnostic> + 'a {
    imports.iter().filter_map(|import| {
        match library.samples_source_state(import.spec.as_deref()?)? {
            crate::samples::SourceState::Failed(reason) => Some(Diagnostic {
                level: Level::Value,
                message: format!("samples: {reason}"),
                from: import.from,
                to: import.to,
            }),
            _ => None,
        }
    })
}

/// Check one text against what a setup already defined, and - in
/// [`LintMode::Setup`] - against what a setup may not do.
///
/// A lint that panics finds nothing: it runs on every edit, in the studio's
/// interface thread too, and evaluation still judges the score.
pub fn lint_with(
    source: &str,
    mini: bool,
    library: Option<&SampleLibrary>,
    context: &LintContext,
) -> Vec<Diagnostic> {
    crate::catch_score_panic(|| lint_with_scan(source, mini, library, context)).unwrap_or_default()
}

fn lint_with_scan(
    source: &str,
    mini: bool,
    library: Option<&SampleLibrary>,
    context: &LintContext,
) -> Vec<Diagnostic> {
    // Setup is JavaScript by definition, never whole-buffer mini-notation.
    let mini = mini && context.mode == LintMode::Score;
    let mut diagnostics = Vec::new();
    // Quiet guidance is appended after all refusal checks, so it cannot
    // consume the diagnostic budget before a real error is encountered.
    let mut notes = Vec::new();
    if mini {
        if let Err(error) = rustel_mini::mini(source) {
            let from = clamp_boundary(source, error.offset);
            diagnostics.push(Diagnostic {
                level: Level::Syntax,
                message: error.to_string(),
                from,
                to: next_boundary(source, from),
            });
        }
        return diagnostics;
    }
    let output = transpile(
        source,
        &TranspileOptions {
            add_return: false,
            ..TranspileOptions::default()
        },
    );
    // The parser reports an unterminated string at a byte inside it, not at
    // the quote and not at the end, so a mark placed there stays on one
    // letter while the string grows. The scanner below knows the string:
    // the mark runs from the opening quote to where the string stops, and
    // follows the typing.
    let mut open_strings = string_literals(source)
        .into_iter()
        .filter(|literal| literal.open)
        .map(|literal| literal.content);
    for diagnostic in &output.diagnostics {
        let mut from = clamp_boundary(source, diagnostic.offset.unwrap_or(0));
        let mut to = next_boundary(source, from);
        if diagnostic.message.starts_with("Unterminated string")
            && let Some(content) = open_strings.next()
        {
            from = content.start.saturating_sub(1);
            to = content.end.max(next_boundary(source, from));
        }
        diagnostics.push(Diagnostic {
            level: Level::Syntax,
            message: diagnostic.message.clone(),
            from,
            to,
        });
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            return diagnostics;
        }
    }

    // A slider whose numbers cannot be a control is refused here, where the
    // score is checked, rather than silently dropped when the layout is
    // built. Only an all-literal slider is judged: one with an expression in
    // it keeps its runtime semantics and is simply never a native control.
    for widget in &output.widgets {
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            return diagnostics;
        }
        if widget.widget_type != "slider" {
            continue;
        }
        let Some(value) = widget
            .value
            .as_deref()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        let (Some(min), Some(max)) = (widget.min, widget.max) else {
            continue;
        };
        let fault = if min >= max {
            Some(format!("min {min} is not below max {max}"))
        } else if value < min || value > max {
            Some(format!("value {value} is outside {min} to {max}"))
        } else {
            widget
                .step
                .filter(|step| *step <= 0.0)
                .map(|step| format!("step {step} is not above zero"))
        };
        if let Some(fault) = fault {
            let from = clamp_boundary(source, widget.from);
            diagnostics.push(Diagnostic {
                level: Level::Value,
                message: format!(
                    "this slider cannot be a control: {fault} - the order is \
                     slider(value, min?, max?, step?)"
                ),
                from,
                to: widget.to.max(next_boundary(source, from)),
            });
        }
    }

    // The lint cannot know what the score changes at evaluation time: its
    // voicings, its dictionary choice, and its samples until they arrive.
    // After the library registers a `samples("...")` source, the score is
    // held to the names it brings, like any other. An inline map's keys are
    // the names it brings, read off the text. A map that has not arrived,
    // or one that cannot be read here (a variable, a map still being
    // typed), leaves sound names alone.
    let imports = crate::sounds::samples_imports(source);
    let map_names = imports
        .iter()
        .filter_map(|import| import.keys.as_deref())
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let registers_samples = imports.iter().any(|import| match &import.spec {
        Some(spec) => !library.is_some_and(|library| library.knows_samples_source(spec)),
        None => import.keys.is_none(),
    });
    // The names a failed import would have brought are left alone, as under
    // a map still on its way.
    if let Some(library) = library {
        let room = MAX_DIAGNOSTICS.saturating_sub(diagnostics.len());
        diagnostics.extend(failed_samples_imports(&imports, library).take(room));
    }
    ignored_midi_options(source, &mut diagnostics);
    #[cfg(feature = "vst")]
    plugin_key_problems(source, &mut diagnostics);
    let registers_voicings = source.contains("addVoicings(");
    let picks_dictionary = source.contains(".dict(") || source.contains("setDefaultVoicings(");
    // Sound names are judged only by a library that has its banks; an empty
    // one, or one still fetching manifests, knows too little to say no.
    let sounds = library.filter(|library| library.manifests_pending() == 0 && library.has_banks());

    // Every string the transpiler turned into a pattern - it rewrites them
    // as `m('…', offset)`, offset being the opening quote - is parsed the
    // way the engine will parse it. The transpiler's own per-word locations
    // only exist for strings that parsed, so they cannot be the signal.
    let mini_quotes = mini_string_offsets(&output.output);
    let code = code_only(source);
    for literal in string_literals(source) {
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            break;
        }
        let is_pattern = mini_quotes
            .iter()
            .any(|&quote| quote + 1 == literal.content.start)
            || output
                .mini_locations
                .iter()
                .any(|&(from, _)| from >= literal.content.start && from <= literal.content.end);
        // A single-quoted argument stays a plain JavaScript string rather
        // than `m(...)`, but `s('bd')` still names a sound and
        // `chord('am7')` a chord, so their names are judged too.
        let raw_named_argument = source.as_bytes()[literal.content.start - 1] == b'\''
            && matches!(
                callee_before(source, literal.content.start - 1)
                    .or_else(|| callee_after(source, literal.content.end + 1)),
                Some("s" | "sound" | "chord" | "voicing")
            );
        // A MIDI port is named in a plain single-quoted string, which is
        // never a pattern, so this check has to happen before the pattern
        // gate below or it would never run at all.
        check_midi_port(source, &literal, context, &mut diagnostics);
        if !is_pattern && !raw_named_argument {
            continue;
        }
        let content = &source[literal.content.clone()];
        if let Err(error) = rustel_mini::mini(content) {
            // Underline from the point the parser gave up to the end of the
            // string. An unclosed bracket fails at the very end, where a
            // one-character mark would sit on the closing quote.
            let mut from = clamp_boundary(source, literal.content.start + error.offset);
            if from >= literal.content.end || error.phase != rustel_mini::ErrorPhase::Parse {
                from = literal.content.start;
            }
            let to = literal.content.end.max(next_boundary(source, from));
            diagnostics.push(Diagnostic {
                level: Level::Syntax,
                message: error.to_string(),
                from,
                to,
            });
            continue;
        }
        // A string that parses can still name something that does not
        // exist. For the calls whose words the engine resolves by name, ask
        // the engine's own resolver, so the finding is the error an update
        // would produce.
        // The enclosing call usually owns the words, as in `s("bd sd")`, and a
        // method chained onto the string usually does not: `s("abcde".fast(2))`
        // still names a sound. The exception is a method that reads the words
        // as selectors into its own arguments, where the enclosing call never
        // sees them: `s("<a b>".pick({a: "bd(3,8)"}))` plays `bd`, so `a` is
        // a key and not a sound.
        let chained = callee_after(source, literal.content.end.saturating_add(1));
        let Some(callee) = literal_role(source, &literal) else {
            continue;
        };
        // The words as the engine builds them from the mini-notation: an
        // atom with every `:tail` hung on it, whether written on the atom
        // (`D:major`) or on a group it sits in (`<A1 D2>/4:minor`).
        let Some(words) = mini_words_of(content) else {
            continue;
        };
        if matches!(callee, "s" | "sound") {
            input_channels::check_literal(
                source,
                &literal,
                &words,
                context,
                &mut diagnostics,
                &mut notes,
            );
        }
        // The machines this string plays through: those of the bank that
        // applies to the pattern it builds, never a sibling's in a `stack`
        // and never one on another `$:`.
        let (banks, banked_opaquely) = literal_banks(source, &code, &literal);
        // A statement routed through `.osc()` is naming SuperDirt's
        // vocabulary, not this engine's: `s` travels verbatim in the
        // `/dirt/play` bundle, so `superzow` and `supersquare` are the point
        // of those examples rather than slips. Our own library is the wrong
        // authority for a name we are only forwarding. Scoped to the
        // statement, and read past comments so a commented-out `.osc()`
        // exempts nothing and a comment line inside the chain ends nothing.
        let routed_to_osc = code[statement_around(&code, literal.content.start)].contains(".osc(");
        let judge = |word: &str| -> Option<(Level, String)> {
            let head = word.split(':').next().unwrap_or(word);
            let numeric = head.parse::<f64>().is_ok();
            let refusal = |message: String| Some((Level::Value, message));
            let found = match callee {
                "scale" if numeric => None,
                // `"<c3 eb3 g3>".scale('C minor')` reads the other way
                // round: the string is the notes and the scale is the
                // argument. Judging its words as scale names reported
                // `c3` as an incomplete scale, which is a complaint about
                // text that is not a scale at all.
                "scale" if chained == Some("scale") => (!numeric
                    && !rustel_core::tonaljs::is_score_note(head))
                .then(|| format!("not a note: \"{head}\"")),
                "scale" => rustel_core::tonaljs::get_scale(word).err(),
                "note" => (!numeric && !rustel_core::tonaljs::is_score_note(head))
                    .then(|| format!("not a note: \"{head}\"")),
                // Native input is checked independently of sample manifests and
                // follows its own non-wrapping, floating-point channel rule.
                "s" | "sound" if head == "in" || numeric || registers_samples || routed_to_osc => {
                    None
                }
                "s" | "sound" => {
                    return sounds.and_then(|library| {
                        // A name the score's own map brings is the score's;
                        // its numbers cannot be counted before it arrives.
                        if map_names.iter().any(|name| name == head) {
                            return None;
                        }
                        // Under a `.bank()` the voice resolves `{bank}_{name}`
                        // and nothing else - a plain spelling is refused even
                        // when some bank's key merely ends with it, and even
                        // when the plain spelling is a bank of its own. The
                        // banked spelling is only askable when the bank names
                        // its machines in literal text: a `.bank(someVar)`
                        // leaves the words to the score, because which machine
                        // it names is not in this text at all.
                        if !banks.is_empty() || banked_opaquely {
                            // A bank this text cannot read (an unknown name or
                            // a `.bank(someVar)`) leaves the words to the
                            // score. An unknown bank is also the only error to
                            // report: every sound under it is unknown for the
                            // same reason, and that list would hide the typo.
                            if banked_opaquely || banks.iter().any(|bank| !library.knows_bank(bank))
                            {
                                return None;
                            }
                            if !library.knows_sound_under_banks(head, &banks) {
                                return refusal(format!("unknown sound \"{head}\""));
                            }
                        } else if !library.knows_sound(head) {
                            return refusal(format!("unknown sound \"{head}\""));
                        }
                        let index = word.split(':').nth(1)?;
                        // `bd:1ssd` is not a sample number, and the engine would
                        // drop every one of its onsets while the rest played on -
                        // a score that has quietly lost a track.
                        let Some(n) = crate::sounds::leading_number(index)
                            .filter(|(_, used)| *used == index.len())
                            .map(|(n, _)| n)
                        else {
                            return refusal(format!(
                                "\"{index}\" is not a sample number - {head}:0, {head}:1 …"
                            ));
                        };
                        // Under `.bank(...)` the sound is the machine's own -
                        // `RolandTR909_bd` - and so is the count. A machine the
                        // library cannot count is not judged.
                        let counted = if banks.is_empty() {
                            vec![(head.to_owned(), library.variants_of(head)?)]
                        } else {
                            banks
                                .iter()
                                .map(|bank| {
                                    let name = format!("{bank}_{head}");
                                    library.variants_of(&name).map(|variants| (name, variants))
                                })
                                .collect::<Option<Vec<_>>>()?
                        };
                        // A number the sound does not have still plays: the
                        // engine wraps it, as strudel.cc does. So it is a
                        // hint that says which sample sounds, not a refusal.
                        plays_instead(word, n, &counted).map(|message| (Level::Note, message))
                    });
                }
                "bank" if registers_samples => None,
                "bank" => sounds
                    .filter(|library| !library.knows_bank(head))
                    .map(|_| format!("unknown bank \"{head}\"")),
                "chord" | "voicing" if registers_voicings => None,
                // `chord("G:7b9")` is not one word to anybody: the colon is
                // mini-notation's, and the pair it makes - `["G", "7b9"]` -
                // voices nothing, on strudel.cc (`tokenizeChord` is handed
                // an array and throws into the caught `logger`) as here
                // (`render_voicing` wants a string and answers silence).
                // So there is nothing to refuse: the score plays, and its
                // chord control is inert. `G7b9`, the spelling that does
                // voice, is judged like any other.
                "chord" | "voicing" if word.contains(':') => None,
                "chord" | "voicing" if picks_dictionary => {
                    (!rustel_core::voicings::chord_symbol_in_any_dictionary(word))
                        .then(|| format!("unknown chord \"{word}\""))
                }
                "chord" | "voicing" => rustel_core::voicings::chord_lookup(word, None).err(),
                _ => None,
            };
            found.and_then(refusal)
        };
        let verdicts = words
            .iter()
            .map(|word| {
                is_plain_atom(&word.text)
                    .then(|| judge(&word.text))
                    .flatten()
            })
            .collect::<Vec<_>>();
        let refusals = verdicts
            .iter()
            .map(|verdict| {
                verdict
                    .as_ref()
                    .filter(|(level, _)| *level != Level::Note)
                    .map(|(_, message)| message.clone())
            })
            .collect::<Vec<_>>();
        push_word_diagnostics(&words, &refusals, literal.content.start, &mut diagnostics);
        for (word, verdict) in words.iter().zip(verdicts) {
            if let Some((Level::Note, message)) = verdict {
                notes.push(Diagnostic {
                    level: Level::Note,
                    message,
                    from: literal.content.start + word.from,
                    to: literal.content.start + word.to,
                });
            }
        }
    }
    // What a setup asks for and cannot have. The engine refuses these even
    // when the call is wrapped in a try/catch, so saying so before it runs
    // costs a set nothing and saves it the puzzle.
    if context.mode == LintMode::Setup {
        for (from, to, message) in refused_setup_calls(source) {
            if diagnostics.len() >= MAX_DIAGNOSTICS {
                break;
            }
            diagnostics.push(Diagnostic {
                level: Level::Value,
                message,
                from,
                to,
            });
        }
    }

    // A call to a name nothing answers to fails the moment the update
    // runs (`lpff is not a function`), so it is a finding now, with the
    // nearest real name beside it.
    if diagnostics.len() < MAX_DIAGNOSTICS {
        let known = known_names();
        let mut registered = HashSet::new();
        // A score that opens a visuals window may call anything Hydra answers
        // to, and those names are not this engine's. Without them a working
        // sketch reads as eight unknown functions.
        //
        // Deliberately keyed on the source mentioning `initHydra` rather than
        // on the feature alone: in a build with no window, `.diff(...)` really
        // is a call nothing answers to, and saying so is the useful answer.
        //
        // Read from the code alone: a score that only mentions `initHydra` in
        // a comment has not called it, and its Hydra names really are unknown.
        #[cfg(feature = "hydra")]
        let hydra_score =
            context.mode == LintMode::Score && code_only(source).contains("initHydra");
        #[cfg(feature = "hydra")]
        if hydra_score {
            for name in rustel_hydra::hydra_names() {
                registered.insert(name.to_owned());
            }
        }
        for name in output
            .registrations
            .names
            .iter()
            .chain(context.setup_names.iter())
        {
            registered.insert(name.to_owned());
            // Public register() can also install a raw `_name` method on
            // Pattern.prototype. The checker does not execute the callback to
            // recover its runtime arity, so retain the valid candidate.
            registered.insert(format!("_{name}"));
        }
        // A computed registration name can legitimately be any later call.
        // The checker never executes score code, so in that case there is no
        // sound basis for an unknown-function refusal.
        let definitions = rustel_transpiler::setup_definitions(source);
        registered.extend(definitions.names);
        let dynamic_names = output.registrations.has_dynamic_names
            || context.setup_has_dynamic_names
            || definitions.has_dynamic_names;
        // Use parser scopes so local bindings can shadow global names.
        let unresolved_reads = if dynamic_names {
            Vec::new()
        } else {
            rustel_transpiler::unresolved_reads(source)
        };
        // A KabelSalat call is refused whole, so no name in its graph is
        // judged here, the patterns lifted from it included.
        let in_graph = |at: usize| {
            let calls = &output.kabelsalat_calls;
            let after = calls.partition_point(|call| call.stringified.start <= at);
            after > 0 && calls[after - 1].stringified.contains(&at)
        };
        let unknown = if dynamic_names {
            Vec::new()
        } else {
            unknown_calls(source, known, &registered)
                .into_iter()
                .filter(|&(from, _)| !in_graph(from))
                .collect()
        };
        // Each KabelSalat call outside another's graph is refused: Rustel does
        // not run KabelSalat.
        for call in &output.kabelsalat_calls {
            diagnostics.push(Diagnostic {
                level: Level::Value,
                message: "`K(…)` needs kabelsalat, strudel.cc's custom-DSP language, which this engine does not implement".to_owned(),
                from: call.name.start,
                to: call.name.end,
            });
        }
        // Resolve non-call references too: a stray word is valid JavaScript,
        // but reading an undefined global fails just as surely as calling one.
        // The parser knows scopes, destructuring and property keys; a token
        // scan would mistake those for undefined names.
        if !dynamic_names {
            for (name, span) in &unresolved_reads {
                if diagnostics.len() >= MAX_DIAGNOSTICS {
                    break;
                }
                if known.contains(name.as_str())
                    || registered.contains(name.as_str())
                    || JAVASCRIPT_GLOBALS.contains(&name.as_str())
                    || unknown.contains(&(span.start, span.end))
                    || in_graph(span.start)
                    || diagnostics
                        .iter()
                        .any(|d| d.from == span.start && d.to == span.end)
                {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    level: Level::Value,
                    message: format!("unknown name `{name}`"),
                    from: span.start,
                    to: span.end,
                });
            }
        }
        for (from, to) in unknown {
            let name = &source[from..to];
            let message = hydra_without_init(name, {
                #[cfg(feature = "hydra")]
                {
                    hydra_score
                }
                #[cfg(not(feature = "hydra"))]
                {
                    false
                }
            })
            .unwrap_or_else(|| {
                if scan::dot_before(source, from).is_none() && pattern_method_only_names().contains(name) {
                    format!("`{name}` is a pattern method - write `.{name}()`, not `{name}(…)`")
                } else if scan::dot_before(source, from).is_some() && global_only_names().contains(name) {
                    format!(
                        "`{name}` builds a pattern of its own - write `{name}(…)` first, not `.{name}(…)`, and chain the rest after it"
                    )
                } else {
                    match closest(
                        name,
                        known.iter().chain(registered.iter()).map(String::as_str),
                    ) {
                        Some(suggestion) => {
                            format!("unknown function `{name}` - did you mean `{suggestion}`?")
                        }
                        None => format!("unknown function `{name}`"),
                    }
                }
            });
            diagnostics.push(Diagnostic {
                level: Level::Value,
                message,
                from,
                to,
            });
            if diagnostics.len() >= MAX_DIAGNOSTICS {
                break;
            }
        }
        // `note("c").sine` reads `undefined`: a sound or a global value is not
        // a pattern method, and the pattern queries as silence.
        for name in values_read_off_patterns(&code, &unresolved_reads, &registered) {
            if diagnostics.len() >= MAX_DIAGNOSTICS {
                break;
            }
            let word = &source[name.clone()];
            let message = if rustel_voice::is_native_synth_sound(word) {
                format!(
                    "`.{word}` is not a pattern method - use `.s(\"{word}\")` to select that sound"
                )
            } else {
                format!("`.{word}` is not a pattern method")
            };
            diagnostics.push(Diagnostic {
                level: Level::Value,
                message,
                from: name.start,
                to: name.end,
            });
        }
        for (from, to, param, callee) in pattern_methods_on_value_callbacks(source) {
            if diagnostics.len() >= MAX_DIAGNOSTICS {
                break;
            }
            let method = &source[from..to];
            diagnostics.push(Diagnostic {
                level: Level::Value,
                message: format!(
                    "`{method}` is a pattern method - write `.{method}()` on the pattern, not `{param}.{method}()` inside `{callee}`"
                ),
                from,
                to,
            });
        }
    }
    // A top-level declaration in a setup file reads like a definition and is
    // gone the moment the file finishes: setup runs in its own scope, as it
    // does on strudel.cc. A note, never a refusal - the setup itself may be
    // using it perfectly well.
    if context.mode == LintMode::Setup && diagnostics.len() < MAX_DIAGNOSTICS {
        let definitions = rustel_transpiler::setup_definitions(source);
        let shared = definitions.names.iter().collect::<HashSet<_>>();
        for declaration in &definitions.top_level_declarations {
            if diagnostics.len() >= MAX_DIAGNOSTICS {
                break;
            }
            // One that is also handed to a global is doing the right thing:
            // `function riff() {}` then `globalThis.riff = riff`.
            if shared.contains(&declaration.name) {
                continue;
            }
            diagnostics.push(Diagnostic {
                level: Level::Note,
                message: format!(
                    "`{}` is a top-level {} of this setup, so no score can see it - \
                     assign `globalThis.{} = …` to share it",
                    declaration.name, declaration.kind, declaration.name
                ),
                from: declaration.from.min(source.len()),
                to: declaration.to.min(source.len()),
            });
        }
    }

    if diagnostics
        .iter()
        .all(|diagnostic| diagnostic.level == Level::Note)
    {
        let room = MAX_DIAGNOSTICS.saturating_sub(diagnostics.len());
        diagnostics.extend(notes.into_iter().take(room));
    }
    diagnostics.sort_by_key(|diagnostic| diagnostic.from);
    diagnostics
}

/// The message for a Hydra name in a score that never opened Hydra.
///
/// Without this, `contrast()` is an unknown function and the nearest real name
/// is `contract` - a stepwise function with nothing to do with visuals. The
/// name is not misspelled; the score is missing its first line.
fn hydra_without_init(name: &str, hydra_score: bool) -> Option<String> {
    #[cfg(not(feature = "hydra"))]
    {
        let _ = (name, hydra_score);
        None
    }
    #[cfg(feature = "hydra")]
    {
        if hydra_score || !rustel_hydra::hydra_names().any(|known| known == name) {
            return None;
        }
        Some(format!(
            "`{name}` is a Hydra function - did you forget `await initHydra()` at the top?"
        ))
    }
}

/// Names the engine answers to as a free call, versus only as a method on
/// a pattern. `log` is the second kind: `s("bd").log()` plays, `log(x)`
/// throws `ReferenceError: log is not defined`.
struct EngineNames {
    known: HashSet<String>,
    /// Pattern property names, including own fields and global aliases.
    pattern_methods: HashSet<String>,
    /// On a pattern, and not a global / registry / control function. A bare
    /// call of one of these throws `ReferenceError`.
    method_only: HashSet<String>,
    /// An extension's free function that builds a pattern of its own and is
    /// nowhere on `silence`'s prototype, such as `trancearp`. A call of
    /// `.trancearp(...)` on a pattern throws `not a function` when the
    /// update runs.
    global_only: HashSet<String>,
    /// Globals whose value is not a function: patterns such as `silence`
    /// and the signals, and objects such as `sliderValues`.
    values: HashSet<String>,
}

fn engine_names() -> &'static EngineNames {
    static NAMES: OnceLock<EngineNames> = OnceLock::new();
    NAMES.get_or_init(|| {
        let catalog = runtime_catalog();
        let mut globals = HashSet::new();
        #[cfg(feature = "extensions")]
        let registry = rustel_ext::default_registry();
        #[cfg(not(feature = "extensions"))]
        let registry = rustel_core::register::default_registry();
        for name in registry.names() {
            globals.insert(name.to_owned());
        }
        for row in rustel_core::controls_generated::CONTROLS {
            for name in row.names.iter().chain(row.aliases.iter()) {
                globals.insert((*name).to_owned());
            }
        }
        for name in rustel_jsruntime::supported_global_names() {
            globals.insert(name.to_owned());
        }
        for method in rustel_transpiler::VISUAL_WIDGET_METHODS {
            globals.insert((*method).to_owned());
            globals.insert(method.trim_start_matches('_').to_owned());
        }
        globals.extend(catalog.globals.iter().cloned());

        let mut known = globals.clone();
        known.extend(catalog.proto.iter().cloned());

        let method_only = catalog
            .proto
            .iter()
            .filter(|name| !globals.contains(*name))
            .cloned()
            .collect();

        #[cfg(feature = "extensions")]
        let global_only = rustel_ext::pattern_callables()
            .filter(|callable| callable.surface.global && !callable.surface.method)
            .flat_map(|callable| callable.names.iter())
            .filter(|name| !catalog.proto.contains(**name))
            .map(|name| (*name).to_owned())
            .collect();
        #[cfg(not(feature = "extensions"))]
        let global_only = HashSet::new();

        EngineNames {
            known,
            pattern_methods: catalog.proto,
            method_only,
            global_only,
            values: catalog.values,
        }
    })
}

/// Find sound or global-value properties on known pattern chains.
/// Preserve registered methods and properties on other objects.
fn values_read_off_patterns(
    code: &str,
    unresolved_reads: &[(String, std::ops::Range<usize>)],
    registered: &HashSet<String>,
) -> Vec<std::ops::Range<usize>> {
    let names = engine_names();
    let mut found = Vec::new();
    for (dot, _) in code.match_indices('.') {
        let Some(name) = scan::identifier_at(code, scan::space_after(code, dot + 1)) else {
            continue;
        };
        let word = &code[name.clone()];
        if code
            .as_bytes()
            .get(name.end)
            .is_some_and(|byte| scan::continues_name(*byte))
            || names.pattern_methods.contains(word)
            || registered.contains(word)
            || !(rustel_voice::is_native_synth_sound(word) || names.values.contains(word))
        {
            continue;
        }
        let Some(mut link) = scan::link_before(code, name.start) else {
            continue;
        };
        loop {
            let receiver = &code[link.name.clone()];
            if !names.pattern_methods.contains(receiver) {
                break;
            }
            if scan::dot_before(code, link.name.start).is_none() {
                if !names.method_only.contains(receiver)
                    && unresolved_reads
                        .iter()
                        .any(|(read, span)| read == receiver && *span == link.name)
                {
                    found.push(name);
                }
                break;
            }
            let Some(previous) = scan::link_before(code, link.name.start) else {
                break;
            };
            link = previous;
        }
    }
    found
}

/// Every function name this engine answers to: the pattern registry, the
/// controls, the visual widgets, and - asked of a runtime once - every
/// global and pattern method the score's JavaScript can actually reach.
pub fn known_names() -> &'static HashSet<String> {
    &engine_names().known
}

/// Pattern methods that are not also free functions. `s("bd").log()` is
/// fine; `log(x)` inside a callback is not.
fn pattern_method_only_names() -> &'static HashSet<String> {
    &engine_names().method_only
}

/// Free functions that build a pattern of their own and are not pattern
/// methods. `trancearp(…).s("piano")` plays; `s("piano").trancearp(…)`
/// throws `not a function`.
fn global_only_names() -> &'static HashSet<String> {
    &engine_names().global_only
}

/// Globals a score reads as values rather than calls, as a fresh runtime
/// reports them: `silence`, `nothing`, the signals, `sliderValues`.
pub fn global_values() -> &'static HashSet<String> {
    &engine_names().values
}

struct RuntimeCatalog {
    globals: HashSet<String>,
    proto: HashSet<String>,
    values: HashSet<String>,
}

/// What a fresh runtime has on `globalThis`, and on `silence` and its
/// prototype chain, kept apart so a method is not mistaken for a global.
/// `silence`'s own fields count: every pattern has its own `query`.
fn runtime_catalog() -> RuntimeCatalog {
    let Ok(runtime) = rustel_jsruntime::JsRuntime::new() else {
        return RuntimeCatalog {
            globals: HashSet::new(),
            proto: HashSet::new(),
            values: HashSet::new(),
        };
    };
    // The same bindings a session installs before its first evaluation.
    if runtime.install_semantic_bindings().is_err() {
        return RuntimeCatalog {
            globals: HashSet::new(),
            proto: HashSet::new(),
            values: HashSet::new(),
        };
    }
    let _ = runtime.install_voicings_prebake();
    let script = r#"globalThis.__rustel_known_names = JSON.stringify((() => {
        const globals = [...Object.getOwnPropertyNames(globalThis)]
            .filter((name) => !name.startsWith('__'));
        const proto = [];
        let object = typeof silence === 'object' && silence ? silence : null;
        while (object && object !== Object.prototype) {
            for (const name of Object.getOwnPropertyNames(object)) {
                if (!name.startsWith('__')) proto.push(name);
            }
            object = Object.getPrototypeOf(object);
        }
        const values = globals.filter((name) => {
            try {
                return typeof globalThis[name] !== 'function';
            } catch (_) {
                return false;
            }
        });
        return { globals, proto, values };
    })());"#;
    if runtime.eval(script).is_err() {
        return RuntimeCatalog {
            globals: HashSet::new(),
            proto: HashSet::new(),
            values: HashSet::new(),
        };
    }
    #[derive(serde::Deserialize, Default)]
    struct Names {
        #[serde(default)]
        globals: Vec<String>,
        #[serde(default)]
        proto: Vec<String>,
        #[serde(default)]
        values: Vec<String>,
    }
    let names = runtime
        .get_string("__rustel_known_names")
        .and_then(|json| serde_json::from_str::<Names>(&json).ok())
        .unwrap_or_default();
    RuntimeCatalog {
        globals: names.globals.into_iter().collect(),
        proto: names.proto.into_iter().collect(),
        values: names.values.into_iter().collect(),
    }
}

/// A port named in `midin('…')`, `midikeys('…')` or `.midi('…')` that this
/// machine is not offering.
///
/// Worth saying because nothing else does: the platform's own "not found"
/// report reaches the CLI's live loop and nowhere else, so in the studio a
/// mistyped port name is silence with no explanation. Matched exactly the
/// way the platform matches - an in-range index, or a case-insensitive
/// substring of a port's name, and never an empty name - so `'Arturia'` for
/// "Arturia KeyStep 32" is not flagged, because it works, and `''` is,
/// because it opens nothing.
fn check_midi_port(
    source: &str,
    literal: &Literal,
    context: &LintContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let callee = callee_before(source, literal.content.start - 1);
    let ports = match callee {
        Some("midin" | "midikeys") => context.midi_inputs.as_deref(),
        Some("midi") => context.midi_outputs.as_deref(),
        _ => None,
    };
    // An unprobed machine, or one with nothing plugged in, knows too little
    // to refuse a name - the same rule an empty sample library follows.
    let Some(ports) = ports.filter(|ports| !ports.is_empty()) else {
        return;
    };
    let wanted = source[literal.content.clone()].trim();
    let kind = if matches!(callee, Some("midi")) {
        "output"
    } else {
        "input"
    };
    if wanted.is_empty() {
        // Still being typed: an auto-closed `''` is not a choice yet.
        if literal.open {
            return;
        }
        diagnostics.push(Diagnostic {
            level: Level::Value,
            message: format!(
                "an empty MIDI {kind} name opens no port - leave it out, or write 0, \
                 for the first one"
            ),
            from: literal.content.start,
            to: literal.content.end,
        });
        return;
    }
    if let Ok(index) = wanted.parse::<usize>() {
        if index < ports.len() {
            return;
        }
    } else {
        let folded = wanted.to_ascii_lowercase();
        if ports
            .iter()
            .any(|port| port.to_ascii_lowercase().contains(&folded))
        {
            return;
        }
    }
    let mut message = format!("no MIDI {kind} here is called \"{wanted}\"");
    if let Some(near) = closest(wanted, ports.iter().map(String::as_str)) {
        message.push_str(&format!(" - did you mean \"{near}\"?"));
    } else {
        message.push_str(&format!(" - plugged in: {}", ports.join(", ")));
    }
    diagnostics.push(Diagnostic {
        level: Level::Value,
        message,
        from: literal.content.start,
        to: literal.content.end,
    });
}

/// The name closest to `word` among `names`, when one is close enough to
/// be a typing slip and not a different word: at most one edit for a short
/// word, and one edit per three characters for a longer one.
pub fn closest<'a>(word: &str, names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let word = word.to_lowercase();
    let allowed = (word.chars().count() / 3).max(1);
    names
        .filter_map(|name| {
            let distance = edit_distance(&word, &name.to_lowercase());
            (distance <= allowed && distance > 0).then_some((distance, name))
        })
        .min_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.len().cmp(&right.1.len()))
                .then_with(|| left.1.cmp(right.1))
        })
        .map(|(_, name)| name)
}

/// Edits between two words, a swap of neighbours counting as one.
pub fn edit_distance(left: &str, right: &str) -> usize {
    edits_between(left, right, true)
}

/// Edits between two words, a swap of neighbours counting as two: the
/// Levenshtein distance.
pub fn levenshtein_distance(left: &str, right: &str) -> usize {
    edits_between(left, right, false)
}

fn edits_between(left: &str, right: &str, swap_is_one_edit: bool) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut rows = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let cost = usize::from(left[i - 1] != right[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if swap_is_one_edit
                && i > 1
                && j > 1
                && left[i - 1] == right[j - 2]
                && left[i - 2] == right[j - 1]
            {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[left.len()][right.len()]
}

/// Names JavaScript itself provides on values, so `.map(` and `Math.floor(`
/// are never questioned.
const JAVASCRIPT_METHODS: &[&str] = &[
    "map",
    "filter",
    "forEach",
    "reduce",
    "reduceRight",
    "join",
    "split",
    "slice",
    "splice",
    "concat",
    "includes",
    "indexOf",
    "lastIndexOf",
    "push",
    "pop",
    "shift",
    "unshift",
    "sort",
    "reverse",
    "flat",
    "flatMap",
    "find",
    "findIndex",
    "findLast",
    "some",
    "every",
    "keys",
    "values",
    "entries",
    "toString",
    "toFixed",
    "toPrecision",
    "toUpperCase",
    "toLowerCase",
    "trim",
    "trimStart",
    "trimEnd",
    "replace",
    "replaceAll",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "padStart",
    "padEnd",
    "at",
    "fill",
    "from",
    "of",
    "apply",
    "call",
    "bind",
    "then",
    "catch",
    "finally",
    "repeat",
    "startsWith",
    "endsWith",
    "match",
    "matchAll",
    "test",
    "exec",
    "substring",
    "substr",
    "hasOwnProperty",
    "floor",
    "ceil",
    "round",
    "random",
    "min",
    "max",
    "abs",
    "pow",
    "sqrt",
    "log",
    "log2",
    "log10",
    "sin",
    "cos",
    "tan",
    "atan",
    "atan2",
    "exp",
    "sign",
    "trunc",
    "parse",
    "stringify",
    "assign",
    "freeze",
    "isArray",
    "isInteger",
    "isNaN",
    "isFinite",
    "valueOf",
    "toJSON",
    "get",
    "set",
    "has",
    "add",
    "delete",
    "clear",
    "next",
    "resolve",
    "reject",
    "all",
    "race",
    "now",
];

/// Names JavaScript provides as globals, and the words that only look like
/// a call.
const JAVASCRIPT_GLOBALS: &[&str] = &[
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "Number",
    "String",
    "Boolean",
    "Array",
    "Object",
    "Math",
    "JSON",
    "console",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "Promise",
    "Map",
    "Set",
    "WeakMap",
    "Symbol",
    "Error",
    "TypeError",
    "RangeError",
    "Date",
    "RegExp",
    "require",
    "fetch",
    "print",
    "alert",
    // `window` is an alias of the global object, not a browser API: scores
    // declare helpers with `window.name = …`. There is no `document`; browser
    // UI code is refused here rather than absorbed into a sink that lets it
    // look like it worked.
    "window",
    "eval",
    "if",
    "for",
    "while",
    "function",
    "return",
    "switch",
    "catch",
    "typeof",
    "new",
    "await",
    "async",
    "yield",
    "do",
    "else",
    "try",
    "throw",
    "delete",
    "void",
    "in",
    "of",
    "instanceof",
    "super",
    "this",
    "import",
    "export",
    "class",
    "let",
    "const",
    "var",
    "with",
];

/// Calls a setup file may not make, with the engine's own reason.
///
/// Read from the code alone, like every other check here: a name in a
/// comment or a string is not a call, and one the setup declares itself is
/// the setup's own function rather than the engine's.
fn refused_setup_calls(source: &str) -> Vec<(usize, usize, String)> {
    /// Every name and the policy the engine would answer with.
    fn policy(name: &str) -> Option<&'static str> {
        match name {
            "setcps" | "setCps" | "setcpm" | "setCpm" => Some(rustel_jsruntime::TEMPO_SCOPE_POLICY),
            "midin" | "midikeys" => Some(rustel_jsruntime::MIDI_INPUT_SCOPE_POLICY),
            #[cfg(feature = "hydra")]
            "initHydra" | "H" => Some(rustel_jsruntime::HYDRA_SCOPE_POLICY),
            _ => None,
        }
    }

    let code = code_only(source);
    let bytes = code.as_bytes();
    let mut identifiers = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match scan::identifier_at(&code, index) {
            Some(name) => {
                index = name.end;
                identifiers.push((name.start, name.end));
            }
            None => index += 1,
        }
    }

    // A setup that declares the name itself is calling its own function, and
    // the engine's policy has nothing to say about it.
    let own = identifiers
        .iter()
        .filter(|&&(start, end)| {
            matches!(
                word_before_in(&code, start),
                Some("function" | "class" | "const" | "let" | "var")
            ) && policy(&code[start..end]).is_some()
        })
        .map(|&(start, end)| &code[start..end])
        .collect::<HashSet<_>>();

    let mut refused = Vec::new();
    for &(start, end) in &identifiers {
        let name = &code[start..end];
        let Some(policy) = policy(name) else {
            continue;
        };
        if own.contains(name) {
            continue;
        }
        // A method of something else - `clock.setCps(1)` - is not this name.
        if scan::dot_before(&code, start).is_some() {
            continue;
        }
        if bytes.get(scan::space_after(&code, end)) != Some(&b'(') {
            continue;
        }
        refused.push((
            start,
            end,
            format!("`{name}` cannot run in a prebake - {policy}"),
        ));
    }
    refused
}

/// The word immediately before `at`, skipping whitespace.
fn word_before_in(code: &str, at: usize) -> Option<&str> {
    scan::name_before(code, at).map(|name| &code[name])
}

fn unknown_calls(
    source: &str,
    known: &HashSet<String>,
    registered: &HashSet<String>,
) -> Vec<(usize, usize)> {
    let code = code_only(source);
    let bytes = code.as_bytes();
    let mut identifiers = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(name) = scan::identifier_at(&code, index) {
            index = name.end;
            identifiers.push((name.start, name.end));
        } else if bytes[index].is_ascii_digit() {
            // `2e3` and `0x1f` must not leave `e3` or `x1f` behind.
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'.')
            {
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    let next_non_space = |at: usize| bytes.get(scan::space_after(&code, at)).copied();
    // The same question, asked of the original source. Blanking replaces a
    // template with spaces, backticks included, so a tag's backtick is not
    // in `code`. Blanking keeps every byte in place, so the offsets match.
    let next_non_space_raw = |at: usize| {
        source
            .as_bytes()
            .get(scan::space_after(source, at))
            .copied()
    };
    // The index of the '(' that follows `end`, when one does.
    let open_paren_after = |end: usize| {
        let at = scan::space_after(&code, end);
        (bytes.get(at) == Some(&b'(')).then_some(at)
    };
    // A definition rather than a call: `NAME(args) { … }` - the call form
    // followed by a block - is an ES `function`, a class method, or an
    // object-literal method, none of which the engine resolves by name.
    let is_definition = |open: usize| {
        matching_paren(&code, open)
            .is_some_and(|close| bytes.get(scan::space_after(&code, close + 1)) == Some(&b'{'))
    };
    // The identifier token ending just before `start`, if any: the keyword
    // in front of a `function NAME` / `class NAME` declaration.
    let word_before = |start: usize| {
        let end = scan::space_before(&code, start);
        identifiers
            .iter()
            .find(|&&(_, token_end)| token_end == end)
            .map(|&(from, to)| &code[from..to])
    };
    // A word the score uses as anything but a call (a const, a parameter,
    // a key) is the score's own, whatever it is called; so is one it
    // defines as a function or method.
    let mut defined = HashSet::new();
    // A word the score DECLARES as a variable, specifically. `const slider =
    // document.createElement('input')` is not the engine's `slider`, so what
    // is called on it is not the engine's either.
    let mut declared = HashSet::new();
    for &(start, end) in &identifiers {
        let name = &code[start..end];
        if matches!(
            word_before(start),
            Some("const") | Some("let") | Some("var")
        ) {
            declared.insert(name);
        }
        match open_paren_after(end) {
            // A name with no call parenthesis is something the score
            // mentions rather than calls, so it counts as defined. A name
            // followed by a backtick is a tagged template, which is a call:
            // it must not define itself, or `` tidal`c4` `` would pass a
            // checker that catches `tidal("c4")`.
            None if next_non_space_raw(end) != Some(b'`') => {
                defined.insert(name);
            }
            None => {}
            Some(open)
                if is_definition(open)
                    || matches!(word_before(start), Some("function") | Some("class")) =>
            {
                defined.insert(name);
            }
            // A call that defines nothing. Its own arm rather than an `if`
            // inside the one above, because the guard can fail and the match
            // still has to be exhaustive.
            Some(_) => {}
        }
    }
    let mut unknown = Vec::new();
    for &(start, end) in &identifiers {
        // A call, written either way. A tagged template is a call:
        // `` tag`...` `` runs `tag`. Strudel's alternative notations use
        // that form (`` tidal`c4` ``) and this engine implements none of
        // them, so an unknown tag is an unknown call.
        if next_non_space(end) != Some(b'(') && next_non_space_raw(end) != Some(b'`') {
            continue;
        }
        let name = &code[start..end];
        let method = scan::dot_before(&code, start).is_some();
        // `log` is a method on a pattern, not a global. A bare `log(x)`
        // inside a callback throws `log is not defined` in the engine, so
        // the checker reports a method-only name called as a free function.
        if !method
            && pattern_method_only_names().contains(name)
            && !registered.contains(name)
            && !defined.contains(name)
            && !declared.contains(name)
            && !JAVASCRIPT_GLOBALS.contains(&name)
        {
            unknown.push((start, end));
            continue;
        }
        // The mirror image. `trancearp` builds a pattern of its own - the
        // prebake declared it as a global, and so does this engine - so
        // `.trancearp(…)` on a pattern throws `not a function`.
        if method
            && global_only_names().contains(name)
            && !registered.contains(name)
            && !defined.contains(name)
            && !declared.contains(name)
            && !JAVASCRIPT_METHODS.contains(&name)
        {
            unknown.push((start, end));
            continue;
        }
        if known.contains(name)
            || registered.contains(name)
            || JAVASCRIPT_GLOBALS.contains(&name)
            || defined.contains(name)
            || name.starts_with(|c: char| c.is_ascii_uppercase())
        {
            continue;
        }
        if method {
            if JAVASCRIPT_METHODS.contains(&name) {
                continue;
            }
            // `Math.floor(`, `console.log(`, `JSON.parse(`: the receiver is
            // JavaScript's, not the engine's.
            let dot = scan::dot_before(&code, start).unwrap_or(0);
            if let Some(receiver) = scan::name_before(&code, dot) {
                let receiver = identifiers
                    .iter()
                    .find(|&&(_, end)| end == receiver.end)
                    .map(|&(from, to)| (from, &code[from..to]));
                // `node.summingNode.gain.setValueAtTime(…)`: the receiver is
                // the whole chain, not the `gain` control that ends it. What
                // an expression carries is not knowable from the text, so
                // nothing is claimed about what may be called on it.
                let chained =
                    receiver.is_some_and(|(from, _)| scan::dot_before(&code, from).is_some());
                if chained {
                    continue;
                }
                if receiver.is_some_and(|(_, receiver)| {
                    JAVASCRIPT_GLOBALS.contains(&receiver)
                        || receiver.starts_with(|c: char| c.is_ascii_uppercase())
                        || declared.contains(receiver)
                        || (defined.contains(receiver)
                            && !known.contains(receiver)
                            && !registered.contains(receiver))
                }) {
                    continue;
                }
            }
        }
        unknown.push((start, end));
    }
    unknown
}

/// Callbacks whose argument is a hap *value* (or the hap itself), not a
/// pattern. `every(2, x => x.fast(2))` is a pattern transformer and is
/// valid; `filterValues(x => x.log())` calls a pattern method on a value.
const VALUE_CALLBACKS: &[&str] = &["filterValues", "filterHaps", "fmap", "withValue", "withHap"];

/// The identifier after `index`, skipping whitespace. It may not start with
/// a digit.
fn ident_at(code: &str, index: usize) -> Option<(usize, usize)> {
    let name = scan::identifier_at(code, scan::space_after(code, index))?;
    Some((name.start, name.end))
}

/// `param.method(` inside a value/hap callback, covering only pattern methods
/// that have no free form. Returns the method's span, the parameter name,
/// and the callee (`filterValues`, …).
fn pattern_methods_on_value_callbacks(source: &str) -> Vec<(usize, usize, String, String)> {
    let code = code_only(source);
    let bytes = code.as_bytes();
    let methods = pattern_method_only_names();
    let mut found = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let Some((start, end)) = ident_at(&code, index) else {
            index += 1;
            continue;
        };
        let name = &code[start..end];
        index = end;
        if !VALUE_CALLBACKS.contains(&name) {
            continue;
        }
        let open = scan::space_after(&code, end);
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        let Some(close) = matching_paren(&code, open) else {
            continue;
        };
        let Some((param, body)) = value_callback_param_and_body(&code, open, close) else {
            continue;
        };
        scan_param_pattern_methods(&code, &param, body, methods, name, &mut found);
        index = close.saturating_add(1);
    }
    found
}

/// The parameter a value callback's body is read for, and that body: the
/// name its first parameter opens with (`x` in `x = 1` too), and the block
/// after the head, or for an arrow the expression running to the call's
/// `)` at `close`.
fn value_callback_param_and_body(
    code: &str,
    open: usize,
    close: usize,
) -> Option<(String, std::ops::Range<usize>)> {
    let head = scan::function_head(code, open + 1)?;
    let param = scan::identifier_at(code, head.params.first()?.start)?;
    let index = scan::space_after(code, head.end);
    let body = if code.as_bytes().get(index) == Some(&b'{') {
        index + 1..matching_pair(code, index, b'{', b'}')?
    } else if head.keyword {
        return None;
    } else {
        index..close
    };
    Some((code[param].to_owned(), body))
}

fn scan_param_pattern_methods(
    code: &str,
    param: &str,
    body: std::ops::Range<usize>,
    methods: &HashSet<String>,
    callee: &str,
    found: &mut Vec<(usize, usize, String, String)>,
) {
    let bytes = code.as_bytes();
    let mut index = body.start;
    let end = body.end.min(bytes.len());
    while index < end {
        let Some((from, to)) = ident_at(code, index) else {
            index += 1;
            continue;
        };
        if from >= end || to > end {
            break;
        }
        index = to;
        if &code[from..to] != param || scan::dot_before(code, from).is_some() {
            continue;
        }
        let dot = scan::space_after(code, to);
        if bytes.get(dot) != Some(&b'.') {
            continue;
        }
        let Some((method_from, method_to)) = ident_at(code, dot + 1) else {
            continue;
        };
        if method_to > end {
            break;
        }
        let method = &code[method_from..method_to];
        let call = scan::space_after(code, method_to);
        if bytes.get(call) != Some(&b'(') {
            continue;
        }
        if methods.contains(method) {
            found.push((method_from, method_to, param.to_owned(), callee.to_owned()));
        }
        index = method_to;
    }
}

/// What [`code_only`] blanked over a stretch of source, for a caller that
/// has to tell a reader which of the two it was: see [`blanked_at`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Blanked {
    /// A `//`, or a `/* */`.
    Comment,
    /// A quoted string, which in a score is usually mini-notation, or a
    /// regex literal, whose body is pattern text just the same.
    Text,
}

/// One stretch [`scan_blanked`] found, told apart as finely as its readers
/// need: [`code_only`] blanks all three alike, [`blanked_at`] tells comment
/// from text, and [`string_literals`] wants the strings - and not the
/// regexes, which blank the same but are never mini-notation.
enum Lexeme {
    /// A `//`, or a `/* */`.
    Comment,
    /// A quoted string, with the text between its quotes.
    String(Literal),
    /// A regex literal, delimiters and flags included.
    Regex,
}

/// The words a `/` right after one of them opens a regex rather than
/// divides: each takes an expression next, and a regex is one. `=>` needs
/// no entry of its own - it ends with `>`, and `>` is an operator below.
const REGEX_WORDS: &[&[u8]] = &[
    b"return", b"typeof", b"case", b"in", b"of", b"new", b"delete", b"void", b"do", b"else",
    b"yield", b"await",
];

/// Estimate whether `/` opens a regex from the preceding code byte or word.
/// This is a lexical heuristic, not a JavaScript parser: operators and words
/// that take expressions allow a regex; unrecognized contexts use division
/// so the scanner does not hide code by treating it as regex text.
fn regex_may_start(source: &str, prev: Option<(usize, u8)>) -> bool {
    let bytes = source.as_bytes();
    let Some((at, byte)) = prev else {
        // Nothing but whitespace and comments before it, or nothing at
        // all: an expression can start here, and so can a regex.
        return true;
    };
    match byte {
        b'(' | b',' | b'=' | b':' | b'[' | b'!' | b'&' | b'|' | b'?' | b'{' | b'}' | b';'
        | b'+' | b'-' | b'*' | b'%' | b'~' | b'^' | b'<' | b'>' => {
            // `a++ / 2 / 3` divides - the `++` belongs to `a`, and the
            // slash follows `a`'s value - while a single `+` or `-` is an
            // operator a regex can be the right-hand side of: `a + /re/`.
            !(matches!(byte, b'+' | b'-') && at >= 2 && bytes[at - 2] == byte)
        }
        b'a'..=b'z' | b'A'..=b'Z' => {
            // The whole word, walking back over whatever continues a
            // JavaScript name, a non-ASCII letter included: `için / 2`
            // divides, and is not `in`.
            let mut start = at;
            while start > 0 && scan::continues_name(bytes[start - 1]) {
                start -= 1;
            }
            let word = &bytes[start..at];
            // `obj.return / 2` divides: after a dot the word is a
            // property, and a property is a value.
            REGEX_WORDS.contains(&word) && scan::dot_before(source, start).is_none()
        }
        // The end of a value: an identifier, a number, a quote, `)`, `]`,
        // `.` - what follows the slash is its right-hand side.
        _ => false,
    }
}

/// The end of the regex literal that opens at `start`, past the closing `/`
/// and its flags, or `None` when no regex opens there.
///
/// A JavaScript regex cannot hold a raw newline, so the search stops at
/// one: a `/` with no unescaped closing `/` on its line is a division, and
/// a search into a later line would blank real code. A `/` inside a `[...]`
/// class is a character and does not close the regex. A class that the
/// line ends inside is covered by the same newline rule.
fn regex_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    let mut in_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 1;
                if index >= bytes.len() || bytes[index] == b'\n' {
                    return None;
                }
            }
            b'\n' => return None,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
                    index += 1;
                }
                return Some(index);
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Visit the strings, regex literals and comments recognized in `source`.
/// [`code_only`], [`blanked_at`] and [`string_literals`] share this scanner
/// so they agree about delimiters inside text, such as the quote in `/"/`.
fn scan_blanked(source: &str, mut visit: impl FnMut(usize, usize, Lexeme)) {
    let bytes = source.as_bytes();
    // The last byte of code the scan passed, and the offset just past it,
    // for the regex-or-division call at each `/`. Comments do not update
    // it: `a // c\n/ 2` divides by what stands before the comment.
    let mut prev: Option<(usize, u8)> = None;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                let start = index;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                visit(start, index, Lexeme::Comment);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let start = index;
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                visit(start, index, Lexeme::Comment);
            }
            b'/' if regex_may_start(source, prev) => {
                // Blank a regex literal whole, delimiters and flags
                // included, as a string's quotes are. A quote inside one is
                // pattern text; left in place, it would read as the start
                // of a string and mis-pair the real quotes after it. A `/`
                // whose line ends before it closes is not a regex: it stays
                // a division.
                match regex_literal_end(bytes, index) {
                    Some(end) => {
                        visit(index, end, Lexeme::Regex);
                        prev = Some((end, bytes[end - 1]));
                        index = end;
                    }
                    None => {
                        prev = Some((index + 1, b'/'));
                        index += 1;
                    }
                }
            }
            quote @ (b'"' | b'\'' | b'`') => {
                let start = index;
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    if index < bytes.len() && quote != b'`' && bytes[index] == b'\n' {
                        break;
                    }
                    index += 1;
                }
                // The text stops at the closing quote, or - never closed -
                // at the newline or the end of source it ran to.
                let content_end = index.min(bytes.len());
                let open = bytes.get(content_end) != Some(&quote);
                index = (index + 1).min(bytes.len());
                visit(
                    start,
                    index,
                    Lexeme::String(Literal {
                        content: start + 1..content_end,
                        open,
                    }),
                );
                prev = Some((index, bytes[index - 1]));
            }
            byte => {
                if !byte.is_ascii_whitespace() {
                    prev = Some((index + 1, byte));
                }
                index += 1;
            }
        }
    }
}

/// Which of the two, if either, covers a byte offset.
pub fn blanked_at(source: &str, at: usize) -> Option<Blanked> {
    let mut found = None;
    scan_blanked(source, |start, end, lexeme| {
        if (start..end).contains(&at) {
            found = Some(match lexeme {
                Lexeme::Comment => Blanked::Comment,
                Lexeme::String(_) | Lexeme::Regex => Blanked::Text,
            });
        }
    });
    found
}

/// `source` with every string, regex literal and comment blanked to
/// spaces, so positions still line up but only code is left to read.
pub fn code_only(source: &str) -> String {
    let mut code = source.as_bytes().to_vec();
    scan_blanked(source, |from, to, _| {
        for byte in &mut code[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    });
    // Each recognized span covers complete characters, preserving UTF-8.
    String::from_utf8(code).unwrap_or_else(|_| source.to_owned())
}

/// The reason an update should be refused, or `None` when the score is
/// clean: the first finding, with its line, in the words the engine would
/// have used.
pub fn rejection(source: &str, mini: bool, library: Option<&SampleLibrary>) -> Option<String> {
    rejection_of(source, &lint(source, mini, library))
}

/// [`rejection`] knowing what a setup already defined.
pub fn rejection_with(
    source: &str,
    mini: bool,
    library: Option<&SampleLibrary>,
    context: &LintContext,
) -> Option<String> {
    rejection_of(source, &lint_with(source, mini, library, context))
}

/// The same answer from findings already in hand, so a caller that wants
/// both the underlines and the reason checks once rather than twice.
pub fn rejection_of(source: &str, diagnostics: &[Diagnostic]) -> Option<String> {
    // A note is not a reason to refuse a save: the score plays, and refusing
    // it mid-set would take the sound away over a portability remark.
    let first = diagnostics.iter().find(|d| d.level != Level::Note)?;
    let line = source[..first.from.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    Some(format!("line {line}: {}", first.message))
}

/// The name of the call a string at `quote` is the first argument of:
/// `scale` for `.scale("C:major")`, `None` for anything else.
pub fn callee_before(source: &str, quote: usize) -> Option<&str> {
    let paren = scan::space_before(source, quote).checked_sub(1)?;
    if source.as_bytes()[paren] != b'(' {
        return None;
    }
    scan::name_before(source, paren).map(|name| &source[name])
}

/// Whether `method` reads its receiver's words as indexes or keys into its
/// own arguments, as `s("<a b>".pick({a: "bd"}))` does. Such words are not
/// sounds, notes or chords. The set is every name of the reference entries
/// tagged `selectors`.
fn rebinds_words(method: &str) -> bool {
    static SELECTORS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SELECTORS
        .get_or_init(|| {
            declared_entries()
                .filter(|entry| entry.tags.contains(&"selectors"))
                .flat_map(entry_names)
                .collect()
        })
        .contains(method)
}

/// The reference entries of the JavaScript surface, the extensions, the
/// controls and the combinators.
fn declared_entries() -> impl Iterator<Item = rustel_core::reference::ReferenceEntry> {
    #[cfg(feature = "extensions")]
    let extension_entries = rustel_ext::reference_entries();
    #[cfg(not(feature = "extensions"))]
    let extension_entries = std::iter::empty::<&'static rustel_core::reference::ReferenceEntry>();
    let combinators = rustel_core::register::default_registry()
        .reference_entries()
        .copied()
        .collect::<Vec<_>>();
    rustel_jsruntime::reference_entries()
        .copied()
        .chain(extension_entries.copied())
        .chain(rustel_core::controls_generated::reference_entries().copied())
        .chain(combinators)
}

/// An entry's name and synonyms.
fn entry_names(
    entry: rustel_core::reference::ReferenceEntry,
) -> impl Iterator<Item = &'static str> {
    std::iter::once(entry.name).chain(entry.synonyms.iter().copied())
}

/// The method called on a string: `voicing` for `"C^7 Dm7".voicing()`.
/// Mirrors [`callee_before`]. The name must follow the `.` and precede the
/// `(` directly.
pub(crate) fn callee_after(source: &str, after_quote: usize) -> Option<&str> {
    let dot = scan::space_after(source, after_quote);
    if source.as_bytes().get(dot) != Some(&b'.') {
        return None;
    }
    let name = scan::name_starting_at(source, dot + 1);
    (!name.is_empty() && source.as_bytes().get(name.end) == Some(&b'(')).then(|| &source[name])
}

/// A word the engine resolves by name: an atom with every `:tail` the
/// pattern hangs on it, so `<A1 D2>/4:minor` gives `A1:minor` and
/// `D2:minor`. Operands such as the `2` of `C:major*2` are not words. The
/// studio's bank list reads these words too.
#[derive(Clone, Debug, PartialEq)]
pub struct MiniWord {
    pub text: String,
    /// The atom, extended over a tail written directly on it (`D:major`).
    from: usize,
    to: usize,
    /// A tail hung on a group the atom sits in (`<C D>:major`): where a
    /// fault every atom of the group shares is marked.
    tail: Option<(usize, usize)>,
    /// The first tail is a single atom (`bd:3`, `[bd hh]:3`), not a pattern
    /// (`bd:<0 3>`).
    pub written_index: bool,
    /// The word comes from a range such as `0 .. 3`.
    pub ranged: bool,
}

#[derive(Clone)]
struct Tail {
    text: String,
    from: usize,
    to: usize,
    /// Written on the atom itself rather than on a group around it.
    direct: bool,
    /// The tail is a single atom (`:3`), not a pattern (`:<0 3>`).
    alone: bool,
}

/// Whether `ast` is one atom, or atoms joined by `:` and nothing else.
fn atom_chain(ast: &rustel_mini::Ast) -> bool {
    use rustel_mini::Ast;
    match ast {
        Ast::Atom { .. } => true,
        Ast::Tail { pat, element, .. } => atom_chain(pat) && matches!(**element, Ast::Atom { .. }),
        _ => false,
    }
}

/// The hint for `word`, whose sample number `n` no machine in `counted`
/// has: what each plays instead, wrapped as playback wraps it, and how many
/// samples it holds. `None` when some machine plays `n` itself.
fn plays_instead(word: &str, n: f64, counted: &[(String, usize)]) -> Option<String> {
    let played = counted
        .iter()
        .map(|(name, variants)| (name, *variants, crate::samples::sound_index(n, *variants)))
        .collect::<Vec<_>>();
    if played.iter().any(|(_, _, index)| *index as f64 == n) {
        return None;
    }
    let plays = played
        .iter()
        .map(|(name, variants, index)| {
            let held = match variants {
                1 => format!("1 sample: {name}:0"),
                _ => format!("{variants} samples: {name}:0 … {name}:{}", variants - 1),
            };
            format!("{name}:{index} (\"{name}\" has {held})")
        })
        .collect::<Vec<_>>();
    Some(format!("{word} plays {}", plays.join(" or ")))
}

/// The `.bank(…)` that decides the machine of the pattern at `at` in
/// `code`: the last bank among [`scan::applied_links`], which says what
/// `head` is. A bank renames the sounds of the pattern it is chained on,
/// never a sibling's.
pub fn applied_bank(code: &str, at: usize, head: Option<usize>) -> Option<scan::Link> {
    scan::applied_links(code, at, head)
        .into_iter()
        .rev()
        .find(|link| {
            rustel_core::controls::canonical_control_name(&code[link.name.clone()]) == Some("bank")
        })
}

/// The call whose words `literal` holds: the enclosing call, or a method
/// chained on the string that reads its words as selectors into its own
/// arguments (`"<a b>".pick({…})`).
fn literal_role<'a>(source: &'a str, literal: &Literal) -> Option<&'a str> {
    let chained = callee_after(source, literal.content.end.saturating_add(1));
    match chained {
        Some(method) if rebinds_words(method) => chained,
        _ => callee_before(source, literal.content.start.saturating_sub(1)).or(chained),
    }
}

/// A sound the live code of a score names: a word of an `s("…")` or
/// `sound("…")` string.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedSound {
    /// The name as written, without its index.
    pub name: String,
    /// The variant its `:` picks, or zero.
    pub n: f64,
    /// The machines of the `.bank()` it plays through, in whose spelling
    /// (`{bank}_{name}`) the voice looks it up; empty for none.
    pub banks: Vec<String>,
}

/// Every sound word the live code names, in the order written. A comment,
/// a muted lane (a label starting or ending with `_`), a statement routed
/// through `.osc()` and a bank named by anything but a quoted string name
/// nothing here; nor do rests, numbers and the input `in`.
pub fn live_sound_names(source: &str) -> Vec<NamedSound> {
    let code = code_only(source);
    let mut named = Vec::new();
    for literal in string_literals(source) {
        if !matches!(literal_role(source, &literal), Some("s" | "sound"))
            || in_muted_lane(&code, literal.content.start)
            || code[statement_around(&code, literal.content.start)].contains(".osc(")
        {
            continue;
        }
        let (banks, banked_opaquely) = literal_banks(source, &code, &literal);
        let Some(words) = mini_words_of(&source[literal.content.clone()]) else {
            continue;
        };
        if banked_opaquely {
            continue;
        }
        for word in words {
            let mut parts = word.text.split(':');
            let head = parts.next().unwrap_or_default();
            if !is_plain_atom(&word.text) || head == "in" || head.parse::<f64>().is_ok() {
                continue;
            }
            let n = parts
                .next()
                .and_then(|index| index.parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .unwrap_or(0.0);
            named.push(NamedSound {
                name: head.to_owned(),
                n,
                banks: banks.clone(),
            });
        }
    }
    named
}

/// A plugin the live code of a score asks for with a `.vst("…")` or
/// `.vsti("…")` call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedPlugin {
    /// The name as written.
    pub name: String,
    /// True for `.vsti()`.
    pub instrument: bool,
    /// The place of an effect in the chain of its statement: the `.vst()`
    /// calls before the call. 0 for an instrument.
    pub stage: usize,
    /// The `preset` of the call, when a quoted string gives the name.
    pub preset: Option<String>,
    /// The orbit of the notes: the number of the one `.orbit()` call of the
    /// statement, or 1 with no such call. `None` when the text gives no one
    /// number: the orbit is a pattern, or the statement has 2 such calls.
    pub orbit: Option<usize>,
    /// The place of each key of the object of the call, in the text.
    pub keys: Vec<std::ops::Range<usize>>,
}

/// The place of each key in the object of a plugin call: `depth` and
/// `in gain` for `{ depth: 0.5, "in gain": 1 }`. `open` and `close` are the
/// brackets of the call, and `code` is the score with comments and strings
/// blank. A key with no colon and a key in square brackets give no place.
fn object_keys(
    code: &str,
    literals: &[Literal],
    open: usize,
    close: usize,
) -> Vec<std::ops::Range<usize>> {
    let Some(start) = code[open..close].find('{').map(|at| open + at) else {
        return Vec::new();
    };
    let end = scan::matching_pair(code, start, b'{', b'}').map_or(close, |end| end.min(close));
    // The brackets open inside the object before the byte at `at`.
    let depth_at = |at: usize| {
        code[start + 1..at]
            .bytes()
            .fold(0i32, |depth, byte| match byte {
                b'{' | b'(' | b'[' => depth + 1,
                b'}' | b')' | b']' => depth - 1,
                _ => depth,
            })
    };
    let colon_after = |at: usize| code[at.min(end)..end].trim_start().starts_with(':');
    let mut keys = Vec::new();
    for literal in literals {
        let content = &literal.content;
        if content.start > start
            && content.end < end
            && !literal.open
            && depth_at(content.start) == 0
            && colon_after(content.end + 1)
        {
            keys.push(content.clone());
        }
    }
    let bytes = code.as_bytes();
    let mut at = start + 1;
    while at < end {
        let from = at;
        while at < end && scan::continues_name(bytes[at]) {
            at += 1;
        }
        if at == from {
            at += 1;
            continue;
        }
        // A key follows the open bracket or a comma.
        let before = code[start..from].trim_end();
        if (before.ends_with('{') || before.ends_with(','))
            && depth_at(from) == 0
            && colon_after(at)
        {
            keys.push(from..at);
        }
    }
    keys.sort_by_key(|key| key.start);
    keys
}

/// Reports each key in the object of a plugin call that is no parameter of
/// the plugin. The check loads no plugin: a plugin not yet loaded is not
/// judged, and the note reports a wrong key at its start.
#[cfg(feature = "vst")]
fn plugin_key_problems(source: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(host) = crate::vst::started() else {
        return;
    };
    for call in live_plugins(source) {
        let Some(plugin) = host.loaded(&call.name).filter(|_| !call.keys.is_empty()) else {
            continue;
        };
        for key in &call.keys {
            let word = &source[key.clone()];
            if word == "preset" || plugin.param(word).is_some() {
                continue;
            }
            let mut message = format!("{} has no parameter \"{word}\"", plugin.name());
            let known = plugin.params().iter().map(|param| param.key.as_str());
            if let Some(near) = closest(word, known) {
                message.push_str(&format!(" - did you mean \"{near}\"?"));
            }
            diagnostics.push(Diagnostic {
                level: Level::Value,
                message,
                from: key.start,
                to: key.end,
            });
        }
    }
}

/// Every plugin call of the live code, in the order written. A comment and
/// a muted lane ask for nothing here, as in [`live_sound_names`].
pub fn live_plugins(source: &str) -> Vec<NamedPlugin> {
    let code = code_only(source);
    let literals = string_literals(source);
    let mut plugins = Vec::new();
    for literal in &literals {
        let at = literal.content.start;
        let instrument = match callee_before(source, at.saturating_sub(1)) {
            Some("vst") => false,
            Some("vsti") => true,
            _ => continue,
        };
        let name = &source[literal.content.clone()];
        if name.trim().is_empty() || in_muted_lane(&code, at) {
            continue;
        }
        // The call runs from its open bracket to its close bracket, or to
        // the end of a text still in the works.
        let open = scan::space_before(source, at - 1) - 1;
        let close = matching_paren(&code, open).unwrap_or(code.len());
        let preset = literals.iter().find(|value| {
            let key = code[..value.content.start - 1].trim_end();
            let key = key.strip_suffix(':').map(str::trim_end);
            (open..close).contains(&value.content.start)
                && key.is_some_and(|key| &key[scan::name_ending_at(key, key.len())] == "preset")
        });
        let around = statement_around(&code, at);
        // A call with a name in a variable has its place in the chain too.
        let stage = match instrument {
            true => 0,
            false => code[around.start..open]
                .match_indices("vst")
                .filter(|(found, call)| {
                    let at = around.start + found;
                    let before = code[..at].chars().next_back();
                    let named = |char: char| char.is_alphanumeric() || char == '_';
                    let after = &code[at + call.len()..open];
                    !before.is_some_and(named) && after.trim_start().starts_with('(')
                })
                .count(),
        };
        let statement = &code[around];
        let mut orbits = statement.match_indices(".orbit(").map(|(found, call)| {
            let argument = statement[found + call.len()..].trim_start();
            let digits = argument.len()
                - argument
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .len();
            argument[digits..]
                .trim_start()
                .starts_with(')')
                .then(|| argument[..digits].parse::<usize>().ok())
                .flatten()
        });
        let orbit = match (orbits.next(), orbits.next()) {
            (None, _) => Some(1),
            (Some(orbit), None) => orbit,
            _ => None,
        };
        plugins.push(NamedPlugin {
            name: name.to_owned(),
            instrument,
            stage,
            preset: preset.map(|value| source[value.content.clone()].to_owned()),
            orbit,
            keys: object_keys(&code, &literals, open, close),
        });
    }
    plugins
}

/// Whether `at` sits in a lane the engine mutes: a statement labelled by a
/// name starting or ending with `_` (`_$:`, `$_:`, `drums_ :`). The label
/// is read as the transpiler reads one: after any indent, with any space
/// before its colon, or alone on the line above its statement. Inside a
/// call, the lane is the outermost call's, whatever lines its brackets
/// hold. `code` is the score with comments and strings blank.
fn in_muted_lane(code: &str, at: usize) -> bool {
    let at = scan::open_parens(code, at).last().unwrap_or(at);
    let statement = statement_around(code, at).start;
    let above = code[..statement].trim_end();
    let head = if above.ends_with(':') {
        above.rfind('\n').map_or(0, |line| line + 1)
    } else {
        statement
    };
    let label_at = code.len() - code[head..].trim_start().len();
    let label = &code[scan::name_starting_at(code, label_at)];
    let colon = code[label_at + label.len()..].trim_start();
    colon.starts_with(':')
        && !colon.starts_with("::")
        && (label.starts_with('_') || label.ends_with('_'))
}

/// The machines of the bank applying to `literal`, a bank on the chain it
/// heads included, and whether that bank names its machine by anything but
/// a quoted string, which leaves the words to the score. An empty `.bank()`
/// names none.
fn literal_banks(source: &str, code: &str, literal: &Literal) -> (Vec<String>, bool) {
    let head = (!literal.open).then_some(literal.content.end);
    let Some(bank) = applied_bank(code, literal.content.start, head) else {
        return (Vec::new(), false);
    };
    match source
        .as_bytes()
        .get(scan::space_after(source, bank.open + 1))
    {
        Some(b'"' | b'\'') => (
            crate::sounds::string_argument(&source[bank.open + 1..])
                .map(|pattern| crate::sounds::bank_pattern_machines(&pattern))
                .unwrap_or_default(),
            false,
        ),
        Some(b')') => (Vec::new(), false),
        _ => (Vec::new(), true),
    }
}

/// The lines of the statement a position sits in: its own line, the lines
/// above that it continues (a chain broken after a `.`, a call left open on
/// `(` or `,`) and the lines below that continue it. A blank line, as a
/// comment's line is in [`code_only`] text, sits inside a statement without
/// ending it. Enough to find the `.osc()` a sound string is routed through
/// without parsing the score, and no further, so a route on another `$:`
/// line stays that line's.
fn statement_around(source: &str, at: usize) -> std::ops::Range<usize> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        lines.push((start, start + line.trim_end_matches('\n').len()));
        start += line.len();
    }
    if lines.is_empty() {
        return 0..0;
    }
    let text = |index: usize| &source[lines[index].0..lines[index].1];
    let opens_below = |line: &str| {
        let line = line.trim_end();
        line.ends_with('(') || line.ends_with(',') || line.ends_with('.') || line.ends_with("=>")
    };
    let hangs_above = |line: &str| {
        let line = line.trim_start();
        line.starts_with('.') || line.starts_with(')') || line.starts_with(']')
    };
    let at = at.min(source.len());
    let mut first = lines
        .iter()
        .position(|(from, to)| at >= *from && at <= *to)
        .unwrap_or(lines.len() - 1);
    let mut last = first;
    let written = |index: &usize| !text(*index).trim().is_empty();
    while let Some(above) = (0..first).rev().find(written)
        && (hangs_above(text(first)) || opens_below(text(above)))
    {
        first = above;
    }
    while let Some(below) = (last + 1..lines.len()).find(written)
        && (opens_below(text(last)) || hangs_above(text(below)))
    {
        last = below;
    }
    lines[first].0..lines[last].1
}

/// The words of a mini-notation string, each with its tails, or `None` when
/// the string does not parse.
pub fn mini_words_of(content: &str) -> Option<Vec<MiniWord>> {
    let ast = rustel_mini::parse(content).ok()?;
    let mut words = Vec::new();
    mini_words(content, &ast, &[], &mut words);
    Some(words)
}

/// Every word in `ast` with the tails in `tails` appended, innermost first.
/// Operands - a `*2`, the Euclid numbers, a polymeter's step count - are
/// not words the engine resolves by name and are not visited.
fn mini_words(src: &str, ast: &rustel_mini::Ast, tails: &[Tail], out: &mut Vec<MiniWord>) {
    use rustel_mini::Ast;
    match ast {
        Ast::Atom { span, .. } => {
            let from = span.start;
            // An atom's span carries the whitespace after it.
            let to = from + src[from..span.end].trim_end().len();
            let mut text = src[from..to].to_owned();
            let mut direct_to = to;
            let mut tail: Option<(usize, usize)> = None;
            for suffix in tails {
                text.push(':');
                text.push_str(&suffix.text);
                if suffix.direct && tail.is_none() {
                    direct_to = suffix.to;
                } else {
                    tail = Some((tail.map_or(suffix.from, |(start, _)| start), suffix.to));
                }
            }
            out.push(MiniWord {
                text,
                from,
                to: direct_to,
                tail,
                written_index: tails.first().is_some_and(|tail| tail.alone),
                ranged: false,
            });
        }
        Ast::Silence { .. } => {}
        Ast::Seq { items, .. } => {
            for step in items {
                mini_words(src, &step.ast, tails, out);
            }
        }
        Ast::Alt { lanes, .. } => {
            for step in lanes.iter().flat_map(|lane| lane.items.iter()) {
                mini_words(src, &step.ast, tails, out);
            }
        }
        Ast::Stack { items, .. }
        | Ast::Choose { items, .. }
        | Ast::Feet { items, .. }
        | Ast::Polymeter { items, .. } => {
            for item in items {
                mini_words(src, item, tails, out);
            }
        }
        Ast::Replicated { pat, .. }
        | Ast::Degrade { pat, .. }
        | Ast::Fast { pat, .. }
        | Ast::Slow { pat, .. }
        | Ast::Euclid { pat, .. } => mini_words(src, pat, tails, out),
        Ast::Range { start, end, .. } => {
            let from = out.len();
            mini_words(src, start, tails, out);
            mini_words(src, end, tails, out);
            for word in &mut out[from..] {
                word.ranged = true;
            }
        }
        Ast::Tail { pat, element, .. } => {
            let mut hung = Vec::new();
            mini_words(src, element, &[], &mut hung);
            if hung.is_empty() {
                mini_words(src, pat, tails, out);
                return;
            }
            let alone = matches!(**element, Ast::Atom { .. });
            let direct = atom_chain(pat) && alone;
            for word in hung {
                let mut chain = vec![Tail {
                    text: word.text,
                    from: word.from,
                    to: word.to,
                    direct,
                    alone,
                }];
                chain.extend(tails.iter().cloned());
                mini_words(src, pat, &chain, out);
            }
        }
    }
}

/// Mark a shared group tail once when all its words fail, otherwise the
/// individual atom. Offsets stay in the original score, including Unicode.
fn push_word_diagnostics(
    words: &[MiniWord],
    verdicts: &[Option<String>],
    offset: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if diagnostics.len() >= MAX_DIAGNOSTICS {
        return;
    }
    // A tail hung on a group fails for every atom in it: `<C D>:majorr`
    // is marked once, on the tail. An atom that fails on its own
    // (`<C H>:major`) is marked itself.
    let mut marked_tails: Vec<(usize, usize)> = Vec::new();
    for (index, message) in verdicts.iter().enumerate() {
        let Some(message) = message else {
            continue;
        };
        let word = &words[index];
        let (from, to) = match word.tail {
            Some(tail)
                if words
                    .iter()
                    .zip(verdicts)
                    .filter(|(other, _)| other.tail == Some(tail))
                    .all(|(_, verdict)| verdict.is_some()) =>
            {
                if marked_tails.contains(&tail) {
                    continue;
                }
                marked_tails.push(tail);
                tail
            }
            _ => (word.from, word.to),
        };
        diagnostics.push(Diagnostic {
            level: Level::Value,
            message: message.clone(),
            from: offset + from,
            to: offset + to,
        });
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            break;
        }
    }
}

/// A mini-notation word the engine will resolve as written: no rests, no
/// operator characters the tokenizer let through.
fn is_plain_atom(word: &str) -> bool {
    !word.is_empty()
        && word != "~"
        && word != "-"
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ':' | '#' | '_' | '-' | '.' | '^' | '+'))
}

/// The source offsets of every `m('…', offset)` call in transpiled output:
/// the opening quote of each string the transpiler treats as mini-notation.
fn mini_string_offsets(transpiled: &str) -> Vec<usize> {
    let bytes = transpiled.as_bytes();
    let mut offsets = Vec::new();
    let mut index = 0;
    while let Some(found) = transpiled[index..].find("m('") {
        let mut cursor = index + found + 3;
        while cursor < bytes.len() && bytes[cursor] != b'\'' {
            if bytes[cursor] == b'\\' {
                cursor += 1;
            }
            cursor += 1;
        }
        cursor += 1;
        let rest = &transpiled[cursor.min(transpiled.len())..];
        if let Some(rest) = rest.strip_prefix(", ") {
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if let Ok(offset) = digits.parse::<usize>()
                && rest[digits.len()..].starts_with(')')
            {
                offsets.push(offset);
            }
        }
        index = cursor.min(transpiled.len());
    }
    offsets
}

fn clamp_boundary(source: &str, offset: usize) -> usize {
    let mut at = offset.min(source.len());
    while at > 0 && !source.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn next_boundary(source: &str, from: usize) -> usize {
    if from >= source.len() {
        return source.len();
    }
    let mut to = from + 1;
    while to < source.len() && !source.is_char_boundary(to) {
        to += 1;
    }
    to
}

struct Literal {
    /// The text between the quotes.
    content: std::ops::Range<usize>,
    /// Never closed: the string runs to the end of its line, or of the
    /// source - the one being typed.
    open: bool,
}

/// Collect quoted strings for matching mini-notation diagnostics to source
/// locations. Using [`scan_blanked`] keeps quotes inside comments and
/// recognized regex literals from being mistaken for string boundaries.
fn string_literals(source: &str) -> Vec<Literal> {
    let mut literals = Vec::new();
    scan_blanked(source, |_, _, lexeme| {
        if let Lexeme::String(literal) = lexeme {
            literals.push(literal);
        }
    });
    literals
}

/// A `.midi()` option this engine has no use for, noted where it is
/// written: `latencyMs` shifts strudel.cc's MIDI to meet its audio, and
/// here MIDI is timed from the device clock, so the number does nothing.
fn ignored_midi_options(source: &str, diagnostics: &mut Vec<Diagnostic>) {
    // A comment or a string is not a call, and every peer check here scans
    // the blanks-for-text version for exactly that reason: a lane commented
    // out while troubleshooting should not be noted for an option it no
    // longer uses. `code_only` blanks in place, so the offsets it answers
    // with are the source's own and the spans need no adjusting.
    let code = code_only(source);
    let mut from = 0;
    while let Some(at) = code[from..].find(".midi(") {
        let open = from + at + ".midi".len();
        let Some(close) = matching_paren(&code, open) else {
            break;
        };
        if let Some(offset) = code[open..close].find("latencyMs") {
            if diagnostics.len() >= MAX_DIAGNOSTICS {
                return;
            }
            diagnostics.push(Diagnostic {
                level: Level::Note,
                message: "midi: latencyMs is ignored here; MIDI is timed from the device clock"
                    .into(),
                from: open + offset,
                to: open + offset + "latencyMs".len(),
            });
        }
        from = close;
    }
}

#[cfg(test)]
mod tests {
    /// `latencyMs` on `.midi()` is a note at the option, not a refusal:
    /// the score plays, the number does nothing here.
    #[test]
    fn a_midi_latency_option_is_noted_as_ignored() {
        let source = "$: note(\"c a\").midi('IAC', { latencyMs: 34, isController: false })\n";
        let diagnostics = super::lint(source, false, None);
        let note = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("latencyMs"))
            .expect("the option is noted");
        assert_eq!(note.level, super::Level::Note);
        assert_eq!(&source[note.from..note.to], "latencyMs");
        let plain = super::lint("$: note(\"c a\").midi('IAC')\n", false, None);
        assert!(
            plain
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("latencyMs"))
        );
    }

    use super::*;

    #[test]
    fn oscillator_property_on_a_pattern_is_a_value_error() {
        for signal in [
            "sine", "sawtooth", "square", "triangle", "saw", "tri", "supersaw",
        ] {
            for source in [
                format!("note(\"c e g\").{signal}"),
                format!("$: note(\"c e g\").slow(2).{signal}"),
                format!("stack(note(\"c\")).{signal}()"),
            ] {
                let findings = lint(&source, false, None);
                let finding = findings
                    .iter()
                    .find(|finding| finding.message.contains("is not a pattern method"))
                    .unwrap_or_else(|| {
                        panic!("missing oscillator finding for {source}: {findings:?}")
                    });
                assert_eq!(finding.level, Level::Value);
                assert_eq!(&source[finding.from..finding.to], signal);
                assert!(
                    finding.message.contains(&format!(".s(\"{signal}\")")),
                    "{finding:?}"
                );
                assert!(rejection_of(&source, &findings).is_some());
            }
        }
    }

    #[test]
    fn a_global_value_read_off_a_pattern_is_a_value_error() {
        let source = "note(\"c e g\").cosine";
        let findings = lint(source, false, None);
        let finding = findings
            .iter()
            .find(|finding| finding.message.contains("is not a pattern method"))
            .unwrap_or_else(|| panic!("missing finding for {source}: {findings:?}"));
        assert_eq!(&source[finding.from..finding.to], "cosine");
        assert!(!finding.message.contains(".s("), "{finding:?}");
    }

    #[test]
    fn a_method_the_score_registers_is_not_read_as_a_value() {
        let source = "register('saw', (pat) => pat)\nnote(\"c\").saw()";
        assert!(
            lint(source, false, None)
                .iter()
                .all(|finding| !finding.message.contains("is not a pattern method")),
            "false finding for {source}"
        );
    }

    #[test]
    fn a_patterns_own_query_is_a_known_method() {
        let source = "new Pattern(state => pure('x').query(state)).take(2)";
        let findings = lint(source, false, None);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn oscillator_property_check_leaves_other_javascript_properties_alone() {
        for source in [
            "const sound = { sine: 1 }; sound.sine",
            "const x = { note: () => ({ sine: 1 }) }; x.note().sine",
            "const x = { slow: () => ({ sine: 1 }) }; x.slow(2).sine",
            "const note = () => ({ sine: 1 }); note().sine; s('bd')",
            "// note('c').sine\nnote('c').s('sine')",
            "note('c').s('sine')",
        ] {
            assert!(
                lint(source, false, None)
                    .iter()
                    .all(|finding| !finding.message.contains("is not a pattern method")),
                "false oscillator finding for {source}"
            );
        }
    }

    #[test]
    fn oscillator_property_check_respects_the_shadowed_call_scope() {
        let source = "{ const note = () => ({ sine: 1 }); note().sine; }\nnote('c').sine";
        let oscillator_findings = lint(source, false, None)
            .into_iter()
            .filter(|finding| finding.message.contains("is not a pattern method"))
            .collect::<Vec<_>>();
        assert_eq!(oscillator_findings.len(), 1, "{oscillator_findings:?}");
        assert_eq!(oscillator_findings[0].from, source.rfind("sine").unwrap());
    }

    #[test]
    fn single_quoted_pattern_constructor_names_are_checked() {
        let source = "chord('am7').voicing()";
        let findings = lint(source, false, None);
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("unknown chord")),
            "missing unknown chord finding for {source}: {findings:?}"
        );
    }

    /// A score may call what the setup that ran before it defined.
    #[test]
    fn a_score_may_call_what_the_setup_defined() {
        let score = "$: liveHelper().fromSetup()";
        let bare = lint(score, false, None);
        assert!(
            bare.iter()
                .any(|finding| finding.message.contains("unknown function `liveHelper`")),
            "without the setup's names this really is unknown: {bare:?}"
        );
        assert!(rejection(score, false, None).is_some());

        let context = LintContext::from_setup_sources([
            "globalThis.liveHelper = () => note('c')\nregister('fromSetup', (pat) => pat)",
        ]);
        assert!(context.setup_names.iter().any(|name| name == "liveHelper"));
        assert!(
            lint_with(score, false, None, &context).is_empty(),
            "the setup's names were not taken: {:?}",
            lint_with(score, false, None, &context)
        );
        assert!(rejection_with(score, false, None, &context).is_none());
        // register() also installs a raw `_name`, and the checker keeps it.
        assert!(lint_with("$: s(\"bd\")._fromSetup(1)", false, None, &context).is_empty());
    }

    /// A setup whose names cannot be read stops the check rather than
    /// guessing, exactly as a computed register() does.
    #[test]
    fn a_setup_with_names_it_cannot_read_turns_the_unknown_call_check_off() {
        let context = LintContext::from_setup_sources(["Object.assign(globalThis, helpers)"]);
        assert!(context.setup_has_dynamic_names);
        assert!(
            lint_with("$: s(\"bd\").wobblez(2)", false, None, &context).is_empty(),
            "a call was refused against a setup nobody could read"
        );
        // A real syntax error is still a real syntax error.
        assert!(!lint_with("$: s(\"bd [hh\")", false, None, &context).is_empty());
    }

    /// What a prebake may not do is refused in it, in the engine's words.
    #[test]
    fn what_a_prebake_may_not_do_is_refused_in_the_engines_own_words() {
        let source = "setcps(1)\nsetCpm(120)\nmidin('IAC')\nglobalThis.h = 1\n";
        let setup = LintContext::default().as_setup();
        let findings = lint_with(source, false, None, &setup);

        let refused = findings
            .iter()
            .filter(|finding| finding.level == Level::Value)
            .map(|finding| &source[finding.from..finding.to])
            .collect::<Vec<_>>();
        assert_eq!(refused, ["setcps", "setCpm", "midin"]);
        assert!(
            findings[0]
                .message
                .contains(rustel_jsruntime::TEMPO_SCOPE_POLICY),
            "the refusal invented its own words: {:?}",
            findings[0].message
        );
        assert!(
            findings[2]
                .message
                .contains(rustel_jsruntime::MIDI_INPUT_SCOPE_POLICY)
        );
        assert_eq!(
            rejection_with(source, false, None, &setup).as_deref(),
            Some(
                format!(
                    "line 1: `setcps` cannot run in a prebake - {}",
                    rustel_jsruntime::TEMPO_SCOPE_POLICY
                )
                .as_str()
            )
        );

        // A score may do all of it.
        assert!(
            lint(source, false, None)
                .iter()
                .all(|finding| !finding.message.contains("cannot run in a prebake")),
            "a score was held to the setup rules"
        );
    }

    /// Only real calls count: not a comment, not a method of something
    /// else, and not the setup's own function of the same name.
    #[test]
    fn a_mentioned_name_is_not_a_refused_call() {
        let setup = LintContext::default().as_setup();
        for source in [
            "// setcps(1) is refused here\n",
            "globalThis.note = 'setcps(1)'\n",
            "clock.setCps(1)\n",
            "function setcps(x) { return x }\nsetcps(1)\n",
            "globalThis.tempo = setcps\n",
        ] {
            assert!(
                lint_with(source, false, None, &setup)
                    .iter()
                    .all(|finding| !finding.message.contains("cannot run in a prebake")),
                "a mention was read as a call: {source}"
            );
        }
    }

    /// A top-level declaration in a setup is a note: it reads like a
    /// definition and reaches no score.
    #[test]
    fn a_top_level_declaration_in_a_prebake_is_a_note_not_a_refusal() {
        let source =
            "const helper = 1\nfunction f() {}\nglobalThis.f = f\nglobalThis.g = () => helper\n";
        let setup = LintContext::default().as_setup();
        let findings = lint_with(source, false, None, &setup);

        assert_eq!(findings.len(), 1, "only the unshared one: {findings:?}");
        assert_eq!(findings[0].level, Level::Note);
        assert_eq!(&source[findings[0].from..findings[0].to], "helper");
        assert!(findings[0].message.contains("globalThis.helper"));
        assert!(
            rejection_with(source, false, None, &setup).is_none(),
            "a note refused a prebake"
        );
        // The same text as a score says nothing of the kind.
        assert!(lint(source, false, None).is_empty());
    }

    #[test]
    fn a_reason_from_findings_already_in_hand_matches_the_one_that_checks_again() {
        let source = "$: s(\"bd\")\n$: n(\"0\").scale(\"C:nope\")";
        assert_eq!(
            rejection_of(source, &lint(source, false, None)),
            rejection(source, false, None)
        );
    }

    fn check(source: &str) -> Vec<Diagnostic> {
        lint(source, false, None)
    }

    /// A port that is not plugged in is worth saying, because nothing else
    /// says it: the platform's own "not found" report reaches the CLI's
    /// live loop and nowhere else, so in the studio a mistyped port name
    /// is silence with no explanation.
    #[test]
    fn a_midi_port_this_machine_does_not_have_is_named() {
        let context = LintContext {
            midi_inputs: Some(vec![
                "Bass Station II".to_owned(),
                "Arturia KeyStep 32".to_owned(),
            ]),
            midi_outputs: Some(vec!["IAC Driver Bus 1".to_owned()]),
            ..LintContext::default()
        };
        let checked = |source: &str| lint_with(source, false, None, &context);

        let found = checked("const keys = await midikeys('Bass Ststion II')\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].level, Level::Value);
        assert!(
            found[0].message.contains("no MIDI input")
                && found[0].message.contains("Bass Station II"),
            "{}",
            found[0].message
        );

        // A name nothing is near gets the list instead of a guess.
        let found = checked("const keys = await midikeys('Moog Subsequent 37')\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("plugged in:"), "{found:?}");

        // The platform accepts an index or a case-insensitive substring, so
        // an exact name, an abbreviation and an index are not flagged.
        for good in [
            "const k = await midikeys('Bass Station II')\n",
            "const k = await midikeys('Arturia')\n",
            "const k = await midikeys('Station')\n",
            "const k = await midikeys('bass station ii')\n",
            "const k = await midin('1')\n",
            "$: note(\"c\").midi('IAC Driver Bus 1')\n",
        ] {
            assert!(checked(good).is_empty(), "flagged a working name: {good}");
        }

        // An empty name opens nothing at the platform, so it is flagged.
        for empty in [
            "const k = await midin('')\n",
            "const k = await midikeys('   ')\n",
            "$: note(\"c\").midi('')\n",
        ] {
            let found = checked(empty);
            assert_eq!(found.len(), 1, "{empty}: {found:?}");
            assert!(found[0].message.contains("empty MIDI"), "{found:?}");
        }

        // An output name is judged against the outputs, not the inputs.
        let found = checked("$: note(\"c\").midi('Bass Station II')\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("no MIDI output"), "{found:?}");

        // An index past the end is a real failure at the platform too.
        assert_eq!(checked("const k = await midin('9')\n").len(), 1);

        // And an unprobed machine judges nothing, the way an empty sample
        // library refuses to call a sound unknown.
        assert!(
            lint_with(
                "const k = await midikeys('anything at all')\n",
                false,
                None,
                &LintContext::default(),
            )
            .is_empty()
        );
    }

    /// A slider whose numbers cannot be a control is reported like any other
    /// score error. `slider(0, 0.1, 0.1, 1)` is written as if min came first.
    #[test]
    fn a_slider_with_impossible_numbers_is_refused_with_its_fault_named() {
        let source = "$: s(\"supersaw\").velocity(slider(0, 0.1, 0.1, 1))";
        let diagnostics = check(source);
        let finding = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("slider"))
            .expect("the slider is a finding");
        assert_eq!(finding.level, Level::Value);
        assert!(
            finding.message.contains("min 0.1 is not below max 0.1"),
            "{}",
            finding.message
        );
        assert!(
            finding.message.contains("slider(value, min?, max?, step?)"),
            "the signature is the correction: {}",
            finding.message
        );
        // The mark sits on the slider's own bytes, not somewhere nearby.
        assert!(
            source[finding.from..finding.to].starts_with('0'),
            "marks the literal: {:?}",
            &source[finding.from..finding.to]
        );

        // Each way the numbers can be impossible is named for what it is.
        let value_out = check("$: s(\"bd\").gain(slider(2, 0, 1))");
        assert!(
            value_out
                .iter()
                .any(|d| d.message.contains("value 2 is outside 0 to 1")),
            "{value_out:?}"
        );
        let bad_step = check("$: s(\"bd\").gain(slider(0.5, 0, 1, 0))");
        assert!(
            bad_step
                .iter()
                .any(|d| d.message.contains("step 0 is not above zero")),
            "{bad_step:?}"
        );
    }

    /// The rule judges only what it can read: a slider with an expression in
    /// it keeps its runtime meaning, and a well-formed one is left alone.
    #[test]
    fn sliders_the_rule_cannot_read_or_fault_are_not_findings() {
        let sliders = |source: &str| {
            check(source)
                .into_iter()
                .filter(|d| d.message.contains("slider"))
                .collect::<Vec<_>>()
        };
        assert_eq!(sliders("$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))"), []);
        assert_eq!(sliders("$: s(\"bd\").gain(slider(0.5))"), []);
        let x = "let x = 2\n$: s(\"bd\").gain(slider(x, 0, 1))";
        assert_eq!(sliders(x), []);
    }

    /// A score that forgot its first line names the line it forgot.
    ///
    /// Without this the nearest real name to `contrast` is `contract`, a
    /// stepwise function with nothing to do with visuals, and the refusal
    /// sends the reader off to fix a spelling that was never wrong.
    #[cfg(feature = "hydra")]
    #[test]
    fn a_hydra_name_without_init_hydra_says_which_line_is_missing() {
        let source = "osc(10).contrast(1.5).out(o0)";
        let diagnostics = check(source);
        let contrast = diagnostics
            .iter()
            .find(|diagnostic| &source[diagnostic.from..diagnostic.to] == "contrast")
            .expect("`contrast` is a finding");
        assert_eq!(
            contrast.message,
            "`contrast` is a Hydra function - did you forget `await initHydra()` at the top?"
        );

        // With the line in place the whole vocabulary is known and there is
        // nothing to say.
        let opened = "await initHydra()\nosc(10).contrast(1.5).out(o0)";
        assert!(
            !check(opened)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("contrast")),
            "{:?}",
            check(opened)
        );

        // A mention in a comment is not a call, and does not open Hydra.
        let commented = "// await initHydra()\nosc(10).contrast(1.5).out(o0)";
        assert!(
            check(commented)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("did you forget")),
            "{:?}",
            check(commented)
        );
    }

    /// Hydra's array modifiers are known names in a score that opens Hydra.
    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_array_modifiers_are_known_in_a_hydra_score() {
        let source = "await initHydra()\nosc([10,30].fit(0,1).smooth(0.5).offset(0.25).ease('sin').fast(2)).out(o0)";
        assert!(check(source).is_empty(), "{:?}", check(source));
    }

    #[test]
    fn a_call_to_a_name_nothing_answers_to_is_a_finding_with_the_nearest_name() {
        let source = "$: s(\"bd\").lpff(800).gian(.5)";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert_eq!(diagnostics[0].level, Level::Value);
        assert_eq!(
            diagnostics[0].message,
            "unknown function `lpff` - did you mean `lpf`?"
        );
        assert_eq!(&source[diagnostics[0].from..diagnostics[0].to], "lpff");
        assert_eq!(
            diagnostics[1].message,
            "unknown function `gian` - did you mean `gain`?"
        );
        assert!(rejection(source, false, None).unwrap().contains("lpff"));
    }

    #[test]
    fn stray_identifiers_are_reported_without_evaluating_the_score() {
        let source = r#"// Type normally. Use the Play menu to update or stop.
_$: s("[bd <hh oh>]*2").bank("tr909").dec(.4)._pianoroll()
_$: note("<[60,64,67,69] [62,65,67,71] [62,65,69,72] [62,64,67,71]>").slow(2).s("basique").bank("wt_digital")
    .wt(0.377).wtrate(0.12).wtdepth(0.278)
    .warp(0.232).warpmode("spin")
    .lpf(sine.range(1741, 2795).slow(16))
    .attack(0.9).decay(0.5).sustain(0.6).release(1.8)
    .pan(sine.range(0.28, 0.72).slow(11)).gain(0.09).room(0.410).delay(0.227).delaytime(0.375).delayfeedback(0.3)
$: note("[57 60] ~ 59 57 ~ [57 53] 53 60").slow(2).s("sine")
    .fm(5.05).fmh(1.98).fmdecay(0.09)
    .attack(0.002).decay(0.16).sustain(0).release(0.12)
    .lpf(10000).pan("0.28 0.72").gain(0.2).room(0.410).delay(0.227).delaytime(0.375).delayfeedback(0.3)
    .lpattack(100)

    kaskaskjas
"#;
        let findings = check(source);
        let finding = findings
            .iter()
            .find(|d| d.message == "unknown name `kaskaskjas`")
            .unwrap_or_else(|| panic!("{findings:?}"));
        assert_eq!(&source[finding.from..finding.to], "kaskaskjas");
        assert!(
            !check(source.replace("kaskaskjas", "silence").as_str())
                .iter()
                .any(|d| d.message.starts_with("unknown name"))
        );
    }

    #[test]
    fn identifier_lint_respects_scopes_properties_and_shared_names() {
        let source = r#"
const { value: local, ...rest } = { value: 2 };
function helper({ value }, [other]) { return value + other + (() => arguments.length)(); }
const obj = { local, property: local, method(arg) { return arg; } };
try { obj.property; } catch (error) { error.message; }
globalThis.shared = local;
shared;
typeof (optionalHelper);
const café = local;
café;
$: note("c").gain(local).sometimes(pat => pat.rev())
"#;
        assert!(check(source).is_empty(), "{:?}", check(source));
        let source = "{ const scopedValue = 1; scopedValue; }\nscopedValue\nconst obj = { absent };\n`value ${missing}`\n(() => arguments)()";
        let names: Vec<_> = check(source)
            .into_iter()
            .filter(|d| d.message.starts_with("unknown name"))
            .map(|d| source[d.from..d.to].to_owned())
            .collect();
        assert_eq!(names, ["scopedValue", "absent", "missing", "arguments"]);
        let context = LintContext::from_setup_sources(["globalThis.customValue = 0.5"]);
        assert!(lint_with("$: s('bd').gain(customValue)", false, None, &context).is_empty());
    }

    /// `log` is a method on a pattern. A bare `log(x)` inside a callback
    /// throws `log is not defined` at query time; the checker used to treat
    /// prototype names as free functions, so the score installed and the
    /// filter fail-opened.
    #[test]
    fn a_pattern_method_used_as_a_free_function_is_a_finding() {
        let source = "$: s(\"bd\").filterValues(x => log(x))";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].level, Level::Value);
        assert_eq!(&source[diagnostics[0].from..diagnostics[0].to], "log");
        assert_eq!(
            diagnostics[0].message,
            "`log` is a pattern method - write `.log()`, not `log(…)`"
        );
        assert!(
            rejection(source, false, None)
                .unwrap()
                .contains("pattern method")
        );

        assert!(
            check("$: s(\"bd\").log()").is_empty(),
            "the method form is the valid one: {:?}",
            check("$: s(\"bd\").log()")
        );
        assert!(
            check("const log = (x) => x\n$: s(\"bd\").filterValues(x => log(x))").is_empty(),
            "a local of the same name is the score's: {:?}",
            check("const log = (x) => x\n$: s(\"bd\").filterValues(x => log(x))")
        );
        assert!(
            check("$: s(\"bd\").gain(0.5)").is_empty(),
            "a control that is also a free function must still be: {:?}",
            check("$: s(\"bd\").gain(0.5)")
        );
        let logged = check("console.log(1)\n$: s(\"bd\")");
        assert!(
            logged
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("`log` is a pattern method")),
            "{logged:?}"
        );
    }

    /// A free call of `scrub`, `slice` or `chunkInto`, whole or curried, is
    /// not a finding; a free `plyWith(…)` is.
    #[test]
    fn an_upstream_exported_curried_function_used_free_is_not_a_finding() {
        for source in [
            r#"$: s("bd").sometimesBy(1, scrub("0.5"))"#,
            r#"$: scrub("0.5", s("bd"))"#,
            r#"$: s("bd").sometimesBy(1, slice(4, "0 2"))"#,
            r#"$: s("bd sd ht lt").sometimesBy(.5, chunkInto(4, hurry(2)))"#,
            r#"$: chunkinto(4, p => p.rev(), s("bd sd ht lt"))"#,
        ] {
            assert!(check(source).is_empty(), "{source}: {:?}", check(source));
        }

        let source = r#"$: s("bd").sometimesBy(1, plyWith(2, p => p.rev()))"#;
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(&source[diagnostics[0].from..diagnostics[0].to], "plyWith");
        assert_eq!(
            diagnostics[0].message,
            "`plyWith` is a pattern method - write `.plyWith()`, not `plyWith(…)`"
        );
    }

    /// `trancearp` is the prebake's global builder, not a pattern method:
    /// `s("piano").trancearp(…)` threw `TypeError: not a function` at the
    /// update, and the checker had nothing to say because `trancearp` is a
    /// known name. The mirror of the bare-`log` footgun.
    #[cfg(feature = "extensions")]
    #[test]
    fn a_global_builder_used_as_a_pattern_method_is_a_finding() {
        let source = "$: s(\"piano*8\").trancearp(['c','e','g','b'], 0, 0)";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].level, Level::Value);
        assert_eq!(&source[diagnostics[0].from..diagnostics[0].to], "trancearp");
        assert_eq!(
            diagnostics[0].message,
            "`trancearp` builds a pattern of its own - write `trancearp(…)` first, not `.trancearp(…)`, and chain the rest after it"
        );

        let global = "$: trancearp(['c','e','g','b'], 0, 0).s(\"piano*8\")";
        assert!(
            check(global).is_empty(),
            "the free-function form is the valid one: {:?}",
            check(global)
        );
        let own = "register('trancearp', (n, pat) => pat)\n$: s(\"bd\").trancearp(1)";
        assert!(
            check(own).is_empty(),
            "a registration of the same name is the score's: {:?}",
            check(own)
        );
        let method = "$: s(\"bd\").noisehat()";
        assert!(
            check(method).is_empty(),
            "a builder that also has a method form keeps it: {:?}",
            check(method)
        );
    }

    /// `filterValues` receives a hap VALUE. `.log()` on that value throws
    /// `not a function`; the checker used to allow it because `log` is a
    /// known pattern method.
    #[test]
    fn a_pattern_method_on_a_filtervalues_param_is_a_finding() {
        let source = r#"$: s("bd").filterValues(x => x.log()).s("hh:4")"#;
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].level, Level::Value);
        assert_eq!(&source[diagnostics[0].from..diagnostics[0].to], "log");
        assert!(
            diagnostics[0]
                .message
                .contains("not `x.log()` inside `filterValues`"),
            "{}",
            diagnostics[0].message
        );
        assert!(rejection(source, false, None).is_some());
        let midi = r#"const keys = await midikeys('keyboard')
$: keys(".5").filterValues(x => x.log()).s("hh:4")"#;
        assert!(
            check(midi)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("not `x.log()`")),
            "{:?}",
            check(midi)
        );

        assert!(
            check(r#"$: s("bd").filterValues(v => v.s === "bd")"#).is_empty(),
            "reading a value field is valid: {:?}",
            check(r#"$: s("bd").filterValues(v => v.s === "bd")"#)
        );
        assert!(
            check("$: s(\"bd\").every(2, x => x.fast(2))").is_empty(),
            "every's callback receives a pattern: {:?}",
            check("$: s(\"bd\").every(2, x => x.fast(2))")
        );
        assert!(
            check("$: s(\"bd\").log()").is_empty(),
            "{:?}",
            check("$: s(\"bd\").log()")
        );
    }

    /// A multi-byte character in a value callback's parameter must not make
    /// the scanner slice mid-character: the score lints to an ordinary
    /// answer. The `function`-spelling check inside the callback scan used
    /// to panic on this source, because the parameter's `é` bytes fell
    /// inside the eight-byte window it compared.
    #[test]
    fn a_multi_byte_param_in_a_value_callback_is_linted_not_panicked() {
        let source = "$: s(\"bd\").filterValues(aaaaaéé => x)";
        let diagnostics = check(source);
        for diagnostic in &diagnostics {
            assert!(
                source.is_char_boundary(diagnostic.from)
                    && source.is_char_boundary(diagnostic.to)
                    && diagnostic.from <= diagnostic.to
                    && diagnostic.to <= source.len(),
                "{source:?}: a span cut mid-character: {diagnostic:?}"
            );
        }
        // The function spelling walks the same window first. Multi-byte
        // text in its body, outside any string the scan blanks, must neither
        // panic it nor hide the finding after it, and the finding's span
        // must still land on the method.
        let spelled_out =
            r#"$: s("bd").filterValues(function (v) { const café = 1; return v.log() })"#;
        let diagnostics = check(spelled_out);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(&spelled_out[diagnostics[0].from..diagnostics[0].to], "log");
        assert!(
            diagnostics[0]
                .message
                .contains("not `v.log()` inside `filterValues`"),
            "{}",
            diagnostics[0].message
        );
    }

    /// A value callback's parameter is the name its first parameter opens with,
    /// so a default still names `x`. A destructured first parameter names
    /// nothing, and a second one is not read.
    #[test]
    fn a_value_callbacks_parameter_is_read_up_to_its_default() {
        let source = r#"$: s("bd").filterValues((x = 1) => x.log())"#;
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(&source[diagnostics[0].from..diagnostics[0].to], "log");
        for source in [
            r#"$: s("bd").filterValues(({ x }) => x.log())"#,
            r#"$: s("bd").filterValues((y, x) => x.log())"#,
        ] {
            assert!(check(source).is_empty(), "{source}: {:?}", check(source));
        }
    }

    #[test]
    fn score_level_register_names_are_valid_globals_methods_and_raw_methods() {
        let source = r#"
            register('inspire', (_scale, density, octaves, seed, bars, x) => {
              return x.n(rand.range(0, pure(12).mul(octaves)))
                .scale(_scale)
                .sometimesBy(pure(1).sub(density), x => x.mask(rand.round()))
                .early(rand2.range(-0.001, 0.001))
                .rev()
                .rib(seed, bars)
            })
            register(['bloom', 'flower'], (x) => x)
            $: s("piano").seg(8).inspire("<ab:major>", 0.4, 2, 10, 4)
            $: pure(1).bloom()
            $: flower(pure(1))._inspire('ab:major', 0.4, 2, 10, 4)
        "#;
        assert!(check(source).is_empty(), "{:?}", check(source));
        assert!(rejection(source, false, None).is_none());
    }

    #[test]
    fn a_method_on_something_the_score_owns_is_not_read_as_an_engine_name() {
        // A local, a member chain, and `window` - the global object under
        // another name. None of them is the engine's, so what is called on
        // them is not the engine's to judge.
        let source = r#"
            const document = { getElementById: () => ({}), createElement: () => ({}) }
            const node = { summingNode: { gain: {} } }
            const box = document.getElementById('x')
            const slider = document.createElement('input')
            slider.addEventListener('input', (e) => {})
            window.thing = 1
            node.summingNode.gain.setValueAtTime(0.5, 0)
            $: s("bd")
        "#;
        assert!(check(source).is_empty(), "{:?}", check(source));
    }

    #[test]
    fn a_local_variable_named_after_a_function_is_the_scores_own() {
        // `slider` is an engine function and, here, a local variable. The
        // declaration wins: what is called on it is not the engine's to
        // judge.
        let source = "const slider = 1\nslider.notAnEngineMethod()\n$: s(\"bd\")";
        assert!(check(source).is_empty(), "{:?}", check(source));
    }

    #[test]
    fn a_kabelsalat_graph_is_reported_before_the_score_is_played() {
        // The transpiler rewrites `K(expr)` into `worklet('expr')`, so the body
        // never reaches the name scan as code, and the scan skips the
        // capitalised `K`. Without this, `check` reports nothing and the score
        // refuses only at evaluation.
        let source = "$: s(\"bd\").FX(K(() => audioin().out()))";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].message.contains("kabelsalat"));
        assert_eq!(&source[diagnostics[0].from..diagnostics[0].to], "K");
        // A refusal, not a note: the score genuinely will not run.
        assert!(rejection(source, false, None).is_some());
    }

    #[test]
    fn a_kabelsalat_call_on_a_receiver_is_reported_like_a_bare_one() {
        // Pins that a call on a receiver is reported and a `K(` in a comment
        // is not.
        let source = "$: s(\"bd\").K(osc(2))\n// K(not code)";
        let member = source.find(".K(").expect("the member call") + 1;
        let kabelsalat = check(source)
            .into_iter()
            .filter(|d| d.message.contains("kabelsalat"))
            .map(|d| (d.from, d.to))
            .collect::<Vec<_>>();
        assert_eq!(kabelsalat, [(member, member + 1)], "{:?}", check(source));
        assert!(rejection(source, false, None).is_some());
    }

    /// The checker reports only the size refusal for a source past the
    /// structural limit, even with a code `/` in it.
    #[test]
    fn an_oversized_source_with_a_division_is_refused() {
        let oversized = format!(
            "1/2;{}",
            "[".repeat(rustel_transpiler::MAX_STRUCTURAL_BYTES + 1)
        );
        let diagnostics = check(&oversized);
        let refusal =
            rustel_transpiler::check_nesting(&oversized).expect_err("over the size limit");
        assert_eq!(
            diagnostics
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>(),
            [refusal.message.as_str()]
        );
    }

    #[test]
    fn a_kabelsalat_call_in_another_ones_later_arguments_is_reported_too() {
        let source = "$: K(a, K(b))";
        let kabelsalat = check(source)
            .into_iter()
            .filter(|d| d.message.contains("kabelsalat"))
            .map(|d| d.from)
            .collect::<Vec<_>>();
        assert_eq!(kabelsalat, [3, 8], "{:?}", check(source));
    }

    #[test]
    fn a_kabelsalat_graph_is_one_refusal_and_no_unknown_name_or_function() {
        for source in ["$: K(K(x))", "$: K(foo(x))"] {
            let diagnostics = check(source);
            assert_eq!(
                diagnostics
                    .iter()
                    .filter(|d| d.message.contains("kabelsalat"))
                    .count(),
                1,
                "{source}: {diagnostics:?}"
            );
            assert!(
                !diagnostics.iter().any(|d| d.message.starts_with("unknown")),
                "{source}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn a_capital_k_that_is_not_a_call_is_left_alone() {
        for source in [
            "const K = 1\n$: s(\"bd\").gain(K)",
            "$: s(\"bd\").gain(myK)",
            "$: s(\"bd\").gain(obj.K)",
        ] {
            assert!(
                !check(source)
                    .iter()
                    .any(|d| d.message.contains("kabelsalat")),
                "{source}: {:?}",
                check(source)
            );
        }
    }

    #[test]
    fn dynamic_register_names_are_not_guessed_and_static_typos_still_are() {
        let dynamic = r#"
            const effectName = ['computed', 'Fx'].join('')
            register(effectName, (pat) => pat)
            $: pure(1).computedFx()
        "#;
        assert!(check(dynamic).is_empty(), "{:?}", check(dynamic));

        let typo = "register('inspire', (pat) => pat)\n$: pure(1).inpsire()";
        let diagnostics = check(typo);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].message,
            "unknown function `inpsire` - did you mean `inspire`?"
        );
    }

    /// A tagged template is a call, so an unknown tag is an unknown call:
    /// `` tidal`c4` `` is refused as `tidal("c4")` is.
    #[test]
    fn an_unknown_tagged_template_is_an_unknown_call() {
        for source in ["$: tidal`c4 e4`", "$: mondo`s hh*8`"] {
            let findings = check(source);
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert!(
                findings[0].message.contains("unknown function"),
                "{source}: {:?}",
                findings[0].message
            );
        }
        // Backticks the engine DOES know are still fine, and so is an
        // ordinary template that tags nothing.
        assert!(
            check("$: s(`bd*4`)").is_empty(),
            "{:?}",
            check("$: s(`bd*4`)")
        );
        let own = "const kit = (strings) => s(\"bd\")\n$: kit`anything`";
        assert!(check(own).is_empty(), "{:?}", check(own));
    }

    #[test]
    fn function_declarations_and_methods_are_not_unknown_calls() {
        // ES function declaration, a class method, and an object-literal
        // method are the score's own - never "unknown function".
        let source = "function swell(p) { return p.lpf(800) }\n\
                      class Kit { hit(n) { return s(\"bd\").fast(n) } }\n\
                      const fx = { wobble(p) { return p.room(.3) } }\n\
                      $: swell(s(\"bd\"))";
        assert!(check(source).is_empty(), "{:?}", check(source));
        // A genuine unknown call is still caught.
        assert_eq!(check("$: s(\"bd\").wobblez(2)").len(), 1);
    }

    #[test]
    fn the_scores_own_functions_and_javascripts_are_never_questioned() {
        let source = "const swell = (p) => p.lpf(sine.range(200, 2000))\n\
                      const beat = 2\n\
                      let mix = { dry: 0.5 }\n\
                      $: s(\"bd\").fast(Math.floor(beat)).gain([1, 2].map(x => x / 2)[0])\n\
                      $: swell(note(\"c3\")).room(mix.dry)\n\
                      $: console.log(JSON.stringify(mix))\n\
                      $: s(\"hh\").fast(2e3 > 1 ? 2 : 1)\n\
                      // lpff(1)\n\
                      $: s(\"sd\")._punchcard()";
        assert!(check(source).is_empty(), "{:?}", check(source));
    }

    #[test]
    fn the_engine_knows_its_own_globals_and_methods() {
        let known = known_names();
        for name in [
            "samples",
            "addVoicings",
            "setDefaultVoicings",
            "setcps",
            "setCpm",
            "hush",
            "slider",
            "stack",
            "cat",
            "seq",
            "arrange",
            "all",
            "note",
            "s",
            "lpf",
            "gain",
            "fast",
            "scale",
            "chord",
            "voicing",
            "bank",
            "punchcard",
            "_punchcard",
            "pianoroll",
            "scope",
            "sine",
            "irand",
            "choose",
            "sometimesBy",
            "euclid",
            "loadSoundfont",
            "registerSynthSounds",
        ] {
            assert!(known.contains(name), "{name} should be known");
        }
        #[cfg(feature = "extensions")]
        {
            for name in rustel_ext::default_registry().names() {
                assert!(
                    known.contains(name),
                    "extension pattern `{name}` should be known"
                );
            }
        }
        assert!(!known.contains("lpff"));
    }

    #[cfg(not(feature = "extensions"))]
    #[test]
    fn an_explicit_minimal_build_has_no_extension_names() {
        let known = known_names();
        for name in ["inspire", "acidenv", "filtval", "grab"] {
            assert!(!known.contains(name), "{name} should require extensions");
        }
    }

    #[test]
    fn the_closest_name_is_one_slip_away() {
        let names = ["lpf", "hpf", "gain", "fast", "slow", "note", "n", "room"];
        assert_eq!(closest("lpff", names.iter().copied()), Some("lpf"));
        assert_eq!(closest("gian", names.iter().copied()), Some("gain"));
        assert_eq!(
            closest("Gain", names.iter().copied()),
            None,
            "exact, ignoring case"
        );
        assert_eq!(closest("reverb", names.iter().copied()), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("ab", "ba"), 1);
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
        assert_eq!(levenshtein_distance("ab", "ba"), 2);
    }

    #[test]
    fn a_clean_score_has_nothing_to_say() {
        assert!(check("$: s(\"bd hh\").lpf(800)\n$: note(\"c3 e3\")").is_empty());
        assert!(lint("bd hh sd hh", true, None).is_empty());
        assert!(check("").is_empty());
    }

    #[test]
    fn a_javascript_syntax_error_is_placed_where_the_parser_stopped() {
        let source = "$: s(\"bd\").lpf(\n$: note(\"c3\")";
        let diagnostics = check(source);
        assert!(!diagnostics.is_empty());
        let first = &diagnostics[0];
        assert_eq!(first.level, Level::Syntax);
        assert!(first.from < source.len(), "{first:?}");
        assert!(first.to > first.from);
        assert!(source.is_char_boundary(first.from) && source.is_char_boundary(first.to));
        assert!(!first.message.is_empty());
    }

    #[test]
    fn a_broken_mini_notation_string_is_underlined_inside_its_quotes() {
        let source = "$: s(\"bd [hh sd\").gain(.5)";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let quote = source.find('"').unwrap();
        let close = source.rfind('"').unwrap();
        assert!(
            diagnostics[0].from > quote && diagnostics[0].to <= close,
            "{diagnostics:?}"
        );
        assert_eq!(
            diagnostics[0].to, close,
            "the underline runs to the end of the string"
        );
        assert!(
            diagnostics[0].message.contains("mini"),
            "{}",
            diagnostics[0].message
        );
    }

    #[test]
    fn an_unknown_scale_name_is_reported_with_the_engines_own_message() {
        let source = "n(\"0 2 4\").scale(\"<C:major D:majorr>\")";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let bad = source.find("D:majorr").unwrap();
        assert_eq!(
            (diagnostics[0].from, diagnostics[0].to),
            (bad, bad + "D:majorr".len())
        );
        assert_eq!(diagnostics[0].level, Level::Value);
        assert!(
            diagnostics[0].message.contains("Invalid scale name"),
            "{}",
            diagnostics[0].message
        );
        assert!(check("n(\"0 2\").scale(\"C:major E:minor pentatonic\")").is_empty());
        assert!(check("n(\"0 2\").scale(\"C4:dorian ~ G:lydian*2\")").is_empty());
        // The same words in an unrelated call are not scales.
        assert!(check("lpf(\"D:majorr\")").is_empty());
    }

    /// strudel.cc's visual-feedback page opens with a scale hung on a
    /// group, `<A1 D2>/4:minor:pentatonic`, which the engine plays as
    /// `A1:minor:pentatonic` and `D2:minor:pentatonic`. Read token by token
    /// it was two notes and a word, and refused.
    #[test]
    fn a_scale_tail_hung_on_a_group_applies_to_every_atom_in_it() {
        let highlighted = "n(\"<0 2 1 3 2>*8\")\n\
.scale(\"<A1 D2>/4:minor:pentatonic\")\n\
.s(\"supersaw\").lpf(300).lpenv(\"<4 3 2>*4\")";
        assert!(check(highlighted).is_empty(), "{:?}", check(highlighted));
        let coloured = "n(\"<0 2 1 3 2>*8\")\n\
.scale(\"<A1 D2>/4:minor:pentatonic\")\n\
.s(\"supersaw\").lpf(300).lpenv(\"<4 3 2>*4\")\n\
.color(\"cyan magenta\")";
        assert!(check(coloured).is_empty(), "{:?}", check(coloured));
        for accepted in [
            "n(\"0\").scale(\"[C D]:major\")",
            "n(\"0\").scale(\"<C D>/4:major\")",
            "n(\"0\").scale(\"<C D>*2:major:pentatonic\")",
            "n(\"0\").scale(\"C:<major minor>\")",
            "n(\"0\").scale(\"[C:major D:minor]:pentatonic\")",
        ] {
            assert!(
                check(accepted).is_empty(),
                "{accepted}: {:?}",
                check(accepted)
            );
        }

        // A tail the whole group shares is marked once, on the tail.
        let source = "n(\"0\").scale(\"<C D>:majorr\")";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let tail = source.find("majorr").unwrap();
        assert_eq!(
            (diagnostics[0].from, diagnostics[0].to),
            (tail, tail + "majorr".len())
        );
        assert!(diagnostics[0].message.contains("Invalid scale name"));
        let source = "n(\"0\").scale(\"C:<major minorr>\")";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let tail = source.find("minorr").unwrap();
        assert_eq!(
            (diagnostics[0].from, diagnostics[0].to),
            (tail, tail + "minorr".len())
        );
        // An atom that fails on its own is marked itself.
        let source = "n(\"0\").scale(\"<C H>:major\")";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let atom = source.find("H>").unwrap();
        assert_eq!((diagnostics[0].from, diagnostics[0].to), (atom, atom + 1));
    }

    #[test]
    fn a_word_that_is_not_a_note_is_reported_in_note_strings() {
        let source = "note(\"c3 h3 [e3,g3]!2 <a3 b3?0.5> 60 ~\")";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let bad = source.find("h3").unwrap();
        assert_eq!((diagnostics[0].from, diagnostics[0].to), (bad, bad + 2));
        assert!(check("note(\"c3 eb4 f#2 bs-1 62.5\")").is_empty());
    }

    #[test]
    fn unknown_chords_are_reported_from_the_voicing_dictionaries() {
        let source = "chord(\"<C^7 Dm7 Gmajorr>\").voicing()";
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let bad = source.find("Gmajorr").unwrap();
        assert_eq!((diagnostics[0].from, diagnostics[0].to), (bad, bad + 7));
        assert!(diagnostics[0].message.contains("Gmajorr"));
        // A string the voicing is called on directly is a chord too.
        let direct = "\"<C^7 Dm7 Gmajorr>\".voicing()";
        assert_eq!(check(direct).len(), 1, "{:?}", check(direct));
        // A score that adds its own voicings is not second-guessed.
        assert!(check("addVoicings('mine', {})\nchord(\"Gmajorr\").voicing()").is_empty());
        // A score that picks a dictionary is checked against all of them.
        assert!(check("chord(\"Cm7\").dict('lefthand').voicing()").is_empty());
        assert_eq!(
            check("chord(\"Gmajorr\").dict('lefthand').voicing()").len(),
            1
        );
    }

    /// A chord written with mini-notation's colon is a pair of words to the
    /// engine, not a chord symbol: it voices nothing here and nothing on
    /// strudel.cc, so it is no reason to refuse a score that plays. The
    /// joined spelling is the one that voices, and it is judged.
    #[test]
    fn a_chord_written_with_a_colon_is_two_words_and_refuses_nothing() {
        // The owner's score, which plays on strudel.cc.
        let owner = "$: note(\"c2 a2 eb2\")\n.chord(\"G:7b9\")\n.euclid(5,8)\n.fill(0.1)\n.lpenv(4).lpf(1000)\n._spiral({ steady: .96 }).color(\"blue\")";
        assert!(check(owner).is_empty(), "{:?}", check(owner));
        assert!(rejection(owner, false, None).is_none());

        // The colon spares the word whatever follows it, since none of it
        // reaches a dictionary.
        assert!(check("chord(\"G:majorr\").voicing()").is_empty());
        // Joined, the same chord is a symbol the dictionary knows - and a
        // typo in a joined chord is still refused.
        assert!(check("chord(\"G7b9\").voicing()").is_empty());
        assert_eq!(check("chord(\"G7b9x\").voicing()").len(), 1);
    }

    #[test]
    fn sound_names_are_judged_only_by_a_library_that_knows_its_banks() {
        // No library, or an empty one: nothing to say about `s("abcde")`.
        assert!(check("s(\"abcde\")").is_empty());
        let empty = SampleLibrary::empty();
        assert!(lint("s(\"abcde\")", false, Some(&empty)).is_empty());

        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{"bd":["http://127.0.0.1:9/a.wav"],"RolandTR909_hh":["http://127.0.0.1:9/b.wav"],"Metal_cymbal":["http://127.0.0.1:9/c.wav"]}"#,
                None,
            )
            .expect("banks");
        let checked = |source: &str| lint(source, false, Some(&library));
        // Under the bank, `hh` is the machine's own and `sine` is a synth,
        // whose dispatch reads the raw name no bank touches.
        assert!(checked("s(\"hh sine\").bank(\"RolandTR909\")").is_empty());
        // `bd` is not the machine's: the plain bank of the same name does
        // not answer under a bank - the voice resolves `RolandTR909_bd`
        // and nothing else, and the eval says so instead of vouching.
        let diagnostics = checked("s(\"bd:3 hh sine\").bank(\"RolandTR909\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown sound \"bd\"");
        let diagnostics = checked("s(\"bd abcde\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown sound \"abcde\"");
        assert_eq!(diagnostics[0].level, Level::Value);
        let diagnostics = checked("s('bd abcde')");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown sound \"abcde\"");
        assert_eq!(diagnostics[0].level, Level::Value);
        let diagnostics = checked("s(\"bd\").bank(\"tr9099\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown bank \"tr9099\"");
        // A bank's key ending with the word does not make the bare word a
        // sound: the voice resolves `cymbal` exactly, refuses it at the
        // first hit, and the eval now says so instead of vouching.
        let diagnostics = checked("s(\"cymbal\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown sound \"cymbal\"");
        // With the bank on the sound's chain, the banked spelling is the one
        // the voice builds, and it resolves.
        assert!(checked("s(\"cymbal\").bank(\"Metal\")").is_empty());
        // A bank named by anything but literal text leaves the words to
        // the score: which machine it names is not in this source.
        assert!(checked("const someVar = 'Metal'; s(\"cymbal\").bank(someVar)").is_empty());
        // A score that registers samples of its own is not second-guessed.
        assert!(checked("samples('github:me/mine')\ns(\"abcde\")").is_empty());
        // A statement routed through `.osc()` names SuperDirt's sounds, past a
        // blank or comment line inside it too; a commented-out route exempts
        // nothing.
        assert!(checked("s(\"abcde\")\n\n  .osc()").is_empty());
        assert!(checked("s(\"abcde\")\n  // to SuperDirt\n  .osc()").is_empty());
        assert_eq!(checked("s(\"abcde\") // .osc()").len(), 1);

        // A bank control is a pattern: its machines alternate across the
        // haps, so a sound that one machine lacks is refused on that
        // machine's turns. Every machine must hold the sound.
        let machines = SampleLibrary::empty();
        machines
            .register_trusted_custom(
                r#"{"BossDR110_bd":["http://127.0.0.1:9/a.wav"],"BossDR110_sd":["http://127.0.0.1:9/b.wav"],"AkaiXR10_ht":["http://127.0.0.1:9/c.wav"],"AkaiXR10_sd":["http://127.0.0.1:9/d.wav","http://127.0.0.1:9/e.wav"]}"#,
                None,
            )
            .expect("machines");
        let machines = |source: &str| lint(source, false, Some(&machines));
        // `ht` dies on the Boss haps: the eval says so now.
        let diagnostics = machines("s(\"ht:0\").bank(\"BossDR110 AkaiXR10\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown sound \"ht\"");
        // Both machines hold `sd`, so it is valid. The engine wraps an
        // out-of-range number (`sound_index`), so `sd:1` plays on both;
        // the machine with the most samples judges the number.
        assert!(machines("s(\"sd:0\").bank(\"BossDR110 AkaiXR10\")").is_empty());
        assert!(machines("s(\"sd:1\").bank(\"BossDR110 AkaiXR10\")").is_empty());
    }

    /// A score that brings its own samples is held to them once they are
    /// in: `b` is nobody's, in the score's map or any other.
    #[test]
    fn a_scores_own_samples_are_judged_once_they_have_arrived() {
        let library = SampleLibrary::empty();
        // The map `samples('github:me/mine')` fetched, and the note the
        // worker takes once it is in.
        library
            .register_trusted_custom(r#"{"bd":["http://127.0.0.1:9/a.wav"]}"#, None)
            .expect("map");
        library.note_samples_source_for_tests("github:me/mine");
        let diagnostics = lint(
            "samples('github:me/mine')\n$: s(\"bd b\")",
            false,
            Some(&library),
        );
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown sound \"b\"");
        // A map that has not arrived, a spec only built from a known one,
        // or a map the checker cannot read back: the names stay the score's.
        for score in [
            "samples('github:me/other')\n$: s(\"b\")",
            "samples('github:me/mine' + '/kits')\n$: s(\"b\")",
        ] {
            assert!(lint(score, false, Some(&library)).is_empty(), "{score}");
        }
        // An inline map's keys are read off the text: the score is held
        // to the library's names and the map's, and a name in neither is
        // unknown.
        let object_form = lint(
            "samples({b: ['http://127.0.0.1:9/a.wav'], kick: 'k.wav'}, 'https://x/')\n$: s(\"b bd kick swpad\")",
            false,
            Some(&library),
        );
        assert_eq!(object_form.len(), 1, "{object_form:?}");
        assert_eq!(object_form[0].message, "unknown sound \"swpad\"");
        // A map this text cannot read leaves the names alone.
        let opaque = lint(
            "const banks = {}; samples(banks)\n$: s(\"swpad\")",
            false,
            Some(&library),
        );
        assert!(opaque.is_empty(), "{opaque:?}");
    }

    /// An import the library could not read is marked where it is written,
    /// with the reason; the names it would have brought stay the score's.
    #[test]
    fn an_import_that_could_not_be_read_is_marked_where_it_is_written() {
        let library = SampleLibrary::empty();
        library.note_samples_source_state_for_tests(
            "github:me/gone",
            crate::samples::SourceState::Failed("no strudel.json at github:me/gone".into()),
        );
        library.note_samples_source_state_for_tests(
            "github:me/slow",
            crate::samples::SourceState::Loading,
        );
        let source = "samples('github:me/gone')\nsamples('github:me/slow')\n$: s(\"b\")";
        let diagnostics = lint(source, false, Some(&library));
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].message,
            "samples: no strudel.json at github:me/gone"
        );
        assert_eq!(diagnostics[0].level, Level::Value);
        assert_eq!(
            &source[diagnostics[0].from..diagnostics[0].to],
            "github:me/gone",
            "the mark sits on the spec"
        );
    }

    /// A `samples("…")` inside a comment is no import: a refusal the library
    /// holds for its spec marks nothing, so it cannot refuse the score.
    #[test]
    fn a_commented_out_import_that_could_not_be_read_refuses_nothing() {
        let library = SampleLibrary::empty();
        library.note_samples_source_state_for_tests(
            "github:me/gone",
            crate::samples::SourceState::Failed("no strudel.json at github:me/gone".into()),
        );
        let source =
            "// samples('github:me/gone')\n/* samples('github:me/gone') */\n$: s(\"sine\")";
        let diagnostics = lint(source, false, Some(&library));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(rejection_of(source, &diagnostics), None);
    }

    /// A sample number the sound does not have plays anyway, wrapped as
    /// playback wraps it - as on strudel.cc - so it is a hint that says which
    /// sample sounds, never a refusal.
    #[test]
    fn a_sample_number_past_the_last_one_is_a_hint_naming_what_plays() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{"bd":["http://127.0.0.1:9/a.wav","http://127.0.0.1:9/b.wav"],"RolandTR909_bd":["http://127.0.0.1:9/c.wav"]}"#,
                None,
            )
            .expect("banks");
        let checked = |source: &str| lint(source, false, Some(&library));
        let hint = |source: &str| {
            let diagnostics = checked(source);
            assert_eq!(diagnostics.len(), 1, "{source}: {diagnostics:?}");
            assert_eq!(diagnostics[0].level, Level::Note, "{source}");
            assert_eq!(rejection_of(source, &diagnostics), None, "{source}");
            diagnostics[0].message.clone()
        };
        assert!(checked("s(\"bd:0 bd:1\")").is_empty());
        assert_eq!(
            hint("s(\"bd:10000\").seg(4)"),
            "bd:10000 plays bd:0 (\"bd\" has 2 samples: bd:0 … bd:1)"
        );
        // Negative and fractional numbers wrap and round as playback does.
        assert_eq!(
            hint("s(\"bd:-1\")"),
            "bd:-1 plays bd:1 (\"bd\" has 2 samples: bd:0 … bd:1)"
        );
        assert_eq!(
            hint("s(\"bd:0.6\")"),
            "bd:0.6 plays bd:1 (\"bd\" has 2 samples: bd:0 … bd:1)"
        );
        // `.bank(...)` renames every sound in the string, so the count is
        // the machine's own bank's.
        assert!(checked("s(\"bd:0\").bank(\"RolandTR909\")").is_empty());
        assert_eq!(
            hint("s(\"bd:3\").bank(\"RolandTR909\")"),
            "bd:3 plays RolandTR909_bd:0 (\"RolandTR909_bd\" has 1 sample: RolandTR909_bd:0)"
        );
        // A number that is not a number is refused whatever the bank: the
        // engine drops those onsets.
        let diagnostics = checked("s(\"bd:1ssd\").bank(\"RolandTR909\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].level, Level::Value);
        assert!(
            diagnostics[0].message.contains("not a sample number"),
            "{}",
            diagnostics[0].message
        );
        // A machine-prefixed bank name is one bank: the underscore is part
        // of the name, not a bank separator.
        assert_eq!(
            hint("$: s(\"RolandTR909_bd:20000\").seg(4)"),
            "RolandTR909_bd:20000 plays RolandTR909_bd:0 (\"RolandTR909_bd\" has 1 sample: RolandTR909_bd:0)"
        );
    }

    /// `bd:22` where `bd` has eight samples plays `bd:6`, on strudel.cc as
    /// here: the score is not refused and each word carries the hint, and
    /// hints never crowd out or sit beside a refusal.
    #[test]
    fn a_score_playing_bd_22_of_eight_is_not_refused_and_carries_the_hint() {
        let library = SampleLibrary::empty();
        let urls = (0..8)
            .map(|k| format!("\"http://127.0.0.1:9/{k}.wav\""))
            .collect::<Vec<_>>()
            .join(",");
        library
            .register_trusted_custom(&format!(r#"{{"bd":[{urls}]}}"#), None)
            .expect("bd");
        let source = "$: s(\"bd:22 - bd:22?0.72 bd:22?0.68\")\n  .delay(\"0.2 0.4\").dist(\"2.5:.8\").lpf(\"12000\").color(\"green\")";
        assert_eq!(rejection(source, false, Some(&library)), None);
        let diagnostics = lint(source, false, Some(&library));
        assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
        for diagnostic in &diagnostics {
            assert_eq!(diagnostic.level, Level::Note);
            assert_eq!(
                diagnostic.message,
                "bd:22 plays bd:6 (\"bd\" has 8 samples: bd:0 … bd:7)"
            );
            assert_eq!(&source[diagnostic.from..diagnostic.to], "bd:22");
        }
        // Hints never use up the room a refusal needs, and wait until the
        // score is otherwise clean.
        let crowded = format!("$: s(\"{} zzz\")", ["bd:22"; 9].join(" "));
        let diagnostics = lint(&crowded, false, Some(&library));
        assert_eq!(
            rejection_of(&crowded, &diagnostics).as_deref(),
            Some("line 1: unknown sound \"zzz\"")
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.level != Level::Note),
            "{diagnostics:?}"
        );
    }

    /// A sound is judged under the bank that applies to its own pattern: one
    /// on its chain, before or after its call, or on a call or group wrapping
    /// it, the last one winning. A sibling's bank in a `stack`, or one on
    /// another `$:`, leaves the sound its plain spelling.
    #[test]
    fn a_bank_applies_to_its_own_chain_and_not_to_siblings() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{"bd":["http://127.0.0.1:9/a.wav"],"clave":["http://127.0.0.1:9/b.wav"],"rim":["http://127.0.0.1:9/c.wav"],"tr909_rim":["http://127.0.0.1:9/d.wav"],"tr909_bd":["http://127.0.0.1:9/e.wav"],"tr909_cp":["http://127.0.0.1:9/f.wav"]}"#,
                None,
            )
            .expect("banks");
        let checked = |source: &str| lint(source, false, Some(&library));
        let unknown_sounds = |source: &str| {
            checked(source)
                .into_iter()
                .filter(|diagnostic| diagnostic.message.contains("unknown sound"))
                .map(|diagnostic| diagnostic.message)
                .collect::<Vec<_>>()
        };
        assert!(
            checked("$: stack(\n  sound(\"rim\").bank(\"tr909\"),\n  sound(\"clave\"),\n)")
                .is_empty()
        );
        let diagnostics = checked("$: stack(sound(\"zzz\"), sound(\"rim\").bank(\"tr909\"))");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown sound \"zzz\"");
        assert!(checked("$: stack(sound(\"bd\"), sound(\"rim\")).bank(\"tr909\")").is_empty());
        // A bank before the sound's own call applies too.
        assert!(checked("$: note(\"c\").bank(\"tr909\").s(\"cp\")").is_empty());
        assert!(checked("$: bank(\"tr909\").s(\"cp\")").is_empty());
        // A string heading its own chain plays through the banks on that chain.
        assert!(checked("$: \"cp\".s().bank(\"tr909\")").is_empty());
        assert_eq!(
            unknown_sounds("$: \"clave\".s().bank(\"tr909\")"),
            ["unknown sound \"clave\""]
        );
        assert!(checked("$: stack(\"cp\".s().bank(\"tr909\"), s(\"clave\"))").is_empty());
        // A bank after a group reaches what the group holds, and no sibling.
        assert!(checked("$: (s(\"cp\")).bank(\"tr909\")").is_empty());
        assert!(checked("$: ((s(\"cp\"))).bank(\"tr909\")").is_empty());
        assert_eq!(
            unknown_sounds("$: stack((s(\"cp\")), s(\"rim\").bank(\"tr909\"))"),
            ["unknown sound \"cp\""]
        );
        assert!(unknown_sounds("$: \"zzz\".s().bank(which)").is_empty());
        assert_eq!(
            unknown_sounds("$: s(\"cp\").bank(\"tr909\").bank(\"\")"),
            ["unknown sound \"cp\""]
        );
        let diagnostics =
            checked("$: stack(sound(\"bd\"), sound(\"rim\")).bank(\"tr909\")\n$: s(\"bd:5\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].message,
            "bd:5 plays bd:0 (\"bd\" has 1 sample: bd:0)"
        );
        // An opaque bank leaves its own sounds to the score, not a sibling's.
        assert!(unknown_sounds("$: s(\"rim\").bank(which)").is_empty());
        assert_eq!(
            unknown_sounds("$: stack(sound(\"zzz\"), sound(\"rim\").bank(which))"),
            ["unknown sound \"zzz\""]
        );
    }

    /// A pitched bank, its files keyed by note, is not judged by sample
    /// number; an array bank in the same score still is.
    #[test]
    fn a_pitched_bank_is_not_judged_by_sample_number() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{"kalimba":{"a4":"http://127.0.0.1:9/a.wav","c5":"http://127.0.0.1:9/c.wav"},"bd":["http://127.0.0.1:9/k.wav"]}"#,
                None,
            )
            .expect("banks");
        let checked = |source: &str| lint(source, false, Some(&library));
        assert!(checked("$: note(\"c\").sound(\"<kalimba:2, sine>\")").is_empty());
        assert!(checked("$: s(\"kalimba:11\")").is_empty());
        let diagnostics = checked("$: s(\"kalimba:2 bd:2\")");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].message,
            "bd:2 plays bd:0 (\"bd\" has 1 sample: bd:0)"
        );
    }

    /// Every place mini-notation lets a sample number ride on a name.
    /// One library, one wrap - `bd:9` where bd has two - and the hint
    /// must come from every spelling the engine would play.
    #[test]
    fn a_sample_number_past_the_last_one_is_caught_wherever_the_name_can_sit() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{"bd":["http://127.0.0.1:9/a.wav","http://127.0.0.1:9/b.wav"],"sd":["http://127.0.0.1:9/c.wav"],"hh":["http://127.0.0.1:9/d.wav"],"RolandTR909_bd":["http://127.0.0.1:9/e.wav"],"tr808_bd":["http://127.0.0.1:9/f.wav","http://127.0.0.1:9/g.wav","http://127.0.0.1:9/h.wav"]}"#,
                None,
            )
            .expect("banks");
        let slips = |source: &str| {
            lint(source, false, Some(&library))
                .into_iter()
                .filter(|diagnostic| {
                    diagnostic.level == Level::Note && diagnostic.message.contains(" plays ")
                })
                .count()
        };
        let must_flag = [
            "s(\"bd:9\")",
            "sound(\"bd:9\")",
            "$: s(\"bd:9\")",
            "_$: s(\"bd:9\")",
            "note(\"c3\").s(\"bd:9\")",
            "\"bd:9\".s()",
            "s(`bd:9`)",
            "s('bd:9')",
            "s(\"bd:9*2\")",
            "s(\"bd:9@3\")",
            "s(\"bd:9!\")",
            "s(\"bd:9!2\")",
            "s(\"bd:9?\")",
            "s(\"bd:9?0.3\")",
            "s(\"bd:9(3,8)\")",
            "s(\"bd:9 sd\")",
            "s(\"sd bd:9\")",
            "s(\"[bd:9 sd]\")",
            "s(\"<bd:9 sd>\")",
            "s(\"{bd:9 sd}%4\")",
            "s(\"bd:9 . sd sd\")",
            "s(\"bd:9 | sd\")",
            "s(\"bd:9, hh\")",
            "s(\"[bd sd]:9\")",
            "s(\"<bd sd>:9\")",
            "s(\"bd:9:1\")",
            "s(\"bd:9/2\")",
            "s(\"bd:9 _ _\")",
            "s(\"~ bd:9\")",
            "s(\"bd:9\").gain(.8)",
            "s(\"bd:9\").fast(2).room(.3)",
            "stack(s(\"bd:9\"), s(\"hh\"))",
            "cat(s(\"hh\"), s(\"bd:9\"))",
            "s(\"hh\").layer(x => x.s(\"bd:9\"))",
            "$: stack(\n  s(\"bd:9\"),\n  s(\"hh\")\n)",
            // Under a bank the count is the machine's own.
            "s(\"bd:1\").bank(\"RolandTR909\")",
            "s(\"bd:3\").bank(\"tr808\")",
            "s(\"bd:5\").bank(\"<RolandTR909 tr808>\")",
            "$: s(\"bd\").bank(\"RolandTR909\")\n$: s(\"bd:9\")",
        ];
        let must_not_flag = [
            "s(\"bd:0 bd:1\")",
            "s(\"bd:1\")",
            "s(\"bd\")",
            "s(\"sine:9\")",
            "// s(\"bd:9\")",
            "s(\"bd:0\").bank(\"RolandTR909\")",
            "s(\"bd:2\").bank(\"tr808\")",
            "s(\"bd:2\").bank(\"<RolandTR909 tr808>\")",
            "s(\"bd\").n(9)",
            "samples('github:me/mine')\ns(\"bd:9\")",
            "s(\"bd:<0 1>\")",
        ];
        let missed = must_flag
            .iter()
            .filter(|source| slips(source) == 0)
            .collect::<Vec<_>>();
        let wrong = must_not_flag
            .iter()
            .filter(|source| slips(source) != 0)
            .collect::<Vec<_>>();
        assert!(
            missed.is_empty() && wrong.is_empty(),
            "missed: {missed:#?}\nwrongly flagged: {wrong:#?}"
        );
    }

    #[test]
    fn a_statement_is_its_own_lines_and_the_ones_it_continues_onto() {
        let source = "$: s(\"hh\").bank(\"tr808\")\n$: stack(\n  s(\"bd:9\"),\n  s(\"sd\")\n).bank(\"tr909\")\n$: s(\"cp\")\n  .room(.2)\n";
        let at = |needle: &str| source.find(needle).unwrap();
        let statement = |needle: &str| &source[statement_around(source, at(needle))];
        assert_eq!(statement("hh"), "$: s(\"hh\").bank(\"tr808\")");
        assert_eq!(
            statement("bd:9"),
            "$: stack(\n  s(\"bd:9\"),\n  s(\"sd\")\n).bank(\"tr909\")"
        );
        assert_eq!(statement("cp"), "$: s(\"cp\")\n  .room(.2)");
        // The very end of the file belongs to the last statement.
        assert_eq!(
            &source[statement_around(source, source.len())],
            "$: s(\"cp\")\n  .room(.2)"
        );
    }

    /// The string being typed is the one that is not closed yet. Its mark
    /// runs from the quote to where the string stops, not from the byte
    /// inside it that the parser reports.
    #[test]
    fn an_unterminated_string_is_marked_from_its_quote_to_where_it_stops() {
        for (source, quote, stop) in [
            ("samples('githu", 8, 14),
            ("samples('github:s", 8, 17),
            (
                "setGainCurve(x => Math.pow(x, 2))\nsamples('github:s",
                42,
                51,
            ),
            ("samples('github:s\n$: s(\"bd\")", 8, 17),
        ] {
            let diagnostics = check(source);
            let mark = diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.starts_with("Unterminated string"))
                .unwrap_or_else(|| panic!("{source:?}: {diagnostics:?}"));
            assert_eq!((mark.from, mark.to), (quote, stop), "{source:?}");
            assert!(source[mark.from..].starts_with('\''), "{source:?}");
        }
        // An incomplete mini string stays intact for the parser, including
        // when its last character occupies more than one UTF-8 byte.
        for source in ["s(\"bd", "s(\"é"] {
            let diagnostics = check(source);
            assert_eq!(
                (diagnostics[0].from, diagnostics[0].to),
                (2, source.len()),
                "{source:?}: {diagnostics:?}"
            );
        }
        // A closed string is nobody's problem.
        assert!(check("samples('github:tidalcycles/dirt-samples')").is_empty());
    }

    /// The score as written on strudel.cc: a lane whose chain breaks after
    /// the dot and carries on below. One statement there, one here.
    #[test]
    fn a_lane_that_breaks_after_a_dot_is_one_clean_statement() {
        let score = "setcps(170/60/4)\n\n$: s(\"breaks/2\").fit().orbit(2)\n\n$: s(\"white!8\").decay(.08).\ngain(.4)";
        assert!(check(score).is_empty(), "{:?}", check(score));
    }

    #[test]
    fn the_rejection_names_the_line_of_the_first_finding() {
        let source = "$: s(\"bd\")\n$: n(\"0\").scale(\"C:nope\")";
        let message = rejection(source, false, None).expect("a finding");
        assert!(message.starts_with("line 2: "), "{message}");
        assert!(message.contains("Invalid scale name"), "{message}");
        assert!(rejection("$: s(\"bd\")", false, None).is_none());
    }

    #[test]
    fn single_quoted_strings_are_plain_text_and_left_alone() {
        let source = "const label = '[[[ not a pattern';\n$: s(\"bd\")";
        assert!(check(source).is_empty());
        let source = "const label = \"[[[ not a pattern\";\n$: s(\"bd\")";
        assert_eq!(check(source).len(), 1);
    }

    /// Each finding as its level and the source text it marks.
    fn marks(source: &str) -> Vec<(Level, &str)> {
        check(source)
            .into_iter()
            .map(|diagnostic| (diagnostic.level, &source[diagnostic.from..diagnostic.to]))
            .collect()
    }

    /// Each string literal as its text and whether it was left open.
    fn literal_texts(source: &str) -> Vec<(&str, bool)> {
        string_literals(source)
            .into_iter()
            .map(|literal| (&source[literal.content], literal.open))
            .collect()
    }

    /// A quote inside a regex literal is pattern text, not the start of a
    /// string. The strings after the regex stay strings: `zzzq(3,8)` in the
    /// `label` is text, not an unknown function.
    #[test]
    fn a_quote_inside_a_regex_literal_opens_no_phantom_string() {
        // Valid - the engine plays it - and clean.
        let source = r#"$: s("bd").every(8, x => x.replace(/"/, "x")).label("zzzq(3,8)")"#;
        assert!(check(source).is_empty(), "{:?}", check(source));
        assert_eq!(
            code_only(source),
            r#"$: s(    ).every(8, x => x.replace(   ,    )).label(           )"#
        );
        assert_eq!(
            literal_texts(source),
            [("bd", false), ("x", false), ("zzzq(3,8)", false)]
        );
        // The engine refuses these for a string after the regex that does
        // not parse, and so does the check: on that string's text, not
        // as an unknown function, and not silently.
        let source = r#"$: s("bd").every(8, x => x.replace(/"/, "x")).label("zzzq(")"#;
        assert_eq!(marks(source), [(Level::Syntax, "zzzq(")]);
        let source = r#"$: s("bd").every(8, x => x.replace(/"/, "x")).s("bd [sd")"#;
        assert_eq!(marks(source), [(Level::Syntax, "bd [sd")]);
        assert!(
            rejection(source, false, None).is_some_and(|reason| reason.contains("[mini]")),
            "{:?}",
            rejection(source, false, None)
        );
    }

    /// The quotes inside a character class such as `/['"]/` are characters.
    /// After the regex, an unknown call is still caught and a string is
    /// still checked as mini-notation.
    #[test]
    fn a_character_class_holds_its_quotes_and_hides_nothing_after_it() {
        let source = r#"$: s("bd").every(8, x => x.replace(/['"]/g, "x")).label("zzzq(3,8)")"#;
        assert!(check(source).is_empty(), "{:?}", check(source));
        let source = r#"$: s("bd").every(8, x => x.replace(/['"]/g, "x")).s("bd [sd")"#;
        assert_eq!(marks(source), [(Level::Syntax, "bd [sd")]);
        let source = r#"$: s("bd").every(8, x => x.replace(/['"]/g, "x")).zzzq()"#;
        assert_eq!(marks(source), [(Level::Value, "zzzq")]);
        assert_eq!(check(source)[0].message, "unknown function `zzzq`");
        // A MIDI port named after it is a string too, judged against the
        // ports like any other.
        let context = LintContext {
            midi_outputs: Some(vec!["IAC Driver Bus 1".to_owned()]),
            ..LintContext::default()
        };
        let source = r#"$: note("c").every(2, x => x.replace(/['"]/g, "x")).midi('IAC Bus 9')"#;
        let found = lint_with(source, false, None, &context);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(&source[found[0].from..found[0].to], "IAC Bus 9");
        assert!(found[0].message.contains("no MIDI output"), "{found:?}");
    }

    /// A division stays a division: the regex rule takes nothing away
    /// from the code around it, and a real unknown call after `1 / 2` is
    /// still found.
    #[test]
    fn a_division_is_not_blanked_as_a_regex_literal() {
        let source = r#"$: s("bd").speed(1 / 2).zzzq()"#;
        let diagnostics = check(source);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].message, "unknown function `zzzq`");
        assert!(rejection(source, false, None).unwrap().contains("zzzq"));
        // `a++ / 2 / 3` divides - the `++` belongs to the value before
        // the slash - even with two slashes on the one line.
        assert_eq!(code_only("b = a++ / 2 / 3"), "b = a++ / 2 / 3");
        assert_eq!(code_only("b = a / 2 / 3"), "b = a / 2 / 3");
    }

    /// An identifier inside a regex body is pattern text: what the regex
    /// matches is not a call, and never becomes a finding.
    ///
    /// The empty replacement `""` draws its own finding, on main and here
    /// alike: the transpiler hands it to the engine as a pattern, and the
    /// mini parser refuses an empty one. That refusal has nothing to do
    /// with the regex beside it, so this test asks of the rejection only
    /// that it is not about `zzzq`.
    #[test]
    fn an_identifier_inside_a_regex_literal_is_not_a_call() {
        let source = r#"$: s("bd").every(8, x => x.replace(/zzzq/, ""))"#;
        assert!(
            !check(source)
                .iter()
                .any(|diagnostic| diagnostic.message.contains("zzzq")),
            "{:?}",
            check(source)
        );
        assert!(
            !rejection(source, false, None).is_some_and(|reason| reason.contains("zzzq")),
            "{:?}",
            check(source)
        );
        assert_eq!(
            code_only("x.replace(/zzzq/, \"\")"),
            "x.replace(      ,   )"
        );
    }

    /// An escaped slash does not close a regex - and `/\//` holds no `//`
    /// comment: the strings after it are still text, and still checked.
    #[test]
    fn an_escaped_slash_does_not_close_a_regex_literal() {
        let source = r#"$: s("bd").every(8, x => x.replace(/\//, "x")).label("zzzq(3,8)")"#;
        assert!(check(source).is_empty(), "{:?}", check(source));
        assert_eq!(
            code_only(source),
            r#"$: s(    ).every(8, x => x.replace(    ,    )).label(           )"#
        );
        let source = r#"$: s("bd").every(8, x => x.replace(/\//, "x")).s("bd [sd")"#;
        assert_eq!(marks(source), [(Level::Syntax, "bd [sd")]);
        // An escaped quote in the string after it is that string's own.
        let source = r#"$: s("bd").every(8, x => x.replace(/\//, "\"")).label("zzzq(")"#;
        assert_eq!(
            code_only(source),
            r#"$: s(    ).every(8, x => x.replace(    ,     )).label(       )"#
        );
        assert_eq!(
            marks(source),
            [(Level::Syntax, r#"\""#), (Level::Syntax, "zzzq(")]
        );
    }

    /// The transpiler reports one "Unterminated string", and the mark goes
    /// to the first string the scanner saw left open. An odd quote count
    /// in a regex on an earlier line leaves no string open: the mark lands
    /// on the one being typed, not on the regex's line.
    #[test]
    fn a_quote_inside_a_regex_does_not_take_the_unterminated_string_mark() {
        let source = "$: s(\"bd\").every(8, x => x.replace(/\"/, \"x\"))\nsamples('github:s";
        let quote = source.find('\'').unwrap();
        let diagnostics = check(source);
        let mark = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.starts_with("Unterminated string"))
            .unwrap_or_else(|| panic!("{diagnostics:?}"));
        assert_eq!(
            (mark.from, mark.to),
            (quote, source.len()),
            "{diagnostics:?}"
        );
        assert_eq!(
            literal_texts(source),
            [("bd", false), ("x", false), ("github:s", true)]
        );
    }

    /// A JavaScript regex cannot hold a raw newline, so a `/` where a regex
    /// could open but with no partner before the end of its line is the
    /// division it reads as. Were the search to cross the newline, the `/`
    /// in `3/2` would close a "regex" that blanked the next line's quotes
    /// and its call.
    #[test]
    fn a_regex_never_runs_past_the_end_of_its_line() {
        // Mid-typing: the `+` puts the `/` where a regex could open.
        let source = "let a = 1 + /\n$: s(\"hh\").zzzq().slow(3/2)";
        assert_eq!(
            code_only(source),
            "let a = 1 + /\n$: s(    ).zzzq().slow(3/2)"
        );
        assert_eq!(literal_texts(source), [("hh", false)]);
        assert!(
            check(source)
                .iter()
                .any(|diagnostic| diagnostic.message == "unknown function `zzzq`"),
            "{:?}",
            check(source)
        );
        // An escape cannot carry it over the newline either.
        let source = "let a = 1 + /\\\n$: s(\"hh\").slow(3/2)";
        assert_eq!(code_only(source), "let a = 1 + /\\\n$: s(    ).slow(3/2)");
    }

    /// A word before a `/` makes it a regex only when the word takes an
    /// expression next, and only when it is a keyword there: through a dot
    /// it is a property, a value, and the slash divides it. The word is
    /// read whole, a non-ASCII letter included, as `scan::starts_with_word` reads
    /// one.
    #[test]
    fn a_slash_after_a_word_opens_a_regex_only_after_a_keyword() {
        assert_eq!(
            code_only(r#"f = s => { return /"/.test(s) }"#),
            "f = s => { return    .test(s) }"
        );
        assert_eq!(code_only(r#"t = typeof /"/"#), "t = typeof    ");
        for division in [
            "x = obj.return / 2 / 3",
            "x = o.in / 2 / 3",
            "x = kin / 2 / 3",
            "x = returns / 2 / 3",
            "x = için / 2 / 3",
        ] {
            assert_eq!(code_only(division), division);
        }
        // Through the whole check: the string after `return /"/` is text,
        // and the engine refuses it for the bracket it leaves open.
        let source = r#"$: s("bd").every(8, x => { return /"/.test("a") ? x : x }).s("bd [sd")"#;
        assert_eq!(marks(source), [(Level::Syntax, "bd [sd")]);
        let source = r#"$: s("bd").every(8, x => { return /"/.test("a") ? x : x }).s("bd [sd]")"#;
        assert!(check(source).is_empty(), "{:?}", check(source));
    }

    #[test]
    fn whole_buffer_mini_mode_reports_at_the_offending_offset() {
        let diagnostics = lint("bd [hh sd", true, None);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].from <= "bd [hh sd".len());
    }

    /// `ident_at` skips whitespace and reads a name that does not start with a
    /// digit.
    #[test]
    fn an_identifier_does_not_start_with_a_digit() {
        assert_eq!(ident_at("  $a1(", 0), Some((2, 5)));
        assert_eq!(ident_at(" _x", 0), Some((1, 3)));
        assert_eq!(ident_at("  2e3", 0), None);
        assert_eq!(ident_at("  ", 0), None);
    }

    #[test]
    fn the_callee_scanners_read_the_call_a_string_belongs_to() {
        let source = "n(\"0\").scale( \"C:major\" ).note(\"c3\")";
        let scale_quote = source.find("\"C").unwrap();
        assert_eq!(callee_before(source, scale_quote), Some("scale"));
        let note_quote = source.rfind("\"c3").unwrap();
        assert_eq!(callee_before(source, note_quote), Some("note"));
        assert_eq!(callee_before("x + \"a\"", 4), None);
        let chained = "\"C^7\".voicing().room(1)";
        assert_eq!(callee_after(chained, 5), Some("voicing"));
        assert_eq!(callee_after("\"a\" + b", 3), None);
        assert_eq!(callee_after("\"a\"\n  .voicing()", 3), Some("voicing"));
        assert_eq!(callee_after("\"a\". voicing()", 3), None);
        assert_eq!(callee_after("\"a\".voicing ()", 3), None);
        assert_eq!(callee_after("\"a\"", 9), None);
    }

    #[test]
    fn the_transpiled_output_names_every_pattern_strings_quote() {
        assert_eq!(
            mini_string_offsets("s(m('bd [hh sd', 5)).gain(0.5).p('$');"),
            [5]
        );
        assert_eq!(
            mini_string_offsets("stack(m('it\\'s', 6), note(m('c3', 20)))"),
            [6, 20]
        );
        assert!(mini_string_offsets("s('bd')").is_empty());
    }

    #[test]
    fn the_literal_scanner_handles_comments_escapes_and_every_quote() {
        let source = "// \"not\" a string\n'a\\'b' `c\nd` \"e\" /* \"f\" */";
        let literals = string_literals(source);
        let texts = literals
            .iter()
            .map(|literal| &source[literal.content.clone()])
            .collect::<Vec<_>>();
        assert_eq!(texts, ["a\\'b", "c\nd", "e"]);
    }

    /// `MiniWord` marks a single-atom first tail and words from a range, and
    /// keeps the text the checks judge.
    #[test]
    fn mini_words_mark_a_written_index_and_a_range() {
        let words = |content: &str| mini_words_of(content).expect("parses");
        let flags = |content: &str| {
            words(content)
                .into_iter()
                .map(|word| (word.text, word.written_index, word.ranged))
                .collect::<Vec<_>>()
        };
        assert_eq!(flags("bd:3"), [("bd:3".to_owned(), true, false)]);
        assert_eq!(
            flags("bd:<0 3>"),
            [
                ("bd:0".to_owned(), false, false),
                ("bd:3".to_owned(), false, false)
            ]
        );
        assert_eq!(
            flags("[bd hh]:3"),
            [
                ("bd:3".to_owned(), true, false),
                ("hh:3".to_owned(), true, false)
            ]
        );
        assert_eq!(flags("bd:3:5"), [("bd:3:5".to_owned(), true, false)]);
        assert_eq!(
            flags("bd 0 .. 3"),
            [
                ("bd".to_owned(), false, false),
                ("0".to_owned(), false, true),
                ("3".to_owned(), false, true)
            ]
        );
        assert!(mini_words_of("bd [").is_none());
    }

    /// Every name of an entry tagged `selectors` rebinds its receiver's words,
    /// each such entry takes the selecting pattern first, and no untagged entry
    /// shares one of those names.
    #[test]
    fn selectors_are_read_off_their_entries() {
        let entries = declared_entries().collect::<Vec<_>>();
        let tagged =
            |entry: &rustel_core::reference::ReferenceEntry| entry.tags.contains(&"selectors");
        for entry in entries.iter().filter(|entry| tagged(entry)) {
            assert!(
                entry.params.len() >= 2 && entry.params[0].r#type == "Pattern",
                "{}: {:?}",
                entry.name,
                entry.params
            );
            for name in entry_names(*entry) {
                assert!(rebinds_words(name), "{name}");
            }
        }
        for entry in entries.iter().filter(|entry| !tagged(entry)) {
            for name in entry_names(*entry) {
                assert!(!rebinds_words(name), "{name} is also an untagged entry");
            }
        }
        for name in ["pick", "pickSqueeze", "inhabit", "squeeze"] {
            assert!(rebinds_words(name), "{name}");
        }
        for name in ["set", "cat", "fast", "s"] {
            assert!(!rebinds_words(name), "{name}");
        }
    }

    /// The widened MIDI completion range and this check have to agree
    /// about where a name ends, and neither may run past its line.
    #[test]
    fn a_port_name_check_stays_inside_its_own_string() {
        let context = LintContext {
            midi_inputs: Some(vec!["LPK25".to_owned()]),
            midi_outputs: Some(vec!["IAC Driver Bus 1".to_owned()]),
            ..LintContext::default()
        };
        // An unterminated string is a syntax error the parser reports; the
        // port check must not also panic or read off the end.
        for source in [
            "const k = await midikeys('LPK25\n",
            "const k = await midikeys('\n",
            "const k = await midikeys('')\n",
            "const k = await midikeys('   ')\n",
            "$: note(\"c\").midi('IAC Driver Bus 1') // midin('nope')\n",
            "const k = await midikeys('LPK25') // 'nope'\n",
        ] {
            let found = lint_with(source, false, None, &context);
            assert!(
                !found
                    .iter()
                    .any(|d| d.message.contains("no MIDI") && d.message.contains("nope")),
                "a name in a comment was judged: {source:?} -> {found:?}"
            );
            for diagnostic in &found {
                assert!(
                    diagnostic.to <= source.len() && diagnostic.from <= diagnostic.to,
                    "{source:?}: a span outside the text: {diagnostic:?}"
                );
            }
        }
        // A name with a multi-byte character in it does not panic and is
        // still judged against the list.
        let found = lint_with(
            "const k = await midikeys('Prophète ♥')\n",
            false,
            None,
            &context,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("no MIDI input"), "{found:?}");
        assert!(
            source_slice_is_the_name("const k = await midikeys('Prophète ♥')\n", &found[0]),
            "the mark covers the name and nothing else"
        );
    }

    fn source_slice_is_the_name(source: &str, diagnostic: &Diagnostic) -> bool {
        source.is_char_boundary(diagnostic.from)
            && source.is_char_boundary(diagnostic.to)
            && &source[diagnostic.from..diagnostic.to] == "Prophète ♥"
    }

    /// The sounds a score's live code names, for a studio waiting on them:
    /// comments, muted lanes, statements routed through `.osc()` and a bank
    /// named by anything but a quoted string name nothing; a `.bank()` is
    /// carried with the name it renames.
    #[test]
    fn live_sound_names_read_only_what_plays() {
        let names = |source: &str| {
            live_sound_names(source)
                .into_iter()
                .map(|sound| (sound.name, sound.n, sound.banks))
                .collect::<Vec<_>>()
        };
        let plain = |name: &str, n: f64| (name.to_owned(), n, Vec::<String>::new());
        assert_eq!(
            names("$: s(\"bd:3 ~ hh\")"),
            [plain("bd", 3.0), plain("hh", 0.0)]
        );
        assert_eq!(names("$: s(\"mlkr-grsl:3\")"), [plain("mlkr-grsl", 3.0)]);
        assert!(names("// s(\"bd\")").is_empty());
        assert!(names("/* $: s(\"bd\") */").is_empty());
        assert_eq!(names("_$: s(\"bd\")\n$: s(\"sd\")"), [plain("sd", 0.0)]);
        // Muted as the engine mutes a lane: its label starts or ends with `_`,
        // read after an indent, a space before the colon, or on the line above,
        // past the comments and blank lines inside the lane.
        for muted in [
            "$_: s(\"bd\")",
            "drums_ : s(\"bd\")",
            "  _kick: s(\"bd\")",
            "_drums:\n  s(\"bd\")",
            "_$: note(\"c e\")\n  .s(\"bd\")",
            "_$: stack(\n  s(\"bd\"),\n)",
            "_$: stack(\n  s(\"bd\"),\n  // hats\n\n  s(\"hh\"),\n)",
            "_$: note(\"c e\")\n  // .lpf(300)\n\n  .s(\"bd\")",
            "_$: \"<a b>\".pick({\n  a: s(\"bd\"),\n  b: s(\"hh\"),\n})",
        ] {
            assert_eq!(
                names(&format!("{muted}\n$: s(\"sd\")")),
                [plain("sd", 0.0)],
                "{muted}"
            );
        }
        assert_eq!(
            names("_$: s(\"bd\")\n$: stack(\n  // hats\n\n  s(\"hh\"),\n)"),
            [plain("hh", 0.0)]
        );
        assert_eq!(names("dr_ums: s(\"bd\")"), [plain("bd", 0.0)]);
        assert_eq!(
            names("s(\"bd\").bank(\"tr909\")"),
            [("bd".to_owned(), 0.0, vec!["tr909".to_owned()])]
        );
        assert!(names("s(\"bd\").bank(kit)").is_empty());
        assert!(names("s(\"superzow\").osc()").is_empty());
        assert!(names("s(\"superzow\")\n  // to SuperDirt\n  .osc()").is_empty());
        assert!(names("note(\"c3 e3\").s(\"in:1\")").is_empty());

        // The plugin calls of the live code, read the same way: the kind,
        // the preset and the one orbit of the statement.
        let score = "$: s(\"bd\").vst(\"ott\", { depth: \"0 1\", preset: \"Loud\" })\n\
                     // .vst(\"gone\")\n\
                     _$: note(\"c\").vsti(\"muted synth\")\n\
                     $: note(\"c\").vsti('serum 2').vst(name).vst(\"\")\n  .orbit(3)\n\
                     $: s(\"hh\").vst(\"ott\").orbit(\"<1 2>\")";
        let plugin = |name: &str, instrument, preset: Option<&str>, orbit| NamedPlugin {
            name: name.to_owned(),
            instrument,
            stage: 0,
            preset: preset.map(str::to_owned),
            orbit,
            keys: Vec::new(),
        };
        let mut found = live_plugins(score);
        let keys = std::mem::take(&mut found[0].keys);
        assert_eq!(
            found,
            [
                plugin("ott", false, Some("Loud"), Some(1)),
                plugin("serum 2", true, None, Some(3)),
                plugin("ott", false, None, None),
            ]
        );
        // The keys of the object, each at its place in the text. A key in
        // a value is no key of the call.
        let words = |score: &str| {
            let keys = live_plugins(score).remove(0).keys;
            keys.iter()
                .map(|key| score[key.clone()].to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys.iter()
                .map(|key| &score[key.clone()])
                .collect::<Vec<_>>(),
            ["depth", "preset"]
        );
        assert_eq!(
            words("s(\"bd\").vst('a', { \"in gain\": 1, mix: f({ x: 1 }), 100: 0.5, tail })"),
            ["in gain", "mix", "100"]
        );
        assert!(words("s(\"bd\").vst('a').fm({ depth: 1 })").is_empty());
        // The second effect of a statement is the second stage of the
        // chain, also after a name in a variable.
        let stages = |score| {
            let plugins = live_plugins(score);
            plugins
                .iter()
                .map(|plugin| plugin.stage)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            stages("$: vsti(\"a\").vst(\"b\").vst(\"c\")\n$: s(\"bd\").vst(\"d\")"),
            [0, 0, 1, 0]
        );
        assert_eq!(
            stages("$: s(\"bd\").vst (name).myvst(\"x\").vst(\"b\")"),
            [1]
        );
    }
}
