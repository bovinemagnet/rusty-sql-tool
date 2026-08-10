# Product Requirements Documents

A fast, native desktop SQL client written in Rust using GPUI, in the spirit of the Zed editor.

Requirements are split by phase. Phase 1 is the committed scope; later phases are proposals and may be re-ordered or reduced once Phase 1 is in real use.

| Phase | Document | Theme | Status |
|---|---|---|---|
| 1 | [initial-prd.md](initial-prd.md) | Connect, query, inspect results | Draft, committed |
| 2 | [phase-2-schema-inspection.md](phase-2-schema-inspection.md) | Understand database objects | Proposal |
| 3 | [phase-3-developer-productivity.md](phase-3-developer-productivity.md) | Write SQL faster and more safely | Proposal |
| 4 | [phase-4-multi-database.md](phase-4-multi-database.md) | Additional database engines | Proposal |

## Requirement identifiers

Each phase numbers its functional requirements with its own prefix, so identifiers are unique and stable across documents:

| Phase | Prefix |
|---|---|
| 1 | `FR-nnn` |
| 2 | `FR2-nnn` |
| 3 | `FR3-nnn` |
| 4 | `FR4-nnn` |

Cite the identifier rather than a section number when tracing work back to a requirement — sections renumber, identifiers do not.

## What each phase depends on

The Phase 1 architecture is what makes the later phases affordable:

* The metadata provider abstraction — Phase 2 extends it rather than adding a second schema-inspection path.
* The SQL parsing layer — Phase 3 builds completion, formatting, diagnostics and statement classification on it.
* The database provider abstraction and the GPUI-independent result model — Phase 4 depends on both, and tests whether either leaked PostgreSQL assumptions.
