//! End-to-end frame composition for the terminal studio.
//!
//! Everything below the widget level is exercised by unit tests. This one
//! draws a complete frame the way the application does - chrome, source,
//! minimap, inline visualizers, master dock and device dock - and checks
//! that the pieces are all present: bars in block glyphs, lines in Braille.

#[test]
fn a_complete_frame_draws_every_surface_in_solid_blocks() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use rustel_runtime::ProcessStats;
    use rustel_studio::devices::{DeviceEntry, DeviceInventory, MidiPortCounts};
    use rustel_studio::editor::{ByteOffset, Editor, KeyboardCapabilities, VirtualRowSpec};
    use rustel_studio::meter::MasterState;
    use rustel_studio::minimap::Minimap;
    use rustel_studio::theme::Theme;
    use rustel_studio::view::{
        self, Decorations, PaneView, SceneChip, SceneStripMode, StudioChrome, StudioView,
    };
    use rustel_studio::visuals::VisualState;
    use std::path::Path;
    use std::time::Instant;

    let source = "// a little groove\n$: s(\"[bd <hh oh>]*2\").bank(\"tr909\")._pianoroll()\n$: note(\"c4 e4 g4\").s(\"piano\").spiral()\n// $: s(\"hh*8\").gain(.3)\n";
    let mut editor = Editor::new(source).unwrap();
    let size = Rect::new(0, 0, 150, 34);
    let envelope = rustel_runtime::ui_events::visual_layout(source, 1).unwrap();
    let mut visual = VisualState::default();
    let revision = envelope.ui_layout.source_revision.clone();
    visual.install_layout(envelope.clone());
    visual.start();
    let mut events = Vec::new();
    for step in 0..8 {
        let begin = format!("{step}/8");
        let end = format!("{}/8", step + 1);
        events.push(rustel_runtime::ui_events::UiScheduledEvent {
            onset_id: step as u64,
            generation: 1,
            whole_begin: begin.clone(),
            whole_end: end.clone(),
            part_begin: begin,
            part_end: end,
            target_time: 10.0 + step as f64 * 0.25,
            duration_seconds: 0.24,
            value: Some(if step % 2 == 0 { "bd" } else { "hh" }.into()),
            color: None,
            label: Some(if step % 2 == 0 { "bd" } else { "hh" }.into()),
            active_label: None,
            scale: None,
            frequency_hz: if step % 3 == 0 {
                Some(220.0 * (1.0 + step as f32 / 8.0))
            } else {
                None
            },
            gain: Some(1.0),
            // The inline roll is the layout's first widget, slot 0; the
            // engine tags every event of its pattern with that bit.
            ui_visuals: 1,
            context: vec![(20, 24)],
        });
    }
    let batch =
        rustel_runtime::ui_events::UiEventBatch::new(10.5, 0.5, 0.5, 1, revision, events, 0)
            .unwrap();
    assert!(visual.install_batch(batch));
    let layout = view::regions(size, false, 1, false, true, None, None);
    let pane_region = layout.panes[0];
    let grid = view::source_grid(&editor, pane_region.editor, true);
    editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
    let rows = envelope
        .ui_layout
        .visuals
        .iter()
        .map(|v| {
            VirtualRowSpec::new(
                v.id.clone(),
                ByteOffset(v.to),
                if v.kind == "spiral" { 12 } else { 8 },
            )
        })
        .collect::<Vec<_>>();
    editor.set_virtual_rows(editor.revision(), rows).unwrap();
    let map = editor.screen_map(grid).unwrap();
    let mut minimap = Minimap::default();
    minimap
        .sync_editor(&editor, pane_region.minimap)
        .expect("minimap");
    let theme = Theme::built_in_default();
    let master = MasterState::new(Instant::now());
    let devices = DeviceInventory {
        audio_outputs: vec![DeviceEntry {
            name: "MacBook Pro Speakers".into(),
            id: String::new(),
            detail: "48000 Hz · 2ch".into(),
            is_default: true,
        }],
        midi_outputs: vec![DeviceEntry {
            name: "IAC Driver Bus 1".into(),
            id: String::new(),
            detail: String::new(),
            is_default: false,
        }],
        ..DeviceInventory::default()
    };
    let chips = vec![
        SceneChip {
            replay: false,
            prebake: None,
            name: "intro".into(),
            current: true,
            playing: true,
            dirty: true,
            rewind: false,
            pad: Some("c1/10".into()),
            errors: false,
            armed: None,
        },
        SceneChip {
            replay: false,
            prebake: None,
            name: "drop".into(),
            current: false,
            playing: false,
            dirty: false,
            rewind: false,
            pad: None,
            errors: false,
            armed: None,
        },
    ];
    let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
    terminal
        .draw(|frame| {
            view::render(
                frame,
                StudioView {
                    show_scrollbars: true,
                    prebake_rows: Default::default(),
                    keybind_rows: Vec::new(),
                    sets_folder: String::new(),
                    recordings_folder: String::new(),
                    set_limiter: None,
                    sample_cache: String::new(),
                    precache: None,
                    sources: Vec::new(),
                    mappings: Default::default(),
                    latency: None,
                    midi_rows: Vec::new(),
                    clock_in: None,
                    clock_out: None,
                    caching_samples: 0,
                    importing_sources: 0,
                    library_loading: false,
                    preview_shape: None,
                    focused_panel: None,
                    help_open: false,
                    preview_gain: 1.0,
                    sounding_note: None,
                    audition_loading: None,
                    panes: vec![PaneView {
                        replay: None,
                        prebake: None,
                        editor: &editor,
                        map: &map,
                        minimap: &minimap,
                        decorations: Decorations::default(),
                        name: "intro".into(),
                        dirty: true,
                        playing: true,
                        focused: true,
                        flash: false,
                        locate_flash: None,
                        timeline_focus: false,
                        line_numbers: true,
                    }],
                    visual: &visual,
                    devices: &devices,
                    scanning: false,
                    panel: None,
                    scenes: &chips,
                    strip_mode: &SceneStripMode::Idle,
                    reference: None,
                    log: None,
                    jobs: None,
                    memory: None,
                    log_memory: None,
                    export: None,
                    theme_picker: None,
                    set_panel: None,
                    set_prompt: None,
                    viz: [None, None],
                    viz_focus: 0,
                    mixer: None,
                    mixer_panel: None,
                    mixer_selection: None,
                    reference_on_top: false,
                    theme_camera: None,
                    settings: None,
                    snippet_picture: false,
                    snippet_refused: None,
                    #[cfg(feature = "hydra")]
                    snippet_playing: None,
                    #[cfg(feature = "hydra")]
                    snippet_preview_status: None,
                    #[cfg(feature = "hydra")]
                    hydra_webcam: None,
                },
                StudioChrome {
                    caret_visible: true,
                    keybinds: &rustel_studio::keybinds::Keybinds::default(),
                    registry: rustel_runtime::capability_registry(),
                    build_features: &[],
                    replay: false,
                    menus: &[],
                    menu: None,
                    prebake: None,
                    path: Path::new("live.strudel"),
                    set_name: "",
                    theme: &theme,
                    playing: true,
                    motion: rustel_studio::viz_panel::Motion::Live,
                    stopping: false,
                    evaluating: false,
                    dirty: true,
                    status: "generation 3 - playback started at cycle zero",
                    remote_control: false,
                    remote_receiving: false,
                    piano: None,
                    piano_notes: None,
                    error: None,
                    audio_warning: None,
                    capabilities: KeyboardCapabilities::enhanced(),
                    fps: 60.0,
                    caret: None,
                    render_ms: 0.8,
                    cycle: Some(12.25),
                    cps: Some(0.5),
                    zen: false,
                    show_menu: true,
                    show_header: true,
                    show_footer: true,
                    evaluation_flash: false,
                    focus_flash: None,
                    stats: ProcessStats {
                        cpu_percent: Some(9.0),
                        machine_cpu_percent: Some(35.0),
                        resident_bytes: Some(96 * 1024 * 1024),
                        footprint_bytes: None,
                    },
                    metric_detail: rustel_studio::settings::MetricDetail::Basic,
                    pressure: None,
                    max_polyphony_override: None,
                    master: &master,
                    master_limiter: None,
                    audio: None,
                    audition_audio: None,
                    lint: None,
                    go: view::GoState::Ready,
                    #[cfg(feature = "hydra")]
                    webcam: None,
                    recording: None,
                    take_notice: None,
                    toast: None,
                    unseen_warnings: 0,
                    exporting: None,
                    jobs: None,
                    launch: None,
                    loading: None,
                    clock_in: None,
                    clock_out: false,
                    orbits: &[],
                    output_pairs: 1,
                    now: Instant::now(),
                    device: Some("MacBook Pro Speakers"),
                    input: None,
                    input_chosen: false,
                    device_info: None,
                    midi_ports: MidiPortCounts {
                        outputs: 1,
                        inputs: 0,
                    },
                    pads: 0,
                    pad_active: false,
                    midi_active: false,
                    input_active: false,
                    input_peak_db: -60.0,
                },
            );
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| {
        (0..size.width)
            .filter_map(|x| buffer.cell((x, y)))
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    let screen = (0..size.height).map(row).collect::<Vec<_>>().join("\n");

    assert!(screen.contains("120.0 bpm"), "tempo is missing:\n{screen}");
    assert!(screen.contains("cpu 9%") && screen.contains("mem 96.0MB"));
    assert!(screen.contains("MASTER"), "the master dock is missing");
    assert!(screen.contains("♪ MacBook Pro Speakers"));
    assert!(screen.contains("⌁ 1 out · 0 in"));
    // The footer's shortcut strip is gone; the menu bar carries the commands
    // now, and menu.rs renders and hit-tests it under its own tests.
    assert!(
        screen.contains("▶1 intro ♪c1/10 ●") && screen.contains("2 drop"),
        "the scene strip is missing:\n{screen}"
    );
    assert!(
        !screen.contains("SPIRAL"),
        "there is no stage any more; every painter is inline"
    );

    // Bars are solid blocks; lines are the tier's raster - quadrants on
    // the default Cells tier, Braille on Fine - and the minimap is always
    // Braille.
    let braille = screen
        .chars()
        .filter(|glyph| ('\u{2800}'..='\u{28ff}').contains(glyph))
        .count();
    assert!(braille > 5, "expected the Braille minimap, saw {braille}");
    let quadrants = screen
        .chars()
        .filter(|glyph| ('\u{2596}'..='\u{259f}').contains(glyph) || *glyph == '▌' || *glyph == '▐')
        .count();
    assert!(
        braille > 20 || quadrants > 5,
        "expected drawn lines (Braille {braille} / quadrants {quadrants})"
    );
    let blocks = screen
        .chars()
        .filter(|glyph| matches!(glyph, '█' | '▀' | '▄'))
        .count();
    // Silent notes are thin strokes (half blocks), the sounding one a
    // full bar, with a gap after each: a run of hits, not a slab.
    assert!(blocks > 12, "expected block-glyph bars, saw {blocks}");

    // The inline piano roll occupies its own rows under its call site, and
    // the spiral sits under its own line below that.
    let inline = (4..12).map(row).collect::<Vec<_>>().join("\n");
    assert!(inline.contains('┊'), "inline widget rail is missing");
    assert!(
        inline.chars().any(|glyph| matches!(glyph, '█' | '▀' | '▄')),
        "the inline piano roll drew nothing:\n{inline}"
    );
}

/// `keep` spares a rect from the backdrop entirely.
#[test]
#[cfg(feature = "hydra")]
fn a_kept_rect_is_spared_the_backdrop_entirely() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use rustel_studio::view::HydraBackdrop;

    let area = Rect::new(0, 0, 40, 10);
    let panel = Rect::new(24, 0, 16, 10);
    let under = Color::Rgb(10, 10, 20);

    let mut buffer = Buffer::empty(area);
    buffer.set_style(area, ratatui::style::Style::default().bg(under));
    // The panel draws its own surface first, as the reference column does.
    let surface = Color::Rgb(34, 29, 50);
    buffer.set_style(panel, ratatui::style::Style::default().bg(surface));

    // A bright picture, so anything bleeding through is obvious.
    let picture = HydraBackdrop {
        width: 4,
        height: 4,
        rgba: vec![255; 4 * 4 * 4],
        strength: 0.55,
        // The reference column is spared whatever this says, which is the
        // point of the assertion below.
        interface: 1.0,
    };
    picture.paint(&mut buffer, area, under, Some(panel), &[], None);

    for y in 0..panel.height {
        for x in 0..panel.width {
            let cell = buffer.cell((panel.x + x, panel.y + y)).expect("cell");
            assert_eq!(
                cell.bg, surface,
                "the panel keeps its own surface at {x},{y}"
            );
        }
    }
    // Outside it, the picture is there.
    let outside = buffer.cell((2, 2)).expect("cell");
    assert_ne!(outside.bg, under, "the backdrop paints outside the panel");
}

/// Every sheet paints over the docked panels: a visuals dock at the
/// right and an export sheet across the bottom share cells, and the
/// sheet's frame is what shows there.
#[test]
fn a_sheet_paints_over_a_visuals_dock() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use rustel_runtime::ProcessStats;
    use rustel_studio::devices::{DeviceInventory, MidiPortCounts};
    use rustel_studio::editor::{Editor, KeyboardCapabilities};
    use rustel_studio::export::{ExportSettings, ExportSheet, ExportSheetView};
    use rustel_studio::meter::MasterState;
    use rustel_studio::minimap::Minimap;
    use rustel_studio::theme::Theme;
    use rustel_studio::view::{
        self, Decorations, PaneView, SceneStripMode, StudioChrome, StudioView, VizView,
    };
    use rustel_studio::visuals::VisualState;
    use rustel_studio::viz_panel::{Edge, VizPanel, WidgetKind, WidgetSpec};
    use std::path::Path;
    use std::time::Instant;

    let mut editor = Editor::new("$: s(\"bd\")\n").unwrap();
    let size = Rect::new(0, 0, 150, 40);
    let visual = VisualState::default();
    let dock = VizPanel {
        edge: Edge::Right,
        ..VizPanel::default()
    };
    let widgets = [WidgetSpec::new(WidgetKind::Scope)];
    let layout = view::regions_with(
        size,
        false,
        1,
        false,
        true,
        None,
        [
            Some(rustel_studio::viz_panel::Dock {
                edge: Edge::Right,
                extent: 36,
            }),
            None,
        ],
        None,
        None,
    );
    let pane_region = layout.panes[0];
    let grid = view::source_grid(&editor, pane_region.editor, true);
    editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
    let map = editor.screen_map(grid).unwrap();
    let mut minimap = Minimap::default();
    minimap
        .sync_editor(&editor, pane_region.minimap)
        .expect("minimap");
    let theme = Theme::built_in_default();
    let master = MasterState::new(Instant::now());
    let devices = DeviceInventory::default();
    let directory = tempfile::tempdir().unwrap();
    let sheet = ExportSheet::open(
        "intro".into(),
        ExportSettings::default(),
        directory.path(),
        0,
    );
    let sheet_area = ExportSheetView::geometry(size).expect("room for the sheet");
    let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
    terminal
        .draw(|frame| {
            view::render(
                frame,
                StudioView {
                    show_scrollbars: true,
                    prebake_rows: Default::default(),
                    keybind_rows: Vec::new(),
                    sets_folder: String::new(),
                    recordings_folder: String::new(),
                    set_limiter: None,
                    sample_cache: String::new(),
                    precache: None,
                    sources: Vec::new(),
                    mappings: Default::default(),
                    latency: None,
                    midi_rows: Vec::new(),
                    clock_in: None,
                    clock_out: None,
                    caching_samples: 0,
                    importing_sources: 0,
                    library_loading: false,
                    preview_shape: None,
                    focused_panel: None,
                    help_open: false,
                    preview_gain: 1.0,
                    sounding_note: None,
                    audition_loading: None,
                    panes: vec![PaneView {
                        replay: None,
                        prebake: None,
                        editor: &editor,
                        map: &map,
                        minimap: &minimap,
                        decorations: Decorations::default(),
                        name: "intro".into(),
                        dirty: false,
                        playing: false,
                        focused: true,
                        flash: false,
                        locate_flash: None,
                        timeline_focus: false,
                        line_numbers: true,
                    }],
                    visual: &visual,
                    devices: &devices,
                    scanning: false,
                    panel: None,
                    scenes: &[],
                    strip_mode: &SceneStripMode::Idle,
                    reference: None,
                    log: None,
                    jobs: None,
                    memory: None,
                    log_memory: None,
                    export: Some(&sheet),
                    theme_picker: None,
                    set_panel: None,
                    set_prompt: None,
                    viz: [
                        Some(VizView {
                            panel: &dock,
                            widgets: &widgets,
                            set_name: "set",
                            level: 0.0,
                            seconds: 1.0,
                            extent: 36,
                        }),
                        None,
                    ],
                    viz_focus: 0,
                    mixer: None,
                    mixer_panel: None,
                    mixer_selection: None,
                    reference_on_top: false,
                    theme_camera: None,
                    settings: None,
                    snippet_picture: false,
                    snippet_refused: None,
                    #[cfg(feature = "hydra")]
                    snippet_playing: None,
                    #[cfg(feature = "hydra")]
                    snippet_preview_status: None,
                    #[cfg(feature = "hydra")]
                    hydra_webcam: None,
                },
                StudioChrome {
                    caret_visible: true,
                    keybinds: &rustel_studio::keybinds::Keybinds::default(),
                    replay: false,
                    menus: &[],
                    menu: None,
                    prebake: None,
                    path: Path::new("live.strudel"),
                    set_name: "set",
                    theme: &theme,
                    playing: false,
                    motion: rustel_studio::viz_panel::Motion::Live,
                    input: None,
                    input_chosen: false,
                    stopping: false,
                    evaluating: false,
                    dirty: false,
                    status: "",
                    remote_control: false,
                    remote_receiving: false,
                    piano: None,
                    piano_notes: None,
                    error: None,
                    audio_warning: None,
                    capabilities: KeyboardCapabilities::enhanced(),
                    fps: 60.0,
                    caret: None,
                    render_ms: 0.8,
                    cycle: None,
                    cps: None,
                    zen: false,
                    show_menu: true,
                    show_header: true,
                    show_footer: true,
                    evaluation_flash: false,
                    focus_flash: None,
                    stats: ProcessStats {
                        cpu_percent: None,
                        machine_cpu_percent: None,
                        resident_bytes: None,
                        footprint_bytes: None,
                    },
                    metric_detail: rustel_studio::settings::MetricDetail::Basic,
                    pressure: None,
                    max_polyphony_override: None,
                    master: &master,
                    master_limiter: None,
                    audio: None,
                    audition_audio: None,
                    lint: None,
                    go: view::GoState::Ready,
                    #[cfg(feature = "hydra")]
                    webcam: None,
                    recording: None,
                    take_notice: None,
                    toast: None,
                    unseen_warnings: 0,
                    exporting: None,
                    jobs: None,
                    launch: None,
                    loading: None,
                    clock_in: None,
                    clock_out: false,
                    orbits: &[],
                    output_pairs: 1,
                    now: Instant::now(),
                    device: None,
                    device_info: None,
                    midi_ports: MidiPortCounts::default(),
                    pads: 0,
                    pad_active: false,
                    midi_active: false,
                    input_active: false,
                    input_peak_db: -60.0,
                    registry: rustel_runtime::capability_registry(),
                    build_features: &[],
                },
            );
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let dock_area = layout.viz[0];
    assert!(!dock_area.is_empty(), "the dock has its column");
    assert!(
        sheet_area.right() > dock_area.x && sheet_area.bottom() > dock_area.y,
        "the sheet and the dock share cells"
    );
    // The sheet's top-right corner lies in the dock's column and is the
    // sheet's frame, not the dock's rule or its picture.
    let corner = buffer
        .cell((sheet_area.right() - 1, sheet_area.y))
        .expect("cell");
    assert!(
        matches!(corner.symbol(), "╮" | "┐" | "─"),
        "the sheet's frame shows over the dock: {:?}",
        corner.symbol()
    );
    let rule = buffer.cell((dock_area.x, sheet_area.y + 1)).expect("cell");
    assert_ne!(
        rule.symbol(),
        "│",
        "the dock's rule does not cut through the sheet"
    );
}
