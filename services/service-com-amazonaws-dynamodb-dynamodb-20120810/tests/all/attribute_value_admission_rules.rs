// SPDX-License-Identifier: CC0-1.0

//! Three rules about the VALUE a request carries, each held wherever a value arrives.
//!
//! * The `NULL` member of an attribute value states the attribute's TYPE, and the only
//!   value that states it is `true` ("An attribute of type Null. For example:
//!   `\"NULL\": true`" — `AttributeValue$NULL`), so a `false` is refused and nothing is
//!   stored. The rows span the write surfaces a value can arrive through, where in the
//!   value it sits (top level, inside a list, inside a map, as a set-free scalar of an
//!   update) and the read surface that compares against one.
//! * A number's EXPONENT is a signed run of decimal digits carried in a 64-bit integer, and
//!   only then held to the documented range, so the two defects stay apart: a run that
//!   integer can hold states a MAGNITUDE, which over- or underflows that range, and only a
//!   text whose exponent is no such run — or a run no such integer can hold, which states a
//!   magnitude no number has — cannot be converted to a number at all. Padding zeros denote
//!   the same exponent however many there are, so the text's WIDTH decides nothing. The rows
//!   walk both published bounds, one past each of them, the limits of the integer the
//!   exponent is carried in and one past each of those, far past them, and an exponent
//!   padded with a long run of leading zeros.
//! * A value's NESTING is bounded wherever it is read: one inside the documented quota
//!   ("DynamoDB supports nested attributes up to 32 levels deep") is stored, and one past
//!   it is refused with a 400 `ValidationException` and stores nothing — including one
//!   nested far past every bound the read of a value is held to, which is answered rather
//!   than walked. The rows walk both containers a value nests through.
//!
//! Everything is driven over the wire, so nothing is asserted that a client could not
//! observe.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below writes to.
const TABLE: &str = "ValueAdmission";

/// The envelope a clause about a value an item-API member carried is stated inside.
const ENVELOPE: &str = "1 validation error detected: ";

/// The clause a `NULL` that does not state `true` earns.
const NULL_CLAUSE: &str = "One or more parameter values were invalid: Null attribute value types \
                           must have the value of true";

/// The documented exponent range of a DynamoDB number: "Number: ... can have up to 38
/// digits of precision. Exponent range is 10^-130 to 10^125" ("Service, Account, and
/// Table Quotas").
const MAX_EXPONENT: i32 = 125;
const MIN_EXPONENT: i32 = -130;

/// The two limits of the signed 32-bit integer a decimal exponent is held in, as a text
/// spells them, and one magnitude far past any integer.
const MAX_SIGNED_32: &str = "2147483647";
const MIN_SIGNED_32: &str = "-2147483648";
const PAST_ANY_INTEGER: &str = "9223372036854775808";

/// The clause a magnitude outside the documented range earns, either way out.
const OVERFLOW_CLAUSE: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";
const UNDERFLOW_CLAUSE: &str =
    "Number underflow. Attempting to store a number with magnitude smaller than supported range";

/// The clause a text that is no number at all earns, echoing the literal as sent.
fn conversion_clause(text: &str) -> String {
    format!("The parameter cannot be converted to a numeric value: {text}")
}

/// An emulator holding one active table keyed on `pk` alone.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
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
    describe_when_active(&router, "the value-admission table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "seed"}, "a": {"S": "x"}}}),
    )
    .await;
    router
}

#[tokio::test]
async fn a_value_nested_past_what_the_read_descends_is_refused_and_stores_nothing() {
    let router = seeded().await;

    // Rows walk the nesting a request can carry, from inside the documented quota
    // ("DynamoDB supports nested attributes up to 32 levels deep") to past every bound the
    // read is held to: the one inside the quota is stored, each one past it is refused with
    // a 400 `ValidationException`, and the deep ones are ANSWERED rather than driving the
    // read down a stack until the process dies. The two containers a value nests through
    // are both walked, since the read descends through each.
    let rows: [(&str, usize, bool); 6] = [
        ("a list inside the documented quota", 30, true),
        ("a map inside the documented quota", 30, true),
        ("a list one level past the quota", 33, false),
        ("a map one level past the quota", 33, false),
        ("a list past what the read descends", 200, false),
        ("a map past what the read descends", 200, false),
    ];

    for (index, (case, levels, admitted)) in rows.into_iter().enumerate() {
        let mut value = json!({"S": "bottom"});
        for _ in 0..levels {
            value = if case.starts_with("a list") {
                json!({"L": [value]})
            } else {
                json!({"M": {"inner": value}})
            };
        }
        let key = format!("deep-{index}");
        let (status, answered) = call(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": &key}, "tree": value}}),
        )
        .await;
        if admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: a value inside the documented nesting quota is stored, got \
                 {status} with {answered}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "ValidationException"),
                "case {case:?}: a value nested {levels} levels deep is refused, got {status} \
                 with {answered}"
            );
            assert!(
                !message(&answered).is_empty(),
                "case {case:?}: the refusal states why, got {answered}"
            );
        }
        // Whatever the answer, the store agrees with it: a refused write left nothing
        // behind, and an admitted one is readable back.
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": &key}}}),
        )
        .await;
        assert_eq!(
            read["Item"].is_null(),
            !admitted,
            "case {case:?}: expected the item to be stored only where the write was \
             admitted, got {read}"
        );
    }
}

#[tokio::test]
async fn a_null_attribute_value_must_state_true() {
    let router = seeded().await;

    // Each row: the case, the operation, the request, and the whole message it must earn
    // — the envelope follows the surface the value arrived through, as every other value
    // clause of this service does.
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a top-level NULL of a put item",
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": "n1"}, "flag": {"NULL": false}},
            }),
            format!("{ENVELOPE}{NULL_CLAUSE}"),
        ),
        (
            "a NULL inside a list",
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": "n2"}, "flags": {"L": [{"BOOL": true}, {"NULL": false}]}},
            }),
            format!("{ENVELOPE}{NULL_CLAUSE}"),
        ),
        (
            "a NULL two levels down, inside a map inside a list",
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {
                    "pk": {"S": "n3"},
                    "nested": {"L": [{"M": {"flag": {"NULL": false}}}]},
                },
            }),
            format!("{ENVELOPE}{NULL_CLAUSE}"),
        ),
        (
            "a NULL an update puts",
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": {"pk": {"S": "seed"}},
                "AttributeUpdates": {"flag": {"Action": "PUT", "Value": {"NULL": false}}},
            }),
            format!("{ENVELOPE}{NULL_CLAUSE}"),
        ),
        (
            "a NULL an update expression binds",
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": {"pk": {"S": "seed"}},
                "UpdateExpression": "SET flag = :v",
                "ExpressionAttributeValues": {":v": {"NULL": false}},
            }),
            format!("{ENVELOPE}{NULL_CLAUSE}"),
        ),
        (
            "a NULL a batched put carries",
            "BatchWriteItem",
            json!({
                "RequestItems": {
                    TABLE: [{"PutRequest": {"Item": {"pk": {"S": "n4"},
                                                     "flag": {"NULL": false}}}}],
                },
            }),
            format!("{ENVELOPE}{NULL_CLAUSE}"),
        ),
        (
            // A value in ONE ELEMENT of a transaction is that element's member rather
            // than the request's, so the clause is stated alone — the same split the
            // element's `Key` and its expression members follow.
            "a NULL a transactional put carries",
            "TransactWriteItems",
            json!({
                "TransactItems": [
                    {"Put": {"TableName": TABLE,
                             "Item": {"pk": {"S": "n5"}, "flag": {"NULL": false}}}},
                ],
            }),
            NULL_CLAUSE.to_string(),
        ),
        (
            // A read states its value clauses bare, which is the same split every other
            // value rule of this service follows.
            "a NULL a scan's filter compares against",
            "Scan",
            json!({
                "TableName": TABLE,
                "FilterExpression": "flag = :v",
                "ExpressionAttributeValues": {":v": {"NULL": false}},
            }),
            NULL_CLAUSE.to_string(),
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, answered) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {answered}"
        );
    }

    // Nothing a refused write carried was stored: every key those rows named holds no
    // item, and the seeded item carries no `flag`.
    for key in ["n1", "n2", "n3", "n4", "n5"] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
        )
        .await;
        assert_eq!(
            read.get("Item"),
            None,
            "case a refused NULL at {key:?}: a refused write stores nothing, got {read}"
        );
    }
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "seed"}}}),
    )
    .await;
    assert_eq!(
        read["Item"].get("flag"),
        None,
        "case a refused update: the attribute is not written, got {read}"
    );

    // The one value the member takes is stored and read back as itself, wherever it
    // sits — so the rows above are the value `false` being refused rather than the type
    // being rejected.
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {
                "pk": {"S": "ok"},
                "flag": {"NULL": true},
                "nested": {"L": [{"M": {"flag": {"NULL": true}}}]},
            },
        }),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "ok"}}}),
    )
    .await;
    assert_eq!(
        (
            read["Item"]["flag"].clone(),
            read["Item"]["nested"]["L"][0]["M"]["flag"].clone()
        ),
        (json!({"NULL": true}), json!({"NULL": true})),
        "case a NULL stating true: expected it read back as itself, got {read}"
    );
}

/// The documented depth a value may nest to: "DynamoDB supports nested attributes up to 32
/// levels deep" ("Service, Account, and Table Quotas").
const MAX_ATTRIBUTE_DEPTH: usize =
    service_com_amazonaws_dynamodb_dynamodb_20120810::state::MAX_ATTRIBUTE_DEPTH;

/// The clause a value nesting past that depth earns.
const NESTING_CLAUSE: &str = "Nesting Levels have exceeded supported limits: Attributes in the \
                              item have nested levels beyond supported limit";

/// A value nesting `levels` deep, counting the value itself: `levels - 1` containers of
/// `container` (`L` or `M`) around one scalar.
fn nested(levels: usize, container: &str) -> Value {
    let mut value = json!({"S": "deep"});
    for _ in 1..levels {
        value = if container == "L" {
            json!({"L": [value]})
        } else {
            json!({"M": {"inner": value}})
        };
    }
    value
}

/// A depth past every bound a value is read under — past the documented one, past what any
/// reader of a value descends to, and past the nesting the request codec itself admits — so
/// a row at this depth shows a refusal is ANSWERED however deep the caller nested.
const PAST_EVERY_BOUND: usize = MAX_ATTRIBUTE_DEPTH * 4;

/// A depth far past the documented one that every reader of a value still descends to, so
/// the rows that assert the quota's CLAUSE are answered by the surface that states that
/// quota rather than by a bound the request met on the way in.
const DEEP_LEVELS: usize = MAX_ATTRIBUTE_DEPTH + 20;

#[tokio::test]
async fn a_value_nesting_past_the_supported_levels_is_refused_however_deep_it_goes() {
    let router = seeded().await;

    // A value AT the documented depth is served, through either nesting datatype, and is
    // read back as itself: the rows below are about the levels past that depth, not about
    // nesting.
    for container in ["L", "M"] {
        let key = format!("at-the-limit-{container}");
        let value = nested(MAX_ATTRIBUTE_DEPTH, container);
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": key.clone()}, "tree": value.clone()}}),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
        )
        .await;
        assert_eq!(
            read["Item"]["tree"], value,
            "case a {container} nested to the documented depth: expected it stored and read \
             back as itself, got {read}"
        );
    }

    // Rows differ in how far past the documented depth the value goes — one level and far
    // past it — in the nesting datatype that carries it, and in the surface it arrives
    // through. Every one is refused with the quota's clause and HTTP 400.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a list one level past the documented depth",
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "d1"}, "tree": nested(MAX_ATTRIBUTE_DEPTH + 1, "L")}}),
        ),
        (
            "a map one level past the documented depth",
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "d2"}, "tree": nested(MAX_ATTRIBUTE_DEPTH + 1, "M")}}),
        ),
        (
            "a list far past the documented depth",
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "d3"}, "tree": nested(DEEP_LEVELS, "L")}}),
        ),
        (
            "a map far past the documented depth",
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "d4"}, "tree": nested(DEEP_LEVELS, "M")}}),
        ),
        (
            "a value an update binds, far past the documented depth",
            "UpdateItem",
            json!({"TableName": TABLE,
                   "Key": {"pk": {"S": "seed"}},
                   "UpdateExpression": "SET tree = :v",
                   "ExpressionAttributeValues": {":v": nested(DEEP_LEVELS, "L")}}),
        ),
    ];

    for (case, operation, request) in rows {
        let (status, answered) = call(&router, operation, request).await;
        let stated = message(&answered);
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} with {answered}"
        );
        assert!(
            stated.contains(NESTING_CLAUSE),
            "case {case:?}: expected the clause {NESTING_CLAUSE:?}, got {stated:?}"
        );
    }

    // And a value nested deeper than anything that walks it will descend is still ANSWERED
    // — with a refusal, by whichever bound the request meets first, rather than by taking
    // the serving process down with it. Both nesting datatypes, since each has its own
    // reader, and one of them through an expression value as well as through an item.
    let unbounded: Vec<(&str, &str, Value)> = vec![
        (
            "a list nested past every bound a value is read under",
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "d6"}, "tree": nested(PAST_EVERY_BOUND, "L")}}),
        ),
        (
            "a map nested past every bound a value is read under",
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "d7"}, "tree": nested(PAST_EVERY_BOUND, "M")}}),
        ),
        (
            "an expression value nested past every bound a value is read under",
            "UpdateItem",
            json!({"TableName": TABLE,
                   "Key": {"pk": {"S": "seed"}},
                   "UpdateExpression": "SET tree = :v",
                   "ExpressionAttributeValues": {
                       ":v": nested(PAST_EVERY_BOUND, "M"),
                   }}),
        ),
    ];

    for (case, operation, request) in unbounded {
        let (status, answered) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} with {answered}"
        );
    }

    // Nothing a refused write carried was stored, and the seeded item is untouched.
    for key in ["d1", "d2", "d3", "d4", "d6", "d7"] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
        )
        .await;
        assert_eq!(
            read.get("Item"),
            None,
            "case a refused nesting at {key:?}: a refused write stores nothing, got {read}"
        );
    }
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "seed"}}}),
    )
    .await;
    assert_eq!(
        read["Item"].get("tree"),
        None,
        "case a refused update: the attribute is not written, got {read}"
    );
}

#[tokio::test]
async fn an_exponent_is_read_as_an_integer_and_then_range_checked() {
    let router = seeded().await;

    // Each row: the case, the `N` text, and the clause the refusal must carry — the
    // magnitude clauses wherever the exponent is a run of digits the integer it is held
    // in can hold and the magnitude it states leaves the documented range, however wide that
    // run is written, and the conversion clause for a text whose exponent is no such run at
    // all or states a magnitude that integer cannot hold. The rows walk one past each
    // published bound, the limits of that integer and one past each of them, far past them,
    // an exponent spelled with a long run of leading zeros, and a mantissa that normalises
    // to zero (which has no magnitude of its own, so the exponent it wrote is what is held
    // to the range).
    let rows: Vec<(&str, String, String)> = vec![
        (
            "an exponent one past the documented maximum",
            format!("1E{}", MAX_EXPONENT + 1),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent one past the documented minimum",
            format!("1E{}", MIN_EXPONENT - 1),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent at the top of the integer it is read into",
            format!("1E{MAX_SIGNED_32}"),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent at the bottom of the integer it is read into",
            format!("1E{MIN_SIGNED_32}"),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        (
            "a zero carrying an exponent at the top of that integer",
            format!("0E{MAX_SIGNED_32}"),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "a zero carrying an exponent at the bottom of that integer",
            format!("0E{MIN_SIGNED_32}"),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent far past any integer",
            format!("1E{PAST_ANY_INTEGER}"),
            conversion_clause(&format!("1E{PAST_ANY_INTEGER}")),
        ),
        (
            "a negative exponent far past any integer",
            "1E-9223372036854775809".to_string(),
            conversion_clause("1E-9223372036854775809"),
        ),
        (
            "an exponent of twenty-five digits",
            "1E99999999999999999999999".to_string(),
            conversion_clause("1E99999999999999999999999"),
        ),
        (
            "an exponent of seventeen digits",
            "1E92233720368547758".to_string(),
            conversion_clause("1E92233720368547758"),
        ),
        (
            "a negative exponent of seventeen digits",
            "1E-92233720368547758".to_string(),
            conversion_clause("1E-92233720368547758"),
        ),
        (
            "a zero carrying an exponent of seventeen digits",
            "0E92233720368547758".to_string(),
            conversion_clause("0E92233720368547758"),
        ),
        (
            "an exponent one past the top of that integer",
            "1E2147483648".to_string(),
            conversion_clause("1E2147483648"),
        ),
        (
            "an exponent one past the bottom of that integer",
            "1E-2147483649".to_string(),
            conversion_clause("1E-2147483649"),
        ),
        (
            "an exponent far past that width",
            "1E99999999999".to_string(),
            conversion_clause("1E99999999999"),
        ),
        (
            "an exponent far past that width, the other way",
            "1E-99999999999".to_string(),
            conversion_clause("1E-99999999999"),
        ),
        (
            "a signed exponent far past that width",
            "1E+99999999999".to_string(),
            conversion_clause("1E+99999999999"),
        ),
        (
            "an exponent past the maximum, spelled with a long run of leading zeros",
            "1E000000000000000000126".to_string(),
            OVERFLOW_CLAUSE.to_string(),
        ),
        (
            "an exponent that is not digits at all",
            "1Eabc".to_string(),
            conversion_clause("1Eabc"),
        ),
        (
            "an exponent marker with nothing after it",
            "1E".to_string(),
            conversion_clause("1E"),
        ),
    ];

    for (case, text, clause) in rows {
        let expected = format!("{ENVELOPE}{clause}");
        let (status, answered) = call(
            &router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": "number"}, "v": {"N": text.as_str()}},
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({text}): expected {expected:?}, got {status} with {answered}"
        );
    }

    // Nothing a refused number carried was stored.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "number"}}}),
    )
    .await;
    assert_eq!(
        read.get("Item"),
        None,
        "case a refused number: no item is written, got {read}"
    );

    // Both published bounds themselves are STORED and read back, so the refusals above
    // are the bound being crossed rather than a rejection of every exponent.
    for (case, text, canonical) in [
        (
            "the documented maximum exponent",
            format!("1E{MAX_EXPONENT}"),
            format!("1{}", "0".repeat(usize::try_from(MAX_EXPONENT).unwrap_or(0))),
        ),
        (
            "the documented minimum exponent",
            format!("1E{MIN_EXPONENT}"),
            format!(
                "0.{}1",
                "0".repeat(usize::try_from(-(MIN_EXPONENT + 1)).unwrap_or(0))
            ),
        ),
        (
            "a zero carrying an exponent inside the range",
            "0E125".to_string(),
            "0".to_string(),
        ),
    ] {
        ok(
            &router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": "bounds"}, "v": {"N": text.as_str()}},
            }),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "bounds"}}}),
        )
        .await;
        assert_eq!(
            read["Item"]["v"]["N"].as_str(),
            Some(canonical.as_str()),
            "case {case:?} ({text}): expected it read back as {canonical:?}, got {read}"
        );
    }
}
