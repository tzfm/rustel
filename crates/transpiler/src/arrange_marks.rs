//! Source spans of `arrange` sections, for the editor's play marks.

use oxc_allocator::Allocator;
use oxc_ast::ast::{BindingIdentifier, CallExpression, Expression};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use super::Splice;

/// One splice for each `[cycles, pattern]` section written in an `arrange`
/// call.
///
/// The splice appends the source span of `cycles` to the section, so
/// `arrange([8, lead])` becomes `arrange([8, lead, 9, 10])`. `arrange` puts
/// the span on the events of the section, and the editor marks the count of
/// the section while the section sounds.
///
/// A score with its own `arrange` binding gets no splice.
pub(super) fn splices(source: &str, block_offset: usize) -> Vec<Splice> {
    if !source.contains("arrange") {
        return Vec::new();
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if parsed.panicked {
        return Vec::new();
    }
    let mut sections = Sections {
        block_offset,
        shadowed: false,
        splices: Vec::new(),
    };
    sections.visit_program(&parsed.program);
    if sections.shadowed {
        return Vec::new();
    }
    sections.splices
}

struct Sections {
    block_offset: usize,
    shadowed: bool,
    splices: Vec<Splice>,
}

impl<'a> Visit<'a> for Sections {
    fn visit_binding_identifier(&mut self, binding: &BindingIdentifier<'a>) {
        self.shadowed |= binding.name == "arrange";
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if call.callee.is_specific_id("arrange") && !call.optional {
            for argument in &call.arguments {
                if let Some(Expression::ArrayExpression(section)) = argument.as_expression()
                    && let [cycles, pattern] = section.elements.as_slice()
                    && let Some(cycles) = cycles.as_expression()
                    && let Some(pattern) = pattern.as_expression()
                {
                    let cycles = cycles.span();
                    self.splices.push((
                        pattern.span().end as usize,
                        format!(
                            ", {}, {}",
                            cycles.start as usize + self.block_offset,
                            cycles.end as usize + self.block_offset
                        ),
                    ));
                }
            }
        }
        walk::walk_call_expression(self, call);
    }
}

#[cfg(test)]
mod tests {
    use crate::{TranspileOptions, transpile};

    /// The printed score with no whitespace, so the printer's line breaks
    /// stay out of the test.
    fn output(source: &str, block_offset: usize) -> String {
        let options = TranspileOptions {
            add_return: false,
            block_offset,
            ..TranspileOptions::default()
        };
        transpile(source, &options)
            .output
            .split_whitespace()
            .collect()
    }

    #[test]
    fn written_sections_carry_the_span_of_their_cycle_count() {
        assert_eq!(
            output("arrange([8, lead], [2 * 2, \"bd\",], ...rest, outro)", 0),
            "arrange([8,lead,9,10],[2*2,m('bd',27),20,25],...rest,outro);"
        );
        assert_eq!(
            output("arrange([4, arrange([1, a])])", 100),
            "arrange([4,arrange([1,a,121,122]),109,110]);"
        );
    }

    #[test]
    fn a_score_with_its_own_arrange_keeps_its_sections() {
        assert_eq!(
            output("const arrange = stepcat;\narrange([8, lead]);", 0),
            "constarrange=stepcat;arrange([8,lead]);"
        );
    }
}
