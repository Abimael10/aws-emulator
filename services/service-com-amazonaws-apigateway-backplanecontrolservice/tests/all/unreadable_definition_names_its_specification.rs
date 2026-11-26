// SPDX-License-Identifier: CC0-1.0

//! Which specification an import names when it refuses a body it could not read as a
//! document at all.
//!
//! One rule is pinned here: the root version member a body writes is what picks the reader
//! the rest of it is read by, and a body that reader cannot finish is refused in the words
//! that reader names its own specification by — the `openapi` member (3.0 on) selects the
//! OpenAPI Specification's reader, which states "Invalid OAS input", and the `swagger`
//! member (2.0 and before) selects the reader that states "Invalid OpenAPI input.". A body
//! writing neither member picks no reader and is refused as the version that could not be
//! determined, which the sibling suite `definition_version_detection_precedes_the_read`
//! pins; the status (400) and the error code (`BadRequestException`) are the same for all
//! of them, so the sentence is the only thing that tells a caller which of them it hit.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, and both operations that read a definition payload are asserted on every row,
//! since they read it through one reader.

use crate::rest_api_create_from_import::api_count;
use crate::rest_api_lifecycle::{create_api, Answer, Gateway};

/// The operation's own client-input error, which every refusal below carries.
const BAD_REQUEST: &str = "BadRequestException";

/// The sentence the OpenAPI Specification's reader states for a body it could not read as
/// a document of it: `crate::ops::put_rest_api::UNPARSEABLE_OAS_DEFINITION`.
const UNPARSEABLE_OAS_DEFINITION: &str = "Invalid OAS input";

/// The same refusal as Swagger's reader states it:
/// `crate::ops::put_rest_api::UNPARSEABLE_SWAGGER_DEFINITION`.
const UNPARSEABLE_SWAGGER_DEFINITION: &str = "Invalid OpenAPI input.";

/// The sentence for a body that declares no version at all, which no reader states because
/// none was selected: `crate::ops::put_rest_api::UNDETERMINED_VERSION`.
const UNDETERMINED_VERSION: &str = "Unable to determine OAS/Swagger version";

/// Assert `answer` is the HTTP 400 `BadRequestException` carrying exactly `expected`.
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

/// RULE: a body an import could not read as a document at all is refused in the words of
/// the reader the version member it writes selects — the `openapi` member's reader states
/// the OAS sentence and the `swagger` member's reader the OpenAPI one — while a body
/// writing neither member selects no reader and is refused as the version that could not be
/// determined. Either way the answer is 400 `BadRequestException` and nothing is stored.
///
/// The rows differ along everything the rule generalises over and nothing else: which of
/// the two members the body writes (`openapi`, `swagger`), the version VALUE it carries
/// (3.0.1 and 3.1.0 under the one member, 2.0 under the other), the spelling the body is
/// written in (JSON and YAML), where the reading stops (cut off mid-member, a flow mapping,
/// a flow sequence or a quoted scalar that never closes, a token that is no document,
/// bytes that are not text at all), and whether the member stands at the document's head or
/// behind other members. The paired rows are the same broken body under each member, which
/// is what shows that the member — and not what is wrong with the body — picks the
/// sentence.
#[tokio::test]
async fn an_unreadable_definition_is_refused_in_the_words_of_the_declared_specification() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "unreadable-definition-sentences").await;
    let before = api_count(&gateway, "before the unreadable bodies").await;

    // (case, body bytes, the sentence the body earns).
    let rows: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "an openapi 3.0 object cut off after a token that is no document",
            b"{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"t\" NOT VALID JSON !!!".to_vec(),
            UNPARSEABLE_OAS_DEFINITION,
        ),
        (
            "the same body under the swagger member",
            b"{\"swagger\": \"2.0\", \"info\": {\"title\": \"t\" NOT VALID JSON !!!".to_vec(),
            UNPARSEABLE_SWAGGER_DEFINITION,
        ),
        (
            "the same body under neither member",
            b"{\"info\": {\"title\": \"t\" NOT VALID JSON !!!".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "an openapi 3.1 object truncated mid-member",
            b"{\"openapi\": \"3.1.0\", \"info\": {\"title\": \"t\" ".to_vec(),
            UNPARSEABLE_OAS_DEFINITION,
        ),
        (
            "a swagger 2.0 object truncated mid-member",
            b"{\"swagger\": \"2.0\", \"info\": {\"title\": \"t\" ".to_vec(),
            UNPARSEABLE_SWAGGER_DEFINITION,
        ),
        (
            "an openapi flow sequence that never closes, the member behind other members",
            b"{\"info\": {\"title\": \"t\"}, \"openapi\": \"3.0.3\", \"servers\": [{".to_vec(),
            UNPARSEABLE_OAS_DEFINITION,
        ),
        (
            "a swagger flow sequence that never closes, the member behind other members",
            b"{\"info\": {\"title\": \"t\"}, \"swagger\": \"2.0\", \"schemes\": [\"https\"".to_vec(),
            UNPARSEABLE_SWAGGER_DEFINITION,
        ),
        (
            "an openapi document written as YAML whose quoted scalar never closes",
            b"openapi: \"3.0.1\ninfo:\n  title: t\n".to_vec(),
            UNPARSEABLE_OAS_DEFINITION,
        ),
        (
            "a swagger document written as YAML whose quoted scalar never closes",
            b"swagger: \"2.0\ninfo:\n  title: t\n".to_vec(),
            UNPARSEABLE_SWAGGER_DEFINITION,
        ),
        (
            "an openapi document written as YAML whose flow mapping never closes",
            b"openapi: '3.0.1'\npaths: {\"/pets\": {\n".to_vec(),
            UNPARSEABLE_OAS_DEFINITION,
        ),
        (
            "a swagger document written as YAML whose flow mapping never closes",
            b"swagger: '2.0'\npaths: {\"/pets\": {\n".to_vec(),
            UNPARSEABLE_SWAGGER_DEFINITION,
        ),
        (
            "bytes that are not text behind an openapi member",
            {
                let mut body = b"{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"".to_vec();
                body.extend_from_slice(&[0xff, 0xfe, 0x00, 0x9c]);
                body
            },
            UNPARSEABLE_OAS_DEFINITION,
        ),
        (
            "bytes that are not text behind a swagger member",
            {
                let mut body = b"{\"swagger\": \"2.0\", \"info\": {\"title\": \"".to_vec();
                body.extend_from_slice(&[0xff, 0xfe, 0x00, 0x9c]);
                body
            },
            UNPARSEABLE_SWAGGER_DEFINITION,
        ),
        (
            "bytes that are not text behind no member at all",
            vec![0xff, 0xfe, 0x00, 0x9c, 0x01],
            UNDETERMINED_VERSION,
        ),
    ];

    for (case, body, expected) in &rows {
        let imported = gateway
            .call_bytes(
                "POST",
                "/restapis?mode=import",
                ::bytes::Bytes::from(body.clone()),
            )
            .await;
        assert_refusal(&imported, expected, &format!("ImportRestApi with {case}"));
        let put = gateway
            .call_bytes(
                "PUT",
                &format!("/restapis/{api}?mode=overwrite"),
                ::bytes::Bytes::from(body.clone()),
            )
            .await;
        assert_refusal(&put, expected, &format!("PutRestApi with {case}"));
    }

    // Nothing was stored for any of them: the reader answers before an API is created.
    let after = api_count(&gateway, "after the unreadable bodies").await;
    assert_eq!(
        after, before,
        "case after the unreadable bodies: a refused import must create no API, expected \
         {before}, got {after}"
    );
}

/// The control for the rule above: a document of EITHER specification that reads whole is
/// imported and read back, so the sentences above are about bodies no reader could finish
/// and not about the member they declare.
#[tokio::test]
async fn a_definition_that_reads_whole_is_imported_under_either_specification() {
    let gateway = Gateway::new();

    let served = [
        (
            "an openapi 3 document",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"served-oas3\", \"version\": \"1\"}, \
             \"paths\": {\"/pets\": {\"get\": {\"responses\": {\"200\": \
             {\"description\": \"ok\"}}}}}}",
            "served-oas3",
        ),
        (
            "a swagger 2 document written as YAML",
            "swagger: '2.0'\ninfo:\n  title: served-swagger\n  version: '1'\npaths: {}\n",
            "served-swagger",
        ),
    ];

    for (case, body, title) in served {
        let imported = gateway.call_raw("POST", "/restapis?mode=import", body).await;
        assert_eq!(
            imported.status, 201,
            "case ImportRestApi with {case}: expected 201, got {} with {:?}",
            imported.status, imported.body
        );
        let id = imported.body["id"].as_str().unwrap_or_else(|| {
            panic!(
                "case ImportRestApi with {case}: expected an api id, got {:?}",
                imported.body
            )
        });
        let read = gateway.call("GET", &format!("/restapis/{id}"), None).await;
        assert_eq!(
            read.status, 200,
            "case GetRestApi after {case}: expected 200, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["name"].as_str(),
            Some(title),
            "case GetRestApi after {case}: expected the imported title {title:?}, got {:?}",
            read.body["name"]
        );
    }
}
