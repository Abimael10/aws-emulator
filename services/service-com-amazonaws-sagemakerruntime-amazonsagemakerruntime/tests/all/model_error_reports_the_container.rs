// SPDX-License-Identifier: CC0-1.0

//! What the modelled `ModelError` (HTTP 424) REPORTS about an invocation the model did not
//! answer, over the wire.
//!
//! RULE — `ModelError`'s `OriginalStatusCode` ("original status code") and `OriginalMessage`
//! ("original message") carry the MODEL CONTAINER's own answer, so they are reported exactly
//! when a container answered at all. Where nothing answered — the variant's model names no
//! artifact, so there is no container to hand the payload to — the failure names the condition
//! that stopped the invocation and claims no container status, no container sentence and no
//! container log stream, whatever media type the payload was offered under: the media-type
//! negotiation belongs to a container, and there is none.
//!
//! The other half of the rule — the refusal a container itself makes for a media type it does
//! not accept, which travels unwrapped in those two members — needs a model whose artifact this
//! emulator's own Amazon S3 really holds, so it is asked in
//! `core/router/tests/all/sagemaker_endpoint_serves_a_trained_model.rs`, the suite that can
//! mount both planes and Amazon S3 beside them.
//!
//! The rows span what the rule generalises over: the media type the payload is offered under
//! (one a container reads records in, one no container accepts, none at all) and the payload's
//! own shape.

use serde_json::{json, Value};

use crate::endpoint_invocation::{content_type, Emulator};

/// One row of the table: the case, the request headers the invocation carries and the payload.
type Row = (&'static str, Vec<(&'static str, &'static str)>, &'static [u8]);

/// RULE: a failure no container answered claims neither the container's status nor its
/// sentence, whatever content type the request offered.
#[tokio::test]
async fn a_failure_no_container_answered_claims_no_container_status_or_message() {
    let emulator = Emulator::new();
    // The deployed model names no `ModelDataUrl`, so nothing is loaded to serve the payload
    // and no container ever sees the request.
    emulator.deploy("unserved", &["AllTraffic"], json!({})).await;

    let rows: [Row; 4] = [
        (
            "a media type a container reads records in",
            vec![content_type("text/csv")],
            b"1,2,3",
        ),
        (
            "a media type no container accepts",
            vec![content_type("application/x-unsupported-by-any-container")],
            b"1,2,3",
        ),
        (
            "a JSON payload under its own media type",
            vec![content_type("application/json")],
            br#"{"instances":[[1,2,3]]}"#,
        ),
        ("no media type at all", vec![], b"1,2,3"),
    ];

    for (case, headers, payload) in rows {
        let call = emulator.invoke("unserved", &headers, payload).await;
        assert_eq!(
            (call.status, call.error_code().as_str()),
            (424, "ModelError"),
            "case {case:?}: expected the modelled ModelError, got {} with {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        let body = call.json();
        for absent in ["OriginalStatusCode", "OriginalMessage", "LogStreamArn"] {
            assert!(
                body.get(absent).is_none(),
                "case {case:?}: {absent} must be absent when no container answered, got {body}"
            );
        }
        assert_eq!(
            call.message(),
            "Model unserved-model carries no ModelDataUrl, so there are no model artifacts \
             to compute an inference from.",
            "case {case:?}: the failure must name the condition that stopped the invocation, \
             got {body}"
        );
    }

    // The endpoint is untouched by any of it: no invocation moved its status and none left a
    // failure reason on it.
    let described = emulator
        .control("DescribeEndpoint", &json!({ "EndpointName": "unserved" }))
        .await;
    assert_eq!(
        described.get("EndpointStatus").and_then(Value::as_str),
        Some("InService"),
        "case \"after the failures\": the endpoint is untouched, got {described}"
    );
    assert!(
        described.get("FailureReason").is_none(),
        "case \"after the failures\": a model's failure is not the endpoint's, got {described}"
    );
}
