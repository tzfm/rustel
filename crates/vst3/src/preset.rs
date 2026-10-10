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
}
