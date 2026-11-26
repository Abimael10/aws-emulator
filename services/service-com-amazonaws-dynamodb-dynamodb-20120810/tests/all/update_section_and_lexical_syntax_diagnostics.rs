// SPDX-License-Identifier: CC0-1.0

//! Two diagnostics about the TEXT of an expression rather than about what it states: a
//! section an `UpdateExpression` opens twice, and a CHARACTER the lexer has no token for.
//!
//! RULE 1: an `UpdateExpression` states each of its four sections at most once ("SET",
//! "REMOVE", "ADD" and "DELETE" — `UpdateItem$UpdateExpression`), so an expression that
//! opens one of them a second time is refused naming that section keyword, quoted, and the
//! grammar it is a section of: `The "<SECTION>" section can only be used once in an update
//! expression;` — behind the member that carried the text and inside the
//! `<N> validation error(s) detected: ` envelope the members a WRITE states its intent
//! through carry.
//!
//! RULE 2: a character the grammar has no token for is ONE token the expression should not
//! carry, so it is refused as the syntax error it is, in the window every syntax refusal of
//! this service quotes — the token before the offending one, the offending one itself and
//! the token after it — read from the expression's own text, so the spacing is the
//! request's. Where the offending character shares a whitespace-delimited run with the text
//! before or after it, that text IS the neighbouring token, and the window opens or closes
//! inside the run.
//!
//! The rows differ along what each rule generalises over: WHICH section is opened twice and
//! which section the expression opens first, and WHERE in the text the offending character
//! sits (inside a run, alone between two runs, at the start of the expression, at its end)
//! together with the member and grammar that carried it.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's front
//! doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::probe::refusal;
use crate::table_items::create_active_table;

/// The table every row addresses.
const TABLE: &str = "SectionAndLexicalDiagnostics";

/// The key of the one seeded item.
fn key() -> Value {
    json!({"pk": {"S": "item-1"}})
}

/// The emulator with the table and one item in place, so no row is refused for its table or
/// its key instead of for the rule under test.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "item-1"},
            "s": {"S": "beta"},
            "n": {"N": "1"},
            "ns": {"NS": ["1"]},
        }}),
    )
    .await;
    router
}

/// The clause a refusal about `member` states.
fn clause(member: &str, detail: &str) -> String {
    format!("Invalid {member}: {detail}")
}

/// The same clause inside the envelope the write-intent members state their refusals in.
fn enveloped(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The whole message an update that opens `section` twice earns.
fn stated_twice(section: &str) -> String {
    enveloped(&clause(
        "UpdateExpression",
        &format!("The \"{section}\" section can only be used once in an update expression;"),
    ))
}

/// The whole clause a syntax error states, over the member it was found in.
fn syntax(member: &str, token: &str, near: &str) -> String {
    clause(
        member,
        &format!("Syntax error; token: \"{token}\", near: \"{near}\""),
    )
}

#[tokio::test]
async fn a_section_an_update_opens_twice_is_refused_naming_that_section() {
    let router = seeded().await;

    // One row per section of the grammar, each opened twice with another section's clause
    // between them where the expression has room for one, so the rule is asserted over
    // every section and not over the one the parser happens to read first.
    let rows: Vec<(&str, &str, &str, Value)> = vec![
        (
            "SET opened twice",
            "SET s = :v REMOVE n SET s = :w",
            "SET",
            json!({":v": {"S": "x"}, ":w": {"S": "y"}}),
        ),
        (
            "REMOVE opened twice",
            "REMOVE s SET n = :v REMOVE ns",
            "REMOVE",
            json!({":v": {"N": "2"}}),
        ),
        (
            "ADD opened twice",
            "ADD n :one REMOVE s ADD n :one",
            "ADD",
            json!({":one": {"N": "1"}}),
        ),
        (
            "DELETE opened twice",
            "DELETE ns :members SET s = :v DELETE ns :members",
            "DELETE",
            json!({":members": {"NS": ["1"]}, ":v": {"S": "x"}}),
        ),
    ];

    for (case, expression, section, values) in rows {
        let stated = refusal(
            &router,
            case,
            "UpdateItem",
            json!({"TableName": TABLE, "Key": key(),
                   "UpdateExpression": expression,
                   "ExpressionAttributeValues": values}),
        )
        .await;
        let expected = stated_twice(section);
        assert_eq!(
            stated, expected,
            "case {case:?} ({expression:?}): expected {expected:?}, got {stated:?}"
        );
    }

    // The control: the same four sections, each stated ONCE, are read and applied — so the
    // refusals above are about the second opening and not about the sections.
    let applied = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression": "SET s = :v REMOVE gone ADD n :one DELETE ns :members",
               "ExpressionAttributeValues": {":v": {"S": "x"}, ":one": {"N": "1"},
                                             ":members": {"NS": ["1"]}},
               "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(
        (
            applied["Attributes"]["s"].clone(),
            applied["Attributes"]["n"].clone(),
            applied["Attributes"].get("ns").cloned()
        ),
        (json!({"S": "x"}), json!({"N": "2"}), None),
        "case every section stated once: expected the set, the add and the delete applied, \
         got {applied}"
    );
}

#[tokio::test]
async fn a_character_the_grammar_has_no_token_for_quotes_the_tokens_around_it() {
    let router = seeded().await;

    // The rows place the offending character at every position the window rule
    // distinguishes: alone between two tokens, sharing its run with the token before it, at
    // the very start of the expression, and at its very end — across the member a READ
    // narrows its result with and the member a WRITE states its change through.
    let filters: Vec<(&str, &str, &str, &str)> = vec![
        (
            "a stray character between two tokens",
            "s = :v ; n = :n",
            ";",
            ":v ; n",
        ),
        (
            "a stray character opening the expression",
            "%s = :v",
            "%",
            "%s",
        ),
        (
            "a stray character closing the expression",
            "s = :v ?",
            "?",
            ":v ?",
        ),
    ];

    for (case, expression, token, near) in filters {
        let stated = refusal(
            &router,
            case,
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": expression,
                   "ExpressionAttributeValues": {":v": {"S": "x"}, ":n": {"N": "1"}}}),
        )
        .await;
        let expected = syntax("FilterExpression", token, near);
        assert_eq!(
            stated, expected,
            "case {case:?} ({expression:?}): expected {expected:?}, got {stated:?}"
        );
    }

    // The same window behind the member a write states its change through, where the
    // offending character ends the run the token before it sits in: that text is the
    // previous token, so the window opens inside the run and closes at the end of the next
    // one.
    let stated = refusal(
        &router,
        "a stray character ending a run of an update",
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression": "SET s = :v REMOVE n; REMOVE ns",
               "ExpressionAttributeValues": {":v": {"S": "x"}}}),
    )
    .await;
    let expected = enveloped(&syntax("UpdateExpression", ";", "n; REMOVE"));
    assert_eq!(
        stated, expected,
        "case a stray character ending a run of an update: expected {expected:?}, got \
         {stated:?}"
    );
}
