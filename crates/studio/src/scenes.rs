//! A set: up to sixteen scores, each its own file, one of them on screen.
//!
//! A scene is nothing more than a `.strudel` file in the set's folder. That
//! keeps the studio's one contract intact - the file is what you hear - so
//! saving, git, `--watch`, replay and every other tool see ordinary scores.
//! Switching scenes swaps the editor on screen; playing one is the same
//! evaluate as always. There is no second format for the music.
//!
//! What the folder gains is one small project file, `rustel-set.json`, that
//! remembers the order of the scenes and which MIDI pad launches each. It
//! is written only once a set actually has something to remember - a second
//! scene, or a learnt pad - so a lone score opened in the studio stays a
//! lone score. Once it exists it is the authority: only the scenes it lists
//! belong to the set, so a folder of stray scores never grows a strip. It
//! also carries the set's own prebake, the setup JavaScript its scores are
//! written against.
//!
//! A prebake tab is not a scene. It edits like one and sits on the same
//! strip, at the right-hand end, but it has no file of its own, no number,
//! no pad and no launch, and it never counts toward the sixteen - so every
//! rule below that treats a scene as a score file asks first.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::bounded_file::{self, MAX_DOCUMENT_BYTES, ReadError};
use super::editor::{Editor, EditorError};
use super::minimap::Minimap;
use super::pads::Pad;
use super::prebake::PrebakeScope;
use rustel_runtime::ui_events::source_revision;

/// Scenes a set may hold. Sixteen is what a pad controller offers a hand.
pub const MAX_SCENES: usize = 16;
/// Files the studio treats as scenes.
pub const SCENE_EXTENSION: &str = "strudel";
/// The project file beside a set's scores.
pub const SET_FILE_NAME: &str = "rustel-set.json";
/// The largest set file the studio reads. Its prebake may be a whole editor
/// document, and JSON writes a control character as six bytes, so this is
/// six documents plus room for the rest of the set.
const MAX_SET_FILE_BYTES: u64 = 6 * MAX_DOCUMENT_BYTES + 1024 * 1024;

/// Stable identity of a scene for the life of the studio, unaffected by
/// renames, reordering or closing other scenes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct SceneId(pub u64);

/// A save the studio is still waiting on.
#[derive(Clone, Debug)]
pub struct PendingSave {
    pub request_id: u64,
    pub source_revision: String,
}

/// What a tab holds: the music, or the setup that runs before it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneKind {
    /// A `.strudel` file in the set's folder.
    Score,
    /// Setup JavaScript, kept where its scope says and never as a score file.
    Prebake(PrebakeScope),
    /// A recorded set opened to replay: the tape's saves along the top, one
    /// of them in the editor. Its path is the tape.
    Replay,
}

pub struct Scene {
    pub id: SceneId,
    pub kind: SceneKind,
    /// A score's file. For a prebake, the file its text is kept in, which
    /// the header shows and the save worker never sees.
    pub path: PathBuf,
    pub editor: Editor,
    pub minimap: Minimap,
    /// Hash of the text on disk, or empty for a scene never written.
    pub saved_source_revision: String,
    pub dirty: bool,
    pub latest_save: Option<PendingSave>,
    /// The MIDI pad that launches this scene.
    pub pad: Option<Pad>,
    /// Whether playing this scene rewinds it to its own cycle zero
    /// instead of joining the cycle already running.
    ///
    /// A property of the score rather than of the key that plays it: a
    /// one-cycle loop does not care where it joins, and an `<a b>` or a
    /// four-cycle phrase is heard from the middle of itself when it
    /// joins wherever the clock happens to stand. So the scene carries
    /// the answer and every way of playing it - a pad, ^S, a click -
    /// reads the same one.
    pub rewind: bool,
}

impl Scene {
    fn new(
        id: SceneId,
        kind: SceneKind,
        path: PathBuf,
        source: &str,
        persisted: bool,
    ) -> Result<Self, EditorError> {
        Ok(Self {
            id,
            kind,
            path,
            editor: Editor::new(source)?,
            minimap: Minimap::default(),
            saved_source_revision: if persisted {
                source_revision(source)
            } else {
                String::new()
            },
            dirty: !persisted,
            latest_save: None,
            pad: None,
            rewind: false,
        })
    }

    /// What the chip shows: a score's file stem, what a prebake is, or the
    /// tape a replay plays, without the `session-` every tape's name starts
    /// with.
    pub fn name(&self) -> String {
        match self.kind {
            SceneKind::Prebake(scope) => scope.tab_name(),
            SceneKind::Score => self
                .path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            SceneKind::Replay => replay_name(&self.path),
        }
    }

    /// A tape opened to replay rather than a score.
    pub fn is_replay(&self) -> bool {
        matches!(self.kind, SceneKind::Replay)
    }

    /// Music rather than setup: the question every file rule asks.
    pub fn is_score(&self) -> bool {
        matches!(self.kind, SceneKind::Score)
    }

    pub fn prebake(&self) -> Option<PrebakeScope> {
        match self.kind {
            SceneKind::Prebake(scope) => Some(scope),
            SceneKind::Score | SceneKind::Replay => None,
        }
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Recompute `dirty` from the text against disk and any pending write.
    pub fn refresh_dirty(&mut self) {
        let current = source_revision(&self.editor.source());
        self.dirty = current != self.saved_source_revision
            || self
                .latest_save
                .as_ref()
                .is_some_and(|pending| pending.source_revision != current);
    }

    /// Whether this score has text that is not in its file: a write failed,
    /// or a write is not complete.
    pub fn unsaved(&self) -> bool {
        self.is_score() && (self.dirty || self.latest_save.is_some())
    }
}

/// What a set says about the master limiter.
///
/// A limiter is part of a sound - a ceiling and a character are as much a
/// part of how a set plays as its samples - so a set can carry its own.
/// It is also a house preference, so a set that has never been asked can
/// decline to have an opinion and take the studio's.
///
/// Three states and not two, because "nobody has said" and "the answer
/// was no" are different answers to a settings sheet that later changes
/// its mind: a set whose limiter was taken off the desk must not have one
/// handed back the next time the studio's default is switched on.
///
/// ```text
/// set file text     state           resolve()         has_slot(on)
/// (absent)          Defer           None (studio's)   on
/// "off"             None            Some(None)        false
/// "-1.0:warm"       Says            Some(Some(..))    true
/// "off:-1.0:warm"   Says, bypassed  Some(None)        true
/// anything else     Unreadable      Some(None)        false
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SetLimiter {
    /// No opinion: whatever the settings sheet says a set starts with.
    /// A set carried to another machine plays with that machine's.
    #[default]
    Defer,
    /// No limiter on this set's desk, whatever the studio's default says.
    /// What removing the strip leaves behind.
    None,
    /// A limiter of its own: a ceiling, a character, and whether it is
    /// bypassed at the moment.
    ///
    /// Bypassed is not the same as saying nothing - a later change to the
    /// studio's default must not turn this set's limiter on - and it is
    /// not the same as having no limiter either. The desk's bypass is a
    /// switch, so what it switches off has to still be there to switch
    /// back on: turning it off to hear the difference and on again must
    /// give back the ceiling and the character it had, not a default.
    Says {
        bypassed: bool,
        settings: rustel_audio::LimiterSettings,
    },
    /// The file said something this build cannot read - a newer rustel's
    /// character, or a line a sync client cut in half.
    ///
    /// Played as off, because a limiter is opted into and a string nobody
    /// can read is not an opt-in. Written back verbatim: the file is the
    /// only copy, and being unable to read something is not a reason to
    /// overwrite it with a guess. Opening a set or changing tab rewrites
    /// the file, so a build that normalised this field would destroy it.
    Unreadable(String),
}

impl SetLimiter {
    /// What the set file said, read.
    ///
    /// `off` alone is a set with no limiter on its desk - which is what a
    /// set written before the strip could be removed meant by it too.
    pub fn read(stored: Option<&str>) -> Self {
        let Some(text) = stored else {
            return Self::Defer;
        };
        let text = text.trim();
        let (bypassed, rest) = match text.strip_prefix(BYPASS_PREFIX) {
            Some(rest) => (true, rest),
            None if text == OFF_LIMITER => return Self::None,
            None => (false, text),
        };
        super::settings::parse_master_limiter(rest).map_or_else(
            || Self::Unreadable(text.to_owned()),
            |settings| Self::Says { bypassed, settings },
        )
    }

    /// What to write back, or `None` for a set with nothing to say.
    pub fn stored(&self) -> Option<String> {
        match self {
            Self::Defer => None,
            Self::None => Some(OFF_LIMITER.to_owned()),
            Self::Says { bypassed, settings } => {
                let held = super::settings::write_master_limiter(Some(*settings));
                Some(if *bypassed {
                    format!("{BYPASS_PREFIX}{held}")
                } else {
                    held
                })
            }
            Self::Unreadable(text) => Some(text.clone()),
        }
    }

    /// The limiter to play with, or `None` to take the studio's.
    ///
    /// Unreadable resolves like a bypass and not like `Defer`: the set
    /// said something, so it is not deferring, and what it said cannot be
    /// honoured - which leaves the answer a limiter is opted into.
    pub fn resolve(&self) -> Option<Option<rustel_audio::LimiterSettings>> {
        match self {
            Self::Defer => None,
            Self::None | Self::Unreadable(_) => Some(None),
            Self::Says { bypassed, settings } => Some((!bypassed).then_some(*settings)),
        }
    }

    /// The ceiling and character this set holds, bypassed or not - what a
    /// bypass switches back on, and what the desk edits.
    pub fn held(&self) -> Option<rustel_audio::LimiterSettings> {
        match self {
            Self::Says { settings, .. } => Some(*settings),
            Self::Defer | Self::None | Self::Unreadable(_) => None,
        }
    }

    /// Whether this set has a limiter on its desk at all - the strip, and
    /// everything the strip does.
    ///
    /// A set that has never said takes the studio's answer, which is the
    /// one place the settings sheet still reaches a set: it decides what
    /// a set opens with, and stops deciding the moment the set is given
    /// one or has its taken away.
    ///
    /// Unreadable has no slot. The file keeps its text either way, so
    /// nothing is lost by not drawing a strip for a limiter nobody can
    /// read - and a strip that could not say its own mode or ceiling
    /// would be a control over a guess.
    pub fn has_slot(&self, studio_default_on: bool) -> bool {
        match self {
            Self::Defer => studio_default_on,
            Self::Says { .. } => true,
            Self::None | Self::Unreadable(_) => false,
        }
    }

    /// Whether this set is bypassing the limiter it has.
    pub fn bypassed(&self) -> bool {
        matches!(self, Self::Says { bypassed: true, .. })
    }
}

/// How a limiter that is deliberately off is written in the preferences,
/// and how a set with no limiter on its desk says so. Absent means
/// "nothing said"; this means "asked for, and the answer was no".
pub const OFF_LIMITER: &str = "off";

/// What a set file puts before a bypassed limiter's ceiling, so that the
/// ceiling and the character survive the switch being off.
const BYPASS_PREFIX: &str = "off:";

/// A fader in tenths of a decibel: what the file keeps.
///
/// Rounded once, here, so the value compared for change is the value that
/// was written. Rounding in two places let a fader at -3.05 read back as
/// -3.0 and count as moved on every save. The range is clamped to what the
/// master's `i16` can hold, which is far outside anything a fader reaches.
fn tenths_of(db: f32) -> i32 {
    if !db.is_finite() {
        return 0;
    }
    (db * 10.0)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i32
}

/// The project file's shape: the tabs open on the strip, in order, with
/// their pads, and the set's own setup. The folder's other scores are
/// not listed; the set panel reads them off the folder.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SetManifest {
    #[serde(default)]
    pub scenes: Vec<SceneEntry>,
    /// The tab that had the caret, by file name: the set opens on it
    /// again. Absent when the set was left on a prebake tab or a tape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// The panes the set was left split into, by file name, left to
    /// right. Absent for one pane, which is most sets - and absent as
    /// well when a pane was on a prebake tab or a tape, which have no
    /// file to name.
    ///
    /// How a set is laid out is part of how it is played: the two scores
    /// you are cutting between are the two you left on screen, and being
    /// given one pane and a hunt through the strip is the set not opening
    /// where you left it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<String>,
    /// Which of those panes had the caret. `current` says the same thing
    /// by name, but two panes can hold the same score, and then the name
    /// cannot say which of them you were in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane: Option<usize>,
    /// The set's own setup JavaScript, verbatim. Absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prebake: Option<String>,
    /// The master limiter this set plays with: `off` for none at all, or
    /// a ceiling and a character as `-1.0:warm`, with `off:` before it
    /// while it is bypassed. Absent when the set has never said -
    /// then the studio's own default applies, and a set carried to
    /// another machine plays with that machine's default rather than
    /// silently bringing one. A limiter is part of a sound, so a set that
    /// has been given one keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limiter: Option<String>,
    /// The master fader this set was left at, in tenths of a decibel.
    ///
    /// A set's loudness is part of how it plays: one mixed for a club and one
    /// mixed to practise against are not played at the same level, and
    /// opening either at 0 dB every time means reaching for the fader before
    /// the first bar, every time. Tenths rather than a float because the file
    /// is compared for change before it is written, and a float that
    /// round-trips through JSON is not reliably equal to itself.
    ///
    /// Absent when the set has never been turned up or down: the fader opens
    /// where it always has, and a set carried to another machine does not
    /// bring a level nobody chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_gain_tenths_db: Option<i16>,
    /// Each audio input's fader, by the device it was set on, in tenths of a
    /// decibel.
    ///
    /// Per device because the right gain belongs to the source, not to the
    /// set: a condenser microphone and a line-level synth want settings tens
    /// of decibels apart, and a set played through first one and then the
    /// other must give each back its own. Keyed by the device's id where it
    /// has one, else its name - the same identity the input picker uses to
    /// find the row that is open.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub input_gains_tenths_db: std::collections::BTreeMap<String, i32>,
    /// The visuals panel's widgets, as set files kept them before the
    /// preferences took them over. Read so a set made then hands its
    /// widgets to the studio once; never written again.
    #[serde(default, skip_serializing)]
    pub widgets: Vec<super::viz_panel::WidgetSpec>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SceneEntry {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad: Option<Pad>,
    /// Whether this scene is played from its own cycle zero. Absent for
    /// the ordinary kind, which is most of them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rewind: bool,
}

impl SetManifest {
    /// The default set file of a folder.
    pub fn path_in(directory: &Path) -> PathBuf {
        directory.join(SET_FILE_NAME)
    }

    /// The default project file beside a set, if there is one that can be
    /// read.
    pub fn load(directory: &Path) -> Option<Self> {
        Self::read(&Self::path_in(directory))
    }

    /// A set file, if the path holds one that can be read.
    pub fn read(path: &Path) -> Option<Self> {
        let text = bounded_file::read_to_string(path, MAX_SET_FILE_BYTES).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The folder's set file. When one exists and cannot be read or parsed,
    /// also where it was moved so that it survives.
    ///
    /// A set file that exists and does not read is not the same thing as
    /// no set file at all: it is the only copy of the set's pads, its tab
    /// list and its prebake. If it were read as absent, the next
    /// `persist_manifest` would write a folder-derived set over it. One
    /// sync conflict marker is enough to cause that. So an unreadable file
    /// is moved aside under `.bad` and the studio says where it went.
    pub fn load_kept(directory: &Path) -> (Option<Self>, KeptManifest) {
        let path = Self::path_in(directory);
        let text = match bounded_file::read_to_string(&path, MAX_SET_FILE_BYTES) {
            Ok(text) => text,
            // No file at all is nothing to keep. A file that exists and does
            // not read as text (cut mid-character by a sync client, a lost
            // permission, or over the size limit) is still the only copy of
            // the set's pads, tab list and prebake. It is moved aside by
            // name: a rename does not have to read it.
            Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return (None, KeptManifest::Nothing);
            }
            Err(_) => return (None, keep_aside(&path)),
        };
        match serde_json::from_str(&text) {
            Ok(manifest) => (Some(manifest), KeptManifest::Nothing),
            Err(_) => (None, keep_aside(&path)),
        }
    }
}

/// What became of a folder's set file that could not be used.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum KeptManifest {
    /// There was no set file to keep.
    #[default]
    Nothing,
    /// It was moved aside, and survives here.
    Kept(PathBuf),
    /// It is there, it could not be read and it could not be moved, so it
    /// must not be written over: it is the only copy of what it holds.
    Stranded(PathBuf),
}

impl KeptManifest {
    /// Where the file survives, when it survives somewhere new.
    pub fn kept(&self) -> Option<&Path> {
        match self {
            Self::Kept(path) => Some(path),
            Self::Nothing | Self::Stranded(_) => None,
        }
    }

    /// The file's path either way, for saying where it is.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Nothing => None,
            Self::Kept(path) | Self::Stranded(path) => Some(path),
        }
    }

    /// Whether a set file is still in the folder's way, and so must not be
    /// written over.
    pub fn blocks_write(&self) -> bool {
        matches!(self, Self::Stranded(_))
    }
}

/// Move a file out of the way rather than over: `rustel-set.json` becomes
/// `rustel-set.json.bad`, and the next one `rustel-set.json.bad 2`, so a
/// folder that keeps arriving broken keeps every copy.
fn keep_aside(path: &Path) -> KeptManifest {
    let mut candidate = path.with_extension("json.bad");
    let mut suffix = 2u32;
    while candidate.exists() {
        candidate = path.with_extension(format!("json.bad {suffix}"));
        suffix += 1;
        // Stop after about a hundred kept copies. The file stays where it
        // is and must not be written over: it is still the only copy of the
        // set's pads, tab list and prebake.
        if suffix > 100 {
            return KeptManifest::Stranded(path.to_path_buf());
        }
    }
    match std::fs::rename(path, &candidate) {
        Ok(()) => KeptManifest::Kept(candidate),
        // A volume that will not let the file move is a volume that must
        // not be written over either.
        Err(_) => KeptManifest::Stranded(path.to_path_buf()),
    }
}

/// A score of the set's folder as the set panel lists it: open on the
/// strip as a scene, or only a file until Enter opens it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetFile {
    pub name: String,
    pub file: String,
    pub path: PathBuf,
    /// The scene showing this file, when one is.
    pub open: Option<SceneId>,
}

#[derive(Debug)]
pub enum SceneError {
    Io(io::Error),
    Editor(EditorError),
    Full,
    Last,
    NameTaken(String),
    BadName(String),
    TooLarge {
        path: PathBuf,
        bytes: usize,
    },
    /// Asked of a prebake tab something only a score can answer.
    Prebake,
    /// Asked to open a folder, or nothing at all, as a score.
    NotAFile(PathBuf),
    /// A set file named where a score or a folder was wanted.
    NotASet(PathBuf),
    /// A set folder cannot be renamed while a take is being written into it.
    Recording,
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Editor(error) => write!(formatter, "{error}"),
            Self::Full => write!(formatter, "a set holds at most {MAX_SCENES} scenes"),
            Self::Last => formatter.write_str("the last scene of a set cannot be closed"),
            Self::NameTaken(name) => write!(formatter, "{name:?} is already in use"),
            Self::BadName(name) => write!(
                formatter,
                "{name:?} is not a scene name; use letters, digits, spaces, '-' or '_'"
            ),
            Self::Prebake => formatter
                .write_str("a prebake is setup, not a scene: it has no file, no pad and no launch"),
            Self::TooLarge { path, bytes } => write!(
                formatter,
                "{} is {bytes} bytes; maximum is {}",
                path.display(),
                super::editor::DEFAULT_MAX_DOCUMENT_BYTES
            ),
            Self::NotAFile(path) => write!(formatter, "{} is not a file", path.display()),
            Self::NotASet(path) => write!(
                formatter,
                "{} is a set file; open its folder instead",
                path.display()
            ),
            Self::Recording => {
                formatter.write_str("a take is being recorded; finish it before renaming the set")
            }
        }
    }
}

impl std::error::Error for SceneError {}

impl From<io::Error> for SceneError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<EditorError> for SceneError {
    fn from(error: EditorError) -> Self {
        Self::Editor(error)
    }
}

/// The set: a folder, its scenes in strip order, and the one on screen.
/// The folder holds the scores, the project file `rustel-set.json`, and
/// the tapes recorded from it under `sessions/`.
pub struct SceneSet {
    directory: PathBuf,
    scenes: Vec<Scene>,
    /// Entries too large to open as tabs, kept for the next set-file write.
    /// Their files remain available in the folder panel and may be repaired.
    oversized: Vec<(usize, SceneEntry)>,
    current: usize,
    next_id: u64,
    /// Whether a project file exists (or has been earned) for this folder.
    has_manifest: bool,
    /// The set's prebake as stored, whether or not a tab is showing it.
    local_prebake: String,
    /// The set's own master limiter, when it has been given one.
    limiter: SetLimiter,
    /// The master fader this set was left at, once it has been moved.
    master_gain_tenths_db: Option<i16>,
    /// Each input's fader, by device.
    input_gains_tenths_db: std::collections::BTreeMap<String, i32>,
    /// The panes the set is laid out in, by file name, left to right, and
    /// which of them has the caret. Empty for one pane.
    ///
    /// Names rather than ids because a set that is opened while another
    /// is playing has its scenes renumbered ([`Self::renumber_from`]), and
    /// a remembered id would then point at somebody else's scene.
    panes: Vec<String>,
    focused_pane: usize,
    /// What became of an unreadable set file. Kept, not taken: a file that
    /// could be neither read nor moved goes on refusing writes for as long
    /// as the set is open.
    kept_manifest: KeptManifest,
    /// Whether the studio has said where that file went, so it says once.
    kept_manifest_said: bool,
    /// The visuals panel's widgets, as the set keeps them.
    widgets: Vec<super::viz_panel::WidgetSpec>,
}

impl SceneSet {
    fn empty(directory: PathBuf) -> Self {
        Self {
            directory,
            scenes: Vec::new(),
            oversized: Vec::new(),
            current: 0,
            next_id: 1,
            has_manifest: false,
            local_prebake: String::new(),
            limiter: SetLimiter::Defer,
            master_gain_tenths_db: None,
            input_gains_tenths_db: std::collections::BTreeMap::new(),
            panes: Vec::new(),
            focused_pane: 0,
            kept_manifest: KeptManifest::default(),
            kept_manifest_said: false,
            widgets: Vec::new(),
        }
    }

    /// What became of this set's unreadable set file. Returns it on the
    /// first call only. The state itself stays: a stranded file must keep
    /// refusing writes.
    pub fn take_kept_manifest(&mut self) -> Option<KeptManifest> {
        if self.kept_manifest_said || self.kept_manifest == KeptManifest::Nothing {
            return None;
        }
        self.kept_manifest_said = true;
        Some(self.kept_manifest.clone())
    }

    /// Whether a set file is still sitting in the folder unwritten-over,
    /// because it could be neither read nor moved.
    pub fn manifest_blocks_write(&self) -> bool {
        self.kept_manifest.blocks_write()
    }

    /// The widgets an older set file kept, handed over once: the studio's
    /// preferences hold the visuals panel's widgets now.
    pub fn take_carried_widgets(&mut self) -> Vec<super::viz_panel::WidgetSpec> {
        std::mem::take(&mut self.widgets)
    }

    fn parent_of(path: &Path) -> PathBuf {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Open a set from a path.
    ///
    /// A folder opens its project file's scenes - the tabs open when the
    /// set was last written - or, without one, every `.strudel` inside it
    /// sorted by name; an empty folder gets a first scene from the starter
    /// text. A file whose folder's project file lists it opens the whole
    /// set with that scene selected; any other file is a one-scene set
    /// whose folder is the file's parent, so a second scene is written
    /// beside it. A missing file becomes an unsaved starter.
    pub fn open(path: &Path, starter: &str) -> Result<Self, SceneError> {
        if path.is_dir() {
            let mut set = Self::empty(path.to_path_buf());
            let (listed, kept) = SetManifest::load_kept(path);
            set.kept_manifest = kept;
            match listed {
                Some(manifest) => set.open_manifest(&manifest, None)?,
                None => set.open_folder()?,
            }
            if set.scenes.is_empty() {
                let file = set.free_path("scene 1");
                set.push(file, starter, false)?;
            }
            return Ok(set);
        }
        // A set file is neither a score nor a set, and neither is one kept
        // aside under `.bad`: opening either would put a tab of broken JSON
        // on the strip under the name `rustel-set.json`.
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            || path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(SET_FILE_NAME))
        {
            return Err(SceneError::NotASet(path.to_path_buf()));
        }
        let directory = Self::parent_of(path);
        let mut set = Self::empty(directory);
        {
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (folder_set, kept) = SetManifest::load_kept(&set.directory);
            set.kept_manifest = kept;
            let listed = folder_set
                .filter(|manifest| manifest.scenes.iter().any(|entry| entry.file == file_name));
            match listed {
                Some(manifest) => set.open_manifest(&manifest, Some(&file_name))?,
                None => match read_score(path) {
                    // Joined onto the folder rather than kept as the
                    // argument was spelled: `folder_files` lists the set by
                    // `read_dir`, which says `./song.strudel` for an
                    // argument of `song.strudel`, and the panel matches an
                    // open score by name while a delete matches it by path.
                    // Left disagreeing, the delete misses the tab and falls
                    // through to removing the file with no last-score check.
                    Ok(source) => set.push(set.directory.join(&file_name), &source, true)?,
                    Err(SceneError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                        set.push(set.directory.join(&file_name), starter, false)?;
                    }
                    Err(error) => return Err(error),
                },
            }
        }
        if set.scenes.is_empty() {
            // Every listed file has gone: the set is its folder, and a set
            // has at least one scene.
            let file = set.free_path("scene 1");
            set.push(file, starter, false)?;
        }
        Ok(set)
    }

    /// A new set: a folder of its own under `root`, named after the day
    /// (`2026-09-06`, then `2026-09-06 b` and so on when the day already
    /// has one), with one starter scene written into it. Nothing is asked:
    /// the set exists and can be renamed later.
    pub fn create_in(root: &Path, day: &str, starter: &str) -> Result<Self, SceneError> {
        std::fs::create_dir_all(root)?;
        let directory = free_set_folder(root, day);
        std::fs::create_dir(&directory)?;
        let mut set = Self::empty(directory);
        // Made by the studio, the folder is a set from the start: its
        // project file is written as soon as there is one to write.
        set.has_manifest = true;
        let file = set.free_path("scene 1");
        std::fs::write(&file, starter)?;
        set.push(file, starter, true)?;
        Ok(set)
    }

    /// Rename the set: its folder moves under the same parent, and every
    /// path the set holds follows it. A tape being written into the
    /// folder is the caller's to refuse over.
    pub fn rename_set(&mut self, name: &str) -> Result<PathBuf, SceneError> {
        let name = name.trim();
        if !valid_name(name) {
            return Err(SceneError::BadName(name.to_owned()));
        }
        let parent = Self::parent_of(&self.directory);
        let target = parent.join(name);
        if self.name() == name {
            return Ok(target);
        }
        if target.exists() {
            return Err(SceneError::NameTaken(name.to_owned()));
        }
        std::fs::rename(&self.directory, &target)?;
        let from = std::mem::replace(&mut self.directory, target.clone());
        for scene in &mut self.scenes {
            if let Ok(rest) = scene.path.strip_prefix(&from) {
                scene.path = target.join(rest);
            }
        }
        Ok(target)
    }

    fn open_manifest(
        &mut self,
        manifest: &SetManifest,
        select: Option<&str>,
    ) -> Result<(), SceneError> {
        self.has_manifest = true;
        self.local_prebake = manifest.prebake.clone().unwrap_or_default();
        self.limiter = SetLimiter::read(manifest.limiter.as_deref());
        self.master_gain_tenths_db = manifest.master_gain_tenths_db;
        self.input_gains_tenths_db = manifest.input_gains_tenths_db.clone();
        self.widgets = manifest.widgets.clone();
        for (position, entry) in manifest.scenes.iter().enumerate() {
            // A set is an ordinary folder people clone, download and share,
            // and a tab's path is what Ctrl+S writes back. `join` with an
            // absolute right-hand side discards the folder altogether, so an
            // entry that is not a bare file name of this set is not a score
            // of it - the theme picker holds a hand-editable, synced path to
            // its own directory for the same reason.
            let name = std::path::Path::new(&entry.file);
            if !name.is_relative()
                || name
                    .parent()
                    .is_some_and(|parent| !parent.as_os_str().is_empty())
            {
                continue;
            }
            let file = self.directory.join(name);
            // More than the strip holds stays a file of the folder.
            if self.score_count() >= MAX_SCENES {
                continue;
            }
            let source = match read_score(&file) {
                Ok(source) => source,
                // A listed score that has gone is not a tab: the folder is
                // the set, and what is not in it is not listed anywhere.
                Err(SceneError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error @ SceneError::TooLarge { .. }) => {
                    // Naming one file explicitly still reports why it could
                    // not open. Opening its set leaves the file and its pad
                    // mapping in place while the other scores become usable.
                    if select == Some(entry.file.as_str()) {
                        return Err(error);
                    }
                    self.oversized.push((position, entry.clone()));
                    continue;
                }
                Err(error) => return Err(error),
            };
            self.push(file, &source, true)?;
            if let Some(scene) = self.scenes.last_mut() {
                scene.pad = entry.pad;
                scene.rewind = entry.rewind;
            }
            if select == Some(entry.file.as_str()) {
                self.current = self.scenes.len() - 1;
            }
        }
        // The layout, once every score it names is on the strip. All or
        // nothing: a pane pointing at a score that has gone is not half a
        // split, it is a set that opens the ordinary way.
        let laid_out = manifest.panes.len() > 1
            && manifest.panes.iter().all(|name| {
                self.scenes
                    .iter()
                    .any(|scene| scene.is_score() && scene.file_name() == *name)
            });
        if laid_out {
            self.panes = manifest.panes.clone();
            self.focused_pane = manifest.focused_pane.unwrap_or(0).min(self.panes.len() - 1);
        }
        // Nothing named: the set opens on the tab it was left on.
        if select.is_none()
            && let Some(current) = manifest.current.as_deref()
            && let Some(index) = self
                .scenes
                .iter()
                .position(|scene| scene.is_score() && scene.file_name() == current)
        {
            self.current = index;
        }
        Ok(())
    }

    fn open_folder(&mut self) -> Result<(), SceneError> {
        let mut files = std::fs::read_dir(&self.directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate.is_file()
                    && candidate
                        .extension()
                        .is_some_and(|extension| extension == SCENE_EXTENSION)
            })
            .collect::<Vec<_>>();
        files.sort_by_key(|candidate| {
            candidate
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
        });
        for (position, file) in files.into_iter().enumerate() {
            if self.score_count() >= MAX_SCENES {
                break;
            }
            let source = match read_score(&file) {
                Ok(source) => source,
                Err(SceneError::TooLarge { .. }) => {
                    self.oversized.push((
                        position,
                        SceneEntry {
                            file: file
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            pad: None,
                            rewind: false,
                        },
                    ));
                    continue;
                }
                Err(error) => return Err(error),
            };
            self.push(file, &source, true)?;
        }
        Ok(())
    }

    fn push(&mut self, path: PathBuf, source: &str, persisted: bool) -> Result<(), SceneError> {
        if self.score_count() >= MAX_SCENES {
            return Err(SceneError::Full);
        }
        let id = SceneId(self.next_id);
        self.next_id += 1;
        self.scenes
            .push(Scene::new(id, SceneKind::Score, path, source, persisted)?);
        Ok(())
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn len(&self) -> usize {
        self.scenes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scenes.is_empty()
    }

    pub fn scenes(&self) -> &[Scene] {
        &self.scenes
    }

    pub fn scenes_mut(&mut self) -> &mut [Scene] {
        &mut self.scenes
    }

    /// How many of them are music. Scores come first and prebake tabs last,
    /// so this is also where the tabs begin.
    pub fn score_count(&self) -> usize {
        self.scenes.iter().filter(|scene| scene.is_score()).count()
    }

    pub fn scores(&self) -> impl Iterator<Item = &Scene> {
        self.scenes.iter().filter(|scene| scene.is_score())
    }

    /// Where a tape's replay tab is on the strip, when one is open.
    pub fn replay_index(&self, path: &Path) -> Option<usize> {
        self.scenes
            .iter()
            .position(|scene| scene.is_replay() && scene.path == path)
    }

    /// The replay tab, if one is open. There is at most one: it shows
    /// whichever tape was chosen last.
    pub fn replay_scene(&self) -> Option<SceneId> {
        self.scenes
            .iter()
            .find(|scene| scene.is_replay())
            .map(|scene| scene.id)
    }

    /// Point the open replay tab at another tape, with that tape's first
    /// block as its text, and select it.
    pub fn retarget_replay(
        &mut self,
        id: SceneId,
        path: &Path,
        text: &str,
    ) -> Result<(), SceneError> {
        let Some(index) = self.index_of(id) else {
            return Err(SceneError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "no replay tab is open",
            )));
        };
        let scene = &mut self.scenes[index];
        if !scene.is_replay() {
            return Err(SceneError::Prebake);
        }
        scene.path = path.to_path_buf();
        scene.editor = Editor::new(text)?;
        // The new document restarts at revision zero, the cache key.
        scene.minimap = Minimap::default();
        scene.saved_source_revision = source_revision(text);
        scene.refresh_dirty();
        self.current = index;
        Ok(())
    }

    /// Show a tape as a replay tab at the end of the strip and select it;
    /// one already open is simply selected. Not a scene: no `MAX_SCENES`
    /// check, no file of the set's, no pad.
    pub fn open_replay(&mut self, path: &Path, text: &str) -> Result<SceneId, SceneError> {
        if let Some(index) = self.replay_index(path) {
            self.current = index;
            return Ok(self.scenes[index].id);
        }
        let id = SceneId(self.next_id);
        self.next_id += 1;
        // Opened clean: the text is the tape's, and an untouched tab closes
        // without a word.
        let scene = Scene::new(id, SceneKind::Replay, path.to_path_buf(), text, true)?;
        self.scenes.push(scene);
        self.current = self.scenes.len() - 1;
        Ok(id)
    }

    /// Where a prebake's tab is on the strip, when one is open.
    pub fn prebake_index(&self, scope: PrebakeScope) -> Option<usize> {
        self.scenes
            .iter()
            .position(|scene| scene.prebake() == Some(scope))
    }

    /// What this set says about the master limiter.
    pub fn limiter(&self) -> &SetLimiter {
        &self.limiter
    }

    /// Say something new about it. Answers whether anything moved, so a
    /// caller can leave the file alone when nothing did.
    pub fn set_limiter(&mut self, limiter: SetLimiter) -> bool {
        let changed = self.limiter != limiter;
        self.limiter = limiter;
        changed
    }

    /// Where this set left the master fader, if it has ever been moved.
    pub fn master_gain_db(&self) -> Option<f32> {
        self.master_gain_tenths_db
            .map(|tenths| f32::from(tenths) / 10.0)
    }

    /// Keep where the master fader is now. Answers whether it moved, so a
    /// caller can leave the file alone when a drag ended where it began.
    pub fn set_master_gain_db(&mut self, db: f32) -> bool {
        let tenths = tenths_of(db) as i16;
        let changed = self.master_gain_tenths_db != Some(tenths);
        self.master_gain_tenths_db = Some(tenths);
        changed
    }

    /// Where this set left the fader for one input device, if it ever did.
    pub fn input_gain_db(&self, device: &str) -> Option<f32> {
        self.input_gains_tenths_db
            .get(device)
            .map(|tenths| *tenths as f32 / 10.0)
    }

    /// Keep one input device's fader. Answers whether it moved.
    pub fn set_input_gain_db(&mut self, device: &str, db: f32) -> bool {
        let tenths = tenths_of(db);
        let changed = self.input_gains_tenths_db.get(device) != Some(&tenths);
        self.input_gains_tenths_db.insert(device.to_owned(), tenths);
        changed
    }

    /// The set's own setup as stored. Empty when it has none.
    pub fn local_prebake(&self) -> &str {
        &self.local_prebake
    }

    /// Keep the set's setup. Nothing at all is kept as nothing, so the
    /// project file stops mentioning a prebake that was emptied.
    pub fn set_local_prebake(&mut self, text: &str) {
        self.local_prebake = if super::prebake::is_blank(text) {
            String::new()
        } else {
            text.to_owned()
        };
    }

    /// Show a prebake as a tab at the end of the strip, global before
    /// local, and select it. One already open is simply selected.
    ///
    /// It is not a scene: no `MAX_SCENES` check, because setup is not one of
    /// the sixteen scores a pad controller offers a hand.
    pub fn open_prebake(&mut self, scope: PrebakeScope, text: &str) -> Result<SceneId, SceneError> {
        if let Some(index) = self.prebake_index(scope) {
            self.current = index;
            return Ok(self.scenes[index].id);
        }
        let path = match scope {
            PrebakeScope::Global => super::prebake::global_path().unwrap_or_default(),
            PrebakeScope::Local => self.manifest_path(),
        };
        let id = SceneId(self.next_id);
        self.next_id += 1;
        // Opened clean: the text came from the store, so an untouched tab
        // closes again without writing anything.
        let scene = Scene::new(id, SceneKind::Prebake(scope), path, text, true)?;
        let at = match scope {
            PrebakeScope::Global => self.score_count(),
            PrebakeScope::Local => self.scenes.len(),
        };
        self.scenes.insert(at, scene);
        self.current = at;
        Ok(id)
    }

    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn current(&self) -> &Scene {
        &self.scenes[self.current]
    }

    pub fn current_mut(&mut self) -> &mut Scene {
        &mut self.scenes[self.current]
    }

    pub fn index_of(&self, id: SceneId) -> Option<usize> {
        self.scenes.iter().position(|scene| scene.id == id)
    }

    pub fn get(&self, id: SceneId) -> Option<&Scene> {
        self.scenes.iter().find(|scene| scene.id == id)
    }

    pub fn get_mut(&mut self, id: SceneId) -> Option<&mut Scene> {
        self.scenes.iter_mut().find(|scene| scene.id == id)
    }

    pub fn by_path(&self, path: &Path) -> Option<SceneId> {
        self.scenes
            .iter()
            .find(|scene| scene.path == path)
            .map(|scene| scene.id)
    }

    fn by_name(&self, name: &str) -> Option<SceneId> {
        self.scores()
            .find(|scene| scene.name().eq_ignore_ascii_case(name))
            .map(|scene| scene.id)
    }

    /// Any scene with unsaved text.
    pub fn any_dirty(&self) -> bool {
        self.scenes.iter().any(|scene| scene.dirty)
    }

    pub fn dirty_count(&self) -> usize {
        self.scenes.iter().filter(|scene| scene.dirty).count()
    }

    /// Make `index` the scene on screen. Returns true when it changed.
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.scenes.len() || index == self.current {
            return false;
        }
        self.current = index;
        true
    }

    /// Move one scene along the strip, wrapping at either end.
    pub fn select_relative(&mut self, delta: isize) -> bool {
        if self.scenes.len() < 2 {
            return false;
        }
        let len = self.scenes.len() as isize;
        let next = (self.current as isize + delta).rem_euclid(len) as usize;
        self.select(next)
    }

    /// A new scene with `source`, written to disk, placed after the current
    /// one and selected. Its file name is the first free `scene N`.
    pub fn create(&mut self, source: &str) -> Result<SceneId, SceneError> {
        if self.score_count() >= MAX_SCENES {
            return Err(SceneError::Full);
        }
        let name = (1..=MAX_SCENES)
            .map(|number| format!("scene {number}"))
            .find(|candidate| self.by_name(candidate).is_none())
            .unwrap_or_else(|| format!("scene {}", self.score_count() + 1));
        let path = self.free_path(&name);
        self.create_at(source, path)
    }

    /// Duplicate the selected score into a sibling named after its source.
    /// Numbered copies continue their sequence instead of becoming
    /// `source 2 2`, and an orphan file is never replaced to claim a name.
    pub fn duplicate_current(&mut self, source: &str) -> Result<SceneId, SceneError> {
        if !self.current().is_score() {
            return Err(SceneError::Prebake);
        }
        if self.score_count() >= MAX_SCENES {
            return Err(SceneError::Full);
        }
        let current_name = self.current().name();
        let (base, first_number) = current_name
            .rsplit_once(' ')
            .and_then(|(base, number)| {
                number
                    .parse::<usize>()
                    .ok()
                    .filter(|number| *number > 0 && !base.is_empty())
                    .and_then(|number| number.checked_add(1).map(|next| (base, next.max(2))))
            })
            .unwrap_or((&current_name, 2));
        let path = (first_number..=usize::MAX)
            .map(|number| {
                let suffix = format!(" {number}");
                let mut end = base.len().min(40 - suffix.len());
                while !base.is_char_boundary(end) {
                    end -= 1;
                }
                let name = format!("{}{}", base[..end].trim_end(), suffix);
                (
                    name.clone(),
                    self.directory.join(format!("{name}.{SCENE_EXTENSION}")),
                )
            })
            .find(|(name, path)| self.by_name(name).is_none() && !path.exists())
            .map(|(_, path)| path)
            .expect("a free numbered scene name");
        self.create_at(source, path)
    }

    fn create_at(&mut self, source: &str, path: PathBuf) -> Result<SceneId, SceneError> {
        // The folder mirrors the strip: a scene exists on disk from the
        // moment it exists on screen.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?
            .write_all(source.as_bytes())?;
        let id = SceneId(self.next_id);
        self.next_id += 1;
        let scene = Scene::new(id, SceneKind::Score, path, source, true)?;
        // After the focused scene, but never among the prebake tabs, which
        // keep the end of the strip.
        let at = (self.current + 1).min(self.score_count());
        self.scenes.insert(at, scene);
        self.current = at;
        Ok(id)
    }

    /// Rename the current scene, and its file if one exists.
    /// A prebake keeps its name: it says which of the two it is, and there
    /// is no third.
    pub fn rename_current(&mut self, name: &str) -> Result<(), SceneError> {
        if !self.scenes[self.current].is_score() {
            return Err(SceneError::Prebake);
        }
        let name = name.trim();
        if !valid_name(name) {
            return Err(SceneError::BadName(name.to_owned()));
        }
        if self.scenes.iter().enumerate().any(|(index, scene)| {
            index != self.current && scene.is_score() && scene.name().eq_ignore_ascii_case(name)
        }) {
            return Err(SceneError::NameTaken(name.to_owned()));
        }
        let scene = &mut self.scenes[self.current];
        if scene.name() == name {
            return Ok(());
        }
        let target = scene
            .path
            .with_file_name(format!("{name}.{SCENE_EXTENSION}"));
        if target.exists() {
            return Err(SceneError::NameTaken(name.to_owned()));
        }
        if scene.path.exists() {
            std::fs::rename(&scene.path, &target)?;
        }
        scene.path = target;
        Ok(())
    }

    /// Close the current scene's tab. The file stays in the folder, where
    /// the set panel lists it, and the project file remembers which tabs
    /// are open. A prebake tab always closes: the set still has its
    /// scores, and the settings sheet brings the tab back.
    pub fn close_current(&mut self) -> Result<Scene, SceneError> {
        if self.scenes[self.current].is_score() && self.score_count() <= 1 {
            return Err(SceneError::Last);
        }
        let removed = self.scenes.remove(self.current);
        if self.current >= self.scenes.len() {
            self.current = self.scenes.len() - 1;
        }
        if removed.is_score() {
            // Which tabs are open is now worth remembering.
            self.has_manifest = true;
        }
        Ok(removed)
    }

    /// The next scene id this set would hand out.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Remember how the editor is laid out: a scene per pane, left to
    /// right, and which pane has the caret. Returns whether that is news.
    ///
    /// One pane is not a layout - it is what every set opens as - so it
    /// is remembered as nothing at all, and a pane on a prebake tab or a
    /// tape names no file and is not remembered either.
    pub fn set_panes(&mut self, panes: &[SceneId], focused: usize) -> bool {
        let names: Vec<String> = panes
            .iter()
            .filter_map(|id| self.get(*id))
            .filter(|scene| scene.is_score())
            .map(Scene::file_name)
            .collect();
        let names = if names.len() > 1 && names.len() == panes.len() {
            names
        } else {
            Vec::new()
        };
        let focused = focused.min(names.len().saturating_sub(1));
        let changed = names != self.panes || focused != self.focused_pane;
        self.panes = names;
        self.focused_pane = focused;
        changed
    }

    /// The layout to open with: the scene for each pane, and which pane
    /// has the caret. Empty for a set that was left in one pane.
    pub fn pane_layout(&self) -> (Vec<SceneId>, usize) {
        let ids: Vec<SceneId> = self
            .panes
            .iter()
            .filter_map(|name| {
                self.scenes
                    .iter()
                    .find(|scene| scene.is_score() && scene.file_name() == *name)
                    .map(|scene| scene.id)
            })
            .collect();
        if ids.len() != self.panes.len() || ids.len() < 2 {
            return (Vec::new(), 0);
        }
        let focused = self.focused_pane.min(ids.len() - 1);
        (ids, focused)
    }

    /// Give every scene an id from `from` on, so a set opened in place of
    /// another never reuses an id the studio still remembers.
    pub fn renumber_from(&mut self, from: u64) {
        let mut next = from.max(self.next_id);
        for scene in &mut self.scenes {
            scene.id = SceneId(next);
            next += 1;
        }
        self.next_id = next;
    }

    /// The set file this set is kept in.
    pub fn manifest_path(&self) -> PathBuf {
        SetManifest::path_in(&self.directory)
    }

    /// Where the set's tapes and takes go.
    pub fn sessions_directory(&self) -> PathBuf {
        self.directory
            .join(rustel_runtime::product::SESSIONS_DIRECTORY_NAME)
    }

    /// What the set is called: its folder's name.
    pub fn name(&self) -> String {
        self.directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The folder's scores by name, each with the scene showing it when
    /// one is: what the set panel lists.
    pub fn folder_files(&self) -> Vec<SetFile> {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        let mut files: Vec<SetFile> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate.is_file()
                    && candidate
                        .extension()
                        .is_some_and(|extension| extension == SCENE_EXTENSION)
            })
            .map(|path| {
                let file = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                SetFile {
                    name: path
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    open: self
                        .scores()
                        .find(|scene| scene.file_name() == file)
                        .map(|scene| scene.id),
                    file,
                    path,
                }
            })
            .collect();
        files.sort_by_key(|file| file.name.to_lowercase());
        files
    }

    /// Open a score of the folder as a tab, after the current scene, and
    /// select it; one already open is selected as it is.
    pub fn open_file(&mut self, path: &Path) -> Result<SceneId, SceneError> {
        if let Some(index) = self
            .scenes
            .iter()
            .position(|scene| scene.is_score() && scene.path == path)
        {
            self.current = index;
            return Ok(self.scenes[index].id);
        }
        if !path.is_file() {
            return Err(SceneError::NotAFile(path.to_path_buf()));
        }
        if self.score_count() >= MAX_SCENES {
            return Err(SceneError::Full);
        }
        let source = read_score(path)?;
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let retained = self
            .oversized
            .iter()
            .find(|(_, entry)| entry.file == file_name)
            .map(|(_, entry)| entry.clone());
        let id = SceneId(self.next_id);
        self.next_id += 1;
        let mut scene = Scene::new(id, SceneKind::Score, path.to_path_buf(), &source, true)?;
        if let Some(retained) = retained {
            scene.pad = retained.pad;
            scene.rewind = retained.rewind;
            self.oversized.retain(|(_, entry)| entry.file != file_name);
        }
        let at = (self.current + 1).min(self.score_count());
        self.scenes.insert(at, scene);
        self.current = at;
        // Which tabs are open is now worth remembering.
        self.has_manifest = true;
        Ok(id)
    }

    /// Take a score of the folder off the disk: its tab too, when it has
    /// one. The last score on the strip stays, as it does for closing.
    pub fn delete_file(&mut self, path: &Path) -> Result<PathBuf, SceneError> {
        let open = self
            .scores()
            .find(|scene| scene.path == path)
            .map(|scene| scene.id);
        if let Some(id) = open {
            return self.delete_scene(id);
        }
        remove_score_file(path)?;
        if let Some(file_name) = path.file_name() {
            self.oversized
                .retain(|(_, entry)| entry.file != file_name.to_string_lossy());
        }
        Ok(path.to_path_buf())
    }

    /// Take a scene off the strip, out of the set and off the disk. The
    /// last score of a set stays, as it does for closing.
    pub fn delete_scene(&mut self, id: SceneId) -> Result<PathBuf, SceneError> {
        let Some(index) = self.index_of(id) else {
            return Err(SceneError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "no such scene in the set",
            )));
        };
        if !self.scenes[index].is_score() {
            return Err(SceneError::Prebake);
        }
        if self.score_count() <= 1 {
            return Err(SceneError::Last);
        }
        // The file first: a delete the disk refuses leaves the tab where
        // it was, rather than gone from the strip with its file still there
        // and nothing told it was closed.
        remove_score_file(&self.scenes[index].path)?;
        let removed = self.scenes.remove(index);
        if self.current >= self.scenes.len() {
            self.current = self.scenes.len() - 1;
        } else if index < self.current {
            self.current -= 1;
        }
        Ok(removed.path)
    }

    /// Bind a pad to the current scene. One pad launches one scene and one
    /// scene has one pad, so both older bindings go.
    /// A pad launches a scene, so a prebake tab refuses one.
    pub fn learn_pad(&mut self, pad: Pad) -> bool {
        if !self.scenes[self.current].is_score() {
            return false;
        }
        for scene in &mut self.scenes {
            if scene
                .pad
                .is_some_and(|bound| bound.matches(pad.note, pad.channel))
            {
                scene.pad = None;
            }
        }
        for (_, entry) in &mut self.oversized {
            if entry
                .pad
                .is_some_and(|bound| bound.matches(pad.note, pad.channel))
            {
                entry.pad = None;
            }
        }
        self.scenes[self.current].pad = Some(pad);
        true
    }

    pub fn forget_pad(&mut self) -> Option<Pad> {
        self.scenes[self.current].pad.take()
    }

    /// Flip whether the current scene rewinds when it is played, and say
    /// what it is now. A prebake tab is setup rather than music: it is
    /// never launched, so there is nothing to rewind.
    pub fn toggle_rewind(&mut self) -> Option<bool> {
        let scene = &mut self.scenes[self.current];
        if !scene.is_score() {
            return None;
        }
        scene.rewind = !scene.rewind;
        Some(scene.rewind)
    }

    /// Whether the scene on screen is played from its own cycle zero.
    pub fn current_rewinds(&self) -> bool {
        self.scenes[self.current].rewind
    }

    /// The scene a pad launches, if any.
    pub fn scene_for_pad(&self, note: u8, channel: u8) -> Option<usize> {
        self.scenes
            .iter()
            .position(|scene| scene.pad.is_some_and(|pad| pad.matches(note, channel)))
    }

    /// The project file as it should read now: the strip's scores, in
    /// order.
    pub fn manifest(&self) -> SetManifest {
        let mut scenes = self
            .scores()
            .map(|scene| SceneEntry {
                file: scene.file_name(),
                pad: scene.pad,
                rewind: scene.rewind,
            })
            .collect::<Vec<_>>();
        for (position, entry) in &self.oversized {
            scenes.insert((*position).min(scenes.len()), entry.clone());
        }
        SetManifest {
            // A prebake tab has no file to list. Oversized scores keep their
            // place and metadata until they can be opened again.
            scenes,
            current: self
                .scenes
                .get(self.current)
                .filter(|scene| scene.is_score())
                .map(Scene::file_name),
            panes: self.panes.clone(),
            focused_pane: (!self.panes.is_empty()).then_some(self.focused_pane),
            prebake: (!self.local_prebake.is_empty()).then(|| self.local_prebake.clone()),
            limiter: self.limiter.stored(),
            master_gain_tenths_db: self.master_gain_tenths_db,
            input_gains_tenths_db: self.input_gains_tenths_db.clone(),
            widgets: Vec::new(),
        }
    }

    /// Write the set file when the set has earned one: more than one
    /// scene, a pad, a prebake, a limiter of its own, a tab opened or
    /// closed, a folder the studio made, or a file that already exists. A
    /// lone score with nothing mapped leaves its folder exactly as it
    /// found it.
    pub fn persist_manifest(&mut self) -> io::Result<bool> {
        // A set file that could be neither read nor moved is still the only
        // copy of the set's pads, tab list and prebake. Writing a
        // folder-derived manifest over it is the loss this whole path
        // exists to prevent, so the folder keeps its file and goes without
        // one until the player moves it by hand.
        if self.kept_manifest.blocks_write() {
            return Ok(false);
        }
        let earned = self.has_manifest
            || self.score_count() > 1
            || self.scenes.iter().any(|scene| scene.pad.is_some())
            || !self.local_prebake.is_empty()
            // A limiter given to a lone score is still a thing the set
            // says about how it sounds, and the only place to keep it is
            // the file it has not written yet.
            || self.limiter != SetLimiter::Defer
            // A level given to a lone score is the same: how loud the set
            // plays is something it says, and the file is the only place
            // to keep it.
            || self.master_gain_tenths_db.is_some()
            || !self.input_gains_tenths_db.is_empty()
            // The same for a scene flagged to rewind: the doc promises the
            // flag is written beside the score, and a lone score flagged
            // has nowhere else to keep it.
            || self.scenes.iter().any(|scene| scene.rewind);
        if !earned {
            return Ok(false);
        }
        let text = serde_json::to_string_pretty(&self.manifest())
            .map_err(|error| io::Error::other(error.to_string()))?;
        // The project file is small and rewritten on every change, so it
        // goes through the same barrier a score does rather than risking a
        // half-written set behind a crash.
        super::save::atomic_write(&self.manifest_path(), &text)?;
        self.has_manifest = true;
        Ok(true)
    }

    fn free_path(&self, name: &str) -> PathBuf {
        let mut candidate = self.directory.join(format!("{name}.{SCENE_EXTENSION}"));
        let mut suffix = 2;
        while candidate.exists() || self.scenes.iter().any(|scene| scene.path == candidate) {
            candidate = self
                .directory
                .join(format!("{name} {suffix}.{SCENE_EXTENSION}"));
            suffix += 1;
        }
        candidate
    }
}

/// A score's name for a chip or the header: its stem, the way the strip
/// writes it - `live`, not `/sets/opening night/live.strudel`.
pub fn score_name(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A tape's name for a chip: its stem, less the `session-` every recording
/// starts with, so the strip reads `2026-09-05T12-09-48` rather than
/// repeating the word.
pub fn replay_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    stem.strip_prefix("session-")
        .map(str::to_owned)
        .unwrap_or(stem)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, ' ' | '-' | '_'))
}

/// The first free folder for a new set on `day`: the day itself, then
/// `day b`, `day c` and on through the alphabet, then numbers.
fn free_set_folder(root: &Path, day: &str) -> PathBuf {
    let plain = root.join(day);
    if !plain.exists() {
        return plain;
    }
    ('b'..='z')
        .map(|letter| root.join(format!("{day} {letter}")))
        .chain((27..).map(|number| root.join(format!("{day} {number}"))))
        .find(|candidate| !candidate.exists())
        .expect("an unbounded search finds a free name")
}

/// Delete a score's file. One already gone is not a failure: the point
/// was that it be gone.
fn remove_score_file(path: &Path) -> Result<(), SceneError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SceneError::Io(error)),
    }
}

fn read_score(path: &Path) -> Result<String, SceneError> {
    match bounded_file::read_to_string(path, MAX_DOCUMENT_BYTES) {
        Ok(source) => Ok(source),
        Err(ReadError::Io(error)) => Err(SceneError::Io(error)),
        Err(ReadError::TooLarge(bytes)) => Err(SceneError::TooLarge {
            path: path.to_path_buf(),
            bytes: usize::try_from(bytes).unwrap_or(usize::MAX),
        }),
    }
}

#[cfg(test)]
mod size_limit_tests {
    //! An oversized score is refused, an oversized set file is moved aside
    //! while the folder is scanned, and a set holding a full-size prebake
    //! still reads back.

    use super::*;

    #[test]
    fn an_oversized_manifest_is_kept_and_the_folder_is_scanned() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.strudel"), "$: s(\"bd\")\n").unwrap();
        let path = SetManifest::path_in(directory.path());
        // Valid JSON, refused for its length alone.
        let mut text =
            String::from(r#"{"scenes":[{"file":"a.strudel","pad":{"note":36,"channel":10}}]}"#);
        text.push_str(&" ".repeat(MAX_SET_FILE_BYTES as usize + 1 - text.len()));
        std::fs::write(&path, &text).unwrap();

        assert!(SetManifest::read(&path).is_none());
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        let kept = set
            .take_kept_manifest()
            .unwrap()
            .kept()
            .unwrap()
            .to_path_buf();
        assert_eq!(set.score_count(), 1);
        assert_eq!(set.scenes()[0].pad, None);
        assert!(!path.exists());
        assert_eq!(std::fs::metadata(&kept).unwrap().len(), text.len() as u64);
    }

    #[test]
    fn a_set_file_holding_a_full_size_prebake_reads_back() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.strudel"), "$: s(\"bd\")\n").unwrap();
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        // A control character is JSON's longest escape.
        let prebake = "\u{1}".repeat(MAX_DOCUMENT_BYTES as usize);
        set.set_local_prebake(&prebake);
        assert!(set.persist_manifest().unwrap());
        let path = SetManifest::path_in(directory.path());
        assert!(std::fs::metadata(&path).unwrap().len() > 6 * MAX_DOCUMENT_BYTES);

        let mut back = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(back.take_kept_manifest(), None);
        assert_eq!(back.local_prebake(), prebake);
    }

    #[test]
    fn an_oversized_score_is_refused_before_loading_its_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("huge.strudel");
        // The last byte is not UTF-8: a full read fails on it and a capped
        // read stops short of it, so only the length check reports this size.
        let mut source = vec![b' '; MAX_DOCUMENT_BYTES as usize + 1];
        source.push(0xFF);
        std::fs::write(&path, &source).unwrap();

        assert!(matches!(
            read_score(&path),
            Err(SceneError::TooLarge { bytes, .. }) if bytes == source.len()
        ));
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn write(directory: &Path, name: &str, text: &str) {
        std::fs::write(directory.join(name), text).unwrap();
    }

    fn oversized_score(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::File::create(&path)
            .unwrap()
            .set_len(super::super::editor::DEFAULT_MAX_DOCUMENT_BYTES as u64 + 1)
            .unwrap();
        path
    }

    #[test]
    fn a_folder_without_a_project_file_opens_every_score_in_name_order() {
        let directory = tempfile::tempdir().unwrap();
        for name in [
            "drop.strudel",
            "intro.strudel",
            "notes.txt",
            "break.strudel",
        ] {
            write(directory.path(), name, &format!("// {name}"));
        }
        let set = SceneSet::open(directory.path(), "starter").unwrap();
        let names = set.scenes().iter().map(Scene::name).collect::<Vec<_>>();
        assert_eq!(names, ["break", "drop", "intro"]);
        assert!(!set.any_dirty());
        assert_eq!(set.current().editor.source(), "// break.strudel");
    }

    #[test]
    fn an_oversized_score_does_not_block_a_folder_set_or_disappear_on_save() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "a.strudel", "// a");
        let large = oversized_score(directory.path(), "b.strudel");
        write(directory.path(), "c.strudel", "// c");

        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(
            set.scenes().iter().map(Scene::name).collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(
            set.folder_files()
                .iter()
                .map(|file| (file.name.as_str(), file.open.is_some()))
                .collect::<Vec<_>>(),
            [("a", true), ("b", false), ("c", true)]
        );
        assert!(matches!(
            set.open_file(&large),
            Err(SceneError::TooLarge { .. })
        ));
        set.persist_manifest().unwrap();
        assert_eq!(
            SetManifest::load(directory.path())
                .unwrap()
                .scenes
                .iter()
                .map(|entry| entry.file.as_str())
                .collect::<Vec<_>>(),
            ["a.strudel", "b.strudel", "c.strudel"]
        );
        assert!(matches!(
            SceneSet::open(&large, "starter"),
            Err(SceneError::TooLarge { .. })
        ));
        std::fs::write(&large, "// repaired").unwrap();
        let repaired = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(
            repaired
                .scenes()
                .iter()
                .map(Scene::name)
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn a_listed_oversized_score_keeps_its_pad_until_it_can_open() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "a.strudel", "// a");
        let large = oversized_score(directory.path(), "b.strudel");
        write(directory.path(), "c.strudel", "// c");
        write(
            directory.path(),
            SET_FILE_NAME,
            r#"{"scenes":[{"file":"a.strudel"},{"file":"b.strudel","pad":{"note":36,"channel":10},"rewind":true},{"file":"c.strudel"}]}"#,
        );

        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(
            set.scenes().iter().map(Scene::name).collect::<Vec<_>>(),
            ["a", "c"]
        );
        set.persist_manifest().unwrap();
        let manifest = SetManifest::load(directory.path()).unwrap();
        assert_eq!(manifest.scenes[1].file, "b.strudel");
        assert_eq!(
            manifest.scenes[1].pad,
            Some(Pad {
                note: 36,
                channel: 10
            })
        );
        assert!(manifest.scenes[1].rewind);
        assert_eq!(
            std::fs::metadata(&large).unwrap().len(),
            super::super::editor::DEFAULT_MAX_DOCUMENT_BYTES as u64 + 1
        );

        std::fs::write(&large, "// repaired").unwrap();
        set.open_file(&large).unwrap();
        assert_eq!(
            set.current().pad,
            Some(Pad {
                note: 36,
                channel: 10
            })
        );
        assert!(set.current().rewind);
        assert_eq!(
            set.manifest()
                .scenes
                .iter()
                .filter(|entry| entry.file == "b.strudel")
                .count(),
            1
        );
    }

    #[test]
    fn reassigning_an_oversized_scores_pad_clears_its_saved_binding() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "a.strudel", "// a");
        oversized_score(directory.path(), "b.strudel");
        write(
            directory.path(),
            SET_FILE_NAME,
            r#"{"scenes":[{"file":"a.strudel"},{"file":"b.strudel","pad":{"note":36,"channel":10}}]}"#,
        );

        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert!(set.learn_pad(Pad {
            note: 36,
            channel: 10,
        }));
        let manifest = set.manifest();
        assert_eq!(
            manifest.scenes[0].pad,
            Some(Pad {
                note: 36,
                channel: 10
            })
        );
        assert_eq!(manifest.scenes[1].pad, None);
    }

    /// A set reopens at the levels it was left at: the master and each input
    /// device.
    #[test]
    fn a_set_reopens_at_the_levels_it_was_left_at() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "one.strudel", "$: s(\"bd\")");
        write(directory.path(), "two.strudel", "$: s(\"hh\")");
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(set.master_gain_db(), None, "never moved: nothing to say");
        assert_eq!(set.input_gain_db("mic"), None);

        assert!(set.set_master_gain_db(-6.5));
        assert!(set.set_input_gain_db("mic", 12.0));
        assert!(set.set_input_gain_db("line", -3.0));
        set.persist_manifest().unwrap();

        let back = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(back.master_gain_db(), Some(-6.5));
        // Each source gets its own back: a microphone and a synth want
        // settings tens of decibels apart, and are one set's two inputs.
        assert_eq!(back.input_gain_db("mic"), Some(12.0));
        assert_eq!(back.input_gain_db("line"), Some(-3.0));
        assert_eq!(back.input_gain_db("never used"), None);
    }

    /// A drag that ends where it began is not a change and does not rewrite
    /// the set file. Rounding to the stored tenths in one place makes that
    /// true.
    #[test]
    fn a_level_that_did_not_move_is_not_a_change() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "one.strudel", "$: s(\"bd\")");
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert!(set.set_master_gain_db(-3.04));
        assert!(!set.set_master_gain_db(-3.0), "the same tenth");
        assert!(!set.set_master_gain_db(-2.96), "rounds to the same tenth");
        assert!(set.set_master_gain_db(-2.9), "a different tenth moved");
        assert!(set.set_input_gain_db("mic", 1.0));
        assert!(!set.set_input_gain_db("mic", 1.04));
    }

    /// A lone score has no set file until it says something only a file can
    /// keep. A level is one of those things, like a limiter is.
    #[test]
    fn a_lone_score_given_a_level_earns_a_set_file() {
        let alone = tempfile::tempdir().unwrap();
        write(alone.path(), "only.strudel", "$: s(\"bd\")");
        let mut lonely = SceneSet::open(alone.path(), "starter").unwrap();
        assert!(!lonely.persist_manifest().unwrap(), "nothing to say yet");
        lonely.set_master_gain_db(-9.0);
        assert!(
            lonely.persist_manifest().unwrap(),
            "now it has a level to keep"
        );
        let back = SceneSet::open(alone.path(), "starter").unwrap();
        assert_eq!(back.master_gain_db(), Some(-9.0));
    }

    #[test]
    fn a_set_keeps_the_limiter_it_was_given_including_off() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "one.strudel", "$: s(\"bd\")");
        write(directory.path(), "two.strudel", "$: s(\"hh\")");

        // A lone score in a folder writes no set file at all - unless it
        // has been given a limiter, which is then the only place that
        // limiter could live.
        let alone = tempfile::tempdir().unwrap();
        write(alone.path(), "only.strudel", "$: s(\"bd\")");
        let mut lonely = SceneSet::open(alone.path(), "starter").unwrap();
        assert!(!lonely.persist_manifest().unwrap(), "nothing to say yet");
        let punchy = rustel_audio::LimiterSettings {
            threshold_db: -4.0,
            character: rustel_audio::LimiterCharacter::Punchy,
        };
        lonely.set_limiter(SetLimiter::Says {
            bypassed: false,
            settings: punchy,
        });
        assert!(lonely.persist_manifest().unwrap(), "and now there is");
        assert_eq!(
            *SceneSet::open(alone.path(), "starter").unwrap().limiter(),
            SetLimiter::Says {
                bypassed: false,
                settings: punchy
            },
            "the ceiling and the character both, not merely something"
        );

        let reopen = |directory: &std::path::Path| SceneSet::open(directory, "starter").unwrap();

        // A fresh folder defers: it has no opinion, so the studio's does.
        let mut set = reopen(directory.path());
        assert_eq!(*set.limiter(), SetLimiter::Defer);
        assert_eq!(set.limiter().resolve(), None, "which is the studio's");

        // Given one, it writes it and reads it back.
        let mine = rustel_audio::LimiterSettings {
            threshold_db: -7.5,
            character: rustel_audio::LimiterCharacter::Warm,
        };
        let says = |settings| SetLimiter::Says {
            bypassed: false,
            settings,
        };
        assert!(set.set_limiter(says(mine)), "that is a change");
        assert!(!set.set_limiter(says(mine)), "and doing it twice is not");
        set.persist_manifest().unwrap();
        assert_eq!(*reopen(directory.path()).limiter(), says(mine));
        assert_eq!(
            reopen(directory.path()).limiter().resolve(),
            Some(Some(mine))
        );

        // And a bypass is an opinion, not the absence of one - and it
        // keeps the ceiling and the character it switched out, because a
        // switch has to have something to switch back to.
        let mut set = reopen(directory.path());
        assert!(set.set_limiter(SetLimiter::Says {
            bypassed: true,
            settings: mine,
        }));
        set.persist_manifest().unwrap();
        let back = reopen(directory.path());
        assert_eq!(
            *back.limiter(),
            SetLimiter::Says {
                bypassed: true,
                settings: mine
            },
            "a bypassed limiter is still the limiter it was"
        );
        assert_eq!(
            back.limiter().resolve(),
            Some(None),
            "a set that plays with the limiter out keeps playing that way"
        );
        assert_eq!(back.limiter().held(), Some(mine), "and what it switches in");

        // `off` and nothing else is a set with no limiter on its desk -
        // which is what a set written before the strip could be removed
        // meant by it too.
        let path = SetManifest::path_in(directory.path());
        let mut manifest = SetManifest::read(&path).expect("a set file");
        manifest.limiter = Some("off".to_owned());
        std::fs::write(&path, serde_json::to_string(&manifest).unwrap()).unwrap();
        let old = reopen(directory.path());
        assert_eq!(*old.limiter(), SetLimiter::None);
        assert!(!old.limiter().bypassed(), "there is nothing to bypass");
        assert_eq!(old.limiter().resolve(), Some(None));
        assert_eq!(old.limiter().held(), None);

        // Which is the whole point of the state: a studio default
        // switched on afterwards does not hand a limiter back to a set it
        // was taken off.
        assert!(!old.limiter().has_slot(true), "still none, still none");
        assert!(!old.limiter().has_slot(false));

        // The three answers a set can give, all reachable and all
        // different: none, the studio's, and its own.
        let mut set = reopen(directory.path());
        assert!(set.set_limiter(SetLimiter::Defer));
        set.persist_manifest().unwrap();
        let back = reopen(directory.path());
        assert_eq!(*back.limiter(), SetLimiter::Defer);
        assert!(
            back.limiter().has_slot(true) && !back.limiter().has_slot(false),
            "a set that has never said opens with whatever the studio hands out"
        );
        assert!(says(mine).has_slot(false), "and its own outranks that");
    }

    /// A limiter value this build cannot parse plays as off, and every save
    /// writes it back unchanged.
    #[test]
    fn a_limiter_this_build_cannot_read_is_kept_word_for_word() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "one.strudel", "$: s(\"bd\")");
        write(directory.path(), "two.strudel", "$: s(\"hh\")");
        let path = SetManifest::path_in(directory.path());
        SceneSet::open(directory.path(), "starter")
            .unwrap()
            .persist_manifest()
            .unwrap();

        for stranger in ["-1.0:gluey", "-3.0:brickwall", "-1.0:war", "-100:warm", ""] {
            let mut manifest = SetManifest::read(&path).expect("a set file");
            manifest.limiter = Some(stranger.to_owned());
            std::fs::write(&path, serde_json::to_string(&manifest).unwrap()).unwrap();

            let mut set = SceneSet::open(directory.path(), "starter").unwrap();
            assert_eq!(
                *set.limiter(),
                SetLimiter::Unreadable(stranger.to_owned()),
                "{stranger:?} is not something this build understands"
            );
            assert_eq!(
                set.limiter().resolve(),
                Some(None),
                "{stranger:?} plays as off, because a limiter is opted into"
            );

            // The part that matters: writing the set back does not
            // overwrite what it could not read.
            set.persist_manifest().unwrap();
            assert_eq!(
                SetManifest::read(&path).expect("still a set file").limiter,
                Some(stranger.to_owned()),
                "{stranger:?} was overwritten by a guess"
            );
        }
    }

    #[test]
    fn a_project_file_is_the_authority_on_order_and_membership() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["a.strudel", "b.strudel", "stray.strudel"] {
            write(directory.path(), name, "// x");
        }
        write(
            directory.path(),
            SET_FILE_NAME,
            r#"{"scenes":[{"file":"b.strudel","pad":{"note":36,"channel":10}},{"file":"a.strudel"},{"file":"gone.strudel"}]}"#,
        );
        let set = SceneSet::open(directory.path(), "starter").unwrap();
        let names = set.scenes().iter().map(Scene::name).collect::<Vec<_>>();
        assert_eq!(
            names,
            ["b", "a"],
            "listed order, strays and missing files excluded"
        );
        assert_eq!(
            set.scenes()[0].pad,
            Some(Pad {
                note: 36,
                channel: 10
            })
        );
        assert_eq!(set.scene_for_pad(36, 10), Some(0));

        // Opening one listed file opens the whole set at that scene.
        let set = SceneSet::open(&directory.path().join("a.strudel"), "starter").unwrap();
        assert_eq!(set.len(), 2);
        assert_eq!(set.current().name(), "a");
    }

    #[test]
    fn a_lone_file_stays_a_lone_file_until_the_set_earns_a_project_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("song.strudel");
        let mut set = SceneSet::open(&path, "starter").unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.current().dirty, "a missing file is an unsaved starter");
        assert_eq!(set.current().name(), "song");
        assert_eq!(set.directory(), directory.path());
        assert!(!set.persist_manifest().unwrap());
        assert!(!SetManifest::path_in(directory.path()).exists());

        set.create("// second").unwrap();
        assert!(set.persist_manifest().unwrap());
        let manifest = SetManifest::load(directory.path()).unwrap();
        assert_eq!(
            manifest
                .scenes
                .iter()
                .map(|entry| entry.file.as_str())
                .collect::<Vec<_>>(),
            ["song.strudel", "scene 1.strudel"]
        );

        // The starter was never written, so it is left out until it is;
        // the new scene was written when it was created.
        let set = SceneSet::open(&path, "starter").unwrap();
        assert_eq!(set.len(), 1, "a never-updated starter has no file to open");
        assert_eq!(set.current().name(), "scene 1");
        write(directory.path(), "song.strudel", "$: s(\"bd\")");
        let set = SceneSet::open(&path, "starter").unwrap();
        assert_eq!(set.len(), 2, "the project file now opens the whole set");
        assert_eq!(set.current().name(), "song");
        assert!(!set.current().dirty);
    }

    #[test]
    fn creating_renaming_and_closing_scenes_keep_files_honest() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "intro.strudel", "// intro");
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();

        let created = set.create("// new").unwrap();
        assert_eq!(set.current().id, created);
        assert_eq!(set.current().name(), "scene 1");
        assert!(!set.current().dirty, "a new scene is on disk at once");
        assert_eq!(set.current().path, directory.path().join("scene 1.strudel"));
        assert_eq!(
            std::fs::read_to_string(&set.current().path).unwrap(),
            "// new"
        );

        set.rename_current("drop").unwrap();
        assert_eq!(set.current().name(), "drop");
        assert!(matches!(
            set.rename_current("intro"),
            Err(SceneError::NameTaken(_))
        ));
        assert!(matches!(
            set.rename_current("../escape"),
            Err(SceneError::BadName(_))
        ));

        // Renaming a scene that exists on disk moves its file.
        std::fs::write(&set.current().path, "// drop").unwrap();
        set.rename_current("the drop").unwrap();
        assert!(directory.path().join("the drop.strudel").exists());
        assert!(!directory.path().join("drop.strudel").exists());

        assert!(set.select_relative(-1));
        assert_eq!(set.current().name(), "intro");
        assert!(set.select_relative(1));
        assert_eq!(set.current().name(), "the drop");
        let closed = set.close_current().unwrap();
        assert_eq!(closed.name(), "the drop");
        assert!(directory.path().join("the drop.strudel").exists());
        assert!(matches!(set.close_current(), Err(SceneError::Last)));
    }

    /// ^W closes a tab, no more: the file stays in the folder, and the
    /// project file remembers which tabs were open, so the set comes back
    /// as it was left and the folder still lists what was closed.
    #[test]
    fn closing_a_tab_keeps_the_file_and_the_set_remembers_its_open_tabs() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "intro.strudel", "// intro");
        write(directory.path(), "drop.strudel", "// drop");
        // Name order puts "drop" first, and first is where the strip opens.
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(set.current().name(), "drop");
        let closed = set.close_current().unwrap();
        assert_eq!(closed.name(), "drop");
        assert_eq!(set.len(), 1);
        assert!(directory.path().join("drop.strudel").exists());
        assert!(
            set.persist_manifest().unwrap(),
            "a closed tab earns the set file"
        );
        let manifest = SetManifest::load(directory.path()).unwrap();
        assert_eq!(
            manifest
                .scenes
                .iter()
                .map(|entry| entry.file.as_str())
                .collect::<Vec<_>>(),
            ["intro.strudel"]
        );
        assert_eq!(
            set.folder_files()
                .iter()
                .map(|file| (file.name.as_str(), file.open.is_some()))
                .collect::<Vec<_>>(),
            [("drop", false), ("intro", true)],
            "the folder still lists the closed score"
        );
        // Reopened, the tabs are as they were left.
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set.current().name(), "intro");
        let drop = directory.path().join("drop.strudel");
        let opened = set.open_file(&drop).unwrap();
        assert_eq!(set.current().id, opened);
        assert_eq!(set.current().name(), "drop");
        assert_eq!(set.current().editor.source(), "// drop");
        assert_eq!(set.len(), 2, "after the current scene");
        assert_eq!(
            set.open_file(&drop).unwrap(),
            opened,
            "a file already open is selected, not opened twice"
        );
        assert_eq!(set.len(), 2);
    }

    /// A scene the project file lists but the folder no longer has is not
    /// a tab: the folder is the set, and what is not in it is nowhere.
    #[test]
    fn a_listed_scene_whose_file_is_gone_is_left_out() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "a.strudel", "// a");
        write(
            directory.path(),
            SET_FILE_NAME,
            r#"{"scenes":[{"file":"gone.strudel","pad":{"note":36,"channel":10}},{"file":"a.strudel"}]}"#,
        );
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set.current().name(), "a");
        assert!(set.folder_files().iter().all(|file| file.name == "a"));
        set.persist_manifest().unwrap();
        assert!(
            !SetManifest::load(directory.path())
                .unwrap()
                .scenes
                .iter()
                .any(|entry| entry.file == "gone.strudel"),
            "the next write forgets it"
        );
        // Every listed file gone: the set still has a scene.
        write(
            directory.path(),
            SET_FILE_NAME,
            r#"{"scenes":[{"file":"gone.strudel"}]}"#,
        );
        std::fs::remove_file(directory.path().join("a.strudel")).unwrap();
        let set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set.current().name(), "scene 1");
        assert!(set.current().dirty, "a starter, unsaved");
    }

    /// Deleting a scene from the strip takes its file with it; the last
    /// score of a set stays, as it does for closing. A file of the folder
    /// that is not open is deleted as a file.
    #[test]
    fn deleting_a_scene_takes_its_file_with_it() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "intro.strudel", "// intro");
        write(directory.path(), "drop.strudel", "// drop");
        write(directory.path(), "spare.strudel", "// spare");
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        let drop = set.scenes()[0].id;
        set.select(1);
        let path = set.delete_scene(drop).unwrap();
        assert_eq!(path, directory.path().join("drop.strudel"));
        assert!(!path.exists());
        assert_eq!(
            set.current().name(),
            "intro",
            "the selection followed the deletion"
        );
        let spare = directory.path().join("spare.strudel");
        set.select(1);
        set.close_current().unwrap();
        assert!(spare.exists(), "closing keeps the file");
        set.delete_file(&spare).unwrap();
        assert!(!spare.exists());
        assert_eq!(set.len(), 1);
        let last = set.current().id;
        assert!(matches!(set.delete_scene(last), Err(SceneError::Last)));
        assert!(directory.path().join("intro.strudel").exists());
        assert!(matches!(
            set.open_file(directory.path()),
            Err(SceneError::NotAFile(_))
        ));
    }

    /// A delete the disk refuses changes nothing: the tab stays on the
    /// strip, and the selection with it, rather than leave the strip with
    /// its file still in the folder.
    #[test]
    fn a_refused_delete_leaves_the_tab_where_it_was() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "intro.strudel", "// intro");
        write(directory.path(), "drop.strudel", "// drop");
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        let (drop, path) = set
            .scenes()
            .iter()
            .find(|scene| scene.name() == "drop")
            .map(|scene| (scene.id, scene.path.clone()))
            .expect("the drop tab");
        // A directory where the file was: removing it fails, and not
        // because it is gone.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let selected = set.current().id;

        assert!(matches!(set.delete_scene(drop), Err(SceneError::Io(_))));
        assert_eq!(set.len(), 2);
        assert!(set.scenes().iter().any(|scene| scene.id == drop));
        assert_eq!(set.current().id, selected);
    }

    /// A new set is a folder of its own under the sets folder, named after
    /// the day and lettered when the day already has one, with a starter
    /// scene on disk; renaming it moves the folder and every path with it.
    #[test]
    fn a_new_set_is_a_folder_named_after_the_day_and_can_be_renamed() {
        let root = tempfile::tempdir().unwrap();
        let sets = root.path().join("sets");
        let mut first = SceneSet::create_in(&sets, "2026-09-06", "// starter").unwrap();
        assert_eq!(first.directory(), sets.join("2026-09-06"));
        assert_eq!(first.name(), "2026-09-06");
        assert_eq!(first.len(), 1);
        assert!(!first.current().dirty, "the starter is written at once");
        assert_eq!(
            first.current().path,
            sets.join("2026-09-06").join("scene 1.strudel")
        );
        assert!(
            first.persist_manifest().unwrap(),
            "made by the studio, it has a set file"
        );
        assert!(SetManifest::load(first.directory()).is_some());
        assert_eq!(
            first.sessions_directory(),
            sets.join("2026-09-06").join("sessions")
        );
        let second = SceneSet::create_in(&sets, "2026-09-06", "// starter").unwrap();
        assert_eq!(second.name(), "2026-09-06 b");
        let third = SceneSet::create_in(&sets, "2026-09-06", "// starter").unwrap();
        assert_eq!(third.name(), "2026-09-06 c");

        let renamed = first.rename_set("opening night").unwrap();
        assert_eq!(renamed, sets.join("opening night"));
        assert_eq!(first.directory(), renamed);
        assert_eq!(first.name(), "opening night");
        assert_eq!(first.current().path, renamed.join("scene 1.strudel"));
        assert!(renamed.join("scene 1.strudel").exists());
        assert!(!sets.join("2026-09-06").exists());
        assert!(matches!(
            first.rename_set("2026-09-06 b"),
            Err(SceneError::NameTaken(_))
        ));
        assert!(matches!(
            first.rename_set("../escape"),
            Err(SceneError::BadName(_))
        ));
        assert_eq!(
            first.rename_set("opening night").unwrap(),
            renamed,
            "the same name is no move"
        );
        // A set file is not a set: its folder is.
        assert!(matches!(
            SceneSet::open(&renamed.join(SET_FILE_NAME), "starter"),
            Err(SceneError::NotASet(_))
        ));
    }

    #[test]
    fn learning_a_pad_moves_it_off_any_other_scene() {
        let directory = tempfile::tempdir().unwrap();
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        set.learn_pad(Pad {
            note: 36,
            channel: 10,
        });
        set.create("").unwrap();
        set.learn_pad(Pad {
            note: 36,
            channel: 10,
        });
        assert_eq!(set.scene_for_pad(36, 10), Some(1));
        assert_eq!(set.scenes()[0].pad, None);
        assert_eq!(
            set.forget_pad(),
            Some(Pad {
                note: 36,
                channel: 10
            })
        );
        assert_eq!(set.scene_for_pad(36, 10), None);
    }

    /// Rewind belongs to the scene, is written into the set file, and
    /// comes back with it.
    #[test]
    fn a_scene_carries_its_own_rewind_and_the_set_file_keeps_it() {
        let directory = tempfile::tempdir().unwrap();
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        set.create("$: s(\"hh*8\")").unwrap();
        assert!(!set.current_rewinds(), "an ordinary scene joins the cycle");
        assert_eq!(set.toggle_rewind(), Some(true));
        assert!(set.current_rewinds());

        // Only this one: the flag is not the studio's.
        let flagged = set.current().file_name();
        assert!(set.select(0));
        assert!(!set.current_rewinds(), "the other scene is untouched");

        set.persist_manifest().expect("write the set file");
        let manifest = SetManifest::load(directory.path()).expect("a set file");
        let entry = manifest
            .scenes
            .iter()
            .find(|entry| entry.file == flagged)
            .expect("the flagged score is listed");
        assert!(entry.rewind, "the set file says so");
        assert!(
            manifest.scenes.iter().filter(|entry| entry.rewind).count() == 1,
            "and says so about one of them"
        );

        // And it opens the way it was left.
        let reopened = SceneSet::open(directory.path(), "starter").unwrap();
        let back = reopened
            .scenes()
            .iter()
            .find(|scene| scene.file_name() == flagged)
            .expect("the score is back");
        assert!(back.rewind, "the flag came back with the set");
        assert!(
            reopened
                .scenes()
                .iter()
                .filter(|scene| scene.rewind)
                .count()
                == 1,
            "and only it"
        );
    }

    #[test]
    fn a_prebake_tab_opens_at_the_end_global_before_local() {
        let directory = tempfile::tempdir().expect("temp dir");
        for name in ["a", "b"] {
            std::fs::write(
                directory.path().join(format!("{name}.strudel")),
                "$: s(\"bd\")",
            )
            .expect("scene");
        }
        let mut set = SceneSet::open(directory.path(), "starter").expect("set");

        set.open_prebake(PrebakeScope::Local, "globalThis.l = 1")
            .expect("local tab");
        let global = set
            .open_prebake(PrebakeScope::Global, "globalThis.g = 1")
            .expect("global tab");

        let names = set.scenes().iter().map(Scene::name).collect::<Vec<_>>();
        assert_eq!(
            names,
            ["a", "b", "prebake (global)", "prebake (local)"],
            "prebake tabs belong at the end, global first"
        );
        assert_eq!(set.score_count(), 2);
        assert_eq!(set.len(), 4);
        assert_eq!(set.prebake_index(PrebakeScope::Global), Some(2));
        assert_eq!(set.prebake_index(PrebakeScope::Local), Some(3));
        assert_eq!(set.current().id, global, "a new tab is selected");
        assert!(!set.current().dirty, "a tab opens on what is stored");

        // Opening one already open selects it rather than doubling it.
        assert_eq!(
            set.open_prebake(PrebakeScope::Local, "ignored")
                .expect("already open"),
            set.scenes()[3].id
        );
        assert_eq!(set.len(), 4);
        assert_eq!(set.current_index(), 3);
    }

    #[test]
    fn prebake_tabs_never_count_toward_the_cap_and_never_reach_the_project_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("song.strudel"), "$: s(\"bd\")").expect("scene");
        let mut set = SceneSet::open(directory.path(), "starter").expect("set");

        set.open_prebake(PrebakeScope::Global, "globalThis.g = 1")
            .expect("global tab");
        set.open_prebake(PrebakeScope::Local, "globalThis.l = 1")
            .expect("local tab");
        set.set_local_prebake("globalThis.l = 1");

        while set.score_count() < MAX_SCENES {
            set.create("").expect("scene");
        }
        assert_eq!(set.len(), MAX_SCENES + 2);
        assert!(
            matches!(set.create(""), Err(SceneError::Full)),
            "the cap counts scores, and it still holds"
        );

        let manifest = set.manifest();
        assert_eq!(manifest.scenes.len(), MAX_SCENES);
        assert!(
            manifest
                .scenes
                .iter()
                .all(|entry| entry.file.ends_with(".strudel")),
            "a prebake tab was listed as a scene: {manifest:?}"
        );
        assert_eq!(manifest.prebake.as_deref(), Some("globalThis.l = 1"));
    }

    #[test]
    fn creating_a_scene_from_a_prebake_tab_lands_after_the_scores() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("song.strudel"), "$: s(\"bd\")").expect("scene");
        let mut set = SceneSet::open(directory.path(), "starter").expect("set");
        set.open_prebake(PrebakeScope::Global, "globalThis.g = 1")
            .expect("global tab");

        set.create("").expect("scene from the tab");

        assert_eq!(set.current_index(), 1, "the new scene is selected");
        assert!(set.scenes()[1].is_score());
        assert_eq!(
            set.scenes()[2].prebake(),
            Some(PrebakeScope::Global),
            "the tab kept the end of the strip"
        );
    }

    #[test]
    fn a_prebake_tab_cannot_be_renamed_or_given_a_pad() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("song.strudel"), "$: s(\"bd\")").expect("scene");
        let mut set = SceneSet::open(directory.path(), "starter").expect("set");
        set.open_prebake(PrebakeScope::Local, "globalThis.l = 1")
            .expect("local tab");

        assert!(matches!(
            set.rename_current("anything"),
            Err(SceneError::Prebake)
        ));
        assert_eq!(set.current().name(), "prebake (local)");
        assert!(!set.learn_pad(Pad {
            note: 36,
            channel: 10
        }));
        assert!(set.current().pad.is_none());
        assert_eq!(set.scene_for_pad(36, 10), None);
        // A score named like the tab is still free to take that name.
        set.select(0);
        set.rename_current("prebake local").expect("scores rename");
    }

    #[test]
    fn a_prebake_tab_always_closes_and_the_last_score_never_does() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("song.strudel"), "$: s(\"bd\")").expect("scene");
        let mut set = SceneSet::open(directory.path(), "starter").expect("set");
        set.open_prebake(PrebakeScope::Global, "globalThis.g = 1")
            .expect("global tab");
        set.open_prebake(PrebakeScope::Local, "globalThis.l = 1")
            .expect("local tab");

        set.select(0);
        assert!(
            matches!(set.close_current(), Err(SceneError::Last)),
            "the only score cannot be closed, tabs open or not"
        );

        set.select(1);
        set.close_current().expect("global tab closes");
        set.close_current().expect("local tab closes");
        assert_eq!(set.len(), 1);
        assert_eq!(set.score_count(), 1);
        assert!(
            directory.path().join("song.strudel").exists(),
            "closing a tab touched the scores"
        );
    }

    #[test]
    fn the_project_file_round_trips_the_sets_own_prebake() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("song.strudel"), "$: s(\"bd\")").expect("scene");
        let mut set = SceneSet::open(directory.path(), "starter").expect("set");
        assert!(!set.persist_manifest().expect("nothing to remember yet"));

        set.set_local_prebake("globalThis.riff = () => note('c')");
        assert!(
            set.persist_manifest().expect("write"),
            "a prebake is something to remember"
        );

        let reopened = SceneSet::open(directory.path(), "starter").expect("reopen");
        assert_eq!(
            reopened.local_prebake(),
            "globalThis.riff = () => note('c')"
        );
        assert_eq!(reopened.score_count(), 1);

        // Emptied, it stops being mentioned.
        let mut set = SceneSet::open(directory.path(), "starter").expect("reopen");
        set.set_local_prebake("   \n");
        set.persist_manifest().expect("write");
        assert!(set.manifest().prebake.is_none());
        let text = std::fs::read_to_string(SetManifest::path_in(directory.path())).expect("read");
        assert!(
            !text.contains("prebake"),
            "an emptied prebake lingered: {text}"
        );
    }

    /// The visuals panel's widgets travel in the set file, in order, with
    /// The set opens on the tab it was left on: the set file names it,
    /// and a set left on a prebake tab names none.
    #[test]
    fn the_set_file_remembers_the_tab_that_had_the_caret() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "a.strudel", "// a");
        write(directory.path(), "b.strudel", "// b");
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(set.score_count(), 2);
        assert_eq!(set.current().name(), "a");
        assert!(set.select(1));
        assert_eq!(set.current().name(), "b");
        assert!(set.persist_manifest().unwrap());
        let manifest = SetManifest::load(directory.path()).expect("set file");
        assert_eq!(manifest.current.as_deref(), Some("b.strudel"));

        let again = SceneSet::open(directory.path(), "starter").unwrap();
        assert_eq!(
            again.current().name(),
            "b",
            "opened on the tab it was left on"
        );
        // A score named on the command line still wins.
        let named = SceneSet::open(&directory.path().join("a.strudel"), "starter").unwrap();
        assert_eq!(named.current().name(), "a");
    }

    /// the art's style and colour, from before the preferences held them;
    /// the set hands them over once and never writes them again.
    #[test]
    fn an_older_set_file_hands_over_its_widgets_once() {
        use super::super::viz_panel::{Colouring, WidgetKind, WidgetSpec};
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "a.strudel", "// a");
        std::fs::write(
            SetManifest::path_in(directory.path()),
            r#"{"scenes":[{"file":"a.strudel"}],"widgets":[{"kind":"art","style":"shadow","colour":"acid"},{"kind":"spectrum"}]}"#,
        )
        .unwrap();
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        let mut art = WidgetSpec::default_art();
        art.style = Some("shadow".into());
        art.colour = Some(Colouring::Acid);
        assert_eq!(
            set.take_carried_widgets(),
            [art, WidgetSpec::new(WidgetKind::Spectrum)]
        );
        assert!(set.take_carried_widgets().is_empty(), "handed over once");
        assert!(set.persist_manifest().unwrap());
        let text = std::fs::read_to_string(SetManifest::path_in(directory.path())).unwrap();
        assert!(!text.contains("\"widgets\""), "never written again: {text}");
    }

    #[test]
    fn a_project_file_without_a_prebake_key_still_opens() {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(directory.path().join("a.strudel"), "$: s(\"bd\")").expect("scene");
        std::fs::write(
            SetManifest::path_in(directory.path()),
            r#"{"scenes":[{"file":"a.strudel"}]}"#,
        )
        .expect("manifest");

        let set = SceneSet::open(directory.path(), "starter").expect("set");
        assert_eq!(set.score_count(), 1);
        assert!(set.local_prebake().is_empty());
    }

    #[test]
    fn a_set_is_capped_at_sixteen_scenes() {
        let directory = tempfile::tempdir().unwrap();
        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        while set.len() < MAX_SCENES {
            set.create("").unwrap();
        }
        assert!(matches!(set.create(""), Err(SceneError::Full)));
        let names = set.scenes().iter().map(Scene::name).collect::<Vec<_>>();
        assert_eq!(names.iter().filter(|name| *name == "scene 1").count(), 1);
    }

    /// A set file that does not parse held the pads, the tab list and the
    /// set's prebake, and nothing else does. Read as absent it opened as a
    /// plain folder scan and the first `persist_manifest` wrote over it,
    /// before a key was pressed; one sync conflict marker was enough.
    #[test]
    fn a_set_file_that_does_not_read_is_kept_rather_than_written_over() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.strudel"), "$: s(\"bd\")\n").unwrap();
        std::fs::write(directory.path().join("b.strudel"), "$: s(\"hh\")\n").unwrap();
        let conflicted = "<<<<<<< HEAD\n{ \"scenes\": [] }\n";
        let path = SetManifest::path_in(directory.path());
        std::fs::write(&path, conflicted).unwrap();

        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        let kept = set
            .take_kept_manifest()
            .and_then(|kept| kept.kept().map(|path| path.to_path_buf()))
            .expect("the file was kept");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), conflicted);
        assert_eq!(set.take_kept_manifest(), None, "said once");
        assert_eq!(set.score_count(), 2, "the folder was scanned instead");

        assert!(
            set.persist_manifest().unwrap(),
            "two scores earn a set file"
        );
        assert_eq!(
            std::fs::read_to_string(&kept).unwrap(),
            conflicted,
            "the copy outlives the write"
        );
        assert!(
            SetManifest::load(directory.path()).is_some(),
            "and a readable one is there now"
        );
    }

    /// A folder that keeps arriving broken keeps every copy.
    #[test]
    fn a_second_unreadable_set_file_does_not_write_over_the_first() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.strudel"), "$: s(\"bd\")\n").unwrap();
        let path = SetManifest::path_in(directory.path());
        for text in ["first broken", "second broken"] {
            std::fs::write(&path, text).unwrap();
            let mut set = SceneSet::open(directory.path(), "starter").unwrap();
            assert_eq!(
                std::fs::read_to_string(
                    set.take_kept_manifest()
                        .and_then(|kept| kept.kept().map(|path| path.to_path_buf()))
                        .expect("kept")
                )
                .unwrap(),
                text
            );
        }
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bad")).unwrap(),
            "first broken"
        );
    }

    /// A set file that is not text at all is kept on the strength of its
    /// name: a rename does not have to read the file, and a sync client
    /// that cut it mid-character has still left the only copy of the set's
    /// pads and prebake sitting there.
    #[test]
    fn a_set_file_that_is_not_text_is_kept_too() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.strudel"), "$: s(\"bd\")\n").unwrap();
        std::fs::write(directory.path().join("b.strudel"), "$: s(\"hh\")\n").unwrap();
        let path = SetManifest::path_in(directory.path());
        // Valid bytes, invalid UTF-8: a four-byte character with one of its
        // three continuations.
        let cut = [0x7bu8, 0x22, 0xf0, 0x9f];
        std::fs::write(&path, cut).unwrap();

        let mut set = SceneSet::open(directory.path(), "starter").unwrap();
        let kept = set
            .take_kept_manifest()
            .and_then(|kept| kept.kept().map(|path| path.to_path_buf()))
            .expect("the file was kept");
        assert_eq!(std::fs::read(&kept).unwrap(), cut, "byte for byte");
        assert!(!path.exists(), "and it is out of the folder's way");
        assert_eq!(set.score_count(), 2, "the folder was scanned instead");
        assert!(
            set.persist_manifest().unwrap(),
            "with the file moved aside the folder may earn its own"
        );
    }
}
