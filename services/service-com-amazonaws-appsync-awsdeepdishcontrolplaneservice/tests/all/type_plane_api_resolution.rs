// SPDX-License-Identifier: CC0-1.0

//! How the operations of an API's TYPE collection answer an `apiId` they cannot resolve.
//!
//! The rule these cases assert, one level above the request that exposed the defect (a
//! `ListTypes` handed a long hyphenated token no API was ever created under):
//!
//! > Which of the two client refusals the model binds to an API-scoped operation an `apiId`
//! > that names no API of this account draws is fixed per OPERATION, not read off the value:
//! > an operation that SCREENS the reference refuses a value of a form no API reference can
//! > have as the malformed request it is (400 `BadRequestException` naming the rejected
//! > value), while an operation that only RESOLVES the reference reports EVERY value it
//! > cannot resolve as the missing API it names — 404 `NotFoundException` with AppSync's
//! > fixed sentence (`state::API_NOT_FOUND_MESSAGE`) — whatever that value's length or
//! > alphabet. Every operation of the type collection (`/v1/apis/{apiId}/types…`) is the
//! > resolving kind.
//!
//! The rows span what the rule generalises over: the OPERATION the reference is handed to
//! (all five of the type collection — the listing, the create, the read, the update and the
//! delete) and WHY the reference resolves to nothing (an API deleted after it was published,
//! in both forms AppSync publishes an API's identity in; an id of the form the service mints
//! that was never minted; values one character past the id length, far past it, hyphenated,
//! hex and a single character; and ARNs of another Region and another account). A positive
//! control drives the same five operations under a reference that DOES resolve, so the rule
//! refuses only what names no API.
//!
//! The last case is the guard against inverting the mapping globally: `ListApiKeys` SCREENS
//! the reference it is handed, so for the same values it keeps the malformed-request refusal
//! that AWS records for it.
//!
//! The harness is the error-contract suite's (the router the production emulator mounts,
//! `call`), and the path encoder is the api-identifier suite's, both reused rather than
//! copied.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::api_identifier_resolution::label;
use super::error_contract::{call, router, Answer};

/// The one type definition these cases send, so a refusal can never be about the document.
const DEFINITION: &str = "type Widget { id: ID! }";

/// The type name the single-type operations address.
const TYPE_NAME: &str = "Widget";

/// One created API, in both forms AppSync publishes its identity in.
struct Published {
    api_id: String,
    arn: String,
}

/// Create one GraphQL API and read back both forms of its identity.
async fn publish(router: &MultiServiceRouter, name: &str) -> Published {
    let answer = call(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "API_KEY"})),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case create {name:?}: expected 200, got {answer:?}"
    );
    let api = &answer.body["graphqlApi"];
    Published {
        api_id: api["apiId"].as_str().unwrap_or_default().to_string(),
        arn: api["arn"].as_str().unwrap_or_default().to_string(),
    }
}

/// The five calls of an API's type collection, each a `(operation, method, path, body)`
/// addressed at `reference`.
fn type_collection(reference: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    let api = label(reference);
    vec![
        (
            "ListTypes",
            "GET",
            format!("/v1/apis/{api}/types?format=SDL"),
            None,
        ),
        (
            "CreateType",
            "POST",
            format!("/v1/apis/{api}/types"),
            Some(json!({"definition": DEFINITION, "format": "SDL"})),
        ),
        (
            "GetType",
            "GET",
            format!("/v1/apis/{api}/types/{TYPE_NAME}?format=SDL"),
            None,
        ),
        (
            "UpdateType",
            "POST",
            format!("/v1/apis/{api}/types/{TYPE_NAME}"),
            Some(json!({"definition": DEFINITION, "format": "SDL"})),
        ),
        (
            "DeleteType",
            "DELETE",
            format!("/v1/apis/{api}/types/{TYPE_NAME}"),
            None,
        ),
    ]
}

/// Every way a reference handed to the type collection can resolve to nothing, labelled by
/// what makes it resolve to nothing. `published` is an API that was created and then
/// deleted, so both of its forms once named an API and now name none.
fn unresolvable(published: &Published) -> Vec<(String, String)> {
    let (arn_prefix, _) = published
        .arn
        .rsplit_once('/')
        .expect("an appsync API ARN ends in apis/<apiId>");
    let region = published
        .arn
        .split(':')
        .nth(3)
        .expect("an ARN carries its Region in the fourth field");
    vec![
        ("the deleted API's id".to_string(), published.api_id.clone()),
        ("the deleted API's ARN".to_string(), published.arn.clone()),
        (
            "an id of the minted form that was never minted".to_string(),
            "z".repeat(state::API_ID_LEN),
        ),
        ("a single character".to_string(), "a".to_string()),
        (
            "one character past the id length".to_string(),
            "b".repeat(state::API_ID_LEN + 1),
        ),
        (
            "a hyphenated token far past the id length".to_string(),
            "probe-appsync-api-that-was-never-created-0001".to_string(),
        ),
        (
            "32 hexadecimal characters".to_string(),
            "f3a91c07e6b5d0a48c2e1976badcafe0".to_string(),
        ),
        (
            "an ARN naming an id that was never minted".to_string(),
            format!("{arn_prefix}/{}", "y".repeat(state::API_ID_LEN)),
        ),
        (
            "an ARN of another Region".to_string(),
            published.arn.replace(region, "ap-south-1"),
        ),
        (
            "an ARN of another account".to_string(),
            published
                .arn
                .replace(engine::EMULATOR_ACCOUNT_ID, "999999999999"),
        ),
    ]
}

/// The absent-API refusal, read off one answer.
fn refusal(answer: &Answer) -> (u16, &str, &str) {
    (
        answer.status,
        answer.error_type.as_str(),
        answer.message.as_str(),
    )
}

#[tokio::test]
async fn every_unresolvable_reference_is_the_missing_api_on_the_type_collection() {
    let router = router();
    let published = publish(&router, "type-plane-reference-deleted").await;
    let standing = publish(&router, "type-plane-reference-standing").await;

    // The API is deleted through its ARN, so both published forms of its identity now name
    // no API of this account.
    let deleted = call(
        &router,
        "DELETE",
        &format!("/v1/apis/{}", label(&published.arn)),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case DeleteGraphqlApi under the ARN: expected 200, got {deleted:?}"
    );

    for (form, reference) in unresolvable(&published) {
        for (operation, method, path, body) in type_collection(&reference) {
            let case = format!("{operation} addressed by {form}");
            let answer = call(&router, method, &path, body).await;
            assert_eq!(
                refusal(&answer),
                (404, "NotFoundException", state::API_NOT_FOUND_MESSAGE),
                "case {case:?}: expected 404 NotFoundException {:?}, got {answer:?}",
                state::API_NOT_FOUND_MESSAGE
            );
        }
    }

    // None of those refusals touched the API left standing: it still holds no type, and the
    // creates that were refused above wrote nothing under it.
    let listed = call(
        &router,
        "GET",
        &format!("/v1/apis/{}/types?format=SDL", label(&standing.api_id)),
        None,
    )
    .await;
    assert_eq!(
        (listed.status, listed.body["types"].as_array().map(Vec::len)),
        (200, Some(0)),
        "case ListTypes on the API left standing: expected an empty listing, got {listed:?}"
    );
}

#[tokio::test]
async fn the_type_collection_serves_a_reference_that_resolves() {
    let router = router();
    let published = publish(&router, "type-plane-reference-served").await;

    // The positive control, in BOTH published forms: the five operations the cases above
    // refuse are each served under a reference that does resolve, so what the rule refuses
    // is the reference naming no API and nothing else. The order is the one the collection
    // admits — the create first, then the reads, the update and the delete.
    for (form, reference) in [
        ("the minted id", published.api_id.clone()),
        ("the API's ARN", published.arn.clone()),
    ] {
        for (operation, method, path, body) in type_collection(&reference) {
            let case = format!("{operation} addressed by {form}");
            let answer = call(&router, method, &path, body).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200, got {answer:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_operation_that_screens_a_reference_keeps_its_malformed_request_refusal() {
    let router = router();
    let published = publish(&router, "type-plane-screening-guard").await;

    // `ListApiKeys` screens the reference it is handed, so the same values the type
    // collection answers as a missing API are a malformed request there when they are of no
    // form an API reference can have (longer than an API id), and the missing API when they
    // are of a form one can have.
    for (form, reference, expected) in [
        (
            "an id of the minted form that was never minted",
            "z".repeat(state::API_ID_LEN),
            (
                404,
                "NotFoundException",
                state::API_NOT_FOUND_MESSAGE.to_string(),
            ),
        ),
        (
            "a single character",
            "a".to_string(),
            (
                404,
                "NotFoundException",
                state::API_NOT_FOUND_MESSAGE.to_string(),
            ),
        ),
        (
            "one character past the id length",
            "b".repeat(state::API_ID_LEN + 1),
            (
                400,
                "BadRequestException",
                format!("Invalid API ID {}", "b".repeat(state::API_ID_LEN + 1)),
            ),
        ),
        (
            "a hyphenated token far past the id length",
            "probe-appsync-api-that-was-never-created-0001".to_string(),
            (
                400,
                "BadRequestException",
                "Invalid API ID probe-appsync-api-that-was-never-created-0001".to_string(),
            ),
        ),
        (
            "32 hexadecimal characters",
            "f3a91c07e6b5d0a48c2e1976badcafe0".to_string(),
            (
                400,
                "BadRequestException",
                "Invalid API ID f3a91c07e6b5d0a48c2e1976badcafe0".to_string(),
            ),
        ),
    ] {
        let answer = call(
            &router,
            "GET",
            &format!("/v1/apis/{}/apikeys", label(&reference)),
            None,
        )
        .await;
        assert_eq!(
            refusal(&answer),
            (expected.0, expected.1, expected.2.as_str()),
            "case ListApiKeys addressed by {form:?}: expected {} {} {:?}, got {answer:?}",
            expected.0,
            expected.1,
            expected.2
        );
    }

    // And the API that does exist is still served by the screening operation, so the guard
    // is about the reference and not about the operation being broken.
    let listed = call(
        &router,
        "GET",
        &format!("/v1/apis/{}/apikeys", label(&published.api_id)),
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case ListApiKeys on the created API: expected 200, got {listed:?}"
    );
}
