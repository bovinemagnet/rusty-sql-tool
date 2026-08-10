use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    SharedString, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
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
}

impl AppView {
    fn new(cx: &mut Context<Self>) -> Self {
        let profiles = discover_profiles();
        let fonts = Fonts::resolve(cx);
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
        }
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
                        this.status = format!("Connected: {}", this.editor.connection_identity());
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
                        this.schemas.clear();
                        this.objects.clear();
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
                    let status = match mode {
                        RunMode::Current => service.run(&mut editor).await.map(|_| ()),
                        RunMode::Explain => service.explain(&mut editor).await.map(|_| ()),
                        RunMode::All => {
                            service
                                .run_all(&mut editor)
                                .await
                                .map(|_| ())
                                .map_err(|error| crate::result::QueryError {
                                    message: error.to_string(),
                                    severity: None,
                                    code: None,
                                    detail: None,
                                    hint: None,
                                    position: None,
                                })
                        }
                    };
                    (editor, status)
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                match joined {
                    Ok((editor, result)) => {
                        this.editor = editor;
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
                                            })
                                        },
                                    );
                                }
                            }
                        }
                        this.status = match result {
                            Ok(()) => completion_status(&this.editor.results),
                            Err(error) => format!("Query failed: {error}"),
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
                    this.editor = editor;
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
                            profile.name = "Manual".into();
                            self.editor.connection = profile.clone();
                            self.profiles.push(profile);
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
                "a" => {
                    self.editor.selection = Some(0..self.editor.document.len());
                    self.editor.cursor = self.editor.document.len();
                    cx.notify();
                }
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
            let active = profile.id == self.editor.connection.id;
            let profile_id = profile.id;
            let indicator_colour = connection_indicator_colour(active, self.connection_state);
            let live = active && self.connection_state == ConnectionState::Connected;
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

    fn results_surface(&self) -> impl IntoElement {
        let mut content = div()
            .id("results-scroll")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .overflow_x_scroll();
        if self.editor.results.is_empty() && self.editor.error.is_none() {
            return content
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
                );
        }
        for (index, result) in self.editor.results.iter().enumerate() {
            content = content.child(result_header(&self.fonts, index, result));
            content = match self.editor.display {
                ResultDisplay::Text => content.child(
                    div()
                        .px_5()
                        .py_3()
                        .font_family(self.fonts.mono.clone())
                        .text_size(px(13.))
                        .line_height(px(22.))
                        .child(result.as_text()),
                ),
                ResultDisplay::Table => content.child(result_table(&self.fonts, result)),
            };
        }
        // Earlier results stay on screen beneath the failure (§46, §47).
        if let Some(error) = &self.editor.error {
            content = content.child(self.error_card(error));
        }
        content
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

struct ResultWindow {
    results: Vec<QueryResult>,
    display: ResultDisplay,
    fonts: Fonts,
}

impl Render for ResultWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut surface = div()
            .id("separate-results-scroll")
            .size_full()
            .overflow_scroll()
            .flex()
            .flex_col()
            .pt_4()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .font_family(self.fonts.body.clone());
        for (index, result) in self.results.iter().enumerate() {
            surface = surface.child(result_header(&self.fonts, index, result));
            surface = match self.display {
                ResultDisplay::Table => surface.child(result_table(&self.fonts, result)),
                ResultDisplay::Text => surface.child(
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
        surface
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
        let running = matches!(
            self.editor.execution_status,
            ExecutionStatus::Running | ExecutionStatus::Cancelling
        );
        div()
            .track_focus(&self.focus_handle)
            .key_context("SqlEditor")
            .on_key_down(cx.listener(|this, event, _, cx| this.handle_key(event, cx)))
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
                                                    .child(self.editor.connection_identity()),
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
                                            true,
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
                                    .id("editor-scroll")
                                    .flex_1()
                                    .min_h_0()
                                    .px(px(30.))
                                    .py(px(20.))
                                    .overflow_y_scroll()
                                    .overflow_x_scroll()
                                    .cursor_text()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            window.focus(&this.focus_handle(cx));
                                        }),
                                    )
                                    .child(if self.active_result_tab {
                                        self.results_surface().into_any_element()
                                    } else {
                                        self.editor_surface().into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .h(
                                        if self.editor.destination == ResultDestination::Pane
                                            && !self.active_result_tab
                                        {
                                            px(RESULT_PANE_HEIGHT)
                                        } else {
                                            px(0.)
                                        },
                                    )
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
                                            .child(self.display_segments(cx))
                                            .child(self.destination_segments(cx)),
                                    )
                                    .child(self.results_surface()),
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

fn result_table(fonts: &Fonts, result: &QueryResult) -> impl IntoElement {
    let mut table = div()
        .flex()
        .flex_col()
        .mx_5()
        .mb_4()
        .rounded(px(CARD_RADIUS))
        .overflow_hidden()
        .bg(rgb(PANEL))
        .font_family(fonts.mono.clone());
    if !result.columns.is_empty() {
        let mut header = div().flex().border_b_1().border_color(rgb(BORDER));
        for column in &result.columns {
            header = header.child(
                div()
                    .w(px(180.))
                    .px_4()
                    .py(px(11.))
                    .text_size(px(10.))
                    .text_color(rgb(FAINT))
                    .child(column.name.to_uppercase()),
            );
        }
        table = table.child(header);
    }
    for row in &result.rows {
        let mut rendered_row = div().flex().border_b_1().border_color(rgb(BORDER));
        for value in row {
            rendered_row = rendered_row.child(
                div()
                    .w(px(180.))
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
        table = table.child(rendered_row);
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
    if command_modifier {
        return match key {
            "enter" if shift => Some(command::RUN_ALL),
            "enter" if alt => Some(command::EXPLAIN),
            "enter" => Some(command::RUN),
            "." if running => Some(command::CANCEL),
            "n" => Some(command::NEW_EDITOR),
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

fn discover_profiles() -> Vec<ConnectionProfile> {
    if Path::new(".env").is_file()
        && let Ok(profiles) = ConnectionProfile::profiles_from_env_file(".env")
        && !profiles.is_empty()
    {
        return profiles;
    }
    if let Ok(Some(profile)) = ConnectionProfile::from_process_env() {
        return vec![profile];
    }
    vec![local_profile().unwrap_or_else(|| {
        ConnectionProfile::from_database_url(
            "postgresql://postgres@localhost:5432/postgres?sslmode=disable",
        )
        .expect("built-in PostgreSQL profile should be valid")
    })]
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
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

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
            panic!("UI connection tests do not execute SQL")
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
            shortcut_command("enter", true, false, false, false, false),
            Some(command::RUN)
        );
        assert_eq!(
            shortcut_command("enter", true, true, false, false, false),
            Some(command::RUN_ALL)
        );
        assert_eq!(
            shortcut_command("enter", true, false, true, false, false),
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
