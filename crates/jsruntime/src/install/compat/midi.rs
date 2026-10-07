use super::*;
use std::{cell::RefCell, rc::Rc, sync::Arc};

#[derive(Clone)]
struct InputOpener {
    bus: Arc<rustel_core::midi_in::InputBus>,
    handles: Rc<RefCell<MidiInputHandles>>,
    transaction: Rc<RefCell<Option<ScoreEffects>>>,
    policy: Rc<Cell<EffectPolicy>>,
    refusal: Rc<RefCell<Option<String>>>,
}

impl InputOpener {
    fn new(runtime: &JsRuntime) -> Self {
        Self {
            bus: Arc::clone(&runtime.midi_in_bus),
            handles: Rc::clone(&runtime.midi_in_handles),
            transaction: Rc::clone(&runtime.effect_transaction),
            policy: Rc::clone(&runtime.effect_policy),
            refusal: Rc::clone(&runtime.effect_policy_refusal),
        }
    }

    fn open(
        &self,
        ctx: &Ctx<'_>,
        selector: String,
    ) -> rquickjs::Result<Arc<rustel_core::midi_in::InputPort>> {
        let mut opened = None;
        stage_effect(
            ctx,
            &self.policy,
            EffectPolicy::MIDI_INPUT,
            &self.transaction,
            &self.refusal,
            MIDI_INPUT_SCOPE_POLICY,
            |ctx, effects| {
                if selector.len() > rustel_core::midi_in::MAX_INPUT_SELECTOR_BYTES {
                    return Err(rquickjs::Exception::throw_range(
                        ctx,
                        &format!(
                            "a MIDI input selector may contain at most {} UTF-8 bytes",
                            rustel_core::midi_in::MAX_INPUT_SELECTOR_BYTES
                        ),
                    ));
                }
                if let Some(binding) = effects
                    .midi_inputs
                    .bindings
                    .iter()
                    .find(|binding| binding.selector == selector)
                {
                    opened = Some(binding.handle);
                    return Ok(());
                }
                if effects.midi_inputs.bindings.len() >= rustel_core::midi_in::MAX_INPUT_PORTS {
                    return Err(rquickjs::Exception::throw_range(
                        ctx,
                        "at most 8 MIDI inputs may be named by one score",
                    ));
                }

                let mut handles = self.handles.borrow_mut();
                let (handle, provisional) = if let Some(handle) =
                    handles.active.get(&selector).copied()
                {
                    (handle, false)
                } else {
                    const MAX_SAFE_HANDLE: u64 = (1_u64 << 53) - 1;
                    let handle = handles.next_handle;
                    if handle > MAX_SAFE_HANDLE {
                        return Err(rquickjs::Exception::throw_range(
                            ctx,
                            "MIDI input handle space was exhausted",
                        ));
                    }
                    handles.next_handle = handles.next_handle.checked_add(1).ok_or_else(|| {
                        rquickjs::Exception::throw_range(
                            ctx,
                            "MIDI input handle space was exhausted",
                        )
                    })?;
                    let port = self.bus.find_retained(&selector).unwrap_or_else(|| {
                        Arc::new(rustel_core::midi_in::InputPort::new(selector.clone()))
                    });
                    handles.ports.insert(handle, port);
                    handles.provisional.insert(handle);
                    (handle, true)
                };
                effects.midi_inputs.bindings.push(MidiInputBinding {
                    selector: selector.clone(),
                    handle,
                });
                if provisional {
                    effects.midi_inputs.provisional_handles.push(handle);
                    effects.midi_inputs.handles = Rc::downgrade(&self.handles);
                }
                opened = Some(handle);
                Ok(())
            },
        )?;
        let handle = opened.expect("MIDI input effect staged no handle");
        self.handles
            .borrow()
            .ports
            .get(&handle)
            .cloned()
            .ok_or_else(|| {
                rquickjs::Error::new_from_js_message(
                    "MIDI input handle",
                    "MIDI input port",
                    "staged port is unavailable",
                )
            })
    }
}

fn selector<'js>(
    ctx: &Ctx<'js>,
    input: Option<&rquickjs::Value<'js>>,
    function: &str,
) -> rquickjs::Result<String> {
    let input = input.filter(|value| !value.is_undefined());
    let Some(input) = input else {
        return Ok("0".into());
    };
    if !(input.is_string() || input.is_number()) {
        return Err(rquickjs::Exception::throw_message(
            ctx,
            &format!(
                "{function}() needs a device name in SINGLE quotes - {function}('Bass Station') - or an index, {function}(0). Double quotes are parsed as mini-notation before {function}() sees them, so the name never arrives. `rustel devices` lists what is plugged in."
            ),
        ));
    }
    let stringify: Function = ctx.globals().get("String")?;
    stringify.call((input.clone(),))
}

fn optional_number<'js>(
    ctx: &Ctx<'js>,
    value: Option<&rquickjs::Value<'js>>,
) -> rquickjs::Result<f64> {
    let Some(value) = value.filter(|value| !value.is_null() && !value.is_undefined()) else {
        return Ok(0.0);
    };
    let number: Function = ctx.globals().get("Number")?;
    number.call((value.clone(),))
}

fn resolved<'js, T: rquickjs::IntoJs<'js>>(
    ctx: &Ctx<'js>,
    value: T,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
    let (promise, resolve, _) = rquickjs::Promise::new(ctx)?;
    resolve.call::<_, ()>((value,))?;
    Ok(promise)
}

fn hr_input<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<rquickjs::Promise<'js>>,
{
    f
}

fn hr_cc<F>(f: F) -> F
where
    F: for<'js> Fn(
        Ctx<'js>,
        rquickjs::function::Opt<rquickjs::Value<'js>>,
        rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> NativeResult<'js>,
{
    f
}

fn hr_length<F>(f: F) -> F
where
    F: for<'js> Fn(Ctx<'js>, rquickjs::function::Opt<rquickjs::Value<'js>>) -> NativeResult<'js>,
{
    f
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let midin_opener = InputOpener::new(runtime);
    let midin = Function::new(
        ctx.clone(),
        hr_input(move |ctx, input| {
            let port = midin_opener.open(&ctx, selector(&ctx, input.0.as_ref(), "midin")?)?;
            let read = Function::new(
                ctx.clone(),
                hr_cc(move |ctx, cc, channel| {
                    let cc = optional_number(&ctx, cc.0.as_ref())?;
                    let channel = optional_number(&ctx, channel.0.as_ref())?;
                    let port = Arc::clone(&port);
                    // Volatile: the controller's latest value, read at
                    // query time, so no result cache may keep it.
                    let pattern = rustel_core::pure(Value::F64(1.0))
                        .fmap(move |_| {
                            let index =
                                |value: f64| if value.is_finite() { value as i32 } else { -1 };
                            Value::F64(port.read_cc(index(cc), index(channel)))
                        })
                        .mark_volatile();
                    derive_wrapper(ctx, pattern, &[])
                }),
            )?;
            read.set_length(2)?;
            resolved(&ctx, read)
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("midin", midin)
        .map_err(|error| error.to_string())?;

    let midikeys_opener = InputOpener::new(runtime);
    let midikeys = Function::new(
        ctx.clone(),
        hr_input(move |ctx, input| {
            let port = midikeys_opener.open(&ctx, selector(&ctx, input.0.as_ref(), "midikeys")?)?;
            let keys = Function::new(
                ctx.clone(),
                hr_length(move |ctx, note_length| {
                    let value = note_length
                        .0
                        .unwrap_or_else(|| rquickjs::Value::new_float(ctx.clone(), 0.5));
                    let (lengths, sidecar) = reify_bridged(&ctx, &value)?;
                    derive_wrapper(
                        ctx,
                        rustel_core::midi_keys(lengths, Arc::clone(&port)),
                        &[sidecar],
                    )
                }),
            )?;
            resolved(&ctx, keys)
        }),
    )
    .map_err(|error| error.to_string())?;
    globals
        .set("midikeys", midikeys)
        .map_err(|error| error.to_string())
}
