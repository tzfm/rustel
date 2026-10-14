//! Bounded KabelSalat programs and allocation-free sample processing.
//!
//! Programs preserve graph order, including one-sample feedback edges. Graphs
//! allocate their delay storage during construction and reset without clearing it.
mod dsp;
mod expression;
use serde::Deserialize;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
/// Maximum graph nodes, including instructions compiled from expressions.
pub const MAX_NODES: usize = 256;
/// Maximum input edges in one program.
pub const MAX_INPUTS: usize = 1024;
const MAX_CHANNELS: usize = 16;
static NEXT_PROGRAM_ID: AtomicU64 = AtomicU64::new(1);

/// Process-local identity of one compiled program, retained by its copies.
///
/// Identical programs compiled separately have distinct identities. Identities
/// are never reused, so a retired asset cannot alias a later program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProgramId(NonZeroU64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
enum Op {
    #[default]
    Constant,
    Boolean,
    Gate,
    Time,
    Input,
    Thru,
    Add,
    Mul,
    Sub,
    Div,
    Mod,
    Pow,
    Log,
    Exp,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Abs,
    Round,
    Floor,
    Ceil,
    Sign,
    Min,
    Max,
    Argmin,
    Argmax,
    Greater,
    Lower,
    Xor,
    And,
    Or,
    Not,
    Bool,
    Ifelse,
    Clamp,
    Range,
    Remap,
    Midinote,
    Src,
    Output,
    Exit,
    Adsr,
    Clock,
    Clockdiv,
    Distort,
    Noise,
    Lcgnoise,
    Pink,
    Brown,
    Dust,
    Impulse,
    Saw,
    Zaw,
    Sine,
    Tri,
    Pulse,
    Slide,
    Lag,
    Slew,
    Filter,
    Bpf,
    Fold,
    Seq,
    Delay,
    Hold,
    Pick,
    Clip,
    Trig,
    Qf,
    Raw,
    Bytebeat,
    Positive,
    Negative,
    LogicalNot,
    BitNot,
    Sqrt,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UnsignedShr,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    LogicalAnd,
    LogicalOr,
    Select,
    Atan2,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Node {
    value: f64,
    start: u16,
    len: u16,
    auxiliary: u16,
    op: Op,
}
/// A validated graph that crosses the audio boundary without shared ownership.
#[derive(Clone, Copy, Debug)]
pub struct Program {
    nodes: [Node; MAX_NODES],
    inputs: [u16; MAX_INPUTS],
    node_count: u16,
    graph_count: u16,
    input_count: u16,
    id: ProgramId,
}
impl PartialEq for Program {
    fn eq(&self, other: &Self) -> bool {
        self.nodes == other.nodes
            && self.inputs == other.inputs
            && self.node_count == other.node_count
            && self.graph_count == other.graph_count
            && self.input_count == other.input_count
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonProgram {
    nodes: Vec<JsonNode>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonNode {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    value: serde_json::Value,
    #[serde(default)]
    inputs: Vec<usize>,
}
impl Program {
    /// Validates a serialized graph, including all feedback references and limits.
    pub fn from_json(source: &str) -> Result<Self, String> {
        if source.len() > 256 * 1024 {
            return Err("KabelSalat graph exceeds 256 KiB".into());
        }
        let graph: JsonProgram = serde_json::from_str(source)
            .map_err(|error| format!("invalid KabelSalat graph: {error}"))?;
        if graph.nodes.is_empty() || graph.nodes.len() > MAX_NODES {
            return Err(format!(
                "KabelSalat graph must contain 1..={MAX_NODES} nodes"
            ));
        }
        let mut program = Self {
            nodes: [Node::default(); MAX_NODES],
            inputs: [0; MAX_INPUTS],
            node_count: graph.nodes.len() as u16,
            graph_count: graph.nodes.len() as u16,
            input_count: 0,
            id: ProgramId(NonZeroU64::MIN),
        };
        let mut delay_count = 0;
        let mut noise_count = 0;
        for (index, node) in graph.nodes.iter().enumerate() {
            let op = match node.kind.as_str() {
                "n" => Op::Constant,
                "gate" => Op::Gate,
                "time" => Op::Time,
                "audioin" => Op::Input,
                "thru" | "scope" => Op::Thru,
                "add" | "mix" => Op::Add,
                "mul" => Op::Mul,
                "sub" => Op::Sub,
                "div" => Op::Div,
                "mod" => Op::Mod,
                "pow" => Op::Pow,
                "log" => Op::Log,
                "exp" => Op::Exp,
                "sin" => Op::Sin,
                "cos" => Op::Cos,
                "tan" => Op::Tan,
                "asin" => Op::Asin,
                "acos" => Op::Acos,
                "atan" => Op::Atan,
                "abs" => Op::Abs,
                "round" => Op::Round,
                "floor" => Op::Floor,
                "ceil" => Op::Ceil,
                "sign" => Op::Sign,
                "min" => Op::Min,
                "max" => Op::Max,
                "argmin" => Op::Argmin,
                "argmax" => Op::Argmax,
                "greater" => Op::Greater,
                "lower" => Op::Lower,
                "xor" => Op::Xor,
                "and" => Op::And,
                "or" => Op::Or,
                "not" => Op::Not,
                "bool" => Op::Bool,
                "ifelse" => Op::Ifelse,
                "clamp" => Op::Clamp,
                "range" => Op::Range,
                "remap" => Op::Remap,
                "midinote" => Op::Midinote,
                "src" => Op::Src,
                "output" => Op::Output,
                "exit" => Op::Exit,
                "adsr" => Op::Adsr,
                "clock" => Op::Clock,
                "clockdiv" => Op::Clockdiv,
                "distort" => Op::Distort,
                "noise" => Op::Noise,
                "lcgnoise" => Op::Lcgnoise,
                "pink" => Op::Pink,
                "brown" => Op::Brown,
                "dust" => Op::Dust,
                "impulse" => Op::Impulse,
                "saw" => Op::Saw,
                "zaw" => Op::Zaw,
                "sine" => Op::Sine,
                "tri" => Op::Tri,
                "pulse" => Op::Pulse,
                "slide" => Op::Slide,
                "lag" => Op::Lag,
                "slew" => Op::Slew,
                "filter" => Op::Filter,
                "bpf" => Op::Bpf,
                "fold" => Op::Fold,
                "seq" => Op::Seq,
                "delay" => Op::Delay,
                "hold" => Op::Hold,
                "pick" => Op::Pick,
                "clip" => Op::Clip,
                "trig" => Op::Trig,
                "qf" => Op::Qf,
                "raw" | "floatbeat" => Op::Raw,
                "bytebeat" => Op::Bytebeat,
                "midifreq" | "midigate" | "midivel" | "midicc" | "cc" => {
                    return Err(format!(
                        "KabelSalat node '{}' requires live control input, which is unavailable in K()",
                        node.kind
                    ));
                }
                _ => return Err(format!("unsupported KabelSalat node '{}'", node.kind)),
            };
            if op == Op::Delay && node.inputs.len() > 2 {
                return Err("KabelSalat persistent delay IDs are unavailable in K(); use delay(input, seconds)".into());
            }
            let start = usize::from(program.input_count);
            if start + node.inputs.len() > MAX_INPUTS {
                return Err(format!("KabelSalat graph exceeds {MAX_INPUTS} input edges"));
            }
            for (offset, &input) in node.inputs.iter().enumerate() {
                if input >= graph.nodes.len() {
                    return Err(format!("KabelSalat node {index} has invalid input {input}"));
                }
                program.inputs[start + offset] = input as u16;
            }
            let value = if op == Op::Constant && node.value.is_string() {
                let mut metadata_only = false;
                for consumer in &graph.nodes {
                    for (inlet, &source) in consumer.inputs.iter().enumerate() {
                        if source == index {
                            if consumer.kind != "scope" || inlet != 1 {
                                return Err(format!(
                                    "KabelSalat string constant {index} is only supported as a scope ID"
                                ));
                            }
                            metadata_only = true;
                        }
                    }
                }
                if !metadata_only {
                    return Err(format!(
                        "KabelSalat constant {index} must be a finite number"
                    ));
                }
                0.0
            } else if op == Op::Constant {
                node.value
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| format!("KabelSalat constant {index} must be a finite number"))?
            } else {
                0.0
            };
            let auxiliary = match op {
                Op::Delay => {
                    let id = delay_count;
                    delay_count += 1;
                    id
                }
                Op::Lcgnoise => {
                    let id = noise_count;
                    noise_count += 1;
                    id
                }
                _ => 0,
            };
            program.nodes[index] = Node {
                op,
                value,
                start: start as u16,
                len: node.inputs.len() as u16,
                auxiliary,
            };
            program.input_count += node.inputs.len() as u16;
        }
        for (index, node) in graph.nodes.iter().enumerate() {
            if matches!(program.nodes[index].op, Op::Raw | Op::Bytebeat) {
                let source = node
                    .value
                    .as_str()
                    .ok_or_else(|| format!("KabelSalat expression {index} must be a string"))?;
                program.compile_expression(index, source)?;
            }
        }
        for (index, node) in program.nodes[..usize::from(program.graph_count)]
            .iter()
            .enumerate()
        {
            let inlet = match node.op {
                Op::Output => 1,
                Op::Src => 0,
                _ => continue,
            };
            if inlet < usize::from(node.len) {
                let source =
                    program.nodes[usize::from(program.inputs[usize::from(node.start) + inlet])];
                if source.op == Op::Constant && channel(source.value).is_none() {
                    return Err(format!(
                        "KabelSalat node {index} requires an output channel from 0 to 15"
                    ));
                }
            }
        }
        let id = NEXT_PROGRAM_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| "KabelSalat program identity space is exhausted".to_owned())?;
        program.id = ProgramId(NonZeroU64::new(id).expect("program identities start at one"));
        Ok(program)
    }

    /// Returns the identity shared by all copies of this compiled program.
    pub fn id(&self) -> ProgramId {
        self.id
    }
    fn compile_expression(&mut self, index: usize, source: &str) -> Result<(), String> {
        use expression::{Binary, Instruction, Unary};
        let instructions = expression::parse(source)?;
        let start = usize::from(self.node_count);
        if start + instructions.len() > MAX_NODES {
            return Err(format!(
                "KabelSalat expressions exceed the {MAX_NODES} node limit"
            ));
        }
        for (offset, instruction) in instructions.iter().enumerate() {
            let mut node = Node::default();
            let mut inputs = [0; 3];
            let input_count = match *instruction {
                Instruction::Constant(value) => {
                    node.value = value;
                    0
                }
                Instruction::Boolean(value) => {
                    node.op = Op::Boolean;
                    node.value = f64::from(value);
                    0
                }
                Instruction::Input => {
                    if self.nodes[index].len == 0 {
                        node.value = f64::NAN;
                        0
                    } else {
                        node.op = Op::Thru;
                        inputs[0] = self.inputs[usize::from(self.nodes[index].start)];
                        1
                    }
                }
                Instruction::Time => {
                    node.op = Op::Time;
                    0
                }
                Instruction::Unary(op, a) => {
                    inputs[0] = a + start as u16;
                    node.op = match op {
                        Unary::Plus => Op::Positive,
                        Unary::Neg => Op::Negative,
                        Unary::Not => Op::LogicalNot,
                        Unary::BitNot => Op::BitNot,
                        Unary::Abs => Op::Abs,
                        Unary::Floor => Op::Floor,
                        Unary::Ceil => Op::Ceil,
                        Unary::Round => Op::Round,
                        Unary::Sign => Op::Sign,
                        Unary::Sin => Op::Sin,
                        Unary::Cos => Op::Cos,
                        Unary::Tan => Op::Tan,
                        Unary::Asin => Op::Asin,
                        Unary::Acos => Op::Acos,
                        Unary::Atan => Op::Atan,
                        Unary::Log => Op::Log,
                        Unary::Exp => Op::Exp,
                        Unary::Sqrt => Op::Sqrt,
                    };
                    1
                }
                Instruction::Binary(op, a, b) => {
                    inputs[0] = a + start as u16;
                    inputs[1] = b + start as u16;
                    node.op = match op {
                        Binary::Add => Op::Add,
                        Binary::Sub => Op::Sub,
                        Binary::Mul => Op::Mul,
                        Binary::Div => Op::Div,
                        Binary::Mod => Op::Mod,
                        Binary::Pow => Op::Pow,
                        Binary::BitAnd => Op::BitAnd,
                        Binary::BitOr => Op::BitOr,
                        Binary::BitXor => Op::BitXor,
                        Binary::Shl => Op::Shl,
                        Binary::Shr => Op::Shr,
                        Binary::UnsignedShr => Op::UnsignedShr,
                        Binary::Less => Op::Lower,
                        Binary::LessEqual => Op::LessEqual,
                        Binary::Greater => Op::Greater,
                        Binary::GreaterEqual => Op::GreaterEqual,
                        Binary::Equal => Op::Equal,
                        Binary::NotEqual => Op::NotEqual,
                        Binary::StrictEqual => Op::StrictEqual,
                        Binary::StrictNotEqual => Op::StrictNotEqual,
                        Binary::And => Op::LogicalAnd,
                        Binary::Or => Op::LogicalOr,
                        Binary::Min => Op::Min,
                        Binary::Max => Op::Max,
                        Binary::Atan2 => Op::Atan2,
                    };
                    2
                }
                Instruction::Select(a, b, c) => {
                    node.op = Op::Select;
                    inputs = [a + start as u16, b + start as u16, c + start as u16];
                    3
                }
            };
            let edge_start = usize::from(self.input_count);
            if edge_start + input_count > MAX_INPUTS {
                return Err(format!(
                    "KabelSalat expressions exceed {MAX_INPUTS} input edges"
                ));
            }
            self.inputs[edge_start..edge_start + input_count]
                .copy_from_slice(&inputs[..input_count]);
            node.start = self.input_count;
            node.len = input_count as u16;
            self.input_count += input_count as u16;
            self.nodes[start + offset] = node;
        }
        self.node_count += instructions.len() as u16;
        self.nodes[index].auxiliary = start as u16;
        self.nodes[index].value = f64::from(self.node_count);
        Ok(())
    }
    /// Returns the number of independent delay buffers required by the graph.
    pub fn delay_count(&self) -> usize {
        self.nodes[..usize::from(self.node_count)]
            .iter()
            .filter(|node| node.op == Op::Delay)
            .count()
    }
}
/// Stateful processor whose sample loop neither allocates nor takes locks.
#[derive(Debug)]
pub struct Graph {
    program: Program,
    values: [f64; MAX_NODES],
    booleans: [bool; MAX_NODES],
    states: [dsp::State; MAX_NODES],
    delays: Vec<dsp::Delay>,
    sources: [f64; MAX_CHANNELS],
    sample_rate: f64,
}
impl Graph {
    /// Allocates delay memory for this program at the given sample rate.
    pub fn new(program: &Program, sample_rate: f64) -> Result<Self, String> {
        if !sample_rate.is_finite() || !(1.0..=384_000.0).contains(&sample_rate) {
            return Err("KabelSalat sample rate must be between 1 and 384000 Hz".into());
        }
        let count = program.delay_count();
        let length = (sample_rate * 10.0).floor() as usize;
        let bytes = count
            .checked_mul(length)
            .and_then(|n| n.checked_mul(4))
            .ok_or("KabelSalat delay memory exceeds address space")?;
        if bytes > 256 * 1024 * 1024 {
            return Err("KabelSalat graph delay memory exceeds 256 MiB".into());
        }
        let mut graph = Self {
            program: *program,
            values: [0.0; MAX_NODES],
            booleans: [false; MAX_NODES],
            states: [dsp::State::default(); MAX_NODES],
            delays: Vec::with_capacity(count),
            sources: [0.0; MAX_CHANNELS],
            sample_rate,
        };
        for _ in 0..count {
            graph.delays.push(dsp::Delay::new(length)?);
        }
        graph.reset();
        Ok(graph)
    }
    /// Returns the immutable program associated with this processor.
    pub fn program(&self) -> &Program {
        &self.program
    }
    /// Returns allocated delay storage and delay bookkeeping, in bytes.
    pub fn heap_bytes(&self) -> usize {
        self.delays.capacity() * std::mem::size_of::<dsp::Delay>()
            + self
                .delays
                .iter()
                .map(dsp::Delay::heap_bytes)
                .sum::<usize>()
    }
    /// Reports whether existing storage can run the supplied program.
    pub fn can_reuse(&self, program: &Program) -> bool {
        program.delay_count() <= self.delays.len()
    }
    /// Replaces the program and restarts it without allocating or freeing storage.
    pub fn restart(&mut self, program: &Program) -> bool {
        if !self.can_reuse(program) {
            return false;
        }
        self.program = *program;
        self.reset();
        true
    }
    /// Restarts the graph without allocating or clearing delay buffers.
    pub fn reset(&mut self) {
        for (value, node) in self.values.iter_mut().zip(self.program.nodes.iter()) {
            *value = if node.op == Op::Constant {
                node.value
            } else {
                0.0
            };
        }
        self.booleans.fill(false);
        self.sources.fill(0.0);
        for (index, state) in self.states.iter_mut().enumerate() {
            *state = dsp::State::for_node(self.program.nodes[index], index);
        }
        for delay in &mut self.delays {
            delay.reset();
        }
    }
    /// Processes one stereo frame. Audio input follows KabelSalat's left channel.
    pub fn process(&mut self, input: [f64; 2], gate: f64, time: f64) -> [f64; 2] {
        let mut output = [0.0; MAX_CHANNELS];
        for index in 0..usize::from(self.program.graph_count) {
            let node = self.program.nodes[index];
            if matches!(node.op, Op::Raw | Op::Bytebeat) {
                for instruction in usize::from(node.auxiliary)..node.value as usize {
                    self.process_node(instruction, input, gate, time, &mut output);
                }
            }
            self.process_node(index, input, gate, time, &mut output);
        }
        [finite(output[0]), finite(output[1])]
    }
    fn process_node(
        &mut self,
        index: usize,
        input: [f64; 2],
        gate: f64,
        time: f64,
        output: &mut [f64; MAX_CHANNELS],
    ) {
        let node = self.program.nodes[index];
        let inputs =
            &self.program.inputs[usize::from(node.start)..usize::from(node.start + node.len)];
        let get = |i: usize, default: f64| -> f64 {
            inputs
                .get(i)
                .map_or(default, |&index| self.values[usize::from(index)])
        };
        let get_boolean = |i: usize| -> bool {
            inputs
                .get(i)
                .is_some_and(|&index| self.booleans[usize::from(index)])
        };
        let mut boolean = false;
        let a = get(0, 0.0);
        let b = get(1, 0.0);
        let result = match node.op {
            Op::Constant => node.value,
            Op::Gate => gate,
            Op::Time => time,
            Op::Input => input[0],
            Op::Boolean => {
                boolean = true;
                node.value
            }
            Op::Thru => {
                boolean = get_boolean(0);
                get(0, f64::NAN)
            }
            Op::Exit => 0.0,
            Op::Raw => {
                let last = node.value as usize - 1;
                boolean = self.booleans[last];
                self.values[last]
            }
            Op::Bytebeat => {
                f64::from(to_u32(self.values[node.value as usize - 1]) & 255) / 127.5 - 1.0
            }
            Op::Positive => a,
            Op::Negative => -a,
            Op::Sqrt => a.sqrt(),
            Op::LogicalNot => {
                boolean = true;
                f64::from(!truthy(a))
            }
            Op::BitNot => f64::from(!(to_u32(a) as i32)),
            Op::BitAnd => f64::from((to_u32(a) & to_u32(b)) as i32),
            Op::BitOr => f64::from((to_u32(a) | to_u32(b)) as i32),
            Op::BitXor => f64::from((to_u32(a) ^ to_u32(b)) as i32),
            Op::Shl => f64::from(to_u32(a).wrapping_shl(to_u32(b) & 31) as i32),
            Op::Shr => f64::from((to_u32(a) as i32).wrapping_shr(to_u32(b) & 31)),
            Op::UnsignedShr => f64::from(to_u32(a).wrapping_shr(to_u32(b) & 31)),
            Op::LessEqual => {
                boolean = true;
                f64::from(a <= b)
            }
            Op::GreaterEqual => {
                boolean = true;
                f64::from(a >= b)
            }
            Op::Equal => {
                boolean = true;
                f64::from(a == b)
            }
            Op::NotEqual => {
                boolean = true;
                f64::from(a != b)
            }
            Op::StrictEqual => {
                boolean = true;
                f64::from(a == b && get_boolean(0) == get_boolean(1))
            }
            Op::StrictNotEqual => {
                boolean = true;
                f64::from(a != b || get_boolean(0) != get_boolean(1))
            }
            Op::LogicalAnd | Op::LogicalOr => {
                let chosen = usize::from(if node.op == Op::LogicalAnd {
                    truthy(a)
                } else {
                    !truthy(a)
                });
                boolean = get_boolean(chosen);
                get(chosen, 0.0)
            }
            Op::Select => {
                let chosen = if truthy(a) { 1 } else { 2 };
                boolean = get_boolean(chosen);
                get(chosen, 0.0)
            }
            Op::Atan2 => a.atan2(b),
            Op::Add | Op::Mul | Op::Sub | Op::Div | Op::Min | Op::Max => {
                boolean = inputs.len() == 1 && get_boolean(0);
                let mut result = a;
                for &i in inputs.iter().skip(1) {
                    let v = self.values[usize::from(i)];
                    result = match node.op {
                        Op::Add => result + v,
                        Op::Mul => result * v,
                        Op::Sub => result - v,
                        Op::Div => result / v,
                        Op::Min => js_min(result, v),
                        _ => js_max(result, v),
                    };
                }
                result
            }
            Op::Mod => a % b,
            Op::Pow => js_pow(a, get(1, 1.0)),
            Op::Log => a.ln(),
            Op::Exp => a.exp(),
            Op::Sin => a.sin(),
            Op::Cos => a.cos(),
            Op::Tan => a.tan(),
            Op::Asin => a.asin(),
            Op::Acos => a.acos(),
            Op::Atan => a.atan(),
            Op::Abs => a.abs(),
            Op::Round => js_round(a),
            Op::Floor => a.floor(),
            Op::Ceil => a.ceil(),
            Op::Sign => {
                if a == 0.0 {
                    a
                } else {
                    a.signum()
                }
            }
            Op::Argmin | Op::Argmax => {
                let mut chosen = 0;
                for i in 1..inputs.len() {
                    let previous = get(chosen, 0.0);
                    if !(if node.op == Op::Argmin {
                        previous < get(i, 0.0)
                    } else {
                        previous > get(i, 0.0)
                    }) {
                        chosen = i;
                    }
                }
                chosen as f64
            }
            Op::Greater => {
                boolean = true;
                f64::from(a > b)
            }
            Op::Lower => {
                boolean = true;
                f64::from(a < b)
            }
            Op::Xor => f64::from(a != b),
            Op::And => f64::from(truthy(a) && truthy(b)),
            Op::Or => f64::from(truthy(a) || truthy(b)),
            Op::Not => f64::from(!get_boolean(0) && a == 0.0),
            Op::Bool => f64::from(get_boolean(0) || a != 0.0),
            Op::Ifelse => {
                let chosen = if !get_boolean(0) && a == 1.0 { 1 } else { 2 };
                boolean = get_boolean(chosen);
                get(chosen, 0.0)
            }
            Op::Clamp => {
                let lo = get(1, -1.0);
                let hi = get(2, 1.0);
                js_min(js_max(a, js_min(lo, hi)), js_max(lo, hi))
            }
            Op::Clip => js_min(js_max(a, get(1, -1.0)), get(2, 1.0)),
            Op::Range => {
                js_pow((get(0, f64::NAN) + 1.0) * 0.5, get(3, 1.0))
                    * (get(2, f64::NAN) - get(1, f64::NAN))
                    + get(1, f64::NAN)
            }
            Op::Remap => dsp::remap(a, get(1, -1.0), get(2, 1.0), get(3, -1.0), get(4, 1.0)),
            Op::Midinote => 2.0_f64.powf((get(0, f64::NAN) - 69.0) / 12.0) * 440.0,
            Op::Src => channel(a).map_or(0.0, |c| self.sources[c]),
            Op::Output => {
                if let Some(c) = channel(b) {
                    output[c] += get(0, f64::NAN);
                    self.sources[c] = output[c];
                }
                0.0
            }
            Op::Delay => self.delays[usize::from(node.auxiliary)].process(a, b, self.sample_rate),
            _ => {
                let result = self.states[index].process(
                    node.op,
                    inputs.len(),
                    &get,
                    &get_boolean,
                    self.sample_rate,
                    time,
                );
                boolean = self.states[index].boolean;
                result
            }
        };
        self.values[index] = result;
        self.booleans[index] = boolean;
    }
}
fn to_u32(value: f64) -> u32 {
    if value.is_finite() {
        value.trunc().rem_euclid(4294967296.0) as u32
    } else {
        0
    }
}
fn truthy(value: f64) -> bool {
    value != 0.0 && !value.is_nan()
}
fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}
fn channel(value: f64) -> Option<usize> {
    (value.is_finite() && value >= 0.0 && value < MAX_CHANNELS as f64 && value.fract() == 0.0)
        .then_some(value as usize)
}

fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() || b.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else {
        a.min(b)
    }
}
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_positive() || b.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else {
        a.max(b)
    }
}
fn js_round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    if (-0.5..0.0).contains(&value) {
        return -0.0;
    }
    let floor = value.floor();
    if value - floor < 0.5 {
        floor
    } else {
        value.ceil()
    }
}
fn js_pow(base: f64, exponent: f64) -> f64 {
    if base.abs() == 1.0 && exponent.is_infinite() {
        f64::NAN
    } else {
        base.powf(exponent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_with_unit_base_and_infinite_curve_is_nan_until_output() {
        let program = Program::from_json(
            r#"{"nodes":[
                {"type":"n","value":1}, {"type":"n","value":0},
                {"type":"div","inputs":[0,1]},
                {"type":"range","inputs":[0,1,0,2]},
                {"type":"output","inputs":[3]}
            ]}"#,
        )
        .unwrap();
        let mut graph = Graph::new(&program, 48_000.0).unwrap();
        assert_eq!(graph.process([0.0; 2], 1.0, 0.0), [0.0; 2]);
        assert!(graph.values[3].is_nan());
    }
}
