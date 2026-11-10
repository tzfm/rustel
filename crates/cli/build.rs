//! Writes `build_features.rs`: the sorted Cargo features this build of the
//! package enables, as a Rust slice expression the binary includes. For a
//! Windows MSVC target, it also links the program icon into `rustel.exe`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// The program icon that Explorer and the taskbar show for `rustel.exe`.
const ICON: &str = "assets/rustel.ico";

/// The Win32 resource type of one icon image.
const RT_ICON: u16 = 3;
/// The Win32 resource type of an icon group, the list of its images.
const RT_GROUP_ICON: u16 = 14;

// Cargo reads the lines of a build script on its standard output.
#[allow(clippy::disallowed_macros)]
fn main() {
    // Cargo reruns this script when the package's features change.
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    write_build_features(&out_dir);

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo sets CARGO_CFG_TARGET_OS");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").expect("Cargo sets CARGO_CFG_TARGET_ENV");
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rerun-if-changed={ICON}");
        let icon = fs::read(ICON).expect("read assets/rustel.ico");
        let res = out_dir.join("rustel.res");
        fs::write(&res, icon_resource(&icon)).expect("write rustel.res");
        // The MSVC linker takes a compiled resource file as an input file.
        println!("cargo:rustc-link-arg-bin=rustel={}", res.display());
    }
}

fn write_build_features(out_dir: &Path) {
    // Cargo sets `CARGO_FEATURE_<NAME>` for each enabled feature, upper-cased
    // with `-` turned into `_`. `default` names a set, not a capability.
    let mut features = env::vars_os()
        .filter_map(|(variable, _)| {
            let name = variable.to_str()?.strip_prefix("CARGO_FEATURE_")?;
            Some(name.to_ascii_lowercase().replace('_', "-"))
        })
        .filter(|feature| feature != "default")
        .collect::<Vec<_>>();
    features.sort_unstable();

    fs::write(out_dir.join("build_features.rs"), format!("&{features:?}"))
        .expect("write build_features.rs");
}

/// Encodes an `.ico` file as a compiled resource file with one icon group.
///
/// The `.ico` has a 6-byte header, then one 16-byte entry per image. Each
/// entry ends with the byte size and the byte offset of its image. The
/// resource file holds each image as an `RT_ICON` resource. The
/// `RT_GROUP_ICON` resource holds the same header and entries, but each
/// entry ends with the resource ID of its image in place of the offset.
fn icon_resource(ico: &[u8]) -> Vec<u8> {
    let le_u32 = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);
    let count = u16::from_le_bytes([ico[4], ico[5]]);

    // An empty first resource marks a 32-bit resource file.
    let mut res = Vec::new();
    push_resource(&mut res, 0, 0, &[]);
    let mut group = ico[..6].to_vec();
    for id in 1..=count {
        let entry = 6 + 16 * usize::from(id - 1);
        let size = usize::try_from(le_u32(entry + 8)).expect("an image size fits in usize");
        let offset = usize::try_from(le_u32(entry + 12)).expect("an image offset fits in usize");
        push_resource(&mut res, RT_ICON, id, &ico[offset..offset + size]);
        group.extend_from_slice(&ico[entry..entry + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    push_resource(&mut res, RT_GROUP_ICON, 1, &group);
    res
}

/// Appends one resource with a numeric type and ID: a 32-byte header, the
/// data, and zero bytes up to a 4-byte boundary.
fn push_resource(res: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    let data_size = u32::try_from(data.len()).expect("an icon image is smaller than 4 GiB");
    res.extend_from_slice(&data_size.to_le_bytes());
    res.extend_from_slice(&32_u32.to_le_bytes());
    for word in [0xFFFF, kind, 0xFFFF, id] {
        res.extend_from_slice(&word.to_le_bytes());
    }
    // Data version, memory flags, language (neutral), version and
    // characteristics, all zero.
    res.extend_from_slice(&[0; 16]);
    res.extend_from_slice(data);
    res.resize(res.len().next_multiple_of(4), 0);
}
