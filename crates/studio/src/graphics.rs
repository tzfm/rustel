//! How the studio draws pictures, by what the terminal can show.
//!
//! Every visualizer paints onto a [`Canvas`](super::visuals::Canvas) in
//! points; the *tier* decides what a point is. On any terminal a point is
//! half a cell (bars) or a Braille dot (lines). A terminal that draws the
//! Unicode sextant glyphs itself gets 2×3 points per cell. An internal pixel
//! tier can ship that canvas through the kitty graphics protocol. Automatic
//! chooses a responsive glyph tier; Advanced settings can override that choice.

use std::io::{self, Write};

use ratatui::layout::Rect;
use ratatui::style::Color;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tier {
    /// Half-cell blocks and Braille dots: every terminal.
    #[default]
    Cells,
    /// Sextant glyphs, 2×3 points per cell: terminals that render them
    /// without a font.
    Fine,
    /// Real pixels through the kitty graphics protocol.
    Pixels,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cells => "cells",
            Self::Fine => "fine",
            Self::Pixels => "pixels",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "cells" => Some(Self::Cells),
            "fine" => Some(Self::Fine),
            "pixels" => Some(Self::Pixels),
            _ => None,
        }
    }
}

/// The persisted choice, separate from the drawing tier actually available
/// in this terminal. Moving preferences to another terminal keeps them usable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RenderingMode {
    #[default]
    Automatic,
    Cells,
    Fine,
    Kitty,
}

impl RenderingMode {
    const MODES: [Self; 4] = [Self::Automatic, Self::Cells, Self::Fine, Self::Kitty];

    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Cells => "Cells (blocks/Braille)",
            Self::Fine => "Fine glyphs (sextants)",
            Self::Kitty => "Kitty pixels",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Cells => "cells",
            Self::Fine => "fine",
            Self::Kitty => "kitty",
        }
    }

    pub fn parse(text: &str) -> Self {
        Self::MODES
            .into_iter()
            .find(|mode| mode.key() == text)
            .unwrap_or_default()
    }

    pub fn supported(self, features: &super::terminal::TerminalFeatures) -> bool {
        match self {
            Self::Automatic | Self::Cells => true,
            Self::Fine => features.fine_glyphs,
            Self::Kitty => {
                features.kitty_graphics && features.cell_pixels.is_some_and(|(w, h)| w > 0 && h > 0)
            }
        }
    }

    pub fn resolve(self, features: &super::terminal::TerminalFeatures) -> Tier {
        if !self.supported(features) {
            return features.default_tier();
        }
        match self {
            Self::Automatic => features.default_tier(),
            Self::Cells => Tier::Cells,
            Self::Fine => Tier::Fine,
            Self::Kitty => Tier::Pixels,
        }
    }

    pub(super) fn step(self, forwards: bool, features: &super::terminal::TerminalFeatures) -> Self {
        let modes: Vec<Self> = Self::MODES
            .into_iter()
            .filter(|mode| mode.supported(features))
            .collect();
        let current = modes.iter().position(|mode| *mode == self).unwrap_or(0);
        modes[if forwards {
            (current + 1) % modes.len()
        } else {
            (current + modes.len() - 1) % modes.len()
        }]
    }
}

/// The pictures the terminal is holding, by the id each was sent under,
/// and how many frames have gone out since they were all sent afresh.
pub struct Sent {
    images: Vec<PixelImage>,
    frames: u32,
}

impl Sent {
    const fn new() -> Self {
        Self {
            images: Vec::new(),
            frames: 0,
        }
    }
}

/// What the renderer is drawing with: the tier, the terminal's cell size,
/// and the queue of pictures for the frame being drawn.
///
/// A running studio renders on one thread and keeps plain statics. Under
/// `cfg(test)` the same state is thread-local, so a test that sets the
/// Kitty tier cannot change the picture of a test on another thread.
#[cfg(not(test))]
mod state {
    use super::{PixelImage, Sent};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

    static TIER: AtomicU8 = AtomicU8::new(0);
    /// Cell size in pixels, `width << 16 | height`; zero means unknown.
    static CELL_PIXELS: AtomicU32 = AtomicU32::new(0);
    static PIXEL_BUDGET: AtomicU64 = AtomicU64::new(super::PIXEL_CEILING);
    static FRAME: Mutex<Vec<PixelImage>> = Mutex::new(Vec::new());
    static SENT: Mutex<Sent> = Mutex::new(Sent::new());

    pub fn tier() -> u8 {
        TIER.load(Ordering::Relaxed)
    }

    pub fn swap_tier(value: u8) -> u8 {
        TIER.swap(value, Ordering::Relaxed)
    }

    pub fn cell_pixels() -> u32 {
        CELL_PIXELS.load(Ordering::Relaxed)
    }

    pub fn set_cell_pixels(packed: u32) {
        CELL_PIXELS.store(packed, Ordering::Relaxed);
    }

    pub fn pixel_budget() -> u64 {
        PIXEL_BUDGET.load(Ordering::Relaxed)
    }

    pub fn set_pixel_budget(pixels: u64) {
        PIXEL_BUDGET.store(pixels, Ordering::Relaxed);
    }

    pub fn with_frame<T>(act: impl FnOnce(&mut Vec<PixelImage>) -> T) -> Option<T> {
        FRAME.lock().ok().map(|mut frame| act(&mut frame))
    }

    pub fn with_sent<T>(act: impl FnOnce(&mut Sent) -> T) -> Option<T> {
        SENT.lock().ok().map(|mut sent| act(&mut sent))
    }
}

#[cfg(test)]
mod state {
    use super::{PixelImage, Sent};
    use std::cell::{Cell, RefCell};

    thread_local! {
        static TIER: Cell<u8> = const { Cell::new(0) };
        static CELL_PIXELS: Cell<u32> = const { Cell::new(0) };
        static PIXEL_BUDGET: Cell<u64> = const { Cell::new(super::PIXEL_CEILING) };
        static FRAME: RefCell<Vec<PixelImage>> = const { RefCell::new(Vec::new()) };
        static SENT: RefCell<Sent> = const { RefCell::new(Sent::new()) };
    }

    pub fn tier() -> u8 {
        TIER.with(Cell::get)
    }

    pub fn swap_tier(value: u8) -> u8 {
        TIER.with(|tier| tier.replace(value))
    }

    pub fn cell_pixels() -> u32 {
        CELL_PIXELS.with(Cell::get)
    }

    pub fn set_cell_pixels(packed: u32) {
        CELL_PIXELS.with(|cells| cells.set(packed));
    }

    pub fn pixel_budget() -> u64 {
        PIXEL_BUDGET.with(Cell::get)
    }

    pub fn set_pixel_budget(pixels: u64) {
        PIXEL_BUDGET.with(|budget| budget.set(pixels));
    }

    pub fn with_frame<T>(act: impl FnOnce(&mut Vec<PixelImage>) -> T) -> Option<T> {
        Some(FRAME.with_borrow_mut(act))
    }

    pub fn with_sent<T>(act: impl FnOnce(&mut Sent) -> T) -> Option<T> {
        Some(SENT.with_borrow_mut(act))
    }
}

pub fn tier() -> Tier {
    match state::tier() {
        1 => Tier::Fine,
        2 => Tier::Pixels,
        _ => Tier::Cells,
    }
}

pub fn set_tier(tier: Tier) {
    let value = match tier {
        Tier::Cells => 0,
        Tier::Fine => 1,
        Tier::Pixels => 2,
    };
    if state::swap_tier(value) != value {
        // A frame queued with the old raster must not be placed after the
        // setting changes. The terminal clears old Kitty placements next frame.
        let _ = take_images();
        forget_sent_images();
    }
}

pub fn cell_pixels() -> Option<(u16, u16)> {
    let packed = state::cell_pixels();
    let (width, height) = ((packed >> 16) as u16, (packed & 0xffff) as u16);
    (width > 0 && height > 0).then_some((width, height))
}

pub fn set_cell_pixels(size: Option<(u16, u16)>) {
    state::set_cell_pixels(
        size.map(|(w, h)| (u32::from(w) << 16) | u32::from(h))
            .unwrap_or(0),
    );
}

/// The most a picture is ever asked for, however fast the terminal.
///
/// Past this it is finer than the glass can show it, and every pixel still
/// has to be composited, compressed and pushed through the terminal's pipe.
pub const PIXEL_CEILING: u64 = 1_700_000;

/// The floor the measurement may drive the pictures down to: coarse, but
/// still a picture rather than a mosaic.
pub const PIXEL_FLOOR: u64 = 350_000;

/// Pixels a whole backdrop may cost, as the renderer has measured this
/// terminal. See `App::watch_pixel_frame`, which is what moves it.
pub fn pixel_budget() -> u64 {
    state::pixel_budget()
}

pub fn set_pixel_budget(pixels: u64) {
    state::set_pixel_budget(pixels.clamp(PIXEL_FLOOR, PIXEL_CEILING));
}

/// The most one canvas may cost. A frame holds several - a minimap, the
/// docked widgets, an inline visualizer or two - so each gets a share of
/// what the terminal was found to carry, never the whole of it.
pub fn canvas_budget() -> u64 {
    (pixel_budget() / 4).clamp(80_000, 600_000)
}

/// One picture for the frame being drawn: where it goes, in cells, and its
/// pixels, row-major RGBA, transparent where the canvas was empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelImage {
    pub area: Rect,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Where this sits relative to the text. Zero - the default, and what
    /// every inline visualizer wants - draws over it. Negative draws under it,
    /// which is how a full-screen picture becomes a backdrop for the code
    /// rather than a curtain across it.
    pub depth: i32,
    /// Cells to scale the picture into. `None` places it at its own size, one
    /// image pixel to one screen pixel. A backdrop names the whole grid, so a
    /// modest render fills the screen without sending a screen's worth of
    /// pixels every frame.
    pub cells: Option<(u16, u16)>,
}

impl PixelImage {
    /// A picture at its natural size, over the text: what an inline
    /// visualizer paints.
    pub fn inline(area: Rect, width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            area,
            width,
            height,
            rgba,
            depth: 0,
            cells: None,
        }
    }

    /// Copy a visible window from a scratch canvas to its screen position.
    /// `destination` is the screen origin of `source`, not of the image:
    /// an image beginning inside the window retains that offset. Native
    /// canvases have integral pixels per cell; scaled images keep covering
    /// boundary pixels and their explicit cell scaling.
    pub fn crop_and_move(mut self, source: Rect, destination: (u16, u16)) -> Option<Self> {
        let coverage = self.coverage()?;
        let visible = coverage.intersection(source);
        if visible.is_empty() {
            return None;
        }
        let x = destination.0.checked_add(visible.x - source.x)?;
        let y = destination.1.checked_add(visible.y - source.y)?;
        x.checked_add(visible.width)?;
        y.checked_add(visible.height)?;

        if visible != coverage {
            let (x0, y0, x1, y1) = self.pixel_bounds(coverage, visible);
            let row_bytes = (x1 - x0) * 4;
            let mut rgba = Vec::with_capacity(row_bytes * (y1 - y0));
            for row in y0..y1 {
                let start = (row * self.width as usize + x0) * 4;
                rgba.extend_from_slice(&self.rgba[start..start + row_bytes]);
            }
            self.width = (x1 - x0) as u32;
            self.height = (y1 - y0) as u32;
            self.rgba = rgba;
        }
        self.area = Rect::new(x, y, visible.width, visible.height);
        self.cells = self.cells.map(|_| (visible.width, visible.height));
        Some(self)
    }

    fn coverage(&self) -> Option<Rect> {
        let (columns, rows) = self.cells.unwrap_or((self.area.width, self.area.height));
        if columns == 0 || rows == 0 || self.width == 0 || self.height == 0 {
            return None;
        }
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(4)?;
        if self.rgba.len() != expected {
            return None;
        }
        self.area.x.checked_add(columns)?;
        self.area.y.checked_add(rows)?;
        Some(Rect::new(self.area.x, self.area.y, columns, rows))
    }

    /// Both rectangles are validated cell coverage; include partially covered
    /// source pixels when a scaled image has fractional pixels per cell.
    fn pixel_bounds(&self, coverage: Rect, visible: Rect) -> (usize, usize, usize, usize) {
        let start = |offset: u16, pixels: u32, cells: u16| {
            (u64::from(offset) * u64::from(pixels) / u64::from(cells)) as usize
        };
        let end = |offset: u16, pixels: u32, cells: u16| {
            (u64::from(offset) * u64::from(pixels)).div_ceil(u64::from(cells)) as usize
        };
        (
            start(visible.x - coverage.x, self.width, coverage.width),
            start(visible.y - coverage.y, self.height, coverage.height),
            end(visible.right() - coverage.x, self.width, coverage.width),
            end(visible.bottom() - coverage.y, self.height, coverage.height),
        )
    }
}

/// Capture only images produced while drawing one scratch surface. Earlier
/// layers remain queued in their original order. Like the frame painter,
/// this scope belongs to the single UI render thread; it may nest.
pub fn capture_images<T>(draw: impl FnOnce() -> T) -> (T, Vec<PixelImage>) {
    struct Restore(Vec<PixelImage>);
    impl Drop for Restore {
        fn drop(&mut self) {
            // On unwind, discard unfinished scratch-local placements.
            let held = std::mem::take(&mut self.0);
            state::with_frame(|frame| *frame = held);
        }
    }
    let restore = Restore(take_images());
    let result = draw();
    let images = take_images();
    drop(restore);
    (result, images)
}

/// What the renderer was set to, put back when this goes.
///
/// A test that draws pixels has to set the tier and the cell size, and
/// those belong to the thread it runs on. Restoring them by hand at the
/// end of the test only restores them when the test PASSES - one failed
/// assertion and every test after it on that thread is drawing pixels
/// too, which buries the one real failure under a pile of invented ones.
#[cfg(test)]
pub struct HeldRenderer {
    tier: Tier,
    cells: Option<(u16, u16)>,
    budget: u64,
    images: Vec<PixelImage>,
}

#[cfg(test)]
impl HeldRenderer {
    /// Hold what the renderer is set to, and set it to draw pixels at
    /// `cells` pixels to a cell.
    pub fn pixels(cells: (u16, u16)) -> Self {
        forget_sent_images();
        let held = Self {
            tier: tier(),
            cells: cell_pixels(),
            budget: pixel_budget(),
            images: take_images(),
        };
        set_cell_pixels(Some(cells));
        set_tier(Tier::Pixels);
        held
    }
}

#[cfg(test)]
impl Drop for HeldRenderer {
    fn drop(&mut self) {
        let _ = take_images();
        forget_sent_images();
        set_tier(self.tier);
        set_cell_pixels(self.cells);
        set_pixel_budget(self.budget);
        for image in std::mem::take(&mut self.images) {
            push_image(image);
        }
    }
}

/// A canvas painted as pixels hands its image here; the frame collects it.
pub fn push_image(image: PixelImage) {
    state::with_frame(|frame| frame.push(image));
}

/// A text overlay covers only images painted before it. Clear their covered
/// pixels without changing placement, scaling, depth, or the order of layers;
/// images painted inside the overlay afterward remain visible.
pub fn cover_images(area: Rect) {
    if area.is_empty() {
        return;
    }
    state::with_frame(|frame| {
        frame.retain_mut(|image| {
            let Some(coverage) = image.coverage() else {
                return false;
            };
            let covered = coverage.intersection(area);
            if covered.is_empty() {
                return true;
            }
            if covered == coverage {
                return false;
            }
            let (x0, y0, x1, y1) = image.pixel_bounds(coverage, covered);
            for row in y0..y1 {
                for column in x0..x1 {
                    image.rgba[(row * image.width as usize + column) * 4 + 3] = 0;
                }
            }
            true
        });
    });
}

/// Everything painted since the last take, in paint order.
pub fn take_images() -> Vec<PixelImage> {
    state::with_frame(std::mem::take).unwrap_or_default()
}

/// A colour as the pixels need it. Named and indexed colours go through
/// the xterm palette, which is what a terminal would have shown for them.
pub fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => (0, 0, 0),
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::Gray => (229, 229, 229),
        Color::DarkGray => (102, 102, 102),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::White => (255, 255, 255),
        Color::Indexed(index) => indexed_rgb(index),
    }
}

fn indexed_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => rgb([
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Gray,
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
        ][usize::from(index)]),
        16..=231 => {
            let value = index - 16;
            let level = |component: u8| {
                if component == 0 {
                    0
                } else {
                    55 + component * 40
                }
            };
            (level(value / 36), level((value / 6) % 6), level(value % 6))
        }
        232..=255 => {
            let grey = 8 + (index - 232) * 10;
            (grey, grey, grey)
        }
    }
}

/// Forget what the terminal is believed to be holding, so the next frame
/// sends every picture again. The tier changing, or the window.
pub fn forget_sent_images() {
    state::with_sent(|sent| {
        sent.images.clear();
        sent.frames = 0;
    });
}

/// What the pictures on this side take: the frame being drawn and the
/// copy kept of every picture the terminal holds. Counted by the room
/// each buffer has, because a kept copy is refilled in place and keeps
/// the largest picture it has held.
pub fn held_bytes() -> usize {
    let room = |images: &[PixelImage]| {
        images
            .iter()
            .map(|image| image.rgba.capacity())
            .sum::<usize>()
    };
    state::with_frame(|frame| room(frame)).unwrap_or(0)
        + state::with_sent(|sent| room(&sent.images)).unwrap_or(0)
}

/// How often every picture goes out again whatever this side believes the
/// terminal is holding.
///
/// `q=2` asks the terminal not to answer, and nothing here reads its
/// replies, so if it drops a picture under memory pressure this side would
/// never learn. Sending the lot every few seconds is the cheap way to be
/// wrong for at most that long.
const REFRESH_FRAMES: u32 = 240;

/// Whether two pictures are the same pixels - where they go is placement,
/// which is sent every frame regardless and costs nothing.
fn same_pixels(held: &PixelImage, image: &PixelImage) -> bool {
    held.width == image.width && held.height == image.height && held.rgba == image.rgba
}

/// Ship a frame's pictures as kitty graphics: every placement of the
/// previous frame is dropped, then each image is placed at its cells with
/// the cursor left where it was. Meant to run inside the terminal's
/// synchronized update, after the text of the frame.
///
/// A picture whose pixels have not changed since the last frame is only
/// placed again, never sent again: the delete is the lowercase one, which
/// drops the placements and leaves the pictures themselves with the
/// terminal. Compressing and encoding is most of what a pixel frame costs,
/// and the docked widgets, the minimap and a paused visualizer are the
/// same pixels frame after frame - only a running sketch really changes.
pub fn write_kitty_frame(images: &[PixelImage], out: &mut impl Write) -> io::Result<()> {
    // Nothing to draw: free the lot rather than hold pictures for a screen
    // that has none.
    if images.is_empty() {
        forget_sent_images();
        return out.write_all(b"\x1b_Ga=d,d=A,q=2\x1b\\");
    }
    let (held, refresh) = state::with_sent(|sent| {
        let refresh = sent.frames % REFRESH_FRAMES == 0;
        sent.frames = sent.frames.wrapping_add(1);
        (std::mem::take(&mut sent.images), refresh)
    })
    .unwrap_or((Vec::new(), true));
    let held: &[PixelImage] = if refresh { &[] } else { &held };
    // Drop every placement. Lowercase leaves the pictures themselves with
    // the terminal, which is what lets an unchanged one be put back without
    // being sent again; on a refresh frame they go too.
    out.write_all(if refresh {
        b"\x1b_Ga=d,d=A,q=2\x1b\\".as_slice()
    } else {
        b"\x1b_Ga=d,d=a,q=2\x1b\\".as_slice()
    })?;
    // Pictures the terminal still holds that this frame has no use for.
    for id in images.len()..held.len() {
        write!(out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", id + 1)?;
    }
    // DECSC/DECRC: the caret goes back where the frame put it.
    out.write_all(b"\x1b7")?;
    for (index, image) in images.iter().enumerate() {
        if image.width == 0 || image.height == 0 {
            continue;
        }
        write!(out, "\x1b[{};{}H", image.area.y + 1, image.area.x + 1)?;
        let id = index as u32 + 1;
        if held.get(index).is_some_and(|held| same_pixels(held, image)) {
            // The terminal has these pixels: this says only where they go.
            write!(out, "\x1b_Ga=p,i={id},q=2,C=1")?;
            if image.depth != 0 {
                write!(out, ",z={}", image.depth)?;
            }
            if let Some((columns, rows)) = image.cells {
                write!(out, ",c={columns},r={rows}")?;
            }
            out.write_all(b"\x1b\\")?;
            continue;
        }
        // A backdrop is a big picture that changes every frame, so it is
        // compressed for speed rather than for size; an inline visualizer is
        // small and can afford to be squeezed.
        let level = if image.depth < 0 { 1 } else { 4 };
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&image.rgba, level);
        let payload = base64_bytes(&compressed);
        let mut chunks = payload.chunks(4096).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more = if chunks.peek().is_some() { 1 } else { 0 };
            if first {
                // `z` places the picture relative to the text: kitty draws a
                // negative z under the glyphs and over the cell background,
                // which is a backdrop. `c`/`r` scale it into that many cells,
                // so the picture sent need not be the size of the screen.
                write!(
                    out,
                    "\x1b_Ga=T,f=32,o=z,s={},v={},i={id},q=2,C=1",
                    image.width, image.height
                )?;
                if image.depth != 0 {
                    write!(out, ",z={}", image.depth)?;
                }
                if let Some((columns, rows)) = image.cells {
                    write!(out, ",c={columns},r={rows}")?;
                }
                write!(out, ",m={more};")?;
                first = false;
            } else {
                write!(out, "\x1b_Gm={more};")?;
            }
            out.write_all(chunk)?;
            out.write_all(b"\x1b\\")?;
        }
    }
    out.write_all(b"\x1b8")?;
    // What the terminal is now holding. The pixels are copied into the
    // buffers already there rather than into fresh ones: a screen-sized
    // backdrop is megabytes, and this runs every frame.
    state::with_sent(|sent| {
        sent.images.truncate(images.len());
        for (slot, image) in images.iter().enumerate() {
            match sent.images.get_mut(slot) {
                Some(held) => held.clone_from(image),
                None => sent.images.push(image.clone()),
            }
        }
    });
    Ok(())
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The payload a picture is sent as. Bytes rather than text: this runs
/// over megabytes every frame, and the whole three-byte group is written
/// at once instead of a character at a time.
fn base64_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    let (groups, rest) = bytes.as_chunks::<3>();
    for group in groups {
        let triple = (u32::from(group[0]) << 16) | (u32::from(group[1]) << 8) | u32::from(group[2]);
        out.extend_from_slice(&[
            BASE64[(triple >> 18) as usize & 63],
            BASE64[(triple >> 12) as usize & 63],
            BASE64[(triple >> 6) as usize & 63],
            BASE64[triple as usize & 63],
        ]);
    }
    if !rest.is_empty() {
        let triple = (u32::from(rest[0]) << 16) | (u32::from(*rest.get(1).unwrap_or(&0)) << 8);
        out.extend_from_slice(&[
            BASE64[(triple >> 18) as usize & 63],
            BASE64[(triple >> 12) as usize & 63],
            if rest.len() > 1 {
                BASE64[(triple >> 6) as usize & 63]
            } else {
                b'='
            },
            b'=',
        ]);
    }
    out
}

pub fn base64(bytes: &[u8]) -> String {
    String::from_utf8(base64_bytes(bytes)).unwrap_or_default()
}

/// The frame just drawn, as the visuals window will paint it.
///
/// The terminal is already a grid of styled cells; the window has real fonts
/// and a canvas. Sending the cells rather than pixels means the syntax
/// highlighting, the theme and the Braille scopes that reach a sketch's `s0`
/// are the ones the performer is looking at, produced once, by this renderer.
#[cfg(feature = "hydra")]
pub fn tui_frame(buffer: &ratatui::buffer::Buffer) -> rustel_hydra::HydraTuiFrame {
    let area = buffer.area();
    let (cell_width, cell_height) = cell_pixels().unwrap_or((0, 0));
    let mut cells = Vec::with_capacity(usize::from(area.width) * usize::from(area.height));
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let Some(cell) = buffer.cell((x, y)) else {
                cells.push(rustel_hydra::HydraTuiCell::default());
                continue;
            };
            let (fg, bg) = if cell.modifier.contains(ratatui::style::Modifier::REVERSED) {
                (rgb(cell.bg), rgb(cell.fg))
            } else {
                (rgb(cell.fg), rgb(cell.bg))
            };
            let mut flags = 0_u8;
            for (bit, modifier) in [
                ratatui::style::Modifier::BOLD,
                ratatui::style::Modifier::ITALIC,
                ratatui::style::Modifier::UNDERLINED,
                ratatui::style::Modifier::REVERSED,
                ratatui::style::Modifier::DIM,
            ]
            .into_iter()
            .enumerate()
            {
                if cell.modifier.contains(modifier) {
                    flags |= 1 << bit;
                }
            }
            cells.push(rustel_hydra::HydraTuiCell {
                // A cell's symbol is a grapheme cluster; the window draws its
                // first scalar, which is the whole of it for every glyph a
                // terminal grid actually holds.
                code: cell.symbol().chars().next().map_or(0, u32::from),
                fg: [fg.0, fg.1, fg.2],
                bg: [bg.0, bg.1, bg.2],
                flags,
            });
        }
    }
    rustel_hydra::HydraTuiFrame {
        cols: area.width,
        rows: area.height,
        cell_width,
        cell_height,
        cells,
    }
}

#[cfg(all(test, feature = "hydra"))]
mod tui_frame_tests {
    /// The terminal frame, as the window will paint it.
    ///
    /// This is the whole of `feedStrudel` on this side: the cells that went to the
    /// screen, with their colours, on their way to becoming a texture. The studio
    /// loop that calls it needs a real terminal and the binary's own allocator, so
    /// the conversion is what gets tested and the loop is what gets run by hand.
    #[test]
    fn the_terminal_frame_crosses_as_cells_and_colours() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::{Color, Modifier, Style};

        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 2));
        buffer.set_string(0, 0, "s(\"", Style::default().fg(Color::Rgb(255, 202, 40)));
        buffer.set_string(
            0,
            1,
            "\u{28ff}b\u{2588}",
            Style::default()
                .fg(Color::Rgb(85, 214, 232))
                .bg(Color::Rgb(20, 20, 24))
                .add_modifier(Modifier::BOLD),
        );

        let frame = super::tui_frame(&buffer);
        assert_eq!((frame.cols, frame.rows), (3, 2));
        assert_eq!(frame.cells.len(), 6);

        // Row one keeps the theme's syntax colour.
        assert_eq!(frame.cells[0].code, u32::from('s'));
        assert_eq!(frame.cells[0].fg, [255, 202, 40]);

        // Row two keeps a Braille glyph, its background, and its bold bit - so a
        // scope drawn in the terminal is a scope in the texture.
        assert_eq!(frame.cells[3].code, 0x28ff);
        assert_eq!(frame.cells[3].fg, [85, 214, 232]);
        assert_eq!(frame.cells[3].bg, [20, 20, 24]);
        assert_eq!(frame.cells[3].flags & 1, 1, "bold");
    }

    /// Reversed cells arrive already swapped, because the window paints a
    /// foreground and a background and has no notion of "reversed".
    #[test]
    fn a_reversed_cell_arrives_with_its_colours_swapped() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::{Color, Modifier, Style};

        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        buffer.set_string(
            0,
            0,
            "x",
            Style::default()
                .fg(Color::Rgb(1, 2, 3))
                .bg(Color::Rgb(9, 8, 7))
                .add_modifier(Modifier::REVERSED),
        );
        let frame = super::tui_frame(&buffer);
        assert_eq!(frame.cells[0].fg, [9, 8, 7]);
        assert_eq!(frame.cells[0].bg, [1, 2, 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinate_image(area: Rect, width: u32, height: u32) -> PixelImage {
        let rgba = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [x as u8, y as u8, 17, 255]))
            .collect();
        PixelImage::inline(area, width, height, rgba)
    }

    #[test]
    fn a_scratch_image_crops_both_axes_and_moves_to_the_visible_screen_window() {
        let mut image = coordinate_image(Rect::new(1, 2, 4, 3), 8, 9);
        image.depth = -2;
        let cropped = image
            .crop_and_move(Rect::new(2, 3, 2, 1), (30, 40))
            .unwrap();
        assert_eq!(cropped.area, Rect::new(30, 40, 2, 1));
        assert_eq!((cropped.width, cropped.height), (4, 3));
        assert_eq!(cropped.cells, None);
        assert_eq!(cropped.depth, -2);
        let expected: Vec<u8> = (3..6)
            .flat_map(|y| (2..6).flat_map(move |x| [x, y, 17, 255]))
            .collect();
        assert_eq!(cropped.rgba, expected);
        let mut output = Vec::new();
        write_kitty_frame(&[cropped], &mut output).unwrap();
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("\x1b[41;31H"), "{output:?}");
        assert!(output.contains("s=4,v=3,"), "{output:?}");
        assert!(output.contains("z=-2,"), "{output:?}");
    }

    #[test]
    fn a_visible_scratch_image_keeps_its_offset_without_copying_pixels() {
        let image = coordinate_image(Rect::new(3, 4, 2, 1), 4, 3);
        let pixels = image.rgba.as_ptr();
        let moved = image
            .crop_and_move(Rect::new(1, 1, 10, 10), (20, 30))
            .unwrap();
        assert_eq!(moved.area, Rect::new(22, 33, 2, 1));
        assert_eq!(moved.rgba.as_ptr(), pixels);
    }

    #[test]
    fn cropping_a_scaled_image_uses_its_cell_coverage_and_keeps_boundary_pixels() {
        let mut image = coordinate_image(Rect::new(2, 2, 1, 1), 5, 5);
        image.cells = Some((4, 3));
        let cropped = image.crop_and_move(Rect::new(3, 3, 2, 1), (7, 9)).unwrap();
        assert_eq!(cropped.area, Rect::new(7, 9, 2, 1));
        assert_eq!(cropped.cells, Some((2, 1)));
        assert_eq!((cropped.width, cropped.height), (3, 3));
        let expected: Vec<u8> = (1..4)
            .flat_map(|y| (1..4).flat_map(move |x| [x, y, 17, 255]))
            .collect();
        assert_eq!(cropped.rgba, expected);
    }

    #[test]
    fn invisible_or_invalid_scratch_images_are_not_queued() {
        let image = coordinate_image(Rect::new(2, 2, 2, 2), 4, 4);
        assert!(
            image
                .clone()
                .crop_and_move(Rect::new(0, 0, 2, 2), (0, 0))
                .is_none()
        );
        assert!(
            image
                .clone()
                .crop_and_move(image.area, (u16::MAX, 0))
                .is_none()
        );
        let mut invalid = image.clone();
        invalid.rgba.pop();
        assert!(invalid.crop_and_move(image.area, (0, 0)).is_none());
        let mut invalid = image.clone();
        invalid.cells = Some((0, 2));
        assert!(invalid.crop_and_move(image.area, (0, 0)).is_none());
    }

    #[test]
    fn scratch_capture_preserves_earlier_layers_and_nested_draw_order() {
        let layer = |x| coordinate_image(Rect::new(x, 0, 1, 1), 1, 1);
        let (_, frame) = capture_images(|| {
            push_image(layer(1));
            let (result, scratch) = capture_images(|| {
                push_image(layer(2));
                let (_, nested) = capture_images(|| push_image(layer(3)));
                assert_eq!(nested.len(), 1);
                assert_eq!(nested[0].area.x, 3);
                for image in nested {
                    push_image(image);
                }
                push_image(layer(4));
                42
            });
            assert_eq!(result, 42);
            assert_eq!(
                scratch.iter().map(|image| image.area.x).collect::<Vec<_>>(),
                vec![2, 3, 4]
            );
            push_image(layer(5));
        });
        assert_eq!(
            frame.iter().map(|image| image.area.x).collect::<Vec<_>>(),
            vec![1, 5]
        );
    }

    #[test]
    fn an_unfinished_scratch_draw_restores_the_earlier_frame() {
        let (_, frame) = capture_images(|| {
            push_image(coordinate_image(Rect::new(1, 0, 1, 1), 1, 1));
            let result = std::panic::catch_unwind(|| {
                capture_images(|| {
                    push_image(coordinate_image(Rect::new(2, 0, 1, 1), 1, 1));
                    panic!("unfinished scratch draw");
                });
            });
            assert!(result.is_err());
            push_image(coordinate_image(Rect::new(3, 0, 1, 1), 1, 1));
        });
        assert_eq!(
            frame.iter().map(|image| image.area.x).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn a_text_overlay_masks_only_earlier_covered_pixels_and_preserves_layer_order() {
        let outside = coordinate_image(Rect::new(20, 20, 1, 1), 2, 2);
        let partial = coordinate_image(Rect::new(5, 6, 4, 3), 8, 6);
        let inside = coordinate_image(Rect::new(6, 7, 1, 1), 2, 2);
        let (_, frame) = capture_images(|| {
            push_image(outside.clone());
            push_image(partial.clone());
            push_image(inside.clone());
            cover_images(Rect::new(6, 7, 2, 1));
            push_image(inside.clone());
        });
        assert_eq!(frame.len(), 3, "the fully covered earlier layer is removed");
        assert_eq!(frame[0], outside);
        assert_eq!(frame[2], inside, "a later overlay image remains visible");
        let masked = &frame[1];
        assert_eq!(masked.area, partial.area);
        assert_eq!((masked.width, masked.height), (8, 6));
        for (index, pixel) in masked.rgba.as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = (index % 8, index / 8);
            let covered = (2..6).contains(&x) && (2..4).contains(&y);
            assert_eq!(
                *pixel,
                [x as u8, y as u8, 17, if covered { 0 } else { 255 }]
            );
        }
    }

    #[test]
    fn an_overlay_preserves_scaled_image_placement_and_masks_cell_boundaries() {
        let mut image = coordinate_image(Rect::new(2, 2, 1, 1), 5, 5);
        image.cells = Some((4, 3));
        image.depth = -1;
        let (_, frame) = capture_images(|| {
            push_image(image.clone());
            cover_images(Rect::new(3, 3, 2, 1));
        });
        let masked = &frame[0];
        assert_eq!(masked.area, image.area);
        assert_eq!(masked.cells, image.cells);
        assert_eq!(masked.depth, image.depth);
        assert_eq!((masked.width, masked.height), (5, 5));
        for (index, pixel) in masked.rgba.as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = (index % 5, index / 5);
            let covered = (1..4).contains(&x) && (1..4).contains(&y);
            assert_eq!(pixel[3], if covered { 0 } else { 255 });
        }
    }

    #[test]
    fn rendering_override_defaults_to_glyphs_and_rechecks_capabilities() {
        let features = super::super::terminal::TerminalFeatures {
            fine_glyphs: true,
            kitty_graphics: true,
            cell_pixels: Some((10, 20)),
            ..Default::default()
        };
        assert_eq!(RenderingMode::Automatic.resolve(&features), Tier::Fine);
        assert_eq!(RenderingMode::Cells.resolve(&features), Tier::Cells);
        assert_eq!(RenderingMode::Fine.resolve(&features), Tier::Fine);
        assert_eq!(RenderingMode::Kitty.resolve(&features), Tier::Pixels);
        assert_eq!(
            RenderingMode::Kitty.resolve(&super::super::terminal::TerminalFeatures {
                cell_pixels: None,
                ..features.clone()
            }),
            Tier::Fine
        );
        assert_eq!(
            RenderingMode::Fine.resolve(&super::super::terminal::TerminalFeatures::default()),
            Tier::Cells
        );
        assert_eq!(RenderingMode::parse("unknown"), RenderingMode::Automatic);
    }

    #[test]
    fn changing_rendering_tiers_drops_queued_images_and_clears_previous_placements() {
        let original = tier();
        set_tier(Tier::Pixels);
        push_image(PixelImage::inline(
            Rect::new(0, 0, 1, 1),
            1,
            1,
            vec![255; 4],
        ));
        set_tier(Tier::Cells);
        assert!(
            take_images().is_empty(),
            "a pixel frame cannot survive a switch to cells"
        );
        let mut output = Vec::new();
        write_kitty_frame(&take_images(), &mut output).unwrap();
        assert_eq!(
            output, b"\x1b_Ga=d,d=A,q=2\x1b\\",
            "the next frame removes old Kitty images"
        );
        set_tier(original);
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_kitty_frame_deletes_places_and_restores_the_cursor() {
        let image = PixelImage::inline(Rect::new(4, 2, 2, 1), 4, 2, vec![255; 4 * 2 * 4]);
        let mut out = Vec::new();
        write_kitty_frame(&[image], &mut out).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with(
                "\x1b_Ga=d,d=A,q=2\x1b\\\x1b7\x1b[3;5H\x1b_Ga=T,f=32,o=z,s=4,v=2,i=1,q=2,C=1,m=0;"
            ),
            "{text:?}"
        );
        assert!(text.ends_with("\x1b\\\x1b8"), "{text:?}");
    }

    /// What the memory breakdown counts as pictures on this side: the frame
    /// being queued, and the copy kept of what the terminal was sent,
    /// which outlives the frame until the terminal is told to forget.
    #[test]
    fn held_bytes_counts_the_queued_frame_and_the_sent_copies() {
        forget_sent_images();
        let _ = take_images();
        assert_eq!(held_bytes(), 0);

        push_image(PixelImage::inline(
            Rect::new(0, 0, 2, 1),
            4,
            2,
            vec![255; 4 * 2 * 4],
        ));
        assert!(held_bytes() >= 32, "the queued frame");
        write_kitty_frame(&take_images(), &mut Vec::new()).unwrap();
        assert!(held_bytes() >= 32, "the copy of what was sent");

        forget_sent_images();
        assert_eq!(held_bytes(), 0);
    }

    /// A backdrop names its depth and the cells it fills; an inline picture
    /// names neither, so the sequence it sends is byte for byte the one it
    /// sent before this existed.
    #[test]
    fn a_backdrop_is_placed_under_the_text_and_scaled_to_the_grid() {
        forget_sent_images();
        let backdrop = PixelImage {
            area: Rect::new(0, 0, 80, 24),
            width: 8,
            height: 4,
            rgba: vec![9; 8 * 4 * 4],
            depth: -1,
            cells: Some((80, 24)),
        };
        let mut out = Vec::new();
        write_kitty_frame(&[backdrop], &mut out).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("\x1b_Ga=T,f=32,o=z,s=8,v=4,i=1,q=2,C=1,z=-1,c=80,r=24,m=0;"),
            "{text:?}"
        );
        // Placed at the top-left cell, which is where a backdrop starts.
        assert!(text.contains("\x1b[1;1H"), "{text:?}");
        let mut none = Vec::new();
        write_kitty_frame(&[], &mut none).unwrap();
        assert_eq!(none, b"\x1b_Ga=d,d=A,q=2\x1b\\");
    }

    /// A picture whose pixels did not change is placed again by its id and
    /// not sent again, also when it moves. The delete is the lowercase
    /// one, so the pictures stay. At intervals all pictures go out again
    /// in case the terminal dropped one.
    #[test]
    fn an_unchanged_picture_is_placed_again_rather_than_sent_again() {
        forget_sent_images();
        let widget = |at: Rect| PixelImage {
            area: at,
            width: 4,
            height: 2,
            rgba: vec![7; 4 * 2 * 4],
            depth: 0,
            cells: Some((at.width, at.height)),
        };
        let at = Rect::new(3, 5, 2, 1);
        let mut first = Vec::new();
        write_kitty_frame(&[widget(at)], &mut first).unwrap();
        let first = String::from_utf8_lossy(&first).into_owned();
        assert!(
            first.contains("d=A"),
            "the first frame starts clean: {first:?}"
        );
        assert!(
            first.contains("a=T,f=32"),
            "and sends the pixels: {first:?}"
        );

        let mut again = Vec::new();
        write_kitty_frame(&[widget(at)], &mut again).unwrap();
        let again = String::from_utf8_lossy(&again).into_owned();
        assert!(
            again.contains("\x1b_Ga=d,d=a,q=2"),
            "placements go, pictures stay: {again:?}"
        );
        assert!(!again.contains("a=T"), "nothing is sent twice: {again:?}");
        assert!(
            again.contains("\x1b_Ga=p,i=1,q=2,C=1,c=2,r=1\x1b\\"),
            "it is put back by its number: {again:?}"
        );

        // Moved, same pixels: still only a placement, at the new cell.
        let moved = Rect::new(9, 1, 2, 1);
        let mut shifted = Vec::new();
        write_kitty_frame(&[widget(moved)], &mut shifted).unwrap();
        let shifted = String::from_utf8_lossy(&shifted).into_owned();
        assert!(
            !shifted.contains("a=T"),
            "moving is not resending: {shifted:?}"
        );
        assert!(
            shifted.contains("\x1b[2;10H"),
            "at the new cell: {shifted:?}"
        );

        // Different pixels: sent again, under the same number.
        let mut changed = PixelImage {
            rgba: vec![8; 4 * 2 * 4],
            ..widget(moved)
        };
        let mut fresh = Vec::new();
        write_kitty_frame(std::slice::from_ref(&changed), &mut fresh).unwrap();
        let fresh = String::from_utf8_lossy(&fresh).into_owned();
        assert!(fresh.contains("a=T,f=32"), "new pixels go out: {fresh:?}");
        assert!(
            fresh.contains("i=1,"),
            "under the number it already had: {fresh:?}"
        );

        // A frame with nothing on it frees what the terminal held, so the
        // next picture cannot be placed against a stale number.
        changed.rgba = vec![8; 4 * 2 * 4];
        let mut empty = Vec::new();
        write_kitty_frame(&[], &mut empty).unwrap();
        assert_eq!(empty, b"\x1b_Ga=d,d=A,q=2\x1b\\");
        let mut after = Vec::new();
        write_kitty_frame(std::slice::from_ref(&changed), &mut after).unwrap();
        let after = String::from_utf8_lossy(&after).into_owned();
        assert!(after.contains("a=T,f=32"), "sent afresh: {after:?}");
    }

    #[test]
    fn colours_resolve_to_pixels() {
        assert_eq!(rgb(Color::Rgb(1, 2, 3)), (1, 2, 3));
        assert_eq!(rgb(Color::Indexed(196)), (255, 0, 0));
        assert_eq!(rgb(Color::Indexed(244)), (128, 128, 128));
        assert_eq!(rgb(Color::White), (255, 255, 255));
        set_cell_pixels(Some((9, 18)));
        assert_eq!(cell_pixels(), Some((9, 18)));
        set_cell_pixels(None);
        assert_eq!(cell_pixels(), None);
    }
}
