use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, Point, ScrollHandle, SharedString, Window,
    WindowBounds, WindowOptions, div, point, prelude::*, px, rgb, size,
};
use tokio::runtime::Runtime;

use crate::application::{CommandService, EditorState, ResultDestination, ResultDisplay, command};
use crate::config::{ConnectionProfile, local_profile};
use crate::database::{ConnectionState, DatabaseObject, DatabaseProvider, ObjectKind};
use crate::definition::{DefinitionSection, ObjectDefinition};
use crate::postgres::PostgresProvider;
use crate::result::{CellValue, ExecutionStatus, QueryError, QueryResult};
use crate::sql::{Highlight, HighlightSpan, highlight_lines};

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
/// Editor lines kept built beyond each edge of the viewport, so a scroll does not expose a gap.
const EDITOR_OVERSCAN: usize = 8;
/// How many lines to assume before the first layout pass has measured the editor viewport.
const UNMEASURED_VIEWPORT_LINES: usize = 80;
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
/// Height of one grid row. Fixed so the windowed grid can place rows by multiplying it.
const RESULT_ROW_HEIGHT: f32 = 46.;
/// Rows of slack allowed per result for chrome the row arithmetic does not model — the result
/// header and block padding, whose heights belong to the styling. Overshooting builds a few extra
/// rows; undershooting would leave a blank band at the edge of the viewport.
const RESULT_CHROME_SLOP_ROWS: usize = 12;
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
/// The width of one monospace character at `text_size`. Selection highlights and the editor caret
/// are positioned by multiplying this, so it must be measured at the size the surface renders.
fn measure_mono_advance(fonts: &Fonts, text_size: f32, cx: &App) -> Pixels {
    let text_system = cx.text_system();
    let font_id = text_system.resolve_font(&gpui::font(fonts.mono.clone()));
    text_system
        .advance(font_id, px(text_size), '0')
        .map(|advance| advance.width)
        .ok()
        .filter(|advance| *advance > px(0.))
        .unwrap_or(px(text_size * 0.6))
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

/// Everything that belongs to one connection rather than to the workspace: its provider session,
/// what that session is doing, and the metadata it has loaded. There is one per profile the user
/// has connected, so two editors can sit on two databases at once (§48, §49).
struct ConnectionSession {
    service: Arc<CommandService>,
    state: ConnectionState,
    server_version: Option<String>,
    schemas: Vec<String>,
    objects: HashMap<String, Vec<DatabaseObject>>,
    expanded_schema: Option<String>,
}

impl ConnectionSession {
    fn new(provider: Arc<dyn DatabaseProvider>) -> Self {
        Self {
            service: Arc::new(CommandService::new(provider)),
            state: ConnectionState::Disconnected,
            server_version: None,
            schemas: Vec::new(),
            objects: HashMap::new(),
            expanded_schema: None,
        }
    }

    /// Forgets what the closed session knew. The metadata cache lives in the provider, but the
    /// tree renders from here, so a reconnect must not repaint the old database's objects.
    fn clear(&mut self) {
        self.server_version = None;
        self.schemas.clear();
        self.objects.clear();
        self.expanded_schema = None;
    }
}

/// One open definition. Keyed to a profile rather than an editor, so editors may be switched or
/// closed beneath it (§8).
struct DefinitionTab {
    id: uuid::Uuid,
    profile_id: uuid::Uuid,
    object: DatabaseObject,
    state: DefinitionState,
}

enum DefinitionState {
    Loading,
    Loaded(ObjectDefinition),
    /// A failed load or refresh replaces the body, so stale content is never shown as current
    /// (§11, FR2-010).
    Failed(QueryError),
}

/// The tree's context menu, open over one object. §8 admits one entry and forbids anything that
/// modifies the database.
struct ObjectMenu {
    object: DatabaseObject,
}

impl ObjectMenu {
    fn entries(&self) -> Vec<&'static str> {
        vec!["Open Definition"]
    }
}

/// Which surface the workspace is showing. Replaces the earlier `active_result_tab` flag, which
/// could not express a third destination.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Result,
    Definition(uuid::Uuid),
}

struct AppView {
    /// One session per profile the user has connected to, keyed by profile. Built on demand, so a
    /// profile that has never been used costs nothing.
    sessions: HashMap<uuid::Uuid, ConnectionSession>,
    /// How a session's provider is built. Production makes a `PostgresProvider`; the tests
    /// substitute a fake, which they can no longer do by replacing a single service.
    provider_factory: Arc<dyn Fn() -> Arc<dyn DatabaseProvider>>,
    runtime: Arc<Runtime>,
    profiles: Vec<ConnectionProfile>,
    editor: EditorState,
    background_editors: Vec<EditorState>,
    /// Definitions open beside the editors, in the order they were opened (FR2-006).
    definitions: Vec<DefinitionTab>,
    focus: Focus,
    status: String,
    focus_handle: FocusHandle,
    /// Document snapshots paired with the caret they were taken at.
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    connection_dialog: bool,
    connection_buffer: String,
    /// Digits typed into the row-limit field, or `None` when it is not being edited. The ± steps
    /// move by a factor of ten, so this is the only way to reach a value between them (§26).
    limit_buffer: Option<String>,
    fonts: Fonts,
    /// Whether the connection selector's list is open over the workspace.
    connection_menu: bool,
    /// The object tree's context menu, open over one object (§8).
    object_menu: Option<ObjectMenu>,
    /// Where `object_menu_card` anchors itself, set from the right click that opened the menu so
    /// it appears beside the row that was acted on rather than at a fixed point on the workspace.
    object_menu_position: Point<Pixels>,
    /// Whether the profile list came from real configuration rather than the built-in fallback.
    configured: bool,
    editor_scroll: ScrollState,
    results_scroll: ScrollState,
    /// The definition surface has its own, so reading a long definition does not move the SQL
    /// document out from under the caret.
    definition_scroll: ScrollState,
    /// Selected text in the results, and whether a drag is extending it.
    result_selection: Option<ResultSelection>,
    selecting_results: bool,
    /// Whether a pointer drag is currently extending the SQL editor's selection.
    selecting_editor: bool,
    /// Advance width of one monospace character at the result text size, used to turn a pointer
    /// position into a column and to size the selection highlight.
    mono_advance: Pixels,
    /// The same measurement for the SQL editor, which renders at its own larger text size.
    editor_advance: Pixels,
    result_pane_height: Pixels,
    pane_drag: Option<PaneDrag>,
    /// The separate results window, kept so repeated runs refresh it instead of opening another
    /// one. Dropped back to `None` once the user closes it (§32.3).
    result_window: Option<gpui::WindowHandle<ResultWindow>>,
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
        Self {
            sessions: HashMap::new(),
            provider_factory: Arc::new(|| PostgresProvider::new()),
            runtime: Arc::new(Runtime::new().expect("could not start asynchronous runtime")),
            profiles,
            editor: EditorState::new(profile),
            background_editors: Vec::new(),
            definitions: Vec::new(),
            focus: Focus::Editor,
            status: "Disconnected · Run a query to see results.".into(),
            focus_handle: cx.focus_handle(),
            undo: Vec::new(),
            redo: Vec::new(),
            connection_dialog: false,
            connection_buffer: String::new(),
            limit_buffer: None,
            fonts,
            connection_menu: false,
            object_menu: None,
            object_menu_position: point(px(SIDEBAR_WIDTH + 30.), px(TITLEBAR_HEIGHT + 74.)),
            configured,
            editor_scroll: ScrollState::default(),
            results_scroll: ScrollState::default(),
            definition_scroll: ScrollState::default(),
            result_selection: None,
            selecting_results: false,
            selecting_editor: false,
            mono_advance: measure_mono_advance(&fonts_for_advance, RESULT_TEXT_SIZE, cx),
            editor_advance: measure_mono_advance(&fonts_for_advance, EDITOR_TEXT_SIZE, cx),
            result_pane_height: px(RESULT_PANE_HEIGHT),
            pane_drag: None,
            result_window: None,
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
    /// Turns a pointer position into a column of the SQL document.
    fn editor_column_at(&self, x: Pixels) -> usize {
        if self.editor_advance <= px(0.) {
            return 0;
        }
        let handle = &self.editor_scroll.handle;
        let text_left = handle.bounds().origin.x + handle.offset().x + px(GUTTER_WIDTH);
        let columns = ((x - text_left) / self.editor_advance).round();
        if columns <= 0. { 0 } else { columns as usize }
    }

    /// Places the caret under the pointer and arms a drag selection.
    fn click_editor(&mut self, line: usize, x: Pixels, extend: bool, cx: &mut Context<Self>) {
        // Clicking into the document is a way out of the row-limit field, which otherwise holds
        // every keystroke it sees, and out of the connection list.
        if self.limit_buffer.is_some() {
            self.cancel_limit_edit(cx);
        }
        if self.connection_menu {
            self.toggle_connection_menu(cx);
        }
        self.object_menu = None;
        self.selecting_editor = true;
        let offset = offset_of(&self.editor.document, line, self.editor_column_at(x));
        self.place_cursor(offset, extend, cx);
    }

    /// Extends an armed drag to the pointer. Does nothing when no drag is in progress.
    fn drag_editor(&mut self, line: usize, x: Pixels, cx: &mut Context<Self>) {
        if !self.selecting_editor {
            return;
        }
        let offset = offset_of(&self.editor.document, line, self.editor_column_at(x));
        if offset != self.editor.cursor {
            self.place_cursor(offset, true, cx);
        }
    }

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
            && (self.focus == Focus::Result || self.result_selection.is_some())
    }

    /// The connection SQL will actually run against. Each editor carries its own, so this is the
    /// active editor's and nothing else's (§49).
    fn target_profile(&self) -> &ConnectionProfile {
        &self.editor.connection
    }

    fn target_identity(&self) -> String {
        self.target_profile().display_identity()
    }

    fn is_running(&self) -> bool {
        is_busy(self.editor.execution_status)
    }

    /// The session behind a profile, created on first use so an untouched profile never builds a
    /// provider.
    fn session_for(&mut self, profile_id: uuid::Uuid) -> &mut ConnectionSession {
        let factory = self.provider_factory.clone();
        self.sessions
            .entry(profile_id)
            .or_insert_with(|| ConnectionSession::new(factory()))
    }

    /// The active editor's session, once it has one.
    fn session(&self) -> Option<&ConnectionSession> {
        self.sessions.get(&self.editor.connection.id)
    }

    fn active_session(&mut self) -> &mut ConnectionSession {
        self.session_for(self.editor.connection.id)
    }

    fn state_of(&self, profile_id: uuid::Uuid) -> ConnectionState {
        self.sessions
            .get(&profile_id)
            .map_or(ConnectionState::Disconnected, |session| session.state)
    }

    /// The state of the connection the active editor is pointed at.
    fn connection_state(&self) -> ConnectionState {
        self.state_of(self.editor.connection.id)
    }

    /// The server behind the active editor's connection, blank until it reports one.
    fn server_version(&self) -> String {
        self.session()
            .and_then(|session| session.server_version.clone())
            .unwrap_or_default()
    }

    /// Points every editor already targeting this profile at what the session actually opened, so
    /// no tab can name a database its own session is not using (§49, §50). Editors targeting other
    /// connections are left alone.
    fn adopt_connected_profile(&mut self, profile: ConnectionProfile) {
        for editor in std::iter::once(&mut self.editor).chain(&mut self.background_editors) {
            if editor.connection.id == profile.id {
                editor.connection = profile.clone();
            }
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        self.connect_profile(self.editor.connection.clone(), cx);
    }

    fn connect_profile(&mut self, profile: ConnectionProfile, cx: &mut Context<Self>) {
        let profile_id = profile.id;
        if matches!(
            self.state_of(profile_id),
            ConnectionState::Connecting | ConnectionState::Connected
        ) {
            return;
        }
        let session = self.session_for(profile_id);
        session.state = ConnectionState::Connecting;
        let service = session.service.clone();
        self.status = format!("Connecting to {}…", profile.name);
        let runtime = self.runtime.clone();
        let connecting_profile = profile.clone();
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
                        let session = this.session_for(profile_id);
                        session.state = ConnectionState::Connected;
                        session.server_version = Some(info.server_version);
                        session.schemas = schemas;
                        let mut connected = connecting_profile;
                        connected.configuration.database = info.database;
                        this.status = format!("Connected: {}", connected.display_identity());
                        this.adopt_connected_profile(connected);
                    }
                    Ok(Err(error)) => {
                        this.session_for(profile_id).state = ConnectionState::Failed;
                        this.status = format!("Connection failed: {error}");
                    }
                    Err(_) => {
                        this.session_for(profile_id).state = ConnectionState::Failed;
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
        self.disconnect_profile(self.editor.connection.id, cx);
    }

    fn disconnect_profile(&mut self, profile_id: uuid::Uuid, cx: &mut Context<Self>) {
        // Closing the session cancels whatever is in flight and loses its results, so the running
        // query has to finish or be stopped first — the same rule the connection rows enforce.
        if self.is_running() && self.editor.connection.id == profile_id {
            self.status = "Wait for the active query before disconnecting".into();
            cx.notify();
            return;
        }
        let session = self.session_for(profile_id);
        session.state = ConnectionState::Disconnecting;
        let service = session.service.clone();
        self.status = "Disconnecting…".into();
        let runtime = self.runtime.clone();
        cx.spawn(async move |view, cx| {
            let result = runtime
                .spawn(async move { service.disconnect().await })
                .await;
            let _ = view.update(cx, |this, cx| {
                let session = this.session_for(profile_id);
                match result {
                    Ok(Ok(())) => {
                        session.state = ConnectionState::Disconnected;
                        session.clear();
                        this.status = "Disconnected · SQL and results preserved".into();
                    }
                    _ => {
                        session.state = ConnectionState::Failed;
                        this.status = "Disconnect failed".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_schema(
        &mut self,
        profile_id: uuid::Uuid,
        schema: String,
        refresh: bool,
        cx: &mut Context<Self>,
    ) {
        let session = self.session_for(profile_id);
        if session.expanded_schema.as_deref() == Some(&schema) && !refresh {
            session.expanded_schema = None;
            cx.notify();
            return;
        }
        session.expanded_schema = Some(schema.clone());
        let service = session.service.clone();
        self.status = format!("Loading {schema}…");
        let runtime = self.runtime.clone();
        cx.spawn(async move |view, cx| {
            let schema_for_query = schema.clone();
            let result = runtime
                .spawn(async move { service.objects(&schema_for_query, refresh).await })
                .await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(objects)) => {
                        this.session_for(profile_id)
                            .objects
                            .insert(schema.clone(), objects);
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

    /// Opens an object's definition beside the editors (FR2-006). The object's connection must be
    /// live: a definition cannot be fetched from a closed session, and a tab that can only show an
    /// error is worse than a message.
    fn open_definition(&mut self, object: DatabaseObject, cx: &mut Context<Self>) {
        let profile_id = self.editor.connection.id;
        if self.state_of(profile_id) != ConnectionState::Connected {
            self.status = "Connect before opening a definition".into();
            cx.notify();
            return;
        }
        // A definition tab shows no result grid, so a selection left over from the result surface
        // must not survive underneath it — otherwise Ctrl+C would copy lines visible nowhere on
        // screen (Task 9 review).
        self.result_selection = None;
        if let Some(existing) = self
            .definitions
            .iter()
            .find(|tab| tab.profile_id == profile_id && tab.object == object)
        {
            self.focus = Focus::Definition(existing.id);
            cx.notify();
            return;
        }
        let id = uuid::Uuid::new_v4();
        self.definitions.push(DefinitionTab {
            id,
            profile_id,
            object: object.clone(),
            state: DefinitionState::Loading,
        });
        self.focus = Focus::Definition(id);
        self.load_definition(id, profile_id, object, false, cx);
    }

    /// Metadata work follows the Phase 1 path: off the render thread, applied back on it (§10).
    ///
    /// The session is resolved from the tab's own `profile_id`, never from the editor in front: a
    /// definition is keyed to a connection (§8), and the editor may have been switched to another
    /// one since the tab was opened.
    fn load_definition(
        &mut self,
        id: uuid::Uuid,
        profile_id: uuid::Uuid,
        object: DatabaseObject,
        refresh: bool,
        cx: &mut Context<Self>,
    ) {
        let service = self.session_for(profile_id).service.clone();
        let runtime = self.runtime.clone();
        cx.spawn(async move |view, cx| {
            let outcome = runtime
                .spawn(async move { service.definition(&object, refresh).await })
                .await;
            let _ = view.update(cx, |app, cx| {
                let Some(tab) = app.definitions.iter_mut().find(|tab| tab.id == id) else {
                    return;
                };
                tab.state = match outcome {
                    Ok(Ok(definition)) => DefinitionState::Loaded(definition),
                    Ok(Err(error)) => DefinitionState::Failed(error),
                    Err(error) => DefinitionState::Failed(QueryError {
                        message: error.to_string(),
                        severity: None,
                        code: None,
                        detail: None,
                        hint: None,
                        position: None,
                    }),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Refetches a definition, bypassing the session cache (FR2-009). It goes back to the
    /// connection the tab was opened against, whatever the editor in front is now pointed at.
    fn refresh_definition(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        let Some(tab) = self.definitions.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        let profile_id = tab.profile_id;
        let object = tab.object.clone();
        tab.state = DefinitionState::Loading;
        self.load_definition(id, profile_id, object, true, cx);
    }

    /// Unlike editors, the workspace need not keep one: closing the last is allowed and focus
    /// falls back to the editor in front.
    fn close_definition(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        self.definitions.retain(|tab| tab.id != id);
        if self.focus == Focus::Definition(id) {
            self.focus = Focus::Editor;
        }
        cx.notify();
    }

    /// The whole tab as text, in section order: heading, then its lines or its SQL. §9 permits
    /// copying a definition; this is what the Copy action places on the clipboard.
    fn definition_text(&self, id: uuid::Uuid) -> Option<String> {
        let tab = self.definitions.iter().find(|tab| tab.id == id)?;
        let DefinitionState::Loaded(definition) = &tab.state else {
            return None;
        };
        let mut blocks = Vec::new();
        for section in definition.sections(&tab.object) {
            match section {
                DefinitionSection::Rows { heading, lines } => {
                    blocks.push(format!("{heading}\n{}", lines.join("\n")))
                }
                DefinitionSection::Sql { heading, sql } => blocks.push(format!("{heading}\n{sql}")),
                DefinitionSection::Note { text } => blocks.push(text),
            }
        }
        Some(blocks.join("\n\n"))
    }

    /// §9: copying is the one thing Phase 2 permits a user to do with a definition besides read it.
    fn copy_definition(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        if let Some(text) = self.definition_text(id) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.status = "Copied definition".into();
            cx.notify();
        }
    }

    /// Refreshes the metadata of the connection the active editor is using (§17).
    fn refresh_metadata(&mut self, cx: &mut Context<Self>) {
        let profile_id = self.editor.connection.id;
        if let Some(schema) = self
            .session()
            .and_then(|session| session.expanded_schema.clone())
        {
            self.load_schema(profile_id, schema, true, cx);
            return;
        }
        let service = self.active_session().service.clone();
        let runtime = self.runtime.clone();
        self.status = "Refreshing schemas…".into();
        cx.spawn(async move |view, cx| {
            let result = runtime
                .spawn(async move { service.schemas(true).await })
                .await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(Ok(schemas)) => {
                        let session = this.session_for(profile_id);
                        session.schemas = schemas;
                        session.objects.clear();
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
        if self.connection_state() != ConnectionState::Connected {
            self.status = format!(
                "Connect {} before executing SQL",
                self.editor.connection.name
            );
            cx.notify();
            return;
        }
        // One session means one statement at a time, and a running editor cannot be sent to the
        // background, so the active editor is the only one that can have a query in flight.
        // Guarding here covers every route in — the toolbar, the shortcuts, and anything added
        // later.
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
        let service = self.active_session().service.clone();
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
                                ResultDestination::Pane => this.focus = Focus::Editor,
                                ResultDestination::Tab => this.focus = Focus::Result,
                                ResultDestination::Window => {
                                    this.focus = Focus::Editor;
                                    this.show_results_in_window(cx);
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
        let service = self.active_session().service.clone();
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
        if self.connection_menu && key == "escape" {
            self.toggle_connection_menu(cx);
            return;
        }
        if self.object_menu.is_some() && key == "escape" {
            self.object_menu = None;
            cx.notify();
            return;
        }
        // The row-limit field has no focus handle of its own, so while it is open the keys are held
        // here rather than reaching the SQL document.
        if let Some(buffer) = self.limit_buffer.as_mut() {
            match key {
                "escape" => self.cancel_limit_edit(cx),
                "enter" => self.commit_limit_edit(cx),
                "backspace" => {
                    buffer.pop();
                    cx.notify();
                }
                _ => {
                    if let Some(character) = event.keystroke.key_char.as_deref()
                        && character
                            .chars()
                            .all(|character| character.is_ascii_digit())
                        && !character.is_empty()
                    {
                        buffer.push_str(character);
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
            self.connection_state() == ConnectionState::Connected,
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
                // A definition tab in front takes Ctrl+C before the results check: a stale result
                // selection must never win over the tab actually on screen (Task 9 review).
                "c" if let Focus::Definition(id) = self.focus => self.copy_definition(id, cx),
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
            "up" => self.move_line(false, modifiers.shift, cx),
            "down" => self.move_line(true, modifiers.shift, cx),
            "home" => self.move_to_line_edge(false, modifiers.shift, cx),
            "end" => self.move_to_line_edge(true, modifiers.shift, cx),
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
            command::CLOSE_EDITOR => self.close_active_editor(cx),
            command::CONNECT => self.connect(cx),
            command::DISCONNECT => self.disconnect(cx),
            // Opening a definition needs the object to open, which only the tree and its context
            // menu can supply, so they call `open_definition` directly.
            command::OPEN_DEFINITION => {}
            command::REFRESH_DEFINITION => {
                if let Focus::Definition(id) = self.focus {
                    self.refresh_definition(id, cx);
                }
            }
            command::CLOSE_DEFINITION => {
                if let Focus::Definition(id) = self.focus {
                    self.close_definition(id, cx);
                }
            }
            _ => {}
        }
    }

    fn toggle_connection_menu(&mut self, cx: &mut Context<Self>) {
        self.connection_menu = !self.connection_menu;
        cx.notify();
    }

    /// Opens the context menu over an object, or closes it if it is already open over that same
    /// object (§8). Opening it over a different object replaces whatever was open before, so the
    /// menu never lingers over the tree pointing at something stale.
    fn toggle_object_menu(&mut self, object: DatabaseObject, cx: &mut Context<Self>) {
        self.object_menu = match &self.object_menu {
            Some(menu) if menu.object == object => None,
            _ => Some(ObjectMenu { object }),
        };
        cx.notify();
    }

    /// FR2-012, §8: the menu's one entry opens the definition and dismisses the menu.
    fn choose_open_definition(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.object_menu.take() else {
            return;
        };
        self.open_definition(menu.object, cx);
    }

    /// Adding a connection is independent of the ones already open — it only retargets the editor
    /// in front, so a running query is the one thing that blocks it.
    fn configure_connection(&mut self, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        self.connection_buffer.clear();
        self.connection_dialog = true;
        self.status = "Enter a PostgreSQL URL; credentials are masked".into();
        cx.notify();
    }

    /// Snapshots the document *and* the caret before an edit, so undo can put the user back where
    /// they were working rather than at the end of the document.
    fn record_edit(&mut self) {
        self.undo
            .push((self.editor.document.clone(), self.editor.cursor));
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

    /// The fixed end of the current selection — the end the cursor is not sitting on. Holding it
    /// implicitly rather than as extra state is what lets a shift-selection shrink as well as grow.
    fn selection_anchor(&self) -> usize {
        match &self.editor.selection {
            Some(range) if self.editor.cursor == range.start => range.end,
            Some(range) => range.start,
            None => self.editor.cursor,
        }
    }

    /// Moves the caret, either collapsing the selection or dragging its free end along.
    fn place_cursor(&mut self, offset: usize, selecting: bool, cx: &mut Context<Self>) {
        self.editor.selection = selecting.then(|| {
            let anchor = self.selection_anchor();
            anchor.min(offset)..anchor.max(offset)
        });
        self.editor.cursor = offset;
        cx.notify();
    }

    fn move_cursor(&mut self, right: bool, selecting: bool, cx: &mut Context<Self>) {
        let offset = if right {
            next_boundary(&self.editor.document, self.editor.cursor)
        } else {
            previous_boundary(&self.editor.document, self.editor.cursor)
        };
        self.place_cursor(offset, selecting, cx);
    }

    /// Moves a line at a time, holding the column wherever the target line is long enough for it.
    fn move_line(&mut self, down: bool, selecting: bool, cx: &mut Context<Self>) {
        let position = document_position(&self.editor.document, self.editor.cursor);
        let line = if down {
            position.line + 1
        } else {
            position.line.saturating_sub(1)
        };
        let offset = offset_of(&self.editor.document, line, position.column);
        self.place_cursor(offset, selecting, cx);
    }

    /// Home and End, which stay within the current line.
    fn move_to_line_edge(&mut self, end: bool, selecting: bool, cx: &mut Context<Self>) {
        let line = document_position(&self.editor.document, self.editor.cursor).line;
        let column = if end { usize::MAX } else { 0 };
        let offset = offset_of(&self.editor.document, line, column);
        self.place_cursor(offset, selecting, cx);
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.undo.pop() {
            let current = self.restore(previous);
            self.redo.push(current);
            cx.notify();
        }
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.redo.pop() {
            let current = self.restore(next);
            self.undo.push(current);
            cx.notify();
        }
    }

    /// Swaps a history entry into the editor, returning the state it replaced for the other stack.
    /// Each entry's caret was recorded against its own document, so it needs no clamping.
    fn restore(&mut self, (document, cursor): (String, usize)) -> (String, usize) {
        let replaced = (
            std::mem::replace(&mut self.editor.document, document),
            self.editor.cursor,
        );
        self.editor.cursor = cursor;
        self.editor.selection = None;
        replaced
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

    /// Puts the current results in the separate native window (§32.3). One window is kept and
    /// refreshed in place; a run only opens another when the user has closed the last one, which
    /// `update` reports by failing.
    fn show_results_in_window(&mut self, cx: &mut Context<Self>) {
        let results = self.editor.results.clone();
        let display = self.editor.display;
        let refreshed = self.result_window.is_some_and(|handle| {
            handle
                .update(cx, |window, _, cx| {
                    window.results = results.clone();
                    window.display = display;
                    cx.notify();
                })
                .is_ok()
        });
        if refreshed {
            return;
        }
        let fonts = self.fonts.clone();
        let bounds = Bounds::centered(None, size(px(900.), px(600.)), cx);
        self.result_window = cx
            .open_window(
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
            )
            .ok();
    }

    fn show_editor_tab(&mut self, cx: &mut Context<Self>) {
        self.focus = Focus::Editor;
        cx.notify();
    }

    fn show_result_tab(&mut self, cx: &mut Context<Self>) {
        self.focus = Focus::Result;
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
        self.focus = Focus::Editor;
        self.status = "New SQL editor".into();
        cx.notify();
    }

    /// Closes a background editor by index. A running editor keeps its tab until its query
    /// settles, so a result never arrives for a document that is no longer there.
    fn close_editor(&mut self, index: usize, cx: &mut Context<Self>) {
        let running = self
            .background_editors
            .get(index)
            .is_some_and(|editor| is_busy(editor.execution_status));
        if running || index >= self.background_editors.len() {
            return;
        }
        let closed = self.background_editors.remove(index);
        self.status = format!("Closed {}", closed.title);
        cx.notify();
    }

    /// Closes the editor in front, promoting the most recent background editor in its place.
    /// The workspace always keeps one editor, so the last one stays.
    fn close_active_editor(&mut self, cx: &mut Context<Self>) {
        if is_busy(self.editor.execution_status) || self.background_editors.is_empty() {
            return;
        }
        let promoted = self
            .background_editors
            .pop()
            .expect("a background editor exists");
        let closed = std::mem::replace(&mut self.editor, promoted);
        self.focus = Focus::Editor;
        self.result_selection = None;
        self.status = format!("Closed {}", closed.title);
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
            self.focus = Focus::Editor;
            self.status = format!("Editor: {}", self.editor.connection_identity());
            cx.notify();
        }
    }

    /// Points the active editor at a connection (§49). Only this editor moves: the others keep
    /// their own targets, and no session is opened or closed by the choice itself.
    fn select_connection(&mut self, profile_id: uuid::Uuid, cx: &mut Context<Self>) {
        self.connection_menu = false;
        if self.is_running() {
            self.status = "Wait for the active query before changing connection".into();
            cx.notify();
            return;
        }
        if let Some(profile) = self
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
        {
            self.editor.connection = profile;
            self.status = format!("Target: {}", self.editor.connection_identity());
            cx.notify();
        }
    }

    /// Left-click points the active editor at a profile and connects it; Alt-click disconnects it
    /// (FR-004, FR-005). Connections are independent, so this never touches another one.
    fn handle_connection_click(
        &mut self,
        profile_id: uuid::Uuid,
        alt: bool,
        cx: &mut Context<Self>,
    ) {
        match self.state_of(profile_id) {
            _ if alt => match self.state_of(profile_id) {
                ConnectionState::Connected | ConnectionState::Failed => {
                    self.disconnect_profile(profile_id, cx)
                }
                _ => {
                    self.status = "Connection is already disconnected".into();
                    cx.notify();
                }
            },
            ConnectionState::Disconnected | ConnectionState::Failed => {
                if self.is_running() {
                    self.status = "Wait for the active query before changing connection".into();
                    cx.notify();
                    return;
                }
                self.select_connection(profile_id, cx);
                if let Some(profile) = self
                    .profiles
                    .iter()
                    .find(|profile| profile.id == profile_id)
                    .cloned()
                {
                    self.connect_profile(profile, cx);
                }
            }
            ConnectionState::Connected => {
                // Already live: the click just retargets the editor at it.
                self.select_connection(profile_id, cx);
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

    /// Opens the row-limit field for typing. The steps below only move by a factor of ten, so
    /// values such as 20 or 50 are reachable this way alone (§26).
    fn edit_limit(&mut self, cx: &mut Context<Self>) {
        self.limit_buffer = Some(String::new());
        self.status = format!("Row limit 1–{}; enter to apply", crate::MAX_ROW_LIMIT);
        cx.notify();
    }

    fn cancel_limit_edit(&mut self, cx: &mut Context<Self>) {
        self.limit_buffer = None;
        self.status = format!("Row limit {}", self.editor.row_limit);
        cx.notify();
    }

    /// Applies the typed digits. A value outside the permitted range leaves the field open with the
    /// text still in it, so a mistyped limit can be corrected rather than retyped.
    fn commit_limit_edit(&mut self, cx: &mut Context<Self>) {
        let typed = self.limit_buffer.clone().unwrap_or_default();
        match typed.parse::<u32>() {
            Ok(limit) if self.editor.set_row_limit(limit).is_ok() => {
                self.limit_buffer = None;
                self.status = format!("Row limit {limit}");
            }
            _ => {
                self.status = format!("Row limit must be between 1 and {}", crate::MAX_ROW_LIMIT);
            }
        }
        cx.notify();
    }

    fn connection_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tree = div().flex().flex_col().gap(px(3.));
        for (profile_index, profile) in self.profiles.iter().enumerate() {
            let profile_id = profile.id;
            // Each profile reports its own session, so several rows can be live at once (§48).
            let state = self.state_of(profile_id);
            let live = state == ConnectionState::Connected;
            // The row the active editor will execute against is the one worth pointing out.
            let targeted = profile_id == self.editor.connection.id;
            let indicator_colour = connection_indicator_colour(live || targeted, state);
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
                    .when(live || targeted, |row| row.bg(rgb(ACCENT_SOFT)))
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
            let session = self.sessions.get(&profile_id);
            if live && let Some(session) = session {
                if session.schemas.is_empty() {
                    tree = tree.child(self.tree_caption("No user schemas"));
                }
                for schema in &session.schemas {
                    let selected = session.expanded_schema.as_deref() == Some(schema);
                    let schema_for_click = schema.clone();
                    tree = tree.child(
                        div()
                            .id(SharedString::from(format!("schema-{profile_id}-{schema}")))
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
                                this.load_schema(profile_id, schema_for_click.clone(), false, cx)
                            }))
                            .child(
                                div()
                                    .text_color(if selected { rgb(ACCENT) } else { rgb(FAINT) })
                                    .child(if selected { "▾" } else { "▸" }),
                            )
                            .child(schema.clone()),
                    );
                    if selected {
                        if let Some(objects) = session.objects.get(schema) {
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
                                    let object_row_id = format!(
                                        "object-{profile_id}-{schema}-{kind}-{}",
                                        object.name
                                    );
                                    tree = tree.child(
                                        div()
                                            .id(SharedString::from(object_row_id.clone()))
                                            .debug_selector(move || object_row_id.clone())
                                            .pl(px(22.))
                                            .pr_3()
                                            .py(px(6.))
                                            .rounded_lg()
                                            .text_size(px(12.5))
                                            .font_family(self.fonts.mono.clone())
                                            .text_color(rgb(MUTED))
                                            // A double-click opens the definition, and a right
                                            // click opens the context menu (FR2-006, §8). Only the
                                            // active editor's own connection responds, because a
                                            // definition is fetched from that session and would
                                            // otherwise describe the wrong database.
                                            .when(profile_id == self.editor.connection.id, |row| {
                                                let double_click_target = object.clone();
                                                let right_click_target = object.clone();
                                                row.cursor_pointer()
                                                    .hover(|style| style.bg(rgb(PANEL)))
                                                    .on_click(cx.listener(
                                                        move |this, event: &gpui::ClickEvent, _, cx| {
                                                            if event.click_count() >= 2 {
                                                                this.object_menu = None;
                                                                this.open_definition(
                                                                    double_click_target.clone(),
                                                                    cx,
                                                                );
                                                            } else {
                                                                this.object_menu = None;
                                                                cx.notify();
                                                            }
                                                        },
                                                    ))
                                                    .on_mouse_down(
                                                        gpui::MouseButton::Right,
                                                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                                            // Anchors the menu at the row that was
                                                            // right-clicked, rather than a fixed
                                                            // point on the workspace.
                                                            this.object_menu_position = event.position;
                                                            this.toggle_object_menu(
                                                                right_click_target.clone(),
                                                                cx,
                                                            )
                                                        }),
                                                    )
                                            })
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
        let version = self.server_version();
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
        // Alt-clicking a tab closes it, matching the alt-click-to-disconnect gesture the
        // Connections pane already uses.
        for (index, editor) in self.background_editors.iter().enumerate() {
            tabs = tabs.child(segment(
                &self.fonts,
                SharedString::from(format!("editor-tab-{index}")),
                editor.title.clone(),
                false,
                true,
                cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    if event.modifiers().alt {
                        this.close_editor(index, cx);
                    } else {
                        this.switch_editor(index, cx);
                    }
                }),
            ));
        }
        tabs = tabs.child(segment(
            &self.fonts,
            "editor-tab",
            self.editor.title.clone(),
            self.focus == Focus::Editor,
            true,
            cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                if event.modifiers().alt {
                    this.close_active_editor(cx);
                } else {
                    this.show_editor_tab(cx);
                }
            }),
        ));
        if !self.editor.results.is_empty() {
            tabs = tabs.child(segment(
                &self.fonts,
                "result-tab",
                format!("Result: {}", self.editor.title),
                self.focus == Focus::Result,
                true,
                cx.listener(|this, _, _, cx| this.show_result_tab(cx)),
            ));
        }
        // A definition tab belongs to a connection, so it outlives the editors it sits beside
        // (§8). Alt-click closes it, like every other tab here.
        for tab in &self.definitions {
            let id = tab.id;
            tabs = tabs.child(segment(
                &self.fonts,
                SharedString::from(format!("definition-tab-{id}")),
                format!("{} [Definition]", tab.object.name),
                self.focus == Focus::Definition(id),
                true,
                cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    if event.modifiers().alt {
                        this.close_definition(id, cx);
                    } else {
                        this.focus = Focus::Definition(id);
                        cx.notify();
                    }
                }),
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

    /// The editor's connection, and the control that changes it (§49). It sits under the editor
    /// title where the identity was already displayed, so the target stays visible whether or not
    /// the list is open.
    fn connection_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.connection_state();
        div()
            .id("connection-selector")
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(13.))
            .text_color(rgb(MUTED))
            .cursor_pointer()
            .hover(|style| style.text_color(rgb(TEXT)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_connection_menu(cx)))
            .child(
                div()
                    .size(px(9.))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(connection_indicator_colour(true, state))),
            )
            .child(format!("Connection: {}", self.target_identity()))
            .child(div().text_color(rgb(FAINT)).child(if self.connection_menu {
                "▴"
            } else {
                "▾"
            }))
    }

    /// The list the selector opens: every profile, its state, and which one this editor is on.
    fn connection_menu_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut card = div()
            .absolute()
            .top(px(TITLEBAR_HEIGHT + 74.))
            .left(px(SIDEBAR_WIDTH + 30.))
            .w(px(360.))
            .p(px(8.))
            .rounded(px(CARD_RADIUS))
            .bg(rgb(PANEL))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(2.));
        for profile in &self.profiles {
            let profile_id = profile.id;
            let state = self.state_of(profile_id);
            let chosen = profile_id == self.editor.connection.id;
            card = card.child(
                div()
                    .id(SharedString::from(format!("choose-{profile_id}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py(px(10.))
                    .rounded(px(CONTROL_RADIUS))
                    .when(chosen, |row| row.bg(rgb(ACCENT_SOFT)))
                    .text_color(if chosen { rgb(ACCENT) } else { rgb(MUTED) })
                    .font_family(self.fonts.body.clone())
                    .text_size(px(13.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(PANEL_LIGHT)))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_connection(profile_id, cx)),
                    )
                    .child(div().size(px(9.)).flex_none().rounded_full().bg(rgb(
                        connection_indicator_colour(state == ConnectionState::Connected, state),
                    )))
                    .child(div().flex_1().child(profile.display_identity()))
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(self.fonts.mono.clone())
                            .text_color(rgb(FAINT))
                            .child(format!("{state:?}").to_uppercase()),
                    ),
            );
        }
        card.child(
            div()
                .px_3()
                .pt_2()
                .pb_1()
                .text_size(px(10.5))
                .font_family(self.fonts.mono.clone())
                .text_color(rgb(FAINT))
                .child("CHOOSING A CONNECTION MOVES THIS EDITOR ONLY"),
        )
    }

    /// §8 admits exactly one entry. Modification actions must never appear here.
    fn object_menu_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut card = div()
            .absolute()
            .top(self.object_menu_position.y)
            .left(self.object_menu_position.x)
            .flex()
            .flex_col()
            .p_1()
            .rounded(px(CONTROL_RADIUS))
            .bg(rgb(PANEL))
            .child(
                self.tree_caption(
                    self.object_menu
                        .as_ref()
                        .map_or_else(String::new, |menu| menu.object.name.clone()),
                ),
            );
        for entry in self
            .object_menu
            .as_ref()
            .map(ObjectMenu::entries)
            .unwrap_or_default()
        {
            card = card.child(
                div()
                    .id(SharedString::from(entry))
                    .px_3()
                    .py(px(7.))
                    .rounded_lg()
                    .text_size(px(12.5))
                    .font_family(self.fonts.display.clone())
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(PANEL_LIGHT)))
                    .on_click(cx.listener(|this, _, _, cx| this.choose_open_definition(cx)))
                    .child(entry),
            );
        }
        card
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

    /// A definition, read-only by construction: no caret, no key handling, no editing path
    /// (FR2-007). `Rows` blocks are already aligned by the model; `Sql` blocks go through the
    /// editor's own highlighter (§7). The header's Refresh and Copy controls are the only actions
    /// §9 permits against a definition.
    fn definition_surface(&self, tab: &DefinitionTab, cx: &mut Context<Self>) -> impl IntoElement {
        let id = tab.id;
        let loaded = matches!(tab.state, DefinitionState::Loaded(_));
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .pt_4()
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.))
                    .font_family(self.fonts.mono.clone())
                    .text_color(rgb(MUTED))
                    .child(format!("{}.{}", tab.object.schema, tab.object.name)),
            )
            .child(icon_button(
                "refresh-definition",
                "↻",
                true,
                false,
                cx.listener(move |this, _, _, cx| this.refresh_definition(id, cx)),
            ))
            .child(icon_button(
                "copy-definition",
                "⧉",
                loaded,
                false,
                cx.listener(move |this, _, _, cx| this.copy_definition(id, cx)),
            ));
        let mut body = div().flex().flex_col().gap_4().p_4().min_w_0();
        match &tab.state {
            DefinitionState::Loading => {
                body = body.child(self.tree_caption("Loading…"));
            }
            DefinitionState::Failed(error) => {
                body = body.child(
                    div()
                        .text_size(px(12.5))
                        .font_family(self.fonts.mono.clone())
                        .text_color(rgb(RED))
                        .child(error.message.clone()),
                );
                for extra in [error.detail.clone(), error.hint.clone()]
                    .into_iter()
                    .flatten()
                {
                    body = body.child(
                        div()
                            .text_size(px(12.))
                            .font_family(self.fonts.mono.clone())
                            .text_color(rgb(MUTED))
                            .child(extra),
                    );
                }
            }
            DefinitionState::Loaded(definition) => {
                for section in definition.sections(&tab.object) {
                    match section {
                        DefinitionSection::Rows { heading, lines } => {
                            body = body.child(self.tree_caption(heading));
                            for line in lines {
                                body = body.child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_family(self.fonts.mono.clone())
                                        .text_color(rgb(TEXT))
                                        .child(line),
                                );
                            }
                        }
                        DefinitionSection::Sql { heading, sql } => {
                            body = body.child(self.tree_caption(heading));
                            let spans = highlight_lines(&sql);
                            for (index, line) in sql.lines().enumerate() {
                                body = body.child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_family(self.fonts.mono.clone())
                                        .child(highlight_line(
                                            line,
                                            spans.get(index).map_or(&[][..], Vec::as_slice),
                                        )),
                                );
                            }
                        }
                        DefinitionSection::Note { text } => {
                            body = body.child(
                                div().text_size(px(12.5)).text_color(rgb(MUTED)).child(text),
                            );
                        }
                    }
                }
            }
        }
        div().flex().flex_col().min_w_0().child(header).child(body)
    }

    /// The SQL document, with the caret drawn where the cursor actually is and the selection
    /// painted behind the glyphs. GPUI paints neither of those itself, so both are placed
    /// arithmetically from the character advance — sound because the editor is monospace.
    ///
    /// The caret and selection are derived from the real document even while the placeholder is
    /// showing, so an empty editor still shows a caret at the point typing will start.
    fn editor_surface(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let document = &self.editor.document;
        let displayed = if document.is_empty() {
            "-- Write PostgreSQL here\nSELECT current_database();"
        } else {
            document
        };
        let caret = document_position(document, self.editor.cursor);
        let advance = self.editor_advance;
        // Only the lines the pane can see are built; the rest become the two spacers below, so a
        // large document costs a screenful of elements rather than one per line (§19).
        let handle = &self.editor_scroll.handle;
        // One pass for both: how many lines there are, and how wide the widest is. Only the
        // visible lines get measured by the layout engine, so the widest line has to be known
        // arithmetically or the horizontal extent would change as you scroll past it.
        let (total, widest) = editor_lines(displayed).fold((0, 0), |(count, widest), line| {
            (count + 1, widest.max(line.chars().count()))
        });
        // Colours are worked out for the whole document, not the visible window: a block comment or
        // a dollar-quoted body opened above the fold still governs the lines on screen.
        let highlights = highlight_lines(displayed);
        let visible = visible_lines(
            total,
            handle.bounds().size.height,
            handle.offset().y,
            EDITOR_OVERSCAN,
        );
        let mut lines = div()
            .flex()
            .flex_col()
            .items_start()
            // `min_w_full`, never `min_h_full`: a minimum height of one viewport is also a licence
            // for the flex row to shrink the lines to fit, which leaves a long document with
            // nothing to scroll. The results surface is sized the same way.
            .min_w_full()
            .font_family(self.fonts.mono.clone())
            .text_size(px(EDITOR_TEXT_SIZE))
            .line_height(px(EDITOR_LINE_HEIGHT))
            // A flat strut as wide as the longest line, holding the horizontal extent steady no
            // matter which lines the window happens to be showing.
            .child(
                div()
                    .flex_none()
                    .h(px(0.))
                    .w(px(GUTTER_WIDTH) + advance * widest as f32),
            )
            .child(line_spacer(visible.above));
        for (index, line) in editor_lines(displayed)
            .enumerate()
            .skip(visible.range.start)
            .take(visible.range.len())
        {
            let span = self
                .editor
                .selection
                .as_ref()
                .and_then(|selection| selected_columns(document, selection, index));
            lines = lines.child(
                div()
                    .id(SharedString::from(format!("editor-line-{index}")))
                    .flex()
                    .flex_row()
                    .relative()
                    // Fixed, not a minimum: the virtual window positions lines by multiplying
                    // this, so a line that can grow puts every line below it out of place.
                    .h(px(EDITOR_LINE_HEIGHT))
                    // Width is arithmetic for the same reason the caret and selection are: each
                    // coloured run is laid out separately and rounds up on its own, so a line built
                    // from several runs would otherwise measure wider than the strut below and the
                    // horizontal extent would change as you scrolled past it.
                    .w(px(GUTTER_WIDTH) + advance * line.chars().count() as f32)
                    .flex_none()
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.click_editor(index, event.position.x, event.modifiers.shift, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        this.drag_editor(index, event.position.x, cx);
                    }))
                    .child(
                        div()
                            .w(px(GUTTER_WIDTH))
                            .pr(px(18.))
                            .text_right()
                            .text_color(rgb(FAINT))
                            .child((index + 1).to_string()),
                    )
                    // Painted before the text, so it sits behind the glyphs.
                    .children(span.map(|span| {
                        div()
                            .absolute()
                            .top(px(0.))
                            .bottom(px(0.))
                            .left(px(GUTTER_WIDTH) + advance * span.start as f32)
                            .w(advance * span.len() as f32)
                            .bg(rgb(ACCENT_SOFT))
                    }))
                    .child(highlight_line(
                        line,
                        highlights.get(index).map_or(&[][..], Vec::as_slice),
                    ))
                    .children((caret.line == index).then(|| {
                        div()
                            .absolute()
                            .top(px(3.))
                            .bottom(px(3.))
                            .left(px(GUTTER_WIDTH) + advance * caret.column as f32)
                            .w(px(2.))
                            .bg(rgb(ACCENT))
                    })),
            );
        }
        lines.child(line_spacer(visible.below)).into_any_element()
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

    /// The grid, with every header and data row selectable. Only the rows the viewport can reach
    /// are built; `line_index` still advances over all of them, because selection and copy address
    /// rows by their position in the whole result, not by what happens to be on screen.
    fn selectable_table(
        &self,
        result: &QueryResult,
        line_index: &mut usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_header = usize::from(!result.columns.is_empty());
        let visible = self.visible_result_rows(
            *line_index,
            has_header + result.rows.len(),
            px(RESULT_ROW_HEIGHT),
        );
        let first = *line_index;
        let mut table = table_shell(&self.fonts).child(row_spacer(visible.above));
        if has_header == 1 && visible.range.contains(&0) {
            table =
                table.child(self.selectable_row(first, "result-header", header_row(result), cx));
        }
        for (offset, row) in result.rows.iter().enumerate() {
            if visible.range.contains(&(offset + has_header)) {
                table = table.child(self.selectable_row(
                    first + offset + has_header,
                    "result-row",
                    data_row(row),
                    cx,
                ));
            }
        }
        *line_index += has_header + result.rows.len();
        table.child(row_spacer(visible.below))
    }

    /// The window for one block of result rows, given how many rows precede it on the surface.
    fn visible_result_rows(&self, before: usize, count: usize, row_height: Pixels) -> VisibleLines {
        let handle = &self.results_scroll.handle;
        visible_rows(
            before,
            count,
            row_height,
            handle.bounds().size.height,
            handle.offset().y,
            RESULT_CHROME_SLOP_ROWS,
        )
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
                    let text = result.as_text();
                    let visible = self.visible_result_rows(
                        line_index,
                        text.lines().count(),
                        px(RESULT_LINE_HEIGHT),
                    );
                    let mut block = div()
                        .flex()
                        .flex_col()
                        .py_3()
                        .min_w_full()
                        .font_family(self.fonts.mono.clone())
                        .text_size(px(RESULT_TEXT_SIZE))
                        .line_height(px(RESULT_LINE_HEIGHT))
                        .child(text_spacer(visible.above));
                    for (offset, line) in text.lines().enumerate() {
                        if visible.range.contains(&offset) {
                            block = block.child(self.result_line(line_index + offset, line, cx));
                        }
                    }
                    line_index += text.lines().count();
                    content.child(block.child(text_spacer(visible.below)))
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

impl ResultWindow {
    fn set_display(&mut self, display: ResultDisplay, cx: &mut Context<Self>) {
        self.display = display;
        cx.notify();
    }

    /// The window for one block of rows, given how many rows precede it. The separate window shows
    /// the same result sets as the pane, so it has to window them the same way (§41).
    fn visible_result_rows(&self, before: usize, count: usize, row_height: Pixels) -> VisibleLines {
        let handle = &self.scroll.handle;
        visible_rows(
            before,
            count,
            row_height,
            handle.bounds().size.height,
            handle.offset().y,
            RESULT_CHROME_SLOP_ROWS,
        )
    }

    /// TABLE / TEXT, so the window is not stuck with whichever mode the editor was in when the
    /// query ran (§32.4).
    fn display_segments(&self, cx: &mut Context<Self>) -> impl IntoElement {
        segmented()
            .child(segment(
                &self.fonts,
                "window-display-table",
                "TABLE",
                self.display == ResultDisplay::Table,
                true,
                cx.listener(|this, _, _, cx| this.set_display(ResultDisplay::Table, cx)),
            ))
            .child(segment(
                &self.fonts,
                "window-display-text",
                "TEXT",
                self.display == ResultDisplay::Text,
                true,
                cx.listener(|this, _, _, cx| this.set_display(ResultDisplay::Text, cx)),
            ))
    }

    fn windowed_table(&self, result: &QueryResult, row_index: &mut usize) -> impl IntoElement {
        let has_header = usize::from(!result.columns.is_empty());
        let visible = self.visible_result_rows(
            *row_index,
            has_header + result.rows.len(),
            px(RESULT_ROW_HEIGHT),
        );
        let mut table = table_shell(&self.fonts).child(row_spacer(visible.above));
        if has_header == 1 && visible.range.contains(&0) {
            table = table.child(header_row(result));
        }
        for (offset, row) in result.rows.iter().enumerate() {
            if visible.range.contains(&(offset + has_header)) {
                table = table.child(data_row(row));
            }
        }
        *row_index += has_header + result.rows.len();
        table.child(row_spacer(visible.below))
    }

    fn windowed_text(&self, result: &QueryResult, row_index: &mut usize) -> impl IntoElement {
        let text = result.as_text();
        let total = text.lines().count();
        let visible = self.visible_result_rows(*row_index, total, px(RESULT_LINE_HEIGHT));
        let mut block = div()
            .flex()
            .flex_col()
            .px_5()
            .py_3()
            .min_w_full()
            .font_family(self.fonts.mono.clone())
            .text_size(px(RESULT_TEXT_SIZE))
            .line_height(px(RESULT_LINE_HEIGHT))
            .child(text_spacer(visible.above));
        for line in text
            .lines()
            .skip(visible.range.start)
            .take(visible.range.len())
        {
            block = block.child(
                div()
                    .flex_none()
                    .h(px(RESULT_LINE_HEIGHT))
                    .child(line.to_owned()),
            );
        }
        *row_index += total;
        block.child(text_spacer(visible.below))
    }
}

impl Render for ResultWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div().flex().flex_col().items_start().min_w_full().pt_4();
        let mut row_index = 0;
        for (index, result) in self.results.iter().enumerate() {
            content = content.child(result_header(&self.fonts, index, result));
            content = match self.display {
                ResultDisplay::Table => content.child(self.windowed_table(result, &mut row_index)),
                ResultDisplay::Text => content.child(self.windowed_text(result, &mut row_index)),
            };
        }
        div()
            .size_full()
            .flex()
            .flex_col()
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
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .px_5()
                    .py(px(12.))
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
                    .child(self.display_segments(cx)),
            )
            .child(div().flex_1().min_h_0().flex().child(scrollable(
                |view: &mut Self| &mut view.scroll,
                &self.scroll,
                "separate-results-scroll",
                content,
                cx,
            )))
    }
}

impl Focusable for AppView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.connection_state() == ConnectionState::Connected;
        let running = self.is_running();
        let editing_limit = self.limit_buffer.is_some();
        let pane_visible =
            self.editor.destination == ResultDestination::Pane && self.focus == Focus::Editor;
        div()
            .track_focus(&self.focus_handle)
            .key_context("SqlEditor")
            .on_key_down(cx.listener(|this, event, _, cx| this.handle_key(event, cx)))
            // A thumb drag continues wherever the pointer goes, so it is tracked at the root.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                let viewport_height = window.viewport_size().height;
                if this.editor_scroll.drag(event)
                    | this.results_scroll.drag(event)
                    | this.definition_scroll.drag(event)
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
                        | this.definition_scroll.drag.take().is_some()
                        | this.pane_drag.take().is_some()
                        | this.selecting_results
                        | this.selecting_editor;
                    this.selecting_results = false;
                    this.selecting_editor = false;
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
                                connection_indicator_colour(true, self.connection_state()),
                            )))
                            .child(format!("{:?}", self.connection_state()).to_uppercase()),
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
                                        !running,
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
                                            .child(self.connection_selector(cx)),
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
                                                    .id("limit-value")
                                                    .min_w(px(56.))
                                                    .text_center()
                                                    .text_size(px(15.))
                                                    .font_family(self.fonts.mono.clone())
                                                    .cursor_text()
                                                    .when(editing_limit, |field| {
                                                        field.text_color(rgb(ACCENT))
                                                    })
                                                    .hover(|style| style.text_color(rgb(ACCENT)))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.edit_limit(cx)
                                                    }))
                                                    .child(match self.limit_buffer.as_deref() {
                                                        // A caret stands in for an empty field so
                                                        // it does not read as a blank control.
                                                        Some(typed) => format!("{typed}▏"),
                                                        None => self.editor.row_limit.to_string(),
                                                    }),
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
                                    // The result and definition tabs share this surface with the
                                    // editor, so it scrolls against whichever handle is showing.
                                    .child(match self.focus {
                                        Focus::Result => scrollable(
                                            |view: &mut Self| &mut view.results_scroll,
                                            &self.results_scroll,
                                            "editor-scroll",
                                            self.results_surface(cx),
                                            cx,
                                        )
                                        .into_any_element(),
                                        Focus::Definition(id) => {
                                            let surface = self
                                                .definitions
                                                .iter()
                                                .find(|tab| tab.id == id)
                                                .map(|tab| self.definition_surface(tab, cx));
                                            scrollable(
                                                |view: &mut Self| &mut view.definition_scroll,
                                                &self.definition_scroll,
                                                "definition-scroll",
                                                div().children(surface),
                                                cx,
                                            )
                                            .into_any_element()
                                        }
                                        Focus::Editor => scrollable(
                                            |view: &mut Self| &mut view.editor_scroll,
                                            &self.editor_scroll,
                                            "editor-scroll",
                                            self.editor_surface(cx),
                                            cx,
                                        )
                                        .into_any_element(),
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
                        connection_indicator_colour(true, self.connection_state()),
                    )))
                    .child(self.status.to_uppercase())
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(rgb(FAINT))
                            .child(self.server_version().to_uppercase()),
                    ),
            )
            .when(self.connection_dialog, |root| {
                root.child(self.connection_dialog_card())
            })
            .when(self.connection_menu, |root| {
                root.child(self.connection_menu_card(cx))
            })
            .when(self.object_menu.is_some(), |root| {
                root.child(self.object_menu_card(cx))
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

/// Paints one line from the spans [`highlight_lines`] worked out for it. The view classifies
/// nothing itself: reading SQL is the parser's job, and a second reading here would disagree with
/// it (59.3).
fn highlight_line(line: &str, spans: &[HighlightSpan]) -> impl IntoElement {
    let mut row = div().flex().flex_row().whitespace_nowrap();
    for span in spans {
        let colour = match span.highlight {
            Highlight::Keyword => ACCENT,
            Highlight::Literal => STRING,
            Highlight::Comment => FAINT,
            Highlight::Function => FUNCTION,
            Highlight::Plain => TEXT,
        };
        row = row.child(
            div()
                .text_color(rgb(colour))
                .child(line[span.range.clone()].to_owned()),
        );
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
        // Notices are shown in full by the text view; the header only has to say one arrived, so a
        // long one cannot push the metrics off the row (§40).
        .children(notice_summary(&result.notices).map(|summary| metric_pill(fonts, summary, WARN)))
}

/// What the result header says about the notices a statement raised: the notice itself when there
/// is one, a count when there are several.
fn notice_summary(notices: &[String]) -> Option<String> {
    match notices {
        [] => None,
        [only] => Some(truncated(only, 72)),
        many => Some(format!("{} notices", many.len())),
    }
}

fn truncated(text: &str, characters: usize) -> String {
    match text.char_indices().nth(characters) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_owned(),
    }
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
        .h(px(RESULT_ROW_HEIGHT))
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
        // Fixed rather than padding-derived, so the windowed grid places rows exactly.
        .h(px(RESULT_ROW_HEIGHT))
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
            "w" if running => None,
            "w" => Some(command::CLOSE_EDITOR),
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

/// A caret position in the SQL document: which line, and how many characters into it.
///
/// Columns are characters rather than bytes because they address rendered glyphs — the editor is
/// monospace, so a column is also what turns a pointer position into an offset.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct DocumentPosition {
    line: usize,
    column: usize,
}

/// The line and column a byte offset falls on.
fn document_position(document: &str, offset: usize) -> DocumentPosition {
    let preceding = &document[..offset.min(document.len())];
    let line_start = preceding.rfind('\n').map_or(0, |index| index + 1);
    DocumentPosition {
        line: preceding.matches('\n').count(),
        column: preceding[line_start..].chars().count(),
    }
}

/// The columns of `line` a byte-offset selection covers, or `None` where there is nothing to paint.
fn selected_columns(document: &str, selection: &Range<usize>, line: usize) -> Option<Range<usize>> {
    if selection.is_empty() {
        return None;
    }
    let start = document_position(document, selection.start);
    let end = document_position(document, selection.end);
    if line < start.line || line > end.line {
        return None;
    }
    let from = if line == start.line { start.column } else { 0 };
    let to = if line == end.line {
        end.column
    } else {
        line_length(document, line)
    };
    (from < to).then_some(from..to)
}

/// Whether an editor has a query in flight, and so must not be closed or swapped out from under it.
fn is_busy(status: ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Running | ExecutionStatus::Cancelling
    )
}

/// Stands in for lines outside the window, holding the scroll height the document really needs so
/// the scrollbar and the pointer arithmetic both stay honest.
fn line_spacer(lines: usize) -> gpui::Div {
    div().flex_none().h(px(EDITOR_LINE_HEIGHT) * lines as f32)
}

/// The same, for the rows of a results grid.
fn row_spacer(rows: usize) -> gpui::Div {
    div().flex_none().h(px(RESULT_ROW_HEIGHT) * rows as f32)
}

/// The same, for the lines of a text result.
fn text_spacer(lines: usize) -> gpui::Div {
    div().flex_none().h(px(RESULT_LINE_HEIGHT) * lines as f32)
}

/// The lines the editor should actually build elements for, and how many it is skipping at each
/// end. The skipped lines become spacers, so the scroll height still covers the whole document.
#[derive(Clone, PartialEq, Eq, Debug)]
struct VisibleLines {
    range: Range<usize>,
    above: usize,
    below: usize,
}

/// Which lines a viewport can see. Editor lines are a fixed height, so this is exact rather than a
/// measurement, and `overscan` keeps a few lines built beyond each edge so a scroll does not
/// expose an unrendered gap before the next frame.
fn visible_lines(
    total: usize,
    viewport_height: Pixels,
    offset_y: Pixels,
    overscan: usize,
) -> VisibleLines {
    visible_rows(
        0,
        total,
        px(EDITOR_LINE_HEIGHT),
        viewport_height,
        offset_y,
        overscan,
    )
}

/// The same window for one block of uniform rows sitting `before` rows into a taller surface.
/// Results stack a header above each block whose height belongs to the stylesheet rather than to
/// this arithmetic, so `slop` widens the window enough to cover it; overshooting costs a handful
/// of extra rows, while undershooting would leave a blank band at the edge of the viewport.
fn visible_rows(
    before: usize,
    count: usize,
    row_height: Pixels,
    viewport_height: Pixels,
    offset_y: Pixels,
    slop: usize,
) -> VisibleLines {
    // Offsets run negative as the content moves up past the viewport.
    let scrolled = (f32::from(-offset_y).max(0.) / f32::from(row_height)) as usize;
    // Before the first layout pass the viewport measures zero. Rendering nothing then would leave
    // the surface blank for a frame, so an unmeasured viewport falls back to a screenful.
    let deep = if viewport_height > px(0.) {
        (f32::from(viewport_height) / f32::from(row_height)).ceil() as usize + 1
    } else {
        UNMEASURED_VIEWPORT_LINES
    };
    let first = scrolled.saturating_sub(slop);
    let last = scrolled.saturating_add(deep + slop);
    // Translate the global window into this block's own rows.
    let start = first.saturating_sub(before).min(count);
    let end = last.saturating_sub(before).min(count);
    VisibleLines {
        above: start,
        below: count - end,
        range: start..end,
    }
}

/// The document as the editor renders it. Unlike `str::lines` this keeps the empty line after a
/// trailing newline, which is a line the caret can occupy and so a line that must be drawn.
fn editor_lines(document: &str) -> std::str::Split<'_, char> {
    document.split('\n')
}

fn line_length(document: &str, line: usize) -> usize {
    editor_lines(document)
        .nth(line)
        .map_or(0, |line| line.chars().count())
}

/// The byte offset of a line and column, each clamped to what the document actually has.
fn offset_of(document: &str, line: usize, column: usize) -> usize {
    let mut line_start = 0;
    for _ in 0..line {
        match document[line_start..].find('\n') {
            Some(index) => line_start += index + 1,
            // Past the last line, which is where End and a click below the text both land.
            None => break,
        }
    }
    let line_end = document[line_start..]
        .find('\n')
        .map_or(document.len(), |index| line_start + index);
    document[line_start..line_end]
        .char_indices()
        .nth(column)
        .map_or(line_end, |(index, _)| line_start + index)
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    use super::*;
    use async_trait::async_trait;
    use gpui::{Modifiers, MouseUpEvent, TestAppContext};

    use crate::database::{ConnectionInfo, DatabaseProvider};
    use crate::definition::{ColumnDefinition, TableDefinition};

    #[derive(Default)]
    struct UiTestProvider {
        state: AtomicU8,
        connect_calls: AtomicUsize,
        disconnect_calls: AtomicUsize,
        /// Holds `execute` open so a test can act while a query is genuinely in flight.
        blocked: AtomicBool,
        /// Fails the next definition request, so a test can assert the failure lands in the tab
        /// and nowhere else.
        definition_fails: AtomicBool,
        definition_calls: AtomicUsize,
        /// Schemas and objects handed back by `schemas`/`objects`, so a test can populate the tree
        /// and drive a real click on a rendered object row.
        schemas_response: Mutex<Vec<String>>,
        objects_response: Mutex<HashMap<String, Vec<DatabaseObject>>>,
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
            Ok(self.schemas_response.lock().unwrap().clone())
        }

        async fn objects(&self, schema: &str, _: bool) -> Result<Vec<DatabaseObject>, QueryError> {
            Ok(self
                .objects_response
                .lock()
                .unwrap()
                .get(schema)
                .cloned()
                .unwrap_or_default())
        }

        async fn definition(
            &self,
            object: &DatabaseObject,
            _refresh: bool,
        ) -> Result<ObjectDefinition, QueryError> {
            self.definition_calls.fetch_add(1, Ordering::SeqCst);
            if self.definition_fails.load(Ordering::SeqCst) {
                return Err(QueryError {
                    message: "permission denied for table customer".into(),
                    severity: None,
                    code: Some("42501".into()),
                    detail: None,
                    hint: None,
                    position: None,
                });
            }
            Ok(ObjectDefinition::Table(TableDefinition {
                columns: vec![ColumnDefinition {
                    position: 1,
                    name: format!("{}_id", object.name),
                    data_type: "bigint".into(),
                    nullable: false,
                    default: None,
                }],
                ..TableDefinition::default()
            }))
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

    /// Hands every session the same fake provider, which is how a test stands in for PostgreSQL
    /// now that sessions build their own.
    fn provider_factory(
        provider: Arc<UiTestProvider>,
    ) -> Arc<dyn Fn() -> Arc<dyn DatabaseProvider>> {
        Arc::new(move || provider.clone() as Arc<dyn DatabaseProvider>)
    }

    /// Hands each session a provider of its own, in the order the sessions are created, so a test
    /// can tell which connection a request actually reached.
    fn provider_factory_per_session(
        providers: Vec<Arc<UiTestProvider>>,
    ) -> Arc<dyn Fn() -> Arc<dyn DatabaseProvider>> {
        let next = Arc::new(AtomicUsize::new(0));
        Arc::new(move || {
            let index = next.fetch_add(1, Ordering::SeqCst);
            providers
                .get(index)
                .cloned()
                .expect("a test should supply one provider per session it opens")
                as Arc<dyn DatabaseProvider>
        })
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
            if view.update(cx, |app, _| app.connection_state()) == expected {
                return;
            }
            std::thread::yield_now();
        }
        view.update(cx, |app, _| assert_eq!(app.connection_state(), expected));
    }

    fn customer() -> DatabaseObject {
        DatabaseObject {
            schema: "public".into(),
            name: "customer".into(),
            kind: ObjectKind::Table,
        }
    }

    /// Waits for the expected number of definition tabs, and for every one of them to have
    /// settled. A tab appears synchronously, so waiting on the count alone would race the load.
    fn wait_for_definitions(
        view: &gpui::Entity<AppView>,
        cx: &mut gpui::VisualTestContext,
        expected: usize,
    ) {
        for _ in 0..1_000 {
            cx.run_until_parked();
            let settled = view.update(cx, |app, _| {
                app.definitions.len() == expected
                    && !app
                        .definitions
                        .iter()
                        .any(|tab| matches!(tab.state, DefinitionState::Loading))
            });
            if settled {
                return;
            }
            std::thread::yield_now();
        }
        view.update(cx, |app, _| assert_eq!(app.definitions.len(), expected));
        panic!("definitions were still loading after {expected} tabs appeared");
    }

    /// FR2-006, §8: the definition arrives beside the editor, which keeps its contents.
    #[gpui::test]
    fn opening_a_definition_leaves_the_editor_untouched(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
            app.editor.document = "SELECT 1;".into();
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| {
            assert_eq!(app.editor.document, "SELECT 1;");
            assert_eq!(app.definitions[0].object.name, "customer");
            assert!(matches!(
                app.definitions[0].state,
                DefinitionState::Loaded(_)
            ));
            assert!(matches!(app.focus, Focus::Definition(_)));
        });
    }

    /// §16: the same object focuses the tab it already has rather than opening a second one.
    #[gpui::test]
    fn opening_the_same_object_twice_focuses_the_existing_tab(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);
        view.update(cx, |app, cx| {
            app.focus = Focus::Editor;
            app.open_definition(customer(), cx);
        });
        cx.run_until_parked();

        view.update(cx, |app, _| {
            assert_eq!(app.definitions.len(), 1);
            assert!(matches!(app.focus, Focus::Definition(_)));
        });
        assert_eq!(provider.definition_calls.load(Ordering::SeqCst), 1);
    }

    /// FR2-010, §12: a failure is confined to its own tab.
    #[gpui::test]
    fn a_definition_failure_stays_inside_its_tab(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        provider.definition_fails.store(true, Ordering::SeqCst);
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
            app.editor.document = "SELECT 1;".into();
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| {
            let DefinitionState::Failed(error) = &app.definitions[0].state else {
                panic!("expected a failed definition");
            };
            assert_eq!(error.message, "permission denied for table customer");
            assert_eq!(app.editor.document, "SELECT 1;");
            assert_eq!(app.connection_state(), ConnectionState::Connected);
        });
    }

    /// §8: a definition belongs to a connection, so editors may come and go beneath it.
    #[gpui::test]
    fn a_definition_survives_opening_another_editor(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);
        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, cx| app.dispatch_command(command::NEW_EDITOR, cx));

        view.update(cx, |app, _| assert_eq!(app.definitions.len(), 1));
    }

    /// FR2-012, §8: the menu offers Open Definition and nothing that modifies the database.
    #[gpui::test]
    fn the_object_menu_offers_only_open_definition(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.toggle_object_menu(customer(), cx));

        view.update(cx, |app, _| {
            assert_eq!(
                app.object_menu.as_ref().map(|menu| menu.entries()),
                Some(vec!["Open Definition"])
            );
        });

        view.update(cx, |app, cx| {
            let object = app.object_menu.as_ref().unwrap().object.clone();
            app.open_definition(object, cx);
        });
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| {
            assert_eq!(app.definitions[0].object.name, "customer")
        });
    }

    /// The menu is dismissed by choosing from it, so it cannot linger over the tree.
    #[gpui::test]
    fn choosing_from_the_object_menu_closes_it(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);
        view.update(cx, |app, cx| app.toggle_object_menu(customer(), cx));

        view.update(cx, |app, cx| app.choose_open_definition(cx));
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| assert!(app.object_menu.is_none()));
    }

    /// The handlers above are reached through the rendered tree row, not called directly, so this
    /// would catch a regression that loosens the `>= 2` double-click threshold back to a single
    /// click (§8, FR2-006) — the exact behaviour Task 9's stopgap handler had, and Task 10 exists
    /// to remove.
    #[gpui::test]
    fn a_single_click_on_a_tree_object_leaves_it_unopened_and_a_double_click_opens_it(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(UiTestProvider::default());
        provider
            .schemas_response
            .lock()
            .unwrap()
            .push("public".into());
        provider
            .objects_response
            .lock()
            .unwrap()
            .insert("public".into(), vec![customer()]);
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        let profile_id = view.update(cx, |app, _| app.editor.connection.id);
        view.update(cx, |app, cx| {
            app.load_schema(profile_id, "public".into(), false, cx)
        });
        for _ in 0..1_000 {
            cx.run_until_parked();
            let loaded = view.update(cx, |app, _| {
                app.session_for(profile_id).objects.contains_key("public")
            });
            if loaded {
                break;
            }
            std::thread::yield_now();
        }

        // `debug_bounds` wants a `'static` selector; the row id is only known once the profile
        // connects, so it is leaked for the lifetime of the test process — a standard trick for
        // driving real, rendered UI in a test rather than calling the handler directly.
        let row_id: &'static str =
            Box::leak(format!("object-{profile_id}-public-Tables-customer").into_boxed_str());
        let row = cx
            .debug_bounds(row_id)
            .expect("the object row should be rendered once its schema is expanded");

        // A single click must not open the definition (§8): it only clears any open menu.
        cx.simulate_click(row.center(), Modifiers::default());
        cx.run_until_parked();
        view.update(cx, |app, _| {
            assert!(
                app.definitions.is_empty(),
                "a single click must not open a definition"
            );
        });

        // A double click opens exactly one tab. `simulate_click` always stamps `click_count: 1`,
        // so the down/up pair is built by hand with `click_count: 2` — the field `ClickEvent`
        // reads to decide whether this was a double-click — and driven through the same
        // `simulate_event` primitive `simulate_click` itself uses underneath.
        cx.simulate_event(MouseDownEvent {
            position: row.center(),
            click_count: 2,
            ..Default::default()
        });
        cx.simulate_event(MouseUpEvent {
            position: row.center(),
            click_count: 2,
            ..Default::default()
        });
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| {
            assert_eq!(app.definitions[0].object.name, "customer");
        });
    }

    /// Mirrors the connection menu's own escape handling (`src/ui.rs` around `handle_key`): a
    /// stray context menu must not survive the key the user pressed to get out of it.
    #[gpui::test]
    fn escape_closes_the_object_menu(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| app.toggle_object_menu(customer(), cx));
        view.update(cx, |app, _| assert!(app.object_menu.is_some()));

        view.update(cx, |app, cx| app.handle_key(&key_event("escape"), cx));

        view.update(cx, |app, _| assert!(app.object_menu.is_none()));
    }

    /// Mirrors the connection menu's own click-away handling in `click_editor`: clicking into the
    /// document is the other way out of a menu left open over the tree.
    #[gpui::test]
    fn clicking_the_editor_closes_the_object_menu(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| app.toggle_object_menu(customer(), cx));

        view.update(cx, |app, cx| app.click_editor(0, px(0.), false, cx));

        view.update(cx, |app, _| assert!(app.object_menu.is_none()));
    }

    /// FR2-009, §8: a refresh goes back to the connection the tab was opened against. The editor
    /// in front can be switched to another database in between, and a refresh that followed it
    /// would repopulate the tab from the wrong server.
    #[gpui::test]
    fn refreshing_a_definition_goes_back_to_its_own_connection(cx: &mut TestAppContext) {
        let first = Arc::new(UiTestProvider::default());
        let second = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        let local = profile_named("Local", "local");
        let production = profile_named("Production", "production");
        view.update(cx, |app, cx| {
            app.provider_factory =
                provider_factory_per_session(vec![first.clone(), second.clone()]);
            app.profiles = vec![local.clone(), production.clone()];
            app.editor.connection = local.clone();
            app.connect(cx);
        });
        wait_for_profile_state(&view, cx, local.id, ConnectionState::Connected);

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);
        let tab = view.update(cx, |app, _| {
            assert_eq!(app.definitions[0].profile_id, local.id);
            app.definitions[0].id
        });

        // The editor moves to the other connection; the definition tab stays where it was.
        view.update(cx, |app, cx| {
            app.select_connection(production.id, cx);
            app.connect(cx);
        });
        wait_for_profile_state(&view, cx, production.id, ConnectionState::Connected);

        view.update(cx, |app, cx| app.refresh_definition(tab, cx));
        wait_for_definitions(&view, cx, 1);

        assert_eq!(
            first.definition_calls.load(Ordering::SeqCst),
            2,
            "the refresh should have gone back to the connection the tab was opened against"
        );
        assert_eq!(
            second.definition_calls.load(Ordering::SeqCst),
            0,
            "the connection the editor was switched to should never have been asked"
        );
        view.update(cx, |app, _| {
            assert_eq!(app.definitions[0].profile_id, local.id);
            assert!(matches!(
                app.definitions[0].state,
                DefinitionState::Loaded(_)
            ));
        });
    }

    /// FR2-009: refresh asks PostgreSQL again rather than replaying the cache.
    #[gpui::test]
    fn refreshing_a_definition_requests_it_again(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);
        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        let id = view.update(cx, |app, _| app.definitions[0].id);
        view.update(cx, |app, cx| app.refresh_definition(id, cx));
        // The database round trip runs on the real `tokio::runtime::Runtime` `AppView` owns, off
        // the deterministic test executor, so settling needs the same poll-and-yield loop the
        // initial load waits on rather than a single `run_until_parked`.
        wait_for_definitions(&view, cx, 1);

        assert_eq!(provider.definition_calls.load(Ordering::SeqCst), 2);
        view.update(cx, |app, _| assert_eq!(app.definitions.len(), 1));
    }

    /// §11: a refresh that fails replaces the body, so nothing stale is shown as current.
    #[gpui::test]
    fn a_failed_refresh_replaces_the_definition_rather_than_keeping_it(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);
        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        provider.definition_fails.store(true, Ordering::SeqCst);
        let id = view.update(cx, |app, _| app.definitions[0].id);
        view.update(cx, |app, cx| app.refresh_definition(id, cx));
        // See the comment in `refreshing_a_definition_requests_it_again`: the refresh settles on
        // the real runtime, not the test executor, so it needs the poll-and-yield loop.
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| {
            assert!(matches!(
                app.definitions[0].state,
                DefinitionState::Failed(_)
            ));
        });
    }

    /// §9: copying is permitted — the whole tab, in section order.
    #[gpui::test]
    fn copying_a_definition_yields_its_whole_text(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);
        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        let text = view.update(cx, |app, _| app.definition_text(app.definitions[0].id));

        assert_eq!(
            text.as_deref(),
            Some("Columns\ncustomer_id  bigint  NOT NULL")
        );
    }

    /// The Task 9 review ruling: opening a definition must clear any stale result selection, and
    /// Ctrl+C under a definition tab must copy that definition rather than result-grid lines that
    /// are visible nowhere on screen.
    #[gpui::test]
    fn ctrl_c_under_a_definition_tab_copies_the_definition_not_stale_result_lines(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
            app.editor.display = ResultDisplay::Text;
            app.editor.results = vec![result_with_rows(&["alpha"])];
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        // Stands in for whatever selection the result surface was left in before a definition tab
        // was opened over it.
        view.update(cx, |app, cx| app.select_all_results(cx));
        assert!(view.update(cx, |app, _| app.result_selection.is_some()));

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);
        assert!(view.update(cx, |app, _| app.result_selection.is_none()));

        view.update(cx, |app, cx| app.handle_key(&command_key("c"), cx));

        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            clipboard.as_deref(),
            Some("Columns\ncustomer_id  bigint  NOT NULL")
        );
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
            app.focus = Focus::Editor;
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
            app.focus = Focus::Editor;
            assert!(!app.results_have_focus());
            app.handle_key(&command_key("a"), cx);
            assert_eq!(app.editor.selection, Some(0..app.editor.document.len()));
        });

        // Result tab in front: select-all targets the result lines instead.
        view.update(cx, |app, cx| {
            app.focus = Focus::Result;
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
            app.provider_factory = provider_factory(provider.clone());
        });

        let connection_row = cx
            .debug_bounds("connection-row-0")
            .expect("the first connection row should be rendered");
        cx.simulate_click(connection_row.center(), Modifiers::default());
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        assert_eq!(provider.connect_calls.load(Ordering::SeqCst), 1);
        view.update(cx, |app, _| {
            assert_eq!(
                connection_indicator_colour(true, app.connection_state()),
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

    /// An editor must name the database its own session opened, or a tab can claim to target one
    /// database while its SQL reaches another. Editors on a different connection keep theirs
    /// (§49, §50).
    #[gpui::test]
    fn an_editor_reports_the_database_its_own_session_opened(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let mut production = profile_named("Production", "production");
        let local = profile_named("Local", "local");

        view.update(cx, |app, _| {
            app.profiles.push(production.clone());
            app.profiles.push(local.clone());
            app.background_editors.push(EditorState::new(local.clone()));
            app.editor.connection = production.clone();

            // The session reports the database it really opened, which need not be the one the
            // profile named.
            production.configuration.database = "production_replica".into();
            app.adopt_connected_profile(production.clone());
        });

        view.update(cx, |app, _| {
            assert_eq!(app.target_identity(), "Production / production_replica");
            assert_eq!(
                app.background_editors[0].connection.configuration.database, "local",
                "the editor on the other connection should be untouched"
            );
        });

        // Switching to the other tab surfaces that tab's own target.
        view.update(cx, |app, cx| app.switch_editor(0, cx));
        view.update(cx, |app, _| {
            assert_eq!(app.target_identity(), "Local / local");
            assert_eq!(app.editor.connection.id, local.id);
        });
    }

    fn wait_for_profile_state(
        view: &gpui::Entity<AppView>,
        cx: &mut gpui::VisualTestContext,
        profile_id: uuid::Uuid,
        expected: ConnectionState,
    ) {
        for _ in 0..1_000 {
            cx.run_until_parked();
            if view.update(cx, |app, _| app.state_of(profile_id)) == expected {
                return;
            }
            std::thread::yield_now();
        }
        view.update(cx, |app, _| assert_eq!(app.state_of(profile_id), expected));
    }

    /// Two connections, two editors, both live at once — the point of a per-editor selector
    /// (§48, §49).
    #[gpui::test]
    fn two_editors_can_target_two_connections_at_once(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        let local = profile_named("Local", "local");
        let production = profile_named("Production", "production");
        view.update(cx, |app, cx| {
            app.provider_factory = provider_factory(provider.clone());
            app.profiles = vec![local.clone(), production.clone()];
            app.editor.connection = local.clone();
            app.connect(cx);
        });
        wait_for_profile_state(&view, cx, local.id, ConnectionState::Connected);

        view.update(cx, |app, cx| {
            app.new_editor(cx);
            app.select_connection(production.id, cx);
            app.connect(cx);
        });
        wait_for_profile_state(&view, cx, production.id, ConnectionState::Connected);

        view.update(cx, |app, _| {
            assert_eq!(app.state_of(local.id), ConnectionState::Connected);
            assert_eq!(app.target_identity(), "Production / production");
            assert_eq!(
                app.background_editors[0].connection.id, local.id,
                "the first editor should still be pointed at its own connection"
            );
        });
        assert_eq!(provider.connect_calls.load(Ordering::SeqCst), 2);
    }

    /// Disconnecting is per connection too: the other session stays live, and only the closed one
    /// forgets its metadata.
    #[gpui::test]
    fn disconnecting_one_connection_leaves_the_other_live(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        let local = profile_named("Local", "local");
        let production = profile_named("Production", "production");
        view.update(cx, |app, cx| {
            app.provider_factory = provider_factory(provider.clone());
            app.profiles = vec![local.clone(), production.clone()];
            app.editor.connection = local.clone();
            app.connect(cx);
            app.connect_profile(production.clone(), cx);
        });
        wait_for_profile_state(&view, cx, local.id, ConnectionState::Connected);
        wait_for_profile_state(&view, cx, production.id, ConnectionState::Connected);
        view.update(cx, |app, _| {
            app.session_for(local.id).schemas = vec!["public".into()];
            app.session_for(production.id).schemas = vec!["audit".into()];
        });

        view.update(cx, |app, cx| app.disconnect_profile(local.id, cx));
        wait_for_profile_state(&view, cx, local.id, ConnectionState::Disconnected);

        view.update(cx, |app, _| {
            assert_eq!(app.state_of(production.id), ConnectionState::Connected);
            assert!(app.session_for(local.id).schemas.is_empty());
            assert_eq!(app.session_for(production.id).schemas, ["audit"]);
        });
    }

    /// Choosing from the selector moves this editor and closes the list. It opens no session of
    /// its own: the connection is a target until the user connects it.
    #[gpui::test]
    fn the_selector_moves_only_the_editor_in_front(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let local = profile_named("Local", "local");
        let production = profile_named("Production", "production");

        view.update(cx, |app, cx| {
            app.profiles = vec![local.clone(), production.clone()];
            app.editor.connection = local.clone();
            app.new_editor(cx);
            app.toggle_connection_menu(cx);
            app.select_connection(production.id, cx);
        });

        view.update(cx, |app, cx| {
            // Clicking into the document is the other way out of the list.
            app.toggle_connection_menu(cx);
            app.click_editor(0, px(0.), false, cx);
            assert!(!app.connection_menu, "clicking away should close the list");
        });

        view.update(cx, |app, _| {
            assert!(!app.connection_menu, "choosing should close the list");
            assert_eq!(app.editor.connection.id, production.id);
            assert_eq!(app.background_editors[0].connection.id, local.id);
            assert_eq!(app.state_of(production.id), ConnectionState::Disconnected);
            assert_eq!(app.status, "Target: Production / production");
        });
    }

    /// Retargeting an editor mid-query would send the result somewhere else, so it waits.
    #[gpui::test]
    fn an_editor_cannot_be_retargeted_while_its_query_runs(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let local = profile_named("Local", "local");
        let production = profile_named("Production", "production");

        view.update(cx, |app, cx| {
            app.profiles = vec![local.clone(), production.clone()];
            app.editor.connection = local.clone();
            app.editor.execution_status = ExecutionStatus::Running;
            app.select_connection(production.id, cx);
        });

        view.update(cx, |app, _| {
            assert_eq!(app.editor.connection.id, local.id);
            assert_eq!(
                app.status,
                "Wait for the active query before changing connection"
            );
        });
    }

    /// A completing query writes back only its outcome, so text typed while it ran survives. The
    /// query is held open by the provider so the typing genuinely races the in-flight run.
    #[gpui::test]
    fn text_typed_while_a_query_runs_is_not_discarded(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
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

    /// `SELECT one\nFROM t\nWHERE x` — a short middle line so clamping is visible.
    fn three_line_editor(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<AppView>, &mut gpui::VisualTestContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.document = "SELECT one\nFROM t\nWHERE x".into();
            app.editor.cursor = 0;
        });
        (view, cx)
    }

    #[gpui::test]
    fn down_holds_the_column_on_the_line_below(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        view.update(cx, |app, _| app.editor.cursor = 4);

        cx.simulate_keystrokes("down");

        // Column 4 of "FROM t" is the offset of its space.
        view.update(cx, |app, _| assert_eq!(app.editor.cursor, 15));
    }

    #[gpui::test]
    fn down_onto_a_shorter_line_stops_at_its_end(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        view.update(cx, |app, _| app.editor.cursor = 9);

        cx.simulate_keystrokes("down");

        // "FROM t" has no column 9, so the caret lands on its end rather than overshooting.
        view.update(cx, |app, _| assert_eq!(app.editor.cursor, 17));
    }

    #[gpui::test]
    fn up_returns_to_the_line_above(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        view.update(cx, |app, _| app.editor.cursor = 20);

        cx.simulate_keystrokes("up");

        view.update(cx, |app, _| assert_eq!(app.editor.cursor, 13));
    }

    #[gpui::test]
    fn up_on_the_first_line_keeps_the_caret_where_it_is(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        view.update(cx, |app, _| app.editor.cursor = 4);

        cx.simulate_keystrokes("up");

        view.update(cx, |app, _| assert_eq!(app.editor.cursor, 4));
    }

    #[gpui::test]
    fn home_and_end_travel_to_the_bounds_of_the_current_line(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        view.update(cx, |app, _| app.editor.cursor = 14);

        cx.simulate_keystrokes("home");
        view.update(cx, |app, _| assert_eq!(app.editor.cursor, 11));

        cx.simulate_keystrokes("end");
        view.update(cx, |app, _| assert_eq!(app.editor.cursor, 17));
    }

    #[gpui::test]
    fn shift_down_selects_through_to_the_line_below(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        view.update(cx, |app, _| app.editor.cursor = 4);

        cx.simulate_keystrokes("shift-down");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.selection, Some(4..15));
            assert_eq!(app.editor.cursor, 15);
        });
    }

    #[gpui::test]
    fn vertical_movement_without_shift_drops_the_selection(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        view.update(cx, |app, _| {
            app.editor.cursor = 4;
            app.editor.selection = Some(0..4);
        });

        cx.simulate_keystrokes("down");

        view.update(cx, |app, _| assert_eq!(app.editor.selection, None));
    }

    #[gpui::test]
    fn shrinking_a_shift_selection_back_to_its_start_selects_nothing(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.document = "SELECT 1".into();
            app.editor.cursor = 0;
        });

        cx.simulate_keystrokes("shift-right shift-right shift-left");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.cursor, 1);
            assert_eq!(app.editor.selection, Some(0..1));
        });
    }

    #[gpui::test]
    fn shift_selecting_leftwards_grows_away_from_the_anchor(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.editor.document = "SELECT 1".into();
            app.editor.cursor = 4;
        });

        cx.simulate_keystrokes("shift-left shift-left");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.cursor, 2);
            assert_eq!(app.editor.selection, Some(2..4));
        });
    }

    const THREE_LINES: &str = "SELECT one\nFROM t\nWHERE x";

    /// A viewport exactly ten lines tall, to keep the arithmetic in these tests obvious.
    const TEN_LINES: Pixels = px(EDITOR_LINE_HEIGHT * 10.);

    #[test]
    fn a_document_shorter_than_the_viewport_is_rendered_whole() {
        let visible = visible_lines(4, TEN_LINES, px(0.), 3);

        assert_eq!(visible.range, 0..4);
        assert_eq!(visible.above, 0);
        assert_eq!(visible.below, 0);
    }

    #[test]
    fn only_a_window_of_a_large_document_is_rendered() {
        let visible = visible_lines(50_000, TEN_LINES, px(0.), 3);

        assert!(
            visible.range.len() < 40,
            "a ten-line viewport should not render {} lines",
            visible.range.len()
        );
        assert_eq!(visible.below, 50_000 - visible.range.end);
    }

    #[test]
    fn scrolling_down_moves_the_window_and_counts_the_lines_above_it() {
        // GPUI scroll offsets run negative as content moves up. Twenty lines scrolled past, less
        // three lines of overscan, puts the window at line 17.
        let visible = visible_lines(1_000, TEN_LINES, px(-EDITOR_LINE_HEIGHT * 20.), 3);

        assert_eq!(visible.range.start, 17);
        assert_eq!(visible.above, 17);
    }

    #[test]
    fn the_window_never_runs_past_the_end_of_the_document() {
        let visible = visible_lines(30, TEN_LINES, px(-EDITOR_LINE_HEIGHT * 25.), 3);

        assert_eq!(visible.range.end, 30);
        assert_eq!(visible.below, 0);
    }

    #[test]
    fn every_line_is_either_rendered_or_spaced_over() {
        // The spacers stand in for the lines that are not rendered, so together they must always
        // account for the whole document or the scroll height is wrong.
        for (total, offset) in [
            (0, 0.),
            (1, 0.),
            (500, 0.),
            (500, -120.),
            (500, -12_400.),
            (500, -999_999.),
        ] {
            let visible = visible_lines(total, TEN_LINES, px(offset), 3);

            assert_eq!(
                visible.above + visible.range.len() + visible.below,
                total,
                "{total} lines at offset {offset} did not add up"
            );
        }
    }

    #[test]
    fn an_unmeasured_viewport_still_renders_something() {
        // Bounds are zero until the first layout pass; rendering nothing would blank the editor.
        let visible = visible_lines(500, px(0.), px(0.), 3);

        assert!(!visible.range.is_empty());
    }

    #[gpui::test]
    fn undo_returns_the_caret_to_where_the_edit_was_made(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);

        view.update(cx, |app, cx| {
            app.editor.cursor = 4;
            app.insert_text("X", cx);
            assert_eq!(app.editor.cursor, 5);

            app.undo(cx);

            assert_eq!(app.editor.document, THREE_LINES);
            assert_eq!(
                app.editor.cursor, 4,
                "the caret should return to the edit, not jump to the end of the document"
            );
        });
    }

    #[gpui::test]
    fn redo_leaves_the_caret_after_the_restored_edit(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);

        view.update(cx, |app, cx| {
            app.editor.cursor = 4;
            app.insert_text("X", cx);
            app.undo(cx);

            app.redo(cx);

            assert_eq!(app.editor.document, "SELEXCT one\nFROM t\nWHERE x");
            assert_eq!(app.editor.cursor, 5);
        });
    }

    /// The left edge of the editor text, derived the way the pointer mapping derives it.
    fn editor_text_left(app: &AppView) -> Pixels {
        let handle = &app.editor_scroll.handle;
        handle.bounds().origin.x + handle.offset().x + px(GUTTER_WIDTH)
    }

    /// Windowing the render makes the rendered lines a slice of the document, so a line's click
    /// handler has to carry its absolute document line. Using the index within the window instead
    /// would put the caret near the top of the file wherever the user clicked.
    #[gpui::test]
    fn a_click_lands_on_the_absolute_line_when_the_document_is_scrolled(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        // Numbered lines, so the caret's offset identifies the line it landed on.
        let document = (0..400)
            .map(|line| format!("SELECT {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        view.update(cx, |app, _| app.editor.document = document.clone());
        // The resize forces the layout pass that measures the content, without which nothing
        // overflows and the scroll offset set below clamps straight back to zero.
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        // Scroll 200 lines down, then click the first line of the viewport.
        let scrolled = 200.;
        view.update(cx, |app, cx| {
            app.editor_scroll
                .handle
                .set_offset(point(px(0.), px(-EDITOR_LINE_HEIGHT * scrolled)));
            cx.notify();
        });
        cx.run_until_parked();

        let position = view.update(cx, |app, _| {
            let bounds = app.editor_scroll.handle.bounds();
            point(
                editor_text_left(app) + app.editor_advance * 3.,
                bounds.origin.y + px(EDITOR_LINE_HEIGHT * 0.5),
            )
        });
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let line = document_position(&app.editor.document, app.editor.cursor).line;
            assert_eq!(
                line, scrolled as usize,
                "clicking the top of the viewport should land on line {scrolled}, not line {line}"
            );
        });
    }

    /// The handlers above are reached through the rendered lines, so this drives a real pointer
    /// event rather than calling them, and would catch the listeners being lost from the surface.
    #[gpui::test]
    fn a_real_click_on_the_rendered_editor_moves_the_caret(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        let position = view.update(cx, |app, _| {
            let bounds = app.editor_scroll.handle.bounds();
            point(
                editor_text_left(app) + app.editor_advance * 3.,
                // The middle of the second rendered line.
                bounds.origin.y + px(EDITOR_LINE_HEIGHT * 1.5),
            )
        });

        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        // Column 3 of "FROM t".
        view.update(cx, |app, _| assert_eq!(app.editor.cursor, 14));
    }

    #[gpui::test]
    fn the_editor_pointer_maps_to_a_character_column(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let advance = app.editor_advance;
            let left = editor_text_left(app);

            assert_eq!(app.editor_column_at(left), 0);
            assert_eq!(app.editor_column_at(left + advance * 5.), 5);
            // Rounding puts the caret at the nearest boundary, not the one to the left.
            assert_eq!(app.editor_column_at(left + advance * 5.6), 6);
            // Left of the text, in the gutter, can never produce a negative column.
            assert_eq!(app.editor_column_at(left - advance * 3.), 0);
        });
    }

    #[gpui::test]
    fn clicking_a_line_puts_the_caret_under_the_pointer(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, cx| {
            let x = editor_text_left(app) + app.editor_advance * 3.;
            app.click_editor(1, x, false, cx);

            // Column 3 of "FROM t".
            assert_eq!(app.editor.cursor, 14);
            assert_eq!(app.editor.selection, None);
        });
    }

    #[gpui::test]
    fn clicking_past_the_end_of_a_line_puts_the_caret_at_its_end(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, cx| {
            let x = editor_text_left(app) + app.editor_advance * 80.;
            app.click_editor(1, x, false, cx);

            assert_eq!(app.editor.cursor, 17);
        });
    }

    #[gpui::test]
    fn dragging_from_a_click_selects_the_span_between(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, cx| {
            let left = editor_text_left(app);
            app.click_editor(0, left + app.editor_advance * 2., false, cx);
            app.drag_editor(2, left + app.editor_advance * 3., cx);

            // Column 2 of line 0 through to column 3 of line 2.
            assert_eq!(app.editor.selection, Some(2..21));
            assert_eq!(app.editor.cursor, 21);
        });
    }

    #[gpui::test]
    fn a_backwards_editor_drag_selects_the_same_span(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, cx| {
            let left = editor_text_left(app);
            app.click_editor(2, left + app.editor_advance * 3., false, cx);
            app.drag_editor(0, left + app.editor_advance * 2., cx);

            assert_eq!(app.editor.selection, Some(2..21));
            assert_eq!(app.editor.cursor, 2);
        });
    }

    #[gpui::test]
    fn pointer_movement_without_a_click_leaves_the_caret_alone(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, cx| {
            app.editor.cursor = 4;
            let x = editor_text_left(app) + app.editor_advance * 3.;

            app.drag_editor(2, x, cx);

            assert_eq!(app.editor.cursor, 4);
            assert_eq!(app.editor.selection, None);
        });
    }

    #[gpui::test]
    fn shift_clicking_extends_the_selection_from_the_caret(cx: &mut TestAppContext) {
        let (view, cx) = three_line_editor(cx);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, cx| {
            app.editor.cursor = 2;
            let x = editor_text_left(app) + app.editor_advance * 3.;

            app.click_editor(2, x, true, cx);

            assert_eq!(app.editor.selection, Some(2..21));
        });
    }

    #[gpui::test]
    fn each_surface_measures_the_character_advance_at_its_own_text_size(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        view.update(cx, |app, _| {
            // A monospace advance scales with the text size, so the two surfaces' advances must
            // differ by the ratio of their sizes. Sharing one measurement drifts the editor caret.
            let ratio = f32::from(app.editor_advance) / f32::from(app.mono_advance);
            let expected = EDITOR_TEXT_SIZE / RESULT_TEXT_SIZE;
            assert!(
                (ratio - expected).abs() < 0.02,
                "editor advance {:?} is not measured at the editor text size: ratio {ratio}, expected {expected}",
                app.editor_advance
            );
        });
    }

    #[test]
    fn a_document_ending_in_a_newline_still_renders_its_empty_last_line() {
        assert_eq!(editor_lines("SELECT 1\n").count(), 2);
    }

    #[test]
    fn the_caret_line_is_always_one_of_the_rendered_lines() {
        // Pressing Enter at the end of the document must not put the caret on a line that the
        // renderer never emits, which is what `str::lines` would do.
        for document in ["", "SELECT 1", "SELECT 1\n", "SELECT 1\n\n"] {
            let caret = document_position(document, document.len());
            assert!(
                caret.line < editor_lines(document).count(),
                "caret line {} is not rendered for {document:?}",
                caret.line
            );
        }
    }

    #[test]
    fn a_selection_within_one_line_paints_only_those_columns() {
        assert_eq!(selected_columns(THREE_LINES, &(2..5), 0), Some(2..5));
    }

    #[test]
    fn the_first_line_of_a_selection_paints_from_its_start_column_to_the_line_end() {
        assert_eq!(selected_columns(THREE_LINES, &(4..20), 0), Some(4..10));
    }

    #[test]
    fn a_line_fully_inside_a_selection_paints_end_to_end() {
        assert_eq!(selected_columns(THREE_LINES, &(4..20), 1), Some(0..6));
    }

    #[test]
    fn the_last_line_of_a_selection_stops_at_its_end_column() {
        assert_eq!(selected_columns(THREE_LINES, &(4..20), 2), Some(0..2));
    }

    #[test]
    fn a_line_outside_the_selection_paints_nothing() {
        assert_eq!(selected_columns(THREE_LINES, &(2..5), 1), None);
    }

    #[test]
    fn an_empty_selection_paints_nothing() {
        assert_eq!(selected_columns(THREE_LINES, &(5..5), 0), None);
    }

    #[test]
    fn a_painted_span_is_measured_in_characters_not_bytes() {
        // "café" is bytes 3..8 but columns 3..7; painting by bytes would overhang by a character.
        assert_eq!(
            selected_columns("-- café\nSELECT 1", &(3..8), 0),
            Some(3..7)
        );
    }

    #[test]
    fn an_offset_reports_the_line_and_column_it_sits_on() {
        let document = "SELECT 1\nFROM t";

        assert_eq!(
            document_position(document, 11),
            DocumentPosition { line: 1, column: 2 }
        );
    }

    #[test]
    fn the_start_of_a_line_is_column_zero() {
        let document = "SELECT 1\nFROM t";

        assert_eq!(
            document_position(document, 9),
            DocumentPosition { line: 1, column: 0 }
        );
    }

    #[test]
    fn a_column_is_counted_in_characters_not_bytes() {
        // 'é' is two bytes, so a byte count would report column 3 for the offset after "café".
        let document = "-- café\nSELECT 1";

        assert_eq!(
            document_position(document, 8).line,
            0,
            "the offset is the last byte of the first line"
        );
        assert_eq!(document_position(document, 8).column, 7);
    }

    #[test]
    fn a_column_past_the_end_of_a_line_lands_on_its_last_character() {
        let document = "SELECT 1\nFROM t";

        assert_eq!(offset_of(document, 0, 40), 8);
    }

    #[test]
    fn a_line_past_the_end_of_the_document_lands_on_the_last_line() {
        let document = "SELECT 1\nFROM t";

        assert_eq!(offset_of(document, 9, 0), 9);
    }

    #[test]
    fn a_position_round_trips_through_an_offset_across_a_multibyte_line() {
        let document = "-- café\nSELECT 1";
        let position = DocumentPosition { line: 1, column: 6 };

        let offset = offset_of(document, position.line, position.column);

        assert_eq!(document_position(document, offset), position);
    }

    /// A document taller than the pane must be reachable. `min_h_full` on the surface caps its
    /// minimum height at the viewport and lets the flex row shrink to fit, so the content never
    /// overflows and neither the scrollbar nor the wheel has any range to work with.
    #[gpui::test]
    fn a_document_taller_than_the_pane_can_be_scrolled(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let document = (0..400)
            .map(|line| format!("SELECT {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        view.update(cx, |app, _| app.editor.document = document);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let viewport = app.editor_scroll.handle.bounds().size.height;
            let overflow = app.editor_scroll.handle.max_offset().height;
            assert!(
                overflow > px(0.),
                "400 lines in a {viewport:?} pane should overflow, got {overflow:?}"
            );
            assert!(
                ThumbMetrics::measure(viewport, overflow, px(0.)).is_some(),
                "a vertical scrollbar should be warranted"
            );
        });
    }

    /// Virtualisation places lines arithmetically, so every rendered line must be exactly
    /// `EDITOR_LINE_HEIGHT` tall. A minimum height instead of a fixed one lets a line grow, and
    /// the window then drifts further from the pointer the further down the document you scroll.
    #[gpui::test]
    fn every_rendered_line_is_exactly_one_line_tall(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let count = 400;
        let document = (0..count)
            .map(|line| format!("SELECT {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        view.update(cx, |app, _| app.editor.document = document);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let viewport = app.editor_scroll.handle.bounds().size.height;
            let content = app.editor_scroll.handle.max_offset().height + viewport;
            let expected = px(EDITOR_LINE_HEIGHT * count as f32);
            let drift = f32::from(content) - f32::from(expected);
            assert!(
                drift.abs() < EDITOR_LINE_HEIGHT,
                "{count} lines should measure {expected:?}, got {content:?}"
            );
        });
    }

    /// §19 "reasonable handling of large SQL documents": the pane builds a window of lines, so a
    /// very large document stays responsive and still maps the pointer to the right place.
    #[gpui::test]
    fn a_very_large_document_stays_interactive(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let count = 20_000;
        let document = (0..count)
            .map(|line| format!("SELECT {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        view.update(cx, |app, _| app.editor.document = document);
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        // Jump most of the way down and click the top of the viewport.
        let target = 15_000.;
        view.update(cx, |app, cx| {
            app.editor_scroll
                .handle
                .set_offset(point(px(0.), px(-EDITOR_LINE_HEIGHT * target)));
            cx.notify();
        });
        cx.run_until_parked();

        let position = view.update(cx, |app, _| {
            let bounds = app.editor_scroll.handle.bounds();
            point(
                editor_text_left(app) + app.editor_advance * 3.,
                bounds.origin.y + px(EDITOR_LINE_HEIGHT * 0.5),
            )
        });
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let line = document_position(&app.editor.document, app.editor.cursor).line;
            assert_eq!(line, target as usize);
            let content = app.editor_scroll.handle.max_offset().height
                + app.editor_scroll.handle.bounds().size.height;
            let expected = px(EDITOR_LINE_HEIGHT * count as f32);
            assert!(
                (f32::from(content) - f32::from(expected)).abs() < EDITOR_LINE_HEIGHT,
                "the spacers should preserve the full scroll height: {content:?} vs {expected:?}"
            );
        });
    }

    /// Windowing the render leaves only the visible lines to measure, so without help the
    /// horizontal extent would grow and shrink as you scroll past longer lines. The font is
    /// monospace, so the document's widest line can be measured arithmetically instead.
    #[gpui::test]
    fn horizontal_extent_covers_the_widest_line_even_when_it_is_scrolled_away(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = build_app_view(cx);
        // One very long line at the top, then enough short lines to scroll it out of view.
        let mut document = vec!["SELECT ".to_owned() + &"x".repeat(600)];
        document.extend((0..400).map(|line| format!("SELECT {line};")));
        view.update(cx, |app, _| app.editor.document = document.join("\n"));
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        let at_top = view.update(cx, |app, _| app.editor_scroll.handle.max_offset().width);
        assert!(at_top > px(0.), "the long line should overflow sideways");

        view.update(cx, |app, cx| {
            app.editor_scroll
                .handle
                .set_offset(point(px(0.), px(-EDITOR_LINE_HEIGHT * 300.)));
            cx.notify();
        });
        cx.run_until_parked();

        let scrolled = view.update(cx, |app, _| app.editor_scroll.handle.max_offset().width);
        assert_eq!(
            scrolled, at_top,
            "the horizontal extent should not change when the widest line scrolls out of view"
        );
    }

    const ROW: Pixels = px(RESULT_LINE_HEIGHT);
    const TEN_ROWS: Pixels = px(RESULT_LINE_HEIGHT * 10.);

    #[test]
    fn a_results_block_at_the_top_windows_from_its_first_row() {
        let visible = visible_rows(0, 10_000, ROW, TEN_ROWS, px(0.), 2);

        assert_eq!(visible.range.start, 0);
        assert!(visible.range.len() < 40);
        assert_eq!(visible.below, 10_000 - visible.range.end);
    }

    #[test]
    fn a_results_block_scrolled_past_entirely_builds_no_rows() {
        // The block occupies global rows 0..50 while the viewport is far below it.
        let visible = visible_rows(0, 50, ROW, TEN_ROWS, px(-RESULT_LINE_HEIGHT * 5_000.), 2);

        assert!(visible.range.is_empty());
        assert_eq!(visible.above, 50, "the whole block sits above the viewport");
        assert_eq!(visible.below, 0);
    }

    #[test]
    fn a_later_block_windows_relative_to_the_rows_before_it() {
        // 1000 rows precede this block, and the viewport sits at global row 1010.
        let visible = visible_rows(
            1_000,
            500,
            ROW,
            TEN_ROWS,
            px(-RESULT_LINE_HEIGHT * 1_010.),
            2,
        );

        assert_eq!(
            visible.range.start, 8,
            "1010 - 2 slop, less the 1000 before it"
        );
        assert_eq!(visible.above, 8);
    }

    #[test]
    fn a_block_not_yet_reached_builds_nothing_but_still_accounts_for_its_rows() {
        let visible = visible_rows(5_000, 100, ROW, TEN_ROWS, px(0.), 2);

        assert!(visible.range.is_empty());
        assert_eq!(visible.above + visible.range.len() + visible.below, 100);
    }

    #[test]
    fn every_result_row_is_either_built_or_spaced_over() {
        for before in [0, 1, 900, 5_000] {
            for offset in [0., -220., -22_000., -999_999.] {
                let visible = visible_rows(before, 500, ROW, TEN_ROWS, px(offset), 2);

                assert_eq!(
                    visible.above + visible.range.len() + visible.below,
                    500,
                    "before {before} at offset {offset} did not add up"
                );
            }
        }
    }

    /// Table rows are positioned arithmetically once the grid is windowed, so each must be exactly
    /// `RESULT_ROW_HEIGHT` tall rather than whatever its padding happens to produce.
    #[gpui::test]
    fn every_table_row_is_exactly_one_row_tall(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let counts = [100usize, 200];
        let mut heights = Vec::new();
        for count in counts {
            let owned: Vec<String> = (0..count).map(|row| format!("row {row}")).collect();
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            view.update(cx, |app, cx| {
                app.editor.display = ResultDisplay::Table;
                app.editor.results = vec![result_with_rows(&refs)];
                app.focus = Focus::Editor;
                cx.notify();
            });
            cx.simulate_resize(size(px(1280.), px(820.)));
            cx.run_until_parked();
            heights.push(view.update(cx, |app, _| {
                app.results_scroll.handle.max_offset().height
                    + app.results_scroll.handle.bounds().size.height
            }));
        }

        let per_row =
            (f32::from(heights[1]) - f32::from(heights[0])) / (counts[1] - counts[0]) as f32;
        assert!(
            (per_row - RESULT_ROW_HEIGHT).abs() < 0.01,
            "each table row should measure {RESULT_ROW_HEIGHT}px, measured {per_row}px"
        );
    }

    /// The row-limit control goes up to `MAX_ROW_LIMIT`, so the grid has to cope with result sets
    /// far larger than the pane. Rows must stay addressable by their absolute position, or a click
    /// deep in a scrolled result selects the wrong row.
    #[gpui::test]
    fn a_very_large_result_stays_interactive(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        let count = 50_000usize;
        let owned: Vec<String> = (0..count).map(|row| format!("row {row}")).collect();
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        view.update(cx, |app, _| {
            app.editor.display = ResultDisplay::Table;
            app.editor.results = vec![result_with_rows(&refs)];
            app.focus = Focus::Editor;
        });
        cx.simulate_resize(size(px(1280.), px(820.)));
        cx.run_until_parked();

        let target = 40_000usize;
        view.update(cx, |app, cx| {
            app.results_scroll
                .handle
                .set_offset(point(px(0.), px(-RESULT_ROW_HEIGHT * target as f32)));
            cx.notify();
        });
        cx.run_until_parked();

        let position = view.update(cx, |app, _| {
            let bounds = app.results_scroll.handle.bounds();
            point(
                bounds.origin.x + px(RESULT_TEXT_INSET),
                bounds.origin.y + px(RESULT_ROW_HEIGHT * 0.5),
            )
        });
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let selected = app
                .result_selection
                .expect("clicking a row should select it")
                .ordered()
                .0
                .line;
            let drift = selected.abs_diff(target);
            assert!(
                drift <= RESULT_CHROME_SLOP_ROWS,
                "clicking at row {target} selected row {selected}"
            );
        });
    }

    #[gpui::test]
    fn closing_a_background_editor_removes_it(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1".into();
            app.new_editor(cx);
            app.editor.document = "SELECT 2".into();
        });

        view.update(cx, |app, cx| {
            assert_eq!(app.background_editors.len(), 1);
            app.close_editor(0, cx);

            assert!(app.background_editors.is_empty());
            assert_eq!(app.editor.document, "SELECT 2", "the active editor stays");
        });
    }

    #[gpui::test]
    fn closing_the_active_editor_falls_back_to_the_one_behind_it(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1".into();
            app.new_editor(cx);
            app.editor.document = "SELECT 2".into();
        });

        view.update(cx, |app, cx| {
            app.close_active_editor(cx);

            assert!(app.background_editors.is_empty());
            assert_eq!(app.editor.document, "SELECT 1");
        });
    }

    #[gpui::test]
    fn the_last_editor_is_never_closed(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1".into();

            app.close_active_editor(cx);

            assert_eq!(
                app.editor.document, "SELECT 1",
                "closing the only editor would leave the workspace with nothing to type into"
            );
        });
    }

    #[gpui::test]
    fn a_running_editor_is_not_closed_from_under_its_query(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1".into();
            app.new_editor(cx);
            app.editor.document = "SELECT 2".into();
            app.editor.execution_status = ExecutionStatus::Running;

            app.close_active_editor(cx);

            assert_eq!(app.editor.document, "SELECT 2");
            assert_eq!(app.background_editors.len(), 1);
        });
    }

    #[gpui::test]
    fn the_close_shortcut_resolves_to_a_stable_command_id(_cx: &mut TestAppContext) {
        assert_eq!(
            shortcut_command("w", true, false, false, false, false),
            Some(command::CLOSE_EDITOR)
        );
        // Not while a query is in flight, matching what close_active_editor itself refuses.
        assert_eq!(shortcut_command("w", true, false, false, true, true), None);
    }

    /// Table mode shows no notice text of its own, so the header has to say one arrived — without
    /// letting a long notice push the row counts off the row (§40).
    #[test]
    fn the_header_reports_notices_without_letting_them_run_away() {
        assert_eq!(notice_summary(&[]), None);
        assert_eq!(
            notice_summary(&["NOTICE: nothing to do".to_owned()]),
            Some("NOTICE: nothing to do".to_owned())
        );
        assert_eq!(
            notice_summary(&["NOTICE: one".to_owned(), "WARNING: two".to_owned()]),
            Some("2 notices".to_owned())
        );

        let long = notice_summary(&["NOTICE: ".to_owned() + &"x".repeat(200)])
            .expect("a single notice should be summarised");
        assert_eq!(long.chars().count(), 73, "72 characters and an ellipsis");
    }

    /// Multibyte notices must not be sliced through a character.
    #[test]
    fn truncation_lands_on_a_character_boundary() {
        assert_eq!(truncated("ünïcödé", 3), "ünï…");
        assert_eq!(truncated("short", 20), "short");
    }

    /// One window, refreshed. Opening a fresh native window per run leaves the user closing a
    /// trail of stale result windows (§32.3).
    #[gpui::test]
    fn a_second_run_refreshes_the_open_result_window(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        let first = view.update(cx, |app, cx| {
            app.editor.results = vec![result_with_rows(&["one"])];
            app.show_results_in_window(cx);
            app.result_window.expect("a result window should be open")
        });
        let second = view.update(cx, |app, cx| {
            app.editor.results = vec![result_with_rows(&["one", "two"])];
            app.editor.display = ResultDisplay::Text;
            app.show_results_in_window(cx);
            app.result_window
                .expect("the result window should still be open")
        });

        assert_eq!(first, second, "the second run should reuse the same window");
        let (rows, display) = view.update(cx, |_, cx| {
            second
                .update(cx, |window, _, _| {
                    (window.results[0].rows.len(), window.display)
                })
                .expect("the window should still be readable")
        });
        assert_eq!(rows, 2, "the window should show the newest results");
        assert_eq!(display, ResultDisplay::Text);
    }

    /// The separate window can be handed a result as large as the row limit allows, so it windows
    /// its rows exactly as the pane does rather than building every one of them (§41).
    #[gpui::test]
    fn the_result_window_builds_only_a_window_of_a_large_grid(cx: &mut TestAppContext) {
        let count = 50_000usize;
        let owned: Vec<String> = (0..count).map(|row| format!("row {row}")).collect();
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let result = result_with_rows(&refs);
        let (window, cx) = cx.add_window_view(|_, cx| ResultWindow {
            results: vec![result],
            display: ResultDisplay::Table,
            fonts: Fonts::resolve(cx),
            scroll: ScrollState::default(),
        });
        cx.simulate_resize(size(px(900.), px(600.)));
        cx.run_until_parked();

        window.update(cx, |window, _| {
            let built = window
                .visible_result_rows(0, count + 1, px(RESULT_ROW_HEIGHT))
                .range
                .len();
            assert!(
                built < 200,
                "a 50 000 row grid should build a screenful, built {built}"
            );
            // The spacers still have to account for every row, or the scrollbar lies.
            let content = window.scroll.handle.max_offset().height
                + window.scroll.handle.bounds().size.height;
            let expected = px(RESULT_ROW_HEIGHT * (count + 1) as f32);
            assert!(
                (f32::from(content) - f32::from(expected)).abs() < RESULT_ROW_HEIGHT * 4.,
                "the spacers should preserve the full scroll height: {content:?} vs {expected:?}"
            );
        });
    }

    #[gpui::test]
    fn the_result_window_switches_between_table_and_text(cx: &mut TestAppContext) {
        let (window, cx) = cx.add_window_view(|_, cx| ResultWindow {
            results: vec![result_with_rows(&["one"])],
            display: ResultDisplay::Table,
            fonts: Fonts::resolve(cx),
            scroll: ScrollState::default(),
        });

        window.update(cx, |window, cx| window.set_display(ResultDisplay::Text, cx));

        window.update(cx, |window, _| {
            assert_eq!(window.display, ResultDisplay::Text);
        });
    }

    /// The ± steps move by a factor of ten, so 20 and 50 are only reachable by typing them (§26).
    #[gpui::test]
    fn a_typed_row_limit_reaches_a_value_the_steps_cannot(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| app.edit_limit(cx));

        cx.simulate_input("20");
        cx.simulate_keystrokes("enter");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.row_limit, 20);
            assert!(app.limit_buffer.is_none());
        });
    }

    #[gpui::test]
    fn a_row_limit_outside_the_permitted_range_is_refused(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| app.edit_limit(cx));

        cx.simulate_input("0");
        cx.simulate_keystrokes("enter");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.row_limit, crate::DEFAULT_ROW_LIMIT);
            // The edit stays open so the user can correct it rather than retype from scratch.
            assert_eq!(app.limit_buffer.as_deref(), Some("0"));
            assert!(app.status.contains("1"), "status should name the range");
        });
    }

    #[gpui::test]
    fn clicking_into_the_document_leaves_the_row_limit_field(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| {
            app.editor.document = "SELECT 1".into();
            app.edit_limit(cx);
            app.click_editor(0, px(0.), false, cx);
        });

        cx.simulate_input("x");

        view.update(cx, |app, _| {
            assert!(app.limit_buffer.is_none());
            assert!(
                app.editor.document.contains('x'),
                "typing should reach the document again"
            );
        });
    }

    #[gpui::test]
    fn abandoning_a_row_limit_edit_keeps_the_previous_value(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| app.edit_limit(cx));

        cx.simulate_input("500");
        cx.simulate_keystrokes("escape");

        view.update(cx, |app, _| {
            assert_eq!(app.editor.row_limit, crate::DEFAULT_ROW_LIMIT);
            assert!(app.limit_buffer.is_none());
        });
    }

    /// The limit field has no focus handle of its own, so the key handler has to hold the keys
    /// away from the SQL document while it is open.
    #[gpui::test]
    fn keys_typed_into_the_row_limit_never_reach_the_document(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, cx| app.edit_limit(cx));

        cx.simulate_input("7x5");
        cx.simulate_keystrokes("backspace");
        cx.simulate_keystrokes("enter");

        view.update(cx, |app, _| {
            assert!(app.editor.document.is_empty());
            // The stray letter is dropped rather than ending the edit or being typed in.
            assert_eq!(app.editor.row_limit, 7);
        });
    }
}
