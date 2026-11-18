// SPDX-License-Identifier: CC0-1.0

//! The IoT DEVICE DEFENDER MITIGATION ACTION surface on the ASSEMBLED emulator: the router
//! claims `POST /mitigationactions/actions/{actionName}`, its `GET` reader and
//! `GET /mitigationactions/actions` for the `iot` signing name, and the action one request
//! creates is the action the next requests read.
//!
//! The crate's own suite
//! (`services/service-com-amazonaws-iot-awsiotservice/tests/all/on_demand_audit_task/
//! mitigation_actions.rs`) owns the rules of the store itself — what a created action
//! reports member for member for every modelled `MitigationActionType`, what an update
//! replaces, what a delete leaves, and how a request it cannot take is refused. What this
//! suite adds is the one thing a single-service harness cannot see: that those routes reach
//! that store once EVERY service is mounted, so a client of the assembled emulator
//! addresses the same records.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests are driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for; the ARN assertion reads it back.
const REGION: &str = "eu-central-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The role the created action is applied with, of more than the modelled minimum length
/// (`RoleArn`, len 20..2048).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-mitigation";

/// A SigV4 credential scope naming `iot` in [`REGION`] — the signal the router picks the
/// owning service by, and the region the ARNs it mints are scoped to.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

#[test]
fn the_assembled_emulator_serves_the_iot_mitigation_action_routes() {
    std::thread::Builder::new()
        .name("iot-mitigation-action-routes".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            let control = |method: &str, path: String, body: String| {
                let request = http::Request::builder()
                    .method(method)
                    .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
                    .header("authorization", authorization())
                    .header("content-type", "application/json")
                    .body(bytes::Bytes::from(body))
                    .expect("a well-formed request");
                let response = runtime.block_on(router.dispatch(request));
                let status = response.status().as_u16();
                let rendered = String::from_utf8_lossy(response.body()).to_string();
                let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
                (status, rendered, parsed)
            };

            let action_name = "assembled-mitigation-action";
            let params = json!({
                "publishFindingToSnsParams": {
                    "topicArn": format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:defender-findings"),
                },
            });
            let (status, rendered, created) = control(
                "POST",
                format!("/mitigationactions/actions/{action_name}"),
                json!({ "roleArn": ROLE_ARN, "actionParams": params }).to_string(),
            );
            assert_eq!(
                status, 200,
                "creating a mitigation action on the assembled router must answer 200, got \
                 {status} with {rendered}"
            );
            let expected_arn =
                format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:mitigationaction/{action_name}");
            assert_eq!(
                created["actionArn"].as_str(),
                Some(expected_arn.as_str()),
                "expected the ARN this plane mints, got {rendered}"
            );
            let action_id = created["actionId"].as_str().unwrap_or_default().to_string();
            assert!(
                !action_id.is_empty(),
                "expected a minted actionId, got {rendered}"
            );

            let (status, rendered, described) = control(
                "GET",
                format!("/mitigationactions/actions/{action_name}"),
                String::new(),
            );
            assert_eq!(
                status, 200,
                "reading it back on the assembled router must answer 200, got {status} with \
                 {rendered}"
            );
            assert_eq!(
                (
                    described["actionName"].as_str(),
                    described["actionId"].as_str(),
                    described["actionType"].as_str(),
                    described["roleArn"].as_str(),
                ),
                (
                    Some(action_name),
                    Some(action_id.as_str()),
                    Some("PUBLISH_FINDING_TO_SNS"),
                    Some(ROLE_ARN),
                ),
                "expected the record the create persisted, got {rendered}"
            );
            assert_eq!(
                described["actionParams"], params,
                "expected the parameters the create carried, got {rendered}"
            );

            let (status, rendered, listed) = control(
                "GET",
                "/mitigationactions/actions".to_string(),
                String::new(),
            );
            assert_eq!(
                status, 200,
                "listing the account's mitigation actions on the assembled router must answer \
                 200, got {status} with {rendered}"
            );
            let names: Vec<&str> = listed["actionIdentifiers"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row["actionName"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                names,
                vec![action_name],
                "expected the listing to hold the created action, got {rendered}"
            );
        })
        .expect("spawning the router thread")
        .join()
        .expect("the router thread runs to completion");
}
