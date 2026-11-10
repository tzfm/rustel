//! The codec of each track in an MP4 or QuickTime file.
//!
//! Symphonia reads every track's sample table but keeps the sample entry
//! only for sound. A picture needs it: the entry names the codec and holds
//! the setup its decoder starts from, such as `avcC` or `hvcC`. The walk
//! reads boxes from the bytes in memory and descends only the path to
//! `stsd`. A box that claims more bytes than its parent holds ends the
//! walk at that level.

use std::collections::HashMap;

use super::codec::{Codec, Entry, Named};

/// Bytes a visual sample entry holds before its child boxes.
const VISUAL_ENTRY_FIELDS: usize = 78;

/// Each track's codec, by its place in the file. Symphonia numbers MP4
/// tracks the same way.
pub(super) fn entries(bytes: &[u8]) -> HashMap<u32, Entry> {
    boxes(bytes)
        .find(|(kind, _)| kind == b"moov")
        .into_iter()
        .flat_map(|(_, moov)| boxes(moov))
        .filter(|(kind, _)| kind == b"trak")
        .zip(0u32..)
        .filter_map(|((_, trak), number)| Some((number, entry(trak)?)))
        .collect()
}

fn entry(trak: &[u8]) -> Option<Entry> {
    let mdia = child(trak, b"mdia")?;
    // Version and flags, a predefined word, then the handler type.
    let handler = child(mdia, b"hdlr").and_then(|hdlr| hdlr.get(8..12));
    let stbl = child(child(mdia, b"minf")?, b"stbl")?;
    let stsd = child(stbl, b"stsd")?;
    // Version and flags, then the entry count.
    let (fourcc, sample_entry) = boxes(stsd.get(8..)?).next()?;
    // Sound, text and timecode tracks are not pictures.
    if handler.is_some_and(|handler| handler != b"vide") {
        return Some(Entry {
            named: Named::NotPicture,
            config: None,
        });
    }
    let config = sample_entry
        .get(VISUAL_ENTRY_FIELDS..)
        .and_then(|children| {
            boxes(children).find(|(kind, _)| matches!(kind, b"avcC" | b"hvcC" | b"av1C"))
        })
        .map(|(_, payload)| payload.into());
    Some(Entry {
        named: named(fourcc),
        config,
    })
}

/// What a picture sample entry's code names.
fn named(fourcc: [u8; 4]) -> Named {
    let unsupported = |name: &str| Named::Unsupported(name.to_owned());
    match &fourcc {
        b"avc1" | b"avc3" | b"dva1" | b"dvav" => Named::Picture(Codec::H264),
        b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => Named::Picture(Codec::Hevc),
        b"av01" => Named::Picture(Codec::Av1),
        b"vp09" => Named::Picture(Codec::Vp9),
        b"vp08" => Named::Picture(Codec::Vp8),
        b"jpeg" | b"mjpa" => Named::Picture(Codec::MotionJpeg),
        b"mp4v" => unsupported("MPEG-4 Part 2"),
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => unsupported("ProRes"),
        b"s263" | b"h263" => unsupported("H.263"),
        b"vvc1" | b"vvi1" => unsupported("H.266"),
        other => {
            let code: String = other
                .iter()
                .map(|&byte| {
                    if byte.is_ascii_graphic() || byte == b' ' {
                        char::from(byte)
                    } else {
                        '?'
                    }
                })
                .collect();
            unsupported(&format!("'{}'", code.trim_end()))
        }
    }
}

fn child<'a>(parent: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(parent)
        .find(|(found, _)| found == kind)
        .map(|(_, payload)| payload)
}

/// The boxes laid end to end in `data`, as their codes and payloads.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let size = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
        let kind: [u8; 4] = rest.get(4..8)?.try_into().ok()?;
        let (header, size) = match size {
            // The box runs to the end of its parent.
            0 => (8, rest.len()),
            1 => {
                let large = u64::from_be_bytes(rest.get(8..16)?.try_into().ok()?);
                (16, usize::try_from(large).ok()?)
            }
            size => (8, size as usize),
        };
        if size < header || size > rest.len() {
            rest = &[];
            return None;
        }
        let payload = &rest[header..size];
        rest = &rest[size..];
        Some((kind, payload))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(payload.len() + 8)
            .expect("a small box")
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn track(handler: &[u8; 4], fourcc: &[u8; 4], config: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
        let mut entry = vec![0u8; VISUAL_ENTRY_FIELDS];
        if let Some((kind, payload)) = config {
            entry.extend(boxed(kind, payload));
        }
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(boxed(fourcc, &entry));
        let stbl = boxed(b"stbl", &boxed(b"stsd", &stsd));
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        let mut mdia = boxed(b"hdlr", &hdlr);
        mdia.extend(boxed(b"minf", &stbl));
        boxed(b"trak", &boxed(b"mdia", &mdia))
    }

    #[test]
    fn each_track_names_its_codec_and_setup_in_file_order() {
        let mut moov = track(b"soun", b"mp4a", None);
        moov.extend(track(b"vide", b"avc1", Some((b"avcC", &[1, 100, 0, 31]))));
        moov.extend(track(b"tmcd", b"tmcd", None));
        let mut file = boxed(b"ftyp", b"isom");
        file.extend(boxed(b"mdat", &[0; 16]));
        file.extend(boxed(b"moov", &moov));
        let entries = entries(&file);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[&0].named, Named::NotPicture, "the sound");
        assert_eq!(
            entries[&1],
            Entry {
                named: Named::Picture(Codec::H264),
                config: Some(vec![1, 100, 0, 31].into()),
            }
        );
        assert_eq!(entries[&2].named, Named::NotPicture, "a timecode track");
    }

    #[test]
    fn sample_entry_codes_name_what_this_build_cannot_decode() {
        assert_eq!(named(*b"hvc1"), Named::Picture(Codec::Hevc));
        assert_eq!(named(*b"jpeg"), Named::Picture(Codec::MotionJpeg));
        assert_eq!(named(*b"apch"), Named::Unsupported("ProRes".to_owned()));
        assert_eq!(named(*b"xyz\0"), Named::Unsupported("'xyz?'".to_owned()));
    }

    #[test]
    fn the_real_h264_fixture_carries_its_avcc() {
        let entries = entries(include_bytes!("../samples/testdata/tiny_video.mp4"));
        let video = entries
            .values()
            .find(|entry| entry.named == Named::Picture(Codec::H264))
            .expect("an H.264 track");
        assert_eq!(video.config.as_deref().map(|config| config[0]), Some(1));
    }

    /// A box that claims more than its parent holds ends the walk, so a
    /// lying size costs no allocation and no read past the file.
    #[test]
    fn a_lying_box_size_ends_the_walk() {
        let mut file = boxed(b"moov", &track(b"vide", b"avc1", None));
        file[3] = 0xFF;
        assert!(entries(&file).is_empty());
        let mut huge = vec![0, 0, 0, 1];
        huge.extend_from_slice(b"moov");
        huge.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(entries(&huge).is_empty());
        assert!(entries(&[0, 0]).is_empty());
    }
}
