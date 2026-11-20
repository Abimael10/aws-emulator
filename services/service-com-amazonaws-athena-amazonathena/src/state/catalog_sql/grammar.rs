// SPDX-License-Identifier: CC0-1.0

//! What Athena says about text its QUERY GRAMMAR cannot read as a statement.
//!
//! Athena parses a submitted statement when it is SUBMITTED, so text that is not a
//! statement of the language is refused by `StartQueryExecution` itself rather than
//! accepted and failed later. The refusal carries the parser's own diagnostic — where in
//! the submitted text the grammar stopped, the token it stopped on, and the tokens it
//! accepts in its place — so this module reads those three things off the submitted text
//! with the same tokenizer and parser the rest of this crate reads a statement with.

use ::sqlparser::dialect::GenericDialect;
use ::sqlparser::parser::Parser;
use ::sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer};

use super::statement::{Position, MAX_PARSER_RECURSION, MAX_STATEMENT_TOKENS};

/// The `AthenaErrorCode` / `ErrorCode` of a submission Athena's grammar could not read:
/// the refusal class of a statement that is not a statement of the language.
pub const ERROR_CODE_MALFORMED_QUERY: &str = "MALFORMED_QUERY";

/// The words Athena's query grammar accepts after `CREATE`, in the order the grammar
/// lists them.
///
/// Source: Athena's own SQL grammar, which is what defines the statements the query
/// engine reads — `CREATE MATERIALIZED VIEW`, `CREATE MULTI DIALECT VIEW`,
/// `CREATE OR REPLACE …`, `CREATE PROTECTED MULTI DIALECT VIEW`, `CREATE ROLE`,
/// `CREATE SCHEMA`, `CREATE TABLE` and `CREATE VIEW`. A `CREATE` statement naming any
/// other object — the Hive DDL forms Athena serves through its DDL engine instead — is
/// not a statement that grammar has a production for, so when the DDL engine does not
/// read it either, this is the set the grammar reports as what it expected.
const CREATE_OBJECTS: [&str; 8] = [
    "MATERIALIZED",
    "MULTI",
    "OR",
    "PROTECTED",
    "ROLE",
    "SCHEMA",
    "TABLE",
    "VIEW",
];

/// The words Athena's query grammar accepts where a STATEMENT begins, in the order the
/// grammar lists them.
///
/// Source: Athena's own SQL grammar, whose statement production is the choice between the
/// statements each of these words begins — `ALTER`, `ANALYZE`, `CALL`, `COMMENT`, `COMMIT`,
/// `CREATE`, `DEALLOCATE`, `DELETE`, `DENY`, `DESC`, `DESCRIBE`, `DROP`, `EXECUTE`,
/// `EXPLAIN`, `GRANT`, `INSERT`, `MERGE`, `PREPARE`, `REFRESH`, `RESET`, `REVOKE`,
/// `ROLLBACK`, `SET`, `SHOW`, `START`, `TRUNCATE`, `UNLOAD`, `UPDATE` and `USE` — and a
/// QUERY, which begins with one of several words (`SELECT`, `WITH`, `TABLE`, `VALUES`) and
/// so is listed as the production itself ([`QUERY_PRODUCTION`]) rather than by any one of
/// them. A word outside this set begins no statement that grammar has a production for.
const STATEMENT_START: [&str; 29] = [
    "ALTER",
    "ANALYZE",
    "CALL",
    "COMMENT",
    "COMMIT",
    "CREATE",
    "DEALLOCATE",
    "DELETE",
    "DENY",
    "DESC",
    "DESCRIBE",
    "DROP",
    "EXECUTE",
    "EXPLAIN",
    "GRANT",
    "INSERT",
    "MERGE",
    "PREPARE",
    "REFRESH",
    "RESET",
    "REVOKE",
    "ROLLBACK",
    "SET",
    "SHOW",
    "START",
    "TRUNCATE",
    "UNLOAD",
    "UPDATE",
    "USE",
];

/// How the grammar names the production it accepts beside the statement-start words: a
/// QUERY, which is named by the production rather than by a word, and which the diagnostic
/// therefore states unquoted and last.
const QUERY_PRODUCTION: &str = "<query>";

/// The tokens Athena's query grammar accepts after the NAME of a `CREATE TABLE`, in the
/// order the grammar lists them.
///
/// Source: Athena's own `CREATE TABLE` and `CREATE TABLE AS SELECT` syntax — the column
/// list in parentheses, a further `.` qualifier on the name, `AS` for the form that
/// defines a table from a query, the table `COMMENT` and the `WITH` property list. A
/// token outside this set is one the grammar has no production for at that position.
const AFTER_TABLE_NAME: [&str; 5] = ["(", ".", "AS", "COMMENT", "WITH"];

/// The words a QUERY may begin with.
///
/// Source: Athena's own SQL grammar, whose query production is a `SELECT`, a query prefixed
/// with a `WITH` clause, a `TABLE <name>` or a `VALUES` list. The grammar lists the query as
/// a production rather than by these words ([`QUERY_PRODUCTION`]), so they are what tells a
/// word that begins a query from one that begins no statement at all.
const QUERY_START: [&str; 4] = ["SELECT", "WITH", "TABLE", "VALUES"];

/// The diagnostic Athena refuses `sql` with, for text no grammar here can read as a
/// statement.
///
/// The form is the parser's own: the line and column the grammar stopped at, the token it
/// stopped on, and — where the grammar's accepted set at that position is known — the
/// tokens it accepts there instead.
pub fn malformed_statement(sql: &str) -> String {
    let tokens = significant_tokens(sql);
    // A `CREATE` statement the Hive DDL engine did not read reaches the query grammar,
    // which has a production for none of the objects outside [`CREATE_OBJECTS`]: the
    // mismatch is then the object word itself, whatever the text after it says.
    let create_object = tokens
        .first()
        .filter(|first| word_is(first, "CREATE"))
        .and_then(|_| tokens.get(1))
        .filter(|object| !accepts(&CREATE_OBJECTS, object));
    if let Some(object) = create_object {
        return mismatched_input(
            position_of(object),
            &token_text(object),
            Some(&expected_one_of(&CREATE_OBJECTS)),
        );
    }
    // A table definition whose NAME the grammar read and whose next token it has no
    // production for: the mismatch is that token, and the accepted set is the grammar's
    // own at that position.
    let after_name = after_table_name(&tokens).filter(|token| !accepts(&AFTER_TABLE_NAME, token));
    if let Some(token) = after_name {
        return mismatched_input(
            position_of(token),
            &token_text(token),
            Some(&expected_one_of(&AFTER_TABLE_NAME)),
        );
    }
    // Text whose FIRST word begins no statement of this grammar is a mismatch THERE: a
    // statement-start production is the first thing the grammar reads, so it stops on that
    // word rather than somewhere later in text it never got to read.
    let statement_word = tokens
        .first()
        .filter(|first| matches!(first.token, Token::Word(..)))
        .filter(|first| !accepts(&STATEMENT_START, first) && !accepts(&QUERY_START, first));
    if let Some(first) = statement_word {
        return mismatched_input(
            position_of(first),
            &token_text(first),
            Some(&expected_statement_start()),
        );
    }
    // Everything else is reported where the parser itself stopped reading. A statement
    // past the bound this emulator reads one under is not parsed again here — the bound is
    // what keeps the tree a parse builds, and the recursive walks over it, inside the
    // stack — so the first token past the bound is where the reading stopped.
    let stop = match tokens.get(MAX_STATEMENT_TOKENS) {
        Some(past_bound) => Some((position_of(past_bound), token_text(past_bound))),
        None => stopped_at(sql),
    };
    // Where the grammar stopped at the START of a statement, its accepted set at that
    // position is known too: every word a statement of this language may begin with, and
    // the production for a query. Elsewhere the set is the parser's own and is not
    // reported — and neither is it reported where the parser named no position at all, the
    // case the diagnostic falls back to the start of the submitted text for, since that
    // position is not where the reading stopped.
    let expecting = match &stop {
        Some((at, _)) => begins_a_statement(&tokens, *at),
        None => tokens.is_empty(),
    };
    let (at, token) = stop
        .or_else(|| tokens.first().map(|first| (position_of(first), token_text(first))))
        .unwrap_or_else(|| (Position::START, String::new()));
    let expecting = expecting.then(expected_statement_start);
    mismatched_input(at, &token, expecting.as_deref())
}

/// Whether `at` is where a STATEMENT begins in the submitted text.
///
/// That is the first token the grammar reads, and the first after a statement separator —
/// the positions the grammar reads a statement-start production at, and so the ones whose
/// accepted set is [`STATEMENT_START`]. Text the grammar read no token of at all begins
/// where a statement would have.
fn begins_a_statement(tokens: &[TokenWithSpan], at: Position) -> bool {
    let Some(index) = tokens.iter().position(|token| position_of(token) == at) else {
        return tokens.is_empty();
    };
    index == 0
        || tokens
            .get(index - 1)
            .is_some_and(|previous| matches!(previous.token, Token::SemiColon))
}

/// The accepted set the grammar reports where a statement begins: its statement-start words,
/// quoted and in the grammar's own order, and the query production last.
fn expected_statement_start() -> String {
    format!("{}, {QUERY_PRODUCTION}", expected_one_of(&STATEMENT_START))
}

/// Whether one of `accepted` is what `token` states, however it was written.
fn accepts(accepted: &[&str], token: &TokenWithSpan) -> bool {
    accepted
        .iter()
        .any(|accepted| token_text(token).eq_ignore_ascii_case(accepted))
}

/// The token right after the NAME of a `CREATE TABLE`, or `None` for text that is not a
/// table definition — the only statement whose name position has a known accepted set
/// here.
fn after_table_name(tokens: &[TokenWithSpan]) -> Option<&TokenWithSpan> {
    let mut rest = tokens;
    rest = after_word(rest, "CREATE")?;
    rest = after_word(rest, "TABLE")?;
    if let Some(after) = after_word(rest, "IF")
        .and_then(|rest| after_word(rest, "NOT"))
        .and_then(|rest| after_word(rest, "EXISTS"))
    {
        rest = after;
    }
    // The name, and every further part a `.` qualifies it with.
    rest = after_name(rest)?;
    while let Some(after) = rest
        .split_first()
        .filter(|(first, _)| matches!(first.token, Token::Period))
        .and_then(|(_, after)| after_name(after))
    {
        rest = after;
    }
    rest.first()
}

/// What is left after the word `word`, or `None` when the next token is not that word.
fn after_word<'t>(tokens: &'t [TokenWithSpan], word: &str) -> Option<&'t [TokenWithSpan]> {
    let (first, rest) = tokens.split_first()?;
    word_is(first, word).then_some(rest)
}

/// What is left after a NAME, or `None` when the next token is not one.
fn after_name(tokens: &[TokenWithSpan]) -> Option<&[TokenWithSpan]> {
    let (first, rest) = tokens.split_first()?;
    matches!(first.token, Token::Word(..)).then_some(rest)
}

/// One diagnostic, as the parser words it: where it stopped, the token it stopped on, and
/// the tokens it accepts there when its accepted set at that position is known.
fn mismatched_input(at: Position, token: &str, expecting: Option<&str>) -> String {
    let mut diagnostic = format!(
        "line {}:{}: mismatched input '{token}'",
        at.line, at.column
    );
    if let Some(expecting) = expecting {
        diagnostic.push_str(&format!(". Expecting: {expecting}"));
    }
    diagnostic
}

/// One accepted set, as the parser lists it: each word quoted, in the grammar's own
/// order.
fn expected_one_of(words: &[&str]) -> String {
    words
        .iter()
        .map(|word| format!("'{word}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where the parser stopped reading `sql`, and the token it stopped on, as its own error
/// reports them; `None` for text it read to the end, or an error that names neither.
fn stopped_at(sql: &str) -> Option<(Position, String)> {
    let error = Parser::new(&GenericDialect {})
        .with_recursion_limit(MAX_PARSER_RECURSION)
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_statements())
        .err()?
        .to_string();
    // The parser states where it stopped as `found: <token> at Line: <n>, Column: <n>`,
    // which is the one place its position and its token are reported.
    let (found, at) = error.rsplit_once(" at Line: ")?;
    let token = found.rsplit_once("found: ").map(|(_, token)| token)?;
    let (line, column) = at.split_once(", Column: ")?;
    Some((
        Position {
            line: line.trim().parse().ok()?,
            column: column.trim().parse().ok()?,
        },
        token.trim().to_string(),
    ))
}

/// The tokens `sql` carries that the grammar reads, in order: whitespace and comments
/// carry no production, so the grammar stops at none of them.
///
/// Reached by the sibling that locates one identifier of a submitted statement
/// ([`super::statement::identifier_position`]), so both read the submitted text through one
/// lexer pass rather than two.
pub(super) fn significant_tokens(sql: &str) -> Vec<TokenWithSpan> {
    Tokenizer::new(&GenericDialect {}, sql)
        .tokenize_with_location()
        .unwrap_or_default()
        .into_iter()
        .filter(|token| !matches!(token.token, Token::Whitespace(..)))
        .collect()
}

/// Whether `token` is the word `word`, however it was written: a keyword is matched
/// without regard to case.
pub(super) fn word_is(token: &TokenWithSpan, word: &str) -> bool {
    match &token.token {
        Token::Word(written) => written.value.eq_ignore_ascii_case(word),
        _ => false,
    }
}

/// Where `token` starts in the submitted text.
pub(super) fn position_of(token: &TokenWithSpan) -> Position {
    let start = token.span.start;
    if start.line == 0 {
        return Position::START;
    }
    Position {
        line: start.line,
        column: start.column,
    }
}

/// One token as the diagnostic names it: the text it was written as.
fn token_text(token: &TokenWithSpan) -> String {
    token.token.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text the grammar cannot read is reported where it stopped reading and on the token
    /// it stopped on, with the tokens it accepts there where its accepted set is known.
    /// The rows differ along the object the statement names, the case it is written in,
    /// where in the text the mismatch is, and whether the position has an accepted set.
    #[test]
    fn a_statement_the_grammar_cannot_read_is_reported_where_it_stopped() {
        /// The accepted set the grammar reports after `CREATE`.
        const AFTER_CREATE: &str = "Expecting: 'MATERIALIZED', 'MULTI', 'OR', \
                                    'PROTECTED', 'ROLE', 'SCHEMA', 'TABLE', 'VIEW'";
        // The accepted set the grammar reports where a statement begins.
        let at_statement_start = format!("Expecting: {}", expected_statement_start());

        // (case, the submitted text, the diagnostic)
        let cases: Vec<(&str, String, String)> = vec![
            (
                "a database definition the DDL engine did not read",
                "CREATE DATABASE bad-name".to_string(),
                format!("line 1:8: mismatched input 'DATABASE'. {AFTER_CREATE}"),
            ),
            (
                "an external table whose name the DDL engine did not read",
                "CREATE EXTERNAL TABLE shop.bad-name (id int)".to_string(),
                format!("line 1:8: mismatched input 'EXTERNAL'. {AFTER_CREATE}"),
            ),
            (
                "the same statement in the case it was written in",
                "create external table shop.bad-name (id int)".to_string(),
                format!("line 1:8: mismatched input 'external'. {AFTER_CREATE}"),
            ),
            (
                "an object the grammar defines no statement for at all",
                "CREATE INDEX idx ON shop.bad-name (id)".to_string(),
                format!("line 1:8: mismatched input 'INDEX'. {AFTER_CREATE}"),
            ),
            (
                "a mismatch on the second line of the submitted text",
                "CREATE\n  DATABASE bad-name".to_string(),
                format!("line 2:3: mismatched input 'DATABASE'. {AFTER_CREATE}"),
            ),
            (
                "text past the bound this reader reads a statement under",
                format!("SELECT 1{}", " + 1".repeat(MAX_STATEMENT_TOKENS)),
                // The first token past the bound is where the reading stopped: the
                // statement is `SELECT 1 + 1 …`, so that token is one of the chain's.
                format!(
                    "line 1:{}: mismatched input '+'",
                    MAX_STATEMENT_TOKENS * 2 + 6
                ),
            ),
            (
                "text that is no statement at all, reported where the parser stopped",
                "SELEKT 1".to_string(),
                format!("line 1:1: mismatched input 'SELEKT'. {at_statement_start}"),
            ),
            (
                "a misspelled query word, with the rest of the text no statement either",
                "SELEC 1 FRM dual !!!".to_string(),
                format!("line 1:1: mismatched input 'SELEC'. {at_statement_start}"),
            ),
            (
                "a word that begins a statement of another engine's dialect",
                "VACUUM INTO 'copy.db'".to_string(),
                format!("line 1:1: mismatched input 'VACUUM'. {at_statement_start}"),
            ),
            (
                "a statement that begins after a separator the grammar read",
                "SELECT 1;\n  SELEKT 2".to_string(),
                format!("line 2:3: mismatched input 'SELEKT'. {at_statement_start}"),
            ),
            (
                "a table name the grammar read, followed by a token it does not accept",
                "CREATE TABLE IF NOT EXISTS shop_ddl.bad-name (id int)".to_string(),
                "line 1:40: mismatched input '-'. Expecting: '(', '.', 'AS', 'COMMENT', \
                 'WITH'"
                    .to_string(),
            ),
            (
                "a qualified table name whose first part is quoted",
                "CREATE TABLE `shop`.bad-name AS SELECT 1".to_string(),
                "line 1:24: mismatched input '-'. Expecting: '(', '.', 'AS', 'COMMENT', \
                 'WITH'"
                    .to_string(),
            ),
        ];

        for (case, sql, expected) in cases {
            let actual = malformed_statement(&sql);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
