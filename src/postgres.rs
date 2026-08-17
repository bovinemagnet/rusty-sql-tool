use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use rustls::crypto::CryptoProvider;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_postgres::config::SslMode as DriverSslMode;
use tokio_postgres::{AsyncMessage, Client, Config, NoTls, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::config::{ConnectionProfile, SslMode};
use crate::database::{
    ConnectionInfo, ConnectionState, DatabaseObject, DatabaseProvider, ObjectKind,
};
use crate::definition::{ObjectDefinition, TableDefinition};
use crate::result::{CellValue, Column, ExecutionStatus, QueryError, QueryResult};

mod catalogue;

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

struct Session {
    client: Arc<Client>,
    connection_task: JoinHandle<()>,
    ssl_mode: SslMode,
    /// Notices the server raised on this connection, filled by the connection task and drained by
    /// the statement they belong to (§40).
    notices: Arc<Mutex<Vec<String>>>,
}

/// PostgreSQL implementation. GPUI never receives any type from this module (59.2, 59.4).
pub struct PostgresProvider {
    session: Mutex<Option<Session>>,
    state: AtomicU8,
    schemas_cache: RwLock<Option<Vec<String>>>,
    objects_cache: RwLock<HashMap<String, Vec<DatabaseObject>>>,
    definitions_cache: RwLock<HashMap<DefinitionKey, ObjectDefinition>>,
}

impl Default for PostgresProvider {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            schemas_cache: RwLock::new(None),
            objects_cache: RwLock::new(HashMap::new()),
            definitions_cache: RwLock::new(HashMap::new()),
        }
    }
}

impl PostgresProvider {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::Release);
    }

    fn driver_config(profile: &ConnectionProfile) -> Config {
        let source = &profile.configuration;
        let mut config = Config::new();
        config
            .host(&source.host)
            .port(source.port)
            .dbname(&source.database)
            .user(&source.username)
            .application_name("rusty-sql-tool")
            .connect_timeout(Duration::from_secs(10))
            .ssl_mode(match source.ssl_mode {
                SslMode::Disable => DriverSslMode::Disable,
                SslMode::Prefer => DriverSslMode::Prefer,
                SslMode::Require => DriverSslMode::Require,
            });
        if !source.password.is_empty() {
            config.password(source.password.expose());
        }
        config
    }

    async fn install_connection<T>(
        &self,
        result: Result<
            (
                Client,
                tokio_postgres::Connection<tokio_postgres::Socket, T>,
            ),
            tokio_postgres::Error,
        >,
        ssl_mode: SslMode,
    ) -> Result<ConnectionInfo, QueryError>
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (client, mut connection) =
            result.map_err(|error| safe_error(&error, "connection failed"))?;
        let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = notices.clone();
        let connection_task = tokio::spawn(async move {
            // Driving the connection message by message rather than awaiting it whole is what makes
            // notices reachable at all. Messages are handled in order, so a notice is recorded
            // before the response that ends the statement it belongs to.
            //
            // Connection errors change observable command results; they are intentionally not
            // logged here because driver messages can contain sensitive server information.
            while let Some(Ok(message)) =
                std::future::poll_fn(|context| connection.poll_message(context)).await
            {
                if let AsyncMessage::Notice(notice) = message {
                    sink.lock()
                        .await
                        .push(notice_line(notice.severity(), notice.message()));
                }
            }
        });
        let row = client
            .query_one("SELECT current_database(), version()", &[])
            .await
            .map_err(|error| safe_error(&error, "connection validation failed"))?;
        let info = ConnectionInfo {
            database: row.get(0),
            server_version: row.get(1),
        };
        *self.session.lock().await = Some(Session {
            client: Arc::new(client),
            connection_task,
            ssl_mode,
            notices,
        });
        self.set_state(ConnectionState::Connected);
        Ok(info)
    }

    fn tls_connector() -> Option<MakeRustlsConnect> {
        if !ensure_rustls_crypto_provider() {
            return None;
        }
        MakeRustlsConnect::with_native_certs()
            .map(|(connector, _certificate_warnings)| connector)
            .ok()
    }

    async fn with_client<R>(
        &self,
        operation: impl AsyncFnOnce(&Client) -> Result<R, tokio_postgres::Error>,
    ) -> Result<R, QueryError> {
        let client = self
            .session
            .lock()
            .await
            .as_ref()
            .map(|session| session.client.clone())
            .ok_or_else(|| simple_error("database is disconnected"))?;
        operation(&client)
            .await
            .map_err(|error| safe_error(&error, "PostgreSQL operation failed"))
    }

    /// Takes whatever the server has raised so far, leaving the buffer empty for the next
    /// statement. Notices are not logged: they are query output, and §44 keeps that out of the log.
    async fn take_notices(&self) -> Vec<String> {
        let buffer = self
            .session
            .lock()
            .await
            .as_ref()
            .map(|session| session.notices.clone());
        match buffer {
            Some(notices) => std::mem::take(&mut *notices.lock().await),
            None => Vec::new(),
        }
    }

    /// Runs only the catalogue queries the object's kind requires. Later tasks add arms; anything
    /// still unhandled reports itself rather than rendering blank (§5).
    // TableDefinition gains `indexes` in the next task; ..default() stays load-bearing.
    #[allow(clippy::needless_update)]
    async fn load_definition(
        &self,
        object: &DatabaseObject,
    ) -> Result<ObjectDefinition, QueryError> {
        match object.kind {
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
            kind => Ok(ObjectDefinition::Unsupported {
                kind,
                reason: "PostgreSQL provides no definition for this object".into(),
            }),
        }
    }
}

/// GPUI and the PostgreSQL TLS adapter enable different Rustls crypto backends.
/// Rustls deliberately panics if both features are present and no process-level
/// provider was selected, so Phase 1 consistently selects Ring before TLS setup.
fn ensure_rustls_crypto_provider() -> bool {
    if CryptoProvider::get_default().is_some() {
        return true;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    CryptoProvider::get_default().is_some()
}

#[async_trait]
impl DatabaseProvider for PostgresProvider {
    async fn connect(&self, profile: &ConnectionProfile) -> Result<ConnectionInfo, QueryError> {
        self.disconnect().await?;
        self.set_state(ConnectionState::Connecting);
        let target = &profile.configuration;
        // Host, port, database and user identify the attempt; the password is not logged and the
        // URL that would carry it is never assembled (FR-033, §43).
        tracing::info!(
            connection = %profile.name,
            host = %target.host,
            port = target.port,
            database = %target.database,
            user = %target.username,
            ssl_mode = ?target.ssl_mode,
            "connecting"
        );
        let config = Self::driver_config(profile);
        let result = match profile.configuration.ssl_mode {
            SslMode::Disable => {
                self.install_connection(config.connect(NoTls).await, SslMode::Disable)
                    .await
            }
            SslMode::Prefer | SslMode::Require => {
                let connector = match Self::tls_connector() {
                    Some(connector) => connector,
                    None => {
                        self.set_state(ConnectionState::Failed);
                        return Err(simple_error("could not load system TLS certificates"));
                    }
                };
                self.install_connection(
                    config.connect(connector).await,
                    profile.configuration.ssl_mode,
                )
                .await
            }
        };
        match &result {
            Ok(info) => tracing::info!(
                database = %info.database,
                server_version = %info.server_version,
                "connected"
            ),
            Err(error) => {
                self.set_state(ConnectionState::Failed);
                tracing::warn!(
                    connection = %profile.name,
                    database = %target.database,
                    error = %error,
                    "connection failed"
                );
            }
        }
        result
    }

    async fn disconnect(&self) -> Result<(), QueryError> {
        if self.state() == ConnectionState::Disconnected {
            return Ok(());
        }
        self.set_state(ConnectionState::Disconnecting);
        if let Some(session) = self.session.lock().await.take() {
            session.connection_task.abort();
        }
        *self.schemas_cache.write().await = None;
        self.objects_cache.write().await.clear();
        self.definitions_cache.write().await.clear();
        self.set_state(ConnectionState::Disconnected);
        tracing::info!("disconnected");
        Ok(())
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult, QueryError> {
        let started = Instant::now();
        tracing::debug!(statement = %crate::logging::statement(sql), "executing");
        // Anything left over belongs to an earlier statement and must not be reported against
        // this one.
        let _ = self.take_notices().await;
        let outcome = self
            .with_client(async |client| {
                let statement = client.prepare(sql).await?;
                if statement.columns().is_empty() {
                    let affected = client.execute(&statement, &[]).await?;
                    return Ok(QueryResult {
                        affected_rows: Some(affected),
                        command_tag: Some(command_name(sql)),
                        status: ExecutionStatus::Completed,
                        ..QueryResult::default()
                    });
                }

                let columns = statement
                    .columns()
                    .iter()
                    .map(|column| Column {
                        name: column.name().to_owned(),
                        database_type: column.type_().name().to_owned(),
                        nullable: None,
                    })
                    .collect::<Vec<_>>();
                let mut rows = Vec::new();
                let messages = client.simple_query(sql).await?;
                for message in messages {
                    if let SimpleQueryMessage::Row(row) = message {
                        rows.push(
                            columns
                                .iter()
                                .enumerate()
                                .map(|(index, column)| match row.get(index) {
                                    Some(value) => value_from_text(value, &column.database_type),
                                    None => CellValue::Null,
                                })
                                .collect(),
                        );
                    }
                }
                Ok(QueryResult {
                    columns,
                    rows,
                    status: ExecutionStatus::Completed,
                    ..QueryResult::default()
                })
            })
            .await;
        // Counts and timings only. Logging the rows themselves would put database content in the
        // log, which §44 rules out.
        match outcome {
            Ok(mut result) => {
                result.execution_time = started.elapsed();
                result.notices = self.take_notices().await;
                tracing::info!(
                    rows = result.rows.len(),
                    affected_rows = ?result.affected_rows,
                    notices = result.notices.len(),
                    elapsed_ms = result.execution_time.as_millis(),
                    "statement completed"
                );
                Ok(result)
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    elapsed_ms = started.elapsed().as_millis(),
                    "statement failed"
                );
                Err(error)
            }
        }
    }

    async fn cancel(&self) -> Result<(), QueryError> {
        tracing::info!("cancelling the running statement");
        let (token, ssl_mode) = self
            .session
            .lock()
            .await
            .as_ref()
            .map(|session| (session.client.cancel_token(), session.ssl_mode))
            .ok_or_else(|| simple_error("database is disconnected"))?;
        match ssl_mode {
            SslMode::Disable => token
                .cancel_query(NoTls)
                .await
                .map_err(|error| safe_error(&error, "query cancellation failed")),
            SslMode::Prefer | SslMode::Require => token
                .cancel_query(
                    Self::tls_connector()
                        .ok_or_else(|| simple_error("could not load system TLS certificates"))?,
                )
                .await
                .map_err(|error| safe_error(&error, "query cancellation failed")),
        }
    }

    async fn schemas(&self, refresh: bool) -> Result<Vec<String>, QueryError> {
        if !refresh && let Some(cached) = self.schemas_cache.read().await.clone() {
            return Ok(cached);
        }
        let schemas: Vec<String> = self
            .with_client(async |client| {
                let rows = client
                    .query(
                        "SELECT nspname FROM pg_catalog.pg_namespace \
                         WHERE nspname NOT LIKE 'pg\\_%' ESCAPE '\\' \
                         AND nspname <> 'information_schema' ORDER BY nspname",
                        &[],
                    )
                    .await?;
                Ok(rows.into_iter().map(|row| row.get(0)).collect())
            })
            .await?;
        tracing::debug!(count = schemas.len(), refresh, "loaded schemas");
        *self.schemas_cache.write().await = Some(schemas.clone());
        Ok(schemas)
    }

    async fn objects(
        &self,
        schema: &str,
        refresh: bool,
    ) -> Result<Vec<DatabaseObject>, QueryError> {
        if !refresh && let Some(cached) = self.objects_cache.read().await.get(schema).cloned() {
            return Ok(cached);
        }
        let objects: Vec<DatabaseObject> = self
            .with_client(async |client| {
                let rows = client
                    .query(
                        "SELECT object_name, object_kind FROM (\
                           SELECT c.relname AS object_name, CASE c.relkind \
                             WHEN 'r' THEN 'table' WHEN 'p' THEN 'table' \
                             WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialised_view' \
                             WHEN 'S' THEN 'sequence' END AS object_kind \
                           FROM pg_catalog.pg_class c \
                           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                           WHERE n.nspname = $1 AND c.relkind IN ('r','p','v','m','S') \
                           UNION ALL \
                           SELECT p.proname, CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END \
                           FROM pg_catalog.pg_proc p \
                           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                           WHERE n.nspname = $1\
                         ) objects ORDER BY object_kind, object_name",
                        &[&schema],
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .filter_map(|row| {
                        let kind: String = row.get(1);
                        let kind = match kind.as_str() {
                            "table" => ObjectKind::Table,
                            "view" => ObjectKind::View,
                            "materialised_view" => ObjectKind::MaterialisedView,
                            "function" => ObjectKind::Function,
                            "procedure" => ObjectKind::Procedure,
                            "sequence" => ObjectKind::Sequence,
                            _ => return None,
                        };
                        Some(DatabaseObject {
                            schema: schema.to_owned(),
                            name: row.get(0),
                            kind,
                        })
                    })
                    .collect())
            })
            .await?;
        tracing::debug!(
            schema,
            count = objects.len(),
            refresh,
            "loaded schema objects"
        );
        self.objects_cache
            .write()
            .await
            .insert(schema.to_owned(), objects.clone());
        Ok(objects)
    }

    async fn definition(
        &self,
        object: &DatabaseObject,
        refresh: bool,
    ) -> Result<ObjectDefinition, QueryError> {
        let key = DefinitionKey::of(object);
        if !refresh && let Some(cached) = self.definitions_cache.read().await.get(&key).cloned() {
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

    fn state(&self) -> ConnectionState {
        match self.state.load(Ordering::Acquire) {
            value if value == ConnectionState::Connecting as u8 => ConnectionState::Connecting,
            value if value == ConnectionState::Connected as u8 => ConnectionState::Connected,
            value if value == ConnectionState::Disconnecting as u8 => {
                ConnectionState::Disconnecting
            }
            value if value == ConnectionState::Failed as u8 => ConnectionState::Failed,
            _ => ConnectionState::Disconnected,
        }
    }
}

fn value_from_text(value: &str, database_type: &str) -> CellValue {
    match database_type {
        "bool" => CellValue::Boolean(value == "t" || value == "true"),
        "int2" | "int4" | "int8" | "oid" => value
            .parse()
            .map(CellValue::Integer)
            .unwrap_or_else(|_| CellValue::Numeric(value.to_owned())),
        "float4" | "float8" => value
            .parse()
            .map(CellValue::Float)
            .unwrap_or_else(|_| CellValue::Numeric(value.to_owned())),
        "numeric" | "money" => CellValue::Numeric(value.to_owned()),
        "date" | "time" | "timetz" | "timestamp" | "timestamptz" | "interval" => {
            CellValue::DateTime(value.to_owned())
        }
        "uuid" => CellValue::Uuid(value.to_owned()),
        "json" | "jsonb" => CellValue::Json(value.to_owned()),
        "bytea" => CellValue::Binary(value.to_owned()),
        name if name.starts_with('_') => CellValue::Array(value.to_owned()),
        "text" | "varchar" | "bpchar" | "name" | "char" => CellValue::Text(value.to_owned()),
        _ => CellValue::Other(value.to_owned()),
    }
}

fn command_name(sql: &str) -> String {
    sql.split_whitespace()
        .next()
        .unwrap_or("COMMAND")
        .trim_matches(|character: char| !character.is_ascii_alphabetic())
        .to_ascii_uppercase()
}

/// A server notice as the result carries it: severity first, so `NOTICE` and `WARNING` are told
/// apart in the text view without a second field.
fn notice_line(severity: &str, message: &str) -> String {
    format!("{severity}: {message}")
}

fn simple_error(message: &str) -> QueryError {
    QueryError {
        message: message.to_owned(),
        severity: None,
        code: None,
        detail: None,
        hint: None,
        position: None,
    }
}

fn safe_error(error: &tokio_postgres::Error, fallback: &str) -> QueryError {
    let Some(database_error) = error.as_db_error() else {
        return simple_error(fallback);
    };
    QueryError {
        message: database_error.message().to_owned(),
        severity: Some(database_error.severity().to_owned()),
        code: Some(database_error.code().code().to_owned()),
        detail: database_error.detail().map(ToOwned::to_owned),
        hint: database_error.hint().map(ToOwned::to_owned),
        position: database_error.position().map(|position| match position {
            tokio_postgres::error::ErrorPosition::Original(position) => *position,
            tokio_postgres::error::ErrorPosition::Internal { position, .. } => *position,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_keeps_its_severity_alongside_its_message() {
        assert_eq!(
            notice_line("NOTICE", "relation \"customer\" already exists, skipping"),
            "NOTICE: relation \"customer\" already exists, skipping"
        );
        assert_eq!(
            notice_line("WARNING", "nothing to do"),
            "WARNING: nothing to do"
        );
    }

    #[test]
    fn maps_common_postgres_values_without_driver_types() {
        assert_eq!(value_from_text("42", "int8"), CellValue::Integer(42));
        assert_eq!(
            value_from_text("{one,two}", "_text"),
            CellValue::Array("{one,two}".into())
        );
        assert_eq!(
            value_from_text("\\x00ff", "bytea"),
            CellValue::Binary("\\x00ff".into())
        );
    }

    /// Pins the crypto-provider selection only. Building the connector additionally needs a system
    /// CA store, which a minimal CI image may not have — asserting on it would fail the build for a
    /// reason unrelated to the multi-backend fix this test exists to cover.
    #[test]
    fn installs_explicit_rustls_provider_when_multiple_backends_are_enabled() {
        assert!(ensure_rustls_crypto_provider());
        assert!(CryptoProvider::get_default().is_some());
    }

    /// Collects everything logged on this thread while `body` runs, so a test can assert on the
    /// real subscriber output rather than on what the call sites were supposed to write.
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn captured_logs(body: impl FnOnce()) -> String {
        let logs = CapturedLogs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, body);
        let captured = logs.0.lock().unwrap().clone();
        String::from_utf8(captured).expect("log output should be text")
    }

    fn unreachable_profile(password: &str) -> ConnectionProfile {
        ConnectionProfile::manual(
            "Test",
            crate::config::PostgresConfiguration {
                host: "127.0.0.1".into(),
                // Nothing listens on port 1, so the attempt is refused immediately.
                port: 1,
                database: "example_db".into(),
                username: "someone".into(),
                password: crate::config::SecretString::new(password),
                ssl_mode: SslMode::Disable,
            },
        )
    }

    /// §44: SQL may itself contain sensitive values, so the statement text is withheld unless it
    /// has been explicitly enabled.
    #[test]
    fn a_statement_is_not_logged_verbatim_by_default() {
        let logs = captured_logs(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime for the attempt");
            let provider = PostgresProvider::new();
            // Disconnected, so this fails at once — the statement is logged on the way in.
            let _ = runtime.block_on(provider.execute("SELECT nhs_number FROM patient"));
        });

        assert!(
            logs.contains("executing"),
            "the statement should be logged at all, got:\n{logs}"
        );
        if !crate::logging::sql_logging_enabled() {
            assert!(
                !logs.contains("patient"),
                "the statement text leaked with SQL logging disabled:\n{logs}"
            );
        }
    }

    /// FR-033 and §43: a failed connection is exactly where a driver error is most likely to carry
    /// the connection string, so the failure must be logged without the password reaching the log.
    #[test]
    fn a_failed_connection_is_logged_without_the_password() {
        let password = "hunter2-must-never-be-logged";
        let profile = unreachable_profile(password);

        let logs = captured_logs(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime for the attempt");
            let provider = PostgresProvider::new();
            let error = runtime
                .block_on(provider.connect(&profile))
                .expect_err("nothing is listening on port 1");
            assert!(
                !error.to_string().contains(password),
                "the password reached the error message: {error}"
            );
        });

        assert!(
            logs.contains("example_db"),
            "the failed connection should be logged with its safe context, got:\n{logs}"
        );
        assert!(
            !logs.contains(password),
            "the password appeared in the log output:\n{logs}"
        );
    }
}
