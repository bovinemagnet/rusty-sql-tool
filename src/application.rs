use std::ops::Range;
use std::sync::Arc;

use uuid::Uuid;

use crate::config::ConnectionProfile;
use crate::database::{ConnectionInfo, ConnectionState, DatabaseObject, DatabaseProvider};
use crate::result::{ExecutionStatus, QueryError, QueryResult};
use crate::sql::{SqlError, prepare_explain, prepare_statement, relevant_sql, split_statements};
use crate::{DEFAULT_ROW_LIMIT, MAX_ROW_LIMIT};

pub mod command {
    pub const RUN: &str = "sql.run";
    pub const RUN_ALL: &str = "sql.run_all";
    pub const EXPLAIN: &str = "sql.explain";
    pub const CANCEL: &str = "sql.cancel";
    pub const NEW_EDITOR: &str = "sql.new_editor";
    pub const CONNECT: &str = "connection.connect";
    pub const DISCONNECT: &str = "connection.disconnect";
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultDisplay {
    #[default]
    Table,
    Text,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultDestination {
    #[default]
    Pane,
    Tab,
    Window,
}

/// State owned by one editor. Failures never clear its document or old results (FR-047).
#[derive(Clone, Debug)]
pub struct EditorState {
    pub id: Uuid,
    pub title: String,
    pub document: String,
    pub cursor: usize,
    pub selection: Option<Range<usize>>,
    pub connection: ConnectionProfile,
    pub row_limit: u32,
    pub display: ResultDisplay,
    pub destination: ResultDestination,
    pub execution_status: ExecutionStatus,
    pub results: Vec<QueryResult>,
    pub error: Option<QueryError>,
}

impl EditorState {
    pub fn new(connection: ConnectionProfile) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: "query.sql".into(),
            document: String::new(),
            cursor: 0,
            selection: None,
            connection,
            row_limit: DEFAULT_ROW_LIMIT,
            display: ResultDisplay::Table,
            destination: ResultDestination::Pane,
            execution_status: ExecutionStatus::Queued,
            results: Vec::new(),
            error: None,
        }
    }

    pub fn connection_identity(&self) -> String {
        self.connection.display_identity()
    }

    pub fn set_row_limit(&mut self, limit: u32) -> Result<(), SqlError> {
        if !(1..=MAX_ROW_LIMIT).contains(&limit) {
            return Err(SqlError::InvalidLimit);
        }
        self.row_limit = limit;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct StatementFailure {
    pub statement_index: usize,
    pub error: QueryError,
}

#[derive(Clone, Debug, Default)]
pub struct RunAllOutcome {
    pub results: Vec<QueryResult>,
    pub failure: Option<StatementFailure>,
}

/// Command/application layer between GPUI and the provider (sections 37–39).
pub struct CommandService {
    provider: Arc<dyn DatabaseProvider>,
}

impl CommandService {
    pub fn new(provider: Arc<dyn DatabaseProvider>) -> Self {
        Self { provider }
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.provider.state()
    }

    pub async fn connect(&self, profile: &ConnectionProfile) -> Result<ConnectionInfo, QueryError> {
        self.provider.connect(profile).await
    }

    pub async fn disconnect(&self) -> Result<(), QueryError> {
        if self.provider.state() == ConnectionState::Connected {
            let _ = self.provider.cancel().await;
        }
        self.provider.disconnect().await
    }

    pub async fn schemas(&self, refresh: bool) -> Result<Vec<String>, QueryError> {
        self.provider.schemas(refresh).await
    }

    pub async fn objects(
        &self,
        schema: &str,
        refresh: bool,
    ) -> Result<Vec<DatabaseObject>, QueryError> {
        self.provider.objects(schema, refresh).await
    }

    pub async fn run(&self, editor: &mut EditorState) -> Result<QueryResult, QueryError> {
        let sql = relevant_sql(&editor.document, editor.selection.clone(), editor.cursor)
            .map_err(query_selection_error)?;
        let prepared = prepare_statement(sql, editor.row_limit).map_err(query_selection_error)?;
        editor.execution_status = ExecutionStatus::Running;
        editor.error = None;
        match self.provider.execute(&prepared.sql).await {
            Ok(mut result) => {
                result.automatic_limit = prepared.automatic_limit;
                editor.execution_status = ExecutionStatus::Completed;
                editor.results = vec![result.clone()];
                Ok(result)
            }
            Err(error) => {
                editor.execution_status = if error.code.as_deref() == Some("57014") {
                    ExecutionStatus::Cancelled
                } else {
                    ExecutionStatus::Failed
                };
                editor.error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Executes in document order and preserves successful results when a statement fails (FR-015).
    pub async fn run_all(&self, editor: &mut EditorState) -> Result<RunAllOutcome, SqlError> {
        let ranges = split_statements(&editor.document)?;
        editor.execution_status = ExecutionStatus::Running;
        editor.error = None;
        let mut outcome = RunAllOutcome::default();
        for (statement_index, range) in ranges.into_iter().enumerate() {
            let prepared = prepare_statement(&editor.document[range], editor.row_limit)?;
            match self.provider.execute(&prepared.sql).await {
                Ok(mut result) => {
                    result.automatic_limit = prepared.automatic_limit;
                    outcome.results.push(result);
                }
                Err(error) => {
                    editor.execution_status = ExecutionStatus::Failed;
                    editor.error = Some(error.clone());
                    outcome.failure = Some(StatementFailure {
                        statement_index,
                        error,
                    });
                    editor.results = outcome.results.clone();
                    return Ok(outcome);
                }
            }
        }
        editor.execution_status = ExecutionStatus::Completed;
        editor.results = outcome.results.clone();
        Ok(outcome)
    }

    pub async fn explain(&self, editor: &mut EditorState) -> Result<QueryResult, QueryError> {
        let sql = relevant_sql(&editor.document, editor.selection.clone(), editor.cursor)
            .map_err(query_selection_error)?;
        let explained = prepare_explain(sql);
        editor.execution_status = ExecutionStatus::Running;
        editor.error = None;
        match self.provider.execute(&explained).await {
            Ok(result) => {
                editor.execution_status = ExecutionStatus::Completed;
                editor.results = vec![result.clone()];
                Ok(result)
            }
            Err(error) => {
                editor.execution_status = ExecutionStatus::Failed;
                editor.error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub async fn cancel(&self, editor: &mut EditorState) -> Result<(), QueryError> {
        editor.execution_status = ExecutionStatus::Cancelling;
        match self.provider.cancel().await {
            Ok(()) => {
                editor.execution_status = ExecutionStatus::Cancelled;
                Ok(())
            }
            Err(error) => {
                editor.execution_status = ExecutionStatus::Failed;
                editor.error = Some(error.clone());
                Err(error)
            }
        }
    }
}

fn query_selection_error(error: SqlError) -> QueryError {
    QueryError {
        message: error.to_string(),
        severity: None,
        code: None,
        detail: None,
        hint: None,
        position: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::config::{PostgresConfiguration, SecretString, SslMode};
    use crate::database::ObjectKind;

    #[derive(Default)]
    struct FakeProvider {
        statements: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl DatabaseProvider for FakeProvider {
        async fn connect(
            &self,
            _profile: &ConnectionProfile,
        ) -> Result<ConnectionInfo, QueryError> {
            Ok(ConnectionInfo {
                database: "test".into(),
                server_version: "PostgreSQL test".into(),
            })
        }

        async fn disconnect(&self) -> Result<(), QueryError> {
            Ok(())
        }

        async fn execute(&self, sql: &str) -> Result<QueryResult, QueryError> {
            self.statements.lock().unwrap().push(sql.to_owned());
            if sql.contains("FAIL") {
                return Err(QueryError {
                    message: "synthetic failure".into(),
                    severity: None,
                    code: None,
                    detail: None,
                    hint: None,
                    position: None,
                });
            }
            Ok(QueryResult::default())
        }

        async fn cancel(&self) -> Result<(), QueryError> {
            Ok(())
        }

        async fn schemas(&self, _refresh: bool) -> Result<Vec<String>, QueryError> {
            Ok(vec!["public".into()])
        }

        async fn objects(
            &self,
            schema: &str,
            _refresh: bool,
        ) -> Result<Vec<DatabaseObject>, QueryError> {
            Ok(vec![DatabaseObject {
                schema: schema.into(),
                name: "customer".into(),
                kind: ObjectKind::Table,
            }])
        }

        fn state(&self) -> ConnectionState {
            ConnectionState::Connected
        }
    }

    fn editor() -> EditorState {
        EditorState::new(ConnectionProfile::manual(
            "Test",
            PostgresConfiguration {
                host: "localhost".into(),
                port: 5432,
                database: "test".into(),
                username: "test".into(),
                password: SecretString::default(),
                ssl_mode: SslMode::Disable,
            },
        ))
    }

    #[tokio::test]
    async fn run_uses_selection_and_automatic_limit() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());
        let mut editor = editor();
        editor.document = "SELECT 1; SELECT 2;".into();
        editor.cursor = editor.document.len();
        editor.selection = Some(0..8);

        let result = service.run(&mut editor).await.unwrap();

        assert_eq!(provider.statements.lock().unwrap()[0], "SELECT 1 LIMIT 10");
        assert_eq!(result.automatic_limit, Some(10));
    }

    #[tokio::test]
    async fn run_all_stops_at_failure_and_preserves_previous_results() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());
        let mut editor = editor();
        editor.document = "SELECT 1; FAIL; SELECT 3;".into();

        let outcome = service.run_all(&mut editor).await.unwrap();

        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.failure.unwrap().statement_index, 1);
        assert_eq!(editor.results.len(), 1);
        assert_eq!(provider.statements.lock().unwrap().len(), 2);
        assert_eq!(editor.document, "SELECT 1; FAIL; SELECT 3;");
    }

    #[tokio::test]
    async fn explain_is_plain_explain() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());
        let mut editor = editor();
        editor.document = "SELECT 1;".into();
        editor.cursor = 3;

        service.explain(&mut editor).await.unwrap();

        let sql = &provider.statements.lock().unwrap()[0];
        assert_eq!(sql, "EXPLAIN SELECT 1;");
        assert!(!sql.contains("ANALYZE"));
    }
}
