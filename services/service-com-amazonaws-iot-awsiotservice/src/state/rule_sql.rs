// SPDX-License-Identifier: CC0-1.0

//! A topic rule's SQL STATEMENT, and what it selects out of one published message.
//!
//! The statement is `SELECT <select clause> FROM '<topic filter>' [WHERE <condition>]` (the
//! AWS IoT SQL reference). The topic filter is a single-quoted literal, which no SQL grammar
//! parses as a relation, so the one span naming it ([`super::selected_topic`], this service's
//! lexer of that clause) is replaced by a quoted identifier and the rest of the statement is
//! READ by the `sqlparser` crate: the select clause and the condition are its AST, never this
//! module's own parse of the text.
//!
//! The same pass decides whether a statement may be RECORDED at all ([`sql_rejection`]): a
//! statement the parser cannot read is refused by the writer, so no rule is stored whose SQL
//! could never evaluate, and the refusal reports the grammar's expectation and the place the
//! parse stopped.
//!
//! What then runs over a message is the purpose-built walk in [`super::rule_expression`],
//! which reads the message document and nothing else. This module FAILS CLOSED around it: a
//! statement it cannot read, a clause an AWS IoT statement has no place for, an expression the
//! walk does not compute, and a projection whose payload is not a document are each
//! [`Selected::Undecided`], which the rules engine answers by performing NO action rather than
//! by approximating one. It is bounded too: the statement must fit
//! [`super::MAX_SQL_SCAN_CHARACTERS`], which caps this pass's work and its one allocation, and
//! nothing here panics on a caller's text — the reading answers `Option` and the walk answers
//! its own verdict.

use ::serde_json::{Map, Value};
use ::sqlparser::ast::{Expr, GroupByExpr, Select, SelectItem, SetExpr, Statement};
use ::sqlparser::dialect::GenericDialect;
use ::sqlparser::keywords::Keyword;
use ::sqlparser::parser::Parser;
use ::sqlparser::tokenizer::{Token, Tokenizer};

use super::rule_expression::{evaluate, Evaluated, Message};

/// The relation name the topic filter's place is read as, so the rest of the statement parses
/// as SQL ([`parsed_statement`]).
const MESSAGE_RELATION: &str = "iot_message";

/// What a rule's statement selects out of ONE published message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selected {
    /// The message the statement selects, as the bytes every action is handed.
    Message(Vec<u8>),
    /// The statement's condition does not hold for this message.
    NotSelected,
    /// A statement this engine does not evaluate. No action is performed for it, because
    /// performing one would hand a target a message the rule never selected.
    Undecided,
}

/// What the rule whose statement is `sql` selects out of the message `payload` published to
/// `topic`.
///
/// * `SELECT *` hands over the message AS PUBLISHED — the whole payload, byte for byte, the
///   one reading that is faithful for a payload that is not a JSON document either;
/// * a select clause naming attributes hands over a document carrying those attributes, each
///   under the name the clause gives it (its alias, else the last segment of the path), with
///   an attribute the message does not carry left out;
/// * a select clause naming the wildcard BESIDE other attributes hands over the message's own
///   members with those attributes added to them, which is what `SELECT *, <expression> AS
///   <alias>` adds a value to the payload with;
/// * a `WHERE` condition decides selection with SQL's three-valued logic: a condition that is
///   unknown for the message selects nothing, as it does in SQL.
///
/// `topic` is what the statement's `topic()` function reports
/// ([`super::rule_expression`]), in the select clause and in the condition alike.
pub fn selected_message(sql: &str, topic: &str, payload: &[u8]) -> Selected {
    let Some(parsed) = parsed_statement(sql) else {
        return Selected::Undecided;
    };
    // The message as a document, which a condition and a projection are evaluated over. A
    // payload that is not a document is only ever selected whole, by `SELECT *`.
    let document = ::serde_json::from_slice::<Value>(payload).ok();

    if let Some(condition) = &parsed.selection {
        let Some(document) = document.as_ref() else {
            return Selected::Undecided;
        };
        match evaluate(condition, Message { topic, document }, 0) {
            Evaluated::Known(Value::Bool(true)) => {}
            Evaluated::Known(Value::Bool(false)) | Evaluated::Known(Value::Null) => {
                return Selected::NotSelected
            }
            // A condition unknown for this message selects nothing: a comparison against an
            // attribute the message does not carry is not a match.
            Evaluated::Absent => return Selected::NotSelected,
            Evaluated::Known(_) | Evaluated::Undecided => return Selected::Undecided,
        }
    }

    // `SELECT *` is the whole message, answered from the payload rather than from the
    // document read out of it.
    if parsed.projection.len() == 1 && matches!(parsed.projection[0], SelectItem::Wildcard(_)) {
        return Selected::Message(payload.to_vec());
    }
    let Some(document) = document.as_ref() else {
        return Selected::Undecided;
    };
    let mut selected = Map::new();
    for item in &parsed.projection {
        let (name, expression) = match item {
            SelectItem::UnnamedExpr(expression) => match selected_name(expression) {
                Some(name) => (name, expression),
                None => return Selected::Undecided,
            },
            SelectItem::ExprWithAlias { expr, alias } => (alias.value.clone(), expr),
            // `SELECT *, <expression> AS <alias>` ADDS to the message: the wildcard beside
            // other items is every member of the message's own document, and the items beside
            // it are added to those — which is how a statement carries the payload through and
            // names one more value with it. A document the message has no members of (a payload
            // that is no JSON object) is a projection this engine does not compute, because
            // there is nothing for the wildcard to carry.
            SelectItem::Wildcard(_) => match document {
                Value::Object(members) => {
                    for (member, value) in members {
                        selected.insert(member.clone(), value.clone());
                    }
                    continue;
                }
                _ => return Selected::Undecided,
            },
            // A qualified wildcard names a relation, which an AWS IoT statement has none of.
            SelectItem::QualifiedWildcard(..) => return Selected::Undecided,
        };
        match evaluate(expression, Message { topic, document }, 0) {
            Evaluated::Known(value) => {
                selected.insert(name, value);
            }
            // An attribute the message does not carry is left out of the selected document.
            Evaluated::Absent => {}
            Evaluated::Undecided => return Selected::Undecided,
        }
    }
    Selected::Message(Value::Object(selected).to_string().into_bytes())
}

/// The clauses of a rule's statement this engine reads.
struct ParsedStatement {
    projection: Vec<SelectItem>,
    selection: Option<Expr>,
}

/// Read `sql` as a topic rule's statement, or `None` when it is not one this engine evaluates:
/// one longer than the prefix the topic-filter scan reads, one `sqlparser` does not parse, or
/// one carrying a clause an AWS IoT statement has no place for (`WITH`, a set operation,
/// `GROUP BY`, `HAVING`, `DISTINCT`, `ORDER BY`, a limit, a join).
fn parsed_statement(sql: &str) -> Option<ParsedStatement> {
    // Read at most one character past the bound, so an over-long statement is refused without
    // allocating the whole of it — the same `take` `super::selected_topic` scans under.
    let characters: Vec<char> = sql.chars().take(super::MAX_SQL_SCAN_CHARACTERS + 1).collect();
    if characters.len() > super::MAX_SQL_SCAN_CHARACTERS {
        return None;
    }
    let topic = super::selected_topic(sql)?;
    let standard = standard_statement(&characters, &topic)?;

    let statements = Parser::parse_sql(&GenericDialect {}, &standard).ok()?;
    let [Statement::Query(query)] = statements.as_slice() else {
        return None;
    };
    if query.with.is_some() || query.order_by.is_some() || query.limit_clause.is_some() {
        return None;
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let select: &Select = select.as_ref();
    if select.having.is_some() || select.distinct.is_some() {
        return None;
    }
    if !matches!(
        &select.group_by,
        GroupByExpr::Expressions(expressions, modifiers)
            if expressions.is_empty() && modifiers.is_empty()
    ) {
        return None;
    }
    // One relation — the topic filter's place — and no join.
    let [from] = select.from.as_slice() else {
        return None;
    };
    if !from.joins.is_empty() || select.projection.is_empty() {
        return None;
    }
    Some(ParsedStatement {
        projection: select.projection.clone(),
        selection: select.selection.clone(),
    })
}

/// `characters` with the topic filter's span rewritten so the whole statement is standard
/// SQL the parser reads.
///
/// The span is replaced by a DOUBLE-quoted identifier holding the literal's own text — which
/// is what lets the parser read as a relation the one clause no SQL grammar reads as one — and
/// it is exactly as many characters as the span it replaces, so a position the parser reports
/// in the result is that same position in the statement the caller sent. A literal carrying a
/// `"` of its own cannot be quoted that way, so that one is replaced by [`MESSAGE_RELATION`]
/// instead: the statement is still read, at the cost of shifting the positions after the span.
fn standard_statement(characters: &[char], topic: &super::SelectedTopic) -> Option<String> {
    let inner = characters.get(topic.start.checked_add(1)?..topic.end.checked_sub(1)?)?;
    let mut replacement: Vec<char> = Vec::with_capacity(inner.len() + 2);
    if inner.contains(&'"') {
        replacement.extend(MESSAGE_RELATION.chars());
    } else {
        replacement.push('"');
        replacement.extend_from_slice(inner);
        replacement.push('"');
    }
    // Both ends are read through `get`, so no offset can index out of bounds.
    Some(
        characters
            .get(..topic.start)?
            .iter()
            .copied()
            .chain(replacement)
            .chain(characters.get(topic.end..)?.iter().copied())
            .collect(),
    )
}

/// The name a selected expression carrying no alias is reported under: the last segment of the
/// attribute path it names, so `SELECT state.reported.temp` selects `temp`, and the function's
/// own name for a CALL, so `SELECT topic()` selects `topic` — which is the name the AWS IoT SQL
/// reference gives each. `None` for an expression that names neither — one this engine would
/// have to name itself.
fn selected_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Identifier(identifier) => Some(identifier.value.clone()),
        Expr::CompoundIdentifier(identifiers) => identifiers.last().map(|last| last.value.clone()),
        Expr::Nested(inner) => selected_name(inner),
        Expr::Function(function) => Some(function.name.to_string()),
        _ => None,
    }
}

// ============================================================================
// Whether a statement is one the rules engine can be GIVEN at all
// ============================================================================
//
// The reading above is what the engine does with a statement it HOLDS. The reading below
// is what a WRITER of a rule does with the statement a request carries: a statement that
// cannot be parsed is refused, so no rule is recorded whose SQL could never evaluate.

/// The keyword every AWS IoT rule statement opens with.
///
/// The AWS IoT SQL reference gives a statement as `SELECT <attributes> FROM <topic filter>
/// [WHERE <condition>]`, so the grammar expects this keyword first and a statement that
/// opens with anything else is not a rule statement at all.
const SELECT_KEYWORD: &str = "SELECT";

/// What a caller is told about a statement the parser refused without saying where: the
/// sentence the modelled error itself carries ("The Rule-SQL expression can't be parsed
/// correctly", `smithy_query(query="shape",shape="SqlParseException")`).
pub const UNREADABLE_SQL_MESSAGE: &str = "The Rule-SQL expression can't be parsed correctly.";

/// Why a rule's SQL statement cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlRejection {
    /// Parsing stopped at a known place: what the grammar expected there, and the 1-based
    /// line and column of the statement it stopped at.
    Stopped {
        /// The token the grammar expected where parsing stopped.
        expected: String,
        /// The 1-based line of the statement parsing stopped at.
        line: usize,
        /// The 1-based column of that line parsing stopped at.
        column: usize,
    },
    /// The parser refused the statement without reporting a place.
    Unreadable,
}

impl SqlRejection {
    /// What to tell the caller about `sql`: the expectation, a blank line, the statement
    /// as submitted, and a caret line marking the column parsing stopped at with the
    /// 1-based position after it.
    ///
    /// The fixed parts are the expectation line and the ` at <line>:<column>` suffix; the
    /// variable parts are the submitted statement and the place its parse stopped, both
    /// read off this request. A statement the parser placed nowhere carries the modelled
    /// error's own sentence instead ([`UNREADABLE_SQL_MESSAGE`]).
    pub fn message(&self, sql: &str) -> String {
        match self {
            Self::Stopped {
                expected,
                line,
                column,
            } => {
                let caret = " ".repeat(column.saturating_sub(1));
                format!("Expected '{expected}'\n\n{sql}\n{caret}^ at {line}:{column}")
            }
            Self::Unreadable => UNREADABLE_SQL_MESSAGE.to_string(),
        }
    }
}

/// Why the statement `sql` cannot be parsed, or `None` for one the parser reads whole.
///
/// Two readings, in the order the grammar takes them: the statement must OPEN with
/// [`SELECT_KEYWORD`], and the whole of it must then parse as one SQL query. Both are the
/// `sqlparser` crate's — its tokenizer says where the statement's first token is, and its
/// parser reads the rest over the same standard-SQL rewrite the rules engine reads a
/// stored statement with ([`parsed_statement`]) — so a statement a writer accepts is one
/// the engine can read.
///
/// A statement longer than [`super::MAX_SQL_SCAN_CHARACTERS`] is NOT parsed here and so is
/// not refused: that bound is the one every reading of a statement in this service takes,
/// and it is what keeps this pass's work and its one allocation finite whatever a caller
/// sends.
pub fn sql_rejection(sql: &str) -> Option<SqlRejection> {
    let characters: Vec<char> = sql.chars().take(super::MAX_SQL_SCAN_CHARACTERS + 1).collect();
    if characters.len() > super::MAX_SQL_SCAN_CHARACTERS {
        return None;
    }

    // The opening keyword. A statement that opens with any other token — or with no token
    // at all, where the grammar expects the keyword at the end of what was sent — stops
    // there, which is the one expectation the grammar states before it has read anything.
    // The opening keyword, decided on the statement's own TOKENS: the tokenizer the
    // parser itself reads with is what says where a token starts, so whitespace, a line
    // comment and a block comment are passed over by the crate rather than by a pass
    // written here. A statement that opens with any other token — or with no token at
    // all — stops there, which is the one expectation the grammar states before it has
    // read anything.
    let tokens = match Tokenizer::new(&GenericDialect {}, sql).tokenize_with_location() {
        Ok(tokens) => tokens,
        // A statement the tokenizer cannot even split into tokens (an unterminated
        // literal) is placed nowhere by it.
        Err(_) => return Some(SqlRejection::Unreadable),
    };
    let opening = tokens
        .iter()
        .find(|token| !matches!(token.token, Token::Whitespace(_)));
    let opens_with_select = opening.is_some_and(|token| {
        matches!(&token.token, Token::Word(word) if word.keyword == Keyword::SELECT)
    });
    if !opens_with_select {
        // An empty statement carries no token to point at, so the keyword was expected
        // at the start of what was sent.
        let (line, column) = opening.map_or((1, 1), |token| {
            (
                token.span.start.line.max(1) as usize,
                token.span.start.column.max(1) as usize,
            )
        });
        return Some(SqlRejection::Stopped {
            expected: SELECT_KEYWORD.to_string(),
            line,
            column,
        });
    }

    let standard = match super::selected_topic(sql) {
        // A statement whose `FROM` clause names a quoted topic filter is read through the
        // rewrite that makes it standard SQL; one that names none is already standard, so
        // it is parsed exactly as it was sent.
        Some(topic) => match standard_statement(&characters, &topic) {
            Some(standard) => standard,
            None => return Some(SqlRejection::Unreadable),
        },
        None => characters.iter().collect(),
    };
    match Parser::parse_sql(&GenericDialect {}, &standard) {
        Ok(_) => None,
        Err(error) => Some(parser_rejection(&error.to_string())),
    }
}

/// What the parser's own refusal says: the token it expected and the place it stopped at.
///
/// `sqlparser` words a refusal as `Expected: <token>, found: <token> at Line: <line>,
/// Column: <column>`, so the expectation and the position are read out of it rather than
/// restated here; a refusal worded any other way places the failure nowhere
/// ([`SqlRejection::Unreadable`]).
fn parser_rejection(reported: &str) -> SqlRejection {
    let expected = reported
        .split("Expected: ")
        .nth(1)
        .and_then(|rest| rest.split(", found:").next())
        .map(|expected| expected.trim().to_string());
    let position = reported
        .rsplit("at Line: ")
        .next()
        .and_then(|position| position.split_once(", Column: "))
        .and_then(|(line, column)| {
            Some((
                line.trim().parse::<usize>().ok()?,
                column.trim().parse::<usize>().ok()?,
            ))
        });
    match (expected, position) {
        (Some(expected), Some((line, column))) => SqlRejection::Stopped {
            expected,
            line,
            column,
        },
        _ => SqlRejection::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a row expects of the statement it names.
    #[derive(Debug)]
    enum Expected {
        /// The document the statement selects, compared as a DOCUMENT: a projection's members
        /// are a set, and the order a JSON object writes them in carries no meaning.
        Document(Value),
        /// The payload as published, byte for byte.
        WholePayload,
        /// The statement's condition does not hold for the message.
        NotSelected,
        /// A statement this engine does not evaluate, so no action is performed for it.
        Undecided,
    }

    /// What `sql` selects out of `payload` published to `topic`, as a row states it.
    fn selected(sql: &str, topic: &str, payload: &str) -> Selected {
        selected_message(sql, topic, payload.as_bytes())
    }

    /// The topic every row publishes to unless it is ABOUT a deeper one.
    const TOPIC: &str = "sensors/room-1/temp";

    /// The message every row publishes unless it names its own.
    const MESSAGE: &str = r#"{"temperature":20,"humidity":55,"extra":"dropped"}"#;

    #[test]
    fn a_statement_selects_the_document_it_names_out_of_the_message_and_the_topic() {
        // The rows differ along what the select clause names and what the condition decides:
        // the wildcard alone and beside another item, the `topic()` builtin called with no
        // argument and with each segment's
        // number (including one past the topic's last), an alias and the name an unaliased
        // expression takes, arithmetic over integers and arithmetic that is not whole, a path
        // the message does not carry, a condition that holds and one that does not, and a
        // function of the AWS IoT SQL library this engine does not compute.
        let rows: &[(&str, &str, &str, &str, Expected)] = &[
            (
                "the wildcard, which is the message as published",
                "SELECT * FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::WholePayload,
            ),
            (
                "topic() with no argument, which is the whole topic",
                "SELECT topic() AS sourceTopic FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "sourceTopic": TOPIC })),
            ),
            (
                "topic() unaliased, which takes the function's own name",
                "SELECT topic() FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "topic": TOPIC })),
            ),
            (
                "topic(1), the first segment",
                "SELECT topic(1) AS level FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "level": "sensors" })),
            ),
            (
                "topic(2), the segment between the separators",
                "SELECT topic(2) AS level FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "level": "room-1" })),
            ),
            (
                "topic(3), the last segment",
                "SELECT topic(3) AS level FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "level": "temp" })),
            ),
            (
                "topic(4), one past the last segment, which names none",
                "SELECT topic(4) AS level FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({})),
            ),
            (
                "topic() over a one-level topic",
                "SELECT topic() AS sourceTopic, topic(1) AS level FROM 'readings'",
                "readings",
                MESSAGE,
                Expected::Document(
                    ::serde_json::json!({ "sourceTopic": "readings", "level": "readings" }),
                ),
            ),
            (
                "arithmetic over the message's integers, which is itself whole",
                "SELECT temperature * 9 / 5 + 32 AS temperatureF, humidity AS humidityPct, \
                 topic() AS sourceTopic FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({
                    "temperatureF": 68,
                    "humidityPct": 55,
                    "sourceTopic": TOPIC,
                })),
            ),
            (
                "arithmetic over integers whose result is not whole",
                "SELECT temperature / 8 AS eighth FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "eighth": 2.5 })),
            ),
            (
                "an attribute the message does not carry, which is left out",
                "SELECT temperature AS t, pressure AS p FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "t": 20 })),
            ),
            (
                "a condition that holds for the message",
                "SELECT temperature AS t FROM 'sensors/room-1/temp' WHERE temperature > 10",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({ "t": 20 })),
            ),
            (
                "a condition that does not hold for the message",
                "SELECT temperature AS t FROM 'sensors/room-1/temp' WHERE temperature > 100",
                TOPIC,
                MESSAGE,
                Expected::NotSelected,
            ),
            (
                "a condition over topic() that holds",
                "SELECT * FROM 'sensors/room-1/temp' WHERE topic(2) = 'room-1'",
                TOPIC,
                MESSAGE,
                Expected::WholePayload,
            ),
            (
                "a condition over topic() that does not hold",
                "SELECT * FROM 'sensors/room-1/temp' WHERE topic(2) = 'room-2'",
                TOPIC,
                MESSAGE,
                Expected::NotSelected,
            ),
            (
                "the wildcard beside another item, which adds to the message",
                "SELECT *, topic(2) AS level FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Document(::serde_json::json!({
                    "temperature": 20,
                    "humidity": 55,
                    "extra": "dropped",
                    "level": "room-1",
                })),
            ),
            (
                "the wildcard beside another item over a payload that is no document",
                "SELECT *, topic(2) AS level FROM 'sensors/room-1/temp'",
                TOPIC,
                "not a document",
                Expected::Undecided,
            ),
            (
                "a function of the library this engine does not compute",
                "SELECT get_thing_shadow('a') AS shadow FROM 'sensors/room-1/temp'",
                TOPIC,
                MESSAGE,
                Expected::Undecided,
            ),
        ];
        for (case, sql, topic, payload, expected) in rows {
            let actual = selected(sql, topic, payload);
            match (expected, &actual) {
                (Expected::WholePayload, Selected::Message(message)) => assert_eq!(
                    message.as_slice(),
                    payload.as_bytes(),
                    "case {case:?}: expected the payload as published, got {:?}",
                    String::from_utf8_lossy(message)
                ),
                (Expected::Document(document), Selected::Message(message)) => {
                    let read: Value = ::serde_json::from_slice(message).unwrap_or(Value::Null);
                    assert_eq!(
                        &read, document,
                        "case {case:?}: expected {document}, got {read}"
                    );
                }
                (Expected::NotSelected, Selected::NotSelected)
                | (Expected::Undecided, Selected::Undecided) => {}
                _ => panic!("case {case:?}: expected {expected:?}, got {actual:?}"),
            }
        }
    }
}
