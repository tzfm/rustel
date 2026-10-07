//! The studio's two prebakes: setup JavaScript run on the session heap
//! before any score.
//!
//! A prebake is where a helper library lives - `register(...)` calls,
//! functions hung on `globalThis`, sample registrations - so a score can
//! call by name what a set has taught the engine. There are two, and the
//! difference is only where the text is kept: the **global** one is a file
//! beside the studio's settings, shared by every set; the **local** one
//! belongs to a set and travels inside its project file.
//!
//! Both are edited as ordinary tabs and applied with the ordinary update
//! gesture. Neither is a scene: a prebake has no file of its own in the set
//! folder, no number, no pad and no launch, because it is not music. It runs
//! before the music.

use std::path::{Path, PathBuf};

use super::bounded_file::{self, MAX_DOCUMENT_BYTES};

/// Which of the two setups a text is.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PrebakeScope {
    /// Kept beside the studio's settings and run for every set.
    Global,
    /// Kept inside the set's project file and run for that set alone.
    Local,
}

impl PrebakeScope {
    /// Both, in the order they are applied. Global first: a set's own setup
    /// is written knowing what the global one already defined.
    pub const ALL: [PrebakeScope; 2] = [PrebakeScope::Global, PrebakeScope::Local];

    /// Index into a two-slot array, in application order.
    pub const fn index(self) -> usize {
        match self {
            PrebakeScope::Global => 0,
            PrebakeScope::Local => 1,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            PrebakeScope::Global => "global",
            PrebakeScope::Local => "local",
        }
    }

    /// What the chip, the header and every status line call it.
    pub fn tab_name(self) -> String {
        format!("prebake ({})", self.label())
    }

    /// Where its text is kept, for a message that has to say so.
    pub const fn home(self) -> &'static str {
        match self {
            PrebakeScope::Global => PREBAKE_FILE_NAME,
            PrebakeScope::Local => super::scenes::SET_FILE_NAME,
        }
    }
}

/// The global prebake's file, beside `studio.json`.
pub const PREBAKE_FILE_NAME: &str = "prebake.strudel";

/// The mark a prebake tab wears instead of a scene's number: one column
/// wide, and nothing else on the strip uses it.
pub const PREBAKE_GLYPH: char = '⚙';

/// What an empty prebake opens with.
///
/// Comments only, so an untouched starter is still blank and is never
/// evaluated. It says the two things that are surprising the first time:
/// a top-level `const` does not reach a score, and setup may not set tempo.
pub const PREBAKE_STARTER: &str = "\
// Setup for every score in this set. It runs on the same heap, before them.
//
// Share a helper by hanging it on globalThis, or by registering a method:
//   globalThis.riff = () => note(\"c e g\")
//   register('swell', (amount, pat) => pat.gain(amount))
//
// A top-level const stays in here - scores cannot see it.
// samples(...) and preload(...) belong here; setcps, midin and initHydra
// are refused, because setup runs before there is anything to play.
//
// The footer shows the current shortcuts to apply setup or close this tab.
// The settings sheet reopens it.
";

/// Whether there is nothing here at all.
///
/// Deliberately whitespace only, not "no code": a header of comments is
/// something someone wrote and is kept and run like any other text, where
/// running it is a no-op. Only a prebake emptied down to nothing stops
/// being stored, so emptying one really does leave the folder as it was.
pub fn is_blank(source: &str) -> bool {
    source.trim().is_empty()
}

/// What became of a prebake's stored text.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PrebakeVerdict {
    /// Never offered to the engine, or edited since it was.
    #[default]
    Unchecked,
    /// The engine ran it.
    Applied,
    /// The checker or the engine refused it. It is still kept.
    Rejected,
}

/// One prebake as the settings sheet shows it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrebakeRow {
    /// Lines of stored text; zero when there is none.
    pub lines: usize,
    pub blank: bool,
    /// Of the STORED text, not of whatever a tab is showing.
    pub verdict: PrebakeVerdict,
    /// A tab is open on it.
    pub open: bool,
    /// That tab has edits the store has not seen.
    pub dirty: bool,
}

impl PrebakeRow {
    /// The sheet's value column: what is there, and whether it ran.
    pub fn value(self) -> String {
        let mut value = if self.blank {
            "empty".to_owned()
        } else {
            let size = format!(
                "{} line{}",
                self.lines,
                if self.lines == 1 { "" } else { "s" }
            );
            match self.verdict {
                PrebakeVerdict::Unchecked => size,
                PrebakeVerdict::Applied => format!("applied · {size}"),
                PrebakeVerdict::Rejected => format!("rejected · {size}"),
            }
        };
        if self.dirty {
            value.push_str(" ●");
        }
        value
    }
}

/// Where the global prebake lives: Rustel's config directory plus
/// `prebake.strudel`, in the folder that already holds `studio.json` and the
/// user themes.
///
/// A plain `.strudel` file rather than a field in the settings: setup is
/// source, and source belongs in a file that an editor, a diff and a paste
/// into a friend's studio can all read.
pub fn global_path() -> Option<PathBuf> {
    super::config::directory().map(|directory| directory.join(PREBAKE_FILE_NAME))
}

/// Where to read the global prebake. The canonical file wins; on macOS only,
/// the former Application Support file remains a last-resort read fallback.
pub(crate) fn global_read_path() -> Option<PathBuf> {
    super::config::read_path(PREBAKE_FILE_NAME)
}

/// The global prebake's text. A missing or unreadable file, or one over the
/// editor's document limit, is no prebake, which is also what an empty one
/// means, so none needs reporting.
pub fn load_global_from(path: Option<&Path>) -> String {
    path.and_then(|path| bounded_file::read_to_string(path, MAX_DOCUMENT_BYTES).ok())
        .unwrap_or_default()
}

/// Write the global prebake, or remove it when there is nothing left to
/// keep, so emptying one leaves the folder as it was found.
pub fn save_global_to(path: Option<&Path>, text: &str) -> Result<(), String> {
    let Some(path) = path else {
        return Err("no config folder on this system".to_owned());
    };
    if is_blank(text) {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
        };
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    super::save::atomic_write(path, text)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod size_limit_tests {
    //! A global prebake over the editor's document limit is no prebake, and its
    //! file is left as it was.

    use super::*;

    #[test]
    fn an_oversized_global_prebake_is_skipped_without_changing_its_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(PREBAKE_FILE_NAME);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_DOCUMENT_BYTES + 1).unwrap();

        assert!(load_global_from(Some(&path)).is_empty());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            MAX_DOCUMENT_BYTES + 1
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_names_itself_and_where_it_lives() {
        assert_eq!(PrebakeScope::Global.tab_name(), "prebake (global)");
        assert_eq!(PrebakeScope::Local.tab_name(), "prebake (local)");
        assert_eq!(PrebakeScope::Global.home(), "prebake.strudel");
        assert_eq!(PrebakeScope::Local.home(), "rustel-set.json");
        assert_eq!(PrebakeScope::ALL[0].index(), 0);
        assert_eq!(PrebakeScope::ALL[1].index(), 1);
    }

    #[test]
    fn only_nothing_at_all_counts_as_blank() {
        assert!(is_blank(""));
        assert!(is_blank("   \n\t\n"));
        // Comments are text someone wrote: kept, and run as the no-op they are.
        assert!(!is_blank(PREBAKE_STARTER));
        assert!(!is_blank("// a note to self\n"));
        assert!(!is_blank("globalThis.a = 1"));
    }

    #[test]
    fn the_settings_value_says_what_is_there_and_whether_it_ran() {
        assert_eq!(
            PrebakeRow {
                blank: true,
                ..PrebakeRow::default()
            }
            .value(),
            "empty"
        );
        assert_eq!(
            PrebakeRow {
                lines: 1,
                ..PrebakeRow::default()
            }
            .value(),
            "1 line"
        );
        assert_eq!(
            PrebakeRow {
                lines: 12,
                verdict: PrebakeVerdict::Applied,
                ..PrebakeRow::default()
            }
            .value(),
            "applied · 12 lines"
        );
        assert_eq!(
            PrebakeRow {
                lines: 7,
                verdict: PrebakeVerdict::Rejected,
                dirty: true,
                ..PrebakeRow::default()
            }
            .value(),
            "rejected · 7 lines ●"
        );
    }

    #[test]
    fn an_emptied_global_prebake_leaves_no_file_behind() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("nested").join(PREBAKE_FILE_NAME);

        save_global_to(Some(&path), "globalThis.riff = () => note('c')").expect("write");
        assert_eq!(
            load_global_from(Some(&path)),
            "globalThis.riff = () => note('c')"
        );

        // A header of comments is text someone wrote: kept.
        save_global_to(Some(&path), "// notes to self\n").expect("comments");
        assert_eq!(load_global_from(Some(&path)), "// notes to self\n");

        save_global_to(Some(&path), "   \n").expect("empty");
        assert!(!path.exists(), "an emptied prebake left its file behind");
        assert!(load_global_from(Some(&path)).is_empty());
        // Removing one that is already gone is not an error.
        save_global_to(Some(&path), "").expect("already gone");
    }

    #[test]
    fn without_a_config_folder_a_global_prebake_says_so() {
        assert!(save_global_to(None, "globalThis.a = 1").is_err());
        assert!(load_global_from(None).is_empty());
    }
}
