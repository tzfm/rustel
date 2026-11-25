//! Live numbers: numeric literals a host moves while the score plays.
//!
//! A host names the literals by their byte ranges in the score. Each one
//! that stands alone as a value - an argument, an object entry or an array
//! element - is evaluated as a live cell, the same cell a `slider(…)` reads,
//! so the host sets it without a new evaluation. The score text stays as it
//! is. A range that is anything else is ignored: the score keeps its meaning.

use std::ops::Range;

use super::{CallSite, RegexLiteralRanges, WidgetRecord, scan_comment, scan_quoted};

/// The widget type of a live number's record.
pub const LIVE_NUMBER_WIDGET: &str = "live_number";

/// A live number's id: the score span of its literal, as a slider's.
pub fn live_number_id(literal: Range<usize>) -> String {
    super::slider_widget_id(literal)
}

/// The literal range a live number's id names: the inverse of
/// [`live_number_id`].
pub fn live_number_range(id: &str) -> Option<Range<usize>> {
    let (from, to) = id.split_once(':')?;
    Some(from.parse().ok()?..to.parse().ok()?)
}

/// The records and splice sites of the live numbers in `source`, in source
/// order. `taken` holds the spans of the recorded widget calls: a number in
/// one of them belongs to that call, so it stays as written.
pub(super) fn collect(
    source: &str,
    ranges: &[Range<usize>],
    block_offset: usize,
    regexes: &RegexLiteralRanges,
    taken: &[Range<usize>],
) -> Vec<(WidgetRecord, CallSite)> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut wanted: Vec<&Range<usize>> = ranges.iter().collect();
    wanted.sort_by_key(|range| range.start);
    wanted.dedup();
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut next = wanted.into_iter().peekable();
    let mut index = 0usize;
    while index < bytes.len() {
        // A wanted range that starts before the walk is inside a string, a
        // comment or a regex the walk jumped over.
        while next.peek().is_some_and(|range| range.start < index) {
            next.next();
        }
        let Some(range) = next.peek() else {
            break;
        };
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            index = scan_quoted(bytes, index, bytes[index]);
            continue;
        }
        if bytes[index..].starts_with(b"//") || bytes[index..].starts_with(b"/*") {
            index = scan_comment(source, index);
            continue;
        }
        if let Some(end) = regexes.end_at(index) {
            index = end;
            continue;
        }
        if index == range.start {
            let range = (*range).clone();
            next.next();
            if let Some(value) = standalone_number(source, range.clone())
                && !taken
                    .iter()
                    .any(|call| call.start <= range.start && range.end <= call.end)
            {
                found.push(record(range, value, block_offset));
            }
        }
        index += 1;
    }
    found
}

/// The number a range holds when it is one numeric literal that stands
/// alone as a value: after `(`, `,`, `:` or `[`, and before `)`, `,`, `}`
/// or `]`. A literal inside an expression - `x - 4`, `4 * y`, `4.toFixed()` -
/// is part of that expression, and a cell in its place would change it.
fn standalone_number(source: &str, range: Range<usize>) -> Option<String> {
    let text = source.get(range.clone())?;
    let numeric = !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'+' | b'e' | b'E'))
        && text.parse::<f64>().is_ok_and(f64::is_finite);
    let before = source[..range.start].trim_end().bytes().last();
    let after = source[range.end..].trim_start().bytes().next();
    (numeric
        && matches!(before, Some(b'(' | b',' | b':' | b'['))
        && matches!(after, Some(b')' | b',' | b'}' | b']')))
    .then(|| text.to_owned())
}

fn record(literal: Range<usize>, value: String, block_offset: usize) -> (WidgetRecord, CallSite) {
    let from = literal.start + block_offset;
    let to = literal.end + block_offset;
    let record = WidgetRecord {
        from,
        to,
        call_from: from,
        call_to: to,
        index: 0,
        widget_type: LIVE_NUMBER_WIDGET.into(),
        id: live_number_id(from..to),
        visual_slot: None,
        value: Some(value),
        // A live number has no bounds of its own: the host keeps its own,
        // and the cell takes any finite value.
        min: None,
        max: None,
        step: None,
        options: None,
        deferred_rendering: false,
        frequency: false,
        label: None,
    };
    (
        record,
        CallSite::Number {
            start: literal.start,
            end: literal.end,
        },
    )
}

#[cfg(test)]
mod tests {
    use crate::{TranspileOptions, transpile};

    use super::*;

    fn options(ranges: Vec<Range<usize>>) -> TranspileOptions {
        TranspileOptions {
            add_return: false,
            live_numbers: ranges,
            ..TranspileOptions::default()
        }
    }

    fn range_of(source: &str, needle: &str) -> Range<usize> {
        let start = source.find(needle).expect("the needle");
        start..start + needle.len()
    }

    #[test]
    fn a_live_number_reads_a_cell_and_the_score_keeps_its_text() {
        let source = "s(\"saw\").lpf(4000).gain(.5)";
        let cutoff = range_of(source, "4000");
        let result = transpile(source, &options(vec![cutoff.clone()]));
        let id = live_number_id(cutoff.clone());
        assert!(
            result
                .output
                .contains(&format!("lpf(sliderWithID('{id}', 4e3))")),
            "{}",
            result.output
        );
        assert!(result.output.contains("gain(0.5)"), "{}", result.output);
        let live: Vec<_> = result
            .widgets
            .iter()
            .filter(|widget| widget.widget_type == LIVE_NUMBER_WIDGET)
            .collect();
        assert_eq!(live.len(), 1);
        assert_eq!((live[0].from, live[0].to), (cutoff.start, cutoff.end));
        assert_eq!(live[0].value.as_deref(), Some("4000"));
    }

    #[test]
    fn an_object_entry_and_a_negative_number_are_live_too() {
        let source = "s(\"bd\").set({ cutoff: 800, pan: -0.5 })";
        let cutoff = range_of(source, "800");
        let pan = range_of(source, "-0.5");
        let result = transpile(source, &options(vec![pan.clone(), cutoff.clone()]));
        assert_eq!(
            result.output.matches("sliderWithID").count(),
            2,
            "{}",
            result.output
        );
        assert!(
            result.output.contains(&format!(
                "pan: sliderWithID('{}', -0.5)",
                live_number_id(pan)
            )),
            "{}",
            result.output
        );
    }

    #[test]
    fn a_number_inside_an_expression_a_string_or_a_slider_stays_as_written() {
        let cases = [
            ("s(\"bd\").gain(1 - 0.5)", "0.5"),
            ("s(\"bd\").gain(\"0.5\")", "0.5"),
            ("s(\"bd\").gain(.5) // gain(4)", "4"),
            ("s(\"bd\").gain(slider(0.5, 0, 1))", "0.5"),
            ("s(\"bd\").gain(0.5.valueOf())", "0.5"),
        ];
        for (source, needle) in cases {
            let range = range_of(source, needle);
            let plain = transpile(source, &options(Vec::new()));
            let live = transpile(source, &options(vec![range]));
            assert_eq!(live.output, plain.output, "{source}");
            assert!(
                !live
                    .widgets
                    .iter()
                    .any(|widget| widget.widget_type == LIVE_NUMBER_WIDGET),
                "{source}"
            );
        }
    }
}
