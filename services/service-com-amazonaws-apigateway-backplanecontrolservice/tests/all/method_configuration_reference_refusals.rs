// SPDX-License-Identifier: CC0-1.0

//! What this control plane REFUSES about the references and the value sets of the members a
//! `Method`, its `Integration` and an `Authorizer` carry, asserted on the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder, the authorizer
//! create helpers and the PATCH wrappers are this crate's existing harness, reused from
//! `rest_api_lifecycle`, `authorizer_lifecycle` and `patch_updates`.
//!
//! The RULES under test, each over a SET of inputs rather than one recorded call, and each
//! applied wherever the member is written (the create AND the patch):
//!
//! 1. A `TOKEN` authorizer authorizes on "a single authorization token submitted in a
//!    custom header" (`smithy_query(query="shape", shape="AuthorizerType")`) and its
//!    `identitySource` "specifies the request header mapping expression for the custom
//!    header holding the authorization token"
//!    (`smithy_query(query="doc", shape="CreateAuthorizerRequest",
//!    member="identitySource")`), so a value that is not such an expression is refused and
//!    no authorizer is created. A `REQUEST` authorizer's identity sources are "a
//!    comma-separated string of one or more mapping expressions" instead, so that form is
//!    not required of it.
//! 2. A `Method`'s `requestValidatorId` "specifies the identifier of a `RequestValidator`
//!    for validating the method request", so it has to name one OF THAT API: an identifier
//!    naming no validator is refused and no method is stored.
//! 3. A `passthroughBehavior` outside the three documented values is refused with that
//!    member's own fixed sentence, which names the options and quotes the rejected value
//!    back nowhere, and no integration is stored.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::ops::create_authorizer::{
    invalid_token_source, AUTHORIZER_TYPE_REQUEST, AUTHORIZER_TYPE_TOKEN,
};
use service_com_amazonaws_apigateway_backplanecontrolservice::ops::put_integration::{
    invalid_passthrough_behavior, PASSTHROUGH_BEHAVIORS,
};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    NO_SUCH_INTEGRATION, NO_SUCH_METHOD, NO_SUCH_REQUEST_VALIDATOR,
};

use crate::authorizer_lifecycle::{create_authorizer, get_authorizer, lambda_uri};
use crate::patch_updates::{patch, replace};
use crate::rest_api_lifecycle::{
    create_api, page_items, resource_under, string_member, Answer, Gateway,
};

const BAD_REQUEST: &str = "BadRequestException";
const NOT_FOUND: &str = "NotFoundException";

/// Assert `answer` is a refusal with `status`, `code` and exactly `message`.
fn assert_refusal(answer: &Answer, status: u16, code: &str, message: &str, case: &str) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected HTTP {status}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error.as_deref(),
        Some(code),
        "case {case}: expected {code}, got {:?} with {:?}",
        answer.error,
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

/// How many authorizers `api` holds, read through `GetAuthorizers`: what a refused create
/// must leave unchanged.
async fn authorizer_count(gateway: &Gateway, api: &str) -> usize {
    let listed = gateway
        .call("GET", &format!("/restapis/{api}/authorizers?limit=500"), None)
        .await;
    assert_eq!(
        listed.status, 200,
        "case GetAuthorizers: expected 200, got {} with {:?}",
        listed.status, listed.body
    );
    page_items(&listed.body, "GetAuthorizers").len()
}

/// The identity sources a `TOKEN` authorizer takes, each a method request header mapping
/// expression: rows differ in the header name's length and in which characters of the
/// published set (`[a-zA-Z0-9._-]`) it uses.
fn accepted_token_sources() -> Vec<&'static str> {
    vec![
        "method.request.header.A",
        "method.request.header.Auth",
        "method.request.header.X-Api-Key",
        "method.request.header.x_api.key-9",
    ]
}

/// The identity sources such an authorizer does NOT take: rows differ in HOW the value
/// fails the form — another parameter location, no location at all, a header name with no
/// characters, a character outside the published set, a comma-separated list (which is the
/// `REQUEST` type's form), surrounding whitespace and another expression language.
fn refused_token_sources() -> Vec<&'static str> {
    vec![
        "method.request.querystring.token",
        "method.request.path.token",
        "method.request.body.token",
        "method.request.header.",
        "method.request.header.Auth Token",
        "method.request.header.Auth,method.request.querystring.Name",
        " method.request.header.Auth",
        "Auth",
        "$request.header.Auth",
        "method.request.HEADER.Auth",
    ]
}

#[tokio::test]
async fn a_token_authorizers_identity_source_must_be_a_method_request_header() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "token-identity-source").await;

    // The type whose identity source is ONE header expression; the `REQUEST` type, whose
    // sources are a comma-separated list of mapping expressions, is driven below.
    let authorizer_type = AUTHORIZER_TYPE_TOKEN;

    for (index, source) in refused_token_sources().iter().enumerate() {
        let case = format!("{authorizer_type} authorizer with identitySource {source:?}");
        let before = authorizer_count(&gateway, &api).await;
        let request = json!({
            "name": format!("refused-{index}"),
            "type": authorizer_type,
            "identitySource": source,
            "authorizerUri": lambda_uri("authorize"),
        });
        let answer = create_authorizer(&gateway, &api, request).await;
        assert_refusal(
            &answer,
            400,
            BAD_REQUEST,
            &invalid_token_source(source),
            &case,
        );
        assert_eq!(
            authorizer_count(&gateway, &api).await,
            before,
            "case {case}: a refused create must add no authorizer"
        );
    }

    for (index, source) in accepted_token_sources().iter().enumerate() {
        let case = format!("{authorizer_type} authorizer with identitySource {source:?}");
        let request = json!({
            "name": format!("accepted-{index}"),
            "type": authorizer_type,
            "identitySource": source,
            "authorizerUri": lambda_uri("authorize"),
        });
        let answer = create_authorizer(&gateway, &api, request).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        // Round trip: the stored authorizer reports the source it was created with.
        let id = string_member(&answer.body, "id", &case);
        let read = get_authorizer(&gateway, &api, &id).await;
        assert_eq!(
            read.body["identitySource"].as_str(),
            Some(*source),
            "case {case}: GetAuthorizer must report the stored source, got {:?}",
            read.body["identitySource"]
        );
    }

    // A `REQUEST` authorizer's identity sources are a comma-separated list of mapping
    // expressions over any parameter location, so the values refused above are its own
    // documented form and are accepted.
    let request_typed = create_authorizer(
        &gateway,
        &api,
        json!({
            "name": "request-list-sources",
            "type": AUTHORIZER_TYPE_REQUEST,
            "identitySource": "method.request.header.Auth,method.request.querystring.Name",
            "authorizerUri": lambda_uri("authorize"),
        }),
    )
    .await;
    assert_eq!(
        request_typed.status, 201,
        "case a REQUEST authorizer over two identity sources: expected 201, got {} with {:?}",
        request_typed.status, request_typed.body
    );
}

#[tokio::test]
async fn a_patch_cannot_leave_a_token_authorizer_with_an_unreadable_identity_source() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "patched-identity-source").await;
    let stored = "method.request.header.Auth";
    let created = create_authorizer(
        &gateway,
        &api,
        json!({
            "name": "patched-token",
            "type": AUTHORIZER_TYPE_TOKEN,
            "identitySource": stored,
            "authorizerUri": lambda_uri("authorize"),
        }),
    )
    .await;
    let id = string_member(&created.body, "id", "CreateAuthorizer");
    let uri = format!("/restapis/{api}/authorizers/{id}");

    for source in refused_token_sources() {
        let case = format!("replace /identitySource with {source:?}");
        let answer = patch(&gateway, &uri, vec![replace("/identitySource", source)]).await;
        assert_refusal(
            &answer,
            400,
            BAD_REQUEST,
            &invalid_token_source(source),
            &case,
        );
        let read = get_authorizer(&gateway, &api, &id).await;
        assert_eq!(
            read.body["identitySource"].as_str(),
            Some(stored),
            "case {case}: the refused patch must leave the stored source, got {:?}",
            read.body["identitySource"]
        );
    }

    // The same patch with a header expression is applied and read back.
    let patched = patch(
        &gateway,
        &uri,
        vec![replace("/identitySource", "method.request.header.X-Token")],
    )
    .await;
    assert_eq!(
        patched.status, 200,
        "case replace /identitySource with a header expression: expected 200, got {} with {:?}",
        patched.status, patched.body
    );
    let read = get_authorizer(&gateway, &api, &id).await;
    assert_eq!(
        read.body["identitySource"].as_str(),
        Some("method.request.header.X-Token"),
        "case replace /identitySource with a header expression: expected the patched source, \
         got {:?}",
        read.body["identitySource"]
    );
}

/// `PutMethod` on `verb` of `resource` with `body`.
async fn put_method(
    gateway: &Gateway,
    api: &str,
    resource: &str,
    verb: &str,
    body: Value,
) -> Answer {
    gateway
        .call(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/{verb}"),
            Some(body),
        )
        .await
}

/// `GetMethod` for `verb` of `resource`.
async fn get_method(gateway: &Gateway, api: &str, resource: &str, verb: &str) -> Answer {
    gateway
        .call(
            "GET",
            &format!("/restapis/{api}/resources/{resource}/methods/{verb}"),
            None,
        )
        .await
}

/// `CreateRequestValidator` on `api`, answering its id.
async fn create_validator(gateway: &Gateway, api: &str, name: &str) -> String {
    let answer = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/requestvalidators"),
            Some(json!({ "name": name, "validateRequestBody": true })),
        )
        .await;
    assert_eq!(
        answer.status, 201,
        "case CreateRequestValidator({name}): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "id", &format!("CreateRequestValidator({name})"))
}

#[tokio::test]
async fn a_methods_request_validator_id_must_name_a_validator_of_its_own_api() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "method-validator-reference").await;
    let resource = resource_under(&gateway, &api, &root, "pets").await;
    let validator = create_validator(&gateway, &api, "body-validator").await;

    // Another API's validator, and one of this API that is then DELETED: both are
    // identifiers that were really minted, which an id-shaped check would miss.
    let (other_api, _other_root) = create_api(&gateway, "method-validator-other").await;
    let other_validator = create_validator(&gateway, &other_api, "other-validator").await;
    let doomed = create_validator(&gateway, &api, "doomed-validator").await;
    let deleted = gateway
        .call(
            "DELETE",
            &format!("/restapis/{api}/requestvalidators/{doomed}"),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 202,
        "case DeleteRequestValidator: expected the modelled 202, got {} with {:?}",
        deleted.status, deleted.body
    );

    // Rows differ in the FORM the identifier takes that names no validator OF THIS API:
    // never minted, id-shaped, the api's own id, the resource's id, another api's
    // validator, and a deleted one.
    let refused: Vec<(String, String)> = vec![
        ("never minted".to_string(), "sim2realnone".to_string()),
        ("id-shaped".to_string(), "0000000000".to_string()),
        ("the api's own id".to_string(), api.clone()),
        ("the resource's id".to_string(), resource.clone()),
        ("another api's validator".to_string(), other_validator.clone()),
        ("a deleted validator".to_string(), doomed.clone()),
    ];

    // The verbs the rows are put on come from the catalog, one per row, so no row collides
    // with another's method — and every refused row is then shown to have stored nothing.
    let verbs = ["POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"];
    for ((case, identifier), verb) in refused.iter().zip(verbs) {
        let case = format!("PutMethod({verb}) with a requestValidatorId that is {case}");
        let answer = put_method(
            &gateway,
            &api,
            &resource,
            verb,
            json!({ "authorizationType": "NONE", "requestValidatorId": identifier }),
        )
        .await;
        assert_refusal(
            &answer,
            400,
            BAD_REQUEST,
            NO_SUCH_REQUEST_VALIDATOR,
            &case,
        );
        // Nothing was stored, so the verb names no method on the resource.
        let read = get_method(&gateway, &api, &resource, verb).await;
        assert_refusal(&read, 404, NOT_FOUND, NO_SUCH_METHOD, &case);
    }

    // The accepted rows: a validator of this api, the member absent, and the member set to
    // no characters — which names no validator, the same unnamed validator an absent
    // member is, so there is no reference to resolve and nothing to refuse.
    let accepted: Vec<(&str, &str, Value, Option<&str>)> = vec![
        (
            "a validator of this api",
            "GET",
            json!({ "authorizationType": "NONE", "requestValidatorId": validator }),
            Some(validator.as_str()),
        ),
        (
            "no requestValidatorId at all",
            "POST",
            json!({ "authorizationType": "NONE" }),
            None,
        ),
        (
            "a requestValidatorId of no characters",
            "PUT",
            json!({ "authorizationType": "NONE", "requestValidatorId": "" }),
            Some(""),
        ),
    ];
    for (case, verb, body, expected) in &accepted {
        let case = format!("PutMethod({verb}) with {case}");
        let answer = put_method(&gateway, &api, &resource, verb, body.clone()).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        // Round trip: the stored method reports the validator it names.
        let read = get_method(&gateway, &api, &resource, verb).await;
        let reported = read.body["requestValidatorId"].as_str();
        assert_eq!(
            reported, *expected,
            "case {case}: GetMethod must report {expected:?}, got {reported:?}"
        );
    }

    // And the patch that writes the same member is held to the same rule.
    let uri = format!("/restapis/{api}/resources/{resource}/methods/GET");
    let patched = patch(
        &gateway,
        &uri,
        vec![replace("/requestValidatorId", &other_validator)],
    )
    .await;
    assert_refusal(
        &patched,
        400,
        BAD_REQUEST,
        NO_SUCH_REQUEST_VALIDATOR,
        "replace /requestValidatorId with another api's validator",
    );
    let read = get_method(&gateway, &api, &resource, "GET").await;
    assert_eq!(
        read.body["requestValidatorId"].as_str(),
        Some(validator.as_str()),
        "case replace /requestValidatorId with another api's validator: the refused patch \
         must leave the stored validator, got {:?}",
        read.body["requestValidatorId"]
    );
}

/// `PutIntegration` on `verb` of `resource` with `body`.
async fn put_integration(
    gateway: &Gateway,
    api: &str,
    resource: &str,
    verb: &str,
    body: Value,
) -> Answer {
    gateway
        .call(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/{verb}/integration"),
            Some(body),
        )
        .await
}

#[tokio::test]
async fn a_passthrough_behavior_outside_the_documented_set_is_refused_with_one_sentence() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "passthrough-behavior").await;
    let resource = resource_under(&gateway, &api, &root, "pets").await;
    let verb = "GET";
    let accepted = put_method(
        &gateway,
        &api,
        &resource,
        verb,
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    assert_eq!(
        accepted.status, 201,
        "case PutMethod(GET): expected 201, got {} with {:?}",
        accepted.status, accepted.body
    );
    let integration_uri =
        format!("/restapis/{api}/resources/{resource}/methods/{verb}/integration");

    // Rows differ in HOW the value is outside the documented set: a value of another
    // member's set, a documented value in another case, one with whitespace around it, a
    // value that is no identifier at all, and a near miss of a documented one.
    let refused = [
        "sim2real-not-a-real-value",
        "when_no_match",
        "WHEN_NO_MATCH ",
        "WHEN-NO-MATCH",
        "ALWAYS",
        "NEVER_EVER",
    ];
    for value in refused {
        let case = format!("PutIntegration(passthroughBehavior={value:?})");
        let answer = put_integration(
            &gateway,
            &api,
            &resource,
            verb,
            json!({
                "type": "MOCK",
                "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
                "passthroughBehavior": value,
            }),
        )
        .await;
        assert_refusal(
            &answer,
            400,
            BAD_REQUEST,
            &invalid_passthrough_behavior(),
            &case,
        );
        // And no integration was stored, so the method still has none.
        let read = gateway.call("GET", &integration_uri, None).await;
        assert_refusal(&read, 404, NOT_FOUND, NO_SUCH_INTEGRATION, &case);
    }

    // Every documented value is accepted and read back: the refusals above are about the
    // value and not about the member.
    for value in PASSTHROUGH_BEHAVIORS {
        let case = format!("PutIntegration(passthroughBehavior={value:?})");
        let answer = put_integration(
            &gateway,
            &api,
            &resource,
            verb,
            json!({
                "type": "MOCK",
                "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
                "passthroughBehavior": value,
            }),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        let read = gateway.call("GET", &integration_uri, None).await;
        assert_eq!(
            read.body["passthroughBehavior"].as_str(),
            Some(*value),
            "case {case}: GetIntegration must report the stored behaviour, got {:?}",
            read.body["passthroughBehavior"]
        );
    }

    // The patch that writes the same member states the same sentence.
    for value in refused {
        let case = format!("UpdateIntegration(replace /passthroughBehavior with {value:?})");
        let answer = patch(
            &gateway,
            &integration_uri,
            vec![replace("/passthroughBehavior", value)],
        )
        .await;
        assert_refusal(
            &answer,
            400,
            BAD_REQUEST,
            &invalid_passthrough_behavior(),
            &case,
        );
    }
}
