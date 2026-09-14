# SQL Diagnostics — Design

**Scope:** Phase 3 Milestone 4, first half — FR3-009 Diagnostics. The editor reports syntax
problems without preventing execution.

Milestone 4's other half, formatting (FR3-007 Formatting, FR3-008 Format Safety), is **out of
scope** and follows as its own piece. The two share only the tokeniser. Diagnostics comes first
because FR3-008 requires formatting to leave unparsable statements unmodified, and deciding a
statement is unparsable is exactly what this design computes — formatting can then ask rather than
grow a private duplicate.

## Decisions

1. **A `Diagnostic` is not a `SqlError`.** `SqlError` answers "why did this command fail" and is
   compared by value in `src/application.rs`; `Diagnostic` answers "what should the editor point
   at". Adding a span to `SqlError` would break those comparisons and sit badly on variants that
   have no position at all (`InvalidLimit`, `InvalidCursor`). `SqlError` is untouched. This follows
   the Milestone 1 precedent of `referenced_tables()` as a separate function rather than a field on
   `StatementAnalysis`: callers that do not need the work do not pay for it.

   ```rust
   pub enum Severity { Error, Warning }

   pub struct Diagnostic {
       pub range: Range<usize>,
       pub severity: Severity,
       pub message: String,
   }

   pub fn diagnose(sql: &str) -> Option<Diagnostic>
   ```

   `range` is a byte range into the `sql` passed in, matching `HighlightSpan` and `Token`.
   `diagnose` takes the whole editor document, not one statement, so a problem anywhere in it is
   found wherever the caret happens to be.

2. **One problem, the earliest.** `diagnose` returns at most one diagnostic. This is what `scan()`
   already does, and for lexical errors it is the honest answer: an unterminated quote turns
   everything after it into a string, so any later problem is an artefact of the first rather than
   a second mistake. Reporting the cascade would be reporting noise.

3. **The scanner keeps the position it already knows.** `scan()` finds every lexical problem and
   throws the location away, holding `Option<SqlError>`. That field widens to carry the byte range
   alongside the error. `tokenize()` discards the range, so no public signature changes and no
   existing caller is affected.

4. **Unbalanced parentheses point at the opener.** Today the error says only that the document is
   unbalanced. `scan()` gains a stack of open-parenthesis positions: a `)` with no opener reports
   its own range, and a `(` still outstanding at end of input reports the innermost one. This is
   the one behavioural improvement to existing detection, and it exists because a diagnostic with
   no useful position is not worth rendering.

5. **Two tiers of detection, with severity marking the difference.** Lexical problems are certain
   and carry `Severity::Error`. Statement-level checks are guesses and carry `Severity::Warning`,
   which reads as "check this" rather than "this is broken" — the honest strength of a guess.

6. **The statement tier runs only on a clean scan.** `split_statements` refuses a document it
   cannot read, so statement checks are unreachable until the lexical tier is satisfied. This
   ordering is what keeps the design free of error recovery.

7. **Diagnostics never gate execution.** FR3-009 requires it. Run, Run All and Explain keep failing
   through their own `SqlError` path, and no command consults `diagnose`.

## Detection

Lexical tier, all `Error`, all already detected by `scan()` and needing only a position:

| Problem | Range reported |
|---|---|
| Unterminated quoted string or identifier | The quote token, opener to end of input |
| Unterminated dollar-quoted body | The literal token, opener to end of input |
| Unterminated block comment | The comment token, `/*` to end of input |
| Unbalanced parentheses | The unmatched `(` or `)`, per decision 4 |

Statement tier, all `Warning`, reported for the first offending statement only:

| Problem | Range reported |
|---|---|
| Statement does not begin with a recognised keyword | The offending first word |
| Empty statement between semicolons | The span between the two semicolons |

The keyword allowlist is drawn from PostgreSQL's real statement set, not the obvious dozen. A name
absent from it produces a false warning on valid SQL, so the list is deliberately generous:
`ABORT`, `ALTER`, `ANALYZE`, `BEGIN`, `CALL`, `CHECKPOINT`, `CLOSE`, `CLUSTER`, `COMMENT`,
`COMMIT`, `COPY`, `CREATE`, `DEALLOCATE`, `DECLARE`, `DELETE`, `DISCARD`, `DO`, `DROP`, `END`,
`EXECUTE`, `EXPLAIN`, `FETCH`, `GRANT`, `IMPORT`, `INSERT`, `LISTEN`, `LOAD`, `LOCK`, `MERGE`,
`MOVE`, `NOTIFY`, `PREPARE`, `REASSIGN`, `REFRESH`, `REINDEX`, `RELEASE`, `RESET`, `REVOKE`,
`ROLLBACK`, `SAVEPOINT`, `SECURITY`, `SELECT`, `SET`, `SHOW`, `START`, `TABLE`, `TRUNCATE`,
`UNLISTEN`, `UPDATE`, `VACUUM`, `VALUES`, `WITH`.

The empty-statement check is the weakest member of the set and the first thing to cut if it proves
noisy in use.

A warning whose range the caret is inside or at the end of is not shown. A warning is a guess about
a word, and a word being typed is not wrong yet — `SELEC` is on its way to `SELECT`, and without
this rule the strip appeared and cleared on every keystroke of every statement. Strictly after the
range's start, so a caret placed in front of a misspelt word still sees it. Errors are certain and
are shown wherever the caret is: an open quote is worth knowing about while you are inside it.
The parser does not know where the caret is, so this is the view's rule; `diagnose` still reports
the document as it stands.

## Layering

`diagnose` lives in `src/sql.rs`, the single PostgreSQL-aware tokenising layer (§59.3). The view
classifies nothing itself, exactly as it classifies no syntax for highlighting.

Computation happens per render beside `highlight_lines`, which already scans the whole document
every frame. Nothing is cached, so no diagnostic can go stale against the text it describes. This
is a second full scan per frame rather than one — the same complexity class, and worth measuring
before it is worth optimising.

Rendering has two parts:

- **The underline.** `Highlight` is not extended. A colour says what a token *is*; an underline
  says something is *wrong*, and merging the two would make every future decoration a new colour
  variant. `highlight_line` instead takes an optional byte range for the line and subdivides its
  spans at that boundary. A bottom border on the affected span is certain to work in GPUI; a wavy
  underline is preferable if available, and the border is the fallback.
- **The message strip**, under the editor, showing severity, message and `line:column`. The
  position is derived through the existing `DocumentPosition` helper rather than counted afresh.

## Testing

`diagnose` is pure and needs neither a display nor a database, so detection is tested entirely in
`src/sql.rs`:

- each lexical class, asserting the exact range and not merely that something was reported
- the parenthesis stack pointing at the innermost outstanding opener, and at an unmatched closer
- a sweep of the allowlist asserting **no** warning, which is the test that guards against false
  positives on valid SQL
- unrecognised leading words asserting a warning
- a clean document yielding `None`
- a lexical error suppressing the statement tier, per decision 6

The strip and the underline are covered by `cx.simulate_keystrokes` tests as the existing UI tests
are: typing an unterminated quote raises the strip, completing the quote clears it.

## Implementation order

1. Positions in the scanner, including the parenthesis stack. No public signature change.
2. `Diagnostic`, `Severity` and `diagnose`, both tiers.
3. The UI surface: underline and message strip.

Roughly three commits, each independently testable.
