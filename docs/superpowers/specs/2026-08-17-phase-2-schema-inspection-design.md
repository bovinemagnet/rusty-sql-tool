# Phase 2 — PostgreSQL Schema Inspection: Design

**Author:** Paul Snow
**Version:** 0.0.0
**Date:** 2026-08-17
**Specifies:** [`docs/prd/phase-2-schema-inspection.md`](../../prd/phase-2-schema-inspection.md)
**Builds on:** [`docs/prd/initial-prd.md`](../../prd/initial-prd.md)

---

## 1. Scope

This design covers the whole of Phase 2: the definition model, the provider extension, every
object type in PRD §5, definition tabs, refresh, and failure handling. The implementation plan
stages it by the PRD's milestones so each is reviewable on its own.

Everything in Phase 2 is read-only. Nothing here issues DDL.

### Resolved PRD open questions (§16)

| Question | Decision | Basis |
|---|---|---|
| Should a definition tab and its object share identity? | Yes — opening an object already open focuses the existing tab. | §16 assumes yes. |
| Should the table definition offer a generated `CREATE TABLE`? | No. | §4 excludes generating `CREATE`/`ALTER` scripts. |
| Where do table sizes and row estimates belong? | Not in Phase 2. | §4 excludes statistics, sizes and bloat analysis. |

### Resolved PRD inconsistencies

* **§5 lists Index as an inspectable object, but the Phase 1 tree has no index nodes** and
  `ObjectKind` has no `Index` variant. Indexes are reachable as part of a table's definition
  (§6), which is where `pg_get_indexdef` belongs. There are no standalone index tabs in this
  phase.
* **`ObjectKind::Type` exists in the enum but the tree never renders it.** It resolves to
  `ObjectDefinition::Unsupported`, which §5 requires for any object PostgreSQL cannot
  meaningfully describe.
* **§5 gives views "structured column list plus definition SQL"**, with no mention of indexes.
  Materialised views can carry indexes, but this design follows §5 literally and omits them.
  Revisit after the phase is in use.

---

## 2. Architecture

Phase 2 extends the Phase 1 layering rather than introducing a second schema-inspection path
(FR2-011, §10, Phase 1 §59.4):

```
GPUI definition tab  ── renders DefinitionSection values, never catalogue SQL
        ↓
CommandService       ── definition(object, refresh)
        ↓
DatabaseProvider     ── async fn definition(...) -> ObjectDefinition
        ↓
PostgresProvider     ── dispatches on ObjectKind, caches per session
        ↓
catalogue.rs         ── pg_catalog queries, row → model conversion
```

### Module layout

| File | Status | Contents |
|---|---|---|
| `src/definition.rs` | new | `ObjectDefinition` and its parts; `sections()` layout. GPUI-independent, sibling of `result.rs`. |
| `src/postgres/catalogue.rs` | new | Catalogue SQL and row → model conversion. |
| `src/database.rs` | extended | One new trait method. |
| `src/postgres.rs` | extended | Definition dispatch and cache. |
| `src/application.rs` | extended | `CommandService::definition`, new command IDs. |
| `src/ui.rs` | extended | Definition tabs, tree interaction, context menu, rendering. |

`src/ui.rs` is already ~6 000 lines. Splitting it into a module directory was considered and
rejected for this phase: it is a large diff unrelated to Phase 2 behaviour and touches every
existing test path. It remains worth doing later.

---

## 3. The definition model (`src/definition.rs`)

An enum over object kinds rather than one struct of optional sections, so states such as "a
sequence with foreign keys" cannot be constructed.

```rust
pub enum ObjectDefinition {
    Table(TableDefinition),
    View(ViewDefinition),          // materialised flag inside
    Routine(RoutineDefinition),    // functions and procedures
    Sequence(SequenceDefinition),
    Unsupported { kind: ObjectKind, reason: String },
}

pub struct ColumnDefinition {
    pub position: i32,             // physical order, never sorted (§6)
    pub name: String,
    pub data_type: String,         // format_type(), e.g. varchar(200)
    pub nullable: bool,
    pub default: Option<String>,
}

pub struct TableDefinition {
    pub columns: Vec<ColumnDefinition>,
    pub primary_key: Option<KeyConstraint>,
    pub foreign_keys: Vec<ForeignKey>,
    pub unique_constraints: Vec<KeyConstraint>,
    pub check_constraints: Vec<CheckConstraint>,
    pub indexes: Vec<IndexDefinition>,
}

pub struct KeyConstraint   { pub name: String, pub columns: Vec<String> }
pub struct CheckConstraint { pub name: String, pub expression: String }
pub struct IndexDefinition { pub name: String, pub definition_sql: String,
                             pub primary: bool, pub unique: bool }

pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

pub struct ViewDefinition {
    pub columns: Vec<ColumnDefinition>,
    pub definition_sql: String,    // pg_get_viewdef
    pub materialised: bool,
}

pub struct RoutineDefinition {
    pub arguments: String,         // pg_get_function_arguments
    pub return_type: Option<String>,   // None for a procedure
    pub language: String,
    pub definition_sql: String,    // pg_get_functiondef
}

pub struct SequenceDefinition {
    pub data_type: String,
    pub start: i64,
    pub increment: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub cycles: bool,
    pub owned_by: Option<String>,
}
```

**Column order is physical order.** `ColumnDefinition::position` carries `attnum` and the
catalogue query orders by it. Nothing in the model or the UI re-sorts columns, because physical
order is what `SELECT *` returns (§6).

### Layout is part of the model

```rust
pub enum DefinitionSection {
    Rows { heading: String, lines: Vec<String> },  // pre-aligned monospace
    Sql  { heading: String, sql: String },         // read-only, highlighted
    Note { text: String },
}

impl ObjectDefinition {
    pub fn sections(&self, object: &DatabaseObject) -> Vec<DefinitionSection>;
}
```

`sections` takes the object being described because a foreign key's referenced table is qualified
only when it lives in a different schema, which cannot be decided from the definition alone.

Putting alignment and section ordering in the model — not the view — is what makes Milestone 1's
success criterion reachable: a definition can be retrieved and asserted in tests with no UI. It
also keeps the door open for §10's "rendered differently or exported" later.

A table yields `Columns`, then `Primary Key`, `Foreign Keys`, `Unique Constraints`,
`Check Constraints`, `Indexes`, matching the §6 example. Empty sections are omitted rather than
rendered as empty headings.

---

## 4. Provider boundary

One method on the existing trait (FR2-011), mirroring `objects()`:

```rust
async fn definition(&self, object: &DatabaseObject, refresh: bool)
    -> Result<ObjectDefinition, QueryError>;
```

Separate `columns()` / `constraints()` / `indexes()` methods composed by the UI were rejected:
they place per-kind knowledge in GPUI, which Phase 1 §59.4 forbids, and cost extra round-trips.
`PostgresProvider` dispatches on `object.kind` and runs only the queries that kind requires.

Catalogue queries prefer `pg_catalog` and PostgreSQL's own helpers — `pg_get_viewdef`,
`pg_get_functiondef`, `pg_get_indexdef`, `pg_get_constraintdef`, `format_type` — over
reconstructing SQL by hand (§10). Definition SQL is displayed exactly as PostgreSQL returns it;
it is never reformatted or normalised (§7).

### Caching and invalidation (§11)

A `RwLock<HashMap<DefinitionKey, ObjectDefinition>>` on `PostgresProvider`, keyed by schema,
name and kind, alongside the existing `schemas_cache` and `objects_cache`. It is cleared on
disconnect with them.

`refresh: true` bypasses and replaces the entry. Because §11 requires that refreshing a tree
branch invalidates the definitions beneath it, `objects(schema, refresh: true)` also evicts
every cached definition for that schema.

---

## 5. Definition tabs

```rust
struct DefinitionTab {
    id: Uuid,
    profile_id: Uuid,          // the connection it belongs to, not an editor
    object: DatabaseObject,    // also the identity key
    state: DefinitionState,
}

enum DefinitionState { Loading, Loaded(ObjectDefinition), Failed(QueryError) }
```

`AppView` keeps `editor` and `background_editors` unchanged and gains `definitions:
Vec<DefinitionTab>`. The existing `active_result_tab: bool` becomes:

```rust
enum Focus { Editor, Result, Definition(Uuid) }
```

a mechanical change across its 20 call sites. Tabs render in `editor_tabs` after the editor and
result segments, titled `<name> [Definition]` per §14.

A definition tab is keyed to a **profile**, not an editor, so switching or closing editors leaves
it intact — which is what §8 requires of opening a definition. Opening an object that is already
open focuses its existing tab instead of duplicating it.

Retrieval uses the existing metadata path: `cx.spawn` wrapping `runtime.spawn`, applied back on
the GPUI thread, with the tab showing `Loading` while in flight (§10, Phase 1 §16). The render
thread is never blocked.

FR2-009's refresh is an action on the tab, re-requesting with `refresh: true`. Definition tabs
close by alt-click, the gesture editor tabs and connections already use. Unlike editors, the
workspace does not need to keep one: closing the last definition tab is allowed, and focus falls
back to the active editor.

Opening a definition requires the object's connection to be live. Invoked against a disconnected
profile, the tree action reports it in the status line and opens no tab, rather than opening a
tab that can only show an error.

---

## 6. Rendering and the read-only guarantee

`Rows` sections paint as monospace lines. `Sql` sections pass through the existing
`sql::highlight_lines` and `ui::highlight_line`, which are already independent of `EditorState`
— so §7's read-only highlighted SQL reuses the Phase 1 highlighting without extracting the
editor.

FR2-007 is structural rather than a set of guards: a definition is not an `EditorState`, so Run,
Run All, Explain, cancel and the row limit have no path to reach it. No action in a definition
tab issues DDL.

**Copying (§9)** is a Copy Definition action on the definition tab, placing that tab's whole
rendered text — every section, in order — on the clipboard,
not per-character selection. §9 requires that copying be permitted, not that a selection UI
exist; the existing `ResultSelection` machinery is wired to `editor.results` and generalising it
is disproportionate here. This is a deliberate deferral.

---

## 7. Interaction

Object rows in the tree (`src/ui.rs`, currently inert) gain:

* **Double-click** — opens the definition (`ClickEvent::click_count() >= 2`).
* **Right-click** — a one-entry context menu offering **Open Definition** (FR2-012).

The context menu reuses the existing `connection_menu` popup-card pattern rather than adding new
machinery, held as `object_menu: Option<ObjectMenu>`. Database modification actions do not appear
in it (§8).

New command IDs in `application::command`, decoupled from key bindings as Phase 1 §51 requires:
`definition.open`, `definition.refresh`, `definition.close`.

---

## 8. Failure handling

One rule covers every case in §12: **a failed load or refresh puts the tab into `Failed(error)`,
replacing its body.** The tab stays open. No other tab and no connection is affected (FR2-010).

| Case | Behaviour |
|---|---|
| Permission denied on the object or a catalogue function | PostgreSQL's error shown in the tab; connection stays usable. |
| Object dropped between tree load and definition request | Error shown in the tab. |
| Object dropped before a refresh | Error replaces the body, so stale content is never shown as current (§11). |
| Connection lost while the request is in flight | Error shown in the tab; other tabs unaffected. |
| Object kind with no available definition | Not a failure — `Unsupported`, rendered as a `Note` (§5). |

Replacing the body rather than retaining it is the deliberate reading of §11's requirement not to
show stale data as if it were current. Disconnecting while a tab is open leaves the already
loaded definition readable; refreshing it then reports that the connection is closed.

---

## 9. Testing

Test-driven throughout, following the repository convention.

| Layer | Coverage |
|---|---|
| `definition.rs` | `sections()` layout and ordering, physical column order preserved, empty sections omitted, unsupported note, sequence and routine formatting. No database, no GPUI. |
| `ui.rs` | `FakeProvider` gains `definition()`. Open from tree, tab identity, refresh, failure confined to the tab while the editor keeps its contents and results, definition surviving an editor switch, context menu offering only Open Definition. |
| `tests/postgres_smoke.rs` | One new `#[ignore]`d live test walking the §14 acceptance scenario, including `ALTER TABLE … ADD COLUMN` followed by refresh. |

**Known gap:** `catalogue.rs` row → model conversion needs `tokio_postgres::Row`, which cannot be
faked cheaply. Conversion functions take plain values where practical so the mapping logic stays
testable, but the catalogue SQL itself is covered only by the live smoke test.

---

## 10. Milestones

The PRD's milestones, with what each delivers here.

| Milestone | Delivers |
|---|---|
| 1 — Definition model and provider | `definition.rs`, the trait method, `catalogue.rs` skeleton, table columns retrievable and asserted in tests with no UI. |
| 2 — Table column metadata | Columns, types, nullability, defaults; `sections()` layout for them (FR2-001). |
| 3 — Keys and constraints | Primary keys, foreign keys, unique and check constraints (FR2-002). |
| 4 — Indexes | Index list and `pg_get_indexdef` SQL (FR2-003). |
| 5 — Views and materialised views | Column list plus definition SQL in read-only highlighted form (FR2-004). |
| 6 — Functions and procedures | Arguments, return type, language, definition SQL (FR2-005). |
| 7 — Definition tabs | `Focus`, tab rendering and titles, tree double-click, context menu, refresh (FR2-006, FR2-009, FR2-012). |
| 8 — Remaining objects and stabilisation | Sequences (FR2-008), `Unsupported`, failure handling (FR2-010), cache invalidation, large-schema behaviour. |

Milestones 1–6 are provider and model work testable without UI. Milestone 7 is where the
workspace changes. Milestone 8 closes the edges.

---

## 11. Requirements traceability

| Requirement | Where satisfied |
|---|---|
| FR2-001 Table Details | §3 `TableDefinition`, `ColumnDefinition` |
| FR2-002 Constraints | §3 `KeyConstraint`, `ForeignKey`, `CheckConstraint` |
| FR2-003 Indexes | §3 `IndexDefinition` |
| FR2-004 View Definitions | §3 `ViewDefinition`, §6 highlighted SQL |
| FR2-005 Function Definitions | §3 `RoutineDefinition` |
| FR2-006 Definition Tabs | §5 |
| FR2-007 Read Only | §6 |
| FR2-008 Sequence Details | §3 `SequenceDefinition` |
| FR2-009 Definition Refresh | §4 invalidation, §5 refresh action |
| FR2-010 Definition Failures | §8 |
| FR2-011 Metadata Reuse | §2, §4 |
| FR2-012 Context Menu | §7 |
