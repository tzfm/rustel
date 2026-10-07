//! Screen regions and layout geometry for Studio.
//!
//! The calculation is pure: a frame size and the requested docks produce
//! the regions used by both rendering and pointer hit testing.

use ratatui::layout::Rect;

use super::super::{set_panel, viz_panel};

/// Narrowest source pane that still gets a minimap.
const MINIMAP_MIN_EDITOR_WIDTH: u16 = 64;
/// Shortest terminal that gets a scene strip between header and source.
const SCENES_MIN_HEIGHT: u16 = 9;
/// Shortest terminal that gets a rule under the scene strip.
///
/// Drawn in a split, and only where the panes start directly under the
/// strip. The chips are tabs and a pane's title is not, but in a split
/// each pane wears one and the two rows read as a stack of tabs saying
/// different things. A dim line ends the tabs and lets the panes begin;
/// with one pane there is one title and nothing to confuse it with, and
/// with a band docked across the top - the mixer, a visuals panel - that
/// panel's own edge already ends the tabs and a second line would be one
/// rule too many. Either way the row goes back to the source.
const SCENES_RULE_MIN_HEIGHT: u16 = SCENES_MIN_HEIGHT + 2;
/// Shortest terminal that gets a menu bar.
///
/// Six is the first height where taking a row still leaves the score two: at
/// five the footer has just grown to two rows and is the worst place to take
/// another, and below that every degraded layout stays exactly as it is today.
const MENU_MIN_HEIGHT: u16 = 6;

/// Narrowest editor pane worth keeping beside the reference column; below
/// this the reference overlays the rightmost pane instead.
pub(super) const PANE_MIN_WIDTH_BESIDE_REFERENCE: u16 = 50;

/// One editor pane: an optional title row, the source text and its minimap.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PaneRegion {
    /// Name row above the source; empty in single-pane mode.
    pub title: Rect,
    /// Source text, excluding the minimap column.
    pub editor: Rect,
    /// The unwrapped editor's horizontal scrollbar, reserved below its text.
    pub horizontal_scrollbar: Rect,
    /// Minimap/scrollbar column on the right of the source pane.
    pub minimap: Rect,
    /// The replay timeline over the text, when the pane shows a tape;
    /// empty otherwise. `editor` already excludes it.
    pub timeline: Rect,
    /// A one-cell scrollbar at the pane's right edge, there when the
    /// minimap is off; empty otherwise. `editor` already excludes it.
    pub scrollbar: Rect,
}

impl PaneRegion {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        [
            self.title,
            self.editor,
            self.horizontal_scrollbar,
            self.minimap,
            self.timeline,
            self.scrollbar,
        ]
        .iter()
        .any(|rect| !rect.is_empty() && rect.contains((x, y).into()))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StudioRegions {
    /// The menu bar's own row, above everything. Empty on a terminal too
    /// short to spare it, when the menu-bar setting is off, and in zen mode.
    pub menu: Rect,
    /// The rustel PLAYING tempo line. Empty when the header setting is off,
    /// on a terminal too short to spare it, and in zen mode. Not the menu
    /// bar, and not the scene tabs.
    pub header: Rect,
    /// The scene strip, one row under the header.
    pub scenes: Rect,
    /// One or two editor panes, left to right.
    pub panes: [PaneRegion; 2],
    pub pane_count: usize,
    /// The set panel's column, when it is docked at either edge.
    pub sidebar: Rect,
    /// Temporarily yield to the reference, without falling back to a sheet.
    pub sidebar_hidden: bool,
    /// Which edge the column is at.
    pub sidebar_side: Side,
    /// The visuals docks' room, when there is any: a column down a side
    /// or a band across the top or the bottom, by the edge asked for.
    pub viz: [Rect; viz_panel::DOCKS],
    pub viz_edge: [viz_panel::Edge; viz_panel::DOCKS],
    /// The mixer panel's band, when it is open: outermost of the bands,
    /// along the top or the bottom.
    pub mixer: Rect,
    /// The sticky log's room, when it is docked rather than a sheet: a
    /// band along the top or the bottom, carved at the same outermost
    /// tier as the mixer - inside it on a shared edge - so the two
    /// fixtures never draw over each other.
    pub log: Rect,
    /// The memory breakdown's room, while it is open: a band along the
    /// top or the bottom, carved at the fixtures' tier right after the
    /// log - inside the desk and the log on a shared edge, and the first
    /// of the three to give up rows. In zen, a band laid over the score.
    pub memory: Rect,
    /// The reference column, when it is open.
    pub reference: Rect,
    /// True when the reference sits over the rightmost pane rather than
    /// beside it, because the terminal is too narrow for both.
    pub reference_overlays: bool,
    /// The notice rows, the status line and, while the footer setting is
    /// on, the meter, orbits and device chips. Empty in zen.
    pub footer: Rect,
}

impl StudioRegions {
    /// The pane a point falls in, if any.
    pub fn pane_at(&self, x: u16, y: u16) -> Option<usize> {
        (0..self.pane_count).find(|&index| self.panes[index].contains(x, y))
    }
}

/// Which of the ordinary chrome rows the layout should carve: the menu bar,
/// the header, the footer's meter and chip row. Zen hides all of them
/// regardless of the other three, which are the player's lasting display
/// preferences - leaving zen restores exactly these, not the factory
/// defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChromeLayout {
    pub zen: bool,
    pub menu: bool,
    pub header: bool,
    pub footer: bool,
}

impl ChromeLayout {
    /// Every chrome row shown, with zen on top when set.
    pub fn shown(zen: bool) -> Self {
        Self {
            zen,
            menu: true,
            header: true,
            footer: true,
        }
    }

    pub fn from_settings(settings: &super::super::settings::UiSettings) -> Self {
        Self {
            zen: settings.zen,
            menu: settings.show_menu,
            header: settings.show_header,
            footer: settings.show_footer,
        }
    }
}

/// The menu bar's row for a terminal this size, without computing the rest
/// of the layout.
///
/// A pure function of the frame, and the single source of the threshold, so
/// that hit-testing, key routing and painting cannot disagree about whether
/// there is a bar. `regions` calls it too. It does not read
/// `StudioRegions`, which is refreshed only inside a draw. A gate on that
/// field means "no menu" before the first frame and in every test that
/// never draws.
pub fn menu_row(area: Rect, chrome: ChromeLayout) -> Rect {
    if chrome.zen || !chrome.menu || area.height < MENU_MIN_HEIGHT || area.width == 0 {
        return Rect::default();
    }
    Rect::new(area.x, area.y, area.width, 1)
}

/// The least a pane may be beside the docked set panel: narrower than
/// that, the panel comes up as a sheet instead.
const PANE_MIN_WIDTH_BESIDE_SIDEBAR: u16 = 40;

/// Which edge of the editor the set panel docks at.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Side {
    Left,
    #[default]
    Right,
}

/// Rows a band leaves the body at least, or it yields.
const BAND_MIN_BODY_HEIGHT: u16 = 8;

/// The mixer's band in zen: laid across the whole frame rather than
/// carved from a shrunk body, so it overlays the editor instead of taking
/// room from it - a popup, not a dock. The same floor `mixer_panel::parts`
/// itself draws under: shorter than this there is nothing to lay a strip
/// or a fader row in, so an empty rect answers "no room" the way a docked
/// mixer's own gate does.
fn zen_popup_band(area: Rect, dock: viz_panel::Dock) -> Rect {
    use viz_panel::Edge;
    if area.height < 5 || area.width == 0 {
        return Rect::default();
    }
    let height = dock.extent.min(area.height);
    match dock.edge {
        Edge::Top => Rect::new(area.x, area.y, area.width, height),
        _ => Rect::new(
            area.x,
            area.bottom().saturating_sub(height),
            area.width,
            height,
        ),
    }
}

/// The rows a fixture's band - the docked log's, the memory breakdown's -
/// gets out of a body `body_height` rows tall, having asked for `asked`:
/// all of it while the panes keep their least, and less when they would
/// not, down to the least a band is drawn at. `None` below that: no band
/// at all.
///
/// Shrunk rather than refused because what it holds scrolls: a log given
/// eleven rows of the thirteen it asked for is still the log, where a band
/// of widgets laid out for its height is not, and the visuals docks and the
/// mixer keep their all-or-nothing gate.
fn fixture_rows(body_height: u16, asked: u16) -> Option<u16> {
    let spare = body_height.saturating_sub(BAND_MIN_BODY_HEIGHT);
    let least = asked.min(viz_panel::BAND_MIN_HEIGHT);
    (asked > 0 && spare >= least).then(|| asked.min(spare))
}

/// A band `rows` tall taken off `body` along the top or the bottom, the
/// whole width; `body` keeps the rest.
fn carve_band(body: &mut Rect, edge: viz_panel::Edge, rows: u16) -> Rect {
    let rows = rows.min(body.height);
    let band = match edge {
        viz_panel::Edge::Top => {
            let band = Rect::new(body.x, body.y, body.width, rows);
            body.y = body.y.saturating_add(rows);
            band
        }
        _ => Rect::new(body.x, body.bottom().saturating_sub(rows), body.width, rows),
    };
    body.height = body.height.saturating_sub(rows);
    band
}

/// `minimap`: whether panes wide enough get one; without it a pane keeps
/// a scrollbar instead. `sidebar`: the side the set panel is open on, and
/// so wants its column at when the body is wide enough. `viz`: the same
/// for the visuals panel, which sits outside the set panel when both are
/// on one side, and yields first when the body cannot hold them all.
pub fn regions(
    area: Rect,
    zen: bool,
    pane_count: usize,
    reference_open: bool,
    minimap: bool,
    sidebar: Option<Side>,
    viz: Option<Side>,
) -> StudioRegions {
    use viz_panel::{Dock, Edge, VIZ_WIDTH};
    let column = viz.map(|side| Dock {
        edge: match side {
            Side::Left => Edge::Left,
            Side::Right => Edge::Right,
        },
        extent: VIZ_WIDTH,
    });
    regions_with(
        area,
        zen,
        pane_count,
        reference_open,
        minimap,
        sidebar,
        [column, None],
        None,
        None,
    )
}

/// [`regions`] with the visuals docks as the app asks for them: each an
/// edge and an extent - cells across for a column, rows for a band.
/// Bands are carved first, the whole width, so a band across the top
/// runs over the columns; then the columns in dock order, the first
/// outermost when both share a side.
// One argument a thing on the screen: the layout is the one place they all
// meet, and a struct of eight booleans-and-rects would be the same list
// with a name in front of it.
#[allow(clippy::too_many_arguments)]
pub fn regions_with(
    area: Rect,
    zen: bool,
    pane_count: usize,
    reference_open: bool,
    minimap: bool,
    sidebar: Option<Side>,
    docks: [Option<viz_panel::Dock>; viz_panel::DOCKS],
    mixer: Option<viz_panel::Dock>,
    log: Option<viz_panel::Dock>,
) -> StudioRegions {
    regions_with_footer(
        area,
        ChromeLayout::shown(zen),
        pane_count,
        reference_open,
        minimap,
        sidebar.map(|side| SetSidebar {
            side,
            width: set_panel::SIDEBAR_WIDTH,
        }),
        docks,
        mixer,
        log,
        None,
        0,
        false,
    )
}

/// The set browser's requested side and width.
#[derive(Clone, Copy, Debug)]
pub struct SetSidebar {
    pub side: Side,
    pub width: u16,
}

/// The full studio layout: the docked memory breakdown with the other
/// fixtures, and separate rows for visible notices and retained piano notes
/// above the ordinary two-row footer.
#[allow(clippy::too_many_arguments)]
pub fn regions_with_footer(
    area: Rect,
    chrome: ChromeLayout,
    pane_count: usize,
    reference_open: bool,
    minimap: bool,
    sidebar: Option<SetSidebar>,
    docks: [Option<viz_panel::Dock>; viz_panel::DOCKS],
    mixer: Option<viz_panel::Dock>,
    log: Option<viz_panel::Dock>,
    memory: Option<viz_panel::Dock>,
    notice_rows: u16,
    has_piano_notes: bool,
) -> StudioRegions {
    let mut docks = docks;
    let mut kept_sidebar = sidebar;
    let layout = |docks, sidebar| {
        layout_regions(
            area,
            chrome,
            pane_count,
            reference_open,
            minimap,
            sidebar,
            docks,
            mixer,
            log,
            memory,
            notice_rows,
            has_piano_notes,
        )
    };
    let mut regions = layout(docks, kept_sidebar);
    if !chrome.zen && reference_open {
        // Keep the reference beside readable code. Temporarily give it the
        // space used by secondary columns: visuals first, then the set.
        // Requests remain unchanged, so widening or closing reference restores
        // the user's arrangement automatically. Horizontal bands - the docked
        // log and the memory breakdown are only ever bands - keep their room.
        for index in (0..docks.len()).rev() {
            if regions.reference_overlays && docks[index].is_some_and(|dock| dock.edge.is_column())
            {
                docks[index] = None;
                regions = layout(docks, kept_sidebar);
            }
        }
        if regions.reference_overlays && kept_sidebar.is_some() {
            kept_sidebar = None;
            regions = layout(docks, kept_sidebar);
        }
        regions.sidebar_hidden = sidebar.is_some() && regions.sidebar.is_empty();
        regions.sidebar_side = sidebar.map_or(Side::default(), |panel| panel.side);
    }
    regions
}

#[allow(clippy::too_many_arguments)]
fn layout_regions(
    area: Rect,
    chrome: ChromeLayout,
    pane_count: usize,
    reference_open: bool,
    minimap: bool,
    sidebar: Option<SetSidebar>,
    docks: [Option<viz_panel::Dock>; viz_panel::DOCKS],
    mixer: Option<viz_panel::Dock>,
    log: Option<viz_panel::Dock>,
    memory: Option<viz_panel::Dock>,
    notice_rows: u16,
    has_piano_notes: bool,
) -> StudioRegions {
    use viz_panel::Edge;
    let pane_count = pane_count.clamp(1, 2);
    if chrome.zen {
        // Zen mode shows the source with no menu, header, scene strip,
        // footer, minimap or docked sidebar/viz column. A split still
        // works, because ^E is not chrome: it shows the score in two
        // places. Two panes share the width as they do outside zen. Zen
        // adds nothing else for the second pane.
        let mut regions = StudioRegions {
            pane_count,
            ..StudioRegions::default()
        };
        let left_width = if pane_count == 2 {
            area.width.div_ceil(2)
        } else {
            area.width
        };
        let widths = [left_width, area.width.saturating_sub(left_width)];
        let mut x = area.x;
        for (index, pane) in regions.panes.iter_mut().enumerate().take(pane_count) {
            pane.editor = Rect::new(x, area.y, widths[index], area.height);
            x = x.saturating_add(widths[index]);
        }
        // The reference is still a tool the score can summon in zen. Keep
        // the editor at full size and paint the browser over its right edge,
        // just as the narrow normal layout does, so Ctrl+D never focuses an
        // invisible zero-sized panel.
        if reference_open && area.width >= 24 {
            let width = ((u32::from(area.width) * 38 / 100) as u16)
                .clamp(36, 72)
                .min(area.width);
            regions.reference = Rect::new(
                area.right().saturating_sub(width),
                area.y,
                width,
                area.height,
            );
            regions.reference_overlays = true;
        }
        regions.sidebar_side = sidebar.map_or(Side::default(), |panel| panel.side);
        // The sidebar column is left empty on purpose: `SetPanel` already
        // reads an empty sidebar as "draw the sheet form instead", which is
        // exactly what a popup with no docked room of its own should do.
        //
        // The mixer has no sheet form, only a band along an edge. Zen lays
        // that band across the whole frame and does not carve it from a
        // pane. The editor keeps its room and the desk paints over it, as
        // every other zen popup does.
        if let Some(dock) = mixer {
            regions.mixer = zen_popup_band(area, dock);
        }
        // The memory breakdown is a popup band over the score too, because
        // zen has no docked panels. On an edge it shares with the desk it
        // stacks inside the desk, as it does outside zen.
        if let Some(dock) = memory {
            let mut room = area;
            if !regions.mixer.is_empty() && mixer.is_some_and(|desk| desk.edge == dock.edge.band())
            {
                carve_band(&mut room, dock.edge.band(), regions.mixer.height);
            }
            regions.memory = zen_popup_band(
                room,
                viz_panel::Dock {
                    edge: dock.edge.band(),
                    ..dock
                },
            );
        }
        return regions;
    }
    let menu = menu_row(area, chrome);
    let menu_height = menu.height;
    let header_height = u16::from(chrome.header && area.height >= 3);
    let chips_height = u16::from(area.height >= SCENES_MIN_HEIGHT);
    // The footer setting takes only the meter and chip row: the notices,
    // retained piano notes and the status line keep theirs.
    let footer_height = if area.height >= 5 {
        1 + u16::from(chrome.footer) + notice_rows.min(2) + u16::from(has_piano_notes)
    } else {
        1
    }
    .min(
        area.height
            .saturating_sub(menu_height + header_height + chips_height),
    );
    // Whether anything is going to be laid across the top of the body,
    // between the strip and the panes. Measured against the body the
    // strip would leave WITHOUT its rule, which is the larger of the two:
    // a band that does not fit in that one does not fit in either, so the
    // answer cannot change under its own weight.
    let banded_top = {
        let without_rule = area
            .height
            .saturating_sub(menu_height)
            .saturating_sub(header_height)
            .saturating_sub(chips_height)
            .saturating_sub(footer_height);
        let lands = |dock: Option<viz_panel::Dock>| {
            dock.is_some_and(|dock| {
                matches!(dock.edge, Edge::Top) && without_rule >= dock.extent + BAND_MIN_BODY_HEIGHT
            })
        };
        // A fixture's band shrinks to fit, so it lands wherever its least
        // does.
        let fixture_lands = |dock: Option<viz_panel::Dock>| {
            dock.is_some_and(|dock| {
                matches!(dock.edge.band(), Edge::Top)
                    && fixture_rows(without_rule, dock.extent).is_some()
            })
        };
        lands(mixer)
            || fixture_lands(log)
            || fixture_lands(memory)
            || docks.iter().copied().any(lands)
    };
    let scenes_height = chips_height
        + u16::from(pane_count > 1 && !banded_top && area.height >= SCENES_RULE_MIN_HEIGHT);
    let chrome_top = area.y.saturating_add(menu_height);
    let scenes = Rect::new(
        area.x,
        chrome_top.saturating_add(header_height),
        area.width,
        scenes_height,
    );
    let body_y = scenes.bottom();
    let body_height = area
        .height
        .saturating_sub(menu_height)
        .saturating_sub(header_height)
        .saturating_sub(scenes_height)
        .saturating_sub(footer_height);
    let footer = Rect::new(
        area.x,
        body_y.saturating_add(body_height),
        area.width,
        footer_height,
    );
    let header = Rect::new(area.x, chrome_top, area.width, header_height);
    let mut body = Rect::new(area.x, body_y, area.width, body_height);

    // The docks take the edges: a band the whole width across the top or
    // the bottom, a column down a side - the first dock outermost - with
    // the set panel inside the columns when it shares their side, the
    // set panel at the left the way an editor's file tree is, or at the
    // right by the setting, and the reference column inside them all,
    // taking its room from the panes and never covering a panel. The
    // panes keep a minimum: the visuals yield first, being the ornament;
    // the set panel next, becoming a sheet.
    let least = PANE_MIN_WIDTH_BESIDE_SIDEBAR * pane_count as u16;
    // The mixer's band goes first, outermost: the desk is a fixture along
    // the bottom (or the top), and the reference, standing the stage's
    // full height, stops at it rather than covering the strips.
    let mut mixer_area = Rect::default();
    if let Some(dock) = mixer
        && !dock.edge.is_column()
        && body.height >= dock.extent + BAND_MIN_BODY_HEIGHT
    {
        let height = dock.extent;
        mixer_area = match dock.edge {
            Edge::Top => {
                let band = Rect::new(body.x, body.y, body.width, height);
                body.y = body.y.saturating_add(height);
                band
            }
            _ => Rect::new(
                body.x,
                body.bottom().saturating_sub(height),
                body.width,
                height,
            ),
        };
        body.height = body.height.saturating_sub(height);
    }
    // A sticky log carves right after the mixer, at the same outermost
    // tier: two application-wide fixtures, neither of which is ornament
    // the way a visuals dock is, so the reference stops at this one too
    // rather than standing over it. On the same edge it stacks inside the
    // desk. Always a band, and shrunk to leave the panes their least
    // rather than dropped (`fixture_rows`).
    let mut log_area = Rect::default();
    if let Some(dock) = log
        && let Some(rows) = fixture_rows(body.height, dock.extent)
    {
        log_area = carve_band(&mut body, dock.edge.band(), rows);
    }
    // The memory breakdown last of the three, innermost: the desk and the
    // log are what a set is played and watched through, the breakdown is
    // looked at now and then, so on a shared edge it is the one nearest
    // the score and, the room running short, the first to shrink.
    let mut memory_area = Rect::default();
    if let Some(dock) = memory
        && let Some(rows) = fixture_rows(body.height, dock.extent)
    {
        memory_area = carve_band(&mut body, dock.edge.band(), rows);
    }
    // The reference stands the stage's full height, over a band, so a
    // band across the bottom never cuts it short.
    let stage = body;
    let mut viz_area = [Rect::default(); viz_panel::DOCKS];
    let viz_edge = docks.map(|dock| dock.map_or(Edge::Right, |dock| dock.edge));
    for (index, dock) in docks.iter().enumerate() {
        let Some(dock) = dock else { continue };
        if dock.edge.is_column() || body.height < dock.extent + BAND_MIN_BODY_HEIGHT {
            continue;
        }
        let height = dock.extent;
        viz_area[index] = match dock.edge {
            Edge::Top => {
                let band = Rect::new(body.x, body.y, body.width, height);
                body.y = body.y.saturating_add(height);
                band
            }
            _ => Rect::new(
                body.x,
                body.bottom().saturating_sub(height),
                body.width,
                height,
            ),
        };
        body.height = body.height.saturating_sub(height);
    }
    for (index, dock) in docks.iter().enumerate() {
        let Some(dock) = dock else { continue };
        if !dock.edge.is_column()
            || body.width < dock.extent + sidebar.map_or(0, |panel| panel.width) + least
        {
            continue;
        }
        let width = dock.extent;
        viz_area[index] = match dock.edge {
            Edge::Left => {
                let column = Rect::new(body.x, body.y, width, body.height);
                body.x = body.x.saturating_add(width);
                column
            }
            _ => Rect::new(
                body.right().saturating_sub(width),
                body.y,
                width,
                body.height,
            ),
        };
        body.width = body.width.saturating_sub(width);
    }
    let mut sidebar_area = Rect::default();
    if let Some(panel) = sidebar
        && body.width >= set_panel::SIDEBAR_MIN_WIDTH + least
    {
        let width = panel.width.min(body.width.saturating_sub(least));
        match panel.side {
            Side::Left => {
                sidebar_area = Rect::new(body.x, body.y, width, body.height);
                body.x = body.x.saturating_add(width);
            }
            Side::Right => {
                sidebar_area = Rect::new(
                    body.right().saturating_sub(width),
                    body.y,
                    width,
                    body.height,
                );
            }
        }
        body.width = body.width.saturating_sub(width);
    }

    // The reference is a column on the right. When the panes beside it
    // would be too narrow to write in, it sits over the rightmost pane
    // instead and Esc gives the pane back.
    let mut reference = Rect::default();
    let mut reference_overlays = false;
    if reference_open && body.width >= 24 {
        let want = ((u32::from(body.width) * 38 / 100) as u16)
            .clamp(36, 72)
            .min(body.width);
        reference = Rect::new(
            body.right().saturating_sub(want),
            stage.y,
            want,
            stage.height,
        );
        if body.width.saturating_sub(want) >= PANE_MIN_WIDTH_BESIDE_REFERENCE * pane_count as u16 {
            body.width = body.width.saturating_sub(want);
        } else {
            reference_overlays = true;
        }
    }

    let mut panes = [PaneRegion::default(); 2];
    let title_height = u16::from(pane_count == 2 && body.height >= 4);
    let left_width = if pane_count == 2 {
        body.width.div_ceil(2)
    } else {
        body.width
    };
    let widths = [left_width, body.width.saturating_sub(left_width)];
    let mut x = body.x;
    for (index, pane) in panes.iter_mut().enumerate().take(pane_count) {
        let width = widths[index];
        let source = Rect::new(
            x,
            body.y.saturating_add(title_height),
            width,
            body.height.saturating_sub(title_height),
        );
        let minimap_width = if source.width >= MINIMAP_MIN_EDITOR_WIDTH && minimap {
            (source.width / 10).clamp(6, 14)
        } else {
            0
        };
        // With the minimap off a one-cell scrollbar keeps the right edge.
        let scrollbar_width = u16::from(!minimap && source.width >= SCROLLBAR_MIN_EDITOR_WIDTH);
        pane.title = Rect::new(x, body.y, width, title_height);
        pane.editor = Rect::new(
            source.x,
            source.y,
            source
                .width
                .saturating_sub(minimap_width)
                .saturating_sub(scrollbar_width),
            source.height,
        );
        pane.minimap = Rect::new(
            source.right().saturating_sub(minimap_width),
            source.y,
            minimap_width,
            source.height,
        );
        pane.scrollbar = Rect::new(
            source.right().saturating_sub(scrollbar_width),
            source.y,
            scrollbar_width,
            source.height,
        );
        x = x.saturating_add(width);
    }
    StudioRegions {
        menu,
        header,
        scenes,
        panes,
        pane_count,
        sidebar: sidebar_area,
        sidebar_hidden: false,
        sidebar_side: sidebar.map_or(Side::default(), |panel| panel.side),
        viz: viz_area,
        viz_edge,
        mixer: mixer_area,
        log: log_area,
        memory: memory_area,
        reference,
        reference_overlays,
        footer,
    }
}

/// Cells the scrollbar needs a pane to keep before it takes one of them.
const SCROLLBAR_MIN_EDITOR_WIDTH: u16 = 20;
