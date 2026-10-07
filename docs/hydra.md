# Visuals

Hydra draws visuals behind the code in Studio. It is included in default
builds. See [building](building.md) to make a build without it.

## A set with a picture

```js
await initHydra({ feedStrudel: 1 })
src(s0).kaleid(H("<4 5 6>")).diff(osc(1, 0.5, 5)).out()
$: s("bd*4").bank("RolandTR909")
```

Save this score as `hydra.strudel`. Then open it in Studio:

```sh
rustel studio hydra.strudel
```

Press **F5**. Saving updates the sketch. Stop clears the score's picture;
remove its Hydra calls to remove it from the set.

`rustel <score>` plays the music but has no editor backdrop. A build without
Hydra logs a notice and plays the music without visuals.

## The snippet shelf

Open the reference column with **Ctrl+F**. Press **Tab** until the last tab,
**examples**, is selected. A narrow column shows this label as `examp` or
`ex`. From the **reference** tab, **Shift+Tab** selects it in one press. Move
to the **HYDRA** section and press **Right** to open it.

| Key | Action |
| --- | --- |
| Up / Down | Select a shelf or snippet |
| Right / Left | Open or close a shelf |
| Enter | Open or close a section or shelf, or copy a snippet |
| `c` | Copy the snippet |

The selected snippet previews behind the editor, even while stopped. Leaving
Hydra browsing restores the score's visuals or theme. The preview does not
replace the score.

The Hydra shelves have no generator. The **generator** tab of the reference
column makes music, not Hydra sketches.

## What a score can use

Call `await initHydra()` before Hydra chains. Use **Ctrl+D** on a name or
search the reference for its arguments.

| Option | Effect |
| --- | --- |
| `detectAudio` | Defaults to true. Feeds `a.fft` from playback, never a microphone. `a.setBins(n)` selects 1-16 bins; default 4. |
| `feedStrudel` | Defaults to false. Sends the terminal frame into `s0`. |
| `width`, `height` | Fallback size, default 640×360. Studio's delivery size overrides it. |

Use **single quotes** for string options and URLs, such as
`contextType: 'webgl2'`. Double-quoted score strings are mini-notation.

### Animated array arguments

An array in a numeric argument changes with time:

```js
solid([0.8, 4].smooth(), [5, 3].smooth(), [1, 20].smooth().fast(0.1)).out()
```

Use `.fast()` for rate, `.smooth()` for transitions, `.ease('sin')` for
transition shape, `.offset()` for position, and `.fit(low, high)` for range.
Array entries must be finite numbers. Invalid numeric arguments refuse the
new score and leave the committed score in place. Raw shelf snippets do not
accept arrays.

## Camera and web-image sources

```js
await initHydra()
s0.initCam() // Use initCam(1) for the second enumerated camera.
s1.initImage('https://example.com/texture.png')
src(s0).blend(src(s1), 0.25).out()
```

**Hydra webcam** in Settings is blocked by default. Set it to **allowed**
to give persisted consent. The header shows when capture is requested or
active. Settings reports opening, ready, or an error with platform guidance.
The first macOS permission prompt can keep it opening for up to 65 seconds.

A score camera requires a playing, visible sketch. A camera theme requires
that theme to be visible. Selecting the allowed webcam row can also open a
small preview; leaving the row closes that preview unless a visible camera
theme still needs it. Settings reuses active capture when possible.

Stop, source removal, hiding the camera theme, or blocking permission clears
that request's frames and starts shutdown. A pending platform open can finish
before shutdown completes. It cannot publish stale frames. Physical camera
operation on Windows and Linux has not been verified. Camera indices can
change after reconnecting.

Images need no webcam permission. They load while the score draws and stop
with it. Studio allows public CORS origins by default. With
`--strict-sample-origins`, grant each origin:

```sh
rustel studio set/ --strict-sample-origins \
  --allow-sample-origin https://images.example
```

Use a direct public **HTTPS** image URL. Its response must allow CORS for
`https://strudel.cc` or `*`. Redirects must stay on the same origin. Private,
loopback, and link-local addresses are refused, even with an origin grant.

PNG, JPEG, GIF, and WebP are decoded; animated files use their first frame.
Limits are 16 MiB downloaded, 2048 pixels per edge, and 4,194,304 pixels.
An explicit camera or image on `s0` takes precedence over `feedStrudel`.
Removing it restores the terminal feed on the next evaluation.

## `H(pattern)`

Use a musical pattern to change a shader value:

```js
osc(20, 0.05, H("<3 4 5 6>")).kaleid(H(sine.range(2, 8))).out()
```

`H` accepts patterns the native engine can query, including signals and
sliders. Patterns containing JavaScript callbacks are refused. One program
can use at most 64 signals.

## `feedStrudel` in a terminal

```js
await initHydra({ feedStrudel: 1 })
src(s0).modulate(osc(6), 0.2).out()
```

The terminal's completed frame feeds `s0`, including code colours and scopes.
Inline visualizers stay visible. The texture draws printable ASCII glyphs;
other characters contribute their background colour.

## Opacity

Settings has three controls, each from 0-100%:

- **visuals opacity**, default 55: picture strength. Zero hides all visuals.
- **editor opacity**, default 25: code background. Higher hides more picture.
- **ui opacity**, default 50: panels and other interface backgrounds.

Selections keep a contrast limit for readable text. A
[theme](studio.md#themes) can draw its own picture while the score is not
drawing. Visuals opacity also limits theme pictures.

The **hydra smoothing** row is on the Advanced page of Settings, in the
Rendering group. It is off by default. The renderer can draw the picture
larger than the terminal shows it, then reduces it to the display size. When
smoothing is on, the renderer averages each block of pixels. This reduces
moving pixel edges. When smoothing is off, the renderer takes one pixel from
each block. The render is up to 8 times wider and taller with smoothing on,
and up to 4 times with smoothing off. The factor depends on the display size:
a smaller picture gets a larger factor. Smoothing on a small picture can use
more GPU work. Terminal capability detection selects the display detail
automatically.

## What a score cannot do here

Browser objects, video, screen and stream sources, WebRTC, custom GLSL
functions, `setResolution()`, render-loop hooks (`update`, `afterUpdate`), and
assignments to `fps`, `speed`, `bpm`, or `time` are unsupported. `sum()` is
refused because its pinned upstream shader is invalid.

Callbacks are compiled shader expressions, not arbitrary JavaScript.
Closures, mutation, `mouse`, `Math.random()`, and user functions are refused.
Use `H(pattern)` for musical values. Use `a.fft`, not `a0()`-`a3()`.

`strength`, `pixelRatio`, `pixelated`, and `contextType` are accepted but
ignored. They do not select opacity, render scale, texture sampling, or a
WebGL version. `a.show()` and `a.hide()` do nothing.

After `initHydra()`, names such as `osc`, `noise`, `shape`, `src`, and `time`
belong to Hydra until the next evaluation or `clearHydra()`. Pattern methods
remain unchanged; `speed(2)` remains a pattern control.

Rendering uses an offscreen wgpu backend. Driver and adapter availability
still apply; `WGPU_BACKEND` can select a backend for diagnosis. See
[the design](hydra-design.md) for ownership and resource limits.
