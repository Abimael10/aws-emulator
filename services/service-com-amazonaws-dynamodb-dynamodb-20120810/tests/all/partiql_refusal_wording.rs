// SPDX-License-Identifier: CC0-1.0

//! How the PartiQL surface WORDS a refusal: `ExecuteStatement`,
//! `BatchExecuteStatement` and `ExecuteTransaction`.
//!
//! One rule, asserted over the classes it generalises over: a statement DynamoDB will
//! not run is refused with the fixed sentence for that defect class, carrying only the
//! parts the request supplied — the parser's phrase for the position a parse stopped
//! at, the function a call names, the position the statement spells a run of text at,
//! and, inside a transaction, the slot of `TransactStatements` the statement was sent
//! in. The rows below span the classes rather than one recorded statement, and every
//! position a row expects is COMPUTED from the statement text, so a row asserts the
//! rule rather than a transcribed number.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: a partition and a sort key, plus one global
/// secondary index for the rows whose source is an index.
const TABLE: &str = "items";

/// That table's index.
const INDEX: &str = "by-owner";

/// The sentence a parse defect is stated with, over the parser's phrase for the
/// position the parse stopped at.
fn parse_failure(reason: &str) -> String {
    format!("Statement wasn't well formed, can't be processed: {reason}")
}

/// Where `run` sits in `statement`, as a refusal reports a position:
/// `<line>:<column>:<length>`, counted in characters from 1 — where `run` is the ONE
/// TOKEN the refused construct opens at, since that is what a positioned sentence of
/// this surface names. Every positioned row below derives what it expects from the
/// statement it sends, so the expectation is the rule and not one transcribed figure.
fn position_of(statement: &str, run: &str) -> String {
    let offset = statement
        .find(run)
        .unwrap_or_else(|| panic!("the run {run:?} is not in the statement {statement:?}"));
    let line = 1 + statement[..offset].matches('\n').count();
    let line_start = statement[..offset]
        .rfind('\n')
        .map_or(0, |break_at| break_at + 1);
    let column = 1 + statement[line_start..offset].chars().count();
    format!("{line}:{column}:{}", run.chars().count())
}

/// An emulator holding one table with one index, with three items in it.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    // The index-qualified rows need a table that declares an index, which the shared
    // helper's single-key table does not: this one is created on its own.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "keyed",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "owner", "AttributeType": "S"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [{
                "IndexName": INDEX,
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    std::thread::sleep(
        service_com_amazonaws_dynamodb_dynamodb_20120810::state::STATUS_TRANSITION
            + std::time::Duration::from_millis(200),
    );
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "n": {"N": "1"}}}),
    )
    .await;
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": "keyed",
            "Item": {"pk": {"S": "a"}, "sk": {"N": "1"}, "owner": {"S": "x"}, "note": {"S": "one"}},
        }),
    )
    .await;
    router
}

/// Run one statement that must be refused, and report the message it was refused with.
async fn refused(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let (status, answered) = call(router, "ExecuteStatement", body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} with {answered}"
    );
    assert_eq!(
        error_code(&answered),
        "ValidationException",
        "case {case}: expected ValidationException, got {answered}"
    );
    message(&answered)
}

/// One row: the case, the statement, the `Parameters` it carries, and the whole
/// sentence the refusal must state.
struct Row {
    case: &'static str,
    statement: String,
    parameters: Value,
    expected: String,
}

/// Every parse-defect class, each with the parser's own phrase for the position the
/// parse stopped at.
fn parse_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a token after a complete statement",
            statement: format!("SELECT * FROM {TABLE} WHERE pk = 'a' junk"),
            parameters: Value::Null,
            expected: parse_failure("Unexpected token after expression"),
        },
        Row {
            case: "a misspelled verb is a name, and the clause word cannot follow it",
            statement: format!("INSRT INTO \"{TABLE}\" VALUE {{'pk':'a'}}"),
            parameters: Value::Null,
            expected: parse_failure("Unexpected token after expression"),
        },
        Row {
            case: "the text reads as an expression and nothing more",
            statement: TABLE.to_string(),
            parameters: Value::Null,
            expected: parse_failure("Expected data manipulation"),
        },
        Row {
            case: "a single-quoted table name is a literal, not a name",
            statement: format!("SELECT * FROM '{TABLE}'"),
            parameters: Value::Null,
            expected: parse_failure("Expected identifier for simple path"),
        },
        Row {
            case: "a single-quoted attribute name in a projection",
            statement: format!("SELECT 'pk' FROM {TABLE}"),
            parameters: Value::Null,
            expected: parse_failure("Expected identifier for simple path"),
        },
        Row {
            case: "the FROM a SELECT list must be followed by is missing",
            statement: format!("SELECT * {TABLE}"),
            parameters: Value::Null,
            expected: parse_failure("Expected FROM after SELECT list"),
        },
        Row {
            // A projection names PATHS into the item, so a call standing there — one the
            // dialect defines or one it does not — is refused as the path component it
            // is not, at the position the statement spells it.
            case: "a defined function in a projection",
            statement: format!("SELECT attribute_exists(pk) FROM {TABLE}"),
            parameters: Value::Null,
            expected: format!(
                "Unexpected path component at {}",
                position_of(
                    &format!("SELECT attribute_exists(pk) FROM {TABLE}"),
                    "attribute_exists"
                )
            ),
        },
        Row {
            case: "an undefined function in a projection",
            statement: format!("SELECT other_fn(pk) FROM {TABLE}"),
            parameters: Value::Null,
            expected: format!(
                "Unexpected path component at {}",
                position_of(&format!("SELECT other_fn(pk) FROM {TABLE}"), "other_fn")
            ),
        },
        Row {
            case: "a defined function on the left of a SET clause",
            statement: format!("UPDATE {TABLE} SET size(n) = 2 WHERE pk = 'a'"),
            parameters: Value::Null,
            expected: parse_failure("Expected identifier for simple path"),
        },
        Row {
            // A lexeme the scanner cannot read is no token at all, so there is no
            // grammar position to name: the sentence's tail is empty, the way it is for
            // a quoted run the statement never closes.
            case: "a malformed numeric literal",
            statement: format!("INSERT INTO {TABLE} VALUE {{'pk': 'z', 'n': 1.2.3}}"),
            parameters: Value::Null,
            expected: parse_failure(""),
        },
        Row {
            case: "an exponent marker with no exponent",
            statement: format!("INSERT INTO {TABLE} VALUE {{'pk': 'z', 'n': 1e}}"),
            parameters: Value::Null,
            expected: parse_failure(""),
        },
        Row {
            case: "a tuple literal the statement never closes",
            statement: format!("INSERT INTO {TABLE} VALUE {{'pk': 'z', 'n': 1"),
            parameters: Value::Null,
            expected: parse_failure("Expected RIGHT_CURLY"),
        },
        Row {
            case: "a bag literal the statement never closes",
            statement: format!("INSERT INTO {TABLE} VALUE {{'pk': 'z', 'n': <<1, 2}}"),
            parameters: Value::Null,
            expected: parse_failure("Expected RIGHT_DOUBLE_ANGLE_BRACKET"),
        },
        Row {
            case: "a list literal the statement never closes",
            statement: format!("INSERT INTO {TABLE} VALUE {{'pk': 'z', 'n': [1, 2}}"),
            parameters: Value::Null,
            expected: parse_failure("Expected RIGHT_BRACKET"),
        },
        Row {
            case: "a clause that stops before its predicate",
            statement: format!("SELECT * FROM {TABLE} WHERE"),
            parameters: Value::Null,
            expected: parse_failure("Unexpected term"),
        },
    ]
}

/// A call to a function the dialect does not define, in each position a call can sit
/// in: the name and the position are read off the statement.
fn unrecognized_function_rows() -> Vec<Row> {
    // The rows differ in WHERE the call sits and in the name's length, which is what
    // the reported position is made of.
    [
        (
            "an unknown function in a predicate",
            format!("SELECT * FROM \"{TABLE}\" WHERE made_up_function(pk)"),
            "made_up_function",
        ),
        (
            "an unknown function on the right of a comparison",
            format!("SELECT * FROM {TABLE} WHERE n = magnitude(n)"),
            "magnitude",
        ),
        (
            "an unknown function in a SET clause",
            format!("UPDATE {TABLE} SET n = twice(n) WHERE pk = 'a'"),
            "twice",
        ),
    ]
    .into_iter()
    .map(|(case, statement, name)| Row {
        case,
        expected: format!(
            "Unrecognized function: {name} at {}",
            position_of(&statement, name)
        ),
        statement,
        parameters: Value::Null,
    })
    .collect()
}

/// A call given something other than the document path it reads, and a call given the
/// wrong number of operands: both name what was called.
fn operand_rows() -> Vec<Row> {
    let requires_path = |function: &str| {
        format!("Operator or function requires a document path; operator or function: {function}")
    };
    let operand_count = |function: &str, operands: usize| {
        format!(
            "Incorrect number of operands for operator or function; operator or function: \
             {function}, number of operands: {operands}"
        )
    };
    // The functions are the ones this dialect defines over a document path
    // (`smithy_query(query="doc",shape="ExecuteStatement")` documents the PartiQL
    // surface; the functions are the Developer Guide's `attribute_exists`,
    // `attribute_not_exists`, `attribute_type`, `begins_with`, `contains` and `size`).
    vec![
        Row {
            case: "attribute_exists given a string literal",
            statement: format!("SELECT * FROM {TABLE} WHERE attribute_exists('pk')"),
            parameters: Value::Null,
            expected: requires_path("attribute_exists"),
        },
        Row {
            case: "attribute_not_exists given a number literal",
            statement: format!("SELECT * FROM {TABLE} WHERE attribute_not_exists(1)"),
            parameters: Value::Null,
            expected: requires_path("attribute_not_exists"),
        },
        Row {
            case: "begins_with given a literal first operand",
            statement: format!("SELECT * FROM {TABLE} WHERE begins_with('a', 'a')"),
            parameters: Value::Null,
            expected: requires_path("begins_with"),
        },
        Row {
            case: "size given a literal",
            statement: format!("SELECT * FROM {TABLE} WHERE size('abc') > 1"),
            parameters: Value::Null,
            expected: requires_path("size"),
        },
        Row {
            case: "attribute_exists given no operand at all",
            statement: format!("SELECT * FROM {TABLE} WHERE attribute_exists()"),
            parameters: Value::Null,
            expected: operand_count("attribute_exists", 0),
        },
        Row {
            case: "attribute_exists given two operands",
            statement: format!("SELECT * FROM {TABLE} WHERE attribute_exists(pk, n)"),
            parameters: Value::Null,
            expected: operand_count("attribute_exists", 2),
        },
        Row {
            case: "begins_with given one operand",
            statement: format!("SELECT * FROM {TABLE} WHERE begins_with(pk)"),
            parameters: Value::Null,
            expected: operand_count("begins_with", 1),
        },
        Row {
            case: "contains given three operands",
            statement: format!("SELECT * FROM {TABLE} WHERE contains(pk, 'a', 'b')"),
            parameters: Value::Null,
            expected: operand_count("contains", 3),
        },
        Row {
            case: "size given two operands",
            statement: format!("SELECT * FROM {TABLE} WHERE size(pk, n) > 1"),
            parameters: Value::Null,
            expected: operand_count("size", 2),
        },
    ]
}

/// A source path with more components than a table and an index of it, and a write
/// addressed through an index qualifier.
///
/// An `INSERT` names WHERE a new item goes inside its `INTO` clause, so that clause is
/// held to what it may spell: at most two components at all (the arity sentence), and the
/// single table name (the positioned sentence, over the token the path opens at). An
/// `UPDATE` and a `DELETE` ADDRESS a stored item through the path they name, so a
/// qualifier there asks for the write through an index and earns the fixed index sentence.
/// A `SELECT`'s `FROM` is a read and takes the qualifier, so only its arity is bounded.
fn source_rows() -> Vec<Row> {
    const PATH_ARITY: &str = "A path may contain at most 2 components in the FROM clause";
    const ON_AN_INDEX: &str = "This operation is not supported on an index";
    let mut rows = vec![
        Row {
            case: "a three-component bare FROM path",
            statement: format!("SELECT * FROM {TABLE}.a.b"),
            parameters: Value::Null,
            expected: PATH_ARITY.to_string(),
        },
        Row {
            case: "a four-component quoted FROM path",
            statement: format!("SELECT * FROM \"keyed\".\"{INDEX}\".\"x\".\"y\""),
            parameters: Value::Null,
            expected: PATH_ARITY.to_string(),
        },
        Row {
            case: "a three-component FROM path in a DELETE",
            statement: format!("DELETE FROM \"keyed\".\"{INDEX}\".\"extra\" WHERE pk = 'a'"),
            parameters: Value::Null,
            expected: PATH_ARITY.to_string(),
        },
        Row {
            case: "a three-component target in an INSERT",
            statement: format!("INSERT INTO {TABLE}.a.b VALUE {{'pk':'b'}}"),
            parameters: Value::Null,
            expected: PATH_ARITY.to_string(),
        },
        Row {
            case: "an UPDATE through an index qualifier",
            statement: format!(
                "UPDATE \"keyed\".\"{INDEX}\" SET note = 'two' WHERE pk = 'a' AND sk = 1"
            ),
            parameters: Value::Null,
            expected: ON_AN_INDEX.to_string(),
        },
        Row {
            case: "an UPDATE through a bare index qualifier",
            statement: "UPDATE keyed.anyindex SET note = 'two' WHERE pk = 'a' AND sk = 1"
                .to_string(),
            parameters: Value::Null,
            expected: ON_AN_INDEX.to_string(),
        },
        Row {
            case: "an UPDATE through a longer qualifier still addresses an index",
            statement: format!(
                "UPDATE \"keyed\".\"{INDEX}\".\"extra\" SET note = 'two' WHERE pk = 'a'"
            ),
            parameters: Value::Null,
            expected: ON_AN_INDEX.to_string(),
        },
        Row {
            case: "a DELETE through an index qualifier",
            statement: format!("DELETE FROM \"keyed\".\"{INDEX}\" WHERE pk = 'a' AND sk = 1"),
            parameters: Value::Null,
            expected: ON_AN_INDEX.to_string(),
        },
        Row {
            case: "a DELETE through a bare index qualifier",
            statement: "DELETE FROM keyed.anyindex WHERE pk = 'a' AND sk = 1".to_string(),
            parameters: Value::Null,
            expected: ON_AN_INDEX.to_string(),
        },
    ];
    // The one verb whose clause only NAMES where an item goes earns the positioned
    // sentence, and the position it reports is where the statement OPENS that path: the
    // token the path begins with, with that token's own width, so a qualifier spelled
    // after it moves neither the column nor the width.
    for (case, statement, opens) in [
        (
            "an INSERT through an index qualifier",
            format!("INSERT INTO \"keyed\".\"{INDEX}\" VALUE {{'pk':'b','sk':2}}"),
            "\"keyed\"".to_string(),
        ),
        (
            "an INSERT through a bare index qualifier",
            "INSERT INTO keyed.anyindex VALUE {'pk':'b','sk':2}".to_string(),
            "keyed".to_string(),
        ),
    ] {
        rows.push(Row {
            expected: single_table_at(&statement, &opens),
            case,
            statement,
            parameters: Value::Null,
        });
    }
    rows
}

/// The refusals a statement that parses earns: a `Parameters` list that does not
/// correspond to the statement's markers, a number past the stored domain, and a
/// singleton write whose `WHERE` clause pins no whole key.
fn semantic_rows() -> Vec<Row> {
    const PARAMETERS: &str = "Number of parameters in request and statement don't match.";
    const KEY_EQUALITY: &str =
        "Where clause does not contain a mandatory equality on all key attributes";
    // 39 digits: one past the documented 38 significant digits an N carries.
    const THIRTY_NINE_DIGITS: &str = "123456789012345678901234567890123456789";
    vec![
        Row {
            case: "more parameters than markers",
            statement: format!("SELECT * FROM {TABLE} WHERE pk = ?"),
            parameters: json!([{"S": "a"}, {"S": "b"}]),
            expected: PARAMETERS.to_string(),
        },
        Row {
            case: "fewer parameters than markers",
            statement: format!("SELECT * FROM {TABLE} WHERE pk = ? AND n = ?"),
            parameters: json!([{"S": "a"}]),
            expected: PARAMETERS.to_string(),
        },
        Row {
            case: "no parameters at all for one marker",
            statement: format!("DELETE FROM {TABLE} WHERE pk = ?"),
            parameters: Value::Null,
            expected: PARAMETERS.to_string(),
        },
        Row {
            case: "a number literal past the stored precision, in a VALUE",
            statement: format!("INSERT INTO {TABLE} VALUE {{'pk': 'z', 'n': {THIRTY_NINE_DIGITS}}}"),
            parameters: Value::Null,
            expected: "Attempting to store more than 38 significant digits in a Number"
                .to_string(),
        },
        Row {
            case: "a number literal past the stored precision, in a SET clause",
            statement: format!("UPDATE {TABLE} SET n = {THIRTY_NINE_DIGITS} WHERE pk = 'a'"),
            parameters: Value::Null,
            expected: "Attempting to store more than 38 significant digits in a Number"
                .to_string(),
        },
        Row {
            case: "an UPDATE that states no WHERE clause",
            statement: format!("UPDATE {TABLE} SET n = 2"),
            parameters: Value::Null,
            expected: KEY_EQUALITY.to_string(),
        },
        Row {
            case: "a DELETE that pins only part of the key",
            statement: "DELETE FROM keyed WHERE pk = 'a'".to_string(),
            parameters: Value::Null,
            expected: KEY_EQUALITY.to_string(),
        },
        Row {
            case: "an UPDATE whose sort key is pinned by an inequality",
            statement: "UPDATE keyed SET note = 'two' WHERE pk = 'a' AND sk > 0".to_string(),
            parameters: Value::Null,
            expected: KEY_EQUALITY.to_string(),
        },
    ]
}

/// Every row of every class, run through `ExecuteStatement`.
fn every_row() -> Vec<Row> {
    let mut rows = parse_rows();
    rows.extend(unrecognized_function_rows());
    rows.extend(operand_rows());
    rows.extend(source_rows());
    rows.extend(semantic_rows());
    rows
}

#[tokio::test]
async fn each_statement_defect_class_states_its_own_sentence() {
    let router = seeded().await;

    for row in every_row() {
        let mut body = json!({"Statement": row.statement});
        if !row.parameters.is_null() {
            body["Parameters"] = row.parameters.clone();
        }
        let stated = refused(&router, row.case, body).await;
        assert_eq!(
            stated, row.expected,
            "case {}: statement {:?} expected the message {:?}, got {stated:?}",
            row.case, row.statement, row.expected
        );
    }

    // None of those refusals wrote anything: the seeded item stands as it was.
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
}

#[tokio::test]
async fn a_batch_slot_states_the_same_sentence_its_own_statement_earns() {
    let router = seeded().await;

    // Three malformed statements, one per class, each sent beside a well-formed INSERT
    // that must still run: a batch reports a statement's defect in ITS OWN slot.
    let rows = [
        (
            "a misspelled verb",
            format!("INSRT INTO \"{TABLE}\" VALUE {{'pk':'q'}}"),
            parse_failure("Unexpected token after expression"),
        ),
        (
            "an unknown function",
            format!("SELECT * FROM {TABLE} WHERE made_up_function(pk) AND pk = 'a'"),
            format!(
                "Unrecognized function: made_up_function at {}",
                position_of(
                    &format!("SELECT * FROM {TABLE} WHERE made_up_function(pk) AND pk = 'a'"),
                    "made_up_function"
                )
            ),
        ),
        (
            "a write addressed through an index qualifier",
            format!("INSERT INTO {TABLE}.gsi VALUE {{'pk':'q'}}"),
            single_table_at(
                &format!("INSERT INTO {TABLE}.gsi VALUE {{'pk':'q'}}"),
                TABLE,
            ),
        ),
    ];

    for (at, (case, statement, expected)) in rows.iter().enumerate() {
        let key = format!("sibling-{at}");
        let (status, answered) = call(
            &router,
            "BatchExecuteStatement",
            json!({"Statements": [
                {"Statement": statement},
                {"Statement": format!("INSERT INTO {TABLE} VALUE {{'pk': '{key}'}}")},
            ]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: a batch answers 200 whatever its slots say, got {status} with {answered}"
        );
        let slot = &answered["Responses"][0];
        assert_eq!(
            slot["Error"]["Code"].as_str(),
            Some("ValidationError"),
            "case {case}: expected the per-slot ValidationError code, got {slot}"
        );
        assert_eq!(
            slot["Error"]["Message"].as_str(),
            Some(expected.as_str()),
            "case {case}: expected the slot message {expected:?}, got {slot}"
        );
        // The slot beside it ran, and its write is readable.
        let sibling = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
        )
        .await;
        assert_eq!(
            sibling["Item"],
            json!({"pk": {"S": key}}),
            "case {case}: the well-formed sibling statement ran, got {sibling}"
        );
    }
}

#[tokio::test]
async fn every_batch_slot_echoes_the_table_its_statement_addressed() {
    let router = seeded().await;

    // A write that reports an item, a write that reports none, a read, and a statement
    // that fails on its own merits: each slot names the table it addressed, so a batch
    // spread over two tables can be attributed slot by slot.
    let (status, answered) = call(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": [
            {"Statement": "UPDATE keyed SET note = 'two' WHERE pk = 'a' AND sk = 1 RETURNING ALL OLD *"},
            {"Statement": format!("UPDATE {TABLE} SET n = 2 WHERE pk = 'a'")},
            {"Statement": "UPDATE keyed SET note = 'three' WHERE pk = 'zz' AND sk = 9"},
        ]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a batch over two tables: expected HTTP 200, got {status} with {answered}"
    );

    for (at, (case, table, item, code)) in [
        (
            "an UPDATE reporting the item it replaced",
            "keyed",
            Some(json!({
                "pk": {"S": "a"},
                "sk": {"N": "1"},
                "owner": {"S": "x"},
                "note": {"S": "one"},
            })),
            None,
        ),
        ("an UPDATE that reports no item", TABLE, None, None),
        (
            "a statement whose condition was not met",
            "keyed",
            None,
            Some("ConditionalCheckFailed"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let slot = &answered["Responses"][at];
        assert_eq!(
            slot["TableName"].as_str(),
            Some(table),
            "case {case}: expected TableName {table:?}, got {slot}"
        );
        assert_eq!(
            slot.get("Item").cloned(),
            item,
            "case {case}: expected Item {item:?}, got {slot}"
        );
        // Which KIND of slot it is: one that ran carries no error, and one that failed
        // on its own merits carries its code beside the table it addressed.
        assert_eq!(
            slot["Error"]["Code"].as_str(),
            code,
            "case {case}: expected Error.Code {code:?}, got {slot}"
        );
    }

    // The slots that ran wrote what they said they did.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": "keyed", "Key": {"pk": {"S": "a"}, "sk": {"N": "1"}}}),
    )
    .await;
    assert_eq!(
        read["Item"]["note"],
        json!({"S": "two"}),
        "case the reported UPDATE landed: got {read}"
    );
}

#[tokio::test]
async fn a_transaction_states_the_slot_a_statement_was_sent_in() {
    let router = seeded().await;

    // The same defects, each sent at a different position of `TransactStatements`: the
    // sentence is the statement's own, behind the slot the request stated it at.
    let defects = [
        (
            "a misspelled verb",
            format!("INSRT INTO \"{TABLE}\" VALUE {{'pk':'q'}}"),
            parse_failure("Unexpected token after expression"),
        ),
        (
            "a single-quoted table name",
            format!("SELECT * FROM '{TABLE}' WHERE pk = 'a'"),
            parse_failure("Expected identifier for simple path"),
        ),
        (
            "an unknown function",
            format!("UPDATE {TABLE} SET n = twice(n) WHERE pk = 'a'"),
            format!(
                "Unrecognized function: twice at {}",
                position_of(
                    &format!("UPDATE {TABLE} SET n = twice(n) WHERE pk = 'a'"),
                    "twice"
                )
            ),
        ),
    ];
    let well_formed = format!("UPDATE {TABLE} SET n = 2 WHERE pk = 'a'");

    for (slot, (case, statement, sentence)) in defects.iter().enumerate() {
        // The malformed statement is sent at its own index, after that many well-formed
        // ones, so the index the message states is a fact of the request.
        let mut statements: Vec<Value> = (0..slot)
            .map(|_| json!({"Statement": well_formed}))
            .collect();
        statements.push(json!({"Statement": statement}));
        let (status, answered) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": statements}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case} at slot {slot}: expected HTTP 400, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case} at slot {slot}: expected ValidationException, got {answered}"
        );
        let expected = format!("Validation failed in TransactStatements[{slot}]: {sentence}");
        assert_eq!(
            message(&answered),
            expected,
            "case {case} at slot {slot}: expected the message {expected:?}, got {answered}"
        );
    }

    // A refused transaction applied nothing, so the well-formed statements beside the
    // malformed one did not write either.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}}}),
    )
    .await;
    assert_eq!(
        read["Item"]["n"],
        json!({"N": "1"}),
        "case a refused transaction writes nothing: got {read}"
    );
}

#[tokio::test]
async fn a_transaction_read_that_addresses_no_one_item_states_that_rule() {
    let router = seeded().await;

    let no_one_item = "Validation failed in TransactStatements[0]: Select statements within \
                       ExecuteTransaction must specify the primary key in the where clause.";
    // The ways a read statement can fail to address ONE item of the table: no clause at
    // all, part of the key, and a key pinned by something other than an equality. Each is
    // a refusal about ONE statement of the request, so each is stated behind the slot that
    // carried it — as is the different defect a source that is an INDEX earns: that
    // statement does pin the index's key, and a transaction reads no index at all.
    let rows = [
        ("no WHERE clause at all", "SELECT * FROM keyed", no_one_item),
        (
            "only the partition key",
            "SELECT * FROM keyed WHERE pk = 'a'",
            no_one_item,
        ),
        (
            "the sort key pinned by an inequality",
            "SELECT * FROM keyed WHERE pk = 'a' AND sk > 0",
            no_one_item,
        ),
        (
            "a source that is an index",
            "SELECT * FROM \"keyed\".\"by-owner\" WHERE owner = 'x'",
            "Validation failed in TransactStatements[0]: Reads on indices are not supported \
             within transactions.",
        ),
    ];

    for (case, statement, expected) in rows {
        let (status, answered) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [{"Statement": statement}]}),
        )
        .await;
        // "top-level validation": the call is refused as a whole rather than reported as
        // a cancellation of a transaction that was attempted.
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case}: expected ValidationException, got {answered}"
        );
        assert_eq!(
            message(&answered),
            expected,
            "case {case}: expected the message {expected:?}, got {answered}"
        );
        assert_eq!(
            answered.get("CancellationReasons"),
            None,
            "case {case}: a top-level validation carries no cancellation, got {answered}"
        );
    }
}

/// One row per defect CLASS, to be stated the same way whichever entry point carried
/// the statement.
///
/// Every class here is one a statement earns while it is READ, so it is the statement's
/// own defect rather than anything about the request that carried it — which is why one
/// sentence answers for all three surfaces.
fn cross_surface_rows() -> Vec<Row> {
    let requires_path = |function: &str| {
        format!("Operator or function requires a document path; operator or function: {function}")
    };
    let operand_count = |function: &str, operands: usize| {
        format!(
            "Incorrect number of operands for operator or function; operator or function: \
             {function}, number of operands: {operands}"
        )
    };
    // 39 digits: one past the documented 38 significant digits an N carries.
    const THIRTY_NINE_DIGITS: &str = "123456789012345678901234567890123456789";
    let unknown_function = format!("SELECT * FROM {TABLE} WHERE made_up_function(pk)");
    vec![
        Row {
            case: "a misspelled verb",
            statement: format!("INSRT INTO \"{TABLE}\" VALUE {{'pk':'q'}}"),
            parameters: Value::Null,
            expected: parse_failure("Unexpected token after expression"),
        },
        Row {
            case: "a single-quoted table name",
            statement: format!("SELECT * FROM '{TABLE}'"),
            parameters: Value::Null,
            expected: parse_failure("Expected identifier for simple path"),
        },
        Row {
            case: "an unknown function",
            expected: format!(
                "Unrecognized function: made_up_function at {}",
                position_of(&unknown_function, "made_up_function")
            ),
            statement: unknown_function,
            parameters: Value::Null,
        },
        Row {
            case: "a function given a literal where it reads a path",
            statement: format!("SELECT * FROM {TABLE} WHERE attribute_exists('pk')"),
            parameters: Value::Null,
            expected: requires_path("attribute_exists"),
        },
        Row {
            case: "a call carrying one operand too many",
            statement: format!("SELECT * FROM {TABLE} WHERE size(pk, n) > 1"),
            parameters: Value::Null,
            expected: operand_count("size", 2),
        },
        Row {
            case: "a three-component FROM path",
            statement: format!("SELECT * FROM {TABLE}.a.b"),
            parameters: Value::Null,
            expected: "A path may contain at most 2 components in the FROM clause".to_string(),
        },
        Row {
            case: "a number literal past the stored precision",
            statement: format!("INSERT INTO {TABLE} VALUE {{'pk': 'z', 'n': {THIRTY_NINE_DIGITS}}}"),
            parameters: Value::Null,
            expected: "Attempting to store more than 38 significant digits in a Number".to_string(),
        },
        Row {
            case: "a parameter list that does not fill the markers",
            statement: format!("SELECT * FROM {TABLE} WHERE pk = ?"),
            parameters: json!([{"S": "a"}, {"S": "b"}]),
            expected: "Number of parameters in request and statement don't match.".to_string(),
        },
    ]
}

/// The statement one slot of a request carries, with its `Parameters` when the row
/// sends any: the shape `BatchStatementRequest` and `ParameterizedStatement` share.
fn slot_request(row: &Row) -> Value {
    let mut request = json!({"Statement": row.statement});
    if !row.parameters.is_null() {
        request["Parameters"] = row.parameters.clone();
    }
    request
}

#[tokio::test]
async fn one_class_states_one_sentence_whichever_surface_carried_the_statement() {
    let router = seeded().await;

    for row in cross_surface_rows() {
        // On its own: the sentence for the class, and nothing of the request that
        // carried it.
        let alone = refused(&router, row.case, slot_request(&row)).await;
        assert_eq!(
            alone, row.expected,
            "case {} through ExecuteStatement: expected {:?}, got {alone:?}",
            row.case, row.expected
        );

        // In a batch: the same sentence, in the slot that carried the statement, under
        // the per-slot code for a statement this service refused.
        let (status, answered) = call(
            &router,
            "BatchExecuteStatement",
            json!({"Statements": [slot_request(&row)]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {} through BatchExecuteStatement: a batch answers 200 whatever its slots \
             say, got {status} with {answered}",
            row.case
        );
        let slot = &answered["Responses"][0];
        assert_eq!(
            slot["Error"]["Code"].as_str(),
            Some("ValidationError"),
            "case {} through BatchExecuteStatement: expected the per-slot ValidationError \
             code, got {slot}",
            row.case
        );
        assert_eq!(
            slot["Error"]["Message"].as_str(),
            Some(row.expected.as_str()),
            "case {} through BatchExecuteStatement: expected the slot message {:?}, got {slot}",
            row.case,
            row.expected
        );

        // In a transaction: the same sentence, behind the slot of `TransactStatements`
        // the request stated the statement at.
        let (status, answered) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [slot_request(&row)]}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {} through ExecuteTransaction: expected HTTP 400, got {status} with {answered}",
            row.case
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {} through ExecuteTransaction: expected ValidationException, got {answered}",
            row.case
        );
        let prefixed = format!("Validation failed in TransactStatements[0]: {}", row.expected);
        assert_eq!(
            message(&answered),
            prefixed,
            "case {} through ExecuteTransaction: expected {prefixed:?}, got {answered}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_reported_position_is_counted_in_the_statements_own_lines_and_characters() {
    let router = seeded().await;

    // The rows differ in what the line and the column are counted off: a statement
    // written over several lines, one indented, one whose text before the offending run
    // holds characters wider than a byte, and one whose own string literal carries the
    // line break — so a row asserts the position is a fact of the TEXT rather than an
    // offset into it.
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "an unknown function on the third line",
            format!("SELECT *\nFROM {TABLE}\nWHERE made_up_function(pk)"),
            "made_up_function",
        ),
        (
            "an unknown function behind leading indentation",
            format!("SELECT *\n    FROM {TABLE}\n        WHERE other_fn(pk)"),
            "other_fn",
        ),
        (
            "an unknown function after characters wider than a byte",
            format!("SELECT * FROM {TABLE} WHERE n = 'piñata señal' AND magnitude(n)"),
            "magnitude",
        ),
        (
            "an unknown function after a literal that carries the line break",
            format!("SELECT * FROM {TABLE} WHERE n = 'one\ntwo' AND twice(n)"),
            "twice",
        ),
    ];
    for (case, statement, name) in &rows {
        let stated = refused(&router, case, json!({"Statement": statement})).await;
        let expected = format!(
            "Unrecognized function: {name} at {}",
            position_of(statement, name)
        );
        assert_eq!(
            stated, expected,
            "case {case}: statement {statement:?} expected {expected:?}, got {stated:?}"
        );
    }

    // The positioned sentence a write through an index qualifier earns reports where the
    // statement OPENS the source path, counted off the line that spells it: the rows
    // differ along which line that is and the indentation before it.
    for (case, statement) in [
        (
            "a source path on the second line",
            format!("INSERT\nINTO \"keyed\".\"{INDEX}\"\nVALUE {{'pk':'b','sk':2}}"),
        ),
        (
            "a source path on the third line",
            format!("INSERT\nINTO\n\"keyed\".\"{INDEX}\"\nVALUE {{'pk':'b','sk':2}}"),
        ),
        (
            "a source path behind leading indentation",
            format!("INSERT\n        INTO \"keyed\".\"{INDEX}\"\n    VALUE {{'pk':'b','sk':2}}"),
        ),
    ] {
        let stated = refused(&router, case, json!({"Statement": statement})).await;
        let expected = single_table_at(&statement, "\"keyed\"");
        assert_eq!(
            stated, expected,
            "case {case}: statement {statement:?} expected {expected:?}, got {stated:?}"
        );
    }
}

/// The sentence a WRITE whose source path carries an index qualifier earns, over every
/// way a statement can spell such a path.
///
/// RULE: a data-manipulation statement changes an item of ONE TABLE — an index holds a
/// projection of a table's items rather than items of its own — so a qualified source is
/// refused `ValidationException`, and WHICH sentence it states follows what the verb names
/// through that path. An `INSERT` names only WHERE a new item goes, inside its `INTO`
/// clause, so it is held to what that clause may spell: the single table name, stated with
/// the position of the TOKEN the path opens at, and at most two components at all, past
/// which the arity sentence is what the clause earns. An `UPDATE` and a `DELETE` ADDRESS a
/// stored item through the path they name, so a qualifier there asks for the write through
/// an index and states that. The same
/// two-component path is a legal source for a READ, which is the control below.
///
/// The rows differ along what the rule generalises over: the verb (`INSERT`, `UPDATE`,
/// `DELETE`), how each component is spelled (`"quoted"` or bare, and the two mixed), the
/// whitespace the path carries around its separator, how wide the qualifier spelled after
/// the opening token is (which the position must not follow), and how many components the
/// path holds (two and three). Every expected position is computed from the statement the
/// row sends.
#[tokio::test]
async fn a_write_through_a_qualified_source_names_the_rule_its_verb_broke() {
    let router = seeded().await;

    /// The sentence a write that ADDRESSES an item through a qualified path earns.
    const ON_AN_INDEX: &str = "This operation is not supported on an index";

    // Each two-component path, spelled four ways, once with whitespace around its
    // separator and once with a qualifier far wider than the table it qualifies, in each
    // of the three verbs that write. `opens` is the token the path begins with, which is
    // the whole of what the position reports however the rest of the path is spelled.
    for (spelling, path, opens) in [
        (
            "both components quoted",
            format!("\"keyed\".\"{INDEX}\""),
            "\"keyed\"",
        ),
        (
            "both components bare",
            "keyed.anyindex".to_string(),
            "keyed",
        ),
        (
            "a quoted table and a bare qualifier",
            "\"keyed\".qualifier".to_string(),
            "\"keyed\"",
        ),
        (
            "a bare table and a quoted qualifier",
            format!("keyed.\"{INDEX}\""),
            "keyed",
        ),
        (
            "whitespace around the separator",
            format!("\"keyed\" . \"{INDEX}\""),
            "\"keyed\"",
        ),
        (
            "a qualifier much wider than the table",
            "keyed.\"a-qualifier-wider-than-the-table-it-qualifies\"".to_string(),
            "keyed",
        ),
    ] {
        for (verb, statement, names_a_clause) in [
            (
                "INSERT",
                format!("INSERT INTO {path} VALUE {{'pk':'b','sk':2}}"),
                true,
            ),
            (
                "DELETE",
                format!("DELETE FROM {path} WHERE pk = 'a' AND sk = 1"),
                false,
            ),
            (
                "UPDATE",
                format!("UPDATE {path} SET note = 'two' WHERE pk = 'a' AND sk = 1"),
                false,
            ),
        ] {
            let case = format!("{verb} with {spelling}");
            let stated = refused(&router, &case, json!({"Statement": statement})).await;
            let expected = if names_a_clause {
                single_table_at(&statement, opens)
            } else {
                ON_AN_INDEX.to_string()
            };
            assert_eq!(
                stated, expected,
                "case {case:?}: statement {statement:?} expected {expected:?}, got {stated:?}"
            );
        }
    }

    // A path of THREE components is past what a source CLAUSE may spell, read or write.
    const PATH_ARITY: &str = "A path may contain at most 2 components in the FROM clause";
    for (case, statement) in [
        (
            "a three-component INSERT target",
            format!("INSERT INTO \"keyed\".\"{INDEX}\".\"extra\" VALUE {{'pk':'b','sk':2}}"),
        ),
        (
            "a three-component DELETE source",
            "DELETE FROM keyed.qual.more WHERE pk = 'a' AND sk = 1".to_string(),
        ),
        (
            "a three-component SELECT source",
            format!("SELECT * FROM \"keyed\".\"{INDEX}\".\"extra\""),
        ),
    ] {
        let stated = refused(&router, case, json!({"Statement": statement})).await;
        assert_eq!(
            stated, PATH_ARITY,
            "case {case}: statement {statement:?} expected {PATH_ARITY:?}, got {stated:?}"
        );
    }

    // An `UPDATE` has no such clause, so a longer qualifier still asks for the write
    // through an index.
    let statement = "UPDATE keyed.qual.more SET note = 'two' WHERE pk = 'a' AND sk = 1";    let stated = refused(
        &router,
        "a three-component UPDATE target",
        json!({"Statement": statement}),
    )
    .await;
    assert_eq!(
        stated, ON_AN_INDEX,
        "case a three-component UPDATE target: statement {statement:?} expected \
         {ON_AN_INDEX:?}, got {stated:?}"
    );

    // The controls the rule draws its line against: the SAME two-component path is a
    // legal source for a READ, and each write verb is served when its source names the
    // table alone.
    let read = ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!("SELECT * FROM \"keyed\".\"{INDEX}\" WHERE owner = 'x'")}),
    )
    .await;
    assert_eq!(
        read["Items"].as_array().map(Vec::len),
        Some(1),
        "case a read through the same qualified path: expected the seeded item, got {read}"
    );
    for (case, statement) in [
        (
            "an UPDATE naming the table alone",
            "UPDATE \"keyed\" SET note = 'two' WHERE pk = 'a' AND sk = 1",
        ),
        (
            "an INSERT naming the table alone",
            "INSERT INTO \"keyed\" VALUE {'pk':'b','sk':2}",
        ),
        (
            "a DELETE naming the table alone",
            "DELETE FROM \"keyed\" WHERE pk = 'a' AND sk = 1",
        ),
    ] {
        let (status, answered) =
            call(&router, "ExecuteStatement", json!({"Statement": statement})).await;
        assert_eq!(
            status, 200,
            "case {case}: statement {statement:?} must be served, got {status} with {answered}"
        );
    }
}

// ---------------------------------------------------------------------------
// WHERE a positioned sentence points
//
// RULE: a positioned sentence states the position of the single TOKEN the refused
// construct OPENS at — `<line>:<column>:<length>`, the line and column counted in
// characters from 1 and the length that token's own, in characters — so a construct
// spelled over several tokens (a source path and the qualifier after it, a call and its
// operands) is reported at its opening token and the text that follows moves neither the
// column nor the length.
// ---------------------------------------------------------------------------

/// The tables the rows below address. Their names differ in WIDTH, because the length a
/// refusal reports is the opening token's: a row whose table name is wide and whose
/// qualifier is narrow, and one spelled the other way round, report different lengths at
/// the same column. Both are spellable bare, so a row can vary the quoting too.
const NARROW_TABLE: &str = "tiny";
const WIDE_TABLE: &str = "orders_by_customer_and_created_at";

/// A qualifier far wider than either table name, so an expectation that followed the
/// whole path rather than its opening token could not agree with one that follows the
/// token.
const WIDE_QUALIFIER: &str = "a-qualifier-wider-than-the-table-it-qualifies";

/// An emulator holding the two tables of different name widths, each ready.
async fn two_widths() -> MultiServiceRouter {
    let router = emulator();
    for table in [NARROW_TABLE, WIDE_TABLE] {
        create_active_table(&router, table, &[("pk", "HASH", "S")]).await;
    }
    router
}

/// The sentence a write whose source clause names more than one table earns, over the
/// token `opens` that the clause's path begins with.
fn single_table_at(statement: &str, opens: &str) -> String {
    format!(
        "FROM clause may only contain a single table name in data manipulation statements at {}",
        position_of(statement, opens)
    )
}

#[tokio::test]
async fn a_qualified_write_source_is_reported_at_the_token_the_path_opens_with() {
    let router = two_widths().await;

    // Each row is a source path and the token it opens with: the rows differ along the
    // width of that token, the width of what the path spells after it, the quoting of
    // each component, and the characters the opening token itself spells — one of them
    // wider than a byte, since the length is counted in characters. The verb that names
    // where an item goes inside a clause runs every row; the verbs that ADDRESS an item
    // through the path report no position at all.
    let paths: Vec<(String, String, String)> = vec![
        (
            "a narrow bare table and a wide quoted qualifier".to_string(),
            format!("{NARROW_TABLE}.\"{WIDE_QUALIFIER}\""),
            NARROW_TABLE.to_string(),
        ),
        (
            "a wide quoted table and a narrow bare qualifier".to_string(),
            format!("\"{WIDE_TABLE}\".q"),
            format!("\"{WIDE_TABLE}\""),
        ),
        (
            "a wide bare table and a wide quoted qualifier".to_string(),
            format!("{WIDE_TABLE}.\"{WIDE_QUALIFIER}\""),
            WIDE_TABLE.to_string(),
        ),
        (
            "both components quoted".to_string(),
            format!("\"{WIDE_TABLE}\".\"{WIDE_QUALIFIER}\""),
            format!("\"{WIDE_TABLE}\""),
        ),
        (
            "whitespace around the separator".to_string(),
            format!("\"{WIDE_TABLE}\"   .   \"{WIDE_QUALIFIER}\""),
            format!("\"{WIDE_TABLE}\""),
        ),
        (
            "an opening token spelling characters wider than a byte".to_string(),
            "\"piñata-señal\".\"q\"".to_string(),
            "\"piñata-señal\"".to_string(),
        ),
    ];

    for (spelling, path, opens) in &paths {
        let statement = format!("INSERT INTO {path} VALUE {{'pk':'b'}}");
        let case = format!("INSERT with {spelling}");
        let stated = refused(&router, &case, json!({"Statement": statement.clone()})).await;
        let expected = single_table_at(&statement, opens);
        assert_eq!(
            stated, expected,
            "case {case:?}: statement {statement:?} expected {expected:?}, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn a_call_is_reported_at_the_name_it_opens_with_whatever_its_operands_are() {
    let router = two_widths().await;

    // The same rule on another construct: a call to a function the dialect does not
    // define is reported at the NAME, with the name's own width, so the operands after it
    // move neither the column nor the length. The rows differ along the width of the name
    // and the width of what follows it.
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a narrow name with one operand",
            format!("SELECT * FROM {NARROW_TABLE} WHERE fx(pk)"),
            "fx",
        ),
        (
            "a narrow name with wide operands",
            format!("SELECT * FROM {NARROW_TABLE} WHERE fx(pk, 'a long literal operand')"),
            "fx",
        ),
        (
            "a wide name with one operand",
            format!("SELECT * FROM {WIDE_TABLE} WHERE a_function_this_dialect_does_not_define(pk)"),
            "a_function_this_dialect_does_not_define",
        ),
    ];

    for (case, statement, name) in &rows {
        let stated = refused(&router, case, json!({"Statement": statement})).await;
        let expected = format!(
            "Unrecognized function: {name} at {}",
            position_of(statement, name)
        );
        assert_eq!(
            stated, expected,
            "case {case:?}: statement {statement:?} expected {expected:?}, got {stated:?}"
        );
    }
}
