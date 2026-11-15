// SPDX-License-Identifier: CC0-1.0

//! Where the parse of a query-form statement stops when a VALUE OPERATOR carries its
//! expression on, end to end over `ExecuteStatement`.
//!
//! RULE under test: text this surface is handed that opens with none of the four
//! data-manipulation verbs it serves is read as the statement language's other form — one
//! query, which is one expression — and the refusal is the fixed sentence `Statement wasn't
//! well formed, can't be processed: ` over the parser's phrase for the position the parse
//! stopped at. An expression CARRIES ON across the language's binary value operators, so the
//! text has completed no expression AT such an operator however much of it read before: what
//! decides is the operand after it. Text whose continued expression reads through to its end
//! is one expression and manipulates no data whatever it spells (`Expected data
//! manipulation`), text whose operand after the operator does not read completed no
//! expression at all and states no data manipulation either, and only text that completed one
//! expression and then carries on with a token no operator introduced stopped at that token
//! (`Unexpected token after expression`).
//!
//! The rows differ along what the rule generalises over: WHICH operator carries the
//! expression on (`*`, `+`, `-`), WHAT stands where its operand belongs (a name, a number, a
//! quoted name, a string literal, a keyword of the dialect, the end of the text, another
//! operator) and WHAT follows the expression once it is complete (nothing, a `;`, a word, a
//! literal). The recorded case — a misspelled `SELECT`, whose `*` is followed by the `FROM`
//! keyword — is one row of the first kind.
//!
//! The control at the end reads the store back through `GetItem` and `Scan`: a refused
//! statement writes nothing, so the seeded item stands as it was and nothing a row named was
//! added.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row names. A bare name of the dialect, so a row can carry it where a
/// value operator's operand belongs.
const TABLE: &str = "OperatorContinuation";

/// The parser's phrase for text that completed no expression, or exactly one, and so
/// manipulates no data.
const NO_MANIPULATION: &str = "Expected data manipulation";

/// The parser's phrase for text a token is left over past the expression it read in.
const TRAILING_TOKEN: &str = "Unexpected token after expression";

/// The whole sentence a parse defect is stated with.
fn parse_failure(phrase: &str) -> String {
    format!("Statement wasn't well formed, can't be processed: {phrase}")
}

/// An emulator holding one active table with one item in it, so a refusal can be seen to have
/// left the store alone.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "n": {"N": "1"}}}),
    )
    .await;
    router
}

/// One row: what it is a case of, the statement text, and the phrase its parse earns.
fn rows() -> Vec<(&'static str, String, &'static str)> {
    vec![
        // The operand after the operator is a KEYWORD of the dialect, which no name may be
        // spelled as — so the expression never completed. The recorded case is the first.
        (
            "a misspelled SELECT: `*` carried on, and FROM stands where its operand belongs",
            format!("SELCT * FROM \"{TABLE}\""),
            NO_MANIPULATION,
        ),
        (
            "`+` carried on, and a keyword stands where its operand belongs",
            format!("{TABLE} + FROM"),
            NO_MANIPULATION,
        ),
        (
            "`-` carried on, and a keyword stands where its operand belongs",
            format!("{TABLE} - WHERE"),
            NO_MANIPULATION,
        ),
        // The operator carries on into the END of the text: there is no operand at all.
        (
            "`*` with nothing after it",
            format!("{TABLE} *"),
            NO_MANIPULATION,
        ),
        (
            "`+` with nothing after it",
            format!("{TABLE} +"),
            NO_MANIPULATION,
        ),
        // The operator carries on into ANOTHER operator, which is no operand either.
        (
            "`*` followed by `*`",
            format!("{TABLE} * *"),
            NO_MANIPULATION,
        ),
        // The continued expression DOES read through to its end: one expression, which
        // manipulates no data whatever it spells.
        (
            "a name, `*`, and a name",
            format!("{TABLE} * rows"),
            NO_MANIPULATION,
        ),
        (
            "a name, `+`, and a number",
            format!("{TABLE} + 1"),
            NO_MANIPULATION,
        ),
        (
            "a number, `-`, and a number",
            "7 - 1".to_string(),
            NO_MANIPULATION,
        ),
        (
            "a quoted name, `*`, and a string literal",
            format!("\"{TABLE}\" * 'a'"),
            NO_MANIPULATION,
        ),
        (
            "three operands joined by two operators",
            format!("{TABLE} + 1 * 2"),
            NO_MANIPULATION,
        ),
        (
            "a continued expression the `;` that ends a query terminates",
            format!("{TABLE} + 1;"),
            NO_MANIPULATION,
        ),
        // The continued expression completed, and the text carries ON past it with a token
        // no operator introduced: that token is where the parse stopped.
        (
            "a continued expression with a word left over",
            format!("{TABLE} * rows junk"),
            TRAILING_TOKEN,
        ),
        (
            "a continued expression with a literal left over",
            format!("{TABLE} + 1 'a'"),
            TRAILING_TOKEN,
        ),
        (
            "a continued expression with a quoted name left over",
            format!("{TABLE} - 1 \"{TABLE}\""),
            TRAILING_TOKEN,
        ),
        // No operator at all: the expression completed at the first term, so the token after
        // it is where the parse stopped — the class the rule above must not swallow.
        (
            "a name with a clause word after it, no operator between them",
            format!("INSRT INTO \"{TABLE}\" VALUE {{'pk':'z'}}"),
            TRAILING_TOKEN,
        ),
    ]
}

/// RULE: the phrase the refusal carries is the position the parse of the continued expression
/// stopped at, and the statement writes nothing.
#[tokio::test]
async fn a_value_operator_carries_the_query_expression_on() {
    let router = seeded().await;

    for (case, statement, phrase) in rows() {
        let (status, answered) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": &statement}),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", parse_failure(phrase).as_str()),
            "case {case:?}: {statement:?} expected {phrase:?}, got {status} with {answered}"
        );
    }

    // Nothing any of those statements named was written: the seeded item stands as it was,
    // and the store still holds exactly it.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}}}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"pk": {"S": "a"}, "n": {"N": "1"}}),
        "case a refused statement leaves the store alone: got {read}"
    );
    let scanned: Value = ok(&router, "Scan", json!({"TableName": TABLE})).await;
    assert_eq!(
        scanned["Count"],
        json!(1),
        "case a refused statement writes nothing: got {scanned}"
    );
}

/// The defect belongs to the STATEMENT, not to the request that carried it: a batch reports
/// the same phrase in the slot that sent the statement, beside a sibling that still runs, and
/// a transaction refuses the whole call behind its zero-based slot prefix.
#[tokio::test]
async fn every_surface_states_the_same_phrase_for_a_continued_expression() {
    let router = seeded().await;
    let well_formed = format!("INSERT INTO \"{TABLE}\" VALUE {{'pk':'sibling'}}");

    for (case, statement, phrase) in rows() {
        let (status, batched) = call(
            &router,
            "BatchExecuteStatement",
            json!({"Statements": [
                {"Statement": &statement},
                {"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'a'")},
            ]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a batch carrying it answers 200, got {status} with {batched}"
        );
        assert_eq!(
            (
                batched["Responses"][0]["Error"]["Code"].clone(),
                batched["Responses"][0]["Error"]["Message"].clone(),
            ),
            (json!("ValidationError"), json!(parse_failure(phrase))),
            "case {case:?}: the slot states the statement's own phrase, got {batched}"
        );
        assert_eq!(
            batched["Responses"][1]["Item"]["n"],
            json!({"N": "1"}),
            "case {case:?}: the slot beside it is served, got {batched}"
        );

        let (status, refused) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [
                {"Statement": &well_formed},
                {"Statement": &statement},
            ]}),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                message(&refused).as_str()
            ),
            (
                400,
                "ValidationException",
                format!(
                    "Validation failed in TransactStatements[1]: {}",
                    parse_failure(phrase)
                )
                .as_str()
            ),
            "case {case:?}: the transaction states the slot and the phrase, got {status} with \
             {refused}"
        );
        let sibling = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "sibling"}}}),
        )
        .await;
        assert!(
            sibling.get("Item").is_none(),
            "case {case:?}: a refused transaction writes none of its statements, got {sibling}"
        );
    }
}
