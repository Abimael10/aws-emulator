// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for how API Gateway V2 WORDS two refusals of its own: a cached
//! authorizer TTL outside the window the model declares for it, and an export asked for in
//! an output type the operation does not serve.
//!
//! The rules these cases assert:
//!
//! * **An `AuthorizerResultTtlInSeconds` outside the window its shape declares
//!   (`IntegerWithLengthBetween0And3600`, range 0..3600 — `smithy_query(query="shape",
//!   shape="IntegerWithLengthBetween0And3600")`) is refused `400 BadRequestException` with
//!   the service's own sentence**, which states the whole window in the member's unit and
//!   echoes neither the rejected value nor the wire spelling of the member; both published
//!   bounds are accepted and round-trip, and a refused call mints nothing.
//! * **An `ExportApi` `OutputType` outside the set that member documents (`JSON` and
//!   `YAML` — `smithy_query(query="operation", operation="ExportApi")`) is refused `400
//!   BadRequestException` with the service's own sentence**, which states the output type
//!   that was asked for verbatim and then the whole set it admits; each member of that set
//!   is served a non-empty document.
//!
//! The rows vary what each rule generalises over: for the TTL, the side of the window the
//! value sits outside and how far (one past each bound, far past the ceiling, the extremes
//! of the member's own integer type) and which writer the value reaches the service through
//! (`CreateAuthorizer`, `UpdateAuthorizer`); for the output type, the form of the value (a
//! name of another serialization, a served name spelled in the wrong case, a name differing
//! in one character, a name carrying punctuation) and the owning api's state.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_export::{export_text, OAS30};
use crate::api_lifecycle::{as_str, call, deployable_route, emulator, error_code};

/// The window `AuthorizerResultTtlInSeconds` has to fall in, from the shape every writer of
/// that member targets (`IntegerWithLengthBetween0And3600`, range 0..3600).
const TTL_WINDOW: (i64, i64) = (0, 3600);

/// The sentence API Gateway refuses a TTL outside [`TTL_WINDOW`] with: fixed text naming
/// the window, with the rejected value nowhere in it.
const TTL_OUT_OF_RANGE_MESSAGE: &str =
    "Authorizer result TTL outside allowable range. TTL must be between 0 and 3600 seconds.";

/// A well-formed Lambda authorizer URI, the form `AuthorizerUri` documents: "For REQUEST
/// authorizers, this must be a well-formed Lambda function URI, for example,
/// arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/{function-arn}/invocations".
fn lambda_uri(function: &str) -> String {
    format!(
        "arn:aws:apigateway:us-east-1:lambda:path/2015-03-31/functions/\
         arn:aws:lambda:us-east-1:123456789012:function:{function}/invocations"
    )
}

/// Create one HTTP api and answer its `ApiId`.
async fn create_http_api(emulator: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// A `REQUEST` authorizer request carrying `ttl` when the row names one.
fn authorizer_request(name: &str, ttl: Option<i64>) -> Value {
    let mut request = json!({
        "name": name,
        "authorizerType": "REQUEST",
        "authorizerUri": lambda_uri("ttl-auth"),
        "authorizerPayloadFormatVersion": "2.0",
        "identitySource": ["$request.header.Authorization"]
    });
    if let Some(ttl) = ttl {
        request["authorizerResultTtlInSeconds"] = json!(ttl);
    }
    request
}

/// How many authorizers the api holds.
async fn authorizer_count(emulator: &MultiServiceRouter, case: &str, api_id: &str) -> usize {
    let (status, listed) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/authorizers"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetAuthorizers, got {status} {listed}"
    );
    listed["items"].as_array().map_or(0, Vec::len)
}

#[tokio::test]
async fn a_result_ttl_outside_the_modelled_window_is_refused_with_the_services_own_sentence() {
    let emulator = emulator();
    let case = "the TTL api";
    let api_id = create_http_api(&emulator, case, "authorizer-ttl-wording-api").await;

    // Each value outside the window: one past each published bound, far past the ceiling,
    // and the extremes of the integer the member is modelled as.
    let outside = [
        ("one past the ceiling", TTL_WINDOW.1 + 1),
        ("one below the floor", TTL_WINDOW.0 - 1),
        ("two hours, far past the ceiling", 7_200),
        ("the largest value the member's type holds", 2_147_483_647),
        ("the smallest value the member's type holds", -2_147_483_648),
    ];
    for (label, ttl) in outside {
        let before = authorizer_count(&emulator, label, &api_id).await;
        let (status, refused) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/authorizers"),
            Some(authorizer_request("ttl-out-of-range", Some(ttl))),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected 400 from CreateAuthorizer for a TTL of {ttl}, \
             got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {label:?}: expected BadRequestException, got {refused}"
        );
        assert_eq!(
            as_str(&refused, "message"),
            TTL_OUT_OF_RANGE_MESSAGE,
            "case {label:?}: expected the refusal to be worded {TTL_OUT_OF_RANGE_MESSAGE:?}, \
             got {refused}"
        );
        // The sentence states the window, not what was sent: neither the rejected value nor
        // the wire spelling of the member appears in it.
        let message = as_str(&refused, "message");
        assert!(
            !message.contains(&ttl.to_string()),
            "case {label:?}: the refusal must not echo the rejected value, got {message:?}"
        );
        assert!(
            !message.contains("authorizerResultTtlInSeconds"),
            "case {label:?}: the refusal must not name the wire member, got {message:?}"
        );
        assert_eq!(
            authorizer_count(&emulator, label, &api_id).await,
            before,
            "case {label:?}: a refused create must mint no authorizer"
        );
    }

    // Both published bounds, and a value between them, are accepted and round-trip.
    for (label, ttl) in [
        ("the floor, which disables caching", TTL_WINDOW.0),
        ("the ceiling, one hour", TTL_WINDOW.1),
        ("a value inside the window", 300),
    ] {
        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/authorizers"),
            Some(authorizer_request(&format!("ttl-{ttl}"), Some(ttl))),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {label:?}: expected 201 from CreateAuthorizer for a TTL of {ttl}, \
             got {status} {created}"
        );
        let authorizer_id = as_str(&created, "authorizerId");
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/authorizers/{authorizer_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200 from GetAuthorizer, got {status} {fetched}"
        );
        assert_eq!(
            fetched.get("authorizerResultTtlInSeconds"),
            Some(&json!(ttl)),
            "case {label:?}: expected the stored TTL {ttl}, got {fetched}"
        );
    }
}

#[tokio::test]
async fn both_writers_of_a_result_ttl_word_the_refusal_the_same_way() {
    let emulator = emulator();
    let case = "the TTL update api";
    let api_id = create_http_api(&emulator, case, "authorizer-ttl-update-api").await;

    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(authorizer_request("ttl-patched", Some(300))),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateAuthorizer, got {status} {created}"
    );
    let authorizer_id = as_str(&created, "authorizerId");
    let authorizer_uri = format!("/v2/apis/{api_id}/authorizers/{authorizer_id}");

    for (label, ttl) in [
        ("one past the ceiling", TTL_WINDOW.1 + 1),
        ("one below the floor", TTL_WINDOW.0 - 1),
    ] {
        let (status, refused) = call(
            &emulator,
            "PATCH",
            &authorizer_uri,
            Some(json!({ "authorizerResultTtlInSeconds": ttl })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected 400 from UpdateAuthorizer for a TTL of {ttl}, \
             got {status} {refused}"
        );
        assert_eq!(
            as_str(&refused, "message"),
            TTL_OUT_OF_RANGE_MESSAGE,
            "case {label:?}: expected the refusal to be worded {TTL_OUT_OF_RANGE_MESSAGE:?}, \
             got {refused}"
        );
    }

    // The refused patches changed nothing: the authorizer still reports the TTL it was
    // created with.
    let (status, fetched) = call(&emulator, "GET", &authorizer_uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetAuthorizer, got {status} {fetched}"
    );
    assert_eq!(
        fetched.get("authorizerResultTtlInSeconds"),
        Some(&json!(300)),
        "case {case:?}: a refused patch must leave the stored TTL alone, got {fetched}"
    );
}

/// The output types `ExportApi$OutputType` documents: "Valid values are JSON and YAML",
/// listed in the order that sentence lists them.
const OUTPUT_TYPES: [&str; 2] = ["JSON", "YAML"];

/// The sentence API Gateway refuses an output type outside [`OUTPUT_TYPES`] with, for the
/// value `asked_for`: fixed text around the value as the request spelled it, and the whole
/// set it admits.
fn output_type_message(asked_for: &str) -> String {
    format!(
        "Output type is {asked_for}. Output type should be one of following : {}.",
        OUTPUT_TYPES.join(", ")
    )
}

#[tokio::test]
async fn an_export_output_type_outside_the_documented_set_is_refused_naming_the_value() {
    let emulator = emulator();
    let case = "the export api";
    let api_id = create_http_api(&emulator, case, "export-output-type-api").await;
    deployable_route(&emulator, case, &api_id).await;

    // The rows differ in the FORM of the value: another serialization's name, a served name
    // in the wrong case, a name one character off, a name carrying punctuation, and a name
    // that is only a prefix of a served one.
    for (label, asked_for) in [
        ("another serialization's name", "XML"),
        ("a served name in lower case", "yaml"),
        ("a served name in mixed case", "Json"),
        ("a name one character off", "YML"),
        ("a name carrying punctuation", "JSON-LD"),
        ("a prefix of a served name", "JS"),
    ] {
        let (status, refused) = export_text(
            &emulator,
            &api_id,
            OAS30,
            &format!("outputType={asked_for}"),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected 400 from ExportApi for {asked_for:?}, got {status} {refused}"
        );
        let body: Value = serde_json::from_str(&refused).unwrap_or_else(|error| {
            panic!("case {label:?}: the refusal must be a JSON envelope, got {refused:?}: {error}")
        });
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {label:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            output_type_message(asked_for),
            "case {label:?}: expected the refusal to name the output type asked for, got {body}"
        );
    }

    // Each output type the member documents is served a document of its own, so the refusal
    // above is about the value and not about this api.
    for output_type in OUTPUT_TYPES {
        let (status, served) = export_text(
            &emulator,
            &api_id,
            OAS30,
            &format!("outputType={output_type}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {output_type:?}: expected 200 from ExportApi, got {status} {served}"
        );
        assert!(
            !served.trim().is_empty(),
            "case {output_type:?}: an export must carry a document, got {served:?}"
        );
    }
}
