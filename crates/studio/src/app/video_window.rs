//! The detached video windows: the dock's video widget opened onto the
//! desktop, playing the same clip the widget chose, in the same sync.
//!
//! winit gives a process one event loop, and macOS gives it only to the
//! main thread. So the studio asks for windows here and hands them their
//! frames, and the thread that owns the loop makes, draws and closes
//! them: the main thread on macOS, which the `rustel` command serves
//! through [`pump_main`], and the studio's own thread elsewhere. A window
//! floats above the others, so the picture stays in sight while the
//! terminal keeps the keys. Closing a window, or Enter on the widget
//! again, folds the picture back into the dock.

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustel_runtime::video::VideoThumbnails;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::platform::pump_events::EventLoopExtPumpEvents as _;
use winit::window::{Window, WindowId, WindowLevel};

use super::App;

/// What a window draws: one clip, at one position inside it.
struct WindowFrame {
    strip: Arc<VideoThumbnails>,
    position: f32,
}

/// Why a window the studio asked for is gone.
#[derive(Clone, Debug, PartialEq)]
enum Ending {
    /// Its close button was used.
    Closed,
    /// The desktop could not open it.
    Failed(String),
}

/// One window the studio asked for, as both threads see it.
struct Wanted {
    id: u64,
    feed: String,
    latest: Option<WindowFrame>,
    ending: Option<Ending>,
}

/// The windows the studio wants, between its thread and the loop's.
static WANTED: Mutex<Vec<Wanted>> = Mutex::new(Vec::new());

static NEXT_WINDOW: AtomicU64 = AtomicU64::new(0);

/// The wanted list. Each entry is written whole, so a panic that held
/// the lock left nothing half-written behind it.
fn wanted() -> MutexGuard<'static, Vec<Wanted>> {
    WANTED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Mark one wanted window gone, unless it already is.
fn end(id: u64, ending: Ending) {
    if let Some(window) = wanted().iter_mut().find(|window| window.id == id) {
        window.ending.get_or_insert(ending);
    }
}

/// The studio's hold on one window it asked for. Dropping it takes the
/// window off the desktop.
pub(super) struct VideoWindow {
    id: u64,
    feed: String,
}

impl VideoWindow {
    /// Ask for a window playing `feed`. The loop's next pump opens it.
    pub(super) fn open(feed: &str) -> Self {
        let id = NEXT_WINDOW.fetch_add(1, Ordering::Relaxed);
        wanted().push(Wanted {
            id,
            feed: feed.to_owned(),
            latest: None,
            ending: None,
        });
        Self {
            id,
            feed: feed.to_owned(),
        }
    }

    /// The clip the window plays.
    pub(super) fn feed(&self) -> &str {
        &self.feed
    }

    /// Why the window is gone, once it is.
    fn ending(&self) -> Option<Ending> {
        wanted()
            .iter()
            .find(|window| window.id == self.id)
            .and_then(|window| window.ending.clone())
    }

    /// Hand the window the frame its clip is showing now. The loop's next
    /// pump draws what arrived last.
    fn show(&self, strip: Arc<VideoThumbnails>, position: f32) {
        if let Some(window) = wanted().iter_mut().find(|window| window.id == self.id) {
            window.latest = Some(WindowFrame { strip, position });
        }
    }
}

impl Drop for VideoWindow {
    fn drop(&mut self) {
        wanted().retain(|window| window.id != self.id);
    }
}

/// Whether the windows' loop lives on the process's main thread.
pub fn needs_the_main_thread() -> bool {
    cfg!(target_os = "macos")
}

/// Make, draw and close the windows the studio asked for. The main
/// thread of a program that serves the studio calls this every few
/// milliseconds. Does nothing where the studio's own thread owns the
/// loop.
pub fn pump_main() {
    if needs_the_main_thread() {
        pump();
    }
}

/// The studio's turn: where its own thread owns the loop, it pumps here.
/// A unit test's studio has no desktop: a test thread that opened a loop
/// would put real windows on a runner's screen, and a loop that failed
/// would end the windows other tests asked for.
fn pump_here() {
    if !needs_the_main_thread() && !cfg!(test) {
        pump();
    }
}

thread_local! {
    /// The loop and its windows, on the one thread that owns them. Made
    /// with the first window and then kept, because winit refuses a
    /// second loop.
    static DESKTOP: RefCell<Option<Result<Desktop, String>>> = const { RefCell::new(None) };
}

fn pump() {
    DESKTOP.with_borrow_mut(|desktop| {
        if desktop.is_none() {
            if wanted().is_empty() {
                return;
            }
            *desktop = Some(Desktop::open());
        }
        match desktop.as_mut() {
            Some(Ok(desktop)) => desktop.pump(),
            Some(Err(error)) => {
                for window in wanted().iter_mut() {
                    window
                        .ending
                        .get_or_insert_with(|| Ending::Failed(error.clone()));
                }
            }
            None => {}
        }
    });
}

/// The event loop and the windows it shows.
struct Desktop {
    event_loop: EventLoop<()>,
    /// Wakes the loop with an event of its own, for a window to make.
    proxy: EventLoopProxy<()>,
    windows: Windows,
}

impl Desktop {
    fn open() -> Result<Self, String> {
        let mut builder = EventLoop::builder();
        // An accessory app keeps its place: no Dock icon, no focus taken
        // from the terminal the studio runs in.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }
        // Elsewhere the studio's own thread owns the loop, which winit
        // allows when it is asked to.
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        }
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        ))]
        {
            use winit::platform::wayland::EventLoopBuilderExtWayland;
            use winit::platform::x11::EventLoopBuilderExtX11;
            EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
            EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        }
        let event_loop = builder
            .build()
            .map_err(|error| format!("the desktop opened no window: {error}"))?;
        Ok(Self {
            proxy: event_loop.create_proxy(),
            event_loop,
            windows: Windows::default(),
        })
    }

    /// Pump the loop's events once, then draw each window's newest frame.
    fn pump(&mut self) {
        self.windows.let_go();
        // winit makes a window only from inside one of the loop's events,
        // so a window to make sends one rather than wait for the loop's
        // own.
        if self.windows.to_make() {
            let _ = self.proxy.send_event(());
        }
        let _ = self
            .event_loop
            .pump_app_events(Some(Duration::ZERO), &mut self.windows);
        for shown in &mut self.windows.shown {
            shown.fit();
            let frame = wanted()
                .iter_mut()
                .find(|window| window.id == shown.id)
                .and_then(|window| window.latest.take());
            if let Some(frame) = frame {
                shown.draw(&frame);
            }
        }
    }
}

/// The open windows; the target of the pumped events.
#[derive(Default)]
struct Windows {
    shown: Vec<Shown>,
}

impl Windows {
    /// Drop the windows the studio let go.
    fn let_go(&mut self) {
        let wanted = wanted();
        self.shown.retain(|shown| {
            wanted
                .iter()
                .any(|window| window.id == shown.id && window.ending.is_none())
        });
    }

    /// Whether a wanted window has none on the desktop yet.
    fn to_make(&self) -> bool {
        wanted().iter().any(|window| {
            window.ending.is_none() && !self.shown.iter().any(|shown| shown.id == window.id)
        })
    }

    /// Open the wanted windows that have none on the desktop yet. winit
    /// makes windows only inside the loop, so this runs from its events.
    fn follow(&mut self, event_loop: &ActiveEventLoop) {
        let mut wanted = wanted();
        for window in wanted.iter_mut().filter(|window| window.ending.is_none()) {
            if self.shown.iter().any(|shown| shown.id == window.id) {
                continue;
            }
            match Shown::open(event_loop, window.id, &window.feed) {
                Ok(shown) => self.shown.push(shown),
                Err(error) => window.ending = Some(Ending::Failed(error)),
            }
        }
    }
}

impl ApplicationHandler<()> for Windows {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.follow(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        self.follow(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.follow(event_loop);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // A resize needs nothing here: each pump reads the size again.
        if event == WindowEvent::CloseRequested
            && let Some(at) = self
                .shown
                .iter()
                .position(|shown| shown.window.id() == window_id)
        {
            end(self.shown.remove(at).id, Ending::Closed);
        }
    }
}

/// One window on the desktop, with the surface it presents.
struct Shown {
    id: u64,
    window: Arc<Window>,
    /// The surface draws through it, so it lives as long as the surface.
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    /// The size the surface presents, in pixels. The blit walks it.
    size: (usize, usize),
}

impl Shown {
    fn open(event_loop: &ActiveEventLoop, id: u64, feed: &str) -> Result<Self, String> {
        let attributes = Window::default_attributes()
            .with_title(format!("rustel video - {feed}"))
            .with_inner_size(winit::dpi::LogicalSize::new(480_u32, 270_u32))
            // Above the terminal, without the keys: a window made while
            // the studio is not the active app would open behind it.
            .with_window_level(WindowLevel::AlwaysOnTop);
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("the desktop opened no window: {error}"))?,
        );
        let context = softbuffer::Context::new(Arc::clone(&window))
            .map_err(|error| format!("the window has nothing to draw with: {error}"))?;
        let surface = softbuffer::Surface::new(&context, Arc::clone(&window))
            .map_err(|error| format!("the window has nothing to draw with: {error}"))?;
        Ok(Self {
            id,
            window,
            _context: context,
            surface,
            size: (1, 1),
        })
    }

    /// Size the surface to the window, when the window changed size.
    fn fit(&mut self) {
        let size = self.window.inner_size();
        let (width, height) = (size.width as usize, size.height as usize);
        if width == 0 || height == 0 || self.size == (width, height) {
            return;
        }
        let (to_width, to_height) = (
            NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
        );
        if self.surface.resize(to_width, to_height).is_ok() {
            self.size = (width, height);
        }
    }

    /// Blit a frame into the window, nearest-sampled into the window's own
    /// size: the strip is thumbnail-sized and the window a few times
    /// larger, where nearest and averaged differ by a softness nobody
    /// watches a clip for.
    fn draw(&mut self, frame: &WindowFrame) {
        let Ok(mut buffer) = self.surface.buffer_mut() else {
            return;
        };
        let (width, height) = (self.size.0.max(1), self.size.1.max(1));
        let pixels = buffer.as_mut();
        let Some(frame_rgba) = frame.strip.frame_at(frame.position) else {
            pixels.fill(0);
            let _ = buffer.present();
            return;
        };
        let (frame_width, frame_height) =
            (frame.strip.width() as usize, frame.strip.height() as usize);
        for y in 0..height {
            let source_row = (y * frame_height / height) * frame_width;
            let row = y * width;
            if row >= pixels.len() {
                break;
            }
            for x in 0..width.min(pixels.len() - row) {
                let source = (source_row + x * frame_width / width) * 4;
                pixels[row + x] = frame_rgba
                    .get(source..source + 3)
                    .and_then(|rgb| {
                        let (red, green, blue) = (*rgb.first()?, *rgb.get(1)?, *rgb.get(2)?);
                        Some((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue))
                    })
                    .unwrap_or(0);
            }
        }
        let _ = buffer.present();
    }
}

impl App {
    /// Enter on the dock's video widget: the widget's clip opens a window
    /// of its own on the desktop, or comes back to the dock. One window
    /// per clip. Each is placed and sized by hand, and none of it is kept
    /// past the studio's exit.
    pub(super) fn toggle_video_window(&mut self) {
        let Some(feed) = self.focused_video_feed() else {
            return;
        };
        if let Some(at) = self
            .video_windows
            .iter()
            .position(|window| window.feed() == feed)
        {
            self.video_windows.remove(at);
            self.status = format!("{feed} is back in the dock");
        } else {
            self.video_windows.push(VideoWindow::open(&feed));
            self.status = format!("{feed} plays in the window - Enter takes it back");
        }
        self.dirty_frame = true;
    }

    /// The feed the focused video widget draws, if it is a video widget
    /// with a feed to draw.
    fn focused_video_feed(&self) -> Option<String> {
        let feeds = self.visual.video_feeds();
        if feeds.is_empty() {
            return None;
        }
        let index = self.focused_dock()?;
        let chosen = self.viz_docks[index].as_ref()?.selected;
        let spec = self.prefs.visuals[index].widgets.get(chosen)?;
        if spec.kind != super::super::viz_panel::WidgetKind::Video {
            return None;
        }
        let feed = spec
            .feed
            .as_deref()
            .filter(|feed| feeds.iter().any(|known| known == *feed))
            .unwrap_or(&feeds[0]);
        Some(feed.to_owned())
    }

    /// Hand each detached window its clip's frame, each turn of the
    /// studio's loop. A window that closed folds its clip back into the
    /// dock.
    pub(super) fn pump_video_window(&mut self) {
        pump_here();
        let mut ended = None;
        for (index, window) in self.video_windows.iter().enumerate() {
            if let Some(ending) = window.ending() {
                ended.get_or_insert((index, ending));
                continue;
            }
            if let Some((position, strip)) = self.visual.video_feed_frame(window.feed()) {
                window.show(strip, position as f32);
            }
        }
        if let Some((index, ending)) = ended {
            let feed = self.video_windows.remove(index).feed().to_owned();
            self.status = match ending {
                Ending::Closed => format!("the {feed} window closed - back in the dock"),
                Ending::Failed(error) => format!("the {feed} window did not open: {error}"),
            };
            self.dirty_frame = true;
        }
    }

    /// The clips playing in detached windows, for the widgets to fold
    /// around. Built for the frame. The windows own the truth.
    pub(super) fn detached_video_feeds(&self) -> Vec<String> {
        self.video_windows
            .iter()
            .map(|window| window.feed().to_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> Arc<VideoThumbnails> {
        Arc::new(
            VideoThumbnails::from_frames(1, 1, vec![(0.0, vec![9, 9, 9, 255])]).expect("strip"),
        )
    }

    fn is_wanted(id: u64) -> bool {
        wanted().iter().any(|window| window.id == id)
    }

    #[test]
    fn a_window_waits_for_the_loop_with_its_newest_frame_and_goes_when_let_go() {
        let window = VideoWindow::open("clip.webm");
        let id = window.id;
        assert!(is_wanted(id), "asking puts the window on the list");
        assert_eq!(window.feed(), "clip.webm");
        window.show(strip(), 0.5);
        window.show(strip(), 0.75);
        let position = wanted()
            .iter()
            .find(|wanted| wanted.id == id)
            .and_then(|wanted| wanted.latest.as_ref().map(|frame| frame.position));
        assert_eq!(position, Some(0.75), "only the newest frame waits");
        assert_eq!(window.ending(), None);
        drop(window);
        assert!(!is_wanted(id), "letting go takes it off the list");
    }

    #[test]
    fn a_window_that_ended_says_why_once() {
        let window = VideoWindow::open("clip.webm");
        end(window.id, Ending::Failed("no display".to_owned()));
        end(window.id, Ending::Closed);
        assert_eq!(
            window.ending(),
            Some(Ending::Failed("no display".to_owned())),
            "the first reason stays"
        );
    }
}
