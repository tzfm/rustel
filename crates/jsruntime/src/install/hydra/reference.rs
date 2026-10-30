//! Reference for the Hydra names a score uses after `initHydra()`.
//!
//! These names live in their own namespace. `osc`, `noise`, `shape` and `src`
//! mean something else in a pattern, so the studio shows these entries only
//! while the caret is in Hydra code. Parameter names and defaults are the ones
//! `rustel-hydra` compiles, and `rustel-runtime` checks them against its table.

use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// One numeric shader input. The type names every form a Hydra argument takes.
macro_rules! param {
    ($name:literal, $description:literal) => {
        ReferenceParam {
            name: $name,
            r#type: "number | array | function | H(pattern)",
            description: $description,
        }
    };
    ($name:literal, $type:literal, $description:literal) => {
        ReferenceParam {
            name: $name,
            r#type: $type,
            description: $description,
        }
    };
}

/// The chain a blend or modulation reads.
const TEXTURE: ReferenceParam = param!(
    "texture",
    "chain | o0-o3 | s0-s3",
    "The picture to mix in: a chain such as osc(10), an output, or an input."
);

macro_rules! entry {
    ($name:literal, [$($alias:literal),*], [$($tag:literal),+], $summary:literal,
     $description:literal, [$($param:expr),* $(,)?], $example:literal) => {
        ReferenceEntry {
            name: $name,
            synonyms: &[$($alias),*],
            summary: $summary,
            description: $description,
            params: &[$($param),*],
            examples: &[concat!("await initHydra()\n", $example)],
            tags: &["hydra", $($tag),+],
            origin: "hydra",
            ..ReferenceEntry::blank($name)
        }
    };
}

/// A name the score writes first in a statement or argument.
macro_rules! global {
    ($name:literal, [$($alias:literal),*], $tag:literal, $summary:literal,
     $description:literal, [$($param:expr),* $(,)?], $example:literal) => {
        entry!($name, [$($alias),*], [$tag, "global"], $summary, $description,
            [$($param),*], $example)
    };
}

/// A global read as a value, never called.
macro_rules! value {
    ($name:literal, [$($alias:literal),*], $tag:literal, $summary:literal,
     $description:literal, $example:literal) => {
        entry!($name, [$($alias),*], [$tag, "global", "value"], $summary, $description,
            [], $example)
    };
}

/// A name the score writes after a dot.
macro_rules! method {
    ($name:literal, $tag:literal, $summary:literal, $description:literal,
     [$($param:expr),* $(,)?], $example:literal) => {
        entry!($name, [], [$tag, "method"], $summary, $description, [$($param),*], $example)
    };
}

/// A Hydra name with no native implementation. Lookup explains why, and
/// completion never offers it.
macro_rules! unavailable {
    ($name:literal, $context:literal, $summary:literal, $description:literal) => {
        ReferenceEntry {
            name: $name,
            summary: $summary,
            description: $description,
            tags: &["hydra", "unsupported", $context],
            no_autocomplete: true,
            origin: "hydra",
            ..ReferenceEntry::blank($name)
        }
    };
}

pub(super) const ENTRIES: &[ReferenceEntry] = &[
    // Sources: each one starts a chain.
    global!(
        "osc",
        [],
        "sources",
        "Sine stripes that move across the picture.",
        "Draws a sine wave as stripes of light. Each colour channel is offset a little, so the stripes have a rainbow edge. Raise frequency for thinner stripes and sync for faster movement.",
        [
            param!("frequency", "Stripes across the picture. Default 60."),
            param!("sync", "Speed of the movement. Default 0.1."),
            param!("offset", "Colour shift between the channels. Default 0.")
        ],
        "osc(10, 0.1, 0.8).out()"
    ),
    global!(
        "noise",
        [],
        "sources",
        "Smooth moving noise.",
        "Draws 3D simplex noise that changes over time. The output is grey, from black to white.",
        [
            param!(
                "scale",
                "Size of the noise. Larger values give smaller details. Default 10."
            ),
            param!("offset", "Speed of the change. Default 0.1.")
        ],
        "noise(3, 0.2).out()"
    ),
    global!(
        "voronoi",
        [],
        "sources",
        "Moving cells.",
        "Draws a Voronoi pattern: cells around points that move. Blending softens the cell borders.",
        [
            param!("scale", "Number of cells. Default 5."),
            param!("speed", "Speed of the points. Default 0.3."),
            param!("blending", "Softness of the borders. Default 0.3.")
        ],
        "voronoi(8, 0.5, 0.2).out()"
    ),
    global!(
        "shape",
        [],
        "sources",
        "A polygon with soft edges.",
        "Draws one white polygon in the centre on black. Use a high number of sides for a circle.",
        [
            param!("sides", "Number of sides. Default 3."),
            param!("radius", "Size, from 0 to 1. Default 0.3."),
            param!("smoothing", "Softness of the edge. Default 0.01.")
        ],
        "shape(4, 0.5, 0.05).out()"
    ),
    global!(
        "gradient",
        [],
        "sources",
        "A colour gradient.",
        "Draws red across and green down, with blue that changes over time at the given speed.",
        [param!("speed", "Speed of the blue change. Default 0.")],
        "gradient(1).out()"
    ),
    global!(
        "solid",
        [],
        "sources",
        "One colour everywhere.",
        "Fills the picture with one colour. Each channel is from 0 to 1.",
        [
            param!("r", "Red. Default 0."),
            param!("g", "Green. Default 0."),
            param!("b", "Blue. Default 0."),
            param!("a", "Alpha. Default 1.")
        ],
        "solid(0.2, 0, 0.6).out()"
    ),
    global!(
        "src",
        [],
        "sources",
        "Start a chain from an output or an input.",
        "Reads an output (o0 to o3) or an external input (s0 to s3). Reading the output that the chain draws into gives feedback.",
        [param!(
            "tex",
            "o0-o3 | s0-s3",
            "The output or input to read."
        )],
        "src(o0).scale(1.01).blend(osc(4), 0.1).out()"
    ),
    global!(
        "prev",
        [],
        "sources",
        "The previous frame of the output being drawn.",
        "Reads the frame that this output drew last time. It gives feedback without naming the output.",
        [],
        "prev().rotate(0.01).blend(shape(5), 0.2).out()"
    ),
    // Outputs, inputs and screen commands.
    value!(
        "o0",
        ["o1", "o2", "o3"],
        "outputs",
        "One of the four outputs.",
        "Hydra has four outputs, o0 to o3. out() draws a chain into one. src() reads one back. The picture shows o0 until render() selects another.",
        "osc(5).out(o1)\nsrc(o1).kaleid(4).out(o0)"
    ),
    value!(
        "s0",
        ["s1", "s2", "s3"],
        "inputs",
        "One of the four external inputs.",
        "Hydra has four inputs, s0 to s3. Fill one with initCam(), initImage() or initVideo(), then read it with src(). An input with nothing loaded is transparent black.",
        "s0.initCam()\nsrc(s0).out()"
    ),
    global!(
        "render",
        [],
        "outputs",
        "Choose the output on the screen.",
        "Shows one output. With no argument, it shows all four outputs in a grid.",
        [param!(
            "output",
            "o0-o3",
            "The output to show. Leave it out for all four."
        )],
        "osc(10).out(o0)\nnoise(4).out(o1)\nrender(o1)"
    ),
    global!(
        "hush",
        [],
        "outputs",
        "Clear every output.",
        "Clears all four outputs to black. Chains that are drawn after hush() draw again.",
        [],
        "hush()"
    ),
    // The audio analyser.
    value!(
        "a",
        [],
        "audio",
        "The audio analyser.",
        "Reads the sound that Rustel plays, never a microphone. a.fft[0] to a.fft[n - 1] hold the level of each frequency band, from 0 upwards. Use them in a function argument. a.setBins(n) sets the number of bands. Turn this off with initHydra({ detectAudio: false }).",
        "osc(10, 0.1, () => a.fft[0] * 4).out()"
    ),
    method!(
        "fft",
        "audio",
        "The levels of the audio bands.",
        "An array with one level for each band of the analyser. Read it in a function argument, for example () => a.fft[0].",
        [],
        "shape(4, () => 0.2 + a.fft[1]).out()"
    ),
    method!(
        "setBins",
        "audio",
        "Set the number of audio bands.",
        "Divides the spectrum into n bands, from 1 to 16. The default is 4.",
        [param!("n", "number", "Number of bands, from 1 to 16.")],
        "a.setBins(8)\nosc(() => a.fft[7] * 40).out()"
    ),
    method!(
        "show",
        "audio",
        "Accepted and ignored.",
        "The browser draws the analyser bands on the canvas. Rustel has no analyser canvas, so a.show() does nothing.",
        [],
        "a.show()"
    ),
    method!(
        "hide",
        "audio",
        "Accepted and ignored.",
        "The browser hides its analyser canvas. Rustel has no analyser canvas, so a.hide() does nothing.",
        [],
        "a.hide()"
    ),
    global!(
        "H",
        [],
        "values",
        "Use a pattern as a Hydra parameter.",
        "Reads a pattern at the current transport position and sends its value to the shader. The pattern must be queryable without JavaScript: mini-notation, arithmetic and signals work, JavaScript callbacks do not. One program can use at most 64 patterns. Inside H() the caret is back in pattern code.",
        [param!(
            "pattern",
            "string | Pattern",
            "The pattern to read, in mini-notation or already built."
        )],
        "shape(H(\"3 4 5 [6 7]*2\")).out(o0)"
    ),
    value!(
        "time",
        [],
        "values",
        "Seconds on Hydra's clock.",
        "The clock that moves every animation. Read it in a function argument, for example () => Math.sin(time). Assign a number to set the clock: with time = 0 every animation starts again at each update. Math.random() in a function argument gives a new value each frame.",
        "osc(() => 10 + Math.sin(time) * 5).out()"
    ),
    value!(
        "speed",
        [],
        "values",
        "How fast Hydra's clock runs.",
        "Assign a number: speed = 0.5 runs every animation at half speed, and 2 at double speed. 1 is real time, and the range is -1000 to 1000. speed(2) is still the playback-speed control.",
        "speed = 0.5\nosc(10, 0.1, 0.8).out()"
    ),
    value!(
        "bpm",
        [],
        "values",
        "The step rate of array arguments.",
        "Assign the beats a minute that array arguments step at. The default is 30, one step every 2 seconds, and the most is 10000. It does not follow the tempo of the score.",
        "bpm = 120\nshape([3, 4, 5]).out()"
    ),
    value!(
        "fps",
        [],
        "values",
        "The most frames a second Hydra draws.",
        "Assign a number from 1 to 240 to draw less often, for a stepped look. The last frame stays on screen between draws. The studio draws at most 30 frames a second.",
        "fps = 8\nnoise(4).out()"
    ),
    value!(
        "width",
        [],
        "values",
        "Render width in pixels.",
        "The width of the picture that Hydra draws. The studio sets it from the size of the backdrop.",
        "osc(() => width / 40).out()"
    ),
    value!(
        "height",
        [],
        "values",
        "Render height in pixels.",
        "The height of the picture that Hydra draws. The studio sets it from the size of the backdrop.",
        "osc(() => height / 20).out()"
    ),
    // Geometry: these move where a chain samples its picture.
    method!(
        "rotate",
        "geometry",
        "Turn the picture.",
        "Turns the picture around its centre by angle radians, plus speed radians each second.",
        [
            param!("angle", "Angle in radians. Default 10."),
            param!("speed", "Extra radians each second. Default 0.")
        ],
        "osc(20).rotate(0.8, 0.1).out()"
    ),
    method!(
        "scale",
        "geometry",
        "Zoom the picture.",
        "Values above 1 make the picture smaller and repeat it. xMult and yMult stretch one axis. offsetX and offsetY move the centre of the zoom.",
        [
            param!("amount", "Zoom. Default 1.5."),
            param!("xMult", "Extra zoom across. Default 1."),
            param!("yMult", "Extra zoom down. Default 1."),
            param!("offsetX", "Centre across, from 0 to 1. Default 0.5."),
            param!("offsetY", "Centre down, from 0 to 1. Default 0.5.")
        ],
        "shape(4).scale(0.5, 2, 1).out()"
    ),
    method!(
        "pixelate",
        "geometry",
        "Make large pixels.",
        "Divides the picture into a grid and gives each cell one colour.",
        [
            param!("pixelX", "Cells across. Default 20."),
            param!("pixelY", "Cells down. Default 20.")
        ],
        "noise(3).pixelate(16, 9).out()"
    ),
    method!(
        "repeat",
        "geometry",
        "Tile the picture.",
        "Repeats the picture in a grid. The offsets shift every second row or column.",
        [
            param!("repeatX", "Tiles across. Default 3."),
            param!("repeatY", "Tiles down. Default 3."),
            param!("offsetX", "Shift of every second row. Default 0."),
            param!("offsetY", "Shift of every second column. Default 0.")
        ],
        "shape(3).repeat(4, 4, 0.5, 0).out()"
    ),
    method!(
        "repeatX",
        "geometry",
        "Tile the picture across.",
        "Repeats the picture across. The offset shifts every second column.",
        [
            param!("reps", "Tiles across. Default 3."),
            param!("offset", "Shift of every second column. Default 0.")
        ],
        "shape(4).repeatX(5).out()"
    ),
    method!(
        "repeatY",
        "geometry",
        "Tile the picture down.",
        "Repeats the picture down. The offset shifts every second row.",
        [
            param!("reps", "Tiles down. Default 3."),
            param!("offset", "Shift of every second row. Default 0.")
        ],
        "shape(4).repeatY(5).out()"
    ),
    method!(
        "kaleid",
        "geometry",
        "Mirror the picture like a kaleidoscope.",
        "Folds the picture around its centre into nSides mirrored slices.",
        [param!("nSides", "Number of slices. Default 4.")],
        "osc(25, 0.1, 0.5).kaleid(6).out()"
    ),
    method!(
        "scroll",
        "geometry",
        "Move the picture.",
        "Moves the picture by scrollX and scrollY, and keeps it moving at speedX and speedY. The picture wraps at the edges.",
        [
            param!("scrollX", "Shift across. Default 0.5."),
            param!("scrollY", "Shift down. Default 0.5."),
            param!("speedX", "Movement across each second. Default 0."),
            param!("speedY", "Movement down each second. Default 0.")
        ],
        "shape(3).scroll(0, 0, 0.1, 0.05).out()"
    ),
    method!(
        "scrollX",
        "geometry",
        "Move the picture across.",
        "Moves the picture across by scrollX and keeps it moving at speed. The picture wraps at the edges.",
        [
            param!("scrollX", "Shift across. Default 0.5."),
            param!("speed", "Movement each second. Default 0.")
        ],
        "osc(10).scrollX(0, 0.2).out()"
    ),
    method!(
        "scrollY",
        "geometry",
        "Move the picture down.",
        "Moves the picture down by scrollY and keeps it moving at speed. The picture wraps at the edges.",
        [
            param!("scrollY", "Shift down. Default 0.5."),
            param!("speed", "Movement each second. Default 0.")
        ],
        "osc(10).rotate(1.57).scrollY(0, 0.2).out()"
    ),
    // Colour: these change the colour of a chain.
    method!(
        "posterize",
        "colour",
        "Reduce the number of colours.",
        "Rounds each channel to a small number of steps. Gamma bends the steps before rounding.",
        [
            param!("bins", "Steps for each channel. Default 3."),
            param!("gamma", "Curve before rounding. Default 0.6.")
        ],
        "gradient(1).posterize(4, 0.5).out()"
    ),
    method!(
        "shift",
        "colour",
        "Shift each colour channel.",
        "Adds an amount to each channel and wraps the result from 0 to 1.",
        [
            param!("r", "Shift of red. Default 0.5."),
            param!("g", "Shift of green. Default 0."),
            param!("b", "Shift of blue. Default 0."),
            param!("a", "Shift of alpha. Default 0.")
        ],
        "osc(10).shift(0.1, 0.9, 0.3).out()"
    ),
    method!(
        "invert",
        "colour",
        "Invert the colours.",
        "Mixes each colour with its opposite. An amount of 1 inverts it fully.",
        [param!("amount", "How much to invert. Default 1.")],
        "shape(4).invert().out()"
    ),
    method!(
        "contrast",
        "colour",
        "Change the contrast.",
        "Values above 1 push colours away from grey. Values below 1 pull them towards grey.",
        [param!("amount", "Contrast. Default 1.6.")],
        "noise(4).contrast(2).out()"
    ),
    method!(
        "brightness",
        "colour",
        "Change the brightness.",
        "Adds the amount to each colour channel.",
        [param!("amount", "Brightness to add. Default 0.4.")],
        "osc(10).brightness(-0.3).out()"
    ),
    method!(
        "luma",
        "colour",
        "Keep only the bright parts.",
        "Makes the parts darker than the threshold transparent. Tolerance softens the border.",
        [
            param!("threshold", "Brightness from 0 to 1. Default 0.5."),
            param!("tolerance", "Softness of the border. Default 0.1.")
        ],
        "osc(10).luma(0.5, 0.05).out()"
    ),
    method!(
        "thresh",
        "colour",
        "Black and white at a threshold.",
        "Makes each pixel black or white, from its brightness. Tolerance softens the border.",
        [
            param!("threshold", "Brightness from 0 to 1. Default 0.5."),
            param!("tolerance", "Softness of the border. Default 0.04.")
        ],
        "noise(3).thresh(0.4).out()"
    ),
    method!(
        "color",
        "colour",
        "Multiply each colour channel.",
        "Multiplies red, green, blue and alpha by the values given.",
        [
            param!("r", "Red. Default 1."),
            param!("g", "Green. Default 1."),
            param!("b", "Blue. Default 1."),
            param!("a", "Alpha. Default 1.")
        ],
        "osc(10).color(1, 0.2, 0.6).out()"
    ),
    method!(
        "saturate",
        "colour",
        "Change the saturation.",
        "Values above 1 make colours stronger. 0 makes the picture grey.",
        [param!("amount", "Saturation. Default 2.")],
        "osc(10, 0.1, 1).saturate(3).out()"
    ),
    method!(
        "hue",
        "colour",
        "Turn the hue.",
        "Turns every colour around the colour wheel by the amount given, from 0 to 1.",
        [param!("hue", "Turn of the colour wheel. Default 0.4.")],
        "osc(10, 0.1, 1).hue(0.5).out()"
    ),
    method!(
        "colorama",
        "colour",
        "Cycle the colours.",
        "Turns the hue by an amount that grows with brightness. Small values give colour bands.",
        [param!("amount", "Strength of the effect. Default 0.005.")],
        "osc(10).colorama(0.3).out()"
    ),
    method!(
        "r",
        "colour",
        "Keep only the red channel.",
        "Shows the red channel as grey, multiplied by scale and moved by offset.",
        [
            param!("scale", "Multiplier. Default 1."),
            param!("offset", "Value to add. Default 0.")
        ],
        "osc(10, 0.1, 1).r().out()"
    ),
    method!(
        "g",
        "colour",
        "Keep only the green channel.",
        "Shows the green channel as grey, multiplied by scale and moved by offset.",
        [
            param!("scale", "Multiplier. Default 1."),
            param!("offset", "Value to add. Default 0.")
        ],
        "osc(10, 0.1, 1).g().out()"
    ),
    method!(
        "b",
        "colour",
        "Keep only the blue channel.",
        "Shows the blue channel as grey, multiplied by scale and moved by offset.",
        [
            param!("scale", "Multiplier. Default 1."),
            param!("offset", "Value to add. Default 0.")
        ],
        "osc(10, 0.1, 1).b().out()"
    ),
    method!(
        "a",
        "colour",
        "Keep only the alpha channel.",
        "Shows the alpha channel as grey, multiplied by scale and moved by offset. The global a is the audio analyser.",
        [
            param!("scale", "Multiplier. Default 1."),
            param!("offset", "Value to add. Default 0.")
        ],
        "shape(4).luma().a().out()"
    ),
    // Blends: these mix a second picture into the chain.
    method!(
        "add",
        "blend",
        "Add another picture.",
        "Adds the colours of the texture, multiplied by amount.",
        [
            TEXTURE,
            param!("amount", "Strength of the texture. Default 1.")
        ],
        "osc(10).add(shape(4), 0.5).out()"
    ),
    method!(
        "sub",
        "blend",
        "Subtract another picture.",
        "Subtracts the colours of the texture, multiplied by amount.",
        [
            TEXTURE,
            param!("amount", "Strength of the texture. Default 1.")
        ],
        "osc(10).sub(shape(4), 0.5).out()"
    ),
    method!(
        "mult",
        "blend",
        "Multiply by another picture.",
        "Multiplies by the colours of the texture. Amount mixes between no change and the full product.",
        [
            TEXTURE,
            param!("amount", "Strength of the texture. Default 1.")
        ],
        "osc(10).mult(shape(4)).out()"
    ),
    method!(
        "blend",
        "blend",
        "Mix with another picture.",
        "Mixes the chain and the texture. 0 keeps the chain and 1 shows only the texture.",
        [TEXTURE, param!("amount", "Mix. Default 0.5.")],
        "osc(10).blend(noise(3), 0.3).out()"
    ),
    method!(
        "diff",
        "blend",
        "Difference with another picture.",
        "Shows the absolute difference between the colours of the chain and the texture.",
        [TEXTURE],
        "osc(10).diff(osc(12).rotate(0.5)).out()"
    ),
    method!(
        "layer",
        "blend",
        "Draw another picture on top.",
        "Draws the texture over the chain, using the alpha of the texture.",
        [TEXTURE],
        "osc(10).layer(shape(4).luma()).out()"
    ),
    method!(
        "mask",
        "blend",
        "Cut the chain with another picture.",
        "Uses the brightness of the texture as the alpha of the chain. Dark parts become transparent.",
        [TEXTURE],
        "osc(10).mask(shape(4)).out()"
    ),
    // Modulators: these move the chain by the colours of another picture.
    method!(
        "modulate",
        "modulate",
        "Warp the chain with another picture.",
        "Moves each point of the chain by the red and green of the texture, multiplied by amount.",
        [
            TEXTURE,
            param!("amount", "Strength of the warp. Default 0.1.")
        ],
        "osc(10).modulate(noise(3), 0.2).out()"
    ),
    method!(
        "modulateScale",
        "modulate",
        "Zoom by another picture.",
        "Zooms each point by the red of the texture, multiplied by multiple, plus offset.",
        [
            TEXTURE,
            param!("multiple", "Strength of the zoom. Default 1."),
            param!("offset", "Zoom without the texture. Default 1.")
        ],
        "shape(4).modulateScale(osc(8), 0.5).out()"
    ),
    method!(
        "modulatePixelate",
        "modulate",
        "Pixelate by another picture.",
        "Sets the cell size of each point from the red of the texture, multiplied by multiple, plus offset.",
        [
            TEXTURE,
            param!("multiple", "Strength of the effect. Default 10."),
            param!("offset", "Cell count without the texture. Default 3.")
        ],
        "noise(3).modulatePixelate(osc(5), 20).out()"
    ),
    method!(
        "modulateRotate",
        "modulate",
        "Rotate by another picture.",
        "Turns each point by the red of the texture, multiplied by multiple, plus offset.",
        [
            TEXTURE,
            param!("multiple", "Strength of the rotation. Default 1."),
            param!("offset", "Rotation without the texture. Default 0.")
        ],
        "osc(10).modulateRotate(shape(3), 2).out()"
    ),
    method!(
        "modulateHue",
        "modulate",
        "Warp by the colour change of another picture.",
        "Moves each point by the difference between the green and red, and the blue and green, of the texture.",
        [
            TEXTURE,
            param!("amount", "Strength of the warp. Default 1.")
        ],
        "src(o0).modulateHue(src(o0), 2).blend(osc(4), 0.1).out()"
    ),
    method!(
        "modulateRepeat",
        "modulate",
        "Tile by another picture.",
        "Repeats the chain in a grid, with offsets read from the texture.",
        [
            TEXTURE,
            param!("repeatX", "Tiles across. Default 3."),
            param!("repeatY", "Tiles down. Default 3."),
            param!("offsetX", "Strength across. Default 0.5."),
            param!("offsetY", "Strength down. Default 0.5.")
        ],
        "shape(4).modulateRepeat(osc(10), 3, 3, 0.5, 0.5).out()"
    ),
    method!(
        "modulateRepeatX",
        "modulate",
        "Tile across by another picture.",
        "Repeats the chain across, with an offset read from the texture.",
        [
            TEXTURE,
            param!("reps", "Tiles across. Default 3."),
            param!("offset", "Strength of the texture. Default 0.5.")
        ],
        "shape(4).modulateRepeatX(osc(10), 4).out()"
    ),
    method!(
        "modulateRepeatY",
        "modulate",
        "Tile down by another picture.",
        "Repeats the chain down, with an offset read from the texture.",
        [
            TEXTURE,
            param!("reps", "Tiles down. Default 3."),
            param!("offset", "Strength of the texture. Default 0.5.")
        ],
        "shape(4).modulateRepeatY(osc(10), 4).out()"
    ),
    method!(
        "modulateKaleid",
        "modulate",
        "Kaleidoscope by another picture.",
        "Folds the chain into nSides slices, with the fold read from the texture.",
        [TEXTURE, param!("nSides", "Number of slices. Default 4.")],
        "osc(10).modulateKaleid(osc(12), 4).out()"
    ),
    method!(
        "modulateScrollX",
        "modulate",
        "Move across by another picture.",
        "Moves each point across by the red of the texture, multiplied by scrollX, plus speed each second.",
        [
            TEXTURE,
            param!("scrollX", "Strength of the movement. Default 0.5."),
            param!("speed", "Movement each second. Default 0.")
        ],
        "osc(10).modulateScrollX(osc(2), 0.5).out()"
    ),
    method!(
        "modulateScrollY",
        "modulate",
        "Move down by another picture.",
        "Moves each point down by the red of the texture, multiplied by scrollY, plus speed each second.",
        [
            TEXTURE,
            param!("scrollY", "Strength of the movement. Default 0.5."),
            param!("speed", "Movement each second. Default 0.")
        ],
        "osc(10).modulateScrollY(osc(2), 0.5).out()"
    ),
    // Drawing a chain.
    method!(
        "out",
        "outputs",
        "Draw the chain into an output.",
        "Draws the chain into o0, or into the output given. Nothing appears until a chain is drawn with out().",
        [param!(
            "output",
            "o0-o3",
            "The output to draw into. Default o0."
        )],
        "osc(10).out(o0)"
    ),
    // Filling an input.
    method!(
        "initCam",
        "inputs",
        "Fill an input from a camera.",
        "Opens a camera into this input. Leave out the index for the default camera. Hydra webcam in Settings must be allowed, and the sketch must be playing and visible.",
        [param!(
            "index",
            "number",
            "The camera to open, from 0. Default: the system default."
        )],
        "s0.initCam()\nsrc(s0).out()"
    ),
    method!(
        "initImage",
        "inputs",
        "Fill an input from an image.",
        "Loads an image into this input. Give a public HTTPS URL that allows CORS, or a path relative to the set folder, such as 'pics/cat.png'. Outside a set, a relative path reads the folder granted with --allow-local-samples. PNG, JPEG, GIF and WebP work. An animated file shows its first frame. Use initVideo() to play it.",
        [param!(
            "url",
            "string",
            "An HTTPS URL, or a relative path such as 'pics/cat.png'."
        )],
        "s0.initImage('https://upload.wikimedia.org/wikipedia/commons/2/26/YellowLabradorLooking_new.jpg')\nsrc(s0).out()"
    ),
    method!(
        "initVideo",
        "inputs",
        "Fill an input from a video.",
        "Plays a video into this input in a loop, without sound. Give a public HTTPS URL that allows CORS, or a path relative to the set folder, such as 'clips/loop.webm'. Outside a set, a relative path reads the folder granted with --allow-local-samples. WebM and MKV files with VP8 or VP9 video play, and so do animated GIF, PNG and WebP files. MP4 files, H.264, H.265 and AV1 do not. Convert those with: ffmpeg -i input.mp4 -c:v libvpx-vp9 -pix_fmt yuv420p -an output.webm",
        [param!(
            "url",
            "string",
            "An HTTPS URL, or a relative path such as 'clips/loop.webm'."
        )],
        "s0.initVideo('clips/loop.webm')\nsrc(s0).modulate(osc(4), 0.1).out()"
    ),
    method!(
        "clear",
        "inputs",
        "Empty an input.",
        "Stops the camera, image or video in this input and makes it transparent black.",
        [],
        "s0.clear()"
    ),
    // Array arguments: an array changes value over time.
    method!(
        "fast",
        "arrays",
        "Step through an array faster.",
        "An array argument steps to its next value at Hydra's bpm. fast(2) steps twice as often.",
        [param!("speed", "number", "Step rate. Default 1.")],
        "shape([3, 4, 5].fast(2)).out()"
    ),
    method!(
        "smooth",
        "arrays",
        "Glide between array values.",
        "Moves smoothly from one array value to the next instead of jumping.",
        [param!(
            "smooth",
            "number",
            "Length of the glide, from 0 to 1. Default 1."
        )],
        "shape(4, [0.2, 0.6].smooth()).out()"
    ),
    method!(
        "ease",
        "arrays",
        "Choose the curve of a glide.",
        "Sets the curve that smooth() uses, by name. Names: linear, easeInQuad, easeOutQuad, easeInOutQuad, easeInCubic, easeOutCubic, easeInOutCubic, easeInQuart, easeOutQuart, easeInOutQuart, easeInQuint, easeOutQuint, easeInOutQuint and sin.",
        [param!(
            "name",
            "string",
            "The curve, in single quotes. Default 'linear'."
        )],
        "shape(4, [0.2, 0.6].ease('easeInOutCubic')).out()"
    ),
    method!(
        "offset",
        "arrays",
        "Shift the steps of an array.",
        "Shifts the moment the array steps, as a fraction of one step.",
        [param!(
            "offset",
            "number",
            "Shift from 0 to 1. Default 0.5."
        )],
        "shape([3, 4].offset(0.25)).out()"
    ),
    method!(
        "fit",
        "arrays",
        "Map array values into a range.",
        "Maps the lowest value of the array to low and the highest to high.",
        [
            param!("low", "number", "New lowest value. Default 0."),
            param!("high", "number", "New highest value. Default 1.")
        ],
        "osc([0, 1, 2, 3].fit(5, 40)).out()"
    ),
    // Hydra names with no native form.
    unavailable!(
        "sum",
        "method",
        "Not supported.",
        "The upstream shader for sum() does not compile, so Rustel refuses it."
    ),
    unavailable!(
        "initScreen",
        "method",
        "Not supported.",
        "Rustel cannot capture the screen. Use initCam(), initImage() or initVideo()."
    ),
    unavailable!(
        "initStream",
        "method",
        "Not supported.",
        "Stream inputs need a browser peer connection, which Rustel does not have."
    ),
    unavailable!(
        "initCanvas",
        "method",
        "Not supported.",
        "Canvas inputs need a browser canvas, which Rustel does not have."
    ),
    unavailable!(
        "setResolution",
        "global",
        "Not supported yet.",
        "The studio sets the render size from the backdrop. A headless host can use initHydra({ width, height })."
    ),
    unavailable!(
        "update",
        "global",
        "Not supported.",
        "update runs JavaScript on every frame. Rustel records the sketch and runs no JavaScript while it draws."
    ),
    unavailable!(
        "afterUpdate",
        "global",
        "Not supported.",
        "afterUpdate runs JavaScript after every frame. Rustel records the sketch and runs no JavaScript while it draws."
    ),
    unavailable!(
        "mouse",
        "global",
        "Not supported yet.",
        "Hydra callbacks cannot read the pointer in Rustel yet."
    ),
    unavailable!(
        "setFunction",
        "global",
        "Not supported yet.",
        "Custom GLSL functions are refused."
    ),
];
