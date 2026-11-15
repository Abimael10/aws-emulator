// SPDX-License-Identifier: CC0-1.0

//! How this service refuses the PAYLOAD of the operations that take one: a request that
//! carries no payload at all, and a definition document that does not say which format it
//! is written in.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, and each rule is asserted over a SET of inputs rather than one call:
//!
//! * a request carrying no payload is refused by whichever layer READS that payload: the
//!   API key import has no reader of its own for zero bytes, so what is wrong with the
//!   request is the absent `@required` member and it is refused as that (400
//!   `ValidationException`, the member named on its own), while the two definition imports
//!   read the body themselves and state their own sentence for an empty definition;
//! * a definition document whose OpenAPI/Swagger version this service cannot determine —
//!   because the document declares neither version member, or because no reader got far
//!   enough into the body to find one — is refused as exactly that, which is a different
//!   refusal from a VERSIONED document that cannot be read as that format.

use crate::rest_api_create_from_import::api_count;
use crate::rest_api_lifecycle::{create_api, page_items, Answer, Gateway};

/// The error class of the layer in FRONT of an operation, which a request that leaves a
/// required member out is refused under: not the operation's own `BadRequestException`.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The operation's own client-input error: a refusal the HANDLER makes about a request it
/// read.
const BAD_REQUEST: &str = "BadRequestException";

/// The sentence that layer states for an absent required member, as it names a member
/// that carries the whole request PAYLOAD: by its own member name, with no input-shape
/// root, because the body is not a member of the input document — it IS the body.
fn absent_member_sentence(member: &str) -> String {
    format!(
        "1 validation error detected: Value null at '{member}' failed to satisfy \
         constraint: Member must not be null"
    )
}

/// The sentence the two definition imports state for a request that carries no definition.
///
/// Source: `crate::ops::put_rest_api::EMPTY_DEFINITION`, the one sentence both of them
/// read an empty body with.
const EMPTY_DEFINITION: &str = "Invalid swagger definition - request body was empty";

/// The sentence both definition imports state for a document that declares no version of
/// the format it is written in.
///
/// Source: `crate::ops::put_rest_api::UNDETERMINED_VERSION`.
const UNDETERMINED_VERSION: &str = "Unable to determine OAS/Swagger version";

/// The `message` an answer carries, or a panic naming the case.
fn message(answer: &Answer, case: &str) -> String {
    answer.body["message"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected a refusal stating what to fix, got {:?}",
                answer.body
            )
        })
        .to_string()
}

/// Assert `answer` is the HTTP 400 refusal `code` carrying the message `expected`.
fn assert_refusal(answer: &Answer, code: &str, expected: &str, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error.as_deref(),
        Some(code),
        "case {case}: expected the error type {code:?}, got {:?}",
        answer.error
    );
    let stated = message(answer, case);
    assert_eq!(
        stated, expected,
        "case {case}: expected {expected:?}, got {stated:?}"
    );
}

/// RULE: a request that carries no payload is refused by the layer that READS that
/// payload — so an operation with no reader of its own for zero bytes refuses the absent
/// `@required` member (400 `ValidationException`, the member named alone), while the
/// operations that read the body themselves state their own sentence for an empty one —
/// and either way the refusal is about the payload the caller did not send rather than
/// about the content of one it did.
///
/// The rows are the operations whose input binds a payload, from
/// `smithy_query(query="operation", operation="ImportApiKeys,ImportRestApi,PutRestApi")`,
/// each of which declares `body` as `@required @httpPayload`. They differ along the verb,
/// the route, whether the operation is addressed under an api, and which of the two
/// refusals the payload's reader makes.
#[tokio::test]
async fn a_request_carrying_no_payload_is_refused_for_the_payload_it_did_not_send() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "payload-refusals").await;
    let before = api_count(&gateway, "before the refusals").await;

    // `body` is the member every one of these operations binds its payload to, so one
    // absent-member sentence serves every row that earns one.
    let absent_body = absent_member_sentence("body");

    // (case, method, uri, error class, sentence)
    let rows: Vec<(String, &str, String, &str, &str)> = vec![
        (
            "ImportApiKeys".to_string(),
            "POST",
            "/apikeys?mode=import&format=csv&failonwarnings=false".to_string(),
            VALIDATION_EXCEPTION,
            absent_body.as_str(),
        ),
        (
            "ImportRestApi".to_string(),
            "POST",
            "/restapis?mode=import".to_string(),
            BAD_REQUEST,
            EMPTY_DEFINITION,
        ),
        (
            "PutRestApi, merging".to_string(),
            "PUT",
            format!("/restapis/{api}?mode=merge"),
            BAD_REQUEST,
            EMPTY_DEFINITION,
        ),
        (
            "PutRestApi, overwriting".to_string(),
            "PUT",
            format!("/restapis/{api}?mode=overwrite"),
            BAD_REQUEST,
            EMPTY_DEFINITION,
        ),
    ];

    for (case, method, uri, code, sentence) in &rows {
        let answer = gateway.call_raw(method, uri, "").await;
        assert_refusal(&answer, code, sentence, case);
    }

    // None of those refusals created anything: no API, and no API key.
    let after = api_count(&gateway, "after the refusals").await;
    assert_eq!(
        after, before,
        "case after the refusals: a refused call must create no API, expected {before}, \
         got {after}"
    );
    let keys = gateway.call("GET", "/apikeys?limit=500", None).await;
    assert_eq!(
        page_items(&keys.body, "GetApiKeys after the refusals").len(),
        0,
        "case after the refusals: a refused import must create no API key, got {:?}",
        keys.body
    );

    // The same calls WITH a payload are served: the rule is about the payload that was
    // not sent, not about those routes.
    let served: Vec<(String, &str, String, String)> = vec![
        (
            "ImportApiKeys with a payload".to_string(),
            "POST",
            "/apikeys?mode=import&format=csv&failonwarnings=false".to_string(),
            "key,name\nserved-api-key-value-0123456789,served\n".to_string(),
        ),
        (
            "ImportRestApi with a payload".to_string(),
            "POST",
            "/restapis?mode=import".to_string(),
            definition_with_version("served import"),
        ),
        (
            "PutRestApi with a payload".to_string(),
            "PUT",
            format!("/restapis/{api}?mode=overwrite"),
            definition_with_version("served put"),
        ),
    ];
    for (case, method, uri, payload) in &served {
        let answer = gateway.call_raw(method, uri, payload).await;
        assert!(
            answer.status < 300,
            "case {case}: expected the call to be served, got {} with {:?}",
            answer.status, answer.body
        );
    }
}

/// An OpenAPI 3 definition declaring one path, as JSON.
fn definition_with_version(title: &str) -> String {
    format!(
        "{{\"openapi\": \"3.0.1\", \"info\": {{\"title\": \"{title}\", \"version\": \"1\"}}, \
         \"paths\": {{\"/pets\": {{\"get\": {{\"responses\": {{\"200\": \
         {{\"description\": \"ok\"}}}}}}}}}}}}"
    )
}

/// RULE: which of the two formats a definition is written in is settled BEFORE the
/// document is read as one, out of the two root members an OpenAPI/Swagger document states
/// its version in — so a body that declares neither, and a body no reader got far enough
/// into to find one, are both refused as a version that could not be determined, whatever
/// else the body carries and whichever spelling it is written in.
///
/// The two members are the OpenAPI Specification's own root fields: `swagger` (2.0 and
/// before) and `openapi` (3.0 on). The refused rows differ along WHY no version was found
/// — what a readable document carries instead (nothing but an `info`, paths as well, a
/// member whose name merely resembles the version field in Swagger 1.2's `swaggerVersion`,
/// an `info.version`, which is the API's own version and not the format's), the shape it
/// parsed to (a mapping, a sequence, a scalar), and where a body stopped being readable at
/// all (an unterminated flow mapping, text that is no document, a quoted scalar that never
/// closes) — and along the spelling, JSON and the YAML the same document is written in.
/// The served rows are the same documents carrying each of the two members, which must
/// NOT earn that sentence.
#[tokio::test]
async fn a_definition_whose_version_cannot_be_determined_is_refused_as_that() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "version-detection").await;
    let before = api_count(&gateway, "before the refusals").await;

    // (case, document)
    let refused: Vec<(&str, String)> = vec![
        (
            "an object carrying only an info",
            "{\"info\": {\"title\": \"no version\"}}".to_string(),
        ),
        (
            "an object carrying paths as well",
            "{\"info\": {\"title\": \"no version\"}, \"paths\": {\"/pets\": {\"get\": {}}}}"
                .to_string(),
        ),
        (
            "a document whose only version is the API's own",
            "{\"info\": {\"title\": \"no version\", \"version\": \"2026-01-01\"}}".to_string(),
        ),
        (
            "a document carrying Swagger 1.2's own version field",
            "{\"swaggerVersion\": \"1.2\", \"info\": {\"title\": \"no version\"}}".to_string(),
        ),
        ("a document that parsed to a list", "[]".to_string()),
        (
            "a document that parsed to a scalar",
            "\"just a string\"".to_string(),
        ),
        (
            "the same document written as YAML",
            "info:\n  title: no version\npaths:\n  /pets:\n    get: {}\n".to_string(),
        ),
        (
            "a YAML mapping carrying nothing but a title",
            "info:\n  title: no version\n".to_string(),
        ),
        (
            "an unterminated flow mapping",
            "{\"openapi\": \"3.0.1\", \"paths\": {".to_string(),
        ),
        (
            "text that is no document at all",
            "}not a document{".to_string(),
        ),
        (
            "an unterminated quoted scalar",
            "openapi: \"3.0.1\ninfo:\n  title: x\n".to_string(),
        ),
    ];

    for (case, body) in &refused {
        let imported = import_rest_api(&gateway, body).await;
        assert_refusal(
            &imported,
            BAD_REQUEST,
            UNDETERMINED_VERSION,
            &format!("ImportRestApi with {case}"),
        );
        let put = gateway
            .call_raw("PUT", &format!("/restapis/{api}?mode=overwrite"), body)
            .await;
        assert_refusal(
            &put,
            BAD_REQUEST,
            UNDETERMINED_VERSION,
            &format!("PutRestApi with {case}"),
        );
    }

    let after = api_count(&gateway, "after the refusals").await;
    assert_eq!(
        after, before,
        "case after the refusals: a refused import must create no API, expected {before}, \
         got {after}"
    );

    // A document that DOES declare its version, in either member and either spelling, is
    // imported: the sentence above is about the absent version and nothing else.
    let served: Vec<(&str, String)> = vec![
        (
            "an openapi 3 document",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"versioned\"}, \"paths\": {}}"
                .to_string(),
        ),
        (
            "a swagger 2 document",
            "{\"swagger\": \"2.0\", \"info\": {\"title\": \"versioned\"}, \"paths\": {}}"
                .to_string(),
        ),
        (
            "an openapi 3 document written as YAML",
            "openapi: 3.0.1\ninfo:\n  title: versioned\npaths: {}\n".to_string(),
        ),
        (
            "a swagger 2 document written as YAML",
            "swagger: '2.0'\ninfo:\n  title: versioned\npaths: {}\n".to_string(),
        ),
    ];
    for (case, body) in &served {
        let imported = import_rest_api(&gateway, body).await;
        assert_eq!(
            imported.status, 201,
            "case ImportRestApi with {case}: expected 201, got {} with {:?}",
            imported.status, imported.body
        );
        let put = gateway
            .call_raw("PUT", &format!("/restapis/{api}?mode=overwrite"), body)
            .await;
        assert_eq!(
            put.status, 200,
            "case PutRestApi with {case}: expected 200, got {} with {:?}",
            put.status, put.body
        );
    }
}

/// RULE: once the version IS declared, the document is read as that format — and a
/// document that cannot be read as one is the refusal beside the version one, so a caller
/// can tell "I could not tell what this is" from "this is not a valid one of those".
///
/// Rows differ along WHERE the versioned document stops being a definition, and along the
/// version member it declares and the spelling it is written in.
#[tokio::test]
async fn a_versioned_document_that_cannot_be_read_keeps_the_invalid_input_sentence() {
    // Source: `crate::ops::put_rest_api::INVALID_DEFINITION`.
    const INVALID_DEFINITION: &str = "Invalid OAS input";

    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "unreadable-definitions").await;

    let refused: Vec<(&str, &str)> = vec![
        (
            "paths that are a list, under openapi",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"x\"}, \"paths\": [\"/pets\"]}",
        ),
        (
            "paths that are a scalar, under swagger",
            "{\"swagger\": \"2.0\", \"info\": {\"title\": \"x\"}, \"paths\": \"/pets\"}",
        ),
        (
            "paths that are a list, written as YAML",
            "openapi: 3.0.1\ninfo:\n  title: x\npaths:\n  - /pets\n",
        ),
    ];

    for (case, body) in &refused {
        let imported = import_rest_api(&gateway, body).await;
        assert_refusal(
            &imported,
            BAD_REQUEST,
            INVALID_DEFINITION,
            &format!("ImportRestApi with {case}"),
        );
        let put = gateway
            .call_raw("PUT", &format!("/restapis/{api}?mode=overwrite"), body)
            .await;
        assert_refusal(
            &put,
            BAD_REQUEST,
            INVALID_DEFINITION,
            &format!("PutRestApi with {case}"),
        );
    }
}

/// `ImportRestApi` with `body` sent verbatim.
async fn import_rest_api(gateway: &Gateway, body: &str) -> Answer {
    gateway
        .call_raw("POST", "/restapis?mode=import", body)
        .await
}
