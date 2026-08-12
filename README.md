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

## Core behaviour

- Run uses selected SQL, then falls back to the statement containing the cursor (FR-013–FR-014).
- Run All executes statements in order and keeps earlier results when a later statement fails
  (FR-015, FR-029).
- Explain always uses plain `EXPLAIN`, never `EXPLAIN ANALYZE` (FR-016).
- Row-returning statements receive `LIMIT 10` by default. Explicit `LIMIT`/`FETCH FIRST`, data
  changes, `RETURNING`, DDL, and uncertain statements are not rewritten (FR-018–FR-020, FR-032).
- Results support table/text rendering and pane/tab/native-window destinations (FR-021–FR-025).
- Schema/object metadata is loaded lazily and can be refreshed with `↻` (FR-006–FR-010, FR-030).

The logical command IDs are defined separately from key bindings in `application::command`, as
required by section 51. Current bindings are:

- `Ctrl/Cmd+Enter` — run the current or selected statement.
- `Ctrl/Cmd+Shift+Enter` — run all statements.
- `Ctrl/Cmd+Alt+Enter` — explain the current or selected statement.
- `Escape` or `Ctrl/Cmd+.` — stop a running query.
- `Ctrl/Cmd+N` — open a new SQL editor.
- `Ctrl/Cmd+Shift+D` — connect or disconnect.

Normal editor copy, cut, paste, select-all, undo, and redo shortcuts are also supported.

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
