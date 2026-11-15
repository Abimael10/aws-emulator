// SPDX-License-Identifier: CC0-1.0

//! WHICH defect a malformed `TransactWriteItems` is refused for, and how many it counts.
//!
//! Two rules, both about a request the service decides on its own — no table is looked up
//! for either:
//!
//! * Every per-element defect the REQUEST alone decides is settled over the elements in
//!   LIST ORDER, so a transaction carrying one fatal defect in each of two actions is
//!   refused for the one the FIRST-listed action carries, whatever class the two defects
//!   belong to. A client told to fix the second action would re-submit and be refused
//!   again for a defect it was never shown.
//! * An input-constraint violation of the request's own members is one CLAUSE of the
//!   `<N> validation error(s) detected: ` envelope, and the envelope counts every clause
//!   the request earned — including the constraint the model declares no trait for, that
//!   an element must state one of `Delete`, `Put`, `Update` or `ConditionCheck`
//!   (`smithy_query(query="shape", shape="com.amazonaws.dynamodb#TransactWriteItem")`
//!   gives the four as optional members).
//!
//! The rows differ along what each rule generalises over: for the first, the ORDER the two
//! defective actions are listed in, the action kinds that carry them (`ConditionCheck`,
//! `Put`, `Update`) and the defect classes (an unreadable condition, an unreadable update,
//! an item nested past the documented ceiling); for the second, which other members the
//! request violates beside the empty element, how many violations there are, and where in
//! the list the empty element sits.
//!
//! Every call is driven over the wire, so nothing is asserted that a client could not
//! observe.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every element below addresses.
const TABLE: &str = "TransactDefects";

/// One emulator holding one active table keyed on `pk`.
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
    describe_when_active(&router, "the transactional-defect table", TABLE).await;
    router
}

/// An attribute value nested `levels` deep: past the documented ceiling at 33, since
/// "DynamoDB supports nested attributes up to 32 levels deep" ("Service, Account, and
/// Table Quotas").
fn nested(levels: usize) -> Value {
    let mut value = json!({"S": "bottom"});
    for _ in 0..levels {
        value = json!({"M": {"n": value}});
    }
    value
}

/// A `Put` of an item nested one level past the documented ceiling.
fn deep_put(key: &str) -> Value {
    json!({"Put": {"TableName": TABLE, "Item": {"pk": {"S": key}, "blob": nested(33)}}})
}

/// A `ConditionCheck` whose `ConditionExpression` the grammar cannot read.
fn unreadable_condition(key: &str) -> Value {
    json!({"ConditionCheck": {"TableName": TABLE, "Key": {"pk": {"S": key}},
                              "ConditionExpression": "attribute_exists(pk AND"}})
}

/// An `Update` whose `UpdateExpression` the grammar cannot read.
fn unreadable_update(key: &str) -> Value {
    json!({"Update": {"TableName": TABLE, "Key": {"pk": {"S": key}},
                      "UpdateExpression": "SET a = , :v",
                      "ExpressionAttributeValues": {":v": {"S": "x"}}}})
}

/// A `Put` nothing is wrong with.
fn good_put(key: &str) -> Value {
    json!({"Put": {"TableName": TABLE, "Item": {"pk": {"S": key}}}})
}

/// The refusal an unreadable `ConditionExpression` of one ELEMENT earns: the clause as the
/// sentence it is, since the member belongs to that element rather than to the request.
const CONDITION_SYNTAX: &str =
    "Invalid ConditionExpression: Syntax error; token: \"AND\", near: \"pk AND\"";

/// The same for an unreadable `UpdateExpression`.
const UPDATE_SYNTAX: &str =
    "Invalid UpdateExpression: Syntax error; token: \",\", near: \"= , :v\"";

/// The fixed sentence AWS states for the item-nesting QUOTA, which an element of a
/// transaction earns as the sentence it is — like the item-size sentence beside it.
const NESTING: &str = "Nesting Levels have exceeded supported limits: Attributes in the item \
                       have nested levels beyond supported limit";

#[tokio::test]
async fn the_first_listed_action_that_carries_a_fatal_defect_is_the_one_reported() {
    let router = seeded().await;

    // (case, the actions in the order the request lists them, the message it must earn)
    let rows: Vec<(&str, Vec<Value>, &str)> = vec![
        (
            "an unreadable condition first, an over-deep item second",
            vec![unreadable_condition("cc-key"), deep_put("deep33")],
            CONDITION_SYNTAX,
        ),
        (
            "the same two actions the other way round",
            vec![deep_put("deep33"), unreadable_condition("cc-key")],
            NESTING,
        ),
        (
            "an unreadable update first, an over-deep item second",
            vec![unreadable_update("up-key"), deep_put("deep33")],
            UPDATE_SYNTAX,
        ),
        (
            "the same two actions the other way round",
            vec![deep_put("deep33"), unreadable_update("up-key")],
            NESTING,
        ),
        (
            "two unreadable expressions, the condition listed first",
            vec![unreadable_condition("cc-key"), unreadable_update("up-key")],
            CONDITION_SYNTAX,
        ),
        (
            "two unreadable expressions, the update listed first",
            vec![unreadable_update("up-key"), unreadable_condition("cc-key")],
            UPDATE_SYNTAX,
        ),
        (
            "a sound action ahead of both defective ones",
            vec![
                good_put("fine"),
                unreadable_condition("cc-key"),
                deep_put("deep33"),
            ],
            CONDITION_SYNTAX,
        ),
        (
            "a sound action between them",
            vec![
                deep_put("deep33"),
                good_put("fine"),
                unreadable_condition("cc-key"),
            ],
            NESTING,
        ),
    ];

    for (case, actions, expected) in rows {
        let request = json!({"TransactItems": actions});
        let (status, answered) = call(&router, "TransactWriteItems", request.clone()).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.to_string()),
            "case {case:?} ({request}): expected {expected:?}, got {status} with {answered}"
        );
    }

    // Nothing any refused transaction carried was written: every key those rows named
    // holds no item.
    for key in ["cc-key", "deep33", "up-key", "fine"] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": key}}, "ConsistentRead": true}),
        )
        .await;
        assert_eq!(
            read.get("Item"),
            None,
            "case a refused transaction at {key:?}: nothing was written, got {read}"
        );
    }
}

#[tokio::test]
async fn every_constraint_violation_the_request_carries_is_counted_in_one_envelope() {
    let router = seeded().await;

    // The clause an element that names none of the four actions earns.
    const NO_ACTION: &str = "Value '' at 'transactItems' failed to satisfy constraint: \
                             TransactWriteRequest should contain Delete or Put or Update or \
                             ConditionCheck";
    // The clause a `ReturnConsumedCapacity` outside its modelled value set earns, over the
    // set the model declares (`smithy_query(query="shape",
    // shape="com.amazonaws.dynamodb#ReturnConsumedCapacity")` gives `INDEXES TOTAL NONE`).
    const BAD_CAPACITY: &str = "Value 'BOGUS' at 'returnConsumedCapacity' failed to satisfy \
                                constraint: Member must satisfy enum value set: [INDEXES, \
                                TOTAL, NONE]";
    // The same for `ReturnItemCollectionMetrics` (`SIZE NONE`).
    const BAD_METRICS: &str = "Value 'NOPE' at 'returnItemCollectionMetrics' failed to satisfy \
                               constraint: Member must satisfy enum value set: [SIZE, NONE]";

    // (case, request, the whole message it must earn)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "an element naming no action, and nothing else wrong",
            json!({"TransactItems": [{}]}),
            format!("1 validation error detected: {NO_ACTION}"),
        ),
        (
            "an element naming no action beside a value outside an enum's set",
            json!({"TransactItems": [{}, good_put("x")],
                   "ReturnConsumedCapacity": "BOGUS"}),
            format!("2 validation errors detected: {NO_ACTION}; {BAD_CAPACITY}"),
        ),
        (
            // The clause names the LIST member, so where in the list the element sits
            // does not change the message.
            "the empty element listed last",
            json!({"TransactItems": [good_put("x"), {}],
                   "ReturnConsumedCapacity": "BOGUS"}),
            format!("2 validation errors detected: {NO_ACTION}; {BAD_CAPACITY}"),
        ),
        (
            "an element naming no action beside another enumerated member's set",
            json!({"TransactItems": [{}, good_put("x")],
                   "ReturnItemCollectionMetrics": "NOPE"}),
            format!("2 validation errors detected: {NO_ACTION}; {BAD_METRICS}"),
        ),
        (
            "an element naming no action beside two members outside their sets",
            json!({"TransactItems": [{}],
                   "ReturnConsumedCapacity": "BOGUS",
                   "ReturnItemCollectionMetrics": "NOPE"}),
            format!("3 validation errors detected: {NO_ACTION}; {BAD_CAPACITY}; {BAD_METRICS}"),
        ),
        (
            "two elements naming no action, which is the one member violated once",
            json!({"TransactItems": [{}, {}],
                   "ReturnConsumedCapacity": "BOGUS"}),
            format!("2 validation errors detected: {NO_ACTION}; {BAD_CAPACITY}"),
        ),
        (
            // An element naming SEVERAL actions states no single action either, which is
            // its own constraint rather than this one.
            "an element naming two actions",
            json!({"TransactItems": [{"Put": {"TableName": TABLE, "Item": {"pk": {"S": "x"}}},
                                      "Delete": {"TableName": TABLE,
                                                 "Key": {"pk": {"S": "y"}}}}]}),
            "1 validation error detected: Value at 'transactItems' failed to satisfy \
             constraint: Member must contain exactly one of ConditionCheck, Put, Delete or \
             Update"
                .to_string(),
        ),
    ];

    for (case, request, expected) in rows {
        let (status, answered) = call(&router, "TransactWriteItems", request.clone()).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({request}): expected {expected:?}, got {status} with {answered}"
        );
    }

    // A request whose idempotency token is outside its declared bound is still refused for
    // THAT alone: the token is read before the request it stands for, so its violation
    // masks every other one — the empty element included.
    let masked = json!({
        "TransactItems": [{}, good_put("x")],
        "ReturnConsumedCapacity": "BOGUS",
        "ClientRequestToken": "t".repeat(37),
    });
    let (status, answered) = call(&router, "TransactWriteItems", masked.clone()).await;
    let stated = message(&answered);
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "ValidationException"),
        "case an over-length token: expected the validation refusal, got {status} with \
         {answered}"
    );
    assert!(
        stated.starts_with("1 validation error detected: ")
            && stated.contains("ClientRequestToken")
            && !stated.contains("transactItems")
            && !stated.contains("returnConsumedCapacity"),
        "case an over-length token: expected the token's violation stated alone, got {stated:?}"
    );

    // And a transaction nothing is wrong with is served: the rows above are the empty
    // element being refused rather than the surface rejecting the request.
    ok(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [good_put("served")], "ReturnConsumedCapacity": "TOTAL"}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "served"}}, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"pk": {"S": "served"}}),
        "case a sound transaction: the item it put is readable, got {read}"
    );
}
