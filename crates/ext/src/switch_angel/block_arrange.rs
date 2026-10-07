use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::controls;
use rustel_core::extension_node::{ExtensionPatternNode, query_inner};
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Hap, Pattern, State, Value};

use super::ORIGIN;
use super::shared::{pure_list, value_as_pattern};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "blockArrange",
    "gate source blocks with symbolic masks",
    "Each lane is a [sources, mask] pair: wherever the mask is non-zero the lane's sources sound, stacked together. Mask values double as markers - one containing R restarts the source at the block, one containing B runs it backwards (speed -1). The optional second argument holds [predicate, transformer] pairs: where a predicate is truthy for a marker, its transformer rewrites the source.",
    params: [
        ReferenceParam {
            name: "lanes",
            r#type: "array",
            description: "array of [patterns, mask] lanes; a lane may hold one source pattern or a list of them, and the mask decides where they sound.",
        },
        ReferenceParam {
            name: "modifiers",
            r#type: "array",
            description: "optional array of [predicate, transformer] pairs applied per mask marker; omitted means no modifiers.",
        },
    ],
    examples: ["blockArrange([[note(\"c4 e4 g4\").s(\"sawtooth\"), \"<1 0 1 0>\"], [note(\"c2*4\").s(\"square\"), \"<0 1 0 1>\"]])"],
    "arrangement"
);

#[derive(Clone, Copy)]
struct BlockModifier {
    predicate_child: usize,
    transform_child: usize,
}

struct BlockLaneNode {
    children: Vec<Pattern>,
    source_count: usize,
    modifiers: Vec<BlockModifier>,
}

impl BlockLaneNode {
    fn mask(&self) -> &Pattern {
        &self.children[0]
    }

    fn sources(&self) -> &[Pattern] {
        &self.children[1..=self.source_count]
    }

    fn function(&self, child: usize) -> Option<rustel_core::value::FunctionRef> {
        self.children.get(child)?.as_pure()?.as_function().cloned()
    }
}

impl ExtensionPatternNode for BlockLaneNode {
    fn kind(&self) -> &'static str {
        "switch_angel.block_arrange.lane"
    }

    /// Selects among its children by their own answers; nothing remembered.
    fn volatile(&self) -> bool {
        false
    }

    fn children(&self) -> &[Pattern] {
        &self.children
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        let mut output = Vec::new();
        for outer in self.mask().query(state) {
            if ComposeOp::Eq.apply_scalar(&outer.value, &Value::F64(0.0)) == Value::Bool(true) {
                continue;
            }
            let marker = outer.value.show();
            let mut selected = Vec::with_capacity(self.source_count);
            for source in self.sources() {
                let mut source = source.clone();
                if marker.contains('R') {
                    source = rustel_core::compose::compose(
                        &source,
                        &rustel_core::pure(Value::F64(1.0)),
                        ComposeOp::KeepIf,
                        Alignment::Restart,
                    );
                }
                if marker.contains('B') {
                    source = controls::apply_pattern(
                        "speed",
                        &source.rev(),
                        &rustel_core::pure(Value::F64(-1.0)),
                    );
                }
                for modifier in &self.modifiers {
                    let Some(predicate) = self.function(modifier.predicate_child) else {
                        rustel_core::signal_query_error(|| {
                            "blockArrange modifier predicate is not a function".into()
                        });
                        return Vec::new();
                    };
                    if !predicate
                        .apply_value(&Value::Str(marker.clone()))
                        .js_truthy()
                    {
                        continue;
                    }
                    let Some(transform) = self.function(modifier.transform_child) else {
                        rustel_core::signal_query_error(|| {
                            "blockArrange modifier callback is not a pattern transformer".into()
                        });
                        return Vec::new();
                    };
                    source = transform.apply(source);
                }
                selected.push(source);
            }
            output.extend(query_inner(&outer, &rustel_core::stack(selected), state));
        }
        output
    }

    fn drain_children(&mut self, output: &mut Vec<Pattern>) {
        output.append(&mut self.children);
    }
}

fn modifiers(pattern: Option<&Pattern>) -> Result<Vec<(Pattern, Pattern)>, &'static str> {
    let Some(pattern) = pattern else {
        return Ok(Vec::new());
    };
    let Some(values) = pure_list(pattern) else {
        return Err("blockArrange modifiers must be an array");
    };
    values
        .iter()
        .map(|value| {
            let Value::List(pair) = value else {
                return Err("blockArrange modifiers must be [predicate, callback] pairs");
            };
            let Some(predicate) = pair.first().and_then(Value::as_function) else {
                return Err("blockArrange modifier predicate must be a function");
            };
            let Some(transform) = pair.get(1).and_then(Value::as_function) else {
                return Err("blockArrange modifier callback must be a function");
            };
            Ok((
                rustel_core::pure(Value::Function(predicate.clone())),
                rustel_core::pure(Value::Function(transform.clone())),
            ))
        })
        .collect()
}

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Result<Pattern, &'static str> {
    let Some(lanes) = args.first().and_then(pure_list) else {
        return Err("blockArrange requires an array of [patterns, mask] lanes");
    };
    let modifiers = modifiers(args.get(1))?;
    let mut output = Vec::with_capacity(lanes.len());
    for lane in lanes {
        let Value::List(pair) = lane else {
            return Err("blockArrange lanes must be [patterns, mask] pairs");
        };
        let Some(sources) = pair.first() else {
            return Err("blockArrange lane is missing its source pattern");
        };
        let sources = match sources {
            Value::List(sources) => sources.iter().map(value_as_pattern).collect::<Vec<_>>(),
            source => vec![value_as_pattern(source)],
        };
        let Some(mask) = pair.get(1) else {
            return Err("blockArrange lane is missing its mask pattern");
        };
        let source_count = sources.len();
        let mut children = Vec::with_capacity(1 + source_count + modifiers.len() * 2);
        children.push(value_as_pattern(mask));
        children.extend(sources);
        let mut node_modifiers = Vec::with_capacity(modifiers.len());
        for (predicate, transform) in &modifiers {
            let predicate_child = children.len();
            children.push(predicate.clone());
            let transform_child = children.len();
            children.push(transform.clone());
            node_modifiers.push(BlockModifier {
                predicate_child,
                transform_child,
            });
        }
        output.push(Pattern::extension_node(BlockLaneNode {
            children,
            source_count,
            modifiers: node_modifiers,
        }));
    }
    Ok(rustel_core::stack(output))
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["blockArrange"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Fallible(apply),
};
