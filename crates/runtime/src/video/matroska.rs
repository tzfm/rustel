//! The codec of each track in a Matroska or WebM file.
//!
//! Symphonia keeps a track's codec ID and codec private data only for
//! sound. A picture needs both: the ID names the codec, and the private
//! data holds the setup its decoder starts from, such as `avcC` or `hvcC`.
//! The walk reads elements from the bytes in memory and descends only the
//! path to the track entries. An element that claims more bytes than its
//! parent holds ends the walk at that level.

use std::collections::HashMap;

use super::codec::{Codec, Entry, Named};

const SEGMENT: u32 = 0x1853_8067;
const TRACKS: u32 = 0x1654_AE6B;
const CLUSTER: u32 = 0x1F43_B675;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;

/// Each track's codec, by track number. Symphonia numbers Matroska tracks
/// the same way.
pub(super) fn entries(bytes: &[u8]) -> HashMap<u32, Entry> {
    let mut entries = HashMap::new();
    let Some((_, segment)) = elements(bytes).find(|(id, _)| *id == SEGMENT) else {
        return entries;
    };
    // The tracks come before the first cluster. A cluster may not say how
    // long it is, so the walk stops there.
    let tracks = elements(segment)
        .take_while(|(id, _)| *id != CLUSTER)
        .find(|(id, _)| *id == TRACKS);
    for (_, track) in tracks
        .into_iter()
        .flat_map(|(_, tracks)| elements(tracks))
        .filter(|(id, _)| *id == TRACK_ENTRY)
    {
        let mut number = None;
        let mut codec_id = None;
        let mut config = None;
        for (id, payload) in elements(track) {
            match id {
                TRACK_NUMBER if payload.len() <= 4 => {
                    number = Some(
                        payload
                            .iter()
                            .fold(0u32, |number, &byte| number << 8 | u32::from(byte)),
                    );
                }
                CODEC_ID => codec_id = Some(payload),
                CODEC_PRIVATE => config = Some(payload.into()),
                _ => {}
            }
        }
        if let (Some(number), Some(codec_id)) = (number, codec_id) {
            let codec_id = String::from_utf8_lossy(codec_id);
            let named = named(codec_id.trim_end_matches('\0'));
            entries.insert(number, Entry { named, config });
        }
    }
    entries
}

/// What a codec ID names. Sound, subtitles and attachments are not
/// pictures.
fn named(codec_id: &str) -> Named {
    let unsupported = |name: &str| Named::Unsupported(name.to_owned());
    match codec_id {
        "V_MPEG4/ISO/AVC" => Named::Picture(Codec::H264),
        "V_MPEGH/ISO/HEVC" => Named::Picture(Codec::Hevc),
        "V_AV1" => Named::Picture(Codec::Av1),
        "V_VP9" => Named::Picture(Codec::Vp9),
        "V_VP8" => Named::Picture(Codec::Vp8),
        "V_MJPEG" => Named::Picture(Codec::MotionJpeg),
        "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/AP" => unsupported("MPEG-4 Part 2"),
        "V_MPEG1" => unsupported("MPEG-1"),
        "V_MPEG2" => unsupported("MPEG-2"),
        "V_THEORA" => unsupported("Theora"),
        "V_PRORES" => unsupported("ProRes"),
        "V_MPEGI/ISO/VVC" => unsupported("H.266"),
        other if other.starts_with("V_") => unsupported(&format!("'{other}'")),
        _ => Named::NotPicture,
    }
}

/// The elements laid end to end in `data`, as their IDs and payloads.
fn elements(data: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let Some((id, size, header)) = element_header(rest) else {
            rest = &[];
            return None;
        };
        let body = &rest[header..];
        // An unknown size runs to the end of the parent.
        let size = size.unwrap_or(body.len());
        if size > body.len() {
            rest = &[];
            return None;
        }
        let payload = &body[..size];
        rest = &body[size..];
        Some((id, payload))
    })
}

/// An element's ID, its payload size if it gives one, and the header's
/// length. IDs keep their length marker, as the specification writes them.
fn element_header(data: &[u8]) -> Option<(u32, Option<usize>, usize)> {
    let &first = data.first()?;
    let id_length = first.leading_zeros() as usize + 1;
    if id_length > 4 {
        return None;
    }
    let id = data
        .get(..id_length)?
        .iter()
        .fold(0u32, |id, &byte| id << 8 | u32::from(byte));
    let &size_first = data.get(id_length)?;
    let size_length = size_first.leading_zeros() as usize + 1;
    if size_length > 8 {
        return None;
    }
    let size_bytes = data.get(id_length..id_length + size_length)?;
    let marker = u8::MAX.checked_shr(size_length as u32).unwrap_or(0);
    let mut size = u64::from(size_first & marker);
    let mut unknown = size_first & marker == marker;
    for &byte in &size_bytes[1..] {
        size = size << 8 | u64::from(byte);
        unknown &= byte == 0xFF;
    }
    let size = if unknown {
        None
    } else {
        Some(usize::try_from(size).ok()?)
    };
    Some((id, size, id_length + size_length))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One element with a one-byte size.
    fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x80 | u8::try_from(payload.len()).expect("a small element"));
        out.extend_from_slice(payload);
        out
    }

    fn track(number: u8, codec_id: &str, private: Option<&[u8]>) -> Vec<u8> {
        let mut entry = element(&[0xD7], &[number]);
        entry.extend(element(&[0x86], codec_id.as_bytes()));
        if let Some(private) = private {
            entry.extend(element(&[0x63, 0xA2], private));
        }
        element(&[0xAE], &entry)
    }

    #[test]
    fn each_track_names_its_codec_by_number() {
        let mut tracks = track(1, "S_TEXT/UTF8", None);
        tracks.extend(track(2, "V_MPEG4/ISO/AVC", Some(&[1, 100, 0, 31])));
        tracks.extend(track(3, "A_OPUS", None));
        let mut segment = element(&[0x15, 0x49, 0xA9, 0x66], &[]);
        segment.extend(element(&[0x16, 0x54, 0xAE, 0x6B], &tracks));
        // A segment of unknown size, as a live recording writes it.
        let mut file = element(&[0x1A, 0x45, 0xDF, 0xA3], &[]);
        file.extend_from_slice(&[
            0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        file.extend(segment);
        let entries = entries(&file);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[&1].named, Named::NotPicture);
        assert_eq!(entries[&2].named, Named::Picture(Codec::H264));
        assert_eq!(entries[&2].config.as_deref(), Some(&[1, 100, 0, 31][..]));
        assert_eq!(entries[&3].named, Named::NotPicture);
    }

    #[test]
    fn codec_ids_name_what_this_build_cannot_decode() {
        assert_eq!(named("V_AV1"), Named::Picture(Codec::Av1));
        assert_eq!(named("V_PRORES"), Named::Unsupported("ProRes".to_owned()));
        assert_eq!(
            named("V_QUICKTIME"),
            Named::Unsupported("'V_QUICKTIME'".to_owned())
        );
        assert_eq!(named("A_AAC"), Named::NotPicture);
    }

    #[test]
    fn the_real_vp8_fixture_names_its_tracks() {
        let entries = entries(include_bytes!("testdata/tiny_video_subs.mkv"));
        let named: Vec<&Named> = {
            let mut numbers: Vec<_> = entries.keys().copied().collect();
            numbers.sort_unstable();
            numbers
                .iter()
                .map(|number| &entries[number].named)
                .collect()
        };
        assert_eq!(
            named,
            [
                &Named::NotPicture,
                &Named::Picture(Codec::Vp8),
                &Named::NotPicture
            ],
            "subtitles, the picture, then sound"
        );
    }

    /// An element that claims more than its parent holds ends the walk,
    /// so a lying size costs no allocation and no read past the file.
    #[test]
    fn a_lying_element_size_ends_the_walk() {
        let mut file = element(&[0x18, 0x53, 0x80, 0x67], &track(1, "V_VP8", None));
        file[4] = 0xFE;
        assert!(entries(&file).is_empty());
        assert!(entries(&[0x18, 0x53]).is_empty());
        assert!(
            entries(&[0x00, 0x00, 0x00]).is_empty(),
            "no ID starts with a zero byte"
        );
    }
}
