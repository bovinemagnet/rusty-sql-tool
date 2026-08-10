use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, ScrollHandle, SharedString, Window, WindowBounds,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};
use tokio::runtime::Runtime;

use crate::application::{CommandService, EditorState, ResultDestination, ResultDisplay, command};
use crate::config::{ConnectionProfile, local_profile};
use crate::database::{ConnectionState, DatabaseObject, ObjectKind};
use crate::postgres::PostgresProvider;
use crate::result::{CellValue, ExecutionStatus, QueryResult};

// RepFoundry desktop — Kinetic Green.
const BACKGROUND: u32 = 0x0a0b0d; // window canvas
const SURFACE: u32 = 0x0e1013; // side nav
const PANEL: u32 = 0x15181c; // cards, wells
const PANEL_LIGHT: u32 = 0x1c2026; // hover, segmented
const PANEL_HIGH: u32 = 0x252a31; // selected segment
const CHROME: u32 = 0x101316; // titlebar + status bar
const BORDER: u32 = 0x1a1e22; // ≈ white 7% on BACKGROUND
const TEXT: u32 = 0xf3f6f4;
const MUTED: u32 = 0x8b938f;
const FAINT: u32 = 0x565d59; // gutter, placeholders
const ACCENT: u32 = 0x00e5a0; // keywords, connected, primary
const ON_ACCENT: u32 = 0x04130d; // text on ACCENT fills
const ACCENT_SOFT: u32 = 0x11241f; // ACCENT @14% over PANEL
const STRING: u32 = 0xc6ff3d; // volt — literals
const FUNCTION: u32 = 0x38bdf8;
const WARN: u32 = 0xffb454; // connection change in progress
const RED: u32 = 0xff5d73;

// Metrics, from the "Metrics & type" table of the same design.
const TITLEBAR_HEIGHT: f32 = 44.;
const STATUS_BAR_HEIGHT: f32 = 34.;
const SIDEBAR_WIDTH: f32 = 252.;
const CONTROL_HEIGHT: f32 = 42.;
const CARD_RADIUS: f32 = 18.;
const CONTROL_RADIUS: f32 = 12.;
const EDITOR_LINE_HEIGHT: f32 = 25.;
const EDITOR_TEXT_SIZE: f32 = 14.;
const GUTTER_WIDTH: f32 = 40.;
const RESULT_PANE_HEIGHT: f32 = 296.;
const RESULT_PANE_MIN_HEIGHT: f32 = 120.;
/// Vertical space the rest of the workspace needs — chrome, header, toolbar, status bar and a
/// usable editor — so dragging the splitter can never squeeze the editor out of existence.
const RESULT_PANE_RESERVED: f32 = 380.;
const SPLITTER_HEIGHT: f32 = 10.;
const RESULT_TEXT_SIZE: f32 = 13.;
const RESULT_LINE_HEIGHT: f32 = 22.;
/// Left inset of a text-result line, matching its `px_5` padding. The selection highlight and the
/// pointer-to-column arithmetic both measure from here.
const RESULT_TEXT_INSET: f32 = 20.;

/// The single reusable slot for a connection typed into the dialog.
const MANUAL_PROFILE_NAME: &str = "Manual";

const SCROLLBAR_THICKNESS: f32 = 10.;
const SCROLLBAR_THUMB: f32 = 6.;
const SCROLLBAR_MIN_THUMB: f32 = 24.;

/// GPUI's `overflow_*_scroll` gives wheel scrolling but paints nothing, so the bars are drawn from
/// the tracked scroll handle's geometry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollAxis {
    Vertical,
    Horizontal,
}

/// An in-progress thumb drag. Anchoring to where the drag began keeps the thumb under the pointer
/// instead of jumping to it.
#[derive(Clone, Copy)]
struct ScrollbarDrag {
    axis: ScrollAxis,
    pointer_origin: Pixels,
    offset_origin: Pixels,
}

/// One scrolling surface: the tracked handle plus any drag in progress. Views own one per surface,
/// which is what lets the same scrollbar code serve the main window and the results window.
#[derive(Default)]
struct ScrollState {
    handle: ScrollHandle,
    drag: Option<ScrollbarDrag>,
}

impl ScrollState {
    fn set_offset(&self, axis: ScrollAxis, value: Pixels) {
        let offset = self.handle.offset();
        self.handle.set_offset(match axis {
            ScrollAxis::Vertical => point(offset.x, value),
            ScrollAxis::Horizontal => point(value, offset.y),
        });
    }

    /// Advances an in-progress drag. Returns whether anything changed.
    fn drag(&mut self, event: &MouseMoveEvent) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        if !event.dragging() {
            self.drag = None;
            return true;
        }
        let vertical = drag.axis == ScrollAxis::Vertical;
        let viewport = self.handle.bounds().size;
        let overflow = self.handle.max_offset();
        let (extent, travel, pointer) = if vertical {
            (viewport.height, overflow.height, event.position.y)
        } else {
            (viewport.width, overflow.width, event.position.x)
        };
        let Some(metrics) = ThumbMetrics::measure(extent, travel, px(0.)) else {
            return false;
        };
        let moved = (pointer - drag.pointer_origin) * metrics.pixels_per_thumb_pixel;
        self.set_offset(
            drag.axis,
            (drag.offset_origin - moved).clamp(-travel, px(0.)),
        );
        true
    }
}

/// Reaches a view's scroll surface from inside an event listener.
type ScrollAccessor<V> = fn(&mut V) -> &mut ScrollState;

/// Wraps scrolling content in a viewport with drawn scrollbars, for any view that owns a
/// [`ScrollState`]. GPUI paints no scrollbars of its own.
fn scrollable<V: 'static>(
    access: ScrollAccessor<V>,
    state: &ScrollState,
    id: &'static str,
    content: impl IntoElement,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let handle = state.handle.clone();
    let viewport = handle.bounds().size;
    let overflow = handle.max_offset();
    let offset = handle.offset();
    let vertical = ThumbMetrics::measure(viewport.height, overflow.height, offset.y);
    let horizontal = ThumbMetrics::measure(viewport.width, overflow.width, offset.x);

    div()
        .relative()
        .flex_1()
        .min_h_0()
        .child(
            div()
                .id(id)
                .absolute()
                .size_full()
                // `items_start` is load-bearing: without it the content stretches to the viewport
                // width, so a wide grid overflows the content wrapper rather than the scroll
                // container, and the container measures nothing to scroll sideways.
                .flex()
                .flex_col()
                .items_start()
                .overflow_y_scroll()
                .overflow_x_scroll()
                .track_scroll(&handle)
                .child(content),
        )
        .children(vertical.map(|metrics| scrollbar(access, id, ScrollAxis::Vertical, metrics, cx)))
        .children(
            horizontal.map(|metrics| scrollbar(access, id, ScrollAxis::Horizontal, metrics, cx)),
        )
}

/// One bar. Clicking the track jumps to that position; dragging the thumb scrolls with it.
fn scrollbar<V: 'static>(
    access: ScrollAccessor<V>,
    id: &'static str,
    axis: ScrollAxis,
    metrics: ThumbMetrics,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let vertical = axis == ScrollAxis::Vertical;
    let suffix = if vertical { "y" } else { "x" };
    let ThumbMetrics {
        length,
        start,
        pixels_per_thumb_pixel,
        scrollable,
    } = metrics;

    let mut thumb = div()
        .id(SharedString::from(format!("{id}-thumb-{suffix}")))
        .absolute()
        .rounded_full()
        .bg(rgb(PANEL_HIGH))
        .hover(|style| style.bg(rgb(MUTED)))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                let state = access(view);
                let offset = state.handle.offset();
                state.drag = Some(ScrollbarDrag {
                    axis,
                    pointer_origin: if vertical {
                        event.position.y
                    } else {
                        event.position.x
                    },
                    offset_origin: if vertical { offset.y } else { offset.x },
                });
                cx.notify();
            }),
        );
    thumb = if vertical {
        thumb
            .top(start)
            .right(px((SCROLLBAR_THICKNESS - SCROLLBAR_THUMB) / 2.))
            .w(px(SCROLLBAR_THUMB))
            .h(length)
    } else {
        thumb
            .left(start)
            .bottom(px((SCROLLBAR_THICKNESS - SCROLLBAR_THUMB) / 2.))
            .h(px(SCROLLBAR_THUMB))
            .w(length)
    };

    let track = div()
        .id(SharedString::from(format!("{id}-track-{suffix}")))
        .absolute()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                // A click on the bare track pages to roughly that position.
                let state = access(view);
                let bounds = state.handle.bounds();
                let (pointer, origin) = if vertical {
                    (event.position.y, bounds.origin.y)
                } else {
                    (event.position.x, bounds.origin.x)
                };
                let travelled = (pointer - origin - length / 2.) * pixels_per_thumb_pixel;
                state.set_offset(axis, -travelled.clamp(px(0.), scrollable));
                cx.notify();
            }),
        )
        .child(thumb);
    if vertical {
        track
            .top(px(0.))
            .bottom(px(0.))
            .right(px(0.))
            .w(px(SCROLLBAR_THICKNESS))
    } else {
        track
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .h(px(SCROLLBAR_THICKNESS))
    }
}

/// Thumb geometry for one axis, or `None` when the content fits and no bar is warranted.
struct ThumbMetrics {
    length: Pixels,
    start: Pixels,
    /// Content pixels travelled per pixel of thumb movement.
    pixels_per_thumb_pixel: f32,
    scrollable: Pixels,
}

impl ThumbMetrics {
    fn measure(viewport: Pixels, overflow: Pixels, offset: Pixels) -> Option<Self> {
        if overflow <= px(0.) || viewport <= px(0.) {
            return None;
        }
        let content = viewport + overflow;
        let track = viewport;
        // The floor keeps the thumb grabbable on very large result sets; on a very short viewport
        // it can reach the whole track, leaving nothing to travel along.
        let length = (track * (viewport / content))
            .max(px(SCROLLBAR_MIN_THUMB))
            .min(track);
        let usable = (track - length).max(px(0.));
        let progress = (-offset / overflow).clamp(0., 1.);
        Some(Self {
            length,
            // Multiplying by a zero progress yields -0px, which sorts below zero under the total
            // ordering `Pixels` uses, so the zero case is taken directly.
            start: if progress <= 0. {
                px(0.)
            } else {
                (usable * progress).min(usable)
            },
            pixels_per_thumb_pixel: if usable > px(0.) {
                overflow / usable
            } else {
                0.
            },
            scrollable: overflow,
        })
    }
}

/// The design names Space Grotesk for headings, Manrope for body copy and JetBrains Mono for every
/// datum. None of the three is guaranteed to be installed, so each resolves against the fonts the
/// platform actually reports and falls back to a generic family.
#[derive(Clone)]
struct Fonts {
    display: SharedString,
    body: SharedString,
    mono: SharedString,
}

impl Fonts {
    fn resolve(cx: &App) -> Self {
        let available = cx.text_system().all_font_names();
        Self {
            display: resolve_family(&available, "Space Grotesk", "sans-serif"),
            body: resolve_family(&available, "Manrope", "sans-serif"),
            mono: resolve_family(&available, "JetBrains Mono", "monospace"),
        }
    }
}

/// The advance width of one monospace character at the result text size. Falls back to a typical
/// 0.6em ratio if the glyph cannot be measured, which keeps selection usable rather than collapsing
/// every column onto zero.
fn measure_mono_advance(fonts: &Fonts, cx: &App) -> Pixels {
    let text_system = cx.text_system();
    let font_id = text_system.resolve_font(&gpui::font(fonts.mono.clone()));
    text_system
        .advance(font_id, px(RESULT_TEXT_SIZE), '0')
        .map(|advance| advance.width)
        .ok()
        .filter(|advance| *advance > px(0.))
        .unwrap_or(px(RESULT_TEXT_SIZE * 0.6))
}

fn resolve_family(available: &[String], preferred: &str, fallback: &'static str) -> SharedString {
    if available.iter().any(|name| name == preferred) {
        preferred.to_owned().into()
    } else {
        fallback.into()
    }
}

pub fn launch() {
    gpui::Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Rusty SQL Tool".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(AppView::new);
                window.focus(&view.read(cx).focus_handle(cx));
                view
            },
        )
        .expect("could not create the application window");
        cx.activate(true);
    });
}

struct AppView {
    service: Arc<CommandService>,
    runtime: Arc<Runtime>,
    profiles: Vec<ConnectionProfile>,
    editor: EditorState,
    background_editors: Vec<EditorState>,
    connection_state: ConnectionState,
    server_version: Option<String>,
    schemas: Vec<String>,
    objects: HashMap<String, Vec<DatabaseObject>>,
    expanded_schema: Option<String>,
    active_result_tab: bool,
    status: String,
    focus_handle: FocusHandle,
    undo: Vec<String>,
    redo: Vec<String>,
    connection_dialog: bool,
    connection_buffer: String,
    fonts: Fonts,
    /// The profile the single provider session is actually bound to. There is exactly one session,
    /// so this — not the active editor — is the truth about which database SQL will reach.
    session_profile: Option<ConnectionProfile>,
    /// Whether the profile list came from real configuration rather than the built-in fallback.
    configured: bool,
    editor_scroll: ScrollState,
    results_scroll: ScrollState,
    /// Selected text in the results, and whether a drag is extending it.
    result_selection: Option<ResultSelection>,
    selecting_results: bool,
    /// Advance width of one monospace character at the result text size, used to turn a pointer
    /// position into a column and to size the selection highlight.
    mono_advance: Pixels,
    result_pane_height: Pixels,
    pane_drag: Option<PaneDrag>,
}

/// An in-progress splitter drag, anchored to where it began so the grip stays under the pointer.
#[derive(Clone, Copy)]
struct PaneDrag {
    pointer_origin: Pixels,
    height_origin: Pixels,
}

/// A caret position in the rendered results: which line, and how many characters into it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct ResultPosition {
    line: usize,
    column: usize,
}

/// Selected text, held as the two ends of the gesture so dragging backwards works.
///
/// GPUI paints no text selection of its own, so the highlight is drawn and the column derived
/// arithmetically from the pointer — sound here because the results are monospace.
#[derive(Clone, Copy)]
struct ResultSelection {
    anchor: ResultPosition,
    head: ResultPosition,
}

impl ResultSelection {
    /// Whole lines end to end, for grid rows and select-all where a partial line has no meaning.
    fn whole_lines(first: usize, last: usize) -> Self {
        Self {
            anchor: ResultPosition {
                line: first,
                column: 0,
            },
            head: ResultPosition {
                line: last,
                column: usize::MAX,
            },
        }
    }

    fn ordered(&self) -> (ResultPosition, ResultPosition) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn bounds(&self) -> (usize, usize) {
        let (start, end) = self.ordered();
        (start.line, end.line)
    }

    fn contains(&self, line: usize) -> bool {
        let (first, last) = self.bounds();
        (first..=last).contains(&line)
    }

    /// The selected characters within one line, clamped to its length. `None` only when the line
    /// falls outside the selection — a line inside it that contributes no characters still yields
    /// an empty range, because it is a blank line the copy must preserve as a line break.
    fn clamped_span(&self, line: usize, length: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line {
            start.column.min(length)
        } else {
            0
        };
        let to = if line == end.line {
            end.column.min(length)
        } else {
            length
        };
        Some(from..to.max(from))
    }

    /// The span worth painting a highlight over: as above, minus the empty ones.
    fn span_for(&self, line: usize, length: usize) -> Option<Range<usize>> {
        self.clamped_span(line, length)
            .filter(|span| !span.is_empty())
    }
}

impl AppView {
    fn new(cx: &mut Context<Self>) -> Self {
        let (profiles, configured) = discover_profiles();
        let fonts = Fonts::resolve(cx);
        let fonts_for_advance = fonts.clone();
        let profile = profiles
            .first()
            .cloned()
            .expect("at least one connection profile should be available");
        let provider = PostgresProvider::new();
        let service = Arc::new(CommandService::new(provider));
        Self {
            service,
            runtime: Arc::new(Runtime::new().expect("could not start asynchronous runtime")),
            profiles,
            editor: EditorState::new(profile),
            background_editors: Vec::new(),
            connection_state: ConnectionState::Disconnected,
            server_version: None,
            schemas: Vec::new(),
            objects: HashMap::new(),
            expanded_schema: None,
            active_result_tab: false,
            status: "Disconnected · Run a query to see results.".into(),
            focus_handle: cx.focus_handle(),
            undo: Vec::new(),
            redo: Vec::new(),
            connection_dialog: false,
            connection_buffer: String::new(),
            fonts,
            session_profile: None,
            configured,
            editor_scroll: ScrollState::default(),
            results_scroll: ScrollState::default(),
            result_selection: None,
            selecting_results: false,
            mono_advance: measure_mono_advance(&fonts_for_advance, cx),
            result_pane_height: px(RESULT_PANE_HEIGHT),
            pane_drag: None,
        }
    }

    /// Advances a splitter drag. The pane sits at the bottom, so dragging up makes it taller.
    fn drag_pane(&mut self, pointer: Pixels, viewport_height: Pixels) -> bool {
        let Some(drag) = self.pane_drag else {
            return false;
        };
        let ceiling = (viewport_height - px(RESULT_PANE_RESERVED)).max(px(RESULT_PANE_MIN_HEIGHT));
        let height = (drag.height_origin + (drag.pointer_origin - pointer))
            .clamp(px(RESULT_PANE_MIN_HEIGHT), ceiling);
        if height == self.result_pane_height {
            return false;
        }
        self.result_pane_height = height;
        true
    }

    /// The drag handle between the editor and the results pane.
    fn results_splitter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dragging = self.pane_drag.is_some();
        div()
            .id("results-splitter")
            .flex_none()
            .h(px(SPLITTER_HEIGHT))
            .mx(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .cursor(gpui::CursorStyle::ResizeUpDown)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.pane_drag = Some(PaneDrag {
                        pointer_origin: event.position.y,
                        height_origin: this.result_pane_height,
                    });
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(48.))
                    .h(px(3.))
                    .rounded_full()
                    .bg(rgb(if dragging { ACCENT } else { PANEL_HIGH })),
            )
    }

    /// Turns a pointer x-position into a character column. Every text line shares one left edge, so
    /// this needs no per-line geometry; rounding gives caret-between-characters behaviour.
    fn column_at(&self, x: Pixels) -> usize {
        if self.mono_advance <= px(0.) {
            return 0;
        }
        let bounds = self.results_scroll.handle.bounds();
        let offset = self.results_scroll.handle.offset();
        let text_left = bounds.origin.x + offset.x + px(RESULT_TEXT_INSET);
        let columns = ((x - text_left) / self.mono_advance).round();
        if columns <= 0. { 0 } else { columns as usize }
    }

    fn begin_selection(&mut self, position: ResultPosition, extend: bool) {
        self.selecting_results = true;
        self.result_selection = Some(match (extend, self.result_selection) {
            (true, Some(existing)) => ResultSelection {
                anchor: existing.anchor,
                head: position,
            },
            _ => ResultSelection {
                anchor: position,
                head: position,
            },
        });
    }

    /// Extends an in-progress drag. Returns whether anything moved.
    fn extend_selection(&mut self, position: ResultPosition) -> bool {
        if !self.selecting_results {
            return false;
        }
        match self.result_selection.as_mut() {
            Some(selection) if selection.head != position => {
                selection.head = position;
                true
            }
            _ => false,
        }
    }

    /// Every selectable unit of every result, in render order — one entry per rendered line in
    /// Text mode, per header and data row in Table mode. Selection indices point into this, and it
    /// is also what gets copied, so what you select is exactly what you get.
    ///
    /// Table rows are tab separated so a paste lands in spreadsheet columns.
    fn selectable_lines(&self) -> Vec<String> {
        self.editor
            .results
            .iter()
            .flat_map(|result| match self.editor.display {
                ResultDisplay::Text => result
                    .as_text()
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>(),
                ResultDisplay::Table => {
                    let mut lines = Vec::with_capacity(result.rows.len() + 1);
                    if !result.columns.is_empty() {
                        lines.push(
                            result
                                .columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect::<Vec<_>>()
                                .join("\t"),
                        );
                    }
                    lines.extend(result.rows.iter().map(|row| {
                        row.iter()
                            .map(CellValue::to_display_string)
                            .collect::<Vec<_>>()
                            .join("\t")
                    }));
                    lines
                }
            })
            .collect()
    }

    /// The selected lines, or the whole result when nothing is selected — copying with no selection
    /// should still give the user their data.
    fn copyable_result_text(&self) -> Option<String> {
        let lines = self.selectable_lines();
        if lines.is_empty() {
            return None;
        }
        let Some(selection) = self.result_selection else {
            return Some(lines.join("\n"));
        };
        let (start, end) = selection.ordered();
        let last = end.line.min(lines.len().saturating_sub(1));
        if start.line > last {
            return None;
        }
        let mut selected = Vec::new();
        for (index, line) in lines.iter().enumerate().take(last + 1).skip(start.line) {
            let characters: Vec<char> = line.chars().collect();
            if let Some(span) = selection.clamped_span(index, characters.len()) {
                selected.push(characters[span].iter().collect::<String>());
            }
        }
        // An unmoved caret selects no text at all, so there is nothing to put on the clipboard.
        let text = selected.join("\n");
        (!text.is_empty()).then_some(text)
    }

    fn copy_results(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.copyable_result_text() else {
            return;
        };
        let lines = text.lines().count();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.status = format!("Copied {lines} line{}", if lines == 1 { "" } else { "s" });
        cx.notify();
    }

    fn select_all_results(&mut self, cx: &mut Context<Self>) {
        let count = self.selectable_lines().len();
        if count == 0 {
            return;
        }
        self.result_selection = Some(ResultSelection::whole_lines(0, count - 1));
        cx.notify();
    }

    /// True when the results surface, rather than the SQL document, should answer copy and
    /// select-all: the result tab is in front, or lines are already selected.
    fn results_have_focus(&self) -> bool {
        !self.editor.results.is_empty()
            && (self.active_result_tab || self.result_selection.is_some())
    }

    /// The connection SQL will actually run against: the live session when there is one, otherwise
    /// the target the editor is pointed at.
    fn target_profile(&self) -> &ConnectionProfile {
        self.session_profile
            .as_ref()
            .unwrap_or(&self.editor.connection)
    }

    fn target_identity(&self) -> String {
        self.target_profile().display_identity()
    }

    fn is_running(&self) -> bool {
        matches!(
            self.editor.execution_status,
            ExecutionStatus::Running | ExecutionStatus::Cancelling
        )
    }

    /// Binds every editor to the profile the session actually opened, so switching tabs can never
    /// surface a stale target while one connection is live.
    fn adopt_session_profile(&mut self, profile: ConnectionProfile) {
        self.editor.connection = profile.clone();
        for editor in &mut self.background_editors {
            editor.connection = profile.clone();
        }
        self.session_profile = Some(profile);
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.connection_state,
            ConnectionState::Connecting | ConnectionState::Connected
        ) {
            return;
        }
        self.connection_state = ConnectionState::Connecting;
        self.status = "Connecting…".into();
        let service = self.service.clone();
        let runtime = self.runtime.clone();
        let profile = self.editor.connection.clone();
        let connected_profile = profile.clone();
        cx.spawn(async move |view, cx| {
            let joined = runtime
                .spawn(async move {
                    let info = service.connect(&profile).await?;
                    let schemas = service.schemas(false).await?;
                    Ok::<_, crate::result::QueryError>((info, schemas))
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                match joined {
                    Ok(Ok((info, schemas))) => {
                        this.connection_state = ConnectionState::Connected;
                        this.server_version = Some(info.server_version);
                        this.schemas = schemas;
                        // Record what the session is bound to, and point every editor at it so no
                        // tab can display a target the session is not using (§49, §50).
                        this.adopt_session_profile(connected_profile);
                        this.status = format!("Connected: {}", this.target_identity());
                    }
                    Ok(Err(error)) => {
                        this.connection_state = ConnectionState::Failed;
                        this.status = format!("Connection failed: {error}");
                    }
                    Err(_) => {
                        this.connection_state = ConnectionState::Failed;
                        this.status = "Connection task failed".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        // Closing the session cancels whatever is in flight and loses its results, so the running
        // query has to finish or be stopped first — the same rule the connection rows enforce.
        if self.is_running() {
            self.status = "Wait for the active query before disconnecting".into();
            cx.notify();
            return;
        }
        self.connection_state = ConnectionState::Disconnecting;
        self.status = "Disconnecting…".into();
        let service = self.service.clone();
        let runtime = self.runtime.clone();
        cx.spawn(async move |view, cx| {
            let result = runtime
                .spawn(async move { service.disconnect().await })
                .await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {
                        this.connection_state = ConnectionState::Disconnected;
                        this.session_profile = None;
                        this.schemas.clear();
                        this.objects.clear();
                        this.expanded_schema = None;
                        this.status = "Disconnected · SQL and results preserved".into();
                    }
                    _ => {
                        this.connection_state = ConnectionState::Failed;
                        this.status = "Disconnect failed".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_schema(&mut self, schema: String, refresh: bool, cx: &mut Context<Self>) {
        if self.expanded_schema.as_deref() == Some(&schema) && !refresh {
            self.expanded_schema = None;
            cx.notify();
            return;
        }
        self.expanded_schema = Some(schema.clone());
        self.status = format!("Loading {schema}…");
        let service = self.service.clone();
        let runtime = self.runtime.clone();
        cx.spawn(async move |view, cx| {
            let schema_for_query = schema.clone();
            let result = runtime
                .spawn(async move { service.objects(&schema_for_query, refresh).await })
                .await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(objects)) => {
                        this.objects.insert(schema.clone(), objects);
                        this.status = format!("Loaded schema {schema}");
                    }
                    Ok(Err(error)) => this.status = format!("Metadata failed: {error}"),
                    Err(_) => this.status = "Metadata task failed".into(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn refresh_metadata(&mut self, cx: &mut Context<Self>) {
        if let Some(schema) = self.expanded_schema.clone() {
            self.load_schema(schema, true, cx);
            return;
        }
        let service = self.service.clone();
        let runtime = self.runtime.clone();
        self.status = "Refreshing schemas…".into();
        cx.spawn(async move |view, cx| {
            let result = runtime
                .spawn(async move { service.schemas(true).await })
                .await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(schemas)) => {
                        this.schemas = schemas;
                        this.objects.clear();
                        this.status = "Metadata refreshed".into();
                    }
                    Ok(Err(error)) => this.status = format!("Refresh failed: {error}"),
                    Err(_) => this.status = "Refresh task failed".into(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn run(&mut self, mode: RunMode, cx: &mut Context<Self>) {
        if self.connection_state != ConnectionState::Connected {
            self.status = "Connect before executing SQL".into();
            cx.notify();
            return;
        }
        // One session means one statement at a time. Guarding here covers every route in — the
        // toolbar, the shortcuts, and anything added later.
        if self.is_running() {
            self.status = "A query is already running".into();
            cx.notify();
            return;
        }
        self.editor.execution_status = ExecutionStatus::Running;
        self.status = match mode {
            RunMode::Current => "Running statement…",
            RunMode::All => "Running all statements…",
            RunMode::Explain => "Explaining statement…",
        }
        .into();
        let service = self.service.clone();
        let runtime = self.runtime.clone();
        let mut editor = self.editor.clone();
        cx.spawn(async move |view, cx| {
            let joined = runtime
                .spawn(async move {
                    let status =
                        match mode {
                            RunMode::Current => {
                                service.run(&mut editor).await.map(|_| ()).map_err(|error| {
                                    RunFailure {
                                        statement_index: None,
                                        error,
                                    }
                                })
                            }
                            RunMode::Explain => service
                                .explain(&mut editor)
                                .await
                                .map(|_| ())
                                .map_err(|error| RunFailure {
                                    statement_index: None,
                                    error,
                                }),
                            // `run_all` reports a failing statement inside a successful `Result`, so
                            // the outcome has to be inspected or a failure reads as a clean run.
                            RunMode::All => match service.run_all(&mut editor).await {
                                Ok(outcome) => match outcome.failure {
                                    Some(failure) => Err(RunFailure {
                                        statement_index: Some(failure.statement_index),
                                        error: failure.error,
                                    }),
                                    None => Ok(()),
                                },
                                Err(error) => Err(RunFailure {
                                    statement_index: None,
                                    error: crate::result::QueryError {
                                        message: error.to_string(),
                                        severity: None,
                                        code: None,
                                        detail: None,
                                        hint: None,
                                        position: None,
                                    },
                                }),
                            },
                        };
                    (editor, status)
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                match joined {
                    Ok((editor, result)) => {
                        // Only the execution outcome comes back. The document, cursor and
                        // selection stay as they are so anything typed while the query ran
                        // survives (FR-046, FR-047).
                        this.editor.results = editor.results;
                        this.editor.error = editor.error;
                        this.editor.execution_status = editor.execution_status;
                        // A selection into the previous result set means nothing against this one.
                        this.result_selection = None;
                        if result.is_ok() {
                            match this.editor.destination {
                                ResultDestination::Pane => this.active_result_tab = false,
                                ResultDestination::Tab => this.active_result_tab = true,
                                ResultDestination::Window => {
                                    this.active_result_tab = false;
                                    let results = this.editor.results.clone();
                                    let display = this.editor.display;
                                    let fonts = this.fonts.clone();
                                    let bounds =
                                        Bounds::centered(None, size(px(900.), px(600.)), cx);
                                    let _ = cx.open_window(
                                        WindowOptions {
                                            window_bounds: Some(WindowBounds::Windowed(bounds)),
                                            titlebar: Some(gpui::TitlebarOptions {
                                                title: Some("SQL Results".into()),
                                                ..Default::default()
                                            }),
                                            ..Default::default()
                                        },
                                        |_, cx| {
                                            cx.new(|_| ResultWindow {
                                                results,
                                                display,
                                                fonts,
                                                scroll: ScrollState::default(),
                                            })
                                        },
                                    );
                                }
                            }
                        }
                        this.status = match result {
                            Ok(()) => completion_status(&this.editor.results),
                            Err(failure) => failure.status_message(),
                        };
                    }
                    Err(_) => this.status = "Query task failed".into(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.editor.execution_status != ExecutionStatus::Running {
            return;
        }
        self.editor.execution_status = ExecutionStatus::Cancelling;
        self.status = "Cancelling…".into();
        let service = self.service.clone();
        let runtime = self.runtime.clone();
        let mut editor = self.editor.clone();
        cx.spawn(async move |view, cx| {
            let result = runtime
                .spawn(async move {
                    let result = service.cancel(&mut editor).await;
                    (editor, result)
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                if let Ok((editor, outcome)) = result {
                    this.editor.execution_status = editor.execution_status;
                    this.editor.error = editor.error;
                    this.status = if outcome.is_ok() {
                        "Query cancelled".into()
                    } else {
                        "Cancellation failed".into()
                    };
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let command = modifiers.platform || modifiers.control;
        if self.connection_dialog {
            if command && key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.connection_buffer.push_str(text.trim());
                    cx.notify();
                }
                return;
            }
            match key {
                "escape" => {
                    self.connection_buffer.clear();
                    self.connection_dialog = false;
                    cx.notify();
                }
                "enter" => {
                    match ConnectionProfile::from_database_url(&self.connection_buffer) {
                        Ok(mut profile) => {
                            profile.name = MANUAL_PROFILE_NAME.into();
                            self.editor.connection = profile.clone();
                            // One manual slot, reused. Correcting a mistyped URL must not leave the
                            // unreachable host behind as a second identical row.
                            match self
                                .profiles
                                .iter_mut()
                                .find(|candidate| candidate.name == MANUAL_PROFILE_NAME)
                            {
                                Some(existing) => *existing = profile,
                                None => self.profiles.push(profile),
                            }
                            self.status = "Manual connection configured".into();
                            self.connection_buffer.clear();
                            self.connection_dialog = false;
                        }
                        Err(error) => self.status = format!("Invalid connection: {error}"),
                    }
                    cx.notify();
                }
                "backspace" => {
                    self.connection_buffer.pop();
                    cx.notify();
                }
                _ => {
                    if let Some(character) = event.keystroke.key_char.as_deref()
                        && !character.chars().any(char::is_control)
                    {
                        self.connection_buffer.push_str(character);
                        cx.notify();
                    }
                }
            }
            return;
        }
        if let Some(command_id) = shortcut_command(
            key,
            command,
            modifiers.shift,
            modifiers.alt,
            self.editor.execution_status == ExecutionStatus::Running,
            self.connection_state == ConnectionState::Connected,
        ) {
            self.dispatch_command(command_id, cx);
            return;
        }
        if command {
            match key {
                // Copy and select-all address the results when those are what the user is looking
                // at, and the SQL document otherwise.
                "a" if self.results_have_focus() => self.select_all_results(cx),
                "a" => {
                    self.editor.selection = Some(0..self.editor.document.len());
                    self.editor.cursor = self.editor.document.len();
                    cx.notify();
                }
                "c" if self.results_have_focus() => self.copy_results(cx),
                "c" => {
                    if let Some(range) = self.editor.selection.clone()
                        && !range.is_empty()
                    {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.editor.document[range].to_owned(),
                        ));
                    }
                }
                "x" => {
                    if let Some(range) = self.editor.selection.clone()
                        && !range.is_empty()
                    {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.editor.document[range.clone()].to_owned(),
                        ));
                        self.record_edit();
                        self.editor.document.replace_range(range.clone(), "");
                        self.editor.cursor = range.start;
                        self.editor.selection = None;
                        cx.notify();
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.insert_text(&text, cx);
                    }
                }
                "z" if modifiers.shift => self.redo(cx),
                "z" => self.undo(cx),
                _ => {}
            }
            return;
        }
        match key {
            "backspace" => self.backspace(cx),
            "delete" => self.delete(cx),
            "left" => self.move_cursor(false, modifiers.shift, cx),
            "right" => self.move_cursor(true, modifiers.shift, cx),
            "enter" => self.insert_text("\n", cx),
            "tab" => self.insert_text("    ", cx),
            _ => {
                if let Some(character) = event.keystroke.key_char.as_deref()
                    && !character.chars().any(char::is_control)
                {
                    self.insert_text(character, cx);
                }
            }
        }
    }

    /// Dispatches stable command IDs independently from their current key bindings (section 51).
    fn dispatch_command(&mut self, command_id: &str, cx: &mut Context<Self>) {
        match command_id {
            command::RUN => self.run(RunMode::Current, cx),
            command::RUN_ALL => self.run(RunMode::All, cx),
            command::EXPLAIN => self.run(RunMode::Explain, cx),
            command::CANCEL => self.cancel(cx),
            command::NEW_EDITOR => self.new_editor(cx),
            command::CONNECT => self.connect(cx),
            command::DISCONNECT => self.disconnect(cx),
            _ => {}
        }
    }

    fn configure_connection(&mut self, cx: &mut Context<Self>) {
        if self.connection_state != ConnectionState::Disconnected {
            return;
        }
        self.connection_buffer.clear();
        self.connection_dialog = true;
        self.status = "Enter a PostgreSQL URL; credentials are masked".into();
        cx.notify();
    }

    fn record_edit(&mut self) {
        self.undo.push(self.editor.document.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.record_edit();
        let range = self
            .editor
            .selection
            .take()
            .filter(|range| !range.is_empty())
            .unwrap_or(self.editor.cursor..self.editor.cursor);
        self.editor.document.replace_range(range.clone(), text);
        self.editor.cursor = range.start + text.len();
        cx.notify();
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        if let Some(range) = self.editor.selection.take()
            && !range.is_empty()
        {
            self.record_edit();
            self.editor.document.replace_range(range.clone(), "");
            self.editor.cursor = range.start;
            cx.notify();
            return;
        }
        if self.editor.cursor > 0 {
            self.record_edit();
            let previous = previous_boundary(&self.editor.document, self.editor.cursor);
            self.editor
                .document
                .replace_range(previous..self.editor.cursor, "");
            self.editor.cursor = previous;
            cx.notify();
        }
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if self.editor.cursor < self.editor.document.len() {
            self.record_edit();
            let next = next_boundary(&self.editor.document, self.editor.cursor);
            self.editor
                .document
                .replace_range(self.editor.cursor..next, "");
            cx.notify();
        }
    }

    fn move_cursor(&mut self, right: bool, selecting: bool, cx: &mut Context<Self>) {
        let old = self.editor.cursor;
        let new = if right {
            next_boundary(&self.editor.document, old)
        } else {
            previous_boundary(&self.editor.document, old)
        };
        self.editor.cursor = new;
        if selecting {
            let existing = self.editor.selection.take().unwrap_or(old..old);
            self.editor.selection = Some(existing.start.min(new)..existing.end.max(new));
        } else {
            self.editor.selection = None;
        }
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.editor.document, previous));
            self.editor.cursor = self.editor.document.len();
            self.editor.selection = None;
            cx.notify();
        }
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.redo.pop() {
            self.undo
                .push(std::mem::replace(&mut self.editor.document, next));
            self.editor.cursor = self.editor.document.len();
            self.editor.selection = None;
            cx.notify();
        }
    }

    fn set_display(&mut self, display: ResultDisplay, cx: &mut Context<Self>) {
        if self.editor.display != display {
            // Indices address rendered lines in Text mode and rows in Table mode, so a selection
            // carried across the switch would highlight unrelated data.
            self.result_selection = None;
        }
        self.editor.display = display;
        cx.notify();
    }

    fn set_destination(&mut self, destination: ResultDestination, cx: &mut Context<Self>) {
        self.editor.destination = destination;
        cx.notify();
    }

    fn show_editor_tab(&mut self, cx: &mut Context<Self>) {
        self.active_result_tab = false;
        cx.notify();
    }

    fn show_result_tab(&mut self, cx: &mut Context<Self>) {
        self.active_result_tab = true;
        cx.notify();
    }

    fn new_editor(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.editor.execution_status,
            ExecutionStatus::Running | ExecutionStatus::Cancelling
        ) {
            return;
        }
        let mut editor = EditorState::new(self.editor.connection.clone());
        editor.title = format!("query-{}.sql", self.background_editors.len() + 2);
        self.background_editors
            .push(std::mem::replace(&mut self.editor, editor));
        self.active_result_tab = false;
        self.status = "New SQL editor".into();
        cx.notify();
    }

    fn switch_editor(&mut self, index: usize, cx: &mut Context<Self>) {
        if matches!(
            self.editor.execution_status,
            ExecutionStatus::Running | ExecutionStatus::Cancelling
        ) {
            return;
        }
        if let Some(editor) = self.background_editors.get_mut(index) {
            std::mem::swap(&mut self.editor, editor);
            self.active_result_tab = false;
            self.status = format!("Editor: {}", self.editor.connection_identity());
            cx.notify();
        }
    }

    fn select_connection(&mut self, profile_id: uuid::Uuid, cx: &mut Context<Self>) {
        if !matches!(
            self.connection_state,
            ConnectionState::Disconnected | ConnectionState::Failed
        ) || matches!(
            self.editor.execution_status,
            ExecutionStatus::Running | ExecutionStatus::Cancelling
        ) {
            return;
        }
        if let Some(profile) = self
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
        {
            self.editor.connection = profile;
            self.schemas.clear();
            self.objects.clear();
            self.expanded_schema = None;
            self.status = format!("Target: {}", self.editor.connection_identity());
            cx.notify();
        }
    }

    /// Left-click selects and connects a profile; Alt-click disconnects the active profile
    /// (FR-004, FR-005). Switching targets while connected remains an explicit operation.
    fn handle_connection_click(
        &mut self,
        profile_id: uuid::Uuid,
        alt: bool,
        cx: &mut Context<Self>,
    ) {
        let active = self.editor.connection.id == profile_id;
        match self.connection_state {
            ConnectionState::Disconnected | ConnectionState::Failed if !alt => {
                if matches!(
                    self.editor.execution_status,
                    ExecutionStatus::Running | ExecutionStatus::Cancelling
                ) {
                    self.status = "Wait for the active query before changing connection".into();
                    cx.notify();
                    return;
                }
                self.select_connection(profile_id, cx);
                self.connect(cx);
            }
            ConnectionState::Connected if alt && active => self.disconnect(cx),
            ConnectionState::Connected if !active => {
                self.status = "Disconnect the active connection before selecting another".into();
                cx.notify();
            }
            ConnectionState::Connected => {
                self.status = format!("Connected: {}", self.editor.connection_identity());
                cx.notify();
            }
            ConnectionState::Disconnected | ConnectionState::Failed => {
                self.status = "Connection is already disconnected".into();
                cx.notify();
            }
            ConnectionState::Connecting | ConnectionState::Disconnecting => {
                self.status = "Connection change already in progress".into();
                cx.notify();
            }
        }
    }

    fn change_limit(&mut self, increase: bool, cx: &mut Context<Self>) {
        let new = if increase {
            self.editor.row_limit.saturating_mul(10)
        } else {
            self.editor.row_limit / 10
        };
        if self.editor.set_row_limit(new).is_ok() {
            cx.notify();
        }
    }

    fn connection_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tree = div().flex().flex_col().gap(px(3.));
        for (profile_index, profile) in self.profiles.iter().enumerate() {
            let profile_id = profile.id;
            // "Connected" is a property of the one session, not of whichever editor is in front.
            let live = self
                .session_profile
                .as_ref()
                .is_some_and(|session| session.id == profile_id)
                && self.connection_state == ConnectionState::Connected;
            let active =
                live || (self.session_profile.is_none() && profile_id == self.editor.connection.id);
            let indicator_colour = connection_indicator_colour(active, self.connection_state);
            tree = tree.child(
                div()
                    .id(SharedString::from(format!("connection-{profile_id}")))
                    .debug_selector(move || format!("connection-row-{profile_index}"))
                    .flex()
                    .items_center()
                    .gap(px(13.))
                    .px_3()
                    .py(px(11.))
                    .rounded(px(CONTROL_RADIUS))
                    .when(live, |row| row.bg(rgb(ACCENT_SOFT)))
                    .text_color(if live { rgb(ACCENT) } else { rgb(MUTED) })
                    .font_family(self.fonts.body.clone())
                    .hover(|style| style.bg(rgb(PANEL)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        this.handle_connection_click(profile_id, event.modifiers().alt, cx)
                    }))
                    .child(div().size(px(9.)).rounded_full().bg(rgb(indicator_colour)))
                    .child(profile.name.clone()),
            );
            tree = tree.child(
                div()
                    .pl(px(46.))
                    .pr_3()
                    .pb(px(10.))
                    .text_size(px(11.))
                    .font_family(self.fonts.mono.clone())
                    .text_color(rgb(FAINT))
                    .child(format!(
                        "{} · {}:{}",
                        profile.configuration.database,
                        profile.configuration.host,
                        profile.configuration.port
                    )),
            );
            if live {
                if self.schemas.is_empty() {
                    tree = tree.child(self.tree_caption("No user schemas"));
                }
                for schema in &self.schemas {
                    let selected = self.expanded_schema.as_deref() == Some(schema);
                    let schema_for_click = schema.clone();
                    tree = tree.child(
                        div()
                            .id(SharedString::from(format!("schema-{schema}")))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py(px(9.))
                            .rounded(px(11.))
                            .when(selected, |row| row.bg(rgb(PANEL)))
                            .font_family(self.fonts.display.clone())
                            .text_size(px(13.))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(PANEL)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.load_schema(schema_for_click.clone(), false, cx)
                            }))
                            .child(
                                div()
                                    .text_color(if selected { rgb(ACCENT) } else { rgb(FAINT) })
                                    .child(if selected { "▾" } else { "▸" }),
                            )
                            .child(schema.clone()),
                    );
                    if selected {
                        if let Some(objects) = self.objects.get(schema) {
                            for kind in [
                                ObjectKind::Table,
                                ObjectKind::View,
                                ObjectKind::MaterialisedView,
                                ObjectKind::Function,
                                ObjectKind::Procedure,
                                ObjectKind::Sequence,
                            ] {
                                let matching: Vec<_> = objects
                                    .iter()
                                    .filter(|object| object.kind == kind)
                                    .collect();
                                tree = tree.child(
                                    self.tree_caption(format!("{kind} · {}", matching.len())),
                                );
                                for object in matching {
                                    tree = tree.child(
                                        div()
                                            .pl(px(22.))
                                            .pr_3()
                                            .py(px(6.))
                                            .rounded_lg()
                                            .text_size(px(12.5))
                                            .font_family(self.fonts.mono.clone())
                                            .text_color(rgb(MUTED))
                                            .child(object.name.clone()),
                                    );
                                }
                            }
                        } else {
                            tree = tree.child(self.tree_caption("Loading…"));
                        }
                    }
                }
            }
        }
        tree
    }

    /// The uppercase mono captions that separate groups in the side nav.
    fn tree_caption(&self, label: impl Into<SharedString>) -> impl IntoElement {
        div()
            .pl(px(22.))
            .pr_3()
            .pt_3()
            .pb(px(6.))
            .text_size(px(10.))
            .font_family(self.fonts.mono.clone())
            .text_color(rgb(FAINT))
            .child(label.into().to_uppercase())
    }

    /// The identity card pinned to the foot of the side nav.
    fn server_card(&self, connected: bool) -> impl IntoElement {
        let version = self.server_version.clone().unwrap_or_default();
        let (headline, detail) = match version.split_once(" on ") {
            Some((product, platform)) => (product.to_owned(), platform.to_owned()),
            None if connected => (version, String::from("Connected")),
            None => (String::from("Not connected"), String::from("No server")),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .mt_2()
            .p(px(10.))
            .rounded(px(CONTROL_RADIUS))
            .bg(rgb(PANEL))
            .child(
                div()
                    .size(px(36.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(if connected { ACCENT } else { PANEL_LIGHT }))
                    .text_size(px(14.))
                    .font_family(self.fonts.mono.clone())
                    .text_color(rgb(if connected { ON_ACCENT } else { FAINT }))
                    .child(if connected { "PG" } else { "—" }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_family(self.fonts.display.clone())
                            .child(headline),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(detail),
                    ),
            )
    }

    fn editor_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = segmented();
        for (index, editor) in self.background_editors.iter().enumerate() {
            tabs = tabs.child(segment(
                &self.fonts,
                SharedString::from(format!("editor-tab-{index}")),
                editor.title.clone(),
                false,
                true,
                cx.listener(move |this, _, _, cx| this.switch_editor(index, cx)),
            ));
        }
        tabs = tabs.child(segment(
            &self.fonts,
            "editor-tab",
            self.editor.title.clone(),
            !self.active_result_tab,
            true,
            cx.listener(|this, _, _, cx| this.show_editor_tab(cx)),
        ));
        if !self.editor.results.is_empty() {
            tabs = tabs.child(segment(
                &self.fonts,
                "result-tab",
                format!("Result: {}", self.editor.title),
                self.active_result_tab,
                true,
                cx.listener(|this, _, _, cx| this.show_result_tab(cx)),
            ));
        }
        tabs.child(segment(
            &self.fonts,
            "new-editor",
            "+",
            false,
            true,
            cx.listener(|this, _, _, cx| this.dispatch_command(command::NEW_EDITOR, cx)),
        ))
    }

    fn display_segments(&self, cx: &mut Context<Self>) -> impl IntoElement {
        segmented()
            .child(segment(
                &self.fonts,
                "display-table",
                "TABLE",
                self.editor.display == ResultDisplay::Table,
                true,
                cx.listener(|this, _, _, cx| this.set_display(ResultDisplay::Table, cx)),
            ))
            .child(segment(
                &self.fonts,
                "display-text",
                "TEXT",
                self.editor.display == ResultDisplay::Text,
                true,
                cx.listener(|this, _, _, cx| this.set_display(ResultDisplay::Text, cx)),
            ))
    }

    fn destination_segments(&self, cx: &mut Context<Self>) -> impl IntoElement {
        segmented()
            .child(segment(
                &self.fonts,
                "destination-pane",
                "PANE",
                self.editor.destination == ResultDestination::Pane,
                false,
                cx.listener(|this, _, _, cx| this.set_destination(ResultDestination::Pane, cx)),
            ))
            .child(segment(
                &self.fonts,
                "destination-tab",
                "TAB",
                self.editor.destination == ResultDestination::Tab,
                false,
                cx.listener(|this, _, _, cx| this.set_destination(ResultDestination::Tab, cx)),
            ))
            .child(segment(
                &self.fonts,
                "destination-window",
                "WINDOW",
                self.editor.destination == ResultDestination::Window,
                false,
                cx.listener(|this, _, _, cx| this.set_destination(ResultDestination::Window, cx)),
            ))
    }

    /// The connection dialog. The URL is only ever echoed back as bullets, and the placeholder is a
    /// generic template rather than anything the user typed (§43, §44).
    fn connection_dialog_card(&self) -> impl IntoElement {
        div()
            .absolute()
            .top(px(90.))
            .left(px(360.))
            .w(px(620.))
            .p(px(26.))
            .rounded(px(CARD_RADIUS))
            .bg(rgb(PANEL))
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(36.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded(px(11.))
                            .bg(rgb(ACCENT_SOFT))
                            .text_color(rgb(ACCENT))
                            .child("⚿"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_family(self.fonts.display.clone())
                                    .child("Configure PostgreSQL connection"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(MUTED))
                                    .child("Credentials are masked and never persisted"),
                            ),
                    ),
            )
            .child(
                div()
                    .mt_5()
                    .px_4()
                    .py_3()
                    .rounded(px(13.))
                    .bg(rgb(BACKGROUND))
                    .border_1()
                    .border_color(rgb(ACCENT))
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(self.fonts.mono.clone())
                            .text_color(rgb(MUTED))
                            .child("CONNECTION URL"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(18.))
                            .font_family(self.fonts.mono.clone())
                            .child(if self.connection_buffer.is_empty() {
                                "••••••••••••••••".to_owned()
                            } else {
                                "•".repeat(self.connection_buffer.chars().count())
                            }),
                    ),
            )
            .child(
                div()
                    .mt_3()
                    .text_size(px(12.))
                    .text_color(rgb(FAINT))
                    .child("postgresql://user:password@host:5432/database?sslmode=prefer"),
            )
            .child(
                div()
                    .mt_5()
                    .text_size(px(11.))
                    .font_family(self.fonts.mono.clone())
                    .text_color(rgb(MUTED))
                    .child("ENTER TO APPLY · ESCAPE TO CANCEL · PASTE SUPPORTED"),
            )
    }

    fn editor_surface(&self) -> impl IntoElement {
        let mut lines = div()
            .flex()
            .flex_col()
            .min_h_full()
            .font_family(self.fonts.mono.clone())
            .text_size(px(EDITOR_TEXT_SIZE))
            .line_height(px(EDITOR_LINE_HEIGHT));
        let document = if self.editor.document.is_empty() {
            "-- Write PostgreSQL here\nSELECT current_database();"
        } else {
            &self.editor.document
        };
        for (index, line) in document.lines().enumerate() {
            lines = lines.child(
                div()
                    .flex()
                    .flex_row()
                    .min_h(px(EDITOR_LINE_HEIGHT))
                    .child(
                        div()
                            .w(px(GUTTER_WIDTH))
                            .pr(px(18.))
                            .text_right()
                            .text_color(rgb(FAINT))
                            .child((index + 1).to_string()),
                    )
                    .child(highlight_line(line)),
            );
        }
        lines.child(
            div()
                .pl(px(GUTTER_WIDTH))
                .text_color(rgb(ACCENT))
                .child("▏"),
        )
    }

    /// Makes one grid row selectable. Rows select whole, which is how SQL grids normally behave —
    /// character selection belongs to the text view, where the output is one aligned block.
    fn selectable_row(
        &self,
        index: usize,
        id: &'static str,
        row: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = self
            .result_selection
            .is_some_and(|selection| selection.contains(index));
        row.id(SharedString::from(format!("{id}-{index}")))
            .when(selected, |row| row.bg(rgb(ACCENT_SOFT)))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let extend = event.modifiers.shift;
                    let anchor = match (extend, this.result_selection) {
                        (true, Some(existing)) => existing.ordered().0.line,
                        _ => index,
                    };
                    this.selecting_results = true;
                    this.result_selection = Some(ResultSelection::whole_lines(
                        anchor.min(index),
                        anchor.max(index),
                    ));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.selecting_results
                    && let Some(existing) = this.result_selection
                {
                    let anchor = existing.ordered().0.line;
                    let extended =
                        ResultSelection::whole_lines(anchor.min(index), anchor.max(index));
                    if existing.bounds() != extended.bounds() {
                        this.result_selection = Some(extended);
                        cx.notify();
                    }
                }
            }))
    }

    /// One character-selectable line of a text result. The highlight is a rectangle painted behind
    /// the glyphs, sized from the monospace advance, so the text itself is never split up.
    fn result_line(&self, index: usize, line: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let length = line.chars().count();
        let span = self
            .result_selection
            .and_then(|selection| selection.span_for(index, length));
        let advance = self.mono_advance;
        div()
            .id(SharedString::from(format!("result-line-{index}")))
            .relative()
            .min_w_full()
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let column = this.column_at(event.position.x);
                    this.begin_selection(
                        ResultPosition {
                            line: index,
                            column,
                        },
                        event.modifiers.shift,
                    );
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                let column = this.column_at(event.position.x);
                if this.extend_selection(ResultPosition {
                    line: index,
                    column,
                }) {
                    cx.notify();
                }
            }))
            // Painted before the text, so it sits behind the glyphs.
            .children(span.map(|span| {
                div()
                    .absolute()
                    .top(px(0.))
                    .bottom(px(0.))
                    .left(px(RESULT_TEXT_INSET) + advance * span.start as f32)
                    .w(advance * span.len() as f32)
                    .bg(rgb(ACCENT_SOFT))
            }))
            .child(div().px_5().child(SharedString::from(line.to_owned())))
    }

    /// The grid, with every header and data row selectable.
    fn selectable_table(
        &self,
        result: &QueryResult,
        line_index: &mut usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut table = table_shell(&self.fonts);
        if !result.columns.is_empty() {
            table = table.child(self.selectable_row(
                *line_index,
                "result-header",
                header_row(result),
                cx,
            ));
            *line_index += 1;
        }
        for row in &result.rows {
            table = table.child(self.selectable_row(*line_index, "result-row", data_row(row), cx));
            *line_index += 1;
        }
        table
    }

    /// The result content itself. Scrolling and the bars belong to the viewport that wraps this,
    /// so the same content works in the pane, the result tab and the separate window.
    fn results_surface(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut content = div().flex().flex_col().items_start().min_w_full();
        let mut line_index = 0;
        if self.editor.results.is_empty() && self.editor.error.is_none() {
            return content
                .h_full()
                .items_center()
                .justify_center()
                .gap_3()
                .font_family(self.fonts.body.clone())
                .text_size(px(13.))
                .text_color(rgb(FAINT))
                .child("Run a query to see results")
                .child(
                    div()
                        .text_size(px(11.))
                        .font_family(self.fonts.mono.clone())
                        .text_color(rgb(PANEL_LIGHT))
                        .child("⌘↵ RUN · ⇧⌘↵ RUN ALL · ⌥⌘↵ EXPLAIN"),
                )
                .into_any_element();
        }
        for (index, result) in self.editor.results.iter().enumerate() {
            content = content.child(result_header(&self.fonts, index, result));
            content = match self.editor.display {
                ResultDisplay::Text => {
                    let mut block = div()
                        .flex()
                        .flex_col()
                        .py_3()
                        .min_w_full()
                        .font_family(self.fonts.mono.clone())
                        .text_size(px(RESULT_TEXT_SIZE))
                        .line_height(px(RESULT_LINE_HEIGHT));
                    for line in result.as_text().lines() {
                        block = block.child(self.result_line(line_index, line, cx));
                        line_index += 1;
                    }
                    content.child(block)
                }
                ResultDisplay::Table => {
                    content.child(self.selectable_table(result, &mut line_index, cx))
                }
            };
        }
        // Earlier results stay on screen beneath the failure (§46, §47).
        if let Some(error) = &self.editor.error {
            content = content.child(self.error_card(error));
        }
        content.into_any_element()
    }

    /// The failure card: severity strip, SQLSTATE, message, and a reassurance that whatever ran
    /// before the failing statement is still rendered above it.
    fn error_card(&self, error: &crate::result::QueryError) -> impl IntoElement {
        let heading = match &error.code {
            Some(code) => format!("Statement failed · {code}"),
            None => "Statement failed".to_owned(),
        };
        let mut detail = String::from("Earlier results are preserved.");
        if let Some(position) = error.position {
            detail.push_str(&format!(" Position {position}."));
        }
        div()
            .mx_5()
            .my_3()
            .px_4()
            .py_3()
            .rounded(px(13.))
            .bg(rgb(PANEL_LIGHT))
            .border_l(px(3.))
            .border_color(rgb(RED))
            .flex()
            .flex_col()
            .gap(px(7.))
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(self.fonts.mono.clone())
                    .text_color(rgb(RED))
                    .child(heading.to_uppercase()),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .font_family(self.fonts.mono.clone())
                    .text_color(rgb(TEXT))
                    .child(error.message.clone()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .font_family(self.fonts.body.clone())
                    .text_color(rgb(MUTED))
                    .child(detail),
            )
    }
}

#[derive(Clone, Copy)]
enum RunMode {
    Current,
    All,
    Explain,
}

/// A failed execution. Run All also records which statement stopped the batch, so the user is told
/// what actually happened rather than being shown a success line.
struct RunFailure {
    statement_index: Option<usize>,
    error: crate::result::QueryError,
}

impl RunFailure {
    fn status_message(&self) -> String {
        match self.statement_index {
            Some(index) => format!("Statement {} failed: {}", index + 1, self.error),
            None => format!("Query failed: {}", self.error),
        }
    }
}

struct ResultWindow {
    results: Vec<QueryResult>,
    display: ResultDisplay,
    fonts: Fonts,
    scroll: ScrollState,
}

impl Render for ResultWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div().flex().flex_col().items_start().min_w_full().pt_4();
        for (index, result) in self.results.iter().enumerate() {
            content = content.child(result_header(&self.fonts, index, result));
            content = match self.display {
                ResultDisplay::Table => content.child(result_table(&self.fonts, result)),
                ResultDisplay::Text => content.child(
                    div()
                        .px_5()
                        .py_3()
                        .font_family(self.fonts.mono.clone())
                        .text_size(px(13.))
                        .line_height(px(22.))
                        .child(result.as_text()),
                ),
            };
        }
        div()
            .size_full()
            .flex()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .font_family(self.fonts.body.clone())
            .on_mouse_move(cx.listener(|this, event, _, cx| {
                if this.scroll.drag(event) {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.scroll.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(scrollable(
                |view: &mut Self| &mut view.scroll,
                &self.scroll,
                "separate-results-scroll",
                content,
                cx,
            ))
    }
}

impl Focusable for AppView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.connection_state == ConnectionState::Connected;
        let running = self.is_running();
        let pane_visible =
            self.editor.destination == ResultDestination::Pane && !self.active_result_tab;
        div()
            .track_focus(&self.focus_handle)
            .key_context("SqlEditor")
            .on_key_down(cx.listener(|this, event, _, cx| this.handle_key(event, cx)))
            // A thumb drag continues wherever the pointer goes, so it is tracked at the root.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                let viewport_height = window.viewport_size().height;
                if this.editor_scroll.drag(event)
                    | this.results_scroll.drag(event)
                    | this.drag_pane(event.position.y, viewport_height)
                {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    let was_dragging = this.editor_scroll.drag.take().is_some()
                        | this.results_scroll.drag.take().is_some()
                        | this.pane_drag.take().is_some()
                        | this.selecting_results;
                    this.selecting_results = false;
                    if was_dragging {
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .font_family(self.fonts.body.clone())
            .child(
                div()
                    .h(px(TITLEBAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_4()
                    .px(px(18.))
                    .bg(rgb(CHROME))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .flex()
                            .text_size(px(15.))
                            .font_family(self.fonts.display.clone())
                            .child("Rusty")
                            .child(div().text_color(rgb(ACCENT)).child("SQL")),
                    )
                    .child(
                        div()
                            .mx_auto()
                            .px_4()
                            .py(px(7.))
                            .rounded_lg()
                            .bg(rgb(PANEL))
                            .text_size(px(12.))
                            .font_family(self.fonts.mono.clone())
                            .text_color(rgb(FAINT))
                            .child(format!("rusty-sql — {}", self.editor.title)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(11.))
                            .font_family(self.fonts.mono.clone())
                            .text_color(rgb(MUTED))
                            .child(div().size(px(9.)).rounded_full().bg(rgb(
                                connection_indicator_colour(true, self.connection_state),
                            )))
                            .child(format!("{:?}", self.connection_state).to_uppercase()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(SIDEBAR_WIDTH))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .p(px(14.))
                            .pt(px(22.))
                            .bg(rgb(SURFACE))
                            .border_r_1()
                            .border_color(rgb(BORDER))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .pl(px(10.))
                                    .pb_2()
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .font_family(self.fonts.mono.clone())
                                            .text_color(rgb(FAINT))
                                            .child("CONNECTIONS"),
                                    )
                                    .child(div().flex_1())
                                    .child(icon_button(
                                        "configure-connection",
                                        "＋",
                                        self.connection_state == ConnectionState::Disconnected,
                                        false,
                                        cx.listener(|this, _, _, cx| this.configure_connection(cx)),
                                    ))
                                    .child(icon_button(
                                        "refresh-metadata",
                                        "↻",
                                        connected,
                                        false,
                                        cx.listener(|this, _, _, cx| this.refresh_metadata(cx)),
                                    )),
                            )
                            .child(
                                div()
                                    .px(px(10.))
                                    .pb_2()
                                    .text_size(px(11.))
                                    .text_color(rgb(FAINT))
                                    .child("Click to connect · Alt-click to disconnect"),
                            )
                            // Without configuration the list below is a built-in guess, not
                            // anything the user set up. Say so rather than letting it fail silently.
                            .when(!self.configured, |pane| {
                                pane.child(
                                    div()
                                        .mx_1()
                                        .mb_2()
                                        .px_3()
                                        .py_3()
                                        .rounded(px(13.))
                                        .bg(rgb(PANEL))
                                        .text_size(px(12.))
                                        .text_color(rgb(MUTED))
                                        .child(
                                            "No database connections found. \
                                             Load a .env file or connect manually with ＋.",
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id("connection-tree-scroll")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(self.connection_tree(cx)),
                            )
                            .child(self.server_card(connected)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .px(px(30.))
                                    .py(px(20.))
                                    .border_b_1()
                                    .border_color(rgb(BORDER))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(6.))
                                            .child(
                                                div()
                                                    .text_size(px(26.))
                                                    .font_family(self.fonts.display.clone())
                                                    .child(self.editor.title.clone()),
                                            )
                                            // The target connection stays on screen so destructive
                                            // SQL can be checked before it runs (§49, §50).
                                            .child(
                                                div()
                                                    .text_size(px(13.))
                                                    .text_color(rgb(MUTED))
                                                    .child(self.target_identity()),
                                            ),
                                    )
                                    .child(div().flex_1())
                                    .child(self.editor_tabs(cx))
                                    .child(if connected {
                                        button(
                                            &self.fonts,
                                            "disconnect",
                                            "Disconnect",
                                            Tone::Neutral,
                                            !running,
                                            cx.listener(|this, _, _, cx| {
                                                this.dispatch_command(command::DISCONNECT, cx)
                                            }),
                                        )
                                        .into_any_element()
                                    } else {
                                        button(
                                            &self.fonts,
                                            "connect",
                                            "Connect",
                                            Tone::Primary,
                                            !running,
                                            cx.listener(|this, _, _, cx| {
                                                this.dispatch_command(command::CONNECT, cx)
                                            }),
                                        )
                                        .into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .px(px(30.))
                                    .py_4()
                                    .border_b_1()
                                    .border_color(rgb(BORDER))
                                    .child(button(
                                        &self.fonts,
                                        "run",
                                        "▶ Run",
                                        Tone::Neutral,
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| {
                                            this.dispatch_command(command::RUN, cx)
                                        }),
                                    ))
                                    .child(button(
                                        &self.fonts,
                                        "run-all",
                                        "▶▶ Run All",
                                        Tone::Neutral,
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| {
                                            this.dispatch_command(command::RUN_ALL, cx)
                                        }),
                                    ))
                                    .child(button(
                                        &self.fonts,
                                        "explain",
                                        "Explain",
                                        Tone::Neutral,
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| {
                                            this.dispatch_command(command::EXPLAIN, cx)
                                        }),
                                    ))
                                    .child(button(
                                        &self.fonts,
                                        "stop",
                                        "■ Stop",
                                        Tone::Danger,
                                        running,
                                        cx.listener(|this, _, _, cx| {
                                            this.dispatch_command(command::CANCEL, cx)
                                        }),
                                    ))
                                    .when(running, |bar| {
                                        bar.child(metric_pill(
                                            &self.fonts,
                                            execution_label(self.editor.execution_status),
                                            ACCENT,
                                        ))
                                    })
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .font_family(self.fonts.mono.clone())
                                            .text_color(rgb(MUTED))
                                            .child("ROW LIMIT"),
                                    )
                                    .child(
                                        segmented()
                                            .child(icon_button(
                                                "limit-down",
                                                "−",
                                                self.editor.row_limit > 1,
                                                true,
                                                cx.listener(|this, _, _, cx| {
                                                    this.change_limit(false, cx)
                                                }),
                                            ))
                                            .child(
                                                div()
                                                    .min_w(px(44.))
                                                    .text_center()
                                                    .text_size(px(15.))
                                                    .font_family(self.fonts.mono.clone())
                                                    .child(self.editor.row_limit.to_string()),
                                            )
                                            .child(icon_button(
                                                "limit-up",
                                                "+",
                                                self.editor.row_limit < crate::MAX_ROW_LIMIT,
                                                true,
                                                cx.listener(|this, _, _, cx| {
                                                    this.change_limit(true, cx)
                                                }),
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .px(px(30.))
                                    .py(px(20.))
                                    .cursor_text()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            window.focus(&this.focus_handle(cx));
                                        }),
                                    )
                                    // The result tab shares this surface with the editor, so it
                                    // scrolls against whichever handle is currently showing.
                                    .child(if self.active_result_tab {
                                        scrollable(
                                            |view: &mut Self| &mut view.results_scroll,
                                            &self.results_scroll,
                                            "editor-scroll",
                                            self.results_surface(cx),
                                            cx,
                                        )
                                        .into_any_element()
                                    } else {
                                        scrollable(
                                            |view: &mut Self| &mut view.editor_scroll,
                                            &self.editor_scroll,
                                            "editor-scroll",
                                            self.editor_surface(),
                                            cx,
                                        )
                                        .into_any_element()
                                    }),
                            )
                            .when(pane_visible, |column| {
                                column.child(self.results_splitter(cx))
                            })
                            .child(
                                div()
                                    .h(if pane_visible {
                                        self.result_pane_height
                                    } else {
                                        px(0.)
                                    })
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .mx(px(30.))
                                    .mb(px(24.))
                                    .rounded(px(CARD_RADIUS))
                                    .overflow_hidden()
                                    .bg(rgb(PANEL))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .px_5()
                                            .py(px(14.))
                                            .border_b_1()
                                            .border_color(rgb(BORDER))
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .font_family(self.fonts.mono.clone())
                                                    .text_color(rgb(MUTED))
                                                    .child("RESULTS"),
                                            )
                                            .child(div().flex_1())
                                            .when(!self.editor.results.is_empty(), |bar| {
                                                bar.child(segment(
                                                    &self.fonts,
                                                    "copy-results",
                                                    match self.result_selection {
                                                        Some(_) => "COPY SELECTION",
                                                        None => "COPY ALL",
                                                    },
                                                    false,
                                                    true,
                                                    cx.listener(|this, _, _, cx| {
                                                        this.copy_results(cx)
                                                    }),
                                                ))
                                            })
                                            .child(self.display_segments(cx))
                                            .child(self.destination_segments(cx)),
                                    )
                                    // Only the visible surface tracks the results scroll handle;
                                    // the collapsed pane must not claim it from the result tab.
                                    .when(pane_visible, |pane| {
                                        pane.child(scrollable(
                                            |view: &mut Self| &mut view.results_scroll,
                                            &self.results_scroll,
                                            "results-scroll",
                                            self.results_surface(cx),
                                            cx,
                                        ))
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(STATUS_BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(18.))
                    .bg(rgb(CHROME))
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_size(px(11.))
                    .font_family(self.fonts.mono.clone())
                    .text_color(rgb(MUTED))
                    .child(div().size(px(9.)).rounded_full().flex_none().bg(rgb(
                        connection_indicator_colour(true, self.connection_state),
                    )))
                    .child(self.status.to_uppercase())
                    .child(div().flex_1())
                    .child(
                        div().text_color(rgb(FAINT)).child(
                            self.server_version
                                .clone()
                                .unwrap_or_default()
                                .to_uppercase(),
                        ),
                    ),
            )
            .when(self.connection_dialog, |root| {
                root.child(self.connection_dialog_card())
            })
    }
}

/// Toolbar buttons come in three tones: the mint primary action, the red destructive Stop, and the
/// neutral panel fill everything else uses.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Neutral,
    Primary,
    Danger,
}

fn button(
    fonts: &Fonts,
    id: &'static str,
    label: &'static str,
    tone: Tone,
    enabled: bool,
    handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (background, foreground) = match (enabled, tone) {
        (false, _) => (PANEL, FAINT),
        (true, Tone::Primary) => (ACCENT, ON_ACCENT),
        (true, Tone::Danger) => (RED, BACKGROUND),
        (true, Tone::Neutral) => (PANEL, TEXT),
    };
    let hovered = match tone {
        Tone::Neutral => PANEL_LIGHT,
        Tone::Primary | Tone::Danger => background,
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(18.))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgb(background))
        .text_color(rgb(foreground))
        .text_size(px(13.))
        .font_family(fonts.display.clone())
        .when(enabled, |element| {
            element
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(hovered)))
                .on_click(handler)
        })
        .child(label)
}

/// A square glyph button: the side-nav actions and the row-limit stepper.
fn icon_button(
    id: &'static str,
    glyph: &'static str,
    enabled: bool,
    accented: bool,
    handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(34.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_lg()
        .text_color(match (enabled, accented) {
            (false, _) => rgb(PANEL_HIGH),
            (true, true) => rgb(ACCENT),
            (true, false) => rgb(MUTED),
        })
        .when(enabled, |element| {
            element
                .cursor_pointer()
                .hover(|style| style.bg(rgb(PANEL_HIGH)))
                .on_click(handler)
        })
        .child(glyph)
}

fn execution_label(status: ExecutionStatus) -> String {
    match status {
        ExecutionStatus::Cancelling => "cancelling".to_owned(),
        _ => "running".to_owned(),
    }
}

/// The container for a segmented control — a recessed well the segments sit inside.
fn segmented() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .p_1()
        .rounded(px(11.))
        .bg(rgb(PANEL_LIGHT))
}

fn segment(
    fonts: &Fonts,
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    accented: bool,
    handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (background, foreground) = match (selected, accented) {
        (true, true) => (ACCENT, ON_ACCENT),
        (true, false) => (PANEL_HIGH, TEXT),
        (false, _) => (PANEL_LIGHT, MUTED),
    };
    div()
        .id(id)
        .px(px(14.))
        .py_2()
        .rounded_lg()
        .bg(rgb(background))
        .text_size(px(12.))
        .font_family(fonts.mono.clone())
        .text_color(rgb(foreground))
        .cursor_pointer()
        .when(!selected, |element| {
            element.hover(|style| style.bg(rgb(PANEL_HIGH)))
        })
        .on_click(handler)
        .child(label.into())
}

fn highlight_line(line: &str) -> impl IntoElement {
    let keywords = [
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "WITH",
        "CREATE",
        "ALTER",
        "DROP",
        "JOIN",
        "WHERE",
        "GROUP",
        "BY",
        "ORDER",
        "HAVING",
        "LIMIT",
        "RETURNING",
        "EXPLAIN",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "FROM",
        "AS",
        "VALUES",
        "FETCH",
        "FIRST",
        "ROWS",
        "ONLY",
    ];
    let comment_start = line.find("--");
    let mut row = div().flex().flex_row().whitespace_nowrap();
    for (index, piece) in line
        .split_inclusive(|character: char| character.is_whitespace())
        .enumerate()
    {
        let offset: usize = line
            .split_inclusive(|character: char| character.is_whitespace())
            .take(index)
            .map(str::len)
            .sum();
        let word = piece.trim_matches(|character: char| !character.is_ascii_alphabetic());
        let colour = if comment_start.is_some_and(|start| offset >= start) {
            FAINT
        } else if keywords.contains(&word.to_ascii_uppercase().as_str()) {
            ACCENT
        } else if piece.contains('\'') {
            STRING
        } else if piece.contains('(') && !word.is_empty() {
            FUNCTION
        } else {
            TEXT
        };
        row = row.child(div().text_color(rgb(colour)).child(piece.to_owned()));
    }
    row
}

/// A metric pill: the compact accent-on-dark badges the design uses for row counts and timings.
fn metric_pill(fonts: &Fonts, label: String, colour: u32) -> impl IntoElement {
    div()
        .px(px(10.))
        .py(px(6.))
        .rounded_full()
        .bg(rgb(ACCENT_SOFT))
        .text_size(px(11.))
        .font_family(fonts.mono.clone())
        .text_color(rgb(colour))
        .child(label.to_uppercase())
}

fn result_header(fonts: &Fonts, index: usize, result: &QueryResult) -> impl IntoElement {
    let count = if result.columns.is_empty() {
        format!("{} affected", result.affected_rows.unwrap_or(0))
    } else {
        format!("{} rows", result.rows.len())
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_5()
        .py_3()
        .child(
            div()
                .text_size(px(11.))
                .font_family(fonts.mono.clone())
                .text_color(rgb(MUTED))
                .child(format!("RESULT {}", index + 1)),
        )
        .child(metric_pill(
            fonts,
            format!("{count} · {} ms", result.execution_time.as_millis()),
            ACCENT,
        ))
        .children(
            result
                .automatic_limit
                .map(|limit| metric_pill(fonts, format!("auto limit {limit}"), STRING)),
        )
}

/// The grid shell. `flex_none` here and on the cells keeps the grid at its intrinsic width so it
/// overflows sideways rather than compressing its columns; `overflow_hidden` clips selected-row
/// backgrounds to the rounded corners.
fn table_shell(fonts: &Fonts) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .flex_col()
        .mx_5()
        .mb_4()
        .rounded(px(CARD_RADIUS))
        .overflow_hidden()
        .bg(rgb(PANEL))
        .font_family(fonts.mono.clone())
}

fn header_row(result: &QueryResult) -> gpui::Div {
    let mut header = div()
        .flex()
        .flex_none()
        .border_b_1()
        .border_color(rgb(BORDER));
    for column in &result.columns {
        header = header.child(
            div()
                .w(px(180.))
                .flex_none()
                .px_4()
                .py(px(11.))
                .text_size(px(10.))
                .text_color(rgb(FAINT))
                .child(column.name.to_uppercase()),
        );
    }
    header
}

fn data_row(values: &[CellValue]) -> gpui::Div {
    let mut row = div()
        .flex()
        .flex_none()
        .border_b_1()
        .border_color(rgb(BORDER));
    for value in values {
        row = row.child(
            div()
                .w(px(180.))
                .flex_none()
                .px_4()
                .py_3()
                .text_size(px(13.))
                .text_color(if matches!(value, CellValue::Null) {
                    rgb(FAINT)
                } else {
                    rgb(TEXT)
                })
                .child(value.to_display_string()),
        );
    }
    row
}

/// The read-only grid used by the separate results window.
fn result_table(fonts: &Fonts, result: &QueryResult) -> impl IntoElement {
    let mut table = table_shell(fonts);
    if !result.columns.is_empty() {
        table = table.child(header_row(result));
    }
    for row in &result.rows {
        table = table.child(data_row(row));
    }
    table
}

fn completion_status(results: &[QueryResult]) -> String {
    let elapsed: u128 = results
        .iter()
        .map(|result| result.execution_time.as_millis())
        .sum();
    let rows: usize = results.iter().map(|result| result.rows.len()).sum();
    format!("Completed in {elapsed} ms · {rows} rows")
}

fn shortcut_command(
    key: &str,
    command_modifier: bool,
    shift: bool,
    alt: bool,
    running: bool,
    connected: bool,
) -> Option<&'static str> {
    // Shortcuts honour exactly the same preconditions as the toolbar buttons, so a key binding can
    // never reach a command the equivalent button refuses to dispatch.
    let runnable = connected && !running;
    if command_modifier {
        return match key {
            "enter" if shift => runnable.then_some(command::RUN_ALL),
            "enter" if alt => runnable.then_some(command::EXPLAIN),
            "enter" => runnable.then_some(command::RUN),
            "." if running => Some(command::CANCEL),
            "n" => Some(command::NEW_EDITOR),
            "d" if shift && running => None,
            "d" if shift && connected => Some(command::DISCONNECT),
            "d" if shift => Some(command::CONNECT),
            _ => None,
        };
    }
    (key == "escape" && running).then_some(command::CANCEL)
}

/// Mint means connected. Because the palette reuses that mint for keywords and primary actions, the
/// transitional states take the amber WARN rather than sharing the connected colour.
fn connection_indicator_colour(active: bool, state: ConnectionState) -> u32 {
    if !active {
        return FAINT;
    }
    match state {
        ConnectionState::Connected => ACCENT,
        ConnectionState::Connecting | ConnectionState::Disconnecting => WARN,
        ConnectionState::Failed => RED,
        ConnectionState::Disconnected => FAINT,
    }
}

/// The discovered connections, and whether they came from real configuration. The built-in
/// fallback must not be presented as though the user had configured it.
fn discover_profiles() -> (Vec<ConnectionProfile>, bool) {
    if Path::new(".env").is_file()
        && let Ok(profiles) = ConnectionProfile::profiles_from_env_file(".env")
        && !profiles.is_empty()
    {
        return (profiles, true);
    }
    if let Ok(profiles) = ConnectionProfile::profiles_from_process_env()
        && !profiles.is_empty()
    {
        return (profiles, true);
    }
    let fallback = local_profile().unwrap_or_else(|| {
        ConnectionProfile::from_database_url(
            "postgresql://postgres@localhost:5432/postgres?sslmode=disable",
        )
        .expect("built-in PostgreSQL profile should be valid")
    });
    (vec![fallback], false)
}

fn previous_boundary(text: &str, offset: usize) -> usize {
    text[..offset]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, offset: usize) -> usize {
    text[offset..]
        .char_indices()
        .nth(1)
        .map_or(text.len(), |(index, _)| offset + index)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    use super::*;
    use async_trait::async_trait;
    use gpui::{Modifiers, TestAppContext};

    use crate::database::{ConnectionInfo, DatabaseProvider};
    use crate::result::QueryError;

    #[derive(Default)]
    struct UiTestProvider {
        state: AtomicU8,
        connect_calls: AtomicUsize,
        disconnect_calls: AtomicUsize,
        /// Holds `execute` open so a test can act while a query is genuinely in flight.
        blocked: AtomicBool,
    }

    impl UiTestProvider {
        fn set_state(&self, state: ConnectionState) {
            self.state.store(state as u8, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl DatabaseProvider for UiTestProvider {
        async fn connect(&self, profile: &ConnectionProfile) -> Result<ConnectionInfo, QueryError> {
            self.connect_calls.fetch_add(1, Ordering::SeqCst);
            self.set_state(ConnectionState::Connected);
            Ok(ConnectionInfo {
                database: profile.configuration.database.clone(),
                server_version: "PostgreSQL test".into(),
            })
        }

        async fn disconnect(&self) -> Result<(), QueryError> {
            self.disconnect_calls.fetch_add(1, Ordering::SeqCst);
            self.set_state(ConnectionState::Disconnected);
            Ok(())
        }

        async fn execute(&self, _: &str) -> Result<QueryResult, QueryError> {
            while self.blocked.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            Ok(QueryResult::default())
        }

        async fn cancel(&self) -> Result<(), QueryError> {
            Ok(())
        }

        async fn schemas(&self, _: bool) -> Result<Vec<String>, QueryError> {
            Ok(Vec::new())
        }

        async fn objects(&self, _: &str, _: bool) -> Result<Vec<DatabaseObject>, QueryError> {
            Ok(Vec::new())
        }

        fn state(&self) -> ConnectionState {
            match self.state.load(Ordering::SeqCst) {
                value if value == ConnectionState::Connecting as u8 => ConnectionState::Connecting,
                value if value == ConnectionState::Connected as u8 => ConnectionState::Connected,
                value if value == ConnectionState::Disconnecting as u8 => {
                    ConnectionState::Disconnecting
                }
                value if value == ConnectionState::Failed as u8 => ConnectionState::Failed,
                _ => ConnectionState::Disconnected,
            }
        }
    }

    fn build_app_view(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<AppView>, &mut gpui::VisualTestContext) {
        cx.add_window_view(|window, cx| {
            let view = AppView::new(cx);
            window.focus(&view.focus_handle(cx));
            window.activate_window();
            window.on_window_should_close(cx, |_, _| true);
            view
        })
    }

    fn wait_for_connection_state(
        view: &gpui::Entity<AppView>,
        cx: &mut gpui::VisualTestContext,
        expected: ConnectionState,
    ) {
        for _ in 0..1_000 {
            cx.run_until_parked();
            if view.update(cx, |app, _| app.connection_state) == expected {
                return;
            }
            std::thread::yield_now();
        }
        view.update(cx, |app, _| assert_eq!(app.connection_state, expected));
    }

    #[test]
    fn core_shortcuts_resolve_to_stable_command_ids() {
        assert_eq!(
            shortcut_command("enter", true, false, false, false, true),
            Some(command::RUN)
        );
        assert_eq!(
            shortcut_command("enter", true, true, false, false, true),
            Some(command::RUN_ALL)
        );
        assert_eq!(
            shortcut_command("enter", true, false, true, false, true),
            Some(command::EXPLAIN)
        );
        assert_eq!(
            shortcut_command("n", true, false, false, false, false),
            Some(command::NEW_EDITOR)
        );
        assert_eq!(
            shortcut_command("d", true, true, false, false, false),
            Some(command::CONNECT)
        );
        assert_eq!(
            shortcut_command("d", true, true, false, false, true),
            Some(command::DISCONNECT)
        );
    }

    /// A key binding must not reach a command the equivalent toolbar button refuses to dispatch:
    /// two concurrent runs share one session, and disconnecting mid-query discards its results.
    #[test]
    fn execution_shortcuts_are_refused_while_a_query_is_running() {
        for key in ["enter"] {
            for (shift, alt) in [(false, false), (true, false), (false, true)] {
                assert_eq!(
                    shortcut_command(key, true, shift, alt, true, true),
                    None,
                    "{key} shift={shift} alt={alt} should be refused while running"
                );
            }
        }
        assert_eq!(shortcut_command("d", true, true, false, true, true), None);
        assert_eq!(shortcut_command("d", true, true, false, true, false), None);
        // Cancelling is the one thing that must still work mid-query.
        assert_eq!(
            shortcut_command(".", true, false, false, true, true),
            Some(command::CANCEL)
        );
    }

    /// Run and friends need a connection, not just an idle editor.
    #[test]
    fn execution_shortcuts_are_refused_while_disconnected() {
        assert_eq!(
            shortcut_command("enter", true, false, false, false, false),
            None
        );
        assert_eq!(
            shortcut_command("enter", true, true, false, false, false),
            None
        );
        assert_eq!(
            shortcut_command("enter", true, false, true, false, false),
            None
        );
    }

    #[test]
    fn stop_shortcuts_only_dispatch_while_running() {
        assert_eq!(
            shortcut_command("escape", false, false, false, true, true),
            Some(command::CANCEL)
        );
        assert_eq!(
            shortcut_command(".", true, false, false, true, true),
            Some(command::CANCEL)
        );
        assert_eq!(
            shortcut_command("escape", false, false, false, false, true),
            None
        );
    }

    /// None of Space Grotesk, Manrope or JetBrains Mono is guaranteed to be installed, so a missing
    /// family must degrade to a generic one rather than being passed through to the text system.
    #[test]
    fn font_families_fall_back_when_the_design_font_is_not_installed() {
        let installed = vec!["JetBrains Mono".to_owned(), "Noto Sans".to_owned()];
        assert_eq!(
            resolve_family(&installed, "JetBrains Mono", "monospace"),
            SharedString::from("JetBrains Mono")
        );
        assert_eq!(
            resolve_family(&installed, "Space Grotesk", "sans-serif"),
            SharedString::from("sans-serif")
        );
        assert_eq!(
            resolve_family(&[], "Manrope", "sans-serif"),
            SharedString::from("sans-serif")
        );
    }

    fn result_with_rows(rows: &[&str]) -> QueryResult {
        use crate::result::Column;
        QueryResult {
            columns: vec![Column {
                name: "value".into(),
                database_type: "text".into(),
                nullable: Some(false),
            }],
            rows: rows
                .iter()
                .map(|value| vec![CellValue::Text((*value).into())])
                .collect(),
            ..QueryResult::default()
        }
    }

    /// Dragging over the lines selects an inclusive range, and copying yields just those lines.
    #[gpui::test]
    fn selected_result_lines_are_the_ones_copied(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.display = ResultDisplay::Text;
            app.editor.results = vec![result_with_rows(&["alpha", "beta", "gamma"])];
        });

        let lines = view.update(cx, |app, _| app.selectable_lines());
        assert!(lines.len() >= 3, "text output should have several lines");
        let last = lines.len() - 1;

        // Drag backwards, from the final line up to the one before it.
        view.update(cx, |app, _| {
            app.result_selection = ResultSelection::whole_lines(last - 1, last).into();
        });

        view.update(cx, |app, cx| {
            let copied = app
                .copyable_result_text()
                .expect("there are results to copy");
            assert_eq!(copied, format!("{}\n{}", lines[last - 1], lines[last]));
            app.copy_results(cx);
            assert_eq!(app.status, "Copied 2 lines");
        });

        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            clipboard.as_deref(),
            Some(format!("{}\n{}", lines[last - 1], lines[last]).as_str())
        );
    }

    /// Dragging the splitter up makes the bottom pane taller, and it is clamped so neither the
    /// pane nor the editor can be squeezed away.
    #[gpui::test]
    fn dragging_the_splitter_resizes_the_results_pane(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let viewport = px(820.);

        view.update(cx, |app, _| {
            assert_eq!(app.result_pane_height, px(RESULT_PANE_HEIGHT));

            app.pane_drag = Some(PaneDrag {
                pointer_origin: px(500.),
                height_origin: px(RESULT_PANE_HEIGHT),
            });

            // Up 100px: the pane grows by 100.
            assert!(app.drag_pane(px(400.), viewport));
            assert_eq!(app.result_pane_height, px(RESULT_PANE_HEIGHT + 100.));

            // Back down past the start: it shrinks.
            assert!(app.drag_pane(px(600.), viewport));
            assert_eq!(app.result_pane_height, px(RESULT_PANE_HEIGHT - 100.));

            // Dragging far down stops at the floor rather than collapsing the pane.
            assert!(app.drag_pane(px(5_000.), viewport));
            assert_eq!(app.result_pane_height, px(RESULT_PANE_MIN_HEIGHT));

            // Dragging far up leaves room for the editor rather than filling the window.
            assert!(app.drag_pane(px(-5_000.), viewport));
            assert_eq!(app.result_pane_height, viewport - px(RESULT_PANE_RESERVED));
            assert!(app.result_pane_height < viewport);
        });
    }

    /// On a window too short for the reserved space, the floor still wins over the ceiling so the
    /// clamp can never invert and panic.
    #[gpui::test]
    fn splitter_clamp_survives_a_very_short_window(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        view.update(cx, |app, _| {
            app.pane_drag = Some(PaneDrag {
                pointer_origin: px(100.),
                height_origin: px(RESULT_PANE_HEIGHT),
            });

            app.drag_pane(px(0.), px(200.));
            assert_eq!(app.result_pane_height, px(RESULT_PANE_MIN_HEIGHT));
        });
    }

    /// Nothing moves unless a drag is actually in progress.
    #[gpui::test]
    fn pointer_movement_without_a_drag_leaves_the_pane_alone(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        view.update(cx, |app, _| {
            assert!(!app.drag_pane(px(50.), px(820.)));
            assert_eq!(app.result_pane_height, px(RESULT_PANE_HEIGHT));
        });
    }

    /// Partial lines at each end, whole lines in between — the standard shape of a text selection.
    #[test]
    fn a_selection_spans_partial_lines_at_each_end() {
        let selection = ResultSelection {
            anchor: ResultPosition { line: 1, column: 4 },
            head: ResultPosition { line: 3, column: 2 },
        };

        assert_eq!(selection.span_for(0, 10), None, "before the selection");
        assert_eq!(
            selection.span_for(1, 10),
            Some(4..10),
            "from column 4 to EOL"
        );
        assert_eq!(selection.span_for(2, 10), Some(0..10), "whole middle line");
        assert_eq!(selection.span_for(3, 10), Some(0..2), "up to column 2");
        assert_eq!(selection.span_for(4, 10), None, "after the selection");

        // Columns beyond the end of a short line clamp to its length.
        assert_eq!(selection.span_for(1, 6), Some(4..6));
        assert_eq!(selection.span_for(2, 3), Some(0..3));
    }

    /// Dragging right-to-left or bottom-to-top selects the same text as the forward gesture.
    #[test]
    fn a_backwards_drag_selects_the_same_span() {
        let forwards = ResultSelection {
            anchor: ResultPosition { line: 1, column: 2 },
            head: ResultPosition { line: 2, column: 5 },
        };
        let backwards = ResultSelection {
            anchor: ResultPosition { line: 2, column: 5 },
            head: ResultPosition { line: 1, column: 2 },
        };

        for line in 0..4 {
            assert_eq!(
                forwards.span_for(line, 8),
                backwards.span_for(line, 8),
                "line {line}"
            );
        }
    }

    /// A single click selects nothing until it is dragged; it must not copy a stray character.
    #[test]
    fn an_unmoved_caret_selects_nothing() {
        let caret = ResultSelection {
            anchor: ResultPosition { line: 2, column: 3 },
            head: ResultPosition { line: 2, column: 3 },
        };

        assert_eq!(caret.span_for(2, 10), None, "nothing to highlight");
        assert_eq!(
            caret.clamped_span(2, 10),
            Some(3..3),
            "the line is still inside the selection, it just contributes no characters"
        );
    }

    /// A blank line inside the selection is a line break that copying must preserve, so it is
    /// distinguished from a line that falls outside the selection entirely.
    #[test]
    fn blank_lines_inside_a_selection_survive_the_copy() {
        let selection = ResultSelection::whole_lines(0, 2);

        assert_eq!(
            selection.clamped_span(1, 0),
            Some(0..0),
            "blank middle line"
        );
        assert_eq!(selection.span_for(1, 0), None, "but nothing to highlight");
        assert_eq!(selection.clamped_span(3, 5), None, "outside the selection");
    }

    /// Grid rows and select-all cover each line end to end regardless of its length.
    #[test]
    fn whole_line_selections_cover_every_column() {
        let rows = ResultSelection::whole_lines(1, 2);

        assert_eq!(rows.span_for(1, 4), Some(0..4));
        assert_eq!(rows.span_for(2, 120), Some(0..120));
        assert_eq!(rows.span_for(3, 10), None);
    }

    /// Copying a character-level selection yields exactly the selected substring.
    #[gpui::test]
    fn copying_a_character_selection_yields_the_selected_substring(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| {
            app.editor.display = ResultDisplay::Text;
            app.editor.results = vec![result_with_rows(&["alpha", "beta"])];

            let lines = app.selectable_lines();
            let first = lines[0].chars().count();
            // From two characters into the first line, to two into the second.
            app.result_selection = Some(ResultSelection {
                anchor: ResultPosition { line: 0, column: 2 },
                head: ResultPosition { line: 1, column: 2 },
            });

            let copied = app.copyable_result_text().expect("results exist");
            let expected = format!(
                "{}\n{}",
                lines[0].chars().skip(2).take(first - 2).collect::<String>(),
                lines[1].chars().take(2).collect::<String>()
            );
            assert_eq!(copied, expected);
            app.copy_results(cx);
        });

        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert!(clipboard.is_some_and(|text| !text.is_empty()));
    }

    /// The pointer maps to a column through the monospace advance, measured from the shared left
    /// edge of every line and following the horizontal scroll.
    #[gpui::test]
    fn pointer_position_maps_to_a_character_column(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let advance = app.mono_advance;
            assert!(advance > px(0.), "the monospace advance should be measured");

            let origin = app.results_scroll.handle.bounds().origin.x;
            let text_left = origin + px(RESULT_TEXT_INSET);

            assert_eq!(app.column_at(text_left), 0);
            assert_eq!(app.column_at(text_left + advance * 5.), 5);
            // Rounding puts the caret at the nearest boundary, not the one to the left.
            assert_eq!(app.column_at(text_left + advance * 5. + advance * 0.6), 6);
            // Left of the text can never produce a negative column.
            assert_eq!(app.column_at(text_left - advance * 3.), 0);
        });
    }

    /// A table wider than its viewport must overflow so the horizontal bar has something to
    /// scroll. `overflow_hidden` on the grid used to zero taffy's automatic minimum size, letting
    /// it shrink to the viewport and clip its own columns instead.
    #[gpui::test]
    fn a_wide_table_overflows_sideways_instead_of_being_clipped(cx: &mut TestAppContext) {
        use crate::result::Column;
        let (view, cx) = build_app_view(cx);

        // Twenty 180px columns is far wider than the 1280px test window.
        let columns: Vec<_> = (0..20)
            .map(|index| Column {
                name: format!("column_{index}"),
                database_type: "text".into(),
                nullable: Some(false),
            })
            .collect();
        let row: Vec<_> = (0..20)
            .map(|index| CellValue::Text(format!("value_{index}")))
            .collect();

        view.update(cx, |app, _| {
            app.editor.display = ResultDisplay::Table;
            app.editor.results = vec![QueryResult {
                columns,
                rows: vec![row],
                ..QueryResult::default()
            }];
            app.editor.destination = ResultDestination::Pane;
            app.active_result_tab = false;
        });
        // Force a layout pass so the scroll handle has measured geometry.
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let overflow = app.results_scroll.handle.max_offset();
            assert!(
                overflow.width > px(0.),
                "a 20-column grid should overflow its viewport, got {:?}",
                overflow.width
            );
            // And the drawn bar follows from that overflow.
            let viewport = app.results_scroll.handle.bounds().size;
            assert!(
                ThumbMetrics::measure(viewport.width, overflow.width, px(0.)).is_some(),
                "a horizontal scrollbar should be warranted"
            );
        });
    }

    /// Table rows are selectable too, and copy as tab-separated values so a paste lands in
    /// spreadsheet columns. The header is a selectable unit in its own right.
    #[gpui::test]
    fn table_rows_are_selectable_and_copy_as_tab_separated_values(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.display = ResultDisplay::Table;
            app.editor.results = vec![result_with_rows(&["alpha", "beta", "gamma"])];
        });

        view.update(cx, |app, _| {
            // Header, then one entry per row.
            assert_eq!(
                app.selectable_lines(),
                vec!["value", "alpha", "beta", "gamma"]
            );
        });

        // Select the two middle data rows, skipping the header.
        view.update(cx, |app, cx| {
            app.result_selection = Some(ResultSelection::whole_lines(1, 2));
            app.copy_results(cx);
            assert_eq!(app.status, "Copied 2 lines");
        });

        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(clipboard.as_deref(), Some("alpha\nbeta"));
    }

    /// Multi-column rows keep their columns tab separated.
    #[gpui::test]
    fn multi_column_rows_copy_one_tab_separated_line_each(cx: &mut TestAppContext) {
        use crate::result::Column;
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.display = ResultDisplay::Table;
            app.editor.results = vec![QueryResult {
                columns: vec![
                    Column {
                        name: "muscle_group".into(),
                        database_type: "text".into(),
                        nullable: Some(false),
                    },
                    Column {
                        name: "sets".into(),
                        database_type: "int8".into(),
                        nullable: Some(true),
                    },
                ],
                rows: vec![
                    vec![CellValue::Text("back".into()), CellValue::Integer(48)],
                    vec![CellValue::Text("legs".into()), CellValue::Null],
                ],
                ..QueryResult::default()
            }];
        });

        view.update(cx, |app, _| {
            let lines = app.selectable_lines();
            assert_eq!(lines[0], "muscle_group\tsets");
            assert_eq!(lines[1], "back\t48");
            // NULL copies as its display form rather than an empty cell.
            assert!(lines[2].starts_with("legs\t"));
            assert_ne!(lines[2], "legs\t");
        });
    }

    /// Indices mean lines in Text mode and rows in Table mode, so a selection must not survive the
    /// switch and highlight unrelated data.
    #[gpui::test]
    fn switching_display_mode_clears_the_selection(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| {
            app.editor.display = ResultDisplay::Table;
            app.editor.results = vec![result_with_rows(&["alpha", "beta"])];
            app.result_selection = Some(ResultSelection::whole_lines(1, 2));

            app.set_display(ResultDisplay::Text, cx);
            assert!(app.result_selection.is_none());

            // Re-selecting and setting the same mode again is not a change, so it survives.
            app.result_selection = Some(ResultSelection::whole_lines(0, 0));
            app.set_display(ResultDisplay::Text, cx);
            assert!(app.result_selection.is_some());
        });
    }

    /// Copying with nothing selected should still hand over the data, not an empty clipboard.
    #[gpui::test]
    fn copying_with_no_selection_copies_the_whole_result(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.display = ResultDisplay::Text;
            app.editor.results = vec![result_with_rows(&["alpha", "beta"])];
        });

        let (expected, copied) = view.update(cx, |app, _| {
            (
                app.selectable_lines().join("\n"),
                app.copyable_result_text().expect("results exist"),
            )
        });

        assert_eq!(copied, expected);
        assert!(copied.contains("alpha") && copied.contains("beta"));
    }

    /// Ctrl+C and Ctrl+A go to the results when the result tab is in front, and to the SQL
    /// document otherwise.
    #[gpui::test]
    fn copy_and_select_all_follow_whichever_surface_is_in_front(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.document = "SELECT 1;".into();
            app.editor.display = ResultDisplay::Text;
            app.editor.results = vec![result_with_rows(&["alpha"])];
        });

        // Editor in front: select-all targets the document.
        view.update(cx, |app, cx| {
            app.active_result_tab = false;
            assert!(!app.results_have_focus());
            app.handle_key(&command_key("a"), cx);
            assert_eq!(app.editor.selection, Some(0..app.editor.document.len()));
        });

        // Result tab in front: select-all targets the result lines instead.
        view.update(cx, |app, cx| {
            app.active_result_tab = true;
            app.editor.selection = None;
            assert!(app.results_have_focus());
            app.handle_key(&command_key("a"), cx);
            assert!(app.editor.selection.is_none());
            let expected = app.selectable_lines().len() - 1;
            assert_eq!(
                app.result_selection.map(|s| s.bounds()),
                Some((0, expected))
            );
        });

        view.update(cx, |app, cx| app.handle_key(&command_key("c"), cx));
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            clipboard.as_deref().map(|text| text.contains("alpha")),
            Some(true)
        );
    }

    fn command_key(key: &str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers: Modifiers {
                    control: true,
                    ..Modifiers::default()
                },
                key: key.into(),
                key_char: None,
            },
            is_held: false,
        }
    }

    /// No bar when everything fits; otherwise a thumb proportional to the visible fraction that
    /// reaches the far end at full scroll.
    #[test]
    fn scrollbar_thumb_tracks_the_visible_fraction() {
        assert!(ThumbMetrics::measure(px(300.), px(0.), px(0.)).is_none());

        // 300px of viewport over 900px of content: a third of the track, parked at the top.
        let top = ThumbMetrics::measure(px(300.), px(600.), px(0.)).expect("content overflows");
        assert_eq!(top.length, px(100.));
        assert_eq!(top.start, px(0.));

        // Fully scrolled: the thumb ends flush with the bottom of the track.
        let bottom =
            ThumbMetrics::measure(px(300.), px(600.), px(-600.)).expect("content overflows");
        assert_eq!(bottom.start + bottom.length, px(300.));

        // Dragging the thumb one pixel moves the content by the overflow-to-track ratio.
        assert_eq!(top.pixels_per_thumb_pixel, 3.);
    }

    /// A tiny thumb on a huge result set would be unusable, so it has a floor.
    #[test]
    fn scrollbar_thumb_never_shrinks_below_the_minimum() {
        let metrics =
            ThumbMetrics::measure(px(300.), px(100_000.), px(0.)).expect("content overflows");

        assert_eq!(metrics.length, px(SCROLLBAR_MIN_THUMB));
        assert!(metrics.start >= px(0.), "start was {:?}", metrics.start);
    }

    #[test]
    fn connected_profile_uses_the_mint_accent_status_indicator() {
        assert_eq!(
            connection_indicator_colour(true, ConnectionState::Connected),
            ACCENT
        );
        assert_eq!(
            connection_indicator_colour(false, ConnectionState::Connected),
            FAINT
        );
        assert_eq!(
            connection_indicator_colour(true, ConnectionState::Failed),
            RED
        );
    }

    /// The accent is also the keyword and primary-action colour, so an in-progress connection must
    /// not be painted with it — otherwise "connecting" is indistinguishable from "connected".
    #[test]
    fn in_progress_connection_states_are_distinct_from_connected() {
        for state in [ConnectionState::Connecting, ConnectionState::Disconnecting] {
            let colour = connection_indicator_colour(true, state);
            assert_eq!(colour, WARN);
            assert_ne!(colour, ACCENT);
        }
    }

    #[gpui::test]
    fn native_connection_row_click_connects_and_alt_click_disconnects(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.service = Arc::new(CommandService::new(provider.clone()));
        });

        let connection_row = cx
            .debug_bounds("connection-row-0")
            .expect("the first connection row should be rendered");
        cx.simulate_click(connection_row.center(), Modifiers::default());
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        assert_eq!(provider.connect_calls.load(Ordering::SeqCst), 1);
        view.update(cx, |app, _| {
            assert_eq!(
                connection_indicator_colour(true, app.connection_state),
                ACCENT
            );
        });

        let connection_row = cx
            .debug_bounds("connection-row-0")
            .expect("the connected row should remain rendered");
        cx.simulate_click(
            connection_row.center(),
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        );
        wait_for_connection_state(&view, cx, ConnectionState::Disconnected);

        assert_eq!(provider.disconnect_calls.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn native_keyboard_input_edits_the_focused_document(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        cx.simulate_input("select 1");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("from pg_catalog.pg_tables");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.document, "select 1\nfrom pg_catalog.pg_tables");
            assert_eq!(app.editor.cursor, app.editor.document.len());
        });
    }

    #[gpui::test]
    fn native_shortcut_opens_and_switches_to_a_new_editor(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        cx.simulate_input("select current_database()");

        cx.simulate_keystrokes("ctrl-n");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.title, "query-2.sql");
            assert!(app.editor.document.is_empty());
            assert_eq!(app.background_editors.len(), 1);
            assert_eq!(
                app.background_editors[0].document,
                "select current_database()"
            );
            assert_eq!(app.status, "New SQL editor");
        });
    }

    fn profile_named(name: &str, database: &str) -> ConnectionProfile {
        let mut profile = ConnectionProfile::from_database_url(&format!(
            "postgresql://user@localhost:5432/{database}"
        ))
        .expect("test profile should be valid");
        profile.name = name.to_owned();
        profile
    }

    /// There is one session. Every editor must name the database that session opened, or a tab can
    /// claim to target one database while its SQL reaches another (§49, §50).
    #[gpui::test]
    fn every_editor_reports_the_database_the_session_actually_opened(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let production = profile_named("Production", "production");

        view.update(cx, |app, _| {
            app.profiles.push(production.clone());
            app.background_editors
                .push(EditorState::new(profile_named("Local", "local")));
            app.editor.connection = profile_named("Local", "local");

            app.adopt_session_profile(production.clone());
        });

        view.update(cx, |app, _| {
            assert_eq!(app.target_identity(), "Production / production");
            assert_eq!(app.editor.connection.id, production.id);
            assert_eq!(app.background_editors[0].connection.id, production.id);
        });

        // Switching to the other tab must not resurrect the stale target.
        view.update(cx, |app, cx| app.switch_editor(0, cx));
        view.update(cx, |app, _| {
            assert_eq!(app.target_identity(), "Production / production");
            assert_eq!(app.editor.connection.id, production.id);
        });
    }

    /// A completing query writes back only its outcome, so text typed while it ran survives. The
    /// query is held open by the provider so the typing genuinely races the in-flight run.
    #[gpui::test]
    fn text_typed_while_a_query_runs_is_not_discarded(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.service = Arc::new(CommandService::new(provider.clone()));
        });

        provider.blocked.store(true, Ordering::SeqCst);
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1;".into();
            app.editor.cursor = app.editor.document.len();
            app.connect(cx);
        });
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.dispatch_command(command::RUN, cx));
        cx.run_until_parked();
        view.update(cx, |app, _| {
            assert_eq!(app.editor.execution_status, ExecutionStatus::Running)
        });

        // The user carries on typing while the statement is still executing.
        cx.simulate_input(" -- note");
        provider.blocked.store(false, Ordering::SeqCst);
        wait_for_execution_status(&view, cx, ExecutionStatus::Completed);

        view.update(cx, |app, _| {
            assert_eq!(app.editor.document, "SELECT 1; -- note");
            assert_eq!(app.editor.results.len(), 1);
        });
    }

    fn wait_for_execution_status(
        view: &gpui::Entity<AppView>,
        cx: &mut gpui::VisualTestContext,
        expected: ExecutionStatus,
    ) {
        // The provider polls on a separate runtime, so this has to yield real time rather than
        // just spinning the GPUI executor.
        for _ in 0..1_000 {
            cx.run_until_parked();
            if view.update(cx, |app, _| app.editor.execution_status) == expected {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        view.update(cx, |app, _| {
            assert_eq!(app.editor.execution_status, expected)
        });
    }

    /// Re-entering a corrected URL replaces the manual row rather than stacking a second one that
    /// still points at the unreachable host.
    #[gpui::test]
    fn correcting_a_manual_connection_reuses_the_single_manual_row(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let baseline = view.update(cx, |app, _| app.profiles.len());

        for database in ["typo", "corrected"] {
            view.update(cx, |app, cx| {
                app.connection_dialog = true;
                app.connection_buffer = format!("postgresql://user@localhost:5432/{database}");
                app.handle_key(&key_event("enter"), cx);
            });
        }

        view.update(cx, |app, _| {
            let manual: Vec<_> = app
                .profiles
                .iter()
                .filter(|profile| profile.name == MANUAL_PROFILE_NAME)
                .collect();
            assert_eq!(manual.len(), 1);
            assert_eq!(manual[0].configuration.database, "corrected");
            assert_eq!(app.profiles.len(), baseline + 1);
        });
    }

    fn key_event(key: &str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers: Modifiers::default(),
                key: key.into(),
                key_char: None,
            },
            is_held: false,
        }
    }

    #[gpui::test]
    fn native_window_lifecycle_survives_resize_and_accepts_close(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let editor_id = view.update(cx, |app, _| app.editor.id);

        cx.simulate_resize(size(px(900.), px(600.)));
        cx.run_until_parked();

        view.update(cx, |app, _| assert_eq!(app.editor.id, editor_id));
        assert!(cx.simulate_close());
    }
}
