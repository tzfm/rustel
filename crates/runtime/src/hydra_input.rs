//! External textures for native Hydra.
//!
//! Network and camera work lives here, above `rustel-hydra`: the renderer only
//! accepts complete RGBA frames through a generation-scoped latest-value sink.
//! That keeps operating-system permissions and score-selected I/O out of the
//! GPU thread, and makes removal/revocation an immediate texture clear.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use image::{ImageReader, Limits};
use oximedia_capture::{
    BackendKind, CaptureConfig, CaptureEncoding, CaptureFormat, DeviceSelector,
};
use oximedia_codec::{MatrixCoefficients, Plane, VideoFrame};
use oximedia_core::PixelFormat;
use rustel_hydra::{HYDRA_SOURCE_SLOTS, HydraEvent, HydraInputLease, HydraInputSink, HydraSource};

use crate::sample_fetch;
use crate::samples::ScoreSampleAccess;

/// A decoded RGBA source occupies at most 16 MiB. With Hydra's four source
/// slots, retained CPU-side image pixels are therefore bounded to 64 MiB.
const MAX_IMAGE_EDGE: u32 = 2048;
const MAX_IMAGE_PIXELS: u64 = 4_194_304;
const MAX_DECODE_ALLOC: u64 = 32 * 1024 * 1024;
const IMAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const CAMERA_POLL: Duration = Duration::from_millis(100);
/// How many capture modes a worker tries before giving up on a device.
///
/// See [`camera_candidate_formats`] for why one mode is not enough.
const CAMERA_FORMAT_CANDIDATES: usize = 4;
const CAMERA_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// OxiMedia's AVFoundation runner requests TCC authorization asynchronously
/// after `open` returns and waits for up to 60 seconds. Its public session API
/// does not expose authorization state, so the first-frame allowance includes
/// that full prompt plus five seconds for capture startup.
const MACOS_CAMERA_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(65);
const CAMERA_RETRY: Duration = Duration::from_secs(5);
const CAMERA_PREVIEW_EDGE: usize = 64;
/// The picture a natively drawn camera theme paints from. 256×144 is about
/// 36 KiB of luminance - enough dots for a 128-column terminal at two
/// across and four down a cell, and a fiftieth of the RGBA frame a Hydra
/// lease already carries every tick.
const CAMERA_PICTURE_WIDTH: usize = 256;
const CAMERA_PICTURE_HEIGHT: usize = 144;
const SETTINGS_PREVIEW_SLOT: usize = HYDRA_SOURCE_SLOTS;
const CAMERA_REQUEST_SLOTS: usize = HYDRA_SOURCE_SLOTS + 1;
const SETTINGS_PREVIEW_BIT: u8 = 1 << SETTINGS_PREVIEW_SLOT;
/// The two consumers of the lease-less slot, as bits of one mask.
const NATIVE_REQUEST_ROW: u8 = 1;
const NATIVE_REQUEST_THEME: u8 = 2;
const CAMERA_PREFERRED_ENCODINGS: [CaptureEncoding; 11] = [
    CaptureEncoding::Raw(PixelFormat::Nv12),
    CaptureEncoding::Raw(PixelFormat::Nv21),
    CaptureEncoding::Raw(PixelFormat::Yuyv422),
    CaptureEncoding::Raw(PixelFormat::Uyvy422),
    CaptureEncoding::Raw(PixelFormat::Yuv420p),
    CaptureEncoding::Raw(PixelFormat::Yuv422p),
    CaptureEncoding::Raw(PixelFormat::Yuv444p),
    CaptureEncoding::Raw(PixelFormat::Rgb24),
    CaptureEncoding::Raw(PixelFormat::Rgba32),
    CaptureEncoding::Raw(PixelFormat::Gray8),
    CaptureEncoding::Mjpeg,
];

#[derive(Clone, Debug)]
struct InputDiagnostic {
    slot: u8,
    message: String,
    failed: bool,
}

/// The camera state shown by the Settings sheet.
///
/// This deliberately contains no backend error text, device name or path.
/// Capture backends are allowed to return platform-specific diagnostics that
/// are useful to developers but inappropriate for a persistent UI surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HydraWebcamState {
    Blocked,
    Allowed,
    Requested,
    Opening,
    Ready,
    Error,
}

impl HydraWebcamState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Allowed => "allowed",
            Self::Requested => "requested",
            Self::Opening => "opening",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }
}

/// A bounded, sanitized snapshot for a UI that must remain useful after the
/// asynchronous camera worker has exited.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HydraWebcamStatus {
    pub state: HydraWebcamState,
    /// At least one score or confirmed theme currently declares a camera,
    /// or the enabled Settings row requests its live preview, even if consent
    /// or visibility keeps the device closed.
    pub requested: bool,
    pub detail: String,
    pub preview: Option<HydraWebcamPreview>,
}

/// A 64×64 center crop retained for the Settings sheet. It is bounded to
/// 12 KiB, reuses an active Hydra camera when there is one, and
/// can come from a dedicated preview-only capture while the enabled row is
/// visibly selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HydraWebcamPreview {
    pub width: u8,
    pub height: u8,
    pub rgb: Vec<u8>,
}

/// The same camera at picture size, for a theme the studio draws itself.
///
/// Luminance only, and cover-cropped to the picture's own shape rather than
/// the thumbnail's square, because this one fills a terminal rather than a
/// corner of a panel. It is built only while a theme has asked for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HydraCameraPicture {
    pub width: u16,
    pub height: u16,
    pub luma: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CameraSlotStatus {
    #[default]
    Idle,
    Requested,
    Opening,
    Ready,
    Error,
}

struct ImageJob {
    slot: u8,
    url: String,
    access: ScoreSampleAccess,
    cancel: Arc<AtomicBool>,
    lease: HydraInputLease,
}

#[derive(Default)]
struct ImageQueueState {
    pending: Mutex<Option<ImageJob>>,
    wake: Condvar,
    closed: AtomicBool,
}

/// One lazy, latest-job worker per Hydra slot. A slow socket remains bounded
/// by its fetch deadline, but repeated evaluations can only replace one
/// pending request; they cannot create an unbounded pile of detached threads.
#[derive(Default)]
struct ImageQueue {
    state: Arc<ImageQueueState>,
    started: bool,
}

impl ImageQueue {
    fn submit(
        &mut self,
        job: ImageJob,
        diagnostics: mpsc::Sender<InputDiagnostic>,
    ) -> Result<(), String> {
        if let Ok(mut pending) = self.state.pending.lock() {
            *pending = Some(job);
        } else {
            return Err("web-image request queue is unavailable".into());
        }
        if !self.started {
            let state = Arc::clone(&self.state);
            std::thread::Builder::new()
                .name("hydra-image".into())
                .spawn(move || image_worker(state, diagnostics))
                .map_err(|error| format!("could not start web-image worker: {error}"))?;
            self.started = true;
        }
        self.state.wake.notify_one();
        Ok(())
    }

    fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
        self.state.wake.notify_all();
    }
}

struct InputPolicyState {
    webcam_allowed: AtomicBool,
    drawing: AtomicBool,
    /// Who wants the fifth logical camera slot, which never maps to a Hydra
    /// texture. Two consumers share it - the selected Settings row and a
    /// theme the studio paints itself - and it is cancelled only when both
    /// have let go, or one would close the camera out from under the other.
    native_camera_requests: AtomicU8,
    camera_slots: AtomicU8,
    camera_registrations: Mutex<[Option<CameraRegistration>; CAMERA_REQUEST_SLOTS]>,
    camera_statuses: Mutex<[CameraSlotStatus; CAMERA_REQUEST_SLOTS]>,
    camera_previews: Mutex<[Option<HydraWebcamPreview>; CAMERA_REQUEST_SLOTS]>,
    /// One cell rather than one a slot: only ever one lease-less session
    /// exists, and only it builds a picture.
    camera_picture: Mutex<Option<HydraCameraPicture>>,
    /// One log warning per requested lifetime. The status remains visible and
    /// retries continue, but a missing permission cannot write every five
    /// seconds forever.
    camera_errors_reported: AtomicU8,
    camera_ready_reported: AtomicU8,
    diagnostics: mpsc::Sender<InputDiagnostic>,
}

#[derive(Clone)]
struct CameraRegistration {
    cancel: Arc<AtomicBool>,
    /// Settings-only preview capture has no renderer lease and cannot touch
    /// any `sN` texture.
    lease: Option<HydraInputLease>,
}

impl InputPolicyState {
    fn requested_camera_slots(&self) -> u8 {
        self.camera_slots.load(Ordering::Acquire)
            | if self.native_camera_requests.load(Ordering::Acquire) != 0 {
                SETTINGS_PREVIEW_BIT
            } else {
                0
            }
    }

    fn camera_registrations(
        &self,
    ) -> std::sync::MutexGuard<'_, [Option<CameraRegistration>; CAMERA_REQUEST_SLOTS]> {
        self.camera_registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn register_camera(
        &self,
        slot: usize,
        cancel: Arc<AtomicBool>,
        lease: Option<HydraInputLease>,
    ) {
        self.camera_registrations()[slot] = Some(CameraRegistration { cancel, lease });
    }

    fn unregister_camera(&self, slot: usize, cancel: &Arc<AtomicBool>) {
        let mut cameras = self.camera_registrations();
        if cameras[slot]
            .as_ref()
            .is_some_and(|registered| Arc::ptr_eq(&registered.cancel, cancel))
        {
            cameras[slot] = None;
        }
    }

    fn cancel_camera(&self, slot: usize) {
        if let Some(camera) = self.camera_registrations()[slot].as_ref()
            && !camera.cancel.swap(true, Ordering::AcqRel)
            && let Some(lease) = &camera.lease
        {
            lease.clear_if_current();
        }
    }

    fn cancel_all_cameras(&self) {
        for camera in self.camera_registrations().iter().flatten() {
            camera.cancel.store(true, Ordering::Release);
        }
    }

    /// Cancel workers that still own a live camera lease, returning their
    /// slots. A registered worker whose cancellation bit is already set no
    /// longer owns the shared source slot: a score image may have rebound it
    /// while the platform open/read call winds down.
    fn cancel_active_cameras(&self) -> u8 {
        self.camera_registrations()
            .iter()
            .enumerate()
            .fold(0u8, |mask, (slot, camera)| {
                let Some(camera) = camera else {
                    return mask;
                };
                if camera.cancel.swap(true, Ordering::AcqRel) {
                    return mask;
                }
                if let Some(lease) = &camera.lease {
                    lease.clear_if_current();
                }
                mask | (1 << slot)
            })
    }

    fn camera_statuses(
        &self,
    ) -> std::sync::MutexGuard<'_, [CameraSlotStatus; CAMERA_REQUEST_SLOTS]> {
        self.camera_statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_camera_status(&self, slot: usize, status: CameraSlotStatus) {
        if let Some(held) = self.camera_statuses().get_mut(slot) {
            *held = status;
        }
        if status != CameraSlotStatus::Ready {
            if let Some(preview) = self
                .camera_previews
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(slot)
            {
                *preview = None;
            }
            if slot == SETTINGS_PREVIEW_SLOT {
                self.clear_camera_picture();
            }
        }
    }

    fn clear_camera_picture(&self) {
        *self
            .camera_picture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn set_camera_picture(&self, picture: HydraCameraPicture) {
        *self
            .camera_picture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(picture);
    }

    fn set_camera_preview(&self, slot: usize, preview: HydraWebcamPreview) {
        if let Some(held) = self
            .camera_previews
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(slot)
        {
            *held = Some(preview);
        }
    }

    fn reset_camera_reports(&self, slot: usize) {
        self.camera_errors_reported
            .fetch_and(!(1 << slot), Ordering::AcqRel);
        self.camera_ready_reported
            .fetch_and(!(1 << slot), Ordering::AcqRel);
    }

    fn report_camera_error(&self, slot: u8, message: &'static str) {
        let bit = 1 << slot;
        if self.camera_errors_reported.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            let _ = self.diagnostics.send(InputDiagnostic {
                slot: diagnostic_slot(slot),
                message: message.into(),
                failed: true,
            });
        }
    }

    fn report_camera_ready(&self, slot: u8) {
        let bit = 1 << slot;
        if self.camera_ready_reported.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            let _ = self.diagnostics.send(InputDiagnostic {
                slot: diagnostic_slot(slot),
                message: "webcam ready".into(),
                failed: false,
            });
        }
    }

    /// Commit UI state for a frame only while this exact camera registration
    /// still owns the slot. Revocation takes the same registration mutex, so
    /// it either happens after this commit and clears it, or happens first and
    /// makes this stale worker a no-op - even across a rapid re-enable.
    fn commit_camera_frame(
        &self,
        slot: usize,
        cancel: &Arc<AtomicBool>,
        preview: Option<HydraWebcamPreview>,
        picture: Option<HydraCameraPicture>,
        first: bool,
    ) -> bool {
        self.with_current_camera(slot, cancel, || {
            if let Some(preview) = preview {
                self.set_camera_preview(slot, preview);
            }
            if let Some(picture) = picture {
                self.set_camera_picture(picture);
            }
            if first {
                self.set_camera_status(slot, CameraSlotStatus::Ready);
                self.report_camera_ready(slot as u8);
            }
        })
    }

    fn commit_camera_opening(&self, slot: usize, cancel: &Arc<AtomicBool>) -> bool {
        self.with_current_camera(slot, cancel, || {
            self.set_camera_status(slot, CameraSlotStatus::Opening);
        })
    }

    fn commit_camera_error(
        &self,
        slot: usize,
        cancel: &Arc<AtomicBool>,
        message: &'static str,
    ) -> bool {
        self.with_current_camera(slot, cancel, || {
            self.set_camera_status(slot, CameraSlotStatus::Error);
            self.report_camera_error(slot as u8, message);
        })
    }

    fn with_current_camera(
        &self,
        slot: usize,
        cancel: &Arc<AtomicBool>,
        commit: impl FnOnce(),
    ) -> bool {
        let registrations = self.camera_registrations();
        let current = registrations[slot]
            .as_ref()
            .is_some_and(|camera| Arc::ptr_eq(&camera.cancel, cancel));
        if !current
            || cancel.load(Ordering::Acquire)
            || !self.webcam_allowed.load(Ordering::Acquire)
        {
            return false;
        }
        commit();
        drop(registrations);
        true
    }

    fn set_requested_camera_slots(&self, mask: u8) {
        self.camera_slots.store(mask, Ordering::Release);
        self.refresh_requested_camera_statuses();
    }

    fn refresh_requested_camera_statuses(&self) {
        let mask = self.requested_camera_slots();
        let mut clear_previews = 0u8;
        let mut statuses = self.camera_statuses();
        for (slot, status) in statuses.iter_mut().enumerate() {
            if mask & (1 << slot) == 0 {
                *status = CameraSlotStatus::Idle;
                clear_previews |= 1 << slot;
            } else if *status == CameraSlotStatus::Idle {
                *status = CameraSlotStatus::Requested;
                clear_previews |= 1 << slot;
            }
        }
        drop(statuses);
        self.clear_camera_previews(clear_previews);
    }

    fn reset_inactive_camera_statuses(&self, active_mask: u8) {
        let requested = self.requested_camera_slots();
        let mut clear_previews = 0u8;
        let mut statuses = self.camera_statuses();
        for (slot, status) in statuses.iter_mut().enumerate() {
            let bit = 1 << slot;
            if requested & bit == 0 {
                *status = CameraSlotStatus::Idle;
                clear_previews |= bit;
            } else if active_mask & bit == 0 {
                *status = CameraSlotStatus::Requested;
                clear_previews |= bit;
            }
        }
        drop(statuses);
        self.clear_camera_previews(clear_previews);
    }

    fn clear_camera_previews(&self, mask: u8) {
        if mask == 0 {
            return;
        }
        let mut previews = self
            .camera_previews
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (slot, preview) in previews.iter_mut().enumerate() {
            if mask & (1 << slot) != 0 {
                *preview = None;
            }
        }
        drop(previews);
        // The picture belongs to the same lease-less session, so it goes
        // when that session's thumbnail does: a frame left here after
        // revocation is a frame a theme could still paint.
        if mask & SETTINGS_PREVIEW_BIT != 0 {
            self.clear_camera_picture();
        }
    }

    fn webcam_status(&self) -> HydraWebcamStatus {
        let requested = self.requested_camera_slots();
        if !self.webcam_allowed.load(Ordering::Acquire) {
            return HydraWebcamStatus {
                state: HydraWebcamState::Blocked,
                requested: requested != 0,
                detail: if requested == 0 {
                    "Camera access is off; camera-backed Hydra sources stay closed.".into()
                } else {
                    "A Hydra backdrop requests the camera; enable this row to allow it.".into()
                },
                preview: None,
            };
        }
        if requested == 0 {
            return HydraWebcamStatus {
                state: HydraWebcamState::Allowed,
                requested: false,
                detail: "Camera access is allowed; no Hydra backdrop currently requests it.".into(),
                preview: None,
            };
        }

        let statuses = self.camera_statuses();
        let requested_statuses = statuses
            .iter()
            .enumerate()
            .filter(|(slot, _)| requested & (1 << slot) != 0)
            .map(|(_, status)| *status)
            .collect::<Vec<_>>();
        let hydra_worker_active = statuses[..HYDRA_SOURCE_SLOTS].iter().any(|status| {
            matches!(
                status,
                CameraSlotStatus::Opening | CameraSlotStatus::Ready | CameraSlotStatus::Error
            )
        });
        let settings_drives_camera = requested & SETTINGS_PREVIEW_BIT != 0 && !hydra_worker_active;
        let state = if requested_statuses.contains(&CameraSlotStatus::Error) {
            HydraWebcamState::Error
        } else if requested_statuses.contains(&CameraSlotStatus::Ready) {
            HydraWebcamState::Ready
        } else if requested_statuses.contains(&CameraSlotStatus::Opening) {
            HydraWebcamState::Opening
        } else {
            HydraWebcamState::Requested
        };
        drop(statuses);
        let preview = if state == HydraWebcamState::Ready {
            self.camera_previews
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .enumerate()
                .find(|(slot, preview)| requested & (1 << slot) != 0 && preview.is_some())
                .and_then(|(_, preview)| preview.clone())
        } else {
            None
        };
        HydraWebcamStatus {
            state,
            requested: true,
            detail: match state {
                HydraWebcamState::Requested => {
                    if settings_drives_camera {
                        "Settings live preview requested; it opens only while this enabled row is selected."
                            .into()
                    } else {
                        "Camera requested; it opens only while that Hydra backdrop is visible."
                            .into()
                    }
                }
                HydraWebcamState::Opening => webcam_opening_detail(settings_drives_camera).into(),
                HydraWebcamState::Ready if settings_drives_camera => {
                    "Camera is ready for the Settings live preview.".into()
                }
                HydraWebcamState::Ready => "Camera is ready and feeding Hydra.".into(),
                HydraWebcamState::Error => webcam_error_guidance().into(),
                HydraWebcamState::Blocked | HydraWebcamState::Allowed => unreachable!(),
            },
            preview,
        }
    }
}

fn webcam_opening_detail(settings_preview: bool) -> &'static str {
    if settings_preview && cfg!(target_os = "macos") {
        "Opening the camera for Settings live preview; waiting for the macOS permission answer if prompted…"
    } else if settings_preview {
        "Opening the camera for Settings live preview…"
    } else if cfg!(target_os = "macos") {
        "Opening the requested Hydra camera; waiting for the macOS permission answer if prompted…"
    } else {
        "Opening the requested Hydra camera…"
    }
}

fn diagnostic_slot(slot: u8) -> u8 {
    // The fifth logical worker exists only for the Settings thumbnail. Keep
    // diagnostics phrased against the real default camera source rather than
    // inventing a protocol-level `s4`.
    if usize::from(slot) == SETTINGS_PREVIEW_SLOT {
        0
    } else {
        slot
    }
}

fn camera_first_frame_timeout(backend: BackendKind) -> Duration {
    if backend == BackendKind::AvFoundation {
        MACOS_CAMERA_FIRST_FRAME_TIMEOUT
    } else {
        CAMERA_FIRST_FRAME_TIMEOUT
    }
}

fn webcam_error_guidance() -> &'static str {
    if cfg!(target_os = "macos") {
        "Camera could not open. In System Settings → Privacy & Security → Camera, allow this app; then retry (the camera may also be busy)."
    } else if cfg!(target_os = "windows") {
        "Camera could not open. In Settings → Privacy & security → Camera, allow camera access for this app; then retry (the camera may also be busy)."
    } else if cfg!(target_os = "linux") {
        "Camera could not open. Check this app's camera permission and /dev/video access, then retry (the camera may also be busy)."
    } else {
        "Camera could not open. Check camera permission and device availability for this app, then retry."
    }
}

/// A live, non-queued privacy policy handle retained by the studio UI.
///
/// The engine's ordinary command channel is deliberately lossy under load.
/// Webcam revocation cannot be: this handle flips the policy atomically and
/// invalidates every camera lease before returning to the settings sheet.
#[derive(Clone)]
pub struct HydraInputPolicy {
    state: Arc<InputPolicyState>,
}

impl HydraInputPolicy {
    pub fn webcam_allowed(&self) -> bool {
        self.state.webcam_allowed.load(Ordering::Acquire)
    }

    pub fn webcam_status(&self) -> HydraWebcamStatus {
        self.state.webcam_status()
    }

    /// Request a live Settings thumbnail. The caller keeps this true only
    /// while the enabled webcam row is visibly selected. Turning it off
    /// synchronously makes the worker token stale; a blocking platform call
    /// may return later, but it cannot publish another preview.
    pub fn set_settings_preview_requested(&self, requested: bool) {
        self.set_native_camera_request(NATIVE_REQUEST_ROW, requested);
    }

    /// Request the picture a natively drawn camera theme paints from. The
    /// caller keeps this true only while that theme's picture is on screen.
    pub fn set_theme_picture_requested(&self, requested: bool) {
        self.set_native_camera_request(NATIVE_REQUEST_THEME, requested);
    }

    /// The newest picture, taken rather than read: the painter holds its own
    /// copy for as long as it is fresh, and leaving it here would mean
    /// deciding twice how long that is.
    pub fn take_camera_picture(&self) -> Option<HydraCameraPicture> {
        self.state
            .camera_picture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// One consumer of the lease-less slot changing its mind. The camera is
    /// cancelled only once nobody wants it.
    fn set_native_camera_request(&self, bit: u8, requested: bool) {
        let before = if requested {
            self.state
                .native_camera_requests
                .fetch_or(bit, Ordering::AcqRel)
        } else {
            self.state
                .native_camera_requests
                .fetch_and(!bit, Ordering::AcqRel)
        };
        let after = if requested {
            before | bit
        } else {
            before & !bit
        };
        if before == after {
            return;
        }
        self.state.reset_camera_reports(SETTINGS_PREVIEW_SLOT);
        if after == 0 {
            self.state.cancel_camera(SETTINGS_PREVIEW_SLOT);
        }
        self.state.refresh_requested_camera_statuses();
    }

    pub fn set_webcam_allowed(&self, allowed: bool) {
        if self.state.webcam_allowed.swap(allowed, Ordering::AcqRel) == allowed {
            return;
        }
        self.state
            .camera_errors_reported
            .store(0, Ordering::Release);
        self.state.camera_ready_reported.store(0, Ordering::Release);
        if !allowed {
            // Nothing painted from a camera outlives the permission to have
            // opened it.
            self.state.clear_camera_picture();
            // A later re-allow must not let a worker that existed at the
            // moment of revocation resume. Cancellation is sticky per worker.
            let active_camera_mask = self.state.cancel_active_cameras();
            self.state.reset_inactive_camera_statuses(0);
            if active_camera_mask != 0 {
                let _ = self.state.diagnostics.send(InputDiagnostic {
                    slot: diagnostic_slot(active_camera_mask.trailing_zeros() as u8),
                    message: "Hydra webcam access was revoked; camera sources were cleared".into(),
                    failed: false,
                });
            }
        }
    }
}

struct ManagedSource {
    source: HydraSource,
    cancel: Arc<AtomicBool>,
}

impl ManagedSource {
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CameraPurpose {
    Score,
    Theme,
    SettingsPreview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CameraRequest {
    pub(crate) purpose: CameraPurpose,
    device: Option<u32>,
}

struct CameraWorker {
    request: CameraRequest,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    lease: Option<HydraInputLease>,
}

/// Acquisition owned by the engine thread. Workers never touch the Session or
/// the renderer; they can only publish through their generation-scoped lease.
pub struct HydraInputs {
    sink: HydraInputSink,
    policy: HydraInputPolicy,
    score_sample_access: ScoreSampleAccess,
    slots: [Option<ManagedSource>; HYDRA_SOURCE_SLOTS],
    /// Whether the selected theme explicitly uses the default camera as s0.
    /// It is only an acquisition request while the score renderer is absent.
    theme_camera: bool,
    cameras: [Option<CameraWorker>; CAMERA_REQUEST_SLOTS],
    camera_retry_at: [Option<Instant>; CAMERA_REQUEST_SLOTS],
    images: [ImageQueue; HYDRA_SOURCE_SLOTS],
    diagnostics: mpsc::Receiver<InputDiagnostic>,
    diagnostics_tx: mpsc::Sender<InputDiagnostic>,
}

impl HydraInputs {
    pub fn new(sink: HydraInputSink) -> Self {
        let (diagnostics_tx, diagnostics) = mpsc::channel();
        let policy = HydraInputPolicy {
            state: Arc::new(InputPolicyState {
                webcam_allowed: AtomicBool::new(false),
                drawing: AtomicBool::new(false),
                native_camera_requests: AtomicU8::new(0),
                camera_slots: AtomicU8::new(0),
                camera_registrations: Mutex::new(std::array::from_fn(|_| None)),
                camera_statuses: Mutex::new([CameraSlotStatus::Idle; CAMERA_REQUEST_SLOTS]),
                camera_previews: Mutex::new(std::array::from_fn(|_| None)),
                camera_picture: Mutex::new(None),
                camera_errors_reported: AtomicU8::new(0),
                camera_ready_reported: AtomicU8::new(0),
                diagnostics: diagnostics_tx.clone(),
            }),
        };
        Self {
            sink,
            policy,
            score_sample_access: ScoreSampleAccess::denied(),
            slots: std::array::from_fn(|_| None),
            theme_camera: false,
            cameras: std::array::from_fn(|_| None),
            camera_retry_at: [None; CAMERA_REQUEST_SLOTS],
            images: std::array::from_fn(|_| ImageQueue::default()),
            diagnostics,
            diagnostics_tx,
        }
    }

    pub fn policy(&self) -> HydraInputPolicy {
        self.policy.clone()
    }

    pub(crate) fn set_score_sample_access(&mut self, access: ScoreSampleAccess) {
        self.score_sample_access = access;
    }

    /// Replace the source plan after a validated program was accepted.
    /// Re-evaluation creates fresh generations even for identical text, which
    /// both retries a failed load and mirrors another `initCam/initImage` call.
    pub(crate) fn configure(&mut self, sources: [Option<HydraSource>; HYDRA_SOURCE_SLOTS]) {
        // A score evaluation is a fresh request lifetime only for protocol
        // source slots. It must not reset the independent Settings-preview
        // retry/dedup state and turn ordinary typing into repeated opens.
        self.policy
            .state
            .camera_errors_reported
            .fetch_and(SETTINGS_PREVIEW_BIT, Ordering::AcqRel);
        self.policy
            .state
            .camera_ready_reported
            .fetch_and(SETTINGS_PREVIEW_BIT, Ordering::AcqRel);
        let preserve_theme_s0 = !self.policy.state.drawing.load(Ordering::Acquire)
            && self.theme_camera
            && self.cameras[0].as_ref().is_some_and(|worker| {
                worker.request.purpose == CameraPurpose::Theme
                    && !worker.cancel.load(Ordering::Acquire)
                    && !worker.done.load(Ordering::Acquire)
            });
        for (slot, old) in self.slots.iter_mut().enumerate() {
            if let Some(mut old) = old.take() {
                old.stop();
            }
            if slot == 0 && preserve_theme_s0 {
                // A stopped/non-Hydra score is not the visible source owner.
                // Replacing its dormant plan must not blink or reopen the
                // camera-backed theme that still owns s0.
                continue;
            }
            // Keep the worker tracked until its monolithic platform open or
            // next frame poll returns. A replacement may wait, never overlap
            // an old attempt and grow an unbounded set of device threads.
            self.policy.state.cancel_camera(slot);
            self.sink.clear(slot as u8);
        }

        let mut camera_mask = 0u8;
        for (slot, source) in sources.into_iter().enumerate() {
            let Some(source) = source else {
                continue;
            };
            if matches!(source, HydraSource::Camera { .. }) {
                camera_mask |= 1 << slot;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            self.slots[slot] = Some(ManagedSource { source, cancel });
        }
        self.camera_retry_at[..HYDRA_SOURCE_SLOTS].fill(None);
        self.refresh_requested_camera_slots();

        self.reconcile_cameras();
        if self.policy.state.drawing.load(Ordering::Acquire) {
            // Retire a camera's lease before an image binds the same slot.
            // Reversing these two calls lets the camera clear invalidate the
            // freshly queued one-shot image generation.
            self.start_images();
        }

        if camera_mask != 0 && !self.policy.webcam_allowed() {
            let _ = self.diagnostics_tx.send(InputDiagnostic {
                slot: camera_mask.trailing_zeros() as u8,
                message: "webcam source blocked; enable Hydra webcam in Settings".into(),
                failed: true,
            });
        }
    }

    /// Select whether the theme renderer owns a default-camera `s0` while it
    /// is the visible backdrop. Changing themes never disturbs a score camera
    /// that currently owns the slot; ownership turns over in reconciliation
    /// when the score renderer actually becomes inactive.
    pub(crate) fn set_theme_camera(&mut self, requested: bool) {
        if self.theme_camera == requested {
            return;
        }
        self.theme_camera = requested;
        self.policy.state.reset_camera_reports(0);
        self.camera_retry_at[0] = None;
        if self.cameras[0]
            .as_ref()
            .is_some_and(|worker| worker.request.purpose == CameraPurpose::Theme)
        {
            self.policy.state.cancel_camera(0);
        }
        self.refresh_requested_camera_slots();
        self.reconcile_cameras();
        if requested && !self.policy.webcam_allowed() {
            let _ = self.diagnostics_tx.send(InputDiagnostic {
                slot: 0,
                message: "webcam theme blocked; enable Hydra webcam in Settings".into(),
                failed: true,
            });
        }
    }

    pub(crate) fn set_drawing(&mut self, drawing: bool) {
        let changed = self.policy.state.drawing.swap(drawing, Ordering::AcqRel) != drawing;
        if changed && !drawing {
            for (slot, managed) in self.slots.iter_mut().enumerate() {
                if let Some(managed) = managed {
                    managed.stop();
                    self.sink.clear(slot as u8);
                }
            }
        }
        self.refresh_requested_camera_slots();
        self.reconcile_cameras();
        if changed && drawing {
            // Camera handoff must clear its old generation first. Images need
            // no webcam consent, but they share the transport lifecycle: Play
            // now binds and fetches a fresh generation after Stop.
            self.start_images();
        }
    }

    fn score_camera_mask(&self) -> u8 {
        self.slots
            .iter()
            .enumerate()
            .fold(0u8, |mask, (slot, managed)| {
                if managed
                    .as_ref()
                    .is_some_and(|managed| matches!(managed.source, HydraSource::Camera { .. }))
                {
                    mask | (1 << slot)
                } else {
                    mask
                }
            })
    }

    fn refresh_requested_camera_slots(&self) {
        let mut mask = self.score_camera_mask();
        if self.theme_camera {
            mask |= 1;
        }
        self.policy.state.set_requested_camera_slots(mask);
    }

    pub(crate) fn desired_camera(&self, slot: usize) -> Option<CameraRequest> {
        if slot == SETTINGS_PREVIEW_SLOT {
            let requested = self
                .policy
                .state
                .native_camera_requests
                .load(Ordering::Acquire)
                != 0;
            let hydra_camera_visible = (0..HYDRA_SOURCE_SLOTS)
                .any(|source_slot| self.desired_camera(source_slot).is_some());
            return (requested && !hydra_camera_visible).then_some(CameraRequest {
                purpose: CameraPurpose::SettingsPreview,
                device: None,
            });
        }
        if slot >= HYDRA_SOURCE_SLOTS {
            return None;
        }
        if self.policy.state.drawing.load(Ordering::Acquire) {
            return self.slots[slot].as_ref().and_then(|managed| {
                let HydraSource::Camera { device } = managed.source else {
                    return None;
                };
                Some(CameraRequest {
                    purpose: CameraPurpose::Score,
                    device,
                })
            });
        }
        (self.theme_camera && slot == 0).then_some(CameraRequest {
            purpose: CameraPurpose::Theme,
            device: None,
        })
    }

    fn start_images(&mut self) {
        for slot in 0..HYDRA_SOURCE_SLOTS {
            let Some(url) = self.slots[slot].as_ref().and_then(|managed| {
                if let HydraSource::ImageUrl { url } = &managed.source {
                    Some(url.clone())
                } else {
                    None
                }
            }) else {
                continue;
            };
            let cancel = Arc::new(AtomicBool::new(false));
            if let Some(managed) = self.slots[slot].as_mut() {
                managed.cancel.store(true, Ordering::Release);
                managed.cancel = Arc::clone(&cancel);
            }
            let Some(lease) = self.sink.bind(slot as u8) else {
                continue;
            };
            let job = ImageJob {
                slot: slot as u8,
                url,
                access: self.score_sample_access.clone(),
                cancel,
                lease,
            };
            if let Err(message) = self.images[slot].submit(job, self.diagnostics_tx.clone()) {
                let _ = self.diagnostics_tx.send(InputDiagnostic {
                    slot: slot as u8,
                    message,
                    failed: true,
                });
            }
        }
    }

    /// Observe live UI policy changes even on ticks with no Hydra signals.
    pub(crate) fn reconcile_cameras(&mut self) {
        let now = Instant::now();
        let active_mask = (0..CAMERA_REQUEST_SLOTS).fold(0u8, |mask, slot| {
            if self.desired_camera(slot).is_some() {
                mask | (1 << slot)
            } else {
                mask
            }
        });
        self.policy
            .state
            .reset_inactive_camera_statuses(active_mask);

        for slot in 0..CAMERA_REQUEST_SLOTS {
            if self.cameras[slot]
                .as_ref()
                .is_some_and(|worker| worker.done.load(Ordering::Acquire))
            {
                let worker = self.cameras[slot]
                    .take()
                    .expect("the completed camera was just observed");
                self.policy.state.unregister_camera(slot, &worker.cancel);
                let was_cancelled = worker.cancel.load(Ordering::Acquire);
                if !was_cancelled && let Some(lease) = &worker.lease {
                    lease.clear_if_current();
                }
                self.camera_retry_at[slot] = (!was_cancelled).then_some(now + CAMERA_RETRY);
                if was_cancelled {
                    let status = if self.policy.state.requested_camera_slots() & (1 << slot) == 0 {
                        CameraSlotStatus::Idle
                    } else {
                        CameraSlotStatus::Requested
                    };
                    self.policy.state.set_camera_status(slot, status);
                }
            }

            let desired = self.desired_camera(slot);
            if let Some(worker) = self.cameras[slot].as_ref()
                && Some(worker.request) != desired
            {
                self.policy.state.cancel_camera(slot);
                let status = if self.policy.state.requested_camera_slots() & (1 << slot) == 0 {
                    CameraSlotStatus::Idle
                } else {
                    CameraSlotStatus::Requested
                };
                self.policy.state.set_camera_status(slot, status);
                continue;
            }

            let Some(request) = desired else {
                continue;
            };
            if !self.policy.webcam_allowed() {
                if self.cameras[slot].is_some() {
                    self.policy.state.cancel_camera(slot);
                }
                continue;
            }
            if self.cameras[slot].is_some() {
                continue;
            }
            // A Settings thumbnail has no Hydra texture, but it is still a
            // real device session. Hand it off before starting a score/theme
            // camera, and never open it beside any source camera.
            if (slot < HYDRA_SOURCE_SLOTS && self.cameras[SETTINGS_PREVIEW_SLOT].is_some())
                || (slot == SETTINGS_PREVIEW_SLOT
                    && self.cameras[..HYDRA_SOURCE_SLOTS]
                        .iter()
                        .any(Option::is_some))
            {
                continue;
            }
            if self.camera_retry_at[slot].is_some_and(|retry| now < retry) {
                continue;
            }

            let lease = if slot < HYDRA_SOURCE_SLOTS {
                let Some(lease) = self.sink.bind(slot as u8) else {
                    continue;
                };
                Some(lease)
            } else {
                None
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            self.policy
                .state
                .register_camera(slot, Arc::clone(&cancel), lease.clone());

            // Registration closes the race with a UI-thread revocation: if
            // policy changed just before it, this check cancels; if it changes
            // after it, set_webcam_allowed(false) finds this exact token.
            if !self.policy.webcam_allowed()
                || self.desired_camera(slot) != Some(request)
                || !self.policy.state.commit_camera_opening(slot, &cancel)
            {
                self.policy.state.cancel_camera(slot);
                self.policy.state.unregister_camera(slot, &cancel);
                continue;
            }

            let worker_cancel = Arc::clone(&cancel);
            let worker_done = Arc::clone(&done);
            let worker_lease = lease.clone();
            let policy = Arc::clone(&self.policy.state);
            let worker_name = if slot == SETTINGS_PREVIEW_SLOT {
                "hydra-camera-settings".to_owned()
            } else {
                format!("hydra-camera-s{slot}")
            };
            match std::thread::Builder::new()
                .name(worker_name)
                .spawn(move || {
                    let _done = CameraDone(worker_done);
                    capture_camera(slot as u8, request.device, worker_cancel, policy, lease)
                }) {
                Ok(_) => {
                    self.cameras[slot] = Some(CameraWorker {
                        request,
                        cancel,
                        done,
                        lease: worker_lease,
                    });
                    self.camera_retry_at[slot] = None;
                }
                Err(_error) => {
                    self.policy.state.commit_camera_error(
                        slot,
                        &cancel,
                        "could not start the webcam worker; retrying shortly",
                    );
                    self.policy.state.unregister_camera(slot, &cancel);
                    if let Some(lease) = &worker_lease {
                        lease.clear_if_current();
                    }
                    self.camera_retry_at[slot] = Some(now + CAMERA_RETRY);
                }
            }
        }
    }

    pub(crate) fn take_events(&self) -> Vec<HydraEvent> {
        self.diagnostics
            .try_iter()
            .map(|event| HydraEvent::Input {
                slot: event.slot,
                message: event.message,
                failed: event.failed,
            })
            .collect()
    }
}

struct CameraDone(Arc<AtomicBool>);

impl Drop for CameraDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct CameraLeaseClear(Option<HydraInputLease>);

impl Drop for CameraLeaseClear {
    fn drop(&mut self) {
        if let Some(lease) = &self.0 {
            lease.clear_if_current();
        }
    }
}

impl Drop for HydraInputs {
    fn drop(&mut self) {
        self.policy
            .state
            .webcam_allowed
            .store(false, Ordering::Release);
        self.policy.state.drawing.store(false, Ordering::Release);
        self.policy.state.cancel_all_cameras();
        for (slot, managed) in self.slots.iter_mut().enumerate() {
            if let Some(managed) = managed {
                managed.stop();
            }
            self.sink.clear(slot as u8);
            self.images[slot].close();
        }
    }
}

fn image_worker(state: Arc<ImageQueueState>, diagnostics: mpsc::Sender<InputDiagnostic>) {
    loop {
        let job = {
            let Ok(mut pending) = state.pending.lock() else {
                return;
            };
            while pending.is_none() && !state.closed.load(Ordering::Acquire) {
                let Ok(next) = state.wake.wait(pending) else {
                    return;
                };
                pending = next;
            }
            if state.closed.load(Ordering::Acquire) {
                return;
            }
            pending.take()
        };
        let Some(ImageJob {
            slot,
            url,
            access,
            cancel,
            lease,
        }) = job
        else {
            continue;
        };
        let result = (|| {
            let budget = sample_fetch::FetchBudget::until(
                Instant::now() + IMAGE_FETCH_TIMEOUT,
                Arc::clone(&cancel),
            );
            let bytes = sample_fetch::fetch_image_with_budget(&url, &budget, &access)?;
            budget.check()?;
            let (width, height, rgba) = decode_image(&bytes)?;
            budget.check()?;
            if lease.publish(width, height, rgba)? {
                let _ = diagnostics.send(InputDiagnostic {
                    slot,
                    message: format!("web image ready ({width}x{height})"),
                    failed: false,
                });
            }
            Ok::<(), String>(())
        })();
        if let Err(message) = result
            && !cancel.load(Ordering::Acquire)
        {
            let _ = diagnostics.send(InputDiagnostic {
                slot,
                message,
                failed: true,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CameraSetupError {
    Unavailable,
    NoSafeFormat,
    NegotiatedFormatChanged,
}

impl CameraSetupError {
    fn message(self) -> &'static str {
        match self {
            Self::Unavailable => webcam_error_guidance(),
            Self::NoSafeFormat => {
                "webcam has no supported mode within Hydra's 2048px/4MP safety limit; try another device index"
            }
            Self::NegotiatedFormatChanged => {
                "webcam capture modes changed while opening; retrying shortly"
            }
        }
    }
}

fn supported_camera_format(format: CaptureFormat) -> bool {
    format.width != 0
        && format.height != 0
        && format.width <= MAX_IMAGE_EDGE
        && format.height <= MAX_IMAGE_EDGE
        && format.area() <= MAX_IMAGE_PIXELS
        && CAMERA_PREFERRED_ENCODINGS.contains(&format.encoding)
        && match format.encoding {
            CaptureEncoding::Raw(
                PixelFormat::Nv12 | PixelFormat::Nv21 | PixelFormat::Yuyv422 | PixelFormat::Uyvy422,
            ) => format.width.is_multiple_of(2),
            _ => true,
        }
}

fn select_camera_format(formats: &[CaptureFormat]) -> Option<CaptureFormat> {
    let safe = formats
        .iter()
        .copied()
        .filter(|format| supported_camera_format(*format))
        .collect::<Vec<_>>();
    let request = CaptureConfig::default()
        .with_size(1280, 720)
        .with_fps(30.0)
        .with_preferred(CAMERA_PREFERRED_ENCODINGS);
    oximedia_capture::negotiate(&safe, &request).ok()
}

fn camera_inventory_format(formats: &[CaptureFormat]) -> Result<CaptureFormat, CameraSetupError> {
    // Media Foundation can expose a device while withholding its formats
    // when Windows camera privacy access is denied. An empty inventory is
    // therefore an availability/permission failure, not evidence that the
    // user's camera only supports unsafe modes. This classification is also
    // the most actionable one if another backend returns an empty inventory.
    if formats.is_empty() {
        return Err(CameraSetupError::Unavailable);
    }
    select_camera_format(formats).ok_or(CameraSetupError::NoSafeFormat)
}

/// The mode an untouched capture session is most likely to actually deliver.
///
/// Every `AVCaptureSession` starts on `AVCaptureSessionPresetHigh`, and on a
/// device that refuses `AVCaptureSessionPresetInputPriority` - Apple's
/// built-in cameras among them - that preset outranks the `activeFormat` the
/// backend selected: `-startRunning` silently restores the device's default
/// mode and every frame then arrives in a size the session did not ask for.
///
/// The preset asks for the highest-quality *ordinary* mode, so the square and
/// portrait modes that back Center Stage and Desk View are never its answer.
/// The widest landscape mode is the closest a format inventory can come to
/// naming what the preset will pick, which makes it the candidate worth
/// spending the first (and longest) attempt on.
fn preset_default_format(formats: &[CaptureFormat]) -> Option<CaptureFormat> {
    formats
        .iter()
        .copied()
        .filter(|format| format.width > format.height)
        .max_by(|left, right| {
            left.width
                .cmp(&right.width)
                .then_with(|| left.height.cmp(&right.height))
                .then_with(|| left.cmp_fps(*right))
        })
}

/// Add `format` unless the list is full or already holds an equivalent mode.
fn push_camera_candidate(candidates: &mut Vec<CaptureFormat>, format: CaptureFormat) {
    if candidates.len() < CAMERA_FORMAT_CANDIDATES
        && !candidates
            .iter()
            .any(|held| camera_format_matches(*held, format))
    {
        candidates.push(format);
    }
}

/// Order the device's safe modes into the sequence a worker will try.
///
/// A camera can advertise a mode, accept it, and then deliver a different one
/// anyway - see [`preset_default_format`] for the macOS mechanism. The frames
/// are dropped before they ever reach this crate, so from here the device is
/// indistinguishable from one that opened and went silent; the only way to
/// tell the two apart is to try another mode. Hence a list rather than a
/// single choice, ordered so the first entry is the one most likely to be
/// honoured and the rest descend from largest to smallest.
fn camera_candidate_formats(
    formats: &[CaptureFormat],
) -> Result<Vec<CaptureFormat>, CameraSetupError> {
    let negotiated = camera_inventory_format(formats)?;
    let mut safe = formats
        .iter()
        .copied()
        .filter(|format| supported_camera_format(*format))
        .collect::<Vec<_>>();
    let mut candidates = Vec::with_capacity(CAMERA_FORMAT_CANDIDATES);
    if cfg!(target_os = "macos")
        && let Some(default_mode) = preset_default_format(&safe)
    {
        push_camera_candidate(&mut candidates, default_mode);
    }
    push_camera_candidate(&mut candidates, negotiated);
    safe.sort_by(|left, right| {
        right
            .width
            .cmp(&left.width)
            .then_with(|| right.height.cmp(&left.height))
            .then_with(|| right.cmp_fps(*left))
    });
    for format in safe {
        push_camera_candidate(&mut candidates, format);
    }
    Ok(candidates)
}

fn camera_config(id: &str, selected: CaptureFormat) -> CaptureConfig {
    let mut config = CaptureConfig::default()
        .with_device(DeviceSelector::Id(id.to_owned()))
        .with_size(selected.width, selected.height)
        .with_preferred([selected.encoding])
        .with_queue_depth(1);
    if selected.fps() > 0.0 {
        config = config.with_fps(selected.fps());
    }
    config
}

/// The modes to try for one device, best first. Never empty on success.
fn camera_candidates(
    device: Option<u32>,
) -> Result<Vec<(CaptureConfig, CaptureFormat)>, CameraSetupError> {
    let devices = oximedia_capture::enumerate().map_err(|_| CameraSetupError::Unavailable)?;
    let index = device.map_or(0, |index| index as usize);
    let device = devices.get(index).ok_or(CameraSetupError::Unavailable)?;
    Ok(camera_candidate_formats(&device.formats)?
        .into_iter()
        .map(|format| (camera_config(&device.id, format), format))
        .collect())
}

fn camera_format_matches(selected: CaptureFormat, negotiated: CaptureFormat) -> bool {
    selected.encoding == negotiated.encoding
        && selected.width == negotiated.width
        && selected.height == negotiated.height
        && selected.cmp_fps(negotiated).is_eq()
}

/// How an attempt at one capture mode ended.
enum CameraAttempt {
    /// The mode produced no frame. Another mode may still work; the message is
    /// what to report if none does.
    NoFrames(&'static str),
    /// The attempt is over and anything worth showing has been reported.
    Done,
    /// The mode delivered frames and then failed. No other mode can fix that,
    /// so the message is reported and the worker exits to the engine's retry.
    Fatal(&'static str),
}

fn capture_camera(
    slot: u8,
    device: Option<u32>,
    cancel: Arc<AtomicBool>,
    policy: Arc<InputPolicyState>,
    lease: Option<HydraInputLease>,
) {
    // A source worker retires only its own renderer generation on exit. The
    // Settings-preview worker deliberately has no lease at all.
    let _clear_lease = CameraLeaseClear(lease.clone());
    let active =
        || !cancel.load(Ordering::Acquire) && policy.webcam_allowed.load(Ordering::Acquire);
    let fail = |message| {
        policy.commit_camera_error(usize::from(slot), &cancel, message);
    };
    if !active() {
        return;
    }
    let candidates = match camera_candidates(device) {
        Ok(candidates) => candidates,
        Err(error) => {
            fail(error.message());
            return;
        }
    };
    let last = candidates.len().saturating_sub(1);
    for (attempt, (config, selected_format)) in candidates.into_iter().enumerate() {
        if !active() {
            return;
        }
        match capture_camera_mode(
            slot,
            config,
            selected_format,
            attempt == 0,
            &cancel,
            &policy,
            &lease,
        ) {
            CameraAttempt::Done => return,
            CameraAttempt::Fatal(message) => {
                fail(message);
                return;
            }
            // Only the last candidate reports: an earlier one going silent is
            // how a mode the device does not honour looks from here, and the
            // status would otherwise flicker through Error on the way to a
            // camera that is about to work.
            CameraAttempt::NoFrames(message) => {
                if attempt == last {
                    fail(message);
                    return;
                }
            }
        }
    }
}

/// Run one capture mode until it fails, is revoked, or the worker is replaced.
fn capture_camera_mode(
    slot: u8,
    config: CaptureConfig,
    selected_format: CaptureFormat,
    first: bool,
    cancel: &Arc<AtomicBool>,
    policy: &Arc<InputPolicyState>,
    lease: &Option<HydraInputLease>,
) -> CameraAttempt {
    let active =
        || !cancel.load(Ordering::Acquire) && policy.webcam_allowed.load(Ordering::Acquire);
    // Every failure before the first published frame leaves the next mode
    // worth trying; after one, the mode is proven and the failure is its own.
    let ended = |published: bool, message: &'static str| {
        if published {
            CameraAttempt::Fatal(message)
        } else {
            CameraAttempt::NoFrames(message)
        }
    };
    let mut session = match oximedia_capture::open(config) {
        Ok(session) => session,
        Err(_error) => return CameraAttempt::NoFrames(webcam_error_guidance()),
    };
    // Platform open is a monolithic API and cannot be interrupted from this
    // thread. Re-check immediately after it returns so revocation, Stop or a
    // replacement that happened meanwhile closes the session without taking
    // a stream or announcing a stale camera.
    if !active() {
        return CameraAttempt::Done;
    }
    if !camera_format_matches(selected_format, session.negotiated_format()) {
        return CameraAttempt::NoFrames(CameraSetupError::NegotiatedFormatChanged.message());
    }
    // Only the first mode can still be waiting on a permission answer. Once a
    // session has opened and run its course, a later mode that stays silent is
    // a mode the device does not honour, not a prompt nobody has answered, so
    // it is given the ordinary allowance instead of the macOS TCC one.
    let first_frame_timeout = if first {
        camera_first_frame_timeout(session.backend())
    } else {
        CAMERA_FIRST_FRAME_TIMEOUT
    };
    let Some(mut stream) = session.take_stream() else {
        return CameraAttempt::NoFrames("camera opened without a frame stream; retrying shortly");
    };
    if !active() {
        return CameraAttempt::Done;
    }
    let first_frame_deadline = Instant::now() + first_frame_timeout;
    let mut published_frame = false;

    while active() {
        let frame = match stream.recv_timeout(CAMERA_POLL) {
            Ok(Some(frame)) => frame,
            Ok(None) if stream.is_ended() => {
                return ended(
                    published_frame,
                    "webcam frame stream ended; retrying shortly",
                );
            }
            Ok(None) => {
                if !published_frame && Instant::now() >= first_frame_deadline {
                    return CameraAttempt::NoFrames(
                        "webcam opened but delivered no frames; retrying shortly",
                    );
                }
                continue;
            }
            Err(_error) => {
                return ended(published_frame, "webcam capture stopped; retrying shortly");
            }
        };
        let (width, height, mut rgba) = match camera_frame_rgba(&frame) {
            Ok(converted) => converted,
            Err(_message) => {
                return ended(
                    published_frame,
                    "webcam returned an unsupported frame; retrying shortly",
                );
            }
        };
        if !active() {
            return CameraAttempt::Done;
        }
        // Before either consumer, so `s0` and the Settings thumbnail agree
        // about which way round the picture is.
        mirror_camera_frame(width, height, &mut rgba);
        let preview = camera_preview(width, height, &rgba);
        // Only built when a theme has asked for one: it is the cost of the
        // whole feature, and nobody else pays it.
        let picture =
            (policy.native_camera_requests.load(Ordering::Relaxed) & NATIVE_REQUEST_THEME != 0)
                .then(|| camera_picture(width, height, &rgba))
                .flatten();
        let publication = match lease {
            Some(lease) => lease.publish(width, height, rgba),
            None => Ok(true),
        };
        match publication {
            Ok(true) => {
                if !policy.commit_camera_frame(
                    usize::from(slot),
                    cancel,
                    preview,
                    picture,
                    !published_frame,
                ) {
                    return CameraAttempt::Done;
                }
                published_frame = true;
            }
            Ok(false) => return CameraAttempt::Done,
            Err(_message) => {
                return ended(
                    published_frame,
                    "webcam frame was refused by Hydra; retrying shortly",
                );
            }
        }
    }
    CameraAttempt::Done
}

/// Mirror a camera frame left to right, in place.
///
/// A camera pointed at the person using it is a mirror: raising your left hand
/// has to raise the hand on the left of the picture, or nothing done in front
/// of it lines up with what is seen. Every self-view does this, and a Hydra
/// sketch is watched the same way.
///
/// Only the camera. An image fetched from the web and the terminal's own grid
/// are pictures of something else and are left as they are.
fn mirror_camera_frame(width: u32, height: u32, rgba: &mut [u8]) {
    let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
        return;
    };
    if width < 2 || width.checked_mul(height).and_then(|p| p.checked_mul(4)) != Some(rgba.len()) {
        return;
    }
    for row in rgba.chunks_exact_mut(width * 4) {
        for near in 0..width / 2 {
            let far = width - 1 - near;
            let (head, tail) = row.split_at_mut(far * 4);
            head[near * 4..near * 4 + 4].swap_with_slice(&mut tail[..4]);
        }
    }
}

/// The camera at picture size: cover-cropped to the picture's own shape so
/// a wide terminal is filled rather than letterboxed, then reduced to one
/// luminance byte a pixel. It runs after the mirror, like the thumbnail, so
/// the picture is a mirror too.
fn camera_picture(width: u32, height: u32, rgba: &[u8]) -> Option<HydraCameraPicture> {
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if width == 0 || height == 0 || rgba.len() != expected {
        return None;
    }
    // Cover: the surplus of whichever axis has one is cropped away evenly.
    let scale = (width as f32 / CAMERA_PICTURE_WIDTH as f32)
        .min(height as f32 / CAMERA_PICTURE_HEIGHT as f32);
    let left = (width as f32 - CAMERA_PICTURE_WIDTH as f32 * scale) / 2.0;
    let top = (height as f32 - CAMERA_PICTURE_HEIGHT as f32 * scale) / 2.0;
    let mut luma = Vec::with_capacity(CAMERA_PICTURE_WIDTH * CAMERA_PICTURE_HEIGHT);
    for y in 0..CAMERA_PICTURE_HEIGHT {
        let source_y = ((top + y as f32 * scale) as usize).min(height - 1);
        for x in 0..CAMERA_PICTURE_WIDTH {
            let source_x = ((left + x as f32 * scale) as usize).min(width - 1);
            let at = (source_y * width + source_x) * 4;
            luma.push(
                ((2126 * u32::from(rgba[at])
                    + 7152 * u32::from(rgba[at + 1])
                    + 722 * u32::from(rgba[at + 2]))
                    / 10_000)
                    .min(255) as u8,
            );
        }
    }
    Some(HydraCameraPicture {
        width: CAMERA_PICTURE_WIDTH as u16,
        height: CAMERA_PICTURE_HEIGHT as u16,
        luma,
    })
}

fn camera_preview(width: u32, height: u32, rgba: &[u8]) -> Option<HydraWebcamPreview> {
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if width == 0 || height == 0 || rgba.len() != expected {
        return None;
    }
    let side = width.min(height);
    let left = (width - side) / 2;
    let top = (height - side) / 2;
    let mut rgb = Vec::with_capacity(CAMERA_PREVIEW_EDGE * CAMERA_PREVIEW_EDGE * 3);
    for y in 0..CAMERA_PREVIEW_EDGE {
        let source_y = top + y * side / CAMERA_PREVIEW_EDGE;
        for x in 0..CAMERA_PREVIEW_EDGE {
            let source_x = left + x * side / CAMERA_PREVIEW_EDGE;
            let at = (source_y * width + source_x) * 4;
            rgb.extend_from_slice(&rgba[at..at + 3]);
        }
    }
    Some(HydraWebcamPreview {
        width: CAMERA_PREVIEW_EDGE as u8,
        height: CAMERA_PREVIEW_EDGE as u8,
        rgb,
    })
}

fn decode_image(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("Hydra image format could not be read: {error}"))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_EDGE);
    limits.max_image_height = Some(MAX_IMAGE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| format!("Hydra image could not be decoded: {error}"))?;
    let (width, height) = (image.width(), image.height());
    checked_rgba_len(width, height, "Hydra image")?;
    let rgba = image.into_rgba8();
    Ok((width, height, rgba.into_raw()))
}

fn camera_frame_rgba(
    frame: &oximedia_capture::CaptureFrame,
) -> Result<(u32, u32, Vec<u8>), String> {
    if let Some(bytes) = frame.compressed() {
        return decode_image(bytes);
    }
    let video = frame
        .video()
        .ok_or_else(|| "webcam returned neither raw nor compressed pixels".to_owned())?;
    raw_frame_rgba(video)
}

fn raw_frame_rgba(video: &VideoFrame) -> Result<(u32, u32, Vec<u8>), String> {
    let (width, height) = (video.width, video.height);
    let bytes = checked_rgba_len(width, height, "webcam frame")?;
    let mut rgba = vec![0u8; bytes];
    let full_range = video.color_info.full_range;
    let matrix = yuv_matrix(video.color_info.matrix);

    match video.format {
        PixelFormat::Rgb24 => copy_rgb(video, &mut rgba, false)?,
        PixelFormat::Rgba32 => copy_rgb(video, &mut rgba, true)?,
        PixelFormat::Gray8 => {
            let plane = plane(video, 0)?;
            for y in 0..height as usize {
                let row = row_at(plane, y, width as usize)?;
                for (x, value) in row[..width as usize].iter().enumerate() {
                    put_rgba(&mut rgba, width, x, y, [*value, *value, *value]);
                }
            }
        }
        PixelFormat::Nv12 | PixelFormat::Nv21 => {
            if width % 2 != 0 {
                return Err("webcam returned odd-width NV12/NV21 pixels".into());
            }
            let y_plane = plane(video, 0)?;
            let uv_plane = plane(video, 1)?;
            for y in 0..height as usize {
                let ys = row_at(y_plane, y, width as usize)?;
                let uv = row_at(uv_plane, y / 2, width as usize)?;
                for (x, &luma) in ys[..width as usize].iter().enumerate() {
                    let at = (x / 2) * 2;
                    let (u, v) = if video.format == PixelFormat::Nv12 {
                        (uv[at], uv[at + 1])
                    } else {
                        (uv[at + 1], uv[at])
                    };
                    put_rgba(
                        &mut rgba,
                        width,
                        x,
                        y,
                        yuv_to_rgb(luma, u, v, full_range, matrix),
                    );
                }
            }
        }
        PixelFormat::Yuv420p | PixelFormat::Yuv422p | PixelFormat::Yuv444p => {
            let y_plane = plane(video, 0)?;
            let u_plane = plane(video, 1)?;
            let v_plane = plane(video, 2)?;
            let (x_div, y_div) = match video.format {
                PixelFormat::Yuv420p => (2, 2),
                PixelFormat::Yuv422p => (2, 1),
                _ => (1, 1),
            };
            for y in 0..height as usize {
                let ys = row_at(y_plane, y, width as usize)?;
                let chroma_width = (width as usize).div_ceil(x_div);
                let us = row_at(u_plane, y / y_div, chroma_width)?;
                let vs = row_at(v_plane, y / y_div, chroma_width)?;
                for x in 0..width as usize {
                    put_rgba(
                        &mut rgba,
                        width,
                        x,
                        y,
                        yuv_to_rgb(ys[x], us[x / x_div], vs[x / x_div], full_range, matrix),
                    );
                }
            }
        }
        PixelFormat::Yuyv422 | PixelFormat::Uyvy422 => {
            if width % 2 != 0 {
                return Err("webcam returned odd-width packed 4:2:2 pixels".into());
            }
            let plane = plane(video, 0)?;
            for y in 0..height as usize {
                let row = row_at(plane, y, width as usize * 2)?;
                for pair in 0..width as usize / 2 {
                    let at = pair * 4;
                    let (y0, u, y1, v) = if video.format == PixelFormat::Yuyv422 {
                        (row[at], row[at + 1], row[at + 2], row[at + 3])
                    } else {
                        (row[at + 1], row[at], row[at + 3], row[at + 2])
                    };
                    put_rgba(
                        &mut rgba,
                        width,
                        pair * 2,
                        y,
                        yuv_to_rgb(y0, u, v, full_range, matrix),
                    );
                    put_rgba(
                        &mut rgba,
                        width,
                        pair * 2 + 1,
                        y,
                        yuv_to_rgb(y1, u, v, full_range, matrix),
                    );
                }
            }
        }
        unsupported => {
            return Err(format!(
                "webcam negotiated unsupported raw pixel format {unsupported:?}"
            ));
        }
    }
    Ok((width, height, rgba))
}

fn checked_rgba_len(width: u32, height: u32, kind: &str) -> Result<usize, String> {
    if width == 0 || height == 0 || width > MAX_IMAGE_EDGE || height > MAX_IMAGE_EDGE {
        return Err(format!(
            "{kind} {width}x{height} is outside 1..={MAX_IMAGE_EDGE}"
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("{kind} dimensions overflow host memory"))?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(format!("{kind} exceeds the Hydra pixel limit"));
    }
    let bytes = pixels
        .checked_mul(4)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| format!("{kind} dimensions overflow host memory"))?;
    if bytes as u64 > MAX_DECODE_ALLOC {
        return Err(format!("{kind} exceeds the Hydra decode allocation limit"));
    }
    Ok(bytes)
}

fn plane(video: &VideoFrame, index: usize) -> Result<&Plane, String> {
    video
        .planes
        .get(index)
        .ok_or_else(|| format!("webcam frame is missing plane {index}"))
}

fn row_at(plane: &Plane, y: usize, needed: usize) -> Result<&[u8], String> {
    let row = plane.row(y);
    if row.len() < needed {
        return Err(format!(
            "webcam plane row {y} has {} bytes; {needed} are required",
            row.len()
        ));
    }
    Ok(row)
}

fn copy_rgb(video: &VideoFrame, rgba: &mut [u8], alpha: bool) -> Result<(), String> {
    let plane = plane(video, 0)?;
    let channels = if alpha { 4 } else { 3 };
    for y in 0..video.height as usize {
        let row = row_at(plane, y, video.width as usize * channels)?;
        for x in 0..video.width as usize {
            let at = x * channels;
            let out = ((y * video.width as usize) + x) * 4;
            rgba[out..out + 3].copy_from_slice(&row[at..at + 3]);
            rgba[out + 3] = if alpha { row[at + 3] } else { 255 };
        }
    }
    Ok(())
}

fn put_rgba(rgba: &mut [u8], width: u32, x: usize, y: usize, rgb: [u8; 3]) {
    let at = (y * width as usize + x) * 4;
    rgba[at..at + 3].copy_from_slice(&rgb);
    rgba[at + 3] = 255;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum YuvMatrix {
    Bt601,
    Bt709,
}

fn yuv_matrix(matrix: MatrixCoefficients) -> YuvMatrix {
    match matrix {
        // These are the two standard-definition matrices whose coefficients
        // match BT.601 for 8-bit capture closely enough to share this path.
        MatrixCoefficients::Fcc | MatrixCoefficients::Bt470Bg | MatrixCoefficients::Smpte170M => {
            YuvMatrix::Bt601
        }
        // Capture requests 1280x720. OxiMedia's default metadata is BT.709,
        // and absent/unspecified or uncommon matrices therefore take the HD
        // fallback rather than silently applying SD coefficients.
        _ => YuvMatrix::Bt709,
    }
}

fn yuv_to_rgb(y: u8, u: u8, v: u8, full_range: bool, matrix: YuvMatrix) -> [u8; 3] {
    let (y, u, v) = (i32::from(y), i32::from(u) - 128, i32::from(v) - 128);
    let (r, g, b) = match (matrix, full_range) {
        (YuvMatrix::Bt601, true) => (
            y + ((359 * v) >> 8),
            y - ((88 * u + 183 * v) >> 8),
            y + ((454 * u) >> 8),
        ),
        (YuvMatrix::Bt709, true) => (
            y + ((403 * v) >> 8),
            y - ((48 * u + 120 * v) >> 8),
            y + ((475 * u) >> 8),
        ),
        (YuvMatrix::Bt601, false) => {
            let y = (y - 16).max(0);
            (
                (298 * y + 409 * v + 128) >> 8,
                (298 * y - 100 * u - 208 * v + 128) >> 8,
                (298 * y + 516 * u + 128) >> 8,
            )
        }
        (YuvMatrix::Bt709, false) => {
            let y = (y - 16).max(0);
            (
                (298 * y + 459 * v + 128) >> 8,
                (298 * y - 55 * u - 136 * v + 128) >> 8,
                (298 * y + 541 * u + 128) >> 8,
            )
        }
    };
    [clamp_u8(r), clamp_u8(g), clamp_u8(b)]
}

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    fn frame(format: PixelFormat, width: u32, height: u32, planes: Vec<Plane>) -> VideoFrame {
        let mut frame = VideoFrame::new(format, width, height);
        frame.planes = planes;
        frame
    }

    #[test]
    fn rgb_and_padded_stride_become_tightly_packed_rgba() {
        let video = frame(
            PixelFormat::Rgb24,
            2,
            1,
            vec![Plane::with_dimensions(
                vec![1, 2, 3, 4, 5, 6, 99, 99],
                8,
                2,
                1,
            )],
        );
        assert_eq!(
            raw_frame_rgba(&video).unwrap().2,
            vec![1, 2, 3, 255, 4, 5, 6, 255]
        );
    }

    #[test]
    fn yuyv_neutral_black_and_white_convert_with_shared_chroma() {
        let video = frame(
            PixelFormat::Yuyv422,
            2,
            1,
            vec![Plane::with_dimensions(vec![16, 128, 235, 128], 4, 2, 1)],
        );
        assert_eq!(
            raw_frame_rgba(&video).unwrap().2,
            vec![0, 0, 0, 255, 255, 255, 255, 255]
        );
    }

    #[test]
    fn nv12_uses_the_chroma_row_for_each_two_by_two_block() {
        let video = frame(
            PixelFormat::Nv12,
            2,
            2,
            vec![
                Plane::with_dimensions(vec![16, 16, 235, 235], 2, 2, 2),
                Plane::with_dimensions(vec![128, 128], 2, 1, 1),
            ],
        );
        assert_eq!(
            raw_frame_rgba(&video).unwrap().2,
            vec![
                0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ]
        );
    }

    #[test]
    fn webcam_policy_defaults_blocked_and_revocation_invalidates_a_lease() {
        let host = rustel_hydra::HydraHost::new();
        let sink = host.input_sink();
        let inputs = HydraInputs::new(sink.clone());
        assert!(!inputs.policy().webcam_allowed());
        inputs.policy.state.camera_slots.store(1, Ordering::Release);
        let cancel = Arc::new(AtomicBool::new(false));
        let lease = sink.bind(0).unwrap();
        inputs
            .policy
            .state
            .register_camera(0, Arc::clone(&cancel), Some(lease.clone()));
        assert!(lease.publish(1, 1, vec![1, 2, 3, 4]).unwrap());
        inputs.policy().set_webcam_allowed(true);
        inputs.policy().set_webcam_allowed(false);
        assert!(cancel.load(Ordering::Acquire));
        assert!(!lease.publish(1, 1, vec![5, 6, 7, 8]).unwrap());
    }

    #[test]
    fn camera_revocation_is_sticky_and_replacement_waits_for_worker_exit() {
        let host = rustel_hydra::HydraHost::new();
        let sink = host.input_sink();
        let mut inputs = HydraInputs::new(sink.clone());
        let old_cancel = Arc::new(AtomicBool::new(false));
        let old_done = Arc::new(AtomicBool::new(false));
        let old_lease = sink.bind(0).expect("old camera lease");
        inputs
            .policy
            .state
            .register_camera(0, Arc::clone(&old_cancel), Some(old_lease.clone()));
        inputs.cameras[0] = Some(CameraWorker {
            request: CameraRequest {
                purpose: CameraPurpose::Score,
                device: None,
            },
            cancel: Arc::clone(&old_cancel),
            done: Arc::clone(&old_done),
            lease: Some(old_lease),
        });

        let mut plan = std::array::from_fn(|_| None);
        plan[0] = Some(HydraSource::Camera { device: Some(1) });
        inputs.configure(plan);
        assert!(old_cancel.load(Ordering::Acquire));

        // Re-allowing cannot clear a worker's cancellation, and drawing does
        // not create an overlapping platform open while that worker remains.
        inputs.policy().set_webcam_allowed(true);
        inputs.set_drawing(true);
        assert!(Arc::ptr_eq(
            &inputs.cameras[0].as_ref().unwrap().cancel,
            &old_cancel
        ));
        assert!(old_cancel.load(Ordering::Acquire));

        inputs.set_drawing(false);
        old_done.store(true, Ordering::Release);
        inputs.reconcile_cameras();
        assert!(inputs.cameras[0].is_none());
    }

    #[test]
    fn webcam_status_is_sanitized_persistent_and_log_errors_are_deduplicated() {
        let host = rustel_hydra::HydraHost::new();
        let inputs = HydraInputs::new(host.input_sink());
        let policy = inputs.policy();
        assert_eq!(policy.webcam_status().state, HydraWebcamState::Blocked);
        assert!(!policy.webcam_status().requested);

        policy.set_webcam_allowed(true);
        assert_eq!(policy.webcam_status().state, HydraWebcamState::Allowed);
        policy.state.set_requested_camera_slots(1);
        assert_eq!(policy.webcam_status().state, HydraWebcamState::Requested);
        policy.state.set_camera_status(0, CameraSlotStatus::Opening);
        assert_eq!(policy.webcam_status().state, HydraWebcamState::Opening);
        policy.state.set_camera_status(0, CameraSlotStatus::Ready);
        policy.state.set_camera_preview(
            0,
            HydraWebcamPreview {
                width: 1,
                height: 1,
                rgb: vec![1, 2, 3],
            },
        );
        assert!(policy.webcam_status().preview.is_some());
        policy.state.reset_inactive_camera_statuses(0);
        let waiting = policy.webcam_status();
        assert_eq!(waiting.state, HydraWebcamState::Requested);
        assert!(
            waiting.preview.is_none(),
            "an inactive request cannot retain the previous camera image"
        );

        policy.state.set_camera_status(0, CameraSlotStatus::Ready);
        policy.state.set_camera_preview(
            0,
            HydraWebcamPreview {
                width: 1,
                height: 1,
                rgb: vec![1, 2, 3],
            },
        );
        policy.set_webcam_allowed(false);
        assert!(
            policy.webcam_status().preview.is_none(),
            "revocation clears the cached portrait immediately"
        );
        policy.set_webcam_allowed(true);

        policy.state.set_camera_status(0, CameraSlotStatus::Error);
        let failed = policy.webcam_status();
        assert_eq!(failed.state, HydraWebcamState::Error);
        assert!(
            failed.preview.is_none(),
            "a failed camera cannot leave a portrait"
        );
        assert!(failed.detail.len() < 200, "status text stays bounded");
        if cfg!(target_os = "macos") {
            assert!(failed.detail.contains("Privacy & Security"));
        } else if cfg!(target_os = "windows") {
            assert!(failed.detail.contains("Privacy & security"));
        } else if cfg!(target_os = "linux") {
            assert!(failed.detail.contains("/dev/video"));
        } else {
            assert!(failed.detail.contains("camera permission"));
        }
        assert!(!failed.detail.contains("backend-secret"));

        policy.state.report_camera_error(0, "camera could not open");
        policy.state.report_camera_error(0, "camera could not open");
        assert_eq!(inputs.take_events().len(), 1, "identical retries log once");
        policy.state.reset_camera_reports(0);
        policy.state.report_camera_error(0, "camera could not open");
        assert_eq!(
            inputs.take_events().len(),
            1,
            "a new request may report again"
        );
    }

    #[test]
    fn first_frame_allowance_covers_macos_tcc_without_weakening_other_backends() {
        assert_eq!(
            camera_first_frame_timeout(BackendKind::AvFoundation),
            Duration::from_secs(65),
            "AVFoundation may spend the dependency's full 60 seconds waiting for TCC"
        );
        for backend in [
            BackendKind::V4l2,
            BackendKind::MediaFoundation,
            BackendKind::Mock,
            BackendKind::Unsupported,
        ] {
            assert_eq!(
                camera_first_frame_timeout(backend),
                Duration::from_secs(5),
                "{backend} has no asynchronous macOS authorization wait"
            );
        }
        assert!(MACOS_CAMERA_FIRST_FRAME_TIMEOUT >= Duration::from_secs(60) + CAMERA_POLL);
        assert!(CAMERA_FIRST_FRAME_TIMEOUT > CAMERA_POLL);
    }

    #[test]
    fn camera_mode_selection_filters_oversized_preferred_raw_before_negotiating() {
        let oversized_raw =
            CaptureFormat::new(CaptureEncoding::Raw(PixelFormat::Nv12), 3840, 2160, 30, 1);
        let safe_mjpeg = CaptureFormat::new(CaptureEncoding::Mjpeg, 1280, 720, 30, 1);
        assert_eq!(
            select_camera_format(&[oversized_raw, safe_mjpeg]),
            Some(safe_mjpeg),
            "encoding preference cannot resurrect a mode beyond the decoder/frame cap"
        );
        assert!(!supported_camera_format(oversized_raw));
        assert!(supported_camera_format(safe_mjpeg));

        let equivalent_rate = CaptureFormat::new(CaptureEncoding::Mjpeg, 1280, 720, 60_000, 2002);
        assert!(camera_format_matches(
            CaptureFormat::new(CaptureEncoding::Mjpeg, 1280, 720, 30_000, 1001),
            equivalent_rate,
        ));

        assert_eq!(
            camera_inventory_format(&[]),
            Err(CameraSetupError::Unavailable),
            "an empty Windows privacy-filtered inventory needs permission guidance"
        );
        let guidance = CameraSetupError::Unavailable.message();
        assert!(guidance.starts_with("Camera could not open."), "{guidance}");
        assert!(
            guidance.contains(if cfg!(target_os = "macos") {
                "Privacy & Security"
            } else if cfg!(target_os = "windows") {
                "Privacy & security"
            } else {
                "camera permission"
            }),
            "the guidance must name this platform's own permission setting: {guidance}"
        );
    }

    #[test]
    fn a_camera_frame_is_mirrored_left_to_right_and_nothing_else_is() {
        // Two rows, three pixels each, every pixel its own colour so a
        // vertical flip or a row swap cannot pass for a horizontal one.
        let mut frame = vec![
            1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, //
            10, 11, 12, 255, 13, 14, 15, 255, 16, 17, 18, 255,
        ];
        mirror_camera_frame(3, 2, &mut frame);
        assert_eq!(
            frame,
            vec![
                7, 8, 9, 255, 4, 5, 6, 255, 1, 2, 3, 255, //
                16, 17, 18, 255, 13, 14, 15, 255, 10, 11, 12, 255,
            ],
            "each row reverses on its own; the middle pixel and the row order stay put"
        );

        let mut twice = frame.clone();
        mirror_camera_frame(3, 2, &mut twice);
        mirror_camera_frame(3, 2, &mut twice);
        assert_eq!(twice, frame, "mirroring is its own inverse");

        // A frame that lies about its size is left alone rather than
        // scrambled: publishing a half-swapped picture is worse than a
        // picture facing the wrong way.
        let mut short = vec![0u8; 3 * 2 * 4 - 1];
        let untouched = short.clone();
        mirror_camera_frame(3, 2, &mut short);
        assert_eq!(
            short, untouched,
            "a short buffer is not reversed past its end"
        );
        let mut single_column = vec![1, 2, 3, 255];
        mirror_camera_frame(1, 1, &mut single_column);
        assert_eq!(
            single_column,
            vec![1, 2, 3, 255],
            "one column has no far side"
        );
    }

    #[test]
    fn candidate_modes_lead_with_the_one_a_session_preset_will_actually_deliver() {
        // The FaceTime HD camera's inventory, in AVFoundation's order.
        // `AVCaptureSessionPresetHigh` delivers 1920x1080 whatever
        // `activeFormat` is, so a negotiated 1280x720 yields dropped frames.
        let mode = |width, height, fps| {
            CaptureFormat::new(
                CaptureEncoding::Raw(PixelFormat::Nv12),
                width,
                height,
                fps,
                1,
            )
        };
        let inventory = [
            mode(1920, 1080, 30),
            mode(1920, 1080, 15),
            mode(1280, 720, 30),
            mode(1280, 720, 15),
            mode(1080, 1920, 30),
            mode(1760, 1328, 30),
            mode(640, 480, 30),
            mode(1328, 1760, 30),
            mode(1552, 1552, 30),
        ];
        let candidates = camera_candidate_formats(&inventory).expect("safe modes");
        assert!(
            candidates.len() > 1,
            "one mode leaves nothing to fall back to"
        );
        assert!(candidates.len() <= CAMERA_FORMAT_CANDIDATES);
        assert_eq!(
            preset_default_format(&inventory),
            Some(mode(1920, 1080, 30)),
            "the widest landscape mode stands in for the preset's own choice"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                candidates.first().copied(),
                Some(mode(1920, 1080, 30)),
                "the mode the preset delivers must carry the long first attempt"
            );
        }
        assert!(
            candidates
                .iter()
                .any(|format| camera_format_matches(*format, mode(1280, 720, 30))),
            "the negotiated mode stays a candidate on every platform"
        );
        for (index, format) in candidates.iter().enumerate() {
            assert!(
                supported_camera_format(*format),
                "candidate {index} is outside the frame safety limits"
            );
            assert_eq!(
                candidates
                    .iter()
                    .filter(|held| camera_format_matches(**held, *format))
                    .count(),
                1,
                "candidate {index} repeats a mode already tried"
            );
        }

        assert_eq!(
            camera_candidate_formats(&[]),
            Err(CameraSetupError::Unavailable),
            "an empty inventory is a permission failure, not a mode to fall back from"
        );
        assert_eq!(
            camera_candidate_formats(&[mode(3840, 2160, 30)]),
            Err(CameraSetupError::NoSafeFormat),
            "no candidate may come from outside the safety limits"
        );
        assert_eq!(
            preset_default_format(&[mode(1552, 1552, 30), mode(1080, 1920, 30)]),
            None,
            "a device with no landscape mode has nothing to guess with"
        );
    }

    #[test]
    fn revoked_registration_cannot_publish_ready_state_after_rapid_reenable() {
        let host = rustel_hydra::HydraHost::new();
        let sink = host.input_sink();
        let inputs = HydraInputs::new(sink.clone());
        let policy = inputs.policy();
        policy.set_webcam_allowed(true);
        policy.state.set_requested_camera_slots(1);
        policy.state.set_camera_status(0, CameraSlotStatus::Opening);

        let cancel = Arc::new(AtomicBool::new(false));
        let lease = sink.bind(0).expect("camera lease");
        policy
            .state
            .register_camera(0, Arc::clone(&cancel), Some(lease.clone()));
        assert!(lease.publish(1, 1, vec![1, 2, 3, 255]).unwrap());

        // Model revocation in the exact gap between pixel publication and the
        // worker's Ready/preview commit, followed by an immediate re-enable.
        policy.set_webcam_allowed(false);
        policy.set_webcam_allowed(true);
        let revocation_events = inputs.take_events();
        assert_eq!(revocation_events.len(), 1, "revocation is reported once");
        assert!(!policy.state.commit_camera_frame(
            0,
            &cancel,
            Some(HydraWebcamPreview {
                width: 1,
                height: 1,
                rgb: vec![1, 2, 3],
            }),
            None,
            true,
        ));
        let status = policy.webcam_status();
        assert_eq!(status.state, HydraWebcamState::Requested);
        assert!(status.preview.is_none());

        assert!(!policy.state.commit_camera_opening(0, &cancel));
        assert!(!policy.state.commit_camera_error(
            0,
            &cancel,
            "stale worker must not overwrite the new request",
        ));
        assert_eq!(policy.webcam_status().state, HydraWebcamState::Requested);
        assert!(inputs.take_events().is_empty());
    }

    #[test]
    fn settings_preview_is_live_dedicated_and_cancelled_without_touching_sources() {
        let host = rustel_hydra::HydraHost::new();
        let sink = host.input_sink();
        let mut inputs = HydraInputs::new(sink.clone());
        let policy = inputs.policy();
        policy.set_webcam_allowed(true);

        // Merely selecting the enabled row is a live request, but cannot bind
        // or invalidate any Hydra texture.
        let image = sink.bind(0).expect("existing s0 image");
        assert!(image.publish(1, 1, vec![1, 2, 3, 255]).unwrap());
        policy.set_settings_preview_requested(true);
        assert_eq!(
            inputs.desired_camera(SETTINGS_PREVIEW_SLOT),
            Some(CameraRequest {
                purpose: CameraPurpose::SettingsPreview,
                device: None,
            })
        );
        let requested = policy.webcam_status();
        assert_eq!(requested.state, HydraWebcamState::Requested);
        assert!(requested.detail.contains("Settings live preview"));
        assert!(
            image.publish(1, 1, vec![4, 5, 6, 255]).unwrap(),
            "the preview request never rebinds s0"
        );

        // Model the dedicated worker reaching its first frame. Its
        // registration contains no renderer lease and only retains the tiny
        // Settings thumbnail.
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        policy
            .state
            .register_camera(SETTINGS_PREVIEW_SLOT, Arc::clone(&cancel), None);
        assert!(
            policy
                .state
                .commit_camera_opening(SETTINGS_PREVIEW_SLOT, &cancel)
        );
        assert!(
            policy
                .webcam_status()
                .detail
                .contains("Settings live preview")
        );
        inputs.cameras[SETTINGS_PREVIEW_SLOT] = Some(CameraWorker {
            request: CameraRequest {
                purpose: CameraPurpose::SettingsPreview,
                device: None,
            },
            cancel: Arc::clone(&cancel),
            done,
            lease: None,
        });
        assert!(policy.state.commit_camera_frame(
            SETTINGS_PREVIEW_SLOT,
            &cancel,
            Some(HydraWebcamPreview {
                width: 1,
                height: 1,
                rgb: vec![9, 8, 7],
            }),
            None,
            true,
        ));
        assert_eq!(policy.webcam_status().state, HydraWebcamState::Ready);
        assert!(
            policy
                .webcam_status()
                .detail
                .contains("Settings live preview")
        );
        assert!(policy.webcam_status().preview.is_some());

        policy.set_settings_preview_requested(false);
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(policy.webcam_status().state, HydraWebcamState::Allowed);
        assert!(policy.webcam_status().preview.is_none());
        assert!(
            image.publish(1, 1, vec![7, 6, 5, 255]).unwrap(),
            "closing Settings cannot clear an active external image"
        );

        // A visible Hydra camera is the preview source; selecting Settings
        // must not create a second platform session.
        policy.set_settings_preview_requested(true);
        inputs.theme_camera = true;
        assert_eq!(
            inputs.desired_camera(0).map(|request| request.purpose),
            Some(CameraPurpose::Theme)
        );
        assert_eq!(inputs.desired_camera(SETTINGS_PREVIEW_SLOT), None);
    }

    /// Enter on the row grants one preview session while the switch is off. A
    /// camera that fails to open shows the ordinary camera error guidance, and
    /// Esc's revocation stops the failed worker.
    #[test]
    fn a_settings_preview_that_fails_to_open_reports_plainly_and_recovers_cleanly() {
        let host = rustel_hydra::HydraHost::new();
        let policy = HydraInputs::new(host.input_sink()).policy();
        policy.set_webcam_allowed(true);
        policy.set_settings_preview_requested(true);

        let cancel = Arc::new(AtomicBool::new(false));
        policy
            .state
            .register_camera(SETTINGS_PREVIEW_SLOT, Arc::clone(&cancel), None);
        assert!(policy.state.commit_camera_error(
            SETTINGS_PREVIEW_SLOT,
            &cancel,
            "no camera found",
        ));

        let status = policy.webcam_status();
        assert_eq!(status.state, HydraWebcamState::Error);
        assert!(
            status.preview.is_none(),
            "an error never leaves a stale or blank picture up"
        );
        assert_eq!(
            status.detail,
            webcam_error_guidance(),
            "the reader is told plainly what to do, not left to guess at a hang"
        );

        // Esc: the switch never agreed, so backing out of the preview is
        // exactly the revocation an ordinary Esc performs.
        policy.set_webcam_allowed(false);
        policy.set_settings_preview_requested(false);
        assert!(
            cancel.load(Ordering::Acquire),
            "the failed worker is told to stop rather than left running"
        );
        let after = policy.webcam_status();
        assert_eq!(after.state, HydraWebcamState::Blocked);
        assert!(!after.requested, "nothing is left asking for the camera");
    }

    #[test]
    fn score_re_evaluation_preserves_settings_preview_retry_and_dedup_state() {
        let host = rustel_hydra::HydraHost::new();
        let mut inputs = HydraInputs::new(host.input_sink());
        let retry = Instant::now() + CAMERA_RETRY;
        inputs.camera_retry_at[SETTINGS_PREVIEW_SLOT] = Some(retry);
        inputs
            .policy
            .state
            .camera_errors_reported
            .store(SETTINGS_PREVIEW_BIT | 1, Ordering::Release);
        inputs
            .policy
            .state
            .camera_ready_reported
            .store(SETTINGS_PREVIEW_BIT | 1, Ordering::Release);

        inputs.configure(std::array::from_fn(|_| None));

        assert_eq!(inputs.camera_retry_at[SETTINGS_PREVIEW_SLOT], Some(retry));
        assert_eq!(
            inputs
                .policy
                .state
                .camera_errors_reported
                .load(Ordering::Acquire),
            SETTINGS_PREVIEW_BIT
        );
        assert_eq!(
            inputs
                .policy
                .state
                .camera_ready_reported
                .load(Ordering::Acquire),
            SETTINGS_PREVIEW_BIT
        );
    }

    #[test]
    fn theme_camera_survives_a_dormant_score_plan_replacement() {
        let host = rustel_hydra::HydraHost::new();
        let sink = host.input_sink();
        let mut inputs = HydraInputs::new(sink.clone());
        inputs.set_theme_camera(true);
        inputs.policy().set_webcam_allowed(true);

        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let lease = sink.bind(0).expect("theme s0");
        inputs
            .policy
            .state
            .register_camera(0, Arc::clone(&cancel), Some(lease.clone()));
        inputs.cameras[0] = Some(CameraWorker {
            request: CameraRequest {
                purpose: CameraPurpose::Theme,
                device: None,
            },
            cancel: Arc::clone(&cancel),
            done,
            lease: Some(lease.clone()),
        });
        assert!(lease.publish(1, 1, vec![1, 2, 3, 255]).unwrap());

        inputs.configure(std::array::from_fn(|_| None));
        assert!(!cancel.load(Ordering::Acquire));
        assert!(Arc::ptr_eq(
            &inputs.cameras[0].as_ref().expect("same worker").cancel,
            &cancel
        ));
        assert!(
            lease.publish(1, 1, vec![4, 5, 6, 255]).unwrap(),
            "score re-evaluation did not invalidate the visible theme lease"
        );
    }

    #[test]
    fn play_retires_theme_camera_before_binding_an_s0_image() {
        let host = rustel_hydra::HydraHost::new();
        let sink = host.input_sink();
        let mut inputs = HydraInputs::new(sink.clone());
        let mut access = ScoreSampleAccess::denied();
        access.permit_origin("https://example.com").unwrap();
        inputs.set_score_sample_access(access.clone());
        inputs.theme_camera = true;
        inputs.policy().set_webcam_allowed(true);

        // Model a platform camera open that has not returned yet. Keeping the
        // image queue worker marked as started lets the test inspect the exact
        // lease submitted by Play without doing network I/O.
        let camera_cancel = Arc::new(AtomicBool::new(false));
        let camera_lease = sink.bind(0).expect("theme camera lease");
        inputs.policy.state.register_camera(
            0,
            Arc::clone(&camera_cancel),
            Some(camera_lease.clone()),
        );
        inputs.cameras[0] = Some(CameraWorker {
            request: CameraRequest {
                purpose: CameraPurpose::Theme,
                device: None,
            },
            cancel: Arc::clone(&camera_cancel),
            done: Arc::new(AtomicBool::new(false)),
            lease: Some(camera_lease),
        });
        let mut plan = std::array::from_fn(|_| None);
        plan[0] = Some(HydraSource::ImageUrl {
            url: "https://example.com/texture.png".into(),
        });
        inputs.configure(plan);
        inputs.images[0].started = true;

        inputs.set_drawing(true);
        assert!(camera_cancel.load(Ordering::Acquire));
        let image = inputs.images[0]
            .state
            .pending
            .lock()
            .unwrap()
            .take()
            .expect("Play queued the image after retiring the camera");
        assert_eq!(
            image.access, access,
            "the worker receives this score's grant"
        );
        assert!(
            image.lease.publish(1, 1, vec![4, 5, 6, 255]).unwrap(),
            "the camera handoff must not clear the new image generation"
        );
    }

    #[test]
    fn revocation_does_not_clear_an_image_that_hides_a_camera_theme() {
        let host = rustel_hydra::HydraHost::new();
        let mut inputs = HydraInputs::new(host.input_sink());
        inputs.theme_camera = true;
        inputs.policy().set_webcam_allowed(true);
        inputs.images[0].started = true;
        inputs.policy.state.drawing.store(true, Ordering::Release);
        let mut plan = std::array::from_fn(|_| None);
        plan[0] = Some(HydraSource::ImageUrl {
            url: "https://example.com/texture.png".into(),
        });
        inputs.configure(plan);
        let image = inputs.images[0]
            .state
            .pending
            .lock()
            .unwrap()
            .take()
            .expect("score image request");

        assert_ne!(
            inputs.policy.state.camera_slots.load(Ordering::Acquire) & 1,
            0,
            "the hidden theme remains a declared request"
        );
        assert!(
            inputs.cameras[0].is_none(),
            "no camera owns s0 while the score image is visible"
        );
        inputs.policy().set_webcam_allowed(false);
        assert!(
            image.lease.publish(1, 1, vec![7, 8, 9, 255]).unwrap(),
            "revoking webcam consent must not invalidate a non-camera source"
        );
    }

    #[test]
    fn score_camera_owns_visibility_then_hands_s0_back_to_the_theme() {
        let host = rustel_hydra::HydraHost::new();
        let mut inputs = HydraInputs::new(host.input_sink());
        inputs.theme_camera = true;
        let mut plan = std::array::from_fn(|_| None);
        plan[0] = Some(HydraSource::Camera { device: Some(2) });
        inputs.configure(plan);

        assert_eq!(
            inputs.desired_camera(0),
            Some(CameraRequest {
                purpose: CameraPurpose::Theme,
                device: None,
            })
        );
        inputs.policy.state.drawing.store(true, Ordering::Release);
        assert_eq!(
            inputs.desired_camera(0),
            Some(CameraRequest {
                purpose: CameraPurpose::Score,
                device: Some(2),
            })
        );
        inputs.policy.state.drawing.store(false, Ordering::Release);
        assert_eq!(
            inputs.desired_camera(0),
            Some(CameraRequest {
                purpose: CameraPurpose::Theme,
                device: None,
            })
        );
    }

    /// The picture a natively drawn camera theme paints from: one fixed
    /// shape, cover-cropped so a wide terminal is filled rather than
    /// letterboxed, and one luminance byte a pixel.
    #[test]
    fn camera_picture_is_a_cover_cropped_luminance_frame() {
        // A 4:3 source: the picture is 16:9, so the crop takes from the
        // top and bottom and keeps the full width.
        let (width, height) = (320u32, 240u32);
        let rgba: Vec<u8> = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    let middle = x == width / 2 && y == height / 2;
                    if middle {
                        [255, 255, 255, 255]
                    } else {
                        [(x % 200) as u8, (y % 200) as u8, 20, 255]
                    }
                })
            })
            .collect();
        let picture = camera_picture(width, height, &rgba).expect("a picture");
        assert_eq!(
            (picture.width, picture.height),
            (CAMERA_PICTURE_WIDTH as u16, CAMERA_PICTURE_HEIGHT as u16)
        );
        assert_eq!(
            picture.luma.len(),
            CAMERA_PICTURE_WIDTH * CAMERA_PICTURE_HEIGHT
        );
        // The middle of the source is the middle of the picture: the crop
        // is centred on both axes.
        let middle = picture.luma
            [CAMERA_PICTURE_HEIGHT / 2 * CAMERA_PICTURE_WIDTH + CAMERA_PICTURE_WIDTH / 2];
        assert!(middle > 240, "the marked centre pixel survives: {middle}");
        assert!(camera_picture(1, 1, &[0, 1, 2]).is_none());
        assert!(camera_picture(0, 0, &[]).is_none());
    }

    /// One camera session, two consumers. Either alone holds it open, and
    /// only letting go of both closes it - or leaving the Settings row
    /// would close the camera a theme is drawing.
    #[test]
    fn the_two_native_camera_consumers_share_one_session_and_cancel_at_zero() {
        let host = rustel_hydra::HydraHost::new();
        let inputs = HydraInputs::new(host.input_sink());
        let policy = inputs.policy();
        policy.set_webcam_allowed(true);

        policy.set_settings_preview_requested(true);
        assert!(policy.webcam_status().requested);
        policy.set_theme_picture_requested(true);
        assert!(policy.webcam_status().requested);

        policy.set_settings_preview_requested(false);
        assert!(
            policy.webcam_status().requested,
            "the theme still wants the camera"
        );
        policy.set_theme_picture_requested(false);
        assert!(!policy.webcam_status().requested, "nobody wants it now");

        // And the other way round.
        policy.set_theme_picture_requested(true);
        policy.set_settings_preview_requested(true);
        policy.set_theme_picture_requested(false);
        assert!(
            policy.webcam_status().requested,
            "the row still wants the camera"
        );
        policy.set_settings_preview_requested(false);
        assert!(!policy.webcam_status().requested);

        // And nothing painted from a camera outlives the permission.
        policy.set_theme_picture_requested(true);
        policy.state.set_camera_picture(HydraCameraPicture {
            width: 2,
            height: 1,
            luma: vec![9, 9],
        });
        assert!(policy.take_camera_picture().is_some());
        policy.state.set_camera_picture(HydraCameraPicture {
            width: 2,
            height: 1,
            luma: vec![9, 9],
        });
        policy.set_webcam_allowed(false);
        assert!(
            policy.take_camera_picture().is_none(),
            "a revoked camera leaves no picture behind"
        );
    }

    #[test]
    fn camera_preview_is_a_bounded_center_crop_with_detail_for_braille() {
        let mut rgba = Vec::new();
        for y in 0..2u8 {
            for x in 0..4u8 {
                rgba.extend_from_slice(&[x, y, 9, 255]);
            }
        }
        let preview = camera_preview(4, 2, &rgba).expect("preview");
        assert_eq!((preview.width, preview.height), (64, 64));
        assert_eq!(preview.rgb.len(), 12 * 1024);
        assert_eq!(
            &preview.rgb[..3],
            &[1, 0, 9],
            "landscape frame is center-cropped"
        );
        assert!(camera_preview(1, 1, &[0, 1, 2]).is_none());
        let rgba: Vec<_> = (0..64_u8)
            .flat_map(|y| (0..64_u8).flat_map(move |x| [x, y, 9, 255]))
            .collect();
        let detailed = camera_preview(64, 64, &rgba).unwrap();
        assert_eq!(&detailed.rgb[(63 * 64 + 63) * 3..], &[63, 63, 9]);
        assert_eq!(
            &detailed.rgb[3..6],
            &[1, 0, 9],
            "adjacent source pixels survive"
        );
    }

    #[test]
    fn image_sources_follow_stop_and_play_with_fresh_generations() {
        let host = rustel_hydra::HydraHost::new();
        let mut inputs = HydraInputs::new(host.input_sink());
        let mut plan = std::array::from_fn(|_| None);
        // A syntactically HTTPS URL that is refused before connecting keeps
        // this lifecycle test deterministic and offline.
        plan[0] = Some(HydraSource::ImageUrl {
            url: "https://127.0.0.1/texture.png".into(),
        });
        inputs.configure(plan);
        let configured = Arc::clone(&inputs.slots[0].as_ref().unwrap().cancel);
        assert!(!inputs.images[0].started, "stopped scores do not fetch");
        assert!(!configured.load(Ordering::Acquire));

        inputs.set_drawing(true);
        let first_play = Arc::clone(&inputs.slots[0].as_ref().unwrap().cancel);
        assert!(inputs.images[0].started);
        assert!(!Arc::ptr_eq(&configured, &first_play));
        assert!(configured.load(Ordering::Acquire));
        let first_lease = inputs.sink.bind(0).unwrap();
        assert!(first_lease.publish(1, 1, vec![1, 2, 3, 255]).unwrap());

        inputs.set_drawing(false);
        assert!(first_play.load(Ordering::Acquire));
        assert!(
            !first_lease.publish(1, 1, vec![4, 5, 6, 255]).unwrap(),
            "Stop invalidates publication before active I/O can finish"
        );
        inputs.set_drawing(true);
        let second_play = Arc::clone(&inputs.slots[0].as_ref().unwrap().cancel);
        assert!(!Arc::ptr_eq(&first_play, &second_play));
        assert!(!second_play.load(Ordering::Acquire));
        inputs.set_drawing(false);
        assert!(second_play.load(Ordering::Acquire));
    }

    #[test]
    fn image_decoder_enforces_dimension_limits() {
        assert!(decode_image(b"not an image").is_err());
        assert!(checked_rgba_len(MAX_IMAGE_EDGE + 1, 1, "test image").is_err());
        assert!(checked_rgba_len(MAX_IMAGE_EDGE, MAX_IMAGE_EDGE, "test image").is_ok());
        assert_eq!(MAX_IMAGE_EDGE, 2048);
        assert_eq!(MAX_IMAGE_PIXELS, 4_194_304);
        assert_eq!(MAX_DECODE_ALLOC, 32 * 1024 * 1024);
        assert_eq!(sample_fetch::MAX_REMOTE_IMAGE_BYTES, 16 * 1024 * 1024);
    }

    #[test]
    fn image_decoder_accepts_a_bounded_rgba_png() {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[7, 8, 9, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert_eq!(decode_image(&png).unwrap(), (1, 1, vec![7, 8, 9, 255]));
    }

    #[test]
    fn yuv_matrix_metadata_selects_chroma_coefficients() {
        assert_eq!(yuv_matrix(MatrixCoefficients::Smpte170M), YuvMatrix::Bt601);
        assert_eq!(yuv_matrix(MatrixCoefficients::Bt470Bg), YuvMatrix::Bt601);
        assert_eq!(yuv_matrix(MatrixCoefficients::Bt709), YuvMatrix::Bt709);
        assert_eq!(
            yuv_matrix(MatrixCoefficients::Unspecified),
            YuvMatrix::Bt709,
            "unspecified capture metadata uses the requested HD default"
        );

        // The non-neutral chroma sample makes a mistaken matrix visibly and
        // numerically different; black/white vectors could not catch it.
        assert_eq!(
            yuv_to_rgb(100, 160, 180, false, YuvMatrix::Bt601),
            [181, 43, 162]
        );
        assert_eq!(
            yuv_to_rgb(100, 160, 180, false, YuvMatrix::Bt709),
            [191, 63, 165]
        );

        let pixels = vec![100, 160, 100, 180];
        let mut bt601 = frame(
            PixelFormat::Yuyv422,
            2,
            1,
            vec![Plane::with_dimensions(pixels.clone(), 4, 2, 1)],
        );
        bt601.color_info.matrix = MatrixCoefficients::Smpte170M;
        let mut bt709 = frame(
            PixelFormat::Yuyv422,
            2,
            1,
            vec![Plane::with_dimensions(pixels, 4, 2, 1)],
        );
        bt709.color_info.matrix = MatrixCoefficients::Bt709;
        assert_eq!(
            raw_frame_rgba(&bt601).unwrap().2,
            vec![181, 43, 162, 255, 181, 43, 162, 255]
        );
        assert_eq!(
            raw_frame_rgba(&bt709).unwrap().2,
            vec![191, 63, 165, 255, 191, 63, 165, 255]
        );
    }
}
