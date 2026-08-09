# Product Requirements Document

## Rust SQL Desktop Client using GPUI

- **Working title:** RustSQL / GPUI SQL Client
- **Status:** Draft
- **Target platform:** Desktop
- **Primary implementation language:** Rust
- **UI framework:** GPUI
- **Initial database:** PostgreSQL
- **Primary inspiration:** Zed editor layout and interaction model

## 1. Product Summary

Build a fast, native desktop SQL client in Rust using GPUI for developers and DBAs to:

- Connect/disconnect from databases
- Browse database objects
- Write and execute SQL
- View query results and execution plans
- Cancel running queries

Phase 1 supports PostgreSQL only.

## 2. Product Vision

The experience should feel like **“Zed, but focused on interacting with databases and SQL.”**

Priorities:

1. Fast startup
2. Low memory usage
3. Native desktop performance
4. Keyboard-oriented workflows
5. Minimal UI clutter
6. Excellent SQL editing
7. Fast query execution
8. Clear result presentation
9. Safe defaults
10. Provider-extensible architecture

## 3. Phase 1 Goals

Users must be able to:

- Launch app and create/load PostgreSQL connections
- Load connection info from `.env`
- Connect/disconnect
- Browse database objects in a read-only tree
- Use multiple SQL editor tabs
- Execute selected/current/all SQL
- Run `EXPLAIN`
- Cancel running queries
- Use configurable automatic result row limits
- View results as table/text
- Choose result destination
- View clear errors and execution metadata

## 4. Phase 1 Non-Goals

Out of scope: other DB engines, schema/data editing UIs, visual builders/diagrams, AI SQL generation, formatting tools, migrations, scheduling, collaboration/cloud integrations, graphical EXPLAIN, and import/export tooling.

## 5. Design Principles

- **Editor First**
- **Fast & Responsive**
- **Progressive Complexity**
- **Keyboard Friendly**
- **Safe Defaults**

`EXPLAIN` must be plain `EXPLAIN`, not automatic `EXPLAIN ANALYZE`.

## 6. High-Level Layout

Zed-inspired split:

- Left: connections + object explorer (collapsible/resizable)
- Right: SQL tabs, toolbar, results
- Bottom/status area: execution state/metadata

## 7. Core Areas

1. Connections pane
2. SQL editor
3. SQL toolbar
4. Results viewer

## 8. Connection Model

Connection profiles should include host/port/database/user/password/SSL and be provider-aware:

```text
ConnectionProfile
  id
  name
  provider
  configuration
```

Phase 1 provider: `postgres`.

## 9. `.env` Support

Support at minimum:

- `DATABASE_URL=postgresql://...`
- `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD`

Must not modify `.env` automatically. Passwords loaded from `.env` must not be shown in plaintext.

## 10. Connection States

Required conceptual states:

- Disconnected
- Connecting
- Connected
- Disconnecting
- Failed

State must be visibly indicated.

## 11. Toolbar Actions

Per SQL tab:

- Connect / Disconnect
- Run (selection else current statement)
- Run All
- Explain
- Stop
- Row Limit control (see Automatic Row Limit)

## 12. Execution Semantics

- **Run:** execute selected SQL, otherwise statement at cursor
- **Run All:** execute all statements in order
- **Explain:** execute `EXPLAIN <statement>`
- **Stop:** attempt PostgreSQL cancel, keep UI responsive

Execution states should include queued/running/completed/failed/cancelling/cancelled.

## 13. Automatic Row Limit

Default **Row Limit: 10**.

This intentionally conservative default favors safety and responsiveness for ad-hoc queries.

Requirements:

- Apply only to row-returning statements (e.g., `SELECT`, `WITH ... SELECT`, `VALUES`)
- Preserve explicit SQL limits (`LIMIT`, `FETCH FIRST ...`)
- Do not append limits to non-row statements (`UPDATE`, `CREATE`, `DROP`, transaction statements)
- Use SQL-aware analysis robust to comments, literals, semicolons, CTEs, and PostgreSQL syntax

## 14. Results

Two render modes:

1. Table (default)
2. Text

Result destinations:

- Pane below editor (default)
- Results tab
- Separate native window

Must provide metadata: rows/affected rows, timing, status, and whether automatic limit was applied.

## 15. Error Handling

Show PostgreSQL error message clearly, with detail/hint/location when available. Keep editor content intact and connection usable where possible.

## 16. Metadata Explorer

Read-only object tree with lazy async loading and refresh:

- Schemas
- Tables
- Views
- Materialized views
- Functions
- Procedures (where available)
- Sequences

Metadata access should be abstracted behind a provider interface.

## 17. Architecture Requirements

Keep strong separation among:

- GPUI presentation
- App command/state layer
- DB abstraction (connect/execute/explain/cancel/metadata)
- PostgreSQL provider implementation
- Result model independent of UI renderer

Execution must run off the rendering thread.

## 18. Security & Logging

Must:

- Avoid logging passwords or full credential-bearing URLs
- Avoid exposing sensitive `.env`/credential content
- Avoid routine logging of full result sets
- Keep SQL logging configurable due to potentially sensitive values

## 19. Reliability & Performance

Must remain responsive during metadata load and query execution, handle common failure modes gracefully, and preserve editor content across failures/cancellations/disconnects.

## 20. Future Phases

Phase 2 introduces read-only object-definition inspection (table definitions and object SQL definitions) while deferring editing.
