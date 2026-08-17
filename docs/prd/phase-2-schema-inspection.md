# Product Requirements Document — Phase 2

## PostgreSQL Schema Inspection

**Working title:** RustSQL / GPUI SQL Client
**Status:** Implemented
**Phase:** 2 — PostgreSQL Schema Inspection
**Author:** Paul Snow
**Version:** 0.0.0
**Depends on:** [Phase 1 — PostgreSQL Query Client](initial-prd.md)

### Related documents

| Document | Theme |
|---|---|
| [Phase 1 — PostgreSQL Query Client](initial-prd.md) | Connect, query, inspect results |
| [Phase 3 — Developer Productivity](phase-3-developer-productivity.md) | Write SQL faster and more safely |
| [Phase 4 — Multi-Database Support](phase-4-multi-database.md) | Additional database engines |

---

# 1. Summary

Phase 1 shows the user **what objects exist**. Phase 2 shows the user **what those objects are**.

The user will be able to select an object in the Connections tree and inspect its definition: columns, types, keys, constraints, indexes, and the SQL definition where PostgreSQL provides one.

Everything in Phase 2 remains read-only. Editing database objects is not in scope for this phase and is not currently scheduled for any phase.

---

# 2. Theme

**Understand database objects.**

The typical workflow this phase serves:

```text
See a table name in a query
      ↓
Open its definition
      ↓
Read the columns and types
      ↓
Return to the SQL editor and write the query correctly
```

Today that workflow requires either memory or a `\d customer` in a separate terminal.

---

# 3. Goals

The user must be able to:

* Open the definition of a table from the Connections tree.
* See columns, data types, nullability and defaults.
* See primary keys, foreign keys, unique constraints and check constraints.
* See indexes.
* See the SQL definition of a view, materialised view, function, procedure, index or sequence.
* Open a definition as a workspace tab alongside SQL editors.
* Refresh a definition after the underlying object changes.

---

# 4. Non-Goals

Outside Phase 2 scope:

* Editing any database object.
* Generating `CREATE`/`ALTER` scripts for modification purposes.
* Schema comparison or diffing.
* Table data editing.
* Table data preview (this is a query, and belongs to the SQL editor).
* Dependency graphs between objects.
* ER diagrams.
* Statistics, table sizes or bloat analysis.
* Permissions and role inspection.

Some of these may be reconsidered after Phase 2 is in use.

---

# 5. Objects in Scope

Definitions must be available for the object types already listed in the Phase 1 tree:

| Object | Definition content |
|---|---|
| Table | Structured definition (section 6) |
| View | Structured column list plus definition SQL |
| Materialised view | Structured column list plus definition SQL |
| Function | Definition SQL, arguments, return type, language |
| Procedure | Definition SQL, arguments, language |
| Index | Definition SQL |
| Sequence | Structured properties (start, increment, min, max, cycle, owned-by) |

Where PostgreSQL cannot provide a meaningful definition for an object type, the definition view must say so rather than displaying an empty panel.

---

# 6. Table Definition

Selecting or opening a table should provide details including:

* Table name.
* Schema.
* Columns.
* Data types.
* Nullable state.
* Default values.
* Primary keys.
* Foreign keys.
* Unique constraints.
* Check constraints.
* Indexes.

Example:

```text
customer

Columns
---------------------------------------------------
id             bigint        NOT NULL
name           varchar(200)  NOT NULL
email          varchar(320)
active         boolean       NOT NULL DEFAULT true
created_at     timestamptz   NOT NULL

Primary Key
---------------------------------------------------
customer_pkey (id)

Foreign Keys
---------------------------------------------------
customer_region_fkey (region_id) → region (id)

Indexes
---------------------------------------------------
customer_email_idx (email)
```

Column ordering must match the physical column order reported by PostgreSQL, not alphabetical order, because that is the order users see in `SELECT *`.

---

# 7. Object Definition SQL

Where PostgreSQL provides a meaningful SQL representation, Phase 2 should expose definition SQL.

Examples include:

* Views.
* Materialised views.
* Functions.
* Procedures.
* Indexes.
* Sequences.

For example, opening a view displays its SQL definition in a read-only SQL editor, reusing the Phase 1 editor component and its PostgreSQL syntax highlighting.

The definition SQL is whatever PostgreSQL returns. The application must not attempt to reformat or normalise it in this phase.

---

# 8. Interaction

The database tree should support an interaction such as:

```text
customer
    Open Definition
```

or:

```text
Double-click customer
```

which opens:

```text
customer [Definition]
```

as another workspace tab.

The Phase 1 context menu gains one entry:

```text
Open Definition
```

Database modification actions must still not appear in the object-tree context menu.

Opening a definition must not disturb the SQL editor the user was working in: the definition opens as a new tab and the previous editor remains loaded with its contents and results intact.

---

# 9. Read-Only Guarantee

The definition view remains read-only in Phase 2.

Specifically:

* Definition SQL is displayed in a read-only editor.
* No action in the definition view issues DDL.
* Copying definition text is permitted.

Editing database definitions is explicitly deferred.

---

# 10. Metadata Architecture

Phase 2 must build on the Phase 1 metadata abstraction rather than introducing a second schema-inspection architecture.

Concretely:

* Definition retrieval extends the existing `DatabaseMetadataProvider` interface described in [Phase 1 section 36](initial-prd.md).
* GPUI definition views must not execute catalogue SQL directly, in line with [Phase 1 section 59.4](initial-prd.md).
* Definitions are represented by a GPUI-independent model, in the same spirit as the Phase 1 `QueryResult`, so a definition can later be rendered differently or exported.
* Catalogue queries should prefer `pg_catalog` and `information_schema`, and should prefer PostgreSQL's own helper functions (for example `pg_get_viewdef`, `pg_get_functiondef`, `pg_get_indexdef`) over reconstructing SQL by hand.

Definition retrieval is a metadata request, so [Phase 1 section 16](initial-prd.md) applies: it runs asynchronously off the rendering thread, and the UI shows a loading state while it is in flight.

---

# 11. Caching and Refresh

Definitions are cached for the connection session, consistent with Phase 1 tree caching.

A definition tab must offer a refresh action, because objects change while the tab is open.

Refreshing the Connections tree branch containing an object should invalidate the cached definition for that object.

If the object no longer exists when a refresh occurs, the definition tab must report that clearly and remain open rather than closing itself or showing stale data as if it were current.

---

# 12. Failure Handling

Definition retrieval can fail in ways ordinary tree loading does not. Phase 2 must handle at minimum:

* Permission denied on the object or on a catalogue function.
* Object dropped between tree load and definition request.
* Connection lost while the definition request is in flight.
* Object type with no available definition.

Each case shows a clear message inside the definition tab. None of them may close other tabs or invalidate the connection.

---

# 13. Functional Requirements

## FR2-001 Table Details

Users shall be able to inspect table columns, data types, nullability and default values.

## FR2-002 Constraints

Users shall be able to inspect primary keys, foreign keys, unique constraints and check constraints.

## FR2-003 Indexes

Users shall be able to inspect table indexes.

## FR2-004 View Definitions

Users shall be able to inspect view and materialised view definitions.

## FR2-005 Function Definitions

Users shall be able to inspect PostgreSQL function and procedure definitions.

## FR2-006 Definition Tabs

Database object definitions shall be openable in workspace tabs.

## FR2-007 Read Only

Database object definitions shall remain read-only in Phase 2.

## FR2-008 Sequence Details

Users shall be able to inspect sequence properties.

## FR2-009 Definition Refresh

A definition tab shall provide a refresh action that retrieves the definition again from PostgreSQL.

## FR2-010 Definition Failures

Definition retrieval failures shall be reported within the definition tab without affecting other tabs or the connection.

## FR2-011 Metadata Reuse

Definition retrieval shall extend the existing metadata provider abstraction rather than introducing a separate schema-inspection path.

## FR2-012 Context Menu

The object tree context menu shall offer an Open Definition action for supported object types.

---

# 14. Acceptance Criteria

Phase 2 is considered functionally complete when the following scenario succeeds.

Starting from a connected session as defined by the Phase 1 acceptance criteria:

The user expands `public → Tables` and opens `customer`.

A tab titled:

```text
customer [Definition]
```

opens beside the existing SQL editor, which retains its contents.

The definition shows columns in physical order with types, nullability and defaults, followed by the primary key, foreign keys and indexes.

The user opens a view from `public → Views`. Its definition SQL is displayed in a read-only editor with syntax highlighting, and cannot be edited.

The user runs `ALTER TABLE customer ADD COLUMN notes text;` in a SQL editor, then refreshes the `customer` definition tab. The new column appears.

The user opens an object for which permission is denied. The definition tab shows the PostgreSQL permission error, and the connection remains usable.

---

# 15. Milestones

Phase 2 should build on the metadata abstraction rather than introducing a second schema-inspection architecture.

## Milestone 1 — Definition Model and Provider

Extend the metadata provider with definition retrieval and add the GPUI-independent definition model.

Success criteria: a table definition can be retrieved and asserted in tests without any UI.

## Milestone 2 — Table Column Metadata

Columns, types, nullability, defaults.

## Milestone 3 — Keys and Constraints

Primary keys, foreign keys, unique constraints, check constraints.

## Milestone 4 — Indexes

Index list and index definition SQL.

## Milestone 5 — View and Materialised View Definitions

Column list plus definition SQL in a read-only editor.

## Milestone 6 — Function and Procedure Definitions

Arguments, return type, language, definition SQL.

## Milestone 7 — Definition Tabs

Tab opening, tab titles, context menu entry, double-click interaction, refresh.

## Milestone 8 — Additional Object Metadata and Stabilisation

Sequences, remaining object types, failure handling, caching behaviour, large-schema testing.

---

# 16. Open Questions

* Should a definition tab and its object share identity, so opening the same table twice focuses the existing tab rather than opening a duplicate? Assumed yes.
* Should the structured table definition also offer a generated `CREATE TABLE` representation? This is useful but risks being read as an editing feature, and PostgreSQL does not provide it directly.
* Where do table sizes and row estimates belong? They are frequently wanted alongside a definition, but they are statistics rather than structure.
