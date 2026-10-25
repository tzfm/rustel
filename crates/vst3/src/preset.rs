//! The `.vstpreset` file: a header, the state chunks, and a chunk list.
//!
//! ```text
//!   "VST3"  version:i32  class id:32 chars  list offset:i64
//!   ... chunk data ...
//!   "List"  count:i32  then for each chunk:  id:4 chars  offset:i64  size:i64
//! ```
//!
//! All numbers are little-endian. Chunk "Comp" is the processor state and
//! chunk "Cont" is the controller state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(crate) struct Preset<'a> {
    /// The class id of the plugin the preset is for, as 32 hex characters.
    pub class: &'a str,
    pub component: &'a [u8],
    pub controller: Option<&'a [u8]>,
}

pub(crate) fn parse(file: &[u8]) -> Result<Preset<'_>, String> {
    let bad = || "the file is not a VST3 preset".to_string();
    let bytes = |at: usize, len: usize| file.get(at..at.checked_add(len)?);
    let number = |at: usize| -> Option<usize> {
        let raw = i64::from_le_bytes(bytes(at, 8)?.try_into().ok()?);
        usize::try_from(raw).ok()
    };
    if bytes(0, 4) != Some(b"VST3") {
        return Err(bad());
    }
    let class = std::str::from_utf8(bytes(8, 32).ok_or_else(bad)?).map_err(|_| bad())?;
    let list = number(40).ok_or_else(bad)?;
    if bytes(list, 4) != Some(b"List") {
        return Err(bad());
    }
    let count = i32::from_le_bytes(bytes(list + 4, 4).ok_or_else(bad)?.try_into().unwrap());
    let mut component = None;
    let mut controller = None;
    for index in 0..usize::try_from(count).map_err(|_| bad())? {
        let entry = list + 8 + index * 20;
        let id = bytes(entry, 4).ok_or_else(bad)?;
        let offset = number(entry + 4).ok_or_else(bad)?;
        let size = number(entry + 12).ok_or_else(bad)?;
        let chunk = bytes(offset, size).ok_or_else(bad)?;
        match id {
            b"Comp" => component = Some(chunk),
            b"Cont" => controller = Some(chunk),
            _ => {}
        }
    }
    Ok(Preset {
        class,
        component: component.ok_or("the preset has no plugin state")?,
        controller,
    })
}

/// The class id of the plugin the preset is for, from the header alone:
/// 32 hex characters. `None` when the bytes are no VST3 preset header. The
/// header is 40 bytes, so a scan of a folder reads no whole preset file.
pub(crate) fn class_of(file: &[u8]) -> Option<&str> {
    let class = std::str::from_utf8(file.get(8..40)?).ok()?;
    let hex = class.chars().all(|char| char.is_ascii_hexdigit());
    (hex && file.get(0..4) == Some(b"VST3")).then_some(class)
}

/// The `.vstpreset` files under `folders`, by the lower case class id each
/// file names. Any folder structure counts: the header says the plugin, not
/// the folder. A file a folder before holds wins over a file of a later
/// folder with the same preset name. Each list is sorted by name.
pub(crate) fn scan_folders(folders: &[PathBuf]) -> HashMap<String, Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for folder in folders {
        walk(folder, &mut files);
    }
    let mut index: HashMap<String, Vec<(String, PathBuf)>> = HashMap::new();
    for path in files {
        let mut header = [0; 40];
        let read = std::fs::File::open(&path)
            .and_then(|mut file| std::io::Read::read(&mut file, &mut header));
        let Some(class) = read.ok().and_then(|_| class_of(&header)) else {
            continue;
        };
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let files = index.entry(class.to_ascii_lowercase()).or_default();
        let known = files.iter().any(|(known, _)| *known == name);
        if !known {
            files.push((name.into_owned(), path));
        }
    }
    for files in index.values_mut() {
        files.sort();
    }
    index
}

/// The `.vstpreset` files of one folder: name and path, sorted by name.
/// The read of the preset root reads the folder of each plugin this way -
/// the folder names the plugin - and the load of a plugin reads its own
/// folder this way: see [`Host::finish_presets`] and
/// [`Host::refresh_preset_folder`].
pub(crate) fn read_folder(folder: &Path) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vstpreset"))
        })
        .filter_map(|path| Some((path.file_stem()?.to_string_lossy().into_owned(), path)))
        .collect();
    files.sort();
    files
}

/// Takes each `.vstpreset` file under `folder` in, from every folder below.
/// A link to a folder does not count, so a cycle ends the walk.
fn walk(folder: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            walk(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vstpreset"))
        {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_gives_its_class_and_state_and_a_cut_file_is_refused() {
        let file = rustel_vst3_fixture::preset(0.25, 1.0);
        let preset = parse(&file).expect("preset");
        assert_eq!(preset.class.len(), 32);
        assert_eq!(preset.component.len(), 16);
        assert_eq!(preset.component[..8], 0.25f64.to_le_bytes());
        assert!(preset.controller.is_none());

        for cut in 0..file.len() {
            assert!(parse(&file[..cut]).is_err(), "cut at {cut}");
        }
        let mut far = file.clone();
        far[40..48].copy_from_slice(&i64::MAX.to_le_bytes());
        assert!(parse(&far).is_err());
    }

    #[test]
    fn a_scan_of_folders_takes_each_preset_by_the_class_of_its_header() {
        let file = rustel_vst3_fixture::preset(0.25, 1.0);
        let class = class_of(&file).expect("class").to_ascii_lowercase();
        let tag = format!("rustel-preset-scan-{}", std::process::id());
        let root = std::env::temp_dir().join(&tag);
        let other = std::env::temp_dir().join(format!("{tag}-other"));
        for folder in [&root, &other] {
            let _ = std::fs::remove_dir_all(folder);
        }
        // Any structure counts: a file at the root, one in a folder of the
        // vendor, one with the wrong extension, one with no header.
        std::fs::create_dir_all(root.join("Vendor/Synth/Banks")).unwrap();
        std::fs::write(root.join("lead.vstpreset"), &file).unwrap();
        let bank = root.join("Vendor/Synth/Banks");
        std::fs::write(bank.join("pad.vstpreset"), &file).unwrap();
        std::fs::write(bank.join("cut.vstpreset"), &file[..20]).unwrap();
        std::fs::write(bank.join("notes.txt"), "no preset").unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("lead.vstpreset"), &file).unwrap();

        let folders = vec![root.clone(), other.clone()];
        let index = scan_folders(&folders);
        let files = index.get(&class).expect("the class of the fixture");
        assert_eq!(
            files
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["lead", "pad"],
            "a file of a folder before wins over a same-named file of a later one"
        );
        // The paths keep the place each file came from.
        assert!(files[0].1.starts_with(&root));
        assert!(files[1].1.starts_with(&bank));

        for folder in [root, other] {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}
