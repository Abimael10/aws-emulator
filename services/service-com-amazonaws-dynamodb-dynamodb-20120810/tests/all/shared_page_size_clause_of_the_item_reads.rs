// SPDX-License-Identifier: CC0-1.0

//! The two ITEM READS word the page-size clause they SHARE identically.
//!
//! `Query$Limit` and `Scan$Limit` are the same member name targeting the same shape under the
//! same trait — `PositiveIntegerObject` with `@range{min:1}`
//! (`smithy_query(query="operation", operation="Query,Scan")`) — so a page size below that
//! minimum is one violation class on one member, and both reads state it in the one form an
//! item request states its own members in: the member by its WIRE spelling, with none of the
//! rejected value echoed. That is asserted here as an EQUALITY between the two operations'
//! messages rather than against a literal, so a build that changes either read's wording on
//! its own fails here whichever wording it chose; the literal is pinned beside it, and the
//! contrast — a CONTROL-PLANE listing, which states its own page size by the validator's
//! lowerCamel path with the value echoed — is pinned too, so the equality is not simply
//! "every page-size clause is alike".
//!
//! The sweep spans what the rule generalises over: the rejected value (zero, one below the
//! minimum, a handful below, and both integral extremes a 32-bit member carries) and, for the
//! other side of the rule, the published bound itself, which is served rather than refused.
//! `tests/all/item_request_clause_form.rs` and `tests/all/page_size_clause_forms.rs` table the
//! forms of every other bounded member beside these.

use serde_json::json;

use crate::emulator::{call, describe_when_active, error_code, message, ok};

/// The table both reads address: one hash key, which is all a refusal about the page size
/// needs.
const TABLE: &str = "SharedPageSizeClause";

/// The whole message both item reads must carry for a page size below the published minimum:
/// the member as the model spells it, with nothing echoed.
const WIRE_FORM: &str = "1 validation error detected: Value at 'Limit' failed to satisfy \
                         constraint: Member must have value greater than or equal to 1";

/// The page sizes the sweep rejects: zero, one below the minimum, a handful below it, and both
/// integral extremes a 32-bit member can carry below it.
const REJECTED: [i64; 5] = [0, -1, -9, -2_147_483_647, -2_147_483_648];

/// Create [`TABLE`] and wait out the `CREATING` -> `ACTIVE` transition the clock owes it.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
    let router = crate::emulator::emulator();
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
    describe_when_active(&router, "create the table both reads address", TABLE).await;
    router
}

/// The message the refusal of a page size of `limit` through `operation` carries, asserted to
/// be the 400 `ValidationException` the class is reported as first.
async fn refused_page_size(
    router: &smithy_server_runtime::MultiServiceRouter,
    operation: &str,
    limit: i64,
) -> String {
    let request = match operation {
        "Query" => json!({"TableName": TABLE, "Limit": limit,
                          "KeyConditionExpression": "pk = :p",
                          "ExpressionAttributeValues": {":p": {"S": "one"}}}),
        _ => json!({"TableName": TABLE, "Limit": limit}),
    };
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "case {operation} with Limit {limit}: expected 400 ValidationException, got {status} \
         {body}"
    );
    message(&body)
}

#[tokio::test]
async fn both_item_reads_word_one_page_size_violation_identically() {
    let router = seeded().await;

    for limit in REJECTED {
        let keyed = refused_page_size(&router, "Query", limit).await;
        let walk = refused_page_size(&router, "Scan", limit).await;
        assert_eq!(
            keyed, walk,
            "case \"Limit {limit}\": the two item reads share the member, the shape and the \
             bound, so they state one clause alike; the keyed read said {keyed:?} and the \
             table walk {walk:?}"
        );
        assert_eq!(
            walk, WIRE_FORM,
            "case \"Limit {limit}\": an item request names the member as the model spells it \
             and echoes nothing, got {walk:?}"
        );
        assert!(
            !walk.contains(&limit.to_string()),
            "case \"Limit {limit}\": this form echoes no part of the rejected value, got \
             {walk:?}"
        );
    }
}

#[tokio::test]
async fn a_control_plane_listing_still_words_its_own_page_size_the_other_way() {
    let router = seeded().await;

    // The contrast: `ListBackups$Limit` is a page size too, and the listing states it by the
    // validator's lowerCamel path WITH the rejected value — so the equality above is a fact of
    // the two item reads and not of every page-size clause this service words.
    for limit in REJECTED {
        let (status, body) = call(&router, "ListBackups", json!({"Limit": limit})).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case \"ListBackups Limit {limit}\": expected 400 ValidationException, got \
             {status} {body}"
        );
        let stated = message(&body);
        let expected = format!(
            "1 validation error detected: Value '{limit}' at 'limit' failed to satisfy \
             constraint: Member must have value greater than or equal to 1"
        );
        assert_eq!(
            stated, expected,
            "case \"ListBackups Limit {limit}\": expected {expected:?}, got {stated:?}"
        );
        assert_ne!(
            stated, WIRE_FORM,
            "case \"ListBackups Limit {limit}\": the listing's own form is not the item \
             request's, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn the_published_minimum_is_served_by_both_item_reads() {
    let router = seeded().await;

    // The other side of the rule: the clauses above are about the BOUND, not about the member
    // being read, so the smallest page size the range admits is served by both reads.
    for operation in ["Query", "Scan"] {
        let request = match operation {
            "Query" => json!({"TableName": TABLE, "Limit": 1,
                              "KeyConditionExpression": "pk = :p",
                              "ExpressionAttributeValues": {":p": {"S": "one"}}}),
            _ => json!({"TableName": TABLE, "Limit": 1}),
        };
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (200, "<no __type>"),
            "case {operation:?}: the published minimum page size is served, got {status} {body}"
        );
        let stated = message(&body);
        assert!(
            !stated.contains("failed to satisfy constraint"),
            "case {operation:?}: a page size at its bound is no constraint violation, got \
             {stated:?}"
        );
    }
}
