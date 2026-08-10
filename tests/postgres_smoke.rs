use std::time::Duration;

use rusty_sql_tool::application::{CommandService, EditorState};
use rusty_sql_tool::config::ConnectionProfile;
use rusty_sql_tool::database::DatabaseProvider;
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

    let info = provider
        .connect(&profile)
        .await
        .expect("provider should connect");
    assert_eq!(info.database, profile.configuration.database);
    assert!(info.server_version.contains("PostgreSQL"));

    let schemas = provider.schemas(true).await.expect("schemas should load");
    assert!(!schemas.is_empty());
    let objects = provider
        .objects(&schemas[0], true)
        .await
        .expect("objects should load");
    assert!(objects.iter().all(|object| object.schema == schemas[0]));

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
}
