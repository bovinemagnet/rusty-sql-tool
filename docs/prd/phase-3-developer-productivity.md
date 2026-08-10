# Product Requirements Document — Phase 3

## Developer Productivity

**Working title:** RustSQL / GPUI SQL Client
**Status:** Proposal
**Phase:** 3 — Developer Productivity
**Author:** Paul Snow
**Version:** 0.0.0
**Depends on:** [Phase 1 — PostgreSQL Query Client](initial-prd.md), [Phase 2 — PostgreSQL Schema Inspection](phase-2-schema-inspection.md)

### Related documents

| Document | Theme |
|---|---|
| [Phase 1 — PostgreSQL Query Client](initial-prd.md) | Connect, query, inspect results |
| [Phase 2 — PostgreSQL Schema Inspection](phase-2-schema-inspection.md) | Understand database objects |
| [Phase 4 — Multi-Database Support](phase-4-multi-database.md) | Additional database engines |

---

# 1. Summary

Phases 1 and 2 make the application usable. Phase 3 makes it preferable.

This phase turns the SQL editor from a competent text box into an editing surface that knows about the connected database: completion, hover information, navigation, formatting and diagnostics. It also adds the workflow features a user notices missing after a week of daily use — query history, saved SQL, object search and result export.

Phase 3 is where the product claim in [Phase 1 section 61](initial-prd.md) is actually earned:

> A fast, native, editor-first database client that brings the interaction model and responsiveness of a modern code editor to SQL development.

---

# 2. Theme

**Write SQL faster, and with fewer accidents.**

---

# 3. Dependencies on Earlier Phases

Phase 3 is comparatively cheap only if the earlier architecture holds:

| Phase 3 capability | Depends on |
|---|---|
| Autocomplete, formatting, diagnostics | The SQL parsing layer from [Phase 1 section 59.3](initial-prd.md) |
| Schema-aware completion, hover, go-to-definition | The metadata provider from [Phase 1 section 36](initial-prd.md) and the definitions from Phase 2 |
| Export and copy | The GPUI-independent result model from [Phase 1 section 40](initial-prd.md) |
| Enhanced `EXPLAIN` | The rendered results surface from [Phase 1 section 32.4](initial-prd.md) |

If any of these turn out to require rework, that rework belongs at the start of this phase rather than being spread through it.

---

# 4. Goals

The user should be able to:

* Complete schema object names while typing.
* See type and definition information on hover.
* Jump from a table name in SQL to its definition.
* Format a SQL statement or document.
* See syntax problems before executing.
* Find a previously executed query.
* Save and reopen frequently used SQL.
* Search database objects by name across schemas.
* Copy and export results as CSV and JSON.
* Run `EXPLAIN ANALYZE` deliberately, and read the plan more easily.
* Mark a connection read-only or as production, and be protected accordingly.

---

# 5. Non-Goals

Outside Phase 3 scope:

* AI-assisted SQL generation.
* Additional database engines — see [Phase 4](phase-4-multi-database.md).
* Table data editing.
* Transaction management UI.
* Query history synchronisation between machines.
* Team sharing of saved queries.
* Import tooling.
* Scheduling.

---

# 6. Schema-Aware Autocomplete

Completion is the single largest productivity feature in this phase.

Requirements:

* Complete schema names, table names, view names, function names and column names.
* Use metadata already cached for the connection; completion must never block on a network round trip in the typing path.
* Restrict column suggestions to tables referenced in the current statement where the parser can determine them, including through aliases.
* Complete SQL keywords.
* Work when the editor is disconnected, degrading to keyword-only completion rather than erroring.

Completion must be cancellable and must never delay keystroke rendering. If suggestions cannot be produced within the frame budget, no popup appears.

Completion is driven by the parsing layer plus the metadata cache. It must not issue its own catalogue queries from the UI.

---

# 7. Hover Information

Hovering a table, view, column or function in the editor shows a compact summary drawn from Phase 2 metadata:

```text
customer.email
varchar(320)  NULL
Index: customer_email_idx
```

Hover is informational only and must not trigger metadata loading that blocks the editor.

---

# 8. Go To Definition

Invoking go-to-definition on an object name in the SQL editor opens the Phase 2 definition tab for that object.

Where the name is ambiguous across schemas, the application should resolve using the connection's `search_path` where it can, and otherwise offer a choice.

---

# 9. SQL Formatting

The application should format:

* The selected SQL.
* The statement containing the cursor.
* The whole document.

Formatting must be a pure transformation of the SQL text. It must never execute anything, and it must preserve comments.

If a statement cannot be parsed, formatting leaves it untouched and reports why, rather than emitting mangled SQL.

---

# 10. SQL Diagnostics

Diagnostics report problems before execution:

* Syntax errors from the parsing layer.
* Unknown schema, table or column names, where metadata is loaded and the reference is unambiguous.

Diagnostics must be advisory. They never prevent execution — the database is the authority, and the parser may lag PostgreSQL syntax.

Unknown-object diagnostics must be suppressed when metadata is not loaded, to avoid a wall of false positives immediately after connecting.

---

# 11. Query History

Every executed statement is recorded locally with:

* The SQL text.
* The connection name and database.
* Start time and duration.
* Status: completed, failed or cancelled.
* Rows returned or affected.

Requirements:

* History is searchable by text and filterable by connection.
* A history entry can be opened into a new SQL editor.
* History is stored locally only.
* History storage must respect [Phase 1 section 43](initial-prd.md): SQL may contain sensitive literal values, so history must be excludable by configuration, and clearable by the user.

---

# 12. Saved Queries

The user can save a SQL document under a name and reopen it later.

Saved queries are local files where practical, so they can live in a project directory and be version-controlled by the user's own tooling. The application does not add Git integration.

---

# 13. Database Object Search

A search action finds database objects by name across schemas for the active connection.

```text
Search objects: custo
  public.customer            Table
  public.customer_orders_v   View
  audit.customer_history     Table
```

Selecting a result reveals it in the Connections tree and offers to open its definition.

Search operates over the metadata cache where populated, and falls back to a catalogue query for names not yet loaded, because [Phase 1 section 16](initial-prd.md) means most of the tree is unloaded most of the time.

---

# 14. Result Copy and Export

The result grid should support:

* Copying selected cells or rows to the clipboard.
* Copying a result as CSV, JSON or Markdown table.
* Exporting a result to a CSV file.
* Exporting a result to a JSON file.

Export operates on the result model, not on the rendered grid, so exported values match the values PostgreSQL returned rather than the strings displayed.

Export must state clearly whether it is exporting the limited result or re-running the query without the row limit. Phase 3 exports what is in the result. Re-running to export more rows is a separate, explicit action.

---

# 15. Enhanced EXPLAIN

Phase 1 deliberately restricts `EXPLAIN` to the plain form. Phase 3 adds the rest, deliberately.

## 15.1 EXPLAIN ANALYZE

`EXPLAIN ANALYZE` becomes available as a distinct, explicitly chosen action — never as the default Explain behaviour.

Because `ANALYZE` executes the statement:

* It must be a separate command with its own command ID.
* It must warn before running against a statement that is not a plain row-returning query.
* It must be blocked entirely on connections marked read-only or production (section 17).

## 15.2 Plan Rendering

Plan output moves from raw text into a structured plan view using the rendered results surface:

* Node tree with per-node cost and row estimates.
* Actual versus estimated rows where `ANALYZE` was used.
* Highlighting of the most expensive nodes.

The raw text plan must remain available.

---

# 16. SQL Snippets

Reusable SQL snippets with placeholder expansion, invoked from completion.

This is deliberately the smallest feature in the phase and should be dropped first if the phase needs to shrink.

---

# 17. Read-Only and Production Protection

[Phase 1 section 50](initial-prd.md) accepts that visible connection identity is the only safeguard against running destructive SQL against the wrong database. Phase 3 adds real protection.

A connection profile gains two optional flags:

| Flag | Effect |
|---|---|
| Read-only | Statements that modify data or schema are refused before execution. `EXPLAIN ANALYZE` is refused. |
| Production | Modifying statements require an explicit confirmation before execution, and the editor displays a distinct visual treatment. |

Requirements:

* Classification of a statement as modifying uses the same parsing layer as limit injection, and fails closed: if the statement cannot be classified confidently on a protected connection, it is treated as modifying.
* Refusal happens in the application, before the statement reaches the database. Where PostgreSQL also offers a read-only session, using it as well is preferable, but it does not replace the application-level check.
* The flags are per profile, not per editor, so they cannot be casually toggled while working.

---

# 18. PostgreSQL Notices

Server notices raised during execution (for example `RAISE NOTICE` output) are captured into the result model's notices field and displayed alongside the result.

---

# 19. Multiple Active Result Sets

Results from Run All, and results from separate editors, can be retained and switched between rather than each execution replacing the last.

The user must be able to see which result belongs to which statement and editor.

---

# 20. Functional Requirements

## FR3-001 Keyword Completion

The editor shall complete SQL keywords.

## FR3-002 Schema-Aware Completion

The editor shall complete schema, table, view, function and column names from connection metadata.

## FR3-003 Alias-Aware Column Completion

Column completion shall respect table aliases within the current statement where they can be determined.

## FR3-004 Non-Blocking Completion

Completion shall never block keystroke handling or issue a synchronous database request.

## FR3-005 Hover Information

The editor shall display type and structural information on hover for recognised objects.

## FR3-006 Go To Definition

The editor shall open the Phase 2 definition tab for a recognised object name.

## FR3-007 Formatting

The application shall format the selection, the current statement, or the whole document.

## FR3-008 Format Safety

Formatting shall preserve comments and shall leave unparsable statements unmodified.

## FR3-009 Diagnostics

The editor shall report syntax problems without preventing execution.

## FR3-010 Query History

The application shall record executed statements with connection, timing, status and row counts.

## FR3-011 History Reuse

A history entry shall be openable into a SQL editor.

## FR3-012 History Control

Query history shall be disableable and clearable by the user.

## FR3-013 Saved Queries

The application shall save and reopen named SQL documents.

## FR3-014 Object Search

The application shall search database objects by name across schemas.

## FR3-015 Result Copy

The application shall copy selected result cells and rows to the clipboard.

## FR3-016 CSV Export

The application shall export a result as CSV.

## FR3-017 JSON Export

The application shall export a result as JSON.

## FR3-018 Export Fidelity

Export shall derive values from the result model rather than the rendered grid.

## FR3-019 Explain Analyze

The application shall provide `EXPLAIN ANALYZE` as a separate, explicitly invoked action.

## FR3-020 Plan View

The application shall render execution plans as a structured node tree, retaining access to the raw text plan.

## FR3-021 Read-Only Profiles

A connection profile shall support a read-only flag that refuses modifying statements before execution.

## FR3-022 Production Profiles

A connection profile shall support a production flag that requires confirmation for modifying statements.

## FR3-023 Fail Closed

On a protected connection, a statement that cannot be confidently classified shall be treated as modifying.

## FR3-024 Notices

The application shall display PostgreSQL notices raised during execution.

## FR3-025 Multiple Results

The application shall retain multiple result sets and allow the user to switch between them.

## FR3-026 Snippets

The application shall support reusable SQL snippets with placeholder expansion.

---

# 21. Acceptance Criteria

Phase 3 is considered functionally complete when the following scenario succeeds.

The user connects to a database containing `public.customer`.

Typing `SELECT * FROM cus` offers `public.customer`. Accepting it and typing `WHERE c.` after aliasing the table as `c` offers that table's columns. Typing continues without perceptible latency throughout.

Hovering `email` shows its type and nullability.

Go-to-definition on `customer` opens the Phase 2 definition tab.

Formatting the document reflows the SQL and leaves comments intact. Introducing a syntax error produces a diagnostic without blocking Run.

Running the query records an entry in history, which the user can find by searching for `customer` and reopen in a new editor.

The result is exported to CSV, and the exported values match the values returned by PostgreSQL.

`EXPLAIN ANALYZE` is invoked deliberately from its own action and renders a plan tree showing estimated and actual rows.

The connection is then marked read-only. `DELETE FROM customer;` is refused before reaching the database, and `EXPLAIN ANALYZE` is refused. `SELECT` still runs.

---

# 22. Milestones

## Milestone 1 — Parsing Layer Hardening

Extend the Phase 1 parsing layer to expose the statement structure that completion, formatting, diagnostics and statement classification need.

Success criteria: aliases, referenced tables and statement kind can be extracted and unit tested without a database.

## Milestone 2 — Completion

Keyword completion, then schema-aware completion, then alias-aware column completion.

## Milestone 3 — Hover and Go To Definition

## Milestone 4 — Formatting and Diagnostics

## Milestone 5 — Query History and Saved Queries

## Milestone 6 — Object Search

## Milestone 7 — Copy and Export

## Milestone 8 — Enhanced EXPLAIN

`EXPLAIN ANALYZE` action, plan model, plan tree renderer.

## Milestone 9 — Connection Protection

Read-only and production profiles, statement classification, fail-closed behaviour.

## Milestone 10 — Notices, Multiple Results, Snippets and Stabilisation

---

# 23. Deferred Beyond Phase 3

Recorded so that they are not lost, but not scheduled:

* Editable table data.
* Transaction controls.
* Incremental fetch, virtualised tables and true pagination for very large results — the architecture allows for these under [Phase 1 section 41](initial-prd.md), but they need a driving use case.

---

# 24. Open Questions

* Should query history be per connection profile or global with a filter? Global with a filter is assumed.
* Should saved queries be plain `.sql` files in a user-chosen directory, or an application-managed store? Plain files are assumed, to keep the application out of the way of version control.
* Does formatting need to be configurable in this phase, or is one opinionated style enough to start? One style is assumed.
