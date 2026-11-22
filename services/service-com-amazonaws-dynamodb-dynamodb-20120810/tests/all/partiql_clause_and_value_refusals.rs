// SPDX-License-Identifier: CC0-1.0

//! The clauses and the operands a statement may state where it has already read a complete
//! predicate or a complete statement, and what it is told when it states one this surface
//! does not serve.
//!
//! RULES under test, each one level above the case that found it:
//!
//! 1. A predicate IS a boolean value of the language, so a clause may state one by comparing
//!    it with another boolean (`= TRUE`, `<> FALSE`) rather than on its own: the comparison
//!    says WHICH outcome the clause selects and nothing else, so the items selected are the
//!    predicate's own, or the ones it does not select where the comparison asks for that.
//! 2. A clause the language states that this surface does not serve is refused as the CLAUSE
//!    it is — the keyword in the language's own spelling, and where the statement spells the
//!    expression that clause carries — rather than as a parse defect of the statement it
//!    follows, and the statement it is attached to writes nothing.
//! 3. `MISSING` is an attribute the item does not CARRY rather than a value one can hold, so
//!    the only operator that reads it is the type test: every comparison against it is
//!    refused with AWS's fixed sentence, whichever operator and whichever side the statement
//!    writes it on — while the same keyword standing where a stored VALUE belongs compares
//!    nothing and is reported as the keyword it is. `IS MISSING` and `IS NOT MISSING` select
//!    items as before.
//! 4. A value position takes a literal, a collection of literals or a `?` marker, so an
//!    unquoted word there denotes no value an item can hold: the refusal names the token, the
//!    document that holds it (`root` for the statement's own value, the attribute a nested
//!    document — or the one a clause assigns — is held under) and where the statement spells
//!    the token, while a word that spells one of the dialect's KEYWORDS is reported as the
//!    keyword it is.
//!
//! The rows differ along what each rule generalises over: which predicate the comparison is
//! stated over and which of the four boolean comparisons states it; which clause follows the
//! statement, which verb that statement states, how the clause is cased and which line it
//! stands on; which operator a `MISSING` comparison uses and which side it stands on; and how
//! deeply the document holding an unquoted token is nested.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row below addresses.
const TABLE: &str = "PartiqlClauseRefusals";

/// The fixed sentence a comparison against `MISSING` earns.
const MISSING_COMPARISON: &str = "MISSING must be compared using IS operator";

/// The parser's phrase for a bare word that spells one of the dialect's keywords standing
/// where the grammar has no place for one.
const UNEXPECTED_KEYWORD: &str =
    "Statement wasn't well formed, can't be processed: Unexpected keyword";

/// One emulator holding one active table and the two items every row reads: one carrying an
/// attribute the other does not, so a predicate that selects by presence can be seen to
/// select one of them.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "alpha"}, "s": {"S": "abcdef"}, "n": {"N": "6"}, "extra": {"S": "here"}
        }}),
    )
    .await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "beta"}, "s": {"S": "zzz"}, "n": {"N": "3"}
        }}),
    )
    .await;
    router
}

/// Run one statement, answering its HTTP status and body.
async fn execute(router: &MultiServiceRouter, statement: &str) -> (u16, Value) {
    call(router, "ExecuteStatement", json!({"Statement": statement})).await
}

/// The partition keys a `SELECT pk` reported, in one order so two readings compare.
fn selected_keys(answered: &Value) -> Vec<String> {
    let mut keys: Vec<String> = answered["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| item["pk"]["S"].as_str().unwrap_or_default().to_string())
        .collect();
    keys.sort();
    keys
}

/// The item stored under `key`, or `Value::Null` when the store holds none.
async fn stored(router: &MultiServiceRouter, key: &str) -> Value {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
    )
    .await;
    read.get("Item").cloned().unwrap_or(Value::Null)
}

/// Where a statement spells `token`, as a positioned refusal states it: the line, the column
/// counted in characters from the start of that line, and the token's own width.
///
/// Read off the text the row built rather than written down, so the expectation is a fact of
/// the statement under test. Every row names a token its statement spells exactly once.
fn position_of(statement: &str, token: &str) -> String {
    let at = statement
        .find(token)
        .unwrap_or_else(|| panic!("the statement {statement:?} spells the token {token:?}"));
    let before = &statement[..at];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or_default().chars().count() + 1;
    format!("{line}:{column}:{}", token.chars().count())
}

/// RULE 1: a predicate compared with a boolean selects what the comparison says it does.
#[tokio::test]
async fn a_predicate_compared_with_a_boolean_selects_what_it_states() {
    let router = seeded().await;

    // One row per predicate form the grammar reads, over each of the four comparisons a
    // boolean admits; the bare predicate is the control each `= TRUE` row is read against.
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        ("a prefix test, bare", "begins_with(s, 'abc')", vec!["alpha"]),
        (
            "a prefix test equal to TRUE",
            "begins_with(s, 'abc') = true",
            vec!["alpha"],
        ),
        (
            "a prefix test equal to FALSE",
            "begins_with(s, 'abc') = false",
            vec!["beta"],
        ),
        (
            "a prefix test unequal to TRUE",
            "begins_with(s, 'abc') <> true",
            vec!["beta"],
        ),
        (
            "a prefix test unequal to FALSE, written with the other operator",
            "begins_with(s, 'abc') != false",
            vec!["alpha"],
        ),
        (
            "a substring test equal to FALSE, which no item satisfies",
            "contains(s, 'qqq') = false",
            vec!["alpha", "beta"],
        ),
        (
            "an existence test equal to TRUE",
            "attribute_exists(extra) = true",
            vec!["alpha"],
        ),
        (
            "an absence test equal to TRUE, the literal in upper case",
            "attribute_not_exists(extra) = TRUE",
            vec!["beta"],
        ),
        (
            "a type test equal to TRUE",
            "attribute_type(n, 'N') = true",
            vec!["alpha", "beta"],
        ),
        (
            "the language's own type test equal to FALSE",
            "s IS STRING = false",
            vec![],
        ),
        (
            "a parenthesised comparison equal to TRUE",
            "(pk = 'alpha') = true",
            vec!["alpha"],
        ),
        (
            "a size comparison equal to TRUE",
            "size(s) = 3 = true",
            vec!["beta"],
        ),
        (
            "a negated predicate compared with TRUE",
            "NOT begins_with(s, 'abc') = true",
            vec!["beta"],
        ),
        (
            "a conjunction whose second branch is compared with TRUE",
            "attribute_type(s, 'S') AND attribute_exists(extra) = true",
            vec!["alpha"],
        ),
    ];

    for (case, predicate, expected) in rows {
        let statement = format!("SELECT pk FROM \"{TABLE}\" WHERE {predicate}");
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {statement:?} expected HTTP 200, got {status} with {answered}"
        );
        assert_eq!(
            selected_keys(&answered),
            expected,
            "case {case:?}: {predicate:?} selects {expected:?}, got {answered}"
        );
    }
}

/// RULE 2: a clause this surface does not serve is refused by its own name and the position
/// of the expression it carries, and the statement it follows writes nothing.
#[tokio::test]
async fn a_clause_this_surface_does_not_serve_is_refused_by_name_and_position() {
    let router = seeded().await;

    // One row per clause, per verb it is attached to, per casing, and over a statement
    // spelled across two lines — the keyword a refusal states is the language's own
    // spelling whichever casing the statement wrote.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "a row count after a read of the whole table",
            format!("SELECT * FROM \"{TABLE}\" LIMIT 17"),
            "LIMIT",
            "17",
        ),
        (
            "a row count after a read under a predicate",
            format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'alpha' LIMIT 3"),
            "LIMIT",
            "3",
        ),
        (
            "a row count written in lower case",
            format!("select * from \"{TABLE}\" limit 9"),
            "LIMIT",
            "9",
        ),
        (
            "a row count on the second line of the statement",
            format!("SELECT * FROM \"{TABLE}\"\nLIMIT 11"),
            "LIMIT",
            "11",
        ),
        (
            "a row count whose expression is a parameter marker",
            format!("SELECT * FROM \"{TABLE}\" LIMIT ?"),
            "LIMIT",
            "?",
        ),
        (
            "an offset after a read",
            format!("SELECT * FROM \"{TABLE}\" OFFSET 23"),
            "OFFSET",
            "23",
        ),
        (
            "an offset written in lower case",
            format!("SELECT * FROM \"{TABLE}\" offset 5"),
            "OFFSET",
            "5",
        ),
        (
            "a row count after a write that states one item",
            format!("UPDATE \"{TABLE}\" SET n = 42 WHERE pk = 'alpha' LIMIT 1"),
            "LIMIT",
            "1",
        ),
        (
            "a row count after a removal",
            format!("DELETE FROM \"{TABLE}\" WHERE pk = 'alpha' LIMIT 2"),
            "LIMIT",
            "2",
        ),
    ];

    for (case, statement, clause, argument) in rows {
        let expected = format!(
            "Unsupported clause: {clause} at {}",
            position_of(&statement, argument)
        );
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", expected.as_str()),
            "case {case:?}: {statement:?} expected {expected:?}, got {status} with {answered}"
        );
    }

    // A clause stating no expression at all is positioned just past the text, with the
    // empty width a position past the last token has.
    let bare = format!("SELECT * FROM \"{TABLE}\" LIMIT");
    let expected = format!(
        "Unsupported clause: LIMIT at 1:{}:0",
        bare.chars().count() + 1
    );
    let (status, answered) = execute(&router, &bare).await;
    assert_eq!(
        (
            status,
            error_code(&answered).as_str(),
            message(&answered).as_str()
        ),
        (400, "ValidationException", expected.as_str()),
        "case a clause with no expression behind it: {bare:?} expected {expected:?}, got \
         {status} with {answered}"
    );

    // None of the refused statements ran: the write and the removal above left the item
    // exactly as the suite seeded it.
    assert_eq!(
        stored(&router, "alpha").await,
        json!({
            "pk": {"S": "alpha"}, "s": {"S": "abcdef"}, "n": {"N": "6"}, "extra": {"S": "here"}
        }),
        "case a refused clause writes nothing: the seeded item stands as it was"
    );
}

/// RULE 3: the absence of an attribute is read by the type test and by no other operator.
#[tokio::test]
async fn the_absence_of_an_attribute_is_only_read_by_the_type_test() {
    let router = seeded().await;

    // One row per operator a comparison can use and per side `MISSING` stands on, over each
    // verb that states a predicate.
    let rows: Vec<(&str, String)> = vec![
        (
            "an equality against a quoted attribute",
            format!("SELECT * FROM \"{TABLE}\" WHERE \"extra\" = MISSING"),
            ),
        (
            "an inequality",
            format!("SELECT * FROM \"{TABLE}\" WHERE extra <> MISSING"),
        ),
        (
            "an ordering",
            format!("SELECT * FROM \"{TABLE}\" WHERE n > MISSING"),
        ),
        (
            "the keyword written in lower case",
            format!("SELECT * FROM \"{TABLE}\" WHERE extra = missing"),
        ),
        (
            "the keyword on the left of the comparison",
            format!("SELECT * FROM \"{TABLE}\" WHERE MISSING = extra"),
        ),
        (
            "a comparison inside a conjunction",
            format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'alpha' AND extra = MISSING"),
        ),
        (
            "a comparison in the predicate of a write",
            format!("UPDATE \"{TABLE}\" SET n = 99 WHERE pk = 'alpha' AND extra = MISSING"),
        ),
        (
            "a comparison in the predicate of a removal",
            format!("DELETE FROM \"{TABLE}\" WHERE pk = 'alpha' AND extra = MISSING"),
        ),
    ];

    for (case, statement) in rows {
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", MISSING_COMPARISON),
            "case {case:?}: {statement:?} expected {MISSING_COMPARISON:?}, got {status} with \
             {answered}"
        );
    }

    // The type test is the operator that reads it, and the refusals above wrote nothing.
    for (case, predicate, expected) in [
        ("the test for an absent attribute", "extra IS MISSING", vec!["beta"]),
        (
            "the test for one the item carries",
            "extra IS NOT MISSING",
            vec!["alpha"],
        ),
    ] {
        let statement = format!("SELECT pk FROM \"{TABLE}\" WHERE {predicate}");
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {statement:?} expected HTTP 200, got {status} with {answered}"
        );
        assert_eq!(
            selected_keys(&answered),
            expected,
            "case {case:?}: {predicate:?} selects {expected:?}, got {answered}"
        );
    }

    // The sentence belongs to a COMPARISON: the same keyword standing where a stored
    // value belongs compares nothing, so it is reported as the keyword it is — which is
    // what every other keyword of the dialect earns in that position.
    for (case, statement) in [
        (
            "the keyword as a member of the value an INSERT carries",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'flag': MISSING}}"),
        ),
        (
            "the keyword as the value a SET clause assigns",
            format!("UPDATE \"{TABLE}\" SET flag = MISSING WHERE pk = 'alpha'"),
        ),
    ] {
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", UNEXPECTED_KEYWORD),
            "case {case:?}: {statement:?} expected {UNEXPECTED_KEYWORD:?}, got {status} with \
             {answered}"
        );
    }
    assert_eq!(
        stored(&router, "alpha").await["n"],
        json!({"N": "6"}),
        "case a refused comparison writes nothing: the seeded number stands as it was"
    );
}

/// RULE 4: an unquoted word where a value belongs is refused naming the token, the document
/// that holds it and where the statement spells it.
#[tokio::test]
async fn an_unquoted_word_where_a_value_belongs_names_the_token_and_its_document() {
    let router = seeded().await;

    // One row per DEPTH the holding document sits at and per collection that holds the
    // value: the statement's own value, a member of its top-level tuple, a member of a
    // nested tuple, a member of a list and a member of a bag.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "the whole value an INSERT carries",
            format!("INSERT INTO \"{TABLE}\" VALUE unquoted"),
            "unquoted",
            "root",
        ),
        (
            "a member of the statement's own tuple",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'flag': yes}}"),
            "yes",
            "root",
        ),
        (
            "a member of a nested tuple",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'm': {{'flag': maybe}}}}"),
            "maybe",
            "m",
        ),
        (
            "a member of a tuple nested twice",
            format!(
                "INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'm': {{'deep': {{'flag': \
                 further}}}}}}"
            ),
            "further",
            "deep",
        ),
        (
            "a member of a list",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'l': [nope]}}"),
            "nope",
            "l",
        ),
        (
            "a member of a list inside a nested tuple",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'm': {{'l': [farther]}}}}"),
            "farther",
            "l",
        ),
        (
            "a member of a bag",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'ss': <<sometimes>>}}"),
            "sometimes",
            "ss",
        ),
        (
            "the value a SET clause assigns a list member",
            format!("UPDATE \"{TABLE}\" SET l = [whatever] WHERE pk = 'alpha'"),
            "whatever",
            "l",
        ),
        (
            "the value a SET clause assigns a member of a nested list",
            format!("UPDATE \"{TABLE}\" SET m.deeper = [elsewhere] WHERE pk = 'alpha'"),
            "elsewhere",
            "deeper",
        ),
        (
            "the member a set_add clause adds",
            format!("UPDATE \"{TABLE}\" SET ss = set_add(ss, <<anyhow>>) WHERE pk = 'alpha'"),
            "anyhow",
            "ss",
        ),
    ];

    for (case, statement, token, key) in rows {
        let expected = format!(
            "Unsupported data type: {token} under key {key} at {}",
            position_of(&statement, token)
        );
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", expected.as_str()),
            "case {case:?}: {statement:?} expected {expected:?}, got {status} with {answered}"
        );
    }

    // A word that spells one of the dialect's KEYWORDS is reported as the keyword it is,
    // which is the other half of the classification.
    for (case, statement) in [
        (
            "a clause word where a value belongs",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'flag': by}}"),
        ),
        (
            "a type name where a value belongs",
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'flag': timestamp}}"),
        ),
    ] {
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", UNEXPECTED_KEYWORD),
            "case {case:?}: {statement:?} expected {UNEXPECTED_KEYWORD:?}, got {status} with \
             {answered}"
        );
    }

    // The quoted spelling is the one that names a value, and none of the refusals above
    // wrote anything.
    let quoted = format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': 'fresh', 'flag': 'yes'}}");
    let (status, answered) = execute(&router, &quoted).await;
    assert_eq!(
        status, 200,
        "case the quoted spelling: {quoted:?} expected HTTP 200, got {status} with {answered}"
    );
    assert_eq!(
        stored(&router, "fresh").await,
        json!({"pk": {"S": "fresh"}, "flag": {"S": "yes"}}),
        "case the quoted spelling: the item it carries is stored, and the refusals stored none"
    );
    assert_eq!(
        stored(&router, "alpha").await.get("l").cloned(),
        None,
        "case a refused value writes nothing: the seeded item gained no attribute"
    );
}
