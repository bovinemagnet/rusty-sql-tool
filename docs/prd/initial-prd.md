# Product Requirements Document

## Rust SQL Desktop Client using GPUI

**Working title:** RustSQL / GPUI SQL Client
**Status:** Draft
**Target platform:** Desktop
**Primary implementation language:** Rust
**UI framework:** GPUI
**Initial database:** PostgreSQL
**Primary inspiration:** Zed editor layout and interaction model

---

# 1. Product Summary

The product is a fast, native desktop SQL client written in Rust using the GPUI framework used by the Zed editor.

The application will provide developers and database administrators with a lightweight environment for:

* Connecting to databases.
* Browsing database objects.
* Writing SQL.
* Executing SQL statements.
* Viewing query results.
* Inspecting query execution plans.
* Cancelling running queries.

The user interface will follow a layout similar to Zed, with a database connection/object explorer on the left and an editor-oriented workspace on the right.

The initial release will focus exclusively on PostgreSQL.

The primary goal is not to reproduce the functionality of large database IDEs such as DataGrip or DBeaver. Instead, the product should provide a fast, focused, keyboard-friendly SQL environment with the responsiveness and visual simplicity associated with Zed.

---

# 2. Product Vision

The product should feel like:

> **Zed, but focused on interacting with databases and SQL.**

The product should prioritize:

1. Fast startup.
2. Low memory usage.
3. Native desktop performance.
4. Keyboard-oriented workflows.
5. Minimal UI clutter.
6. Excellent SQL editing.
7. Fast query execution.
8. Clear presentation of query results.
9. Safe interaction with databases.
10. An architecture that can support additional database engines in future phases.

The application should remain useful even when only a single SQL editor tab is open.

It should not require the user to create a project or workspace before executing SQL.

---

# 3. Goals

## 3.1 Phase 1 Goals

Phase 1 will provide a usable PostgreSQL SQL client.

The user must be able to:

* Launch the application.
* Define or load a PostgreSQL connection.
* Load PostgreSQL connection information from a `.env` file.
* Connect to PostgreSQL.
* Disconnect from PostgreSQL.
* Browse database objects in a read-only tree.
* Create one or more SQL editor tabs.
* Write SQL with syntax highlighting.
* Execute the current SQL statement.
* Execute selected SQL.
* Execute all SQL in the current editor.
* Execute `EXPLAIN` against a query.
* Cancel a running query.
* Limit returned rows automatically.
* Change the default result row limit.
* View results as a table.
* View results as text.
* Choose where results appear.
* View SQL execution errors.
* View query timing and result metadata.

---

# 4. Non-Goals for Phase 1

The following functionality is explicitly outside the Phase 1 scope:

* MySQL.
* MariaDB.
* Oracle.
* Microsoft SQL Server.
* SQLite.
* CockroachDB-specific functionality.
* Database object editing.
* Table data editing.
* Schema modification UI.
* Visual query builder.
* Database diagrams.
* ER diagrams.
* Git integration.
* SQL autocomplete based on schema metadata.
* AI-assisted SQL generation.
* SQL formatting.
* Stored procedure debugging.
* Database migrations.
* Query scheduling.
* Query history synchronization.
* Cloud account integration.
* SSH tunnels.
* Connection sharing.
* Team collaboration.
* Database monitoring dashboards.
* Graphical `EXPLAIN` plans.
* Import/export tooling.
* CSV editing.
* Transaction management UI beyond the normal PostgreSQL connection behavior.

Some of these capabilities may be considered for subsequent phases.

---

# 5. Target Users

## 5.1 Software Developers

Developers who regularly need to:

* Inspect database schemas.
* Test SQL queries.
* Debug application data.
* Explore PostgreSQL databases.
* Run ad-hoc SQL.

## 5.2 Solution Architects

Architects who need a lightweight database exploration tool without launching a full database IDE.

## 5.3 Database Engineers

Database-focused users who want fast access to PostgreSQL for query development and schema inspection.

## 5.4 Power Users

Users comfortable working directly with SQL who prefer a keyboard-centric environment rather than a GUI-heavy database administration tool.

---

# 6. Design Principles

## 6.1 Editor First

The SQL editor is the primary application surface.

Database browsing and query results support the editor rather than dominate the interface.

## 6.2 Fast

Interactions should feel immediate.

Opening an editor, switching tabs, opening the schema tree, or rendering several hundred rows should not introduce noticeable UI latency.

## 6.3 Progressive Complexity

The initial interface should remain simple.

Advanced functionality should be discoverable without overwhelming a user who only wants to:

1. Connect.
2. Write SQL.
3. Run SQL.
4. Inspect results.

## 6.4 Keyboard Friendly

All common operations should eventually have keyboard shortcuts.

Phase 1 should establish commands for at least:

* Execute.
* Execute all.
* Explain.
* Stop execution.
* New SQL tab.
* Connect/disconnect where appropriate.

## 6.5 Safe Defaults

The application must avoid unnecessarily expensive queries where reasonable.

The default result limit is one important safety mechanism.

`EXPLAIN` should use plain PostgreSQL `EXPLAIN` in Phase 1 and **must not automatically use `EXPLAIN ANALYZE`**, because `ANALYZE` executes the statement and can have side effects.

---

# 7. High-Level Layout

The default window will follow a Zed-inspired structure.

```text
+---------------------------------------------------------------+
| Application / Workspace Tabs                                  |
+----------------------+----------------------------------------+
|                      | SQL Tab                                |
| Connections          |----------------------------------------|
|                      | [Connect] [Disconnect]                  |
| > Development        | [▶ Run] [▶▶ Run All] [Explain] [Stop] |
|   > public           |                             Limit: [10] |
|     > Tables         |----------------------------------------|
|       > customer     |                                        |
|       > orders       | SELECT *                               |
|     > Views          | FROM customer;                         |
|     > Functions      |                                        |
|     > ...            |                                        |
|                      |                                        |
|                      |----------------------------------------|
|                      | Results                                |
|                      | [Table] [Text]                         |
|                      |                                        |
+----------------------+----------------------------------------+
| Status / query execution information                          |
+---------------------------------------------------------------+
```

The exact visual design should follow GPUI/Zed conventions where practical rather than attempting to copy Zed pixel-for-pixel.

---

# 8. Main Application Areas

The application consists of four major concepts:

1. **Connections pane**
2. **SQL editor**
3. **SQL toolbar**
4. **Results viewer**

---

# 9. Connections Pane

## 9.1 Purpose

The Connections pane provides:

* Database connections.
* Connection state.
* Database object navigation.

It appears on the left side of the application by default.

The pane should be collapsible and resizable.

---

# 10. Connection Profiles

Phase 1 supports PostgreSQL.

A connection profile should contain:

* Name.
* Host.
* Port.
* Database.
* Username.
* Password.
* SSL configuration where required.

Default PostgreSQL port:

```text
5432
```

The internal connection model should not be PostgreSQL-specific where avoidable, because additional database providers are expected in later phases.

Conceptually:

```text
ConnectionProfile
    id
    name
    provider
    configuration
```

Where:

```text
provider = postgres
```

for Phase 1.

---

# 11. `.env` File Support

Phase 1 must support creating a connection from environment variables.

At minimum the application should support:

```text
DATABASE_URL=postgresql://user:password@localhost:5432/database
```

The application should also consider supporting commonly used individual PostgreSQL environment variables:

```text
PGHOST
PGPORT
PGDATABASE
PGUSER
PGPASSWORD
```

A project-local `.env` file may contain:

```text
DATABASE_URL=postgresql://developer:secret@localhost:5432/example
```

The application must not modify `.env` files automatically.

Passwords loaded from `.env` should not be displayed in plain text in the user interface.

---

# 12. Connection States

A connection has the following conceptual states:

```text
Disconnected
Connecting
Connected
Disconnecting
Failed
```

The UI must visibly indicate the current state.

Example:

```text
● Development
```

could indicate connected, while another visual treatment indicates disconnected.

Exact icons and colours are implementation details.

---

# 13. Connect Button

The SQL editor toolbar will contain a **Connect** action.

When pressed:

1. Determine the connection assigned to the SQL tab.
2. Establish a PostgreSQL connection.
3. Validate the connection.
4. Update the UI state.
5. Load database metadata.

If a connection is already established, Connect should be disabled or replaced by an appropriate connected state.

---

# 14. Disconnect Button

The toolbar will contain a **Disconnect** action.

Disconnect must:

* Close database resources belonging to that connection.
* Update the connection state.
* Preserve SQL editor contents.
* Preserve already-rendered query results.
* Prevent further execution until reconnected.

If a query is currently executing, disconnect behaviour must be predictable.

Preferred Phase 1 behaviour:

1. Attempt to cancel the active query.
2. Close the database connection.
3. Update UI state.

---

# 15. Database Object Tree

After connecting, the Connections pane will display database objects.

The tree is read-only in Phase 1.

Example:

```text
Development
└── database_name
    ├── Schemas
    │   ├── public
    │   │   ├── Tables
    │   │   │   ├── customer
    │   │   │   └── orders
    │   │   ├── Views
    │   │   ├── Materialized Views
    │   │   ├── Functions
    │   │   ├── Procedures
    │   │   ├── Sequences
    │   │   └── Types
    │   └── audit
    └── ...
```

At minimum Phase 1 should display:

* Schemas.
* Tables.
* Views.
* Materialized views.
* Functions.
* Procedures where available.
* Sequences.

Additional object types may be added where metadata retrieval is straightforward.

---

# 16. Lazy Loading

Database metadata should be lazily loaded.

For example, connecting should not require retrieving every table and function from a database containing thousands of objects.

Preferred behaviour:

1. Load schemas.
2. User expands schema.
3. Load categories/object names.
4. Cache results for the connection session.

The UI must remain responsive while metadata is retrieved.

Metadata requests must run asynchronously from the GPUI rendering thread.

---

# 17. Refreshing the Object Tree

Phase 1 should provide a refresh operation for either:

* The connection, or
* A schema/object branch.

Refreshing retrieves metadata again from PostgreSQL.

This is useful after SQL such as:

```sql
CREATE TABLE ...
```

has been executed outside or inside the application.

---

# 18. SQL Workspace

The central application area contains SQL editor tabs.

Example:

```text
query.sql
customer-analysis.sql
orders.sql
```

A SQL editor tab has an associated:

* SQL document.
* Connection.
* Result destination preference.
* Result display preference.
* Result row limit.

---

# 19. SQL Editor

The SQL editor is the main interface for entering SQL.

Phase 1 requirements:

* Multiline editing.
* PostgreSQL SQL syntax highlighting.
* Cursor movement.
* Text selection.
* Copy/paste.
* Undo/redo.
* Line numbers.
* Scrollbars or equivalent scrolling.
* Multiple SQL statements.
* SQL comments.
* Reasonable handling of large SQL documents.

PostgreSQL syntax highlighting should recognise common constructs including:

```sql
SELECT
INSERT
UPDATE
DELETE
WITH
CREATE
ALTER
DROP
JOIN
WHERE
GROUP BY
ORDER BY
HAVING
LIMIT
RETURNING
EXPLAIN
BEGIN
COMMIT
ROLLBACK
```

and PostgreSQL-specific syntax where supported by the syntax grammar.

---

# 20. SQL Toolbar

Each SQL editor will expose the following primary actions:

```text
Connect
Disconnect

▶ Run
▶▶ Run All
Explain
■ Stop

Result Limit: 10
```

The exact icons may follow the icon conventions available through the GPUI/Zed ecosystem.

---

# 21. Run / Play

The **Run** action executes the most relevant SQL statement.

Execution priority should be:

### Case 1 — Text selected

If the user has selected SQL text:

```sql
SELECT *
FROM customer
WHERE active = true;
```

execute the selected text.

### Case 2 — No selection

Determine the SQL statement containing the cursor and execute that statement.

For example:

```sql
SELECT * FROM customer;

SELECT * FROM orders;
```

If the cursor is inside the second statement, execute:

```sql
SELECT * FROM orders;
```

This behaviour is preferable to requiring users to manually select every query.

---

# 22. Run All / Play All

The **Run All** action executes all statements in the SQL editor.

Example:

```sql
CREATE TEMP TABLE x AS
SELECT ...

SELECT *
FROM x;
```

Both statements will execute in document order.

The result viewer must support multiple statement results.

A conceptual display could be:

```text
Result 1
CREATE TABLE
Execution time: 14 ms

Result 2
10 rows
Execution time: 23 ms
```

The application must not assume every SQL statement produces rows.

---

# 23. Explain

The **Explain** action operates on:

1. Selected SQL, otherwise
2. The SQL statement containing the cursor.

The application executes:

```sql
EXPLAIN <statement>
```

Phase 1 must **not** automatically run:

```sql
EXPLAIN ANALYZE
```

because it executes the underlying statement.

The returned PostgreSQL plan can initially be displayed using the standard text result viewer.

Example:

```text
Seq Scan on customer
  Filter: (active = true)
```

A richer graphical execution-plan viewer can be considered for a future phase.

---

# 24. Stop

The **Stop** button is enabled when a SQL operation is running.

Selecting Stop must attempt to cancel the active PostgreSQL query.

The UI must transition to a cancellation state rather than freezing while cancellation occurs.

Possible states:

```text
Running
Cancelling
Cancelled
Completed
Failed
```

After cancellation, the SQL editor must remain usable.

---

# 25. Result Row Limit

A **Result Limit** control appears at the top of the SQL editor.

Default:

```text
10
```

This means a query such as:

```sql
SELECT *
FROM customer;
```

should effectively execute as:

```sql
SELECT *
FROM customer
LIMIT 10;
```

The UI should indicate that the limit was automatically applied.

For example:

```text
10 rows
Automatic LIMIT 10 applied
```

---

# 26. User-Defined Result Limit

The user may change the value.

Example:

```text
Result Limit: [100]
```

A query without a limit will therefore receive:

```sql
LIMIT 100
```

The control should accept sensible positive integer values.

A reasonable Phase 1 maximum may be defined to protect application stability, although the architecture should not assume results always fit completely in memory.

---

# 27. Explicit SQL Limits

An explicitly supplied SQL limit takes precedence over the editor Result Limit.

Example:

```sql
SELECT *
FROM customer
LIMIT 50;
```

must remain:

```sql
SELECT *
FROM customer
LIMIT 50;
```

The application must not rewrite it to `LIMIT 10`.

Similarly, queries using PostgreSQL equivalents such as:

```sql
FETCH FIRST 20 ROWS ONLY
```

should not receive an additional automatic limit.

---

# 28. Limit Injection Rules

Automatic result limiting must only apply when appropriate.

It should primarily target row-returning query statements such as:

```sql
SELECT
WITH ... SELECT
VALUES
```

The application must not blindly append:

```sql
LIMIT 10
```

to every SQL string.

For example:

```sql
UPDATE customer
SET active = false;
```

must **not** become:

```sql
UPDATE customer
SET active = false
LIMIT 10;
```

Likewise:

```sql
CREATE TABLE ...
DROP TABLE ...
BEGIN
COMMIT
```

must not receive limits.

The implementation should perform SQL-aware statement analysis rather than simplistic string matching.

This requirement is important because SQL can contain:

* Comments.
* CTEs.
* Subqueries.
* String literals containing words such as `LIMIT`.
* Trailing semicolons.
* PostgreSQL-specific syntax.

The product requirement is therefore:

> Automatic limit injection must preserve the semantic meaning and syntactic validity of supported PostgreSQL statements.

---

# 29. Result Presentation

Query results can be displayed in two formats:

1. Table.
2. Text.

---

# 30. Table Result View

The default result mode for tabular queries should be a grid.

Example:

```text
| id | first_name | last_name | active |
|----|------------|-----------|--------|
| 1  | Alice      | Jones     | true   |
| 2  | Bob        | Smith     | false  |
```

The result grid must support:

* Column headers.
* Horizontal scrolling.
* Vertical scrolling.
* Null values.
* PostgreSQL numeric values.
* Text values.
* Boolean values.
* Date/time values.
* UUIDs.
* JSON/JSONB.
* Arrays represented appropriately.
* Binary values represented safely.

`NULL` must be visually distinguishable from:

```text
""
```

and:

```text
"NULL"
```

The exact representation may initially be:

```text
NULL
```

with a distinct visual treatment.

---

# 31. Text Result View

The user may switch results into a text-oriented representation.

This view is useful for:

* Copying data.
* EXPLAIN output.
* Logs.
* SQL commands returning textual data.
* JSON-heavy results.

The text view should use a monospaced font.

---

# 32. Result Destination

The user can determine where query results are displayed.

Phase 1 should support the following conceptual destinations:

### 32.1 Results Pane

Results appear below the SQL editor.

```text
SQL Editor
--------------------
Results
```

This should be the default.

### 32.2 Results Tab

Results open as another workspace tab.

Example:

```text
query.sql | Result: query.sql
```

This is useful when the user wants more screen space for examining data.

### 32.3 Separate Window

Results open in a separate native application window.

This supports multi-monitor workflows.

### 32.4 Rendered Results View

The architecture should support a dedicated rendered result surface so future result types can display richer content.

For Phase 1 this primarily represents:

* Table rendering.
* Text rendering.

Future renderers might include:

* JSON.
* Charts.
* Query plans.
* Markdown.
* Spatial data.

The result destination preference should be controlled by the user rather than hard-coded by the query type.

---

# 33. Result Metadata

Every completed SQL statement should provide useful execution metadata.

At minimum:

```text
Rows returned: 10
Execution time: 18 ms
Status: Completed
```

Where relevant:

```text
Rows affected: 234
```

For automatically limited queries:

```text
Result limit: 10
Automatic limit applied
```

Error queries display:

```text
Status: Failed
Execution time: 4 ms
```

---

# 34. Query Errors

PostgreSQL errors must be shown clearly.

For example:

```text
ERROR: column "custmer_id" does not exist
LINE 3: WHERE custmer_id = 10
              ^
```

Where PostgreSQL returns location information, the editor should eventually use it to indicate the source location.

Phase 1 should at minimum:

* Display the PostgreSQL error message.
* Display PostgreSQL detail/hint information when available.
* Preserve the SQL editor contents.
* Keep the database connection usable where possible.

The user should not have to open application logs to understand a normal SQL syntax error.

---

# 35. Status Information

The application status area should expose relevant information such as:

```text
Connected: Development / mydatabase
PostgreSQL 17
Query completed in 18 ms
10 rows
```

The application should avoid excessive notifications for successful queries.

Normal query execution information belongs in the result/status interface rather than modal dialogs.

---

# 36. PostgreSQL Metadata

Phase 1 needs sufficient PostgreSQL metadata queries to construct the database tree.

Metadata should preferably use stable PostgreSQL catalogue or information-schema queries.

Potential metadata sources include:

```text
pg_catalog
information_schema
```

The database metadata layer should be hidden behind an abstraction such as:

```text
DatabaseMetadataProvider
```

rather than allowing GPUI components to directly execute metadata SQL.

---

# 37. Phase 2 — Object Definitions

Phase 2 introduces inspection of database object definitions.

The user will be able to select an object from the Connections tree and inspect its definition.

---

# 38. Table Definition

Selecting or opening a table should provide details including:

* Table name.
* Schema.
* Columns.
* Data types.
* Nullable state.
* Default values.
* Primary keys.
* Foreign keys.
* Unique constraints.
* Check constraints.
* Indexes.

Example:

```text
customer

Columns
---------------------------------------------------
id             bigint        NOT NULL
name           varchar(200)  NOT NULL
email          varchar(320)
active         boolean       NOT NULL DEFAULT true
created_at     timestamptz   NOT NULL

Primary Key
---------------------------------------------------
customer_pkey (id)

Indexes
---------------------------------------------------
customer_email_idx (email)
```

---

# 39. Object Definition SQL

Where PostgreSQL provides a meaningful SQL representation, Phase 2 should expose definition SQL.

Examples include:

* Views.
* Functions.
* Procedures.
* Indexes.
* Sequences.

For example, opening a view could display its SQL definition in a read-only SQL editor.

---

# 40. Phase 2 Interaction

The database tree might eventually support an interaction such as:

```text
customer
    Open Definition
```

or:

```text
Double-click customer
```

which opens:

```text
customer [Definition]
```

as another workspace tab.

The definition view remains read-only in Phase 2.

Editing database definitions is explicitly deferred.

---

# 41. Proposed Architecture

The application should maintain strong separation between:

* GPUI presentation.
* Editor state.
* Database state.
* SQL execution.
* PostgreSQL-specific implementation.
* Result representation.

A conceptual architecture is:

```text
+------------------------------------------------+
|                    GPUI                        |
|                                                |
| Connections Pane   SQL Editor   Result Viewer  |
+-------------------------+----------------------+
                          |
+------------------------------------------------+
|              Application / Command Layer       |
|                                                |
| Execute Query                                  |
| Explain Query                                  |
| Cancel Query                                   |
| Connect / Disconnect                           |
| Metadata Commands                              |
+------------------------------------------------+
                          |
+------------------------------------------------+
|                Database Abstraction            |
|                                                |
| Connection Manager                             |
| Query Executor                                 |
| Metadata Provider                              |
| Query Cancellation                             |
+------------------------------------------------+
                          |
+------------------------------------------------+
|              PostgreSQL Provider               |
|                                                |
| postgres connection                            |
| pg_catalog queries                             |
| PostgreSQL result conversion                   |
+------------------------------------------------+
```

---

# 42. Database Provider Abstraction

Although Phase 1 only supports PostgreSQL, the application should avoid embedding PostgreSQL assumptions throughout the UI.

A conceptual Rust interface might eventually resemble:

```text
DatabaseProvider
    connect()
    disconnect()
    execute()
    explain()
    cancel()
    schemas()
    tables()
    views()
    functions()
```

The exact Rust trait design is an implementation concern.

The product requirement is that adding another database engine should primarily require a new provider rather than rewriting the editor and result UI.

---

# 43. Query Execution Model

Query execution must occur outside the UI/rendering thread.

The GPUI event loop must remain responsive while SQL executes.

Conceptually:

```text
SQL Editor
     |
     v
Execution Request
     |
     v
Background Async Task
     |
     v
PostgreSQL
     |
     v
Result Model
     |
     v
GPUI Update
```

The UI should receive state transitions such as:

```text
Queued
Running
Completed
Failed
Cancelling
Cancelled
```

---

# 44. Query Result Model

The core result representation should be independent of GPUI.

For example, conceptually:

```text
QueryResult
    columns
    rows
    affected_rows
    execution_time
    status
    notices
```

Column metadata should include information such as:

```text
name
database_type
nullable
```

This abstraction enables the same result to be rendered:

* As a table.
* As text.
* In another tab.
* In another window.
* By future custom renderers.

---

# 45. Large Result Handling

Although Phase 1 defaults to only 10 rows, the result architecture should avoid assumptions that result sets are always tiny.

The application should be designed so later versions can support:

* Incremental result fetching.
* Virtualized tables.
* Pagination.
* Streaming.

The Phase 1 `LIMIT` feature is primarily a result-safety mechanism rather than full server-side pagination.

For that reason, the UI label should preferably be:

```text
Row Limit
```

rather than:

```text
Page Size
```

unless actual Next/Previous page navigation is implemented.

Default:

```text
Row Limit: 10
```

is clearer to users than implying that there are currently multiple navigable pages.

---

# 46. Connection Pooling

A SQL desktop client does not necessarily need a large application-style connection pool.

The implementation should favour predictable connection ownership and cancellation over aggressive pooling.

The architecture should nevertheless allow:

* Metadata requests.
* Query execution.
* Query cancellation.

to operate without blocking one another unnecessarily.

A minimal pool or controlled set of connections may therefore be appropriate.

The specific PostgreSQL Rust driver and pooling implementation are engineering decisions.

---

# 47. Security Requirements

Database credentials are sensitive.

Phase 1 must:

* Never write passwords to logs.
* Avoid displaying passwords in plain text.
* Avoid including credentials in error telemetry.
* Avoid exposing credentials in result windows.
* Treat `.env` contents as sensitive.
* Prevent accidental logging of complete connection URLs containing passwords.

If connection profiles are persisted, plaintext password persistence should preferably be avoided.

Native OS credential storage can be considered in a later phase.

---

# 48. Logging

Application logging should assist debugging without leaking SQL credentials or sensitive database content.

Suggested levels:

```text
ERROR
WARN
INFO
DEBUG
TRACE
```

Production logging should not routinely log:

* Passwords.
* Connection strings containing passwords.
* Entire result sets.

Whether complete SQL statements are logged should be configurable because SQL itself may contain sensitive values.

---

# 49. Performance Requirements

The application should target the responsiveness expected of a native editor.

Targets for normal workloads:

* Startup should feel immediate.
* Opening a new SQL tab should be effectively instantaneous.
* Typing must remain responsive regardless of database activity.
* Expanding a metadata tree must not block rendering.
* Query execution must not block editing.
* Switching result tabs should appear immediate for small result sets.
* Cancelling a query must not freeze the application.

Large database schemas may contain tens of thousands of objects, therefore database metadata trees should use lazy loading and efficient rendering.

---

# 50. Reliability Requirements

The application must handle common failures gracefully:

* PostgreSQL server unavailable.
* Incorrect password.
* DNS failure.
* SSL failure.
* Connection timeout.
* Query timeout.
* Syntax error.
* Permission denied.
* Query cancellation.
* Connection dropped while executing.
* Database server restart.
* Invalid `.env`.
* Missing `.env`.
* Unsupported `.env` variables.

Failures must not crash the entire application.

A failure in one SQL editor should not destroy other open SQL editors.

---

# 51. SQL Document Behaviour

SQL editor contents should remain available when:

* A query fails.
* A connection fails.
* The user disconnects.
* A query is cancelled.
* Result views are closed.

Longer term, SQL documents may be persisted automatically.

For Phase 1, normal Save/Open behaviour is desirable if straightforward, but database functionality should take priority over advanced file-management functionality.

---

# 52. Multiple Connections

The architecture should support multiple connection profiles from Phase 1 even if only a single database connection is commonly active.

An editor should clearly indicate the connection it will execute against.

Example:

```text
query.sql
Connection: Development
Database: customer_db
```

This is an important safety feature.

The application should avoid situations where users accidentally execute SQL against an unexpected database.

---

# 53. Connection Assignment

Each SQL editor should have a selected connection.

Possible UI:

```text
Connection: [ Development ▼ ]
```

Changing this selector changes the target of subsequent SQL execution.

The current connection should be visually prominent enough that a user can verify it before executing destructive SQL.

---

# 54. Destructive SQL

Phase 1 does not require confirmation dialogs for every:

```sql
DELETE
UPDATE
DROP
TRUNCATE
```

command.

Constant confirmation dialogs would interfere with the intended editor-first workflow.

Instead, protection should initially come from:

* Clearly visible connection/database identity.
* Predictable execution commands.
* No accidental execution during editing.
* Distinction between Run and Run All.

A configurable production-environment protection mechanism can be considered later.

---

# 55. Keyboard Commands

Phase 1 should provide keyboard actions for core operations.

Suggested logical commands:

```text
sql.run
sql.run_all
sql.explain
sql.cancel
sql.new_editor
connection.connect
connection.disconnect
```

Exact default key bindings should follow platform conventions and avoid conflicting with editor commands.

Command IDs should be independent of keyboard shortcuts so shortcuts can later be configurable.

---

# 56. Context Menus

The connection tree may expose lightweight context menus.

Phase 1 candidates:

```text
Connect
Disconnect
Refresh
New SQL Query
```

Phase 2 adds:

```text
Open Definition
```

Database modification actions should not appear in the Phase 1 object-tree context menu.

---

# 57. Visual States

The UI should clearly communicate asynchronous actions.

For query execution:

```text
▶ Running...
■ Stop
```

For connection:

```text
Connecting...
Connected
Connection failed
```

For metadata:

```text
Loading...
```

Long-running operations should not be represented solely through a spinning cursor.

---

# 58. Empty States

When no connection exists:

```text
No database connections.

Add a PostgreSQL connection
or load one from .env.
```

When disconnected:

```text
Development

Disconnected
[Connect]
```

When a query has not yet been executed:

```text
Run a query to see results.
```

Empty states should be useful but visually lightweight.

---

# 59. Phase 1 User Flow

A typical new-user flow:

### Step 1

Launch the application.

### Step 2

Application discovers a `.env` containing:

```text
DATABASE_URL=...
```

or the user creates a PostgreSQL connection manually.

### Step 3

User selects:

```text
Connect
```

### Step 4

The Connections pane becomes:

```text
Development
└── public
    ├── Tables
    ├── Views
    └── Functions
```

### Step 5

User opens a SQL tab and enters:

```sql
SELECT *
FROM customer;
```

### Step 6

The toolbar displays:

```text
Row Limit: 10
```

### Step 7

The user presses Run.

### Step 8

The application executes an effective query equivalent to:

```sql
SELECT *
FROM customer
LIMIT 10;
```

### Step 9

Results appear in the configured results destination.

### Step 10

The user changes:

```text
Table
```

to:

```text
Text
```

if desired.

---

# 60. Phase 1 Functional Requirements

## FR-001 PostgreSQL Connection

The application shall connect to a PostgreSQL database.

## FR-002 Manual Connection Configuration

The application shall allow users to configure PostgreSQL connection details.

## FR-003 `.env` Support

The application shall support reading PostgreSQL connection configuration from `.env`.

## FR-004 Connect

The application shall expose a Connect action.

## FR-005 Disconnect

The application shall expose a Disconnect action.

## FR-006 Database Tree

The application shall display PostgreSQL database objects in a read-only tree.

## FR-007 Schema Browser

The database tree shall display schemas.

## FR-008 Table Browser

The database tree shall display tables.

## FR-009 View Browser

The database tree shall display views.

## FR-010 Function Browser

The database tree shall display functions.

## FR-011 SQL Editor

The application shall provide a multiline SQL editor.

## FR-012 Syntax Highlighting

The SQL editor shall provide PostgreSQL SQL syntax highlighting.

## FR-013 Execute Selected SQL

Run shall execute selected SQL when a selection exists.

## FR-014 Execute Current Statement

Run shall execute the SQL statement containing the cursor when there is no selection.

## FR-015 Execute All

Run All shall execute all SQL statements in the active editor.

## FR-016 Explain

Explain shall execute PostgreSQL `EXPLAIN` against the selected/current statement.

## FR-017 Cancel

Stop shall attempt to cancel the currently executing SQL statement.

## FR-018 Default Limit

Row-returning queries without an explicit limit shall default to a result limit of 10.

## FR-019 Configurable Limit

The user shall be able to change the result limit from the SQL editor.

## FR-020 Preserve Explicit Limit

The application shall not replace an explicit SQL result limit with the editor default.

## FR-021 Table Results

The application shall display tabular results in a table/grid.

## FR-022 Text Results

The application shall display results in a text representation.

## FR-023 Result Pane

The application shall support displaying results in a pane associated with the SQL editor.

## FR-024 Result Tab

The application shall support displaying results in a separate tab.

## FR-025 Result Window

The application shall support displaying results in a separate application window.

## FR-026 Execution Time

The application shall display query execution time.

## FR-027 Row Count

The application shall display returned or affected row counts where available.

## FR-028 SQL Errors

The application shall display PostgreSQL query errors to the user.

## FR-029 Multiple Statements

The SQL editor shall support multiple SQL statements.

## FR-030 Metadata Refresh

The database object tree shall be refreshable.

---

# 61. Phase 2 Functional Requirements

## FR2-001 Table Details

Users shall be able to inspect table columns.

## FR2-002 Constraints

Users shall be able to inspect table constraints.

## FR2-003 Indexes

Users shall be able to inspect table indexes.

## FR2-004 View Definitions

Users shall be able to inspect view definitions.

## FR2-005 Function Definitions

Users shall be able to inspect PostgreSQL function definitions.

## FR2-006 Definition Tabs

Database object definitions shall be openable in workspace tabs.

## FR2-007 Read Only

Database object definitions shall remain read-only in Phase 2.

---

# 62. Phase 1 Acceptance Criteria

Phase 1 is considered functionally complete when the following scenario succeeds.

Given a `.env` containing:

```text
DATABASE_URL=postgresql://user:password@localhost/example
```

the user can launch the application and connect to PostgreSQL.

After connecting:

```text
public
├── Tables
├── Views
└── Functions
```

is visible in the Connections pane.

The user creates a SQL editor containing:

```sql
SELECT *
FROM customer;
```

Syntax highlighting is visible.

The toolbar displays:

```text
Run
Run All
Explain
Stop
Connect
Disconnect
Row Limit: 10
```

Selecting Run executes the query with an automatic effective limit of 10.

No more than 10 rows are returned unless the SQL contains its own explicit limit.

The results can be viewed:

* In table format.
* In text format.

The results can be opened:

* Below the SQL editor.
* In another tab.
* In another window.

The result includes:

* Row count.
* Execution duration.
* Success or error status.

A long-running query can be cancelled using Stop.

The SQL editor remains responsive while the query is executing.

---

# 63. Recommended Phase 1 Milestones

## Milestone 1 — GPUI Application Shell

Implement:

* Main window.
* Connections pane.
* Workspace.
* Tabs.
* SQL editor placeholder.
* Results placeholder.

Success criteria:

The basic Zed-style layout operates correctly.

---

## Milestone 2 — PostgreSQL Connection

Implement:

* PostgreSQL connection abstraction.
* Manual connection configuration.
* `.env`.
* Connect.
* Disconnect.
* Connection state.

Success criteria:

The application can reliably connect to PostgreSQL.

---

## Milestone 3 — Metadata Browser

Implement:

* Schemas.
* Tables.
* Views.
* Functions.
* Lazy tree loading.
* Refresh.

Success criteria:

The user can browse database structure without modifying it.

---

## Milestone 4 — SQL Editor

Implement:

* Editor.
* Syntax highlighting.
* Multiple SQL statements.
* Statement identification.
* Selection handling.

Success criteria:

The user can comfortably write PostgreSQL SQL.

---

## Milestone 5 — Query Execution

Implement:

* Run.
* Run All.
* Error handling.
* Execution timing.
* Row counts.

Success criteria:

SQL can be executed reliably without blocking the UI.

---

## Milestone 6 — Safe Result Limiting

Implement:

* Default limit 10.
* Limit control.
* SQL-aware limit detection.
* Automatic limit injection.

Success criteria:

```sql
SELECT * FROM customer;
```

returns a maximum of 10 rows by default, while:

```sql
SELECT * FROM customer LIMIT 50;
```

retains its explicit limit.

---

## Milestone 7 — Result Renderers

Implement:

* Result model.
* Table renderer.
* Text renderer.
* Null handling.
* PostgreSQL type rendering.

Success criteria:

Common PostgreSQL results render correctly.

---

## Milestone 8 — Result Destinations

Implement:

* Results pane.
* Results tab.
* Results window.
* User preference.

Success criteria:

A query result can be moved or rendered into each supported destination.

---

## Milestone 9 — Explain and Cancellation

Implement:

* Explain.
* Query cancellation.
* Running/cancelling states.

Success criteria:

Long-running queries can be interrupted without restarting the application.

---

## Milestone 10 — Phase 1 Stabilisation

Focus on:

* Error handling.
* Keyboard commands.
* Performance.
* Large schema testing.
* Connection failure handling.
* UI polish.
* Logging.
* Credential security.

---

# 64. Phase 2 Milestones

Phase 2 should build on the metadata abstraction rather than introducing a second schema-inspection architecture.

Suggested sequence:

1. Table column metadata.
2. Keys and constraints.
3. Indexes.
4. View definitions.
5. Function definitions.
6. Definition tabs.
7. Additional PostgreSQL object metadata.

---

# 65. Future Considerations

The architecture should leave room for subsequent functionality including:

* SQL autocomplete.
* Schema-aware autocomplete.
* Hover information.
* Go-to-table-definition.
* SQL formatting.
* SQL diagnostics.
* Query history.
* Saved queries.
* PostgreSQL notices.
* Multiple active result sets.
* CSV export.
* JSON export.
* Copy rows.
* Editable table data.
* Transaction controls.
* Execution-plan visualization.
* `EXPLAIN ANALYZE`.
* SQL snippets.
* Connection groups.
* SSH tunnelling.
* TLS certificate configuration.
* Secret/keychain storage.
* Read-only connection profiles.
* Production environment protection.
* MySQL.
* SQLite.
* Oracle.
* SQL Server.
* CockroachDB.
* DuckDB.

---

# 66. Product Decisions Recommended for Phase 1

Several decisions should be made early because they significantly affect architecture.

## 66.1 Use “Row Limit”, Not “Page Limit”

The first release is limiting rows rather than implementing true pagination.

Therefore:

```text
Row Limit: 10
```

is clearer than:

```text
Page Limit: 10
```

True pagination can later introduce:

```text
Previous
Page 2
Next
```

without changing the meaning of the existing control.

---

## 66.2 Keep Query Results Separate From Database Drivers

Database-driver row objects should not flow directly into GPUI components.

Convert them into an application-level result model first.

This will make:

* Multiple renderers.
* Separate windows.
* Additional database engines.
* Export.
* Virtualized tables.

considerably easier later.

---

## 66.3 Treat SQL Parsing as a Core Capability

Statement detection and safe `LIMIT` injection are more complicated than they initially appear.

They affect:

* Run current statement.
* Run selection.
* Run All.
* Explain.
* Limit injection.
* Future SQL diagnostics.
* Future autocomplete.

A reusable PostgreSQL-aware SQL parsing/tokenization layer should therefore be treated as part of the application's core architecture rather than implemented independently inside toolbar commands.

---

## 66.4 Keep GPUI Away From Database Logic

GPUI views should represent state and dispatch commands.

They should not directly contain PostgreSQL connection/query logic.

This separation will make async execution, testing, cancellation, and additional databases significantly easier.

---

## 66.5 Make Connection Identity Highly Visible

Running:

```sql
DROP TABLE customer;
```

against the wrong database is substantially more serious than an editor UX inconvenience.

Every SQL editor should therefore make its target connection visible without requiring the user to inspect the Connections pane.

For example:

```text
Development / customer_db
```

could appear directly in the SQL editor toolbar.

---

# 67. Proposed Phase Boundaries

## Phase 1 — PostgreSQL Query Client

**Theme:** Connect, query, inspect results.

Includes:

* GPUI desktop application.
* Zed-style layout.
* PostgreSQL.
* `.env`.
* Connections.
* Database object tree.
* SQL editor.
* Syntax highlighting.
* Run.
* Run All.
* Explain.
* Stop.
* Default row limit 10.
* User-configurable row limit.
* Table results.
* Text results.
* Results pane.
* Results tab.
* Results window.
* Errors.
* Timing.
* Row counts.

---

## Phase 2 — PostgreSQL Schema Inspection

**Theme:** Understand database objects.

Includes:

* Table definitions.
* Column metadata.
* Keys.
* Constraints.
* Indexes.
* View definitions.
* Function definitions.
* Definition tabs.
* Read-only schema inspection.

---

## Potential Phase 3 — Developer Productivity

Possible scope:

* SQL autocomplete.
* Schema-aware completion.
* Query history.
* Saved SQL.
* Formatting.
* Search database objects.
* Copy/export results.
* Enhanced `EXPLAIN`.

---

## Potential Phase 4 — Multi-Database

Possible scope:

* SQLite.
* MySQL/MariaDB.
* SQL Server.
* Oracle.
* DuckDB.

The generic database-provider architecture created during Phase 1 should significantly reduce the cost of this phase.

---

# 68. Definition of Success

Phase 1 will be successful if a developer can install the application and choose it instead of a general-purpose database IDE for everyday PostgreSQL query work.

The minimum successful workflow is:

```text
Open application
      ↓
Connect to PostgreSQL
      ↓
Browse schema
      ↓
Write SQL
      ↓
Run SQL
      ↓
Inspect results
      ↓
Modify SQL
      ↓
Run again
```

Every step should feel fast and require minimal interaction.

The central product differentiator should not simply be that the application is written in Rust.

The differentiator should be:

> **A fast, native, editor-first database client that brings the interaction model and responsiveness of a modern code editor to SQL development.**
