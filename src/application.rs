use std::ops::Range;
use std::sync::Arc;

use uuid::Uuid;

use crate::config::ConnectionProfile;
use crate::database::{ConnectionInfo, ConnectionState, DatabaseObject, DatabaseProvider};
use crate::definition::ObjectDefinition;
use crate::result::{ExecutionStatus, QueryError, QueryResult};
use crate::sql::{SqlError, prepare_explain, prepare_statement, relevant_sql, split_statements};
use crate::{DEFAULT_ROW_LIMIT, MAX_ROW_LIMIT};

pub mod command {
    pub const RUN: &str = "sql.run";
    pub const RUN_ALL: &str = "sql.run_all";
    pub const EXPLAIN: &str = "sql.explain";
    pub const CANCEL: &str = "sql.cancel";
    pub const NEW_EDITOR: &str = "sql.new_editor";
    pub const CLOSE_EDITOR: &str = "sql.close_editor";
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

    pub async fn definition(
        &self,
        object: &DatabaseObject,
        refresh: bool,
    ) -> Result<ObjectDefinition, QueryError> {
        self.provider.definition(object, refresh).await
    }

    pub async fn run(&self, editor: &mut EditorState) -> Result<QueryResult, QueryError> {
        let sql = relevant_sql(&editor.document, editor.selection.clone(), editor.cursor)
            .map_err(query_selection_error)?;
        let prepared = prepare_statement(sql, editor.row_limit).map_err(query_selection_error)?;
        // The limit decision is invisible from the provider, and FR-018/FR-020 are exactly the
        // rules a user will want to check when a result set surprises them.
        tracing::debug!(
            row_limit = editor.row_limit,
            automatic_limit = ?prepared.automatic_limit,
            "prepared statement"
        );
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
                editor.execution_status = failure_status(&error);
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
                    editor.execution_status = failure_status(&error);
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
                editor.execution_status = failure_status(&error);
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

fn failure_status(error: &QueryError) -> ExecutionStatus {
    if error.code.as_deref() == Some("57014") {
        ExecutionStatus::Cancelled
    } else {
        ExecutionStatus::Failed
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
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::config::{PostgresConfiguration, SecretString, SslMode};
    use crate::database::ObjectKind;
    use crate::definition::{ColumnDefinition, ObjectDefinition, TableDefinition};

    struct FakeProvider {
        statements: Mutex<Vec<String>>,
        state: AtomicU8,
        connect_calls: AtomicUsize,
        disconnect_calls: AtomicUsize,
        cancel_calls: AtomicUsize,
        cancel_fails: AtomicBool,
    }

    impl Default for FakeProvider {
        fn default() -> Self {
            Self {
                statements: Mutex::default(),
                state: AtomicU8::new(ConnectionState::Disconnected as u8),
                connect_calls: AtomicUsize::default(),
                disconnect_calls: AtomicUsize::default(),
                cancel_calls: AtomicUsize::default(),
                cancel_fails: AtomicBool::default(),
            }
        }
    }

    #[async_trait]
    impl DatabaseProvider for FakeProvider {
        async fn connect(
            &self,
            _profile: &ConnectionProfile,
        ) -> Result<ConnectionInfo, QueryError> {
            self.connect_calls.fetch_add(1, Ordering::Relaxed);
            self.state
                .store(ConnectionState::Connected as u8, Ordering::Release);
            Ok(ConnectionInfo {
                database: "test".into(),
                server_version: "PostgreSQL test".into(),
            })
        }

        async fn disconnect(&self) -> Result<(), QueryError> {
            self.disconnect_calls.fetch_add(1, Ordering::Relaxed);
            self.state
                .store(ConnectionState::Disconnected as u8, Ordering::Release);
            Ok(())
        }

        async fn execute(&self, sql: &str) -> Result<QueryResult, QueryError> {
            self.statements.lock().unwrap().push(sql.to_owned());
            if sql.contains("FAIL") {
                return Err(test_error("synthetic failure", None));
            }
            if sql.contains("CANCELLED") {
                return Err(test_error("cancelled", Some("57014")));
            }
            Ok(QueryResult::default())
        }

        async fn cancel(&self) -> Result<(), QueryError> {
            self.cancel_calls.fetch_add(1, Ordering::Relaxed);
            if self.cancel_fails.load(Ordering::Acquire) {
                return Err(test_error("cancellation failed", None));
            }
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

        // `TableDefinition` currently has one field; the struct-update syntax stays ready for the
        // constraint and index fields Tasks 4-5 add.
        #[allow(clippy::needless_update)]
        async fn definition(
            &self,
            _object: &DatabaseObject,
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

        fn state(&self) -> ConnectionState {
            match self.state.load(Ordering::Acquire) {
                value if value == ConnectionState::Connected as u8 => ConnectionState::Connected,
                value if value == ConnectionState::Failed as u8 => ConnectionState::Failed,
                _ => ConnectionState::Disconnected,
            }
        }
    }

    fn test_error(message: &str, code: Option<&str>) -> QueryError {
        QueryError {
            message: message.into(),
            severity: None,
            code: code.map(ToOwned::to_owned),
            detail: None,
            hint: None,
            position: None,
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

    #[tokio::test]
    async fn connect_and_disconnect_update_state_and_cancel_before_closing() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());
        let editor = editor();

        assert_eq!(service.connection_state(), ConnectionState::Disconnected);
        service.connect(&editor.connection).await.unwrap();
        assert_eq!(service.connection_state(), ConnectionState::Connected);

        service.disconnect().await.unwrap();

        assert_eq!(service.connection_state(), ConnectionState::Disconnected);
        assert_eq!(provider.connect_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.cancel_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.disconnect_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn disconnect_while_already_disconnected_does_not_cancel() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());

        service.disconnect().await.unwrap();

        assert_eq!(provider.cancel_calls.load(Ordering::Relaxed), 0);
        assert_eq!(provider.disconnect_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn disconnect_still_closes_when_cancellation_fails() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());
        let editor = editor();
        service.connect(&editor.connection).await.unwrap();
        provider.cancel_fails.store(true, Ordering::Release);

        service.disconnect().await.unwrap();

        assert_eq!(service.connection_state(), ConnectionState::Disconnected);
        assert_eq!(provider.cancel_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.disconnect_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn query_failure_preserves_document_and_previous_results() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider);
        let mut editor = editor();
        editor.document = "FAIL;".into();
        editor.cursor = 2;
        editor.results = vec![QueryResult {
            command_tag: Some("PREVIOUS".into()),
            ..QueryResult::default()
        }];

        let error = service.run(&mut editor).await.unwrap_err();

        assert_eq!(error.message, "synthetic failure");
        assert_eq!(editor.execution_status, ExecutionStatus::Failed);
        assert_eq!(editor.document, "FAIL;");
        assert_eq!(editor.results[0].command_tag.as_deref(), Some("PREVIOUS"));
        assert_eq!(editor.error.as_ref(), Some(&error));
    }

    #[tokio::test]
    async fn database_cancellation_sets_cancelled_state() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider);
        let mut editor = editor();
        editor.document = "CANCELLED;".into();
        editor.cursor = 3;

        let error = service.run(&mut editor).await.unwrap_err();

        assert_eq!(error.code.as_deref(), Some("57014"));
        assert_eq!(editor.execution_status, ExecutionStatus::Cancelled);
    }

    #[tokio::test]
    async fn explain_cancellation_sets_cancelled_state() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider);
        let mut editor = editor();
        editor.document = "CANCELLED;".into();
        editor.cursor = 3;

        let error = service.explain(&mut editor).await.unwrap_err();

        assert_eq!(error.code.as_deref(), Some("57014"));
        assert_eq!(editor.execution_status, ExecutionStatus::Cancelled);
    }

    #[tokio::test]
    async fn run_all_reports_database_cancellation_as_cancelled() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider);
        let mut editor = editor();
        editor.document = "SELECT 1; CANCELLED; SELECT 3;".into();

        let outcome = service.run_all(&mut editor).await.unwrap();

        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.failure.unwrap().statement_index, 1);
        assert_eq!(editor.execution_status, ExecutionStatus::Cancelled);
    }

    #[tokio::test]
    async fn stop_transitions_to_cancelled_on_success() {
        let provider = Arc::new(FakeProvider::default());
        let service = CommandService::new(provider.clone());
        let mut editor = editor();
        editor.execution_status = ExecutionStatus::Running;

        service.cancel(&mut editor).await.unwrap();

        assert_eq!(editor.execution_status, ExecutionStatus::Cancelled);
        assert_eq!(provider.cancel_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn stop_failure_is_visible_and_sets_failed_state() {
        let provider = Arc::new(FakeProvider::default());
        provider.cancel_fails.store(true, Ordering::Release);
        let service = CommandService::new(provider);
        let mut editor = editor();
        editor.execution_status = ExecutionStatus::Running;

        let error = service.cancel(&mut editor).await.unwrap_err();

        assert_eq!(editor.execution_status, ExecutionStatus::Failed);
        assert_eq!(editor.error.as_ref(), Some(&error));
    }

    #[test]
    fn row_limit_accepts_only_supported_positive_values() {
        let mut editor = editor();

        assert!(editor.set_row_limit(1).is_ok());
        assert!(editor.set_row_limit(MAX_ROW_LIMIT).is_ok());
        assert_eq!(editor.set_row_limit(0), Err(SqlError::InvalidLimit));
        assert_eq!(
            editor.set_row_limit(MAX_ROW_LIMIT + 1),
            Err(SqlError::InvalidLimit)
        );
    }

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
}
