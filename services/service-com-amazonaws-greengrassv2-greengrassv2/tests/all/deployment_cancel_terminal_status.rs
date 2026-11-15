// SPDX-License-Identifier: CC0-1.0

//! The terminal status a cancelled Greengrass V2 deployment carries on the DEVICE.
//!
//! `tests/all/deployment_cancellation.rs` owns the deployment record itself — the terminal
//! `CANCELED` status its own reader and the listing report, and the refusal of an id this
//! service holds no deployment for. This suite covers the other half of the same write:
//! "this operation cancels the deployment for devices that haven't yet received it", so the
//! AWS IoT job that applies the deployment is cancelled in the plane that owns jobs, and
//! `ListEffectiveDeployments` — which reports the status of that job on the device —
//! answers `CANCELED` for the deployment that was cancelled and leaves every other
//! deployment reaching that device where it was.
//!
//! Every assertion drives the real `restJson1` front door through [`MultiServiceRouter`]
//! and judges the HTTP status and the JSON body; nothing reaches into the state struct.
//!
//! The rows differ along what the rule generalises over: how many deployments reach the
//! device, which of them is cancelled, and how many times the cancel is repeated.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_greengrassv2_greengrassv2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these requests are signed for; every minted ARN carries it.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// `EffectiveDeploymentExecutionStatus` values this suite expects
/// (`smithy_query(query="shape",shape="EffectiveDeploymentExecutionStatus")`): the status of
/// a deployment job that has reached the device and is waiting on it, and the status of one
/// whose rollout was cancelled.
const EXECUTION_QUEUED: &str = "QUEUED";
const EXECUTION_CANCELED: &str = "CANCELED";

/// The emulator front door with only this service mounted, over ONE shared engine —
/// exactly how `core/router` assembles it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Dispatch one signed request and return its status and parsed JSON body.
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Vec<u8>) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://greengrass.{REGION}.amazonaws.com{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/greengrass/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .header("content-type", "application/json")
        .body(Bytes::from(body))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or(Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, value)
}

/// A string member of a JSON object, or the empty string when absent.
fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// An IoT thing ARN: the target form a deployment names to reach ONE core device.
fn thing_target(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/{name}")
}

/// `CreateDeployment` for `target`; answers the new deployment's id.
async fn create_deployment(router: &MultiServiceRouter, target: &str) -> String {
    let body =
        serde_json::to_vec(&json!({ "targetArn": target })).expect("a serializable request body");
    let (status, created) = call(router, "POST", "/greengrass/v2/deployments", body).await;
    assert_eq!(
        status, 201,
        "CreateDeployment for {target} must answer 201, got {status} {created}"
    );
    let id = text(&created, "deploymentId");
    assert!(
        !id.is_empty(),
        "CreateDeployment must answer a deployment id, got {created}"
    );
    id
}

/// `CancelDeployment` on `deployment_id`.
async fn cancel(router: &MultiServiceRouter, deployment_id: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/greengrass/v2/deployments/{deployment_id}/cancel"),
        Vec::new(),
    )
    .await
}

/// The execution status `ListEffectiveDeployments` reports for each deployment reaching
/// `thing_name`, by deployment id.
async fn effective_statuses(
    router: &MultiServiceRouter,
    case: &str,
    thing_name: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        "GET",
        &format!("/greengrass/v2/coreDevices/{thing_name}/effectiveDeployments"),
        Vec::new(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: ListEffectiveDeployments for {thing_name} must answer 200, got \
         {status} {body}"
    );
    body.get("effectiveDeployments")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        text(entry, "deploymentId"),
                        text(entry, "coreDeviceExecutionStatus"),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The status reported for `deployment_id` among `statuses`, or `None` when the listing did
/// not report that deployment at all.
fn status_of(statuses: &[(String, String)], deployment_id: &str) -> Option<String> {
    statuses
        .iter()
        .find(|(id, _)| id == deployment_id)
        .map(|(_, status)| status.clone())
}

#[tokio::test]
async fn a_cancel_is_reported_to_the_device_for_that_deployment_alone() {
    let router = emulator();

    // The rows differ in how many deployments reach the device and which of them is
    // cancelled: the only one there is, the first of three, and the last of three — plus
    // how many times the cancel is repeated, since a repeat asks for a status the job
    // already has.
    let cases: [(&str, &str, usize, usize, usize); 3] = [
        ("the only deployment on the device", "CoreSolo", 1, 0, 1),
        ("the first of three deployments", "CoreFirst", 3, 0, 2),
        ("the last of three deployments", "CoreLast", 3, 2, 1),
    ];

    for (case, thing, count, cancelled_index, repeats) in cases {
        let target = thing_target(thing);
        let mut ids = Vec::new();
        for _ in 0..count {
            ids.push(create_deployment(&router, &target).await);
        }

        // Before the cancel every deployment that reaches the device is waiting on it.
        let before = effective_statuses(&router, case, thing).await;
        assert_eq!(
            before.len(),
            count,
            "case {case:?}: expected the {count} deployment(s) that target {thing}, got \
             {before:?}"
        );
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(
                status_of(&before, id).as_deref(),
                Some(EXECUTION_QUEUED),
                "case {case:?}: deployment {index} must be {EXECUTION_QUEUED} before any \
                 cancel, got {before:?}"
            );
        }

        let cancelled_id = &ids[cancelled_index];
        for attempt in 1..=repeats {
            let (status, answer) = cancel(&router, cancelled_id).await;
            assert_eq!(
                status, 200,
                "case {case:?} cancel attempt {attempt}: expected 200, got {status} {answer}"
            );

            // The device-facing reader answers for the cancelled deployment's job, and for
            // nothing else: every other deployment reaching the device is still queued.
            let after = effective_statuses(&router, case, thing).await;
            assert_eq!(
                after.len(),
                count,
                "case {case:?} cancel attempt {attempt}: the cancel must not add or drop a \
                 deployment, got {after:?}"
            );
            for (index, id) in ids.iter().enumerate() {
                let expected = if id == cancelled_id {
                    EXECUTION_CANCELED
                } else {
                    EXECUTION_QUEUED
                };
                assert_eq!(
                    status_of(&after, id).as_deref(),
                    Some(expected),
                    "case {case:?} cancel attempt {attempt}: deployment {index} must be \
                     {expected}, got {after:?}"
                );
            }
        }
    }
}
