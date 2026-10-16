//! Rendering: paint terminal grids, manager overlays, and the name
//! prompt as escape-sequence streams into any `Write` (a client socket
//! buffer on the server, stdout in tests).

use std::io::Write;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    queue,
    style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate},
};

use libghostty_vt::{
    Terminal,
    render::{CellIterator, Dirty, RenderState, RowIterator},
    screen::CellWide,
    style::{RgbColor, StyleColor, Underline},
};

use std::collections::HashMap;

use crate::Result;
use crate::agent_status::AgentActivity;
use crate::model::{Layout, Rect, Session, SplitDir, split_rect};

/// One session drawn on the status bar.
pub struct BarSession {
    /// Index into the server's session vec when the session is running.
    /// Clicking the chip switches to it.
    pub index: Option<usize>,
    /// Pin slot when this chip is a configured session. Clicking a chip
    /// that is not running starts that pin.
    pub pin: Option<usize>,
    pub name: String,
    pub activity: Option<AgentActivity>,
    /// The session went working → idle and nobody is viewing it. Drawn
    /// with the `agent_finished_background` chip instead of a dim one.
    pub finished: bool,
}

/// Fill and text of a highlighted chip: the open session or tab
/// (`active_background` / `active_foreground`) or a just-finished agent
/// session (`agent_finished_background` / `agent_finished_foreground`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChipColor {
    pub background: Color,
    /// Text on the chip. Absent falls back to the bar's font color, or
    /// to reversed terminal text when that is unset too.
    pub foreground: Option<Color>,
}

impl ChipColor {
    /// A chip with only a fill; the text follows the bar.
    #[cfg(test)]
    pub fn fill(background: Color) -> Self {
        Self {
            background,
            foreground: None,
        }
    }

    /// The color for this signal when there is no chip behind it: the
    /// names in the managers. That is
    /// the brighter of the pair, so a bright fill with dark text and a
    /// dark fill with bright text both stay readable on a dark screen.
    /// Named palette colors have no known brightness; the fill wins.
    pub fn signal(&self) -> Color {
        match (self.foreground, luminance(self.background)) {
            (Some(fg), Some(bg)) if luminance(fg).is_some_and(|l| l > bg) => fg,
            _ => self.background,
        }
    }
}

/// A chip whose fill and text are both optional: the working-agent
/// chip, which looks like an ordinary chip until either is configured.
/// The open and finished chips convert into this to share one painter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChipStyle {
    pub background: Option<Color>,
    pub foreground: Option<Color>,
}

impl From<ChipColor> for ChipStyle {
    fn from(chip: ChipColor) -> Self {
        Self {
            background: Some(chip.background),
            foreground: chip.foreground,
        }
    }
}

/// Relative luminance (WCAG) of an RGB color; `None` for palette colors.
fn luminance(color: Color) -> Option<f64> {
    let Color::Rgb { r, g, b } = color else {
        return None;
    };
    let channel = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b))
}

/// What a click on the status bar landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarHit {
    /// A running session.
    Session(usize),
    /// A pinned session that is not running.
    Pin(usize),
    /// A tab of the session on screen.
    Tab(usize),
}

/// Where the status bar, the agent bar, and the pane area sit on one
/// client's screen.
///
/// `content` is the pane area's size (origin is always the pane area's
/// own top-left; `content_y` is where that lands on the screen). Each
/// bar is a contiguous run of screen rows. When both bars share an edge
/// the agent bar sits on the pane side: above the status bar at the
/// bottom, below it at the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    pub content: (u16, u16),
    pub content_y: u16,
    /// First screen row of the bar, when it is shown.
    pub bar_row: Option<u16>,
    pub bar_rows: u16,
    /// First screen row of the agent bar, when it is shown.
    pub agent_row: Option<u16>,
    pub agent_rows: u16,
}

/// Screen geometry for one client.
///
/// The status bar takes `bar_rows` once the terminal is at least two
/// rows tall, and always leaves one row for the panes. The agent bar
/// gets `agent_rows` of whatever the status bar left, still keeping
/// one pane row. A bar on top shifts the pane area down. The row
/// counts are the wrapped heights the layouts want; this only places
/// the blocks and caps them.
pub fn chrome(
    size: (u16, u16),
    bar_top: bool,
    bar_rows: u16,
    agent_top: bool,
    agent_rows: u16,
) -> Chrome {
    let (cols, rows) = size;
    let budget = rows.saturating_sub(1);
    let n = bar_rows.min(budget);
    let a = agent_rows.min(budget - n);
    let top_rows = if bar_top { n } else { 0 } + if agent_top { a } else { 0 };
    let bottom_rows = n + a - top_rows;
    let bar_row = (n > 0).then(|| if bar_top { 0 } else { rows - n });
    let agent_row = (a > 0).then(|| match (agent_top, bar_top) {
        (true, true) => n,
        (true, false) => 0,
        (false, false) => rows - n - a,
        (false, true) => rows - a,
    });
    Chrome {
        content: (cols, rows - top_rows - bottom_rows),
        content_y: top_rows,
        bar_row,
        bar_rows: n,
        agent_row,
        agent_rows: a,
    }
}

/// The pane area of a client screen: everything except `bar_rows` of
/// status bar. Sessions are laid out and shells sized to this, so it
/// must be used for split and navigation geometry too. Which edge the
/// bar sits on changes the pane origin, not this size.
pub fn content_size(size: (u16, u16), bar_rows: u16) -> (u16, u16) {
    chrome(size, false, bar_rows, false, 0).content
}

/// Both bars laid out for one client, and the screen geometry they
/// leave. Drawing, pane sizing, and click hit-testing all go through
/// this so they agree on where everything is. The status bar is laid
/// out first; the agent bar wraps into the rows it leaves.
pub fn screen_layout(
    items: &[BarSession],
    agent_items: &[BarSession],
    active: &Session,
    size: (u16, u16),
    bar_top: bool,
    agent_top: bool,
) -> (Chrome, BarLayout, BarLayout) {
    let bar = bar_layout(items, active, size.0, size.1);
    let agent = agent_bar_layout(agent_items, size.0, size.1.saturating_sub(bar.rows));
    let screen = chrome(size, bar_top, bar.rows, agent_top, agent.rows);
    (screen, bar, agent)
}

/// Truncate to `max` display characters, ellipsized.
fn fit(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Draw one frame of the viewed session: the active tab's panes at their
/// rectangles, dim divider lines between them, the status bar, and the
/// focused pane's cursor.
///
/// `full` repaints every cell instead of only the rows the panes marked
/// dirty. The caller sets it when something outside the panes owns what
/// is on the client's screen — an overlay that cleared it, a resize —
/// because the panes' dirty state says nothing about that.
pub fn draw_session(
    renderer: &mut Renderer<'static>,
    session: &Session,
    bar_sessions: &[BarSession],
    agent_sessions: &[BarSession],
    active: usize,
    out: &mut impl Write,
    size: (u16, u16),
    accent: Color,
    active_chip: ChipColor,
    finished_chip: ChipColor,
    working_chip: ChipStyle,
    bar_background: Option<Color>,
    agent_bar_background: Option<Color>,
    bar_foreground: Option<Color>,
    agent_bar_foreground: Option<Color>,
    bar_top: bool,
    agent_top: bool,
    synchronized: bool,
    full: bool,
) -> Result<()> {
    let (screen, layout, agent_layout) = screen_layout(
        bar_sessions,
        agent_sessions,
        session,
        size,
        bar_top,
        agent_top,
    );
    let area = Rect {
        x: 0,
        y: screen.content_y,
        w: screen.content.0,
        h: screen.content.1,
    };
    let tab = &session.tabs[session.active_tab];
    if synchronized {
        queue!(out, BeginSynchronizedUpdate, Hide)?;
    } else {
        queue!(out, Hide)?;
    }
    // Wipe first on a full frame: the pane rects and the bar cover every
    // cell, but this way nothing an overlay left behind can survive a
    // gap. Inside the synchronized update, so it never shows.
    if full {
        queue!(out, SetAttribute(Attribute::Reset), Clear(ClearType::All))?;
    }

    let mut cursor = None;
    let mut focus_rect = None;
    let focused = tab.focused;
    if tab.zoomed
        && let Some(pane) = tab.layout.pane(focused)
    {
        // Fullscreen: only the focused pane, no dividers.
        cursor = renderer.draw_at(&pane.term, out, area, full)?;
    } else {
        tab.layout.for_each(area, &mut |pane, rect| {
            let pane_cursor = renderer.draw_at(&pane.term, out, rect, full)?;
            if pane.id == focused {
                cursor = pane_cursor;
                focus_rect = Some(rect);
            }
            Ok(())
        })?;
        draw_dividers(out, &tab.layout, area, focus_rect, accent)?;
    }

    if let Some(origin) = screen.bar_row {
        draw_bar(
            out,
            &layout,
            bar_sessions,
            active,
            session.active_tab,
            active_chip,
            finished_chip,
            working_chip,
            bar_background,
            bar_foreground,
            origin,
            size.0,
        )?;
    }
    if let Some(origin) = screen.agent_row {
        draw_bar(
            out,
            &agent_layout,
            agent_sessions,
            active,
            session.active_tab,
            active_chip,
            finished_chip,
            working_chip,
            agent_bar_background,
            agent_bar_foreground,
            origin,
            size.0,
        )?;
    }

    if let Some((x, y)) = cursor {
        queue!(out, MoveTo(x, y), Show)?;
    }
    if synchronized {
        queue!(out, EndSynchronizedUpdate)?;
    }
    out.flush()?;
    Ok(())
}

/// One chip after layout. `row` is relative to the top of the bar block.
struct Placed {
    kind: ChipKind,
    label: String,
    col: u16,
    row: u16,
}

#[derive(Clone, Copy)]
enum ChipKind {
    Session {
        index: Option<usize>,
        pin: Option<usize>,
    },
    Tab(usize),
}

#[derive(Clone)]
struct Chip {
    kind: ChipKind,
    label: String,
}

/// The status bar's chips. Drawing and click hit-testing share this, so
/// they cannot disagree about where a session or a tab is.
///
/// Sessions sit on the left and tabs on the right of one row when both
/// fit. When they would overlap, the tabs move up a line (and wrap
/// upward, right-aligned, if they still do not fit) and the sessions
/// stay below, left-aligned, wrapping onto further rows the same way.
/// A chip wider than the screen is ellipsized so it still occupies one row.
pub struct BarLayout {
    chips: Vec<Placed>,
    pub rows: u16,
}

pub fn bar_layout(
    items: &[BarSession],
    active: &Session,
    cols: u16,
    screen_rows: u16,
) -> BarLayout {
    let cols = cols as usize;
    let max_rows = if screen_rows < 2 {
        0
    } else {
        (screen_rows - 1) as usize
    };
    if cols == 0 || max_rows == 0 {
        return BarLayout {
            chips: Vec::new(),
            rows: 0,
        };
    }

    let sessions: Vec<Chip> = items
        .iter()
        .map(|item| Chip {
            kind: ChipKind::Session {
                index: item.index,
                pin: item.pin,
            },
            label: clamp_label(session_chip_label(item), cols),
        })
        .filter(|chip| !chip.label.is_empty())
        .collect();
    let tabs: Vec<Chip> = active
        .tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| Chip {
            kind: ChipKind::Tab(i),
            label: clamp_label(tab_chip_label(tab), cols),
        })
        .filter(|chip| !chip.label.is_empty())
        .collect();

    let session_width = width_of(&sessions);
    let tab_width = width_of(&tabs);
    let chips = if session_width + tab_width <= cols {
        place_shared_row(&sessions, &tabs, cols)
    } else if max_rows == 1 {
        place_clipped_row(&sessions, &tabs, cols)
    } else {
        place_stacked(&sessions, &tabs, cols, max_rows)
    };
    let rows = chips
        .iter()
        .map(|chip| chip.row)
        .max()
        .map_or(0, |row| row + 1);
    BarLayout { chips, rows }
}

/// The agent bar: agent sessions only, left-aligned, wrapping onto
/// further rows. No agent sessions means no rows, so the bar is gone.
/// Like [`bar_layout`], it keeps one of `screen_rows` for the panes.
pub fn agent_bar_layout(items: &[BarSession], cols: u16, screen_rows: u16) -> BarLayout {
    let cols = cols as usize;
    let max_rows = screen_rows.saturating_sub(1) as usize;
    let sessions: Vec<Chip> = items
        .iter()
        .map(|item| Chip {
            kind: ChipKind::Session {
                index: item.index,
                pin: item.pin,
            },
            label: clamp_label(session_chip_label(item), cols),
        })
        .filter(|chip| !chip.label.is_empty())
        .collect();
    let chips = pack(&sessions, cols, false, max_rows, 0);
    let rows = chips
        .iter()
        .map(|chip| chip.row)
        .max()
        .map_or(0, |row| row + 1);
    BarLayout { chips, rows }
}

impl BarLayout {
    /// The session or tab whose chip covers `(row, x)` of this bar block.
    pub fn hit(&self, row: u16, x: u16) -> Option<BarHit> {
        self.chips
            .iter()
            .find(|chip| {
                chip.row == row
                    && x >= chip.col
                    && (x - chip.col) < chip.label.chars().count() as u16
            })
            .and_then(|chip| match chip.kind {
                ChipKind::Tab(i) => Some(BarHit::Tab(i)),
                ChipKind::Session { index: Some(i), .. } => Some(BarHit::Session(i)),
                ChipKind::Session { pin: Some(p), .. } => Some(BarHit::Pin(p)),
                ChipKind::Session { .. } => None,
            })
    }
}

/// The session or tab whose chip covers `(row, x)` of the bar block.
#[cfg(test)]
pub fn bar_hit(
    items: &[BarSession],
    active: &Session,
    cols: u16,
    screen_rows: u16,
    row: u16,
    x: u16,
) -> Option<BarHit> {
    bar_layout(items, active, cols, screen_rows).hit(row, x)
}

/// How a run of text is styled.
#[derive(Clone, Copy)]
enum TextPaint {
    /// Foreground `color` with reverse video, so the chip background is
    /// that color.
    Reverse(Color),
    Dim,
    /// Plain foreground, with optional bold and dim. Manager rows.
    Fg {
        color: Color,
        bold: bool,
        dim: bool,
    },
    /// Explicit chip text and fill. `bg` absent keeps the terminal
    /// background. Used when a bar font color is configured.
    Chip {
        fg: Color,
        bg: Option<Color>,
    },
}

fn apply_text_paint(
    out: &mut impl Write,
    paint: TextPaint,
    background: Option<Color>,
) -> Result<()> {
    match paint {
        TextPaint::Reverse(color) => {
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                SetForegroundColor(color),
                SetAttribute(Attribute::Reverse),
            )?;
        }
        TextPaint::Dim => {
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                SetAttribute(Attribute::Dim),
            )?;
            // Reset drops the bar fill. Put it back so a dim chip is the
            // same color as the gap beside it.
            if let Some(color) = background {
                queue!(out, SetBackgroundColor(color))?;
            }
        }
        TextPaint::Fg { color, bold, dim } => {
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                SetForegroundColor(color),
            )?;
            if bold {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if dim {
                queue!(out, SetAttribute(Attribute::Dim))?;
            }
        }
        TextPaint::Chip { fg, bg } => {
            queue!(out, SetAttribute(Attribute::Reset), SetForegroundColor(fg),)?;
            if let Some(bg) = bg {
                queue!(out, SetBackgroundColor(bg))?;
            }
        }
    }
    Ok(())
}

/// Sessions on the left, tabs of the open session on the right. The
/// open session and the open tab are `active` chips. A session whose
/// agent just finished a turn, and that is not the one on screen, is a
/// `finished` chip. A session whose agent is working is a `working`
/// chip.
fn draw_bar(
    out: &mut impl Write,
    layout: &BarLayout,
    items: &[BarSession],
    active_session: usize,
    active_tab: usize,
    active: ChipColor,
    finished: ChipColor,
    working_chip: ChipStyle,
    background: Option<Color>,
    foreground: Option<Color>,
    origin: u16,
    cols: u16,
) -> Result<()> {
    // Blank every row first. Chips are not always flush left, and a
    // shared row has a gap in the middle, so clearing only up to the
    // last chip would leave the previous frame in that gap.
    for row in 0..layout.rows {
        fill_bar_row(out, origin + row, cols, background)?;
    }
    for chip in &layout.chips {
        queue!(out, MoveTo(chip.col, origin + chip.row))?;
        let paint = match chip.kind {
            ChipKind::Session { index, .. } => {
                let is_finished = index.is_some_and(|i| {
                    items.iter().any(|item| {
                        item.index == Some(i)
                            && item.finished
                            && item.activity == Some(AgentActivity::Idle)
                    })
                });
                let working = index.is_some_and(|i| {
                    items.iter().any(|item| {
                        item.index == Some(i) && item.activity == Some(AgentActivity::Working)
                    })
                });
                // The open session keeps the active chip, so the bar
                // still shows where you are, working or not. Finished
                // and working never overlap: finished needs an idle
                // agent.
                let fill = if index == Some(active_session) {
                    Some(active.into())
                } else if is_finished {
                    Some(finished.into())
                } else if working {
                    Some(working_chip)
                } else {
                    None
                };
                chip_paint(fill, foreground, background)
            }
            ChipKind::Tab(i) => {
                let fill = (i == active_tab).then_some(active.into());
                chip_paint(fill, foreground, background)
            }
        };
        apply_text_paint(out, paint, background)?;
        queue!(out, Print(&chip.label))?;
        queue!(out, SetAttribute(Attribute::Reset))?;
    }
    Ok(())
}

/// Chip style. `fill` is the open, finished, or working chip; absent
/// means an ordinary chip that sits on the bar background, and so does
/// a style with no fill of its own. Text is the chip's own foreground,
/// else the bar's font color. With neither, the old treatment stays:
/// reverse for a filled chip, dim terminal text otherwise.
fn chip_paint(
    fill: Option<ChipStyle>,
    foreground: Option<Color>,
    bar_background: Option<Color>,
) -> TextPaint {
    let text = fill.and_then(|chip| chip.foreground).or(foreground);
    match (text, fill.and_then(|chip| chip.background)) {
        (Some(fg), Some(bg)) => TextPaint::Chip { fg, bg: Some(bg) },
        (Some(fg), None) => TextPaint::Chip {
            fg,
            bg: bar_background,
        },
        (None, Some(bg)) => TextPaint::Reverse(bg),
        (None, None) => TextPaint::Dim,
    }
}

fn session_chip_label(item: &BarSession) -> String {
    format!(" {} ", item.name)
}

fn tab_chip_label(tab: &crate::model::Tab) -> String {
    // A fullscreened tab advertises it in its label.
    if tab.zoomed {
        format!(" {} [F] ", tab.name)
    } else {
        format!(" {} ", tab.name)
    }
}

fn clamp_label(label: String, cols: usize) -> String {
    if label.chars().count() <= cols {
        label
    } else {
        fit(&label, cols)
    }
}

fn width_of(chips: &[Chip]) -> usize {
    chips.iter().map(|chip| chip.label.chars().count()).sum()
}

/// Both groups fit on one row: sessions flush left, tabs flush right.
fn place_shared_row(sessions: &[Chip], tabs: &[Chip], cols: usize) -> Vec<Placed> {
    let mut chips = place_run(sessions, 0, 0);
    let tab_col = cols.saturating_sub(width_of(tabs)) as u16;
    chips.extend(place_run(tabs, tab_col, 0));
    chips
}

/// One row is all the screen will give. Keep chips from the front of
/// each group and drop the ones that would overlap.
fn place_clipped_row(sessions: &[Chip], tabs: &[Chip], cols: usize) -> Vec<Placed> {
    let sessions = take_fitting(sessions, cols);
    let tabs = take_fitting(tabs, cols.saturating_sub(width_of(&sessions)));
    let mut chips = place_run(&sessions, 0, 0);
    let tab_col = cols.saturating_sub(width_of(&tabs)) as u16;
    chips.extend(place_run(&tabs, tab_col, 0));
    chips
}

/// Tabs move up a line, right-aligned, and wrap upward if they still
/// do not fit. Sessions stay on the rows below, left-aligned.
fn place_stacked(sessions: &[Chip], tabs: &[Chip], cols: usize, max_rows: usize) -> Vec<Placed> {
    let tab_need = row_count(tabs, cols);
    let session_need = row_count(sessions, cols);
    let (tab_rows, session_rows) = allocate_rows(tab_need, session_need, max_rows);
    let mut chips = pack(tabs, cols, true, tab_rows, 0);
    let session_base = chips
        .iter()
        .map(|chip| chip.row)
        .max()
        .map_or(0, |row| row + 1);
    chips.extend(pack(sessions, cols, false, session_rows, session_base));
    chips
}

/// Split `max_rows` between the two sides. Each side that has chips
/// keeps a row when there are two or more rows to give.
fn allocate_rows(tab_need: usize, session_need: usize, max_rows: usize) -> (usize, usize) {
    if max_rows == 0 {
        return (0, 0);
    }
    if tab_need + session_need <= max_rows {
        return (tab_need, session_need);
    }
    if tab_need == 0 {
        return (0, session_need.min(max_rows));
    }
    if session_need == 0 {
        return (tab_need.min(max_rows), 0);
    }
    if max_rows == 1 {
        return (1, 0);
    }
    let mut tabs = 1;
    let mut sessions = 1;
    let mut extra = max_rows - 2;
    let tab_extra = tab_need.saturating_sub(1).min(extra);
    tabs += tab_extra;
    extra -= tab_extra;
    sessions += session_need.saturating_sub(1).min(extra);
    (tabs, sessions)
}

fn row_count(chips: &[Chip], cols: usize) -> usize {
    pack(chips, cols, false, usize::MAX, 0)
        .iter()
        .map(|chip| chip.row)
        .max()
        .map_or(0, |row| row as usize + 1)
}

/// `right` packs each wrapped row against the right edge. Sessions pass
/// `false` and stay on the left.
fn pack(chips: &[Chip], cols: usize, right: bool, max_rows: usize, row_base: u16) -> Vec<Placed> {
    if max_rows == 0 || cols == 0 {
        return Vec::new();
    }
    let mut placed = Vec::new();
    let mut current: Vec<Chip> = Vec::new();
    let mut used = 0usize;
    let mut rows = 0usize;
    for chip in chips {
        let w = chip.label.chars().count();
        if w == 0 || w > cols {
            continue;
        }
        if !current.is_empty() && used + w > cols {
            placed.extend(place_row(&current, cols, right, row_base + rows as u16));
            current.clear();
            used = 0;
            rows += 1;
            if rows >= max_rows {
                return placed;
            }
        }
        used += w;
        current.push(Chip {
            kind: chip.kind,
            label: chip.label.clone(),
        });
    }
    if !current.is_empty() && rows < max_rows {
        placed.extend(place_row(&current, cols, right, row_base + rows as u16));
    }
    placed
}

fn place_row(chips: &[Chip], cols: usize, right: bool, row: u16) -> Vec<Placed> {
    let start = if right {
        cols.saturating_sub(width_of(chips)) as u16
    } else {
        0
    };
    place_run(chips, start, row)
}

fn place_run(chips: &[Chip], start: u16, row: u16) -> Vec<Placed> {
    let mut x = start;
    let mut out = Vec::with_capacity(chips.len());
    for chip in chips {
        let w = chip.label.chars().count() as u16;
        out.push(Placed {
            kind: chip.kind,
            label: chip.label.clone(),
            col: x,
            row,
        });
        x += w;
    }
    out
}

fn take_fitting(chips: &[Chip], cols: usize) -> Vec<Chip> {
    let mut kept = Vec::new();
    let mut used = 0usize;
    for chip in chips {
        let w = chip.label.chars().count();
        if used + w > cols {
            break;
        }
        used += w;
        kept.push(Chip {
            kind: chip.kind,
            label: chip.label.clone(),
        });
    }
    kept
}

/// Blank a bar row from column 0.
///
/// With no configured background, erase-in-line restores the terminal's
/// own background. A configured color is painted with spaces: erase-in-line
/// uses the default background on some terminals and would leave a hole.
fn fill_bar_row(
    out: &mut impl Write,
    row: u16,
    cols: u16,
    background: Option<Color>,
) -> Result<()> {
    queue!(out, MoveTo(0, row), SetAttribute(Attribute::Reset))?;
    if let Some(color) = background {
        let blank = " ".repeat(cols as usize);
        queue!(out, SetBackgroundColor(color), Print(blank))?;
    } else {
        queue!(out, Clear(ClearType::UntilNewLine))?;
    }
    Ok(())
}

// Line-component bits for box-drawing junction resolution.
pub(crate) const B_UP: u8 = 1;
pub(crate) const B_DOWN: u8 = 2;
pub(crate) const B_LEFT: u8 = 4;
pub(crate) const B_RIGHT: u8 = 8;

/// Draw the divider lines of every split, resolving crossings and tees
/// (`┬ ┴ ├ ┤ ┼`) where dividers meet instead of overdrawing. Divider
/// cells that border the focused pane are drawn in the accent color so
/// the active terminal reads as framed.
fn draw_dividers(
    out: &mut impl Write,
    layout: &Layout,
    rect: Rect,
    focused: Option<Rect>,
    accent: Color,
) -> Result<()> {
    // (bits, real): `real` cells are on a divider line; hint-only cells
    // exist so a neighboring divider knows a line abuts it.
    let mut cells: HashMap<(u16, u16), (u8, bool)> = HashMap::new();
    collect_dividers(layout, rect, &mut cells);
    if cells.is_empty() {
        return Ok(());
    }

    // Dim pass for dividers away from the focused pane...
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Dim)
    )?;
    for (&(x, y), &(bits, real)) in &cells {
        if !real || focused.is_some_and(|f| touches(f, x, y)) {
            continue;
        }
        queue!(out, MoveTo(x, y), Print(box_char(bits)))?;
    }
    // ...then an accent pass for the ones framing it.
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(accent),
    )?;
    for (&(x, y), &(bits, real)) in &cells {
        if !real || !focused.is_some_and(|f| touches(f, x, y)) {
            continue;
        }
        queue!(out, MoveTo(x, y), Print(box_char(bits)))?;
    }

    // Focus arrows: one on every divider bordering the focused pane,
    // centered on the shared edge and pointing into the pane.
    if let Some(f) = focused {
        let sides: [(bool, Option<u16>, u16, u16, char); 4] = [
            (true, f.x.checked_sub(1), f.y, f.h, '▸'),  // left divider
            (true, Some(f.x + f.w), f.y, f.h, '◂'),     // right divider
            (false, f.y.checked_sub(1), f.x, f.w, '▾'), // top divider
            (false, Some(f.y + f.h), f.x, f.w, '▴'),    // bottom divider
        ];
        for (vertical, fixed, lo, len, glyph) in sides {
            let Some(fixed) = fixed else { continue };
            if let Some((x, y)) = arrow_cell(&cells, vertical, fixed, lo, len) {
                queue!(out, MoveTo(x, y), Print(glyph))?;
            }
        }
    }
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(Color::Reset)
    )?;
    Ok(())
}

/// The cell an arrow lands on for one side of the focused pane: the
/// center of the shared edge along the divider at `fixed`, nudged
/// sideways when the center is a junction (`┼ ├ ┬ …`), so junctions
/// stay readable. Returns `None` when the side has no plain divider
/// cell at all (e.g. it is a screen edge).
fn arrow_cell(
    cells: &HashMap<(u16, u16), (u8, bool)>,
    vertical: bool,
    fixed: u16,
    lo: u16,
    len: u16,
) -> Option<(u16, u16)> {
    if len == 0 {
        return None;
    }
    let plain = if vertical {
        B_UP | B_DOWN
    } else {
        B_LEFT | B_RIGHT
    };
    let pos = |v: u16| if vertical { (fixed, v) } else { (v, fixed) };
    let center = lo + (len - 1) / 2;
    // Center first, then nudge outward in both directions.
    let candidates = std::iter::once(center).chain((1..len).flat_map(|d| {
        let up = (center + d < lo + len).then_some(center + d);
        let down = center.checked_sub(d).filter(|v| *v >= lo);
        [up, down].into_iter().flatten()
    }));
    for v in candidates {
        if let Some(&(bits, real)) = cells.get(&pos(v)) {
            if real && bits == plain {
                return Some(pos(v));
            }
        }
    }
    None
}

/// Whether a divider cell lies on the one-cell ring around `f` — the
/// dividers that visually frame that pane (corners included).
fn touches(f: Rect, x: u16, y: u16) -> bool {
    let x_in = x + 1 >= f.x && x <= f.x + f.w;
    let y_in = y + 1 >= f.y && y <= f.y + f.h;
    let on_vertical = (x + 1 == f.x || x == f.x + f.w) && y_in;
    let on_horizontal = (y + 1 == f.y || y == f.y + f.h) && x_in;
    on_vertical || on_horizontal
}

pub(crate) fn collect_dividers(
    layout: &Layout,
    rect: Rect,
    cells: &mut HashMap<(u16, u16), (u8, bool)>,
) {
    let Layout::Split { dir, a, b } = layout else {
        return;
    };
    let (ra, rb) = split_rect(*dir, rect);
    match dir {
        SplitDir::Horizontal => {
            let y = rect.y + ra.h;
            for x in rect.x..rect.x + rect.w {
                let cell = cells.entry((x, y)).or_insert((0, false));
                cell.0 |= B_LEFT | B_RIGHT;
                cell.1 = true;
            }
            // Tell abutting vertical dividers a line arrives from the side.
            if rect.x > 0 {
                cells.entry((rect.x - 1, y)).or_insert((0, false)).0 |= B_RIGHT;
            }
            cells.entry((rect.x + rect.w, y)).or_insert((0, false)).0 |= B_LEFT;
        }
        SplitDir::Vertical => {
            let x = rect.x + ra.w;
            for y in rect.y..rect.y + rect.h {
                let cell = cells.entry((x, y)).or_insert((0, false));
                cell.0 |= B_UP | B_DOWN;
                cell.1 = true;
            }
            if rect.y > 0 {
                cells.entry((x, rect.y - 1)).or_insert((0, false)).0 |= B_DOWN;
            }
            cells.entry((x, rect.y + rect.h)).or_insert((0, false)).0 |= B_UP;
        }
    }
    collect_dividers(a, ra, cells);
    collect_dividers(b, rb, cells);
}

pub(crate) fn box_char(bits: u8) -> char {
    let (u, d, l, r) = (
        bits & B_UP != 0,
        bits & B_DOWN != 0,
        bits & B_LEFT != 0,
        bits & B_RIGHT != 0,
    );
    match (u, d, l, r) {
        (true, true, true, true) => '┼',
        (true, true, true, false) => '┤',
        (true, true, false, true) => '├',
        (true, false, true, true) => '┴',
        (false, true, true, true) => '┬',
        (true, true, _, _) => '│',
        _ => '─',
    }
}

/// One row of a manager panel.
pub struct ListItem {
    pub label: String,
    /// The currently open session/tab — marked with an accent dot.
    pub active: bool,
    /// Rendered dim (e.g. a pinned session that isn't running).
    pub dim: bool,
    /// The session's agent is working. The name is drawn in the menu's
    /// working color when one is configured.
    pub working: bool,
    /// The session's agent just finished a turn and nobody is viewing
    /// its panes. The name is drawn in the menu's finished color.
    pub finished: bool,
}

/// A centered, rounded panel geometry for the overlays.
struct Panel {
    x: u16,
    y: u16,
    /// Interior width (inside the borders, minus 1-cell side padding).
    iw: usize,
}

/// Draw the panel frame (rounded corners, dim border, bold inline title)
/// and `body_rows` blank interior rows, returning the geometry.
fn draw_panel(
    out: &mut impl Write,
    title: &str,
    body_rows: u16,
    min_interior: usize,
    size: (u16, u16),
) -> Result<Panel> {
    let need = (min_interior.max(title.chars().count() + 2) + 4) as u16;
    let w = need.clamp(24, size.0.saturating_sub(2).max(10));
    let h = body_rows + 2;
    let x = size.0.saturating_sub(w) / 2;
    let y = size.1.saturating_sub(h) / 2;
    let iw = w.saturating_sub(4) as usize;

    // Top border with the title inline: ╭─ title ────╮
    let title = fit(title, iw);
    let dash_count = (w as usize).saturating_sub(title.chars().count() + 5);
    queue!(
        out,
        MoveTo(x, y),
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Dim),
        Print("╭─ "),
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Bold),
        Print(&title),
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Dim),
        Print(format!(" {}╮", "─".repeat(dash_count))),
    )?;
    for row in 1..=body_rows {
        queue!(
            out,
            MoveTo(x, y + row),
            Print(format!("│{}│", " ".repeat(w as usize - 2))),
        )?;
    }
    queue!(
        out,
        MoveTo(x, y + body_rows + 1),
        Print(format!("╰{}╯", "─".repeat(w as usize - 2))),
        SetAttribute(Attribute::Reset),
    )?;
    Ok(Panel { x, y, iw })
}

/// Draw a manager overlay: a centered panel listing sessions or tabs,
/// with a `❯` selector, an accent dot on the open entry, and stopped
/// entries dimmed.
pub struct ManagerView<'a> {
    pub title: &'a str,
    /// Entries to show (already filtered when a search is active).
    pub items: &'a [ListItem],
    pub selected: usize,
    pub footer: &'a str,
    /// Active `/` query, drawn as a search bar in the panel's top row.
    pub search: Option<&'a str>,
    /// Caret position within that query, in characters.
    pub search_cursor: usize,
    /// Reserve space for at least this many rows / this interior width,
    /// so the panel doesn't jump around while a search filters it.
    pub min_rows: usize,
    pub min_interior: usize,
}

pub fn draw_manager(
    out: &mut impl Write,
    view: &ManagerView,
    size: (u16, u16),
    accent: Color,
    active: Color,
    finished_name: Color,
    working_name: Option<Color>,
) -> Result<()> {
    let ManagerView {
        title,
        items,
        selected,
        footer,
        search,
        search_cursor,
        min_rows,
        min_interior,
    } = *view;
    queue!(
        out,
        BeginSynchronizedUpdate,
        Hide,
        SetAttribute(Attribute::Reset),
    )?;
    queue!(out, Clear(ClearType::All))?;
    // Window the list if the screen is short.
    let max_shown = (size.1.saturating_sub(6) as usize).max(1);
    let offset = (selected + 1).saturating_sub(max_shown);
    let shown = &items[offset.min(items.len())..(offset + max_shown).min(items.len())];

    let min_interior = items
        .iter()
        .map(|i| i.label.chars().count() + 2)
        .chain([footer.chars().count(), min_interior])
        .max()
        .unwrap_or(0);
    // Filtering leaves blank rows instead of shrinking the panel.
    let list_rows = shown.len().max(min_rows.min(max_shown));
    let body_rows = list_rows as u16 + 3;
    let panel = draw_panel(out, title, body_rows, min_interior, size)?;

    // The search bar sits in the blank row under the title, with the
    // terminal's own cursor parked at the caret.
    let mut caret = None;
    if let Some(query) = search {
        let shown = fit(query, panel.iw.saturating_sub(3));
        queue!(
            out,
            MoveTo(panel.x + 2, panel.y + 1),
            SetForegroundColor(accent),
            Print("/"),
            SetForegroundColor(Color::Reset),
            Print(&shown),
            SetForegroundColor(Color::Reset),
        )?;
        caret = Some((
            panel.x + 3 + search_cursor.min(shown.chars().count()) as u16,
            panel.y + 1,
        ));
    }

    for (row, item) in shown.iter().enumerate() {
        let is_selected = offset + row == selected;
        queue!(out, MoveTo(panel.x + 2, panel.y + 2 + row as u16))?;
        if is_selected {
            queue!(
                out,
                SetForegroundColor(accent),
                Print("❯ "),
                SetAttribute(Attribute::Bold),
            )?;
        } else {
            queue!(out, Print("  "))?;
        }
        // The open session/tab is named in the active signal color; the ❯
        // above marks where the cursor sits, in the accent color, so the
        // two signals stay independent. A just-finished session that is
        // not the one on screen is named in `finished_name`. A working
        // session is named in `working_name` when one is set.
        let color = if item.finished && !item.active {
            finished_name
        } else if item.active {
            active
        } else if let Some(color) = working_name.filter(|_| item.working) {
            color
        } else {
            Color::Reset
        };
        let paint = TextPaint::Fg {
            color,
            bold: is_selected,
            dim: item.dim && !is_selected,
        };
        apply_text_paint(out, paint, None)?;
        queue!(out, Print(fit(&item.label, panel.iw.saturating_sub(2))))?;
        queue!(
            out,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::Reset),
        )?;
    }

    queue!(
        out,
        MoveTo(panel.x + 2, panel.y + body_rows),
        SetAttribute(Attribute::Dim),
        Print(fit(footer, panel.iw)),
        SetAttribute(Attribute::Reset),
    )?;
    if let Some((x, y)) = caret {
        queue!(out, MoveTo(x, y), Show)?;
    }
    queue!(out, EndSynchronizedUpdate)?;
    out.flush()?;
    Ok(())
}

/// Draw the name prompt for a new session/tab as a centered panel.
pub fn draw_naming(
    out: &mut impl Write,
    title: &str,
    name: &str,
    cursor: usize,
    size: (u16, u16),
    accent: Color,
    footer: &str,
) -> Result<()> {
    queue!(
        out,
        BeginSynchronizedUpdate,
        Hide,
        SetAttribute(Attribute::Reset),
        Clear(ClearType::All),
    )?;
    let min_interior = (name.chars().count() + 6).max(footer.chars().count());
    let panel = draw_panel(out, title, 4, min_interior, size)?;

    // Text longer than the field scrolls horizontally to keep the
    // cursor in view instead of being ellipsized.
    let chars: Vec<char> = name.chars().collect();
    let width = panel.iw.saturating_sub(2).max(1);
    let start = if chars.len() <= width {
        0
    } else {
        cursor
            .saturating_sub(width - 1)
            .min(chars.len().saturating_sub(width))
    };
    let visible: String = chars[start..(start + width).min(chars.len())]
        .iter()
        .collect();

    queue!(
        out,
        MoveTo(panel.x + 2, panel.y + 2),
        SetForegroundColor(accent),
        Print("❯ "),
        SetForegroundColor(Color::Reset),
        Print(&visible),
        MoveTo(panel.x + 2, panel.y + 4),
        SetAttribute(Attribute::Dim),
        Print(fit(footer, panel.iw)),
        SetAttribute(Attribute::Reset),
        // Put the terminal's own cursor where the caret is.
        MoveTo(
            panel.x + 4 + cursor.saturating_sub(start).min(width) as u16,
            panel.y + 2,
        ),
        Show,
        EndSynchronizedUpdate,
    )?;
    out.flush()?;
    Ok(())
}

pub struct Renderer<'alloc> {
    render_state: RenderState<'alloc>,
    row_it: RowIterator<'alloc>,
    cell_it: CellIterator<'alloc>,
}

/// The SGR state we last emitted, so we only send color/attribute
/// sequences when a cell actually differs from the previous one.
#[derive(PartialEq, Clone, Copy)]
struct Pen {
    fg: Color,
    bg: Color,
    bold: bool,
    italic: bool,
    underline: bool,
    reverse: bool,
}

/// Map a cell's color to what we emit: palette indices and unset
/// (default) colors pass through untouched, so the *host* terminal's
/// theme decides what they look like — only genuine truecolor cells
/// are sent as RGB. This is why xmux panes match the colors of the
/// terminal they run in.
fn style_color(c: StyleColor, default: Color) -> Color {
    match c {
        StyleColor::None => default,
        StyleColor::Palette(idx) => Color::AnsiValue(idx.0),
        StyleColor::Rgb(rgb) => color(rgb),
    }
}

impl<'alloc> Renderer<'alloc> {
    pub fn new() -> Result<Self> {
        Ok(Self {
            render_state: RenderState::new()?,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
        })
    }

    /// Draw one terminal's grid with its top-left at `rect`'s origin
    /// (the terminal is kept sized to the rect by `Tab::apply_layout`).
    /// Returns the coordinates of the terminal's cursor if visible.
    /// The caller wraps the frame in a synchronized update and flushes.
    /// `full` paints every row regardless of the terminal's dirty state.
    fn draw_at(
        &mut self,
        term: &Terminal<'alloc, '_>,
        out: &mut impl Write,
        rect: Rect,
        full: bool,
    ) -> Result<Option<(u16, u16)>> {
        // Snapshot the terminal state; everything below reads the snapshot.
        let snapshot = self.render_state.update(term)?;

        // Pane defaults: the host terminal's own defaults (SGR 39/49),
        // unless a program inside the pane overrode them via OSC 10/11.
        let default = Pen {
            fg: term.fg_color()?.map_or(Color::Reset, color),
            bg: term.bg_color()?.map_or(Color::Reset, color),
            bold: false,
            italic: false,
            underline: false,
            reverse: false,
        };
        let mut pen = default;

        queue!(
            out,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(pen.fg),
            SetBackgroundColor(pen.bg),
        )?;

        let frame_dirty = if full { Dirty::Full } else { snapshot.dirty()? };
        if frame_dirty != Dirty::Clean {
            let mut row_it = self.row_it.update(&snapshot)?;
            let mut y: u16 = 0;
            let mut text = String::with_capacity(16);

            while let Some(row) = row_it.next() {
                let paint = frame_dirty == Dirty::Full || row.dirty()?;
                if !paint {
                    y += 1;
                    continue;
                }
                queue!(out, MoveTo(rect.x, rect.y + y))?;
                let sel = row.selection()?;
                let mut cell_it = self.cell_it.update(row)?;
                let mut col: u16 = 0;

                while let Some(cell) = cell_it.next() {
                    // A wide character already advanced the cursor two
                    // columns; printing anything for its spacer cell would
                    // clobber the glyph's right half.
                    let wide = cell.raw_cell()?.wide()?;
                    match wide {
                        CellWide::SpacerTail | CellWide::SpacerHead => {
                            col = col.saturating_add(1);
                            continue;
                        }
                        CellWide::Narrow | CellWide::Wide => {}
                    }

                    let mut next = default;
                    if cell.has_styling()? {
                        let style = cell.style()?;
                        next.fg = style_color(style.fg_color, default.fg);
                        next.bg = style_color(style.bg_color, default.bg);
                        next.bold = style.bold;
                        next.italic = style.italic;
                        next.underline = style.underline != Underline::None;
                        // Pass inverse through as an attribute instead of
                        // swapping colors ourselves: default fg/bg can't be
                        // swapped in SGR, and the host does it correctly.
                        next.reverse = style.inverse;
                    }
                    // Mouse selection highlight: one range query per row
                    // instead of a C call per cell.
                    if let Some(sel) = sel {
                        let last = if wide == CellWide::Wide {
                            col.saturating_add(1)
                        } else {
                            col
                        };
                        if col <= sel.end_x && last >= sel.start_x {
                            next.reverse = !next.reverse;
                        }
                    }

                    Self::apply_pen(out, &mut pen, next)?;

                    if cell.graphemes_len()? == 0 {
                        queue!(out, Print(' '))?;
                    } else {
                        cell.graphemes_utf8(&mut text)?;
                        queue!(out, Print(&text))?;
                    }
                    col = col.saturating_add(1);
                }

                row.set_dirty(false)?;
                y += 1;
            }
        }

        // Report where the cursor should sit for this terminal.
        let cursor = if snapshot.cursor_visible()? {
            snapshot
                .cursor_viewport()?
                .map(|vp| (rect.x + vp.x, rect.y + vp.y as u16))
        } else {
            None
        };

        snapshot.set_dirty(Dirty::Clean)?;
        Ok(cursor)
    }

    /// Emit the escape sequences needed to go from `pen` to `next`.
    fn apply_pen(out: &mut impl Write, pen: &mut Pen, next: Pen) -> Result<()> {
        if *pen == next {
            return Ok(());
        }

        // Attributes can only be cleared by a full reset, which also
        // clears colors, so re-emit everything in that case.
        let attrs_changed = (pen.bold, pen.italic, pen.underline, pen.reverse)
            != (next.bold, next.italic, next.underline, next.reverse);

        if attrs_changed {
            queue!(out, SetAttribute(Attribute::Reset))?;
            if next.bold {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if next.italic {
                queue!(out, SetAttribute(Attribute::Italic))?;
            }
            if next.underline {
                queue!(out, SetAttribute(Attribute::Underlined))?;
            }
            if next.reverse {
                queue!(out, SetAttribute(Attribute::Reverse))?;
            }
        }
        if attrs_changed || pen.fg != next.fg {
            queue!(out, SetForegroundColor(next.fg))?;
        }
        if attrs_changed || pen.bg != next.bg {
            queue!(out, SetBackgroundColor(next.bg))?;
        }

        *pen = next;
        Ok(())
    }
}

fn color(rgb: RgbColor) -> Color {
    Color::Rgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::TerminalOptions;

    /// Crossterm's color switch is process-global, and these tests run in
    /// parallel. The lock is held until the previous setting is restored,
    /// so one test cannot turn colors off while another is still drawing.
    fn colors_on() -> ColorLock {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let prev = crossterm::style::Colored::ansi_color_disabled_memoized();
        crossterm::style::Colored::set_ansi_color_disabled(false);
        ColorLock { _lock: lock, prev }
    }

    struct ColorLock {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: bool,
    }

    impl Drop for ColorLock {
        fn drop(&mut self) {
            crossterm::style::Colored::set_ansi_color_disabled(self.prev);
        }
    }

    /// A pane that produced no output since the last frame reports
    /// nothing dirty, so an ordinary frame paints nothing at all. That is
    /// what left the manager's panel on screen after Esc: the overlay
    /// cleared the client's screen, and the frame that was supposed to
    /// restore the panes was a no-op. A full frame must paint anyway.
    #[test]
    fn a_full_frame_repaints_an_idle_terminal() {
        let mut term = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 3,
            max_scrollback: 0,
        })
        .unwrap();
        term.vt_write(b"hello");
        let mut renderer = Renderer::new().unwrap();
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 3,
        };
        let mut paint = |full: bool| {
            let mut buf = Vec::new();
            renderer.draw_at(&term, &mut buf, rect, full).unwrap();
            String::from_utf8_lossy(&buf).contains("hello")
        };

        assert!(paint(false), "first frame paints the pane");
        assert!(!paint(false), "idle pane stays unpainted");
        assert!(paint(true), "a full frame repaints it");
    }

    fn chip(
        index: Option<usize>,
        pin: Option<usize>,
        name: &str,
        activity: Option<AgentActivity>,
        finished: bool,
    ) -> BarSession {
        BarSession {
            index,
            pin,
            name: name.to_string(),
            activity,
            finished,
        }
    }

    fn running(
        index: usize,
        name: &str,
        activity: Option<AgentActivity>,
        finished: bool,
    ) -> BarSession {
        chip(Some(index), None, name, activity, finished)
    }

    fn laid_out(items: &[BarSession], session: &Session, cols: u16, rows: u16) -> BarLayout {
        bar_layout(items, session, cols, rows)
    }

    fn hit(
        items: &[BarSession],
        session: &Session,
        cols: u16,
        rows: u16,
        row: u16,
        x: u16,
    ) -> Option<BarHit> {
        bar_hit(items, session, cols, rows, row, x)
    }

    fn view(name: &str, tabs: &[&str]) -> Session {
        Session {
            id: 1,
            name: name.to_string(),
            tabs: tabs
                .iter()
                .map(|name| crate::model::Tab {
                    name: name.to_string(),
                    layout: Layout::Empty,
                    focused: 0,
                    zoomed: false,
                })
                .collect(),
            active_tab: 0,
            agent: false,
            last_activity: std::time::Instant::now(),
            last_size: (80, 24),
            last_agent_activity: None,
            finished_unseen: false,
        }
    }

    fn labels_on(layout: &BarLayout, row: u16) -> Vec<(String, u16)> {
        layout
            .chips
            .iter()
            .filter(|chip| chip.row == row)
            .map(|chip| (chip.label.clone(), chip.col))
            .collect()
    }

    #[test]
    fn content_shrinks_by_the_bar_rows_and_keeps_one_pane_row() {
        assert_eq!(content_size((80, 24), 1), (80, 23));
        assert_eq!(content_size((80, 24), 3), (80, 21));
        // Two rows: the bar keeps one and the panes keep the other.
        assert_eq!(content_size((80, 2), 4), (80, 1));
        assert_eq!(content_size((80, 1), 1), (80, 1));
    }

    #[test]
    fn chrome_puts_the_bar_on_the_configured_edge() {
        let bottom = chrome((80, 24), false, 1, false, 0);
        assert_eq!(bottom.bar_row, Some(23));
        assert_eq!(bottom.bar_rows, 1);
        assert_eq!(bottom.content, (80, 23));
        assert_eq!(bottom.content_y, 0);

        let top = chrome((80, 24), true, 1, false, 0);
        assert_eq!(top.bar_row, Some(0));
        assert_eq!(top.bar_rows, 1);
        assert_eq!(top.content_y, 1);
        assert_eq!(top.content, (80, 23));

        // Wrapped bar: tabs occupy the upper row of the block, sessions
        // the lower one. On the bottom edge that puts sessions on the
        // last screen row; on the top edge, tabs are row 0.
        let stacked_bottom = chrome((80, 24), false, 2, false, 0);
        assert_eq!(stacked_bottom.bar_row, Some(22));
        assert_eq!(stacked_bottom.content, (80, 22));
        assert_eq!(stacked_bottom.content_y, 0);

        let stacked_top = chrome((80, 24), true, 2, false, 0);
        assert_eq!(stacked_top.bar_row, Some(0));
        assert_eq!(stacked_top.bar_rows, 2);
        assert_eq!(stacked_top.content_y, 2);
        assert_eq!(stacked_top.content, (80, 22));
    }

    #[test]
    fn the_agent_bar_sits_on_the_pane_side_of_a_shared_edge() {
        // Both at the bottom: agent bar directly above the status bar.
        let bottom = chrome((80, 24), false, 1, false, 2);
        assert_eq!(bottom.bar_row, Some(23));
        assert_eq!(bottom.agent_row, Some(21));
        assert_eq!(bottom.agent_rows, 2);
        assert_eq!(bottom.content, (80, 21));
        assert_eq!(bottom.content_y, 0);

        // Both at the top: agent bar directly below the status bar.
        let top = chrome((80, 24), true, 2, true, 1);
        assert_eq!(top.bar_row, Some(0));
        assert_eq!(top.agent_row, Some(2));
        assert_eq!(top.content, (80, 21));
        assert_eq!(top.content_y, 3);
    }

    #[test]
    fn the_agent_bar_can_take_the_other_edge() {
        let agent_top = chrome((80, 24), false, 1, true, 1);
        assert_eq!(agent_top.agent_row, Some(0));
        assert_eq!(agent_top.bar_row, Some(23));
        assert_eq!(agent_top.content, (80, 22));
        assert_eq!(agent_top.content_y, 1);

        let agent_bottom = chrome((80, 24), true, 1, false, 1);
        assert_eq!(agent_bottom.bar_row, Some(0));
        assert_eq!(agent_bottom.agent_row, Some(23));
        assert_eq!(agent_bottom.content, (80, 22));
        assert_eq!(agent_bottom.content_y, 1);
    }

    #[test]
    fn no_agents_means_no_agent_bar_and_panes_keep_a_row() {
        let none = chrome((80, 24), false, 1, false, 0);
        assert_eq!(none.agent_row, None);
        assert_eq!(none.content, (80, 23));

        // Three rows: status bar 1, agent bar 1, panes 1 — the agent
        // bar never eats the last pane row.
        let tight = chrome((80, 3), false, 1, false, 5);
        assert_eq!(tight.agent_rows, 1);
        assert_eq!(tight.agent_row, Some(1));
        assert_eq!(tight.content, (80, 1));
        let full = chrome((80, 2), false, 1, true, 1);
        assert_eq!(full.agent_row, None);
        assert_eq!(full.content, (80, 1));
    }

    #[test]
    fn agent_chips_wrap_left_aligned_and_hit_their_session() {
        let items = vec![
            running(3, "claude-1", None, false),
            running(4, "grok-2", None, false),
        ];
        let layout = agent_bar_layout(&items, 12, 24);
        assert_eq!(layout.rows, 2);
        assert_eq!(labels_on(&layout, 0), vec![(" claude-1 ".to_string(), 0)]);
        assert_eq!(labels_on(&layout, 1), vec![(" grok-2 ".to_string(), 0)]);
        assert_eq!(layout.hit(1, 2), Some(BarHit::Session(4)));
        assert_eq!(agent_bar_layout(&[], 80, 24).rows, 0);
    }

    #[test]
    fn the_signal_is_the_brighter_of_the_pair() {
        let rgb = |r, g, b| Color::Rgb { r, g, b };
        let gold = rgb(0xff, 0xd2, 0x7d);
        let bronze = rgb(0x3a, 0x2a, 0x0c);
        // Dark fill, bright text: the text carries the signal.
        let dark = ChipColor {
            background: bronze,
            foreground: Some(gold),
        };
        assert_eq!(dark.signal(), gold);
        // Bright fill, dark text: the fill does, as before.
        let bright = ChipColor {
            background: gold,
            foreground: Some(bronze),
        };
        assert_eq!(bright.signal(), gold);
        assert_eq!(ChipColor::fill(bronze).signal(), bronze);
        // Palette colors have no brightness to compare.
        let named = ChipColor {
            background: Color::Magenta,
            foreground: Some(gold),
        };
        assert_eq!(named.signal(), Color::Magenta);
    }

    #[test]
    fn a_chip_foreground_overrides_the_bar_text() {
        let navy = Color::Rgb {
            r: 0x11,
            g: 0x22,
            b: 0x33,
        };
        let white = Color::Rgb {
            r: 0xff,
            g: 0xff,
            b: 0xff,
        };
        let chip = ChipColor {
            background: Color::Cyan,
            foreground: Some(navy),
        };
        assert!(matches!(
            chip_paint(Some(chip.into()), Some(white), None),
            TextPaint::Chip { fg, bg: Some(Color::Cyan) } if fg == navy
        ));
        // Without its own foreground the chip takes the bar text, and
        // with neither it falls back to reverse video.
        assert!(matches!(
            chip_paint(Some(ChipColor::fill(Color::Cyan).into()), Some(white), None),
            TextPaint::Chip { fg, bg: Some(Color::Cyan) } if fg == white
        ));
        assert!(matches!(
            chip_paint(Some(ChipColor::fill(Color::Cyan).into()), None, None),
            TextPaint::Reverse(Color::Cyan)
        ));
        // Ordinary chips ignore chip foregrounds entirely.
        assert!(matches!(chip_paint(None, None, None), TextPaint::Dim));
    }

    #[test]
    fn shared_row_puts_sessions_left_and_tabs_right() {
        // " work " is 6 columns, " shell " is 7. Together they fit in 20.
        let items = vec![running(3, "work", None, false)];
        let session = view("work", &["shell"]);
        let layout = laid_out(&items, &session, 20, 24);
        assert_eq!(layout.rows, 1);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" work ".into(), 0), (" shell ".into(), 13)]
        );
        assert_eq!(
            hit(&items, &session, 20, 24, 0, 0),
            Some(BarHit::Session(3))
        );
        assert_eq!(
            hit(&items, &session, 20, 24, 0, 5),
            Some(BarHit::Session(3))
        );
        assert_eq!(hit(&items, &session, 20, 24, 0, 6), None);
        assert_eq!(hit(&items, &session, 20, 24, 0, 13), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 20, 24, 0, 19), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 20, 24, 0, 20), None);
    }

    #[test]
    fn overlap_stacks_tabs_above_sessions() {
        // " work " is 6 and " shell " is 7, which does not fit in 11. The
        // tab moves up and stays on the right (11 - 7 = 4); the session
        // stays on the left.
        let items = vec![running(3, "work", None, false)];
        let session = view("work", &["shell"]);
        let layout = laid_out(&items, &session, 11, 24);
        assert_eq!(layout.rows, 2);
        assert_eq!(labels_on(&layout, 0), vec![(" shell ".into(), 4)]);
        assert_eq!(labels_on(&layout, 1), vec![(" work ".into(), 0)]);
        assert_eq!(hit(&items, &session, 11, 24, 0, 3), None);
        assert_eq!(hit(&items, &session, 11, 24, 0, 4), Some(BarHit::Tab(0)));
        assert_eq!(
            hit(&items, &session, 11, 24, 1, 0),
            Some(BarHit::Session(3))
        );
    }

    #[test]
    fn sessions_and_tabs_wrap_onto_extra_rows() {
        // Session chips are 6 columns; tab chips are 7. Width 14 holds
        // two of either. Three sessions and three tabs cannot share a
        // row, so tabs wrap on top (2 rows) and sessions below (2 rows).
        let items = vec![
            running(0, "work", None, false),
            running(1, "meow", None, false),
            running(2, "abcd", None, false),
        ];
        let session = view("work", &["shell", "build", "tests"]);
        let layout = laid_out(&items, &session, 14, 24);
        assert_eq!(layout.rows, 4);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" shell ".into(), 0), (" build ".into(), 7)]
        );
        // The leftover tab row is right-aligned: 14 - 7 = 7.
        assert_eq!(labels_on(&layout, 1), vec![(" tests ".into(), 7)]);
        assert_eq!(
            labels_on(&layout, 2),
            vec![(" work ".into(), 0), (" meow ".into(), 6)]
        );
        assert_eq!(labels_on(&layout, 3), vec![(" abcd ".into(), 0)]);
        assert_eq!(hit(&items, &session, 14, 24, 1, 7), Some(BarHit::Tab(2)));
        assert_eq!(
            hit(&items, &session, 14, 24, 3, 0),
            Some(BarHit::Session(2))
        );
    }

    #[test]
    fn a_short_screen_keeps_one_row_of_each_side() {
        // Three sessions need two rows and three tabs need two, four in
        // all. A 3-row screen can spare two bar rows, one per side, so
        // the third chip of each side is dropped.
        let items = vec![
            running(0, "work", None, false),
            running(1, "meow", None, false),
            running(2, "abcd", None, false),
        ];
        let session = view("work", &["shell", "build", "tests"]);
        let layout = laid_out(&items, &session, 14, 3);
        assert_eq!(layout.rows, 2);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" shell ".into(), 0), (" build ".into(), 7)]
        );
        assert_eq!(
            labels_on(&layout, 1),
            vec![(" work ".into(), 0), (" meow ".into(), 6)]
        );
        assert_eq!(hit(&items, &session, 14, 3, 0, 0), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 14, 3, 1, 0), Some(BarHit::Session(0)));
    }

    #[test]
    fn one_row_budget_clips_instead_of_overlapping() {
        // " work " is 6, " shell " and " build " are 7. Width 16 holds the
        // session and the first tab (6 + 7) and drops the second tab.
        let items = vec![running(1, "work", None, false)];
        let session = view("work", &["shell", "build"]);
        let layout = laid_out(&items, &session, 16, 2);
        assert_eq!(layout.rows, 1);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" work ".into(), 0), (" shell ".into(), 9)]
        );
        assert_eq!(hit(&items, &session, 16, 2, 0, 9), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 16, 2, 0, 8), None);
    }

    #[test]
    fn a_chip_wider_than_the_screen_is_ellipsized() {
        let items = vec![running(0, "workspace", None, false)];
        let session = view("workspace", &["sh"]);
        // " workspace " is 11 columns. Width 5 keeps an ellipsized chip
        // on its own row, and the tab still gets the row above it,
        // right-aligned: " sh " is 4 columns, so it starts at column 1.
        let layout = laid_out(&items, &session, 5, 24);
        assert_eq!(layout.rows, 2);
        assert_eq!(labels_on(&layout, 0), vec![(" sh ".into(), 1)]);
        let sessions = labels_on(&layout, 1);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].1, 0);
        assert_eq!(sessions[0].0.chars().count(), 5);
        assert!(sessions[0].0.contains('…'), "{sessions:?}");
    }

    #[test]
    fn working_and_idle_sessions_keep_their_names() {
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(1, "notes", Some(AgentActivity::Idle), true),
        ];
        let session = view("work", &["shell"]);
        // " work " is 6 columns, " notes " is 7. The activity does not
        // change the label; the letter color is applied at paint time.
        let laid = laid_out(&items, &session, 40, 24);
        assert_eq!(laid.rows, 1);
        assert_eq!(
            labels_on(&laid, 0),
            vec![
                (" work ".into(), 0),
                (" notes ".into(), 6),
                (" shell ".into(), 33),
            ]
        );
    }

    #[test]
    fn a_stopped_pin_is_clickable_and_a_running_pin_switches() {
        let items = vec![
            chip(None, Some(2), "meow", None, false),
            chip(Some(4), Some(0), "work", None, false),
        ];
        let session = view("work", &["shell"]);
        assert_eq!(hit(&items, &session, 40, 24, 0, 0), Some(BarHit::Pin(2)));
        // " meow " is 6 columns, so the trailing space is still the pin
        // and the running session starts at 6.
        assert_eq!(hit(&items, &session, 40, 24, 0, 5), Some(BarHit::Pin(2)));
        assert_eq!(
            hit(&items, &session, 40, 24, 0, 6),
            Some(BarHit::Session(4))
        );
    }

    #[test]
    fn the_bar_draws_the_tabs_on_the_right_after_clearing_the_row() {
        let items = vec![running(0, "work", None, false)];
        let session = view("work", &["shell"]);
        // " shell " is 7 columns. Width 20 puts it at column 13:
        // MoveTo(13, 5) is ESC[6;14H.
        let layout = laid_out(&items, &session, 20, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &items,
            0,
            0,
            ChipColor::fill(Color::Cyan),
            ChipColor::fill(Color::Magenta),
            ChipStyle::default(),
            None,
            None,
            5,
            20,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let clear = text.find("\u{1b}[K").expect(&text);
        let jump = text.find("\u{1b}[6;14H").expect(&text);
        let label = text.find(" shell ").expect(&text);
        assert!(clear < jump && jump < label, "{text:?}");
    }

    #[test]
    fn the_open_session_and_tab_use_the_accent_chip() {
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(2, "notes", Some(AgentActivity::Idle), false),
        ];
        let session = view("work", &["shell", "build"]);
        let layout = laid_out(&items, &session, 80, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &items,
            0,
            0,
            ChipColor::fill(Color::Cyan),
            ChipColor::fill(Color::Magenta),
            ChipStyle::default(),
            None,
            None,
            5,
            80,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        // Tick 0 lights 'w', so the rest of the name is still one run.
        assert!(text.contains("ork"), "{text:?}");
        assert!(text.contains(" notes"), "{text:?}");
        assert!(text.contains(" shell "), "{text:?}");
        assert!(text.contains(" build "), "{text:?}");
        let work = text.find("ork").unwrap();
        let notes = text.find(" notes").unwrap();
        let shell = text.find(" shell ").unwrap();
        let build = text.find(" build ").unwrap();
        assert!(text[..work].contains("\u{1b}[7m"), "{text:?}");
        assert!(text[work..notes].contains("\u{1b}[2m"), "{text:?}");
        assert!(text[notes..shell].contains("\u{1b}[7m"), "{text:?}");
        assert!(text[shell..build].contains("\u{1b}[2m"), "{text:?}");
    }

    #[test]
    fn a_working_agent_uses_the_working_chip_unless_it_is_open() {
        let _colors = colors_on();
        let navy = Color::Rgb {
            r: 0x28,
            g: 0x34,
            b: 0x57,
        };
        let cyan = Color::Rgb {
            r: 0x7d,
            g: 0xcf,
            b: 0xff,
        };
        let working = ChipStyle {
            background: Some(navy),
            foreground: Some(cyan),
        };
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(2, "build", Some(AgentActivity::Working), false),
        ];
        let session = view("work", &["sh"]);
        let layout = laid_out(&items, &session, 80, 24);
        let draw = |chip: ChipStyle| {
            let mut buf = Vec::new();
            draw_bar(
                &mut buf,
                &layout,
                &items,
                0,
                0,
                ChipColor::fill(Color::Cyan),
                ChipColor::fill(Color::Magenta),
                chip,
                None,
                None,
                5,
                80,
            )
            .unwrap();
            String::from_utf8(buf).unwrap()
        };
        let text = draw(working);
        // "build" is working and not open: navy fill, cyan text, drawn
        // before its name.
        let build = text.find("uild").unwrap();
        let fill = text.find("48;2;40;52;87").expect("working fill");
        assert!(text.contains("38;2;125;207;255"), "working text: {text:?}");
        assert!(fill < build, "{text:?}");
        // "work" is working too but open, so it keeps the active chip:
        // nothing navy is drawn before it.
        let work = text.find("ork").unwrap();
        assert!(fill > work, "{text:?}");
        // Unset, a working chip is an ordinary one.
        assert!(!draw(ChipStyle::default()).contains("48;2;40;52;87"));
    }

    #[test]
    fn a_finished_agent_uses_the_configured_background() {
        // NO_COLOR makes crossterm drop color sequences.
        let _colors = colors_on();
        let purple = Color::Rgb {
            r: 0xa8,
            g: 0x55,
            b: 0xf7,
        };
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(2, "notes", Some(AgentActivity::Idle), true),
            running(4, "shell", Some(AgentActivity::Idle), false),
        ];
        let session = view("work", &["sh"]);
        let layout = laid_out(&items, &session, 80, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &items,
            0,
            0,
            ChipColor::fill(Color::Cyan),
            ChipColor::fill(purple),
            ChipStyle::default(),
            None,
            None,
            5,
            80,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let work = text.find(" work").unwrap();
        let notes = text.find(" notes").unwrap();
        let shell = text.find(" shell").unwrap();
        let rgb = "38;2;168;85;247";
        // The open session stays an active chip, working or not.
        assert!(text[..work].contains("\u{1b}[7m"), "{text:?}");
        assert!(!text[..work].contains(rgb), "{text:?}");
        // A working → finished session is a purple reverse chip, not dim.
        // The style is applied immediately before the label.
        assert!(text[..notes].ends_with("\u{1b}[7m"), "{text:?}");
        assert!(text[work..notes].contains(rgb), "{text:?}");
        assert!(!text[work..notes].contains("\u{1b}[2m"), "{text:?}");
        // Idle that never finished a turn stays dim.
        assert!(text[notes..shell].contains("\u{1b}[2m"), "{text:?}");
        assert!(!text[notes..shell].contains(rgb), "{text:?}");

        // The session you are in keeps the active chip even if its flag
        // is still set, so the bar still shows where you are.
        let open = vec![running(2, "notes", Some(AgentActivity::Idle), true)];
        let session = view("notes", &["sh"]);
        let layout = laid_out(&open, &session, 40, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &open,
            2,
            0,
            ChipColor::fill(Color::Cyan),
            ChipColor::fill(purple),
            ChipStyle::default(),
            None,
            None,
            5,
            40,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let notes = text.find(" notes").unwrap();
        assert!(text[..notes].contains("\u{1b}[7m"), "{text:?}");
        assert!(!text[..notes].contains(rgb), "{text:?}");
    }

    #[test]
    fn a_working_menu_entry_uses_the_menu_working_colors() {
        let _colors = colors_on();
        let name_color = Color::Rgb {
            r: 0x7d,
            g: 0xcf,
            b: 0xff,
        };
        let items = vec![
            ListItem {
                label: "shell".into(),
                active: true,
                dim: false,
                working: false,
                finished: false,
            },
            ListItem {
                label: "build".into(),
                active: false,
                dim: false,
                working: true,
                finished: false,
            },
        ];
        let view = ManagerView {
            title: "sessions",
            items: &items,
            selected: 0,
            footer: "esc close",
            search: None,
            search_cursor: 0,
            min_rows: 2,
            min_interior: 0,
        };
        let draw = |working_name: Option<Color>| {
            let mut buf = Vec::new();
            draw_manager(
                &mut buf,
                &view,
                (80, 24),
                Color::Cyan,
                Color::Cyan,
                Color::Magenta,
                working_name,
            )
            .unwrap();
            String::from_utf8(buf).unwrap()
        };
        let text = draw(Some(name_color));
        let build = text.find("build").expect(&text);
        // The working name is drawn in the working name color.
        assert!(text[..build].contains("38;2;125;207;255"), "{text:?}");
        // Unset, the name keeps the terminal's text color.
        assert!(!draw(None).contains("38;2;125;207;255"));
    }

    #[test]
    fn the_open_manager_entry_uses_the_active_background() {
        let _colors = colors_on();
        let accent = Color::Rgb {
            r: 0x11,
            g: 0x22,
            b: 0x33,
        };
        let active = Color::Rgb {
            r: 0xaa,
            g: 0xbb,
            b: 0xcc,
        };
        let items = vec![ListItem {
            label: "work".into(),
            active: true,
            dim: false,
            working: false,
            finished: false,
        }];
        let view = ManagerView {
            title: "sessions",
            items: &items,
            selected: 0,
            footer: "esc close",
            search: None,
            search_cursor: 0,
            min_rows: 1,
            min_interior: 0,
        };
        let mut buf = Vec::new();
        draw_manager(
            &mut buf,
            &view,
            (80, 24),
            accent,
            active,
            Color::Magenta,
            None,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let arrow = text.find('❯').expect(&text);
        let name = text.find("work").expect(&text);
        let accent_sgr = "38;2;17;34;51";
        let active_sgr = "38;2;170;187;204";
        // The selector is the accent. The open name is the active color.
        assert!(text[..arrow].contains(accent_sgr), "{text:?}");
        assert!(!text[..arrow].contains(active_sgr), "{text:?}");
        assert!(text[arrow..name].contains(active_sgr), "{text:?}");
        assert!(!text[arrow..name].contains(accent_sgr), "{text:?}");
    }

    #[test]
    fn a_finished_session_is_named_in_the_highlight_color() {
        let _colors = colors_on();
        let highlight = Color::Rgb {
            r: 0xc7,
            g: 0x61,
            b: 0x1e,
        };
        let active = Color::Rgb {
            r: 0xe4,
            g: 0xe4,
            b: 0xe7,
        };
        let items = vec![
            ListItem {
                label: "build".into(),
                active: false,
                dim: false,
                working: false,
                finished: true,
            },
            ListItem {
                label: "work".into(),
                active: true,
                dim: false,
                working: false,
                finished: true,
            },
        ];
        let view = ManagerView {
            title: "sessions",
            items: &items,
            selected: 1,
            footer: "esc close",
            search: None,
            search_cursor: 0,
            min_rows: 2,
            min_interior: 0,
        };
        let mut buf = Vec::new();
        draw_manager(
            &mut buf,
            &view,
            (80, 24),
            Color::Cyan,
            active,
            highlight,
            None,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let build = text.find("build").expect(&text);
        let work = text.find("work").expect(&text);
        let highlight_sgr = "38;2;199;97;30";
        let active_sgr = "38;2;228;228;231";
        // The finished session that is not on screen takes the highlight
        // as its foreground. The open session keeps the active color.
        assert!(text[..build].contains(highlight_sgr), "{text:?}");
        assert!(text[build..work].contains(active_sgr), "{text:?}");
        assert!(!text[build..work].contains(highlight_sgr), "{text:?}");
    }

    #[test]
    fn a_bar_background_fills_the_row_and_dim_chips_keep_it() {
        let _colors = colors_on();
        let bar = Color::Rgb {
            r: 0x1a,
            g: 0x1b,
            b: 0x26,
        };
        let purple = Color::Rgb {
            r: 0xa8,
            g: 0x55,
            b: 0xf7,
        };
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(1, "notes", Some(AgentActivity::Working), false),
        ];
        let session = view("work", &["sh"]);
        let layout = laid_out(&items, &session, 20, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &items,
            0,
            0,
            ChipColor::fill(Color::Cyan),
            ChipColor::fill(purple),
            ChipStyle::default(),
            Some(bar),
            None,
            5,
            20,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let fill = "\u{1b}[48;2;26;27;38m";
        // The row is painted with spaces in the bar color, not erased
        // to the terminal background.
        assert!(text.contains(fill), "{text:?}");
        assert!(
            text.contains(&format!("{fill}{}", " ".repeat(20))),
            "{text:?}"
        );
        assert!(!text.contains("\u{1b}[K"), "{text:?}");
        // The open chip stays reverse; the dim chip repeats the bar color.
        let notes = text.find(" notes").expect(&text);
        assert!(text[..notes].contains("\u{1b}[7m"), "{text:?}");
        let dim = text[..notes].rfind("\u{1b}[2m").expect(&text);
        assert!(text[dim..notes].contains(fill), "{text:?}");
    }

    #[test]
    fn a_configured_bar_font_colors_every_chip() {
        let _colors = colors_on();
        let font = Color::Rgb {
            r: 0xe4,
            g: 0xe4,
            b: 0xe7,
        };
        let bar = Color::Rgb {
            r: 0x0a,
            g: 0x0a,
            b: 0x0a,
        };
        let open = Color::Rgb {
            r: 0x77,
            g: 0x77,
            b: 0x77,
        };
        let finished = Color::Rgb {
            r: 0xc7,
            g: 0x61,
            b: 0x1e,
        };
        let items = vec![
            running(0, "work", Some(AgentActivity::Idle), false),
            running(1, "notes", Some(AgentActivity::Idle), true),
        ];
        let session = view("work", &["sh"]);
        let layout = laid_out(&items, &session, 40, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &items,
            0,
            0,
            ChipColor::fill(open),
            ChipColor::fill(finished),
            ChipStyle::default(),
            Some(bar),
            Some(font),
            5,
            40,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let font_sgr = "38;2;228;228;231";
        let open_sgr = "48;2;119;119;119";
        let finished_sgr = "48;2;199;97;30";
        let bar_sgr = "48;2;10;10;10";
        let work = text.find("work").expect(&text);
        let notes = text.find("notes").expect(&text);
        // No reverse video. The open chip is font-on-active, the
        // finished chip is font-on-highlight, and an ordinary chip
        // would sit on the bar color.
        assert!(!text.contains("\u{1b}[7m"), "{text:?}");
        assert!(text[..work].contains(font_sgr), "{text:?}");
        assert!(text[..work].contains(open_sgr), "{text:?}");
        assert!(text[work..notes].contains(font_sgr), "{text:?}");
        assert!(text[work..notes].contains(finished_sgr), "{text:?}");
        assert!(text.contains(bar_sgr), "{text:?}");
    }
}
