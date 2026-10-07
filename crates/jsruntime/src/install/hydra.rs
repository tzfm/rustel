//! Recording, not running: the score realm's half of the visuals window.
//!
//! `hydra-synth` is a WebGL library. It cannot run here and is not asked to.
//! What this module installs is a set of globals that look exactly like
//! Hydra's and do nothing but *write down* what they were asked to draw -
//! which generator, which arguments, which methods, in which order. The
//! recording is staged as an ordinary score effect, so a score that throws
//! halfway through changes nothing on screen, and a score that succeeds hands
//! the window a complete picture in one piece.
//!
//! Two consequences are worth stating plainly, because they are the price of
//! not shipping a browser inside the engine:
//!
//! * A function argument - `osc(() => Math.sin(time))` - crosses as its own
//!   source text and is compiled in the window, where Hydra's `time` and
//!   `mouse` live. It cannot close over score variables.
//! * `H(pattern)` needs a pattern the native engine can query without
//!   JavaScript, because the values are sampled long after the score has
//!   finished evaluating, on a thread with no realm.
//!
//! The whole module is behind this crate's `hydra` feature. The complete
//! `rustel-runtime` enables it by default; an explicit lean runtime leaves it
//! out, where `initHydra` becomes a one-line refusal instead.

use super::*;
use rquickjs::function::{Rest, This};
use rquickjs::object::Accessor;

/// Marks a recorded chain: the value is its index in the per-evaluation arena.
const CHAIN_KEY: &str = "__rustelHydraChain";
/// Marks an `H(pattern)` handle: the value is its signal slot.
const SIGNAL_KEY: &str = "__rustelHydraSignal";

/// Hydra's source generators. These take arguments and start a chain.
const GENERATORS: &[&str] = &[
    "osc", "noise", "voronoi", "shape", "gradient", "solid", "src", "prev",
];

/// Hydra's outputs, sources and read-only values. These are used as values and
/// occasionally have methods called on them (`s0.init(...)`).
const VALUES: &[&str] = &[
    "o0", "o1", "o2", "o3", "s0", "s1", "s2", "s3", "time", "mouse", "width", "height", "a",
];

/// Hydra globals that are plain calls with no chain.
const ACTIONS: &[&str] = &["render", "hush", "setResolution", "update"];

/// Hydra globals a sketch may try to assign. Their native timing semantics are
/// not implemented, so setters refuse explicitly; the getter still preserves
/// the `speed(...)` pattern control instead of shadowing it with a number.
const SETTINGS: &[&str] = &["speed", "bpm", "fps", "time", "update", "afterUpdate"];

/// Defaults mirrored from `rustel-hydra`, so a score that says nothing gets
/// the same picture the renderer would have chosen.
const DEFAULT_WIDTH: u16 = 640;
const DEFAULT_HEIGHT: u16 = 360;
const DEFAULT_STRENGTH: f32 = 0.45;

/// Ceilings mirrored from `rustel-hydra`, enforced here so a runaway score is
/// refused where it is written rather than where it is drawn.
const MAX_STATEMENTS: usize = 64;
const MAX_CHAINS: usize = 2_048;
const MAX_ARGS: usize = 16;
const MAX_CALLS: usize = 128;
const MAX_SIGNALS: usize = 64;
const MAX_SOURCE_BYTES: usize = 4 * 1024;
const MAX_TEXT_BYTES: usize = 4 * 1024;
const MAX_DEPTH: usize = 24;
const MAX_RECORDED_AUDIO_BINS: usize = 16;

pub const HYDRA_SCOPE_POLICY: &str = "initHydra()/H() can open a visuals window only during a Session-owned score evaluation, not from raw code, a setup file, or a query-time callback";

/// One recorded chain, before it becomes a statement.
#[derive(Clone, Debug)]
struct Chain {
    head: String,
    /// `None` until the generator is called: `osc` is a value, `osc(10)` is a
    /// call, and `o0` is never called at all.
    args: Option<Vec<serde_json::Value>>,
    calls: Vec<serde_json::Value>,
}

/// What one score has asked to be drawn, while it is still being written.
///
/// Reaches the host as part of [`ScoreEffects`], and only after the score it
/// belongs to has evaluated without throwing.
#[derive(Default)]
pub struct HydraCandidate {
    /// Set by `initHydra(...)`. `None` means the score never called it, which
    /// is also what a score with no visuals at all looks like.
    options: Option<serde_json::Map<String, serde_json::Value>>,
    statements: Vec<serde_json::Value>,
    /// The patterns behind `H(...)`, in slot order. Pure by construction: the
    /// values are sampled after evaluation, on a thread with no JavaScript.
    signals: Vec<rustel_core::purity::PurePattern>,
}

/// A `Pattern` is neither `Debug` nor `PartialEq` - it is a graph, not a
/// value - so a candidate reports and compares the thing that actually
/// describes it: the program it would send, plus how many signals feed it.
impl std::fmt::Debug for HydraCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HydraCandidate")
            .field("program", &self.program())
            .finish()
    }
}

impl PartialEq for HydraCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.signals.len() == other.signals.len() && self.program() == other.program()
    }
}

impl HydraCandidate {
    /// Recording schema emitted by this side of the score/window boundary.
    /// `rustel-runtime` asserts this against `rustel-hydra` so the otherwise
    /// intentionally decoupled crates cannot silently drift.
    pub const PROGRAM_VERSION: u16 = 3;

    /// Number of typed source slots emitted by this recording version.
    pub const SOURCE_SLOTS: usize = 4;

    /// Fixed capacity of the typed analyser configuration.
    pub const MAX_AUDIO_BINS: usize = MAX_RECORDED_AUDIO_BINS;

    /// hydra-synth's analyser constructor default.
    pub const DEFAULT_AUDIO_BINS: usize = 4;

    /// hydra-synth's easing names, in its order: the ones `[…].ease(name)`
    /// marks. `rustel-runtime` asserts that `rustel-hydra` compiles exactly
    /// these.
    pub const EASINGS: &[&str] = &[
        "linear",
        "easeInQuad",
        "easeOutQuad",
        "easeInOutQuad",
        "easeInCubic",
        "easeOutCubic",
        "easeInOutCubic",
        "easeInQuart",
        "easeOutQuart",
        "easeInOutQuart",
        "easeInQuint",
        "easeOutQuint",
        "easeInOutQuint",
        "sin",
    ];

    /// True when the score asked for nothing to be drawn: it never called
    /// `initHydra()`, it drew nothing, or it called `clearHydra()`.
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// The patterns behind `H(...)`, in slot order.
    pub fn signals(&self) -> &[rustel_core::purity::PurePattern] {
        &self.signals
    }

    /// The recording, in the shape `rustel-hydra` deserializes.
    pub fn program(&self) -> serde_json::Value {
        serde_json::json!({
            "version": Self::PROGRAM_VERSION,
            "signals": self.signals.len(),
            "options": self
                .options
                .clone()
                .map_or_else(|| serde_json::json!(null), serde_json::Value::Object),
            "statements": self.statements,
        })
    }
}

/// The arena of chains being built by the score currently evaluating.
#[derive(Default)]
pub(crate) struct HydraChains {
    chains: Vec<Chain>,
    /// Whether the Hydra globals currently shadow the pattern globals of the
    /// same name. What they shadowed is remembered on the JavaScript side,
    /// in the host roots, where a score cannot reach it.
    installed: bool,
    /// The names that this surface protected from `clearScope()` at install.
    /// The surface releases them when it gives the names back.
    protected: Vec<&'static str>,
}

fn candidate(effects: &mut ScoreEffects) -> &mut HydraCandidate {
    effects
        .hydra
        .get_or_insert_with(|| Box::new(HydraCandidate::default()))
}

fn too_many<'js>(ctx: &Ctx<'js>, what: &str, limit: usize) -> rquickjs::Error {
    throw_type_error(
        ctx,
        &format!("a score may record at most {limit} hydra {what}"),
    )
}

/// Convert one JavaScript argument into the recorded form.
fn node<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    value: &rquickjs::Value<'js>,
    depth: usize,
) -> rquickjs::Result<serde_json::Value> {
    if depth > MAX_DEPTH {
        return Err(throw_type_error(
            ctx,
            &format!("a hydra argument may nest at most {MAX_DEPTH} levels deep"),
        ));
    }
    if value.is_undefined() || value.is_null() {
        return Ok(serde_json::json!({ "c": "null" }));
    }
    if let Some(flag) = value.as_bool() {
        return Ok(serde_json::json!({ "c": "bool", "v": flag }));
    }
    if let Some(number) = value.as_number() {
        if !number.is_finite() {
            return Err(throw_type_error(
                ctx,
                "a hydra numeric argument must be finite (NaN and Infinity are not supported)",
            ));
        }
        return Ok(serde_json::json!({ "c": "num", "v": number }));
    }
    if let Some(text) = value.as_string() {
        let text = text.to_string()?;
        if text.len() > MAX_TEXT_BYTES {
            return Err(too_many(
                ctx,
                "string bytes in one argument",
                MAX_TEXT_BYTES,
            ));
        }
        return Ok(serde_json::json!({ "c": "str", "v": text }));
    }
    if let Some(array) = value.as_array() {
        // A score-built array: its length is read through the guard (see
        // `js_array_len`) so the MAX_ARGS check can refuse any claim.
        let len = js_array_len(array)?;
        if len > MAX_ARGS {
            return Err(too_many(ctx, "list entries", MAX_ARGS));
        }
        let mut entries = Vec::with_capacity(len);
        for index in 0..len {
            let entry = array.get::<rquickjs::Value>(index)?;
            entries.push(node(ctx, chains, &entry, depth + 1)?);
        }
        // The marks `ARRAY_UTILS` leaves. A numeric mark that is 0 or NaN
        // reads as unset, as in hydra's `getValue`.
        let mut list = serde_json::json!({ "c": "list", "v": entries });
        let marks = array.as_object();
        for (mark, field) in [
            ("_speed", "speed"),
            ("_smooth", "smooth"),
            ("_offset", "offset"),
        ] {
            if let Ok(value) = marks.get::<_, f64>(mark)
                && value != 0.0
                && !value.is_nan()
            {
                if !value.is_finite() {
                    return Err(throw_type_error(
                        ctx,
                        &format!("a hydra array {mark} value must be finite"),
                    ));
                }
                list[field] = serde_json::json!(value);
            }
        }
        if let Ok(ease) = marks.get::<_, std::string::String>("_easeName") {
            list["ease"] = serde_json::json!(ease);
        }
        return Ok(list);
    }
    if let Some(object) = value.as_object() {
        if let Ok(slot) = object.get::<_, u32>(SIGNAL_KEY) {
            return Ok(serde_json::json!({ "c": "sig", "slot": slot }));
        }
        if let Ok(index) = object.get::<_, u32>(CHAIN_KEY) {
            return chain_node(ctx, chains, index as usize);
        }
    }
    if let Some(function) = value.as_function() {
        // The score's own text, carried across and compiled in the window.
        let source: rquickjs::String = common_to_string(ctx, function.clone().into_value())?;
        let source = source.to_string()?;
        if source.len() > MAX_SOURCE_BYTES {
            return Err(too_many(
                ctx,
                "source bytes in one function",
                MAX_SOURCE_BYTES,
            ));
        }
        return Ok(serde_json::json!({ "c": "fn", "src": source }));
    }
    Ok(serde_json::json!({ "c": "null" }))
}

fn common_to_string<'js>(
    ctx: &Ctx<'js>,
    value: rquickjs::Value<'js>,
) -> rquickjs::Result<rquickjs::String<'js>> {
    let global: Function = ctx.globals().get("String")?;
    global.call((value,))
}

fn chain_node<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    index: usize,
) -> rquickjs::Result<serde_json::Value> {
    let chain = {
        let chains = chains.borrow();
        chains.chains.get(index).cloned().ok_or_else(|| {
            throw_type_error(ctx, "a hydra chain outlived the score that built it")
        })?
    };
    Ok(serde_json::json!({
        "c": "call",
        "head": chain.head,
        "args": chain.args.unwrap_or_default(),
        "calls": chain.calls,
    }))
}

fn arguments<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<Vec<serde_json::Value>> {
    if args.len() > MAX_ARGS {
        return Err(too_many(ctx, "arguments to one call", MAX_ARGS));
    }
    args.iter().map(|arg| node(ctx, chains, arg, 0)).collect()
}

/// Turn the two browser-source initialisers we can acquire natively into a
/// typed protocol statement. A source setup is deliberately not left as an
/// arbitrary chain: it may open hardware or the network and therefore has to
/// reach the host as something it can permission-check before acting.
fn source_statement<'js>(
    ctx: &Ctx<'js>,
    chain: &Chain,
    method: &str,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<Option<serde_json::Value>> {
    let Some(slot) = chain
        .head
        .strip_prefix('s')
        .and_then(|suffix| suffix.parse::<u8>().ok())
        .filter(|slot| usize::from(*slot) < HydraCandidate::SOURCE_SLOTS)
    else {
        return Ok(None);
    };
    const SUPPORTED: &[&str] = &["initCam", "initImage", "clear"];
    const UNSUPPORTED: &[&str] = &[
        "initVideo",
        "initScreen",
        "initStream",
        "initCanvas",
        "init",
    ];
    if !SUPPORTED.contains(&method) && !UNSUPPORTED.contains(&method) {
        return Ok(None);
    }
    if chain.args.is_some() || !chain.calls.is_empty() {
        return Err(throw_type_error(
            ctx,
            &format!(
                "{method}(...) must be called directly on a pristine s0, s1, s2, or s3 source"
            ),
        ));
    }
    if UNSUPPORTED.contains(&method) {
        return Err(throw_type_error(
            ctx,
            &format!(
                "{method}(...) is not supported by native Hydra; use initCam(...) or initImage(url)"
            ),
        ));
    }
    if method == "clear" {
        if !args.is_empty() {
            return Err(throw_type_error(ctx, "sN.clear() accepts no arguments"));
        }
        return Ok(Some(serde_json::json!({
            "s": "clear_source",
            "slot": slot,
        })));
    }

    let source = match method {
        "initCam" => {
            if args.len() > 1 {
                return Err(throw_type_error(
                    ctx,
                    "initCam(...) accepts no arguments for the default camera, or one numeric device index",
                ));
            }
            let device = match args.first() {
                None => None,
                Some(value) if value.is_undefined() => None,
                Some(value) => {
                    let Some(value) = value.as_number() else {
                        return Err(throw_type_error(
                            ctx,
                            "initCam(device) needs a numeric device index",
                        ));
                    };
                    if !value.is_finite()
                        || value < 0.0
                        || value.fract() != 0.0
                        || value > f64::from(u32::MAX)
                    {
                        return Err(throw_type_error(
                            ctx,
                            "initCam(device) needs a non-negative whole-number device index",
                        ));
                    }
                    Some(value as u32)
                }
            };
            serde_json::json!({ "kind": "camera", "device": device })
        }
        "initImage" => {
            if args.len() != 1 {
                return Err(throw_type_error(
                    ctx,
                    "initImage(url) accepts exactly one JavaScript string",
                ));
            }
            let Some(value) = args[0].as_string() else {
                return Err(throw_type_error(
                    ctx,
                    "initImage(url) needs an actual JavaScript string",
                ));
            };
            let url = value.to_string()?;
            if url.is_empty() {
                return Err(throw_type_error(
                    ctx,
                    "initImage(url) needs a non-empty URL",
                ));
            }
            if url.len() > MAX_TEXT_BYTES {
                return Err(too_many(
                    ctx,
                    "string bytes in one source URL",
                    MAX_TEXT_BYTES,
                ));
            }
            serde_json::json!({ "kind": "image_url", "url": url })
        }
        _ => return Ok(None),
    };
    Ok(Some(serde_json::json!({
        "s": "source",
        "slot": slot,
        "source": source,
    })))
}

enum AnalyserEffect {
    Record(serde_json::Value),
    Noop,
}

/// Interpret the analyser methods whose semantics matter outside a GLSL
/// chain. Upstream resizes `a.fft` and divides its spectrum into `n`
/// contiguous regions. Its analyser-canvas visibility methods have no native
/// canvas to affect, while its scaling/smoothing controls must be refused
/// until their settings are carried by the typed protocol.
fn analyser_effect<'js>(
    ctx: &Ctx<'js>,
    chain: &Chain,
    method: &str,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<Option<AnalyserEffect>> {
    if chain.head != "a" {
        return Ok(None);
    }
    const MATERIAL_CONTROLS: &[&str] = &["setCutoff", "setScale", "setSmooth", "setMax"];
    const CANVAS_CONTROLS: &[&str] = &["show", "hide"];
    if method != "setBins"
        && !MATERIAL_CONTROLS.contains(&method)
        && !CANVAS_CONTROLS.contains(&method)
    {
        return Ok(None);
    }
    if chain.args.is_some() || !chain.calls.is_empty() {
        return Err(throw_type_error(
            ctx,
            &format!("a.{method}(...) must be called directly on the pristine audio analyser"),
        ));
    }
    if MATERIAL_CONTROLS.contains(&method) {
        return Err(throw_type_error(
            ctx,
            &format!(
                "a.{method}(...) is not supported by native Hydra yet; only a.setBins(n) is available"
            ),
        ));
    }
    if CANVAS_CONTROLS.contains(&method) {
        if !args.is_empty() {
            return Err(throw_type_error(
                ctx,
                &format!("a.{method}() accepts no arguments"),
            ));
        }
        // Native Hydra has no browser analyser canvas. These calls are kept
        // as intentional compatibility no-ops so shared sketches need not
        // remove their `a.hide()` line.
        return Ok(Some(AnalyserEffect::Noop));
    }
    if args.len() != 1 {
        return Err(throw_type_error(
            ctx,
            "a.setBins(n) accepts exactly one numeric bin count",
        ));
    }
    let Some(bins) = args[0].as_number() else {
        return Err(throw_type_error(
            ctx,
            "a.setBins(n) needs a numeric whole-number bin count",
        ));
    };
    if !bins.is_finite()
        || bins.fract() != 0.0
        || !(1.0..=MAX_RECORDED_AUDIO_BINS as f64).contains(&bins)
    {
        return Err(throw_type_error(
            ctx,
            &format!(
                "a.setBins(n) needs a whole-number bin count from 1 through {MAX_RECORDED_AUDIO_BINS}"
            ),
        ));
    }
    Ok(Some(AnalyserEffect::Record(serde_json::json!({
        "s": "audio",
        "bins": bins as u8,
    }))))
}

/// Private state, kept in the host roots where a score cannot reach it: the
/// globals this surface shadowed and what they were, plus the values a score
/// has assigned to Hydra's settable names.
const HYDRA_STATE: &str = "__rustel_hydra_state";
const SAVED: usize = 0;
const ASSIGNED: usize = 1;

fn state<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    host_stack(ctx)?.as_object().get(HYDRA_STATE)
}

fn bare_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Object<'js>> {
    let object = rquickjs::Object::new(ctx.clone())?;
    object.set_prototype(None)?;
    Ok(object)
}

/// Shadow the Hydra names, remembering what each meant first.
///
/// Six of them already name a pattern global: `osc`, `noise`, `shape`, `src`,
/// `speed` and `time`. Upstream has the same collision and resolves it the
/// same way: after `initHydra()` the name is Hydra's, and `clearHydra()`
/// gives it back. `speed` is the exception and differs from upstream: it is
/// installed as a property whose getter still returns the pattern control,
/// so `speed(2)` keeps working. Assigning Hydra's clock settings is refused
/// until the native renderer can honor them.
fn install_surface<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    transaction: &Rc<RefCell<Option<ScoreEffects>>>,
    policy: &Rc<Cell<EffectPolicy>>,
    refusal: &Rc<RefCell<Option<String>>>,
) -> rquickjs::Result<()> {
    if chains.borrow().installed {
        return Ok(());
    }
    let globals = ctx.globals();
    let state = state(ctx)?;
    let saved: rquickjs::Object = state.get(SAVED)?;

    for name in GENERATORS.iter().chain(VALUES).chain(ACTIONS) {
        remember(ctx, &globals, &saved, name)?;
    }
    for name in SETTINGS {
        remember(ctx, &globals, &saved, name)?;
    }

    for name in GENERATORS.iter().chain(VALUES) {
        let value = new_chain(
            ctx,
            chains,
            transaction,
            policy,
            refusal,
            Chain {
                head: (*name).to_owned(),
                args: None,
                calls: Vec::new(),
            },
        )?;
        globals.set(*name, value)?;
    }

    for name in ACTIONS {
        let action = action_function(ctx, chains, transaction, policy, refusal, name)?;
        globals.set(*name, action)?;
    }

    for name in SETTINGS {
        install_setting(ctx, chains, transaction, policy, refusal, name)?;
    }

    // Like hydra-synth's, the array methods stay once installed.
    let array_utils: Function = ctx.eval(ARRAY_UTILS)?;
    array_utils.call::<_, ()>((HydraCandidate::EASINGS.to_vec(),))?;

    // Protected while installed. A name the host protected before keeps its
    // protection after the surface gives it back.
    let mut protected = Vec::new();
    for name in GENERATORS
        .iter()
        .chain(VALUES)
        .chain(ACTIONS)
        .chain(SETTINGS)
    {
        if protect_host_global(ctx, name)? {
            protected.push(*name);
        }
    }
    let mut chains = chains.borrow_mut();
    chains.protected = protected;
    chains.installed = true;
    Ok(())
}

/// hydra-synth's array-utils: `fast`, `smooth`, `ease`, `offset` and `fit` on
/// `Array.prototype`, marking the array with underscore-prefixed own
/// properties that [`node`] records. Called with [`HydraCandidate::EASINGS`];
/// `.ease` marks one of those names, ignores any other, and refuses a
/// function, which cannot run at render time.
const ARRAY_UTILS: &str = r#"
(function (easings) {
  const define = (name, value) => {
    Object.defineProperty(Array.prototype, name, {
      value, writable: true, configurable: true, enumerable: false,
    });
  };
  define('fast', function (speed = 1) { this._speed = speed; return this; });
  define('smooth', function (smooth = 1) { this._smooth = smooth; return this; });
  define('ease', function (ease = 'linear') {
    if (typeof ease === 'function') {
      throw new TypeError(
        "array .ease(fn) cannot run at render time here - use one of hydra's easing names, like .ease('sin')"
      );
    }
    if (easings.includes(ease)) { this._smooth = 1; this._easeName = ease; }
    return this;
  });
  define('offset', function (offset = 0.5) { this._offset = offset % 1.0; return this; });
  define('fit', function (low = 0, high = 1) {
    const lowest = Math.min(...this);
    const highest = Math.max(...this);
    const mapped = this.map(
      (num) => (num - lowest) * (high - low) / (highest - lowest) + low
    );
    mapped._speed = this._speed;
    mapped._smooth = this._smooth;
    mapped._easeName = this._easeName;
    return mapped;
  });
})
"#;

fn remember<'js>(
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
    saved: &rquickjs::Object<'js>,
    name: &str,
) -> rquickjs::Result<()> {
    if saved.contains_key(name)? {
        return Ok(());
    }
    let current: rquickjs::Value = globals.get(name)?;
    saved.set(name, current)?;
    let _ = ctx;
    Ok(())
}

/// Give the shadowed names back.
///
/// Called by `clearHydra()` and again at the start of every score evaluation,
/// so a save that removes the visuals also removes their hold on `shape`.
pub(crate) fn restore_surface<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
) -> rquickjs::Result<()> {
    if !chains.borrow().installed {
        return Ok(());
    }
    let globals = ctx.globals();
    let state = state(ctx)?;
    let saved: rquickjs::Object = state.get(SAVED)?;
    for entry in saved.props::<String, rquickjs::Value>() {
        let (name, original) = entry?;
        // Delete first, always. While the surface is installed `speed` is an
        // accessor, and assigning to an accessor calls its SETTER - which
        // would record a drawing instruction for a window that is closing.
        globals.remove(name.as_str())?;
        // Defined, not assigned: an assignment runs any setter on the
        // prototype chain, and a score turn restores before its deadline
        // starts. The attributes are the ones the engine's `set` gave it.
        if !original.is_undefined() {
            globals.prop(
                name.as_str(),
                rquickjs::object::Property::from(original)
                    .writable()
                    .enumerable()
                    .configurable(),
            )?;
        }
    }
    state.set(SAVED, bare_object(ctx)?)?;
    state.set(ASSIGNED, bare_object(ctx)?)?;
    let mut chains = chains.borrow_mut();
    for name in std::mem::take(&mut chains.protected) {
        release_host_global(ctx, name)?;
    }
    chains.chains.clear();
    chains.installed = false;
    Ok(())
}

/// Build the recorder for one chain.
///
/// It is a `Proxy` over a callable target: the `apply` trap records the
/// generator's arguments, the `get` trap records a method. Every trap returns a
/// NEW recorder, so `osc(10)` can be branched into two different chains the
/// way Hydra allows, and nothing a score builds mutates something it already
/// handed to `.out()`.
fn new_chain<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    transaction: &Rc<RefCell<Option<ScoreEffects>>>,
    policy: &Rc<Cell<EffectPolicy>>,
    refusal: &Rc<RefCell<Option<String>>>,
    chain: Chain,
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let index = {
        let mut chains = chains.borrow_mut();
        if chains.chains.len() >= MAX_CHAINS {
            return Err(too_many(ctx, "chains in one score", MAX_CHAINS));
        }
        chains.chains.push(chain);
        chains.chains.len() - 1
    };

    let target = Function::new(ctx.clone(), || ())?;
    target.set(CHAIN_KEY, index as u32)?;

    let handler = bare_object(ctx)?;
    handler.set(
        "get",
        Function::new(ctx.clone(), {
            let chains = chains.clone();
            let transaction = transaction.clone();
            let policy = policy.clone();
            let refusal = refusal.clone();
            move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
                chain_get(&ctx, &chains, &transaction, &policy, &refusal, &args.0)
            }
        })?,
    )?;
    handler.set(
        "apply",
        Function::new(ctx.clone(), {
            let chains = chains.clone();
            let transaction = transaction.clone();
            let policy = policy.clone();
            let refusal = refusal.clone();
            move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
                chain_apply(&ctx, &chains, &transaction, &policy, &refusal, &args.0)
            }
        })?,
    )?;

    let proxy: rquickjs::function::Constructor = ctx.globals().get("Proxy")?;
    proxy.construct((target, handler))
}

fn chain_index<'js>(
    ctx: &Ctx<'js>,
    target: Option<&rquickjs::Value<'js>>,
) -> rquickjs::Result<usize> {
    target
        .and_then(rquickjs::Value::as_object)
        .and_then(|object| object.get::<_, u32>(CHAIN_KEY).ok())
        .map(|index| index as usize)
        .ok_or_else(|| throw_type_error(ctx, "a hydra chain lost track of itself"))
}

fn chain_at<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    index: usize,
) -> rquickjs::Result<Chain> {
    chains
        .borrow()
        .chains
        .get(index)
        .cloned()
        .ok_or_else(|| throw_type_error(ctx, "a hydra chain outlived the score that built it"))
}

fn chain_apply<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    transaction: &Rc<RefCell<Option<ScoreEffects>>>,
    policy: &Rc<Cell<EffectPolicy>>,
    refusal: &Rc<RefCell<Option<String>>>,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let index = chain_index(ctx, args.first())?;
    let mut chain = chain_at(ctx, chains, index)?;
    if chain.args.is_some() {
        return Err(throw_type_error(
            ctx,
            &format!(
                "hydra's {}(...) is already called; call a method on it",
                chain.head
            ),
        ));
    }
    let called: Vec<rquickjs::Value> = match args.get(2).and_then(rquickjs::Value::as_array) {
        // The recorded call arguments are a score-authored length claim (see
        // `js_array_len`), held to the argument limit `arguments` enforces
        // BEFORE any of it is copied into host slots.
        Some(list) => {
            if js_array_len(list)? > MAX_ARGS {
                return Err(too_many(ctx, "arguments to one call", MAX_ARGS));
            }
            js_array_values(ctx, list)?
        }
        None => Vec::new(),
    };
    chain.args = Some(arguments(ctx, chains, &called)?);
    new_chain(ctx, chains, transaction, policy, refusal, chain)
}

/// Names a chain must NOT record, because the language asks for them.
///
/// `then` is the one that matters: a score is evaluated inside an async
/// function, and if the value it ends on looks thenable the runtime waits for
/// a resolution that a drawing instruction is never going to provide.
const RESERVED: &[&str] = &[
    "then",
    "catch",
    "finally",
    "constructor",
    "prototype",
    "toJSON",
    "valueOf",
    "length",
    "name",
];

fn chain_get<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    transaction: &Rc<RefCell<Option<ScoreEffects>>>,
    policy: &Rc<Cell<EffectPolicy>>,
    refusal: &Rc<RefCell<Option<String>>>,
    args: &[rquickjs::Value<'js>],
) -> rquickjs::Result<rquickjs::Value<'js>> {
    let index = chain_index(ctx, args.first())?;
    let Some(key) = args.get(1).and_then(rquickjs::Value::as_string) else {
        // A symbol. Nothing Hydra has is reached by one.
        return Ok(rquickjs::Value::new_undefined(ctx.clone()));
    };
    let key = key.to_string()?;
    if key == CHAIN_KEY {
        return Ok(rquickjs::Value::new_number(ctx.clone(), index as f64));
    }
    if key == "toString" {
        let head = chain_at(ctx, chains, index)?.head;
        let text = format!("[hydra {head}]");
        let function = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            rquickjs::String::from_str(ctx, &text)
        })?;
        return Ok(function.into_value());
    }
    if RESERVED.contains(&key.as_str()) {
        return Ok(rquickjs::Value::new_undefined(ctx.clone()));
    }

    let chains = chains.clone();
    let transaction = transaction.clone();
    let policy = policy.clone();
    let refusal = refusal.clone();
    let method = key;
    let named = method.clone();
    let function = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
            let mut chain = chain_at(&ctx, &chains, index)?;
            if chain.calls.len() >= MAX_CALLS {
                return Err(too_many(&ctx, "calls in one chain", MAX_CALLS));
            }
            if let Some(statement) = source_statement(&ctx, &chain, &method, &args.0)? {
                push_statement(&ctx, &transaction, &policy, &refusal, statement)?;
                // A source configuration is a complete visual effect even
                // without a following `.out()`. Return the same sentinel as
                // `.out()` so the score commit seam does not mistake a
                // source-only program for an undefined/no-pattern result and
                // discard its staged transaction.
                return ctx.globals().get("silence");
            }
            if let Some(effect) = analyser_effect(&ctx, &chain, &method, &args.0)? {
                if let AnalyserEffect::Record(statement) = effect {
                    push_statement(&ctx, &transaction, &policy, &refusal, statement)?;
                }
                // Like source setup, analyser setup is a complete visual
                // effect and must survive a score containing no `.out()`.
                // show/hide are deliberate native no-ops because there is no
                // analyser canvas, but still return the statement sentinel.
                return ctx.globals().get("silence");
            }
            let recorded = arguments(&ctx, &chains, &args.0)?;
            let out = method == "out";
            chain.calls.push(serde_json::json!({
                "method": method.clone(),
                "args": recorded,
            }));
            if out {
                // `.out()` is the only method that means anything on its own: it
                // is what puts a chain on the screen. Recording the statement here
                // is what makes a score's drawing order the order it was written.
                let node = serde_json::json!({
                    "c": "call",
                    "head": chain.head,
                    "args": chain.args.clone().unwrap_or_default(),
                    "calls": chain.calls,
                });
                push_statement(
                    &ctx,
                    &transaction,
                    &policy,
                    &refusal,
                    serde_json::json!({
                        "s": "eval",
                        "node": node,
                    }),
                )?;
                // Return silence, for two reasons. A drawing instruction is
                // not a pattern, so returning the chain would leave a
                // visuals-only score without a pattern. The score realm
                // reports that as an error, and the error would discard the
                // recording. Silence is also correct: this statement makes
                // no sound.
                return ctx.globals().get("silence");
            }
            new_chain(&ctx, &chains, &transaction, &policy, &refusal, chain)
        },
    )?;
    function.set_name(&named)?;
    Ok(function.into_value())
}

fn push_statement<'js>(
    ctx: &Ctx<'js>,
    transaction: &Rc<RefCell<Option<ScoreEffects>>>,
    policy: &Rc<Cell<EffectPolicy>>,
    refusal: &Rc<RefCell<Option<String>>>,
    statement: serde_json::Value,
) -> rquickjs::Result<()> {
    stage_effect(
        ctx,
        policy,
        EffectPolicy::HYDRA,
        transaction,
        refusal,
        HYDRA_SCOPE_POLICY,
        move |ctx, effects| {
            let candidate = candidate(effects);
            if candidate.options.is_none() {
                return Err(throw_type_error(
                    ctx,
                    "call initHydra() before drawing: a hydra chain needs somewhere to draw",
                ));
            }
            if candidate.statements.len() >= MAX_STATEMENTS {
                return Err(too_many(ctx, "drawing statements", MAX_STATEMENTS));
            }
            candidate.statements.push(statement);
            Ok(())
        },
    )
}

fn action_function<'js>(
    ctx: &Ctx<'js>,
    chains: &Rc<RefCell<HydraChains>>,
    transaction: &Rc<RefCell<Option<ScoreEffects>>>,
    policy: &Rc<Cell<EffectPolicy>>,
    refusal: &Rc<RefCell<Option<String>>>,
    name: &'static str,
) -> rquickjs::Result<Function<'js>> {
    let chains = chains.clone();
    let transaction = transaction.clone();
    let policy = policy.clone();
    let refusal = refusal.clone();
    let function = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
            match name {
                "setResolution" => {
                    return Err(throw_type_error(
                        &ctx,
                        "setResolution(width, height) is not supported by native Hydra; Studio chooses its delivery size, while headless or custom hosts can use initHydra({ width, height }) as a fallback",
                    ));
                }
                "update" => {
                    return Err(throw_type_error(
                        &ctx,
                        "update(callback) is not supported by native Hydra",
                    ));
                }
                "hush" if !args.0.is_empty() => {
                    return Err(throw_type_error(&ctx, "hush() accepts no arguments"));
                }
                _ => {}
            }
            let recorded = arguments(&ctx, &chains, &args.0)?;
            push_statement(
                &ctx,
                &transaction,
                &policy,
                &refusal,
                serde_json::json!({
                    "s": "eval",
                    "node": { "c": "call", "head": name, "args": recorded, "calls": [] },
                }),
            )?;
            // `render(o3)` commonly ends a visuals-only sketch. Like `.out()`
            // and source initialisers, a command must leave a pattern-shaped
            // sentinel for the score commit seam or its already-staged Hydra
            // statements would be discarded as a no-pattern fallback.
            ctx.globals().get::<_, rquickjs::Value>("silence")
        },
    )?;
    function.set_name(name)?;
    Ok(function)
}

/// Keep a colliding pattern global readable, but reject an assignment whose
/// Hydra timing semantics the native renderer cannot reproduce.
fn install_setting<'js>(
    ctx: &Ctx<'js>,
    _chains: &Rc<RefCell<HydraChains>>,
    _transaction: &Rc<RefCell<Option<ScoreEffects>>>,
    _policy: &Rc<Cell<EffectPolicy>>,
    _refusal: &Rc<RefCell<Option<String>>>,
    name: &'static str,
) -> rquickjs::Result<()> {
    // Preserve the post-install getter while replacing only assignment:
    // the pattern speed control, Hydra's time recorder, or update(...)'s clear
    // unsupported error all remain callable/readable.
    let current: rquickjs::Value = ctx.globals().get(name)?;
    let assigned: rquickjs::Object = state(ctx)?.get(ASSIGNED)?;
    assigned.set(name, current)?;
    let get = move |ctx: Ctx<'js>| -> rquickjs::Result<rquickjs::Value<'js>> {
        let assigned: rquickjs::Object = state(&ctx)?.get(ASSIGNED)?;
        assigned.get(name)
    };
    let set = move |ctx: Ctx<'js>, _args: Rest<rquickjs::Value<'js>>| -> rquickjs::Result<()> {
        Err(throw_type_error(
            &ctx,
            &format!(
                "assigning Hydra's {name} is not supported by native Hydra; the timing effect would otherwise be silently wrong"
            ),
        ))
    };
    ctx.globals()
        .prop(name, Accessor::new(get, set).configurable().enumerable())
}

fn resolved<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let promise: rquickjs::Object = ctx.globals().get("Promise")?;
    let resolve: Function = promise.get("resolve")?;
    resolve.call((This(promise),))
}

fn flag<'js>(options: &rquickjs::Object<'js>, names: &[&str]) -> rquickjs::Result<Option<bool>> {
    for name in names {
        let value: rquickjs::Value = options.get(*name)?;
        if !value.is_undefined() {
            return Ok(Some(value.as_bool().unwrap_or(!value.is_null())));
        }
    }
    Ok(None)
}

fn number<'js>(options: &rquickjs::Object<'js>, names: &[&str]) -> rquickjs::Result<Option<f64>> {
    for name in names {
        let value: rquickjs::Value = options.get(*name)?;
        if let Some(number) = value.as_number()
            && number.is_finite()
        {
            return Ok(Some(number));
        }
    }
    Ok(None)
}

fn text<'js>(options: &rquickjs::Object<'js>, names: &[&str]) -> rquickjs::Result<Option<String>> {
    for name in names {
        let value: rquickjs::Value = options.get(*name)?;
        if let Some(string) = value.as_string() {
            return Ok(Some(string.to_string()?));
        }
    }
    Ok(None)
}

/// Read `initHydra({...})` into the description the renderer understands.
///
/// Upstream's five names keep upstream's meaning. `width` and `height` are a
/// fallback for a headless or custom host that does not request a delivery
/// size; Studio supplies its own size. `strength` is retained and validated
/// for protocol compatibility, but Studio uses its persisted visuals opacity.
///
/// String options must be written in single quotes - `contextType: 'webgl2'` -
/// because a double-quoted string in a score is mini-notation, everywhere in
/// this engine. Every option that matters is a number or a flag for exactly
/// that reason.
fn parse_options<'js>(
    ctx: &Ctx<'js>,
    value: Option<&rquickjs::Value<'js>>,
) -> rquickjs::Result<serde_json::Map<String, serde_json::Value>> {
    let empty = bare_object(ctx)?;
    let options = value
        .and_then(rquickjs::Value::as_object)
        .cloned()
        .unwrap_or(empty);

    let mut map = serde_json::Map::new();
    map.insert(
        "width".into(),
        serde_json::json!(
            number(&options, &["width"])?
                .unwrap_or(f64::from(DEFAULT_WIDTH))
                .clamp(64.0, 4096.0) as u32
        ),
    );
    map.insert(
        "height".into(),
        serde_json::json!(
            number(&options, &["height"])?
                .unwrap_or(f64::from(DEFAULT_HEIGHT))
                .clamp(64.0, 4096.0) as u32
        ),
    );
    map.insert(
        "strength".into(),
        serde_json::json!(
            number(&options, &["strength"])?
                .unwrap_or(f64::from(DEFAULT_STRENGTH))
                .clamp(0.0, 1.0) as f32
        ),
    );
    map.insert(
        "detect_audio".into(),
        serde_json::json!(flag(&options, &["detectAudio"])?.unwrap_or(true)),
    );
    map.insert(
        "feed_strudel".into(),
        serde_json::json!(flag(&options, &["feedStrudel"])?.unwrap_or(false)),
    );
    map.insert(
        "pixel_ratio".into(),
        serde_json::json!(
            number(&options, &["pixelRatio"])?
                .unwrap_or(1.0)
                .clamp(0.05, 4.0)
        ),
    );
    map.insert(
        "pixelated".into(),
        serde_json::json!(flag(&options, &["pixelated"])?.unwrap_or(true)),
    );
    map.insert(
        "context_type".into(),
        serde_json::json!(match text(&options, &["contextType"])?.as_deref() {
            Some("webgl2") => "webgl2",
            _ => "webgl",
        }),
    );
    Ok(map)
}

pub(super) fn install<'js>(
    runtime: &JsRuntime,
    ctx: &Ctx<'js>,
    globals: &rquickjs::Object<'js>,
) -> Result<(), String> {
    let state = rquickjs::Array::new(ctx.clone()).map_err(|error| error.to_string())?;
    state
        .as_object()
        .set_prototype(None)
        .map_err(|error| error.to_string())?;
    state
        .set(SAVED, bare_object(ctx).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    state
        .set(
            ASSIGNED,
            bare_object(ctx).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    host_stack(ctx)
        .map_err(|error| error.to_string())?
        .as_object()
        .set(HYDRA_STATE, state)
        .map_err(|error| error.to_string())?;

    let chains = runtime.hydra_chains.clone();
    let transaction = runtime.effect_transaction.clone();
    let policy = runtime.effect_policy.clone();
    let refusal = runtime.effect_policy_refusal.clone();

    let open = Function::new(ctx.clone(), {
        let chains = chains.clone();
        let transaction = transaction.clone();
        let policy = policy.clone();
        let refusal = refusal.clone();
        move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
            let options = parse_options(&ctx, args.0.first())?;
            stage_effect(
                &ctx,
                &policy,
                EffectPolicy::HYDRA,
                &transaction,
                &refusal,
                HYDRA_SCOPE_POLICY,
                |_ctx, effects| {
                    if effects.hydra.is_none() {
                        // First visuals call of this evaluation: the chains
                        // the last score built are of no use to this one.
                        chains.borrow_mut().chains.clear();
                    }
                    candidate(effects).options = Some(options.clone());
                    Ok(())
                },
            )?;
            install_surface(&ctx, &chains, &transaction, &policy, &refusal)?;
            resolved(&ctx)
        }
    })
    .map_err(|error| error.to_string())?;
    open.set_name("initHydra")
        .map_err(|error| error.to_string())?;
    globals
        .set("initHydra", open)
        .map_err(|error| error.to_string())?;

    let clear = Function::new(ctx.clone(), {
        let chains = chains.clone();
        let transaction = transaction.clone();
        let policy = policy.clone();
        let refusal = refusal.clone();
        move |ctx: Ctx<'js>| {
            stage_effect(
                &ctx,
                &policy,
                EffectPolicy::HYDRA,
                &transaction,
                &refusal,
                HYDRA_SCOPE_POLICY,
                |_ctx, effects| {
                    // An empty candidate is the instruction to stop drawing:
                    // it is the same thing a score that never mentions hydra
                    // says.
                    effects.hydra = Some(Box::new(HydraCandidate::default()));
                    Ok(())
                },
            )?;
            restore_surface(&ctx, &chains)
        }
    })
    .map_err(|error| error.to_string())?;
    clear
        .set_name("clearHydra")
        .map_err(|error| error.to_string())?;
    globals
        .set("clearHydra", clear)
        .map_err(|error| error.to_string())?;

    let signal = Function::new(ctx.clone(), {
        let transaction = transaction.clone();
        let policy = policy.clone();
        let refusal = refusal.clone();
        move |ctx: Ctx<'js>, args: Rest<rquickjs::Value<'js>>| {
            let value = args
                .0
                .first()
                .cloned()
                .unwrap_or_else(|| rquickjs::Value::new_undefined(ctx.clone()));
            let (pattern, _sidecar) = reify_bridged(&ctx, &value)?;
            if !pattern.is_pure() {
                return Err(throw_type_error(
                    &ctx,
                    "H(...) needs a pattern the engine can query on its own, because its values are \
                     read while the window is drawing, long after the score has finished. This one \
                     calls back into JavaScript; build it from mini-notation, signals and numbers \
                     instead",
                ));
            }
            let slot = Cell::new(0_u32);
            stage_effect(
                &ctx,
                &policy,
                EffectPolicy::HYDRA,
                &transaction,
                &refusal,
                HYDRA_SCOPE_POLICY,
                |ctx, effects| {
                    let candidate = candidate(effects);
                    if candidate.signals.len() >= MAX_SIGNALS {
                        return Err(too_many(ctx, "H(...) signals", MAX_SIGNALS));
                    }
                    slot.set(candidate.signals.len() as u32);
                    candidate
                        .signals
                        .push(rustel_core::purity::PurePattern::assert_pure(pattern));
                    Ok(())
                },
            )?;
            let handle = bare_object(&ctx)?;
            handle.set(SIGNAL_KEY, slot.get())?;
            Ok(handle)
        }
    })
    .map_err(|error| error.to_string())?;
    signal.set_name("H").map_err(|error| error.to_string())?;
    globals
        .set("H", signal)
        .map_err(|error| error.to_string())?;

    Ok(())
}
