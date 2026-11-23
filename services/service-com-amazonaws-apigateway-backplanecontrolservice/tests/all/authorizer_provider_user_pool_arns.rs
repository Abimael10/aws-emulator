// SPDX-License-Identifier: CC0-1.0

//! What a `COGNITO_USER_POOLS` authorizer's `providerARNs` entries have to BE, asserted on
//! the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client sees,
//! not what a handler returns. The harness is this crate's existing one
//! (`rest_api_lifecycle`), reused rather than stood up again.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > `providerARNs` is "a list of the Amazon Cognito user pool ARNs for the
//! > `COGNITO_USER_POOLS` authorizer. Each element is of this format:
//! > `arn:aws:cognito-idp:{region}:{account_id}:userpool/{user_pool_id}`", and the pool an
//! > entry names is the one whose tokens that authorizer accepts — so EVERY entry of such an
//! > authorizer has to name a user pool. A write of the member carrying an entry that names
//! > none is refused `400` `BadRequestException`, whose message is the member's own fixed
//! > sentence followed by the rejected entries exactly as the request wrote them, and nothing
//! > is stored: the api goes on holding the authorizers it held. The rule is the MEMBER's, so
//! > it holds of the record whichever call wrote it — `CreateAuthorizer` judges the record it
//! > would store and `UpdateAuthorizer` the record its patches produced — and it is the
//! > `COGNITO_USER_POOLS` authorizer's member alone: "for a TOKEN or REQUEST authorizer, this
//! > is not defined", so an authorizer of either type lists no pools for this rule to read.
//!
//! The rows span what that rule generalises over: WHAT an entry is (a pool ARN, a value that
//! is no ARN at all, an ARN of another service, an ARN reaching past one pool to its app
//! client, a pool ARN carrying no id), HOW MANY entries the list carries and which of them is
//! at fault, WHICH `AuthorizerType` the record carries, and WHICH call wrote the member.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::user_pool_state::INVALID_PROVIDER_ARNS;

use crate::authorizer_lifecycle::{create_authorizer, get_authorizer, lambda_uri, user_pool_arn};
use crate::patch_updates::{op, patch};
use crate::rest_api_lifecycle::{assert_failure, create_api, string_member, Answer, Gateway};

/// The modelled failure a refused write of this member carries: "the submitted request is not
/// valid, for example, the input is incomplete or incorrect" at HTTP 400.
const BAD_REQUEST: (u16, &str) = (400, "BadRequestException");

/// The message a write carrying `rejected` is refused with: the member's own fixed sentence
/// and then those entries, one per line.
fn refusal(rejected: &[&str]) -> String {
    let mut message = String::from(INVALID_PROVIDER_ARNS);
    for arn in rejected {
        message.push('\n');
        message.push_str(arn);
    }
    message
}

/// Assert `answer` is that refusal, carrying exactly the message `rejected` earns.
fn assert_refused(answer: &Answer, rejected: &[&str], case: &str) {
    assert_failure(answer, BAD_REQUEST.0, BAD_REQUEST.1, case);
    let expected = refusal(rejected);
    assert_eq!(
        answer.body["message"].as_str(),
        Some(expected.as_str()),
        "case {case}: expected the message {expected:?}, got {:?}",
        answer.body["message"]
    );
}

/// `GetAuthorizers` for `api`: the collection entries the api holds, under the `item` member
/// this control plane's paged collections carry.
async fn authorizers(gateway: &Gateway, api: &str, case: &str) -> Vec<Value> {
    let answer = gateway
        .call("GET", &format!("/restapis/{api}/authorizers"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from GetAuthorizers, got {} with {:?}",
        answer.status, answer.body
    );
    // An EMPTY collection carries no `item` member at all, which is the same claim as an
    // empty list: the api holds no authorizer.
    answer.body["item"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A value that is no ARN at all, as a template typo leaves one.
const NOT_AN_ARN: &str = "not-an-arn";

/// An identity pool's ARN: Amazon Cognito Identity's resource, which is not a user pool.
const IDENTITY_POOL_ARN: &str =
    "arn:aws:cognito-identity:us-east-1:123456789012:identitypool/us-east-1:a1b2c3d4";

/// A pool's APP CLIENT, an ARN that reaches PAST one pool.
const APP_CLIENT_ARN: &str =
    "arn:aws:cognito-idp:us-east-1:123456789012:userpool/us-east-1_pool/client/1a2b3c";

/// A user-pool ARN carrying no pool id at all.
const EMPTY_POOL_ARN: &str = "arn:aws:cognito-idp:us-east-1:123456789012:userpool/";

#[tokio::test]
async fn every_provider_arn_of_a_user_pool_authorizer_has_to_name_a_pool() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "provider-arn-rules").await;

    // Rows differ along what each entry is, how many entries the list carries, which of them
    // is at fault, and the authorizer's own type. `None` is a write the member admits; `Some`
    // carries the entries the refusal has to list, in the order the request wrote them.
    let rows: Vec<(&str, Value, Option<Vec<&str>>)> = vec![
        (
            "one pool ARN",
            json!({
                "name": "one-pool",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [user_pool_arn("us-east-1_first")],
            }),
            None,
        ),
        (
            "two pool ARNs",
            json!({
                "name": "two-pools",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [
                    user_pool_arn("us-east-1_first"),
                    user_pool_arn("us-east-1_second"),
                ],
            }),
            None,
        ),
        (
            "a value that is no ARN at all",
            json!({
                "name": "not-an-arn",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [NOT_AN_ARN],
            }),
            Some(vec![NOT_AN_ARN]),
        ),
        (
            "an identity pool, which is another service's resource",
            json!({
                "name": "identity-pool",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [IDENTITY_POOL_ARN],
            }),
            Some(vec![IDENTITY_POOL_ARN]),
        ),
        (
            "a pool's app client, which is not the pool",
            json!({
                "name": "app-client",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [APP_CLIENT_ARN],
            }),
            Some(vec![APP_CLIENT_ARN]),
        ),
        (
            "a pool ARN carrying no id",
            json!({
                "name": "no-pool-id",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [EMPTY_POOL_ARN],
            }),
            Some(vec![EMPTY_POOL_ARN]),
        ),
        (
            "a pool ARN beside a value that is not one",
            json!({
                "name": "one-good-one-bad",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [user_pool_arn("us-east-1_first"), NOT_AN_ARN],
            }),
            Some(vec![NOT_AN_ARN]),
        ),
        (
            "two entries, neither of them a pool",
            json!({
                "name": "two-bad",
                "type": "COGNITO_USER_POOLS",
                "identitySource": "method.request.header.Authorization",
                "providerARNs": [NOT_AN_ARN, IDENTITY_POOL_ARN],
            }),
            Some(vec![NOT_AN_ARN, IDENTITY_POOL_ARN]),
        ),
        (
            "a TOKEN authorizer, whose type lists no pools",
            json!({
                "name": "token-with-a-stray-entry",
                "type": "TOKEN",
                "identitySource": "method.request.header.Authorization",
                "authorizerUri": lambda_uri("authorize"),
                "providerARNs": [NOT_AN_ARN],
            }),
            None,
        ),
        (
            "a REQUEST authorizer, whose type lists no pools",
            json!({
                "name": "request-with-a-stray-entry",
                "type": "REQUEST",
                "identitySource": "method.request.header.Authorization",
                "authorizerUri": lambda_uri("authorize"),
                "providerARNs": [NOT_AN_ARN],
            }),
            None,
        ),
    ];

    let mut admitted = 0usize;
    for (case, request, rejected) in rows {
        let declared = request["providerARNs"].clone();
        let answer = create_authorizer(&gateway, &api, request).await;
        match &rejected {
            Some(rejected) => {
                assert_refused(&answer, rejected, case);
            }
            None => {
                assert_eq!(
                    answer.status, 201,
                    "case {case}: expected 201 from CreateAuthorizer, got {} with {:?}",
                    answer.status, answer.body
                );
                admitted += 1;
                // Round trip: the authorizer the api stored is what a later read reports, so
                // an admitted list is one a client reads back entry for entry.
                let id = string_member(&answer.body, "id", case);
                let read = get_authorizer(&gateway, &api, &id).await;
                assert_eq!(
                    read.status, 200,
                    "case {case}: expected 200 from GetAuthorizer, got {} with {:?}",
                    read.status, read.body
                );
                assert_eq!(
                    read.body["providerARNs"], declared,
                    "case {case}: expected GetAuthorizer to report {declared:?}, got {:?}",
                    read.body["providerARNs"]
                );
            }
        }
        // A refused write stores nothing: the api holds exactly the authorizers its admitted
        // writes created.
        let held = authorizers(&gateway, &api, case).await;
        assert_eq!(
            held.len(),
            admitted,
            "case {case}: expected the api to hold {admitted} authorizer(s), got {held:?}"
        );
    }
}

#[tokio::test]
async fn a_patch_that_adds_a_provider_arn_naming_no_pool_is_refused() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "provider-arn-patches").await;
    let base = user_pool_arn("us-east-1_base");
    let created = create_authorizer(
        &gateway,
        &api,
        json!({
            "name": "patched",
            "type": "COGNITO_USER_POOLS",
            "identitySource": "method.request.header.Authorization",
            "providerARNs": [base.clone()],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: expected 201 from CreateAuthorizer, got {} with {:?}",
        created.status, created.body
    );
    let id = string_member(&created.body, "id", "setup");
    let uri = format!("/restapis/{api}/authorizers/{id}");

    // Rows differ along the entry the patch adds and along WHERE the op names it — the member
    // itself with the entry as the op's `value`, and the pointer that names the entry — since
    // either spelling writes the same member.
    let second = user_pool_arn("us-east-1_second");
    // One row: the case, the ops it sends, and the entries a refusal has to name — `None` for
    // a patch the member admits.
    type PatchRow = (&'static str, Vec<Value>, Option<Vec<&'static str>>);
    let rows: Vec<PatchRow> = vec![
        (
            "adding a pool ARN as the op's value",
            vec![op("add", "/providerARNs", Some(&second))],
            None,
        ),
        (
            "adding a value that is no ARN as the op's value",
            vec![op("add", "/providerARNs", Some(NOT_AN_ARN))],
            Some(vec![NOT_AN_ARN]),
        ),
        (
            "adding an identity pool through the pointer that names the entry",
            vec![op(
                "add",
                &format!("/providerARNs/{IDENTITY_POOL_ARN}"),
                None,
            )],
            Some(vec![IDENTITY_POOL_ARN]),
        ),
        (
            "adding a pool ARN and then a value that is no ARN",
            vec![
                op("add", "/providerARNs", Some(&user_pool_arn("us-east-1_third"))),
                op("add", "/providerARNs", Some(APP_CLIENT_ARN)),
            ],
            Some(vec![APP_CLIENT_ARN]),
        ),
    ];

    // The pools the record carries, which only an admitted patch changes.
    let mut held: Vec<String> = vec![base.clone()];
    for (case, operations, rejected) in rows {
        let answer = patch(&gateway, &uri, operations).await;
        match &rejected {
            Some(rejected) => assert_refused(&answer, rejected, case),
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected 200 from UpdateAuthorizer, got {} with {:?}",
                    answer.status, answer.body
                );
                held.push(second.clone());
            }
        }
        // A refused patch leaves the stored record exactly as it was — including the entry an
        // earlier op of the same refused list would have added.
        let read = get_authorizer(&gateway, &api, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetAuthorizer, got {} with {:?}",
            read.status, read.body
        );
        let reported: Vec<String> = read.body["providerARNs"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported, held,
            "case {case}: expected the stored pools {held:?}, got {reported:?}"
        );
    }
}
