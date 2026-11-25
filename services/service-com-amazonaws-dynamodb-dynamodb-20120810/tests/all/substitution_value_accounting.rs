// SPDX-License-Identifier: CC0-1.0

//! What a value BOUND to a substitution token is charged against, and what caps the
//! substitution table as a whole.
//!
//! The rule under test: binding a value to a `:token` stores nothing, so a bound value is not
//! item bytes and the documented 400 KB item ceiling ("Service, Account, and Table Quotas":
//! "The maximum item size in DynamoDB is 400 KB") is no bound on it — a value a request only
//! COMPARES (a `ConditionExpression` or `FilterExpression` operand) and one an action
//! SUBTRACTS or resolves to a no-op (a `DELETE` of members from an attribute that is not
//! there) are admitted however large they are, and only a value the change would STORE is
//! held to that ceiling. What bounds the table as a whole is its own cap over the SUM of the
//! values it carries, two binary megabytes, past which the request is refused 400
//! `ValidationException` `1 validation error detected: ExpressionAttributeValues exceeds max
//! size` (measured behaviour: the Smithy model constrains neither the member nor its values).
//!
//! The rows span what the rule generalises over: the surface the token is bound through
//! (`UpdateItem`, `PutItem`, `DeleteItem`, `Query`, `Scan`, `TransactWriteItems` and
//! `SearchVectors`), what the expression does with the value (compares it, subtracts it,
//! stores it), the value's type (`S` and `SS`), and where the total sits relative to the cap
//! (one value at it, one byte past it, and a total spread over several entries). WHICH FORM
//! the cap's refusal takes follows this service's existing split: the members a write states
//! its intent through carry the counting envelope, and the members a read narrows its result
//! with — and the expression members of one transaction ELEMENT — state the clause alone.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "SubstitutionAccounting";

/// The documented item ceiling, 400 KB — what a bound value is NOT held to.
const MAX_ITEM_BYTES: usize = 400 * 1024;

/// The cap on the SUM of one request's bound values, as the service states it
/// (`state::MAX_EXPRESSION_ATTRIBUTE_VALUES_BYTES`).
const MAX_SUBSTITUTION_BYTES: usize = 2 * 1024 * 1024;

/// AWS's whole message for a substitution table past that cap, on the members a WRITE states
/// its intent through: the counting envelope over the clause.
const OVER_THE_CAP: &str =
    "1 validation error detected: ExpressionAttributeValues exceeds max size";

/// The same clause on its own, which is the form the members a READ narrows its result with
/// — and the expression members of one transaction ELEMENT — state it in.
const OVER_THE_CAP_BARE: &str = "ExpressionAttributeValues exceeds max size";

/// AWS's fixed sentence for a change that would leave the stored item past the item ceiling.
const UPDATE_TOO_BIG: &str = "Item size to update has exceeded the maximum allowed size";

/// A value of `bytes` single-byte characters.
fn filler(bytes: usize) -> String {
    "x".repeat(bytes)
}

/// A value the item ceiling would refuse on its own: one byte past it.
fn past_the_item_ceiling() -> Value {
    json!({"S": filler(MAX_ITEM_BYTES + 1)})
}

fn key(of: &str) -> Value {
    json!({"pk": {"S": of}})
}

/// The table with one item under each key the rows address.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    for of in ["compared", "deleted", "stored", "read", "capped"] {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": of}, "held": {"S": "small"}}}),
        )
        .await;
    }
    router
}

/// RULE: a value the request only compares or subtracts is admitted past the item ceiling,
/// and the change it guards is applied — the recorded shape, a condition comparing against a
/// large value beside an update that writes one small attribute and subtracts members from an
/// attribute that is not there.
#[tokio::test]
async fn a_value_only_compared_or_subtracted_is_not_charged_against_the_item_ceiling() {
    let router = seeded().await;

    let body = ok(
        &router,
        "UpdateItem",
        json!({
            "TableName": TABLE,
            "Key": key("compared"),
            "UpdateExpression": "SET marker = :m DELETE tags :vu",
            "ConditionExpression": "attribute_exists(pk) OR pk = :vc",
            "ExpressionAttributeValues": {
                ":m": {"S": "written"},
                ":vu": {"SS": [filler(MAX_ITEM_BYTES + 1)]},
                ":vc": past_the_item_ceiling(),
            },
            "ReturnValues": "UPDATED_NEW",
        }),
    )
    .await;
    assert_eq!(
        body["Attributes"],
        json!({"marker": {"S": "written"}}),
        "case \"a large compared value beside a small write\": expected only the written \
         attribute reported, got {body}"
    );

    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key("compared"), "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"]["marker"],
        json!({"S": "written"}),
        "case \"a large compared value beside a small write\": expected the marker stored, got \
         {read}"
    );
    assert_eq!(
        read["Item"].get("tags"),
        None,
        "case \"a DELETE of an attribute that is not there\": expected no attribute created, \
         got {read}"
    );
}

/// RULE, over the surfaces: every operation that binds a token admits a value the item
/// ceiling would refuse, because none of them stores it.
#[tokio::test]
async fn every_surface_admits_a_large_value_it_only_compares() {
    let router = seeded().await;

    // Each row: the operation, and a request binding a value past the item ceiling that the
    // call only COMPARES — a write's condition, and a read's filter.
    for (case, operation, request) in [
        (
            "a DeleteItem whose condition compares a large value",
            "DeleteItem",
            json!({
                "TableName": TABLE,
                "Key": key("deleted"),
                "ConditionExpression": "attribute_exists(pk) OR held = :big",
                "ExpressionAttributeValues": {":big": past_the_item_ceiling()},
            }),
        ),
        (
            "a PutItem whose condition compares a large value",
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": "put"}, "held": {"S": "small"}},
                "ConditionExpression": "attribute_not_exists(pk) OR held = :big",
                "ExpressionAttributeValues": {":big": past_the_item_ceiling()},
            }),
        ),
        (
            "a Query whose filter compares a large value",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "FilterExpression": "held = :big",
                "ExpressionAttributeValues": {
                    ":p": {"S": "read"},
                    ":big": past_the_item_ceiling(),
                },
            }),
        ),
        (
            "a Scan whose filter compares a large value",
            "Scan",
            json!({
                "TableName": TABLE,
                "FilterExpression": "held = :big",
                "ExpressionAttributeValues": {":big": past_the_item_ceiling()},
            }),
        ),
    ] {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: expected the call to be served, got {status} with {}",
            message(&body)
        );
    }
}

/// RULE, the other half: a value the change would STORE is still held to the item ceiling, so
/// the ceiling moved rather than went away.
#[tokio::test]
async fn a_value_the_change_stores_is_still_held_to_the_item_ceiling() {
    let router = seeded().await;

    // Each row: an update whose clause WRITES the large value, through each clause kind that
    // stores its operand.
    for (case, update) in [
        (
            "an assignment of a large value",
            json!({
                "UpdateExpression": "SET body = :big",
                "ExpressionAttributeValues": {":big": past_the_item_ceiling()},
            }),
        ),
        (
            "an assignment through if_not_exists",
            json!({
                "UpdateExpression": "SET body = if_not_exists(body, :big)",
                "ExpressionAttributeValues": {":big": past_the_item_ceiling()},
            }),
        ),
        (
            "an ADD of a large set",
            json!({
                "UpdateExpression": "ADD tags :big",
                "ExpressionAttributeValues":
                    {":big": {"SS": [filler(MAX_ITEM_BYTES + 1)]}},
            }),
        ),
        (
            "a legacy AttributeUpdates PUT of a large value",
            json!({
                "AttributeUpdates": {"body": {"Action": "PUT", "Value": past_the_item_ceiling()}},
            }),
        ),
    ] {
        let mut request = json!({"TableName": TABLE, "Key": key("stored")});
        for (member, value) in update.as_object().expect("the row states its members") {
            request[member.as_str()] = value.clone();
        }
        let (status, body) = call(&router, "UpdateItem", request).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", UPDATE_TOO_BIG),
            "case {case:?}: expected the update-path item-ceiling refusal, got {status} with \
             {body}"
        );
    }

    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key("stored"), "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"].get("body"),
        None,
        "case \"the refused writes stored nothing\": expected no oversized attribute, got {read}"
    );
}

/// RULE: the SUM of the bound values is what the member is capped by — the total at the cap
/// is served and one byte past it is refused, whether the bytes sit in one entry or several.
#[tokio::test]
async fn the_substitution_table_is_capped_by_the_total_it_carries() {
    let router = seeded().await;

    // Each row: the case, the values bound, and whether the rule requires them admitted.
    // Every token is referenced by the condition below, because an unused definition is its
    // own refusal.
    for (case, values, admitted) in [
        (
            "one value at the cap",
            json!({":a": {"S": filler(MAX_SUBSTITUTION_BYTES)}}),
            true,
        ),
        (
            "one value one byte past the cap",
            json!({":a": {"S": filler(MAX_SUBSTITUTION_BYTES + 1)}}),
            false,
        ),
        (
            "two values summing to the cap",
            json!({
                ":a": {"S": filler(MAX_SUBSTITUTION_BYTES / 2)},
                ":b": {"S": filler(MAX_SUBSTITUTION_BYTES / 2)},
            }),
            true,
        ),
        (
            "two values summing one byte past the cap",
            json!({
                ":a": {"S": filler(MAX_SUBSTITUTION_BYTES / 2)},
                ":b": {"S": filler(MAX_SUBSTITUTION_BYTES / 2 + 1)},
            }),
            false,
        ),
        (
            "a set whose members sum one byte past the cap",
            json!({
                ":a": {"SS": [
                    filler(MAX_SUBSTITUTION_BYTES / 2),
                    filler(MAX_SUBSTITUTION_BYTES / 2 + 1),
                ]},
            }),
            false,
        ),
    ] {
        let references = values
            .as_object()
            .expect("the row binds a table")
            .keys()
            .map(|token| format!("held = {token}"))
            .collect::<Vec<String>>()
            .join(" OR ");
        // The request binds NOTHING but the values under test — it states no update at all,
        // so the total the cap weighs is exactly the row's own bytes — and every token is
        // referenced by the condition, because an unused definition is its own refusal.
        let request = json!({
            "TableName": TABLE,
            "Key": key("capped"),
            "ConditionExpression": format!("attribute_exists(pk) OR {references}"),
            "ExpressionAttributeValues": values.clone(),
        });
        let (status, body) = call(&router, "UpdateItem", request).await;
        if admitted {
            assert_eq!(
                status,
                200,
                "case {case:?}: a total at the cap is served, got {status} with {}",
                message(&body)
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", OVER_THE_CAP),
            "case {case:?}: expected the substitution-table cap refusal, got {status} with \
             {}",
            message(&body)
        );
    }
}

/// The table and the vector index the `SearchVectors` row searches, which is the one surface
/// whose request needs an index to reach the member at all.
const VECTOR_TABLE: &str = "SubstitutionAccountingVectors";
const VECTOR_INDEX: &str = "by-emb";

/// RULE: every surface that binds the member states the SAME cap, each in the form that
/// surface states its clauses in — the members a write states its intent through inside the
/// counting envelope, the members a read narrows its result with and one transaction
/// element's own expression members on their own.
#[tokio::test]
async fn every_surface_states_the_same_cap_in_its_own_form() {
    let router = seeded().await;
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": VECTOR_TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [{
                "IndexName": VECTOR_INDEX,
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": 2,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(&router, "the vector table", VECTOR_TABLE).await;

    // One value one byte past the cap, bound through each surface's own member.
    let over = json!({":big": {"S": filler(MAX_SUBSTITUTION_BYTES + 1)}});

    // Each row: the operation, the request that binds it, and the whole sentence that
    // surface must state.
    for (case, operation, request, expected) in [
        (
            "an UpdateItem condition",
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": key("capped"),
                "ConditionExpression": "held = :big",
                "ExpressionAttributeValues": over.clone(),
            }),
            OVER_THE_CAP,
        ),
        (
            "a PutItem condition",
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": "capped-put"}},
                "ConditionExpression": "held = :big",
                "ExpressionAttributeValues": over.clone(),
            }),
            OVER_THE_CAP,
        ),
        (
            "a DeleteItem condition",
            "DeleteItem",
            json!({
                "TableName": TABLE,
                "Key": key("capped"),
                "ConditionExpression": "held = :big",
                "ExpressionAttributeValues": over.clone(),
            }),
            OVER_THE_CAP,
        ),
        (
            "a Query filter",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "FilterExpression": "held = :big",
                "ExpressionAttributeValues": {
                    ":p": {"S": "read"},
                    ":big": over[":big"].clone(),
                },
            }),
            OVER_THE_CAP_BARE,
        ),
        (
            "a Scan filter",
            "Scan",
            json!({
                "TableName": TABLE,
                "FilterExpression": "held = :big",
                "ExpressionAttributeValues": over.clone(),
            }),
            OVER_THE_CAP_BARE,
        ),
        (
            "one element of a transaction",
            "TransactWriteItems",
            json!({"TransactItems": [{"ConditionCheck": {
                "TableName": TABLE,
                "Key": key("capped"),
                "ConditionExpression": "held = :big",
                "ExpressionAttributeValues": over.clone(),
            }}]}),
            OVER_THE_CAP_BARE,
        ),
        (
            "a SearchVectors condition",
            "SearchVectors",
            json!({
                "TableName": VECTOR_TABLE,
                "IndexName": VECTOR_INDEX,
                "SearchVector": [{"N": "1"}, {"N": "0"}],
                "TopK": 1,
                "SearchConditionExpression": "held = :big",
                "ExpressionAttributeValues": over.clone(),
            }),
            OVER_THE_CAP,
        ),
    ] {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", expected),
            "case {case:?} ({operation}): expected {expected:?}, got {status} with {body}"
        );
    }
}
