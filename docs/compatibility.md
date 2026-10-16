# Compatibility limits

This page lists missing or inactive features compared with strudel.cc.
Studio's reference describes each call and its limits.

## Unsupported features

| Feature | Limit |
| --- | --- |
| KabelSalat external inputs | Graphs run natively with `K`, `worklet`, `S` and `audioin`. MIDI, CC and pointer nodes have no native input and report an error. See [KabelSalat graphs](kabelsalat.md) for the supported API and resource limits. |
| Csound | Requires native Csound 6.18 and `--allow-csound`. Audio uses the host sample rate, stereo output and `ksmps=1`. See [Csound](csound.md) for loading, instruments and routing. |
| `getDuration`, `getDur` | Sample-duration lookup is not implemented. These calls report an error. |
| `mondo` and `tidal` template tags | These alternative languages are not registered. |
| `mqtt` | MQTT output is not implemented. |
| Browser DOM | There is no `document`. DOM calls, including browser slider creation, fail. |
| ES modules | `import ... from`, `export` and a relative `import()` are refused. `await import('https://...')` runs a script from a trusted origin as setup code and gives an empty module. See [Script imports](studio.md#script-imports). |
| Phone motion and orientation | There is no native sensor input. |
| Hydra browser features | DOM, video, screen and stream sources, WebRTC, custom GLSL functions, and browser render-loop hooks are unsupported. `setResolution()` is refused. See the [Hydra limits](hydra.md#what-a-score-cannot-do-here). |

## Accepted without native support

These names remain available, but the named feature is inactive.

| Names | Native behavior |
| --- | --- |
| `aliasBank` | Does not register bank aliases of your own. The default aliases are applied anyway. |
| `registerSynthSounds` | Does not register additional synths. |
| `loadSoundfont` | Returns an empty object without loading a soundfont. |
| `.webaudio()`, `.tone()`, `.webdirt()`, `.speak()`, `.wave()`, `.soundfont()`, `.dough()`, `.fscope()` | Return the pattern unchanged. They do not select an output, produce speech, load a font, or open a frequency scope. |
| `.onSchedule()`, `.onTrigger()`, `.onTriggerTime()` | Return the pattern without calling the callback. |
| `addEventListener`, `removeEventListener` | Accept the call. Rustel has no browser events, and the listener is never called. |
| `theme`, `fontFamily`, `fontSize` | Do not change Studio's appearance. |
| `mousex`, `mouseX`, `mousey`, `mouseY` | Return zero. They do not read the pointer. |
| `keyDown`, `whenKey` | Do not read keyboard input. `keyDown` replaces each value with `false`. `whenKey` returns the pattern unchanged. |

## Native audio gaps

These controls have no native audio effect:

- `chorus`, `fadeInTime`, `gate` (`gat`), `octaveR`, `overshape`,
  `expression`, and `sustainpedal`.
- `panspan`, `pansplay`, and `panorient`. OSC sends these names unchanged,
  but SuperDirt expects `span`, `splay`, and `orientation`.
- Controls marked **SuperDirt (OSC)** in Studio. They need an external
  SuperDirt receiver and any extra setup named in their reference entry.
