# SQL Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The editor underlines the earliest syntax problem in the document and names it in a strip beneath the editor, without ever preventing execution (FR3-009).

**Architecture:** The scanner in `src/sql.rs` already finds every lexical problem and throws the position away; it keeps it, and grows a stack of open parentheses so an unbalanced document points at the offending bracket. A new pure function `diagnose(sql) -> Option<Diagnostic>` turns that into one positioned finding, falling through to a statement-level guess (unrecognised first word, empty statement) only when the scan is clean. `src/ui.rs` computes the diagnostic once per frame beside the highlighting, subdivides the highlight spans at its edges to draw a wavy underline, and renders a message strip between the editor and the results pane. No command consults it.

**Tech Stack:** Rust edition 2024, GPUI 0.2.2 (`Styled::underline`, `text_decoration_wavy`, `text_decoration_color`), `thiserror`.

**Spec:** [`docs/superpowers/specs/2026-09-09-sql-diagnostics-design.md`](../specs/2026-09-09-sql-diagnostics-design.md) — read it first. The numbered decisions there are cited below as "design decision N".

## Global Constraints

- **British spelling** in code, comments and documentation: `colour`, `analyse`, `recognised`, `artefact`. PostgreSQL keywords keep their own spelling (`ANALYZE`).
- **Author is Paul Snow; version 0.0.0.**
- **Cite the requirement** in doc comments: `FR3-009` (Diagnostics), `59.3` (single tokenising layer).
- **`SqlError` is untouched** (design decision 1): no new variants, no span field. `src/application.rs` compares it by value.
- **No public signature in `src/sql.rs` changes** except the additions `Severity`, `Diagnostic`, `diagnose`. `tokenize`, `split_statements`, `highlight_lines` keep their signatures.
- **Diagnostics never gate execution** (design decision 7). Run, Run All, Explain and Explain Analyze are not modified and do not call `diagnose`.
- **The view classifies nothing** (59.3). `src/ui.rs` may cut ranges at line boundaries and pick colours; it never inspects SQL text to decide what is wrong.
- **Nothing is cached** (spec, Layering): the diagnostic is computed from the document on every render, so it can never be stale.
- **TDD**: write the failing test, run it and see it fail, implement minimally, run it and see it pass, commit.
- **After every task**: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test` must all be clean before committing.
- **Commit messages**: imperative summary line, a body explaining why, no co-author or tool trailers (see recent `git log` for the house style).
- All commands run from the repository root `/home/paul/gitHUB/rusty-sql-tool`.

## Out of scope

- Formatting (FR3-007, FR3-008) — the other half of Milestone 4, its own piece later.
- Unknown schema/table/column diagnostics (PRD §10, second bullet) — needs loaded metadata; not in this spec.
- Any caching, incremental scanning or performance work — measure first (spec, Layering).

## File Structure

| File | Responsibility |
|---|---|
| `src/sql.rs` | `ScanError` (private: which `SqlError`, and where). `scan` keeps positions and a parenthesis stack. `statement_ranges` (private, split out of `split_statements` so the statement tier does not re-scan). `Severity`, `Diagnostic`, `diagnose`, `STATEMENT_KEYWORDS`. All detection tests. |
| `src/ui.rs` | `Underline` (one line's slice of the diagnostic), `line_slice`, `split_at_underline`, `severity_colour`; `highlight_line` gains an underline parameter; `editor_surface` takes the diagnostic; `AppView::editor_diagnostic`, `AppView::diagnostic_strip`; `render` computes the diagnostic once. Keystroke tests. |
| `CLAUDE.md` | Repository-state paragraph and module map mention diagnostics. |

The spec's three commits become five tasks here: the scanner (1), the lexical tier (2), the statement tier (3), the underline (4) and the strip (5). Each is independently testable and each is one commit.

## Interfaces at a glance

```rust
// src/sql.rs — new public surface
pub enum Severity { Error, Warning }
pub struct Diagnostic { pub range: Range<usize>, pub severity: Severity, pub message: String }
pub fn diagnose(sql: &str) -> Option<Diagnostic>

// src/sql.rs — private, referenced across tasks
struct ScanError { error: SqlError, range: Range<usize> }
fn scan(sql: &str) -> (Vec<Token>, Option<ScanError>)
fn statement_ranges(sql: &str, tokens: &[Token]) -> Vec<Range<usize>>
const STATEMENT_KEYWORDS: &[&str]

// src/ui.rs — private
struct Underline { line: usize, range: Range<usize>, severity: Severity }
fn line_slice(document: &str, range: &Range<usize>, line: usize) -> Option<Range<usize>>
fn split_at_underline(spans: &[HighlightSpan], underline: &Range<usize>) -> Vec<(HighlightSpan, bool)>
fn severity_colour(severity: Severity) -> u32
fn highlight_line(line: &str, spans: &[HighlightSpan], underline: Option<&Underline>) -> impl IntoElement
impl AppView {
    fn editor_diagnostic(&self) -> Option<Diagnostic>
    fn editor_surface(&self, diagnostic: Option<&Diagnostic>, cx: &mut Context<Self>) -> gpui::AnyElement
    fn diagnostic_strip(&self, diagnostic: &Diagnostic) -> impl IntoElement
}
```

---

### Task 1: The scanner keeps the position it already knows

Design decisions 3 and 4. `scan()` currently holds `Option<SqlError>` and sets it with `error.or(...)` so the first problem wins. It widens to `Option<ScanError>` carrying the byte range, and the `depth` counter becomes a stack of open-parenthesis ranges so an unbalanced document can point at the bracket. `tokenize` discards the range, so nothing outside `scan` changes.

**Files:**
- Modify: `src/sql.rs` — `tokenize` (≈ line 595), `scan` (≈ lines 601–744), tests at the bottom of `mod tests`.

**Interfaces:**
- Consumes: `SqlError`, `Token`, `TokenKind` as they are.
- Produces: `struct ScanError { error: SqlError, range: Range<usize> }` and `fn scan(sql: &str) -> (Vec<Token>, Option<ScanError>)`. Task 2 reads both.

- [ ] **Step 1: Write the failing tests**

Add to the end of `mod tests` in `src/sql.rs` (before the closing `}` of the module):

```rust
    fn scan_error(sql: &str) -> Option<(SqlError, Range<usize>)> {
        scan(sql).1.map(|found| (found.error, found.range))
    }

    /// Design decision 3: the scanner reports where each lexical problem is, opener to end of
    /// input, so a diagnostic has something to point at.
    #[test]
    fn lexical_errors_carry_the_range_of_the_offending_token() {
        assert_eq!(
            scan_error("SELECT 'abc"),
            Some((SqlError::UnterminatedQuote, 7..11))
        );
        assert_eq!(
            scan_error("SELECT \"abc"),
            Some((SqlError::UnterminatedQuote, 7..11))
        );
        assert_eq!(
            scan_error("SELECT $$abc"),
            Some((SqlError::UnterminatedQuote, 7..12))
        );
        assert_eq!(
            scan_error("SELECT $fn$abc"),
            Some((SqlError::UnterminatedQuote, 7..14))
        );
        assert_eq!(
            scan_error("SELECT 1 /* note"),
            Some((SqlError::UnterminatedComment, 9..16))
        );
    }

    /// Design decision 4: a stray `)` is its own position; an unclosed `(` is the innermost one
    /// still outstanding — not merely the last one opened.
    #[test]
    fn unbalanced_parentheses_point_at_the_unmatched_bracket() {
        assert_eq!(
            scan_error("SELECT 1)"),
            Some((SqlError::UnbalancedParentheses, 8..9))
        );
        assert_eq!(
            scan_error("SELECT (1, (2"),
            Some((SqlError::UnbalancedParentheses, 11..12))
        );
        assert_eq!(
            scan_error("SELECT ((1)"),
            Some((SqlError::UnbalancedParentheses, 7..8))
        );
        assert_eq!(scan_error("SELECT (1)"), None);
    }

    /// The first problem wins, in the order the scanner meets them: a quote left open inside a
    /// bracket is the quote's fault, because it is what stopped the bracket from closing.
    #[test]
    fn the_earliest_lexical_error_is_the_one_reported() {
        assert_eq!(
            scan_error("SELECT ) 'x"),
            Some((SqlError::UnbalancedParentheses, 7..8))
        );
        assert_eq!(
            scan_error("SELECT (1, 'x"),
            Some((SqlError::UnterminatedQuote, 11..13))
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib sql::tests::lexical_errors_carry_the_range_of_the_offending_token`
Expected: compile error — `found.error`/`found.range` do not exist on `SqlError`.

- [ ] **Step 3: Widen the scanner**

Replace `tokenize` and `scan` in `src/sql.rs` (everything from `fn tokenize` down to, but not including, `fn dollar_delimiter_end`) with:

```rust
fn tokenize(sql: &str) -> Result<Vec<Token>, SqlError> {
    match scan(sql) {
        (_, Some(ScanError { error, .. })) => Err(error),
        (tokens, None) => Ok(tokens),
    }
}

/// What `scan` found wrong, and where. `tokenize` keeps only the error; `diagnose` keeps both,
/// because a diagnostic with nothing to point at is not worth rendering (FR3-009).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ScanError {
    error: SqlError,
    range: Range<usize>,
}

/// Records a problem unless an earlier one is already held: the first is the real one, and
/// anything after it is likely an artefact of it.
fn report(slot: &mut Option<ScanError>, error: SqlError, range: Range<usize>) {
    if slot.is_none() {
        *slot = Some(ScanError { error, range });
    }
}

/// Tokenises as far as the text allows, reporting the first thing wrong with it rather than
/// stopping. Statement splitting refuses a document it cannot read; highlighting has to colour one
/// that is halfway through being typed, and both need the same reading of the SQL (59.3).
fn scan(sql: &str) -> (Vec<Token>, Option<ScanError>) {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut error = None;
    let mut index = 0;
    // Every `(` still waiting for its `)`, innermost last. Its length is the nesting depth, and
    // whichever is left over at the end is the bracket to point at.
    let mut open: Vec<Range<usize>> = Vec::new();
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let start = index;
        if bytes[index] == b'-' && bytes.get(index + 1) == Some(&b'-') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                range: start..index,
                depth: open.len(),
            });
            continue;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            let mut nesting = 1usize;
            while index < bytes.len() && nesting > 0 {
                if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    nesting += 1;
                    index += 2;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    nesting -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            if nesting != 0 {
                report(&mut error, SqlError::UnterminatedComment, start..index);
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                range: start..index,
                depth: open.len(),
            });
            continue;
        }
        if matches!(bytes[index], b'\'' | b'"') {
            let quote = bytes[index];
            index += 1;
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == quote {
                    if bytes.get(index + 1) == Some(&quote) {
                        index += 2;
                    } else {
                        index += 1;
                        closed = true;
                        break;
                    }
                } else if quote == b'\'' && bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else {
                    index += 1;
                }
            }
            if !closed {
                report(&mut error, SqlError::UnterminatedQuote, start..index);
            }
            tokens.push(Token {
                kind: TokenKind::Literal,
                range: start..index,
                depth: open.len(),
            });
            continue;
        }
        if bytes[index] == b'$'
            && let Some(delimiter_end) = dollar_delimiter_end(bytes, index)
        {
            let delimiter = &bytes[index..delimiter_end];
            index = delimiter_end;
            match bytes[index..]
                .windows(delimiter.len())
                .position(|window| window == delimiter)
            {
                Some(relative_end) => index += relative_end + delimiter.len(),
                None => {
                    index = bytes.len();
                    report(&mut error, SqlError::UnterminatedQuote, start..index);
                }
            }
            tokens.push(Token {
                kind: TokenKind::Literal,
                range: start..index,
                depth: open.len(),
            });
            continue;
        }
        if is_word_start(bytes[index]) {
            index += 1;
            while index < bytes.len() && is_word_continue(bytes[index]) {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Word(sql[start..index].to_ascii_uppercase()),
                range: start..index,
                depth: open.len(),
            });
            continue;
        }
        let character = sql[index..].chars().next().expect("valid UTF-8");
        index += character.len_utf8();
        // A `)` is popped before its token is recorded, so it carries the depth outside it — the
        // same depth its `(` carried.
        if character == ')' && open.pop().is_none() {
            report(&mut error, SqlError::UnbalancedParentheses, start..index);
        }
        tokens.push(Token {
            kind: TokenKind::Symbol(character),
            range: start..index,
            depth: open.len(),
        });
        if character == '(' {
            open.push(start..index);
        }
    }
    if let Some(innermost) = open.last() {
        report(
            &mut error,
            SqlError::UnbalancedParentheses,
            innermost.clone(),
        );
    }
    (tokens, error)
}
```

`document_spans` already destructures `let (tokens, _) = scan(sql);` and needs no change.

- [ ] **Step 4: Run the whole sql module's tests**

Run: `cargo test --lib sql::`
Expected: all pass — the three new tests, and every existing splitting, limit and highlighting test (they prove the token stream and the "first error wins" ordering are unchanged).

- [ ] **Step 5: Lint and format**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/sql.rs
git commit -m "Keep the position of each lexical error in the scanner

scan() found every unterminated quote, comment and unbalanced bracket
and then threw the location away. It now holds the byte range beside
the error, and tracks open parentheses as a stack so an unbalanced
document points at the bracket at fault rather than at nothing.
tokenize() still discards the range, so no caller changes."
```

---

### Task 2: `Diagnostic`, `Severity` and the lexical tier of `diagnose`

Design decisions 1, 2, 5. A `Diagnostic` is a byte range, a severity and a message. `diagnose` maps the scanner's `ScanError` to an `Error`-severity diagnostic with a message specific enough to act on. The statement tier arrives in Task 3.

**Files:**
- Modify: `src/sql.rs` — new types and `diagnose` placed directly after `prepare_explain` and before the `Highlight` enum (≈ line 445); tests appended to `mod tests`.

**Interfaces:**
- Consumes: `ScanError`, `scan` from Task 1.
- Produces: `pub enum Severity`, `pub struct Diagnostic`, `pub fn diagnose(sql: &str) -> Option<Diagnostic>`. Tasks 3, 4 and 5 use all three.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/sql.rs`:

```rust
    fn error_at(range: Range<usize>, message: &str) -> Option<Diagnostic> {
        Some(Diagnostic {
            range,
            severity: Severity::Error,
            message: message.to_owned(),
        })
    }

    /// FR3-009, design decision 5: lexical problems are certain, so they are errors, and each
    /// names what is open rather than repeating the generic `SqlError` text.
    #[test]
    fn each_lexical_problem_is_an_error_with_its_own_message_and_range() {
        assert_eq!(
            diagnose("SELECT 'abc"),
            error_at(7..11, "unterminated quoted string or identifier")
        );
        assert_eq!(
            diagnose("SELECT \"abc"),
            error_at(7..11, "unterminated quoted string or identifier")
        );
        assert_eq!(
            diagnose("SELECT $body$abc"),
            error_at(7..16, "unterminated dollar-quoted string")
        );
        assert_eq!(
            diagnose("SELECT 1 /* note"),
            error_at(9..16, "unterminated block comment")
        );
        assert_eq!(
            diagnose("SELECT (1, (2"),
            error_at(11..12, "unclosed parenthesis")
        );
        assert_eq!(
            diagnose("SELECT 1)"),
            error_at(8..9, "unmatched closing parenthesis")
        );
    }

    /// A clean document reports nothing, and so does an empty one.
    #[test]
    fn a_clean_document_has_no_diagnostic() {
        assert_eq!(diagnose("SELECT (1, 'a''b') /* ok */ -- fine"), None);
        assert_eq!(diagnose(""), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib sql::tests::each_lexical_problem_is_an_error_with_its_own_message_and_range`
Expected: compile error — `Diagnostic`, `Severity` and `diagnose` are not defined.

- [ ] **Step 3: Add the types and the lexical tier**

Insert after `prepare_explain` (just before `/// What the editor should paint a stretch of SQL as (FR-012).`):

```rust
/// How sure the editor is about what it points at (FR3-009).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The text cannot be read: every command would refuse it.
    Error,
    /// A statement-level guess — "check this", not "this is broken".
    Warning,
}

/// One problem for the editor to point at. `range` is a byte range into the document handed to
/// [`diagnose`], the same addressing [`HighlightSpan`] uses.
///
/// Deliberately not a [`SqlError`]: that answers "why did this command fail" and is compared by
/// value in the command layer, and some of its variants have no position at all. This answers
/// "what should the editor point at" (FR3-009).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

/// The earliest problem in a whole document, or `None` when there is nothing to report. One at
/// most: an unterminated quote turns everything after it into a string, so a second finding would
/// be an artefact of the first rather than a second mistake.
///
/// Advisory only. Run, Run All and Explain fail through [`SqlError`] on their own and never
/// consult this (FR3-009).
pub fn diagnose(sql: &str) -> Option<Diagnostic> {
    let (_, error) = scan(sql);
    let ScanError { error, range } = error?;
    let text = &sql[range.clone()];
    let message = match error {
        SqlError::UnterminatedQuote if text.starts_with('$') => {
            "unterminated dollar-quoted string".to_owned()
        }
        SqlError::UnterminatedQuote => "unterminated quoted string or identifier".to_owned(),
        SqlError::UnterminatedComment => "unterminated block comment".to_owned(),
        SqlError::UnbalancedParentheses if text == "(" => "unclosed parenthesis".to_owned(),
        SqlError::UnbalancedParentheses => "unmatched closing parenthesis".to_owned(),
        // `scan` reports nothing else; the generic text keeps this honest if that ever changes.
        other => other.to_string(),
    };
    Some(Diagnostic {
        range,
        severity: Severity::Error,
        message,
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib sql::`
Expected: all pass.

- [ ] **Step 5: Lint and format**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/sql.rs
git commit -m "Add diagnose() for the editor's lexical problems

A Diagnostic is a byte range, a severity and a message — what the
editor needs to point at something. It is kept apart from SqlError,
which says why a command failed and is compared by value. This first
tier covers what the scanner is certain of; a statement-level tier
follows."
```

---

### Task 3: The statement tier

Design decisions 5 and 6. When the scan is clean, each statement's first word is checked against PostgreSQL's real statement set, and a statement that is nothing but a `;` is reported as empty. Only the first offending statement is reported, as a `Warning`. `split_statements` is split into a tokenising wrapper and `statement_ranges`, so the tier reuses the tokens `diagnose` already has instead of scanning a third time.

**Files:**
- Modify: `src/sql.rs` — `split_statements` (≈ line 71), `diagnose` from Task 2, a new `STATEMENT_KEYWORDS` const beside `KEYWORDS`, a new `statement_warning` fn; tests appended to `mod tests`.

**Interfaces:**
- Consumes: `Diagnostic`, `Severity`, `diagnose` from Task 2; `Token`, `TokenKind`, `trimmed_range` as they are.
- Produces: `fn statement_ranges(sql: &str, tokens: &[Token]) -> Vec<Range<usize>>` (private), `const STATEMENT_KEYWORDS: &[&str]` (private; the allowlist test iterates it). `diagnose` now returns `Warning` diagnostics too.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/sql.rs`:

```rust
    fn warning_at(range: Range<usize>, message: &str) -> Option<Diagnostic> {
        Some(Diagnostic {
            range,
            severity: Severity::Warning,
            message: message.to_owned(),
        })
    }

    /// The guard against false positives: every statement PostgreSQL accepts opens a statement
    /// here without comment, in either case.
    #[test]
    fn every_statement_keyword_opens_a_statement_without_a_warning() {
        for keyword in STATEMENT_KEYWORDS {
            assert_eq!(diagnose(&format!("{keyword} something;")), None, "{keyword}");
            assert_eq!(
                diagnose(&format!("{} something;", keyword.to_ascii_lowercase())),
                None,
                "{keyword}"
            );
        }
    }

    /// FR3-009, design decision 5: a first word that is not a statement is a guess, so a warning,
    /// pointing at the word in the user's own spelling.
    #[test]
    fn an_unrecognised_first_word_is_a_warning_on_that_word() {
        assert_eq!(
            diagnose("selct 1"),
            warning_at(0..5, "`selct` does not begin a PostgreSQL statement")
        );
        assert_eq!(
            diagnose("SELECT 1; SELCT 2"),
            warning_at(10..15, "`SELCT` does not begin a PostgreSQL statement")
        );
    }

    /// Only the first offending statement is reported.
    #[test]
    fn only_the_first_offending_statement_is_reported() {
        assert_eq!(
            diagnose("SELCT 1; SELCT 2"),
            warning_at(0..5, "`SELCT` does not begin a PostgreSQL statement")
        );
    }

    /// A statement that is nothing but its semicolon is empty; a trailing comment with no
    /// semicolon is not a statement at all and is left alone.
    #[test]
    fn an_empty_statement_is_a_warning_but_a_trailing_comment_is_not() {
        assert_eq!(diagnose("SELECT 1;;"), warning_at(9..10, "empty statement"));
        assert_eq!(
            diagnose("SELECT 1; /* gap */ ;"),
            warning_at(10..21, "empty statement")
        );
        assert_eq!(diagnose("SELECT 1; -- done"), None);
        assert_eq!(diagnose("-- note\nSELECT 1"), None);
    }

    /// A statement opening with something other than a word is beyond a guess and is not judged.
    #[test]
    fn a_statement_that_does_not_open_with_a_word_is_not_judged() {
        assert_eq!(diagnose("(SELECT 1) UNION (SELECT 2)"), None);
    }

    /// Design decision 6: the statement tier runs only on a clean scan, so a lexical error is
    /// reported as itself and never as a misread statement.
    #[test]
    fn a_lexical_error_suppresses_the_statement_tier() {
        assert_eq!(
            diagnose("SELCT 'x"),
            error_at(6..8, "unterminated quoted string or identifier")
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib sql::tests::an_unrecognised_first_word_is_a_warning_on_that_word`
Expected: compile error — `STATEMENT_KEYWORDS` is not defined (and, once it is, the assertion fails with `None`).

- [ ] **Step 3: Split `split_statements`**

Replace `split_statements` (the function under `/// Splits a document on top-level semicolons only (FR-029, 59.3).`) with:

```rust
/// Splits a document on top-level semicolons only (FR-029, 59.3).
pub fn split_statements(sql: &str) -> Result<Vec<Range<usize>>, SqlError> {
    Ok(statement_ranges(sql, &tokenize(sql)?))
}

/// The statements a token stream delimits, as trimmed byte ranges; each includes its own `;`.
/// Shared with [`diagnose`], which already holds the tokens and must not scan again.
fn statement_ranges(sql: &str, tokens: &[Token]) -> Vec<Range<usize>> {
    let mut statements = Vec::new();
    let mut start = 0;
    for token in tokens {
        if token.depth == 0 && token.kind == TokenKind::Symbol(';') {
            if let Some(range) = trimmed_range(sql, start..token.range.end) {
                statements.push(range);
            }
            start = token.range.end;
        }
    }
    if let Some(range) = trimmed_range(sql, start..sql.len()) {
        statements.push(range);
    }
    statements
}
```

- [ ] **Step 4: Add the allowlist and the statement tier**

Change the first two lines of `diagnose` from

```rust
    let (_, error) = scan(sql);
    let ScanError { error, range } = error?;
```

to

```rust
    let (tokens, error) = scan(sql);
    let Some(ScanError { error, range }) = error else {
        return statement_warning(sql, &tokens);
    };
```

Then add, directly after `diagnose`:

```rust
/// The first statement that looks wrong, judged by its first word (design decision 6: reached
/// only on a clean scan). Only a word is judged — a statement opening with `(` or a literal is
/// beyond a guess, and a guess that is often wrong is worse than none (FR3-009).
fn statement_warning(sql: &str, tokens: &[Token]) -> Option<Diagnostic> {
    let mut next = 0;
    for statement in statement_ranges(sql, tokens) {
        // Tokens and statements are both in document order, so each statement picks up its
        // search where the last one left off.
        while tokens
            .get(next)
            .is_some_and(|token| token.range.start < statement.start)
        {
            next += 1;
        }
        let first = tokens[next..]
            .iter()
            .take_while(|token| token.range.end <= statement.end)
            .find(|token| token.kind != TokenKind::Comment);
        match first {
            Some(Token {
                kind: TokenKind::Symbol(';'),
                ..
            }) => {
                return Some(Diagnostic {
                    range: statement,
                    severity: Severity::Warning,
                    message: "empty statement".to_owned(),
                });
            }
            Some(Token {
                kind: TokenKind::Word(word),
                range,
                ..
            }) if !STATEMENT_KEYWORDS.contains(&word.as_str()) => {
                return Some(Diagnostic {
                    range: range.clone(),
                    severity: Severity::Warning,
                    message: format!(
                        "`{}` does not begin a PostgreSQL statement",
                        &sql[range.clone()]
                    ),
                });
            }
            _ => {}
        }
    }
    None
}

/// Every word that can open a PostgreSQL statement. Uppercase because [`TokenKind::Word`] already
/// folds case. Deliberately generous: a name missing here is a false warning on valid SQL, which
/// is the one thing a guess must not do. `ANALYSE` is PostgreSQL's own accepted alternative
/// spelling of `ANALYZE`.
const STATEMENT_KEYWORDS: &[&str] = &[
    "ABORT",
    "ALTER",
    "ANALYSE",
    "ANALYZE",
    "BEGIN",
    "CALL",
    "CHECKPOINT",
    "CLOSE",
    "CLUSTER",
    "COMMENT",
    "COMMIT",
    "COPY",
    "CREATE",
    "DEALLOCATE",
    "DECLARE",
    "DELETE",
    "DISCARD",
    "DO",
    "DROP",
    "END",
    "EXECUTE",
    "EXPLAIN",
    "FETCH",
    "GRANT",
    "IMPORT",
    "INSERT",
    "LISTEN",
    "LOAD",
    "LOCK",
    "MERGE",
    "MOVE",
    "NOTIFY",
    "PREPARE",
    "REASSIGN",
    "REFRESH",
    "REINDEX",
    "RELEASE",
    "RESET",
    "REVOKE",
    "ROLLBACK",
    "SAVEPOINT",
    "SECURITY",
    "SELECT",
    "SET",
    "SHOW",
    "START",
    "TABLE",
    "TRUNCATE",
    "UNLISTEN",
    "UPDATE",
    "VACUUM",
    "VALUES",
    "WITH",
];
```

Why the "empty statement" range is the trimmed statement rather than literally "between the semicolons": `statement_ranges` trims leading whitespace, so for `SELECT 1;;` the range is the stray `;` itself (9..10), and for `SELECT 1; /* gap */ ;` it is the comment and the `;`. Both are non-empty, which an underline needs; the literal gap between two adjacent semicolons is zero bytes wide and could not be drawn.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib sql::`
Expected: all pass, including the existing `splits_only_top_level_semicolons`, `selection_takes_precedence_over_cursor_statement` and `unparseable_sql_yields_no_relations` (which prove `split_statements` is unchanged in behaviour).

- [ ] **Step 6: Lint and format**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/sql.rs
git commit -m "Warn on a statement that does not open with a PostgreSQL keyword

The second tier of diagnose(): once the scan is clean, each statement's
first word is checked against the full set of words PostgreSQL accepts
at the start of a statement, and a statement that is only a semicolon
is called empty. These are guesses, so they carry Warning severity and
only the first is reported. split_statements() gives up its body to
statement_ranges() so the tier reuses the tokens rather than scanning
again."
```

---

### Task 4: The underline

Spec, Rendering. `highlight_line` takes an optional per-line underline and subdivides its spans at that boundary, so a keyword stays keyword-coloured whether or not it is also wrong. Underlined pieces get a wavy underline in the severity's colour. `editor_surface` takes the diagnostic, which `render` computes once per frame.

**Files:**
- Modify: `src/ui.rs` — the `use crate::sql` import (line 22); `editor_surface` (≈ line 2931) and its call in `render` (≈ line 4358); `highlight_line` (≈ line 4605) and its other caller in `definition_surface` (≈ line 2894); new helpers beside `document_position` (≈ line 4958); tests in `mod tests`.

**Interfaces:**
- Consumes: `Diagnostic`, `Severity`, `diagnose`, `HighlightSpan`, `Highlight` from `crate::sql`.
- Produces: `struct Underline { line, range, severity }`, `fn line_slice`, `fn split_at_underline`, `fn severity_colour`, `fn highlight_line(line, spans, underline: Option<&Underline>)`, `AppView::editor_diagnostic(&self) -> Option<Diagnostic>`, `AppView::editor_surface(&self, diagnostic: Option<&Diagnostic>, cx)`. Task 5 uses `severity_colour` and `editor_diagnostic`. Each underlined piece is rendered with the debug selector `diagnostic-underline-{line}-{byte start of the piece within the line}`.

- [ ] **Step 1: Write the failing unit tests for the pure helpers**

Add to `mod tests` in `src/ui.rs`, next to the existing `document_position` tests (search for `DocumentPosition { line: 1, column: 2 }`):

```rust
    /// The bytes of one line a document range covers; a range over several lines is cut at each
    /// newline, and a line it does not touch gets nothing.
    #[test]
    fn line_slice_cuts_a_document_range_at_line_boundaries() {
        let document = "SELECT 1\n/* open\ncomment";
        let range = 9..24;
        assert_eq!(line_slice(document, &range, 0), None);
        assert_eq!(line_slice(document, &range, 1), Some(0..7));
        assert_eq!(line_slice(document, &range, 2), Some(0..7));
        assert_eq!(line_slice(document, &range, 3), None);
        assert_eq!(line_slice("SELECT 'abc", &(7..11), 0), Some(7..11));
    }

    /// Spans are cut at the underline's edges so each piece is wholly under it or wholly clear of
    /// it, and every piece keeps the colour of the span it came from.
    #[test]
    fn split_at_underline_cuts_spans_at_the_underline_edges() {
        let spans = [
            HighlightSpan {
                range: 0..6,
                highlight: Highlight::Keyword,
            },
            HighlightSpan {
                range: 6..12,
                highlight: Highlight::Plain,
            },
        ];
        let piece = |range: Range<usize>, highlight, underlined| {
            (HighlightSpan { range, highlight }, underlined)
        };

        assert_eq!(
            split_at_underline(&spans, &(8..10)),
            vec![
                piece(0..6, Highlight::Keyword, false),
                piece(6..8, Highlight::Plain, false),
                piece(8..10, Highlight::Plain, true),
                piece(10..12, Highlight::Plain, false),
            ]
        );
        assert_eq!(
            split_at_underline(&spans, &(3..9)),
            vec![
                piece(0..3, Highlight::Keyword, false),
                piece(3..6, Highlight::Keyword, true),
                piece(6..9, Highlight::Plain, true),
                piece(9..12, Highlight::Plain, false),
            ]
        );
        assert_eq!(
            split_at_underline(&spans, &(0..12)),
            vec![
                piece(0..6, Highlight::Keyword, true),
                piece(6..12, Highlight::Plain, true),
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ui::tests::line_slice_cuts_a_document_range_at_line_boundaries`
Expected: compile error — `line_slice` and `split_at_underline` are not defined.

- [ ] **Step 3: Add the helpers**

Change the import on line 22 of `src/ui.rs` to:

```rust
use crate::sql::{Diagnostic, Highlight, HighlightSpan, Severity, diagnose, highlight_lines};
```

Insert directly after `fn document_position` (before `/// The columns of `line` a byte-offset selection covers`):

```rust
/// The stretch of one editor line a diagnostic covers, and how loudly to paint it.
struct Underline {
    line: usize,
    range: Range<usize>,
    severity: Severity,
}

/// The bytes of `line` a document byte range covers, or `None` where it touches none of them.
/// Bytes rather than columns because it addresses a [`HighlightSpan`], which is in bytes too.
fn line_slice(document: &str, range: &Range<usize>, line: usize) -> Option<Range<usize>> {
    let line_start = document
        .split_inclusive('\n')
        .take(line)
        .map(str::len)
        .sum::<usize>();
    let line_end = document[line_start..]
        .find('\n')
        .map_or(document.len(), |offset| line_start + offset);
    let start = range.start.max(line_start);
    let end = range.end.min(line_end);
    (start < end).then_some(start - line_start..end - line_start)
}

/// Cuts a line's spans at the edges of `underline`, so every piece is either wholly under it or
/// wholly clear of it. Colour and decoration stay independent — a keyword is still a keyword when
/// it is also wrong — which is why [`Highlight`] gains no variant for this.
fn split_at_underline(
    spans: &[HighlightSpan],
    underline: &Range<usize>,
) -> Vec<(HighlightSpan, bool)> {
    let mut pieces = Vec::new();
    for span in spans {
        let mut cuts = vec![span.range.start];
        cuts.extend(
            [underline.start, underline.end]
                .into_iter()
                .filter(|&edge| span.range.start < edge && edge < span.range.end),
        );
        cuts.push(span.range.end);
        for pair in cuts.windows(2) {
            let range = pair[0]..pair[1];
            let underlined = underline.start <= range.start && range.end <= underline.end;
            pieces.push((
                HighlightSpan {
                    range,
                    highlight: span.highlight,
                },
                underlined,
            ));
        }
    }
    pieces
}

fn severity_colour(severity: Severity) -> u32 {
    match severity {
        Severity::Error => RED,
        Severity::Warning => WARN,
    }
}
```

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test --lib ui::tests::line_slice_cuts_a_document_range_at_line_boundaries ui::tests::split_at_underline_cuts_spans_at_the_underline_edges`
Expected: both PASS. (`Underline` and `severity_colour` are unused until Step 6; `cargo test` builds with dead-code warnings but clippy would not — do not run clippy until Step 6 is done.)

- [ ] **Step 5: Write the failing keystroke test**

Add to `mod tests` in `src/ui.rs`, after `native_keyboard_input_edits_the_focused_document`:

```rust
    /// FR3-009: the offending text is underlined where it sits, and the underline goes when the
    /// text is fixed.
    #[gpui::test]
    fn an_unterminated_quote_is_underlined_until_it_is_closed(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        cx.simulate_input("SELECT 'abc");
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let diagnostic = app
                .editor_diagnostic()
                .expect("an open quote should be reported");
            assert_eq!(diagnostic.range, 7..11);
            assert_eq!(diagnostic.severity, Severity::Error);
        });
        assert!(
            cx.debug_bounds("diagnostic-underline-0-7").is_some(),
            "the literal on line 0 should be underlined from byte 7"
        );

        cx.simulate_input("'");
        cx.run_until_parked();
        // gpui's test harness never evicts a painted selector from its debug-bounds map (see the
        // plan-tree tests), so the cleared underline is asserted on the state the render reads.
        view.update(cx, |app, _| assert_eq!(app.editor_diagnostic(), None));
    }
```

Run: `cargo test --lib ui::tests::an_unterminated_quote_is_underlined_until_it_is_closed`
Expected: compile error — `editor_diagnostic` is not defined.

- [ ] **Step 6: Thread the diagnostic through `render`, `editor_surface` and `highlight_line`**

(a) Add to `impl AppView`, directly above `fn editor_surface`:

```rust
    /// The one thing wrong with the front editor's document, computed afresh each frame beside
    /// the highlighting so it can never be stale against the text it describes (FR3-009).
    fn editor_diagnostic(&self) -> Option<Diagnostic> {
        diagnose(&self.editor.document)
    }
```

(b) Change the signature of `editor_surface` to:

```rust
    fn editor_surface(
        &self,
        diagnostic: Option<&Diagnostic>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
```

Inside its per-line loop, directly after the `let span = self.editor.selection ... selected_columns(...)` statement, add:

```rust
            let underline = diagnostic.and_then(|diagnostic| {
                line_slice(document, &diagnostic.range, index).map(|range| Underline {
                    line: index,
                    range,
                    severity: diagnostic.severity,
                })
            });
```

and change that loop's `highlight_line` call to:

```rust
                    .child(highlight_line(
                        line,
                        highlights.get(index).map_or(&[][..], Vec::as_slice),
                        underline.as_ref(),
                    ))
```

(The diagnostic is computed from `document`, never from the placeholder `displayed`; an empty document has no diagnostic, so the two never disagree.)

(c) In `definition_surface`, change its `highlight_line` call (under `DefinitionSection::Sql`) to pass `None` as the third argument:

```rust
                                        .child(highlight_line(
                                            line,
                                            spans.get(index).map_or(&[][..], Vec::as_slice),
                                            None,
                                        )),
```

(d) In `render`, after `let pane_visible = ...;` add:

```rust
        // Only the editor tab paints diagnostics, and only one scan per frame is paid for: the
        // underline and the strip both read this.
        let diagnostic = (self.focus == Focus::Editor)
            .then(|| self.editor_diagnostic())
            .flatten();
```

and change the `Focus::Editor` arm's call to `self.editor_surface(diagnostic.as_ref(), cx)`.

(e) Replace `highlight_line` with:

```rust
/// Paints one line from the spans [`highlight_lines`] worked out for it, with the diagnostic's
/// stretch of the line underlined. The view classifies nothing itself: reading SQL is the
/// parser's job, and a second reading here would disagree with it (59.3).
fn highlight_line(
    line: &str,
    spans: &[HighlightSpan],
    underline: Option<&Underline>,
) -> impl IntoElement {
    let mut row = div().flex().flex_row().whitespace_nowrap();
    let pieces = match underline {
        Some(underline) => split_at_underline(spans, &underline.range),
        None => spans.iter().map(|span| (span.clone(), false)).collect(),
    };
    for (span, underlined) in pieces {
        let colour = match span.highlight {
            Highlight::Keyword => ACCENT,
            Highlight::Literal => STRING,
            Highlight::Comment => FAINT,
            Highlight::Function => FUNCTION,
            Highlight::Plain => TEXT,
        };
        let piece = div()
            .text_color(rgb(colour))
            .child(line[span.range.clone()].to_owned());
        row = row.child(match (underlined, underline) {
            (true, Some(underline)) => {
                // Selectable in tests by line and start byte, the way object rows and tabs are.
                let selector = SharedString::from(format!(
                    "diagnostic-underline-{}-{}",
                    underline.line, span.range.start
                ));
                let id = selector.clone();
                piece
                    .id(id)
                    .debug_selector(move || selector.to_string())
                    .underline()
                    .text_decoration_wavy()
                    .text_decoration_color(rgb(severity_colour(underline.severity)))
                    .into_any_element()
            }
            _ => piece.into_any_element(),
        });
    }
    row
}
```

If the wavy underline does not appear when the app is run (`cargo run`, type `SELECT 'abc`), replace the three `.underline().text_decoration_wavy().text_decoration_color(...)` calls with `.border_b_1().border_color(rgb(severity_colour(underline.severity)))` — the spec's fallback. Do not spend more than a few minutes on it either way.

- [ ] **Step 7: Run the UI tests to verify they pass**

Run: `cargo test --lib ui::`
Expected: all pass, including `an_unterminated_quote_is_underlined_until_it_is_closed` and every existing highlighting and editing test.

- [ ] **Step 8: Lint and format**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add src/ui.rs
git commit -m "Underline the diagnostic in the editor

The diagnostic is computed once per frame in render() and handed to
the editor surface, which cuts each visible line's highlight spans at
the diagnostic's edges and draws a wavy underline under the pieces
inside it. Colour and decoration stay independent, so Highlight gains
no variant. The definition tab passes no underline and is unchanged."
```

---

### Task 5: The message strip

Spec, Rendering. A strip between the editor and the results pane shows the severity, `line:column` and message. The position comes from the existing `DocumentPosition` helper. The strip is visible only on the editor tab and only while there is a diagnostic. Finally `CLAUDE.md` records that this half of Milestone 4 is implemented.

**Files:**
- Modify: `src/ui.rs` — new `diagnostic_strip` in `impl AppView` beside `editor_diagnostic`; one `.children(...)` in `render` between the editor container and the results splitter; a test in `mod tests`.
- Modify: `CLAUDE.md` — repository-state paragraph and the `src/sql.rs` row of the module map.

**Interfaces:**
- Consumes: `editor_diagnostic`, `severity_colour`, `document_position`, the `diagnostic` local in `render` from Task 4.
- Produces: `AppView::diagnostic_strip(&self, diagnostic: &Diagnostic) -> impl IntoElement`, rendered with debug selector `diagnostic-strip`.

- [ ] **Step 1: Write the failing keystroke test**

Add to `mod tests` in `src/ui.rs`, after `an_unterminated_quote_is_underlined_until_it_is_closed`:

```rust
    /// FR3-009: the strip names the problem and where it is, and clears when the text is fixed.
    #[gpui::test]
    fn the_strip_reports_the_problem_and_clears_when_it_is_fixed(cx: &mut TestAppContext) {
        let (view, cx) = build_app_view(cx);

        cx.simulate_input("SELECT 1;");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("SELCT 2");
        cx.run_until_parked();

        view.update(cx, |app, _| {
            let diagnostic = app
                .editor_diagnostic()
                .expect("a misspelt keyword should be reported");
            assert_eq!(diagnostic.severity, Severity::Warning);
            assert_eq!(
                document_position(&app.editor.document, diagnostic.range.start),
                DocumentPosition { line: 1, column: 0 }
            );
        });
        assert!(
            cx.debug_bounds("diagnostic-strip").is_some(),
            "the strip should be rendered while there is something to report"
        );

        // Four to the left puts the caret after `SEL`; the `E` mends the keyword.
        cx.simulate_keystrokes("left left left left");
        cx.simulate_input("E");
        cx.run_until_parked();
        view.update(cx, |app, _| {
            assert_eq!(app.editor.document, "SELECT 1;\nSELECT 2");
            // The harness never evicts a painted selector, so the cleared strip is asserted on
            // the state the render reads rather than on `debug_bounds`.
            assert_eq!(app.editor_diagnostic(), None);
        });
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib ui::tests::the_strip_reports_the_problem_and_clears_when_it_is_fixed`
Expected: FAIL at the `debug_bounds("diagnostic-strip")` assertion — nothing renders that selector yet.

- [ ] **Step 3: Add the strip**

Add to `impl AppView`, directly after `fn editor_diagnostic`:

```rust
    /// The message strip under the editor: severity, `line:column` and the message, from the
    /// same diagnostic the underline was painted from (FR3-009). Positions are one-based, as
    /// editors show them.
    fn diagnostic_strip(&self, diagnostic: &Diagnostic) -> impl IntoElement {
        let position = document_position(&self.editor.document, diagnostic.range.start);
        let label = match diagnostic.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARNING",
        };
        div()
            .id("diagnostic-strip")
            .debug_selector(|| "diagnostic-strip".to_owned())
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .mx(px(30.))
            .mb(px(12.))
            .px(px(14.))
            .py(px(8.))
            .rounded(px(CARD_RADIUS))
            .bg(rgb(PANEL))
            .font_family(self.fonts.mono.clone())
            .text_size(px(11.))
            .child(
                div()
                    .text_color(rgb(severity_colour(diagnostic.severity)))
                    .child(label),
            )
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .child(format!("{}:{}", position.line + 1, position.column + 1)),
            )
            .child(
                div()
                    .text_color(rgb(TEXT))
                    .child(diagnostic.message.clone()),
            )
    }
```

In `render`, find the editor container — the `.child(div().flex_1().min_h_0().flex().px(px(30.)).py(px(20.)).cursor_text() ... )` whose body is the `match self.focus { ... }` over the three surfaces — and directly after its closing `)`, before `.when(pane_visible, |column| { column.child(self.results_splitter(cx)) })`, add:

```rust
                            .children(
                                diagnostic
                                    .as_ref()
                                    .map(|diagnostic| self.diagnostic_strip(diagnostic)),
                            )
```

`diagnostic` is already `None` whenever `self.focus != Focus::Editor` (Task 4, step 6d), so the strip never appears over a result or definition tab.

- [ ] **Step 4: Run the UI tests to verify they pass**

Run: `cargo test --lib ui::`
Expected: all pass.

- [ ] **Step 5: See it in the running app**

Run: `cargo run`, type `SELECT 'abc` — the literal is underlined and the strip reads `ERROR 1:8 unterminated quoted string or identifier`. Type the closing `'` — both disappear. Press Ctrl+Enter with the quote open — the run still goes to the command layer and fails there with its own message (design decision 7; nothing in this task touched `dispatch_command`). Close the app.

- [ ] **Step 6: Record the milestone in `CLAUDE.md`**

In the "Repository state" paragraph, change

```
Two pieces of Phase 3 are implemented on top — Milestone 8 (enhanced `EXPLAIN` — FR3-019,
FR3-020) and Milestone 1 (parsing layer hardening, the groundwork for completion); the rest of
Phase 3 remains a proposal.
```

to

```
Three pieces of Phase 3 are implemented on top — Milestone 8 (enhanced `EXPLAIN` — FR3-019,
FR3-020), Milestone 1 (parsing layer hardening, the groundwork for completion) and the
diagnostics half of Milestone 4 (FR3-009 — advisory syntax problems underlined in the editor,
never gating execution); the rest of Phase 3, formatting included, remains a proposal.
```

In the module map, change the `src/sql.rs` row's contents to:

```
`split_statements`, `relevant_sql`, `analyse_statement`, `referenced_tables`, `prepare_statement`, `prepare_explain`, `highlight_lines`, `diagnose`
```

- [ ] **Step 7: Full verification**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/ui.rs CLAUDE.md
git commit -m "Show the diagnostic in a strip under the editor

Severity, line:column and the message, from the same diagnostic the
underline is painted from, between the editor and the results pane.
It shows only on the editor tab and only while there is something to
report. This completes the diagnostics half of Phase 3 Milestone 4;
formatting is the other half and follows separately."
```

---

## Self-review against the spec

| Spec item | Where |
|---|---|
| Decision 1 — `Diagnostic` separate from `SqlError`, `SqlError` untouched | Task 2; Global Constraints |
| Decision 2 — at most one diagnostic, the earliest | Task 1 `report` (first wins); Task 3 first statement only; tests `the_earliest_lexical_error_is_the_one_reported`, `only_the_first_offending_statement_is_reported` |
| Decision 3 — scanner keeps the position, no public signature change | Task 1 |
| Decision 4 — parenthesis stack, opener/closer ranges | Task 1 `open` stack; test `unbalanced_parentheses_point_at_the_unmatched_bracket` |
| Decision 5 — two tiers, severity marks the difference | Task 2 (`Error`), Task 3 (`Warning`) |
| Decision 6 — statement tier only on a clean scan | Task 3 `let … else`; test `a_lexical_error_suppresses_the_statement_tier` |
| Decision 7 — never gates execution | No task modifies `dispatch_command` or `CommandService`; Task 5 step 5 checks it by hand |
| Detection table, lexical | Task 2 test `each_lexical_problem_is_an_error_with_its_own_message_and_range` |
| Detection table, statement; generous allowlist | Task 3; `ANALYSE` added because PostgreSQL accepts it |
| Layering — `diagnose` in `sql.rs`, per render, no cache | Task 2; Task 4 step 6d |
| Underline — `Highlight` not extended, spans subdivided, wavy with border fallback | Task 4 |
| Strip — severity, message, `line:column` via `DocumentPosition` | Task 5 |
| Testing — pure detection tests; keystroke tests raise and clear | Tasks 1–3; Tasks 4–5 |
| Implementation order — scanner, `diagnose`, UI | Tasks 1, 2–3, 4–5 |

Known deviation: the "empty statement" range is the trimmed statement (the stray `;` plus anything before it after the previous `;`) rather than the zero-width gap between two adjacent semicolons, because an empty range cannot be underlined. Noted in Task 3.
