use std::fmt::Write as _;
use std::time::Duration;

/// A driver-independent cell value (FR-021, 59.2).
#[derive(Clone, Debug, PartialEq)]
pub enum CellValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    Numeric(String),
    Text(String),
    DateTime(String),
    Uuid(String),
    Json(String),
    Array(String),
    Binary(String),
    Other(String),
}

impl CellValue {
    pub fn display_text(&self) -> &str {
        match self {
            Self::Null => "NULL",
            Self::Boolean(true) => "true",
            Self::Boolean(false) => "false",
            Self::Integer(_) | Self::Float(_) => unreachable!("numeric values use to_string"),
            Self::Numeric(value)
            | Self::Text(value)
            | Self::DateTime(value)
            | Self::Uuid(value)
            | Self::Json(value)
            | Self::Array(value)
            | Self::Binary(value)
            | Self::Other(value) => value,
        }
    }

    pub fn to_display_string(&self) -> String {
        match self {
            Self::Integer(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            value => value.display_text().to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub database_type: String,
    pub nullable: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionStatus {
    Queued,
    Running,
    #[default]
    Completed,
    Failed,
    Cancelling,
    Cancelled,
}

/// Complete result of one statement, suitable for any result destination (FR-021–FR-028).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<CellValue>>,
    pub affected_rows: Option<u64>,
    pub execution_time: Duration,
    pub status: ExecutionStatus,
    pub command_tag: Option<String>,
    pub notices: Vec<String>,
    pub automatic_limit: Option<u32>,
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// A tab-separated, copy-friendly representation (FR-022).
    pub fn as_text(&self) -> String {
        let mut output = String::new();
        if !self.columns.is_empty() {
            output.push_str(
                &self
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect::<Vec<_>>()
                    .join("\t"),
            );
            output.push('\n');
        }
        for row in &self.rows {
            output.push_str(
                &row.iter()
                    .map(CellValue::to_display_string)
                    .collect::<Vec<_>>()
                    .join("\t"),
            );
            output.push('\n');
        }
        if self.rows.is_empty()
            && let Some(tag) = &self.command_tag
        {
            output.push_str(tag);
            output.push('\n');
        }
        for notice in &self.notices {
            output.push_str(notice);
            output.push('\n');
        }
        let _ = write!(
            output,
            "\nStatus: {:?} · {} ms",
            self.status,
            self.execution_time.as_millis()
        );
        if !self.columns.is_empty() {
            let _ = write!(output, " · {} rows", self.row_count());
        } else if let Some(count) = self.affected_rows {
            let _ = write!(output, " · {count} rows affected");
        }
        if let Some(limit) = self.automatic_limit {
            let _ = write!(output, " · Automatic LIMIT {limit} applied");
        }
        output
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
    pub severity: Option<String>,
    pub code: Option<String>,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<u32>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let Some(detail) = &self.detail {
            write!(formatter, "\nDETAIL: {detail}")?;
        }
        if let Some(hint) = &self.hint {
            write!(formatter, "\nHINT: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for QueryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_result_distinguishes_null_from_strings() {
        let result = QueryResult {
            columns: vec![Column {
                name: "value".into(),
                database_type: "text".into(),
                nullable: Some(true),
            }],
            rows: vec![
                vec![CellValue::Null],
                vec![CellValue::Text(String::new())],
                vec![CellValue::Text("NULL".into())],
            ],
            ..Default::default()
        };

        assert!(result.as_text().starts_with("value\nNULL\n\nNULL\n"));
    }

    /// `RAISE NOTICE` output is part of what the statement produced, so it travels with the result
    /// rather than being dropped on the floor (§40).
    #[test]
    fn server_notices_are_rendered_with_the_result() {
        let result = QueryResult {
            notices: vec![
                "NOTICE: relation \"customer\" already exists, skipping".into(),
                "WARNING: nothing to do".into(),
            ],
            command_tag: Some("CREATE TABLE".into()),
            ..Default::default()
        };

        let text = result.as_text();
        assert!(
            text.contains("NOTICE: relation \"customer\" already exists, skipping"),
            "the notice should be readable in the text view: {text}"
        );
        assert!(text.contains("WARNING: nothing to do"));
        // Still one status line, and the notices come above it rather than after the summary.
        let notice_line = text.find("WARNING").expect("the notice should be present");
        let status_line = text.find("Status:").expect("the status should be present");
        assert!(notice_line < status_line);
    }
}
