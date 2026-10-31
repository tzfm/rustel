//! The detached video window: the dock's video widget opened onto the
//! desktop, playing the same clip the widget chose, in the same sync.
//!
//! The window belongs to the studio's own thread, beside the terminal:
//! winit's event loop is pumped between terminal events, so no second
//! thread and no second process is involved. Closing the window - or
//! Enter on the widget again - folds the picture back into the dock.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use rustel_runtime::video::VideoThumbnails;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::pump_events::EventLoopExtPumpEvents as _;
use winit::window::{Window, WindowId};

use super::App;

/// What the window draws: one clip, at one position inside it.
struct WindowFrame {
    strip: Arc<VideoThumbnails>,
    position: f32,
}

/// The open window and everything it needs, on the studio's own thread.
pub(super) struct VideoWindowHost {
    event_loop: EventLoop<()>,
    handler: WindowHandler,
}

/// The window, its surface and its latest frame; the target of the
/// pumped events. winit hands out windows only from inside the loop, so
/// the first pump creates it.
struct WindowHandler {
    feed: String,
    window: Option<Arc<Window>>,
    context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    latest: Option<WindowFrame>,
    /// Set when the window's close button was used.
    closed: bool,
    /// The size the surface presents, in pixels; the blit walks it.
    size: (usize, usize),
}

impl VideoWindowHost {
    /// Open a window playing `feed`. The event loop is made here, on the
    /// studio's own thread, which on every platform is the thread winit
    /// wants; the window itself arrives with the first pump.
    pub(super) fn open(feed: &str) -> Result<Self, String> {
        let mut builder = EventLoop::builder();
        // An accessory app keeps its place: no Dock icon, no focus stolen
        // from the terminal the studio is running in.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }
        let event_loop = builder
            .build()
            .map_err(|error| format!("open the video window: {error}"))?;
        Ok(Self {
            event_loop,
            handler: WindowHandler {
                feed: feed.to_owned(),
                window: None,
                context: None,
                surface: None,
                latest: None,
                closed: false,
                size: (1, 1),
            },
        })
    }

    /// The clip the window plays.
    pub(super) fn feed(&self) -> &str {
        &self.handler.feed
    }

    /// Whether the window was closed by its own close button.
    pub(super) fn is_closed(&self) -> bool {
        self.handler.closed
    }

    /// Hand the window the frame its clip is showing now. Nothing is
    /// drawn here: the next pump draws what arrived last.
    pub(super) fn show(&mut self, strip: Arc<VideoThumbnails>, position: f32) {
        self.handler.latest = Some(WindowFrame { strip, position });
        if let Some(window) = self.handler.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Pump the window's events once and draw the newest frame. Called
    /// between the terminal's events, so the window and the studio take
    /// turns on the same thread and neither waits for the other.
    pub(super) fn pump(&mut self) {
        let Self {
            event_loop,
            handler,
            ..
        } = self;
        let _ = event_loop.pump_app_events(Some(Duration::ZERO), handler);
        if handler.closed {
            return;
        }
        let Some(window) = handler.window.as_ref() else {
            return;
        };
        let size = window.inner_size();
        let (width, height) = (size.width as usize, size.height as usize);
        if width > 0
            && height > 0
            && handler.size != (width, height)
            && let Some(surface) = handler.surface.as_mut()
        {
            let (to_width, to_height) = (
                NonZeroU32::new(width as u32).unwrap_or(NonZeroU32::MIN),
                NonZeroU32::new(height as u32).unwrap_or(NonZeroU32::MIN),
            );
            if surface.resize(to_width, to_height).is_ok() {
                handler.size = (width, height);
            }
        }
        handler.draw();
    }
}

impl WindowHandler {
    /// Make the window and its surface, inside the loop as winit wants.
    fn open_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(format!("rustel video - {}", self.feed))
            .with_inner_size(winit::dpi::LogicalSize::new(480_u32, 270_u32));
        let Ok(window) = event_loop.create_window(attributes) else {
            self.closed = true;
            return;
        };
        let window = Arc::new(window);
        let Ok(context) = softbuffer::Context::new(Arc::clone(&window)) else {
            self.closed = true;
            return;
        };
        let Ok(surface) = softbuffer::Surface::new(&context, Arc::clone(&window)) else {
            self.closed = true;
            return;
        };
        self.context = Some(context);
        self.surface = Some(surface);
        self.window = Some(window);
    }

    /// Blit the newest frame into the window, nearest-sampled into the
    /// window's own size: the strip is thumbnail-sized and the window a
    /// few times larger, where nearest and averaged differ by a softness
    /// nobody watches a clip for.
    fn draw(&mut self) {
        let Some(frame) = self.latest.take() else {
            return;
        };
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let Ok(mut buffer) = surface.buffer_mut() else {
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

impl ApplicationHandler<()> for WindowHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.open_window(event_loop);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.closed = true,
            // The pump reads the size again each turn, so a resize only
            // needs the next turn.
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => {}
            _ => {}
        }
    }
}

impl App {
    /// Enter on the dock's video widget: the widget's clip opens a window
    /// of its own on the desktop, or comes back to the dock. One window
    /// per clip; each is placed and sized by hand, and none of it is
    /// kept past the studio's exit.
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
            self.dirty_frame = true;
            return;
        }
        match VideoWindowHost::open(&feed) {
            Ok(host) => {
                self.video_windows.push(host);
                self.status = format!("{feed} plays in the window - Enter takes it back");
            }
            Err(error) => {
                self.status = error;
            }
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

    /// Hand each detached window its clip's frame and pump its events,
    /// each turn of the studio's loop. A window closed by its own button
    /// folds its clip back into the dock.
    pub(super) fn pump_video_window(&mut self) {
        let mut closed = None;
        for (index, window) in self.video_windows.iter_mut().enumerate() {
            window.pump();
            if window.is_closed() {
                closed.get_or_insert(index);
                continue;
            }
            if let Some((position, strip)) = self.visual.video_feed_frame(window.feed()) {
                window.show(strip, position as f32);
            }
        }
        if let Some(index) = closed {
            let feed = self.video_windows.remove(index).feed().to_owned();
            self.status = format!("the {feed} window closed - back in the dock");
            self.dirty_frame = true;
        }
    }

    /// The clips playing in detached windows, for the widgets to fold
    /// around. Built for the frame; the windows own the truth.
    pub(super) fn detached_video_feeds(&self) -> Vec<String> {
        self.video_windows
            .iter()
            .map(|window| window.feed().to_owned())
            .collect()
    }

    /// Whether anything wants the set's video clips known: a dock's video
    /// widget picks its feed from them, and a detached window keeps
    /// playing its own after the widget is gone.
    pub(super) fn wants_video_feeds(&self) -> bool {
        self.prefs.visuals.iter().any(|dock| {
            dock.widgets
                .iter()
                .any(|widget| widget.kind == super::super::viz_panel::WidgetKind::Video)
        }) || !self.video_windows.is_empty()
    }
}
