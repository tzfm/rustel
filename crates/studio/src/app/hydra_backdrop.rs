//! The Hydra pictures drawn behind the score, and the one in the reference
//! shelf's thumbnail box. This file tells the Hydra renderer what frame sizes
//! to render, which snippet the shelf is previewing and which theme sketch to
//! draw (checked first, along with the theme's webcam flag). It also says why
//! a shelf snippet will not draw, and picks up each renderer's latest frame at
//! the strength the opacity settings allow.

use super::*;

/// How strongly the shelf's thumbnail shows through.
///
/// Nearly all of it, and not the reader's business: this one is a picture you
/// are looking AT, in a box of its own, rather than a backdrop something else
/// is written over. The backdrop's own strength is a setting.
#[cfg(feature = "hydra")]
const HYDRA_PREVIEW_STRENGTH: f32 = 0.92;

/// The newest frame from one of Hydra's two renderers, or the last one still.
///
/// A terminal redraws on its own schedule and Hydra on its own, so a frame
/// nothing has replaced is still the picture. When the renderer stops wanting
/// the screen, the picture goes with it rather than freezing on the last one.
#[cfg(feature = "hydra")]
fn latest_hydra_frame(
    frames: Option<&rustel_hydra::HydraFrames>,
    held: Option<super::super::view::VisualBackdrop>,
    strength: f32,
    interface: f32,
) -> Option<super::super::view::VisualBackdrop> {
    let frames = frames.filter(|frames| frames.wanted())?;
    match frames.take() {
        Some((width, height, rgba)) => Some(super::super::view::VisualBackdrop {
            width,
            height,
            rgba,
            strength,
            interface,
        }),
        None => held,
    }
}

impl App {
    /// Keep the engine showing whatever the shelf's cursor is on.
    ///
    /// Sent only when it changes: browsing is a lot of keystrokes and each one
    /// would otherwise reinstall a snippet that is already running.
    #[cfg(feature = "hydra")]
    pub(super) fn sync_snippet_preview(&mut self) {
        // Only a Hydra chain has a picture; a score line is left unsent so
        // the box empties rather than keeping the last sketch's frame.
        let wanted = self
            .reference_panel
            .as_ref()
            .filter(|panel| panel.shows_picture())
            .and_then(|panel| panel.selected_snippet_code())
            .map(std::borrow::Cow::into_owned);
        // Hide immediately, even if the renderer has not processed the stop
        // yet or its command queue is full.
        if wanted.is_none() && self.hydra_shelf_last.take().is_some() {
            self.dirty_frame = true;
        }
        if wanted == self.hydra_preview_sent {
            return;
        }
        if self.worker.try_preview_snippet(wanted.clone()) {
            self.hydra_preview_sent = wanted;
        }
    }

    /// Ask the renderer for frames the size of what will be looked at.
    ///
    /// On the cell path the backdrop samples one pixel per character, so a
    /// full-resolution render is a megabyte encoded to use forty kilobytes of
    /// it. On the kitty path the pixels really are drawn, so the render's own
    /// size is what is wanted. Sent only when it changes.
    #[cfg(feature = "hydra")]
    pub(super) fn sync_hydra_frame_size(&mut self) {
        let pixels = super::super::graphics::tier() == super::super::graphics::Tier::Pixels;
        let cap = |area: Rect| {
            if pixels {
                // The render matches the glass: one image pixel per screen
                // pixel, from the cell size the terminal reported. The
                // pixel budget keeps a huge window from demanding a larger
                // render: past the budget the frame scales down in
                // proportion. `watch_pixel_frame` measures the budget; it
                // never exceeds `PIXEL_CEILING`.
                let (cell_w, cell_h) = super::super::graphics::cell_pixels().unwrap_or((10, 20));
                let width = u64::from(area.width) * u64::from(cell_w.max(1));
                let height = u64::from(area.height) * u64::from(cell_h.max(1));
                let frame_pixels = width.saturating_mul(height);
                let budget = self.pixel_budget;
                let scale = if frame_pixels > budget {
                    (budget as f64 / frame_pixels as f64).sqrt()
                } else {
                    1.0
                };
                let scaled =
                    |dimension: u64| (dimension as f64 * scale).min(f64::from(u16::MAX)) as u16;
                (scaled(width), scaled(height))
            } else {
                (area.width, area.height)
            }
        };
        let score = cap(self.frame);
        let preview = cap(super::super::reference::preview_area(
            super::super::reference::inner_area(self.regions.reference),
        ));
        let wanted = rustel_runtime::hydra::HydraFrameRequest {
            score,
            preview,
            smoothing: self.ui_settings.backdrop_smoothing,
        };
        if self.hydra_frame_size_sent == Some(wanted) {
            return;
        }
        if self.worker.try_set_hydra_frame_size(wanted) {
            self.hydra_frame_size_sent = Some(wanted);
        }
    }

    #[cfg(feature = "hydra")]
    pub(super) fn hydra_frame_count(&self) -> Option<u64> {
        self.hydra_backdrop
            .as_ref()
            .map(rustel_hydra::HydraFrames::delivered)
    }

    #[cfg(not(feature = "hydra"))]
    pub(super) fn hydra_frame_count(&self) -> Option<u64> {
        None
    }

    /// The shelf's thumbnail box, when the reference column shows a
    /// picture there.
    ///
    /// While the shelf is open the picture belongs in its box, not behind
    /// the whole screen: a reader comparing snippets wants a thumbnail, and
    /// a full-screen wash of the one under the cursor is not that.
    #[cfg(feature = "hydra")]
    pub(super) fn snippet_box(&self) -> Option<Rect> {
        self.reference_panel
            .as_ref()
            .filter(|panel| panel.shows_picture())
            .map(|_| {
                super::super::reference::preview_area(super::super::reference::inner_area(
                    self.regions.reference,
                ))
            })
            .filter(|area| area.width > 0 && area.height > 0)
    }

    /// Without Hydra there is no picture for the shelf's box.
    #[cfg(not(feature = "hydra"))]
    pub(super) fn snippet_box(&self) -> Option<Rect> {
        None
    }

    /// Hand the engine the current theme's sketch, when it changed.
    ///
    /// Validated here first - parse and compose are pure and cost nothing -
    /// so a theme with a broken sketch logs one line and draws nothing,
    /// rather than asking a render thread to find out.
    #[cfg(feature = "hydra")]
    pub(super) fn sync_theme_sketch(&mut self) {
        // A theme change is also a camera-request change, and this function
        // returns early at its own dedup below whenever two sketchless
        // themes follow each other - which is every switch to or from the
        // natively drawn camera theme. So it goes first.
        self.sync_settings_webcam_preview();
        let raw = self
            .theme
            .hydra_code()
            .map(str::trim)
            .filter(|code| !code.is_empty());
        let desired = match raw {
            None => None,
            // The parse is memoized on the text, because this runs every
            // loop pass: the pass costs a compare, a changed sketch costs
            // one parse (and one log line when it is broken).
            Some(code) => match &self.theme_sketch_checked {
                Some((checked, verdict)) if checked == code => verdict.clone(),
                _ => {
                    let verdict = match rustel_hydra::glsl::parse_chain(code)
                        .map_err(|error| error.to_string())
                        .and_then(|node| {
                            if self.theme.hydra_camera_enabled()
                                && !super::super::theme::hydra_node_uses_s0(&node)
                            {
                                return Err(
                                    "hydra_camera requires a sketch that visibly samples s0"
                                        .to_owned(),
                                );
                            }
                            rustel_hydra::glsl::compose(&node, "highp")
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        }) {
                        Ok(()) => Some(code.to_owned()),
                        Err(error) => {
                            self.log.push(
                                LogLevel::Warn,
                                "theme",
                                format!("the theme's sketch does not parse: {error}"),
                            );
                            None
                        }
                    };
                    self.theme_sketch_checked = Some((code.to_owned(), verdict.clone()));
                    verdict
                }
            },
        };
        // A camera theme kept by Enter, or one being looked at in the picker
        // while the Hydra webcam setting is already on.
        //
        // Browsing acquires the camera only under that setting, so a camera
        // theme can be seen before it is chosen. The switch in the settings
        // sheet is the consent, and it is already on here, so showing the
        // camera under it is not a new decision - which is the same
        // reasoning that lets the sheet's own row hold a live thumbnail.
        // With the switch off, nothing opens and the row previews as the
        // still theme it is.
        //
        // The theme editor never acquires: a draft is being typed rather
        // than chosen, and its sketch changes under the fingers.
        let confirmed = self.theme_camera_confirmed.as_deref() == Some(self.theme.name.as_str());
        let previewing = self.theme_picker.is_some() && self.ui_settings.hydra_webcam;
        let webcam = desired.is_some()
            && self.theme.hydra_camera_enabled()
            && self.theme_editor.is_none()
            && self.opacity_before_theme_editor.is_none()
            && (confirmed || previewing);
        if self.theme_sketch_sent.as_ref() == Some(&(desired.clone(), webcam)) {
            return;
        }
        // The last frame of the sketch that was showing belongs to that
        // sketch: held past a theme change it would stay painted under the
        // next theme - synthwave's gradient under a reset-colour
        // background - until a new frame replaced it, which for a theme
        // without a sketch is never.
        self.hydra_theme_last = None;
        // try_send over a 2-slot queue can drop; `sent` only advances on
        // success, and the event loop pumps this again, so a dropped send
        // is a beat late rather than wedged forever.
        if self.worker.try_set_theme_sketch(desired.clone(), webcam) {
            self.theme_sketch_sent = Some((desired, webcam));
        }
    }

    #[cfg(feature = "hydra")]
    pub(super) fn snippet_refusal(&self) -> Option<String> {
        let code = self
            .reference_panel
            .as_ref()
            .filter(|panel| {
                panel.tab.is_snippets()
                    && panel.selected_snippet_kind() == Some(super::super::examples::Kind::Hydra)
            })?
            .selected_snippet_code()?;
        match rustel_hydra::glsl::parse_chain(&code) {
            Err(error) => Some(error.to_string()),
            Ok(node) => rustel_hydra::glsl::compose(&node, "highp")
                .err()
                .map(|error| error.to_string()),
        }
    }

    /// Refresh the pictures this frame paints from: the score's backdrop,
    /// the shelf's thumbnail, and the theme's own picture. Each keeps its
    /// last complete render so the terminal can repaint between renders.
    #[cfg(feature = "hydra")]
    pub(super) fn update_hydra_latest(&mut self) {
        // Two surfaces over one picture, each with its own opacity: the
        // code area (editor opacity) and the chrome around it (ui
        // opacity). Each takes the picture as bright as visuals opacity
        // asks and never more than its own opacity leaves, so a solid
        // setting on either really is solid.
        let editor_visual_strength = backdrop_strength(
            self.ui_settings.backdrop_opacity,
            self.ui_settings.editor_opacity,
        );
        let interface_visual_strength = backdrop_strength(
            self.ui_settings.backdrop_opacity,
            self.ui_settings.interface_opacity,
        );
        self.hydra_last = latest_hydra_frame(
            self.hydra_backdrop.as_ref(),
            self.hydra_last.take(),
            editor_visual_strength,
            interface_visual_strength,
        );
        // The shelf's thumbnail is a picture, not a wash behind text, so
        // it goes on at nearly full strength.
        self.hydra_shelf_last = if self
            .reference_panel
            .as_ref()
            .is_some_and(|panel| panel.shows_picture() && panel.selected_snippet().is_some())
        {
            latest_hydra_frame(
                self.hydra_shelf.as_ref(),
                self.hydra_shelf_last.take(),
                HYDRA_PREVIEW_STRENGTH,
                // The thumbnail has no interface inside it to see through.
                HYDRA_PREVIEW_STRENGTH,
            )
        } else {
            None
        };
        // The theme's picture, at the theme's own strength - forged,
        // because it is part of the look, the way a wallpaper's tint is
        // not a screen setting. Forged is a look, though, not an
        // exemption: visuals opacity caps it exactly as it caps a score's
        // picture, or a camera theme would be the one visual on screen
        // that a reader could not turn down. The surfaces' opacities cap
        // what is left.
        let theme_strength = self
            .theme
            .hydra_opacity_percent()
            .unwrap_or(45)
            .min(self.ui_settings.backdrop_opacity);
        self.hydra_theme_last = latest_hydra_frame(
            self.hydra_theme.as_ref(),
            self.hydra_theme_last.take(),
            backdrop_strength(theme_strength, self.ui_settings.editor_opacity),
            backdrop_strength(theme_strength, self.ui_settings.interface_opacity),
        );
        // A theme without a sketch holds no frame, whatever the sketch
        // before it is still sending: switched to fast, its last frames
        // arrive after the switch, and held they would be its picture
        // under this theme for good.
        if self
            .theme
            .hydra_code()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            self.hydra_theme_last = None;
        }
    }
}

#[cfg(test)]
mod tests {
    mod hydra_backdrop {
        //! Tests for `app/hydra_backdrop.rs`.

        use crate::app::*;

        /// The webcam switch in the settings sheet is the consent for a camera
        /// theme: browsing to the theme opens the camera only with the switch
        /// on. A theme-editor draft never opens the camera.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_camera_theme_previews_under_the_webcam_switch_and_never_from_a_draft() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let camera_theme = Theme::built_in("webcam-clean").expect("webcam-clean");
            assert!(
                camera_theme.hydra_camera_enabled(),
                "the fixture must ask for s0"
            );

            // What `sync_theme_sketch` last decided. The send is a `try_send`
            // that can drop, so the helper calls it until one send lands.
            let asked = |app: &mut App| -> bool {
                app.theme_sketch_sent = None;
                for _ in 0..400 {
                    app.sync_theme_sketch();
                    if let Some((_, webcam)) = app.theme_sketch_sent.as_ref() {
                        return *webcam;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                panic!("the theme sketch was never accepted by the worker");
            };

            // Browsing to it with the switch on: the camera comes up, the way
            // every other theme in the picker shows what it looks like.
            app.theme = camera_theme.clone();
            app.ui_settings.hydra_webcam = true;
            app.theme_picker = Some(ThemePicker::open(&app.theme));
            assert!(
                asked(&mut app),
                "a browsed camera theme must show its camera"
            );

            // The same row with the switch off opens nothing.
            app.ui_settings.hydra_webcam = false;
            assert!(
                !asked(&mut app),
                "the webcam switch is the consent and it is off"
            );

            // Kept by Enter, the switch is no longer what carries it.
            app.ui_settings.hydra_webcam = true;
            app.keep_theme();
            assert!(app.theme_picker.is_none());
            assert!(asked(&mut app), "a theme kept by Enter keeps its camera");

            // A draft in the editor never acquires, switch or no switch.
            app.open_theme_editor(&camera_theme, String::new(), true);
            assert!(
                !asked(&mut app),
                "a theme being typed is not a theme being chosen"
            );
        }

        /// A surface at opacity 100 hides the picture at any visuals opacity. A
        /// lower surface opacity lets the picture through, never brighter than
        /// visuals opacity asks. The defaults do not change the picture in the
        /// code area.
        #[test]
        fn a_surfaces_opacity_caps_the_picture_behind_it() {
            use crate::settings::{BACKDROP_OPACITY_DEFAULT, EDITOR_OPACITY_DEFAULT};
            assert_eq!(backdrop_strength(100, 100), 0.0, "a solid surface hides it");
            assert_eq!(backdrop_strength(55, 100), 0.0);
            assert!(
                (backdrop_strength(55, 0) - 0.55).abs() < 1e-6,
                "never past the picture's own"
            );
            assert_eq!(backdrop_strength(0, 0), 0.0, "visuals opacity zero is off");
            // Perceptual: half opaque reads as half the picture, which on a
            // linear mix would have taken the slider most of the way up.
            let half = backdrop_strength(100, 50);
            assert!((0.2..0.25).contains(&half), "{half}");
            let steps: Vec<f32> = (0..=10).map(|i| backdrop_strength(100, i * 10)).collect();
            assert!(
                steps.windows(2).all(|pair| pair[0] > pair[1]),
                "every step of the slider changes something: {steps:?}"
            );
            assert!(
                (backdrop_strength(BACKDROP_OPACITY_DEFAULT, EDITOR_OPACITY_DEFAULT) - 0.53).abs()
                    < 0.02,
                "the shipped look is unchanged"
            );
        }

        /// `visuals opacity` applies to a theme's own Hydra opacity too: at
        /// zero it turns off every kind of visual, a camera theme included.
        #[test]
        fn visuals_opacity_caps_a_themes_forged_hydra_strength() {
            let theme_hydra = |forged: u8, visuals: u8, surface: u8| {
                backdrop_strength(forged.min(visuals), surface)
            };
            assert_eq!(
                theme_hydra(100, 0, 0),
                0.0,
                "zero turns a theme's picture off like any other"
            );
            assert!(
                (theme_hydra(100, 10, 0) - 0.10).abs() < 1e-6,
                "a theme cannot outshine what visuals opacity asks for"
            );
            assert!(
                (theme_hydra(45, 100, 0) - 0.45).abs() < 1e-6,
                "and a slider wide open leaves the forged look exactly as it was"
            );
            assert!(
                theme_hydra(100, 100, 100) == 0.0,
                "a solid surface still hides it"
            );
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn a_music_example_clears_the_hydra_preview_before_the_renderer_stops() {
            use crate::examples::{Kind, section_of};
            use crate::reference::{ReferencePanel, SnippetLine};

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let mut panel = ReferencePanel::browse(&app.reference);
            panel.select_tab(Tab::Examples);
            let hydra = section_of(Kind::Hydra).unwrap();
            panel.section_open.insert(hydra);
            panel.snippet_open.insert((hydra, 0));
            panel.section_open.insert(0);
            panel.snippet_open.insert((0, 0));
            let lines = panel.snippet_lines();
            panel.snippet_selected = lines
                .iter()
                .position(|line| *line == SnippetLine::Snippet(hydra, 0, 0))
                .unwrap();
            app.reference_panel = Some(panel);
            app.sync_snippet_preview();
            assert!(app.hydra_preview_sent.is_some());
            app.hydra_shelf_last = Some(view::VisualBackdrop {
                width: 1,
                height: 1,
                rgba: vec![255, 0, 0, 255],
                strength: 1.0,
                interface: 1.0,
            });

            let panel = app.reference_panel.as_mut().unwrap();
            panel.snippet_selected = lines
                .iter()
                .position(|line| *line == SnippetLine::Snippet(0, 0, 0))
                .unwrap();
            let code = panel.selected_snippet_code().unwrap().into_owned();
            app.preview_snippet_score(&code);
            app.sync_snippet_preview();
            assert!(
                app.hydra_shelf_last.is_none(),
                "no stale picture while the engine stops"
            );
            // The sound preview can fill the bounded command queue. Like the
            // frame loop, retry the renderer stop after the worker makes room;
            // hiding the old picture above must not wait for that acceptance.
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.hydra_preview_sent.is_some() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                app.sync_snippet_preview();
            }
            assert!(app.hydra_preview_sent.is_none(), "stop the Hydra renderer");
            assert!(
                app.snippet_preview.is_some(),
                "keep the sound preview playing"
            );
            assert!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .section_open
                    .contains(&hydra)
            );
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn leaving_hydra_browsing_removes_only_the_temporary_background() {
            use crate::examples::{Kind, section_of};
            use crate::reference::{ReferencePanel, SnippetLine};

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let picture = |red| view::VisualBackdrop {
                width: 1,
                height: 1,
                rgba: vec![red, 0, 0, 255],
                strength: 1.0,
                interface: 1.0,
            };
            for close in [false, true] {
                let mut panel = ReferencePanel::browse(&app.reference);
                panel.select_tab(Tab::Examples);
                let hydra = section_of(Kind::Hydra).unwrap();
                panel.section_open.insert(hydra);
                panel.snippet_open.insert((hydra, 0));
                panel.snippet_selected = panel
                    .snippet_lines()
                    .iter()
                    .position(|line| *line == SnippetLine::Snippet(hydra, 0, 0))
                    .unwrap();
                app.hydra_preview_sent = Some(panel.selected_snippet_code().unwrap().into_owned());
                app.reference_panel = Some(panel);
                app.focus_panel(PanelKind::Reference);
                app.hydra_shelf_last = Some(picture(255));
                app.hydra_last = Some(picture(128));
                app.hydra_theme_last = Some(picture(64));
                if close {
                    crate::app::tests::support::press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                    assert!(app.reference_panel.is_none());
                } else {
                    app.reference_panel
                        .as_mut()
                        .unwrap()
                        .select_tab(Tab::Samples);
                }
                app.sync_snippet_preview();
                assert!(app.hydra_shelf_last.is_none());
                assert_eq!(app.hydra_last.as_ref().unwrap().rgba, [128, 0, 0, 255]);
                assert_eq!(app.hydra_theme_last.as_ref().unwrap().rgba, [64, 0, 0, 255]);
            }
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn hydra_preview_matches_the_shelf_box_in_both_renderers() {
            use crate::graphics::{self, HeldRenderer, Tier};

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let _held = HeldRenderer::pixels((10, 20));
            app.frame = Rect::new(0, 0, 100, 40);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            let area =
                crate::reference::preview_area(crate::reference::inner_area(app.regions.reference));
            for tier in [Tier::Pixels, Tier::Cells] {
                graphics::set_tier(tier);
                app.hydra_frame_size_sent = None;
                app.sync_hydra_frame_size();
                let size = app.hydra_frame_size_sent.expect("renderer size sent");
                let scale = if tier == Tier::Pixels {
                    (10, 20)
                } else {
                    (1, 1)
                };
                assert_eq!(size.score, (100 * scale.0, 40 * scale.1));
                assert_eq!(size.preview, (area.width * scale.0, area.height * scale.1),);
            }
        }

        /// A theme's last sketch frame goes with the theme: a reset-colour theme
        /// after `synthwave` is the terminal's own background, not a gradient
        /// nobody is drawing any more.
        #[cfg(feature = "hydra")]
        #[test]
        fn changing_theme_drops_the_previous_sketch_frame() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.theme = Theme::built_in("synthwave").expect("synthwave");
            app.sync_theme_sketch();
            app.hydra_theme_last = Some(crate::view::VisualBackdrop {
                width: 2,
                height: 1,
                rgba: vec![255, 0, 255, 255, 255, 0, 255, 255],
                strength: 0.5,
                interface: 0.2,
            });
            app.theme = Theme::built_in("rustel-light").expect("rustel-light");
            app.theme.name = "custom-reset".into();
            app.theme.background = ratatui::style::Color::Reset;
            app.theme.surface = ratatui::style::Color::Reset;
            app.sync_theme_sketch();
            assert!(
                app.hydra_theme_last.is_none(),
                "the old sketch's frame is not painted under the new theme"
            );
        }
    }
}
