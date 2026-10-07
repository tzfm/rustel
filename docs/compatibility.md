# Compatibility limits

This page lists missing or inactive features compared with strudel.cc.
Studio's reference describes each call and its limits.

## Unsupported features

| Feature | Limit |
| --- | --- |
| KabelSalat: `K`, `worklet`, `S`, `audioin` | Custom DSP is not implemented. These calls report an error. |
| Csound: `loadOrc` | Orchestra loading is not implemented. The call reports an error. |
| `getDuration`, `getDur` | Sample-duration lookup is not implemented. These calls report an error. |
| `mondo` and `tidal` template tags | These alternative languages are not registered. |
| `mqtt` | MQTT output is not implemented. |
| Browser DOM | There is no `document`. DOM calls, including browser slider creation, fail. |
| Phone motion and orientation | There is no native sensor input. |
| Hydra browser features | DOM, video, screen and stream sources, WebRTC, custom GLSL functions, and browser render-loop hooks are unsupported. `setResolution()` is refused. See the [Hydra limits](hydra.md#what-a-score-cannot-do-here). |

## Accepted without native support

These names remain available, but the named feature is inactive.

| Names | Native behavior |
| --- | --- |
| `aliasBank` | Does not register bank aliases. |
| `registerSynthSounds` | Does not register additional synths. |
| `loadSoundfont` | Returns an empty object without loading a soundfont. |
| `.webaudio()`, `.csound()`, `.tone()`, `.webdirt()`, `.speak()`, `.wave()`, `.soundfont()`, `.dough()`, `.fscope()` | Return the pattern unchanged. They do not select an output, produce speech, load a font, or open a frequency scope. |
| `.onTrigger()`, `.onTriggerTime()` | Return the pattern without calling the callback. |
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
