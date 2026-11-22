// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE `StopDeployment` refuses an already-settled deployment with,
//! driven over the wire.
//!
//! RULE under test: `DeploymentAlreadyCompletedException` is worded by the
//! operation that raises it, and a stop of a deployment that has already reached
//! its outcome says `The deployment <deploymentId> is already in state <STATE>` —
//! the fixed text `The deployment `, ` is already in state ` and nothing after the
//! state token, with the id taken from the request and `<STATE>` the internal
//! state name of the status the service itself reports for that deployment (the
//! upper-snake form of its `DeploymentStatus`). The operations that act on a
//! deployment still making progress keep their own sentence, which
//! `deployment_progress_refusal_sentences` holds.
//!
//! The rows differ along what the rule generalises over: the outcome the
//! deployment settled in and so the state token the sentence reports, the compute
//! platform the deployment ran on, the deployment type (in-place, blue/green), the
//! form of the revision deployed, and whether the refused request asked for a
//! rollback at all — a refusal names the state whatever the caller asked of the
//! stop.
//!
//! The harness (`Emulator`, `error_code`, `s3_revision`, `terminal_deployment`) is
//! the lifecycle suite's, so every case goes through the same front door a client
//! uses.

use std::time::Duration;

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{
    error_code, s3_revision, terminal_deployment_within, Emulator,
};

/// The service role every deployment group here is created with.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The FIXED phrase of the stop's refusal, between the deployment id the request
/// named and the state the deployment is already in — the part a client splits the
/// sentence on to read that state back.
const ALREADY_IN_STATE_PHRASE: &str = " is already in state ";

/// The FIXED phrase the operations over a deployment's PROGRESS word the same
/// refusal with, which the stop's sentence is not
/// (`deployment_progress_refusal_sentences` holds that rule).
const ALREADY_COMPLETED_PHRASE: &str = " has already completed, current state ";

/// How long a case here is prepared to wait for the outcome the clock owes a
/// deployment.
///
/// Deliberately generous: the span this emulator gives such a transition is a few
/// seconds and no case here is ABOUT a duration, so nothing is measured against
/// this deadline — it only keeps a case that hangs from hanging forever, with room
/// for a host running every suite of this workspace at once.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The `deploymentStyle` of a blue/green deployment group.
fn blue_green_style() -> Value {
    json!({"deploymentType": "BLUE_GREEN", "deploymentOption": "WITH_TRAFFIC_CONTROL"})
}

/// Create an application and one deployment group on it, carrying `style` when it
/// is not null, and answer the group name.
async fn seeded_group(
    emulator: &Emulator,
    application: &str,
    platform: &str,
    style: &Value,
) -> String {
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": platform}),
        )
        .await;
    let group = format!("{application}-dg");
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "serviceRoleArn": SERVICE_ROLE,
    });
    if !style.is_null() {
        request["deploymentStyle"] = style.clone();
    }
    emulator.ok("CreateDeploymentGroup", request).await;
    group
}

/// Create a deployment of `revision` on `application`/`group`, answering its id.
async fn deploy(
    emulator: &Emulator,
    application: &str,
    group: &str,
    revision: &Value,
) -> String {
    emulator
        .ok(
            "CreateDeployment",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "revision": revision,
            }),
        )
        .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The stored `deploymentInfo` of `deployment_id`.
async fn deployment_info(emulator: &Emulator, deployment_id: &str) -> Value {
    emulator
        .ok("GetDeployment", json!({"deploymentId": deployment_id}))
        .await["deploymentInfo"]
        .clone()
}

/// The `$.message` of a failure body.
fn message(body: &Value, case: &str) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected a string $.message, got {body}"))
        .to_string()
}

/// Call `operation`, require HTTP 400 with `DeploymentAlreadyCompletedException`,
/// and answer the message.
async fn refused_as_completed(
    emulator: &Emulator,
    operation: &str,
    request: Value,
    case: &str,
) -> String {
    let (status, body) = emulator.call(operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from {operation}, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "DeploymentAlreadyCompletedException",
        "case {case:?}: expected DeploymentAlreadyCompletedException from {operation}, got \
         {body:?}"
    );
    message(&body, case)
}

/// One row of the table: a deployment driven to an outcome, and the stop that is
/// then refused on it.
struct Settled {
    case: &'static str,
    platform: &'static str,
    /// The group's `deploymentStyle`, absent when null.
    style: Value,
    /// The revision the deployment deploys.
    revision: Value,
    /// `autoRollbackEnabled` on the REFUSED stop, absent when `None`.
    auto_rollback: Option<bool>,
    /// The `DeploymentStatus` the clock settles the deployment in.
    status: &'static str,
    /// The internal state name the refusal reports for that status.
    state: &'static str,
}

/// RULE: a stop of a deployment that has already reached its outcome is refused
/// with `The deployment <deploymentId> is already in state <STATE>` — the id the
/// request carried, the state the deployment is already in, and nothing after it.
#[tokio::test]
async fn a_stop_of_a_settled_deployment_reports_the_state_it_is_already_in() {
    // Rows span both outcomes the clock settles a deployment in (a group that
    // resolves a target succeeds, one that resolves none fails), the compute
    // platform, the deployment type, the form the revision travels in and what the
    // refused stop asked about rolling back.
    let rows: &[Settled] = &[
        Settled {
            // An EC2/on-premises group resolves no target here, so the clock
            // settles the deployment as Failed.
            case: "clock-failed in-place server",
            platform: "Server",
            style: Value::Null,
            revision: s3_revision("artifacts", "app.zip"),
            auto_rollback: None,
            status: "Failed",
            state: "FAILED",
        },
        Settled {
            case: "clock-failed blue/green server, rollback asked for",
            platform: "Server",
            style: blue_green_style(),
            revision: json!({
                "revisionType": "GitHub",
                "gitHubLocation": {"repository": "octo/app", "commitId": "0f1e2d3"},
            }),
            auto_rollback: Some(true),
            status: "Failed",
            state: "FAILED",
        },
        Settled {
            // A Lambda group's target is the function its revision names, so the
            // clock settles the deployment as Succeeded.
            case: "clock-succeeded in-place lambda, rollback declined",
            platform: "Lambda",
            style: Value::Null,
            revision: s3_revision("artifacts", "function.zip"),
            auto_rollback: Some(false),
            status: "Succeeded",
            state: "SUCCEEDED",
        },
        Settled {
            case: "clock-succeeded blue/green lambda",
            platform: "Lambda",
            style: blue_green_style(),
            revision: s3_revision("artifacts", "function.zip"),
            auto_rollback: None,
            status: "Succeeded",
            state: "SUCCEEDED",
        },
    ];

    for row in rows {
        let emulator = Emulator::new();
        let application = row.case.replace([' ', '/', ','], "-");
        let group = seeded_group(&emulator, &application, row.platform, &row.style).await;
        let deployment_id = deploy(&emulator, &application, &group, &row.revision).await;

        let settled =
            terminal_deployment_within(&emulator, &deployment_id, row.case, SETTLE_DEADLINE)
                .await;
        assert_eq!(
            settled["status"].as_str(),
            Some(row.status),
            "case {:?}: expected the clock to settle the deployment as {}, got {settled:?}",
            row.case,
            row.status
        );

        let mut request = json!({"deploymentId": deployment_id});
        if let Some(enabled) = row.auto_rollback {
            request["autoRollbackEnabled"] = json!(enabled);
        }
        let actual =
            refused_as_completed(&emulator, "StopDeployment", request, row.case).await;
        let expected =
            format!("The deployment {deployment_id}{ALREADY_IN_STATE_PHRASE}{}", row.state);
        assert_eq!(
            actual, expected,
            "case {:?}: expected {expected:?}, got {actual:?}",
            row.case
        );
        // Nothing follows the state token: the sentence ends on the state itself.
        assert!(
            actual.ends_with(row.state),
            "case {:?}: the sentence ends on the state {:?} it reports, got {actual:?}",
            row.case,
            row.state
        );

        // The refusal left the settled record exactly as it stood.
        let after = deployment_info(&emulator, &deployment_id).await;
        assert_eq!(
            after, settled,
            "case {:?}: expected the refusal to leave the record untouched, got {after:?}",
            row.case
        );
    }
}

/// RULE: the sentence belongs to the OPERATION that was refused — on one and the
/// same settled deployment a stop reports the state it is already in while an
/// operation over the deployment's progress reports that it has completed — and
/// both name the same state.
#[tokio::test]
async fn the_stop_and_the_progress_refusals_word_the_same_state_their_own_way() {
    // Rows differ along the outcome the deployment settled in, and so along the
    // state both sentences report. Each group is blue/green, which is the type the
    // progress operations serve, so both refusals are reached on one deployment.
    let rows: &[(&str, &str, &str, &str)] = &[
        ("blue/green server", "Server", "Failed", "FAILED"),
        ("blue/green lambda", "Lambda", "Succeeded", "SUCCEEDED"),
    ];

    for (case, platform, status, state) in rows {
        let emulator = Emulator::new();
        let application = case.replace([' ', '/'], "-");
        let group =
            seeded_group(&emulator, &application, platform, &blue_green_style()).await;
        let deployment_id = deploy(
            &emulator,
            &application,
            &group,
            &s3_revision("artifacts", "app.zip"),
        )
        .await;
        let settled =
            terminal_deployment_within(&emulator, &deployment_id, case, SETTLE_DEADLINE).await;
        assert_eq!(
            settled["status"].as_str(),
            Some(*status),
            "case {case:?}: expected the clock to settle the deployment as {status}, got \
             {settled:?}"
        );

        let stopped = refused_as_completed(
            &emulator,
            "StopDeployment",
            json!({"deploymentId": deployment_id}),
            case,
        )
        .await;
        let skipped = refused_as_completed(
            &emulator,
            "SkipWaitTimeForInstanceTermination",
            json!({"deploymentId": deployment_id}),
            case,
        )
        .await;

        assert_eq!(
            stopped,
            format!("The deployment {deployment_id}{ALREADY_IN_STATE_PHRASE}{state}"),
            "case {case:?}: expected the stop's own sentence, got {stopped:?}"
        );
        assert_eq!(
            skipped,
            format!("The deployment {deployment_id}{ALREADY_COMPLETED_PHRASE}{state}."),
            "case {case:?}: expected the progress sentence, got {skipped:?}"
        );
        assert!(
            !stopped.contains(ALREADY_COMPLETED_PHRASE),
            "case {case:?}: the stop's refusal is not worded as the progress refusal, got \
             {stopped:?}"
        );
        assert!(
            stopped.contains(state) && skipped.contains(state),
            "case {case:?}: both refusals report the state {state:?}, got {stopped:?} and \
             {skipped:?}"
        );
    }
}
