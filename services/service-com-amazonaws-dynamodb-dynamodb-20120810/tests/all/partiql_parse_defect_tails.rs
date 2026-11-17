// SPDX-License-Identifier: CC0-1.0

//! What TAIL a PartiQL parse refusal carries, for the two defect classes whose tail is
//! decided before any grammar position can be named.
//!
//! One rule per test, asserted over a table of statements rather than one recorded
//! request:
//!
//! * a run of text the SCANNER cannot read as a lexeme — a number whose exponent marker
//!   carries no exponent, a second decimal point, a number run together with a word, a
//!   quoted run the text never closes — never became a token, so there is no grammar
//!   position to name and the refusal states the fixed sentence with an EMPTY tail;
//! * an `UPDATE` that states neither `SET` nor `REMOVE` manipulates no data, so it is
//!   refused as the parse defect text stating no manipulation of data earns, with that
//!   defect's own phrase — not with a rule of its own and not under another prefix.
//!
//! Both classes are contrasted with the token-expectation class, whose tail NAMES the
//! token the grammar required, so a client can tell the three apart; and each refusal is
//! checked to have written nothing, with the well-formed statement of the same shape
//! served and read back, so a row is the defect being reported rather than the surface
//! refusing that shape of statement.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every statement below addresses.
const TABLE: &str = "StatementTails";

/// The fixed sentence a parse defect is stated with, over the parser's phrase for the
/// position the parse stopped at — EMPTY where the defect reaches no such position.
fn parse_failure(phrase: &str) -> String {
    format!("Statement wasn't well formed, can't be processed: {phrase}")
}

/// An emulator holding one active table with one item in it.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the statement-tails table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": "seed"}, "n": {"N": "1"}, "note": {"S": "kept"}},
        }),
    )
    .await;
    router
}

/// The item the table holds under `key`, or `Value::Null` when it holds none.
async fn stored(router: &MultiServiceRouter, key: &str) -> Value {
    let read = ok(
        router,
        "GetItem",
        json!({
            "TableName": TABLE,
            "Key": {"pk": {"S": key}},
            "ConsistentRead": true,
        }),
    )
    .await;
    read.get("Item").cloned().unwrap_or(Value::Null)
}

/// Run one statement that must be refused and report the sentence it was refused with,
/// having checked the status and the code every PartiQL refusal carries.
async fn refused(router: &MultiServiceRouter, case: &str, statement: &str) -> String {
    let (status, answered) = call(router, "ExecuteStatement", json!({"Statement": statement})).await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "ValidationException"),
        "case {case:?} ({statement:?}): expected a 400 ValidationException, got {status} with \
         {answered}"
    );
    message(&answered)
}

#[tokio::test]
async fn a_lexeme_the_scanner_cannot_read_is_refused_with_an_empty_tail() {
    let router = seeded().await;

    // The rows differ in the KIND of unreadable lexeme (an exponent marker with no
    // exponent, in either case and with either sign; a second decimal point; a number run
    // together with a word; a quoted run left open) and in WHERE the statement spells it
    // (an INSERT's value, a projection, a predicate, a SET clause, inside a list, inside a
    // bag, in the source path itself) — the tail is empty for every one of them, because
    // no token was ever made.
    for (case, statement) in [
        (
            "an exponent marker with no exponent, in an INSERT value",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 'n': 1e}}"),
        ),
        (
            "an uppercase exponent marker with no exponent, in a predicate",
            format!("SELECT * FROM \"{TABLE}\" WHERE n = 2E"),
        ),
        (
            "a positively signed exponent marker with no exponent, in a SET clause",
            format!("UPDATE \"{TABLE}\" SET n = 3e+ WHERE pk = 'seed'"),
        ),
        (
            "a negatively signed exponent marker with no exponent, in a DELETE predicate",
            format!("DELETE FROM \"{TABLE}\" WHERE pk = 'seed' AND n = 4e-"),
        ),
        (
            "a second decimal point, in an INSERT value",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 'n': 1.2.3}}"),
        ),
        (
            "a second decimal point, inside a list literal",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 'l': [1.2.3]}}"),
        ),
        (
            "a number run together with a word, inside a bag literal",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 's': <<5abc>>}}"),
        ),
        (
            "a number run together with an underscore word, in a projection",
            format!("SELECT 6_7 FROM \"{TABLE}\""),
        ),
        (
            "a string literal the statement never closes",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'open"),
        ),
        (
            "a quoted name the statement never closes",
            format!("SELECT * FROM \"{TABLE}"),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered,
            parse_failure(""),
            "case {case:?} ({statement:?}): a lexeme that never became a token names no \
             position, so the tail must be empty, got {answered:?}"
        );
    }

    // The contrast: a literal the scanner READ and the grammar then found unclosed stops
    // at a token position, so that class names the token it required. The two classes are
    // therefore distinguishable, which is what the empty tail above is for.
    for (case, statement, phrase) in [
        (
            "a tuple literal the statement never closes",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 'n': 1"),
            "Expected RIGHT_CURLY",
        ),
        (
            "a list literal the statement never closes",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 'l': [1, 2}}"),
            "Expected RIGHT_BRACKET",
        ),
        (
            "a bag literal the statement never closes",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 's': <<1, 2}}"),
            "Expected RIGHT_DOUBLE_ANGLE_BRACKET",
        ),
        (
            "a clause that stops before its predicate",
            format!("SELECT * FROM \"{TABLE}\" WHERE"),
            "Unexpected term",
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered,
            parse_failure(phrase),
            "case {case:?} ({statement:?}): expected the token the grammar required in the \
             tail, got {answered:?}"
        );
    }

    // Nothing above wrote: the key every refused INSERT named holds no item.
    let absent = stored(&router, "py").await;
    assert_eq!(
        absent,
        Value::Null,
        "case a refused statement writes nothing: the key the refused INSERTs named must \
         hold no item, got {absent}"
    );

    // The same INSERT with a lexeme the scanner CAN read is served and readable back, so
    // the rows above report the lexeme rather than refuse that shape of statement.
    let (status, answered) = call(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 'n': 1e2}}")}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a well-formed exponent: expected HTTP 200, got {status} with {answered}"
    );
    let written = stored(&router, "py").await;
    assert_eq!(
        written,
        json!({"pk": {"S": "py"}, "n": {"N": "100"}}),
        "case a well-formed exponent: the item must be readable back, got {written}"
    );
}

#[tokio::test]
async fn an_update_stating_no_action_is_refused_as_a_parse_defect_and_changes_nothing() {
    let router = seeded().await;

    // The rows differ in what the UPDATE states AROUND its missing action — nothing, a
    // predicate on the key, a predicate on another attribute, a RETURNING clause, an alias
    // — and in how the target and the verb are spelled (quoted, bare, lower case): every
    // one of them manipulates no data, so every one earns that defect's phrase under the
    // same prefix as the parser's other refusals.
    for (case, statement) in [
        (
            "a quoted target and a key predicate",
            format!("UPDATE \"{TABLE}\" WHERE pk = 'seed'"),
        ),
        (
            "nothing at all after the target",
            format!("UPDATE \"{TABLE}\""),
        ),
        (
            "a bare target",
            format!("UPDATE {TABLE} WHERE pk = 'seed'"),
        ),
        (
            "a lower-case verb and clause word",
            format!("update \"{TABLE}\" where pk = 'seed'"),
        ),
        (
            "a predicate on a non-key attribute",
            format!("UPDATE \"{TABLE}\" WHERE n = 1"),
        ),
        (
            "a RETURNING clause after the predicate",
            format!("UPDATE \"{TABLE}\" WHERE pk = 'seed' RETURNING ALL NEW *"),
        ),
        (
            "an alias bound to the target",
            format!("UPDATE \"{TABLE}\" AS t WHERE t.pk = 'seed'"),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered,
            parse_failure("Expected data manipulation"),
            "case {case:?} ({statement:?}): an UPDATE stating no action manipulates no data, \
             got {answered:?}"
        );
    }

    // The rejected statements left the item exactly as it was.
    let unchanged = stored(&router, "seed").await;
    assert_eq!(
        unchanged,
        json!({"pk": {"S": "seed"}, "n": {"N": "1"}, "note": {"S": "kept"}}),
        "case a refused UPDATE changes nothing: the seeded item must be untouched, got \
         {unchanged}"
    );

    // Either action makes the same statement run, so the refusals above are the missing
    // action and not the verb: each one is read back through GetItem.
    for (case, statement, expected) in [
        (
            "an UPDATE stating SET",
            format!("UPDATE \"{TABLE}\" SET n = 2 WHERE pk = 'seed'"),
            json!({"pk": {"S": "seed"}, "n": {"N": "2"}, "note": {"S": "kept"}}),
        ),
        (
            "an UPDATE stating REMOVE",
            format!("UPDATE \"{TABLE}\" REMOVE note WHERE pk = 'seed'"),
            json!({"pk": {"S": "seed"}, "n": {"N": "2"}}),
        ),
    ] {
        let (status, answered) =
            call(&router, "ExecuteStatement", json!({"Statement": &statement})).await;
        assert_eq!(
            status, 200,
            "case {case:?} ({statement:?}): expected HTTP 200, got {status} with {answered}"
        );
        let read_back = stored(&router, "seed").await;
        assert_eq!(
            read_back, expected,
            "case {case:?} ({statement:?}): the item must carry the action's effect, got \
             {read_back}"
        );
    }
}

#[tokio::test]
async fn the_batch_surface_reports_the_same_two_tails_per_slot() {
    let router = seeded().await;

    // A batch answers 200 and reports each statement in its own slot, so the same two
    // defects are asserted there: the slot is the only part of the refusal that differs
    // between the surfaces, and the sentence is the parser's either way.
    let batched = ok(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": [
            {"Statement": format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'py', 'n': 1e}}")},
            {"Statement": format!("UPDATE \"{TABLE}\" WHERE pk = 'seed'")},
            {"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'seed'")},
        ]}),
    )
    .await;
    for (slot, phrase) in [(0usize, ""), (1usize, "Expected data manipulation")] {
        assert_eq!(
            (
                batched["Responses"][slot]["Error"]["Code"].clone(),
                batched["Responses"][slot]["Error"]["Message"].clone(),
            ),
            (
                json!("ValidationError"),
                json!(parse_failure(phrase)),
            ),
            "case the batch slot {slot}: expected the parser's own sentence, got {batched}"
        );
    }
    assert_eq!(
        batched["Responses"][2]["Item"]["n"],
        json!({"N": "1"}),
        "case the batch slot beside the refused ones: it must be served, got {batched}"
    );

    // Neither refused slot wrote: the INSERT's key holds no item and the seeded item is as
    // it was.
    let absent = stored(&router, "py").await;
    assert_eq!(
        absent,
        Value::Null,
        "case a refused batch slot writes nothing: the INSERT's key must hold no item, got \
         {absent}"
    );
    let unchanged = stored(&router, "seed").await;
    assert_eq!(
        unchanged,
        json!({"pk": {"S": "seed"}, "n": {"N": "1"}, "note": {"S": "kept"}}),
        "case a refused batch slot writes nothing: the seeded item must be untouched, got \
         {unchanged}"
    );
}
