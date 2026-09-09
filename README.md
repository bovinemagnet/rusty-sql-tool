# Rusty SQL Tool

A native, editor-first PostgreSQL client built with Rust and GPUI. Phase 1 implements the
workflow specified in [`docs/prd/initial-prd.md`](docs/prd/initial-prd.md): connect, browse
database objects, write SQL, execute or explain statements, cancel work, and inspect results.

## Run

```bash
cargo run
```

At startup the application reads a project-local `.env` when present (FR-003). Supported forms:
Copy [`.env.sample`](.env.sample) to `.env` and replace its placeholders, or create `.env`
manually using one of the following forms.

```dotenv
CONNECTION_NAME=Local Development
DATABASE_URL=postgresql://user:password@localhost:5432/database?sslmode=prefer
```

Multiple connections can be listed with matching suffixes. They are shown by name in the
Connections pane and can be assigned to the active SQL editor before connecting:

```dotenv
CONNECTION_NAME_STAGING=Staging
DATABASE_URL_STAGING=postgresql://user:password@staging.example:5432/database?sslmode=require

CONNECTION_NAME_PRODUCTION=Production Read Only
DATABASE_URL_PRODUCTION=postgresql://readonly:password@db.example:5432/database?sslmode=require
```

or:

```dotenv
CONNECTION_NAME=Local Development
PGHOST=localhost
PGPORT=5432
PGDATABASE=database
PGUSER=user
PGPASSWORD=password
PGSSLMODE=prefer
```

The app never modifies `.env`. A manual PostgreSQL URL can also be entered with the `＋` action in
the Connections pane; its contents are masked and passwords are never persisted or formatted in
logs/errors (FR-002, FR-033).

## Desktop entry and icon

GPUI has no window-icon API, so on Linux the icon reaches the window indirectly. The window
announces an application identifier — `app_id` on Wayland, `WM_CLASS` on X11 — and the desktop
environment matches it against a desktop entry, then draws that entry's `Icon`. Under Wayland
there is no way for the process to set a window icon itself, so installing both halves is the
only mechanism.

```bash
cargo build --release        # or: cargo install --path .
packaging/install-linux.sh
```

The script scales [`art/rusty_sql_icon.png`](art/rusty_sql_icon.png) into the hicolor theme at
sizes 16 through 512, installs
[`packaging/rusty-sql-tool.desktop`](packaging/rusty-sql-tool.desktop) under
`$XDG_DATA_HOME/applications`, and rewrites its `Exec` to the absolute path of the binary it
finds — a desktop entry runs with the session's `PATH`, which usually does not include
`~/.cargo/bin`. It needs ImageMagick, and installs for the current user only.

`APP_ID` in `src/ui.rs` and the entry's `StartupWMClass` have to stay identical or the icon
silently disappears, so a unit test asserts they match.

## Core behaviour

- Run uses selected SQL, then falls back to the statement containing the cursor (FR-013–FR-014).
- Run All executes statements in order and keeps earlier results when a later statement fails
  (FR-015, FR-029).
- Explain always uses plain `EXPLAIN`, never `EXPLAIN ANALYZE` (FR-016). Explain Analyze
  (⌥⇧⌘↵) is a separate command that refuses anything but a row-returning statement before it
  runs (FR3-019). Plans render as a tree, a graph or text; click a node for its detail.
- Row-returning statements receive `LIMIT 10` by default. Explicit `LIMIT`/`FETCH FIRST`, data
  changes, `RETURNING`, DDL, and uncertain statements are not rewritten (FR-018–FR-020, FR-032).
- Results support table/text rendering and pane/tab/native-window destinations (FR-021–FR-025).
- Schema/object metadata is loaded lazily and can be refreshed with `↻` (FR-006–FR-010, FR-030).

The logical command IDs are defined separately from key bindings in `application::command`, as
required by section 51. Current bindings are:

- `Ctrl/Cmd+Enter` — run the current or selected statement.
- `Ctrl/Cmd+Shift+Enter` — run all statements.
- `Ctrl/Cmd+Alt+Enter` — explain the current or selected statement.
- `Ctrl/Cmd+Alt+Shift+Enter` — Explain Analyze the current or selected statement.
- `Escape` or `Ctrl/Cmd+.` — stop a running query.
- `Ctrl/Cmd+N` — open a new SQL editor.
- `Ctrl/Cmd+W` — close the active SQL editor. Alt-click a tab to close that one. The last editor
  stays open, and an editor with a query in flight is not closed.
- `Ctrl/Cmd+Shift+D` — connect or disconnect.

Normal editor copy, cut, paste, select-all, undo, and redo shortcuts are also supported, along
with click/drag selection and `Shift` to extend any movement into a selection. Movement covers
the arrow keys and `Home`/`End` on the current line, `Ctrl/Cmd` with the arrows to step a word at
a time (a SQL identifier such as `order_line` is one word) or with `Home`/`End` to reach the ends
of the document, and `PageUp`/`PageDown` to move a viewport at a time. The editor scrolls to
follow the caret whenever it goes off screen, sideways on a long line as well as vertically, and
after an edit or an undo as well as after a movement.

Undo history belongs to the editor whose document it was recorded against, so switching tabs
carries each editor's history with it (FR-046, FR-047).

## Logging

Logs are written to standard error (section 44). Verbosity is set with `RUSTY_SQL_LOG`, which
accepts a level or a per-target filter and defaults to `info`:

```bash
RUSTY_SQL_LOG=debug cargo run
RUSTY_SQL_LOG='rusty_sql_tool=trace,tokio_postgres=warn' cargo run
```

SQL statement text is **withheld by default**, because a statement can itself contain sensitive
values. Log it in full only when you need to, with:

```bash
RUSTY_SQL_LOG=debug RUSTY_SQL_LOG_SQL=1 cargo run
```

Passwords, connection URLs containing passwords, and result rows are never logged at any level
(FR-033, sections 43 and 44). Statements log counts and durations, not their contents.

## Verify

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The provider tests are deterministic and do not require a live PostgreSQL instance. End-to-end
connection testing requires the PostgreSQL configuration described above:

```bash
RUSTY_SQL_TEST_DATABASE_URL='postgresql://user@localhost/database?sslmode=disable' \
  cargo test --test postgres_smoke -- --ignored
```
