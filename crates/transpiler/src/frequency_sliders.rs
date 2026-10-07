//! Native slider travel metadata, independent of the generated JavaScript.

use std::collections::{BTreeMap, BTreeSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{Argument, BindingIdentifier, CallExpression, Expression};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::SourceType;

use super::WidgetRecord;

// The first control in each registry row carries frequency. The remaining
// controls in those rows (resonance/envelope) deliberately do not qualify.
const CONTROLS: &[&str] = &[
    "cutoff", "ctf", "lpf", "lp", "hcutoff", "hpf", "hp", "bandf", "bpf", "bp", "freq",
];

/// Mark which sliders travel logarithmically, and name the call each one
/// sits inside.
///
/// The two go together because they are the same walk: a slider's travel
/// depends on whether its enclosing call is a frequency control, and its
/// label IS that enclosing call's name. `.lpf(slider(800, 100, 4000))` is a
/// frequency slider called `lpf`. A slider that is nobody's first
/// argument - a bare `slider(0.5)` in a variable - has no label, and the
/// mixer numbers it instead.
pub(super) fn annotate(source: &str, widgets: &mut [WidgetRecord], block_offset: usize) {
    if !widgets.iter().any(|widget| widget.widget_type == "slider") {
        return;
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    // A recovered program still has exact spans for intact calls. An error
    // elsewhere must not change a slider's travel under the pointer.
    if parsed.panicked {
        return;
    }
    let syntax_errors = parsed
        .diagnostics
        .iter()
        .flat_map(|error| {
            error.labels.iter().map(|label| {
                let start = label.offset() as usize;
                (start, start.saturating_add(label.len().max(1) as usize))
            })
        })
        .collect::<Vec<_>>();
    let mut bindings = Bindings::default();
    bindings.visit_program(&parsed.program);
    let mut calls = FrequencyCalls {
        bindings: &bindings.names,
        syntax_errors: &syntax_errors,
        sliders: BTreeSet::new(),
        labels: BTreeMap::new(),
    };
    calls.visit_program(&parsed.program);
    for widget in widgets {
        if widget.widget_type != "slider" {
            continue;
        }
        let span = (
            widget.call_from.saturating_sub(block_offset),
            widget.call_to.saturating_sub(block_offset),
        );
        widget.frequency = calls.sliders.contains(&span);
        widget.label = calls.labels.get(&span).cloned();
    }
}

#[derive(Default)]
struct Bindings {
    names: BTreeSet<String>,
}

impl<'a> Visit<'a> for Bindings {
    fn visit_binding_identifier(&mut self, binding: &BindingIdentifier<'a>) {
        let name = binding.name.as_str();
        if name == "slider" || CONTROLS.contains(&name) {
            self.names.insert(name.to_owned());
        }
    }
}

struct FrequencyCalls<'b> {
    // Any local declaration is enough to decline a bare global call. This
    // intentionally favors false negatives across scopes over treating a
    // user-defined function as a known control.
    bindings: &'b BTreeSet<String>,
    syntax_errors: &'b [(usize, usize)],
    sliders: BTreeSet<(usize, usize)>,
    /// The name of the call each slider is the first argument of.
    labels: BTreeMap<(usize, usize), String>,
}

/// What a call is called, for a label: `lpf` for both `lpf(...)` and
/// `.lpf(...)`.
fn callee_name(call: &CallExpression<'_>) -> Option<String> {
    match call.callee.without_parentheses() {
        Expression::Identifier(identifier) => Some(identifier.name.as_str().to_owned()),
        Expression::StaticMemberExpression(member) if !member.optional => {
            Some(member.property.name.as_str().to_owned())
        }
        _ => None,
    }
}

impl<'a> Visit<'a> for FrequencyCalls<'_> {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        let control = match call.callee.without_parentheses() {
            Expression::Identifier(identifier) => {
                let name = identifier.name.as_str();
                CONTROLS.contains(&name) && !self.bindings.contains(name)
            }
            Expression::StaticMemberExpression(member) => {
                CONTROLS.contains(&member.property.name.as_str()) && !member.optional
            }
            _ => false,
        };
        // The label is every call's, not only a frequency control's: the
        // mixer wants "gain" as much as it wants "lpf". Recorded before the
        // frequency gate so the two questions stay separate.
        if !call.optional
            && let Some(name) = callee_name(call)
            && let Some(argument) = call.arguments.first().and_then(Argument::as_expression)
            && let Expression::CallExpression(slider) = argument.without_parentheses()
            && slider.callee.is_specific_id("slider")
            && !slider.optional
        {
            self.labels
                .insert((slider.span.start as usize, slider.span.end as usize), name);
        }
        if control
            && !call.optional
            // Recovery may omit a malformed argument. Do not infer a
            // direct binding from any call touched by a syntax diagnostic.
            && !self.syntax_errors.iter().any(|&(start, end)|
                start < call.span.end as usize && end > call.span.start as usize)
            && !self.bindings.contains("slider")
            && let Some(argument) = call.arguments.first().and_then(Argument::as_expression)
            && let Expression::CallExpression(slider) = argument.without_parentheses()
            && slider.callee.is_specific_id("slider")
            && !slider.optional
        {
            self.sliders
                .insert((slider.span.start as usize, slider.span.end as usize));
        }
        walk::walk_call_expression(self, call);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TranspileOptions, transpile};

    fn sliders(source: &str) -> Vec<WidgetRecord> {
        transpile(source, &TranspileOptions::default())
            .widgets
            .into_iter()
            .filter(|widget| widget.widget_type == "slider")
            .collect()
    }

    /// A fader on a desk away from the score has to say what it is
    /// attached to, and the call it sits in is the only name it has.
    #[test]
    fn a_slider_is_named_by_the_call_it_sits_in() {
        for (source, expected) in [
            ("s('saw').lpf(slider(800, 100, 4000))", Some("lpf")),
            ("gain(slider(0.5))", Some("gain")),
            // A method chain names the method, not the receiver.
            ("s('bd').room(slider(0.3)).gain(0.8)", Some("room")),
            // Nobody's first argument: no name, and the desk numbers it.
            ("const cutoff = slider(0.5)", None),
            // Not the FIRST argument either - the label would be a lie
            // about which parameter this is.
            ("s('bd').range(0, slider(1))", None),
        ] {
            let widgets = sliders(source);
            assert_eq!(widgets.len(), 1, "{source}");
            assert_eq!(widgets[0].label.as_deref(), expected, "{source}");
        }

        // Two sliders in one score keep their own names.
        let widgets = sliders("s('saw').lpf(slider(800, 100, 4000)).gain(slider(0.5))");
        assert_eq!(
            widgets
                .iter()
                .map(|w| w.label.as_deref())
                .collect::<Vec<_>>(),
            [Some("lpf"), Some("gain")]
        );

        // A label is not a frequency: naming happens for every control.
        let widgets = sliders("gain(slider(0.5))");
        assert!(!widgets[0].frequency);
        assert_eq!(widgets[0].label.as_deref(), Some("gain"));
    }

    #[test]
    fn direct_frequency_controls_and_registry_aliases_are_classified() {
        for name in CONTROLS {
            for source in [
                format!("{name}(slider(440, 20, 20000))"),
                format!("s('saw').{name}(slider(440, 20, 20000))"),
            ] {
                let widgets = sliders(&source);
                assert_eq!(widgets.len(), 1, "{source}");
                assert!(widgets[0].frequency, "{source}");
            }
        }
    }

    #[test]
    fn parentheses_comments_offsets_and_multiple_arguments_keep_exact_call_identity() {
        let source = "s('saw').lpf /* slider(123) */ ( /* cutoff */ ((slider(440, 20, 20000))), slider(1, 0, 10))";
        let widgets = transpile(
            source,
            &TranspileOptions {
                block_offset: 37,
                ..Default::default()
            },
        )
        .widgets;
        assert_eq!(
            widgets
                .iter()
                .map(|widget| widget.frequency)
                .collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(
            &source[widgets[0].call_from - 37..widgets[0].call_to - 37],
            "slider(440, 20, 20000)"
        );
    }

    #[test]
    fn computed_or_shared_values_and_nonfrequency_controls_keep_linear_metadata() {
        for source in [
            "lpf(slider(440, 20, 20000) * 2)",
            "lpf(+slider(440, 20, 20000))",
            "lpf(Math.abs(slider(440, 20, 20000)))",
            "lpf([slider(440, 20, 20000)])",
            "lpf((slider(440, 20, 20000), 1000))",
            "lpf(ok ? slider(440, 20, 20000) : 1000)",
            "const value = slider(440, 20, 20000); lpf(value)",
            "mystery(slider(440, 20, 20000))",
            "gain(slider(440, 20, 20000))",
            "resonance(slider(440, 20, 20000))",
            "lpenv(slider(440, 20, 20000))",
            "bandq(slider(440, 20, 20000))",
            "lpf(object.slider(440, 20, 20000))",
            "pattern['lpf'](slider(440, 20, 20000))",
            "pattern?.lpf(slider(440, 20, 20000))",
            "function lpf(value) { return value }; lpf(slider(440, 20, 20000))",
            "function example(slider) { return lpf(slider(440, 20, 20000)) }",
        ] {
            assert!(
                sliders(source).iter().all(|widget| !widget.frequency),
                "{source}"
            );
        }
    }

    #[test]
    fn comments_strings_and_regexes_do_not_create_frequency_context() {
        let source = r#"const text = 'lpf(slider(440, 20, 20000))';
const matchText = /lpf\(slider\(440\)\)/;
// lpf(slider(440, 20, 20000))
gain(slider(440, 20, 20000));"#;
        let widgets = sliders(source);
        assert_eq!(widgets.len(), 1);
        assert!(!widgets[0].frequency);
    }

    #[test]
    fn metadata_does_not_change_generated_slider_javascript() {
        let output = transpile("lpf(slider(440, 20, 20000))", &TranspileOptions::default());
        assert!(output.diagnostics.is_empty());
        assert!(output.output.contains("sliderWithID("));
        assert!(!output.output.contains("frequency"));
        assert!(!output.output.contains("log"));
        assert_eq!(output.widgets[0].value.as_deref(), Some("440"));
    }

    #[test]
    fn unrelated_recoverable_errors_keep_intact_frequency_calls() {
        let mut recovered = 0;
        for suffix in [
            "const x;",
            "const regex = /x/gg;",
            "const x = ;",
            "s('bd').",
        ] {
            let source = format!("s('saw').lpf(slider(1000,100,10000,.01));\n{suffix}");
            let allocator = Allocator::default();
            let parsed = Parser::new(&allocator, &source, SourceType::mjs()).parse();
            assert!(
                !parsed.diagnostics.is_empty(),
                "the regression must contain a real error: {suffix}"
            );
            let widgets = sliders(&source);
            assert_eq!(widgets.len(), 1);
            assert_eq!(widgets[0].frequency, !parsed.panicked, "{suffix}");
            recovered += usize::from(!parsed.panicked);
        }
        assert!(recovered > 0, "exercise an actual recovered AST");
    }

    #[test]
    fn a_recovered_error_inside_the_control_does_not_invent_frequency_metadata() {
        let mut recovered = 0;
        for source in [
            "lpf(, slider(1000,100,10000,.01))",
            "lpf(slider(1000,100,10000,.01) + )",
            "lpf(slider(1000,100,10000,.01), /x/gg)",
            "lpf(slider(1000,100,10000,.01), () => { const x; })",
        ] {
            let allocator = Allocator::default();
            let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
            assert!(!parsed.diagnostics.is_empty());
            let widgets = sliders(source);
            assert_eq!(widgets.len(), 1);
            assert!(!widgets[0].frequency, "{source}");
            recovered += usize::from(!parsed.panicked);
        }
        assert!(recovered > 0, "exercise an actual recovered local error");
    }
}
