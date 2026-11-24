// SPDX-License-Identifier: CC0-1.0

//! The request every ssm response names itself as the answer to.
//!
//! RULE: every response of this service carries the header this protocol reports a request
//! id in (`x-amzn-RequestId`, matched case-insensitively), freshly minted per call, on a
//! success as much as on a failure — no output or error shape of the model declares the id,
//! so it belongs to the response and no handler can set it. Where the service words a
//! refusal whose MESSAGE embeds the request id, the id in that message is the id the header
//! carries: one call names one request wherever a client reads it from.
//!
//! Every case goes through the front door a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched through the
//! multi-service router (`OPERATIONS.md`, "Wire protocol"). The rows differ along what the
//! rule generalises over — which operation answered, whether it answered a success, a
//! modelled error, a refusal this service words itself or a target naming no operation at
//! all — rather than the single call a recorded unit happened to make.

use std::collections::BTreeSet;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{router, REGION, TARGET_PREFIX};

/// The header AWS reports a request id in on this protocol, read the way an SDK reads it:
/// a header name is case-insensitive, so the spelling below is the one the rule names.
const REQUEST_ID_HEADER: &str = "x-amzn-RequestId";

/// The marker the SDK envelope of a self-worded refusal states the request id after
/// (`state::with_sdk_error_envelope`), and the one it closes with — what a client parses the
/// id back out of a message with.
const MESSAGE_REQUEST_ID_OPENING: &str = "; Request ID: ";

/// See [`MESSAGE_REQUEST_ID_OPENING`].
const MESSAGE_REQUEST_ID_CLOSING: &str = "; Proxy: null)";

/// An OpsItem id of the form `OpsItemId` admits that no create minted, so a call naming it
/// is refused `OpsItemNotFoundException` — the refusal whose message embeds the request id.
const UNKNOWN_OPS_ITEM: &str = "oi-0123456789ab";

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form this emulator mints a request id in
/// (`service_common::query_error_envelope::is_request_id`).
fn names_a_request(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    let widths = [8usize, 4, 4, 4, 12];
    groups.len() == widths.len()
        && groups.iter().zip(widths).all(|(group, width)| {
            group.len() == width
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

/// One answer, as a client sees it: the status, the request id the header named, and the
/// body.
struct Answered {
    status: u16,
    request_id: Option<String>,
    body: Value,
}

/// Dispatch `target` with `body` and report what came back.
///
/// `target` is the whole `X-Amz-Target` value, so a row can address an operation this model
/// does not declare — a response no operation produced names its request too.
async fn answered(router: &MultiServiceRouter, target: &str, body: &Value) -> Answered {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    Answered {
        status,
        request_id,
        body,
    }
}

/// The id the header named, asserting it is one and has the form a request id has.
fn named_request(case: &str, answered: &Answered) -> String {
    let Some(request_id) = answered.request_id.clone() else {
        panic!(
            "case {case:?}: every response names the request it answers in {REQUEST_ID_HEADER}, \
             got none ({} with {})",
            answered.status, answered.body
        );
    };
    assert!(
        names_a_request(&request_id),
        "case {case:?}: expected a request id, got {request_id:?}"
    );
    request_id
}

/// `CreateOpsItem` of a minimal OpsItem, so a success row has state behind it.
fn open_ops_item(title: &str) -> Value {
    json!({
        "Title": title,
        "Description": "a work item opened by the request-id suite",
        "Source": "emulator-tests",
    })
}

/// RULE: every answer of this service names the request it answered, whichever layer
/// produced it, and two answers never name the same request.
#[tokio::test]
async fn every_answer_names_the_request_it_answers() {
    let router = router();

    // (case, target, body) — one row per layer that can answer: a handler's success, a
    // handler's modelled error, a refusal the boundary walk raised, a refusal this service
    // words itself, and a target naming no modelled operation at all.
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "a success",
            format!("{TARGET_PREFIX}.CreateOpsItem"),
            open_ops_item("a first work item"),
        ),
        (
            "another success, of another operation",
            format!("{TARGET_PREFIX}.DescribeOpsItems"),
            json!({}),
        ),
        (
            "a modelled error: an OpsItem id nothing minted",
            format!("{TARGET_PREFIX}.GetOpsItem"),
            json!({ "OpsItemId": UNKNOWN_OPS_ITEM }),
        ),
        (
            "a boundary refusal: a required member absent",
            format!("{TARGET_PREFIX}.CreateActivation"),
            json!({}),
        ),
        (
            "a boundary refusal: a value outside its enumerated type",
            format!("{TARGET_PREFIX}.GetDefaultPatchBaseline"),
            json!({ "OperatingSystem": "NOT_A_REAL_OS" }),
        ),
        (
            "a refusal this service words itself",
            format!("{TARGET_PREFIX}.ModifyDocumentPermission"),
            json!({ "Name": "AWS-RunShellScript", "PermissionType": "Share" }),
        ),
        (
            "a target naming no modelled operation",
            format!("{TARGET_PREFIX}.NotAnOperationOfThisService"),
            json!({}),
        ),
    ];

    let mut named: Vec<(String, String)> = Vec::new();
    for (case, target, body) in rows {
        let answer = answered(&router, &target, &body).await;
        let request_id = named_request(case, &answer);
        named.push((case.to_string(), request_id));
    }

    let distinct: BTreeSet<&String> = named.iter().map(|(_, id)| id).collect();
    assert_eq!(
        distinct.len(),
        named.len(),
        "a request id names ONE call: expected every id to differ, got {named:?}"
    );
}

/// RULE: a refusal whose message embeds the request id embeds the id the header carries, so
/// the two readings of one call cannot disagree.
#[tokio::test]
async fn a_message_that_embeds_the_request_id_embeds_the_one_the_header_names() {
    let router = router();

    // Every refusal of this service that states the id inside its own message: the OpsItem
    // reads and writes that cannot resolve the id they were given.
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "a read of an OpsItem nothing minted",
            format!("{TARGET_PREFIX}.GetOpsItem"),
            json!({ "OpsItemId": UNKNOWN_OPS_ITEM }),
        ),
        (
            "an edit of an OpsItem nothing minted",
            format!("{TARGET_PREFIX}.UpdateOpsItem"),
            json!({ "OpsItemId": UNKNOWN_OPS_ITEM, "Status": "InProgress" }),
        ),
        (
            "a relation added to an OpsItem nothing minted",
            format!("{TARGET_PREFIX}.AssociateOpsItemRelatedItem"),
            json!({
                "OpsItemId": UNKNOWN_OPS_ITEM,
                "AssociationType": "RelatesTo",
                "ResourceType": "AWS::SSMIncidents::IncidentRecord",
                "ResourceUri": format!(
                    "arn:aws:ssm-incidents::{}:incident-record/response/00000000",
                    crate::parameter_store::ACCOUNT
                ),
            }),
        ),
    ];

    for (case, target, body) in rows {
        let answer = answered(&router, &target, &body).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected the refusal, got {} with {}",
            answer.status, answer.body
        );
        let request_id = named_request(case, &answer);
        let message = answer.body["Message"]
            .as_str()
            .or_else(|| answer.body["message"].as_str())
            .unwrap_or_default()
            .to_string();
        let embedded = message
            .rsplit_once(MESSAGE_REQUEST_ID_OPENING)
            .and_then(|(_, tail)| tail.strip_suffix(MESSAGE_REQUEST_ID_CLOSING))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the message to state a request id, got {message:?}")
            });
        assert_eq!(
            embedded, request_id,
            "case {case:?}: the id in the message must be the id the header names, got \
             {message:?} beside {request_id:?}"
        );
    }
}

/// RULE: the header is the RESPONSE's, so a body the codec cannot read at all is still
/// answered under a request id — nothing of the request decides it.
#[tokio::test]
async fn an_answer_no_operation_produced_names_its_request() {
    let router = router();
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.DescribeOpsItems"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from_static(b"{not json at all"))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let request_id = request_id.unwrap_or_else(|| {
        panic!("a body the codec cannot read is still answered under a request id ({status})")
    });
    assert!(
        names_a_request(&request_id),
        "expected a request id, got {request_id:?}"
    );
}
