//! Native graph objects and bounded graph construction.
//!
//! Graph operations follow KabelSalat's graph.js semantics (AGPL-3.0-or-later).
//! JavaScript is used only to execute the user's score and callbacks.

use super::*;
use rquickjs::{
    Coerced, Exception, FromJs, IntoJs, Result,
    function::{Rest, This},
};
use std::collections::{HashMap, HashSet};

mod schemas;

const MAX_TRANSIENT_NODES: usize = 8192;
const MAX_OPERATIONS: usize = 32768;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 256;

#[derive(JsLifetime)]
pub(super) struct State<'js> {
    prototype: Object<'js>,
    registrations: RefCell<HashMap<usize, Registration<'js>>>,
    pi: RefCell<Option<Value<'js>>>,
    schemas: Object<'js>,
    pub(super) outputs: RefCell<Vec<Value<'js>>>,
    nodes: Cell<usize>,
    operations: Cell<usize>,
    depth: Cell<usize>,
    module_id: Cell<usize>,
}

#[derive(Clone, JsLifetime)]
struct Registration<'js> {
    function: Function<'js>,
    module: bool,
    name: String,
}

impl<'js> State<'js> {
    pub(super) fn new(ctx: &Ctx<'js>) -> Result<Self> {
        let prototype = Object::new(ctx.clone())?;
        let schemas = Object::new(ctx.clone())?;
        schemas.set_prototype(None)?;
        schemas::install(ctx, &schemas)?;
        Ok(Self {
            prototype,
            schemas,
            registrations: RefCell::new(HashMap::new()),
            pi: RefCell::new(None),
            outputs: RefCell::new(Vec::new()),
            nodes: Cell::new(0),
            operations: Cell::new(0),
            depth: Cell::new(0),
            module_id: Cell::new(0),
        })
    }
}

fn state<'js>(ctx: &Ctx<'js>) -> Result<Rc<State<'js>>> {
    ctx.userdata::<GraphCompiler>()
        .and_then(|compiler| compiler.active.borrow().last().cloned())
        .ok_or_else(|| {
            Exception::throw_type(ctx, "KabelSalat graph operation outside graph construction")
        })
}

struct Depth<'js>(Rc<State<'js>>);
impl Drop for Depth<'_> {
    fn drop(&mut self) {
        self.0.depth.set(self.0.depth.get() - 1);
    }
}
fn enter<'js>(ctx: &Ctx<'js>) -> Result<Depth<'js>> {
    let state = state(ctx)?;
    charge(ctx, &state)?;
    if state.depth.get() >= MAX_DEPTH {
        return Err(Exception::throw_range(
            ctx,
            "KabelSalat graph construction exceeds operation or depth limits",
        ));
    }
    state.depth.set(state.depth.get() + 1);
    Ok(Depth(state))
}

fn charge(ctx: &Ctx<'_>, state: &State<'_>) -> Result<()> {
    let operations = state.operations.get() + 1;
    if operations > MAX_OPERATIONS {
        return Err(Exception::throw_range(
            ctx,
            "KabelSalat graph construction exceeds operation or depth limits",
        ));
    }
    state.operations.set(operations);
    Ok(())
}

pub(super) fn number<'js>(ctx: &Ctx<'js>, value: f64) -> Value<'js> {
    Value::new_number(ctx.clone(), value)
}
pub(super) fn pi<'js>(ctx: &Ctx<'js>) -> Result<Value<'js>> {
    let state = state(ctx)?;
    if let Some(value) = state.pi.borrow().clone() {
        return Ok(value);
    }
    let value = constant(ctx, number(ctx, std::f64::consts::PI))?;
    *state.pi.borrow_mut() = Some(value.clone());
    Ok(value)
}

fn undefined<'js>(ctx: &Ctx<'js>) -> Value<'js> {
    Value::new_undefined(ctx.clone())
}
fn argument<'js>(ctx: &Ctx<'js>, args: &[Value<'js>], index: usize) -> Value<'js> {
    args.get(index).cloned().unwrap_or_else(|| undefined(ctx))
}
fn check_inputs(ctx: &Ctx<'_>, count: usize) -> Result<()> {
    if count > MAX_INPUTS {
        Err(Exception::throw_range(
            ctx,
            "KabelSalat graph input list exceeds 1024 values",
        ))
    } else {
        Ok(())
    }
}
fn array<'js>(ctx: &Ctx<'js>, values: Vec<Value<'js>>) -> Result<Array<'js>> {
    check_inputs(ctx, values.len())?;
    let array = Array::new(ctx.clone())?;
    for (index, value) in values.into_iter().enumerate() {
        array.set(index, value)?;
    }
    Ok(array)
}
fn array_values<'js>(ctx: &Ctx<'js>, value: &Array<'js>) -> Result<Vec<Value<'js>>> {
    let len = js_array_len(value)?;
    check_inputs(ctx, len)?;
    (0..len).map(|index| value.get(index)).collect()
}

pub(super) fn kind<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<String> {
    value
        .as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat graph contains an invalid node"))?
        .get("type")
}
pub(super) fn inputs<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<Vec<Value<'js>>> {
    let object = value
        .as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat graph contains an invalid node"))?;
    let ins: Array = object.get("ins")?;
    array_values(ctx, &ins)
}
fn set_inputs<'js>(ctx: &Ctx<'js>, value: &Value<'js>, ins: Vec<Value<'js>>) -> Result<()> {
    value
        .as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat graph contains an invalid node"))?
        .set("ins", array(ctx, ins)?)
}
fn is_poly(value: &Value<'_>) -> Result<bool> {
    let Some(object) = value.as_object() else {
        return Ok(false);
    };
    let kind: Value = object.get("type")?;
    Ok(kind
        .as_string()
        .map(|kind| kind.to_string())
        .transpose()?
        .is_some_and(|kind| kind == "poly"))
}

pub(super) fn node<'js>(
    ctx: &Ctx<'js>,
    type_name: &str,
    value: Option<Value<'js>>,
) -> Result<Value<'js>> {
    let state = state(ctx)?;
    if state.nodes.get() >= MAX_TRANSIENT_NODES || type_name.len() > 128 {
        return Err(Exception::throw_range(
            ctx,
            "KabelSalat graph construction exceeds node or type limits",
        ));
    }
    state.nodes.set(state.nodes.get() + 1);
    let object = Object::new(ctx.clone())?;
    object.set_prototype(Some(&state.prototype))?;
    object.set("type", type_name)?;
    if let Some(value) = value.filter(|value| !value.is_undefined()) {
        object.set("value", value)?;
    }
    object.set("ins", Array::new(ctx.clone())?)?;
    Ok(object.into_value())
}

pub(super) fn constant<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Value<'js>> {
    let _depth = enter(ctx)?;
    if let Some(array) = value.as_array() {
        let values = array_values(ctx, array)?
            .into_iter()
            .map(|value| constant(ctx, value))
            .collect::<Result<Vec<_>>>()?;
        return get_node(ctx, "poly", values);
    }
    if value.is_object() || value.is_null() {
        return Ok(value);
    }
    node(ctx, "n", Some(value))
}

pub(super) fn poly<'js>(ctx: &Ctx<'js>, values: Vec<Value<'js>>) -> Result<Value<'js>> {
    let node = node(ctx, "poly", None)?;
    set_inputs(ctx, &node, values)?;
    Ok(node)
}

pub(super) fn invoke<'js>(
    ctx: &Ctx<'js>,
    function: &Value<'js>,
    args: Vec<Value<'js>>,
) -> Result<Value<'js>> {
    let _depth = enter(ctx)?;
    check_inputs(ctx, args.len())?;
    let function = function
        .as_function()
        .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat expected a callback function"))?;
    let mut arguments = Args::new(ctx.clone(), args.len());
    arguments.push_args(args)?;
    function.call_arg(arguments)
}

fn parse_input<'js>(
    ctx: &Ctx<'js>,
    input: Value<'js>,
    receiver: Option<&Value<'js>>,
) -> Result<Value<'js>> {
    if input.is_function() {
        let receiver = receiver.ok_or_else(|| {
            Exception::throw_type(ctx, "KabelSalat callback input needs a feedback node")
        })?;
        return invoke(ctx, &input, vec![receiver.clone()]);
    }
    if input.is_object() || input.is_null() {
        return Ok(input);
    }
    if input.is_number() || input.is_string() {
        return constant(ctx, input);
    }
    // Upstream's fallback produces an invalid edge, rejected at the native boundary.
    Err(Exception::throw_type(
        ctx,
        "KabelSalat node inputs must be numbers, strings, nodes or callbacks",
    ))
}

pub(super) fn get_node<'js>(
    ctx: &Ctx<'js>,
    type_name: &str,
    args: Vec<Value<'js>>,
) -> Result<Value<'js>> {
    let _depth = enter(ctx)?;
    check_inputs(ctx, args.len())?;
    let mut expanded = 1;
    let mut arguments = Vec::with_capacity(args.len());
    for mut arg in args {
        if let Some(array) = arg.as_array() {
            let mut values = array_values(ctx, array)?;
            arg = if values.len() == 1 {
                values.remove(0)
            } else {
                poly(ctx, values)?
            };
        }
        if arg.is_function() {
            let peek = node(ctx, "peek", None)?;
            let returned = invoke(ctx, &arg, vec![peek])?;
            if is_poly(&returned)? {
                expanded = expanded.max(inputs(ctx, &returned)?.len());
            }
        }
        if is_poly(&arg)? {
            expanded = expanded.max(inputs(ctx, &arg)?.len());
        }
        arguments.push(arg);
    }
    if expanded == 1 {
        let next = node(ctx, type_name, None)?;
        let ins = arguments
            .into_iter()
            .map(|arg| parse_input(ctx, arg, Some(&next)))
            .collect::<Result<_>>()?;
        set_inputs(ctx, &next, ins)?;
        return Ok(next);
    }
    if type_name == "exit" {
        let mut ins = Vec::new();
        for arg in arguments {
            if is_poly(&arg)? {
                for input in inputs(ctx, &arg)? {
                    ins.push(parse_input(ctx, input, None)?);
                }
            } else {
                ins.push(arg);
            }
            check_inputs(ctx, ins.len())?;
        }
        let next = node(ctx, type_name, None)?;
        set_inputs(ctx, &next, ins)?;
        return Ok(next);
    }
    let mut channels = Vec::with_capacity(expanded);
    for index in 0..expanded {
        let next = node(ctx, type_name, None)?;
        let mut ins = Vec::with_capacity(arguments.len());
        for arg in &arguments {
            let input = if is_poly(arg)? {
                let values = inputs(ctx, arg)?;
                let value = if values.is_empty() {
                    undefined(ctx)
                } else {
                    values[index % values.len()].clone()
                };
                let value = parse_input(ctx, value, Some(&next))?;
                inherit(&value, arg)?;
                value
            } else {
                let value = parse_input(ctx, arg.clone(), Some(&next))?;
                if is_poly(&value)? {
                    inputs(ctx, &value)?
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| undefined(ctx))
                } else {
                    value
                }
            };
            ins.push(input);
        }
        set_inputs(ctx, &next, ins)?;
        channels.push(next);
    }
    poly(ctx, channels)
}

fn inherit<'js>(value: &Value<'js>, parent: &Value<'js>) -> Result<()> {
    if let (Some(value), Some(parent)) = (value.as_object(), parent.as_object()) {
        for key in ["inputOf", "outputOf"] {
            let property: Value = parent.get(key)?;
            if !property.is_undefined() {
                value.set(key, property)?;
            }
        }
    }
    Ok(())
}

fn native_function<'js>(ctx: &Ctx<'js>, name: &str, method: bool) -> Result<Function<'js>> {
    let owned_name = name.to_owned();
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, This(this): This<Value<'js>>, Rest(mut args): Rest<Value<'js>>| {
            if method && owned_name == "feedback" {
                let _depth = enter(&ctx)?;
                let add: Function = this
                    .as_object()
                    .ok_or_else(|| Exception::throw_type(&ctx, "KabelSalat expected a graph node"))?
                    .get("add")?;
                let mut arguments = Args::new(ctx.clone(), 1);
                arguments.this(this)?;
                arguments.push_arg(argument(&ctx, &args, 0))?;
                return add.call_arg::<Value>(arguments);
            }
            if method {
                args.insert(0, this);
            }
            call(&ctx, &owned_name, args)
        },
    )
}
fn install_method<'js>(ctx: &Ctx<'js>, name: &str) -> Result<()> {
    let state = state(ctx)?;
    state
        .prototype
        .set(name, native_function(ctx, name, true)?)?;
    let bypass = format!("_{name}");
    state.prototype.set(
        bypass,
        Function::new(ctx.clone(), |This(this): This<Value<'js>>| this)?,
    )?;
    Ok(())
}

fn registered_function<'js>(ctx: &Ctx<'js>, id: usize, method: bool) -> Result<Function<'js>> {
    let owner = Rc::downgrade(&state(ctx)?);
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, This(this): This<Value<'js>>, Rest(mut args): Rest<Value<'js>>| {
            let _depth = enter(&ctx)?;
            let state = owner
                .upgrade()
                .filter(|owner| state(&ctx).is_ok_and(|active| Rc::ptr_eq(owner, &active)))
                .ok_or_else(|| {
                    Exception::throw_type(&ctx, "KabelSalat registration belongs to another graph")
                })?;
            if method {
                args.insert(0, this);
            }
            check_inputs(&ctx, args.len())?;
            let registration = state
                .registrations
                .borrow()
                .get(&id)
                .cloned()
                .expect("registered callback is retained during construction");
            invoke_registration(&ctx, registration, args)
        },
    )
}

pub(super) fn module_inputs<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    args: Vec<Value<'js>>,
) -> Result<(Vec<Value<'js>>, usize)> {
    let state = state(ctx)?;
    let id = state.module_id.get();
    state.module_id.set(id + 1);
    let mut converted = Vec::with_capacity(args.len());
    for (index, arg) in args.into_iter().enumerate() {
        let value = parse_input(ctx, arg, None)?;
        converted.push(call(
            ctx,
            "asModuleInput",
            vec![
                value,
                name.into_js(ctx)?,
                number(ctx, id as f64),
                number(ctx, index as f64),
            ],
        )?);
    }
    Ok((converted, id))
}

pub(super) fn module_output<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    id: usize,
    result: Value<'js>,
) -> Result<Value<'js>> {
    call(
        ctx,
        "asModuleOutput",
        vec![result, name.into_js(ctx)?, number(ctx, id as f64)],
    )
}

fn invoke_registration<'js>(
    ctx: &Ctx<'js>,
    registration: Registration<'js>,
    args: Vec<Value<'js>>,
) -> Result<Value<'js>> {
    if registration.module {
        let (converted, id) = module_inputs(ctx, &registration.name, args)?;
        let result = invoke(ctx, registration.function.as_value(), converted)?;
        return module_output(ctx, &registration.name, id, result);
    }
    invoke(ctx, registration.function.as_value(), args)
}

const CORE_NAMES: &[&str] = &[
    "Node",
    "node",
    "getNode",
    "getInletName",
    "registerNode",
    "register",
    "module",
    "n",
    "B",
    "_",
    "poly",
    "exit",
    "out",
    "feedback",
    "compile",
    "evaluate",
    "exportModule",
    "registerUgen",
];
const METHODS: &[&str] = &[
    "withIns",
    "inherit",
    "clone",
    "apply",
    "apply2",
    "map",
    "channel",
    "select",
    "over",
    "debug",
    "dfs",
    "flatten",
    "toObject",
    "stringify",
    "asModuleInput",
    "asModuleOutput",
];

pub(super) fn scope<'js>(
    ctx: &Ctx<'js>,
    pattern_inputs: &[f64],
    frequency: f64,
) -> Result<Vec<(String, Value<'js>)>> {
    let current_state = state(ctx)?;
    let mut scope = Vec::new();
    for name in CORE_NAMES.iter().chain(library::NAMES.iter()) {
        if scope.iter().any(|(existing, _)| existing == name) {
            continue;
        }
        let function = native_function(ctx, name, false)?;
        if *name == "Node" {
            function.set_constructor(true);
            function.set("prototype", current_state.prototype.clone())?;
            function.set(
                "parseInput",
                Function::new(
                    ctx.clone(),
                    |ctx: Ctx<'js>, Rest(args): Rest<Value<'js>>| {
                        let _depth = enter(&ctx)?;
                        parse_input(&ctx, argument(&ctx, &args, 0), args.get(1))
                    },
                )?,
            )?;
        }
        scope.push(((*name).to_owned(), function.into_value()));
        if !matches!(
            *name,
            "Node"
                | "node"
                | "getNode"
                | "getInletName"
                | "registerNode"
                | "register"
                | "module"
                | "compile"
                | "evaluate"
                | "exportModule"
                | "registerUgen"
        ) {
            install_method(ctx, name)?;
        }
    }
    for name in METHODS {
        current_state
            .prototype
            .set(*name, native_function(ctx, name, true)?)?;
    }
    // These are graph constants, not constructors.
    scope.push(("PI".into(), pi(ctx)?));
    scope.push(("polyType".into(), "poly".into_js(ctx)?));
    scope.push(("outputType".into(), "exit".into_js(ctx)?));
    scope.push(("sFreq".into(), number(ctx, frequency)));
    scope.push(("sGate".into(), node(ctx, "gate", None)?));
    scope.push((
        "pat".into(),
        array(
            ctx,
            pattern_inputs
                .iter()
                .map(|value| number(ctx, *value))
                .collect(),
        )?
        .into_value(),
    ));
    for name in ["mouseX", "mouseY"] {
        scope.push((name.into(), get_node(ctx, "cc", vec![name.into_js(ctx)?])?));
    }
    let registry = Object::new(ctx.clone())?;
    registry.set(
        "get",
        Function::new(
            ctx.clone(),
            |ctx: Ctx<'js>, name: String| -> Result<Value<'js>> {
                let _depth = enter(&ctx)?;
                state(&ctx)?.schemas.get(name)
            },
        )?,
    )?;
    registry.set(
        "set",
        Function::new(
            ctx.clone(),
            |ctx: Ctx<'js>,
             This(this): This<Value<'js>>,
             name: String,
             value: Value<'js>|
             -> Result<Value<'js>> {
                let _depth = enter(&ctx)?;
                if name.len() > 128 {
                    return Err(Exception::throw_range(
                        &ctx,
                        "KabelSalat schema name exceeds 128 bytes",
                    ));
                }
                state(&ctx)?.schemas.set(name, value)?;
                Ok(this)
            },
        )?,
    )?;
    scope.push(("nodeRegistry".into(), registry.into_value()));
    Ok(scope)
}

pub(super) fn call<'js>(ctx: &Ctx<'js>, name: &str, args: Vec<Value<'js>>) -> Result<Value<'js>> {
    let _depth = enter(ctx)?;
    check_inputs(ctx, args.len())?;
    let state = state(ctx)?;
    let first = argument(ctx, &args, 0);
    match name {
        "n" | "B" | "_" => constant(ctx, first),
        "poly" => get_node(ctx, "poly", args),
        "Node" | "node" => node(ctx, &String::from_js(ctx, first)?, args.get(1).cloned()),
        "getNode" => get_node(
            ctx,
            &String::from_js(ctx, first)?,
            args.into_iter().skip(1).collect(),
        ),
        "exit" => get_node(ctx, "exit", args),
        "feedback" => get_node(ctx, "add", vec![first]),
        "out" => {
            let channels = if args.get(1).is_none_or(Value::is_undefined) {
                array(ctx, vec![number(ctx, 0.0), number(ctx, 1.0)])?.into_value()
            } else {
                args[1].clone()
            };
            let output = get_node(ctx, "output", vec![first.clone(), channels])?;
            let mut outputs = state.outputs.borrow_mut();
            check_inputs(ctx, outputs.len() + 1)?;
            outputs.push(output);
            Ok(first)
        }
        "register" | "module" | "registerNode" => {
            let operation = name;
            let name = String::from_js(ctx, first)?;
            if name.len() > 128 || state.registrations.borrow().len() >= MAX_INPUTS {
                return Err(Exception::throw_range(
                    ctx,
                    "KabelSalat registration limit exceeded",
                ));
            }
            let function = if name.is_empty() {
                return Err(Exception::throw_type(
                    ctx,
                    "KabelSalat registration needs a name",
                ));
            } else if matches!(name.as_str(), "__proto__") {
                return Err(Exception::throw_type(
                    ctx,
                    "KabelSalat registration name is reserved",
                ));
            } else if matches!(args.get(1), Some(value) if value.is_function()) {
                Function::from_js(ctx, args[1].clone())?
            } else {
                let node_name = name.clone();
                Function::new(
                    ctx.clone(),
                    move |ctx: Ctx<'js>, Rest(args): Rest<Value<'js>>| {
                        get_node(&ctx, &node_name, args)
                    },
                )?
            };
            let is_module = operation == "module";
            let id = state.registrations.borrow().len();
            state.registrations.borrow_mut().insert(
                id,
                Registration {
                    function: function.clone(),
                    module: is_module,
                    name: name.clone(),
                },
            );
            state
                .prototype
                .set(name.as_str(), registered_function(ctx, id, true)?)?;
            state.prototype.set(
                format!("_{name}"),
                Function::new(ctx.clone(), |This(this): This<Value<'js>>| this)?,
            )?;
            if let Some(schema) = args.get(if operation == "registerNode" { 1 } else { 2 }) {
                state.schemas.set(name.as_str(), schema.clone())?;
            }
            if is_module {
                registered_function(ctx, id, false).map(Function::into_value)
            } else {
                Ok(function.into_value())
            }
        }
        "getInletName" => {
            let schema: Value = state
                .schemas
                .get(Coerced::<String>::from_js(ctx, first)?.0)?;
            let index = Coerced::<f64>::from_js(ctx, argument(ctx, &args, 1))?.0;
            if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                return "".into_js(ctx);
            }
            let index = index as usize;
            if let Some(schema) = schema.as_object() {
                let ins: Value = schema.get("ins")?;
                if let Some(ins) = ins.as_array() {
                    let input: Value = ins.get(index)?;
                    if let Some(input) = input.as_object() {
                        return input.get("name");
                    }
                }
            }
            "".into_js(ctx)
        }
        "withIns" => {
            set_inputs(ctx, &first, args.into_iter().skip(1).collect())?;
            Ok(first)
        }
        "inherit" => {
            inherit(&first, &argument(ctx, &args, 1))?;
            Ok(first)
        }
        "clone" => {
            let object = first
                .as_object()
                .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat clone expected node"))?;
            let next = node(ctx, &kind(ctx, &first)?, Some(object.get("value")?))?;
            set_inputs(ctx, &next, inputs(ctx, &first)?)?;
            Ok(next)
        }
        "apply" | "apply2" => {
            let values = if name == "apply2" {
                vec![first.clone(), first]
            } else {
                vec![first]
            };
            invoke(ctx, &argument(ctx, &args, 1), values)
        }
        "map" => {
            let callback = argument(ctx, &args, 1);
            if !is_poly(&first)? {
                return invoke(ctx, &callback, vec![first]);
            }
            let ins = inputs(ctx, &first)?;
            let values = array(ctx, ins.clone())?;
            let mut mapped = Vec::with_capacity(ins.len());
            for (index, input) in ins.into_iter().enumerate() {
                mapped.push(invoke(
                    ctx,
                    &callback,
                    vec![
                        input,
                        number(ctx, index as f64),
                        values.clone().into_value(),
                    ],
                )?);
            }
            get_node(ctx, "poly", mapped)
        }
        "channel" => {
            if !is_poly(&first)? {
                return Ok(first);
            }
            let ins = inputs(ctx, &first)?;
            let index = Coerced::<f64>::from_js(ctx, argument(ctx, &args, 1))?.0 % ins.len() as f64;
            if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                return Ok(undefined(ctx));
            }
            Ok(ins
                .get(index as usize)
                .cloned()
                .unwrap_or_else(|| undefined(ctx)))
        }
        "select" => select(ctx, first, String::from_js(ctx, argument(ctx, &args, 1))?),
        "over" => {
            let other = invoke(ctx, &argument(ctx, &args, 1), vec![first.clone()])?;
            get_node(ctx, "add", vec![first, other])
        }
        "debug" => {
            if args.get(1).is_some_and(Value::is_function) {
                let _ = invoke(ctx, &args[1], vec![first.clone()])?;
            }
            Ok(first)
        }
        "dfs" => {
            let visited = if let Some(value) = args.get(2).filter(|value| !value.is_undefined()) {
                Array::from_js(ctx, value.clone())?
            } else {
                Array::new(ctx.clone())?
            };
            dfs(ctx, first, argument(ctx, &args, 1), &visited)
        }
        "asModuleInput" | "asModuleOutput" => {
            let object = first
                .as_object()
                .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat module expected node"))?;
            let metadata = array(ctx, args.into_iter().skip(1).collect())?;
            if name == "asModuleInput" {
                let existing: Value = object.get("inputOf")?;
                let values = if existing.is_undefined() {
                    Array::new(ctx.clone())?
                } else {
                    Array::from_js(ctx, existing)?
                };
                let len = js_array_len(&values)?;
                check_inputs(ctx, len + 1)?;
                values.set(len, metadata)?;
                object.set("inputOf", values)?;
            } else {
                object.set("outputOf", metadata)?;
            }
            Ok(first)
        }
        "flatten" => flatten(ctx, first),
        "toObject" | "stringify" => {
            let json = ctx
                .json_stringify_replacer_space(
                    first,
                    undefined(ctx),
                    if name == "stringify" { 2 } else { 0 },
                )?
                .ok_or_else(|| Exception::throw_type(ctx, "KabelSalat node cannot be serialized"))?
                .to_string()?;
            if name == "toObject" {
                ctx.json_parse(json)
            } else {
                json.replace('"', "'").into_js(ctx)
            }
        }
        "compile" => serialize(ctx, first)?.into_js(ctx),
        "evaluate" | "exportModule" => Err(Exception::throw_type(
            ctx,
            "KabelSalat nested evaluation and source export are unavailable; use native graph functions",
        )),
        _ => library::dispatch(ctx, name, args),
    }
}

fn select<'js>(ctx: &Ctx<'js>, root: Value<'js>, target: String) -> Result<Value<'js>> {
    let state = state(ctx)?;
    let mut stack = inputs(ctx, &root)?;
    stack.reverse();
    let mut visited = HashSet::new();
    while let Some(current) = stack.pop() {
        charge(ctx, &state)?;
        if visited.contains(&current) {
            continue;
        }
        if visited.len() >= MAX_TRANSIENT_NODES {
            return Err(Exception::throw_range(
                ctx,
                "KabelSalat traversal limit exceeded",
            ));
        }
        if kind(ctx, &current)? == target {
            return Ok(current);
        }
        visited.insert(current.clone());
        let mut children = inputs(ctx, &current)?;
        children.reverse();
        stack.extend(children);
        if stack.len() > MAX_TRANSIENT_NODES {
            return Err(Exception::throw_range(
                ctx,
                "KabelSalat traversal limit exceeded",
            ));
        }
    }
    Ok(undefined(ctx))
}

fn dfs<'js>(
    ctx: &Ctx<'js>,
    current: Value<'js>,
    callback: Value<'js>,
    visited: &Array<'js>,
) -> Result<Value<'js>> {
    let _depth = enter(ctx)?;
    let current = invoke(ctx, &callback, vec![current, visited.clone().into_value()])?;
    let len = js_array_len(visited)?;
    check_inputs(ctx, len + 1)?;
    visited.set(len, current.clone())?;
    let mut mapped = Vec::new();
    for input in inputs(ctx, &current)? {
        if array_values(ctx, visited)?.contains(&input) {
            mapped.push(input);
        } else {
            mapped.push(dfs(ctx, input, callback.clone(), visited)?);
        }
    }
    set_inputs(ctx, &current, mapped)?;
    Ok(current)
}

struct Snapshot<'js> {
    value: Value<'js>,
    kind: String,
    scalar: Option<serde_json::Value>,
    inputs: Vec<Value<'js>>,
}
fn snapshot<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Snapshot<'js>> {
    let kind = kind(ctx, &value)?;
    if kind.len() > 128 {
        return Err(Exception::throw_type(
            ctx,
            "KabelSalat graph contains an invalid node type",
        ));
    }
    let inputs = inputs(ctx, &value)?;
    let mut scalar: Value = value
        .as_object()
        .expect("validated node object")
        .get("value")?;
    if matches!(kind.as_str(), "raw" | "bytebeat" | "floatbeat") {
        scalar = Coerced::<String>::from_js(ctx, scalar)?.0.into_js(ctx)?;
    }
    let scalar = if scalar.is_undefined() {
        None
    } else if let Some(number) = scalar.as_number().filter(|number| number.is_finite()) {
        Some(
            if number.fract() == 0.0
                && (-9_007_199_254_740_991.0..=9_007_199_254_740_991.0).contains(&number)
            {
                serde_json::Value::from(number as i64)
            } else {
                serde_json::Value::from(number)
            },
        )
    } else if let Some(string) = scalar.as_string() {
        let string = string.to_string()?;
        if string.len() > 8192 {
            return Err(Exception::throw_range(
                ctx,
                "KabelSalat expression exceeds 8192 bytes",
            ));
        }
        Some(serde_json::Value::String(string))
    } else {
        return Err(Exception::throw_type(
            ctx,
            "KabelSalat node values must be finite numbers or bounded strings",
        ));
    };
    Ok(Snapshot {
        value,
        kind,
        scalar,
        inputs,
    })
}

fn snapshots<'js>(ctx: &Ctx<'js>, root: Value<'js>, postorder: bool) -> Result<Vec<Snapshot<'js>>> {
    let state = state(ctx)?;
    let mut nodes = vec![snapshot(ctx, root)?];
    let mut edges = nodes[0].inputs.len();
    let mut stack = vec![(0usize, 0usize)];
    let mut sorted = Vec::new();
    while let Some((index, next)) = stack.last_mut() {
        charge(ctx, &state)?;
        if *next == nodes[*index].inputs.len() {
            sorted.push(*index);
            stack.pop();
            continue;
        }
        let child = nodes[*index].inputs[*next].clone();
        *next += 1;
        if nodes.iter().any(|node| node.value == child) {
            continue;
        }
        if nodes.len() >= MAX_NODES {
            return Err(Exception::throw_range(
                ctx,
                "KabelSalat graph exceeds 256 nodes",
            ));
        }
        let child = snapshot(ctx, child)?;
        edges += child.inputs.len();
        if edges > MAX_INPUTS {
            return Err(Exception::throw_range(
                ctx,
                "KabelSalat graph exceeds 1024 inputs",
            ));
        }
        stack.push((nodes.len(), 0));
        nodes.push(child);
    }
    if !postorder {
        return Ok(nodes);
    }
    let mut nodes: Vec<Option<Snapshot>> = nodes.into_iter().map(Some).collect();
    Ok(sorted
        .into_iter()
        .map(|index| nodes[index].take().expect("unique node traversal"))
        .collect())
}

pub(super) fn serialize<'js>(ctx: &Ctx<'js>, root: Value<'js>) -> Result<String> {
    let nodes = snapshots(ctx, root, true)?;
    let mut json = String::from("{\"nodes\":[");
    for (index, node) in nodes.iter().enumerate() {
        if index != 0 {
            json.push(',');
        }
        json.push_str("{\"type\":");
        json.push_str(&serde_json::to_string(&node.kind).expect("string serialization"));
        if let Some(value) = &node.scalar {
            json.push_str(",\"value\":");
            json.push_str(&value.to_string());
        }
        json.push_str(",\"inputs\":[");
        for (input_index, input) in node.inputs.iter().enumerate() {
            if input_index != 0 {
                json.push(',');
            }
            let register = nodes
                .iter()
                .position(|node| node.value == *input)
                .expect("snapshot visited every edge");
            json.push_str(&register.to_string());
        }
        json.push_str("]}");
    }
    json.push_str("]}");
    Ok(json)
}

fn flatten<'js>(ctx: &Ctx<'js>, root: Value<'js>) -> Result<Value<'js>> {
    let nodes = snapshots(ctx, root, false)?;
    let array = Array::new(ctx.clone())?;
    for (index, node) in nodes.iter().enumerate() {
        let object = Object::new(ctx.clone())?;
        object.set("type", node.kind.as_str())?;
        if let Some(scalar) = &node.scalar {
            object.set("value", ctx.json_parse(scalar.to_string())?)?;
        }
        let ins = node
            .inputs
            .iter()
            .map(|input| {
                nodes
                    .iter()
                    .position(|node| node.value == *input)
                    .expect("snapshot visited every edge")
                    .to_string()
                    .into_js(ctx)
            })
            .collect::<Result<_>>()?;
        object.set("ins", self::array(ctx, ins)?)?;
        array.set(index, object)?;
    }
    Ok(array.into_value())
}
