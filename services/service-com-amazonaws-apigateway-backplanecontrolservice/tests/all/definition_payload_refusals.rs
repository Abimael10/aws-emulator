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
//! * a definition document that declares neither version member — or that stopped being
//!   readable before it declared one — is refused as a version that could not be
//!   determined, which is a different refusal from a document that DID declare its version
//!   and is not a readable definition of that format;
//! * a readable definition whose operations carry more than this emulator's resource guards
//!   store is refused by the same guards that bound the operations which write those
//!   records one at a time, and stores nothing.

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

/// The sentence both definition imports state for a body that is not a readable API
/// definition: one no reader could parse, and one that parsed and declares its version but
/// cannot be read as that format.
///
/// Source: `crate::ops::put_rest_api::INVALID_DEFINITION`.
const INVALID_DEFINITION: &str = "Invalid OpenAPI input.";

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
/// its version in — so a document that PARSED and declares neither is refused as a version
/// that could not be determined, whatever else it carries and whichever spelling it is
/// written in.
///
/// The two members are the OpenAPI Specification's own root fields: `swagger` (2.0 and
/// before) and `openapi` (3.0 on). The refused rows differ along WHY no version was found
/// — what the parsed document carries instead (nothing but an `info`, paths as well, a
/// member whose name merely resembles the version field in Swagger 1.2's `swaggerVersion`,
/// an `info.version`, which is the API's own version and not the format's) and the shape it
/// parsed to (a mapping, a sequence, a scalar) — along whether the document read whole at
/// all, since a body that stopped before any version member declares none either — and
/// along the spelling, JSON and the YAML
/// the same document is written in. A document that DECLARED a version and is then not a
/// readable definition of that format earns the OTHER sentence, which the siblings
/// [`a_document_that_did_not_read_keeps_the_invalid_input_sentence`] and
/// [`a_body_the_reader_could_not_finish_is_refused_by_the_version_it_declared`] pin. The
/// served rows are the same documents carrying each of the two members, which must NOT earn
/// this sentence.
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
        // A body the reader could make no document of carries no version member either, so
        // it declares none: the version is what could not be determined about it.
        (
            "text that is no document at all",
            "}not a document{".to_string(),
        ),
        (
            "a version-less document the reader could not finish",
            "{\"info\": {\"title\": \"no version\"}, \"paths\": {".to_string(),
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

/// RULE: a document that declared the format it is written in and is not a readable
/// definition of that format — whether the reading stopped partway or the document read
/// whole — is the "this is not an API definition" refusal, the one beside the version
/// sentence, so a caller can tell "nothing here says what this is" from "this is not a
/// valid one of those".
///
/// Rows span both halves of that class and differ along WHERE the reading stopped: a
/// truncated document, an unterminated flow mapping, an unterminated quoted scalar, and a
/// versioned document whose `paths` is not a mapping — in both version members and both
/// spellings. Each of them declares its version first, which is why they are here and not
/// in the sibling
/// [`a_definition_whose_version_cannot_be_determined_is_refused_as_that`]. A refused
/// request creates no API, which the count below checks.
#[tokio::test]
async fn a_document_that_did_not_read_keeps_the_invalid_input_sentence() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "unreadable-definitions").await;
    let before = api_count(&gateway, "before the unreadable bodies").await;

    // Rows span the half of the class whose reading stopped after the version member and
    // the half that read whole and is no valid definition: both earn the one sentence.
    let refused: Vec<(&str, String)> = vec![
        (
            "a document truncated mid-object",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"truncated\"".to_string(),
        ),
        (
            "an unterminated flow mapping",
            "{\"openapi\": \"3.0.1\", \"paths\": {".to_string(),
        ),
        (
            "an unterminated quoted scalar",
            "openapi: \"3.0.1\ninfo:\n  title: x\n".to_string(),
        ),
        (
            "paths that are a list, under openapi",
            "{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"x\"}, \"paths\": [\"/pets\"]}"
                .to_string(),
        ),
        (
            "paths that are a scalar, under swagger",
            "{\"swagger\": \"2.0\", \"info\": {\"title\": \"x\"}, \"paths\": \"/pets\"}".to_string(),
        ),
        (
            "paths that are a list, written as YAML",
            "openapi: 3.0.1\ninfo:\n  title: x\npaths:\n  - /pets\n".to_string(),
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

    let after = api_count(&gateway, "after the unreadable bodies").await;
    assert_eq!(
        after, before,
        "case after the unreadable bodies: a refused import must create no API, expected \
         {before}, got {after}"
    );
}

/// RULE: a body the reader could not finish reading is refused by WHETHER it declared the
/// format it is written in before the reading stopped — a body carrying one of the two root
/// version members is refused as a document that is not a valid definition of the format it
/// says it is, and a body that stopped before any version member (text that is no document,
/// bytes that are not text) declares no version at all and is refused as that, the same
/// sentence a document that parsed whole and declares neither earns.
///
/// Rows differ along WHERE the body stops being parseable (a flow mapping, a flow sequence
/// or a quoted scalar that never closes, a document cut off mid-member, bytes that are no
/// document at all), along the spelling the body starts out as (JSON and YAML), along
/// which version member the truncated prefix happens to carry, along whether the body
/// is text in the first place — and so along which of the two sentences each earns: the
/// PAIRED rows are the same prefix with and without its version member, which is what
/// isolates the one thing that decides the sentence. The control rows are the two
/// neighbouring classes: a document that PARSED and declares no version, and a well-formed
/// document, which is served.
#[tokio::test]
async fn a_body_the_reader_could_not_finish_is_refused_by_the_version_it_declared() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "unparseable-definitions").await;
    let before = api_count(&gateway, "before the refusals").await;

    // (case, body bytes, the sentence the body earns)
    let refused: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "a JSON object cut off mid-member",
            b"{\"openapi\": \"3.0.1\", \"info\": {\"title\": \"cut off\" NOT A DOCUMENT !!!".to_vec(),
            INVALID_DEFINITION,
        ),
        (
            "the same object cut off with no version member",
            b"{\"info\": {\"title\": \"cut off\" NOT A DOCUMENT !!!".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "an unterminated flow mapping",
            b"{\"openapi\": \"3.0.1\", \"paths\": {".to_vec(),
            INVALID_DEFINITION,
        ),
        (
            "the same flow mapping with no version member",
            b"{\"paths\": {".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "an unterminated flow sequence",
            b"{\"swagger\": \"2.0\", \"schemes\": [\"https\"".to_vec(),
            INVALID_DEFINITION,
        ),
        (
            "the same flow sequence with no version member",
            b"{\"schemes\": [\"https\"".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "an unterminated quoted scalar, written as YAML",
            b"openapi: \"3.0.1\ninfo:\n  title: x\n".to_vec(),
            INVALID_DEFINITION,
        ),
        (
            "an unterminated quoted scalar whose key is not a version member",
            b"title: \"x\ninfo:\n  title: x\n".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "a YAML document whose flow mapping never closes",
            b"swagger: '2.0'\npaths: {\"/pets\": {\n".to_vec(),
            INVALID_DEFINITION,
        ),
        (
            "the same YAML document with no version member",
            b"paths: {\"/pets\": {\n".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "text that is no document at all",
            b"}not a document{".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "a version-looking name inside a string value, with no member of that name",
            b"{\"info\": {\"description\": \"see \\\"openapi\\\": 3\"}, \"paths\": {".to_vec(),
            UNDETERMINED_VERSION,
        ),
        (
            "bytes that are not text",
            vec![0xff, 0xfe, 0x00, 0x9c, 0x01],
            UNDETERMINED_VERSION,
        ),
    ];

    for (case, body, expected) in &refused {
        let imported = gateway
            .call_bytes(
                "POST",
                "/restapis?mode=import",
                bytes::Bytes::from(body.clone()),
            )
            .await;
        assert_refusal(
            &imported,
            BAD_REQUEST,
            expected,
            &format!("ImportRestApi with {case}"),
        );
        let put = gateway
            .call_bytes(
                "PUT",
                &format!("/restapis/{api}?mode=overwrite"),
                bytes::Bytes::from(body.clone()),
            )
            .await;
        assert_refusal(
            &put,
            BAD_REQUEST,
            expected,
            &format!("PutRestApi with {case}"),
        );
    }

    let after = api_count(&gateway, "after the refusals").await;
    assert_eq!(
        after, before,
        "case after the refusals: a refused import must create no API, expected {before}, \
         got {after}"
    );

    // The neighbouring class: a body that DID parse and declares neither version member
    // keeps the version-detection sentence, so the two refusals stay apart.
    let parsed_without_version = import_rest_api(&gateway, "{\"info\": {\"title\": \"x\"}}").await;
    assert_refusal(
        &parsed_without_version,
        BAD_REQUEST,
        UNDETERMINED_VERSION,
        "ImportRestApi with a parsed document declaring no version",
    );

    // And a body that parses into a versioned document is still served: the sentence above
    // is about the body that could not be read and nothing else.
    let served = import_rest_api(&gateway, &definition_with_version("parsed import")).await;
    assert_eq!(
        served.status, 201,
        "case ImportRestApi with a well-formed document: expected 201, got {} with {:?}",
        served.status, served.body
    );
}

/// RULE: every caller-sized member an imported operation writes into the store is bounded
/// by the same resource guard as the same member of the operation that writes it one at a
/// time — so a definition carrying more than a guard allows is refused with the operation's
/// own `BadRequestException` and stores nothing, however it was imported.
///
/// The guards are `crate::state::MAX_COLLECTION_ENTRIES` (entries of a caller-sized map or
/// list) and `crate::ops::put_integration::MAX_TEMPLATE_CHARS` (one mapping template), which
/// is what `PutMethod`, `PutMethodResponse` and `PutIntegration` bound the same members by,
/// and which this test reads from the crate rather than re-typing.
/// The rows differ along WHICH member of the operation is over its bound — the method's
/// request parameters, a response's headers, an integration's request templates, one
/// template's length — and the served rows are the same documents exactly AT each bound,
/// which must still import.
#[tokio::test]
async fn an_imported_operation_is_bounded_like_the_operations_that_write_it_one_at_a_time() {
    // The guards themselves, imported rather than re-typed, so a row at the bound is at the
    // bound the code enforces.
    use service_com_amazonaws_apigateway_backplanecontrolservice::ops::put_integration::MAX_TEMPLATE_CHARS;
    use service_com_amazonaws_apigateway_backplanecontrolservice::state::MAX_COLLECTION_ENTRIES as MAX_ENTRIES;

    /// An OpenAPI 3 document declaring one GET operation with `parameters` header
    /// parameters, a 200 response carrying `headers`, and a MOCK integration whose
    /// `requestTemplates` map holds `templates` entries of `template_chars` characters each.
    fn definition(
        title: &str,
        parameters: usize,
        headers: usize,
        templates: usize,
        template_chars: usize,
    ) -> String {
        let parameters: Vec<String> = (0..parameters)
            .map(|index| {
                format!("{{\"name\": \"h{index}\", \"in\": \"header\", \"required\": false}}")
            })
            .collect();
        let headers: Vec<String> = (0..headers)
            .map(|index| format!("\"x-h{index}\": {{\"schema\": {{\"type\": \"string\"}}}}"))
            .collect();
        let templates: Vec<String> = (0..templates)
            .map(|index| {
                format!(
                    "\"application/t{index}\": \"{}\"",
                    "t".repeat(template_chars)
                )
            })
            .collect();
        format!(
            "{{\"openapi\": \"3.0.1\", \"info\": {{\"title\": \"{title}\", \"version\": \"1\"}}, \
             \"paths\": {{\"/pets\": {{\"get\": {{\"parameters\": [{}], \"responses\": \
             {{\"200\": {{\"description\": \"ok\", \"headers\": {{{}}}}}}}, \
             \"x-amazon-apigateway-integration\": {{\"type\": \"mock\", \
             \"requestTemplates\": {{{}}}}}}}}}}}}}",
            parameters.join(", "),
            headers.join(", "),
            templates.join(", ")
        )
    }

    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "definition-guards").await;
    let before = api_count(&gateway, "before the refusals").await;

    // (case, document) — each one member past its bound, the rest at or under it.
    let refused: Vec<(String, String)> = vec![
        (
            format!("{} request parameters", MAX_ENTRIES + 1),
            definition("too many parameters", MAX_ENTRIES + 1, 0, 0, 0),
        ),
        (
            format!("{} response headers", MAX_ENTRIES + 1),
            definition("too many headers", 0, MAX_ENTRIES + 1, 0, 0),
        ),
        (
            format!("{} request templates", MAX_ENTRIES + 1),
            definition("too many templates", 0, 0, MAX_ENTRIES + 1, 1),
        ),
        (
            format!("a request template of {} characters", MAX_TEMPLATE_CHARS + 1),
            definition("an oversized template", 0, 0, 1, MAX_TEMPLATE_CHARS + 1),
        ),
    ];

    for (case, body) in &refused {
        let imported = import_rest_api(&gateway, body).await;
        assert_eq!(
            imported.status, 400,
            "case ImportRestApi with {case}: expected 400, got {} with {:?}",
            imported.status, imported.body
        );
        assert_eq!(
            imported.error.as_deref(),
            Some(BAD_REQUEST),
            "case ImportRestApi with {case}: expected {BAD_REQUEST:?}, got {:?}",
            imported.error
        );
        let stated = message(&imported, case);
        assert!(
            stated.contains("at most"),
            "case ImportRestApi with {case}: expected a refusal naming the bound it broke, \
             got {stated:?}"
        );
        let put = gateway
            .call_raw("PUT", &format!("/restapis/{api}?mode=overwrite"), body)
            .await;
        assert_eq!(
            put.status, 400,
            "case PutRestApi with {case}: expected 400, got {} with {:?}",
            put.status, put.body
        );
    }

    let after = api_count(&gateway, "after the refusals").await;
    assert_eq!(
        after, before,
        "case after the refusals: a refused import must create no API, expected {before}, \
         got {after}"
    );

    // The same documents exactly AT each bound are imported: the guard refuses what is past
    // it, not what reaches it.
    let served: Vec<(String, String)> = vec![
        (
            format!("{MAX_ENTRIES} request parameters"),
            definition("parameters at the bound", MAX_ENTRIES, 0, 0, 0),
        ),
        (
            format!("{MAX_ENTRIES} response headers"),
            definition("headers at the bound", 0, MAX_ENTRIES, 0, 0),
        ),
        (
            format!("{MAX_ENTRIES} request templates"),
            definition("templates at the bound", 0, 0, MAX_ENTRIES, 1),
        ),
        (
            format!("a request template of {MAX_TEMPLATE_CHARS} characters"),
            definition("a template at the bound", 0, 0, 1, MAX_TEMPLATE_CHARS),
        ),
    ];
    for (case, body) in &served {
        let imported = import_rest_api(&gateway, body).await;
        assert_eq!(
            imported.status, 201,
            "case ImportRestApi with {case}: expected 201, got {} with {:?}",
            imported.status, imported.body
        );
    }
}

/// `ImportRestApi` with `body` sent verbatim.
async fn import_rest_api(gateway: &Gateway, body: &str) -> Answer {
    gateway
        .call_raw("POST", "/restapis?mode=import", body)
        .await
}
