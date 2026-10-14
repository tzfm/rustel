//! KabelSalat library graph builders, adapted from packages/lib/src/lib.js.

use rquickjs::{Array, Coerced, Ctx, Exception, FromJs, IntoJs, Result, Value, function::Args};

use super::graph;

pub(super) const NAMES: &[&str] = &[
    "time",
    "raw",
    "bytebeat",
    "floatbeat",
    "adsr",
    "ar",
    "ad",
    "clock",
    "clockdiv",
    "distort",
    "noise",
    "rng",
    "lcgnoise",
    "pink",
    "brown",
    "dust",
    "impulse",
    "saw",
    "zaw",
    "sine",
    "tri",
    "pulse",
    "slide",
    "lag",
    "slew",
    "filter",
    "fold",
    "seq",
    "delay",
    "hold",
    "midifreq",
    "midigate",
    "midivel",
    "midicc",
    "cc",
    "audioin",
    "log",
    "exp",
    "pow",
    "sin",
    "cos",
    "tan",
    "acos",
    "asin",
    "atan",
    "mul",
    "add",
    "div",
    "sub",
    "mod",
    "abs",
    "round",
    "clamp",
    "floor",
    "ceil",
    "sign",
    "min",
    "max",
    "argmin",
    "argmax",
    "greater",
    "lower",
    "gt",
    "lt",
    "xor",
    "and",
    "or",
    "not",
    "bool",
    "ifelse",
    "range",
    "remap",
    "thru",
    "rangex",
    "midinote",
    "src",
    "output",
    "fork",
    "perc",
    "hpf",
    "lpf",
    "bpf",
    "lfnoise",
    "bipolar",
    "unipolar",
    "pan",
    "pick",
    "clip",
    "trig",
    "qf",
    "qlpf",
    "qhpf",
    "qbpf",
    "qnf",
    "qapf",
    "split",
    "mix",
    "feedback",
    "B",
    "_",
    "scope",
    "registerUgen",
];

fn argument<'js>(ctx: &Ctx<'js>, args: &[Value<'js>], index: usize) -> Value<'js> {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()))
}

fn default_argument<'js>(
    ctx: &Ctx<'js>,
    args: &[Value<'js>],
    index: usize,
    default: f64,
) -> Value<'js> {
    let value = argument(ctx, args, index);
    if value.is_undefined() {
        graph::number(ctx, default)
    } else {
        value
    }
}

fn array<'js>(ctx: &Ctx<'js>, values: Vec<Value<'js>>) -> Result<Value<'js>> {
    if values.len() > super::MAX_INPUTS {
        return Err(Exception::throw_range(
            ctx,
            "KabelSalat inputs exceed 1024 values",
        ));
    }
    let array = Array::new(ctx.clone())?;
    for (index, value) in values.into_iter().enumerate() {
        array.set(index, value)?;
    }
    Ok(array.into_value())
}

fn with_inputs<'js>(
    ctx: &Ctx<'js>,
    node: Value<'js>,
    inputs: Vec<Value<'js>>,
) -> Result<Value<'js>> {
    node.as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat expected a graph node"))?
        .set("ins", array(ctx, inputs)?)?;
    Ok(node)
}

fn method<'js>(
    ctx: &Ctx<'js>,
    input: Value<'js>,
    name: &str,
    args: Vec<Value<'js>>,
) -> Result<Value<'js>> {
    let function: Value = input
        .as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat expected a graph node"))?
        .get(name)?;
    let function = function
        .as_function()
        .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat expected a graph method"))?;
    let mut arguments = Args::new(ctx.clone(), args.len());
    arguments.this(input)?;
    arguments.push_args(args)?;
    function.call_arg(arguments)
}

pub(super) fn dispatch<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    args: Vec<Value<'js>>,
) -> Result<Value<'js>> {
    if args.len() > super::MAX_INPUTS {
        return Err(Exception::throw_range(
            ctx,
            "KabelSalat inputs exceed 1024 values",
        ));
    }
    let module = matches!(
        name,
        "ar" | "ad"
            | "perc"
            | "lpf"
            | "hpf"
            | "lfnoise"
            | "bipolar"
            | "unipolar"
            | "rangex"
            | "pan"
    );
    let (args, module_id) = if module {
        let (args, id) = graph::module_inputs(ctx, name, args)?;
        (args, Some(id))
    } else {
        (args, None)
    };
    let value = |index| argument(ctx, &args, index);
    let number = |number| graph::number(ctx, number);
    let result = match name {
        "B" | "_" => graph::constant(ctx, value(0)),
        "rng" => graph::get_node(ctx, "lcgnoise", args),
        "gt" => graph::get_node(ctx, "greater", args),
        "lt" => graph::get_node(ctx, "lower", args),
        "time" => {
            let code = value(0);
            graph::node(ctx, "time", (!code.is_undefined()).then_some(code))
        }
        "raw" | "bytebeat" | "floatbeat" => {
            // Template arrays stringify with commas, as in the upstream compiler.
            let code = Coerced::<String>::from_js(ctx, value(1))?.0;
            if code.len() > 8192 {
                return Err(Exception::throw_range(
                    ctx,
                    "KabelSalat expression exceeds 8192 bytes",
                ));
            }
            // The upstream floatbeat builder emits a bytebeat node.
            let kind = if name == "floatbeat" {
                "bytebeat"
            } else {
                name
            };
            let node = graph::node(ctx, kind, Some(code.into_js(ctx)?))?;
            with_inputs(ctx, node, vec![graph::constant(ctx, value(0))?])
        }
        "ar" => method(
            ctx,
            graph::constant(ctx, default_argument(ctx, &args, 0, 0.0))?,
            "adsr",
            vec![
                default_argument(ctx, &args, 1, 0.02),
                number(0.0),
                number(1.0),
                default_argument(ctx, &args, 2, 0.1),
            ],
        ),
        "ad" => {
            let decay = default_argument(ctx, &args, 2, 0.1);
            method(
                ctx,
                graph::constant(ctx, default_argument(ctx, &args, 0, 0.0))?,
                "adsr",
                vec![
                    default_argument(ctx, &args, 1, 0.02),
                    decay.clone(),
                    number(0.0),
                    decay,
                ],
            )
        }
        "perc" => method(
            ctx,
            value(0),
            "adsr",
            vec![number(0.0), number(0.0), number(1.0), value(1)],
        ),
        "lpf" => graph::get_node(ctx, "filter", args),
        "hpf" => {
            let input = graph::constant(ctx, value(0))?;
            let low = method(
                ctx,
                input.clone(),
                "lpf",
                vec![value(1), default_argument(ctx, &args, 2, 0.0)],
            )?;
            method(ctx, input, "sub", vec![low])
        }
        "lfnoise" => {
            let noise = graph::get_node(ctx, "noise", Vec::new())?;
            let trigger = graph::get_node(ctx, "impulse", vec![value(0)])?;
            method(ctx, noise, "hold", vec![trigger])
        }
        "bipolar" => {
            let input = graph::constant(ctx, value(0))?;
            let scaled = method(ctx, input, "mul", vec![number(2.0)])?;
            method(ctx, scaled, "sub", vec![number(1.0)])
        }
        "unipolar" => {
            let input = graph::constant(ctx, value(0))?;
            let shifted = method(ctx, input, "add", vec![number(1.0)])?;
            method(ctx, shifted, "div", vec![number(2.0)])
        }
        "rangex" => {
            let minimum = graph::get_node(ctx, "log", vec![value(1)])?;
            let maximum = graph::get_node(ctx, "log", vec![value(2)])?;
            let range = method(ctx, maximum, "sub", vec![minimum.clone()])?;
            let unipolar = method(ctx, value(0), "unipolar", Vec::new())?;
            let scaled = method(ctx, unipolar, "mul", vec![range])?;
            let shifted = method(ctx, scaled, "add", vec![minimum])?;
            graph::get_node(ctx, "exp", vec![shifted])
        }
        "pan" => {
            let position = graph::constant(ctx, value(1))?;
            let shifted = method(ctx, position, "add", vec![number(1.0)])?;
            let angle = method(ctx, shifted, "mul", vec![graph::pi(ctx)?, number(0.25)])?;
            let left = graph::get_node(ctx, "cos", vec![angle.clone()])?;
            let right = graph::get_node(ctx, "sin", vec![angle])?;
            method(ctx, value(0), "mul", vec![array(ctx, vec![left, right])?])
        }
        "qlpf" | "qhpf" | "qbpf" | "qnf" | "qapf" => {
            // qapf selects the notch type in the upstream graph library.
            let kind = match name {
                "qlpf" => 0.0,
                "qhpf" => 1.0,
                "qbpf" => 2.0,
                _ => 3.0,
            };
            method(
                ctx,
                value(0),
                "qf",
                vec![
                    number(kind),
                    value(1),
                    default_argument(ctx, &args, 2, 10.0),
                ],
            )
        }
        "fork" => {
            let count = Coerced::<f64>::from_js(ctx, default_argument(ctx, &args, 1, 1.0))?.0;
            if count > super::MAX_INPUTS as f64 {
                return Err(Exception::throw_range(
                    ctx,
                    "KabelSalat fork exceeds 1024 channels",
                ));
            }
            let count = if count.is_nan() || count <= 0.0 {
                0
            } else {
                count.floor() as usize
            };
            let input = value(0);
            let mut channels = Vec::with_capacity(count);
            for _ in 0..count {
                channels.push(method(ctx, input.clone(), "clone", Vec::new())?);
            }
            graph::get_node(ctx, "poly", channels)
        }
        "split" => {
            let input = value(0);
            let channels = if graph::kind(ctx, &input)? == "poly" {
                graph::inputs(ctx, &input)?
            } else {
                vec![input]
            };
            graph::invoke(ctx, &value(1), vec![array(ctx, channels)?])
        }
        "mix" => mix(ctx, value(0), default_argument(ctx, &args, 1, 1.0)),
        "feedback" => graph::get_node(ctx, "add", args),
        "scope" => {
            let input = value(0);
            let channels = if graph::kind(ctx, &input)? == "poly" {
                graph::inputs(ctx, &input)?.len()
            } else {
                1
            };
            let scope =
                graph::get_node(ctx, "scope", vec![input, value(1), number(channels as f64)])?;
            let outputs = if graph::kind(ctx, &scope)? == "poly" {
                graph::inputs(ctx, &scope)?
            } else {
                vec![scope.clone()]
            };
            for (channel, output) in outputs.into_iter().enumerate() {
                let mut inputs = graph::inputs(ctx, &output)?;
                inputs.push(graph::constant(ctx, number(channel as f64))?);
                with_inputs(ctx, output, inputs)?;
            }
            Ok(scope)
        }
        "registerUgen" => Err(Exception::throw_type(
            ctx,
            "KabelSalat custom unit generators are unavailable; compose native nodes with register or module",
        )),
        _ if NAMES.contains(&name) => graph::get_node(ctx, name, args),
        _ => Err(Exception::throw_type(
            ctx,
            &format!("Unknown KabelSalat library function: {name}"),
        )),
    }?;
    if let Some(id) = module_id {
        graph::module_output(ctx, name, id, result)
    } else {
        Ok(result)
    }
}

fn mix<'js>(ctx: &Ctx<'js>, input: Value<'js>, channels: Value<'js>) -> Result<Value<'js>> {
    if graph::kind(ctx, &input)? != "poly" {
        return Ok(input);
    }
    let inputs = graph::inputs(ctx, &input)?;
    if channels.as_number() == Some(1.0) {
        let inputs = inputs
            .into_iter()
            .map(|inlet| method(ctx, inlet, "inherit", vec![input.clone()]))
            .collect::<Result<Vec<_>>>()?;
        with_inputs(ctx, input, inputs.clone())?;
        return with_inputs(ctx, graph::node(ctx, "mix", None)?, inputs);
    }
    let count = inputs.len();
    let mut panned = Vec::with_capacity(count);
    for (index, inlet) in inputs.into_iter().enumerate() {
        let position = (index as f64 / (count as f64 - 1.0)) * 2.0 - 1.0;
        let angle = ((position + 1.0) * std::f64::consts::PI) / 4.0;
        let gains = array(
            ctx,
            vec![
                graph::number(ctx, angle.cos()),
                graph::number(ctx, angle.sin()),
            ],
        )?;
        let stereo = method(ctx, inlet, "mul", vec![gains])?;
        panned.push(method(ctx, stereo, "inherit", vec![input.clone()])?);
    }
    graph::get_node(ctx, "add", panned)
}

#[cfg(test)]
mod tests {
    use crate::{JsRuntime, with_ctx};

    fn compile(source: &str) -> String {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            super::super::install(&ctx).expect("bindings");
            super::super::compile_graph(&ctx, source, &[], 220.0)
                .unwrap_or_else(|error| panic!("{source}: {error:?}; {:?}", ctx.catch()))
        })
    }

    fn render(source: &str) -> [f64; 2] {
        let program = rustel_kabel::Program::from_json(&compile(source)).expect("native program");
        rustel_kabel::Graph::new(&program, 48_000.0)
            .expect("graph")
            .process([0.0; 2], 1.0, 0.0)
    }

    fn assert_sample(source: &str, expected: [f64; 2]) {
        let actual = render(source);
        for channel in 0..2 {
            assert!(
                (actual[channel] - expected[channel]).abs() < 1e-12,
                "{source}: {actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn macros_render_their_graph_arithmetic() {
        assert_sample("n(.25).bipolar().out()", [-0.5; 2]);
        assert_sample("n(.5).unipolar().out()", [0.75; 2]);
        assert_sample("n(0).rangex(100, 10000).out()", [1000.0; 2]);
        assert_sample("n([2, 3]).split(ch => mul(...ch)).out()", [6.0; 2]);
        assert_sample("n([2, 3, 5]).mix().out()", [10.0; 2]);
        assert_sample("n(2).mix(2).out()", [2.0; 2]);
        assert_sample("B(2).add(_(3)).scope('test').out()", [5.0; 2]);
    }

    #[test]
    fn panning_and_stereo_mix_use_equal_power_gains() {
        assert_sample("n(1).pan(-1).out()", [1.0, 0.0]);
        assert_sample("n(1).pan(0).out()", [std::f64::consts::FRAC_1_SQRT_2; 2]);
        assert_sample("n(1).pan(1).out()", [0.0, 1.0]);
        assert_sample(
            "n([1, 2, 3]).mix(2).out()",
            [
                1.0 + 2.0 * std::f64::consts::FRAC_1_SQRT_2,
                3.0 + 2.0 * std::f64::consts::FRAC_1_SQRT_2,
            ],
        );
    }

    #[test]
    fn fork_clones_processors_and_keeps_shared_inputs() {
        let graph: serde_json::Value =
            serde_json::from_str(&compile("sine(220).fork(3).mix().out()")).unwrap();
        let nodes = graph["nodes"].as_array().unwrap();
        assert_eq!(
            nodes.iter().filter(|node| node["type"] == "sine").count(),
            3
        );
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node["type"] == "n" && node["value"] == 220)
                .count(),
            1
        );
        assert_sample("n(2).fork(3).mix().out()", [6.0; 2]);
    }

    #[test]
    fn envelope_macros_materialize_only_their_defaults() {
        for (source, expected) in [
            ("ar().out()", [0.0, 0.02, 0.0, 1.0, 0.1]),
            ("ad().out()", [0.0, 0.02, 0.1, 0.0, 0.1]),
            ("n(1).perc(.4).out()", [1.0, 0.0, 0.0, 1.0, 0.4]),
        ] {
            let graph: serde_json::Value = serde_json::from_str(&compile(source)).unwrap();
            let nodes = graph["nodes"].as_array().unwrap();
            let envelope = nodes.iter().find(|node| node["type"] == "adsr").unwrap();
            let inputs: Vec<f64> = envelope["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|index| {
                    nodes[index.as_u64().unwrap() as usize]["value"]
                        .as_f64()
                        .unwrap()
                })
                .collect();
            assert_eq!(inputs, expected);
        }
    }

    #[test]
    fn module_arguments_keep_shared_edges_and_metadata() {
        let graph: serde_json::Value = serde_json::from_str(&compile(
            "const envelope = ad(sGate,.02,.3); if(envelope.outputOf[0] !== 'ad') throw Error('module output'); if(envelope.ins[2].inputOf[0][2] !== 2) throw Error('module input'); envelope.out()",
        ))
        .unwrap();
        let envelope = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["type"] == "adsr")
            .unwrap();
        assert_eq!(envelope["inputs"][2], envelope["inputs"][4]);

        let graph: serde_json::Value =
            serde_json::from_str(&compile("n(1).pan(0).out(); n(2).pan(0).out(); PI.out()"))
                .unwrap();
        assert_eq!(
            graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|node| node["value"].as_f64() == Some(std::f64::consts::PI))
                .count(),
            1
        );
    }

    #[test]
    fn macros_resolve_registered_node_methods() {
        assert_sample(
            "register('mul', input => input.add(5)); n(2).bipolar().out()",
            [6.0; 2],
        );
        assert_sample(
            "register('adsr', input => input.mul(.5)); n(1).ad(.1,.2).out()",
            [0.5; 2],
        );
        assert_sample(
            "register('lpf', input => input.mul(.25)); n(1).hpf(.5).out()",
            [0.75; 2],
        );
        assert_sample(
            "register('qf', input => input.mul(.75)); n(1).qlpf(500).out()",
            [0.75; 2],
        );
        assert_sample(
            "register('clone', input => input.add(1)); n(2).fork(2).mix().out()",
            [6.0; 2],
        );
    }

    #[test]
    fn expression_templates_and_filter_aliases_keep_upstream_semantics() {
        assert_sample("n(128).bytebeat`t`.out()", [128.0 / 127.5 - 1.0; 2]);
        assert_sample("n(0).floatbeat`0`.out()", [-1.0; 2]);
        assert_sample("n(.25).raw`$input * 2`.out()", [0.5; 2]);
        for (name, kind) in [
            ("qlpf", 0),
            ("qhpf", 1),
            ("qbpf", 2),
            ("qnf", 3),
            ("qapf", 3),
        ] {
            let graph: serde_json::Value =
                serde_json::from_str(&compile(&format!("n(1).{name}(500).out()"))).unwrap();
            let nodes = graph["nodes"].as_array().unwrap();
            let filter = nodes.iter().find(|node| node["type"] == "qf").unwrap();
            assert_eq!(
                nodes[filter["inputs"][1].as_u64().unwrap() as usize]["value"],
                kind
            );
            assert_eq!(
                nodes[filter["inputs"][3].as_u64().unwrap() as usize]["value"],
                10
            );
        }
    }

    #[test]
    fn unbounded_forks_and_custom_processors_are_rejected() {
        let runtime = JsRuntime::new().expect("runtime");
        with_ctx(&runtime.ctx, |ctx| {
            super::super::install(&ctx).expect("bindings");
            for source in [
                "n(1).fork(Infinity).out()",
                "n(1).fork(1025).out()",
                "registerUgen('custom', 'Custom')",
            ] {
                assert!(super::super::compile_graph(&ctx, source, &[], 220.0).is_err());
                let _ = ctx.catch();
            }
        });
    }
}
