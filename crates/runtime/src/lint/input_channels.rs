//! Native input channels do not wrap like sample indices. Only references
//! whose sound/channel survives a readable control chain are judged here;
//! an arbitrary transform may replace either field and is left to runtime.
//!
//! The chain is read across JavaScript's whitespace in both directions, the
//! scan's `js_space` rule, so a vertical tab or a no-break space between its
//! tokens reads as the chain the engine runs.

use std::ops::Range;

use rustel_audio::input::MAX_INPUT_CHANNELS;

use super::scan::{
    dot_before_js_space, is_name_byte, js_space_after, js_space_before, matching_paren,
    method_at_js_space, name_before_js_space, open_parens, opening_paren,
};
use super::{
    Diagnostic, Level, LintContext, Literal, MAX_DIAGNOSTICS, MiniWord, code_only, is_plain_atom,
    push_word_diagnostics,
};

#[derive(Clone)]
pub(super) struct Number {
    value: f64,
    span: Range<usize>,
}

fn check_words(
    words: &[MiniWord],
    offset: usize,
    replacement: Option<&Number>,
    inherited: Option<&Number>,
    context: &LintContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if diagnostics.len() >= MAX_DIAGNOSTICS {
        return;
    }
    let verdicts = words
        .iter()
        .map(|word| {
            if !is_plain_atom(&word.text) || word.text.split(':').next() != Some("in") {
                return None;
            }
            let index = word.text.split(':').nth(1);
            // `in:0` supplies its own n, while a bare `in` preserves the
            // receiver's channel. A following `.n(...)` overrides both.
            let number = replacement.or_else(|| index.is_none().then_some(inherited).flatten());
            let value = number
                .map(|number| number.value)
                .or_else(|| mini_number(index.unwrap_or("0")));
            let message = channel_error(value, context.input_channels)?;
            if let Some(number) = number {
                // Several sound literals can share one `.n(...)`: that is
                // one finding, not one finding per member of a stack.
                if diagnostics.len() < MAX_DIAGNOSTICS
                    && !diagnostics.iter().any(|diagnostic| {
                        diagnostic.from == number.span.start
                            && diagnostic.to == number.span.end
                            && diagnostic.message == message
                    })
                {
                    diagnostics.push(Diagnostic {
                        level: Level::Value,
                        message,
                        from: number.span.start,
                        to: number.span.end,
                    });
                }
                None
            } else {
                Some(message)
            }
        })
        .collect::<Vec<_>>();
    push_word_diagnostics(words, &verdicts, offset, diagnostics);
}

/// Reuse the mini parser's Number grammar (including hex, binary and octal)
/// without querying or evaluating the score. Nonfinite numbers become null
/// at the runtime JSON boundary, where n uses its default; leave those and
/// nonnumeric values to the runtime's coercion/type checks.
fn mini_number(index: &str) -> Option<f64> {
    match rustel_mini::mini(index).ok()?.as_pure()? {
        rustel_core::Value::F64(value) if value.is_finite() => Some(value),
        _ => None,
    }
}

fn channel_error(value: Option<f64>, channels: Option<usize>) -> Option<String> {
    let value = value.filter(|value| value.is_finite())?;
    // Match voice construction: validate the f64 first, then cast to the
    // zero-based channel. Positive fractions such as 1.9 select channel 1.
    if !(0.0..MAX_INPUT_CHANNELS as f64).contains(&value) {
        return Some(format!(
            "audio input in:{value} is outside the supported channels in:0 … in:{}",
            MAX_INPUT_CHANNELS - 1
        ));
    }
    // Zero is the device snapshot's sentinel before capture opens, not a
    // verified device with no channels.
    let channels = channels
        .filter(|&channels| channels > 0)?
        .min(MAX_INPUT_CHANNELS);
    if value < channels as f64 {
        return None;
    }
    Some(format!(
        "audio input in:{value} is unavailable: the active input has {channels} channel{} (in:0 … in:{})",
        if channels == 1 { "" } else { "s" },
        channels - 1
    ))
}

pub(super) fn check_literal(
    source: &str,
    literal: &Literal,
    words: &[MiniWord],
    context: &LintContext,
    diagnostics: &mut Vec<Diagnostic>,
    notes: &mut Vec<Diagnostic>,
) {
    if !words
        .iter()
        .any(|word| word.text.split(':').next() == Some("in"))
    {
        return;
    }
    let Some(chain) = channel_after_literal(source, literal, context) else {
        return;
    };
    check_words(
        words,
        literal.content.start,
        chain.replacement.as_ref(),
        chain.inherited.as_ref(),
        context,
        diagnostics,
    );
    if words
        .iter()
        .all(|word| is_plain_atom(&word.text) && word.text.split(':').next() == Some("in"))
    {
        ineffective_sample_controls(source, chain.direct_methods, notes);
    }
}

struct Chain {
    replacement: Option<Number>,
    inherited: Option<Number>,
    /// Only the methods directly on this sound, before an enclosing stack
    /// can mix it with samples and give sample controls a purpose.
    direct_methods: Range<usize>,
}

/// One grouped hint per direct input chain, at most two distinct hints per
/// score. The caller appends these only after all errors have been checked.
fn ineffective_sample_controls(source: &str, span: Range<usize>, notes: &mut Vec<Diagnostic>) {
    if notes.len() >= 2 {
        return;
    }
    let code = code_only(source);
    let mut at = span.start;
    let mut controls = Vec::new();
    let mut mark = None;
    while at < span.end {
        at = js_space_after(&code, at);
        let Some((name, open)) = method_at_js_space(&code, at) else {
            break;
        };
        let Some(close) = matching_paren(&code, open) else {
            break;
        };
        let canonical = rustel_core::controls::canonical_control_name(&code[name.clone()]);
        if let Some(control @ ("speed" | "begin" | "end")) = canonical
            && !source[open + 1..close].trim().is_empty()
            && !controls.contains(&control)
        {
            controls.push(control);
            mark.get_or_insert(name);
        }
        at = close + 1;
    }
    let Some(mark) = mark else { return };
    let message = format!(
        "{} {} sample playback and {} change native audio input",
        controls.join(", "),
        if controls.len() == 1 {
            "affects"
        } else {
            "affect"
        },
        if controls.len() == 1 {
            "does not"
        } else {
            "do not"
        },
    );
    if !notes.iter().any(|note| note.message == message) {
        notes.push(Diagnostic {
            level: Level::Note,
            message,
            from: mark.start,
            to: mark.end,
        });
    }
}

#[derive(Default)]
enum Channel {
    #[default]
    Original,
    Constant(Number),
    Unknown,
}

/// Read just the surrounding call chain, not the whole statement: the `.n`
/// on a sibling in `stack(...)` must never excuse this input. Returns the
/// final constant override and the receiver's earlier channel, if known.
fn channel_after_literal(source: &str, literal: &Literal, context: &LintContext) -> Option<Chain> {
    let code = code_only(source);
    let bytes = code.as_bytes();
    let quote = literal.content.start.checked_sub(1)?;
    let before = js_space_before(&code, quote);
    let open = if before > 0 && bytes[before - 1] == b'(' {
        before - 1
    } else {
        // Receiver form: `"in:3".s()`.
        let dot = js_space_after(&code, literal.content.end + 1);
        let (_, open) = method_at_js_space(&code, dot)?;
        open
    };
    if name_before_js_space(&code, open)
        .is_some_and(|name| name_is_shadowed(&code, &code[name], context))
    {
        return None;
    }
    let close = matching_paren(&code, open)?;
    if open < quote {
        if js_space_after(&code, literal.content.end + 1) != close {
            return None; // Additional arguments are not a simple control.
        }
    } else if !source[open + 1..close].trim().is_empty() {
        return None; // A receiver's `.s(value)` replaces the literal.
    }
    let inherited = (open < quote)
        .then(|| channel_before_call(source, &code, open, context))
        .flatten();
    let mut parents = open_parens(&code, open);
    let mut channel = Channel::Original;
    let mut end = close + 1;
    let mut direct_end = None;
    loop {
        end = read_methods(source, &code, end, &mut channel, context, code.len())?;
        direct_end.get_or_insert(end);
        let Some(parent) = parents.next() else {
            // Arithmetic, indexing and optional/dynamic member access may
            // transform controls too. A new statement does not.
            if matches!(
                bytes.get(end),
                Some(b'.' | b'[' | b'?' | b'+' | b'-' | b'*' | b'/')
            ) {
                return None;
            }
            break;
        };
        let parent_close = matching_paren(&code, parent)?;
        if end > parent_close {
            return None;
        }
        let name = name_before_js_space(&code, parent).map_or("", |name| &code[name]);
        match name {
            // These containers preserve their members' control values.
            "stack" | "cat" | "slowcat" | "fastcat" if !name_is_shadowed(&code, name, context) => {}
            "" if end == parent_close && !code[parent + 1..open].contains("=>") => {}
            _ => return None,
        }
        end = parent_close + 1;
    }
    let replacement = match channel {
        Channel::Original => None,
        Channel::Constant(number) => Some(number),
        Channel::Unknown => return None,
    };
    Some(Chain {
        replacement,
        inherited,
        direct_methods: close + 1..direct_end.unwrap_or(close + 1),
    })
}

/// Read a preceding simple control chain such as `n(3).gain(.5).s("in")`.
/// Starting from a known control constructor avoids assuming an arbitrary
/// object's `.n(...)` method has the builtin pattern semantics.
fn channel_before_call(
    source: &str,
    code: &str,
    open: usize,
    context: &LintContext,
) -> Option<Number> {
    // Walked link by link rather than with `scan::link_before`, which skips
    // only ASCII whitespace where this module skips JavaScript's (see the
    // scan's module doc).
    let head = name_before_js_space(code, open).map_or(open, |name| name.start);
    let dot = dot_before_js_space(code, head)?;
    let mut link_dot = dot;
    let (root_open, root_close, root_name) = loop {
        let close = js_space_before(code, link_dot).checked_sub(1)?;
        if code.as_bytes()[close] != b')' {
            return None;
        }
        let root_open = opening_paren(code, close)?;
        let name = name_before_js_space(code, root_open)?;
        match dot_before_js_space(code, name.start) {
            Some(before) => link_dot = before,
            None => break (root_open, close, &code[name]),
        }
    };
    let spec = rustel_core::controls::default_control_registry().get(root_name)?;
    if name_is_shadowed(code, root_name, context) {
        return None;
    }
    let argument = root_open + 1..root_close;
    let mut channel = if spec.name() == "n" {
        Channel::Constant(number_at(source, argument)?)
    } else if scalar_argument(source, code, argument, context) {
        Channel::Unknown
    } else {
        return None;
    };
    read_methods(source, code, root_close + 1, &mut channel, context, dot)?;
    match channel {
        Channel::Constant(number) => Some(number),
        _ => None,
    }
}

/// A local binding or a setup-provided name may replace a builtin. Defer
/// ambiguous identifier uses (including parameters and destructuring), but
/// an ordinary property key or member access cannot bind a global name.
fn name_is_shadowed(code: &str, name: &str, context: &LintContext) -> bool {
    if name.is_empty() {
        return false;
    }
    if context.setup_names.iter().any(|defined| defined == name) {
        return true;
    }
    let word_before = |at: usize| name_before_js_space(code, at).map(|word| &code[word]);
    code.match_indices(name).any(|(at, _)| {
        let end = at + name.len();
        if at > 0 && is_name_byte(code.as_bytes()[at - 1])
            || code.as_bytes().get(end).copied().is_some_and(is_name_byte)
        {
            return false;
        }
        if matches!(
            word_before(at),
            Some("const" | "let" | "var" | "function" | "class")
        ) {
            return true;
        }
        let after = code.as_bytes().get(js_space_after(code, end));
        if after == Some(&b':') {
            return false; // `s: other` is a property key, even in destructuring.
        }
        let before = &code[..js_space_before(code, at)];
        if before.ends_with('.') && !before.ends_with("...") {
            // `globalThis.s = ...` really does replace the global. Other
            // objects' properties, and reading a global property, do not.
            return matches!(word_before(before.len() - 1), Some("globalThis" | "window"))
                && after == Some(&b'=');
        }
        after != Some(&b'(')
    })
}

fn read_methods(
    source: &str,
    code: &str,
    mut end: usize,
    channel: &mut Channel,
    context: &LintContext,
    until: usize,
) -> Option<usize> {
    let bytes = code.as_bytes();
    loop {
        end = js_space_after(code, end);
        if end >= until || bytes.get(end) != Some(&b'.') {
            return Some(end);
        }
        let (name, open) = method_at_js_space(code, end)?;
        let name = &code[name];
        let close = matching_paren(code, open)?;
        let argument = open + 1..close;
        match rustel_core::controls::default_control_registry().get(name) {
            Some(spec) if spec.name() == "n" => {
                if !source[argument.clone()].trim().is_empty() {
                    *channel =
                        number_at(source, argument).map_or(Channel::Unknown, Channel::Constant);
                }
            }
            Some(spec)
                if spec
                    .names
                    .iter()
                    .any(|name| matches!(name.as_ref(), "s" | "n")) =>
            {
                // A new sound literal is checked separately in its own turn.
                return None;
            }
            Some(_) if scalar_argument(source, code, argument, context) => {}
            // These methods change time/structure, not event controls.
            None if matches!(
                name,
                "fast" | "slow" | "seg" | "segment" | "early" | "late" | "rev"
            ) => {}
            _ => return None,
        }
        end = close + 1;
    }
}

fn number_at(source: &str, mut span: Range<usize>) -> Option<Number> {
    loop {
        let text = &source[span.clone()];
        span.start += text.len() - text.trim_start().len();
        span.end = span.start + text.trim().len();
        if source.as_bytes().get(span.start) == Some(&b'(')
            && matching_paren(source, span.start) == span.end.checked_sub(1)
        {
            span.start += 1;
            span.end -= 1;
        } else {
            break;
        }
    }
    let text = &source[span.clone()];
    // JS identifiers (`Infinity`, `NaN`, a variable) and expressions are not
    // constants whose runtime coercion this source check can promise.
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || b"+-.eE".contains(&byte))
    {
        return None;
    }
    let value = text.parse::<f64>().ok().filter(|value| value.is_finite())?;
    Some(Number { value, span })
}

fn scalar_argument(source: &str, code: &str, span: Range<usize>, context: &LintContext) -> bool {
    let text = source[span.clone()].trim();
    text.is_empty()
        || matches!(text, "true" | "false" | "null")
        || number_at(source, span.clone()).is_some()
        || numeric_slider(code, span, context)
        || super::string_literals(text)
            .as_slice()
            .first()
            .is_some_and(|literal| {
                !literal.open
                    && literal.content.start == 1
                    && literal.content.end + 1 == text.len()
                    && !text.contains("${")
            })
}

/// The builtin slider with literal numeric arguments yields a scalar, so a
/// gain/filter slider cannot replace the input sound or its channel. Keep
/// arbitrary calls and shadowed sliders conservative: those can return a
/// control object carrying a different `s` or `n`.
fn numeric_slider(code: &str, span: Range<usize>, context: &LintContext) -> bool {
    let text = code[span].trim();
    let Some(after_name) = text.strip_prefix("slider") else {
        return false;
    };
    let after_name = after_name.trim_start();
    if !after_name.starts_with('(')
        || matching_paren(after_name, 0) != after_name.len().checked_sub(1)
        || name_is_shadowed(code, "slider", context)
    {
        return false;
    }
    let arguments = &after_name[1..after_name.len() - 1];
    let arguments = arguments.trim_end().strip_suffix(',').unwrap_or(arguments);
    let mut count = 0;
    for argument in arguments.split(',') {
        count += 1;
        if count > 4 || number_at(argument, 0..argument.len()).is_none() {
            return false;
        }
    }
    count > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint::{lint_with, rejection_of};
    use crate::samples::SampleLibrary;

    fn input_findings(source: &str, mini: bool, channels: Option<usize>) -> Vec<Diagnostic> {
        lint_with(
            source,
            mini,
            None,
            &LintContext {
                input_channels: channels,
                ..LintContext::default()
            },
        )
        .into_iter()
        .filter(|finding| finding.level == Level::Value && finding.message.contains("audio input"))
        .collect()
    }

    fn input_notes(source: &str) -> Vec<Diagnostic> {
        lint_with(source, false, None, &LintContext::default())
            .into_iter()
            .filter(|finding| {
                finding.level == Level::Note && finding.message.contains("native audio input")
            })
            .collect()
    }

    #[test]
    fn sample_controls_on_pure_input_have_one_nonblocking_hint() {
        let source = "s(\"in:0 in:1\").speed(.5).begin(.1).end(.8).gain(slider(1,0,2))";
        let findings = lint_with(source, false, None, &LintContext::default());
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].level, Level::Note);
        assert_eq!(&source[findings[0].from..findings[0].to], "speed");
        assert!(findings[0].message.contains("speed, begin, end"));
        assert_eq!(rejection_of(source, &findings), None);
        for source in [
            "sound(\"in\").speed(slider(.5,0,2))",
            "\"in\".s().begin(.2)",
            "s('in').end(.8)",
        ] {
            assert_eq!(input_notes(source).len(), 1, "{source}");
        }
        // Time and pitch controls have separate uses; this hint does not
        // blacklist transformations or claim they are sample-only controls.
        assert!(input_notes("s(\"in\").fast(2).rev().note(60)").is_empty());
        assert!(input_notes("s(\"in\").speed().begin().end()").is_empty());
    }

    #[test]
    fn mixed_dynamic_or_overridden_input_does_not_get_sample_control_hints() {
        for source in [
            "s(\"in bd\").speed(2)",
            "stack(s(\"in\"), s(\"bd\")).speed(2)",
            "s(\"bd\").speed(2).begin(.1)",
            "s(\"in\").speed(sine)",
            "s(\"in\").speed(2).s(\"bd\")",
            "s(\"in\").gain({value: 1, s: 'bd'}).speed(2)",
            "s(\"in\").speed(2).fmap(v => ({...v, s: 'bd'}))",
            "s(\"in\").speed(2).n(sine)",
            "const s = () => sound('bd'); s(\"in\").speed(2)",
        ] {
            assert!(input_notes(source).is_empty(), "{source}");
        }
        let context = LintContext {
            setup_names: vec!["s".into()],
            ..LintContext::default()
        };
        let findings = lint_with("s(\"in\").speed(2)", false, None, &context);
        assert!(
            findings.iter().all(|finding| finding.level != Level::Note),
            "{findings:?}"
        );
    }

    #[test]
    fn input_hints_are_deduplicated_capped_and_never_crowd_out_errors() {
        let repeated = ["s(\"in\").speed(2)"; 12].join("; ");
        assert_eq!(input_notes(&repeated).len(), 1);
        let several = "s(\"in\").speed(2); s(\"in\").begin(.2); s(\"in\").end(.8)";
        assert_eq!(input_notes(several).len(), 2);
        for source in [
            "s(\"in:2323\").speed(2)".to_owned(),
            format!("{repeated}; s(\"in:2323\")"),
            format!("{repeated}; definitelyNotAFunction()"),
        ] {
            let findings = lint_with(&source, false, None, &LintContext::default());
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert_eq!(findings[0].level, Level::Value);
            assert!(rejection_of(&source, &findings).is_some());
        }
    }

    #[test]
    fn native_input_indices_obey_the_engine_limit_without_a_library_or_device() {
        for source in [
            "s(\"in:2323\")",
            "sound(\"in:16\")",
            "$: s(`in:16`)",
            "\"in:16\".s()",
            "note(\"c3\").s(\"in:16\")",
            "s(\"in:-1\")",
            "s(\"in:16\").gain(.5).fast(2).lpf(800)",
            "s(\"in:0x10\")",
            "s(\"in:0\").n(16)",
            "s('in').n(16)",
            "\"in\".sound().n(16)",
        ] {
            let findings = input_findings(source, false, None);
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert_eq!(findings[0].level, Level::Value);
            assert!(rejection_of(source, &findings).is_some());
        }
        for source in ["s(\"in:0 in:15 in:15.9\")", "s(\"in:-0\")", "s(\"in\")"] {
            assert!(input_findings(source, false, None).is_empty(), "{source}");
        }
        for source in [
            "s(\"in:0x1 in:0b1 in:0o1\")",
            "s(\"in:Infinity in:-Infinity in:1e999\")",
        ] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
        assert!(input_findings("s(\"in:15.9\")", false, Some(0)).is_empty());
        assert_eq!(input_findings("s(\"in:2323\")", false, Some(0)).len(), 1);
        // More hardware channels cannot expand the engine's native limit.
        assert_eq!(input_findings("s(\"in:16\")", false, Some(32)).len(), 1);
    }

    #[test]
    fn verified_device_channels_use_the_same_fractional_cast_as_runtime() {
        for source in ["s(\"in:2\")", "s(\"in:2.1\")", "s(\"in\").n(2)"] {
            let findings = input_findings(source, false, Some(2));
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert!(findings[0].message.contains("active input has 2 channels"));
            assert!(
                input_findings(source, false, Some(4)).is_empty(),
                "{source}"
            );
        }
        for source in ["s(\"in:0 in:1 in:1.9\")", "s(\"in\").n(1.999)"] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
        assert_eq!(input_findings("s(\"in:1\")", false, Some(1)).len(), 1);
        assert!(input_findings("s(\"in:0.9\")", false, Some(1)).is_empty());
    }

    #[test]
    fn input_mini_operators_and_group_tails_keep_exact_source_marks() {
        for (source, mini, marked) in [
            ("s(\"in:2323*2\")", false, "in:2323"),
            ("s(\"in:2323(3,8)\")", false, "in:2323"),
            ("s(\"<in in>:2323\")", false, "2323"),
            ("s(\"[in sine]:2323\")", false, "in"),
            ("s(\"in:<0 2323>\")", false, "2323"),
            ("// é\nsound(\"in:2323\")", false, "in:2323"),
        ] {
            let findings = input_findings(source, mini, Some(2));
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert_eq!(
                &source[findings[0].from..findings[0].to],
                marked,
                "{source}"
            );
        }
    }

    #[test]
    fn whole_buffer_mini_values_are_not_native_input_controls() {
        for source in ["in:2323", "<in in>:2323", "[in:0 in:2]/2"] {
            assert!(input_findings(source, true, Some(2)).is_empty(), "{source}");
        }
        assert_eq!(input_findings("s(\"in:2323\")", false, Some(2)).len(), 1);
    }

    #[test]
    fn a_shared_channel_argument_is_one_finding_and_does_not_use_up_the_cap() {
        let source = "stack(s(\"in:0\"), s(\"in:1\")).n(2323)";
        let findings = input_findings(source, false, Some(2));
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(&source[findings[0].from..findings[0].to], "2323");
        let source = format!(
            "stack({}).n(2323); s(\"in:3\")",
            ["s(\"in:0\")"; 8].join(",")
        );
        let findings = input_findings(&source, false, Some(2));
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert_eq!(&source[findings[0].from..findings[0].to], "2323");
        assert_eq!(&source[findings[1].from..findings[1].to], "in:3");
    }

    #[test]
    fn preceding_numeric_channels_apply_only_to_bare_input_words() {
        for source in [
            "n(3).s(\"in\")",
            "n(3).sound('in')",
            "n(3).gain(.5).s(\"in\")",
            "n(3).gain(slider(1,0,2)).s(\"in\")",
            "note(\"c3\").n(3).s(\"in\")",
            "n(3).s(\"in in:0\")",
            "n(0).n(3).s(\"in\")",
        ] {
            let findings = input_findings(source, false, Some(2));
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert_eq!(&source[findings[0].from..findings[0].to], "3", "{source}");
        }
        for source in [
            "n(3).s(\"in:0\")",
            "n(3).s(\"in:0 in:1\")",
            "n(3).s(\"in\").n(0)",
            "n(3).n(0).s(\"in\")",
            "n(1.9).s(\"in\")",
            "n(sine).s(\"in\")",
            "n(3).scale(\"C:major\").s(\"in\")",
            "n(3).gain({value: 1, n: 0}).s(\"in\")",
            "n(3).gain(slider(1,0,2)).s(\"in\"); function slider() { return {value: 1, n: 0}; }",
            "const n = () => pure({n: 0}); n(3).s(\"in\")",
            "const custom = {n: () => pure({n: 0})}; custom.n(3).s(\"in\")",
        ] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
        let source = "n(3).s(\"in:0 in\").n(2323)";
        let findings = input_findings(source, false, Some(2));
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(&source[findings[0].from..findings[0].to], "2323");
        let context = LintContext {
            input_channels: Some(2),
            setup_names: vec!["n".into()],
            ..LintContext::default()
        };
        let findings = lint_with("n(3).s(\"in\")", false, None, &context);
        assert!(
            findings
                .iter()
                .all(|finding| !finding.message.contains("audio input")),
            "{findings:?}"
        );
    }

    #[test]
    fn unrelated_properties_do_not_shadow_builtin_control_names() {
        for source in [
            "$: s(\"in:2323\"); s(\"bd\").fmap(v => ({...v, s: 'sd'}))",
            "$: s(\"in:2323\"); s(\"bd\").fmap(v => v.s)",
            "const data = {sound: 'sd', stack: []}; sound(\"in:2323\")",
            "const data = {s: 'sd'}; const other = data.s; s(\"in:2323\")",
            "const data = {s: 'sd'}; const {s: other} = data; s(\"in:2323\")",
            "const data = {n: 0}; n(3).s(\"in\")",
            "const data = {slider: 0}; s(\"in:2323\").gain(slider(1,0,2))",
        ] {
            let findings = input_findings(source, false, Some(2));
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
        }
        for source in [
            "const {s} = {s: () => silence}; s(\"in:2323\")",
            "const {sound: s} = {sound: () => silence}; s(\"in:2323\")",
            "const {...s} = {}; s(\"in:2323\")",
            "globalThis.s = () => silence; s(\"in:2323\")",
        ] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn a_final_constant_channel_is_marked_instead_of_the_earlier_sound_word() {
        for source in [
            "s(\"in:0 in:1\").n(2323)",
            "s(\"in:2323\").n(0).n((2323))",
            "(s(\"in:0\")).n(2323)",
            "stack(s(\"in:0\"), s(\"sine\")).n(2323)",
        ] {
            let findings = input_findings(source, false, Some(2));
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert_eq!(&source[findings[0].from..findings[0].to], "2323");
            assert_eq!(findings[0].from, source.rfind("2323").unwrap());
        }
    }

    #[test]
    fn rewritten_or_dynamic_channels_are_not_refused_from_superseded_literals() {
        for source in [
            "s(\"in:2323\").n(0)",
            "sound(\"in:2323\").n(1.9)",
            "s(\"in:2323\").n(sine)",
            "s(\"in:2323\").n(\"0 1\")",
            "s(\"in:2323\").s(\"sine\")",
            "s(\"in:2323\").scale(\"C:major\")",
            "s(\"in:2323\").fmap(() => ({s:'in', n:0}))",
            "s(\"in:2323\").gain({value: 1, n: 0})",
            "(s(\"in:2323\")).n(0)",
            "stack(s(\"in:2323\"), s(\"sine\")).n(0)",
            "cat(s(\"in:2323\"), s(\"sine\")).n(0)",
            "note(\"in:2323\")",
            "s(\"begin:2323\")",
        ] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
        let single_quoted = "s('in:2323')";
        let findings = input_findings(single_quoted, false, Some(2));
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(&single_quoted[findings[0].from..findings[0].to], "in:2323");
        assert!(lint_with("note('in')", false, None, &LintContext::default()).is_empty());
        // A sibling's override does not rewrite the first input.
        let source = "stack(s(\"in:2323\"), s(\"in:2323\").n(0))";
        let findings = input_findings(source, false, Some(2));
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].from, source.find("in:2323").unwrap());
    }

    #[test]
    fn numeric_gain_and_filter_sliders_do_not_hide_invalid_input_channels() {
        for source in [
            "$: s(\"in:2323\").gain(slider(1,0,2))",
            "s(\"in:2323\").lpf(slider(800, 100, 10000, .01))",
            "s(\"in:2323\").gain(slider(1)).lpf(slider(800, 100, 10000))",
            "s(\"in:2323\").gain(slider /* live level */ (1, 0, 2,))",
        ] {
            let findings = input_findings(source, false, None);
            assert_eq!(findings.len(), 1, "{source}: {findings:?}");
            assert_eq!(&source[findings[0].from..findings[0].to], "in:2323");
        }
        assert_eq!(
            input_findings("s(\"in:2\").gain(slider(1,0,2))", false, Some(2)).len(),
            1
        );
        for source in [
            "s(\"in:1\").gain(slider(1,0,2)).lpf(slider(800,100,10000))",
            "s(\"in:2323\").gain(slider(1,0,2)).n(0)",
            "const slider = () => ({value: 1, n: 0}); s(\"in:2323\").gain(slider(1,0,2))",
            "s(\"in:2323\").gain(slider({value: 1, n: 0}))",
        ] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
        let context = LintContext {
            input_channels: Some(2),
            setup_names: vec!["slider".into()],
            ..LintContext::default()
        };
        let source = "s(\"in:2323\").gain(slider(1,0,2))";
        let findings = lint_with(source, false, None, &context);
        assert!(
            findings
                .iter()
                .all(|finding| !finding.message.contains("audio input")),
            "{findings:?}"
        );
    }

    #[test]
    fn visible_local_and_setup_bindings_are_not_assumed_to_be_builtin_containers() {
        for source in [
            "const stack = p => p.n(0); stack(s(\"in:2323\"))",
            "function cat(p) { return p.n(0); } cat(s(\"in:2323\"))",
            "const s = () => silence; s(\"in:2323\")",
            "function fix(stack) { return stack(s(\"in:2323\")); }",
        ] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
        for name in ["stack", "cat", "slowcat", "fastcat"] {
            let source = format!("{name}(s(\"in:2323\"))");
            let context = LintContext {
                input_channels: Some(2),
                setup_names: vec![name.into()],
                ..LintContext::default()
            };
            let findings = lint_with(&source, false, None, &context);
            assert!(
                findings
                    .iter()
                    .all(|finding| !finding.message.contains("audio input")),
                "{source}: {findings:?}"
            );
        }
    }

    /// JavaScript skips a vertical tab or a no-break space between a name and
    /// its `(`, so a call spaced by either is still a call: an unknown one stops
    /// the check, where a bare bracket does not.
    #[test]
    fn a_call_spaced_from_its_bracket_by_unicode_whitespace_is_still_a_call() {
        for space in ["", " ", "\u{b}", "\u{a0}"] {
            let source = format!("$: foo{space}(s(\"in:2323\"))");
            assert!(
                input_findings(&source, false, None).is_empty(),
                "{source:?}"
            );
        }
        assert_eq!(input_findings("$: (s(\"in:2323\"))", false, None).len(), 1);
    }

    /// A longer name that contains a builtin's name does not shadow it.
    #[test]
    fn a_longer_name_containing_a_builtin_does_not_shadow_it() {
        for source in [
            "const mystack = 1; stack(s(\"in:2323\"))",
            "const stacked = 1; stack(s(\"in:2323\"))",
            "const $s = 1; s(\"in:2323\")",
            "const s_ = 1; s(\"in:2323\")",
        ] {
            assert_eq!(input_findings(source, false, None).len(), 1, "{source}");
        }
    }

    /// The chain before a bare input word is read through any whitespace
    /// between its links, backward to its head and forward again from there, so
    /// the `n` it sets still names the channel and a later `.n(…)` still
    /// replaces it.
    #[test]
    fn a_preceding_channel_is_read_through_any_whitespace_between_links() {
        for space in ["", " ", "\n  ", "\u{b}", "\u{a0}"] {
            let source = format!("n(3){space}.{space}gain(.5){space}.{space}s(\"in\")");
            let findings = input_findings(&source, false, Some(2));
            assert_eq!(findings.len(), 1, "{source:?}: {findings:?}");
            assert_eq!(&source[findings[0].from..findings[0].to], "3", "{source:?}");
            let source = format!("n(3){space}.{space}n(1){space}.{space}s(\"in\")");
            let findings = input_findings(&source, false, Some(2));
            assert!(findings.is_empty(), "{source:?}: {findings:?}");
        }
        // JavaScript plays channel 1 here: the `.n(1)` replaces the `n(3)`.
        assert!(input_findings("n(3)\u{a0}.n(1).s(\"in\")", false, Some(2)).is_empty());
        for source in [
            "x.n(3).s(\"in\")",
            "(a).n(3).s(\"in\")",
            "n(3)[0].s(\"in\")",
        ] {
            assert!(
                input_findings(source, false, Some(2)).is_empty(),
                "{source}"
            );
        }
        // Inside an enclosing call each link's `)` still closes its own `(`,
        // not the call's.
        let source = "stack(n(3).gain(.5).s(\"in\"))";
        let findings = input_findings(source, false, Some(2));
        assert_eq!(findings.len(), 1, "{source}: {findings:?}");
        assert_eq!(&source[findings[0].from..findings[0].to], "3");
    }

    /// The chain after an input is read through the same whitespace: a later
    /// `.n(…)` still replaces the channel, a link nothing declares still stops
    /// the check, the call and the brackets around it still close where
    /// JavaScript closes them, a spaced call to `s` elsewhere is still a call
    /// rather than a binding, and the sample-control note still finds its
    /// controls.
    #[test]
    fn the_chain_after_an_input_is_read_through_any_whitespace() {
        for space in ["", " ", "\n  ", "\u{b}", "\u{a0}"] {
            for source in [
                format!("s(\"in:2323\"){space}.{space}gain(1)"),
                format!("s(\"in:2323\"{space})"),
                format!("stack(s(\"in:2323\"){space})"),
                format!("(s(\"in:2323\"){space})"),
                format!("s(\"in:2323\")\n$: s{space}(\"hh\")"),
            ] {
                let findings = input_findings(&source, false, None);
                assert_eq!(findings.len(), 1, "{source:?}: {findings:?}");
                assert_eq!(
                    &source[findings[0].from..findings[0].to],
                    "in:2323",
                    "{source:?}"
                );
            }
            for source in [
                format!("s(\"in:5\"){space}.{space}n(0)"),
                format!("s(\"in:5\"){space}.{space}custom(1)"),
            ] {
                let findings = input_findings(&source, false, Some(2));
                assert!(findings.is_empty(), "{source:?}: {findings:?}");
            }
            let source = format!("s(\"in\"){space}.{space}speed(.5)");
            assert_eq!(input_notes(&source).len(), 1, "{source:?}");
        }
    }

    /// A declaration shadows a builtin whatever whitespace stands between its
    /// keyword and its name, as `function\u{a0}s` still declares `s`.
    #[test]
    fn a_declaration_spaced_by_any_whitespace_shadows_a_builtin() {
        for space in [" ", "\n", "\u{b}", "\u{a0}"] {
            for declaration in [
                format!("function{space}s(x) {{ return x }}"),
                format!("const{space}s = sound"),
            ] {
                let source = format!("{declaration}\n$: s(\"in:2323\")");
                let findings = input_findings(&source, false, None);
                assert!(findings.is_empty(), "{source:?}: {findings:?}");
            }
        }
    }

    /// Only the calls around the sound decide whether its controls survive, the
    /// innermost first, and a bracket closed before it is none of them.
    #[test]
    fn the_calls_around_an_input_are_read_innermost_first() {
        for source in [
            "stack(cat(s(\"in:2323\")))",
            "stack(f(1), s(\"in:2323\"))",
            "(stack(s(\"in:2323\")))",
            "stack((1), s(\"in:2323\"))",
        ] {
            assert_eq!(input_findings(source, false, None).len(), 1, "{source}");
        }
        for source in [
            "custom(stack(s(\"in:2323\")))",
            "stack(custom(s(\"in:2323\")))",
            "stack(s(\"in:2323\").fast(2)).custom(1)",
        ] {
            assert!(input_findings(source, false, None).is_empty(), "{source}");
        }
    }

    #[test]
    fn native_input_validation_does_not_depend_on_sample_imports_or_bank_names() {
        let library = SampleLibrary::empty();
        library
            .register_trusted_custom(
                r#"{"in":["http://127.0.0.1:9/input.wav"],"bd":["http://127.0.0.1:9/kick.wav"]}"#,
                None,
            )
            .unwrap();
        let context = LintContext {
            input_channels: Some(2),
            ..LintContext::default()
        };
        for source in [
            "samples('github:me/arriving')\ns(\"in:2323\")",
            "samples({in:['x.wav']})\ns(\"in:2323\")",
            "s(\"in:2323\").bank('other')",
        ] {
            let findings = lint_with(source, false, Some(&library), &context);
            assert_eq!(
                findings
                    .iter()
                    .filter(|finding| finding.message.contains("audio input"))
                    .count(),
                1,
                "{source}: {findings:?}"
            );
        }
        // Custom sample counts do not change native input's channel rules;
        // ordinary explicit sample checks and wrapped `.n(...)` stay intact.
        assert!(lint_with("s(\"in:1.9\")", false, Some(&library), &context).is_empty());
        assert!(lint_with("s(\"bd\").n(2323)", false, Some(&library), &context).is_empty());
        let findings = lint_with("s(\"bd:2323\")", false, Some(&library), &context);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].level, Level::Note);
        assert!(findings[0].message.starts_with("bd:2323 plays bd:0 "));
    }
}
