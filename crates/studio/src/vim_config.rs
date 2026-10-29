//! Native Vim key overrides and their external configuration file.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::keybinds::BindAction;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_SEQUENCE_KEYS: usize = 64;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const SETTLE_TIME: Duration = Duration::from_millis(250);
const STARTER: &str = "\" Studio reads Vim mappings from this file.\n\
\" Supported: noremap, map, nnoremap, inoremap, vnoremap, xnoremap, onoremap.\n\
\" Lines Studio cannot use are skipped.\n";

/// Mappings shipped with the studio, beneath the player's own file. An
/// empty line here changes nothing. A user line for the same keys wins.
const DEFAULT_VIMRC: &str = include_str!("../assets/vimrc");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VimMappingMode {
    Normal,
    Insert,
    Visual,
    OperatorPending,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VimMapping {
    pub(crate) keys: Vec<KeyEvent>,
    pub(crate) replacement: Vec<VimMappedKey>,
}

/// One step of a replacement: a key, or a named Studio action. The action
/// follows its binding, so a rebind changes the mapping with it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VimMappedKey {
    Key(KeyEvent),
    Action(BindAction),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct VimMappings {
    normal: Vec<VimMapping>,
    insert: Vec<VimMapping>,
    visual: Vec<VimMapping>,
    operator_pending: Vec<VimMapping>,
}

impl VimMappings {
    pub(crate) fn for_mode(&self, mode: VimMappingMode) -> &[VimMapping] {
        match mode {
            VimMappingMode::Normal => &self.normal,
            VimMappingMode::Insert => &self.insert,
            VimMappingMode::Visual => &self.visual,
            VimMappingMode::OperatorPending => &self.operator_pending,
        }
    }

    /// Parse exactly the vimrc, without the studio defaults. Tests use
    /// this to check the file rules alone.
    #[cfg(test)]
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        Self::parse_text(text, false)
    }

    /// Parse a vimrc with the studio's Vim defaults beneath it. A later
    /// definition of the same keys replaces a default, as in Vim.
    pub(crate) fn parse_with_defaults(text: &str) -> Result<Self, String> {
        Self::parse_text(text, true)
    }

    fn parse_text(text: &str, defaults: bool) -> Result<Self, String> {
        let mut mappings = Self::default();
        if defaults {
            // gd is Vim's go-to-definition. Docs answers here while Vim
            // owns Ctrl+D. The shipped defaults may replace it, and a vimrc
            // line replaces both.
            parse_map_line("nnoremap gd <Action:docs>", &mut mappings)?;
            for (number, line) in DEFAULT_VIMRC.lines().enumerate() {
                parse_map_line(line, &mut mappings)
                    .map_err(|error| format!("default vimrc line {}: {error}", number + 1))?;
            }
        }
        // An editor can save the file with a byte order mark.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for (number, line) in text.lines().enumerate() {
            parse_map_line(line, &mut mappings)
                .map_err(|error| format!("line {}: {error}", number + 1))?;
        }
        Ok(mappings)
    }

    fn for_mode_mut(&mut self, mode: VimMappingMode) -> &mut Vec<VimMapping> {
        match mode {
            VimMappingMode::Normal => &mut self.normal,
            VimMappingMode::Insert => &mut self.insert,
            VimMappingMode::Visual => &mut self.visual,
            VimMappingMode::OperatorPending => &mut self.operator_pending,
        }
    }
}

/// One Vimscript map line. Everything else in the file - settings, plugins,
/// functions, comments - is skipped, so a real `.vimrc` or `init.vim` can be
/// used as the mapping file.
fn parse_map_line(line: &str, mappings: &mut VimMappings) -> Result<(), String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('"') {
        return Ok(());
    }
    let line = line.strip_prefix(':').unwrap_or(line);
    let Some((command, after)) = first_word(line) else {
        return Ok(());
    };
    let modes: &[VimMappingMode] = match command {
        "nnoremap" | "nmap" => &[VimMappingMode::Normal],
        "inoremap" | "imap" => &[VimMappingMode::Insert],
        "vnoremap" | "vmap" | "xnoremap" | "xmap" => &[VimMappingMode::Visual],
        "onoremap" | "omap" => &[VimMappingMode::OperatorPending],
        "noremap" | "map" => &[
            VimMappingMode::Normal,
            VimMappingMode::Visual,
            VimMappingMode::OperatorPending,
        ],
        _ => return Ok(()),
    };
    let Some((lhs, after)) = first_word(without_map_options(after)) else {
        return Ok(());
    };
    // Keep runs of spaces inside the replacement. The first run after the
    // left side only separates the two.
    let rhs = after.trim_start();
    if rhs.is_empty() {
        return Ok(());
    }
    let lower = rhs.to_ascii_lowercase();
    // Studio does not run plugin calls.
    if ["<cmd>", "<plug>", "<sid>"]
        .iter()
        .any(|notation| lower.contains(notation))
    {
        return Ok(());
    }
    let Ok(keys) = parse_keys(lhs) else {
        return Ok(());
    };
    if keys.is_empty() {
        return Ok(());
    }
    let Ok(replacement) = parse_replacement(rhs) else {
        return Ok(());
    };
    if reaches_the_command_line(modes.contains(&VimMappingMode::Insert), &replacement) {
        return Ok(());
    }
    for mode in modes {
        let list = mappings.for_mode_mut(*mode);
        // The last definition of the same keys wins, as in Vim, however
        // they are spelt.
        if let Some(existing) = list.iter_mut().find(|mapping| mapping.keys == keys) {
            existing.replacement = replacement.clone();
        } else {
            if list.len() >= 256 {
                return Err(format!("{mode:?} has more than 256 mappings"));
            }
            list.push(VimMapping {
                keys: keys.clone(),
                replacement: replacement.clone(),
            });
        }
    }
    Ok(())
}

const MAP_OPTIONS: [&str; 6] = [
    "<silent>",
    "<nowait>",
    "<buffer>",
    "<unique>",
    "<special>",
    "<script>",
];

/// The text after the map flags. Vim reads the flags with or without
/// spaces between them, as in `<silent><Space>V`.
fn without_map_options(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        let Some(option) = MAP_OPTIONS.iter().find(|option| {
            text.get(..option.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(option))
        }) else {
            return text;
        };
        text = &text[option.len()..];
    }
}

/// Whether a replacement reaches Vim's command line, as an Ex command
/// does, or asks for register entry with Ctrl+R. Studio runs neither, so
/// the keys would land somewhere else.
///
/// A `:` opens the command line in Normal, Visual or a pending operator.
/// A map starts there unless it is an Insert map, and Esc, Ctrl+[, Ctrl+C
/// and Ctrl+O return there. The keys that start Insert or a search make
/// the keys after them text, and `r`, `f` and `t` read the next key as a
/// character.
fn reaches_the_command_line(insert: bool, replacement: &[VimMappedKey]) -> bool {
    #[derive(Clone, Copy)]
    enum Typing {
        Command,
        Character,
        Insert,
        Search,
    }
    let mut typing = if insert {
        Typing::Insert
    } else {
        Typing::Command
    };
    for step in replacement {
        let VimMappedKey::Key(key) = step else {
            continue;
        };
        let control = key.modifiers == KeyModifiers::CONTROL;
        if key.code == KeyCode::Esc || control && matches!(key.code, KeyCode::Char('[' | 'c' | 'o'))
        {
            typing = Typing::Command;
            continue;
        }
        if control && key.code == KeyCode::Char('r') {
            if matches!(typing, Typing::Insert | Typing::Search) {
                return true;
            }
            continue;
        }
        typing = match (typing, key.code) {
            (Typing::Character, _) | (Typing::Search, KeyCode::Enter) => Typing::Command,
            (typing, _) if !key.modifiers.is_empty() => typing,
            (Typing::Command, KeyCode::Char(':')) => return true,
            (Typing::Command, KeyCode::Char('/' | '?')) => Typing::Search,
            (
                Typing::Command,
                KeyCode::Char('i' | 'a' | 'I' | 'A' | 'o' | 'O' | 's' | 'S' | 'c' | 'C' | 'R'),
            ) => Typing::Insert,
            (Typing::Command, KeyCode::Char('r' | 'f' | 't' | 'F' | 'T')) => Typing::Character,
            (typing, _) => typing,
        };
    }
    false
}

/// The first word, and the rest of the line after it.
fn first_word(text: &str) -> Option<(&str, &str)> {
    let start = text.find(|character: char| !character.is_whitespace())?;
    let word = &text[start..];
    let end = word.find(char::is_whitespace).unwrap_or(word.len());
    Some((&word[..end], &word[end..]))
}

const ACTION_PREFIX: &str = "<action:";

/// A replacement is keys plus named Studio actions. An action follows its
/// binding, so a rebind moves the mapping with it.
fn parse_replacement(text: &str) -> Result<Vec<VimMappedKey>, String> {
    let mut mapped = Vec::new();
    let mut rest = text;
    while let Some((start, end)) = action_token(rest)? {
        let before = &rest[..start];
        if !before.is_empty() {
            mapped.extend(parse_keys(before)?.into_iter().map(VimMappedKey::Key));
        }
        let name = rest[start + ACTION_PREFIX.len()..end].trim();
        let action =
            parse_action(name).ok_or_else(|| format!("unknown action <{}>", &rest[start..=end]))?;
        mapped.push(VimMappedKey::Action(action));
        rest = &rest[end + 1..];
        if mapped.len() > MAX_SEQUENCE_KEYS {
            return Err(format!("a sequence exceeds {MAX_SEQUENCE_KEYS} keys"));
        }
    }
    if !rest.is_empty() {
        mapped.extend(parse_keys(rest)?.into_iter().map(VimMappedKey::Key));
    }
    if mapped.len() > MAX_SEQUENCE_KEYS {
        return Err(format!("a sequence exceeds {MAX_SEQUENCE_KEYS} keys"));
    }
    Ok(mapped)
}

/// The stable action name, or the label shown on the Keybinds page.
fn parse_action(name: &str) -> Option<BindAction> {
    let name = name.trim();
    BindAction::parse_key(&name.to_ascii_lowercase()).or_else(|| {
        BindAction::ALL
            .into_iter()
            .find(|action| action.label().eq_ignore_ascii_case(name))
    })
}

/// The byte range of the next `<Action:...>` token: the `<` and the `>`.
fn action_token(text: &str) -> Result<Option<(usize, usize)>, String> {
    let lower = text.to_ascii_lowercase();
    let Some(start) = lower.find(ACTION_PREFIX) else {
        return Ok(None);
    };
    let Some(offset) = text[start..].find('>') else {
        return Err("unclosed action notation, use <Action:name>".into());
    };
    Ok(Some((start, start + offset)))
}

fn parse_keys(text: &str) -> Result<Vec<KeyEvent>, String> {
    if text.eq_ignore_ascii_case("<Nop>") {
        return Ok(Vec::new());
    }
    let mut keys = Vec::new();
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        let key = if character == '<' {
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('>') => break,
                    Some(character) => name.push(character),
                    None => return Err("unclosed key notation, use <lt> for <".into()),
                }
            }
            named_key(&name)?
        } else if character.is_control() {
            return Err("use key notation for control characters".into());
        } else {
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
        };
        keys.push(canonical_key(key));
        if keys.len() > MAX_SEQUENCE_KEYS {
            return Err(format!("a sequence exceeds {MAX_SEQUENCE_KEYS} keys"));
        }
    }
    Ok(keys)
}

fn named_key(name: &str) -> Result<KeyEvent, String> {
    let mut rest = name;
    let mut modifiers = KeyModifiers::NONE;
    while let Some((prefix, remaining)) = rest.split_once('-') {
        let modifier = match prefix.to_ascii_lowercase().as_str() {
            "c" | "ctrl" => KeyModifiers::CONTROL,
            "s" | "shift" => KeyModifiers::SHIFT,
            "a" | "m" | "alt" => KeyModifiers::ALT,
            _ => break,
        };
        modifiers.insert(modifier);
        rest = remaining;
    }
    let code = match rest.to_ascii_lowercase().as_str() {
        "esc" => KeyCode::Esc,
        "cr" | "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "bs" | "backspace" => KeyCode::Backspace,
        "del" | "delete" => KeyCode::Delete,
        "space" => KeyCode::Char(' '),
        "lt" => KeyCode::Char('<'),
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        value if value.starts_with('f') && value.len() > 1 => {
            let number = value[1..]
                .parse::<u8>()
                .map_err(|_| format!("unknown key <{name}>"))?;
            if !(1..=24).contains(&number) {
                return Err("function keys must be F1 through F24".into());
            }
            KeyCode::F(number)
        }
        _ => {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(character), None) => KeyCode::Char(
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT)
                    {
                        character.to_ascii_lowercase()
                    } else {
                        character
                    },
                ),
                _ => return Err(format!("unknown key <{name}>")),
            }
        }
    };
    Ok(KeyEvent::new(code, modifiers))
}

pub(crate) fn canonical_key(key: KeyEvent) -> KeyEvent {
    let mut code = key.code;
    let mut modifiers = key.modifiers;
    if let KeyCode::Char(character) = code {
        if modifiers.contains(KeyModifiers::SHIFT) && character.is_ascii_lowercase() {
            code = KeyCode::Char(character.to_ascii_uppercase());
            modifiers.remove(KeyModifiers::SHIFT);
        } else if character.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(character) {
            modifiers.remove(KeyModifiers::SHIFT);
        }
    } else if code == KeyCode::BackTab {
        code = KeyCode::Tab;
        modifiers.insert(KeyModifiers::SHIFT);
    }
    KeyEvent::new(code, modifiers)
}

pub(crate) fn path(custom: Option<&str>) -> Option<PathBuf> {
    if let Some(custom) = custom.filter(|value| !value.trim().is_empty()) {
        if let Some(rest) = custom
            .strip_prefix("~/")
            .or_else(|| custom.strip_prefix("~\\"))
        {
            return rustel_runtime::config_dir::home().map(|home| PathBuf::from(home).join(rest));
        }
        Some(PathBuf::from(custom))
    } else {
        crate::config::directory()
            .and_then(|directory| std::path::absolute(directory.join("vimrc")).ok())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Snapshot {
    Missing,
    Text(String),
    Error(String),
}

/// A cheap identity for the file, checked before reading its text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileStamp {
    Missing,
    Present {
        modified: Option<SystemTime>,
        len: u64,
    },
    Unreadable,
}

fn file_stamp(path: &Path) -> FileStamp {
    match fs::metadata(path) {
        Ok(metadata) => FileStamp::Present {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => FileStamp::Missing,
        Err(_) => FileStamp::Unreadable,
    }
}

fn snapshot(path: &Path) -> Snapshot {
    if !path.is_absolute() {
        return Snapshot::Error("use a full file path".into());
    }
    match crate::bounded_file::read_to_string(path, MAX_CONFIG_BYTES) {
        Ok(text) => Snapshot::Text(text),
        Err(crate::bounded_file::ReadError::Io(error))
            if error.kind() == io::ErrorKind::NotFound =>
        {
            Snapshot::Missing
        }
        Err(crate::bounded_file::ReadError::Io(error)) => {
            Snapshot::Error(format!("cannot read the file ({:?})", error.kind()))
        }
        Err(crate::bounded_file::ReadError::TooLarge(_)) => {
            Snapshot::Error("file exceeds 64 KiB".into())
        }
    }
}

fn create_starter(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(STARTER.as_bytes()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Default)]
pub(crate) struct VimConfig {
    enabled: bool,
    path: Option<PathBuf>,
    starter_attempted: bool,
    mappings: Arc<VimMappings>,
    observed: Option<Snapshot>,
    stamp: Option<FileStamp>,
    pending: Option<(Snapshot, Instant)>,
    next_poll: Option<Instant>,
    error: Option<String>,
    notice: Option<String>,
    has_valid: bool,
}

impl VimConfig {
    pub(crate) fn mappings(&self) -> Arc<VimMappings> {
        Arc::clone(&self.mappings)
    }

    #[cfg(test)]
    pub(crate) fn set_mappings_for_test(&mut self, mappings: VimMappings) {
        self.mappings = Arc::new(mappings);
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref().filter(|_| self.enabled)
    }

    pub(crate) fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    pub(crate) fn configure(&mut self, enabled: bool, custom: Option<&str>, now: Instant) -> bool {
        if !enabled {
            self.enabled = false;
            return false;
        }
        let custom = custom.filter(|value| !value.trim().is_empty());
        let path = path(custom);
        if self.enabled && path == self.path {
            return false;
        }
        let changed_path = path != self.path;
        self.enabled = true;
        self.path = path;
        self.pending = None;
        self.next_poll = Some(now + POLL_INTERVAL);
        if changed_path {
            self.mappings = Arc::default();
            self.has_valid = false;
        }
        let mut creation_error = None;
        if custom.is_none() && !self.starter_attempted {
            self.starter_attempted = true;
            if let Some(path) = &self.path
                && let Err(error) = create_starter(path)
            {
                creation_error = Some(format!(
                    "Vim mappings: cannot create the starter file ({:?})",
                    error.kind()
                ));
            }
        }
        self.stamp = self.path.as_deref().map(file_stamp);
        let current =
            self.path.as_deref().map(snapshot).unwrap_or_else(|| {
                Snapshot::Error("configuration directory is unavailable".into())
            });
        self.apply(current);
        if let Some(error) = creation_error
            && self.error.is_none()
        {
            self.error = Some(error.clone());
            self.notice = Some(error);
        }
        true
    }

    pub(crate) fn poll(&mut self, now: Instant) -> bool {
        if !self.enabled || self.next_poll.is_some_and(|next| now < next) {
            return false;
        }
        self.next_poll = Some(now + POLL_INTERVAL);
        let Some(path) = &self.path else {
            return false;
        };
        // Stat first. The text is read only when the file actually changed,
        // so an idle studio does not read the mapping file twice a second.
        // A file that failed to read is read again: a permission fix does
        // not change its stamp.
        let stamp = file_stamp(path);
        let failed = matches!(self.observed, Some(Snapshot::Error(_)));
        if self.stamp == Some(stamp) && !failed {
            // The file is unchanged. A read taken on the last change may
            // have settled by now.
            if let Some((pending, since)) = &self.pending
                && now.saturating_duration_since(*since) >= SETTLE_TIME
            {
                let current = pending.clone();
                self.pending = None;
                return self.apply(current);
            }
            return false;
        }
        self.stamp = Some(stamp);
        let current = snapshot(path);
        if self.observed.as_ref() == Some(&current) {
            self.pending = None;
            return false;
        }
        if let Some((pending, since)) = &self.pending
            && pending == &current
            && now.saturating_duration_since(*since) >= SETTLE_TIME
        {
            self.pending = None;
            return self.apply(current);
        }
        self.pending = Some((current, now));
        false
    }

    fn apply(&mut self, current: Snapshot) -> bool {
        let result = match &current {
            Snapshot::Missing => VimMappings::parse_with_defaults(""),
            Snapshot::Text(text) => VimMappings::parse_with_defaults(text),
            Snapshot::Error(error) => Err(error.clone()),
        };
        self.observed = Some(current);
        match result {
            Ok(mappings) => {
                let changed = *self.mappings != mappings;
                if changed {
                    self.mappings = Arc::new(mappings);
                    self.notice = Some(if self.has_valid {
                        "Vim mappings reloaded".into()
                    } else {
                        "Vim mappings loaded".into()
                    });
                } else if self.error.is_some() {
                    self.notice = Some("Vim mappings loaded".into());
                }
                self.error = None;
                self.has_valid = true;
                changed
            }
            Err(error) => {
                let mut changed = false;
                if !self.has_valid {
                    // A file Studio cannot use still leaves the shipped
                    // mappings, as a missing file does.
                    let shipped = VimMappings::parse_with_defaults("").unwrap_or_default();
                    changed = *self.mappings != shipped;
                    self.mappings = Arc::new(shipped);
                }
                let fallback = if self.has_valid {
                    "previous bindings kept"
                } else {
                    "built-in bindings active"
                };
                let message = format!("Vim mappings: {error} · {fallback}");
                self.error = Some(message.clone());
                self.notice = Some(message);
                changed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gd_opens_docs_and_a_vimrc_mapping_replaces_it() {
        let gd = |mappings: &VimMappings| {
            mappings
                .for_mode(VimMappingMode::Normal)
                .iter()
                .find(|mapping| {
                    mapping.keys
                        == vec![
                            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
                            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
                        ]
                })
                .cloned()
        };
        let defaults = VimMappings::parse_with_defaults("").expect("defaults");
        assert_eq!(
            gd(&defaults).expect("gd default").replacement,
            vec![VimMappedKey::Action(BindAction::Docs)]
        );

        let overridden = VimMappings::parse_with_defaults("nnoremap gd x\n").expect("vimrc");
        assert_eq!(
            gd(&overridden).expect("gd mapping").replacement,
            vec![VimMappedKey::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE
            ))]
        );

        let kept = VimMappings::parse_with_defaults("nnoremap K ?\\$<CR>\n").expect("vimrc");
        assert_eq!(
            gd(&kept).expect("gd default").replacement,
            vec![VimMappedKey::Action(BindAction::Docs)],
            "another mapping leaves gd alone"
        );
    }

    #[test]
    fn the_shipped_default_vimrc_binds_studio_actions() {
        let mappings = VimMappings::parse_with_defaults("").expect("defaults");
        let normal = mappings.for_mode(VimMappingMode::Normal);
        let visual = mappings.for_mode(VimMappingMode::Visual);
        let insert = mappings.for_mode(VimMappingMode::Insert);
        assert_eq!(normal.len(), 17, "the 16 shipped normal maps and gd");
        assert_eq!(visual.len(), 3);
        assert_eq!(insert.len(), 1);
        let replacement = |list: &[VimMapping], text: &str| {
            let wanted = parse_keys(text).expect("keys");
            list.iter()
                .find(|mapping| mapping.keys == wanted)
                .unwrap_or_else(|| panic!("missing mapping {text}"))
                .replacement
                .clone()
        };
        assert_eq!(
            replacement(normal, "U"),
            vec![VimMappedKey::Action(BindAction::Evaluate)]
        );
        assert_eq!(
            replacement(normal, "Q"),
            vec![VimMappedKey::Action(BindAction::Stop)]
        );
        assert_eq!(
            replacement(visual, "gc"),
            vec![VimMappedKey::Action(BindAction::ToggleComment)]
        );
        assert_eq!(
            replacement(insert, "jk"),
            vec![VimMappedKey::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            ))]
        );
        assert_eq!(
            replacement(normal, "<Space>V"),
            vec![VimMappedKey::Action(BindAction::Visualizers)]
        );
    }

    #[test]
    fn vimscript_maps_keep_modes_and_key_sequences_distinct() {
        let mappings = VimMappings::parse(
            "\" Studio reads this file like a small .vimrc.\n\
             nnoremap <C-R> u\n\
             nnoremap <S-x> <Nop>\n\
             nnoremap <silent> J /\\$<CR>\n\
             inoremap jk <Esc>\n\
             vnoremap H 0\n\
             onoremap q iw\n\
             set number\n\
             nnoremap <Leader>x :noh<CR>\n\
             nnoremap U :RustelUpdate<CR>\n\
             nnoremap <Space>V <Action:visualizers>\n\
             nnoremap <Space>U <action:update>\n\
             nnoremap <Space>X <Action:not-an-action>\n\
             nnoremap <unknown> x\n",
        )
        .expect("valid mappings");
        assert_eq!(
            mappings.for_mode(VimMappingMode::Insert),
            &[VimMapping {
                keys: vec![
                    KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                    KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)
                ],
                replacement: vec![VimMappedKey::Key(KeyEvent::new(
                    KeyCode::Esc,
                    KeyModifiers::NONE
                ))],
            }]
        );
        let normal = mappings.for_mode(VimMappingMode::Normal);
        assert_eq!(
            normal[0].keys,
            vec![KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)]
        );
        assert_eq!(
            normal[1].keys,
            vec![KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE)]
        );
        assert!(normal[1].replacement.is_empty());
        let search = normal
            .iter()
            .find(|mapping| {
                mapping.keys == vec![KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE)]
            })
            .expect("the J search mapping");
        assert_eq!(
            search.replacement,
            vec![
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ],
            "the search prefix, the escaped dollar and Enter"
        );
        let action_of = |keys: &[KeyEvent]| {
            normal
                .iter()
                .find(|mapping| mapping.keys.as_slice() == keys)
                .map(|mapping| mapping.replacement.clone())
        };
        assert_eq!(
            action_of(&[
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
            ]),
            Some(vec![VimMappedKey::Action(BindAction::Visualizers)]),
            "a named action follows its keybinding"
        );
        assert_eq!(
            action_of(&[
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('U'), KeyModifiers::NONE),
            ]),
            Some(vec![VimMappedKey::Action(BindAction::Evaluate)]),
            "the action name is case-insensitive"
        );
        assert!(
            action_of(&[
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
            ])
            .is_none(),
            "an unknown action skips the line"
        );
        assert!(
            normal.iter().all(|mapping| mapping.keys
                != vec![KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)]),
            "<leader> and unknown notation are skipped, not guessed"
        );
        assert!(
            normal.iter().all(|mapping| mapping.keys
                != vec![KeyEvent::new(KeyCode::Char('U'), KeyModifiers::NONE)]),
            "an Ex command replacement is skipped, not typed into the score"
        );
        assert_ne!(
            mappings.for_mode(VimMappingMode::Visual),
            mappings.for_mode(VimMappingMode::OperatorPending)
        );
        // Two spellings of the same key do not fail the file: the last
        // definition wins, as in Vim.
        let duplicates = VimMappings::parse("nnoremap X x\nnnoremap <S-x> h\n")
            .expect("duplicates are tolerated");
        let duplicates = duplicates.for_mode(VimMappingMode::Normal);
        assert_eq!(duplicates.len(), 1);
        assert_eq!(
            duplicates[0].replacement,
            vec![VimMappedKey::Key(KeyEvent::new(
                KeyCode::Char('h'),
                KeyModifiers::NONE
            ))],
            "the last definition wins"
        );
        let spaced = VimMappings::parse("inoremap ,, i a  b<Esc>\n").expect("spacing");
        assert_eq!(
            spaced.for_mode(VimMappingMode::Insert)[0].replacement,
            vec![
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)),
                VimMappedKey::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ],
            "runs of spaces stay in the replacement"
        );
        let brackets = VimMappings::parse("nnoremap <C-[> h\nnnoremap <C-S-[> l\n")
            .expect("distinct bracket shortcuts");
        let brackets = brackets.for_mode(VimMappingMode::Normal);
        assert_ne!(brackets[0].keys, brackets[1].keys);
        assert_eq!(
            canonical_key(KeyEvent::new(
                KeyCode::Char('['),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
            KeyEvent::new(
                KeyCode::Char('['),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )
        );
    }

    fn replacement_of(
        mappings: &VimMappings,
        mode: VimMappingMode,
        keys: &str,
    ) -> Option<Vec<VimMappedKey>> {
        let wanted = parse_keys(keys).expect("keys");
        mappings
            .for_mode(mode)
            .iter()
            .find(|mapping| mapping.keys == wanted)
            .map(|mapping| mapping.replacement.clone())
    }

    /// A replacement that reaches the command line or asks for register
    /// entry runs nothing Studio supports, so the line is skipped. A
    /// search and a typed colon still map.
    #[test]
    fn a_command_or_register_entry_inside_a_replacement_skips_the_line() {
        let mappings = VimMappings::parse(
            "inoremap <C-s> <Esc>:w<CR>\n\
             inoremap <C-q> <C-o>:w<CR>\n\
             inoremap <C-x> <C-[>:w<CR>\n\
             inoremap <C-y> <C-c>:w<CR>\n\
             inoremap <F2> <C-r>=strftime('%c')<CR>\n\
             nnoremap <F3> A;<Esc>:w<CR>\n\
             vnoremap <F4> y:echo<CR>\n\
             inoremap ,, a:b\n\
             nnoremap J /\\$<CR>\n\
             nnoremap ,c a:<Esc>\n\
             nnoremap ,r r:\n\
             nnoremap ,u <C-r>\n",
        )
        .expect("mappings");
        for (mode, keys) in [
            (VimMappingMode::Insert, "<C-s>"),
            (VimMappingMode::Insert, "<C-q>"),
            (VimMappingMode::Insert, "<C-x>"),
            (VimMappingMode::Insert, "<C-y>"),
            (VimMappingMode::Insert, "<F2>"),
            (VimMappingMode::Normal, "<F3>"),
            (VimMappingMode::Visual, "<F4>"),
        ] {
            assert_eq!(
                replacement_of(&mappings, mode, keys),
                None,
                "{keys} is skipped"
            );
        }
        for (mode, keys) in [
            (VimMappingMode::Insert, ",,"),
            (VimMappingMode::Normal, "J"),
            (VimMappingMode::Normal, ",c"),
            (VimMappingMode::Normal, ",r"),
            (VimMappingMode::Normal, ",u"),
        ] {
            assert!(
                replacement_of(&mappings, mode, keys).is_some(),
                "{keys} maps"
            );
        }
    }

    /// Two spellings of the same keys are one mapping, and the last
    /// definition wins, as in Vim.
    #[test]
    fn the_last_definition_of_the_same_keys_wins_across_spellings() {
        let mappings = VimMappings::parse_with_defaults(
            "nnoremap <space>V <Action:mixer>\nnnoremap <Space>V <Action:log>\n",
        )
        .expect("mappings");
        assert_eq!(
            replacement_of(&mappings, VimMappingMode::Normal, "<Space>V"),
            Some(vec![VimMappedKey::Action(BindAction::Log)])
        );
        let mappings =
            VimMappings::parse("nnoremap X a\nnnoremap <S-x> b\nnnoremap X c\n").expect("mappings");
        assert_eq!(
            replacement_of(&mappings, VimMappingMode::Normal, "X"),
            Some(vec![VimMappedKey::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::NONE
            ))])
        );
    }

    /// Vim reads map flags written together, with no space before the keys.
    #[test]
    fn flags_written_together_are_read_before_the_keys() {
        let mappings = VimMappings::parse(
            "nnoremap <silent><Space>V <Action:mixer>\n\
             nnoremap <silent><nowait> Q <Action:stop>\n",
        )
        .expect("mappings");
        assert_eq!(
            replacement_of(&mappings, VimMappingMode::Normal, "<Space>V"),
            Some(vec![VimMappedKey::Action(BindAction::Mixer)])
        );
        assert_eq!(
            replacement_of(&mappings, VimMappingMode::Normal, "Q"),
            Some(vec![VimMappedKey::Action(BindAction::Stop)])
        );
    }

    /// A file saved with a UTF-8 byte order mark keeps its first line.
    #[test]
    fn a_byte_order_mark_does_not_hide_the_first_map() {
        let mappings = VimMappings::parse("\u{feff}nnoremap Q <Action:stop>\n").expect("mappings");
        assert_eq!(
            replacement_of(&mappings, VimMappingMode::Normal, "Q"),
            Some(vec![VimMappedKey::Action(BindAction::Stop)])
        );
    }

    /// A vimrc Studio cannot read reports why, and the shipped mappings
    /// stay, as they do for a missing file.
    #[test]
    fn an_unreadable_vimrc_keeps_the_shipped_mappings() {
        let directory = tempfile::tempdir().expect("temporary folder");
        let shipped = VimMappings::parse_with_defaults("").expect("defaults");
        for (name, bytes) in [
            ("latin1", b"\" caf\xe9\nnnoremap Q dd\n".to_vec()),
            ("large", vec![b'"'; MAX_CONFIG_BYTES as usize + 1]),
        ] {
            let path = directory.path().join(name);
            fs::write(&path, bytes).expect("vimrc");
            let mut config = VimConfig::default();
            config.configure(true, path.to_str(), Instant::now());
            assert!(config.error().is_some(), "{name}: the error is reported");
            assert_eq!(
                *config.mappings(),
                shipped,
                "{name}: the shipped layer stays"
            );
        }
    }

    /// A file that failed to read is read again, so fixing its permissions
    /// loads it without an edit.
    #[cfg(unix)]
    #[test]
    fn a_vimrc_made_readable_again_loads_without_an_edit() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary folder");
        let path = directory.path().join("vimrc");
        fs::write(&path, "nnoremap Q dd\n").expect("vimrc");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("lock the file");
        if fs::read(&path).is_ok() {
            // A superuser reads the file anyway, so there is nothing to fix.
            return;
        }
        let mut config = VimConfig::default();
        let now = Instant::now();
        config.configure(true, path.to_str(), now);
        assert!(config.error().is_some(), "the locked file is an error");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("unlock the file");
        config.poll(now + POLL_INTERVAL);
        config.poll(now + POLL_INTERVAL * 2);
        assert_eq!(config.error(), None, "the file is read again");
        assert_eq!(
            *config.mappings(),
            VimMappings::parse_with_defaults("nnoremap Q dd\n").expect("mappings")
        );
    }

    #[test]
    fn external_edits_reload_and_removal_returns_to_builtins() {
        let directory = tempfile::tempdir().expect("temporary folder");
        let path = directory.path().join("vimrc");
        create_starter(&path).expect("starter");
        assert_eq!(
            VimMappings::parse(&fs::read_to_string(&path).expect("starter file"))
                .expect("valid starter"),
            VimMappings::default()
        );
        let first = "inoremap jk <Esc>\n";
        fs::write(&path, first).expect("custom mappings");
        create_starter(&path).expect("existing file is kept");
        assert_eq!(fs::read_to_string(&path).expect("kept file"), first);
        let mut config = VimConfig::default();
        let now = Instant::now();
        assert!(config.configure(true, path.to_str(), now));
        let loaded = config.mappings();
        assert_eq!(
            loaded.for_mode(VimMappingMode::Insert).len(),
            1,
            "the file loaded"
        );
        fs::write(&path, "nnoremap Q dd\n").expect("second save");
        assert!(!config.poll(now + POLL_INTERVAL));
        assert!(config.poll(now + POLL_INTERVAL * 2));
        assert_ne!(config.mappings(), loaded);
        assert_eq!(
            *config.mappings(),
            VimMappings::parse_with_defaults("nnoremap Q dd\n").expect("mappings"),
            "the user's Q replaces the shipped Q"
        );
        config.configure(false, path.to_str(), now + POLL_INTERVAL * 3);
        assert!(config.configure(true, path.to_str(), now + POLL_INTERVAL * 4));
        fs::remove_file(&path).expect("remove custom file");
        assert!(!config.poll(now + POLL_INTERVAL * 5));
        assert!(config.poll(now + POLL_INTERVAL * 6));
        assert_eq!(
            *config.mappings(),
            VimMappings::parse_with_defaults("").expect("defaults")
        );
        assert!(!path.exists());
    }
}
