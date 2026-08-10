# Rusty SQL Tool

A native, editor-first PostgreSQL client built with Rust and GPUI. Phase 1 implements the
workflow specified in [`docs/prd/initial-prd.md`](docs/prd/initial-prd.md): connect, browse
database objects, write SQL, execute or explain statements, cancel work, and inspect results.

## Run

```bash
cargo run
```

At startup the application reads a project-local `.env` when present (FR-003). Supported forms:

```dotenv
DATABASE_URL=postgresql://user:password@localhost:5432/database?sslmode=prefer
```

or:

```dotenv
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
required by section 51. `Ctrl/Cmd+Enter` runs the current statement; normal editor copy, cut,
paste, select-all, undo, and redo shortcuts are supported.

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
