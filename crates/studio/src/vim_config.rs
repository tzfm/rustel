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
        let mut maps = ModeMaps::default();
        if defaults {
            // gd is Vim's go-to-definition. Docs answers here while Vim
            // owns Ctrl+D. The shipped defaults may replace it, and a vimrc
            // line replaces both.
            maps.normal.push(("gd".into(), "<Action:docs>".into()));
            for (number, line) in DEFAULT_VIMRC.lines().enumerate() {
                parse_map_line(line, &mut maps)
                    .map_err(|error| format!("default vimrc line {}: {error}", number + 1))?;
            }
        }
        for (number, line) in text.lines().enumerate() {
            parse_map_line(line, &mut maps)
                .map_err(|error| format!("line {}: {error}", number + 1))?;
        }
        Ok(Self {
            normal: parse_mode("normal", maps.normal)?,
            insert: parse_mode("insert", maps.insert)?,
            visual: parse_mode("visual", maps.visual)?,
            operator_pending: parse_mode("operator_pending", maps.operator_pending)?,
        })
    }
}

#[derive(Default)]
struct ModeMaps {
    normal: Vec<(String, String)>,
    insert: Vec<(String, String)>,
    visual: Vec<(String, String)>,
    operator_pending: Vec<(String, String)>,
}

impl ModeMaps {
    fn for_mode(&mut self, mode: VimMappingMode) -> &mut Vec<(String, String)> {
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
fn parse_map_line(line: &str, maps: &mut ModeMaps) -> Result<(), String> {
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
    let mut rest = after;
    let (lhs, after) = loop {
        let Some((word, after)) = first_word(rest) else {
            return Ok(());
        };
        if is_map_option(word) {
            rest = after;
            continue;
        }
        break (word, after);
    };
    // Keep runs of spaces inside the replacement. The first run after the
    // left side only separates the two.
    let rhs = after.trim_start();
    if rhs.is_empty() {
        return Ok(());
    }
    let lower = rhs.to_ascii_lowercase();
    let plugin_command = ["<cmd>", "<plug>", "<sid>"]
        .iter()
        .any(|notation| lower.contains(notation));
    let insert = modes.contains(&VimMappingMode::Insert);
    // Studio does not run Ex commands or plugin calls. A Normal-mode RHS
    // that starts with `:` is a command, not typed text.
    if plugin_command || (rhs.starts_with(':') && !insert) {
        return Ok(());
    }
    let Ok(keys) = parse_keys(lhs) else {
        return Ok(());
    };
    if keys.is_empty() {
        return Ok(());
    }
    if parse_replacement(rhs).is_err() {
        return Ok(());
    }
    for mode in modes {
        let map = maps.for_mode(*mode);
        if let Some(existing) = map.iter_mut().find(|(key, _)| key == lhs) {
            existing.1 = rhs.to_owned();
        } else {
            if map.len() >= 256 {
                return Err(format!("{mode:?} has more than 256 mappings"));
            }
            map.push((lhs.to_owned(), rhs.to_owned()));
        }
    }
    Ok(())
}

fn is_map_option(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "<silent>" | "<nowait>" | "<buffer>" | "<unique>" | "<special>" | "<script>"
    )
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

fn parse_mode(mode: &str, entries: Vec<(String, String)>) -> Result<Vec<VimMapping>, String> {
    if entries.len() > 256 {
        return Err(format!("{mode} has more than 256 mappings"));
    }
    let mut mappings: Vec<VimMapping> = Vec::with_capacity(entries.len());
    for (keys, replacement) in entries {
        let keys = parse_keys(&keys).map_err(|error| format!("{mode}: {error}"))?;
        if keys.is_empty() {
            return Err(format!("{mode}: a mapping needs an input key"));
        }
        let replacement =
            parse_replacement(&replacement).map_err(|error| format!("{mode}: {error}"))?;
        if let Some(existing) = mappings.iter_mut().find(|mapping| mapping.keys == keys) {
            // The last definition of a key wins, as in Vim.
            existing.replacement = replacement;
        } else {
            mappings.push(VimMapping { keys, replacement });
        }
    }
    Ok(mappings)
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
        let stamp = file_stamp(path);
        if self.stamp == Some(stamp) {
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
                let fallback = if self.has_valid {
                    "previous bindings kept"
                } else {
                    "built-in bindings active"
                };
                let message = format!("Vim mappings: {error} · {fallback}");
                self.error = Some(message.clone());
                self.notice = Some(message);
                false
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
