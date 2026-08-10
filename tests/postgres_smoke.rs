use std::time::Duration;

use rusty_sql_tool::application::{CommandService, EditorState};
use rusty_sql_tool::config::{ConnectionProfile, SecretString};
use rusty_sql_tool::database::{ConnectionState, DatabaseProvider};
use rusty_sql_tool::postgres::PostgresProvider;
use rusty_sql_tool::result::CellValue;

/// Live acceptance smoke test for FR-001, FR-006–FR-010 and FR-021–FR-028.
///
/// It is ignored by default because normal development and CI must not depend on a
/// PostgreSQL server. Set `RUSTY_SQL_TEST_DATABASE_URL` to run it explicitly.
#[tokio::test]
#[ignore = "requires RUSTY_SQL_TEST_DATABASE_URL and a live PostgreSQL server"]
async fn connects_browses_and_executes_against_postgres() {
    let database_url = std::env::var("RUSTY_SQL_TEST_DATABASE_URL")
        .expect("RUSTY_SQL_TEST_DATABASE_URL must be set for the live smoke test");
    let profile = ConnectionProfile::from_database_url(&database_url)
        .expect("test database URL should be valid");
    let provider = PostgresProvider::new();
    assert_eq!(provider.state(), ConnectionState::Disconnected);

    let info = provider
        .connect(&profile)
        .await
        .expect("provider should connect");
    assert_eq!(info.database, profile.configuration.database);
    assert!(info.server_version.contains("PostgreSQL"));
    assert_eq!(provider.state(), ConnectionState::Connected);

    let schemas = provider.schemas(true).await.expect("schemas should load");
    assert!(!schemas.is_empty());
    assert_eq!(
        provider.schemas(false).await.expect("schemas should cache"),
        schemas
    );
    let objects = provider
        .objects(&schemas[0], true)
        .await
        .expect("objects should load");
    assert!(objects.iter().all(|object| object.schema == schemas[0]));
    assert_eq!(
        provider
            .objects(&schemas[0], false)
            .await
            .expect("objects should cache"),
        objects
    );

    let result = provider
        .execute("SELECT 1::bigint AS one, NULL::text AS absent")
        .await
        .expect("query should execute");
    assert_eq!(result.columns.len(), 2);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], CellValue::Integer(1));
    assert_eq!(result.rows[0][1], CellValue::Null);

    let service = CommandService::new(provider.clone());
    let mut editor = EditorState::new(profile.clone());
    editor.document = "SELECT generate_series(1, 100) AS number;".into();
    editor.cursor = 10;
    let limited = service
        .run(&mut editor)
        .await
        .expect("command layer query should execute");
    assert_eq!(limited.rows.len(), 10);
    assert_eq!(limited.automatic_limit, Some(10));

    editor.document = "SELECT 1;".into();
    editor.cursor = 3;
    let explained = service
        .explain(&mut editor)
        .await
        .expect("plain EXPLAIN should execute");
    assert!(!explained.rows.is_empty());

    let provider_for_query = provider.clone();
    let long_query =
        tokio::spawn(async move { provider_for_query.execute("SELECT pg_sleep(30)").await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    provider
        .cancel()
        .await
        .expect("active query should be cancellable");
    let cancellation = tokio::time::timeout(Duration::from_secs(3), long_query)
        .await
        .expect("cancelled query should finish promptly")
        .expect("query task should not panic")
        .expect_err("pg_sleep query should report cancellation");
    assert_eq!(cancellation.code.as_deref(), Some("57014"));

    provider
        .disconnect()
        .await
        .expect("provider should disconnect");
    assert_eq!(provider.state(), ConnectionState::Disconnected);
    provider
        .disconnect()
        .await
        .expect("disconnect should be idempotent");
    let disconnected_error = provider
        .execute("SELECT 1")
        .await
        .expect_err("execution must be blocked while disconnected");
    assert_eq!(disconnected_error.message, "database is disconnected");

    provider
        .connect(&profile)
        .await
        .expect("provider should reconnect");
    assert_eq!(provider.state(), ConnectionState::Connected);
    provider
        .disconnect()
        .await
        .expect("provider should disconnect after reconnect");
}

/// Failure-state acceptance for FR-028, FR-033 and reliability section 46.
#[tokio::test]
#[ignore = "requires RUSTY_SQL_TEST_DATABASE_URL and a live PostgreSQL server"]
async fn connection_failure_is_safe_and_recoverable() {
    let database_url = std::env::var("RUSTY_SQL_TEST_DATABASE_URL")
        .expect("RUSTY_SQL_TEST_DATABASE_URL must be set for the live smoke test");
    let mut profile = ConnectionProfile::from_database_url(&database_url)
        .expect("test database URL should be valid");
    profile.configuration.database = "rusty_sql_tool_database_that_must_not_exist".into();
    profile.configuration.password = SecretString::new("phase-one-secret-must-not-leak");
    let provider = PostgresProvider::new();

    let error = provider
        .connect(&profile)
        .await
        .expect_err("missing database should fail to connect");

    assert_eq!(provider.state(), ConnectionState::Failed);
    assert_eq!(error.code.as_deref(), Some("3D000"));
    assert!(!error.to_string().contains("postgresql://"));
    assert!(!error.to_string().contains("phase-one-secret-must-not-leak"));

    provider
        .disconnect()
        .await
        .expect("failed provider should disconnect cleanly");
    assert_eq!(provider.state(), ConnectionState::Disconnected);
}
