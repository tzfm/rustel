//! Terminal ownership for the native studio.
//!
//! The alternate screen, mouse protocol and keyboard enhancements are a
//! single RAII transaction.  A normal return or an unwind restores the user's
//! shell before the CLI reports an error.

use std::io::{self, BufWriter, IsTerminal, Stdout, Write};

use crossterm::cursor::{Hide, MoveTo, SetCursorStyle, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode, supports_keyboard_enhancement,
};
use crossterm::{execute, queue};
use ratatui::style::Color;
use ratatui::{Terminal, buffer::Buffer};
use rustel_runtime::terminal_text::{control_picture, is_unsafe_terminal_character};
use serde::{Deserialize, Serialize};

mod backend;
#[cfg(windows)]
mod windows_input;

pub use backend::StudioBackend;

// Crossterm formats each changed cell through many small writes. Batch them
// before touching stdout's lock and line buffer; draw and end_frame retain
// their explicit flushes, including the boundary before pointer-shape output.
const FRAME_OUTPUT_CAPACITY: usize = 64 * 1024;
pub type StudioTerminal = Terminal<StudioBackend<BufWriter<Stdout>>>;

/// The six DECSCUSR cursor shapes. Rustel starts with a steady bar, its
/// long-standing shape; a theme may temporarily override the saved choice.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaretShape {
    #[default]
    SteadyBar,
    BlinkingBar,
    SteadyBlock,
    BlinkingBlock,
    SteadyUnderline,
    BlinkingUnderline,
}

impl CaretShape {
    pub const ALL: [Self; 6] = [
        Self::SteadyBar,
        Self::BlinkingBar,
        Self::SteadyBlock,
        Self::BlinkingBlock,
        Self::SteadyUnderline,
        Self::BlinkingUnderline,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::SteadyBar => "steady-bar",
            Self::BlinkingBar => "blinking-bar",
            Self::SteadyBlock => "steady-block",
            Self::BlinkingBlock => "blinking-block",
            Self::SteadyUnderline => "steady-underline",
            Self::BlinkingUnderline => "blinking-underline",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SteadyBar => "steady bar",
            Self::BlinkingBar => "blinking bar",
            Self::SteadyBlock => "steady block",
            Self::BlinkingBlock => "blinking block",
            Self::SteadyUnderline => "steady underline",
            Self::BlinkingUnderline => "blinking underline",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|shape| shape.key().eq_ignore_ascii_case(text.trim()))
    }

    pub fn step(self, forwards: bool) -> Self {
        let at = Self::ALL
            .iter()
            .position(|shape| *shape == self)
            .unwrap_or(0);
        let next = if forwards {
            (at + 1) % Self::ALL.len()
        } else {
            (at + Self::ALL.len() - 1) % Self::ALL.len()
        };
        Self::ALL[next]
    }

    /// Terminal output can restart hardware blinking. The app owns the
    /// half-second phase and uses a steady hardware shape underneath.
    pub fn steady(self) -> Self {
        match self {
            Self::BlinkingBar => Self::SteadyBar,
            Self::BlinkingBlock => Self::SteadyBlock,
            Self::BlinkingUnderline => Self::SteadyUnderline,
            other => other,
        }
    }

    pub fn visible_after(self, idle: std::time::Duration) -> bool {
        self == self.steady() || (idle.as_millis() / 500).is_multiple_of(2)
    }

    fn decscusr(self) -> u8 {
        match self {
            Self::BlinkingBlock => 1,
            Self::SteadyBlock => 2,
            Self::BlinkingUnderline => 3,
            Self::SteadyUnderline => 4,
            Self::BlinkingBar => 5,
            Self::SteadyBar => 6,
        }
    }
}

/// What this terminal can do beyond text, found out once at the start:
/// identity from the environment, the rest by asking it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFeatures {
    /// `kitty 0.35.2`, `WezTerm 20240203`, `iTerm.app`, … or `unknown`.
    pub name: String,
    pub truecolor: bool,
    /// The kitty keyboard protocol: modified Enter and punctuation reported
    /// distinctly, and Shift told apart from its unshifted key.
    ///
    /// This does not mean that Command chords work. iTerm2 answers this
    /// handshake, but macOS still consumes every Command chord at the menu
    /// layer. Whether Command reaches the studio is
    /// `KeyboardCapabilities::super_seen`, learnt by watching a chord
    /// arrive, and it is never used to spell a shortcut.
    pub keyboard: bool,
    /// DEC 2026 synchronized output: frames land whole.
    pub sync_output: bool,
    pub kitty_graphics: bool,
    pub sixel: bool,
    /// Draws the Unicode sextant glyphs itself, whatever the font.
    pub fine_glyphs: bool,
    /// One cell in pixels, when the terminal says.
    pub cell_pixels: Option<(u16, u16)>,
    /// The pointer reported in pixels rather than cells (DECSET 1016), so
    /// a drag across a slider's pill has every pixel of the pill rather
    /// than one position per cell. Only where the cell size is known too,
    /// or the pixels could not be brought back to the grid.
    pub pixel_mouse: bool,
    /// Draws the studio's decorative glyphs (▸ ▾ ⇧ …) rather than tofu.
    /// False only for conhost - Windows' legacy console host, the window
    /// `cmd.exe` opens on its own - whose one font, Consolas, has none of
    /// them. See [`symbol`] for what is drawn instead.
    pub symbols: bool,
}

impl Default for TerminalFeatures {
    /// Every capability absent, because an untested terminal is assumed to
    /// lack it. `symbols` is the exception and defaults to true: a terminal
    /// that no probe reached draws its glyphs, and `detect` clears the flag
    /// where they are missing.
    fn default() -> Self {
        Self {
            name: String::new(),
            truecolor: false,
            keyboard: false,
            sync_output: false,
            kitty_graphics: false,
            sixel: false,
            fine_glyphs: false,
            cell_pixels: None,
            pixel_mouse: false,
            symbols: true,
        }
    }
}

impl TerminalFeatures {
    /// One line for the log and the settings sheet.
    pub fn summary(&self) -> String {
        let mark = |on: bool| if on { "✓" } else { "✗" };
        let cell = self
            .cell_pixels
            .map(|(w, h)| format!("{w}×{h}px"))
            .unwrap_or_else(|| "?".to_owned());
        format!(
            "{} · truecolor {} · keyboard {} · sync {} · kitty graphics {} · sixel {} · fine glyphs {} · pixel mouse {} · symbols {} · cell {}",
            self.name,
            mark(self.truecolor),
            mark(self.keyboard),
            mark(self.sync_output),
            mark(self.kitty_graphics),
            mark(self.sixel),
            mark(self.fine_glyphs),
            mark(self.pixel_mouse),
            mark(self.symbols),
            cell
        )
    }

    /// The default drawing tier for this terminal. It is always a glyph
    /// tier, because the glyph tiers respond fastest. The pixel tier sends
    /// megabytes of in-band image per frame through the pty and is
    /// measurably slow on every terminal tried (kitty and Ghostty under
    /// WSLg included), so automatic negotiation never selects it.
    pub fn default_tier(&self) -> super::graphics::Tier {
        if self.fine_glyphs {
            super::graphics::Tier::Fine
        } else {
            super::graphics::Tier::Cells
        }
    }

    /// One probe, no studio: what THIS terminal answers, for `--probe-terminal`
    /// and for anyone asking why a tier did or did not engage.
    pub fn probe_now() -> Self {
        // Keep raw input throughout both queries. The standalone probe used
        // to skip the keyboard query and therefore always report false.
        #[cfg(unix)]
        let _raw_mode = ProbeRawMode::enter();
        let keyboard =
            io::stdin().is_terminal() && matches!(supports_keyboard_enhancement(), Ok(true));
        let features = Self::detect(keyboard);
        #[cfg(windows)]
        {
            let mut features = features;
            let _input = windows_terminal_input(&mut features);
            features
        }
        #[cfg(not(windows))]
        features
    }

    /// Identity from the environment, then capability answers from the terminal.
    fn detect(enhanced_keyboard: bool) -> Self {
        let name = identity();
        // An inherited outer name is useful for shortcut conflicts, not proof
        // that the multiplexer forwards that terminal's drawing protocols.
        let profile = profiles::capabilities(active_identity(&name));
        let fine_glyphs = profile.fine_glyphs;
        let truecolor = std::env::var("COLORTERM")
            .map(|value| value.contains("truecolor") || value.contains("24bit"))
            .unwrap_or(false)
            || profile.truecolor;
        let symbols = profile.symbols;
        // The one place this is learnt: drawing code with no
        // `TerminalFeatures` of its own to ask - the mixer and device
        // panels keep only a `Theme` - reads it back through `symbol`.
        symbol_state::set(symbols);
        let mut features = Self {
            name,
            truecolor,
            keyboard: enhanced_keyboard,
            sync_output: false,
            kitty_graphics: false,
            sixel: false,
            fine_glyphs,
            cell_pixels: cell_pixels_now(),
            pixel_mouse: false,
            symbols,
        };
        if let Some(answers) = ask_terminal() {
            features.apply_answers(parse_answers(&answers));
        }
        features
    }

    fn apply_answers(&mut self, answers: Answers) {
        self.sync_output = answers.sync_output;
        self.kitty_graphics = answers.kitty_graphics;
        self.sixel = answers.sixel;
        // XTWINOPS reports the cell size directly. Prefer it to the pty's
        // window dimensions, which can be stale or include terminal padding.
        self.cell_pixels = answers.cell_pixels.or(self.cell_pixels);
        // Pixel reports are useful only with a valid scale back to the grid.
        self.pixel_mouse = answers.pixel_mouse && self.cell_pixels.is_some();
    }
}

/// [`TerminalFeatures::probe_now`] for `--probe-terminal`, in raw mode as the
/// studio has it: cooked mode holds the answers for a newline that never
/// comes and echoes their escapes at the screen.
pub fn probe_terminal() -> TerminalFeatures {
    let _ = enable_raw_mode();
    let features = TerminalFeatures::probe_now();
    let _ = disable_raw_mode();
    features
}

/// The endpoint talking to Rustel, before an inherited outer-terminal hint.
fn active_identity(name: &str) -> &str {
    name.split(" via ").next().unwrap_or(name)
}

#[cfg(unix)]
struct ProbeRawMode(bool);

#[cfg(unix)]
impl ProbeRawMode {
    fn enter() -> Self {
        Self(
            io::stdin().is_terminal()
                && !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false)
                && enable_raw_mode().is_ok(),
        )
    }
}

#[cfg(unix)]
impl Drop for ProbeRawMode {
    fn drop(&mut self) {
        if self.0 {
            let _ = disable_raw_mode();
        }
    }
}

/// Environment identity is a hint, never proof of keyboard protocol support.
fn identity() -> String {
    identity_from_env(
        |key| std::env::var(key).ok().filter(|value| !value.is_empty()),
        cfg!(windows),
    )
}

fn identity_from_env(get: impl Fn(&str) -> Option<String>, on_windows: bool) -> String {
    let term = get("TERM").unwrap_or_default();
    let program = get("TERM_PROGRAM").unwrap_or_default();
    let lower_term = term.to_ascii_lowercase();
    let multiplexer = if lower_term.starts_with("tmux")
        || lower_term.starts_with("screen")
            && (program.eq_ignore_ascii_case("tmux")
                || get("TMUX").is_some() && get("STY").is_none())
        || term.is_empty() && program.eq_ignore_ascii_case("tmux")
    {
        Some("tmux")
    } else if lower_term.starts_with("screen") {
        Some("screen")
    } else {
        None
    };
    let outer = outer_identity(&get, &term, &program, on_windows, multiplexer.is_some());
    match multiplexer {
        Some(mux) if outer != "unknown" => format!("{mux} via {outer}"),
        Some(mux) => mux.to_owned(),
        None => outer,
    }
}

fn outer_identity(
    get: &impl Fn(&str) -> Option<String>,
    term: &str,
    program: &str,
    on_windows: bool,
    multiplexed: bool,
) -> String {
    // A distinctive current TERM outranks variables inherited from an emulator
    // that launched this one. Generic xterm-256color proves no identity.
    let lower = term.to_ascii_lowercase();
    for (prefix, name) in [
        ("xterm-kitty", "kitty"),
        ("xterm-ghostty", "Ghostty"),
        ("alacritty", "Alacritty"),
        ("foot", "foot"),
        ("contour", "contour"),
        ("rio", "rio"),
        ("st-", "st"),
    ] {
        if lower.starts_with(prefix) {
            return name.to_owned();
        }
    }
    // An IDE is usually started from another terminal (`code .`) and
    // inherits that terminal's variables; the ones it sets itself are fresh,
    // and the IDE takes its keys before its terminal sees them.
    if program.eq_ignore_ascii_case("vscode") {
        let version = get("TERM_PROGRAM_VERSION")
            .map(|version| format!(" {version}"))
            .unwrap_or_default();
        return format!("vscode{version}");
    }
    if get("TERMINAL_EMULATOR").is_some_and(|name| name.starts_with("JetBrains")) {
        return "JetBrains".into();
    }
    if get("KITTY_WINDOW_ID").is_some() {
        return "kitty".into();
    }
    if let Some(version) = get("KONSOLE_VERSION") {
        return format!("Konsole {version}");
    }
    if let Some(version) = get("XTERM_VERSION") {
        return format!("xterm {version}");
    }
    if get("RXVT_SOCKET").is_some() || lower.starts_with("rxvt") {
        return "rxvt-unicode".into();
    }
    if get("GNOME_TERMINAL_SCREEN").is_some() || get("GNOME_TERMINAL_SERVICE").is_some() {
        return "GNOME Terminal".into();
    }
    // VTE hosts that leave a variable of their own. Every one of them also
    // has VTE_VERSION, which alone names no host.
    if get("TILIX_ID").is_some() {
        return "Tilix".into();
    }
    if get("TERMINATOR_UUID").is_some() {
        return "Terminator".into();
    }
    if get("GUAKE_TAB_UUID").is_some() || program.eq_ignore_ascii_case("guake") {
        return "Guake".into();
    }
    if let Some(version) = get("PTYXIS_VERSION") {
        return format!("Ptyxis {version}");
    }
    if program.eq_ignore_ascii_case("kgx") {
        let version = get("TERM_PROGRAM_VERSION")
            .map(|version| format!(" {version}"))
            .unwrap_or_default();
        return format!("GNOME Console{version}");
    }
    if get("VTE_VERSION").is_some() {
        // VTE is shared by many applications and does not identify its host.
        return "VTE".into();
    }
    if !program.is_empty()
        && !program.eq_ignore_ascii_case("tmux")
        && !program.eq_ignore_ascii_case("screen")
    {
        let version = get("TERM_PROGRAM_VERSION")
            .map(|version| format!(" {version}"))
            .unwrap_or_default();
        let name = if program.eq_ignore_ascii_case("Apple_Terminal") {
            "Apple Terminal"
        } else if program.eq_ignore_ascii_case("iTerm.app") {
            "iTerm2"
        } else {
            program
        };
        return format!("{name}{version}");
    }
    if get("WEZTERM_PANE").is_some() {
        return "WezTerm".into();
    }
    if get("GHOSTTY_RESOURCES_DIR").is_some() {
        return "Ghostty".into();
    }
    if get("WT_SESSION").is_some() {
        return "Windows Terminal".into();
    }
    if multiplexed {
        "unknown".into()
    } else if on_windows && (term.is_empty() || lower == "xterm" || lower == "xterm-256color") {
        // This is a conservative console fallback, not an identified emulator.
        "Windows Console".into()
    } else if term.is_empty() {
        "unknown".into()
    } else {
        format!("unknown ({term})")
    }
}

/// What the terminal keeps for itself, so the studio never ships a default
/// it cannot receive.
///
/// A terminal does not advertise the keys it consumes. The key never
/// arrives, and the control looks broken to the player.
///
/// The table is data, in `terminals.json`, so a changed terminal default
/// is a text edit and not a code change. `include_str!` embeds it, because
/// the studio reads it at startup and an unreadable table must cost no
/// more than an empty table. It describes shipped defaults, not the user's
/// terminal configuration:
/// custom terminal/desktop shortcuts cannot be discovered by protocol probes.
/// Studio bindings remain independently configurable in Settings.
pub mod profiles {
    use super::super::keybinds::KeyCombo;
    use std::sync::OnceLock;

    const BUILT_IN: &str = include_str!("terminals.json");

    #[derive(Clone, Debug, serde::Deserialize)]
    pub struct Stolen {
        pub chord: String,
        /// What the terminal does with it, in words a player can act on.
        pub does: String,
        /// Empty means all platforms; otherwise Rust target OS names.
        #[serde(default)]
        pub platforms: Vec<String>,
    }

    #[derive(Clone, Debug, serde::Deserialize)]
    pub struct Terminal {
        /// Matched case-insensitively at a name boundary in `identity()`;
        /// several emulators stamp a version after their name.
        pub terminal: String,
        #[serde(default)]
        pub source: String,
        /// How somebody takes the chord back, if their terminal allows it.
        #[serde(default)]
        pub unbind: String,
        /// Rendering behavior that cannot be established by a terminal
        /// protocol. Keep terminal-specific policy in the data table.
        #[serde(default)]
        capabilities: Capabilities,
        #[serde(default)]
        pub steals: Vec<Stolen>,
        #[serde(default)]
        legacy_input: Vec<InputLimitation>,
        /// The name stands for a family of emulators and identifies none.
        #[serde(default)]
        generic: bool,
    }

    #[derive(Clone, Debug, serde::Deserialize)]
    struct InputLimitation {
        chord: String,
        reason: String,
        /// A different decoded key sent by this terminal for the physical chord.
        #[serde(default)]
        received: Option<String>,
        #[serde(default, rename = "source")]
        _source: String,
    }

    #[derive(Clone, Copy, Debug, serde::Deserialize)]
    pub(super) struct Capabilities {
        #[serde(default)]
        pub truecolor: bool,
        #[serde(default)]
        pub fine_glyphs: bool,
        #[serde(default = "symbols_by_default")]
        pub symbols: bool,
        /// Some releases override the visible OSC 22 pointer on modifier
        /// events without updating their cached application shape.
        #[serde(default)]
        pointer_modifier_reset: bool,
        #[cfg_attr(not(any(windows, test)), allow(dead_code))]
        #[serde(default)]
        windows_pixel_input_since: Option<u32>,
    }

    const fn symbols_by_default() -> bool {
        true
    }

    impl Default for Capabilities {
        fn default() -> Self {
            Self {
                truecolor: false,
                fine_glyphs: false,
                symbols: true,
                pointer_modifier_reset: false,
                windows_pixel_input_since: None,
            }
        }
    }

    #[derive(Clone, Debug, serde::Deserialize)]
    struct Desktop {
        platform: String,
        #[serde(default)]
        desktop: Option<String>,
        #[serde(rename = "source")]
        _source: String,
        unbind: String,
        steals: Vec<Stolen>,
    }

    #[derive(Clone, Debug, Default, serde::Deserialize)]
    struct Table {
        #[serde(default)]
        terminals: Vec<Terminal>,
        #[serde(default)]
        platforms: Vec<Desktop>,
        #[serde(default)]
        legacy_input: Vec<InputLimitation>,
    }

    fn table() -> &'static Table {
        static TABLE: OnceLock<Table> = OnceLock::new();
        TABLE.get_or_init(|| {
            // An unreadable table is no table. It decides defaults and
            // nothing else, so the studio starts either way.
            serde_json::from_str(BUILT_IN).unwrap_or_default()
        })
    }

    fn platform() -> &'static str {
        #[cfg(test)]
        if let Some(platform) = TEST_PLATFORM.get() {
            return platform;
        }
        std::env::consts::OS
    }

    #[cfg(test)]
    thread_local! {
        static TEST_PLATFORM: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    }

    /// Exercise the same platform policy on every CI host, without changing
    /// process environment or leaking a fake desktop into parallel tests.
    #[cfg(test)]
    pub(crate) struct ForcePlatformForTest(Option<&'static str>);

    #[cfg(test)]
    impl ForcePlatformForTest {
        pub(crate) fn set(platform: &'static str) -> Self {
            Self(TEST_PLATFORM.replace(Some(platform)))
        }
    }

    #[cfg(test)]
    impl Drop for ForcePlatformForTest {
        fn drop(&mut self) {
            TEST_PLATFORM.set(self.0);
        }
    }

    fn desktop_from_env(get: impl Fn(&str) -> Option<String>) -> &'static str {
        let kde_session = get("KDE_FULL_SESSION")
            .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1");
        let named_plasma = ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP"]
            .iter()
            .any(|key| {
                get(key).is_some_and(|names| {
                    names.split(':').any(|name| {
                        name.eq_ignore_ascii_case("kde") || name.eq_ignore_ascii_case("plasma")
                    })
                })
            });
        let named = |wanted: &str| {
            ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP"]
                .iter()
                .any(|key| {
                    get(key).is_some_and(|names| {
                        names
                            .split(':')
                            .any(|name| name.eq_ignore_ascii_case(wanted))
                    })
                })
        };
        if kde_session || named_plasma {
            "plasma"
        } else if named("xfce") {
            "xfce"
        } else {
            ""
        }
    }

    fn desktop() -> &'static str {
        #[cfg(test)]
        if let Some(desktop) = TEST_DESKTOP.get() {
            return desktop;
        }
        static DESKTOP: OnceLock<&'static str> = OnceLock::new();
        DESKTOP.get_or_init(|| desktop_from_env(|key| std::env::var(key).ok()))
    }

    #[cfg(test)]
    thread_local! {
        static TEST_DESKTOP: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    }

    #[cfg(test)]
    pub(crate) struct ForceDesktopForTest(Option<&'static str>);

    #[cfg(test)]
    impl ForceDesktopForTest {
        pub(crate) fn set(desktop: &'static str) -> Self {
            Self(TEST_DESKTOP.replace(Some(desktop)))
        }
    }

    #[cfg(test)]
    impl Drop for ForceDesktopForTest {
        fn drop(&mut self) {
            TEST_DESKTOP.set(self.0);
        }
    }

    fn matches_profile(identity: &str, profile: &str) -> bool {
        identity.split(" via ").any(|layer| {
            let layer = layer.trim();
            layer
                .get(..profile.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(profile))
                && layer
                    .get(profile.len()..)
                    .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
        })
    }

    fn applies_on(stolen: &Stolen, platform: &str) -> bool {
        stolen.platforms.is_empty() || stolen.platforms.iter().any(|known| known == platform)
    }

    /// Whether every detected layer has a shipped conflict profile.
    pub fn profile_known(terminal: &str) -> bool {
        terminal.split(" via ").all(|layer| {
            table()
                .terminals
                .iter()
                .any(|known| matches_profile(layer, &known.terminal))
        })
    }

    /// A manual selection is one shipped profile, stored using its canonical
    /// name. Unknown or removed profile names fall back to automatic detection.
    pub fn canonical_profile(name: &str) -> Option<&'static str> {
        known().find(|known| known.eq_ignore_ascii_case(name.trim()))
    }

    /// Override the emulator's conflict profile while preserving detected
    /// multiplexers: their prefix keys still intercept input inside that emulator.
    pub fn effective_profile(detected: &str, selected: Option<&str>) -> String {
        let Some(selected) = selected.and_then(canonical_profile) else {
            return detected.to_owned();
        };
        let mut layers = Vec::new();
        for layer in detected.split(" via ") {
            for multiplexer in ["tmux", "screen"] {
                if matches_profile(layer, multiplexer) && !layers.contains(&multiplexer) {
                    layers.push(multiplexer);
                }
            }
        }
        if !layers.contains(&selected) {
            layers.push(selected);
        }
        layers.join(" via ")
    }

    /// The detected emulator, when the pinned profile describes another one.
    ///
    /// Detected multiplexers stay in the effective profile, so they are not
    /// compared. An unknown or generic name identifies no emulator: a pin
    /// over it is a correction and not a mismatch.
    pub fn pinned_over<'a>(detected: &'a str, selected: Option<&str>) -> Option<&'a str> {
        let selected = selected.and_then(canonical_profile)?;
        detected.split(" via ").map(str::trim).find(|layer| {
            !matches_profile(layer, selected)
                && table().terminals.iter().any(|known| {
                    !known.generic
                        && !matches!(known.terminal.as_str(), "tmux" | "screen")
                        && matches_profile(layer, &known.terminal)
                })
        })
    }

    /// The terminals this table knows, by the name `identity()` reports.
    pub fn known() -> impl Iterator<Item = &'static str> {
        table()
            .terminals
            .iter()
            .map(|terminal| terminal.terminal.as_str())
    }

    /// A decoder/encoding limitation is not a shortcut owned by a terminal.
    /// Native console and negotiated enhanced input do not use these legacy
    /// sequences, so callers consult this only for legacy VT input.
    pub fn legacy_input_limitation(terminal: &str, chord: &KeyCombo) -> Option<&'static str> {
        let wanted = chord.key();
        table()
            .legacy_input
            .iter()
            .chain(
                table()
                    .terminals
                    .iter()
                    .filter(|known| matches_profile(terminal, &known.terminal))
                    .flat_map(|known| &known.legacy_input),
            )
            .find(|row| {
                (row.chord == wanted && row.received.is_none())
                    || row.received.as_deref() == Some(wanted.as_str())
            })
            .map(|row| row.reason.as_str())
    }

    /// Recover physical chord names from a terminal's legacy keymap. This
    /// keeps shortcut hints and learned keys consistent with actual input.
    pub fn legacy_key(terminal: &str, received: &KeyCombo) -> Option<KeyCombo> {
        let wanted = received.key();
        table()
            .legacy_input
            .iter()
            .chain(
                table()
                    .terminals
                    .iter()
                    .filter(|known| matches_profile(terminal, &known.terminal))
                    .flat_map(|known| &known.legacy_input),
            )
            .find(|row| row.received.as_deref() == Some(wanted.as_str()))
            .and_then(|row| KeyCombo::parse(&row.chord))
    }

    /// Rendering policy for the active terminal layer. Protocol probes still
    /// override what they can observe; this table owns terminal-name policy.
    pub(super) fn capabilities(terminal: &str) -> Capabilities {
        table()
            .terminals
            .iter()
            .find(|known| matches_profile(terminal, &known.terminal))
            .map(|known| known.capabilities)
            .unwrap_or_default()
    }

    /// Whether modifier events can desynchronize the visible pointer from
    /// its OSC 22 state. Call with the detected identity, never the manually
    /// selected shortcut profile: this describes terminal protocol behavior.
    pub fn pointer_modifier_reset(terminal: &str) -> bool {
        table().terminals.iter().any(|known| {
            matches_profile(terminal, &known.terminal) && known.capabilities.pointer_modifier_reset
        })
    }

    /// Whether this terminal profile and release can provide native Windows
    /// pixel mouse input around ConPTY. The profile owns the release floor;
    /// Rust only parses the version reported by terminal identification.
    #[cfg(any(windows, test))]
    pub(super) fn supports_windows_pixel_input(terminal: &str) -> bool {
        let active = super::active_identity(terminal);
        let Some(profile) = table()
            .terminals
            .iter()
            .find(|known| matches_profile(active, &known.terminal))
        else {
            return false;
        };
        let Some(minimum) = profile.capabilities.windows_pixel_input_since else {
            return false;
        };
        active
            .split_ascii_whitespace()
            .nth(1)
            .and_then(|version| version.get(..8))
            .and_then(|date| date.parse::<u32>().ok())
            .is_some_and(|date| date >= minimum)
    }

    /// What `terminal` does with `chord`, if it keeps it for itself.
    ///
    /// `terminal` is what `identity()` reported. Known desktop shortcuts apply
    /// even to unknown emulators. `None` means no known conflict; customized
    /// terminal or desktop bindings cannot be established by this table.
    pub fn steals(terminal: &str, chord: &KeyCombo) -> Option<&'static str> {
        // Crossterm reports modified Tab as BackTab on several transports.
        // Both spellings describe the same terminal accelerator.
        let wanted = if chord.code == crossterm::event::KeyCode::BackTab {
            format!("{}shift+tab", if chord.control { "ctrl+" } else { "" })
        } else {
            chord.key()
        };
        let platform = platform();
        let desktop = desktop();
        table()
            .platforms
            .iter()
            .filter(|known| {
                known.platform == platform
                    && known.desktop.as_deref().is_none_or(|name| name == desktop)
            })
            .flat_map(|desktop| desktop.steals.iter())
            .chain(
                table()
                    .terminals
                    .iter()
                    .filter(|known| matches_profile(terminal, &known.terminal))
                    .flat_map(|known| known.steals.iter()),
            )
            .find(|stolen| {
                applies_on(stolen, platform) && stolen.chord.eq_ignore_ascii_case(&wanted)
            })
            .map(|stolen| stolen.does.as_str())
    }

    /// How to take a chord back, for the terminal a player is in.
    pub fn unbind_hint(terminal: &str) -> Option<&'static str> {
        if let Some(desktop) = table().platforms.iter().find(|known| {
            known.platform == platform()
                && known
                    .desktop
                    .as_deref()
                    .is_none_or(|name| name == desktop())
        }) {
            return Some(desktop.unbind.as_str());
        }
        table()
            .terminals
            .iter()
            .find(|known| matches_profile(terminal, &known.terminal))
            .map(|known| known.unbind.as_str())
            .filter(|hint| !hint.is_empty())
    }

    #[cfg(test)]
    mod tests {
        use super::super::super::keybinds::KeyCombo;

        /// The shipped table parses, and every row is a chord the preferences
        /// can hold. The lookup compares against `KeyCombo::key()`, so a chord
        /// that does not round-trip through the parser matches nothing.
        /// `KeyCombo` carries control and shift and no Alt, so the table
        /// lists no Alt chord.
        #[test]
        fn every_row_of_the_table_is_a_chord_the_studio_could_bind() {
            let mut rows = 0;
            for terminal in &super::table().terminals {
                assert!(
                    !terminal.terminal.is_empty(),
                    "a row with no terminal matches every terminal"
                );
                for stolen in &terminal.steals {
                    rows += 1;
                    let parsed = KeyCombo::parse(&stolen.chord).unwrap_or_else(|| {
                        panic!("{}: {:?} is not a chord", terminal.terminal, stolen.chord)
                    });
                    assert_eq!(
                        parsed.key(),
                        stolen.chord,
                        "{}: {:?} is not spelled the way the lookup spells it",
                        terminal.terminal,
                        stolen.chord
                    );
                    assert!(
                        !stolen.does.is_empty(),
                        "{}: {:?} says nothing about what the terminal does with it",
                        terminal.terminal,
                        stolen.chord
                    );
                }
            }
            assert!(rows > 0, "the table came back empty");
            for limitation in super::table().legacy_input.iter().chain(
                super::table()
                    .terminals
                    .iter()
                    .flat_map(|known| &known.legacy_input),
            ) {
                assert_eq!(
                    KeyCombo::parse(&limitation.chord).unwrap().key(),
                    limitation.chord
                );
                assert!(!limitation.reason.is_empty());
                if let Some(received) = &limitation.received {
                    assert_eq!(KeyCombo::parse(received).unwrap().key(), *received);
                }
            }
        }

        #[test]
        fn legacy_f3_decoder_limit_is_distinct_from_a_terminal_steal() {
            let _platform = super::ForcePlatformForTest::set("linux");
            let _desktop = super::ForceDesktopForTest::set("");
            for key in ["shift+f3", "ctrl+f3", "ctrl+shift+f3"] {
                let key = KeyCombo::parse(key).unwrap();
                assert!(super::legacy_input_limitation("iTerm2", &key).is_some());
                assert!(super::steals("iTerm2", &key).is_none());
            }
            for key in ["f3", "shift+f1", "shift+f2", "shift+f4"] {
                assert!(
                    super::legacy_input_limitation("iTerm2", &KeyCombo::parse(key).unwrap())
                        .is_none()
                );
            }
        }

        #[test]
        fn profile_matching_is_case_insensitive_and_respects_name_boundaries() {
            let chord = KeyCombo::parse("ctrl+shift+up").unwrap();
            assert!(super::steals("wezterm 20240203", &chord).is_some());
            assert!(super::steals("tmux via wEzTeRm 20240203", &chord).is_some());
            assert!(super::steals("tmux via kitty", &KeyCombo::parse("ctrl+b").unwrap()).is_some());
            assert!(!super::profile_known("WezTerminal"));
            assert!(!super::profile_known("unknown (xterm-256color)"));
            assert!(!super::profile_known("tmux via made-up"));
            assert!(super::profile_known("tmux via KITTY"));
        }

        #[test]
        fn manual_profiles_keep_detected_multiplexer_conflicts() {
            for (detected, selected, expected) in [
                ("tmux via unknown", Some("KITTY"), "tmux via kitty"),
                ("screen via unknown", Some("kitty"), "screen via kitty"),
                (
                    "tmux 3.5 via screen via unknown",
                    Some("kitty"),
                    "tmux via screen via kitty",
                ),
                ("tmux via unknown", Some("tmux"), "tmux"),
                ("unknown", Some("kitty"), "kitty"),
                ("tmux via unknown", None, "tmux via unknown"),
                (
                    "tmux via unknown",
                    Some("removed profile"),
                    "tmux via unknown",
                ),
            ] {
                assert_eq!(super::effective_profile(detected, selected), expected);
            }
        }

        /// A pin over another known emulator is a mismatch. A pin that keeps
        /// the detected emulator, or that names an unidentified one, is not.
        #[test]
        fn a_pin_over_another_known_emulator_is_a_mismatch() {
            for (detected, selected, expected) in [
                (
                    "kitty 0.35.2",
                    Some("Windows Terminal"),
                    Some("kitty 0.35.2"),
                ),
                (
                    "tmux 3.5 via kitty",
                    Some("Windows Terminal"),
                    Some("kitty"),
                ),
                ("tmux via kitty", Some("tmux"), Some("kitty")),
                ("kitty 0.35.2", Some("KITTY"), None),
                ("tmux via kitty", Some("kitty"), None),
                ("kitty", None, None),
                ("kitty", Some("removed profile"), None),
                ("unknown (xterm-256color)", Some("kitty"), None),
                ("tmux via unknown", Some("kitty"), None),
                ("VTE", Some("Tilix"), None),
                ("Windows Console", Some("Alacritty"), None),
                ("", Some("kitty"), None),
            ] {
                assert_eq!(
                    super::pinned_over(detected, selected),
                    expected,
                    "{detected:?} with {selected:?}"
                );
            }
        }

        #[test]
        fn shifted_tab_conflicts_match_crossterms_backtab_encoding() {
            for key in ["ctrl+shift+tab", "ctrl+backtab", "ctrl+shift+backtab"] {
                assert!(
                    super::steals("Windows Terminal", &KeyCombo::parse(key).unwrap()).is_some(),
                    "{key}"
                );
            }
            assert!(
                super::steals("Windows Terminal", &KeyCombo::parse("backtab").unwrap()).is_none()
            );
        }

        #[test]
        fn macos_system_shortcuts_apply_even_without_a_known_terminal() {
            let _platform = super::ForcePlatformForTest::set("macos");
            for key in [
                "ctrl+space",
                "ctrl+f1",
                "ctrl+f2",
                "ctrl+f3",
                "ctrl+f4",
                "ctrl+f5",
                "ctrl+f6",
                "ctrl+f7",
                "ctrl+f8",
                "ctrl+shift+f6",
                "f11",
                "ctrl+up",
                "ctrl+down",
            ] {
                let chord = KeyCombo::parse(key).unwrap();
                assert!(super::steals("unknown", &chord).is_some(), "{key}");
                assert!(super::steals("kitty", &chord).is_some(), "{key}");
            }
            assert!(super::steals("Ghostty", &KeyCombo::parse("ctrl+enter").unwrap()).is_none());
            {
                let _windows = super::ForcePlatformForTest::set("windows");
                assert!(super::steals("unknown", &KeyCombo::parse("ctrl+f2").unwrap()).is_none());
                assert!(
                    super::steals("Ghostty", &KeyCombo::parse("ctrl+enter").unwrap()).is_some()
                );
            }
            assert!(super::steals("unknown", &KeyCombo::parse("ctrl+f2").unwrap()).is_some());
            for desktop in &super::table().platforms {
                assert!(!desktop._source.is_empty());
                for row in &desktop.steals {
                    assert_eq!(KeyCombo::parse(&row.chord).unwrap().key(), row.chord);
                }
            }
        }

        #[test]
        fn terminal_profiles_reserve_app_shortcuts_but_not_normal_screen_scrolling() {
            let _platform = super::ForcePlatformForTest::set("linux");
            let _desktop = super::ForceDesktopForTest::set("");
            for (terminal, key) in [
                ("Guake", "f12"),
                ("Yakuake", "f12"),
                ("Contour", "ctrl+shift+n"),
                ("kitty", "ctrl+shift+p"),
                ("kitty", "ctrl+shift+."),
                ("kitty", "ctrl+shift+,"),
            ] {
                assert!(
                    super::steals(terminal, &KeyCombo::parse(key).unwrap()).is_some(),
                    "{terminal}: {key} belongs to the terminal"
                );
            }
            for terminal in ["GNOME Terminal", "VTE", "Guake"] {
                for key in ["shift+home", "shift+end", "shift+pageup", "shift+pagedown"] {
                    assert!(
                        super::steals(terminal, &KeyCombo::parse(key).unwrap()).is_none(),
                        "{terminal}: {key} reaches Studio in the alternate screen"
                    );
                }
            }
        }

        #[test]
        fn plasma_conflicts_require_the_detected_desktop() {
            let detect = |pairs: &[(&str, &str)]| {
                super::desktop_from_env(|key| {
                    pairs
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| value.to_string())
                })
            };
            assert_eq!(detect(&[("XDG_CURRENT_DESKTOP", "KDE")]), "plasma");
            assert_eq!(detect(&[("XDG_CURRENT_DESKTOP", "other:Plasma")]), "plasma");
            assert_eq!(detect(&[("KDE_FULL_SESSION", "true")]), "plasma");
            assert_eq!(detect(&[("XDG_CURRENT_DESKTOP", "GNOME")]), "");
            assert_eq!(detect(&[("XDG_CURRENT_DESKTOP", "XFCE")]), "xfce");
            assert_eq!(detect(&[("KDE_FULL_SESSION", "false")]), "");
            let _platform = super::ForcePlatformForTest::set("linux");
            let _desktop = super::ForceDesktopForTest::set("plasma");
            for key in [
                "ctrl+f1", "ctrl+f2", "ctrl+f3", "ctrl+f4", "ctrl+f7", "ctrl+f9", "ctrl+f10",
                "ctrl+f12", "ctrl+esc",
            ] {
                let chord = KeyCombo::parse(key).unwrap();
                assert!(super::steals("unknown", &chord).is_some(), "{key}");
                assert!(super::steals("Konsole", &chord).is_some(), "{key}");
            }
            {
                let _other = super::ForceDesktopForTest::set("");
                assert!(super::steals("Konsole", &KeyCombo::parse("ctrl+f1").unwrap()).is_none());
            }
            let _windows = super::ForcePlatformForTest::set("windows");
            assert!(super::steals("unknown", &KeyCombo::parse("ctrl+f1").unwrap()).is_none());
        }

        /// The record-sample chord starts and stops a recording during play,
        /// so it must arrive in every terminal and on every desktop this
        /// table knows. F-keys do not: a Mac laptop sends Volume Up for F12,
        /// and IDE terminals keep F-keys. The chord is a Ctrl letter, and
        /// this test checks that no row takes it.
        #[test]
        fn the_record_sample_chord_is_taken_by_no_known_terminal_or_desktop() {
            let chord = KeyCombo::parse("ctrl+h").unwrap();
            for platform in ["windows", "macos", "linux", "freebsd"] {
                let _platform = super::ForcePlatformForTest::set(platform);
                for desktop in ["", "plasma", "xfce"] {
                    let _desktop = super::ForceDesktopForTest::set(desktop);
                    assert_eq!(
                        super::steals("unknown", &chord),
                        None,
                        "{platform}/{desktop}"
                    );
                    for terminal in &super::table().terminals {
                        assert_eq!(
                            super::steals(&terminal.terminal, &chord),
                            None,
                            "{platform}/{desktop}: {} takes ^H",
                            terminal.terminal
                        );
                    }
                }
            }
        }

        /// VS Code's terminal on Windows sends ^Enter and ^. as their bare
        /// keys unless win32 input mode is switched on, so update and stop
        /// must not be offered on them there.
        #[test]
        fn vscode_on_windows_does_not_deliver_ctrl_enter() {
            let _platform = super::ForcePlatformForTest::set("windows");
            for key in ["ctrl+enter", "ctrl+."] {
                let chord = KeyCombo::parse(key).unwrap();
                assert!(super::steals("vscode 1.140.0", &chord).is_some(), "{key}");
            }
            let _linux = super::ForcePlatformForTest::set("linux");
            assert!(
                super::steals("vscode 1.140.0", &KeyCombo::parse("ctrl+enter").unwrap()).is_none()
            );
        }

        #[test]
        fn platform_specific_conflicts_do_not_hide_control_keys_on_macos() {
            let ghostty = super::table()
                .terminals
                .iter()
                .find(|row| row.terminal == "Ghostty")
                .unwrap();
            let fullscreen = ghostty
                .steals
                .iter()
                .find(|row| row.chord == "ctrl+enter")
                .unwrap();
            assert!(super::applies_on(fullscreen, "linux"));
            assert!(!super::applies_on(fullscreen, "macos"));
            let tab = ghostty
                .steals
                .iter()
                .find(|row| row.chord == "ctrl+tab")
                .unwrap();
            assert!(super::applies_on(tab, "macos"));
        }

        #[test]
        fn pointer_modifier_reset_follows_the_detected_terminal_layers() {
            for terminal in [
                "Ghostty",
                "ghostty 1.2.3",
                "tmux 3.5 via Ghostty 1.2.3",
                "screen via tmux via Ghostty",
            ] {
                assert!(super::pointer_modifier_reset(terminal), "{terminal}");
            }
            for terminal in ["kitty", "iTerm2", "tmux", "Ghosttyish", "unknown"] {
                assert!(!super::pointer_modifier_reset(terminal), "{terminal}");
            }
        }

        /// A version stamped after the name still finds its row, and a
        /// terminal nobody has written down answers None rather than
        /// pretending it takes nothing.
        #[test]
        fn a_terminal_is_matched_by_name_and_an_unknown_one_says_so() {
            let chord = KeyCombo::parse("ctrl+shift+up").expect("a chord");
            assert!(super::steals("WezTerm 20240203-110809", &chord).is_some());
            assert!(super::steals("Windows Terminal", &chord).is_some());
            assert_eq!(super::steals("some-terminal-from-2031", &chord), None);

            let free = KeyCombo::parse("shift+f9").expect("a chord");
            assert_eq!(
                super::steals("WezTerm 20240203-110809", &free),
                None,
                "the family the panels use is free on the terminals we checked"
            );
        }
    }
}

/// Compatibility name for callers concerned specifically with stolen keys.
/// The underlying table now describes the complete terminal profile.
pub use profiles as conflicts;

/// Whether the studio may spell a decorative glyph outright, set once by
/// `TerminalFeatures::detect` and read back by `symbol` - the one place
/// this is asked from drawing code with no `TerminalFeatures` of its own
/// to keep, such as the mixer and device panels, which carry only a
/// `Theme`. A studio process detects once, but the test suite runs many
/// detections in parallel on one process; a shared atomic would let one
/// test's legacy console tofu another test's glyphs, so `cfg(test)` keeps
/// this thread-local instead, the same reasoning `graphics::tier` does.
#[cfg(not(test))]
mod symbol_state {
    use std::sync::atomic::{AtomicBool, Ordering};

    static SUPPORTED: AtomicBool = AtomicBool::new(true);

    pub fn get() -> bool {
        SUPPORTED.load(Ordering::Relaxed)
    }

    pub fn set(supported: bool) {
        SUPPORTED.store(supported, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod symbol_state {
    use std::cell::Cell;

    thread_local! {
        static SUPPORTED: Cell<bool> = const { Cell::new(true) };
    }

    pub fn get() -> bool {
        SUPPORTED.with(Cell::get)
    }

    pub fn set(supported: bool) {
        SUPPORTED.with(|supported_cell| supported_cell.set(supported));
    }
}

/// The conhost-safe stand-in for `fancy`. One table: a grep for the
/// fancy glyph finds every site that can draw it, and a grep for the
/// stand-in finds what conhost sees instead. Each is width-1, like the
/// glyph it replaces, so a row's columns still line up.
fn stand_in(fancy: &'static str) -> &'static str {
    let mut characters = fancy.chars();
    match (characters.next(), characters.next()) {
        (Some(only), None) => match stand_in_char(only) {
            Some(plain) => plain,
            None => fancy,
        },
        _ => fancy,
    }
}

/// The table itself, one glyph at a time. Each stand-in is width-1, like
/// the glyph it replaces, so a row's columns still line up.
fn stand_in_char(fancy: char) -> Option<&'static str> {
    Some(match fancy {
        '⇧' => "↑", // ^ already spells Ctrl; the arrow still reads as Shift.
        '▸' | '▶' | '▷' | '»' | '↳' => ">",
        '◀' => "<",
        '▮' => "|",
        '▏' | '▕' => "|",
        '▾' => "v",
        '⟲' => "<", // scene rewind; a single cell in conhost too.
        '⟳' => "*", // Generate; keep its accented action marker in conhost.
        '✓' => "+",
        '✗' => "x",
        '⚠' => "!",
        '◐' | '○' | '◌' => "o",
        '⇄' => "~",
        '⇣' => "v",
        '♪' | '♩' | '♫' => "~",
        '⚙' => "*",
        '▁' | '▂' => "_",
        '▃' | '▅' | '▆' | '▇' => "#",
        '╲' => "\\",
        '╱' => "/",
        '▪' => "*",
        '▫' => ".",
        '▼' => "v",
        '▲' => "^",
        '◆' => "*",
        '•' => "\u{b7}",
        '∘' | '°' => "o",
        '⠋' | '⠙' | '⠸' | '⠴' => "*",
        '⌁' => "~",
        '◇' => "o",
        '▣' => "#",
        '✦' => "*",
        '∙' => "\u{b7}", // the same dot, in the codepoint Consolas has.
        '●' => "\u{b7}", // Consolas lacks the filled circle; keep a visible dot.
        '◉' => "o",      // The hollow pulse frame must be safe too, or it blinks as tofu.
        _ => return None,
    })
}

/// `fancy` where this terminal can be trusted with it, its conhost-safe
/// stand-in otherwise. Every drawing site that spells one of the
/// studio's decorative glyphs goes through here instead of branching on
/// the capability itself.
pub fn symbol(fancy: &'static str) -> &'static str {
    if symbol_state::get() {
        fancy
    } else {
        stand_in(fancy)
    }
}

/// A whole line of already-written text, made safe.
///
/// `symbol` is for a drawing site that spells one glyph and can be handed
/// the table's answer instead. This is for the sentences that were written
/// with the glyph inside them - a keyboard reference's `^⇧L`, a status
/// line's `⇧←/→` - where threading the table through would mean taking
/// every sentence apart. Borrowed and untouched in the ordinary case, so
/// a terminal that draws the glyphs pays nothing for this.
pub fn safe_text(text: &str) -> std::borrow::Cow<'_, str> {
    if symbol_state::get() || !text.chars().any(|c| stand_in_char(c).is_some()) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match stand_in_char(character) {
            Some(plain) => out.push_str(plain),
            None => out.push(character),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Apply terminal safety and the active symbol profile to a completed frame.
///
/// This is the render choke point after every widget and theme has drawn.
/// Unsafe terminal characters are replaced for every terminal profile; the
/// decorative-glyph fallback is needed only on consoles without those glyphs.
pub fn sanitize_buffer(buffer: &mut Buffer) {
    let supports_symbols = symbol_state::get();
    for cell in &mut buffer.content {
        if cell.symbol().chars().any(is_unsafe_terminal_character) {
            let escaped = cell
                .symbol()
                .chars()
                .map(|character| {
                    if is_unsafe_terminal_character(character) {
                        control_picture(character)
                    } else {
                        character
                    }
                })
                .collect::<String>();
            cell.set_symbol(&safe_text(&escaped));
        } else if !supports_symbols {
            let plain = safe_text(cell.symbol());
            if let std::borrow::Cow::Owned(plain) = plain {
                cell.set_symbol(&plain);
            }
        }
    }
}

/// Forces the capability for the life of the guard, restoring what it
/// was before on drop - so a render test that turns conhost's tofu on
/// cannot leak it into the next test sharing this thread.
#[cfg(test)]
#[must_use]
pub(crate) struct ForceSymbolsForTest(bool);

#[cfg(test)]
impl ForceSymbolsForTest {
    pub(crate) fn set(supported: bool) -> Self {
        let previous = symbol_state::get();
        symbol_state::set(supported);
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for ForceSymbolsForTest {
    fn drop(&mut self) {
        symbol_state::set(self.0);
    }
}

/// What the answers to the startup queries said.
#[derive(Debug, Default, PartialEq, Eq)]
struct Answers {
    sync_output: bool,
    kitty_graphics: bool,
    sixel: bool,
    /// XTWINOPS 16: one cell in pixels, `CSI 6 ; height ; width t`.
    cell_pixels: Option<(u16, u16)>,
    pixel_mouse: bool,
}

/// Read complete replies independently of their arrival order. ConPTY can
/// answer local queries before the outer terminal's forwarded replies arrive.
fn parse_answers(bytes: &[u8]) -> Answers {
    let text = String::from_utf8_lossy(bytes);
    let mut answers = Answers::default();
    for tail in text.split("\x1b[").skip(1) {
        let Some(end) = tail.bytes().position(|byte| (0x40..=0x7e).contains(&byte)) else {
            continue;
        };
        let reply = &tail[..=end];
        // DECRQM: set, reset and permanently set permit using the feature.
        // Permanently reset cannot enable synchronized output or pixel mouse.
        if let Some(state) = reply
            .strip_prefix("?2026;")
            .and_then(|reply| reply.strip_suffix("$y"))
        {
            answers.sync_output |= matches!(state, "1" | "2" | "3");
        }
        if let Some(state) = reply
            .strip_prefix("?1016;")
            .and_then(|reply| reply.strip_suffix("$y"))
        {
            answers.pixel_mouse |= matches!(state, "1" | "2" | "3");
        }
        // XTWINOPS 16 reports cell height then width, independently of pty
        // window dimensions, which native Windows does not expose.
        if let Some(size) = reply
            .strip_prefix("6;")
            .and_then(|reply| reply.strip_suffix('t'))
        {
            let mut parts = size.split(';');
            let height = parts.next().and_then(|part| part.parse::<u16>().ok());
            let width = parts.next().and_then(|part| part.parse::<u16>().ok());
            if let (Some(height), Some(width), None) = (height, width, parts.next())
                && height > 0
                && width > 0
            {
                answers.cell_pixels = Some((width, height));
            }
        }
        // DA1 attribute 4 advertises sixel. A later DECRQM must not hide it.
        if let Some(attributes) = reply
            .strip_prefix('?')
            .and_then(|reply| reply.strip_suffix('c'))
        {
            answers.sixel |= attributes.split(';').any(|attribute| attribute == "4");
        }
    }
    // A complete response to our harmless one-pixel query proves Kitty
    // protocol support even when the terminal refuses the sample payload.
    answers.kitty_graphics = text.split("\x1b_G").skip(1).any(|tail| {
        tail.split_once("\x1b\\")
            .is_some_and(|(reply, _)| reply.starts_with("i=31;"))
    });
    answers
}

#[cfg(any(unix, windows))]
const TERMINAL_CAPABILITY_QUERY: &[u8] =
    b"\x1b[?2026$p\x1b[?1016$p\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";

/// A wait as poll(2)'s whole milliseconds, rounded up. A floor would turn
/// the last sub-millisecond of each paced wait into a zero-timeout poll,
/// and the loop would spin until the frame is due.
#[cfg(unix)]
fn poll_millis(wait: std::time::Duration) -> i32 {
    i32::try_from(wait.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX)
}

#[cfg(unix)]
fn ask_terminal() -> Option<Vec<u8>> {
    use std::os::unix::io::AsRawFd;
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return None;
    }
    let fd = stdin.as_raw_fd();
    // The question goes to the TERMINAL, never to wherever stdout points:
    // with `--probe-terminal > file` the query used to land in the file
    // and the terminal, never asked, never answered.
    let mut tty = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(TERMINAL_CAPABILITY_QUERY).ok()?;
    tty.flush().ok()?;
    let mut answers = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, and a timeout in milliseconds.
        let ready = unsafe { libc::poll(&mut poll, 1, poll_millis(remaining)) };
        if ready <= 0 {
            break;
        }
        let mut chunk = [0u8; 512];
        // SAFETY: reading into a buffer we own, of the length we pass.
        let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if read <= 0 {
            break;
        }
        answers.extend_from_slice(&chunk[..read as usize]);
        // DA1's answer ends in `c`; once it is here the rest has arrived.
        if let Some(start) = answers.iter().rposition(|&b| b == b'[')
            && answers.get(start + 1) == Some(&b'?')
            && answers[start..].contains(&b'c')
        {
            break;
        }
    }
    Some(answers)
}

#[cfg(not(unix))]
fn ask_terminal() -> Option<Vec<u8>> {
    None
}

#[cfg(windows)]
fn windows_cell_query(input: &mut windows_input::WindowsInput) -> io::Result<Option<(u16, u16)>> {
    windows_query(input, false).map(|answers| answers.cell_pixels)
}

#[cfg(windows)]
fn windows_query(
    input: &mut windows_input::WindowsInput,
    capabilities: bool,
) -> io::Result<Answers> {
    // Startup probes borrow VT input; active pixel sessions retain it so a
    // resize cannot split mouse gestures across ConPTY's two input decoders.
    let temporary = !input.vt_input_active();
    if temporary && let Err(error) = input.enable_vt_input() {
        if error.kind() == io::ErrorKind::Unsupported {
            return Ok(Answers::default());
        }
        return Err(error);
    }
    let result = windows_query_inner(input, capabilities);
    let restored = if temporary {
        input.restore_native_input()
    } else {
        Ok(())
    };
    match (result, restored) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(size), Ok(())) => Ok(size),
    }
}

#[cfg(windows)]
fn windows_query_inner(
    input: &mut windows_input::WindowsInput,
    capabilities: bool,
) -> io::Result<Answers> {
    // CONOUT$ remains the terminal when --probe-terminal redirects stdout.
    let mut output = std::fs::OpenOptions::new().write(true).open("CONOUT$")?;
    output.write_all(if capabilities {
        TERMINAL_CAPABILITY_QUERY
    } else {
        b"\x1b[16t"
    })?;
    output.flush()?;
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(if capabilities { 400 } else { 150 });
    let mut replies = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(parse_answers(&replies));
        }
        replies.extend(input.collect_query(remaining.min(std::time::Duration::from_millis(10)))?);
        let answers = parse_answers(&replies);
        // ConPTY may answer DA1 itself before the outer terminal answers
        // forwarded graphics/synchronization queries. Keep the full startup
        // window; a resize needs only the first valid cell-size report.
        if !capabilities && answers.cell_pixels.is_some() {
            return Ok(answers);
        }
    }
}

#[cfg(windows)]
fn windows_terminal_input(features: &mut TerminalFeatures) -> Option<windows_input::WindowsInput> {
    if !profiles::supports_windows_pixel_input(&features.name) {
        return None;
    }
    let mut input = windows_input::WindowsInput::new().ok()?;
    input.prepare_console().ok()?;
    features.apply_answers(windows_query(&mut input, true).ok()?);
    features.pixel_mouse = features.cell_pixels.is_some();
    // Keep the reader even if the size query times out: it owns any native
    // keys typed during probing. This session then uses ordinary SGR cells.
    Some(input)
}

/// One cell in pixels, from the window size the terminal reports; zero
/// where it reports none (an old terminal, a multiplexer).
#[cfg(unix)]
pub fn cell_pixels_now() -> Option<(u16, u16)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ fills a winsize we own.
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0;
    if !ok {
        return None;
    }
    cell_pixels_from_window(size.ws_col, size.ws_row, size.ws_xpixel, size.ws_ypixel)
}

#[cfg(unix)]
fn cell_pixels_from_window(columns: u16, rows: u16, width: u16, height: u16) -> Option<(u16, u16)> {
    let cell_width = width.checked_div(columns)?;
    let cell_height = height.checked_div(rows)?;
    (cell_width > 0 && cell_height > 0).then_some((cell_width, cell_height))
}

#[cfg(not(unix))]
pub fn cell_pixels_now() -> Option<(u16, u16)> {
    None
}

/// An entered terminal plus the capabilities negotiated for this session.
pub struct TerminalSession {
    terminal: StudioTerminal,
    enhanced_keyboard: bool,
    features: TerminalFeatures,
    cursor_color: Option<Color>,
    cursor_shape: Option<CaretShape>,
    entered: bool,
    /// The terminal has gone: its window was closed, its ssh session
    /// dropped, its tab hung up. Sticky, because a terminal does not come
    /// back and because nothing may touch the input again once it has -
    /// see [`Self::poll_event`].
    #[cfg(unix)]
    hung_up: std::cell::Cell<bool>,
    #[cfg(windows)]
    windows_input: std::cell::RefCell<Option<windows_input::WindowsInput>>,
    #[cfg(windows)]
    input_cell_pixels: std::cell::Cell<Option<(u16, u16)>>,
}

fn cursor_color_escape(color: Color) -> Option<String> {
    match color {
        Color::Rgb(red, green, blue) => {
            Some(format!("\u{1b}]12;#{red:02x}{green:02x}{blue:02x}\u{1b}\\"))
        }
        _ => None,
    }
}

fn cursor_shape_escape(shape: CaretShape) -> String {
    format!("\u{1b}[{} q", shape.decscusr())
}

/// Asks the terminal to report Shift+click instead of selecting with it.
const SHIFT_CLICK_CAPTURE: &[u8] = b"\x1b[>1s";
/// Returns Shift+click to the terminal's own selection.
const SHIFT_CLICK_RELEASE: &[u8] = b"\x1b[>0s";

fn queue_terminal_entry(
    output: &mut impl Write,
    enhanced_keyboard: bool,
    pixel_mouse: bool,
) -> io::Result<()> {
    queue!(output, EnterAlternateScreen)?;
    if enhanced_keyboard {
        queue!(
            output,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        )?;
    }
    queue!(
        output,
        EnableBracketedPaste,
        EnableFocusChange,
        EnableMouseCapture,
        Clear(ClearType::All),
        MoveTo(0, 0),
        SetCursorStyle::SteadyBar,
        Hide
    )?;
    // After the cell-mode capture, which it refines: the same SGR reports,
    // with the pointer in pixels.
    if pixel_mouse {
        output.write_all(b"\x1b[?1016h")?;
    }
    // XTSHIFTESCAPE: xterm, Ghostty and foot keep a Shift+click for their
    // own selection unless the program asks for it. Shift+click extends the
    // editor's selection, so it has to arrive. The Windows console reads a
    // bare `CSI s` as save-cursor; it gets no request at all.
    if !cfg!(windows) {
        output.write_all(SHIFT_CLICK_CAPTURE)?;
    }
    Ok(())
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the terminal studio needs an interactive stdin and stdout",
            ));
        }

        let terminal = Terminal::new(StudioBackend::new(BufWriter::with_capacity(
            FRAME_OUTPUT_CAPACITY,
            io::stdout(),
        )))?;
        enable_raw_mode()?;

        // Set `entered` before emitting any protocol sequence so a partial
        // setup is still unwound if a later write fails.
        let mut session = Self {
            terminal,
            enhanced_keyboard: false,
            features: TerminalFeatures::default(),
            cursor_color: None,
            cursor_shape: None,
            entered: true,
            #[cfg(unix)]
            hung_up: std::cell::Cell::new(false),
            #[cfg(windows)]
            windows_input: std::cell::RefCell::new(None),
            #[cfg(windows)]
            input_cell_pixels: std::cell::Cell::new(None),
        };
        session.enhanced_keyboard = matches!(supports_keyboard_enhancement(), Ok(true));
        session.features = TerminalFeatures::detect(session.enhanced_keyboard);
        #[cfg(windows)]
        {
            let input = windows_terminal_input(&mut session.features);
            session.input_cell_pixels.set(session.features.cell_pixels);
            *session.windows_input.get_mut() = input;
        }
        let mut stdout = io::stdout();

        queue_terminal_entry(
            &mut stdout,
            session.enhanced_keyboard,
            session.features.pixel_mouse && !cfg!(windows),
        )?;
        #[cfg(windows)]
        if let Some(input) = session.windows_input.get_mut().as_mut() {
            // Crossterm's Windows mouse capture replaces the console mode.
            // Retain full key records alongside the original SGR coordinates.
            input.prepare_console()?;
            if session.features.pixel_mouse
                && let Err(error) = input.enable_vt_input()
            {
                if error.kind() != io::ErrorKind::Unsupported {
                    return Err(error);
                }
                session.features.pixel_mouse = false;
            }
            stdout.write_all(b"\x1b[?1002h\x1b[?1003h\x1b[?1006h")?;
            if session.features.pixel_mouse {
                stdout.write_all(b"\x1b[?1016h")?;
            }
            stdout.flush()?;
            input.set_pixel_mouse(if session.features.pixel_mouse {
                session.features.cell_pixels
            } else {
                None
            });
        }
        stdout.flush()?;
        Ok(session)
    }

    pub fn terminal_mut(&mut self) -> &mut StudioTerminal {
        &mut self.terminal
    }

    /// Apply the active theme's caret colour through xterm's OSC 12. This is
    /// understood by modern terminal engines and ignored as one complete OSC
    /// command by older consoles. Avoid repeating it on every frame.
    pub fn set_cursor_color(&mut self, color: Color) -> io::Result<()> {
        if self.cursor_color == Some(color) {
            return Ok(());
        }
        self.cursor_color = Some(color);
        let backend = self.terminal.backend_mut();
        match cursor_color_escape(color) {
            Some(sequence) => backend.write_all(sequence.as_bytes())?,
            None => backend.write_all(b"\x1b]112\x1b\\")?,
        }
        backend.flush()
    }

    /// Apply the resolved settings/theme shape once when it changes. The
    /// sequence is DECSCUSR; unsupported terminals ignore it as a CSI command.
    pub fn set_cursor_shape(&mut self, shape: CaretShape) -> io::Result<()> {
        let shape = shape.steady();
        if self.cursor_shape == Some(shape) {
            return Ok(());
        }
        self.cursor_shape = Some(shape);
        let backend = self.terminal.backend_mut();
        backend.write_all(cursor_shape_escape(shape).as_bytes())?;
        backend.flush()
    }

    /// Whether the terminal has gone - the window closed, the tab hung
    /// up, the connection dropped.
    ///
    /// Sticky once seen. The studio watches it to leave the way ^Q does:
    /// silence, then what is owed to disk, then out.
    #[cfg(unix)]
    pub fn hung_up(&self) -> bool {
        self.hung_up.get()
    }

    #[cfg(not(unix))]
    pub fn hung_up(&self) -> bool {
        false
    }

    /// `POLLHUP` on the input, asked with no wait at all.
    #[cfg(unix)]
    fn note_hangup(&self) -> bool {
        if self.hung_up.get() {
            return true;
        }
        let mut watch = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one `pollfd` is handed over and one is described; poll
        // writes `revents` and reads nothing else.
        let ready = unsafe { libc::poll(&mut watch, 1, 0) };
        let gone =
            ready < 0 || watch.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
        if gone {
            self.hung_up.set(true);
        }
        gone
    }

    /// Whether an event is waiting, giving up after `wait`.
    ///
    /// The wait is ours rather than crossterm's, because crossterm 0.29
    /// cannot survive a terminal that goes away mid-wait. Its reader
    /// handles `WouldBlock` and `Interrupted` and falls through every
    /// other error, so the `EIO` from a closed pty keeps it in its inner
    /// loop forever. The spin takes two cores, and the music plays on
    /// with no window left to stop it.
    ///
    /// So this function looks for the hangup first and does the blocking
    /// itself. On Unix it asks crossterm only with a zero timeout, after
    /// the hangup check.
    pub fn poll_event(&self, wait: std::time::Duration) -> io::Result<bool> {
        #[cfg(windows)]
        if let Some(input) = self.windows_input.borrow_mut().as_mut() {
            return input.poll(wait);
        }
        #[cfg(unix)]
        {
            if self.note_hangup() {
                return Ok(false);
            }
            // Whatever it has already parsed - a key of a paste it read
            // in one gulp - before anybody waits on the fd for more.
            if crossterm::event::poll(std::time::Duration::ZERO)? {
                return Ok(true);
            }
            let millis = poll_millis(wait);
            let mut watch = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: as `note_hangup`.
            let ready = unsafe { libc::poll(&mut watch, 1, millis) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    return Ok(false);
                }
                self.hung_up.set(true);
                return Ok(false);
            }
            if watch.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                self.hung_up.set(true);
                return Ok(false);
            }
            if watch.revents & libc::POLLIN == 0 {
                return Ok(false);
            }
            crossterm::event::poll(std::time::Duration::ZERO)
        }
        #[cfg(not(unix))]
        crossterm::event::poll(wait)
    }

    pub fn read_event(&self) -> io::Result<crossterm::event::Event> {
        #[cfg(windows)]
        if let Some(input) = self.windows_input.borrow_mut().as_mut() {
            let event = input.read()?;
            if self.features.pixel_mouse
                && matches!(event, crossterm::event::Event::Resize(..))
                && let Some(size) = windows_cell_query(input)?
            {
                input.set_pixel_mouse(Some(size));
                self.input_cell_pixels.set(Some(size));
            }
            return Ok(event);
        }
        crossterm::event::read()
    }

    /// New native input metrics, kept with the reader that owns query replies.
    pub fn input_cell_pixels(&self) -> Option<(u16, u16)> {
        #[cfg(windows)]
        return self.input_cell_pixels.get();
        #[cfg(not(windows))]
        None
    }

    /// True when modified Enter/punctuation are reported distinctly instead
    /// of collapsing into their unmodified legacy bytes. Windows console
    /// input preserves these modifiers natively; Unix terminals need the
    /// negotiated progressive keyboard protocol.
    pub fn precise_modified_keys(&self) -> bool {
        cfg!(windows) || self.enhanced_keyboard
    }

    /// Held computer-piano keys require real releases, not a terminal-name
    /// guess or a user-selected shortcut profile. Native console records
    /// report them even in Conhost; other readers need the negotiated protocol.
    pub fn reports_key_releases(&self) -> bool {
        #[cfg(windows)]
        if self.windows_input.borrow().is_some() {
            return true;
        }
        self.enhanced_keyboard
    }

    pub fn features(&self) -> &TerminalFeatures {
        &self.features
    }

    /// Open a synchronized update, where the terminal supports them: the
    /// frame is shown once it is complete, never half-drawn.
    pub fn begin_frame(&mut self) {
        if self.features.sync_output {
            let _ = self.terminal.backend_mut().write_all(b"\x1b[?2026h");
        }
    }

    /// Close the frame; pictures for it go out first, inside the update.
    pub fn end_frame(&mut self, images: &[super::graphics::PixelImage]) {
        let backend = self.terminal.backend_mut();
        if self.features.kitty_graphics {
            let _ = super::graphics::write_kitty_frame(images, backend);
        }
        if self.features.sync_output {
            let _ = backend.write_all(b"\x1b[?2026l");
        }
        let _ = backend.flush();
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if !self.entered {
            return;
        }
        let backend = self.terminal.backend_mut();
        if self.enhanced_keyboard {
            let _ = queue!(backend, PopKeyboardEnhancementFlags);
        }
        // The pointer shape is the terminal's state, not ours to keep: an
        // I-beam left behind over the shell would outlive the studio.
        let _ = std::io::Write::write_all(backend, b"\x1b]22;\x1b\\");
        // Nor is the theme's caret colour. Restore the terminal profile's
        // cursor before returning to the shell.
        let _ = std::io::Write::write_all(backend, b"\x1b]112\x1b\\");
        if self.features.pixel_mouse {
            let _ = std::io::Write::write_all(backend, b"\x1b[?1016l");
        }
        if !cfg!(windows) {
            let _ = std::io::Write::write_all(backend, SHIFT_CLICK_RELEASE);
        }
        #[cfg(windows)]
        let mut windows_input = self.windows_input.get_mut().take();
        #[cfg(windows)]
        if windows_input.is_some() {
            let _ = backend.write_all(b"\x1b[?1006l\x1b[?1003l\x1b[?1002l");
        }
        let _ = execute!(
            backend,
            Show,
            SetCursorStyle::DefaultUserShape,
            DisableMouseCapture,
            DisableFocusChange,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = backend.flush();
        #[cfg(windows)]
        if let Some(input) = windows_input.as_mut() {
            // Mouse capture restores the mode it saw after our probe. Put
            // back the reader's earlier mode before leaving raw input.
            let _ = input.restore_input_mode();
        }
        let _ = disable_raw_mode();
        #[cfg(windows)]
        if let Some(input) = windows_input.as_mut() {
            // ANSI output must remain enabled until the shell screen is back.
            let _ = input.restore_mode();
        }
        self.entered = false;
    }
}

#[cfg(test)]
pub(crate) mod cursor_trace {
    //! Where a terminal could show its cursor while a frame's bytes arrive.

    use std::cell::RefCell;
    use std::io::{self, Write};
    use std::rc::Rc;

    /// Stdout for a test: everything written, until [`Capture::take`].
    #[derive(Clone, Default)]
    pub(crate) struct Capture(Rc<RefCell<Vec<u8>>>);

    impl Capture {
        /// The bytes written since the last call.
        pub(crate) fn take(&self) -> Vec<u8> {
            std::mem::take(&mut *self.0.borrow_mut())
        }
    }

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A terminal's cursor as the frame writer drives it: CUP moves it, DECTCEM
    /// shows and hides it, and each printed character advances it one column.
    /// Other CSI sequences and OSC, APC and DCS strings leave it in place. It
    /// starts hidden at the top-left, where the studio's entry leaves it.
    #[derive(Default)]
    pub(crate) struct Cursor {
        visible: bool,
        x: u16,
        y: u16,
    }

    impl Cursor {
        /// Every cell the cursor is visible on after some byte of `stream`, in
        /// order and without repeats. A write can end after any byte, and a
        /// terminal without synchronized output may paint between two writes.
        pub(crate) fn visible_cells(&mut self, stream: &[u8]) -> Vec<(u16, u16)> {
            let text = String::from_utf8_lossy(stream);
            let mut chars = text.chars().peekable();
            let mut seen = Vec::new();
            while let Some(c) = chars.next() {
                if c != '\x1b' {
                    self.x = self.x.saturating_add(1);
                } else {
                    match chars.next() {
                        Some('[') => {
                            let mut parameters = String::new();
                            let mut last = ' ';
                            for d in chars.by_ref() {
                                if ('\x40'..='\x7e').contains(&d) {
                                    last = d;
                                    break;
                                }
                                parameters.push(d);
                            }
                            self.control(last, &parameters);
                        }
                        Some(']' | '_' | 'P') => {
                            while let Some(d) = chars.next() {
                                if d == '\x07' || (d == '\x1b' && chars.next_if_eq(&'\\').is_some())
                                {
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if self.visible && seen.last() != Some(&(self.x, self.y)) {
                    seen.push((self.x, self.y));
                }
            }
            seen
        }

        fn control(&mut self, last: char, parameters: &str) {
            match (last, parameters) {
                ('h', "?25") => self.visible = true,
                ('l', "?25") => self.visible = false,
                ('H', position) => {
                    let mut parts = position
                        .split(';')
                        .map(|part| part.parse::<u16>().unwrap_or(1).max(1) - 1);
                    self.y = parts.next().unwrap_or(0);
                    self.x = parts.next().unwrap_or(0);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod frame_safety_tests {
    #[test]
    fn a_completed_frame_sanitizes_terminal_controls_with_symbols_enabled() {
        use ratatui::{buffer::Buffer, layout::Rect};

        let _forced = super::ForceSymbolsForTest::set(true);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        buffer.content[0].set_symbol("\u{1b}]52;c;payload\u{7}");
        buffer.content[1].set_symbol("a\u{202e}b");
        buffer.content[2].set_symbol("\u{009b}31m");
        buffer.content[3].set_symbol("▶");

        super::sanitize_buffer(&mut buffer);

        assert_eq!(buffer.content[0].symbol(), "␛]52;c;payload␇");
        assert_eq!(buffer.content[1].symbol(), "a�b");
        assert_eq!(buffer.content[2].symbol(), "�31m");
        assert_eq!(buffer.content[3].symbol(), "▶");
    }

    #[test]
    fn a_completed_frame_sanitizes_terminal_controls_on_legacy_consoles() {
        use ratatui::{buffer::Buffer, layout::Rect};

        let _forced = super::ForceSymbolsForTest::set(false);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        buffer.content[0].set_symbol("\u{1b}[31m▶");
        buffer.content[1].set_symbol("x\u{2067}y");

        super::sanitize_buffer(&mut buffer);

        assert_eq!(buffer.content[0].symbol(), "␛[31m>");
        assert_eq!(buffer.content[1].symbol(), "x�y");
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn buffered_frames_preserve_draw_and_graphics_output_boundaries() {
        use std::{cell::RefCell, rc::Rc};

        use ratatui::{TerminalOptions, Viewport, layout::Rect};

        #[derive(Clone, Default)]
        struct Output(Rc<RefCell<(Vec<u8>, usize)>>);

        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let mut output = self.0.borrow_mut();
                output.0.extend_from_slice(bytes);
                output.1 += 1;
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        fn frame(writer: impl Write, mut output: Output, area: Rect) -> (Vec<Vec<u8>>, usize) {
            let mut terminal = Terminal::with_options(
                StudioBackend::new(writer),
                TerminalOptions {
                    viewport: Viewport::Fixed(area),
                },
            )
            .unwrap();
            terminal.backend_mut().write_all(b"\x1b[?2026h").unwrap();
            terminal
                .draw(|frame| {
                    for y in 0..area.height {
                        for x in 0..area.width {
                            frame.buffer_mut()[(x, y)]
                                .set_symbol("█")
                                .set_fg(Color::Rgb(x as u8, y as u8, 128));
                        }
                    }
                })
                .unwrap();
            let drawn = output.0.borrow().0.clone();
            // The app writes the pointer through a separate stdout handle
            // after draw, so all cell output must already have been flushed.
            output.write_all(b"\x1b]22;text\x1b\\").unwrap();
            super::super::graphics::write_kitty_frame(&[], terminal.backend_mut()).unwrap();
            let image = super::super::graphics::PixelImage::inline(
                Rect::new(1, 1, 1, 1),
                1,
                1,
                vec![255, 128, 64, 255],
            );
            super::super::graphics::write_kitty_frame(&[image], terminal.backend_mut()).unwrap();
            terminal.backend_mut().write_all(b"\x1b[?2026l").unwrap();
            terminal.backend_mut().flush().unwrap();
            let (finished, writes) = output.0.borrow().clone();
            (vec![drawn, finished], writes)
        }

        // Exercise both a small frame and one that must spill the buffer
        // before draw ends. Neither may change a byte or defer frame output.
        for area in [Rect::new(0, 0, 16, 2), Rect::new(0, 0, 300, 90)] {
            let direct = Output::default();
            let (expected, direct_writes) = frame(direct.clone(), direct, area);
            let buffered = Output::default();
            let (actual, buffered_writes) = frame(
                BufWriter::with_capacity(FRAME_OUTPUT_CAPACITY, buffered.clone()),
                buffered,
                area,
            );
            assert_eq!(actual, expected);
            assert!(buffered_writes * 10 < direct_writes);
        }
    }

    /// A frame shows the terminal's cursor on the caret only, after every byte
    /// it sends: while its cells are rewritten away from the caret, when nothing
    /// changed, and not at all when it has no caret.
    #[test]
    fn frames_show_the_cursor_only_on_the_caret() {
        use ratatui::{TerminalOptions, Viewport, layout::Rect};

        let capture = super::cursor_trace::Capture::default();
        let area = Rect::new(0, 0, 40, 8);
        let mut terminal = Terminal::with_options(
            StudioBackend::new(capture.clone()),
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )
        .unwrap();
        let mut cursor = super::cursor_trace::Cursor::default();
        let caret = (4, 3);
        for (shade, caret) in [
            (0, Some(caret)),
            (1, Some(caret)),
            (1, Some(caret)),
            (2, None),
        ] {
            terminal
                .draw(|frame| {
                    for position in area.positions() {
                        frame.buffer_mut()[position]
                            .set_symbol("x")
                            .set_fg(Color::Rgb(position.x as u8, position.y as u8, shade));
                    }
                    if let Some(caret) = caret {
                        frame.set_cursor_position(caret);
                    }
                })
                .unwrap();
            assert_eq!(
                cursor.visible_cells(&capture.take()),
                Vec::from_iter(caret),
                "shade {shade}, caret {caret:?}"
            );
        }
    }

    #[test]
    fn cursor_colour_uses_osc_12_for_rgb_and_reset_for_terminal_colours() {
        use ratatui::style::Color;

        assert_eq!(
            super::cursor_color_escape(Color::Rgb(255, 204, 0)).as_deref(),
            Some("\u{1b}]12;#ffcc00\u{1b}\\")
        );
        assert_eq!(super::cursor_color_escape(Color::White), None);
    }

    #[test]
    fn blinking_carets_change_phase_even_while_the_terminal_keeps_painting() {
        use super::CaretShape;
        use std::time::Duration;
        for (blinking, steady) in [
            (CaretShape::BlinkingBar, CaretShape::SteadyBar),
            (CaretShape::BlinkingBlock, CaretShape::SteadyBlock),
            (CaretShape::BlinkingUnderline, CaretShape::SteadyUnderline),
        ] {
            assert_eq!(blinking.steady(), steady);
            for (ms, visible) in [
                (0, true),
                (499, true),
                (500, false),
                (999, false),
                (1000, true),
            ] {
                assert_eq!(blinking.visible_after(Duration::from_millis(ms)), visible);
                assert!(steady.visible_after(Duration::from_millis(ms)));
            }
        }
    }

    #[test]
    fn every_caret_shape_uses_its_decscusr_parameter() {
        use super::CaretShape;

        for (shape, parameter) in [
            (CaretShape::BlinkingBlock, 1),
            (CaretShape::SteadyBlock, 2),
            (CaretShape::BlinkingUnderline, 3),
            (CaretShape::SteadyUnderline, 4),
            (CaretShape::BlinkingBar, 5),
            (CaretShape::SteadyBar, 6),
        ] {
            assert_eq!(
                super::cursor_shape_escape(shape),
                format!("\u{1b}[{parameter} q")
            );
        }
        assert_eq!(CaretShape::default(), CaretShape::SteadyBar);
    }

    #[cfg(unix)]
    #[test]
    fn a_poll_wait_rounds_up_to_whole_milliseconds() {
        use std::time::Duration;
        assert_eq!(super::poll_millis(Duration::from_micros(16_667)), 17);
        assert_eq!(super::poll_millis(Duration::from_micros(500)), 1);
        assert_eq!(super::poll_millis(Duration::ZERO), 0);
        assert_eq!(super::poll_millis(Duration::from_secs(u64::MAX)), i32::MAX);
    }

    #[test]
    fn terminal_identity_uses_current_term_and_names_major_emulators() {
        let identify = |pairs: &[(&str, &str)], windows| {
            super::identity_from_env(
                |key| {
                    pairs
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| value.to_string())
                },
                windows,
            )
        };
        for (pairs, expected) in [
            (
                vec![
                    ("TERM", "xterm-kitty"),
                    ("TERM_PROGRAM", "WezTerm"),
                    ("WT_SESSION", "old"),
                ],
                "kitty",
            ),
            (
                vec![("TERM", "foot-extra"), ("KITTY_WINDOW_ID", "old")],
                "foot",
            ),
            (vec![("TERM_PROGRAM", "Apple_Terminal")], "Apple Terminal"),
            (vec![("TERM_PROGRAM", "iTerm.app")], "iTerm2"),
            (vec![("TERM", "xterm-ghostty")], "Ghostty"),
            (
                vec![("GNOME_TERMINAL_SCREEN", "one"), ("VTE_VERSION", "8000")],
                "GNOME Terminal",
            ),
            (vec![("VTE_VERSION", "8000")], "VTE"),
            (
                vec![("GUAKE_TAB_UUID", "tab"), ("VTE_VERSION", "8000")],
                "Guake",
            ),
            (vec![("WT_SESSION", "one")], "Windows Terminal"),
            (
                vec![
                    ("TERM_PROGRAM", "vscode"),
                    ("TERM_PROGRAM_VERSION", "1.140.0"),
                ],
                "vscode 1.140.0",
            ),
            // An IDE started from another terminal inherits its variables;
            // the IDE's own terminal is the one whose keys decide.
            (
                vec![
                    ("TERMINAL_EMULATOR", "JetBrains-JediTerm"),
                    ("WT_SESSION", "inherited"),
                ],
                "JetBrains",
            ),
            (
                vec![
                    ("TERM_PROGRAM", "vscode"),
                    ("KONSOLE_VERSION", "260801"),
                    ("VTE_VERSION", "8000"),
                ],
                "vscode",
            ),
            (vec![("TILIX_ID", "one"), ("VTE_VERSION", "8000")], "Tilix"),
            (
                vec![("TERMINATOR_UUID", "urn:uuid:one"), ("VTE_VERSION", "8000")],
                "Terminator",
            ),
            (
                vec![("GUAKE_TAB_UUID", "one"), ("VTE_VERSION", "8000")],
                "Guake",
            ),
            (
                vec![("TERM_PROGRAM", "guake"), ("VTE_VERSION", "8000")],
                "Guake",
            ),
            (
                vec![("PTYXIS_VERSION", "50.2"), ("VTE_VERSION", "8000")],
                "Ptyxis 50.2",
            ),
            (
                vec![
                    ("TERM_PROGRAM", "kgx"),
                    ("TERM_PROGRAM_VERSION", "51.0"),
                    ("VTE_VERSION", "8000"),
                ],
                "GNOME Console 51.0",
            ),
            (vec![("XTERM_VERSION", "XTerm(400)")], "xterm XTerm(400)"),
            (vec![("TERM", "xterm-256color")], "unknown (xterm-256color)"),
        ] {
            assert_eq!(identify(&pairs, false), expected);
        }
        assert_eq!(identify(&[], true), "Windows Console");
    }

    #[test]
    fn multiplexers_do_not_inherit_the_outer_terminals_capabilities() {
        let identify = |pairs: &[(&str, &str)]| {
            super::identity_from_env(
                |key| {
                    pairs
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| value.to_string())
                },
                false,
            )
        };
        let name = identify(&[
            ("TERM", "tmux-256color"),
            ("TMUX", "one"),
            ("KITTY_WINDOW_ID", "old"),
        ]);
        assert_eq!(name, "tmux via kitty");
        assert_eq!(super::active_identity(&name), "tmux");
        assert_eq!(
            identify(&[
                ("TERM", "screen"),
                ("STY", "one"),
                ("TERM_PROGRAM", "WezTerm")
            ]),
            "screen via WezTerm"
        );
        assert_eq!(
            identify(&[("TERM", "screen-256color"), ("TMUX", "one")]),
            "tmux"
        );
        assert_eq!(
            identify(&[
                ("TERM", "xterm-kitty"),
                ("TMUX", "old"),
                ("STY", "old"),
                ("TERM_PROGRAM", "tmux")
            ]),
            "kitty"
        );
    }

    #[test]
    fn windows_pixel_input_uses_the_profiles_release_floor() {
        for name in [
            "WezTerm 20220319-142410-0fcdea07",
            "WezTerm 20240203-110809-5046fc22",
            "wezterm 20260908-000000-example",
        ] {
            assert!(
                super::profiles::supports_windows_pixel_input(name),
                "{name}"
            );
        }
        for name in [
            "WezTerm",
            "WezTerm 20220101-000000-example",
            "WezTerm unknown",
            "Windows Terminal",
            "kitty 0.35.2",
            "not-wezterm 20240203",
        ] {
            assert!(
                !super::profiles::supports_windows_pixel_input(name),
                "{name}"
            );
        }
    }

    #[test]
    fn rendering_capabilities_come_from_terminal_profiles() {
        let console = super::profiles::capabilities("Windows Console");
        assert!(!console.symbols);
        assert!(!console.truecolor);
        assert!(!console.fine_glyphs);

        let windows_terminal = super::profiles::capabilities("Windows Terminal");
        assert!(windows_terminal.symbols);
        assert!(windows_terminal.truecolor);

        let kitty = super::profiles::capabilities("kitty 0.40.1");
        assert!(kitty.symbols);
        assert!(kitty.truecolor);
        assert!(kitty.fine_glyphs);

        let unknown = super::profiles::capabilities("some-terminal-from-2031");
        assert!(unknown.symbols);
        assert!(!unknown.truecolor);
        assert!(!unknown.fine_glyphs);
    }

    #[test]
    fn the_stand_in_table_is_used_only_when_symbols_are_off() {
        assert_eq!(super::symbol("▸"), "▸");
        let _forced = super::ForceSymbolsForTest::set(false);
        assert_eq!(super::symbol("▸"), ">");
        assert_eq!(super::symbol("↳"), ">");
        assert_eq!(super::safe_text("  ↳ g 1 ▸ o 0"), "  > g 1 > o 0");
        assert_eq!(super::symbol("⇧"), "↑");
        assert_eq!(super::symbol("▮"), "|");
        assert_eq!(super::symbol("▏"), "|");
        assert_eq!(super::symbol("▕"), "|");
        assert_eq!(super::symbol("▾"), "v");
        assert_eq!(super::symbol("⌁"), "~");
        assert_eq!(super::symbol("◇"), "o");
        assert_eq!(super::symbol("▣"), "#");
        assert_eq!(super::symbol("✦"), "*");
        assert_eq!(super::symbol("∙"), "\u{b7}");
        assert_eq!(super::symbol("●"), "\u{b7}");
        assert_eq!(super::symbol("◉"), "o");
        assert_eq!(super::symbol("▶"), ">");
        assert_eq!(super::symbol("▷"), ">");
        assert_eq!(super::symbol("⚙"), "*");
        assert_eq!(super::symbol("⟳"), "*");
        assert_eq!(super::symbol("▁"), "_");
        assert_eq!(super::symbol("⚠"), "!");
        assert_eq!(super::symbol("♪"), "~");
        // Anything not in the table is trusted regardless - the glyphs
        // Consolas already has never had to route through here.
        assert_eq!(super::symbol("·"), "·");
        drop(_forced);
        assert_eq!(super::symbol("▸"), "▸", "the guard restores what it forced");
    }

    #[test]
    fn a_completed_conhost_frame_cannot_leave_known_tofu_glyphs_behind() {
        use ratatui::{buffer::Buffer, layout::Rect, style::Style};

        let _forced = super::ForceSymbolsForTest::set(false);
        let area = Rect::new(0, 0, 24, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "▶ ▁ ⚙ ✗ ⚠ ♪ ⠋", Style::default());
        super::sanitize_buffer(&mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.starts_with("> _ * x ! ~ *"), "{text:?}");
        assert!(!text.chars().any(|glyph| {
            matches!(glyph, '▶' | '▁' | '⚙' | '✗' | '⚠' | '♪' | '⠋')
        }));
    }

    #[test]
    fn pixel_mouse_uses_the_terminals_reported_cell_size() {
        let mut features = super::TerminalFeatures {
            cell_pixels: Some((10, 20)),
            ..Default::default()
        };
        features.apply_answers(super::parse_answers(b"\x1b[?1016;2$y\x1b[6;18;9t"));
        assert_eq!(features.cell_pixels, Some((9, 18)));
        assert!(features.pixel_mouse);
    }

    #[test]
    fn pixel_mouse_requires_both_support_and_a_cell_size() {
        let mut features = super::TerminalFeatures::default();
        features.apply_answers(super::parse_answers(b"\x1b[?1016;2$y"));
        assert!(!features.pixel_mouse);

        // The pty remains a fallback when XTWINOPS has no answer.
        features.cell_pixels = Some((9, 18));
        features.apply_answers(super::parse_answers(b"\x1b[?1016;2$y"));
        assert!(features.pixel_mouse);
        assert_eq!(features.cell_pixels, Some((9, 18)));

        features.apply_answers(super::parse_answers(b"\x1b[?1016;0$y\x1b[6;18;9t"));
        assert!(!features.pixel_mouse);
    }

    #[cfg(unix)]
    #[test]
    fn invalid_window_dimensions_do_not_enable_pixel_coordinates() {
        assert_eq!(
            super::cell_pixels_from_window(80, 24, 720, 432),
            Some((9, 18))
        );
        for dimensions in [
            (0, 24, 720, 432),
            (80, 0, 720, 432),
            (80, 24, 0, 432),
            (80, 24, 720, 0),
            (80, 24, 79, 432),
            (80, 24, 720, 23),
        ] {
            let (columns, rows, width, height) = dimensions;
            assert_eq!(
                super::cell_pixels_from_window(columns, rows, width, height),
                None,
                "{dimensions:?}"
            );
        }
    }

    /// XTWINOPS 16 answers `CSI 6 ; height ; width t`. The probe reads it,
    /// so a terminal whose pty carries no pixel sizes (WSL under a Windows
    /// terminal) can still report the size of a cell.
    #[test]
    fn the_cell_size_answer_is_read_out_of_the_probe() {
        let answers = super::parse_answers(b"\x1b[?2026;2$y\x1b[6;19;9t\x1b[?62;4c");
        assert_eq!(answers.cell_pixels, Some((9, 19)));
        assert!(answers.sixel);

        let none = super::parse_answers(b"\x1b[?62c");
        assert_eq!(none.cell_pixels, None);
        assert!(!none.pixel_mouse);
        // DECRQM 1016: set, reset and permanently set all mean the pointer
        // can come in pixels; unknown and permanently reset mean it cannot.
        for (reply, expected) in [
            ("1", true),
            ("2", true),
            ("3", true),
            ("0", false),
            ("4", false),
        ] {
            let bytes = format!("\x1b[?1016;{reply}$y\x1b[?62c");
            assert_eq!(
                super::parse_answers(bytes.as_bytes()).pixel_mouse,
                expected,
                "{reply}"
            );
        }
        // A zero dimension is no answer.
        let zero = super::parse_answers(b"\x1b[6;0;9t");
        assert_eq!(zero.cell_pixels, None);
    }

    use super::*;

    #[cfg(unix)]
    #[test]
    fn keyboard_enhancement_is_enabled_inside_the_alternate_screen() {
        let mut bytes = Vec::new();
        queue_terminal_entry(&mut bytes, true, true).expect("queue terminal entry");
        let output = String::from_utf8(bytes).expect("terminal sequences are ASCII");
        // Alternate characters are required for shifted text in iTerm2;
        // event types retain piano releases. Test the actual wire request.
        assert!(output.contains("\x1b[>15u"));
        let alternate = output.find("\x1b[?1049h").expect("alternate screen");
        let keyboard = output.find("\x1b[>").expect("keyboard enhancement");
        let paste = output.find("\x1b[?2004h").expect("bracketed paste");
        assert!(alternate < keyboard);
        assert!(keyboard < paste);
        // Pixels refine the cell-mode capture, so they are asked for after it.
        let capture = output.find("\x1b[?1006h").expect("mouse capture");
        let pixels = output.find("\x1b[?1016h").expect("pixel mouse");
        assert!(capture < pixels);
        // Shift+click extends the editor's selection only if it arrives.
        let shift = output.find("\x1b[>1s").expect("shift click capture");
        assert!(capture < shift);

        let mut bytes = Vec::new();
        queue_terminal_entry(&mut bytes, false, false).expect("queue terminal entry");
        let output = String::from_utf8(bytes).expect("terminal sequences are ASCII");
        assert!(
            !output.contains("\x1b[?1016h"),
            "pixels asked of a terminal without them"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capability_detection_never_requests_terminal_identity() {
        assert!(
            !TERMINAL_CAPABILITY_QUERY
                .windows(5)
                .any(|sequence| sequence == b"\x1b[>0q")
        );
    }

    #[test]
    fn the_terminals_answers_are_read() {
        let bytes = b"\x1b[?2026;2$y\x1b_Gi=31;OK\x1b\\\x1b[?62;4;22c";
        let answers = parse_answers(bytes);
        assert_eq!(
            answers,
            Answers {
                sync_output: true,
                kitty_graphics: true,
                sixel: true,
                cell_pixels: None,
                pixel_mouse: false,
            }
        );
        // An older terminal answers DA1 only.
        assert_eq!(parse_answers(b"\x1b[?1;2c"), Answers::default());
        // Graphics refused with an error is still a terminal that speaks it.
        assert!(parse_answers(b"\x1b_Gi=31;EINVAL:x\x1b\\\x1b[?62c").kitty_graphics);
        let plain = TerminalFeatures::default();
        assert_eq!(plain.default_tier(), super::super::graphics::Tier::Cells);
        assert!(plain.summary().contains("truecolor ✗"));
    }

    #[test]
    fn forwarded_capability_replies_can_arrive_after_conptys_local_answers() {
        let replies =
            b"\x1b[?2026;0$y\x1b[?1;2c\x1b[6;23;11t\x1b_Gi=31;OK\x1b\\\x1b[?62;4c\x1b[?2026;2$y";
        let answers = parse_answers(replies);
        assert!(answers.sync_output);
        assert!(answers.kitty_graphics);
        assert!(answers.sixel, "a later mode reply cannot hide DA1");
        assert_eq!(answers.cell_pixels, Some((11, 23)));
        let mut features = TerminalFeatures::default();
        features.apply_answers(answers);
        assert!(features.sync_output && features.kitty_graphics && features.sixel);
        assert!(
            !features.pixel_mouse,
            "graphics replies do not prove pixel input"
        );
    }

    #[test]
    fn truncated_or_unrelated_capability_replies_do_not_enable_features() {
        assert_eq!(parse_answers(b"\x1b[?2026;2"), Answers::default());
        assert_eq!(parse_answers(b"\x1b[?2026;4$y"), Answers::default());
        assert_eq!(parse_answers(b"\x1b_Gi=31;OK"), Answers::default());
        assert_eq!(
            parse_answers(b"\x1b_Gi=32;OK\x1b\\\x1b[?2026;20$y\x1b[6;20;10;99t"),
            Answers::default()
        );
    }
}
