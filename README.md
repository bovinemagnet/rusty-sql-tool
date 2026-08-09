# rusty-sql-tool (RustSQL / GPUI SQL Client)

A fast, native desktop SQL client written in Rust with GPUI, focused on PostgreSQL in Phase 1.

## Status

Draft implementation repository. This project is currently driven by a product requirements document.

## Product Requirements

See the full specification in:

- `docs/prd/initial-prd.md`

## Phase 1 Summary

Phase 1 targets a keyboard-friendly PostgreSQL SQL client with:

- Connection profile support (including `.env` loading)
- Connect / disconnect lifecycle and visible connection states
- Read-only metadata explorer (lazy-loaded schemas/objects)
- SQL editor tabs with syntax highlighting
- Execute selected/current/all SQL
- `EXPLAIN` (without automatic `EXPLAIN ANALYZE`)
- Query cancellation support
- Safe automatic row-limit behavior with SQL-aware limit injection
- Table and text result viewers
- Query metadata, status, and error presentation

## Scope Notes

- PostgreSQL only in Phase 1
- No schema/data editing UI in Phase 1
- Architecture should support additional providers in later phases
