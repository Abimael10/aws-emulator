// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the callers an `AWS_IAM` FUNCTION URL does NOT admit — the other half
//! of the rule `function_url_signed_caller` drives, whose table, signer and addressings this
//! suite reuses, as it reuses the wording of each refusal from
//! `function_url_unauthenticated_refusal`.
//!
//! RULE: the only requests an `AWS_IAM` function URL refuses are the ones whose SigV4
//! signature is MISSING, computed over other bytes than the ones that arrived, or scoped to
//! another service than the one the endpoint belongs to. Each is refused `403` with the CLASS
//! of the failure named in `x-amzn-errortype` and the sentence AWS words for it, before the
//! function runs — so a client can tell a credential it must fix from one the endpoint
//! verified, and a correctly signed request is never refused as a signature mismatch.
//!
//! Every row drives a real request through a router that VERIFIES signatures, against a
//! function whose deployment package RUNS, so a row that was refused is one nothing ran for.
//! The rows differ along what the rule generalises over: how the signature fails (absent, a
//! real signature over other bytes, a real signature scoped elsewhere), which of the
//! addressings reached the endpoint, and what the request carries (a path, a query, a body).

use std::sync::Arc;

use serde_json::Value;

use crate::function_url_data_plane::{router_on, send, Addressing, Answer};
use crate::function_url_signed_caller::{
    given_restricted_url, Credential, Row, BACKEND_HEADER,
};
use crate::function_url_unauthenticated_refusal::{
    borrowed, ACCESS_DENIED, ERROR_TYPE_HEADER, FORBIDDEN_BODY, INVALID_SIGNATURE,
    SCOPED_ELSEWHERE_BODY, SIGNATURE_MISMATCH_BODY,
};

/// RULE: a missing signature, a signature over other bytes and a credential scoped elsewhere
/// are each refused `403` — the class and the sentence of that failure, and nothing of the
/// function run — whichever addressing reached the endpoint.
#[tokio::test]
async fn only_a_missing_tampered_or_misscoped_signature_is_refused() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let endpoint = given_restricted_url(&router, "url-refused").await;
    let mut authenticating = router_on(&engine);
    authenticating.set_request_authentication(true);

    // (the row, the class it names, the document it answers)
    let rows: &[(Row, &str, &str)] = &[
        (
            Row {
                case: "no signature at all, on the published authority",
                addressing: Addressing::PublishedAuthority,
                method: "GET",
                within: "/items/one",
                query: "",
                body: "",
                media_type: "",
                credential: Credential::None,
            },
            ACCESS_DENIED,
            FORBIDDEN_BODY,
        ),
        (
            Row {
                case: "no signature at all, on the reserved local-endpoint path",
                addressing: Addressing::LocalEndpointPath,
                method: "POST",
                within: "/orders",
                query: "",
                body: r#"{"item":"one"}"#,
                media_type: "application/json",
                credential: Credential::None,
            },
            ACCESS_DENIED,
            FORBIDDEN_BODY,
        ),
        (
            Row {
                case: "a signature over other bytes than the ones sent",
                addressing: Addressing::PublishedAuthority,
                method: "POST",
                within: "/orders",
                query: "",
                body: r#"{"item":"one"}"#,
                media_type: "application/json",
                credential: Credential::OverOtherBytes,
            },
            INVALID_SIGNATURE,
            SIGNATURE_MISMATCH_BODY,
        ),
        (
            Row {
                case: "a signature over other bytes, on the reserved local-endpoint path",
                addressing: Addressing::LocalEndpointPath,
                method: "POST",
                within: "/orders/two",
                query: "via=local",
                body: r#"{"item":"one"}"#,
                media_type: "application/json",
                credential: Credential::OverOtherBytes,
            },
            INVALID_SIGNATURE,
            SIGNATURE_MISMATCH_BODY,
        ),
        (
            Row {
                case: "a credential scoped to another service",
                addressing: Addressing::PublishedAuthority,
                method: "GET",
                within: "/items/one",
                query: "",
                body: "",
                media_type: "",
                credential: Credential::ScopedElsewhere,
            },
            INVALID_SIGNATURE,
            SCOPED_ELSEWHERE_BODY,
        ),
        (
            Row {
                case: "a credential scoped to another service, path-style",
                addressing: Addressing::PathStyle,
                method: "GET",
                within: "/items/one",
                query: "",
                body: "",
                media_type: "",
                credential: Credential::ScopedElsewhere,
            },
            INVALID_SIGNATURE,
            SCOPED_ELSEWHERE_BODY,
        ),
    ];

    for (row, error_type, document) in rows {
        let case = row.case;
        let (host, target) = row.target(&endpoint);
        let credentialled = row.credentialled(&host, &target).await;
        let answer: Answer = send(
            &authenticating,
            row.method,
            &host,
            &target,
            row.body.as_bytes(),
            &borrowed(&credentialled),
        )
        .await;
        assert_eq!(
            answer.status,
            403,
            "case {case:?}: expected HTTP 403, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.header(ERROR_TYPE_HEADER).as_deref(),
            Some(*error_type),
            "case {case:?}: expected the class {error_type:?} in {ERROR_TYPE_HEADER}, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.text(),
            *document,
            "case {case:?}: expected the document {document}, got {}",
            answer.summary()
        );
        assert!(
            answer.header(BACKEND_HEADER).is_none() && !answer.text().contains("rawQueryString"),
            "case {case:?}: expected nothing of the function run, got {}",
            answer.summary()
        );
    }

    // The positive control: the SAME endpoint serves a request signed over the bytes that
    // arrive, so every row above is about the signature and not about the endpoint.
    let control = Row {
        case: "the positive control",
        addressing: Addressing::PublishedAuthority,
        method: "POST",
        within: "/orders",
        query: "",
        body: r#"{"item":"one"}"#,
        media_type: "application/json",
        credential: Credential::OverThisRequest,
    };
    let (host, target) = control.target(&endpoint);
    let credentialled = control.credentialled(&host, &target).await;
    let served = send(
        &authenticating,
        control.method,
        &host,
        &target,
        control.body.as_bytes(),
        &borrowed(&credentialled),
    )
    .await;
    assert_eq!(
        served.status,
        200,
        "case {:?}: a signature over the bytes sent must be served, got {}",
        control.case,
        served.summary()
    );
    assert_eq!(
        served.json().get("body").and_then(Value::as_str),
        Some(control.body),
        "case {:?}: expected the handler to be told the body it was sent, got {}",
        control.case,
        served.text()
    );
}
