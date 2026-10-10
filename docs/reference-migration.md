# Maintaining the built-in reference

Studio builds its reference from entries beside the code or data they describe.
There is no separate JSON catalogue.

## Where entries live

| Surface | Source |
| --- | --- |
| Controls | `crates/core/src/control_catalog.rs` |
| Pattern combinators | `crates/core/src/register/` |
| JavaScript globals and methods | `crates/jsruntime/src/install/surface.rs` and the host bindings |
| Extensions | `crates/ext/` |
| Visualizers and terminal options | `crates/studio/src/visuals.rs` |
| Synth sounds | `crates/voice/src/lib.rs` |
| Chords | `crates/core/src/voicings.rs` |
| Scales | `crates/core/src/tonaljs_scales.rs` |
| Snippets | `crates/studio/src/snippets.rs` |

Each `ControlRow` and combinator `Registration` needs a `ReferenceEntry`.
Update it with the implementation. The public control module is
`rustel_core::controls_generated`.

Keep adapted documentation credits in the owning module and
[NOTICE.md](../NOTICE.md). `UPSTREAM_DOC_REVISION` in
`crates/studio/src/reference.rs` records the prose source, not a runtime dependency.

## Names and lookup

An entry must use a name or synonym known to the engine. Snippets are separate.
The first entry for a spelling owns that lookup. The order is host entries,
extensions, controls, combinators, visualizers, sounds, chords, then scales.

Callable entries can promote a lowercase alias to the canonical camelCase
name. Sound, chord, and scale names keep their spelling. Exact lookup preserves
chord symbols before tolerant search. Extension entries show their author as
origin. Other documentation credits do not make a function an extension.

## Unsupported features

Keep entries for unsupported features. State whether the call returns a fixed
value, leaves the pattern unchanged, or reports an error. See
[compatibility limits](compatibility.md). A missing native implementation does
not justify hiding a name from search or suggestions.

Hide a compatibility name only after verifying that it has no effect in both
Rustel and Strudel. `REFERENCE_HIDDEN` includes unused FM matrix spellings.
These lists affect visibility, not registration.

Distinguish external routing from native audio support. For controls that need
SuperDirt, put `superdirt` first in `tags`, include `osc`, and start the summary
with `SuperDirt (OSC):`. Describe required receiver setup once. Parameters
should state meaning and units. OSC forwarding alone does not prove receiver
support.

The `osc`, `serial`, `fm_matrix`, `bind` and `internals` tags also set
visibility. Settings ▸ Reference has one switch for each. Give `osc` to every
entry only an OSC receiver reads, including controls with no native effect. A
control with a native effect does not take `superdirt`. A test fails when a
`superdirt` entry has no `osc` tag.

## Existing checks

- Studio reference tests check installed names, aliases, and exact symbols.
- `crates/studio/tests/reference_examples.rs` evaluates examples in real
  sessions, with explicit exceptions for remote samples.
- Core tests check hidden lists and combinator parameter counts.
- `crates/voice/tests/source_controls.rs` checks controls through voice
  conversion and audio rendering with local fixtures.

Each global and pattern method needs an entry under its exact spelling.
The inline tests in `crates/studio/src/reference.rs` record the exceptions:
`JS_BUILTINS` for built-ins and `ENGINE_NAMES_WITHOUT_ENTRIES` for other
installed names. Methods starting with `_`, stepwise aliases starting with
`s_`, and hidden entries are exempt.

Check descriptions against the implementation, especially units, defaults,
and controls with no native effect.
