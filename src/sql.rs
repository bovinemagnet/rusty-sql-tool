use std::ops::Range;

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementKind {
    RowReturning,
    DataModification,
    SchemaModification,
    Other,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementAnalysis {
    pub kind: StatementKind,
    pub has_explicit_limit: bool,
    pub has_returning: bool,
    pub safe_to_limit: bool,
}

/// A relation named by a statement, with the alias it was bound to. Completion needs both:
/// the name to look the columns up, the alias to know what the user typed before the dot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableReference {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedStatement {
    pub sql: String,
    pub automatic_limit: Option<u32>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SqlError {
    #[error("SQL contains an unterminated quoted string or identifier")]
    UnterminatedQuote,
    #[error("SQL contains an unterminated block comment")]
    UnterminatedComment,
    #[error("SQL contains unbalanced parentheses")]
    UnbalancedParentheses,
    #[error("cursor is outside the SQL document")]
    InvalidCursor,
    #[error("selection is outside the SQL document")]
    InvalidSelection,
    #[error("there is no SQL statement at the cursor")]
    NoStatement,
    #[error("row limit must be a positive integer")]
    InvalidLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenKind {
    Word(String),
    Symbol(char),
    Literal,
    Comment,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    range: Range<usize>,
    depth: usize,
}

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

/// Resolves selection-first/current-statement behaviour for Run and Explain (FR-013, FR-014).
pub fn relevant_sql(
    document: &str,
    selection: Option<Range<usize>>,
    cursor: usize,
) -> Result<&str, SqlError> {
    if let Some(selection) = selection
        && !selection.is_empty()
    {
        if selection.start > selection.end
            || selection.end > document.len()
            || !document.is_char_boundary(selection.start)
            || !document.is_char_boundary(selection.end)
        {
            return Err(SqlError::InvalidSelection);
        }
        let selected = document[selection].trim();
        return (!selected.is_empty())
            .then_some(selected)
            .ok_or(SqlError::NoStatement);
    }
    if cursor > document.len() || !document.is_char_boundary(cursor) {
        return Err(SqlError::InvalidCursor);
    }
    let ranges = split_statements(document)?;
    let range = ranges
        .iter()
        .find(|range| range.start <= cursor && cursor <= range.end)
        .or_else(|| ranges.iter().rev().find(|range| range.end <= cursor))
        .ok_or(SqlError::NoStatement)?;
    Ok(&document[range.clone()])
}

pub fn analyse_statement(sql: &str) -> StatementAnalysis {
    let Ok(tokens) = tokenize(sql) else {
        return StatementAnalysis {
            kind: StatementKind::Unknown,
            has_explicit_limit: false,
            has_returning: false,
            safe_to_limit: false,
        };
    };
    let significant: Vec<_> = tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Comment))
        .collect();
    let Some(first_word) = significant.iter().find_map(|token| match &token.kind {
        TokenKind::Word(word) if token.depth == 0 => Some(word.as_str()),
        _ => None,
    }) else {
        return StatementAnalysis {
            kind: StatementKind::Unknown,
            has_explicit_limit: false,
            has_returning: false,
            safe_to_limit: false,
        };
    };

    let main_word = if first_word == "WITH" {
        significant.iter().find_map(|token| match &token.kind {
            TokenKind::Word(word)
                if token.depth == 0
                    && matches!(
                        word.as_str(),
                        "SELECT" | "VALUES" | "INSERT" | "UPDATE" | "DELETE" | "MERGE"
                    ) =>
            {
                Some(word.as_str())
            }
            _ => None,
        })
    } else {
        Some(first_word)
    };

    // Be deliberately conservative around data-modifying CTEs. An outer SELECT may be
    // limitable in theory, but RETURNING anywhere marks the complete statement unsafe.
    let has_returning = significant
        .iter()
        .any(|token| token_is_word(token, "RETURNING"));
    let select_into = main_word == Some("SELECT") && has_top_level_word(&significant, "INTO");
    let has_limit = has_top_level_word(&significant, "LIMIT")
        || significant.windows(2).any(|pair| {
            pair[0].depth == 0
                && pair[1].depth == 0
                && token_is_word(pair[0], "FETCH")
                && (token_is_word(pair[1], "FIRST") || token_is_word(pair[1], "NEXT"))
        });
    let kind = match main_word {
        Some("SELECT" | "VALUES") if !select_into => StatementKind::RowReturning,
        Some("INSERT" | "UPDATE" | "DELETE" | "MERGE") => StatementKind::DataModification,
        // FR3-023: DDL is held apart from harmless session statements so a protected
        // connection can refuse it rather than lumping both under `Other`.
        Some("CREATE" | "DROP" | "ALTER" | "TRUNCATE") => StatementKind::SchemaModification,
        Some(_) => StatementKind::Other,
        None => StatementKind::Unknown,
    };

    StatementAnalysis {
        kind,
        has_explicit_limit: has_limit,
        has_returning,
        safe_to_limit: kind == StatementKind::RowReturning && !has_limit && !has_returning,
    }
}

/// Collects the relations a statement names, flat across the whole statement (FR3-002, FR3-003).
///
/// Extraction is deliberately flat: a relation named inside a subquery is collected alongside the
/// outer ones rather than scoped to it. Completion is the consumer, so over-suggesting a column
/// that belongs to a sibling scope costs the user a glance, while under-suggesting hides a column
/// that exists. Names that resolve to nothing — a CTE, a derived table — simply match no catalogue
/// entry and offer nothing.
pub fn referenced_tables(sql: &str) -> Vec<TableReference> {
    let Ok(tokens) = tokenize(sql) else {
        return Vec::new();
    };
    let significant: Vec<&Token> = tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Comment))
        .collect();

    let mut references = Vec::new();
    let mut index = 0;
    while index < significant.len() {
        match relation_list_at(&significant, index) {
            Some((start, comma_separated)) => {
                index =
                    read_relation_list(sql, &significant, start, comma_separated, &mut references);
            }
            None => index += 1,
        }
    }
    references
}

/// Recognises the keywords that introduce a relation, reporting where the list starts and whether
/// it may continue past a comma. `JOIN` and the write targets take exactly one relation.
fn relation_list_at(tokens: &[&Token], index: usize) -> Option<(usize, bool)> {
    match &tokens[index].kind {
        TokenKind::Word(word) => match word.as_str() {
            "FROM" => Some((index + 1, true)),
            "JOIN" => Some((index + 1, false)),
            "UPDATE" => Some((index + 1, false)),
            // Only `INSERT INTO` and `MERGE INTO` name a relation; `SELECT … INTO` does too, but
            // it creates the target rather than reading it, so completion gains nothing from it.
            "INTO" if index > 0 => match &tokens[index - 1].kind {
                TokenKind::Word(previous) if matches!(previous.as_str(), "INSERT" | "MERGE") => {
                    Some((index + 1, false))
                }
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Reads relations from `start` until the list ends, appending what it finds. Returns the position
/// to resume scanning from, which is always past `start` so the outer loop cannot stall.
fn read_relation_list(
    sql: &str,
    tokens: &[&Token],
    start: usize,
    comma_separated: bool,
    references: &mut Vec<TableReference>,
) -> usize {
    let mut index = start;
    loop {
        index = match read_relation(sql, tokens, index, references) {
            Some(next) => next,
            None => break,
        };
        if !comma_separated
            || !matches!(
                tokens.get(index).map(|t| &t.kind),
                Some(TokenKind::Symbol(','))
            )
        {
            break;
        }
        index += 1;
    }
    index.max(start)
}

/// Reads one entry: a qualified name and its optional alias.
fn read_relation(
    sql: &str,
    tokens: &[&Token],
    start: usize,
    references: &mut Vec<TableReference>,
) -> Option<usize> {
    // A derived table opens with a parenthesis and names no relation, so this reads nothing and
    // leaves the scan to walk into the subquery, where the relations it reads are collected flat.
    let mut parts = Vec::new();
    let mut index = start;
    loop {
        parts.push(identifier(sql, tokens.get(index)?)?);
        index += 1;
        if matches!(
            tokens.get(index).map(|t| &t.kind),
            Some(TokenKind::Symbol('.'))
        ) {
            index += 1;
        } else {
            break;
        }
    }

    let (alias, index) = match read_alias(sql, tokens, index) {
        Some((alias, next)) => (Some(alias), next),
        None => (None, index),
    };
    // A qualified name may carry a catalogue as well as a schema; the last two parts are the ones
    // the catalogue is queried by.
    let name = parts.pop()?;
    references.push(TableReference {
        schema: parts.pop(),
        name,
        alias,
    });
    Some(index)
}

/// Reads `AS name`, or a bare name that is not a keyword continuing the clause.
fn read_alias(sql: &str, tokens: &[&Token], start: usize) -> Option<(String, usize)> {
    if token_is_word(tokens.get(start)?, "AS") {
        return Some((identifier(sql, tokens.get(start + 1)?)?, start + 2));
    }
    Some((identifier(sql, tokens.get(start)?)?, start + 1))
}

/// Reads an identifier from the token's source text rather than its payload, because `Word` holds
/// the text uppercased. Unquoted names fold to lower case as PostgreSQL folds them, so they match
/// the catalogue; a quoted name keeps its exact spelling, with doubled quotes unescaped.
fn identifier(sql: &str, token: &Token) -> Option<String> {
    let text = &sql[token.range.clone()];
    match &token.kind {
        TokenKind::Word(word) if !is_clause_keyword(word) => Some(text.to_lowercase()),
        // A double-quoted identifier and a string literal are the same token kind; only the
        // opening byte tells them apart.
        TokenKind::Literal if text.starts_with('"') => {
            Some(text[1..text.len() - 1].replace("\"\"", "\""))
        }
        _ => None,
    }
}

/// Words that continue the surrounding clause, and so can be neither a relation name nor an alias.
fn is_clause_keyword(word: &str) -> bool {
    matches!(
        word,
        "AND"
            | "AS"
            | "CROSS"
            | "EXCEPT"
            | "FETCH"
            | "FOR"
            | "FROM"
            | "FULL"
            | "GROUP"
            | "HAVING"
            | "INNER"
            | "INTERSECT"
            | "INTO"
            | "JOIN"
            | "LATERAL"
            | "LEFT"
            | "LIMIT"
            | "NATURAL"
            | "NOT"
            | "OFFSET"
            | "ON"
            | "OR"
            | "ORDER"
            | "RETURNING"
            | "RIGHT"
            | "SELECT"
            | "SET"
            | "TABLESAMPLE"
            | "UNION"
            | "USING"
            | "VALUES"
            | "WHERE"
            | "WINDOW"
            | "WITH"
    )
}

/// Adds a limit only when analysis can prove this is safe (FR-018–FR-020, FR-032).
pub fn prepare_statement(sql: &str, row_limit: u32) -> Result<PreparedStatement, SqlError> {
    if row_limit == 0 {
        return Err(SqlError::InvalidLimit);
    }
    let analysis = analyse_statement(sql);
    if !analysis.safe_to_limit {
        return Ok(PreparedStatement {
            sql: sql.to_owned(),
            automatic_limit: None,
        });
    }
    let tokens = tokenize(sql)?;
    let top_level_semicolon = tokens
        .iter()
        .rev()
        .find(|token| token.depth == 0 && matches!(token.kind, TokenKind::Symbol(';')));
    let trailing_boundary = top_level_semicolon.map_or_else(
        || {
            tokens
                .iter()
                .rev()
                .find(|token| !matches!(token.kind, TokenKind::Comment))
                .map_or(0, |token| token.range.end)
        },
        |token| token.range.start,
    );
    // PostgreSQL's locking clause follows LIMIT in SELECT syntax. Appending after
    // `FOR UPDATE` would be invalid, so inject immediately before that clause.
    let insertion = tokens
        .iter()
        .find(|token| {
            token.depth == 0 && token.range.start < trailing_boundary && token_is_word(token, "FOR")
        })
        .map_or(trailing_boundary, |token| token.range.start);
    let (before, after) = sql.split_at(insertion);
    let separator = if before.ends_with(char::is_whitespace) || before.is_empty() {
        ""
    } else {
        " "
    };
    let suffix = if after
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
    {
        " "
    } else {
        ""
    };
    Ok(PreparedStatement {
        sql: format!("{before}{separator}LIMIT {row_limit}{suffix}{after}"),
        automatic_limit: Some(row_limit),
    })
}

/// Builds the EXPLAIN statement. Only `CommandService::explain_analyse` passes `analyse = true`;
/// plain Explain cannot request ANALYZE (FR-016). JSON output is what the provider parses (FR3-020).
pub fn prepare_explain(sql: &str, analyse: bool) -> String {
    let options = if analyse {
        "ANALYZE, FORMAT JSON"
    } else {
        "FORMAT JSON"
    };
    format!("EXPLAIN ({options}) {}", sql.trim())
}

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
    let (tokens, error) = scan(sql);
    let Some(ScanError { error, range }) = error else {
        return statement_warning(sql, &tokens);
    };
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

/// What the editor should paint a stretch of SQL as (FR-012).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlight {
    Keyword,
    Literal,
    Comment,
    Function,
    Plain,
}

/// A run of one line, coloured as a whole. `range` indexes the line, not the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub highlight: Highlight,
}

/// Colours a document line by line. One entry per `split('\n')` line, in order, together covering
/// that line exactly — so a renderer can paint the spans and reproduce the text (FR-012, 59.3).
pub fn highlight_lines(sql: &str) -> Vec<Vec<HighlightSpan>> {
    let starts = line_starts(sql);
    let mut lines = vec![Vec::new(); starts.len()];
    for span in document_spans(sql) {
        // A comment or a dollar-quoted literal is one token over several lines, so it is cut at
        // each newline and handed to the line it belongs to.
        let mut start = span.range.start;
        loop {
            let line = starts.partition_point(|&offset| offset <= start) - 1;
            let line_end = starts.get(line + 1).map_or(sql.len(), |next| next - 1);
            let end = span.range.end.min(line_end);
            if end > start {
                lines[line].push(HighlightSpan {
                    range: start - starts[line]..end - starts[line],
                    highlight: span.highlight,
                });
            }
            if end == span.range.end {
                break;
            }
            start = end + 1;
        }
    }
    lines
}

/// The byte offset each line begins at, always including the first.
fn line_starts(sql: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        sql.bytes()
            .enumerate()
            .filter(|&(_, byte)| byte == b'\n')
            .map(|(index, _)| index + 1),
    );
    starts
}

/// The whole document as coloured runs, in order and without gaps.
fn document_spans(sql: &str) -> Vec<HighlightSpan> {
    let (tokens, _) = scan(sql);
    let mut spans = Vec::new();
    let mut cursor = 0;
    for (position, token) in tokens.iter().enumerate() {
        if token.range.start > cursor {
            spans.push(HighlightSpan {
                range: cursor..token.range.start,
                highlight: Highlight::Plain,
            });
        }
        spans.push(HighlightSpan {
            range: token.range.clone(),
            highlight: classify(token, tokens.get(position + 1)),
        });
        cursor = token.range.end;
    }
    if cursor < sql.len() {
        spans.push(HighlightSpan {
            range: cursor..sql.len(),
            highlight: Highlight::Plain,
        });
    }
    spans
}

fn classify(token: &Token, next: Option<&Token>) -> Highlight {
    match &token.kind {
        TokenKind::Comment => Highlight::Comment,
        TokenKind::Literal => Highlight::Literal,
        TokenKind::Word(word) if KEYWORDS.contains(&word.as_str()) => Highlight::Keyword,
        TokenKind::Word(_) if opens_a_call(token, next) => Highlight::Function,
        TokenKind::Word(_) | TokenKind::Symbol(_) => Highlight::Plain,
    }
}

/// A call is a word with `(` against it — `count(` is a call, `VALUES (` is not.
fn opens_a_call(token: &Token, next: Option<&Token>) -> bool {
    next.is_some_and(|next| {
        next.kind == TokenKind::Symbol('(') && next.range.start == token.range.end
    })
}

/// Uppercase because [`TokenKind::Word`] already folds case.
const KEYWORDS: [&str; 27] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "WITH",
    "CREATE",
    "ALTER",
    "DROP",
    "JOIN",
    "WHERE",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "RETURNING",
    "EXPLAIN",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "FROM",
    "AS",
    "VALUES",
    "FETCH",
    "FIRST",
    "ROWS",
    "ONLY",
];

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

fn has_top_level_word(tokens: &[&Token], expected: &str) -> bool {
    tokens
        .iter()
        .any(|token| token.depth == 0 && token_is_word(token, expected))
}

fn token_is_word(token: &Token, expected: &str) -> bool {
    matches!(&token.kind, TokenKind::Word(word) if word == expected)
}

fn trimmed_range(sql: &str, range: Range<usize>) -> Option<Range<usize>> {
    let text = &sql[range.clone()];
    let leading = text.len() - text.trim_start().len();
    let trailing = text.trim_end().len();
    (leading < trailing).then_some(range.start + leading..range.start + trailing)
}

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

fn dollar_delimiter_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_') {
        index += 1;
    }
    (bytes.get(index) == Some(&b'$')).then_some(index + 1)
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_word_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maps each line's spans back to the text they cover, which is what the editor paints.
    fn painted(sql: &str) -> Vec<Vec<(&str, Highlight)>> {
        highlight_lines(sql)
            .into_iter()
            .zip(sql.split('\n'))
            .map(|(spans, line)| {
                spans
                    .into_iter()
                    .map(|span| (&line[span.range], span.highlight))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_keyword_inside_a_string_literal_is_not_a_keyword() {
        assert_eq!(
            painted("SELECT 'from the table'")[0],
            [
                ("SELECT", Highlight::Keyword),
                (" ", Highlight::Plain),
                ("'from the table'", Highlight::Literal),
            ]
        );
    }

    #[test]
    fn a_comment_marker_inside_a_literal_does_not_start_a_comment() {
        assert_eq!(
            painted("SELECT '--not a comment', 1")[0][2],
            ("'--not a comment'", Highlight::Literal)
        );
    }

    #[test]
    fn a_dollar_quoted_body_is_a_literal() {
        assert_eq!(
            painted("SELECT $$SELECT FROM$$")[0][2],
            ("$$SELECT FROM$$", Highlight::Literal)
        );
    }

    /// Every keystroke leaves the document briefly unparseable, so highlighting cannot depend on
    /// the document being valid the way statement splitting does.
    #[test]
    fn a_half_typed_literal_is_still_highlighted() {
        assert_eq!(
            painted("SELECT 'abc")[0],
            [
                ("SELECT", Highlight::Keyword),
                (" ", Highlight::Plain),
                ("'abc", Highlight::Literal),
            ]
        );
    }

    #[test]
    fn a_word_calling_a_function_is_a_function() {
        assert_eq!(
            painted("SELECT count(*)")[0],
            [
                ("SELECT", Highlight::Keyword),
                (" ", Highlight::Plain),
                ("count", Highlight::Function),
                ("(", Highlight::Plain),
                ("*", Highlight::Plain),
                (")", Highlight::Plain),
            ]
        );
    }

    #[test]
    fn a_block_comment_stays_a_comment_across_lines() {
        let painted = painted("/* two\nline */ SELECT");
        assert_eq!(painted[0], [("/* two", Highlight::Comment)]);
        assert_eq!(
            painted[1],
            [
                ("line */", Highlight::Comment),
                (" ", Highlight::Plain),
                ("SELECT", Highlight::Keyword),
            ]
        );
    }

    #[test]
    fn splits_only_top_level_semicolons() {
        let sql = "SELECT ';' AS x; /* ; */ SELECT $$;$$; SELECT (VALUES (';'));";
        let statements = split_statements(sql).unwrap();
        let text: Vec<_> = statements.iter().map(|range| &sql[range.clone()]).collect();
        assert_eq!(
            text,
            [
                "SELECT ';' AS x;",
                "/* ; */ SELECT $$;$$;",
                "SELECT (VALUES (';'));"
            ]
        );
    }

    #[test]
    fn selection_takes_precedence_over_cursor_statement() {
        let sql = "SELECT 1;\nSELECT 2;";
        assert_eq!(relevant_sql(sql, Some(0..8), 20).unwrap(), "SELECT 1");
        assert_eq!(relevant_sql(sql, None, 19).unwrap(), "SELECT 2;");
    }

    #[test]
    fn injects_default_limit_before_semicolon() {
        let prepared = prepare_statement("SELECT * FROM customer;", 10).unwrap();
        assert_eq!(prepared.sql, "SELECT * FROM customer LIMIT 10;");
        assert_eq!(prepared.automatic_limit, Some(10));
    }

    #[test]
    fn preserves_explicit_limit() {
        let sql = "SELECT * FROM customer LIMIT 50;";
        assert_eq!(prepare_statement(sql, 10).unwrap().sql, sql);
    }

    #[test]
    fn preserves_fetch_first_limit() {
        let sql = "SELECT * FROM customer FETCH FIRST 20 ROWS ONLY;";
        assert_eq!(prepare_statement(sql, 10).unwrap().sql, sql);
    }

    #[test]
    fn ignores_limit_inside_comments_literals_and_subqueries() {
        let sql = "SELECT 'LIMIT 99', (SELECT 1 LIMIT 1) /* LIMIT 4 */;";
        assert_eq!(
            prepare_statement(sql, 10).unwrap().sql,
            "SELECT 'LIMIT 99', (SELECT 1 LIMIT 1) /* LIMIT 4 */ LIMIT 10;"
        );
    }

    #[test]
    fn limits_with_select_and_values() {
        assert_eq!(
            prepare_statement("WITH x AS (SELECT 1) SELECT * FROM x;", 10)
                .unwrap()
                .automatic_limit,
            Some(10)
        );
        assert_eq!(
            prepare_statement("VALUES (1), (2);", 10)
                .unwrap()
                .automatic_limit,
            Some(10)
        );
    }

    #[test]
    fn never_limits_modification_ddl_transactions_or_returning() {
        for sql in [
            "UPDATE customer SET active = false;",
            "DELETE FROM customer;",
            "INSERT INTO customer VALUES (1);",
            "CREATE TABLE x (id int);",
            "DROP TABLE x;",
            "BEGIN;",
            "COMMIT;",
            "UPDATE customer SET active = false RETURNING id;",
            "WITH changed AS (DELETE FROM x RETURNING id) SELECT * FROM changed;",
            "SELECT * INTO temporary x FROM customer;",
        ] {
            assert_eq!(
                prepare_statement(sql, 10).unwrap().automatic_limit,
                None,
                "unexpected limit for {sql}"
            );
        }
    }

    #[test]
    fn uncertain_statement_is_never_modified() {
        let sql = "SELECT 'unterminated";
        assert_eq!(prepare_statement(sql, 10).unwrap().sql, sql);
    }

    #[test]
    fn inserts_before_trailing_line_comment() {
        assert_eq!(
            prepare_statement("SELECT 1 -- LIMIT is absent", 10)
                .unwrap()
                .sql,
            "SELECT 1 LIMIT 10 -- LIMIT is absent"
        );
    }

    #[test]
    fn inserts_limit_before_postgres_locking_clause() {
        assert_eq!(
            prepare_statement("SELECT * FROM jobs FOR UPDATE SKIP LOCKED;", 10)
                .unwrap()
                .sql,
            "SELECT * FROM jobs LIMIT 10 FOR UPDATE SKIP LOCKED;"
        );
    }

    #[test]
    fn unbalanced_statement_is_never_modified() {
        let sql = "SELECT (1;";
        assert_eq!(prepare_statement(sql, 10).unwrap().sql, sql);
    }

    /// FR-016: the plain form never analyses. Both forms ask for JSON so the provider can build
    /// the plan model from one execution.
    #[test]
    fn explain_forms_request_json_and_only_analyse_when_asked() {
        assert_eq!(
            prepare_explain("  SELECT 1;  ", false),
            "EXPLAIN (FORMAT JSON) SELECT 1;"
        );
        assert_eq!(
            prepare_explain("SELECT 1", true),
            "EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1"
        );
    }

    /// FR3-023: a read-only profile has to fail closed, which it cannot do while DDL and
    /// harmless session statements share `Other`.
    #[test]
    fn ddl_is_classified_apart_from_harmless_session_statements() {
        for sql in [
            "CREATE TABLE t (id int)",
            "DROP TABLE t",
            "ALTER TABLE t ADD COLUMN c int",
            "TRUNCATE t",
        ] {
            assert_eq!(
                analyse_statement(sql).kind,
                StatementKind::SchemaModification,
                "{sql}"
            );
        }
        for sql in ["SET search_path TO public", "SHOW work_mem", "BEGIN"] {
            assert_eq!(analyse_statement(sql).kind, StatementKind::Other, "{sql}");
        }
    }

    fn table(schema: Option<&str>, name: &str, alias: Option<&str>) -> TableReference {
        TableReference {
            schema: schema.map(str::to_owned),
            name: name.to_owned(),
            alias: alias.map(str::to_owned),
        }
    }

    /// FR3-003: alias-aware column completion needs the alias bound to the relation it names.
    #[test]
    fn tables_and_aliases_come_from_from_and_join_clauses() {
        assert_eq!(
            referenced_tables(
                "SELECT o.id, c.name FROM order_line o JOIN customer AS c ON c.id = o.customer_id"
            ),
            [
                table(None, "order_line", Some("o")),
                table(None, "customer", Some("c")),
            ]
        );
    }

    /// Names arrive folded the way PostgreSQL folds them, so a later catalogue lookup matches.
    #[test]
    fn unquoted_names_fold_to_lower_case_and_quoted_names_keep_their_spelling() {
        assert_eq!(
            referenced_tables(r#"SELECT * FROM "Order Line" AS "O", Public.Customer c"#),
            [
                table(None, "Order Line", Some("O")),
                table(Some("public"), "customer", Some("c")),
            ]
        );
    }

    #[test]
    fn a_doubled_quote_inside_a_quoted_identifier_is_unescaped() {
        assert_eq!(
            referenced_tables(r#"SELECT * FROM "the ""odd"" table""#),
            [table(None, r#"the "odd" table"#, None)]
        );
    }

    /// The write targets are relations too, and completion is wanted inside them.
    #[test]
    fn write_targets_are_collected() {
        assert_eq!(
            referenced_tables("INSERT INTO audit.entry (id) VALUES (1)"),
            [table(Some("audit"), "entry", None)]
        );
        assert_eq!(
            referenced_tables("UPDATE customer c SET name = 'x' WHERE c.id = 1"),
            [table(None, "customer", Some("c"))]
        );
        assert_eq!(
            referenced_tables("DELETE FROM customer WHERE id = 1"),
            [table(None, "customer", None)]
        );
        assert_eq!(
            referenced_tables("MERGE INTO target t USING source ON t.id = source.id"),
            [table(None, "target", Some("t"))]
        );
    }

    /// Flat extraction: the inner relation is collected, and the derived alias names no relation
    /// so it is skipped rather than invented.
    #[test]
    fn a_relation_inside_a_subquery_is_collected_and_the_derived_alias_is_not() {
        assert_eq!(
            referenced_tables("SELECT * FROM (SELECT id FROM orders) t JOIN customer c ON true"),
            [
                table(None, "orders", None),
                table(None, "customer", Some("c"))
            ]
        );
    }

    /// A CTE name is recorded like any other reference; it resolves to nothing and offers nothing.
    #[test]
    fn a_cte_is_recorded_alongside_the_relation_it_reads() {
        assert_eq!(
            referenced_tables("WITH recent AS (SELECT * FROM orders) SELECT * FROM recent"),
            [table(None, "orders", None), table(None, "recent", None)]
        );
    }

    #[test]
    fn a_comment_between_the_keyword_and_the_name_is_ignored() {
        assert_eq!(
            referenced_tables("SELECT * FROM /* which one? */ orders -- here\n"),
            [table(None, "orders", None)]
        );
    }

    #[test]
    fn a_string_literal_naming_a_clause_introduces_no_relation() {
        assert_eq!(referenced_tables("SELECT 'from orders' AS note"), []);
    }

    #[test]
    fn unparseable_sql_yields_no_relations() {
        assert_eq!(referenced_tables("SELECT * FROM (orders"), []);
        assert_eq!(referenced_tables("SELECT * FROM 'unterminated"), []);
    }

    #[test]
    fn a_clause_keyword_after_the_name_is_not_read_as_an_alias() {
        assert_eq!(
            referenced_tables("SELECT * FROM orders WHERE id = 1"),
            [table(None, "orders", None)]
        );
        assert_eq!(
            referenced_tables("SELECT * FROM orders ORDER BY id"),
            [table(None, "orders", None)]
        );
    }

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
            assert_eq!(
                diagnose(&format!("{keyword} something;")),
                None,
                "{keyword}"
            );
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

    /// A clean document reports nothing, and so does an empty one.
    #[test]
    fn a_clean_document_has_no_diagnostic() {
        assert_eq!(diagnose("SELECT (1, 'a''b') /* ok */ -- fine"), None);
        assert_eq!(diagnose(""), None);
    }
}
