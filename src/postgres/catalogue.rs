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
