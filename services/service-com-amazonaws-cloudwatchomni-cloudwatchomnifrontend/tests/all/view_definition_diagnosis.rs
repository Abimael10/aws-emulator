// SPDX-License-Identifier: CC0-1.0

//! What a refused view `definition` is told about itself.
//!
//! A definition that is not one SQL query is refused with the modelled
//! `ValidationException` (HTTP 400), and what the message says is decided by the CLASS of
//! the defect:
//!
//! * text no SQL parser can read as a statement at all earns the parser's own reading of
//!   the submitted text under one fixed prefix: `Invalid view definition: ` followed
//!   verbatim by the parser's error, itself opening `sql parser error: ` and then naming
//!   what the parser expected, the token it stopped on, and the 1-based line and column
//!   that token sits at. That diagnosis is a function of the SUBMITTED TEXT, so the rows
//!   below move one defect around the definition and must see it reported where it
//!   stands;
//! * text read as exactly one statement that is not a query earns the RULE it broke, as
//!   one fixed sentence with no variable part and nothing of the submitted text in it;
//! * text read as any other number of statements earns the other rule it broke, also as
//!   one fixed sentence.
//!
//! The refusal that travels over the rpcv2Cbor wire is the same message in each class, so
//! a real client reads it in the error envelope.
//!
//! The harness is `view_lifecycle`'s: one shared engine behind the real handlers.

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_view::CreateViewErrorModeled;
use omni::ops::types::CREATE_VIEW_INPUT_SCHEMA;
use omni::scaffolded::Scaffolded;
use omni::state::{
    view_definition_rejection, INVALID_VIEW_DEFINITION, VIEW_DEFINITION_MUST_BE_A_SELECT_QUERY,
    VIEW_DEFINITION_MUST_BE_ONE_STATEMENT,
};

use super::validation_message_wording::error_envelope;
use super::view_lifecycle::{create_input, view_name, Omni};

/// The fixed text the parser's own error rendering opens with, so a refusal message is
/// the service's prefix followed by the parser's error verbatim.
const PARSER_ERROR: &str = "sql parser error: ";

/// The expectation the parser reports for text that carries no statement, as the
/// recorded refusal of a definition that is not SQL words it.
const EXPECTED_A_STATEMENT: &str = "Expected: an SQL statement, found: ";

/// Text a refusal that reported the judging component's own reading would carry: the
/// component's name, and the way it points at a place in the submitted text. A refusal
/// that states a RULE carries none of it.
const AN_INTERNAL_DIAGNOSIS: &[&str] = &["parser", "Expected", "found:", "Line:", "Column:"];

/// A definition whose only defect is a brace where a statement has to start, placed at
/// `line` and `column` BY CONSTRUCTION: on the first line the brace is all there is, and
/// on any later line it follows a complete statement, so the parser stops on the brace
/// rather than on anything before it.
fn brace_at(line: usize, column: usize) -> String {
    let indent = " ".repeat(column - 1);
    if line == 1 {
        format!("{indent}{{}}")
    } else {
        format!("SELECT 1;{}{indent}{{}}", "\n".repeat(line - 1))
    }
}

/// The message a definition whose brace sits at `line`/`column` must be refused with:
/// the service's fixed prefix, the parser's fixed prefix, the parser's expectation and
/// the offending token, then the position that token was BUILT at.
fn expected_brace_message(line: usize, column: usize) -> String {
    format!("{INVALID_VIEW_DEFINITION}{PARSER_ERROR}{EXPECTED_A_STATEMENT}{{ at Line: {line}, Column: {column}")
}

/// The message the operation's modelled `ValidationException` carries for `definition`.
async fn refusal_message(omni: &Omni, case: &str, name: &str, definition: &str) -> String {
    match omni.create_view(&create_input(name, definition)).await {
        Ok(output) => panic!("case {case:?}: expected a ValidationException, got Ok({output:?})"),
        Err(Scaffolded::Modeled(CreateViewErrorModeled::ValidationException(error))) => {
            error.message
        }
        Err(other) => {
            panic!("case {case:?}: expected the modelled ValidationException, got {other:?}")
        }
    }
}

/// Require that `definition` earns exactly `expected`, through the rule and through the
/// operation, and that the message states no part of the submitted text.
///
/// `submitted` is text of the definition the message must not quote back: the token the
/// rejected statement opens with, or the whole definition where it is short enough to be
/// rendered in full.
async fn states_the_rule(
    omni: &Omni,
    case: &str,
    name: &str,
    definition: &str,
    submitted: &str,
    expected: &str,
) {
    let rejection = view_definition_rejection(definition);
    assert_eq!(
        rejection.as_deref(),
        Some(expected),
        "case {case:?}: the definition rule must state {expected:?}, got {rejection:?}"
    );
    let message = refusal_message(omni, case, name, definition).await;
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
    assert!(
        !message.contains(submitted),
        "case {case:?}: the message quotes the submitted text ({submitted:?}), got {message:?}"
    );
    for detail in AN_INTERNAL_DIAGNOSIS {
        assert!(
            !message.contains(detail),
            "case {case:?}: the message carries the internal diagnosis {detail:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn text_that_is_not_sql_is_told_the_parser_diagnosis_where_the_defect_stands() {
    let omni = Omni::new();
    // The first row is the recorded case: the smallest definition that is not SQL at
    // all, whose defect sits at line 1, column 1. The rest move the SAME defect down and
    // across the text, so what varies between rows is only where the parser stops.
    let positions = [(1usize, 1usize), (1, 5), (2, 1), (3, 4), (6, 12)];
    let mut seen: Vec<String> = Vec::new();
    for (line, column) in positions {
        let case = format!("a brace at line {line}, column {column}");
        let definition = brace_at(line, column);
        let expected = expected_brace_message(line, column);

        let rejection = view_definition_rejection(&definition);
        assert_eq!(
            rejection.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: the definition rule must report the parser's diagnosis, got {rejection:?}"
        );

        let name = view_name(&format!("brace-{line}-{column}"));
        let message = refusal_message(&omni, &case, &name, &definition).await;
        assert_eq!(
            message, expected,
            "case {case:?}: CreateView must refuse with the parser's diagnosis, got {message:?}"
        );
        seen.push(message);
    }
    // One defect at five places is five diagnoses: the position is carried, not dropped.
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        positions.len(),
        "the same defect at {} places must earn {} distinct diagnoses, got {seen:?}",
        positions.len(),
        positions.len()
    );
}

/// Definitions the parser reads as exactly ONE statement that is not a query, each with
/// the token that statement opens with.
///
/// The rows span the statement kinds a definition can be written as instead of a query:
/// the three mutations, a definition, a drop and a transaction control.
const ONE_STATEMENT_THAT_IS_NOT_A_QUERY: &[(&str, &str, &str)] = &[
    (
        "a delete",
        "DELETE FROM logs WHERE level = 'ERROR'",
        "DELETE",
    ),
    ("an insert", "INSERT INTO logs (id) VALUES (1)", "INSERT"),
    ("an update", "UPDATE logs SET level = 'INFO'", "UPDATE"),
    ("a DDL statement", "CREATE TABLE logs (id INT)", "CREATE"),
    ("a dropped table", "DROP TABLE logs", "DROP"),
    ("a transaction control", "COMMIT", "COMMIT"),
];

#[tokio::test]
async fn one_statement_that_is_not_a_query_is_told_the_rule_it_broke() {
    let omni = Omni::new();
    for (index, (case, definition, submitted)) in
        ONE_STATEMENT_THAT_IS_NOT_A_QUERY.iter().enumerate()
    {
        states_the_rule(
            &omni,
            case,
            &view_name(&format!("not-a-query-{index}")),
            definition,
            submitted,
            VIEW_DEFINITION_MUST_BE_A_SELECT_QUERY,
        )
        .await;
    }
}

/// Definitions the parser reads as any number of statements other than one, each with
/// text of its own the message must not quote back.
///
/// The rows span both sides of "exactly one": more than one statement — queries, a query
/// followed by a mutation, three of them — and text that carries none at all.
const NOT_EXACTLY_ONE_STATEMENT: &[(&str, &str, &str)] = &[
    ("two queries", "SELECT 1; SELECT 2", "SELECT 2"),
    (
        "a query followed by a mutation",
        "SELECT * FROM logs; DROP TABLE logs",
        "DROP",
    ),
    ("three queries", "SELECT 1; SELECT 2; SELECT 3", "SELECT 3"),
    ("whitespace only", "   ", "   "),
    ("a comment only", "-- nothing but a comment", "nothing"),
];

#[tokio::test]
async fn text_that_is_not_exactly_one_statement_is_told_the_rule_it_broke() {
    let omni = Omni::new();
    for (index, (case, definition, submitted)) in NOT_EXACTLY_ONE_STATEMENT.iter().enumerate() {
        states_the_rule(
            &omni,
            case,
            &view_name(&format!("not-one-statement-{index}")),
            definition,
            submitted,
            VIEW_DEFINITION_MUST_BE_ONE_STATEMENT,
        )
        .await;
    }
}

#[tokio::test]
async fn each_class_of_refusal_travels_over_the_rpcv2cbor_wire() {
    let omni = Omni::new();
    for (case, name, definition, expected) in [
        (
            "one statement that is not a query",
            "wire-dropped-table",
            "DROP TABLE logs",
            VIEW_DEFINITION_MUST_BE_A_SELECT_QUERY.to_string(),
        ),
        (
            "more than one statement",
            "wire-two-queries",
            "SELECT 1; SELECT 2",
            VIEW_DEFINITION_MUST_BE_ONE_STATEMENT.to_string(),
        ),
        (
            "text that is not SQL",
            "wire-not-sql",
            "{}",
            expected_brace_message(1, 1),
        ),
    ] {
        let (status, body) = omni
            .over_the_wire(
                "CreateView",
                CREATE_VIEW_INPUT_SCHEMA,
                &create_input(&view_name(name), definition),
            )
            .await;
        let (code, message) = error_envelope(case, &body);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "ValidationException", expected.as_str()),
            "case {case:?}: the wire must answer 400 ValidationException with {expected:?}, got \
             {status} {code:?}: {message:?}"
        );
    }
}

#[tokio::test]
async fn a_definition_that_is_one_query_earns_no_refusal() {
    // The rule refuses only what is not one query, so a query stays admitted whatever
    // form it takes — including the forms the rows above are built out of, so the tables
    // are testing the defect and not the shape of the text.
    for (case, definition) in [
        ("a bare projection", "SELECT 1"),
        ("a trailing semicolon", "SELECT 1;"),
        ("a parenthesised query", "(SELECT 1)"),
        ("a query over several lines", "SELECT id\nFROM logs\nWHERE id > 1"),
        (
            "a leading comment",
            "-- the day's errors\nSELECT count(*) FROM logs",
        ),
        ("a set operation", "SELECT 1 UNION SELECT 2"),
        (
            "a common table expression",
            "WITH errors AS (SELECT * FROM logs) SELECT count(*) FROM errors",
        ),
    ] {
        let rejection = view_definition_rejection(definition);
        assert_eq!(
            rejection, None,
            "case {case:?}: one SQL query must be admitted, got {rejection:?}"
        );
    }
}
