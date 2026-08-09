# Product Requirements Document — Phase 4

## Multi-Database Support

**Working title:** RustSQL / GPUI SQL Client
**Status:** Proposal
**Phase:** 4 — Multi-Database Support
**Author:** Paul Snow
**Version:** 0.0.0
**Depends on:** [Phase 1 — PostgreSQL Query Client](initial-prd.md), [Phase 2 — PostgreSQL Schema Inspection](phase-2-schema-inspection.md)

### Related documents

| Document | Theme |
|---|---|
| [Phase 1 — PostgreSQL Query Client](initial-prd.md) | Connect, query, inspect results |
| [Phase 2 — PostgreSQL Schema Inspection](phase-2-schema-inspection.md) | Understand database objects |
| [Phase 3 — Developer Productivity](phase-3-developer-productivity.md) | Write SQL faster and more safely |

---

# 1. Summary

Phase 4 adds database engines beyond PostgreSQL.

[Phase 1 section 38](initial-prd.md) states the requirement this phase exists to test:

> Adding another database engine should primarily require a new provider rather than rewriting the editor and result UI.

Phase 4 either confirms that or exposes where PostgreSQL assumptions leaked into the application. The first engine added is therefore as much an architectural audit as a feature.

---

# 2. Theme

**One client, several engines, no dialect knowledge in the UI.**

---

# 3. Engines in Scope

Proposed order, easiest and most useful first:

| Order | Engine | Rationale |
|---|---|---|
| 1 | SQLite | Embedded, no server, trivial test fixtures. Exposes assumptions about hosts, ports, users and schemas immediately. |
| 2 | DuckDB | Embedded, analytics-oriented, PostgreSQL-like dialect. Low marginal cost after SQLite. |
| 3 | MySQL / MariaDB | Most commonly requested server engine. Different metadata model and different cancellation mechanism. |
| 4 | SQL Server | Different limit syntax (`TOP`, `OFFSET/FETCH`), different catalogue. |
| 5 | Oracle | Highest cost: driver, client libraries, dialect and catalogue all differ substantially. |

CockroachDB is handled as a PostgreSQL-protocol variant rather than a separate provider, with capability differences declared rather than assumed.

Each engine is independently shippable. The phase does not require all five.

---

# 4. Goals

* Connect to at least one non-PostgreSQL engine using the existing UI.
* Browse its objects in the same Connections tree.
* Execute SQL against it with correct, dialect-aware row limiting.
* Cancel a running statement.
* Render results through the existing result model and renderers.
* Inspect object definitions where the engine supports it.
* Keep credentials safe across all engines.

---

# 5. Non-Goals

* Cross-database queries or joins between connections.
* Dialect translation of user SQL.
* Schema migration between engines.
* Data transfer between connections.
* Feature parity across engines where the engine itself does not offer the feature.

The application surfaces what an engine can do. It does not emulate what an engine cannot.

---

# 6. Provider Capability Model

Uniform behaviour across engines is impossible; predictable behaviour is not.

Each provider must declare its capabilities, and the UI must react to the declaration rather than to a hard-coded engine check.

Capabilities include at minimum:

| Capability | Effect on UI |
|---|---|
| Supports schemas | Whether the tree includes a schema level |
| Supports multiple databases per connection | Whether the tree includes a database level |
| Supports `EXPLAIN` | Whether the Explain action is enabled |
| Supports plan analysis | Whether the Phase 3 analyse action is offered |
| Supports server-side cancellation | Whether Stop cancels or only abandons |
| Row-limit syntax | Which limit form the injector produces |
| Supports materialised views, procedures, sequences | Which tree categories appear |
| Supports definition retrieval per object type | Which Phase 2 definition tabs are offered |

An action that an engine cannot support is disabled with an explanation, never silently absent and never failing at execution time with a driver error.

---

# 7. Dialect-Aware Row Limiting

[Phase 1 section 28](initial-prd.md) makes automatic limit injection a semantic operation rather than string concatenation. Phase 4 makes it per dialect.

| Engine | Limit form |
|---|---|
| PostgreSQL, MySQL/MariaDB, SQLite, DuckDB | `LIMIT n` |
| SQL Server | `OFFSET 0 ROWS FETCH NEXT n ROWS ONLY`, or `TOP n` where the statement shape requires it |
| Oracle | `FETCH FIRST n ROWS ONLY` on supported versions |

Requirements:

* Statement classification and limit injection are provider-aware operations, not a single PostgreSQL implementation with special cases bolted on.
* Detection of an existing explicit limit must recognise every form the target dialect supports, so [Phase 1 FR-020](initial-prd.md) holds on every engine.
* Where a provider cannot classify or safely rewrite a statement, it executes it unmodified, consistent with the Phase 1 fail-safe rule.
* Where a provider cannot express the row limit at all, the UI must say the limit was not applied rather than implying protection that is absent.

This is the single most dangerous area of the phase: a wrong rewrite changes what a user's statement does.

---

# 8. Metadata Across Engines

The Connections tree must accommodate engines whose object model differs from PostgreSQL's:

* SQLite has one database and no schemas, users or roles.
* MySQL treats database and schema as the same concept.
* SQL Server has database, then schema.
* Oracle organises by user/schema.

The tree structure must be derived from provider capabilities and provider-supplied nodes, rather than the PostgreSQL shape being assumed and then patched.

Object definitions from [Phase 2](phase-2-schema-inspection.md) follow the same rule: each provider supplies definitions for the object types it supports, through the same metadata abstraction.

---

# 9. Connection Profiles

The `provider` field already present in the [Phase 1 connection profile](initial-prd.md) becomes meaningful.

Requirements:

* The connection dialogue is driven by the provider's configuration schema. A file path for SQLite, host and port for MySQL, service name or TNS details for Oracle.
* `.env` support extends per provider where a conventional environment variable exists, without breaking the PostgreSQL behaviour defined in [Phase 1 section 11](initial-prd.md).
* The editor's connection indicator shows the engine as well as the connection and database, because engine now affects behaviour:

```text
Development / customer_db (PostgreSQL)
```

---

# 10. Errors and Results Across Engines

* Driver errors are translated into the application's error model by the provider, preserving the engine's message, and detail or hint where available.
* Type conversion into the result model is a provider responsibility, in line with [Phase 1 section 59.2](initial-prd.md). Engine-specific value types must not reach the renderers.
* Types with no natural representation in the result model must be rendered safely rather than approximated silently.

---

# 11. Connectivity and Credentials

Connectivity work is grouped here because it touches every provider, though it may be pulled forward if a Phase 1 or 2 user needs it sooner.

## 11.1 SSH Tunnelling

Optional SSH tunnel configuration per connection profile, so a connection can reach a database that is not directly routable.

## 11.2 TLS Configuration

Explicit TLS settings per profile: mode, CA certificate, client certificate and key, and host verification.

## 11.3 Native Credential Storage

Passwords stored in the operating system's credential store rather than in application configuration.

[Phase 1 section 43](initial-prd.md) defers this and, until it exists, avoids persisting passwords at all. This section closes that gap.

## 11.4 Connection Groups

Profiles organised into groups, for users who accumulate many connections across engines and environments.

---

# 12. Provider Conformance Testing

Adding an engine must not become a source of silent regressions in the others.

A shared conformance suite runs the same scenarios against every provider:

* Connect, disconnect, reconnect.
* Execute a row-returning statement and assert the result model.
* Execute a non-row-returning statement and assert affected-row reporting.
* Assert automatic limit application and explicit-limit preservation for that dialect.
* Assert that non-row-returning statements receive no limit.
* Cancel a long-running statement, where the engine supports cancellation.
* Enumerate the object tree.
* Retrieve a definition for each supported object type.
* Provoke a syntax error and a permission error, and assert both surface through the error model.

Embedded engines run these directly. Server engines run them against containers. A provider is not complete until its conformance run passes.

---

# 13. Functional Requirements

## FR4-001 Provider Registry

The application shall support multiple database providers selected by the connection profile's provider field.

## FR4-002 Capability Declaration

Each provider shall declare its capabilities, and the UI shall enable or disable actions from that declaration.

## FR4-003 Unsupported Actions

Actions unsupported by the active provider shall be visibly disabled with an explanation rather than failing at execution time.

## FR4-004 Dialect Limit Injection

Automatic row limiting shall use the limit syntax of the active provider's dialect.

## FR4-005 Dialect Limit Detection

Explicit limits shall be detected in every form the active dialect supports, and preserved.

## FR4-006 Limit Transparency

Where a provider cannot apply the row limit, the result metadata shall state that no limit was applied.

## FR4-007 Provider Metadata Tree

The object tree structure shall be derived from provider-supplied nodes rather than assuming the PostgreSQL object model.

## FR4-008 Provider Definitions

Object definitions shall be supplied per provider through the existing metadata abstraction.

## FR4-009 Provider Connection Configuration

The connection dialogue shall be driven by the active provider's configuration schema.

## FR4-010 Engine Identity

The SQL editor shall display the engine alongside the connection and database.

## FR4-011 Provider Error Translation

Driver errors shall be translated into the application error model, preserving the engine's message and any detail or hint.

## FR4-012 Provider Result Conversion

Engine-specific value types shall be converted into the result model by the provider and shall not reach the renderers.

## FR4-013 Cancellation

Where an engine supports server-side cancellation, Stop shall cancel the statement; where it does not, the UI shall state what Stop actually does.

## FR4-014 SSH Tunnelling

A connection profile shall optionally connect through an SSH tunnel.

## FR4-015 TLS Configuration

A connection profile shall support explicit TLS configuration.

## FR4-016 Credential Storage

Passwords shall be storable in the operating system credential store rather than in application configuration.

## FR4-017 Connection Groups

Connection profiles shall be organisable into groups.

## FR4-018 Conformance Suite

Every provider shall pass the shared provider conformance suite.

---

# 14. Acceptance Criteria

Phase 4 is considered functionally complete for a given engine when the following scenario succeeds for that engine.

The user creates a connection profile for the engine using a dialogue appropriate to it, and connects.

The Connections tree shows an object hierarchy appropriate to that engine, without empty PostgreSQL-shaped levels.

The SQL editor toolbar shows the connection, database and engine. Actions the engine does not support are disabled with an explanation.

`SELECT * FROM customer;` returns at most 10 rows, limited using that engine's syntax, and the result metadata reports that the limit was applied.

A statement with an explicit limit in that dialect executes unchanged.

A modifying statement receives no automatic limit.

A syntax error and a permission error each surface with the engine's own message.

A long-running statement is cancelled with Stop, or the UI states that the engine does not support cancellation.

The provider conformance suite passes.

No password for the connection appears in the application log or in application configuration files.

---

# 15. Milestones

## Milestone 1 — Provider Abstraction Audit

Review the Phase 1 to 3 code for leaked PostgreSQL assumptions before adding any engine. Extract the capability model.

Success criteria: a documented list of leaks, each fixed or accepted with a reason.

## Milestone 2 — Conformance Suite

Build the shared suite against the PostgreSQL provider first, so it is proven before it is used to judge new providers.

## Milestone 3 — SQLite Provider

The first non-PostgreSQL engine. Expected to expose assumptions about hosts, ports, users and schemas.

## Milestone 4 — Capability-Driven UI

Tree structure, action enablement and connection dialogue driven by capabilities rather than engine checks.

## Milestone 5 — Dialect-Aware Limiting

Per-provider statement classification and limit injection, with conformance coverage per dialect.

## Milestone 6 — DuckDB Provider

## Milestone 7 — MySQL / MariaDB Provider

## Milestone 8 — Connectivity and Credentials

SSH tunnelling, TLS configuration, native credential storage, connection groups.

## Milestone 9 — SQL Server Provider

## Milestone 10 — Oracle Provider

## Milestone 11 — Stabilisation

Cross-engine testing, error message quality, documentation of per-engine limitations.

---

# 16. Risks

* **Limit injection across dialects.** The highest-consequence risk in the phase: an incorrect rewrite silently changes a user's statement. Mitigated by fail-safe behaviour and per-dialect conformance tests.
* **Capability creep in the UI.** Every `if engine == postgres` added under time pressure erodes the abstraction. The audit in Milestone 1 exists to make that visible early.
* **Driver quality and licensing.** Rust driver maturity varies considerably by engine, and Oracle in particular may require external client libraries. Engine order should be revisited against driver reality before starting each provider.
* **Testing cost.** Server engines need containers in the test loop; without that, conformance results are aspirational.

---

# 17. Open Questions

* Should embedded engines such as SQLite and DuckDB be openable directly from a file path without creating a profile first? This suits their usage pattern, but it complicates connection identity.
* Should CockroachDB be a distinct provider or a capability variant of the PostgreSQL provider? A variant is assumed.
* How much per-engine behaviour should the user be able to see? A per-connection capability summary may be worth exposing directly in the UI.
