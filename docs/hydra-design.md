# How the visuals work

Rustel records Hydra chains, composes GLSL, and renders offscreen through
wgpu. The renderer owns GPU setup, compilation, drawing, and readback. Capture
and image workers do acquisition. None of these tasks run on the audio thread.

## Recording and rendering

Hydra calls form staged `ScoreEffects`. They cross the score commit boundary
with the pattern: failed evaluation or a rejected replacement cannot install
new visuals. `.out()` yields silence so a visuals-only score can commit.
Recorded chains must not be thenable.

The composer uses Hydra's generated function table. The shader preamble
adapts the GLSL dialect, separates texture and sampler bindings, and restores
WebGL's coordinate direction. Function bodies stay unchanged. Each output
`o0`-`o3` has two textures; a pass reads the front and writes the back before
swapping. This prevents simultaneous reads and writes to one texture.

Shelf snippets use a parser without JavaScript. It does not accept
[animated arrays](hydra.md#animated-array-arguments). Callback expressions
compile into GLSL; no JavaScript runs per frame. `H(pattern)` is sampled by
the Session owner at audio time. The recorder, validator, composer, and GPU
buffer enforce the same 64-signal limit. `a.fft` uses playback analysis, not
a microphone.

The completed terminal cells supply `feedStrudel`. The renderer rasterizes
printable ASCII into a texture. Display composition preserves interface
opacity and selection contrast. Reference shader and frame tests check
translation; they do not establish universal pixel identity across GPUs.

## External source acquisition

Camera and image requests are typed effects owned by the runtime. A source
bind returns a generation-scoped lease and clears the old slot. Each slot
holds only its newest frame. Rebind, removal, Stop, or permission revocation
invalidates publication before clearing. Old workers cannot restore stale
pixels. Clearing an old lease cannot erase a replacement source.

Each image job carries the session's sample-access policy. The worker checks
origin permission, public-only pinned DNS, same-origin HTTPS redirects, and
CORS before decoding. `HydraBridge::apply_with_sample_access` supplies that
policy; plain `apply` grants no image origins. Each slot has one active job
and one replaceable pending job. Cancellation prevents publication; active
OS I/O can continue until the shared 15-second deadline.

Images and camera frames have a 2048-pixel edge and 4,194,304-pixel limit.
Images also have 16 MiB transfer and 32 MiB decoder-allocation limits. Four
RGBA source slots retain at most 64 MiB. Camera mode selection filters unsafe
or undecodable formats before opening by stable device ID, then validates
the negotiated mode again.

## Camera consent and lifetime

The UI retains an atomic consent handle. Revocation must work even when the
engine command queue is full. Capture requires permission, an active request,
and a visible consumer. A score camera also requires running playback.

A GPU theme must reference `s0` in a valid shader input and receive an exact
theme-epoch acknowledgement after a successful visible render. Hidden,
stale, or failed themes cannot open capture. A native camera theme instead
requests capture only while its camera painter is visible.

Settings can request a lease-free thumbnail only while its allowed webcam
row is selected. It shares a session with a visible native camera theme and
reuses Hydra capture when available. It cannot bind or clear shader sources.

Shutdown invalidates frames immediately. Capture polling observes cancellation
within 100 ms; a platform open can remain blocked. Replacement waits for that
worker to finish, so opens cannot overlap. Ready requires a converted frame.
The first-frame allowance is 65 seconds on macOS for its permission prompt,
five seconds elsewhere. Persistent failures retry after five seconds without
repeating identical log messages.

See [Visuals](hydra.md) for permissions, settings, and unsupported operations.
