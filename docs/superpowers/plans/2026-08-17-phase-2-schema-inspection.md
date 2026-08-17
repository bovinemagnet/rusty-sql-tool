# Phase 2 Schema Inspection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user open any object in the connections tree and read its definition — columns, keys, constraints, indexes, and the SQL PostgreSQL provides — in a read-only workspace tab.

**Architecture:** A GPUI-independent `ObjectDefinition` model in `src/definition.rs` renders itself to `DefinitionSection` values, so layout is unit-testable without a database or a window. One new `DatabaseProvider::definition` method dispatches on object kind to catalogue queries in `src/postgres/catalogue.rs`. `AppView` keeps its editors untouched and gains a parallel `definitions: Vec<DefinitionTab>` list plus a `Focus` enum replacing `active_result_tab: bool`.

**Tech Stack:** Rust edition 2024, GPUI 0.2.2, tokio-postgres, async-trait, tokio runtime owned by `AppView`.

**Spec:** [`docs/superpowers/specs/2026-08-17-phase-2-schema-inspection-design.md`](../specs/2026-08-17-phase-2-schema-inspection-design.md)

## Global Constraints

- **British spelling** in code, comments, and documentation (`MaterialisedView`, `materialised`, not `Materialized`).
- **Author is Paul Snow; version 0.0.0.**
- **Cite the requirement** being satisfied in doc comments, e.g. `FR2-001`, `§6`. Requirement identifiers are stable; section numbers are not.
- **TDD**: write the failing test, watch it fail, implement minimally, watch it pass, commit.
- **Everything in Phase 2 is read-only.** No task may issue DDL or add an editing path.
- **GPUI never sees a driver type.** Catalogue SQL lives only in `src/postgres/`; `src/ui.rs` renders `DefinitionSection` values only.
- **Credentials are never logged.** Log counts, kinds and timings — never object contents or connection details.
- **After every task**: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test` must all be clean before committing.
- **Column order is physical order** (`attnum`). Nothing sorts columns alphabetically, anywhere.

## Formatting rules used throughout

`DefinitionSection::Rows` lines are pre-aligned by the model with this exact rule, used by every task that adds a section:

- Each column of the row is padded with spaces to `max_width_in_that_column + 2`.
- The final field is not padded; the whole line is then `trim_end()`ed.

The PRD's §6 example is illustrative, not a literal expected output — these tests assert the rule above.

## File Structure

| File | Responsibility |
|---|---|
| `src/definition.rs` (new) | `ObjectDefinition` and its parts; `DefinitionSection`; `sections()` layout. No GPUI, no driver types. |
| `src/postgres/catalogue.rs` (new) | Catalogue SQL constants and row → model conversion. |
| `src/postgres.rs` | Adds `definition()` dispatch and the definition cache. Becomes `mod catalogue;`'s parent. |
| `src/database.rs` | One new trait method. |
| `src/application.rs` | `CommandService::definition`; three new command IDs. |
| `src/lib.rs` | Registers `mod definition;`. |
| `src/ui.rs` | `DefinitionTab`, `Focus`, tab rendering, tree interaction, context menu, definition surface. |
| `tests/postgres_smoke.rs` | One new `#[ignore]`d live acceptance test. |

`src/postgres.rs` becomes a module with a child. Rust allows `src/postgres.rs` alongside `src/postgres/catalogue.rs` in edition 2024 — no `mod.rs` rename is needed.

---

### Task 1: Definition model skeleton and the unsupported case

**Files:**
- Create: `src/definition.rs`
- Modify: `src/lib.rs`
- Test: `src/definition.rs` (`mod tests` at the foot, matching the repository convention)

**Interfaces:**
- Consumes: `crate::database::{DatabaseObject, ObjectKind}`
- Produces: `ObjectDefinition`, `DefinitionSection`, `ObjectDefinition::sections(&self, object: &DatabaseObject) -> Vec<DefinitionSection>`

- [ ] **Step 1: Write the failing test**

Add to `src/definition.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::ObjectKind;

    fn object(name: &str, kind: ObjectKind) -> DatabaseObject {
        DatabaseObject {
            schema: "public".into(),
            name: name.into(),
            kind,
        }
    }

    /// §5: an object PostgreSQL cannot describe says so, rather than rendering an empty panel.
    #[test]
    fn an_unsupported_object_renders_a_note_rather_than_nothing() {
        let definition = ObjectDefinition::Unsupported {
            kind: ObjectKind::Type,
            reason: "PostgreSQL provides no definition for this object".into(),
        };

        let sections = definition.sections(&object("mood", ObjectKind::Type));

        assert_eq!(
            sections,
            vec![DefinitionSection::Note {
                text: "PostgreSQL provides no definition for this object".into(),
            }]
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib definition::tests::an_unsupported_object_renders_a_note_rather_than_nothing`
Expected: FAIL — the crate does not compile, `definition` module does not exist.

- [ ] **Step 3: Write minimal implementation**

Create `src/definition.rs`:

```rust
//! What an object *is*, as opposed to the fact that it exists (Phase 2 §10).
//!
//! GPUI-independent by design, in the same spirit as `QueryResult`: the model decides its own
//! layout so a definition can be asserted in tests with no window, and rendered differently or
//! exported later.

use crate::database::{DatabaseObject, ObjectKind};

/// The definition of one database object. An enum rather than a struct of optional sections, so
/// states such as a sequence carrying foreign keys cannot be constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectDefinition {
    /// §5: the object exists but PostgreSQL offers no meaningful definition for its kind.
    Unsupported { kind: ObjectKind, reason: String },
}

/// One block of a rendered definition. `Rows` is pre-aligned monospace text; `Sql` is handed to
/// the editor's highlighter (§7); `Note` explains an absence (§5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionSection {
    Rows { heading: String, lines: Vec<String> },
    Sql { heading: String, sql: String },
    Note { text: String },
}

impl ObjectDefinition {
    /// The definition laid out for display. Takes the object because a foreign key's referenced
    /// table is qualified only when it lives in a different schema from the object being shown.
    pub fn sections(&self, _object: &DatabaseObject) -> Vec<DefinitionSection> {
        match self {
            Self::Unsupported { reason, .. } => vec![DefinitionSection::Note {
                text: reason.clone(),
            }],
        }
    }
}
```

Add to `src/lib.rs`, in the existing module list, alphabetically among the others:

```rust
pub mod definition;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib definition::`
Expected: PASS, 1 test.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/definition.rs src/lib.rs
git commit -m "Add the definition model skeleton"
```

---

### Task 2: Table columns

**Files:**
- Modify: `src/definition.rs`
- Test: `src/definition.rs` (`mod tests`)

**Interfaces:**
- Consumes: `ObjectDefinition`, `DefinitionSection` from Task 1
- Produces: `ColumnDefinition { position: i32, name: String, data_type: String, nullable: bool, default: Option<String> }`, `TableDefinition { columns: Vec<ColumnDefinition> }`, `ObjectDefinition::Table(TableDefinition)`

`TableDefinition` gains further fields in Tasks 4 and 5. Construct it with `..TableDefinition::default()` in tests so those tasks do not have to edit these tests.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/definition.rs`:

```rust
fn column(position: i32, name: &str, data_type: &str, nullable: bool, default: Option<&str>)
    -> ColumnDefinition
{
    ColumnDefinition {
        position,
        name: name.into(),
        data_type: data_type.into(),
        nullable,
        default: default.map(ToOwned::to_owned),
    }
}

/// FR2-001. The alignment rule is max-width-plus-two per column, with the line right-trimmed.
#[test]
fn table_columns_render_aligned_with_nullability_and_defaults() {
    let definition = ObjectDefinition::Table(TableDefinition {
        columns: vec![
            column(1, "id", "bigint", false, None),
            column(2, "name", "varchar(200)", false, None),
            column(3, "email", "varchar(320)", true, None),
            column(4, "active", "boolean", false, Some("true")),
        ],
        ..TableDefinition::default()
    });

    let sections = definition.sections(&object("customer", ObjectKind::Table));

    assert_eq!(
        sections,
        vec![DefinitionSection::Rows {
            heading: "Columns".into(),
            lines: vec![
                "id      bigint        NOT NULL".into(),
                "name    varchar(200)  NOT NULL".into(),
                "email   varchar(320)".into(),
                "active  boolean       NOT NULL DEFAULT true".into(),
            ],
        }]
    );
}

/// §6: physical order is what `SELECT *` returns, so the model never sorts.
#[test]
fn table_columns_keep_physical_order_rather_than_alphabetical() {
    let definition = ObjectDefinition::Table(TableDefinition {
        columns: vec![
            column(1, "zebra", "text", true, None),
            column(2, "alpha", "text", true, None),
        ],
        ..TableDefinition::default()
    });

    let DefinitionSection::Rows { lines, .. } = &definition
        .sections(&object("beasts", ObjectKind::Table))[0]
    else {
        panic!("expected a rows section");
    };

    assert!(lines[0].starts_with("zebra"));
    assert!(lines[1].starts_with("alpha"));
}

/// Empty sections are omitted rather than rendered as a bare heading.
#[test]
fn a_table_with_no_columns_yields_no_sections() {
    let definition = ObjectDefinition::Table(TableDefinition::default());

    assert!(definition.sections(&object("empty", ObjectKind::Table)).is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib definition::tests::table_columns_render_aligned_with_nullability_and_defaults`
Expected: FAIL — `TableDefinition` and `ColumnDefinition` are not defined.

- [ ] **Step 3: Write minimal implementation**

In `src/definition.rs`, add the variant and types, and the alignment helper:

```rust
/// One column, in the physical order PostgreSQL reports (§6). `position` carries `attnum` so the
/// order survives any later regrouping; nothing in the model or the UI re-sorts columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDefinition {
    pub position: i32,
    pub name: String,
    /// As `format_type` renders it, e.g. `varchar(200)`.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableDefinition {
    pub columns: Vec<ColumnDefinition>,
}
```

Add `Table(TableDefinition),` to `ObjectDefinition`, and extend `sections`:

```rust
    pub fn sections(&self, _object: &DatabaseObject) -> Vec<DefinitionSection> {
        match self {
            Self::Table(table) => {
                let mut sections = Vec::new();
                push_rows(&mut sections, "Columns", column_lines(&table.columns));
                sections
            }
            Self::Unsupported { reason, .. } => vec![DefinitionSection::Note {
                text: reason.clone(),
            }],
        }
    }
```

And the free functions:

```rust
/// Pads every field but the last to the widest value in its column plus two spaces, then trims
/// the trailing run. One rule, used by every `Rows` section, so the blocks line up with each other.
fn aligned(rows: &[Vec<String>]) -> Vec<String> {
    let width = rows.iter().map(Vec::len).max().unwrap_or_default();
    let widths: Vec<usize> = (0..width)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(String::len)
                .max()
                .unwrap_or_default()
                + 2
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (index, field) in row.iter().enumerate() {
                if index + 1 == row.len() {
                    line.push_str(field);
                } else {
                    line.push_str(&format!("{field:<width$}", width = widths[index]));
                }
            }
            line.trim_end().to_owned()
        })
        .collect()
}

/// Skips the section entirely when it has no lines, so no bare heading is ever rendered.
fn push_rows(sections: &mut Vec<DefinitionSection>, heading: &str, lines: Vec<String>) {
    if !lines.is_empty() {
        sections.push(DefinitionSection::Rows {
            heading: heading.to_owned(),
            lines,
        });
    }
}

fn column_lines(columns: &[ColumnDefinition]) -> Vec<String> {
    let rows: Vec<Vec<String>> = columns
        .iter()
        .map(|column| {
            let mut suffix = String::new();
            if !column.nullable {
                suffix.push_str("NOT NULL");
            }
            if let Some(default) = &column.default {
                if !suffix.is_empty() {
                    suffix.push(' ');
                }
                suffix.push_str(&format!("DEFAULT {default}"));
            }
            vec![column.name.clone(), column.data_type.clone(), suffix]
        })
        .collect();
    aligned(&rows)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib definition::`
Expected: PASS, 4 tests.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/definition.rs
git commit -m "Render table columns in physical order"
```

---

### Task 3: The provider method, the catalogue module, and the cache

This is the vertical slice that makes Milestone 1's criterion true: a table definition retrieved from PostgreSQL with no UI involved.

**Files:**
- Create: `src/postgres/catalogue.rs`
- Modify: `src/database.rs`, `src/postgres.rs`, `src/application.rs`
- Test: `src/application.rs` (`mod tests` — the `FakeProvider` must implement the new method)

**Interfaces:**
- Consumes: `ObjectDefinition`, `TableDefinition`, `ColumnDefinition` from Tasks 1–2
- Produces:
  - `DatabaseProvider::definition(&self, object: &DatabaseObject, refresh: bool) -> Result<ObjectDefinition, QueryError>`
  - `CommandService::definition(&self, object: &DatabaseObject, refresh: bool) -> Result<ObjectDefinition, QueryError>`
  - `catalogue::COLUMNS: &str`, `catalogue::columns(rows: &[tokio_postgres::Row]) -> Vec<ColumnDefinition>`

- [ ] **Step 1: Write the failing test**

In `src/application.rs` `mod tests`, add to `impl DatabaseProvider for FakeProvider`:

```rust
        async fn definition(
            &self,
            object: &DatabaseObject,
            _refresh: bool,
        ) -> Result<ObjectDefinition, QueryError> {
            Ok(ObjectDefinition::Table(TableDefinition {
                columns: vec![ColumnDefinition {
                    position: 1,
                    name: "id".into(),
                    data_type: "bigint".into(),
                    nullable: false,
                    default: None,
                }],
                ..TableDefinition::default()
            }))
        }
```

with `use crate::definition::{ColumnDefinition, ObjectDefinition, TableDefinition};` added to the test module's imports, and add this test:

```rust
    /// FR2-011: definitions travel the same command path as every other metadata request.
    #[tokio::test]
    async fn definitions_are_retrieved_through_the_command_service() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider);
        let object = DatabaseObject {
            schema: "public".into(),
            name: "customer".into(),
            kind: ObjectKind::Table,
        };

        let definition = service.definition(&object, false).await.unwrap();

        let ObjectDefinition::Table(table) = definition else {
            panic!("expected a table definition");
        };
        assert_eq!(table.columns[0].name, "id");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib application::tests::definitions_are_retrieved_through_the_command_service`
Expected: FAIL — `CommandService::definition` does not exist and the trait has no such method.

- [ ] **Step 3: Write minimal implementation**

In `src/database.rs`, add the import and the trait method:

```rust
use crate::definition::ObjectDefinition;
```

```rust
    /// The definition of one object (FR2-011). `refresh` bypasses the session cache, exactly as it
    /// does for `schemas` and `objects`.
    async fn definition(
        &self,
        object: &DatabaseObject,
        refresh: bool,
    ) -> Result<ObjectDefinition, QueryError>;
```

Create `src/postgres/catalogue.rs`:

```rust
//! Catalogue queries and their conversion into the definition model.
//!
//! §10 prefers `pg_catalog` and PostgreSQL's own helpers — `format_type`, `pg_get_expr` — over
//! reconstructing SQL by hand. Nothing here reaches GPUI, and nothing here formats for display.

use tokio_postgres::Row;

use crate::definition::ColumnDefinition;

/// Columns in physical order (§6). `attnum > 0` excludes system columns; `attisdropped` excludes
/// the tombstones a dropped column leaves behind.
pub const COLUMNS: &str = "SELECT a.attnum, a.attname, \
     pg_catalog.format_type(a.atttypid, a.atttypmod), \
     a.attnotnull, \
     pg_catalog.pg_get_expr(d.adbin, d.adrelid) \
     FROM pg_catalog.pg_attribute a \
     JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
     WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped \
     ORDER BY a.attnum";

pub fn columns(rows: &[Row]) -> Vec<ColumnDefinition> {
    rows.iter()
        .map(|row| {
            // attnum is int2, so it arrives as i16 and widens rather than truncates.
            let position: i16 = row.get(0);
            ColumnDefinition {
                position: i32::from(position),
                name: row.get(1),
                data_type: row.get(2),
                nullable: !row.get::<_, bool>(3),
                default: row.get(4),
            }
        })
        .collect()
}
```

In `src/postgres.rs`, declare the child module at the top, beneath the existing `use` statements:

```rust
mod catalogue;
```

Add the cache key and field:

```rust
/// Identity of a cached definition. Kind is part of the key because a table and a function may
/// share a name within one schema.
#[derive(Clone, PartialEq, Eq, Hash)]
struct DefinitionKey {
    schema: String,
    name: String,
    kind: ObjectKind,
}

impl DefinitionKey {
    fn of(object: &DatabaseObject) -> Self {
        Self {
            schema: object.schema.clone(),
            name: object.name.clone(),
            kind: object.kind,
        }
    }
}
```

Add `definitions_cache: RwLock<HashMap<DefinitionKey, ObjectDefinition>>` to `PostgresProvider` and `definitions_cache: RwLock::new(HashMap::new())` to its `Default`. In `disconnect`, beside the existing cache clears:

```rust
        self.definitions_cache.write().await.clear();
```

Add the trait implementation:

```rust
    async fn definition(
        &self,
        object: &DatabaseObject,
        refresh: bool,
    ) -> Result<ObjectDefinition, QueryError> {
        let key = DefinitionKey::of(object);
        if !refresh
            && let Some(cached) = self.definitions_cache.read().await.get(&key).cloned()
        {
            return Ok(cached);
        }
        let definition = self.load_definition(object).await?;
        // Kind and counts only: an object's contents are database content, which §44 keeps out of
        // the log.
        tracing::debug!(
            schema = %object.schema,
            kind = ?object.kind,
            refresh,
            "loaded object definition"
        );
        self.definitions_cache
            .write()
            .await
            .insert(key, definition.clone());
        Ok(definition)
    }
```

And the dispatcher, in the inherent `impl PostgresProvider` block:

```rust
    /// Runs only the catalogue queries the object's kind requires. Later tasks add arms; anything
    /// still unhandled reports itself rather than rendering blank (§5).
    async fn load_definition(
        &self,
        object: &DatabaseObject,
    ) -> Result<ObjectDefinition, QueryError> {
        match object.kind {
            ObjectKind::Table => {
                let columns = self
                    .with_client(async |client| {
                        client
                            .query(catalogue::COLUMNS, &[&object.schema, &object.name])
                            .await
                    })
                    .await?;
                Ok(ObjectDefinition::Table(TableDefinition {
                    columns: catalogue::columns(&columns),
                    ..TableDefinition::default()
                }))
            }
            kind => Ok(ObjectDefinition::Unsupported {
                kind,
                reason: "PostgreSQL provides no definition for this object".into(),
            }),
        }
    }
```

with `use crate::definition::{ObjectDefinition, TableDefinition};` added to the imports of `src/postgres.rs`.

In `src/application.rs`, add `use crate::definition::ObjectDefinition;` and the pass-through:

```rust
    pub async fn definition(
        &self,
        object: &DatabaseObject,
        refresh: bool,
    ) -> Result<ObjectDefinition, QueryError> {
        self.provider.definition(object, refresh).await
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib application::`
Expected: PASS. `src/ui.rs`'s `UiTestProvider` will now fail to compile — add the same stub there, returning `ObjectDefinition::Unsupported { kind: object.kind, reason: "not stubbed".into() }`, so the crate builds. Task 9 replaces it with a real stub.

Run: `cargo test`
Expected: PASS, all existing tests still green.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/database.rs src/postgres.rs src/postgres/catalogue.rs src/application.rs src/ui.rs
git commit -m "Retrieve table column definitions through the provider"
```

---

### Task 4: Keys and constraints

**Files:**
- Modify: `src/definition.rs`, `src/postgres/catalogue.rs`, `src/postgres.rs`
- Test: `src/definition.rs` (`mod tests`)

**Interfaces:**
- Consumes: `TableDefinition` from Task 2
- Produces: `KeyConstraint { name, columns }`, `CheckConstraint { name, expression }`, `ForeignKey { name, columns, referenced_schema, referenced_table, referenced_columns }`; `TableDefinition` fields `primary_key: Option<KeyConstraint>`, `foreign_keys: Vec<ForeignKey>`, `unique_constraints: Vec<KeyConstraint>`, `check_constraints: Vec<CheckConstraint>`; `catalogue::CONSTRAINTS`, `catalogue::constraints(rows) -> Constraints`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/definition.rs`:

```rust
/// FR2-002. A referenced table in the same schema is unqualified; one elsewhere is qualified,
/// which is why `sections` needs the object it is describing.
#[test]
fn constraints_render_in_their_own_sections() {
    let definition = ObjectDefinition::Table(TableDefinition {
        columns: vec![column(1, "id", "bigint", false, None)],
        primary_key: Some(KeyConstraint {
            name: "customer_pkey".into(),
            columns: vec!["id".into()],
        }),
        foreign_keys: vec![
            ForeignKey {
                name: "customer_region_fkey".into(),
                columns: vec!["region_id".into()],
                referenced_schema: "public".into(),
                referenced_table: "region".into(),
                referenced_columns: vec!["id".into()],
            },
            ForeignKey {
                name: "customer_tenant_fkey".into(),
                columns: vec!["tenant_id".into()],
                referenced_schema: "billing".into(),
                referenced_table: "tenant".into(),
                referenced_columns: vec!["id".into()],
            },
        ],
        unique_constraints: vec![KeyConstraint {
            name: "customer_email_key".into(),
            columns: vec!["email".into()],
        }],
        check_constraints: vec![CheckConstraint {
            name: "customer_email_check".into(),
            expression: "CHECK ((email <> ''::text))".into(),
        }],
        ..TableDefinition::default()
    });

    let sections = definition.sections(&object("customer", ObjectKind::Table));
    let headings: Vec<&str> = sections
        .iter()
        .map(|section| match section {
            DefinitionSection::Rows { heading, .. } => heading.as_str(),
            DefinitionSection::Sql { heading, .. } => heading.as_str(),
            DefinitionSection::Note { .. } => "",
        })
        .collect();

    assert_eq!(
        headings,
        vec![
            "Columns",
            "Primary Key",
            "Foreign Keys",
            "Unique Constraints",
            "Check Constraints"
        ]
    );

    let DefinitionSection::Rows { lines, .. } = &sections[2] else {
        panic!("expected a rows section");
    };
    assert_eq!(
        lines,
        &vec![
            "customer_region_fkey  (region_id) → region (id)".to_owned(),
            "customer_tenant_fkey  (tenant_id) → billing.tenant (id)".to_owned(),
        ]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib definition::tests::constraints_render_in_their_own_sections`
Expected: FAIL — `KeyConstraint`, `ForeignKey` and `CheckConstraint` are not defined.

- [ ] **Step 3: Write minimal implementation**

In `src/definition.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyConstraint {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckConstraint {
    pub name: String,
    /// As `pg_get_constraintdef` renders it, e.g. `CHECK ((x > 0))`.
    pub expression: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}
```

Add the four fields to `TableDefinition`, then extend the `Table` arm of `sections`:

```rust
            Self::Table(table) => {
                let mut sections = Vec::new();
                push_rows(&mut sections, "Columns", column_lines(&table.columns));
                push_rows(
                    &mut sections,
                    "Primary Key",
                    key_lines(table.primary_key.as_slice()),
                );
                push_rows(
                    &mut sections,
                    "Foreign Keys",
                    foreign_key_lines(&table.foreign_keys, &_object.schema),
                );
                push_rows(
                    &mut sections,
                    "Unique Constraints",
                    key_lines(&table.unique_constraints),
                );
                push_rows(
                    &mut sections,
                    "Check Constraints",
                    aligned(
                        &table
                            .check_constraints
                            .iter()
                            .map(|check| vec![check.name.clone(), check.expression.clone()])
                            .collect::<Vec<_>>(),
                    ),
                );
                sections
            }
```

Rename the `sections` parameter from `_object` to `object` now that it is used. Add the helpers:

```rust
fn key_lines(keys: &[KeyConstraint]) -> Vec<String> {
    let rows: Vec<Vec<String>> = keys
        .iter()
        .map(|key| vec![key.name.clone(), format!("({})", key.columns.join(", "))])
        .collect();
    aligned(&rows)
}

/// A referenced table is qualified only when it lives outside the schema being displayed, which
/// keeps the common same-schema case as short as the §6 example.
fn foreign_key_lines(keys: &[ForeignKey], schema: &str) -> Vec<String> {
    let rows: Vec<Vec<String>> = keys
        .iter()
        .map(|key| {
            let target = if key.referenced_schema == schema {
                key.referenced_table.clone()
            } else {
                format!("{}.{}", key.referenced_schema, key.referenced_table)
            };
            vec![
                key.name.clone(),
                format!(
                    "({}) → {target} ({})",
                    key.columns.join(", "),
                    key.referenced_columns.join(", ")
                ),
            ]
        })
        .collect();
    aligned(&rows)
}
```

`key_lines(table.primary_key.as_slice())` requires `Option::as_slice`, stable since Rust 1.75.

In `src/postgres/catalogue.rs`:

```rust
use crate::definition::{CheckConstraint, ForeignKey, KeyConstraint};

/// Constraints of one table. `WITH ORDINALITY` preserves the declared column order, which matters
/// for composite keys — ordering by `attnum` would silently reorder them.
pub const CONSTRAINTS: &str = "SELECT con.conname, con.contype, \
     pg_catalog.pg_get_constraintdef(con.oid), \
     ARRAY(SELECT a.attname FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) \
           JOIN pg_catalog.pg_attribute a \
             ON a.attrelid = con.conrelid AND a.attnum = k.attnum ORDER BY k.ord), \
     COALESCE(rn.nspname, ''), COALESCE(rc.relname, ''), \
     ARRAY(SELECT a.attname FROM unnest(COALESCE(con.confkey, '{}')) WITH ORDINALITY AS k(attnum, ord) \
           JOIN pg_catalog.pg_attribute a \
             ON a.attrelid = con.confrelid AND a.attnum = k.attnum ORDER BY k.ord) \
     FROM pg_catalog.pg_constraint con \
     JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     LEFT JOIN pg_catalog.pg_class rc ON rc.oid = con.confrelid \
     LEFT JOIN pg_catalog.pg_namespace rn ON rn.oid = rc.relnamespace \
     WHERE n.nspname = $1 AND c.relname = $2 AND con.contype IN ('p','f','u','c') \
     ORDER BY con.contype, con.conname";

#[derive(Default)]
pub struct Constraints {
    pub primary_key: Option<KeyConstraint>,
    pub foreign_keys: Vec<ForeignKey>,
    pub unique_constraints: Vec<KeyConstraint>,
    pub check_constraints: Vec<CheckConstraint>,
}

pub fn constraints(rows: &[Row]) -> Constraints {
    let mut result = Constraints::default();
    for row in rows {
        let name: String = row.get(0);
        // contype is PostgreSQL's `"char"`, a one-byte type the driver hands over as i8 — not the
        // four-byte `char` a Rust reader expects.
        let kind: i8 = row.get(1);
        let definition: String = row.get(2);
        let columns: Vec<String> = row.get(3);
        match kind as u8 {
            b'p' => {
                result.primary_key = Some(KeyConstraint {
                    name,
                    columns,
                })
            }
            b'u' => result.unique_constraints.push(KeyConstraint { name, columns }),
            b'c' => result.check_constraints.push(CheckConstraint {
                name,
                expression: definition,
            }),
            b'f' => result.foreign_keys.push(ForeignKey {
                name,
                columns,
                referenced_schema: row.get(4),
                referenced_table: row.get(5),
                referenced_columns: row.get(6),
            }),
            _ => {}
        }
    }
    result
}
```

In `src/postgres.rs`, extend the `ObjectKind::Table` arm:

```rust
            ObjectKind::Table => {
                let (columns, constraints) = self
                    .with_client(async |client| {
                        let columns = client
                            .query(catalogue::COLUMNS, &[&object.schema, &object.name])
                            .await?;
                        let constraints = client
                            .query(catalogue::CONSTRAINTS, &[&object.schema, &object.name])
                            .await?;
                        Ok((columns, constraints))
                    })
                    .await?;
                let constraints = catalogue::constraints(&constraints);
                Ok(ObjectDefinition::Table(TableDefinition {
                    columns: catalogue::columns(&columns),
                    primary_key: constraints.primary_key,
                    foreign_keys: constraints.foreign_keys,
                    unique_constraints: constraints.unique_constraints,
                    check_constraints: constraints.check_constraints,
                    ..TableDefinition::default()
                }))
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib definition::`
Expected: PASS, 5 tests.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/definition.rs src/postgres/catalogue.rs src/postgres.rs
git commit -m "Show primary keys, foreign keys and constraints"
```

---

### Task 5: Indexes

Indexes render as a `Sql` section carrying `pg_get_indexdef` output rather than the compact form in the PRD's illustrative §6 example. `pg_get_indexdef` is what PostgreSQL itself provides, needs no parsing, and is what §5 asks for; a compact form would mean reconstructing the column list by hand, which §10 rules out.

**Files:**
- Modify: `src/definition.rs`, `src/postgres/catalogue.rs`, `src/postgres.rs`
- Test: `src/definition.rs` (`mod tests`)

**Interfaces:**
- Consumes: `TableDefinition` from Tasks 2 and 4
- Produces: `IndexDefinition { name, definition_sql, primary, unique }`; `TableDefinition::indexes: Vec<IndexDefinition>`; `catalogue::INDEXES`, `catalogue::indexes(rows) -> Vec<IndexDefinition>`

- [ ] **Step 1: Write the failing test**

```rust
/// FR2-003. `pg_get_indexdef` output goes through unchanged (§7 forbids reformatting).
#[test]
fn indexes_render_as_their_postgresql_definitions() {
    let definition = ObjectDefinition::Table(TableDefinition {
        columns: vec![column(1, "id", "bigint", false, None)],
        indexes: vec![
            IndexDefinition {
                name: "customer_pkey".into(),
                definition_sql: "CREATE UNIQUE INDEX customer_pkey ON public.customer USING btree (id)".into(),
                primary: true,
                unique: true,
            },
            IndexDefinition {
                name: "customer_email_idx".into(),
                definition_sql: "CREATE INDEX customer_email_idx ON public.customer USING btree (email)".into(),
                primary: false,
                unique: false,
            },
        ],
        ..TableDefinition::default()
    });

    let sections = definition.sections(&object("customer", ObjectKind::Table));

    assert_eq!(
        sections.last(),
        Some(&DefinitionSection::Sql {
            heading: "Indexes".into(),
            sql: "CREATE UNIQUE INDEX customer_pkey ON public.customer USING btree (id);\n\
                  CREATE INDEX customer_email_idx ON public.customer USING btree (email);"
                .into(),
        })
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib definition::tests::indexes_render_as_their_postgresql_definitions`
Expected: FAIL — `IndexDefinition` is not defined.

- [ ] **Step 3: Write minimal implementation**

In `src/definition.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexDefinition {
    pub name: String,
    /// Exactly as `pg_get_indexdef` returns it; never reformatted (§7).
    pub definition_sql: String,
    pub primary: bool,
    pub unique: bool,
}
```

Add `pub indexes: Vec<IndexDefinition>` to `TableDefinition`, and append to the `Table` arm of `sections`, after the check constraints:

```rust
                push_sql(
                    &mut sections,
                    "Indexes",
                    table
                        .indexes
                        .iter()
                        .map(|index| format!("{};", index.definition_sql))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
```

and the helper beside `push_rows`:

```rust
fn push_sql(sections: &mut Vec<DefinitionSection>, heading: &str, sql: String) {
    if !sql.is_empty() {
        sections.push(DefinitionSection::Sql {
            heading: heading.to_owned(),
            sql,
        });
    }
}
```

In `src/postgres/catalogue.rs`:

```rust
use crate::definition::IndexDefinition;

pub const INDEXES: &str = "SELECT c.relname, pg_catalog.pg_get_indexdef(i.indexrelid), \
     i.indisprimary, i.indisunique \
     FROM pg_catalog.pg_index i \
     JOIN pg_catalog.pg_class c ON c.oid = i.indexrelid \
     JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
     JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
     WHERE n.nspname = $1 AND t.relname = $2 \
     ORDER BY i.indisprimary DESC, c.relname";

pub fn indexes(rows: &[Row]) -> Vec<IndexDefinition> {
    rows.iter()
        .map(|row| IndexDefinition {
            name: row.get(0),
            definition_sql: row.get(1),
            primary: row.get(2),
            unique: row.get(3),
        })
        .collect()
}
```

In `src/postgres.rs`, add the third query to the `ObjectKind::Table` arm's `with_client` closure and set `indexes: catalogue::indexes(&indexes)` in place of the `..TableDefinition::default()` spread:

```rust
                        let indexes = client
                            .query(catalogue::INDEXES, &[&object.schema, &object.name])
                            .await?;
                        Ok((columns, constraints, indexes))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib definition::`
Expected: PASS, 6 tests.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/definition.rs src/postgres/catalogue.rs src/postgres.rs
git commit -m "Show table indexes as their PostgreSQL definitions"
```

---

### Task 6: Views and materialised views

**Files:**
- Modify: `src/definition.rs`, `src/postgres/catalogue.rs`, `src/postgres.rs`
- Test: `src/definition.rs` (`mod tests`)

**Interfaces:**
- Consumes: `ColumnDefinition` from Task 2
- Produces: `ViewDefinition { columns, definition_sql, materialised }`; `ObjectDefinition::View(ViewDefinition)`; `catalogue::VIEW_DEFINITION`

- [ ] **Step 1: Write the failing test**

```rust
/// FR2-004. §5 gives a view a column list plus its definition SQL — and no index section.
#[test]
fn a_view_renders_its_columns_and_its_definition_sql() {
    let definition = ObjectDefinition::View(ViewDefinition {
        columns: vec![column(1, "id", "bigint", true, None)],
        definition_sql: " SELECT customer.id\n   FROM customer;".into(),
        materialised: false,
    });

    let sections = definition.sections(&object("active_customer", ObjectKind::View));

    assert_eq!(
        sections,
        vec![
            DefinitionSection::Rows {
                heading: "Columns".into(),
                lines: vec!["id  bigint".into()],
            },
            DefinitionSection::Sql {
                heading: "Definition".into(),
                sql: " SELECT customer.id\n   FROM customer;".into(),
            },
        ]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib definition::tests::a_view_renders_its_columns_and_its_definition_sql`
Expected: FAIL — `ViewDefinition` is not defined.

- [ ] **Step 3: Write minimal implementation**

In `src/definition.rs`:

```rust
/// §5 gives a view a column list plus its definition SQL. Indexes are omitted even for
/// materialised views, which can carry them — following §5 literally for this phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewDefinition {
    pub columns: Vec<ColumnDefinition>,
    /// Exactly as `pg_get_viewdef` returns it; never reformatted (§7).
    pub definition_sql: String,
    pub materialised: bool,
}
```

Add `View(ViewDefinition),` to `ObjectDefinition` and the arm:

```rust
            Self::View(view) => {
                let mut sections = Vec::new();
                push_rows(&mut sections, "Columns", column_lines(&view.columns));
                push_sql(&mut sections, "Definition", view.definition_sql.clone());
                sections
            }
```

In `src/postgres/catalogue.rs`:

```rust
/// `pg_get_viewdef(oid, true)` pretty-prints, which is PostgreSQL's own formatting rather than
/// ours — §7 forbids reformatting, not asking PostgreSQL to format.
pub const VIEW_DEFINITION: &str = "SELECT pg_catalog.pg_get_viewdef(c.oid, true), \
     c.relkind = 'm' \
     FROM pg_catalog.pg_class c \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('v','m')";
```

In `src/postgres.rs`, add the arm to `load_definition`:

```rust
            ObjectKind::View | ObjectKind::MaterialisedView => {
                let (columns, view) = self
                    .with_client(async |client| {
                        let columns = client
                            .query(catalogue::COLUMNS, &[&object.schema, &object.name])
                            .await?;
                        let view = client
                            .query(catalogue::VIEW_DEFINITION, &[&object.schema, &object.name])
                            .await?;
                        Ok((columns, view))
                    })
                    .await?;
                // The object was in the tree a moment ago; if it is gone now, say so rather than
                // rendering a view with no definition (§12).
                let Some(row) = view.first() else {
                    return Err(simple_error("the object no longer exists"));
                };
                Ok(ObjectDefinition::View(ViewDefinition {
                    columns: catalogue::columns(&columns),
                    definition_sql: row.get(0),
                    materialised: row.get(1),
                }))
            }
```

with `ViewDefinition` added to the `crate::definition` import.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib definition::`
Expected: PASS, 7 tests.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/definition.rs src/postgres/catalogue.rs src/postgres.rs
git commit -m "Show view and materialised view definitions"
```

---

### Task 7: Functions and procedures

**Files:**
- Modify: `src/definition.rs`, `src/postgres/catalogue.rs`, `src/postgres.rs`
- Test: `src/definition.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks beyond `DefinitionSection`
- Produces: `RoutineDefinition { arguments, return_type, language, definition_sql }`; `ObjectDefinition::Routine(RoutineDefinition)`; `catalogue::ROUTINE_DEFINITION`

**Known limitation to record in the doc comment:** the Phase 1 tree lists routines by name alone, so an overloaded name cannot be distinguished. The query takes the lowest `oid` and the doc comment says so. Resolving overloads needs tree changes that belong to a later phase.

- [ ] **Step 1: Write the failing test**

```rust
/// FR2-005. A procedure has no return type, so that row is absent rather than blank.
#[test]
fn a_routine_renders_its_signature_and_body() {
    let function = ObjectDefinition::Routine(RoutineDefinition {
        arguments: "a integer, b integer".into(),
        return_type: Some("integer".into()),
        language: "sql".into(),
        definition_sql: "CREATE OR REPLACE FUNCTION public.add(a integer, b integer)\n\
                         RETURNS integer\nAS $$SELECT a + b$$".into(),
    });

    let sections = function.sections(&object("add", ObjectKind::Function));

    assert_eq!(
        sections[0],
        DefinitionSection::Rows {
            heading: "Signature".into(),
            lines: vec![
                "Arguments  a integer, b integer".into(),
                "Returns    integer".into(),
                "Language   sql".into(),
            ],
        }
    );
    assert!(matches!(
        &sections[1],
        DefinitionSection::Sql { heading, .. } if heading == "Definition"
    ));

    let procedure = ObjectDefinition::Routine(RoutineDefinition {
        arguments: "".into(),
        return_type: None,
        language: "plpgsql".into(),
        definition_sql: "CREATE OR REPLACE PROCEDURE public.tidy()\nAS $$BEGIN END$$".into(),
    });

    let DefinitionSection::Rows { lines, .. } =
        &procedure.sections(&object("tidy", ObjectKind::Procedure))[0]
    else {
        panic!("expected a rows section");
    };
    assert_eq!(
        lines,
        &vec!["Arguments".to_owned(), "Language   plpgsql".to_owned()]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib definition::tests::a_routine_renders_its_signature_and_body`
Expected: FAIL — `RoutineDefinition` is not defined.

- [ ] **Step 3: Write minimal implementation**

In `src/definition.rs`:

```rust
/// A function or a procedure. §5 asks for arguments, return type and language beside the body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineDefinition {
    /// As `pg_get_function_arguments` renders it.
    pub arguments: String,
    /// `None` for a procedure, which returns nothing.
    pub return_type: Option<String>,
    pub language: String,
    /// Exactly as `pg_get_functiondef` returns it; never reformatted (§7).
    pub definition_sql: String,
}
```

Add `Routine(RoutineDefinition),` and the arm:

```rust
            Self::Routine(routine) => {
                let mut rows = vec![vec!["Arguments".to_owned(), routine.arguments.clone()]];
                if let Some(returns) = &routine.return_type {
                    rows.push(vec!["Returns".to_owned(), returns.clone()]);
                }
                rows.push(vec!["Language".to_owned(), routine.language.clone()]);
                let mut sections = Vec::new();
                push_rows(&mut sections, "Signature", aligned(&rows));
                push_sql(&mut sections, "Definition", routine.definition_sql.clone());
                sections
            }
```

In `src/postgres/catalogue.rs`:

```rust
/// The Phase 1 tree lists routines by name alone, so an overloaded name cannot be told apart.
/// This takes the lowest `oid`; distinguishing overloads needs tree changes beyond Phase 2.
/// `prokind` is restricted because `pg_get_functiondef` errors on aggregate and window functions.
pub const ROUTINE_DEFINITION: &str = "SELECT pg_catalog.pg_get_function_arguments(p.oid), \
     CASE WHEN p.prokind = 'p' THEN NULL \
          ELSE pg_catalog.pg_get_function_result(p.oid) END, \
     l.lanname, pg_catalog.pg_get_functiondef(p.oid) \
     FROM pg_catalog.pg_proc p \
     JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
     JOIN pg_catalog.pg_language l ON l.oid = p.prolang \
     WHERE n.nspname = $1 AND p.proname = $2 AND p.prokind IN ('f','p') \
     ORDER BY p.oid LIMIT 1";
```

In `src/postgres.rs`:

```rust
            ObjectKind::Function | ObjectKind::Procedure => {
                let rows = self
                    .with_client(async |client| {
                        client
                            .query(
                                catalogue::ROUTINE_DEFINITION,
                                &[&object.schema, &object.name],
                            )
                            .await
                    })
                    .await?;
                // No row means an aggregate or window function, which `pg_get_functiondef` cannot
                // describe — §5 wants that said, not an empty panel.
                let Some(row) = rows.first() else {
                    return Ok(ObjectDefinition::Unsupported {
                        kind: object.kind,
                        reason: "PostgreSQL provides no definition for this routine".into(),
                    });
                };
                Ok(ObjectDefinition::Routine(RoutineDefinition {
                    arguments: row.get(0),
                    return_type: row.get(1),
                    language: row.get(2),
                    definition_sql: row.get(3),
                }))
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib definition::`
Expected: PASS, 8 tests.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/definition.rs src/postgres/catalogue.rs src/postgres.rs
git commit -m "Show function and procedure definitions"
```

---

### Task 8: Sequences

**Files:**
- Modify: `src/definition.rs`, `src/postgres/catalogue.rs`, `src/postgres.rs`
- Test: `src/definition.rs` (`mod tests`)

**Interfaces:**
- Produces: `SequenceDefinition { data_type, start, increment, minimum, maximum, cycles, owned_by }`; `ObjectDefinition::Sequence(SequenceDefinition)`; `catalogue::SEQUENCE_DEFINITION`

- [ ] **Step 1: Write the failing test**

```rust
/// FR2-008. An unowned sequence omits the row rather than printing "none".
#[test]
fn a_sequence_renders_its_properties() {
    let definition = ObjectDefinition::Sequence(SequenceDefinition {
        data_type: "bigint".into(),
        start: 1,
        increment: 1,
        minimum: 1,
        maximum: 9_223_372_036_854_775_807,
        cycles: false,
        owned_by: Some("public.customer.id".into()),
    });

    let sections = definition.sections(&object("customer_id_seq", ObjectKind::Sequence));

    assert_eq!(
        sections,
        vec![DefinitionSection::Rows {
            heading: "Sequence".into(),
            lines: vec![
                "Type       bigint".into(),
                "Start      1".into(),
                "Increment  1".into(),
                "Minimum    1".into(),
                "Maximum    9223372036854775807".into(),
                "Cycles     no".into(),
                "Owned by   public.customer.id".into(),
            ],
        }]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib definition::tests::a_sequence_renders_its_properties`
Expected: FAIL — `SequenceDefinition` is not defined.

- [ ] **Step 3: Write minimal implementation**

In `src/definition.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceDefinition {
    pub data_type: String,
    pub start: i64,
    pub increment: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub cycles: bool,
    /// The column the sequence backs, as `schema.table.column`, when it is owned by one.
    pub owned_by: Option<String>,
}
```

Add `Sequence(SequenceDefinition),` and the arm:

```rust
            Self::Sequence(sequence) => {
                let mut rows = vec![
                    vec!["Type".to_owned(), sequence.data_type.clone()],
                    vec!["Start".to_owned(), sequence.start.to_string()],
                    vec!["Increment".to_owned(), sequence.increment.to_string()],
                    vec!["Minimum".to_owned(), sequence.minimum.to_string()],
                    vec!["Maximum".to_owned(), sequence.maximum.to_string()],
                    vec![
                        "Cycles".to_owned(),
                        if sequence.cycles { "yes" } else { "no" }.to_owned(),
                    ],
                ];
                if let Some(owner) = &sequence.owned_by {
                    rows.push(vec!["Owned by".to_owned(), owner.clone()]);
                }
                let mut sections = Vec::new();
                push_rows(&mut sections, "Sequence", aligned(&rows));
                sections
            }
```

In `src/postgres/catalogue.rs`:

```rust
use crate::definition::SequenceDefinition;

/// `pg_sequence` holds the parameters; the owning column comes from an auto dependency, which is
/// what `ALTER SEQUENCE … OWNED BY` and `serial` both record.
pub const SEQUENCE_DEFINITION: &str = "SELECT pg_catalog.format_type(s.seqtypid, NULL), \
     s.seqstart, s.seqincrement, s.seqmin, s.seqmax, s.seqcycle, \
     (SELECT dn.nspname || '.' || dc.relname || '.' || da.attname \
        FROM pg_catalog.pg_depend d \
        JOIN pg_catalog.pg_class dc ON dc.oid = d.refobjid \
        JOIN pg_catalog.pg_namespace dn ON dn.oid = dc.relnamespace \
        JOIN pg_catalog.pg_attribute da \
          ON da.attrelid = d.refobjid AND da.attnum = d.refobjsubid \
       WHERE d.objid = c.oid AND d.deptype = 'a' LIMIT 1) \
     FROM pg_catalog.pg_sequence s \
     JOIN pg_catalog.pg_class c ON c.oid = s.seqrelid \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relname = $2";

pub fn sequence(row: &Row) -> SequenceDefinition {
    SequenceDefinition {
        data_type: row.get(0),
        start: row.get(1),
        increment: row.get(2),
        minimum: row.get(3),
        maximum: row.get(4),
        cycles: row.get(5),
        owned_by: row.get(6),
    }
}
```

In `src/postgres.rs`:

```rust
            ObjectKind::Sequence => {
                let rows = self
                    .with_client(async |client| {
                        client
                            .query(
                                catalogue::SEQUENCE_DEFINITION,
                                &[&object.schema, &object.name],
                            )
                            .await
                    })
                    .await?;
                let Some(row) = rows.first() else {
                    return Err(simple_error("the object no longer exists"));
                };
                Ok(ObjectDefinition::Sequence(catalogue::sequence(row)))
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib definition::`
Expected: PASS, 9 tests.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/definition.rs src/postgres/catalogue.rs src/postgres.rs
git commit -m "Show sequence properties"
```

---

### Task 9: Definition tabs and the focus enum

**Files:**
- Modify: `src/ui.rs`, `src/application.rs`
- Test: `src/ui.rs` (`mod tests`)

**Interfaces:**
- Consumes: `CommandService::definition` from Task 3
- Produces: `DefinitionTab`, `DefinitionState`, `Focus`; `AppView::open_definition(&mut self, object: DatabaseObject, cx: &mut Context<Self>)`; `AppView::close_definition(&mut self, id: Uuid, cx: &mut Context<Self>)`; command IDs `command::OPEN_DEFINITION`, `command::REFRESH_DEFINITION`, `command::CLOSE_DEFINITION`

- [ ] **Step 1: Write the failing test**

First extend `UiTestProvider` in `src/ui.rs` `mod tests` — replace the Task 3 stub with a real one:

```rust
        /// Fails the next definition request, so a test can assert the failure lands in the tab
        /// and nowhere else.
        definition_fails: AtomicBool,
        definition_calls: AtomicUsize,
```

```rust
        async fn definition(
            &self,
            object: &DatabaseObject,
            _refresh: bool,
        ) -> Result<ObjectDefinition, QueryError> {
            self.definition_calls.fetch_add(1, Ordering::SeqCst);
            if self.definition_fails.load(Ordering::SeqCst) {
                return Err(QueryError {
                    message: "permission denied for table customer".into(),
                    severity: None,
                    code: Some("42501".into()),
                    detail: None,
                    hint: None,
                    position: None,
                });
            }
            Ok(ObjectDefinition::Table(TableDefinition {
                columns: vec![ColumnDefinition {
                    position: 1,
                    name: format!("{}_id", object.name),
                    data_type: "bigint".into(),
                    nullable: false,
                    default: None,
                }],
                ..TableDefinition::default()
            }))
        }
```

Then the tests:

```rust
    fn customer() -> DatabaseObject {
        DatabaseObject {
            schema: "public".into(),
            name: "customer".into(),
            kind: ObjectKind::Table,
        }
    }

    fn wait_for_definitions(
        view: &gpui::Entity<AppView>,
        cx: &mut gpui::VisualTestContext,
        expected: usize,
    ) {
        for _ in 0..1_000 {
            cx.run_until_parked();
            if view.update(cx, |app, _| app.definitions.len()) == expected {
                return;
            }
            std::thread::yield_now();
        }
        view.update(cx, |app, _| assert_eq!(app.definitions.len(), expected));
    }

    /// FR2-006, §8: the definition arrives beside the editor, which keeps its contents.
    #[gpui::test]
    fn opening_a_definition_leaves_the_editor_untouched(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
            app.editor.document = "SELECT 1;".into();
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| {
            assert_eq!(app.editor.document, "SELECT 1;");
            assert_eq!(app.definitions[0].object.name, "customer");
            assert!(matches!(
                app.definitions[0].state,
                DefinitionState::Loaded(_)
            ));
            assert!(matches!(app.focus, Focus::Definition(_)));
        });
    }

    /// §16: the same object focuses the tab it already has rather than opening a second one.
    #[gpui::test]
    fn opening_the_same_object_twice_focuses_the_existing_tab(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);
        view.update(cx, |app, cx| {
            app.focus = Focus::Editor;
            app.open_definition(customer(), cx);
        });
        cx.run_until_parked();

        view.update(cx, |app, _| {
            assert_eq!(app.definitions.len(), 1);
            assert!(matches!(app.focus, Focus::Definition(_)));
        });
        assert_eq!(provider.definition_calls.load(Ordering::SeqCst), 1);
    }

    /// FR2-010, §12: a failure is confined to its own tab.
    #[gpui::test]
    fn a_definition_failure_stays_inside_its_tab(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        provider.definition_fails.store(true, Ordering::SeqCst);
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
            app.editor.document = "SELECT 1;".into();
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);

        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, _| {
            let DefinitionState::Failed(error) = &app.definitions[0].state else {
                panic!("expected a failed definition");
            };
            assert_eq!(error.message, "permission denied for table customer");
            assert_eq!(app.editor.document, "SELECT 1;");
            assert_eq!(app.connection_state(), ConnectionState::Connected);
        });
    }

    /// §5: a definition belongs to a connection, so editors may come and go beneath it.
    #[gpui::test]
    fn a_definition_survives_opening_another_editor(cx: &mut TestAppContext) {
        let provider = Arc::new(UiTestProvider::default());
        let (view, cx) = build_app_view(cx);
        view.update(cx, |app, _| {
            app.provider_factory = provider_factory(provider.clone());
        });
        view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
        wait_for_connection_state(&view, cx, ConnectionState::Connected);
        view.update(cx, |app, cx| app.open_definition(customer(), cx));
        wait_for_definitions(&view, cx, 1);

        view.update(cx, |app, cx| app.dispatch_command(command::NEW_EDITOR, cx));

        view.update(cx, |app, _| assert_eq!(app.definitions.len(), 1));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib ui::tests::opening_a_definition_leaves_the_editor_untouched`
Expected: FAIL — `open_definition`, `definitions`, `Focus` and `DefinitionState` do not exist.

- [ ] **Step 3: Write minimal implementation**

In `src/application.rs`, add to `mod command`:

```rust
    pub const OPEN_DEFINITION: &str = "definition.open";
    pub const REFRESH_DEFINITION: &str = "definition.refresh";
    pub const CLOSE_DEFINITION: &str = "definition.close";
```

In `src/ui.rs`:

```rust
/// One open definition. Keyed to a profile rather than an editor, so editors may be switched or
/// closed beneath it (§8).
struct DefinitionTab {
    id: uuid::Uuid,
    profile_id: uuid::Uuid,
    object: DatabaseObject,
    state: DefinitionState,
}

enum DefinitionState {
    Loading,
    Loaded(ObjectDefinition),
    /// A failed load or refresh replaces the body, so stale content is never shown as current
    /// (§11, FR2-010).
    Failed(QueryError),
}

/// Which surface the workspace is showing. Replaces the earlier `active_result_tab` flag, which
/// could not express a third destination.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Result,
    Definition(uuid::Uuid),
}
```

Replace `active_result_tab: bool` on `AppView` with `focus: Focus` and add `definitions: Vec<DefinitionTab>`. Initialise with `focus: Focus::Editor` and `definitions: Vec::new()`.

Mechanically update the 20 `active_result_tab` sites: `self.active_result_tab = false` becomes `self.focus = Focus::Editor`; `self.active_result_tab = true` becomes `self.focus = Focus::Result`; reads such as `!self.active_result_tab` become `self.focus == Focus::Editor` and `self.active_result_tab` becomes `self.focus == Focus::Result`.

Add the methods:

```rust
    /// Opens an object's definition beside the editors (FR2-006). The object's connection must be
    /// live: a definition cannot be fetched from a closed session, and a tab that can only show an
    /// error is worse than a message.
    fn open_definition(&mut self, object: DatabaseObject, cx: &mut Context<Self>) {
        let profile_id = self.editor.connection.id;
        if self.state_of(profile_id) != ConnectionState::Connected {
            self.status = "Connect before opening a definition".into();
            cx.notify();
            return;
        }
        if let Some(existing) = self
            .definitions
            .iter()
            .find(|tab| tab.profile_id == profile_id && tab.object == object)
        {
            self.focus = Focus::Definition(existing.id);
            cx.notify();
            return;
        }
        let id = uuid::Uuid::new_v4();
        self.definitions.push(DefinitionTab {
            id,
            profile_id,
            object: object.clone(),
            state: DefinitionState::Loading,
        });
        self.focus = Focus::Definition(id);
        self.load_definition(id, object, false, cx);
    }

    /// Metadata work follows the Phase 1 path: off the render thread, applied back on it (§10).
    fn load_definition(
        &mut self,
        id: uuid::Uuid,
        object: DatabaseObject,
        refresh: bool,
        cx: &mut Context<Self>,
    ) {
        let service = self.active_session().service.clone();
        let runtime = self.runtime.clone();
        cx.spawn(async move |view, cx| {
            let outcome = runtime
                .spawn(async move { service.definition(&object, refresh).await })
                .await;
            let _ = view.update(cx, |app, cx| {
                let Some(tab) = app.definitions.iter_mut().find(|tab| tab.id == id) else {
                    return;
                };
                tab.state = match outcome {
                    Ok(Ok(definition)) => DefinitionState::Loaded(definition),
                    Ok(Err(error)) => DefinitionState::Failed(error),
                    Err(error) => DefinitionState::Failed(QueryError {
                        message: error.to_string(),
                        severity: None,
                        code: None,
                        detail: None,
                        hint: None,
                        position: None,
                    }),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn refresh_definition(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        let Some(tab) = self.definitions.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        let object = tab.object.clone();
        tab.state = DefinitionState::Loading;
        self.load_definition(id, object, true, cx);
    }

    /// Unlike editors, the workspace need not keep one: closing the last is allowed and focus
    /// falls back to the editor in front.
    fn close_definition(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        self.definitions.retain(|tab| tab.id != id);
        if self.focus == Focus::Definition(id) {
            self.focus = Focus::Editor;
        }
        cx.notify();
    }
```

Add the three command IDs to the `dispatch_command` match. `OPEN_DEFINITION` has no object of its own, so it is dispatched only from the tree; in `dispatch_command` it is a no-op guarded by a comment. `REFRESH_DEFINITION` and `CLOSE_DEFINITION` act on `self.focus` when it is `Focus::Definition(id)`.

Render the tabs in `editor_tabs`, after the result segment and before the `+` segment:

```rust
        for tab in &self.definitions {
            let id = tab.id;
            tabs = tabs.child(segment(
                &self.fonts,
                SharedString::from(format!("definition-tab-{id}")),
                format!("{} [Definition]", tab.object.name),
                self.focus == Focus::Definition(id),
                true,
                cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    if event.modifiers().alt {
                        this.close_definition(id, cx);
                    } else {
                        this.focus = Focus::Definition(id);
                        cx.notify();
                    }
                }),
            ));
        }
```

Add the surface itself, and call it from a new `Focus::Definition(id)` branch where `Render for AppView` currently chooses between the editor and result surfaces:

```rust
    /// A definition, read-only by construction: no caret, no key handling, no editing path
    /// (FR2-007). `Rows` blocks are already aligned by the model; `Sql` blocks go through the
    /// editor's own highlighter (§7).
    fn definition_surface(&self, tab: &DefinitionTab, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div().flex().flex_col().gap_4().p_4().min_w_0();
        match &tab.state {
            DefinitionState::Loading => {
                body = body.child(self.tree_caption("Loading…"));
            }
            DefinitionState::Failed(error) => {
                body = body.child(
                    div()
                        .text_size(px(12.5))
                        .font_family(self.fonts.mono.clone())
                        .text_color(rgb(DANGER))
                        .child(error.message.clone()),
                );
                for extra in [error.detail.clone(), error.hint.clone()].into_iter().flatten() {
                    body = body.child(
                        div()
                            .text_size(px(12.))
                            .font_family(self.fonts.mono.clone())
                            .text_color(rgb(MUTED))
                            .child(extra),
                    );
                }
            }
            DefinitionState::Loaded(definition) => {
                for section in definition.sections(&tab.object) {
                    match section {
                        DefinitionSection::Rows { heading, lines } => {
                            body = body.child(self.tree_caption(heading));
                            for line in lines {
                                body = body.child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_family(self.fonts.mono.clone())
                                        .text_color(rgb(TEXT))
                                        .child(line),
                                );
                            }
                        }
                        DefinitionSection::Sql { heading, sql } => {
                            body = body.child(self.tree_caption(heading));
                            let spans = highlight_lines(&sql);
                            for (index, line) in sql.lines().enumerate() {
                                body = body.child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_family(self.fonts.mono.clone())
                                        .child(highlight_line(
                                            line,
                                            spans.get(index).map_or(&[][..], Vec::as_slice),
                                        )),
                                );
                            }
                        }
                        DefinitionSection::Note { text } => {
                            body = body.child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(MUTED))
                                    .child(text),
                            );
                        }
                    }
                }
            }
        }
        body
    }
```

`DANGER`, `TEXT` and `MUTED` are the existing colour constants in this file; use whichever name the file already gives the error colour rather than adding one.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib ui::`
Expected: PASS — the four new tests plus all existing ui tests.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/ui.rs src/application.rs
git commit -m "Open object definitions in workspace tabs"
```

---

### Task 10: Tree interaction and the context menu

**Files:**
- Modify: `src/ui.rs`
- Test: `src/ui.rs` (`mod tests`)

**Interfaces:**
- Consumes: `AppView::open_definition` from Task 9
- Produces: `ObjectMenu { object: DatabaseObject }`; `AppView::object_menu: Option<ObjectMenu>`; `AppView::toggle_object_menu`

- [ ] **Step 1: Write the failing test**

```rust
/// FR2-012, §8: the menu offers Open Definition and nothing that modifies the database.
#[gpui::test]
fn the_object_menu_offers_only_open_definition(cx: &mut TestAppContext) {
    let provider = Arc::new(UiTestProvider::default());
    let (view, cx) = build_app_view(cx);
    view.update(cx, |app, _| {
        app.provider_factory = provider_factory(provider.clone());
    });
    view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
    wait_for_connection_state(&view, cx, ConnectionState::Connected);

    view.update(cx, |app, cx| app.toggle_object_menu(customer(), cx));

    view.update(cx, |app, _| {
        assert_eq!(app.object_menu.as_ref().map(|menu| menu.entries()),
                   Some(vec!["Open Definition"]));
    });

    view.update(cx, |app, cx| {
        let object = app.object_menu.as_ref().unwrap().object.clone();
        app.open_definition(object, cx);
    });
    wait_for_definitions(&view, cx, 1);

    view.update(cx, |app, _| assert_eq!(app.definitions[0].object.name, "customer"));
}

/// The menu is dismissed by choosing from it, so it cannot linger over the tree.
#[gpui::test]
fn choosing_from_the_object_menu_closes_it(cx: &mut TestAppContext) {
    let provider = Arc::new(UiTestProvider::default());
    let (view, cx) = build_app_view(cx);
    view.update(cx, |app, _| {
        app.provider_factory = provider_factory(provider.clone());
    });
    view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
    wait_for_connection_state(&view, cx, ConnectionState::Connected);
    view.update(cx, |app, cx| app.toggle_object_menu(customer(), cx));

    view.update(cx, |app, cx| app.choose_open_definition(cx));
    wait_for_definitions(&view, cx, 1);

    view.update(cx, |app, _| assert!(app.object_menu.is_none()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib ui::tests::the_object_menu_offers_only_open_definition`
Expected: FAIL — `toggle_object_menu` and `object_menu` do not exist.

- [ ] **Step 3: Write minimal implementation**

In `src/ui.rs`:

```rust
/// The tree's context menu, open over one object. §8 admits one entry and forbids anything that
/// modifies the database.
struct ObjectMenu {
    object: DatabaseObject,
}

impl ObjectMenu {
    fn entries(&self) -> Vec<&'static str> {
        vec!["Open Definition"]
    }
}
```

Add `object_menu: Option<ObjectMenu>` to `AppView`, initialised to `None`, and:

```rust
    fn toggle_object_menu(&mut self, object: DatabaseObject, cx: &mut Context<Self>) {
        self.object_menu = match &self.object_menu {
            Some(menu) if menu.object == object => None,
            _ => Some(ObjectMenu { object }),
        };
        cx.notify();
    }

    fn choose_open_definition(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.object_menu.take() else {
            return;
        };
        self.open_definition(menu.object, cx);
    }
```

Make the object rows in the tree interactive. Replace the inert `div()` for each object with one carrying an `.id(...)`, `.cursor_pointer()`, `.hover(...)`, and:

```rust
                                            .on_click(cx.listener({
                                                let object = object.clone();
                                                move |this, event: &gpui::ClickEvent, _, cx| {
                                                    // §8 opens on double-click; a single click
                                                    // only dismisses any open menu.
                                                    if event.click_count() >= 2 {
                                                        this.object_menu = None;
                                                        this.open_definition(object.clone(), cx);
                                                    } else {
                                                        this.object_menu = None;
                                                        cx.notify();
                                                    }
                                                }
                                            }))
                                            .on_mouse_down(
                                                gpui::MouseButton::Right,
                                                cx.listener({
                                                    let object = object.clone();
                                                    move |this, _, _, cx| {
                                                        this.toggle_object_menu(object.clone(), cx)
                                                    }
                                                }),
                                            )
```

`object` is a `&DatabaseObject` borrowed from the session, so clone it before the closure — the existing loop's borrow of `self.sessions` cannot outlive the listener.

Render the menu as a card over the workspace, following the shape of `connection_menu_card`, and call it from `Render for AppView` wherever `connection_menu_card` is already conditionally rendered:

```rust
    /// §8 admits exactly one entry. Modification actions must never appear here.
    fn object_menu_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut card = div()
            .absolute()
            .flex()
            .flex_col()
            .p_1()
            .rounded(px(CONTROL_RADIUS))
            .bg(rgb(PANEL))
            .child(self.tree_caption(
                self.object_menu
                    .as_ref()
                    .map_or_else(String::new, |menu| menu.object.name.clone()),
            ));
        for entry in self
            .object_menu
            .as_ref()
            .map(ObjectMenu::entries)
            .unwrap_or_default()
        {
            card = card.child(
                div()
                    .id(SharedString::from(entry))
                    .px_3()
                    .py(px(7.))
                    .rounded_lg()
                    .text_size(px(12.5))
                    .font_family(self.fonts.display.clone())
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(PANEL_LIGHT)))
                    .on_click(cx.listener(|this, _, _, cx| this.choose_open_definition(cx)))
                    .child(entry),
            );
        }
        card
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib ui::`
Expected: PASS.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/ui.rs
git commit -m "Open definitions from the object tree"
```

---

### Task 11: Refresh, copy, and cache invalidation

**Files:**
- Modify: `src/ui.rs`, `src/postgres.rs`
- Test: `src/ui.rs` (`mod tests`), `src/postgres.rs` (`mod tests`)

**Interfaces:**
- Consumes: `refresh_definition` from Task 9
- Produces: `AppView::copy_definition(&mut self, id: Uuid, cx: &mut Context<Self>)`; definition cache eviction inside `PostgresProvider::objects`

- [ ] **Step 1: Write the failing test**

In `src/ui.rs` `mod tests`:

```rust
/// FR2-009: refresh asks PostgreSQL again rather than replaying the cache.
#[gpui::test]
fn refreshing_a_definition_requests_it_again(cx: &mut TestAppContext) {
    let provider = Arc::new(UiTestProvider::default());
    let (view, cx) = build_app_view(cx);
    view.update(cx, |app, _| {
        app.provider_factory = provider_factory(provider.clone());
    });
    view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
    wait_for_connection_state(&view, cx, ConnectionState::Connected);
    view.update(cx, |app, cx| app.open_definition(customer(), cx));
    wait_for_definitions(&view, cx, 1);

    let id = view.update(cx, |app, _| app.definitions[0].id);
    view.update(cx, |app, cx| app.refresh_definition(id, cx));
    cx.run_until_parked();

    assert_eq!(provider.definition_calls.load(Ordering::SeqCst), 2);
    view.update(cx, |app, _| assert_eq!(app.definitions.len(), 1));
}

/// §11: a refresh that fails replaces the body, so nothing stale is shown as current.
#[gpui::test]
fn a_failed_refresh_replaces_the_definition_rather_than_keeping_it(cx: &mut TestAppContext) {
    let provider = Arc::new(UiTestProvider::default());
    let (view, cx) = build_app_view(cx);
    view.update(cx, |app, _| {
        app.provider_factory = provider_factory(provider.clone());
    });
    view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
    wait_for_connection_state(&view, cx, ConnectionState::Connected);
    view.update(cx, |app, cx| app.open_definition(customer(), cx));
    wait_for_definitions(&view, cx, 1);

    provider.definition_fails.store(true, Ordering::SeqCst);
    let id = view.update(cx, |app, _| app.definitions[0].id);
    view.update(cx, |app, cx| app.refresh_definition(id, cx));
    cx.run_until_parked();

    view.update(cx, |app, _| {
        assert!(matches!(app.definitions[0].state, DefinitionState::Failed(_)));
    });
}

/// §9: copying is permitted — the whole tab, in section order.
#[gpui::test]
fn copying_a_definition_yields_its_whole_text(cx: &mut TestAppContext) {
    let provider = Arc::new(UiTestProvider::default());
    let (view, cx) = build_app_view(cx);
    view.update(cx, |app, _| {
        app.provider_factory = provider_factory(provider.clone());
    });
    view.update(cx, |app, cx| app.dispatch_command(command::CONNECT, cx));
    wait_for_connection_state(&view, cx, ConnectionState::Connected);
    view.update(cx, |app, cx| app.open_definition(customer(), cx));
    wait_for_definitions(&view, cx, 1);

    let text = view.update(cx, |app, _| app.definition_text(app.definitions[0].id));

    assert_eq!(
        text.as_deref(),
        Some("Columns\ncustomer_id  bigint  NOT NULL")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib ui::tests::copying_a_definition_yields_its_whole_text`
Expected: FAIL — `definition_text` does not exist.

- [ ] **Step 3: Write minimal implementation**

In `src/ui.rs`:

```rust
    /// The whole tab as text, in section order: heading, then its lines or its SQL. §9 permits
    /// copying a definition; this is what the Copy action places on the clipboard.
    fn definition_text(&self, id: uuid::Uuid) -> Option<String> {
        let tab = self.definitions.iter().find(|tab| tab.id == id)?;
        let DefinitionState::Loaded(definition) = &tab.state else {
            return None;
        };
        let mut blocks = Vec::new();
        for section in definition.sections(&tab.object) {
            match section {
                DefinitionSection::Rows { heading, lines } => {
                    blocks.push(format!("{heading}\n{}", lines.join("\n")))
                }
                DefinitionSection::Sql { heading, sql } => blocks.push(format!("{heading}\n{sql}")),
                DefinitionSection::Note { text } => blocks.push(text),
            }
        }
        Some(blocks.join("\n\n"))
    }

    fn copy_definition(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        if let Some(text) = self.definition_text(id) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.status = "Copied definition".into();
            cx.notify();
        }
    }
```

Add Refresh and Copy controls to the definition surface's header, dispatching `refresh_definition` and `copy_definition` for the focused tab.

In `src/postgres.rs`, honour §11 by evicting definitions when their schema's object list is refreshed. In `objects`, immediately after the `if !refresh && …` early return:

```rust
        if refresh {
            // §11: refreshing a branch of the tree invalidates the definitions beneath it, or a
            // tab reopened afterwards would show what the branch no longer describes.
            self.definitions_cache
                .write()
                .await
                .retain(|key, _| key.schema != schema);
        }
```

`objects` needs a live client, so the eviction is factored into a helper that a unit test can reach without a server. Replace the inline block above with a call to it:

```rust
    /// §11: refreshing a branch of the tree invalidates the definitions beneath it, or a tab
    /// reopened afterwards would show what the branch no longer describes.
    async fn evict_definitions(&self, schema: &str) {
        self.definitions_cache
            .write()
            .await
            .retain(|key, _| key.schema != schema);
    }
```

called as `self.evict_definitions(schema).await;` inside `objects` when `refresh` is true, and tested directly in `src/postgres.rs` `mod tests`:

```rust
    /// §11 in isolation: the eviction rule, without needing a server to reach it.
    #[tokio::test]
    async fn refreshing_a_schema_evicts_only_that_schemas_definitions() {
        let provider = PostgresProvider::default();
        let kept = DefinitionKey {
            schema: "billing".into(),
            name: "invoice".into(),
            kind: ObjectKind::Table,
        };
        let evicted = DefinitionKey {
            schema: "public".into(),
            name: "customer".into(),
            kind: ObjectKind::Table,
        };
        {
            let mut cache = provider.definitions_cache.write().await;
            for key in [kept.clone(), evicted.clone()] {
                cache.insert(
                    key,
                    ObjectDefinition::Table(TableDefinition::default()),
                );
            }
        }

        provider.evict_definitions("public").await;

        let cache = provider.definitions_cache.read().await;
        assert!(cache.contains_key(&kept));
        assert!(!cache.contains_key(&evicted));
    }
```

`DefinitionKey` needs `#[derive(Clone, PartialEq, Eq, Hash)]`, which Task 3 already gave it.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add src/ui.rs src/postgres.rs
git commit -m "Refresh, copy and invalidate object definitions"
```

---

### Task 12: Live acceptance test

**Files:**
- Modify: `tests/postgres_smoke.rs`

**Interfaces:**
- Consumes: everything above, through `PostgresProvider`

- [ ] **Step 1: Write the failing test**

Add to `tests/postgres_smoke.rs`, following the existing `#[ignore]` convention:

```rust
/// The Phase 2 acceptance scenario (§14), against a real server. Ignored by default because it
/// needs one; run with RUSTY_SQL_TEST_DATABASE_URL set.
#[tokio::test]
#[ignore = "requires RUSTY_SQL_TEST_DATABASE_URL and a live PostgreSQL server"]
async fn inspects_a_table_definition_and_sees_a_new_column_after_refresh() {
    let database_url = std::env::var("RUSTY_SQL_TEST_DATABASE_URL")
        .expect("RUSTY_SQL_TEST_DATABASE_URL must be set for the live smoke test");
    let profile = ConnectionProfile::from_database_url(&database_url)
        .expect("test database URL should be valid");
    let provider = PostgresProvider::new();
    provider.connect(&profile).await.expect("connect");

    provider
        .execute("DROP TABLE IF EXISTS rusty_sql_definition_test")
        .await
        .expect("drop");
    provider
        .execute(
            "CREATE TABLE rusty_sql_definition_test (\
               id bigint PRIMARY KEY, \
               email varchar(320) NOT NULL, \
               active boolean NOT NULL DEFAULT true)",
        )
        .await
        .expect("create");

    let object = DatabaseObject {
        schema: "public".into(),
        name: "rusty_sql_definition_test".into(),
        kind: ObjectKind::Table,
    };

    let ObjectDefinition::Table(table) = provider
        .definition(&object, true)
        .await
        .expect("definition")
    else {
        panic!("expected a table definition");
    };
    assert_eq!(
        table.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["id", "email", "active"]
    );
    assert_eq!(table.columns[1].data_type, "character varying(320)");
    assert!(!table.columns[1].nullable);
    assert_eq!(table.columns[2].default.as_deref(), Some("true"));
    assert!(table.primary_key.is_some());
    assert!(!table.indexes.is_empty());

    provider
        .execute("ALTER TABLE rusty_sql_definition_test ADD COLUMN notes text")
        .await
        .expect("alter");

    let ObjectDefinition::Table(refreshed) = provider
        .definition(&object, true)
        .await
        .expect("refreshed definition")
    else {
        panic!("expected a table definition");
    };
    assert_eq!(refreshed.columns.last().unwrap().name, "notes");

    provider
        .execute("DROP TABLE rusty_sql_definition_test")
        .await
        .expect("cleanup");
    provider.disconnect().await.expect("disconnect");
}
```

This matches the setup the two existing tests in this file already use — there is no shared profile helper, and this task must not add one. Extend the file's `use` list with `rusty_sql_tool::database::{DatabaseObject, ObjectKind}` and `rusty_sql_tool::definition::ObjectDefinition`.

- [ ] **Step 2: Run test to verify it fails**

Without a server it is skipped, which proves nothing. Run it against one:

Run: `RUSTY_SQL_TEST_DATABASE_URL=postgres://… cargo test --test postgres_smoke -- --ignored inspects_a_table_definition`
Expected: PASS if Tasks 1–8 are correct. A failure here is a real catalogue bug — the unit tests cannot catch these, which is the known gap in the spec's §9.

- [ ] **Step 3: Fix whatever the live run reveals**

Likely candidates, all in `src/postgres/catalogue.rs`: `format_type` renders `varchar(320)` as `character varying(320)`, `contype` arriving as `i8`, and `pg_get_expr` returning `NULL` for columns without defaults. Adjust the assertions to what PostgreSQL genuinely returns rather than bending the queries to the assertions.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test` then `RUSTY_SQL_TEST_DATABASE_URL=postgres://… cargo test --test postgres_smoke -- --ignored`
Expected: PASS.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add tests/postgres_smoke.rs src/postgres/catalogue.rs
git commit -m "Cover the Phase 2 acceptance scenario against a live server"
```

---

## Requirements traceability

| Requirement | Task |
|---|---|
| FR2-001 Table Details | 2, 3 |
| FR2-002 Constraints | 4 |
| FR2-003 Indexes | 5 |
| FR2-004 View Definitions | 6 |
| FR2-005 Function Definitions | 7 |
| FR2-006 Definition Tabs | 9 |
| FR2-007 Read Only | 9 (structural — a definition is not an `EditorState`) |
| FR2-008 Sequence Details | 8 |
| FR2-009 Definition Refresh | 11 |
| FR2-010 Definition Failures | 9, 11 |
| FR2-011 Metadata Reuse | 3 |
| FR2-012 Context Menu | 10 |
| §14 Acceptance criteria | 12 |
