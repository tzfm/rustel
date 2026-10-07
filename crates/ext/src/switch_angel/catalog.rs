/// A source callable that is intentionally not installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OmittedCallable {
    pub name: &'static str,
    pub reason: &'static str,
}

pub const OMITTED_CALLABLES: &[OmittedCallable] = &[
    OmittedCallable {
        name: "registerFunc",
        reason: "prebake installation helper superseded by the generic extension host",
    },
    OmittedCallable {
        name: "o",
        reason: "upstream Strudel already owns `o` as the orbit alias",
    },
    OmittedCallable {
        name: "soloOrbit",
        reason: "depends on replacing upstream's `o` orbit alias",
    },
    OmittedCallable {
        name: "solo",
        reason: "depends on replacing upstream's `o` orbit alias",
    },
    OmittedCallable {
        name: "mute",
        reason: "depends on replacing upstream's `o` orbit alias",
    },
    OmittedCallable {
        name: "octave",
        reason: "provided by the upstream-compatible core surface",
    },
    OmittedCallable {
        name: "ar",
        reason: "upstream Strudel already owns `ar` as the attack-release combinator",
    },
    OmittedCallable {
        name: "comb",
        reason: "requires the browser-only WebAudio FX graph API",
    },
    OmittedCallable {
        name: "_comb",
        reason: "global alias of the browser-only WebAudio `comb` helper",
    },
    OmittedCallable {
        name: "spinor",
        reason: "requires the browser-only WebAudio FX graph API",
    },
    OmittedCallable {
        name: "UISlider",
        reason: "requires the browser DOM and WebAudio controller",
    },
    OmittedCallable {
        name: "disperse",
        reason: "requires the browser-only WebAudio FX graph API",
    },
    OmittedCallable {
        name: "oneshot",
        reason: "requires a score-evaluation transport-cycle snapshot the host does not yet expose",
    },
    OmittedCallable {
        name: "bend",
        reason: "requires atomically publishing an auxiliary named modulation lane from a pattern method",
    },
];

pub const OMITTED_GLOBAL_SIDE_EFFECTS: &[OmittedCallable] = &[
    OmittedCallable {
        name: "setGainCurve",
        reason: "would replace the product-wide gain law at realm startup",
    },
    OmittedCallable {
        name: "setDefault(gain)",
        reason: "would replace the upstream score default for every user",
    },
];

pub const REVIEWED_CALLABLES: &[&str] = &[
    "registerFunc",
    "pg",
    "DX",
    "acidenv",
    "soloOrbit",
    "solo",
    "mute",
    "o",
    "col",
    "cue",
    "getCue",
    "setCue",
    "oncue",
    "accent",
    "track",
    "ar",
    "p",
    "blockArrange",
    "fill",
    "trancegate",
    "tgate",
    "dly",
    "colorparty",
    "grab",
    "mpan",
    "rlpf",
    "rhpf",
    "vstruct",
    "toMajorKey",
    "fmtime",
    "irando",
    "randm",
    "pk",
    "acid",
    "stxt",
    "sf",
    "octave",
    "chrd",
    "notearp",
    "nsc",
    "swap",
    "over",
    "overin",
    "sb",
    "setScale",
    "sc",
    "ifit",
    "trancearp",
    "comb",
    "_comb",
    "min",
    "max",
    "flood",
    "noisehat",
    "zap",
    "roller",
    "spinor",
    "roller2",
    "UISlider",
    "up",
    "oneshot",
    "filtval",
    "glide",
    "glitch",
    "sq",
    "strum",
    "humanize",
    "bend",
    "disperse",
];
