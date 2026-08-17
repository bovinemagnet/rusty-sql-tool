use std::time::Duration;

use rusty_sql_tool::application::{CommandService, EditorState};
use rusty_sql_tool::config::{ConnectionProfile, SecretString};
use rusty_sql_tool::database::{ConnectionState, DatabaseObject, DatabaseProvider, ObjectKind};
use rusty_sql_tool::definition::ObjectDefinition;
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

    // Notices are raised on the connection rather than returned as rows, so they only prove
    // themselves against a real server (§40).
    let raised = provider
        .execute("DO $$ BEGIN RAISE NOTICE 'phase one notice'; END $$;")
        .await
        .expect("DO block should execute");
    assert!(
        raised
            .notices
            .iter()
            .any(|notice| notice.contains("phase one notice")),
        "the server notice should travel with the result: {:?}",
        raised.notices
    );
    let quiet = provider
        .execute("SELECT 1")
        .await
        .expect("query should execute");
    assert!(
        quiet.notices.is_empty(),
        "notices must not carry over to the next statement: {:?}",
        quiet.notices
    );

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
        table
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
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
