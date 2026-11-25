// SPDX-License-Identifier: CC0-1.0

//! What the PartiQL surface answers a statement spelled the way SQL habits spell one: a
//! table name written unquoted that may not stand unquoted, a source that is no name, the
//! clauses and tokens of the language this surface does not run, and the rules an `ORDER
//! BY` is held to.
//!
//! RULES, each asserted over the class it generalises over rather than over one statement:
//!
//! * The scanner reads a run of digits and a word running together as TWO lexemes — the
//!   number ends where its digits do — the way the language's own scanner reads that run.
//!   So text no name may be spelled with (a name opening with a digit, a hyphenated name
//!   whose parts carry digits) still becomes tokens, and the statement is refused for the
//!   GRAMMAR that then describes it rather than for a character the scanner could not read.
//! * A `FROM` clause's source is one EXPRESSION of the language, carried on across its
//!   binary value operators, with the binding that may follow it; that the source NAME a
//!   table, and bind no name, is this surface's own requirement, applied once the statement
//!   has parsed. A statement binding a name is refused `Aliasing is not supported`, and one
//!   whose source is no name `Unexpected from source`.
//! * A clause of the language this surface does not serve is refused as that clause: one
//!   spelled with two keywords (`GROUP BY`) names the clause alone, one carrying an
//!   expression of its own (`LIMIT`) names the position of that expression.
//! * A `SELECT` list's set quantifier (`DISTINCT`) is a token of the language standing
//!   where this dialect reads the paths a projection names, and the refusal names the token.
//! * An order is served over one partition of the source, in the order of its key: a
//!   statement that states no predicate at all is refused for the missing `WHERE` clause
//!   BEFORE the attribute it orders by is judged, and one ordering by an attribute that is
//!   no key of the source names that attribute.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table the rows read: a partition key and a sort key, so an `ORDER BY` has a key
/// attribute to name and a non-key one to be refused for.
const TALLY: &str = "habit_tally";

/// A table whose name may NOT stand unquoted in the language — it opens with a digit — so
/// the same name quoted and unquoted can be contrasted on one store.
const DIGIT_LEADING: &str = "2024_orders";

/// The two fixed sentences this surface states about the source a clause reads from.
const ALIASING: &str = "Aliasing is not supported";
const FROM_SOURCE: &str = "Unexpected from source";

/// The sentence an order with no predicate earns, and the one an order by an attribute
/// that is no key of the source earns, over the attribute the statement named.
const ORDER_BY_WITHOUT_WHERE: &str =
    "Must have WHERE clause in the statement when using ORDER BY clause.";
fn order_by_not_a_key(attribute: &str) -> String {
    format!("Attribute {attribute} in ORDER BY clause must be part of the primary key")
}

/// The whole sentence a parse defect is stated with, over the parser's phrase for the
/// position the parse stopped at.
fn parse_failure(phrase: &str) -> String {
    format!("Statement wasn't well formed, can't be processed: {phrase}")
}

/// Where `run` sits in `statement`, as a positioned refusal reports it:
/// `<line>:<column>:<length>`, counted in characters from 1 — derived from the statement
/// the row sends rather than transcribed.
fn position_of(statement: &str, run: &str) -> String {
    let offset = statement
        .find(run)
        .unwrap_or_else(|| panic!("the run {run:?} is not in the statement {statement:?}"));
    let column = 1 + statement[..offset].chars().count();
    format!("1:{column}:{}", run.chars().count())
}

/// An emulator holding both tables, with three items under one partition of the keyed one.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TALLY, &[("pk", "HASH", "S"), ("sk", "RANGE", "N")]).await;
    create_active_table(&router, DIGIT_LEADING, &[("pk", "HASH", "S")]).await;
    for (sort, status) in [(1, "open"), (2, "closed"), (3, "open")] {
        ok(
            &router,
            "PutItem",
            json!({
                "TableName": TALLY,
                "Item": {
                    "pk": {"S": "p"},
                    "sk": {"N": sort.to_string()},
                    "status": {"S": status},
                    "note": {"S": "kept"},
                },
            }),
        )
        .await;
    }
    ok(
        &router,
        "PutItem",
        json!({"TableName": DIGIT_LEADING, "Item": {"pk": {"S": "a"}}}),
    )
    .await;
    router
}

/// Run one statement that must be refused, and report the message it was refused with.
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

/// RULE: a statement that binds a name to the source it reads is refused for the binding,
/// however the text spells that binding — including the bindings the language reads in text
/// written as ONE unquoted name that may not stand unquoted.
#[tokio::test]
async fn a_statement_that_binds_a_name_to_its_source_is_refused_for_the_binding() {
    let router = seeded().await;

    // The rows differ along HOW the binding is spelled (`AS name`, a bare name, a quoted
    // name), what the source it binds to is (a quoted name, a bare name, a literal, a run
    // of text that is no name at all), what the statement carries after it (nothing, a
    // predicate, an order) and which verb reads the source.
    for (case, statement) in [
        (
            "an explicit binding on a quoted name",
            format!("SELECT * FROM \"{TALLY}\" AS t"),
        ),
        (
            "a binding with the keyword left out",
            format!("SELECT * FROM \"{TALLY}\" t"),
        ),
        (
            "a quoted name bound to a bare one",
            format!("SELECT * FROM {TALLY} \"t\""),
        ),
        (
            "a binding, and a predicate after it",
            format!("SELECT * FROM \"{TALLY}\" t WHERE t.pk = 'p'"),
        ),
        (
            "a binding on a projection's source",
            format!("SELECT status FROM \"{TALLY}\" AS t"),
        ),
        (
            // `2024_orders` is one name of the store and no name of the language: it
            // reads as the number `2024` with the name `_orders` bound to it.
            "a name opening with a digit, read as a number with a name after it",
            format!("SELECT * FROM {DIGIT_LEADING}"),
        ),
        (
            "the same name with a predicate after it",
            format!("SELECT * FROM {DIGIT_LEADING} WHERE pk = 'a'"),
        ),
        (
            // `orders-2024q1` reads as the expression `orders - 2024` with the name `q1`
            // bound to it.
            "a hyphenated name whose last part opens with digits",
            "SELECT * FROM orders-2024q1".to_string(),
        ),
        (
            "a binding after a literal source",
            format!("SELECT * FROM '{TALLY}' AS t"),
        ),
        (
            "a DELETE whose source is bound",
            format!("DELETE FROM {DIGIT_LEADING} WHERE pk = 'a'"),
        ),
        (
            "an UPDATE whose target is bound",
            format!("UPDATE \"{TALLY}\" AS t SET note = 'x' WHERE pk = 'p' AND sk = 1"),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered, ALIASING,
            "case {case:?} ({statement:?}): expected {ALIASING:?}, got {answered:?}"
        );
    }

    // The same names QUOTED are names of the language, so the statements they spell are
    // served from the store — which is what makes the refusals above about the binding the
    // unquoted text states and not about the table.
    let served = ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!("SELECT * FROM \"{DIGIT_LEADING}\" WHERE pk = 'a'")}),
    )
    .await;
    assert_eq!(
        served["Items"],
        json!([{"pk": {"S": "a"}}]),
        "case the same name quoted: expected the stored item, got {served}"
    );
    let untouched = ok(
        &router,
        "GetItem",
        json!({"TableName": TALLY, "Key": {"pk": {"S": "p"}, "sk": {"N": "1"}}}),
    )
    .await;
    assert_eq!(
        untouched["Item"]["note"],
        json!({"S": "kept"}),
        "case a refused statement writes nothing: got {untouched}"
    );
}

/// RULE: a source that is no NAME — a literal, or an expression of the language — is
/// refused with the fixed sentence about the source a clause reads from.
#[tokio::test]
async fn a_source_that_names_no_table_is_refused_for_the_source_it_states() {
    let router = seeded().await;

    // The rows differ along the FORM of the source (a string literal, a number, a name
    // continued by each of the language's binary value operators, a literal continued by
    // one), what follows it (nothing, a predicate) and which verb reads it.
    for (case, statement) in [
        (
            "a single-quoted table name, which is a literal",
            format!("SELECT * FROM '{TALLY}'"),
        ),
        (
            "a single-quoted name, with a predicate after it",
            format!("SELECT * FROM '{TALLY}' WHERE pk = 'p'"),
        ),
        ("a number", "SELECT * FROM 42".to_string()),
        (
            "a name the text continues with a subtraction",
            "SELECT * FROM orders-2024".to_string(),
        ),
        (
            "a name the text continues with an addition",
            format!("SELECT * FROM {TALLY}+1"),
        ),
        (
            "a name the text continues with a multiplication",
            format!("SELECT * FROM {TALLY}*2"),
        ),
        (
            "an expression source, with a predicate after it",
            format!("SELECT * FROM {TALLY}-1 WHERE pk = 'p'"),
        ),
        (
            "a DELETE whose source is a literal",
            format!("DELETE FROM '{TALLY}' WHERE pk = 'p' AND sk = 1"),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered, FROM_SOURCE,
            "case {case:?} ({statement:?}): expected {FROM_SOURCE:?}, got {answered:?}"
        );
    }

    // None of them removed the item a DELETE above addressed.
    let kept = ok(
        &router,
        "GetItem",
        json!({"TableName": TALLY, "Key": {"pk": {"S": "p"}, "sk": {"N": "1"}}}),
    )
    .await;
    assert!(
        kept.get("Item").is_some(),
        "case a refused DELETE removes nothing: got {kept}"
    );
}

/// RULE: a run of digits and a word running together are TWO lexemes, so the text becomes
/// tokens and the statement is refused for the grammar that describes them — while a
/// lexeme the scanner cannot finish reading at all still carries no phrase.
#[tokio::test]
async fn a_number_and_a_word_running_together_are_two_lexemes() {
    let router = seeded().await;

    // The rows differ along WHERE the run stands (a predicate's operand, a value an
    // INSERT carries, a projection) and along what the run spells: a number with a word
    // after it, which reads as two tokens, against the two runs that are no lexeme at all
    // — an exponent marker with no exponent, and a second decimal point.
    for (case, statement, expected) in [
        (
            "a number with a word run onto it, in a predicate",
            format!("SELECT * FROM \"{TALLY}\" WHERE pk = 'p' AND sk = 1abc"),
            parse_failure("Unexpected token after expression"),
        ),
        (
            "the same run as the value an INSERT carries",
            format!("INSERT INTO \"{TALLY}\" VALUE {{'pk': 'z', 'sk': 9kg}}"),
            parse_failure("Expected RIGHT_CURLY"),
        ),
        (
            "a number with a word run onto it in a projection",
            format!("SELECT 1abc FROM \"{TALLY}\""),
            parse_failure("Expected identifier for simple path"),
        ),
        (
            "an exponent marker with no exponent, which is no lexeme",
            format!("INSERT INTO \"{TALLY}\" VALUE {{'pk': 'z', 'sk': 1e}}"),
            parse_failure(""),
        ),
        (
            "a second decimal point, which is no lexeme either",
            format!("INSERT INTO \"{TALLY}\" VALUE {{'pk': 'z', 'sk': 1.2.3}}"),
            parse_failure(""),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered, expected,
            "case {case:?} ({statement:?}): expected {expected:?}, got {answered:?}"
        );
    }

    // A numeric literal the scanner CAN read is still read, exponent and all: the statement
    // runs and the item it wrote is reported back under the number it carried.
    ok(
        &router,
        "ExecuteStatement",
        json!({
            "Statement":
                format!("INSERT INTO \"{TALLY}\" VALUE {{'pk': 'num', 'sk': 1e2, 'note': 'n'}}"),
        }),
    )
    .await;
    let written = ok(
        &router,
        "GetItem",
        json!({"TableName": TALLY, "Key": {"pk": {"S": "num"}, "sk": {"N": "100"}}}),
    )
    .await;
    assert_eq!(
        written["Item"]["note"],
        json!({"S": "n"}),
        "case a readable exponent is read: expected the item under sk 100, got {written}"
    );
}

/// RULE: a clause or a token of the language this surface does not run is refused as the
/// clause or token it is, named in the LANGUAGE's own spelling.
#[tokio::test]
async fn a_clause_or_token_of_the_language_this_surface_does_not_run_names_itself() {
    let router = seeded().await;
    let limited = format!("SELECT * FROM \"{TALLY}\" LIMIT 7");

    // The rows differ along which part of the language the statement states, how the
    // statement spells it (upper case, lower case) and what the statement carries around
    // it (a projection list, a predicate, a `*`).
    for (case, statement, expected) in [
        (
            "a GROUP BY after a projection list",
            format!("SELECT status, note FROM \"{TALLY}\" GROUP BY status"),
            "Unsupported clause: GROUP BY".to_string(),
        ),
        (
            "a GROUP BY after a predicate, spelled in lower case",
            format!("SELECT * FROM \"{TALLY}\" where pk = 'p' group by status"),
            "Unsupported clause: GROUP BY".to_string(),
        ),
        (
            "the DISTINCT quantifier over a projection list",
            format!("SELECT DISTINCT status FROM \"{TALLY}\""),
            "Unsupported token in expression: DISTINCT".to_string(),
        ),
        (
            "the same quantifier spelled in lower case, over a `*`",
            format!("select distinct * from \"{TALLY}\""),
            "Unsupported token in expression: DISTINCT".to_string(),
        ),
        (
            "the same quantifier in a statement that also states a predicate",
            format!("SELECT DISTINCT pk FROM \"{TALLY}\" WHERE pk = 'p'"),
            "Unsupported token in expression: DISTINCT".to_string(),
        ),
        (
            // A clause carrying an expression of its own names where the statement
            // spells that expression, which is what every positioned sentence of this
            // surface names.
            "a LIMIT, which carries an expression of its own",
            limited.clone(),
            format!("Unsupported clause: LIMIT at {}", position_of(&limited, "7")),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered, expected,
            "case {case:?} ({statement:?}): expected {expected:?}, got {answered:?}"
        );
    }
}

/// RULE: an order is served over one partition of the source in the order of its key — a
/// statement stating no predicate is refused for the clause it does not state, before the
/// attribute it orders by is judged, and one ordering by an attribute that is no key of the
/// source names that attribute.
#[tokio::test]
async fn an_order_states_a_predicate_and_names_a_key_attribute_of_its_source() {
    let router = seeded().await;

    // The rows differ along whether the statement states a predicate at all and whether
    // the attribute it orders by is a key of the source: a statement with no predicate
    // earns the clause sentence WHATEVER it orders by, which is what pins the order the
    // two rules are decided in.
    for (case, statement, expected) in [
        (
            "no predicate, ordering by the sort key",
            format!("SELECT * FROM \"{TALLY}\" ORDER BY sk"),
            ORDER_BY_WITHOUT_WHERE.to_string(),
        ),
        (
            "no predicate, ordering by an attribute that is no key",
            format!("SELECT * FROM \"{TALLY}\" ORDER BY status"),
            ORDER_BY_WITHOUT_WHERE.to_string(),
        ),
        (
            "no predicate, ordering by the partition key, descending",
            format!("SELECT * FROM \"{TALLY}\" ORDER BY pk DESC"),
            ORDER_BY_WITHOUT_WHERE.to_string(),
        ),
        (
            "a predicate, ordering by an attribute that is no key",
            format!("SELECT * FROM \"{TALLY}\" WHERE pk = 'p' ORDER BY status"),
            order_by_not_a_key("status"),
        ),
        (
            "a predicate, ordering by another attribute that is no key",
            format!("SELECT * FROM \"{TALLY}\" WHERE pk = 'p' ORDER BY note"),
            order_by_not_a_key("note"),
        ),
        (
            "a predicate, ordering by an attribute the items do not carry",
            format!("SELECT * FROM \"{TALLY}\" WHERE pk = 'p' ORDER BY absent"),
            order_by_not_a_key("absent"),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered, expected,
            "case {case:?} ({statement:?}): expected {expected:?}, got {answered:?}"
        );
    }

    // An order the source CAN be read in is still served, in the order asked for.
    for (case, statement, expected) in [
        (
            "ascending by the sort key",
            format!("SELECT sk FROM \"{TALLY}\" WHERE pk = 'p' ORDER BY sk ASC"),
            json!(["1", "2", "3"]),
        ),
        (
            "descending by the sort key",
            format!("SELECT sk FROM \"{TALLY}\" WHERE pk = 'p' ORDER BY sk DESC"),
            json!(["3", "2", "1"]),
        ),
    ] {
        let served = ok(
            &router,
            "ExecuteStatement",
            json!({"Statement": &statement}),
        )
        .await;
        let order: Vec<Value> = served["Items"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|item| item["sk"]["N"].clone())
            .collect();
        assert_eq!(
            json!(order), expected,
            "case {case:?} ({statement:?}): expected {expected}, got {served}"
        );
    }
}

/// RULE: these refusals belong to the STATEMENT, so each surface that carries one states
/// the same sentence: `ExecuteStatement` refuses the call, `BatchExecuteStatement` reports
/// it in the slot that sent it, and `ExecuteTransaction` refuses the whole call behind the
/// slot prefix.
#[tokio::test]
async fn every_surface_states_the_same_sentence_for_the_same_statement() {
    let router = seeded().await;

    for (case, statement, expected) in [
        (
            "a bound source",
            format!("SELECT * FROM {DIGIT_LEADING}"),
            ALIASING.to_string(),
        ),
        (
            "a source that is no name",
            format!("SELECT * FROM '{TALLY}'"),
            FROM_SOURCE.to_string(),
        ),
        (
            "a clause the language states and this surface does not serve",
            format!("SELECT status FROM \"{TALLY}\" GROUP BY status"),
            "Unsupported clause: GROUP BY".to_string(),
        ),
        (
            "a token the language states and this surface does not run",
            format!("SELECT DISTINCT status FROM \"{TALLY}\""),
            "Unsupported token in expression: DISTINCT".to_string(),
        ),
    ] {
        let batched = ok(
            &router,
            "BatchExecuteStatement",
            json!({"Statements": [
                {"Statement": &statement},
                {"Statement": format!("SELECT note FROM \"{TALLY}\" WHERE pk = 'p' AND sk = 1")},
            ]}),
        )
        .await;
        assert_eq!(
            (
                batched["Responses"][0]["Error"]["Code"].clone(),
                batched["Responses"][0]["Error"]["Message"].clone(),
            ),
            (json!("ValidationError"), json!(expected)),
            "case {case:?} ({statement:?}): the slot states the statement's own sentence, got \
             {batched}"
        );
        assert_eq!(
            batched["Responses"][1]["Item"]["note"],
            json!({"S": "kept"}),
            "case {case:?}: the slot beside it is served, got {batched}"
        );

        let (status, refused_call) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [
                {"Statement": format!("UPDATE \"{TALLY}\" SET note = 'moved' WHERE pk = 'p' AND sk = 1")},
                {"Statement": &statement},
            ]}),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&refused_call).as_str(),
                message(&refused_call).as_str()
            ),
            (
                400,
                "ValidationException",
                format!("Validation failed in TransactStatements[1]: {expected}").as_str()
            ),
            "case {case:?} ({statement:?}): the transaction states the slot and the sentence, got \
             {status} with {refused_call}"
        );
    }

    // The refused transactions wrote none of their statements.
    let unchanged = ok(
        &router,
        "GetItem",
        json!({"TableName": TALLY, "Key": {"pk": {"S": "p"}, "sk": {"N": "1"}}}),
    )
    .await;
    assert_eq!(
        unchanged["Item"]["note"],
        json!({"S": "kept"}),
        "case a refused transaction writes nothing: got {unchanged}"
    );
}
