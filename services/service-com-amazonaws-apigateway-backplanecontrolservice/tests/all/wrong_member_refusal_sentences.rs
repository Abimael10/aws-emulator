// SPDX-License-Identifier: CC0-1.0

//! The refusals API Gateway words with a FIXED sentence about the MEMBER that was wrong,
//! asserted on the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi`/`CreateResource`/`PutMethod` helpers are this crate's existing harness,
//! reused from `rest_api_lifecycle`, `method_integration` and `api_key_import`. The
//! sibling `fixed_refusal_sentences` suite pins other sentences of this service; the ones
//! here are the five a wrong member earns.
//!
//! The RULES under test, each a property of the MEMBER that was wrong rather than of one
//! request:
//!
//! * a value read onto the HTTP VERB CATALOG that is not one of it — a method's own verb
//!   or the verb its integration is invoked through — is refused with the catalog's one
//!   sentence, which lists the verbs and carries nothing of the request;
//! * the resource a REST API is created with, its root, is not a resource a caller may
//!   remove, and the refusal names the resource class alone;
//! * a MAPPING EXPRESSION this service cannot read, where a request names the parameter a
//!   mapping writes, is refused with the structured validation-result sentence naming that
//!   expression, whichever member carried it;
//! * an integration of the HTTP family whose `uri` is no HTTP(S) endpoint is refused with
//!   one sentence about the member, with the URI not echoed;
//! * a body that is not content of the FORMAT the import was asked to read it as is
//!   refused with one sentence closing on that format.
//!
//! The rows of each table span what its rule generalises over — every verb of the catalog
//! in a spelling outside it and tokens that are no verb at all, both members a verb
//! travels in, four members that carry a mapping expression, the ways a URI can name no
//! HTTP endpoint over both types of the HTTP family, and four ways a payload carries no
//! readable content — rather than repeating one recorded call. Each table carries its
//! CONTROL: the value the rule admits, which is accepted.

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::ops::put_method::HTTP_METHODS;

use crate::api_key_import::import;
use crate::method_integration::{put_integration, resource_with_a_method};
use crate::rest_api_lifecycle::{
    assert_failure, create_api, create_resource, string_member, Answer, Gateway,
};

/// The sentence a verb outside the catalog is refused with: entirely fixed text, written
/// out here rather than imported, so the assertion fails if the implementation's wording
/// ever moves off it. Source: API Gateway's recorded refusal of a `httpMethod` outside
/// the catalog, whose verbs are comma-separated with no spaces and in this order.
const INVALID_HTTP_METHOD: &str =
    "Invalid HttpMethod specified. Valid options are GET,PUT,POST,DELETE,PATCH,OPTIONS,HEAD,ANY";

/// The sentence a request to delete a REST API's root resource is refused with. Source:
/// API Gateway's recorded refusal of that call.
const ROOT_RESOURCE_NOT_DELETABLE: &str = "Root resource cannot be deleted";

/// The sentence an HTTP integration whose `uri` is no HTTP(S) endpoint is refused with.
/// Source: API Gateway's recorded refusal of such a `uri`.
const INVALID_HTTP_ENDPOINT: &str = "Invalid HTTP endpoint specified for URI";

/// The sentence an import payload that is not content of the requested format is refused
/// with, closing on the format the request named. Source: API Gateway's recorded refusal
/// of an API key payload it could not read.
fn invalid_import_content(format: &str) -> String {
    format!("Invalid API Key import content specified for format: {format}")
}

/// The structured sentence a mapping expression this service could not read is refused
/// with: the validation-result frame carrying the one finding, inside which the offending
/// expression appears again. Source: API Gateway's recorded refusal of a
/// `requestParameters` key that is no mapping expression.
fn invalid_mapping_expression(expression: &str) -> String {
    format!(
        "Invalid mapping expression specified: Validation Result: warnings : [], errors : \
         [Invalid mapping expression specified: {expression}]"
    )
}

/// Assert `answer` is the modelled 400 `BadRequestException` stating exactly `message`.
fn assert_refused_with(answer: &Answer, case: &str, message: &str) {
    assert_failure(answer, 400, "BadRequestException", case);
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

#[tokio::test]
async fn a_verb_outside_the_catalog_is_refused_with_the_catalogs_own_sentence() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "verb-catalog-sentence").await;
    let resource = string_member(
        &create_resource(&gateway, &api, &root, "pets").await,
        "id",
        "verb-catalog-sentence/pets",
    );

    // The verb a METHOD is put under. The rows span every verb of the catalog in a
    // spelling that is NOT one of it (lower case), plus tokens that are no verb at all —
    // an unknown word, a digit-bearing token, a verb of the HTTP protocol the catalog does
    // not list, and a catalog verb in mixed case. The flag says whether the row's token is
    // one the SENTENCE itself does not carry as a word, so that row can also assert the
    // sentence echoes nothing of the request.
    let mut spellings: Vec<(String, bool)> = HTTP_METHODS
        .iter()
        .map(|verb| (verb.to_ascii_lowercase(), false))
        .collect();
    for outside in ["FETCH", "NOTAVERB123", "TRACE", "Get"] {
        spellings.push((outside.to_string(), true));
    }
    for (spelling, foreign) in &spellings {
        let answer = gateway
            .call(
                "PUT",
                &format!("/restapis/{api}/resources/{resource}/methods/{spelling}"),
                Some(json!({ "authorizationType": "NONE" })),
            )
            .await;
        let case = format!("PutMethod({spelling})");
        assert_refused_with(&answer, &case, INVALID_HTTP_METHOD);
        if *foreign {
            assert!(
                !answer.body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(spelling.as_str()),
                "case {case}: expected the sentence to echo no verb, got {:?}",
                answer.body["message"]
            );
        }
    }

    // The INTEGRATION's own verb is read onto the same catalog, so it earns the same
    // sentence — the member travels in the body, which is what makes this a second row of
    // one rule rather than a second rule. (The integration's verb is read onto the catalog
    // rather than addressing a record, so its letter case is normalised first: the rows
    // here are tokens that are no verb in any case.)
    let method = string_member(
        &create_resource(&gateway, &api, &root, "orders").await,
        "id",
        "verb-catalog-sentence/orders",
    );
    let put = gateway
        .call(
            "PUT",
            &format!("/restapis/{api}/resources/{method}/methods/GET"),
            Some(json!({ "authorizationType": "NONE" })),
        )
        .await;
    assert_eq!(
        put.status, 201,
        "case PutMethod(orders/GET): expected 201, got {} with {:?}",
        put.status, put.body
    );
    for spelling in ["fetch", "FETCH", "NOTAVERB123", "TRACE"] {
        let answer = put_integration(
            &gateway,
            &api,
            &method,
            "GET",
            json!({
                "type": "HTTP",
                "httpMethod": spelling,
                "uri": "https://example.com/orders",
            }),
        )
        .await;
        assert_refused_with(
            &answer,
            &format!("PutIntegration(integrationHttpMethod {spelling})"),
            INVALID_HTTP_METHOD,
        );
    }

    // The CONTROL: every verb of the catalog, in the catalog's own spelling, is accepted
    // on a method — so the refusals above are about the spelling and not about the call.
    let accepted = string_member(
        &create_resource(&gateway, &api, &root, "accepted").await,
        "id",
        "verb-catalog-sentence/accepted",
    );
    for verb in HTTP_METHODS {
        let answer = gateway
            .call(
                "PUT",
                &format!("/restapis/{api}/resources/{accepted}/methods/{verb}"),
                Some(json!({ "authorizationType": "NONE" })),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "case PutMethod({verb}): expected 201, got {} with {:?}",
            answer.status, answer.body
        );
    }
}

#[tokio::test]
async fn the_root_resource_of_an_api_is_refused_with_one_sentence() {
    let gateway = Gateway::new();

    // Rows differ in what the API holds when its root is addressed: a fresh API, an API
    // whose root carries a child, and an API whose root carries a method — the root is
    // the one resource that cannot go, whatever hangs off it.
    let cases: [&str; 3] = ["a fresh api", "a root with a child", "a root with a method"];
    for case in cases {
        let (api, root) = create_api(&gateway, case).await;
        if case == "a root with a child" {
            create_resource(&gateway, &api, &root, "pets").await;
        }
        if case == "a root with a method" {
            let put = gateway
                .call(
                    "PUT",
                    &format!("/restapis/{api}/resources/{root}/methods/GET"),
                    Some(json!({ "authorizationType": "NONE" })),
                )
                .await;
            assert_eq!(
                put.status, 201,
                "case {case}: expected 201 from PutMethod, got {} with {:?}",
                put.status, put.body
            );
        }
        let answer = gateway
            .call("DELETE", &format!("/restapis/{api}/resources/{root}"), None)
            .await;
        assert_refused_with(
            &answer,
            &format!("DeleteResource({case}, the root)"),
            ROOT_RESOURCE_NOT_DELETABLE,
        );
        // The refusal removed nothing: the root still answers a read.
        let read = gateway
            .call("GET", &format!("/restapis/{api}/resources/{root}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the root to still be there, got {} with {:?}",
            read.status, read.body
        );
    }

    // The CONTROL: a resource that is NOT the root is removed, so the refusal is about
    // the root and not about the call.
    let (api, root) = create_api(&gateway, "a child resource").await;
    let child = string_member(
        &create_resource(&gateway, &api, &root, "pets").await,
        "id",
        "a child resource/pets",
    );
    let deleted = gateway
        .call("DELETE", &format!("/restapis/{api}/resources/{child}"), None)
        .await;
    assert_eq!(
        deleted.status, 202,
        "case DeleteResource(a child): expected 202, got {} with {:?}",
        deleted.status, deleted.body
    );
}

#[tokio::test]
async fn a_mapping_expression_this_service_cannot_read_is_stated_as_a_validation_result() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "mapping-expression-sentence").await;
    let resource = resource_with_a_method(&gateway, &api, &root, "pets", "GET", json!({})).await;
    let integration = put_integration(
        &gateway,
        &api,
        &resource,
        "GET",
        json!({ "type": "MOCK" }),
    )
    .await;
    assert_eq!(
        integration.status, 201,
        "case PutIntegration(MOCK): expected 201, got {} with {:?}",
        integration.status, integration.body
    );
    let method_response = gateway
        .call(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/responses/200"),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        method_response.status, 201,
        "case PutMethodResponse(200): expected 201, got {} with {:?}",
        method_response.status, method_response.body
    );

    // (case, method, uri, body, the expression the sentence names) — four members that
    // carry a mapping expression as the parameter it names, on four operations, and
    // expressions that miss the form in four different ways: a dotted name that is no
    // mapping target at all, a known prefix with no name after it, the target of another
    // member's mapping, and a target of the wrong side of this mapping.
    let rows: Vec<(&str, &str, String, Value, &str)> = vec![
        (
            "PutMethod, a requestParameters key",
            "PUT",
            format!("/restapis/{api}/resources/{resource}/methods/POST"),
            json!({
                "authorizationType": "NONE",
                "requestParameters": { "invalid.param.name": true },
            }),
            "invalid.param.name",
        ),
        (
            "PutMethodResponse, a responseParameters key",
            "PUT",
            format!("/restapis/{api}/resources/{resource}/methods/GET/responses/201"),
            json!({ "responseParameters": { "method.response.header.": true } }),
            "method.response.header.",
        ),
        (
            "PutGatewayResponse, a responseParameters key",
            "PUT",
            format!("/restapis/{api}/gatewayresponses/DEFAULT_4XX"),
            json!({
                "statusCode": "404",
                "responseParameters": { "method.response.header.Allow": "'GET'" },
            }),
            "method.response.header.Allow",
        ),
        (
            "PutIntegrationResponse, a responseParameters key",
            "PUT",
            format!(
                "/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"
            ),
            json!({
                "responseParameters": {
                    "integration.response.header.Allow": "integration.response.header.Allow",
                },
            }),
            "integration.response.header.Allow",
        ),
    ];

    for (case, method, uri, body, expression) in rows {
        let answer = gateway.call(method, &uri, Some(body)).await;
        assert_refused_with(&answer, case, &invalid_mapping_expression(expression));
    }

    // The CONTROL: the documented form of each of those members is accepted.
    let accepted: Vec<(&str, &str, String, Value)> = vec![
        (
            "PutMethod, a method request parameter",
            "PUT",
            format!("/restapis/{api}/resources/{resource}/methods/PUT"),
            json!({
                "authorizationType": "NONE",
                "requestParameters": { "method.request.querystring.breed": true },
            }),
        ),
        (
            "PutMethodResponse, a method response header",
            "PUT",
            format!("/restapis/{api}/resources/{resource}/methods/GET/responses/202"),
            json!({ "responseParameters": { "method.response.header.Allow": true } }),
        ),
        (
            "PutGatewayResponse, a gateway response header",
            "PUT",
            format!("/restapis/{api}/gatewayresponses/DEFAULT_5XX"),
            json!({
                "statusCode": "500",
                "responseParameters": { "gatewayresponse.header.Allow": "'GET'" },
            }),
        ),
        (
            "PutIntegrationResponse, a method response header mapped from the integration",
            "PUT",
            format!(
                "/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"
            ),
            json!({
                "responseParameters": {
                    "method.response.header.Allow": "integration.response.header.Allow",
                },
            }),
        ),
    ];
    for (case, method, uri, body) in accepted {
        let answer = gateway.call(method, &uri, Some(body)).await;
        assert!(
            (200..300).contains(&answer.status),
            "case {case}: expected the call to be accepted, got {} with {:?}",
            answer.status,
            answer.body
        );
    }
}

#[tokio::test]
async fn an_http_integration_uri_that_is_no_endpoint_is_refused_with_one_sentence() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "http-endpoint-sentence").await;
    let resource = resource_with_a_method(&gateway, &api, &root, "pets", "GET", json!({})).await;

    // Rows differ in HOW the URI names no HTTP(S) endpoint — another scheme, a host with
    // no scheme, a path alone, a scheme-relative reference, a mail address — over BOTH
    // types of the HTTP family, since the rule is the family's.
    let uris: [&str; 5] = [
        "ftp://example.com/resource",
        "example.com/pets",
        "/pets",
        "//example.com/pets",
        "mailto:pets@example.com",
    ];
    for integration_type in ["HTTP", "HTTP_PROXY"] {
        for uri in uris {
            let answer = put_integration(
                &gateway,
                &api,
                &resource,
                "GET",
                json!({ "type": integration_type, "httpMethod": "GET", "uri": uri }),
            )
            .await;
            let case = format!("PutIntegration({integration_type}, uri {uri})");
            assert_refused_with(&answer, &case, INVALID_HTTP_ENDPOINT);
            // The URI is not echoed: the sentence is about the member.
            assert!(
                !answer.body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(uri),
                "case {case}: expected the sentence to echo no URI, got {:?}",
                answer.body["message"]
            );
        }
    }

    // The CONTROL: both schemes the family takes are accepted.
    for uri in ["http://example.com/pets", "https://example.com/pets"] {
        let answer = put_integration(
            &gateway,
            &api,
            &resource,
            "GET",
            json!({ "type": "HTTP", "httpMethod": "GET", "uri": uri }),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case PutIntegration(HTTP, uri {uri}): expected 201, got {} with {:?}",
            answer.status, answer.body
        );
    }
}

#[tokio::test]
async fn an_import_payload_that_is_not_the_requested_format_names_that_format() {
    let gateway = Gateway::new();
    // The format every row names, which is the one value `ImportApiKeys$format` admits
    // (`smithy_query(query="shape", shape="com.amazonaws.apigateway#ApiKeysFormat")`), and
    // the sentence closes on it.
    let expected = invalid_import_content("csv");

    // Rows differ in HOW the payload carries no readable content of the format: a document
    // of whitespace alone, one of line endings alone, one that is nothing but the byte
    // order mark a spreadsheet export leads with, and a body that is not text at all.
    let answer = gateway
        .call_bytes(
            "POST",
            "/apikeys?mode=import&format=csv&failonwarnings=false",
            Bytes::from_static(&[0xff, 0xfe, 0x00, 0x01]),
        )
        .await;
    assert_refused_with(&answer, "a payload that is not text", &expected);

    for (case, document) in [
        ("a document of whitespace alone", "   \n\t\n"),
        ("a document of line endings alone", "\n\n"),
        ("a document that is only the byte order mark", "\u{feff}"),
    ] {
        let answer = import(&gateway, document, false).await;
        assert_refused_with(&answer, case, &expected);
    }

    // The CONTROL: a readable document is imported, so the refusals above are about the
    // content and not about the call.
    let accepted = import(
        &gateway,
        "key,name\nimported-api-key-rule-01,readable\n",
        false,
    )
    .await;
    assert_eq!(
        accepted.status, 201,
        "case a readable document: expected 201, got {} with {:?}",
        accepted.status, accepted.body
    );
}
