//! What an object *is*, as opposed to the fact that it exists (Phase 2 §10).
//!
//! GPUI-independent by design, in the same spirit as `QueryResult`: the model decides its own
//! layout so a definition can be asserted in tests with no window, and rendered differently or
//! exported later.

use crate::database::{DatabaseObject, ObjectKind};

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
    pub primary_key: Option<KeyConstraint>,
    pub foreign_keys: Vec<ForeignKey>,
    pub unique_constraints: Vec<KeyConstraint>,
    pub check_constraints: Vec<CheckConstraint>,
    pub indexes: Vec<IndexDefinition>,
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexDefinition {
    pub name: String,
    /// Exactly as `pg_get_indexdef` returns it; never reformatted (§7).
    pub definition_sql: String,
    pub primary: bool,
    pub unique: bool,
}

/// §5 gives a view a column list plus its definition SQL. Indexes are omitted even for
/// materialised views, which can carry them — following §5 literally for this phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewDefinition {
    pub columns: Vec<ColumnDefinition>,
    /// Exactly as `pg_get_viewdef` returns it; never reformatted (§7).
    pub definition_sql: String,
    pub materialised: bool,
}

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

/// The definition of one database object. An enum rather than a struct of optional sections, so
/// states such as a sequence carrying foreign keys cannot be constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectDefinition {
    /// §5: the object exists but PostgreSQL offers no meaningful definition for its kind.
    Unsupported {
        kind: ObjectKind,
        reason: String,
    },
    Table(TableDefinition),
    View(ViewDefinition),
    Routine(RoutineDefinition),
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
    pub fn sections(&self, object: &DatabaseObject) -> Vec<DefinitionSection> {
        match self {
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
                    foreign_key_lines(&table.foreign_keys, &object.schema),
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
                sections
            }
            Self::View(view) => {
                let mut sections = Vec::new();
                push_rows(&mut sections, "Columns", column_lines(&view.columns));
                push_sql(&mut sections, "Definition", view.definition_sql.clone());
                sections
            }
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
            Self::Unsupported { reason, .. } => vec![DefinitionSection::Note {
                text: reason.clone(),
            }],
        }
    }
}

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

/// Skips the section entirely when the SQL is empty, matching `push_rows`.
fn push_sql(sections: &mut Vec<DefinitionSection>, heading: &str, sql: String) {
    if !sql.is_empty() {
        sections.push(DefinitionSection::Sql {
            heading: heading.to_owned(),
            sql,
        });
    }
}

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

    fn column(
        position: i32,
        name: &str,
        data_type: &str,
        nullable: bool,
        default: Option<&str>,
    ) -> ColumnDefinition {
        ColumnDefinition {
            position,
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            default: default.map(ToOwned::to_owned),
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

        let DefinitionSection::Rows { lines, .. } =
            &definition.sections(&object("beasts", ObjectKind::Table))[0]
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

        assert!(
            definition
                .sections(&object("empty", ObjectKind::Table))
                .is_empty()
        );
    }

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

    /// FR2-005. A procedure has no return type, so that row is absent rather than blank.
    #[test]
    fn a_routine_renders_its_signature_and_body() {
        let function = ObjectDefinition::Routine(RoutineDefinition {
            arguments: "a integer, b integer".into(),
            return_type: Some("integer".into()),
            language: "sql".into(),
            definition_sql: "CREATE OR REPLACE FUNCTION public.add(a integer, b integer)\n\
                             RETURNS integer\nAS $$SELECT a + b$$"
                .into(),
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

    /// FR2-003. `pg_get_indexdef` output goes through unchanged (§7 forbids reformatting).
    #[test]
    fn indexes_render_as_their_postgresql_definitions() {
        let definition = ObjectDefinition::Table(TableDefinition {
            columns: vec![column(1, "id", "bigint", false, None)],
            indexes: vec![
                IndexDefinition {
                    name: "customer_pkey".into(),
                    definition_sql:
                        "CREATE UNIQUE INDEX customer_pkey ON public.customer USING btree (id)"
                            .into(),
                    primary: true,
                    unique: true,
                },
                IndexDefinition {
                    name: "customer_email_idx".into(),
                    definition_sql:
                        "CREATE INDEX customer_email_idx ON public.customer USING btree (email)"
                            .into(),
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
}
