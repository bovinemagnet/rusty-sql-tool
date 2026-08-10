use std::ops::Range;

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementKind {
    RowReturning,
    DataModification,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedStatement {
    pub sql: String,
    pub automatic_limit: Option<u32>,
}

#[derive(Debug, Error, PartialEq, Eq)]
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
    let tokens = tokenize(sql)?;
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
    Ok(statements)
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

/// Plain EXPLAIN only; callers cannot accidentally request ANALYZE (FR-016).
pub fn prepare_explain(sql: &str) -> String {
    format!("EXPLAIN {}", sql.trim())
}

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
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut depth = 0usize;
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
                depth,
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
                return Err(SqlError::UnterminatedComment);
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                range: start..index,
                depth,
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
                return Err(SqlError::UnterminatedQuote);
            }
            tokens.push(Token {
                kind: TokenKind::Literal,
                range: start..index,
                depth,
            });
            continue;
        }
        if bytes[index] == b'$'
            && let Some(delimiter_end) = dollar_delimiter_end(bytes, index)
        {
            let delimiter = &bytes[index..delimiter_end];
            index = delimiter_end;
            let Some(relative_end) = bytes[index..]
                .windows(delimiter.len())
                .position(|window| window == delimiter)
            else {
                return Err(SqlError::UnterminatedQuote);
            };
            index += relative_end + delimiter.len();
            tokens.push(Token {
                kind: TokenKind::Literal,
                range: start..index,
                depth,
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
                depth,
            });
            continue;
        }
        let character = sql[index..].chars().next().expect("valid UTF-8");
        index += character.len_utf8();
        let token_depth = if character == ')' {
            if depth == 0 {
                return Err(SqlError::UnbalancedParentheses);
            }
            depth -= 1;
            depth
        } else {
            depth
        };
        tokens.push(Token {
            kind: TokenKind::Symbol(character),
            range: start..index,
            depth: token_depth,
        });
        if character == '(' {
            depth += 1;
        }
    }
    if depth != 0 {
        return Err(SqlError::UnbalancedParentheses);
    }
    Ok(tokens)
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

    #[test]
    fn explain_is_never_analyse() {
        let explained = prepare_explain("SELECT 1;");
        assert_eq!(explained, "EXPLAIN SELECT 1;");
        assert!(!explained.contains("ANALYZE"));
    }
}
