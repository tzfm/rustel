use super::*;

/// Raise a real JavaScript `TypeError`, so the message a caller sees is the
/// pinned one rather than a Rust conversion string.
///
/// `stepJoin`'s construction failure is the case that needs it: it throws
/// `TypeError: x.value.withHap is not a function` out of the constructor, and
/// user code can legitimately `try`/`catch` it.
pub(crate) fn throw_type_error(ctx: &Ctx<'_>, message: &str) -> rquickjs::Error {
    rquickjs::Exception::throw_type(ctx, message)
}

/// The refusal owed to a call that was given nothing.
///
/// If a missing argument were read as `undefined`, it would reify into
/// silence: a half-typed call would remove one track from a running set with
/// no error. A refusal is recoverable: the set keeps playing what it last
/// accepted, and the message names the call. `call` is written as the player
/// types it, `.fast` or `squeeze`.
pub(crate) fn refuse_empty_call(ctx: &Ctx<'_>, call: &str, inputs: usize) -> rquickjs::Error {
    throw_type_error(ctx, &format!("{call}() expects {inputs} inputs but got 0."))
}

pub const TEMPO_SCOPE_POLICY: &str = "setCps/setcps and setCpm/setcpm can change tempo only during a Session-owned score evaluation; use --cps on commands that provide it in raw, prebake, or query-time callback contexts";
pub(crate) const TEMPO_VALUE_POLICY: &str = "native setCps/setcps and setCpm/setcpm require a finite tempo greater than zero; pass a valid value or use --cps on commands that provide it";
pub(crate) const SAMPLES_SCOPE_POLICY: &str = "samples(...) can register sample sources only during a Session-owned score or setup evaluation, not from raw code or query-time callbacks";
pub(crate) const PRELOAD_SCOPE_POLICY: &str = "preload(...) can request samples only during a Session-owned score or setup evaluation, not from raw code or query-time callbacks";
pub const MIDI_INPUT_SCOPE_POLICY: &str = "midin()/midikeys() can name an input only during a Session-owned score evaluation; create the input outside pattern callbacks";

pub(crate) fn throw_effect_policy(
    ctx: &Ctx<'_>,
    refusal: &RefCell<Option<String>>,
    message: &'static str,
) -> rquickjs::Error {
    let mut refusal = refusal.borrow_mut();
    if refusal.is_none() {
        *refusal = Some(message.to_string());
    }
    rquickjs::Exception::throw_range(ctx, message)
}

pub(crate) fn stage_effect(
    ctx: &Ctx<'_>,
    policy: &Cell<EffectPolicy>,
    required: u8,
    transaction: &RefCell<Option<ScoreEffects>>,
    refusal: &RefCell<Option<String>>,
    message: &'static str,
    stage: impl FnOnce(&Ctx<'_>, &mut ScoreEffects) -> rquickjs::Result<()>,
) -> rquickjs::Result<()> {
    if !policy.get().allows(required) {
        return Err(throw_effect_policy(ctx, refusal, message));
    }
    let mut transaction = transaction.borrow_mut();
    let Some(effects) = transaction.as_mut() else {
        return Err(throw_effect_policy(ctx, refusal, message));
    };
    stage(ctx, effects)
}

pub(crate) fn describe_caught_js_value(value: rquickjs::Value<'_>) -> String {
    let ctx = value.ctx().clone();
    if let Some(exception) = value.as_exception() {
        // The message is read as a VALUE, not through `ToString` up front: a
        // double-quoted argument is mini-notation, so `new Error("boom")`
        // can hand the constructor an already-compiled Pattern, and the
        // default object stringification would read it as `[object
        // Object]`, burying the word a player actually threw. An undefined
        // message property is no message at all, the way a missing one is.
        let message = quiet(&ctx, exception.get::<_, rquickjs::Value>("message"))
            .filter(|value| !value.is_undefined())
            .and_then(|value| thrown_text(&value))
            .unwrap_or_default();
        let name: String = exception
            .get::<_, rquickjs::Value>("name")
            .ok()
            .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
            .unwrap_or_else(|| "Error".to_string());
        // The stack's first frame carries where it happened; a message
        // with no line sends the reader hunting through the whole score.
        let place = exception
            .stack()
            .as_deref()
            .and_then(first_stack_position)
            .map(|(line, column)| match column {
                Some(column) => format!(" - line {line}, column {column}"),
                None => format!(" - line {line}"),
            })
            .unwrap_or_default();
        if message.is_empty() {
            return format!("{name}{place}");
        }
        return format!("{name}: {message}{place}");
    }
    describe_thrown_text(&value)
}

/// Render an arbitrary thrown (non-`Error`) value, or an `Error`'s own
/// `message` property, as readable text - never blank, and never the default
/// `[object Object]`.
///
/// A quoted score literal compiles to mini-notation before it ever reaches
/// user code, so `throw "boom"` and `throw {message: "custom"}` throw a
/// Pattern rather than the word a player typed. A plain string or number
/// reads as itself; a Pattern built from exactly one bare word or number
/// reads as that value; an object reads its own `message` property (one
/// level, so a cyclic `message` cannot recurse) and then its own
/// `toString`; anything with no text worth reporting becomes a short,
/// honest description instead.
fn describe_thrown_text(value: &rquickjs::Value<'_>) -> String {
    thrown_text(value).unwrap_or_else(|| format!("a thrown {} value", value.type_name()))
}

/// The text `value` reads as, or `None` when it has none worth reporting:
/// a blank message, a plain object, a structured Pattern.
fn thrown_text(value: &rquickjs::Value<'_>) -> Option<String> {
    if let Some(text) = primitive_or_pattern_text(value) {
        return nonempty(text);
    }
    let ctx = value.ctx();
    if let Some(object) = value.as_object() {
        if let Some(message) = quiet(ctx, object.get::<_, rquickjs::Value>("message"))
            && !message.is_undefined()
            && let Some(text) = primitive_or_pattern_text(&message)
        {
            return nonempty(text);
        }
        // An object may still describe itself: a custom `toString`, or an
        // array's join, reads back real text. The DEFAULT object
        // stringification is exactly the "[object Object]" this reporting
        // exists to avoid, so that one result is refused like a blank.
        if let Some(text) = quiet(ctx, value.get::<rquickjs::convert::Coerced<String>>())
            && text.0 != "[object Object]"
        {
            return nonempty(text.0);
        }
    }
    None
}

/// A string, an ordinary primitive, or a single-word/number mini-notation
/// Pattern, read as its own text. `None` for anything else - an object with
/// its own shape, which the caller decides how to describe.
fn primitive_or_pattern_text(value: &rquickjs::Value<'_>) -> Option<String> {
    if let Some(text) = value.as_string().and_then(|s| s.to_string().ok()) {
        return Some(text);
    }
    if let Some((pattern, _sidecar)) = unwrap_pattern(value)
        && let Some(pure) = pattern.as_pure()
    {
        return Some(pure.show());
    }
    if value.as_object().is_none() {
        // Every remaining primitive (number, bool, null, undefined) coerces
        // through `ToString` predictably, with no user-defined method to go
        // wrong - except a symbol, whose coercion throws instead.
        return quiet(
            value.ctx(),
            value.get::<rquickjs::convert::Coerced<String>>(),
        )
        .map(|coerced| coerced.0);
    }
    None
}

/// Run one JS-touching read whose failure is not itself worth reporting.
///
/// A failed property read or coercion can leave a fresh exception pending
/// in the context: QuickJS keeps it installed until it is fetched. A stale
/// exception would be reported as the next failure in a shared runtime. So
/// the failure is discarded, and any exception it raised is fetched and
/// dropped.
fn quiet<'js, T>(ctx: &rquickjs::Ctx<'js>, result: rquickjs::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(_) => {
            let _ = ctx.catch();
            None
        }
    }
}

/// `Some` for text worth reporting, `None` for the blank that would print a
/// diagnostic with nothing in it.
fn nonempty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

/// The `line[:column]` of a stack's first frame - QuickJS writes frames as
/// `    at fn (source:LINE:COLUMN)` or `    at fn (source:LINE)`.
fn first_stack_position(stack: &str) -> Option<(u32, Option<u32>)> {
    let frame = stack.lines().find(|line| line.contains(':'))?;
    let inside = frame.rsplit_once('(').map_or(frame, |(_, tail)| tail);
    let inside = inside.trim_end_matches([')', ' ']);
    let mut numbers = inside
        .rsplit(':')
        .map_while(|piece| piece.parse::<u32>().ok())
        .collect::<Vec<_>>();
    numbers.reverse();
    match numbers.as_slice() {
        [line] => Some((*line, None)),
        [line, column, ..] => Some((*line, Some(*column))),
        [] => None,
    }
}

/// Rewrite the " - line N[, column C]" suffix [`describe_caught_js_value`]
/// wrote down so it names the SCORE's position: the shim lines above the
/// evaluated text are subtracted, then the printer's own map carries the
/// position back through the reprint to the score's line and column. A
/// position that cannot be mapped loses the suffix - no line is better
/// than a wrong one.
pub(crate) fn remap_reported_position(
    message: String,
    lines_above_score: u32,
    line_map: &rustel_transpiler::LineMap,
) -> String {
    let Some(at) = message.rfind(" - line ") else {
        return message;
    };
    let (head, tail) = message.split_at(at);
    let rest = &tail[" - line ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let Ok(line) = digits.parse::<u32>() else {
        return message;
    };
    let column = rest[digits.len()..]
        .strip_prefix(", column ")
        .map(|piece| {
            piece
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse::<u32>().ok());
    if line <= lines_above_score || line_map.is_empty() {
        return head.to_owned();
    }
    let printed_line = line - lines_above_score - 1;
    let printed_column = column.unwrap_or(1).saturating_sub(1);
    // The line maps exactly; the column has been through a reprint and
    // QuickJS's own idea of where an expression "is", so it is not offered
    // at all - a column pointing past the line's end helps nobody.
    match line_map.original_position(printed_line, printed_column) {
        Some((source_line, _)) => format!("{head} - line {}", source_line + 1),
        None => head.to_owned(),
    }
}

/// Describe an rquickjs error with the text of the pending exception.
///
/// `rquickjs::Error::Exception` stringifies to the constant "Exception
/// generated by QuickJS". The actual message is in the context's pending
/// exception, which this function takes.
pub(crate) fn describe_js_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if !matches!(error, rquickjs::Error::Exception) {
        return error.to_string();
    }
    describe_caught_js_value(ctx.catch())
}

/// Convert a JavaScript value recursively while preserving functions as
/// native `FunctionRef`s and returning the callback cells that own them.
/// `pure([f, { g }])` is observable through pickF, so bridging only a top-level
/// function would leave a graph classified pure whose nested callback cannot
/// be resolved at query time.
pub(crate) fn from_js_bridged_value<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Value, Vec<Sidecar<'js>>, bool)> {
    if let Some((pattern, mut sidecar)) = unwrap_pattern(value) {
        let (id, root) = bridge_js_value(ctx, value.clone())?;
        sidecar.absorb(root).map_err(ownership_set_error_to_js)?;
        return Ok((
            Value::Pattern(Box::new(rustel_core::value::PatternValue::new(id, pattern))),
            vec![sidecar],
            true,
        ));
    }
    if let Some(function) = value.as_function() {
        if let Some(object) = value.as_object()
            && let Some(reference) = combinator_reference(object.clone())
        {
            return Ok((Value::Function(reference), Vec::new(), false));
        }
        let (id, sidecar) = bridge_callable(ctx, function.clone())?;
        return Ok((
            Value::Function(rustel_core::value::FunctionRef::js(id)),
            vec![sidecar],
            false,
        ));
    }
    if value.is_array() || value.as_object().is_some() {
        let (id, sidecar) = bridge_js_value(ctx, value.clone())?;
        return Ok((
            Value::JsValue(rustel_core::value::JsValueRef::new(id, value.is_array())),
            vec![sidecar],
            false,
        ));
    }
    Ok((from_js(value), Vec::new(), false))
}

/// Read a JS-owned value at the semantic boundary that actually needs its
/// contents. Unlike `from_js_bridged_value`, this deliberately enumerates
/// arrays/objects now; nested identities remain rooted and round-trippable.
///
/// The enumeration is budgeted: the whole recursion shares one element cap
/// derived from the QuickJS heap ceiling, because a JS `length` costs the
/// sandbox nothing to inflate. See [`reserve_js_elements`]. A caller that
/// materializes several values it KEEPS together - every hap one callback
/// returns - seeds one budget and uses [`materialize_js_value_within`], or
/// each value would get its own full cap.
pub(crate) fn materialize_js_value<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
) -> rquickjs::Result<(Value, Vec<Sidecar<'js>>)> {
    let remaining = Cell::new(js_element_cap::<Value>(ctx)?);
    materialize_js_value_within(ctx, value, &remaining)
}

/// [`materialize_js_value`] charging a caller-owned element budget, seeded
/// from `js_element_cap::<Value>`, so the host mirrors of several values
/// kept together cannot sum past one heap ceiling. Throws `RangeError` for a
/// tree nested deeper than [`MAX_JS_VALUE_DEPTH`].
pub(crate) fn materialize_js_value_within<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
    remaining: &Cell<usize>,
) -> rquickjs::Result<(Value, Vec<Sidecar<'js>>)> {
    materialize_js_value_within_depth(ctx, value, remaining, 0)
}

fn materialize_js_value_within_depth<'js>(
    ctx: &Ctx<'js>,
    value: &rquickjs::Value<'js>,
    remaining: &Cell<usize>,
    depth: usize,
) -> rquickjs::Result<(Value, Vec<Sidecar<'js>>)> {
    if let Some((pattern, mut sidecar)) = unwrap_pattern(value) {
        let (id, root) = bridge_js_value(ctx, value.clone())?;
        sidecar.absorb(root).map_err(ownership_set_error_to_js)?;
        return Ok((
            Value::Pattern(Box::new(rustel_core::value::PatternValue::new(id, pattern))),
            vec![sidecar],
        ));
    }
    if value.is_function() {
        let (pattern, sidecar) = reify_bridged(ctx, value)?;
        return Ok((pattern.as_pure().unwrap_or(Value::Undefined), vec![sidecar]));
    }
    if depth >= MAX_JS_VALUE_DEPTH && value.is_object() {
        return Err(rquickjs::Exception::throw_range(
            ctx,
            "cannot materialize a JS value nested too deeply",
        ));
    }
    if let Some(array) = value.as_array() {
        let mut values = reserve_js_elements(ctx, js_array_len(array)?, remaining)?;
        let mut sidecars = Vec::new();
        for item in array.iter::<rquickjs::Value>() {
            let item = item?;
            // NO element-level collapse. The transpiler wraps every
            // double-quoted literal as `m('…')`, so `["C", "major"]` reaches a
            // combinator as an array of Patterns -- and that
            // array passes through AS IS (`reify(array)` is `pure(array)`; nothing
            // unwraps the elements). The reference behavior logs "Scale name [object
            // Object] … is incomplete" and plays NOTHING. Collapsing pure
            // elements back to scalars made that same score play here, which
            // is a divergence dressed up as a fix; a score that wants the
            // array form writes it single-quoted, which the transpiler leaves
            // alone on both engines.
            let (item, mut nested) =
                materialize_js_value_within_depth(ctx, &item, remaining, depth + 1)?;
            values.push(item);
            sidecars.append(&mut nested);
        }
        return Ok((Value::List(values), sidecars));
    }
    if let Some(object) = value.as_object() {
        // Each property charges one element, so a child shared by several
        // keys cannot multiply the walk past the budget. Arrays reached
        // through the values charge their lengths.
        let mut entries = Vec::new();
        let mut sidecars = Vec::new();
        for entry in object.props::<String, rquickjs::Value>() {
            let (key, value) = entry?;
            charge_js_elements(ctx, 1, remaining)?;
            let (value, mut nested) =
                materialize_js_value_within_depth(ctx, &value, remaining, depth + 1)?;
            entries.push((key, value));
            sidecars.append(&mut nested);
        }
        return Ok((Value::object(entries), sidecars));
    }
    Ok((from_js(value), Vec::new()))
}
