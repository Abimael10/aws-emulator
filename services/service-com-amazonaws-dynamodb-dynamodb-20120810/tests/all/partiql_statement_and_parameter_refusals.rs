// SPDX-License-Identifier: CC0-1.0

//! The rules the PartiQL surface refuses a STATEMENT or a PARAMETER by, each asserted over
//! the set of inputs it holds for rather than over one statement.
//!
//! 1. A numeric PARAMETER is a number this service can CONVERT or it is not, and those are
//!    two different refusals: a text whose exponent is no signed run of decimal digits \u2014 or
//!    is a run no integer a decimal exponent is carried in can hold, which states a
//!    magnitude no number has \u2014 is reported as a parameter that cannot be converted to a
//!    numeric value (echoing the text), while a text whose exponent IS such a run states a
//!    magnitude, and a magnitude outside the documented 10^-130..10^125 range is reported as
//!    the over- or underflow it is, however many digits that exponent is written with. The
//!    rows walk both sides of the width the exponent is carried in and both ends of the
//!    documented range, on every surface that binds a parameter.
//! 2. A request that mixes reads and writes is refused as a whole, in the sentence the
//!    SURFACE states for its own shape: a batch and a transaction word it differently, and
//!    neither wording mentions the statements, so the rows vary how many statements the
//!    request carries and in which order the read sits.
//! 3. `IS <type>` is a predicate of the language: inside a `WHERE` clause it tests the TYPE
//!    of the attribute at that path, over the nine types the operator takes; a type the
//!    language spells that the operator does not take is refused naming both, a word that
//!    is no type of the language is refused for the position, and a statement that IS the
//!    test manipulates no data, so it names an operation this surface does not run.
//! 4. A transaction answers no per-statement image, so a `RETURNING` clause is refused
//!    behind the slot that carried it and the transaction commits nothing.
//! 5. A read is served over ONE range of its source's keys, so a `WHERE` clause whose `OR`
//!    branches each state their own KEY CONDITION on the sort key is refused rather than
//!    planned; a branch whose only mention of that key is in a form no key condition takes
//!    states no range and is a filter, so such a clause is served.
//! 6. `EXISTS(<select>)` is the check statement: the single statement surface and the batch
//!    refuse it, and a write transaction applies it as the condition it is.
//!
//! Everything is driven over the wire, and every write a rule refuses is read back through
//! `GetItem`, so nothing is asserted that a client could not observe.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table with a composite primary key every statement below addresses.
const PAIRED: &str = "PartiqlRefusalsPaired";

/// The table keyed on a partition key alone, for the statements that address one item of
/// a single-key table.
const SOLO: &str = "PartiqlRefusalsSolo";

/// A table carrying a global secondary index whose own key schema has a sort key, so the
/// range-key rule can be asserted against a SOURCE that is an index.
const INDEXED: &str = "PartiqlRefusalsIndexed";

/// That index's name.
const INDEX: &str = "by-owner";

/// The envelope a clause about a value an item-API member carried is stated inside.
const ENVELOPE: &str = "1 validation error detected: ";

/// An emulator holding the three tables above, each with one item in it.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(
        &router,
        PAIRED,
        &[("pk", "HASH", "S"), ("sk", "RANGE", "N")],
    )
    .await;
    create_active_table(&router, SOLO, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": INDEXED,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "owner", "AttributeType": "S"},
                {"AttributeName": "rank", "AttributeType": "N"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [{
                "IndexName": INDEX,
                "KeySchema": [
                    {"AttributeName": "owner", "KeyType": "HASH"},
                    {"AttributeName": "rank", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(&router, "the indexed refusal table", INDEXED).await;
    for (table, item) in [
        (
            PAIRED,
            json!({"pk": {"S": "p1"}, "sk": {"N": "1"}, "note": {"S": "n1"}}),
        ),
        (SOLO, json!({"pk": {"S": "p1"}, "note": {"S": "n1"}})),
        (
            INDEXED,
            json!({
                "pk": {"S": "p1"},
                "sk": {"N": "1"},
                "owner": {"S": "o1"},
                "rank": {"N": "1"},
            }),
        ),
    ] {
        ok(
            &router,
            "PutItem",
            json!({"TableName": table, "Item": item}),
        )
        .await;
    }
    router
}

// ===========================================================================
// 1. A numeric parameter is converted, or it is not
// ===========================================================================

/// The clause a text that is no number this service can convert earns, echoing the text as
/// the request sent it.
fn conversion_clause(text: &str) -> String {
    format!("The parameter cannot be converted to a numeric value: {text}")
}

/// The clauses a magnitude outside the documented exponent range earns, either way out.
const OVERFLOW_CLAUSE: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";
const UNDERFLOW_CLAUSE: &str =
    "Number underflow. Attempting to store a number with magnitude smaller than supported range";

/// The documented exponent range of a DynamoDB number: "Number: ... can have up to 38
/// digits of precision. Exponent range is 10^-130 to 10^125" ("Service, Account, and Table
/// Quotas"). The two limits of the integer the exponent is carried in are the 64-bit ones.
const MAX_EXPONENT: i32 = 125;
const MIN_EXPONENT: i32 = -130;
const MAX_SIGNED_64: &str = "9223372036854775807";
const MIN_SIGNED_64: &str = "-9223372036854775808";

#[tokio::test]
async fn a_numeric_parameter_outside_the_numeric_domain_states_the_rule_it_broke() {
    let router = seeded().await;

    // Each row: the case, the `N` text the parameter carries, and the whole sentence the
    // refusal must state. The rows walk the two sides of the width the exponent is carried
    // in — one past each of its limits, and far past it both ways — beside one past each
    // end of the documented range, a magnitude far out of range that the integer still
    // holds, a text that is no number at all, and a mantissa that normalises to zero under
    // an exponent outside the documented range.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "an exponent one past the top of the integer it is carried in",
            "1e9223372036854775808".to_string(),
            conversion_clause("1e9223372036854775808"),
        ),
        (
            "an exponent one past the bottom of that integer",
            "1e-9223372036854775809".to_string(),
            conversion_clause("1e-9223372036854775809"),
        ),
        (
            "an exponent far past the width of that integer",
            "1e99999999999999999999999".to_string(),
            conversion_clause("1e99999999999999999999999"),
        ),
        (
            "an exponent past the width of a 32-bit scale",
            "1e2147483648".to_string(),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "a negative exponent past that width",
            "1e-2147483649".to_string(),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent far past that width",
            "1e99999999999".to_string(),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent far past that width, the other way",
            "1e-99999999999".to_string(),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        (
            "a text that is no number at all",
            "not-a-number".to_string(),
            conversion_clause("not-a-number"),
        ),
        (
            "an exponent marker with nothing behind it",
            "1e".to_string(),
            conversion_clause("1e"),
        ),
        (
            "an exponent at the top of that integer, which states a magnitude",
            format!("1e{MAX_SIGNED_64}"),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent at the bottom of that integer, which states a magnitude",
            format!("1e{MIN_SIGNED_64}"),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent inside that integer but far past the documented range",
            "1e92233720368547758".to_string(),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "a magnitude one past the documented maximum",
            format!("1e{}", MAX_EXPONENT + 1),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "a magnitude one past the documented minimum",
            format!("1e{}", MIN_EXPONENT - 1),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        (
            "a zero carrying an exponent past the documented maximum",
            "0e126".to_string(),
            OVERFLOW_CLAUSE.to_string(),
        ),
    ];

    for (case, text, expected) in rows {
        // The same parameter on every surface that binds one: the single statement, the
        // transaction (which refuses the whole request behind the slot that carried it)
        // and the batch (which reports it in that statement's own slot).
        let statement = format!("SELECT * FROM \"{SOLO}\" WHERE pk = ?");
        let (status, body) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": statement, "Parameters": [{"N": text}]}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} through ExecuteStatement: expected {expected:?}, got {status} with \
             {body}"
        );

        let (status, body) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [
                {"Statement": statement.clone(), "Parameters": [{"N": text}]},
            ]}),
        )
        .await;
        // The clause is about the `Parameters` entry the request carried rather than
        // about the statement's own text, so it is stated on its own and not behind the
        // slot that carried the statement.
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} through ExecuteTransaction: expected {expected:?}, got {status} with \
             {body}"
        );

        let batched = ok(
            &router,
            "BatchExecuteStatement",
            json!({"Statements": [
                {"Statement": statement, "Parameters": [{"N": text}]},
            ]}),
        )
        .await;
        assert_eq!(
            (
                batched["Responses"][0]["Error"]["Code"].as_str(),
                batched["Responses"][0]["Error"]["Message"].as_str(),
            ),
            (Some("ValidationError"), Some(expected.as_str())),
            "case {case:?} through BatchExecuteStatement: expected the slot to state \
             {expected:?}, got {batched}"
        );
    }

    // The control: a parameter at each published bound of the range IS converted, so the
    // rows above are the bound being crossed and not a refusal of every exponent.
    for (case, text) in [
        ("the documented maximum exponent", format!("1e{MAX_EXPONENT}")),
        ("the documented minimum exponent", format!("1e{MIN_EXPONENT}")),
        ("an exponent well inside the range", "1e2".to_string()),
    ] {
        let served = ok(
            &router,
            "ExecuteStatement",
            json!({
                "Statement": format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = ?"),
                "Parameters": [{"N": text}],
            }),
        )
        .await;
        assert_eq!(
            served["Items"],
            json!([]),
            "case {case:?} ({text}): expected the statement served over no matching item, got \
             {served}"
        );
    }
}

// ===========================================================================
// 2. A request that mixes reads and writes
// ===========================================================================

#[tokio::test]
async fn a_request_mixing_reads_and_writes_is_refused_in_its_surfaces_own_sentence() {
    let router = seeded().await;
    let read = format!("SELECT * FROM \"{SOLO}\" WHERE pk = 'p1'");
    let write = format!("UPDATE \"{SOLO}\" SET note = 'two' WHERE pk = 'p1'");
    let insert = format!("INSERT INTO \"{SOLO}\" VALUE {{'pk': 'fresh'}}");

    // Each row: the case, and the statements the request carries — the read first, the
    // read last, and a request carrying more writes than reads.
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("the read first", vec![read.clone(), write.clone()]),
        ("the read last", vec![write.clone(), read.clone()]),
        (
            "the read between two writes",
            vec![write.clone(), read.clone(), insert.clone()],
        ),
    ];

    for (case, statements) in rows {
        let entries: Vec<Value> = statements
            .iter()
            .map(|text| json!({"Statement": text}))
            .collect();
        let (status, body) = call(
            &router,
            "BatchExecuteStatement",
            json!({"Statements": entries.clone()}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (
                400,
                "ValidationException",
                "Read and write requests together in the same batch is not supported.".to_string(),
            ),
            "case {case:?} through BatchExecuteStatement: got {status} with {body}"
        );

        let (status, body) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": entries}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (
                400,
                "ValidationException",
                "ExecuteTransaction API does not support both read and write operations in the \
                 same request."
                    .to_string(),
            ),
            "case {case:?} through ExecuteTransaction: got {status} with {body}"
        );
    }

    // Nothing of a refused request was applied, on either surface.
    let stored = ok(
        &router,
        "GetItem",
        json!({"TableName": SOLO, "Key": {"pk": {"S": "p1"}}}),
    )
    .await;
    assert_eq!(
        stored["Item"]["note"],
        json!({"S": "n1"}),
        "case a mixed request: the write beside the read applied nothing, got {stored}"
    );
    let fresh = ok(
        &router,
        "GetItem",
        json!({"TableName": SOLO, "Key": {"pk": {"S": "fresh"}}}),
    )
    .await;
    assert_eq!(
        fresh.get("Item"),
        None,
        "case a mixed request: the insert beside the read applied nothing, got {fresh}"
    );
}

#[tokio::test]
async fn a_statement_list_that_is_empty_is_quoted_as_the_empty_list() {
    let router = seeded().await;

    // The member, the operation that carries it, and the path the clause names it by.
    for (case, operation, member, path) in [
        (
            "a batch carrying no statement",
            "BatchExecuteStatement",
            "Statements",
            "statements",
        ),
        (
            "a transaction carrying no statement",
            "ExecuteTransaction",
            "TransactStatements",
            "transactStatements",
        ),
    ] {
        let (status, body) = call(&router, operation, json!({member: []})).await;
        let expected = format!(
            "{ENVELOPE}Value '[]' at '{path}' failed to satisfy constraint: Member must have \
             length greater than or equal to 1"
        );
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {body}"
        );
    }
}

// ===========================================================================
// 3. The type test
// ===========================================================================

/// The nine types the `IS` operator takes, each with the keywords that spell it and the
/// attribute of the stored item that IS of that type. Source: the refusal AWS states for a
/// type it does not take, which lists `[MISSING, NULL, BOOLEAN, BLOB, STRING, NUMERIC,
/// STRUCT, TUPLE, LIST]`.
fn type_rows() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        // (the keyword the statement spells, the attribute of that type, another one)
        ("BOOLEAN", "flag", "text"),
        ("BOOL", "flag", "text"),
        ("BLOB", "bytes", "text"),
        ("STRING", "text", "number"),
        ("NUMERIC", "number", "text"),
        ("STRUCT", "entries", "members"),
        ("TUPLE", "entries", "members"),
        ("LIST", "members", "entries"),
        ("NULL", "nothing", "text"),
    ]
}

#[tokio::test]
async fn a_where_clause_type_test_reports_the_items_whose_attribute_has_that_type() {
    let router = seeded().await;
    // One item carrying one attribute of every type the operator can test for. The
    // attribute names are names and not keywords of the language, which is what an
    // unquoted path may be.
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": SOLO,
            "Item": {
                "pk": {"S": "typed"},
                "flag": {"BOOL": true},
                "bytes": {"B": "YmxvYg=="},
                "text": {"S": "s"},
                "number": {"N": "1"},
                "entries": {"M": {"inner": {"S": "m"}}},
                "members": {"L": [{"S": "l"}]},
                "nothing": {"NULL": true},
            },
        }),
    )
    .await;

    for (keyword, matching, other) in type_rows() {
        // The attribute that IS of that type satisfies the test, and the one that is not
        // does not — so the predicate reads the stored type rather than passing everything.
        let answered = ok(
            &router,
            "ExecuteStatement",
            json!({
                "Statement": format!(
                    "SELECT pk FROM \"{SOLO}\" WHERE pk = 'typed' AND {matching} IS {keyword}"
                ),
            }),
        )
        .await;
        assert_eq!(
            answered["Items"],
            json!([{"pk": {"S": "typed"}}]),
            "case IS {keyword} over {matching}: expected the item reported, got {answered}"
        );

        let answered = ok(
            &router,
            "ExecuteStatement",
            json!({
                "Statement": format!(
                    "SELECT pk FROM \"{SOLO}\" WHERE pk = 'typed' AND {other} IS {keyword}"
                ),
            }),
        )
        .await;
        assert_eq!(
            answered["Items"],
            json!([]),
            "case IS {keyword} over {other}: expected no item reported, got {answered}"
        );

        // `IS NOT <type>` is the negation of the same test.
        let answered = ok(
            &router,
            "ExecuteStatement",
            json!({
                "Statement": format!(
                    "SELECT pk FROM \"{SOLO}\" WHERE pk = 'typed' AND {other} IS NOT {keyword}"
                ),
            }),
        )
        .await;
        assert_eq!(
            answered["Items"],
            json!([{"pk": {"S": "typed"}}]),
            "case IS NOT {keyword} over {other}: expected the item reported, got {answered}"
        );
    }

    // `IS MISSING` is the test for an attribute the item does not carry at all, which is
    // the ninth type the operator takes.
    let answered = ok(
        &router,
        "ExecuteStatement",
        json!({
            "Statement": format!(
                "SELECT pk FROM \"{SOLO}\" WHERE pk = 'typed' AND unseen IS MISSING"
            ),
        }),
    )
    .await;
    assert_eq!(
        answered["Items"],
        json!([{"pk": {"S": "typed"}}]),
        "case IS MISSING over an attribute the item does not carry: got {answered}"
    );
}

/// Every type the operator takes, as the refusal of one it does not lists them.
const SUPPORTED_TYPES: &str = "[MISSING, NULL, BOOLEAN, BLOB, STRING, NUMERIC, STRUCT, TUPLE, LIST]";

#[tokio::test]
async fn a_type_test_naming_a_type_the_operator_does_not_take_names_both() {
    let router = seeded().await;

    // Each row: the type keywords the statement spells, and the language's name for the
    // type they name. The rows span the numeric widths that share spellings, the two
    // character types, the types with no DynamoDB counterpart at all, and a type spelled
    // in two keywords. Source: the type vocabulary of the statement language.
    let rows: Vec<(&str, &str)> = vec![
        ("INT", "IntegerType"),
        ("INTEGER", "IntegerType"),
        ("INT2", "SmallintType"),
        ("SMALLINT", "SmallintType"),
        ("INT4", "Integer4Type"),
        ("INT8", "Integer8Type"),
        ("BIGINT", "Integer8Type"),
        ("DECIMAL", "DecimalType"),
        ("FLOAT", "FloatType"),
        ("REAL", "RealType"),
        ("DOUBLE PRECISION", "DoublePrecisionType"),
        ("CHAR", "CharacterType"),
        ("CHARACTER", "CharacterType"),
        ("CHARACTER VARYING", "CharacterVaryingType"),
        ("VARCHAR", "CharacterVaryingType"),
        ("SYMBOL", "SymbolType"),
        ("CLOB", "ClobType"),
        ("BAG", "BagType"),
        ("SEXP", "SexpType"),
        ("DATE", "DateType"),
        ("TIME", "TimeType"),
        ("TIMESTAMP", "TimestampType"),
        ("ANY", "AnyType"),
    ];

    for (keyword, named) in rows {
        // The clause the test opens at is `n`, one character wide, at the column the
        // statement spells it — every part of the position is a fact of the text.
        let prefix = format!("SELECT pk FROM \"{SOLO}\" WHERE pk = 'p1' AND ");
        let column = prefix.chars().count() + 1;
        let (status, body) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": format!("{prefix}n IS {keyword}")}),
        )
        .await;
        let expected = format!(
            "Unsupported data type for IS operator: {named}. Supported data types are \
             {SUPPORTED_TYPES} at 1:{column}:1"
        );
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case IS {keyword}: expected {expected:?}, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn the_type_position_of_a_test_must_name_a_type_of_the_language() {
    let router = seeded().await;

    // Each row: the case, the statement, and the whole sentence it earns. A test whose
    // type position names no type of the language is refused for THAT position; a
    // statement that IS the test names an operation this surface does not run, whichever
    // type it tests for.
    let parse_defect = |phrase: &str| {
        format!("Statement wasn't well formed, can't be processed: {phrase}")
    };
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a word that is no type, in a WHERE clause",
            format!("SELECT pk FROM \"{SOLO}\" WHERE pk = 'p1' AND n IS NOTATYPE"),
            parse_defect("Expected type name"),
        ),
        (
            "a literal where a type belongs",
            format!("SELECT pk FROM \"{SOLO}\" WHERE pk = 'p1' AND n IS 'STRING'"),
            parse_defect("Expected type name"),
        ),
        (
            "nothing at all after the operator",
            format!("SELECT pk FROM \"{SOLO}\" WHERE pk = 'p1' AND n IS"),
            parse_defect("Expected type name"),
        ),
        (
            "a word that is no type, as the whole statement",
            "n IS NOTATYPE".to_string(),
            parse_defect("Expected type name"),
        ),
        (
            "the test as the whole statement, over a type the operator takes",
            "n IS STRING".to_string(),
            "Unsupported operation: IsType".to_string(),
        ),
        (
            "the test as the whole statement, over a type it does not take",
            "n IS INT4".to_string(),
            "Unsupported operation: IsType".to_string(),
        ),
        (
            "the negated test as the whole statement",
            "n IS NOT MISSING".to_string(),
            "Unsupported operation: IsType".to_string(),
        ),
    ];

    for (case, statement, expected) in rows {
        let (status, body) = call(&router, "ExecuteStatement", json!({"Statement": statement})).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn a_call_to_a_function_the_dialect_does_not_define_names_it_in_lower_case() {
    let router = seeded().await;

    // A function name is matched without regard to case, so the name the refusal states is
    // the one the parser resolved: folded to lower case, whichever case the statement
    // spelled it in. The position is the statement's own.
    for (case, spelled) in [
        ("an upper-case name", "MADE_UP"),
        ("a mixed-case name", "Made_Up"),
        ("a lower-case name", "made_up"),
    ] {
        let prefix = format!("SELECT pk FROM \"{SOLO}\" WHERE ");
        let column = prefix.chars().count() + 1;
        let (status, body) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": format!("{prefix}{spelled}(pk)")}),
        )
        .await;
        let expected = format!(
            "Unrecognized function: made_up at 1:{column}:{}",
            spelled.chars().count()
        );
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {body}"
        );
    }
}

// ===========================================================================
// 4. A RETURNING clause inside a transaction
// ===========================================================================

#[tokio::test]
async fn a_transaction_refusing_a_returning_clause_commits_nothing() {
    let router = seeded().await;

    // Each row: the case, the statements the transaction carries, and the slot the clause
    // sits in. The rows span the verb that states the clause, the form it asks for, and
    // whether a well-formed statement sits before it.
    let rows: Vec<(&str, Vec<String>, usize)> = vec![
        (
            "an UPDATE asking for the image it leaves",
            vec![format!(
                "UPDATE \"{SOLO}\" SET note = 'two' WHERE pk = 'p1' RETURNING ALL NEW *"
            )],
            0,
        ),
        (
            "a DELETE asking for the item it removed",
            vec![format!(
                "DELETE FROM \"{SOLO}\" WHERE pk = 'p1' RETURNING ALL OLD *"
            )],
            0,
        ),
        (
            "the second statement of two asking for an image",
            vec![
                format!("UPDATE \"{SOLO}\" SET note = 'two' WHERE pk = 'p1'"),
                format!(
                    "UPDATE \"{PAIRED}\" SET note = 'two' WHERE pk = 'p1' AND sk = 1 RETURNING \
                     MODIFIED OLD *"
                ),
            ],
            1,
        ),
    ];

    for (case, statements, slot) in rows {
        let entries: Vec<Value> = statements
            .iter()
            .map(|text| json!({"Statement": text}))
            .collect();
        let (status, body) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": entries}),
        )
        .await;
        let expected = format!(
            "Validation failed in TransactStatements[{slot}]: RETURNING clause is not supported \
             in ExecuteTransaction."
        );
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {body}"
        );
        assert_eq!(
            body.get("CancellationReasons"),
            None,
            "case {case:?}: a refusal of the request carries no cancellation, got {body}"
        );
    }

    // The refusals above committed nothing: both items still hold what they held before.
    for (table, key) in [
        (SOLO, json!({"pk": {"S": "p1"}})),
        (PAIRED, json!({"pk": {"S": "p1"}, "sk": {"N": "1"}})),
    ] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": table, "Key": key}),
        )
        .await;
        assert_eq!(
            read["Item"]["note"],
            json!({"S": "n1"}),
            "case a refused clause: {table} must still hold its pre-transaction value, got {read}"
        );
    }

    // The control: the same statements WITHOUT the clause are applied, so the refusals
    // above are the clause and not the write.
    ok(
        &router,
        "ExecuteTransaction",
        json!({"TransactStatements": [
            {"Statement": format!("UPDATE \"{SOLO}\" SET note = 'two' WHERE pk = 'p1'")},
        ]}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": SOLO, "Key": {"pk": {"S": "p1"}}}),
    )
    .await;
    assert_eq!(
        read["Item"]["note"],
        json!({"S": "two"}),
        "case the same statement without the clause: the write landed, got {read}"
    );
}

// ===========================================================================
// 5. Overlapping conditions on the source's range key
// ===========================================================================

/// The sentence a `WHERE` clause whose `OR` branches each ask for their own range of the
/// sort key earns.
const OVERLAPPING_RANGE_KEYS: &str =
    "Overlapping conditions with range keys are not supported in where clause";

#[tokio::test]
async fn a_where_clause_stating_a_range_key_inside_a_disjunction_is_refused() {
    let router = seeded().await;

    // Each row: the case and the statement. The rows span which branch names the sort key,
    // what the other branch states, the operator the branch uses, how many branches there
    // are, whether the disjunction is nested inside a conjunction, and whether the source
    // is the table or one of its indexes (whose OWN sort key is the one that counts).
    let rows: Vec<(&str, String)> = vec![
        (
            "the sort key in the first branch of two",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND (sk = 1 OR note = 'n1')"),
        ),
        (
            "the sort key in the second branch of two",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND (note = 'n1' OR sk = 1)"),
        ),
        (
            "the sort key under an inequality in a branch",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND (sk > 0 OR note = 'n1')"),
        ),
        (
            "the sort key inside a BETWEEN in a branch",
            format!(
                "SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND (sk BETWEEN 0 AND 9 OR note = \
                 'n1')"
            ),
        ),
        (
            "the sort key in one of three branches",
            format!(
                "SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND (note = 'x' OR note = 'y' OR sk \
                 = 1)"
            ),
        ),
        (
            "the whole clause a disjunction naming the sort key",
            format!("SELECT * FROM \"{PAIRED}\" WHERE sk = 1 OR note = 'n1'"),
        ),
        (
            "both branches naming the sort key",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND (sk = 1 OR sk = 2)"),
        ),
        (
            "the sort key inside a NOT over a disjunction",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND NOT (sk = 1 OR note = 'n1')"),
        ),
        (
            "the INDEX's own sort key in a branch",
            format!(
                "SELECT * FROM \"{INDEXED}\".\"{INDEX}\" WHERE owner = 'o1' AND (rank = 1 OR \
                 note = 'n1')"
            ),
        ),
    ];

    for (case, statement) in rows {
        let (status, body) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": statement.clone()}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (
                400,
                "ValidationException",
                OVERLAPPING_RANGE_KEYS.to_string(),
            ),
            "case {case:?} ({statement}): expected the clause refused, got {status} with {body}"
        );
    }

    // The controls: a clause whose disjunction names no key of the source is served, so is
    // one that states the sort key in the top-level conjunction (one range), one whose
    // sort-key branch states no KEY CONDITION — a negation or an inequality narrows no
    // range of keys, so the clause is served as the union of its branches — and so is a
    // disjunction over the TABLE's sort key when the source is the index it is not a key
    // of.
    for (case, statement, items) in [
        (
            "a disjunction over no key attribute",
            format!("SELECT pk FROM \"{PAIRED}\" WHERE pk = 'p1' AND (note = 'n1' OR note = 'x')"),
            json!([{"pk": {"S": "p1"}}]),
        ),
        (
            "the sort key in the top-level conjunction",
            format!("SELECT pk FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 1 AND note = 'n1'"),
            json!([{"pk": {"S": "p1"}}]),
        ),
        (
            "a negated sort-key branch, which states no key condition",
            format!(
                "SELECT pk FROM \"{PAIRED}\" WHERE pk = 'p1' AND (NOT sk = 2 OR note = 'x')"
            ),
            json!([{"pk": {"S": "p1"}}]),
        ),
        (
            "a bare partition branch beside a negated sort-key branch",
            format!(
                "SELECT pk FROM \"{PAIRED}\" WHERE pk = 'p1' OR (pk = 'p1' AND NOT sk = 1)"
            ),
            json!([{"pk": {"S": "p1"}}]),
        ),
        (
            "a sort-key branch stating an inequality, which narrows no range",
            format!("SELECT pk FROM \"{PAIRED}\" WHERE pk = 'p1' AND (sk <> 1 OR note = 'n1')"),
            json!([{"pk": {"S": "p1"}}]),
        ),
        (
            "a disjunction over the single-key table, which has no sort key at all",
            format!("SELECT pk FROM \"{SOLO}\" WHERE pk = 'p1' AND (note = 'n1' OR note = 'x')"),
            json!([{"pk": {"S": "p1"}}]),
        ),
        (
            "a disjunction over the table's sort key, read through an index it is not a key of",
            format!(
                "SELECT pk FROM \"{INDEXED}\".\"{INDEX}\" WHERE owner = 'o1' AND (sk = 1 OR sk = \
                 2)"
            ),
            json!([{"pk": {"S": "p1"}}]),
        ),
    ] {
        let served = ok(
            &router,
            "ExecuteStatement",
            json!({"Statement": statement.clone()}),
        )
        .await;
        assert_eq!(
            served["Items"], items,
            "case {case:?} ({statement}): expected {items}, got {served}"
        );
    }
}

// ===========================================================================
// 6. The EXISTS check statement
// ===========================================================================

#[tokio::test]
async fn an_exists_check_is_served_only_as_part_of_a_write_transaction() {
    let router = seeded().await;
    let check = format!("EXISTS(SELECT * FROM \"{SOLO}\" WHERE pk = 'p1' AND note = 'n1')");

    // The single statement surface serves no write for a check to guard, so it refuses it;
    // the batch applies each statement on its own, so its slot does.
    let (status, body) = call(
        &router,
        "ExecuteStatement",
        json!({"Statement": check.clone()}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str(), message(&body)),
        (
            400,
            "ValidationException",
            "EXISTS can only be used in ExecuteTransaction write requests.".to_string(),
        ),
        "case a check through ExecuteStatement: got {status} with {body}"
    );
    let batched = ok(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": [{"Statement": check.clone()}]}),
    )
    .await;
    assert_eq!(
        (
            batched["Responses"][0]["Error"]["Code"].as_str(),
            batched["Responses"][0]["Error"]["Message"].as_str(),
        ),
        (
            Some("ValidationError"),
            Some("EXISTS can only be used in ExecuteTransaction write requests."),
        ),
        "case a check through BatchExecuteStatement: got {batched}"
    );

    // A check whose clause does not name ONE item under a further condition has nothing to
    // check, whichever half is missing.
    for (case, inner) in [
        (
            "no WHERE clause at all",
            format!("SELECT * FROM \"{PAIRED}\""),
        ),
        (
            "only part of the key",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1'"),
        ),
        (
            "the whole key and nothing besides it",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 1"),
        ),
    ] {
        let (status, body) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [
                {"Statement": format!("EXISTS({inner})")},
                {"Statement": format!("UPDATE \"{SOLO}\" SET note = 'two' WHERE pk = 'p1'")},
            ]}),
        )
        .await;
        let expected = "Validation failed in TransactStatements[0]: EXISTS() must contain a \
                        single item read with additional condition";
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.to_string()),
            "case a check stating {case:?}: expected {expected:?}, got {status} with {body}"
        );
    }

    // A check that HOLDS lets the write beside it land, and the check itself writes
    // nothing: the item it is about is untouched.
    ok(
        &router,
        "ExecuteTransaction",
        json!({"TransactStatements": [
            {"Statement": format!(
                "EXISTS(SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 1 AND note = 'n1')"
            )},
            {"Statement": format!("UPDATE \"{SOLO}\" SET note = 'guarded' WHERE pk = 'p1'")},
        ]}),
    )
    .await;
    let guarded = ok(
        &router,
        "GetItem",
        json!({"TableName": SOLO, "Key": {"pk": {"S": "p1"}}}),
    )
    .await;
    assert_eq!(
        guarded["Item"]["note"],
        json!({"S": "guarded"}),
        "case a check that holds: the write beside it landed, got {guarded}"
    );
    let checked = ok(
        &router,
        "GetItem",
        json!({"TableName": PAIRED, "Key": {"pk": {"S": "p1"}, "sk": {"N": "1"}}}),
    )
    .await;
    assert_eq!(
        checked["Item"],
        json!({"pk": {"S": "p1"}, "sk": {"N": "1"}, "note": {"S": "n1"}}),
        "case a check that holds: the item it read is untouched, got {checked}"
    );

    // A check that does NOT hold cancels the transaction, so the write beside it applies
    // nothing — whether the condition fails or the key holds no item at all.
    for (case, inner) in [
        (
            "a condition the stored item fails",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'p1' AND sk = 1 AND note = 'other'"),
        ),
        (
            "a key that holds no item",
            format!("SELECT * FROM \"{PAIRED}\" WHERE pk = 'ghost' AND sk = 1 AND note = 'n1'"),
        ),
    ] {
        let (status, body) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [
                {"Statement": format!("EXISTS({inner})")},
                {"Statement": format!("UPDATE \"{SOLO}\" SET note = 'unguarded' WHERE pk = 'p1'")},
            ]}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "TransactionCanceledException"),
            "case a check stating {case:?}: expected the transaction cancelled, got {status} \
             with {body}"
        );
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": SOLO, "Key": {"pk": {"S": "p1"}}}),
        )
        .await;
        assert_eq!(
            read["Item"]["note"],
            json!({"S": "guarded"}),
            "case a check stating {case:?}: the write beside it applied nothing, got {read}"
        );
    }
}
