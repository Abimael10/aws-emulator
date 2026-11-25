// SPDX-License-Identifier: CC0-1.0

//! Which of the three sentences an imported API definition is refused with, as a client
//! sees it: the step that failed picks the sentence, and version detection is the step
//! that answers FIRST.
//!
//! One rule is pinned here, over the cross product of what a body does and does not
//! declare: which of the two formats a definition is written in is settled out of the two
//! root members an OpenAPI/Swagger document states its version in (`openapi` from 3.0 on,
//! `swagger` before it), and that detection answers in front of the reading — so a body
//! that declares neither member is refused as the version that could not be determined,
//! whether it parsed or not, and a body that DOES declare one is refused by whichever
//! later step failed: as no readable document, or as no valid definition of the format it
//! names. Both operations that take a definition payload are asserted on every row, since
//! they read a body through one reader.
//!
//! Every assertion drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — rather than calling a handler directly.

use crate::rest_api_create_from_import::api_count;
use crate::rest_api_lifecycle::{create_api, Answer, Gateway};

/// The operation's own client-input error, which every refusal below carries.
const BAD_REQUEST: &str = "BadRequestException";

/// The sentence for a body that declares no version of the format it is written in.
///
/// Source: `crate::ops::put_rest_api::UNDETERMINED_VERSION`.
const UNDETERMINED_VERSION: &str = "Unable to determine OAS/Swagger version";

/// The sentence for a body whose format IS declared and that is no document.
///
/// Source: `crate::ops::put_rest_api::UNPARSEABLE_DEFINITION`.
const UNPARSEABLE_DEFINITION: &str = "Invalid OAS input";

/// The sentence for a document that read whole under the format it declares and is no
/// valid definition of it.
///
/// Source: `crate::ops::put_rest_api::INVALID_DEFINITION`.
const INVALID_DEFINITION: &str = "Invalid OpenAPI input.";

/// Assert `answer` is the HTTP 400 `BadRequestException` carrying `expected`.
fn assert_refusal(answer: &Answer, expected: &str, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error.as_deref(),
        Some(BAD_REQUEST),
        "case {case}: expected the error type {BAD_REQUEST:?}, got {:?}",
        answer.error
    );
    let stated = answer.body["message"].as_str().unwrap_or_else(|| {
        panic!(
            "case {case}: expected a refusal stating what to fix, got {:?}",
            answer.body
        )
    });
    assert_eq!(
        stated, expected,
        "case {case}: expected {expected:?}, got {stated:?}"
    );
}

/// RULE: the version detection in front of the reading decides the refusal — a body
/// declaring neither root version member is refused as the version that could not be
/// determined however far the reading got, and a body declaring one is refused by the step
/// that then failed.
///
/// The rows are the cross product of what decides the sentence: whether the body declares a
/// version member (and in which of the two members, in both spellings), against what is
/// wrong with it (it does not parse, it parses and is no valid definition of the format it
/// names, it parses and declares nothing). The served rows are the control: a document
/// declaring either member and reading whole is imported.
#[tokio::test]
async fn the_version_member_decides_which_refusal_a_definition_earns() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "version-detection-order").await;
    let before = api_count(&gateway, "before the refusals").await;

    // (case, body, the sentence it earns)
    let rows: Vec<(&str, &str, &str)> = vec![
        // Declares `openapi`, and stops being readable: the format is settled, so what is
        // left to state is that these bytes are no document.
        (
            "an openapi document cut off mid-member",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"x\"",
            UNPARSEABLE_DEFINITION,
        ),
        // The same prefix with the version member taken out: nothing says what this is.
        (
            "the same prefix declaring no version",
            "{\"info\": {\"title\": \"x\"",
            UNDETERMINED_VERSION,
        ),
        // Swagger's own member, same two cases, in the YAML spelling.
        (
            "a swagger document written as YAML whose flow mapping never closes",
            "swagger: '2.0'\npaths: {\"/pets\": {\n",
            UNPARSEABLE_DEFINITION,
        ),
        (
            "the same YAML prefix declaring no version",
            "paths: {\"/pets\": {\n",
            UNDETERMINED_VERSION,
        ),
        // Declares `openapi` and reads WHOLE, and is no valid definition of that format.
        (
            "an openapi document whose paths are a list",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"x\"}, \"paths\": [\"/pets\"]}",
            INVALID_DEFINITION,
        ),
        (
            "a swagger document whose paths are a scalar",
            "{\"swagger\": \"2.0\", \"info\": {\"title\": \"x\"}, \"paths\": \"/pets\"}",
            INVALID_DEFINITION,
        ),
        // Reads whole and declares neither member, in either spelling and whatever shape
        // it parsed to.
        (
            "a document carrying only an info",
            "{\"info\": {\"title\": \"x\"}, \"paths\": {}}",
            UNDETERMINED_VERSION,
        ),
        (
            "a YAML mapping carrying only an info",
            "info:\n  title: x\npaths: {}\n",
            UNDETERMINED_VERSION,
        ),
        (
            "a body that is one plain scalar",
            "not an api definition",
            UNDETERMINED_VERSION,
        ),
        // A name that is only PART of a version member's name is no member of it.
        (
            "a document declaring Swagger 1.2's own version field",
            "{\"swaggerVersion\": \"1.2\", \"info\": {\"title\": \"x\"}}",
            UNDETERMINED_VERSION,
        ),
    ];

    for (case, body, expected) in &rows {
        let imported = gateway.call_raw("POST", "/restapis?mode=import", body).await;
        assert_refusal(&imported, expected, &format!("ImportRestApi with {case}"));
        let put = gateway
            .call_raw("PUT", &format!("/restapis/{api}?mode=overwrite"), body)
            .await;
        assert_refusal(&put, expected, &format!("PutRestApi with {case}"));
    }

    // None of those refusals created an API: the reader answers before anything is stored.
    let after = api_count(&gateway, "after the refusals").await;
    assert_eq!(
        after, before,
        "case after the refusals: a refused import must create no API, expected {before}, \
         got {after}"
    );

    // The control: a document declaring either member and reading whole is served, so the
    // sentences above are about the bodies they name and nothing else.
    let served: Vec<(&str, String)> = vec![
        (
            "an openapi 3 document",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"served-oas3\"}, \"paths\": {}}"
                .to_string(),
        ),
        (
            "a swagger 2 document written as YAML",
            "swagger: '2.0'\ninfo:\n  title: served-swagger\npaths: {}\n".to_string(),
        ),
    ];
    for (case, body) in &served {
        let imported = gateway
            .call_raw("POST", "/restapis?mode=import", body)
            .await;
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
