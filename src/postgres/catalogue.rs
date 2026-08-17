//! Catalogue queries and their conversion into the definition model.
//!
//! §10 prefers `pg_catalog` and PostgreSQL's own helpers — `format_type`, `pg_get_expr` — over
//! reconstructing SQL by hand. Nothing here reaches GPUI, and nothing here formats for display.

use tokio_postgres::Row;

use crate::definition::{
    CheckConstraint, ColumnDefinition, ForeignKey, IndexDefinition, KeyConstraint,
    SequenceDefinition,
};

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
            b'p' => result.primary_key = Some(KeyConstraint { name, columns }),
            b'u' => result
                .unique_constraints
                .push(KeyConstraint { name, columns }),
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

/// Indexes of one table, PostgreSQL's own definitions via `pg_get_indexdef` rather than a
/// hand-reconstructed column list (§10). The primary key's index sorts first.
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

/// `pg_get_viewdef(oid, true)` pretty-prints, which is PostgreSQL's own formatting rather than
/// ours — §7 forbids reformatting, not asking PostgreSQL to format.
pub const VIEW_DEFINITION: &str = "SELECT pg_catalog.pg_get_viewdef(c.oid, true), \
     c.relkind = 'm' \
     FROM pg_catalog.pg_class c \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('v','m')";

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
