//! Render the e2e suite's frame recordings into the demo video.
//!
//! The suite records every painted frame when run with
//! `RUSTEL_E2E_RECORD_DIR=<dir>` (see `crates/studio-e2e/README.md`). This
//! tool turns those recordings into one video: every test plays back to
//! back with no interstitials - the banner across the top of each frame is
//! the only thing that changes between tests, naming the test that is
//! running and how far the suite has come.
//!
//! ```sh
//! # 1. record (default parallel threads - that is where the test names come from)
//! RUSTEL_E2E_RECORD_DIR=target/e2e-records cargo test -p rustel-studio-e2e
//!
//! # 2. render
//! cargo run -p rustel-studio-e2e --features demo --bin e2e-demo
//! ```
//!
//! When `ffmpeg` is on the path the output is an mp4 (H.264, wide player
//! support). When it is not, the frames are encoded into an animated GIF
//! entirely in Rust - no Python, no external tool needed, just a larger
//! file. Either way the result lands beside the recordings.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use font8x8::UnicodeFonts;
use image::{Delay, Frame as AnimFrame, Rgb, RgbImage};
use serde::Deserialize;

// -- look and pace ---------------------------------------------------------

const TERMINAL_BG: [u8; 3] = [10, 10, 14];
const DEFAULT_FG: [u8; 3] = [202, 207, 218];
const BANNER_BG: [u8; 3] = [15, 23, 42];
const BANNER_EDGE: [u8; 3] = [30, 41, 59];
const MUTED: [u8; 3] = [148, 163, 184];
const ACCENT: [u8; 3] = [56, 189, 248];
const WHITE: [u8; 3] = [241, 245, 249];

const BANNER_H: u32 = 58;
const H_PAD: u32 = 10;
const V_PAD: u32 = 6;
const SCALE: u32 = 2;

/// Source fps for sampled playback, and the GIF's frame delay.
const FPS: f64 = 12.0;
/// Longest a test's frames may run, in seconds.
const MAX_SEGMENT: f64 = 10.0;
/// Shortest a test's segment may run, in seconds.
const MIN_SEGMENT: f64 = 1.0;
/// How long a test with no visible frames holds, in seconds.
const EMPTY_HOLD: f64 = 0.9;

// -- recording format ------------------------------------------------------

#[derive(Deserialize)]
struct Frame {
    test: String,
    #[allow(dead_code)]
    n: u32,
    w: u16,
    h: u16,
    ansi: String,
}

/// One entry per recording file: test name, instance, and frames.
///
/// Each JSON line is `{"test", "n", "w", "h", "ansi"}`; identical
/// consecutive frames were already skipped by the recorder.
fn read_recordings(dir: &Path) -> std::io::Result<Vec<(String, Vec<Frame>)>> {
    let mut by_test: BTreeMap<String, Vec<(u32, Vec<Frame>)>> = BTreeMap::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path)?;
        let mut frames = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<Frame>(line) {
                Ok(frame) => frames.push(frame),
                Err(error) => eprintln!("  skipping a line of {}: {error}", path.display()),
            }
        }
        if frames.is_empty() {
            continue;
        }
        let test = frames[0].test.clone();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let instance = stem
            .rsplit("__")
            .next()
            .and_then(|suffix| suffix.parse::<u32>().ok())
            .unwrap_or(0);
        by_test.entry(test).or_default().push((instance, frames));
    }
    // A test that drove several studios (reopen_over) plays as one segment,
    // its studio instances in order.
    Ok(by_test
        .into_iter()
        .map(|(test, mut instances)| {
            instances.sort_by_key(|(instance, _)| *instance);
            (
                test,
                instances
                    .into_iter()
                    .flat_map(|(_, frames)| frames)
                    .collect(),
            )
        })
        .collect())
}

/// `\x1b[38;2;r;g;bm` / `\x1b[48;2;r;g;bm` / `\x1b[0m` → cells, row-major.
/// A rendered cell: the glyph with its foreground and background colours.
type AnsiCell = (char, Option<[u8; 3]>, Option<[u8; 3]>);

fn parse_ansi(ansi: &str) -> Vec<AnsiCell> {
    let mut cells = Vec::new();
    let mut fg = None;
    let mut bg = None;
    let bytes: Vec<char> = ansi.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '\u{1b}' && i + 1 < bytes.len() && bytes[i + 1] == '[' {
            let mut end = None;
            for (offset, ch) in bytes[i + 2..].iter().enumerate() {
                if *ch == 'm' {
                    end = Some(i + 2 + offset);
                    break;
                }
            }
            let Some(end) = end else { break };
            let code: String = bytes[i + 2..end].iter().collect();
            if let Some(rest) = code.strip_prefix("38;2;") {
                fg = parse_rgb(rest);
            } else if let Some(rest) = code.strip_prefix("48;2;") {
                bg = parse_rgb(rest);
            } else if code == "0" || code.is_empty() {
                fg = None;
                bg = None;
            }
            i = end + 1;
            continue;
        }
        let ch = bytes[i];
        if ch != '\n' {
            cells.push((ch, fg, bg));
        }
        i += 1;
    }
    cells
}

fn parse_rgb(rest: &str) -> Option<[u8; 3]> {
    let mut parts = rest.split(';');
    let r = parts.next()?.parse().ok()?;
    let g = parts.next()?.parse().ok()?;
    let b = parts.next()?.parse().ok()?;
    Some([r, g, b])
}

// -- drawing ---------------------------------------------------------------

struct Canvas {
    image: RgbImage,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            image: RgbImage::from_pixel(width, height, Rgb(TERMINAL_BG)),
        }
    }

    fn rect(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, colour: [u8; 3]) {
        for y in y0..=y1.min(self.image.height().saturating_sub(1)) {
            for x in x0..=x1.min(self.image.width().saturating_sub(1)) {
                self.image.put_pixel(x, y, Rgb(colour));
            }
        }
    }

    /// Draw one 8×8 glyph scaled up, skipping transparent pixels. Returns
    /// the advance width.
    fn glyph(&mut self, ch: char, x: u32, y: u32, colour: [u8; 3]) -> u32 {
        let Some(glyph) = font8x8::BASIC_FONTS.get(ch) else {
            return 8 * SCALE;
        };
        for (row, bits) in glyph.into_iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            let px = x + col * SCALE + dx;
                            let py = y + row as u32 * SCALE + dy;
                            if px < self.image.width() && py < self.image.height() {
                                self.image.put_pixel(px, py, Rgb(colour));
                            }
                        }
                    }
                }
            }
        }
        8 * SCALE
    }

    /// Draw a line of text; anything outside Basic Latin falls back to a
    /// space's advance, which is all the banner ever needs.
    fn text(&mut self, x: u32, y: u32, text: &str, colour: [u8; 3]) -> u32 {
        let mut cursor = x;
        for ch in text.chars() {
            cursor += self.glyph(ch, cursor, y, colour);
        }
        cursor - x
    }

    fn text_width(text: &str) -> u32 {
        text.chars().count() as u32 * 8 * SCALE
    }
}

fn image_size(cols: u16, rows: u16) -> (u32, u32) {
    let cell_w = 8 * SCALE;
    let cell_h = 8 * SCALE + 4;
    let mut width = u32::from(cols) * cell_w + 2 * H_PAD;
    let mut height = BANNER_H + u32::from(rows) * cell_h + 2 * V_PAD;
    width -= width % 2;
    height -= height % 2;
    (width, height)
}

fn draw_banner(canvas: &mut Canvas, width: u32, test: &str, idx: usize, total: usize) {
    canvas.rect(0, 0, width - 1, BANNER_H - 1, BANNER_BG);
    canvas.rect(0, BANNER_H - 1, width - 1, BANNER_H - 1, BANNER_EDGE);
    canvas.text(H_PAD, 7, "RUSTEL STUDIO  ·  E2E TEST SUITE DEMO", MUTED);
    canvas.text(H_PAD, 25, &format!("> {test}"), WHITE);
    let label = format!("test {idx} / {total}");
    let label_w = Canvas::text_width(&label);
    canvas.text(width - H_PAD - label_w, 25, &label, MUTED);
    let bar_y = BANNER_H - 4;
    canvas.rect(H_PAD, bar_y, width - H_PAD, BANNER_H - 1, BANNER_EDGE);
    let fill_w = ((width - 2 * H_PAD) * idx as u32 / total as u32).max(4);
    canvas.rect(H_PAD, bar_y, H_PAD + fill_w, BANNER_H - 1, ACCENT);
}

fn render_frame(frame: &Frame, test: &str, idx: usize, total: usize) -> RgbImage {
    let (width, height) = image_size(frame.w, frame.h);
    let mut canvas = Canvas::new(width, height);
    draw_banner(&mut canvas, width, test, idx, total);
    let cell_w = 8 * SCALE;
    let cell_h = 8 * SCALE + 4;
    let mut grid = parse_ansi(&frame.ansi);
    let expected = (u32::from(frame.w) * u32::from(frame.h)) as usize;
    if grid.len() < expected {
        grid.resize(expected, (' ', None, None));
    }
    for y in 0..u32::from(frame.h) {
        for x in 0..u32::from(frame.w) {
            let (ch, fg, bg) = grid[(y * u32::from(frame.w) + x) as usize];
            let px = H_PAD + x * cell_w;
            let py = BANNER_H + V_PAD + y * cell_h;
            if let Some(bg) = bg {
                canvas.rect(px, py, px + cell_w - 1, py + cell_h - 1, bg);
            }
            if !ch.is_whitespace() {
                canvas.glyph(ch, px, py + 2, fg.unwrap_or(DEFAULT_FG));
            }
        }
    }
    canvas.image
}

fn message_card(width: u32, height: u32, title: &str, sub: &str) -> RgbImage {
    let mut canvas = Canvas::new(width, height);
    canvas.rect(0, 0, width - 1, height - 1, BANNER_BG);
    let title_x = width.saturating_sub(Canvas::text_width(title)) / 2;
    let title_y = height / 2 - 30;
    canvas.text(title_x, title_y, title, WHITE);
    let sub_x = width.saturating_sub(Canvas::text_width(sub)) / 2;
    let sub_y = height / 2 + 24;
    canvas.text(sub_x, sub_y, sub, MUTED);
    let rule_y = height / 2 + 52;
    canvas.rect(width / 2 - 160, rule_y, width / 2 + 160, rule_y + 4, ACCENT);
    canvas.image
}

/// At most `fps * max_segment` frames, spread evenly - always ending on the
/// test's final state, which is the state it asserted on.
fn sample_frames(frames: &[Frame]) -> Vec<&Frame> {
    let cap = ((FPS * MAX_SEGMENT) as usize).max(1);
    if frames.len() <= cap {
        return frames.iter().collect();
    }
    let last = frames.len() - 1;
    (0..cap).map(|i| &frames[i * last / (cap - 1)]).collect()
}

// -- encoding ---------------------------------------------------------------

fn ffmpeg_on_path() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Encode through ffmpeg: every segment becomes a png, and the concat
/// demuxer assembles them with their holds. The mp4 is what a musician can
/// scrub; the tool says so when ffmpeg is missing and the GIF path runs.
fn encode_mp4(segments: &[(RgbImage, f64)], out: &Path, frames_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(frames_dir).map_err(|error| error.to_string())?;
    for old in std::fs::read_dir(frames_dir)
        .map_err(|error| error.to_string())?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
    {
        std::fs::remove_file(old.path()).map_err(|error| error.to_string())?;
    }
    let mut concat = String::new();
    for (i, (img, hold)) in segments.iter().enumerate() {
        let path = frames_dir.join(format!("{i:06}.png"));
        img.save(&path).map_err(|error| error.to_string())?;
        concat.push_str(&format!(
            "file '{}'\n",
            path.to_string_lossy().replace('\'', "'\\''")
        ));
        concat.push_str(&format!("duration {hold:.6}\n"));
    }
    // The concat demuxer ignores the last image's duration; listing it
    // again keeps the final hold.
    if let Some((_, _)) = segments.last() {
        let last = frames_dir.join(format!("{:06}.png", segments.len() - 1));
        concat.push_str(&format!(
            "file '{}'\n",
            last.to_string_lossy().replace('\'', "'\\''")
        ));
    }
    let concat_path = frames_dir.join("concat.txt");
    std::fs::write(&concat_path, concat).map_err(|error| error.to_string())?;
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            &concat_path.to_string_lossy(),
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "24",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("ffmpeg failed to start: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg exited with {status}"))
    }
}

/// The pure-Rust fallback: one animated GIF, every frame with a
/// centisecond delay. No external tool, at the cost of a larger file - a
/// demo of terminal frames compresses well, so the cost is tolerable.
fn encode_gif(segments: &[(RgbImage, f64)], out: &Path) -> Result<(), String> {
    let file = std::fs::File::create(out).map_err(|error| error.to_string())?;
    let mut stream = image::codecs::gif::GifEncoder::new(file);
    stream
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .map_err(|error| error.to_string())?;
    for (img, hold) in segments {
        let ms = ((*hold * 1000.0).round() as u32).max(20);
        let frame = AnimFrame::from_parts(
            image::DynamicImage::ImageRgb8(img.clone()).to_rgba8(),
            0,
            0,
            Delay::from_numer_denom_ms(ms, 1),
        );
        stream
            .encode_frame(frame)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

// -- assembly ---------------------------------------------------------------

fn main() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let default_recordings = root.join("recordings");
    let fallback_recordings = root
        .parent()
        .and_then(|parent| parent.parent())
        .map(|workspace| workspace.join("target").join("e2e-records"))
        .unwrap_or_else(|| PathBuf::from("target/e2e-records"));
    let out_default = root.join("demo").join("e2e-demo.mp4");

    let mut recordings = default_recordings.clone();
    let mut out = out_default.clone();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--recordings" => {
                recordings = args
                    .next()
                    .map(PathBuf::from)
                    .expect("--recordings takes a directory");
            }
            "--out" => {
                out = args.next().map(PathBuf::from).expect("--out takes a path");
            }
            other => {
                eprintln!("unknown argument {other:?}; --recordings <dir> and --out <path> exist");
                return ExitCode::FAILURE;
            }
        }
    }

    let recordings_empty = !recordings.is_dir()
        || std::fs::read_dir(&recordings)
            .map(|entries| entries.count() == 0)
            .unwrap_or(true);
    if recordings_empty && recordings == default_recordings && fallback_recordings.is_dir() {
        recordings = fallback_recordings;
    }
    let entries = match read_recordings(&recordings) {
        Ok(entries) if !entries.is_empty() => entries,
        Ok(_) => {
            eprintln!(
                "no recordings in {} - run the suite with\n  RUSTEL_E2E_RECORD_DIR=recordings cargo test -p rustel-studio-e2e",
                recordings.display()
            );
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("cannot read {}: {error}", recordings.display());
            return ExitCode::FAILURE;
        }
    };
    let total = entries.len();
    let total_frames: usize = entries.iter().map(|(_, frames)| frames.len()).sum();

    let first = entries
        .iter()
        .find(|(_, frames)| !frames.is_empty())
        .map(|(_, frames)| (frames[0].w, frames[0].h))
        .expect("at least one recording has frames");
    let (width, height) = image_size(first.0, first.1);

    let mut segments: Vec<(RgbImage, f64)> = Vec::new();
    segments.push((
        message_card(width, height, "rustel studio", "E2E TEST SUITE DEMO"),
        3.0,
    ));
    for (idx, (test, frames)) in entries.iter().enumerate() {
        let idx = idx + 1;
        if frames.is_empty() {
            // A test that never opened a studio (a pure table assertion,
            // e.g.) has nothing to show: a brief blank screen with its
            // banner, so the suite still reads complete top to bottom.
            segments.push((render_blank(width, height, test, idx, total), EMPTY_HOLD));
            continue;
        }
        let sampled = sample_frames(frames);
        let hold = if sampled.len() as f64 * (1.0 / FPS) < MIN_SEGMENT {
            MIN_SEGMENT / sampled.len() as f64
        } else {
            1.0 / FPS
        };
        for frame in sampled {
            segments.push((render_frame(frame, test, idx, total), hold));
        }
    }
    segments.push((
        message_card(
            width,
            height,
            "suite complete [ok]",
            &format!("{total} tests, recorded live from the suite"),
        ),
        2.5,
    ));

    let use_mp4 = ffmpeg_on_path();
    let result = if use_mp4 {
        encode_mp4(&segments, &out, &root.join("../../target/e2e-demo-frames"))
    } else {
        let gif_out = out.with_extension("gif");
        println!("ffmpeg not found - writing an animated GIF instead (pure Rust)");
        encode_gif(&segments, &gif_out).map(|()| {
            println!("video: {}", gif_out.display());
        })
    };
    if let Err(error) = result {
        eprintln!("encoding failed: {error}");
        return ExitCode::FAILURE;
    }
    if use_mp4 {
        println!("video: {}", out.display());
    }

    println!("recorded {total} tests, {total_frames} distinct frames");
    let _ = std::io::stdout().flush();
    ExitCode::SUCCESS
}

fn render_blank(width: u32, height: u32, test: &str, idx: usize, total: usize) -> RgbImage {
    let mut canvas = Canvas::new(width, height);
    draw_banner(&mut canvas, width, test, idx, total);
    canvas.image
}
