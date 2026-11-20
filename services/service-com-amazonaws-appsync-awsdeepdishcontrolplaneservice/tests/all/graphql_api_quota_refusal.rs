// SPDX-License-Identifier: CC0-1.0

//! The refusal `CreateGraphqlApi` answers past the account's per-Region GraphQL API
//! quota, end to end over HTTP.
//!
//! One rule: a create refused because the account already holds as many GraphQL APIs in
//! the Region as the service limit allows answers 400 `ApiLimitExceededException` with
//! the service's own fixed sentence for that limit — it names the limited resource and
//! states the limit has been reached, and interpolates nothing: not a member of the
//! request, and not the quota number, so the wording a client branches or reports on
//! cannot move with the limit.
//!
//! The rows differ along what the rule generalises over: the members the refused create
//! carries (name, authentication type, visibility, tags, `xrayEnabled`, `ownerContact`,
//! `apiType`) and how many creates have already been refused — none of which the rule
//! reads. The quota itself is reached by creating up to it first, so the refusal is the
//! store's own answer rather than a configured one; freeing a slot and filling it again
//! shows the same sentence returns.
//!
//! The harness (`call`, `router`, `Answer`) is the error-contract suite's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, router, Answer};

/// AppSync's service limit for GraphQL APIs an account holds in one Region — the
/// handler's own `APIS_PER_REGION`. Creates are accepted up to it and refused past it.
const QUOTA: usize = 25;

/// The fixed sentence the refusal carries. Entirely fixed text: it states the limited
/// resource and that the limit has been reached, and embeds no number and nothing of the
/// request.
const LIMIT_REACHED: &str = "The service limit for number of GraphQL APIs has been reached.";

/// A `CreateGraphqlApi` body that only names the API and its authentication type — the
/// two members a create needs.
/// Source: `smithy_query(query="operation", operation="CreateGraphqlApi")`.
fn plain_body(name: &str) -> Value {
    json!({ "name": name, "authenticationType": "API_KEY" })
}

/// Create one GraphQL API, answering what the service replied.
async fn create(router: &MultiServiceRouter, body: Value) -> Answer {
    call(router, "POST", "/v1/apis", Some(body)).await
}

/// Fill the account's Region to its quota, answering the ids of the APIs created.
async fn fill_to_the_quota(router: &MultiServiceRouter) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..QUOTA {
        let created = create(router, plain_body(&format!("Api_quota_{index}"))).await;
        assert_eq!(
            created.status,
            200,
            "case create {} of {QUOTA}: expected the create to be accepted, got {created:?}",
            index + 1
        );
        ids.push(
            created.body["graphqlApi"]["apiId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    ids
}

/// The rule: every create past the quota answers the one fixed sentence, whatever it
/// carries and however many came before it.
#[tokio::test]
async fn a_create_past_the_quota_carries_the_service_limit_sentence() {
    let router = router();
    let ids = fill_to_the_quota(&router).await;

    // (case, the body the refused create carries). The rows span the members a create
    // can set; the refusal reads none of them.
    let rows: &[(&str, Value)] = &[
        ("only the required members", plain_body("Over_quota_plain")),
        (
            "IAM authentication, private visibility",
            json!({
                "name": "Over_quota_iam",
                "authenticationType": "AWS_IAM",
                "visibility": "PRIVATE",
            }),
        ),
        (
            "tags, X-Ray and an owner contact",
            json!({
                "name": "Over_quota_tagged",
                "authenticationType": "API_KEY",
                "xrayEnabled": true,
                "ownerContact": "team@example.invalid",
                "tags": { "stage": "test" },
            }),
        ),
        (
            "an explicit apiType and query limits",
            json!({
                "name": "Over_quota_typed",
                "authenticationType": "API_KEY",
                "apiType": "GRAPHQL",
                "queryDepthLimit": 5,
                "resolverCountLimit": 7,
            }),
        ),
        (
            "a single-character name",
            json!({ "name": "O", "authenticationType": "API_KEY" }),
        ),
    ];

    for (case, body) in rows {
        // Twice over, so the answer does not depend on how many refusals came before.
        for attempt in 1..=2 {
            let refused = create(&router, body.clone()).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_type.as_str(),
                    refused.message.as_str()
                ),
                (400, "ApiLimitExceededException", LIMIT_REACHED),
                "case {case:?} attempt {attempt}: expected 400 ApiLimitExceededException \
                 {LIMIT_REACHED:?}, got {refused:?}"
            );
            // The sentence carries no quota number, so the limit is not readable off it.
            assert!(
                !refused.message.chars().any(char::is_numeric),
                "case {case:?} attempt {attempt}: expected the sentence to embed no number, \
                 got {refused:?}"
            );
            // Nor anything of the request.
            let name = body["name"].as_str().unwrap_or_default();
            assert!(
                !refused.message.contains(name),
                "case {case:?} attempt {attempt}: expected the sentence to carry nothing of \
                 the request, got {refused:?}"
            );
        }
    }

    // And nothing was stored for any of them: the store still holds exactly the accepted
    // APIs.
    let listed = call(
        &router,
        "GET",
        &format!("/v1/apis?maxResults={QUOTA}"),
        None,
    )
    .await;
    let count = listed.body["graphqlApis"].as_array().map_or(0, Vec::len);
    assert_eq!(
        count, QUOTA,
        "case at the quota: expected the store to hold exactly the accepted APIs, \
         got {listed:?}"
    );

    // Freeing a slot lets one create through again — and the one after it takes the same
    // sentence, so the refusal follows the stored count rather than a latch.
    let freed = call(&router, "DELETE", &format!("/v1/apis/{}", ids[0]), None).await;
    assert_eq!(
        freed.status, 200,
        "case free a slot: expected the delete to be served, got {freed:?}"
    );
    let accepted = create(&router, plain_body("Api_quota_after_delete")).await;
    assert_eq!(
        accepted.status, 200,
        "case after freeing a slot: expected the create to be accepted, got {accepted:?}"
    );
    let refused = create(&router, plain_body("Over_quota_again")).await;
    assert_eq!(
        (
            refused.status,
            refused.error_type.as_str(),
            refused.message.as_str()
        ),
        (400, "ApiLimitExceededException", LIMIT_REACHED),
        "case the slot is taken again: expected 400 ApiLimitExceededException \
         {LIMIT_REACHED:?}, got {refused:?}"
    );
}
