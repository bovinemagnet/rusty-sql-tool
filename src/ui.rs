use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    SharedString, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use tokio::runtime::Runtime;

use crate::application::{CommandService, EditorState, ResultDestination, ResultDisplay};
use crate::config::{ConnectionProfile, local_profile};
use crate::database::{ConnectionState, DatabaseObject, ObjectKind};
use crate::postgres::PostgresProvider;
use crate::result::{CellValue, ExecutionStatus, QueryResult};

const BACKGROUND: u32 = 0x111318;
const PANEL: u32 = 0x181b22;
const PANEL_LIGHT: u32 = 0x20242d;
const BORDER: u32 = 0x303642;
const TEXT: u32 = 0xd7dae0;
const MUTED: u32 = 0x8d96a5;
const ACCENT: u32 = 0x7aa2f7;
const GREEN: u32 = 0x73daca;
const RED: u32 = 0xf7768e;
const KEYWORD: u32 = 0xbb9af7;
const STRING: u32 = 0x9ece6a;

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
}

impl AppView {
    fn new(cx: &mut Context<Self>) -> Self {
        let profile = discover_profile();
        let provider = PostgresProvider::new();
        let service = Arc::new(CommandService::new(provider));
        Self {
            service,
            runtime: Arc::new(Runtime::new().expect("could not start asynchronous runtime")),
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
                                        |_, cx| cx.new(|_| ResultWindow { results, display }),
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
                            self.editor.connection = profile;
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
        if command {
            match key {
                "enter" => self.run(RunMode::Current, cx),
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

    fn cycle_display(&mut self, cx: &mut Context<Self>) {
        self.editor.display = match self.editor.display {
            ResultDisplay::Table => ResultDisplay::Text,
            ResultDisplay::Text => ResultDisplay::Table,
        };
        cx.notify();
    }

    fn cycle_destination(&mut self, cx: &mut Context<Self>) {
        self.editor.destination = match self.editor.destination {
            ResultDestination::Pane => ResultDestination::Tab,
            ResultDestination::Tab => ResultDestination::Window,
            ResultDestination::Window => ResultDestination::Pane,
        };
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
        let mut tree = div().flex().flex_col().gap_1().text_sm();
        if self.schemas.is_empty() {
            tree = tree.child(div().p_3().text_color(rgb(MUTED)).child(
                if self.connection_state == ConnectionState::Connected {
                    "No user schemas"
                } else {
                    "No database connections.\nLoad .env or connect manually."
                },
            ));
        }
        for schema in &self.schemas {
            let selected = self.expanded_schema.as_deref() == Some(schema);
            let schema_for_click = schema.clone();
            tree = tree.child(
                div()
                    .id(SharedString::from(format!("schema-{schema}")))
                    .px_3()
                    .py_1()
                    .hover(|style| style.bg(rgb(PANEL_LIGHT)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.load_schema(schema_for_click.clone(), false, cx)
                    }))
                    .child(format!("{} {schema}", if selected { "▾" } else { "▸" })),
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
                            div()
                                .pl_6()
                                .py_1()
                                .text_color(rgb(MUTED))
                                .child(format!("{kind} ({})", matching.len())),
                        );
                        for object in matching {
                            tree = tree.child(div().pl_10().py_1().child(object.name.clone()));
                        }
                    }
                } else {
                    tree = tree.child(div().pl_6().py_1().child("Loading…"));
                }
            }
        }
        tree
    }

    fn editor_surface(&self) -> impl IntoElement {
        let mut lines = div().flex().flex_col().min_h_full();
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
                    .min_h(px(22.))
                    .child(
                        div()
                            .w(px(46.))
                            .pr_3()
                            .text_right()
                            .text_color(rgb(MUTED))
                            .child((index + 1).to_string()),
                    )
                    .child(highlight_line(line)),
            );
        }
        lines.child(div().pl(px(46.)).text_color(rgb(ACCENT)).child("▏"))
    }

    fn results_surface(&self) -> impl IntoElement {
        let mut content = div()
            .id("results-scroll")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .overflow_x_scroll();
        if let Some(error) = &self.editor.error {
            return content.p_3().text_color(rgb(RED)).child(error.to_string());
        }
        if self.editor.results.is_empty() {
            return content
                .items_center()
                .justify_center()
                .text_color(rgb(MUTED))
                .child("Run a query to see results.");
        }
        for (index, result) in self.editor.results.iter().enumerate() {
            content = content.child(result_header(index, result));
            content = match self.editor.display {
                ResultDisplay::Text => {
                    content.child(div().p_3().font_family("monospace").child(result.as_text()))
                }
                ResultDisplay::Table => content.child(result_table(result)),
            };
        }
        content
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
}

impl Render for ResultWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut surface = div()
            .id("separate-results-scroll")
            .size_full()
            .overflow_scroll()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .font_family("monospace");
        for (index, result) in self.results.iter().enumerate() {
            surface = surface.child(result_header(index, result));
            surface = match self.display {
                ResultDisplay::Table => surface.child(result_table(result)),
                ResultDisplay::Text => surface.child(div().p_3().child(result.as_text())),
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
            .font_family("monospace")
            .child(
                div()
                    .h(px(38.))
                    .flex()
                    .items_center()
                    .px_3()
                    .bg(rgb(PANEL))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child("Rusty SQL Tool")
                    .child(
                        div()
                            .ml_auto()
                            .text_sm()
                            .text_color(if connected { rgb(GREEN) } else { rgb(MUTED) })
                            .child(format!("● {:?}", self.connection_state)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(260.))
                            .flex()
                            .flex_col()
                            .bg(rgb(PANEL))
                            .border_r_1()
                            .border_color(rgb(BORDER))
                            .child(
                                div()
                                    .h(px(42.))
                                    .flex()
                                    .items_center()
                                    .px_3()
                                    .border_b_1()
                                    .border_color(rgb(BORDER))
                                    .child("CONNECTIONS")
                                    .child(
                                        div()
                                            .id("configure-connection")
                                            .ml_auto()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .when(
                                                self.connection_state
                                                    == ConnectionState::Disconnected,
                                                |element| {
                                                    element
                                                        .cursor_pointer()
                                                        .hover(|style| style.bg(rgb(BORDER)))
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.configure_connection(cx)
                                                        }))
                                                },
                                            )
                                            .child("＋"),
                                    )
                                    .child(
                                        div()
                                            .id("refresh-metadata")
                                            .ml_1()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .when(connected, |element| {
                                                element
                                                    .cursor_pointer()
                                                    .hover(|style| style.bg(rgb(BORDER)))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.refresh_metadata(cx)
                                                    }))
                                            })
                                            .child("↻"),
                                    ),
                            )
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_color(if connected { rgb(GREEN) } else { rgb(MUTED) })
                                    .child(self.editor.connection_identity()),
                            )
                            .child(self.connection_tree(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .h(px(38.))
                                    .flex()
                                    .items_center()
                                    .px_3()
                                    .bg(rgb(PANEL_LIGHT))
                                    .border_b_1()
                                    .border_color(rgb(BORDER))
                                    .children(self.background_editors.iter().enumerate().map(
                                        |(index, editor)| {
                                            div()
                                                .id(SharedString::from(format!(
                                                    "editor-tab-{index}"
                                                )))
                                                .mr_2()
                                                .px_2()
                                                .py_1()
                                                .rounded_sm()
                                                .bg(rgb(PANEL))
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.switch_editor(index, cx)
                                                }))
                                                .child(editor.title.clone())
                                        },
                                    ))
                                    .child(
                                        div()
                                            .id("editor-tab")
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .bg(if self.active_result_tab {
                                                rgb(PANEL)
                                            } else {
                                                rgb(BORDER)
                                            })
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.show_editor_tab(cx)
                                            }))
                                            .child(self.editor.title.clone()),
                                    )
                                    .when(!self.editor.results.is_empty(), |tabs| {
                                        tabs.child(
                                            div()
                                                .id("result-tab")
                                                .ml_2()
                                                .px_2()
                                                .py_1()
                                                .rounded_sm()
                                                .bg(if self.active_result_tab {
                                                    rgb(BORDER)
                                                } else {
                                                    rgb(PANEL)
                                                })
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.show_result_tab(cx)
                                                }))
                                                .child(format!("Result: {}", self.editor.title)),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("new-editor")
                                            .ml_2()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .hover(|style| style.bg(rgb(BORDER)))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.new_editor(cx)),
                                            )
                                            .child("+"),
                                    )
                                    .child(
                                        div()
                                            .ml_auto()
                                            .text_sm()
                                            .text_color(rgb(ACCENT))
                                            .child(self.editor.connection_identity()),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(44.))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .border_b_1()
                                    .border_color(rgb(BORDER))
                                    .child(button(
                                        "connect",
                                        "Connect",
                                        !connected && !running,
                                        cx.listener(|this, _, _, cx| this.connect(cx)),
                                    ))
                                    .child(button(
                                        "disconnect",
                                        "Disconnect",
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| this.disconnect(cx)),
                                    ))
                                    .child(button(
                                        "run",
                                        "▶ Run",
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| {
                                            this.run(RunMode::Current, cx)
                                        }),
                                    ))
                                    .child(button(
                                        "run-all",
                                        "▶▶ Run All",
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| this.run(RunMode::All, cx)),
                                    ))
                                    .child(button(
                                        "explain",
                                        "Explain",
                                        connected && !running,
                                        cx.listener(|this, _, _, cx| {
                                            this.run(RunMode::Explain, cx)
                                        }),
                                    ))
                                    .child(button(
                                        "stop",
                                        "■ Stop",
                                        running,
                                        cx.listener(|this, _, _, cx| this.cancel(cx)),
                                    ))
                                    .child(div().ml_auto().text_sm().child("Row Limit"))
                                    .child(button(
                                        "limit-down",
                                        "−",
                                        self.editor.row_limit > 1,
                                        cx.listener(|this, _, _, cx| this.change_limit(false, cx)),
                                    ))
                                    .child(self.editor.row_limit.to_string())
                                    .child(button(
                                        "limit-up",
                                        "+",
                                        self.editor.row_limit < crate::MAX_ROW_LIMIT,
                                        cx.listener(|this, _, _, cx| this.change_limit(true, cx)),
                                    )),
                            )
                            .child(
                                div()
                                    .id("editor-scroll")
                                    .flex_1()
                                    .min_h_0()
                                    .p_3()
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
                                            px(260.)
                                        } else {
                                            px(0.)
                                        },
                                    )
                                    .flex()
                                    .flex_col()
                                    .border_t_1()
                                    .border_color(rgb(BORDER))
                                    .child(
                                        div()
                                            .h(px(38.))
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_3()
                                            .bg(rgb(PANEL))
                                            .child("RESULTS")
                                            .child(button(
                                                "display",
                                                match self.editor.display {
                                                    ResultDisplay::Table => "Table",
                                                    ResultDisplay::Text => "Text",
                                                },
                                                true,
                                                cx.listener(|this, _, _, cx| {
                                                    this.cycle_display(cx)
                                                }),
                                            ))
                                            .child(button(
                                                "destination",
                                                match self.editor.destination {
                                                    ResultDestination::Pane => "Pane",
                                                    ResultDestination::Tab => "Tab",
                                                    ResultDestination::Window => "Window",
                                                },
                                                true,
                                                cx.listener(|this, _, _, cx| {
                                                    this.cycle_destination(cx)
                                                }),
                                            )),
                                    )
                                    .child(self.results_surface()),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .px_3()
                    .bg(rgb(PANEL_LIGHT))
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(self.status.clone())
                    .child(
                        div()
                            .ml_auto()
                            .child(self.server_version.clone().unwrap_or_default()),
                    ),
            )
            .when(self.connection_dialog, |root| {
                root.child(
                    div()
                        .absolute()
                        .top(px(90.))
                        .left(px(360.))
                        .w(px(620.))
                        .p_4()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(ACCENT))
                        .bg(rgb(PANEL_LIGHT))
                        .shadow_lg()
                        .child("Configure PostgreSQL connection")
                        .child(div().mt_2().text_sm().text_color(rgb(MUTED)).child(
                            "Enter postgresql://user:password@host:5432/database?sslmode=prefer",
                        ))
                        .child(
                            div()
                                .mt_3()
                                .p_2()
                                .border_1()
                                .border_color(rgb(BORDER))
                                .child(if self.connection_buffer.is_empty() {
                                    "••••••••••••••••".into()
                                } else {
                                    "•".repeat(self.connection_buffer.chars().count())
                                }),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child("Enter to apply · Escape to cancel · Paste is supported"),
                        ),
                )
            })
    }
}

fn button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(BORDER))
        .bg(if enabled {
            rgb(PANEL_LIGHT)
        } else {
            rgb(PANEL)
        })
        .text_color(if enabled { rgb(TEXT) } else { rgb(MUTED) })
        .when(enabled, |element| {
            element
                .cursor_pointer()
                .hover(|style| style.bg(rgb(BORDER)))
                .on_click(handler)
        })
        .child(label)
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
            MUTED
        } else if keywords.contains(&word.to_ascii_uppercase().as_str()) {
            KEYWORD
        } else if piece.contains('\'') {
            STRING
        } else {
            TEXT
        };
        row = row.child(div().text_color(rgb(colour)).child(piece.to_owned()));
    }
    row
}

fn result_header(index: usize, result: &QueryResult) -> impl IntoElement {
    let count = if result.columns.is_empty() {
        format!("{} affected", result.affected_rows.unwrap_or(0))
    } else {
        format!("{} rows", result.rows.len())
    };
    div()
        .px_3()
        .py_2()
        .bg(rgb(PANEL_LIGHT))
        .text_sm()
        .child(format!(
            "Result {} · {count} · {} ms{}",
            index + 1,
            result.execution_time.as_millis(),
            result
                .automatic_limit
                .map(|limit| format!(" · Automatic LIMIT {limit} applied"))
                .unwrap_or_default()
        ))
}

fn result_table(result: &QueryResult) -> impl IntoElement {
    let mut table = div().flex().flex_col();
    if !result.columns.is_empty() {
        let mut header = div().flex().bg(rgb(PANEL_LIGHT));
        for column in &result.columns {
            header = header.child(
                div()
                    .w(px(180.))
                    .p_2()
                    .border_r_1()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(column.name.clone()),
            );
        }
        table = table.child(header);
    }
    for row in &result.rows {
        let mut rendered_row = div().flex();
        for value in row {
            rendered_row = rendered_row.child(
                div()
                    .w(px(180.))
                    .p_2()
                    .border_r_1()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .text_color(if matches!(value, CellValue::Null) {
                        rgb(MUTED)
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

fn discover_profile() -> ConnectionProfile {
    if Path::new(".env").is_file()
        && let Ok(profile) = ConnectionProfile::from_env_file(".env")
    {
        return profile;
    }
    if let Ok(Some(profile)) = ConnectionProfile::from_process_env() {
        return profile;
    }
    local_profile().unwrap_or_else(|| {
        ConnectionProfile::from_database_url(
            "postgresql://postgres@localhost:5432/postgres?sslmode=disable",
        )
        .expect("built-in PostgreSQL profile should be valid")
    })
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
