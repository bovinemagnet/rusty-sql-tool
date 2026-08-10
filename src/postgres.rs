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
use tokio_postgres::{Client, Config, NoTls, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::config::{ConnectionProfile, SslMode};
use crate::database::{
    ConnectionInfo, ConnectionState, DatabaseObject, DatabaseProvider, ObjectKind,
};
use crate::result::{CellValue, Column, ExecutionStatus, QueryError, QueryResult};

struct Session {
    client: Arc<Client>,
    connection_task: JoinHandle<()>,
    ssl_mode: SslMode,
}

/// PostgreSQL implementation. GPUI never receives any type from this module (59.2, 59.4).
pub struct PostgresProvider {
    session: Mutex<Option<Session>>,
    state: AtomicU8,
    schemas_cache: RwLock<Option<Vec<String>>>,
    objects_cache: RwLock<HashMap<String, Vec<DatabaseObject>>>,
}

impl Default for PostgresProvider {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            state: AtomicU8::new(ConnectionState::Disconnected as u8),
            schemas_cache: RwLock::new(None),
            objects_cache: RwLock::new(HashMap::new()),
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
        let (client, connection) =
            result.map_err(|error| safe_error(&error, "connection failed"))?;
        let connection_task = tokio::spawn(async move {
            // Connection errors change observable command results; they are intentionally not
            // logged here because driver messages can contain sensitive server information.
            let _ = connection.await;
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
        if result.is_err() {
            self.set_state(ConnectionState::Failed);
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
        self.set_state(ConnectionState::Disconnected);
        Ok(())
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult, QueryError> {
        let started = Instant::now();
        let mut result = self
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
            .await?;
        result.execution_time = started.elapsed();
        Ok(result)
    }

    async fn cancel(&self) -> Result<(), QueryError> {
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
        self.objects_cache
            .write()
            .await
            .insert(schema.to_owned(), objects.clone());
        Ok(objects)
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
}
