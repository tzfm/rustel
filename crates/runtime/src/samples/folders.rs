//! Local sample discovery and bounded folder scans.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustel_audio::SAMPLE_BANK_CAPACITY;

use super::{
    Bank, MAX_SAMPLE_SCAN_WORK_BYTES, MAX_SCORE_SAMPLE_MAP_BYTES, MAX_SCORE_SAMPLE_SCAN_ENTRIES,
    SampleAudioKind, SampleFolderScanError, SampleScanLimits, local_names,
};
use crate::product;

/// Bank name → file paths relative to `root`, in filename order.
///
/// `@strudel/sampler`'s convention, which both the engine and `serve-samples`
/// read through this one function so they cannot disagree about what a folder
/// holds: a sound's name is its parent folder, `wav`/`mp3`/`ogg` are audio,
/// and hidden entries or targets are skipped. A file symlink is advertised
/// only when its final target is a regular supported audio file beneath the
/// scanned root.
pub fn scan_sample_folder(root: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("local samples: cannot read {}: {error}", root.display()))?;
    scan_sample_folder_with_policy(&root, SampleScanLimits::DEFAULT, true, &[])
        .map_err(|error| error.to_string())
}

/// A set folder's audio as banks, under the set's own naming rule.
///
/// Most sets hold no audio, so an empty scan answers with no banks, not an
/// error. The scan skips what the studio writes into a set: takes and tapes
/// in `sessions/`, bounces in `exports/`. They are not instruments, and many
/// of them would use up the scan budget and fail the set's real samples.
pub(super) fn set_folder_banks(root: &Path) -> Result<Vec<(String, Bank)>, String> {
    folder_banks(root, &product::SET_OUTPUT_DIRECTORY_NAMES)
}

/// A folder of audio as banks. Audio directly inside the selected folder is
/// one bank named after that folder; audio in child folders is grouped under
/// each child's name. Thus selecting `samples/bank1` yields `bank1:0`,
/// `bank1:1`, while selecting `samples` yields the child banks `bank1`,
/// `bank2`, and so on.
pub(super) fn folder_banks(root: &Path, skip: &[&str]) -> Result<Vec<(String, Bank)>, String> {
    let scanned = match scan_sample_folder_with_policy(root, SampleScanLimits::DEFAULT, true, skip)
    {
        Ok(scanned) => scanned,
        Err(SampleFolderScanError::Empty(_)) => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut banks: BTreeMap<String, Vec<Arc<str>>> = BTreeMap::new();
    // One order whatever the set is called. The scan files a loose root
    // file under the root's own name, so walking its groups would let the
    // name of the set's folder decide which of two banks sharing a name
    // comes first - and `n(0)` is what `s("name")` plays.
    let mut relatives: Vec<&String> = scanned.values().flatten().collect();
    relatives.sort();
    for relative in relatives {
        let path = Path::new(relative);
        // Direct files belong to the selected folder's bank. A file in a
        // child folder belongs to that child, so selecting the parent exposes
        // each child as a bank without turning every loose recording into a
        // separate bank.
        let loose = path
            .parent()
            .is_none_or(|parent| parent.as_os_str().is_empty());
        let name = if loose {
            root.file_name().and_then(|name| name.to_str())
        } else {
            path.parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
        };
        let Some(name) = name else {
            continue;
        };
        // The raw spelling `fetch_located` opens. These banks get no
        // `score_sources` grant, so nothing between here and `File::open`
        // would undo a percent-encoding - `deep bass.wav` has to arrive as
        // itself, the way `register_local_folder` spells it.
        banks
            .entry(name.to_owned())
            .or_default()
            .push(Arc::from(local_file_url(&root.join(relative)).as_str()));
    }
    Ok(banks
        .into_iter()
        .map(|(name, mut urls)| {
            local_names::order_urls(&mut urls);
            (name, Bank::Array(urls))
        })
        .collect())
}

/// The `file://` URL a local sample is registered under: the raw path, not
/// percent-encoded, so `fetch_located` opens it as itself.
///
/// Without the Windows verbatim prefix. Every folder walk canonicalises its
/// root first, and on Windows `canonicalize` answers in the `\\?\C:\…` form.
/// Spelled into a URL, that `?` reads as the start of a query string to
/// anything that parses one: `codec_for` cut the path off at `file://\\`,
/// found no extension, and sent every imported `.mp3` and `.ogg` to the WAV
/// decoder, which refused them as "not a RIFF/WAVE file". `File::open`
/// takes either spelling, so the plain one is the one that travels.
pub(super) fn local_file_url(path: &Path) -> String {
    format!(
        "file://{}",
        without_verbatim_prefix(&path.display().to_string())
    )
}

/// `\\?\C:\x` is `C:\x` and `\\?\UNC\server\share` is `\\server\share`;
/// anything else is itself.
pub fn without_verbatim_prefix(path: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        std::borrow::Cow::Owned(format!(r"\\{rest}"))
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        std::borrow::Cow::Borrowed(rest)
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}

pub(super) fn scan_score_sample_folder(
    root: &Path,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    scan_sample_folder_with_policy(
        root,
        SampleScanLimits {
            examined_entries: MAX_SCORE_SAMPLE_SCAN_ENTRIES,
            manifest_entries: SAMPLE_BANK_CAPACITY - 1,
            manifest_bytes: MAX_SCORE_SAMPLE_MAP_BYTES,
            working_bytes: MAX_SAMPLE_SCAN_WORK_BYTES,
        },
        false,
        &[],
    )
    .map_err(|error| error.to_string())
}

/// Whether a path is audio this studio can decode - the same test the
/// folder scan makes, for a host that has to decide what a dropped file is.
pub fn is_sample_audio(path: &Path) -> bool {
    sample_audio_kind(path).is_some()
}

pub(crate) fn sample_audio_kind(path: &Path) -> Option<SampleAudioKind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "wav" => Some(SampleAudioKind::Wav),
        "mp3" => Some(SampleAudioKind::Mp3),
        "ogg" => Some(SampleAudioKind::Ogg),
        _ => None,
    }
}

// Retained strings need collection nodes, spare capacity, and allocator
// bookkeeping in addition to their visible bytes. Keep the estimate coarse
// and conservative; stack backing storage is charged separately.
const SAMPLE_SCAN_ENTRY_OVERHEAD_BYTES: usize = 256;

pub(crate) fn scan_sample_folder_with_limits(
    root: &Path,
    limits: SampleScanLimits,
) -> Result<BTreeMap<String, Vec<String>>, SampleFolderScanError> {
    scan_sample_folder_with_policy(root, limits, true, &[])
}

pub(super) struct PendingSampleDirectory {
    path: PathBuf,
    charged_bytes: usize,
}

pub(super) fn sample_scan_path_bytes(path: &Path) -> Option<usize> {
    path.as_os_str()
        .len()
        .checked_add(SAMPLE_SCAN_ENTRY_OVERHEAD_BYTES)
}

pub(super) fn sample_scan_entry_bytes(bank: &str, relative: &str) -> Option<usize> {
    bank.len()
        .checked_add(relative.len())
        .and_then(|bytes| bytes.checked_add(SAMPLE_SCAN_ENTRY_OVERHEAD_BYTES))
}

pub(super) fn sample_scan_stack_slots(examined_entries: usize) -> Option<usize> {
    examined_entries
        .checked_add(1)?
        .checked_mul(2)
        .map(|slots| slots.max(4))
}

fn charge_sample_scan_work(
    used: &mut usize,
    bytes: Option<usize>,
    limit: usize,
) -> Result<usize, SampleFolderScanError> {
    let Some(bytes) = bytes else {
        return Err(SampleFolderScanError::Limit {
            resource: "scanner working-set bytes",
            limit,
        });
    };
    let Some(next) = used.checked_add(bytes) else {
        return Err(SampleFolderScanError::Limit {
            resource: "scanner working-set bytes",
            limit,
        });
    };
    if next > limit {
        return Err(SampleFolderScanError::Limit {
            resource: "scanner working-set bytes",
            limit,
        });
    }
    *used = next;
    Ok(bytes)
}

/// `skip_at_root` names folders directly inside `root` that the walk does
/// not enter at all. A set keeps its takes and its tapes in one of those,
/// and a set with a season of recordings in it would otherwise spend the
/// whole entry budget on them and fail the scan for the samples beside.
fn scan_sample_folder_with_policy(
    root: &Path,
    limits: SampleScanLimits,
    allow_file_symlinks: bool,
    skip_at_root: &[&str],
) -> Result<BTreeMap<String, Vec<String>>, SampleFolderScanError> {
    if limits.manifest_bytes < 2 {
        return Err(SampleFolderScanError::Limit {
            resource: "serialized manifest bytes",
            limit: limits.manifest_bytes,
        });
    }
    let mut examined = 0usize;
    let mut files = 0usize;
    // The wire form is a JSON object. Charge its braces immediately, then
    // charge every key and path before retaining either string.
    let mut manifest_bytes = 2usize;

    let mut banks: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut working_bytes = 0usize;
    let Some(stack_slots) = sample_scan_stack_slots(limits.examined_entries) else {
        return Err(SampleFolderScanError::Limit {
            resource: "scanner working-set bytes",
            limit: limits.working_bytes,
        });
    };
    charge_sample_scan_work(
        &mut working_bytes,
        stack_slots.checked_mul(std::mem::size_of::<PendingSampleDirectory>()),
        limits.working_bytes,
    )?;
    let root_bytes = charge_sample_scan_work(
        &mut working_bytes,
        sample_scan_path_bytes(root),
        limits.working_bytes,
    )?;
    let mut stack = Vec::new();
    stack.try_reserve(1).map_err(|_| {
        SampleFolderScanError::Io(
            "not enough host memory to begin the local sample scan".to_owned(),
        )
    })?;
    if stack.capacity() > stack_slots {
        return Err(SampleFolderScanError::Limit {
            resource: "scanner working-set bytes",
            limit: limits.working_bytes,
        });
    }
    stack.push(PendingSampleDirectory {
        path: root.to_path_buf(),
        charged_bytes: root_bytes,
    });
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir.path).map_err(|error| {
            SampleFolderScanError::Io(format!(
                "local samples: cannot read {}: {error}",
                dir.path.display()
            ))
        })?;
        for entry in entries {
            if examined == limits.examined_entries {
                return Err(SampleFolderScanError::Limit {
                    resource: "directory entries examined",
                    limit: limits.examined_entries,
                });
            }
            examined += 1;
            let entry = entry.map_err(|error| {
                SampleFolderScanError::Io(format!(
                    "local samples: cannot read an entry under {}: {error}",
                    dir.path.display()
                ))
            })?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            // `file_type` describes the entry rather than what it points
            // at, so a directory symlink is deliberately never traversed.
            let kind = entry.file_type().map_err(|error| {
                SampleFolderScanError::Io(format!(
                    "local samples: cannot inspect {}: {error}",
                    path.display()
                ))
            })?;
            if kind.is_dir() {
                if dir.path == root
                    && skip_at_root
                        .iter()
                        .any(|name| entry.file_name().as_os_str() == *name)
                {
                    continue;
                }
                let charged_bytes = charge_sample_scan_work(
                    &mut working_bytes,
                    sample_scan_path_bytes(&path),
                    limits.working_bytes,
                )?;
                stack.try_reserve(1).map_err(|_| {
                    SampleFolderScanError::Io(
                        "not enough host memory to continue the local sample scan".to_owned(),
                    )
                })?;
                if stack.capacity() > stack_slots {
                    return Err(SampleFolderScanError::Limit {
                        resource: "scanner working-set bytes",
                        limit: limits.working_bytes,
                    });
                }
                stack.push(PendingSampleDirectory {
                    path,
                    charged_bytes,
                });
                continue;
            }
            if kind.is_symlink() && !allow_file_symlinks {
                continue;
            }
            // Only a symlink can resolve to a different name. Any other
            // entry without an audio extension is not a sample, so its path
            // is not resolved. A set folder is scanned at each update, and
            // this keeps a folder of many other files cheap.
            if !kind.is_symlink() && sample_audio_kind(&path).is_none() {
                continue;
            }
            let Some(final_path) = path
                .canonicalize()
                .ok()
                .filter(|path| path.starts_with(root))
            else {
                continue;
            };
            let final_relative = final_path.strip_prefix(root).unwrap_or(&final_path);
            if final_relative.components().any(|component| {
                !matches!(component, std::path::Component::Normal(_))
                    || component.as_os_str().to_string_lossy().starts_with('.')
            }) {
                continue;
            }
            if sample_audio_kind(&final_path).is_none()
                || !final_path
                    .metadata()
                    .map(|metadata| metadata.is_file())
                    .unwrap_or(false)
            {
                continue;
            }
            if files == limits.manifest_entries {
                return Err(SampleFolderScanError::Limit {
                    resource: "audio-file count",
                    limit: limits.manifest_entries,
                });
            }
            // The bank is the parent folder's name - and for a file sitting
            // directly in the root, the root's own name, which is what
            // `path.split('/').slice(-2)[0]` yields there too.
            let bank = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("local");
            let Some(relative) = path.strip_prefix(root).unwrap_or(&path).to_str() else {
                // A lossy path cannot be requested back through a URL: the
                // replacement character would name a different file.
                continue;
            };
            let paths_in_bank = banks.get(bank).map_or(0, Vec::len);
            let bank_overhead = if paths_in_bank == 0 {
                usize::from(!banks.is_empty()) + json_string_bytes(bank) + 3
            } else {
                0
            };
            let path_overhead = usize::from(paths_in_bank != 0) + json_path_string_bytes(relative);
            let Some(next_bytes) = manifest_bytes
                .checked_add(bank_overhead)
                .and_then(|bytes| bytes.checked_add(path_overhead))
            else {
                return Err(SampleFolderScanError::Limit {
                    resource: "serialized manifest bytes",
                    limit: limits.manifest_bytes,
                });
            };
            if next_bytes > limits.manifest_bytes {
                return Err(SampleFolderScanError::Limit {
                    resource: "serialized manifest bytes",
                    limit: limits.manifest_bytes,
                });
            }

            // Charge the owned key/path and their collection storage before
            // either string is retained in the result map.
            charge_sample_scan_work(
                &mut working_bytes,
                sample_scan_entry_bytes(bank, relative),
                limits.working_bytes,
            )?;

            files += 1;
            manifest_bytes = next_bytes;
            banks
                .entry(bank.to_owned())
                .or_default()
                .push(relative.replace('\\', "/"));
        }
        working_bytes = working_bytes
            .checked_sub(dir.charged_bytes)
            .ok_or_else(|| {
                SampleFolderScanError::Io("local sample scan accounting failed".to_owned())
            })?;
    }
    if banks.is_empty() {
        return Err(SampleFolderScanError::Empty(format!(
            "local samples: no .wav/.mp3/.ogg files under {}",
            root.display()
        )));
    }
    // Filename order, preserving the original position of in-app renames.
    for paths in banks.values_mut() {
        local_names::order_paths(root, paths);
    }
    Ok(banks)
}

fn json_path_string_bytes(value: &str) -> usize {
    2 + json_escaped_char_bytes('/')
        + value
            .chars()
            .map(|character| {
                json_escaped_char_bytes(if character == '\\' { '/' } else { character })
            })
            .sum::<usize>()
}

fn json_string_bytes(value: &str) -> usize {
    2 + value.chars().map(json_escaped_char_bytes).sum::<usize>()
}

fn json_escaped_char_bytes(character: char) -> usize {
    match character {
        '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
        '\u{0000}'..='\u{001f}' => 6,
        _ => character.len_utf8(),
    }
}
