//! AV1 through rav1d, the Rust port of dav1d, built without its assembly.
//!
//! rav1d exposes only dav1d's C interface, so the calls here are `unsafe`.
//! This file is the only one that makes them. The rest of the module sees
//! a decoder that hands over borrowed planes.

use std::ptr::NonNull;

use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_unref, dav1d_data_wrap, dav1d_default_settings, dav1d_get_picture,
    dav1d_open, dav1d_picture_unref, dav1d_send_data,
};
use rav1d::src::send_sync_non_null::SendSyncNonNull;

use super::picture::{Colour, Picture, Plane, Samples, Yuv};

/// The result rav1d gives when it needs the caller to act first: take a
/// picture before it accepts more data, or send data before it has one.
const AGAIN: i32 = -libc::EAGAIN;

/// Calls one send or drain may make before it gives up on a decoder that
/// neither takes data nor gives pictures.
const MAX_CALLS: usize = 1024;

pub(super) struct Decoder {
    context: Option<Dav1dContext>,
}

impl Decoder {
    /// A decoder that refuses frames over `pixel_limit` pixels before it
    /// allocates for them. Its `threads` split each frame's tiles and
    /// filters. One frame is in flight at a time, so every picture leaves
    /// with the packet that shows it, as on a single thread.
    pub(super) fn new(pixel_limit: u32, threads: usize) -> Result<Self, String> {
        let mut settings = std::mem::MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: the pointer is to writable memory for one settings
        // struct, which the call fills in whole.
        unsafe { dav1d_default_settings(NonNull::from(&mut settings).cast()) };
        // SAFETY: `dav1d_default_settings` initialised every field.
        let mut settings = unsafe { settings.assume_init() };
        settings.n_threads = i32::try_from(threads).unwrap_or(1);
        settings.max_frame_delay = 1;
        settings.frame_size_limit = pixel_limit;
        // A thumbnail averages film grain away, so synthesising it is waste.
        settings.apply_grain = 0;
        let mut context = None;
        // SAFETY: both pointers are to live locals of the types the call
        // reads and writes.
        let opened = unsafe {
            dav1d_open(
                Some(NonNull::from(&mut context)),
                Some(NonNull::from(&mut settings)),
            )
        };
        if opened.0 != 0 || context.is_none() {
            return Err(format!("AV1 decoder did not open ({})", opened.0));
        }
        Ok(Self { context })
    }

    /// Decode one temporal unit, handing each picture it shows to `each`.
    pub(super) fn decode(
        &mut self,
        packet: &[u8],
        each: &mut dyn FnMut(Picture<'_>),
    ) -> Result<(), String> {
        if packet.is_empty() {
            return Ok(());
        }
        // rav1d reads a copy it owns and frees it through `free_packet`
        // once its last frame is done with it.
        let copy = SendSyncNonNull::from_box(Box::new(Box::<[u8]>::from(packet)));
        // SAFETY: the cookie holds the live copy until `free_packet` runs.
        let start = unsafe { copy.as_ptr().as_ref() }.as_ptr();
        let mut data = Dav1dData::default();
        // SAFETY: `data` is a live default data struct the call fills in,
        // and `start` begins the copy's `packet.len()` bytes, which stay
        // alive until rav1d calls `free_packet` with the cookie.
        let wrapped = unsafe {
            dav1d_data_wrap(
                Some(NonNull::from(&mut data)),
                NonNull::new(start.cast_mut()),
                packet.len(),
                Some(free_packet),
                Some(copy.cast()),
            )
        };
        if wrapped.0 != 0 {
            return Err(format!("AV1 packet did not wrap ({})", wrapped.0));
        }
        let mut result = Ok(());
        for _ in 0..MAX_CALLS {
            // SAFETY: the context is open, and `data` holds the buffer the
            // decoder owns until it reads it all.
            let sent = unsafe { dav1d_send_data(self.context, Some(NonNull::from(&mut data))) };
            if sent.0 != 0 && sent.0 != AGAIN {
                result = Err(format!("AV1 packet did not decode ({})", sent.0));
                break;
            }
            if let Err(message) = self.drain(each) {
                result = Err(message);
                break;
            }
            if data.sz == 0 {
                break;
            }
        }
        if data.sz > 0 {
            // SAFETY: `data` still holds bytes the decoder did not take.
            // Its unref hands the copy to `free_packet`.
            unsafe { dav1d_data_unref(Some(NonNull::from(&mut data))) };
        }
        result
    }

    /// The pictures still held once the stream ends.
    pub(super) fn finish(&mut self, each: &mut dyn FnMut(Picture<'_>)) {
        // A failed picture at the end costs that picture alone.
        let _ = self.drain(each);
    }

    /// Take every picture ready now. The count of pictures taken.
    fn drain(&mut self, each: &mut dyn FnMut(Picture<'_>)) -> Result<usize, String> {
        let mut taken = 0;
        for _ in 0..MAX_CALLS {
            let mut picture = Dav1dPicture::default();
            // SAFETY: the context is open and `picture` is a live default
            // picture the call fills in.
            let got = unsafe { dav1d_get_picture(self.context, Some(NonNull::from(&mut picture))) };
            if got.0 == AGAIN {
                return Ok(taken);
            }
            if got.0 != 0 {
                return Err(format!("AV1 picture did not decode ({})", got.0));
            }
            taken += 1;
            // SAFETY: the picture came from `dav1d_get_picture` and stays
            // referenced until the unref below.
            if let Some(view) = unsafe { borrow_picture(&picture) } {
                each(view);
            }
            // SAFETY: the picture holds one reference, given back here once.
            unsafe { dav1d_picture_unref(Some(NonNull::from(&mut picture))) };
        }
        Ok(taken)
    }
}

/// Free a packet copy once rav1d is done reading it.
///
/// # Safety
///
/// `cookie` came from `SendSyncNonNull::from_box` of a `Box<Box<[u8]>>`,
/// and this is its only call.
unsafe extern "C" fn free_packet(_: *const u8, cookie: Option<SendSyncNonNull<std::ffi::c_void>>) {
    if let Some(cookie) = cookie {
        // SAFETY: the cookie is the box `decode` leaked, freed once here.
        drop(unsafe { Box::from_raw(cookie.cast::<Box<[u8]>>().as_ptr().as_ptr()) });
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context came from `dav1d_open` and is closed once.
        unsafe { dav1d_close(Some(NonNull::from(&mut self.context))) };
    }
}

/// A picture's planes as borrowed slices.
///
/// # Safety
///
/// `picture` came from `dav1d_get_picture` and has not been unreferenced.
unsafe fn borrow_picture(picture: &Dav1dPicture) -> Option<Picture<'_>> {
    let width = usize::try_from(picture.p.w).ok()?;
    let height = usize::try_from(picture.p.h).ok()?;
    let depth = u8::try_from(picture.p.bpc).ok()?;
    let wide = depth > 8;
    let (shift_x, shift_y) = match picture.p.layout {
        DAV1D_PIXEL_LAYOUT_I400 => (0, 0),
        DAV1D_PIXEL_LAYOUT_I420 => (1, 1),
        DAV1D_PIXEL_LAYOUT_I422 => (1, 0),
        _ => (0, 0),
    };
    let plane = |index: usize, width: usize, height: usize| -> Option<Plane<'_>> {
        let stride_bytes = usize::try_from(picture.stride[index.min(1)]).ok()?;
        let base = picture.data[index]?.as_ptr();
        let bytes_per_sample = if wide { 2 } else { 1 };
        let stride = stride_bytes / bytes_per_sample;
        if width == 0 || height == 0 || stride < width {
            return None;
        }
        let len = (height - 1) * stride + width;
        // SAFETY: rav1d allocated each plane as `height` rows of
        // `stride_bytes`, aligned for its sample type, and keeps it alive
        // while the picture is referenced.
        let samples = unsafe {
            if wide {
                Samples::Wide(std::slice::from_raw_parts(base.cast::<u16>(), len))
            } else {
                Samples::Narrow(std::slice::from_raw_parts(base.cast::<u8>(), len))
            }
        };
        Some(Plane {
            samples,
            stride,
            width,
            height,
        })
    };
    let luma = plane(0, width, height)?;
    let chroma = if picture.p.layout == DAV1D_PIXEL_LAYOUT_I400 {
        None
    } else {
        let chroma_width = (width + shift_x) >> shift_x;
        let chroma_height = (height + shift_y) >> shift_y;
        Some([
            plane(1, chroma_width, chroma_height)?,
            plane(2, chroma_width, chroma_height)?,
        ])
    };
    let colour = picture
        .seq_hdr
        // SAFETY: the sequence header lives as long as the picture.
        .map(|header| unsafe { header.as_ref() })
        .map_or(Colour::BT601, |header| {
            Colour::signalled(
                u8::try_from(header.mtrx).unwrap_or(2),
                header.color_range != 0,
                picture.p.h.unsigned_abs(),
            )
        });
    Some(Picture::Yuv(Yuv {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        luma,
        chroma,
        depth,
        colour,
    }))
}

#[cfg(test)]
mod tests {
    //! The calls into rav1d, driven by real streams.

    use super::*;

    /// The temporal units of a one-track WebM fixture.
    fn units(clip: &'static [u8]) -> Vec<Box<[u8]>> {
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;
        let stream =
            MediaSourceStream::new(Box::new(std::io::Cursor::new(clip)), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("mkv");
        let mut format = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .expect("a WebM")
            .format;
        std::iter::from_fn(|| format.next_packet().ok())
            .map(|packet| packet.data)
            .collect()
    }

    /// Each shown frame leaves once, with its own size and depth, and its
    /// grey steps up with the clip's ramp.
    #[test]
    fn each_shown_frame_leaves_once_with_its_planes() {
        let clips: [(&'static [u8], u8); 2] = [
            (include_bytes!("testdata/tiny_ramp_av1.webm"), 8),
            (include_bytes!("testdata/tiny_ramp_av1_10bit.webm"), 10),
        ];
        for (clip, depth) in clips {
            let mut decoder = Decoder::new(64 * 64, 2).expect("the decoder opens");
            let mut seen = Vec::new();
            let mut look = |picture: Picture<'_>| {
                let Picture::Yuv(yuv) = &picture else {
                    panic!("AV1 decodes to YUV");
                };
                let grey = picture.thumbnail(1, 1).expect("the planes fit")[1];
                seen.push((yuv.width, yuv.height, yuv.depth, grey));
            };
            for unit in units(clip) {
                decoder.decode(&unit, &mut look).expect("the unit decodes");
            }
            decoder.finish(&mut look);
            assert_eq!(seen.len(), 6, "{depth}-bit");
            for (index, &(width, height, bits, grey)) in seen.iter().enumerate() {
                assert_eq!((width, height, bits), (64, 36, depth));
                let expected = u8::try_from(index * 40 * 255 / 219).expect("a byte");
                assert!(grey.abs_diff(expected) <= 4, "frame {index}: {grey}");
            }
        }
    }

    /// A frame over the pixel limit is refused before rav1d allocates its
    /// planes, and no picture leaves.
    #[test]
    fn a_frame_over_the_pixel_limit_is_refused() {
        let mut decoder = Decoder::new(32 * 32, 1).expect("the decoder opens");
        let mut pictures = 0;
        let mut refused = false;
        for unit in units(include_bytes!("testdata/tiny_ramp_av1.webm")) {
            refused |= decoder.decode(&unit, &mut |_| pictures += 1).is_err();
        }
        decoder.finish(&mut |_| pictures += 1);
        assert!(refused, "64x36 is past 1024 pixels");
        assert_eq!(pictures, 0);
    }
}
