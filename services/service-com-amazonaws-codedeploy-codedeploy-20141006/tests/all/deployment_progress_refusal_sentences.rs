// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE the two refusals about a deployment's PROGRESS and TYPE carry,
//! driven over the wire.
//!
//! RULES under test:
//!
//! * `DeploymentAlreadyCompletedException` says
//!   `The deployment <deploymentId> has already completed, current state <STATE>.`
//!   — the id the
//!   request named and the deployment's INTERNAL state name, the upper-snake form
//!   of its `DeploymentStatus`, in which a deployment a caller stopped is
//!   `ABORTED` rather than `Stopped`. Rows span the three outcomes a deployment
//!   reaches, how it reached each one (the clock, or a caller stopping it), the
//!   compute platform, the deployment type and every operation of this service
//!   that raises the error on it. A second `StopDeployment` of a deployment a
//!   stop ALREADY ended is not one of them: it asks for the state that deployment
//!   is already in, so it is answered rather than refused (`stop_deployment`
//!   holds that rule). The same sentence is what a deployment one of those
//!   operations ITSELF completed is refused with, and its fixed phrase is what a
//!   client reads the state back out of.
//! * `UnsupportedActionForDeploymentTypeException` says
//!   `Operation is not supported for the deployment <deploymentId> of type <TYPE>.`
//!   — the id and the `DeploymentType` the deployment runs as, so a caller
//!   learns which type was rejected. Rows span the ways a group ends up in-place
//!   (no style at all, an explicit one), the compute platform, and both
//!   operations that act only on a blue/green deployment.
//!
//! The harness (`Emulator`, `error_code`, `s3_revision`, `terminal_deployment`)
//! is the lifecycle suite's, so every case goes through the same front door a
//! client uses.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_codedeploy_codedeploy_20141006::state::deployment_internal_state;

use crate::codedeploy_lifecycle::{
    error_code, poll_deployment_until, s3_revision, terminal_deployment,
    terminal_deployment_within, Emulator,
};

/// The service role every deployment group here is created with.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The FIXED phrase of the already-completed sentence, between the deployment id
/// the request named and the internal state the deployment settled in — the part
/// a client splits the sentence on to read that state back.
const ALREADY_COMPLETED_PHRASE: &str = " has already completed, current state ";

/// Create an application and one deployment group on it carrying `style` and
/// `blue_green` when those are not null, answering the group name.
async fn seeded_group(
    emulator: &Emulator,
    application: &str,
    platform: &str,
    style: &Value,
    blue_green: &Value,
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
    if !blue_green.is_null() {
        request["blueGreenDeploymentConfiguration"] = blue_green.clone();
    }
    emulator.ok("CreateDeploymentGroup", request).await;
    group
}

/// Create a deployment on `application`/`group`, answering its id.
async fn deploy(emulator: &Emulator, application: &str, group: &str) -> String {
    emulator
        .ok(
            "CreateDeployment",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "revision": s3_revision("artifacts", "app.zip"),
            }),
        )
        .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `$.message` of a failure body.
fn message(body: &Value, case: &str) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected a string $.message, got {body}"))
        .to_string()
}

/// Call `operation`, require HTTP 400 with `code`, and answer the message.
async fn refused(
    emulator: &Emulator,
    operation: &str,
    request: Value,
    code: &str,
    case: &str,
) -> String {
    let (status, body) = emulator.call(operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from {operation}, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        code,
        "case {case:?}: expected {code} from {operation}, got {body:?}"
    );
    message(&body, case)
}

/// The stored `deploymentInfo` of `deployment_id`.
async fn deployment_info(emulator: &Emulator, deployment_id: &str) -> Value {
    emulator
        .ok("GetDeployment", json!({"deploymentId": deployment_id}))
        .await["deploymentInfo"]
        .clone()
}

/// A deployment driven to one of the three outcomes, and the operations that are
/// refused on it for having completed.
struct Completed {
    case: &'static str,
    platform: &'static str,
    /// The group's `deploymentStyle`, absent when null.
    style: Value,
    /// The group's `blueGreenDeploymentConfiguration`, absent when null.
    blue_green: Value,
    /// Whether a caller STOPS the deployment rather than letting the clock
    /// settle it.
    stopped_by_caller: bool,
    /// The `DeploymentStatus` the deployment ends in.
    status: &'static str,
    /// The INTERNAL state name the refusal reports for that status.
    state: &'static str,
    /// The operations refused with `DeploymentAlreadyCompletedException` on it.
    /// `ContinueDeployment` refuses an in-place deployment for its TYPE before
    /// it looks at progress, so it only appears on a blue/green row; a second
    /// `StopDeployment` only appears on a row whose outcome is not a stop,
    /// because a stop of an already-stopped deployment is answered.
    operations: &'static [&'static str],
}

/// The `deploymentStyle` of a blue/green deployment group.
fn blue_green_style() -> Value {
    json!({"deploymentType": "BLUE_GREEN", "deploymentOption": "WITH_TRAFFIC_CONTROL"})
}

/// RULE: `DeploymentAlreadyCompletedException` says
/// `The deployment <deploymentId> has already completed, current state <STATE>.` —
/// the deployment
/// id the request carried and the deployment's internal state name, which for a
/// stopped deployment is `ABORTED` and not its API status — whichever operation
/// was refused and however the deployment reached its outcome.
#[tokio::test]
async fn a_completed_deployment_is_refused_with_its_internal_state() {
    // Rows span every outcome a deployment reaches (Succeeded, Failed, Stopped),
    // how it got there (the clock settling it, a caller stopping it), the
    // compute platform, the deployment type and the operations refused on it.
    let rows: &[Completed] = &[
        Completed {
            case: "stopped in-place server",
            platform: "Server",
            style: Value::Null,
            blue_green: Value::Null,
            stopped_by_caller: true,
            status: "Stopped",
            state: "ABORTED",
            operations: &["SkipWaitTimeForInstanceTermination"],
        },
        Completed {
            case: "stopped blue/green server",
            platform: "Server",
            style: blue_green_style(),
            blue_green: json!({
                "terminateBlueInstancesOnDeploymentSuccess": {
                    "action": "TERMINATE",
                    "terminationWaitTimeInMinutes": 2880,
                }
            }),
            stopped_by_caller: true,
            status: "Stopped",
            state: "ABORTED",
            operations: &["SkipWaitTimeForInstanceTermination", "ContinueDeployment"],
        },
        Completed {
            // An EC2/on-premises group resolves no target here, so the clock
            // settles the deployment as Failed.
            case: "clock-failed in-place server",
            platform: "Server",
            style: Value::Null,
            blue_green: Value::Null,
            stopped_by_caller: false,
            status: "Failed",
            state: "FAILED",
            operations: &["SkipWaitTimeForInstanceTermination", "StopDeployment"],
        },
        Completed {
            case: "clock-failed blue/green server",
            platform: "Server",
            style: blue_green_style(),
            blue_green: json!({
                "terminateBlueInstancesOnDeploymentSuccess": {
                    "action": "KEEP_ALIVE",
                    "terminationWaitTimeInMinutes": 0,
                }
            }),
            stopped_by_caller: false,
            status: "Failed",
            state: "FAILED",
            operations: &[
                "SkipWaitTimeForInstanceTermination",
                "ContinueDeployment",
                "StopDeployment",
            ],
        },
        Completed {
            // A Lambda group's target is the function its revision names, so the
            // clock settles the deployment as Succeeded.
            case: "clock-succeeded in-place lambda",
            platform: "Lambda",
            style: Value::Null,
            blue_green: Value::Null,
            stopped_by_caller: false,
            status: "Succeeded",
            state: "SUCCEEDED",
            operations: &["SkipWaitTimeForInstanceTermination", "StopDeployment"],
        },
        Completed {
            case: "clock-succeeded blue/green lambda",
            platform: "Lambda",
            style: blue_green_style(),
            blue_green: Value::Null,
            stopped_by_caller: false,
            status: "Succeeded",
            state: "SUCCEEDED",
            operations: &[
                "SkipWaitTimeForInstanceTermination",
                "ContinueDeployment",
                "StopDeployment",
            ],
        },
    ];

    for row in rows {
        let emulator = Emulator::new();
        let application = row.case.replace([' ', '/'], "-");
        let group =
            seeded_group(&emulator, &application, row.platform, &row.style, &row.blue_green).await;
        let deployment_id = deploy(&emulator, &application, &group).await;

        if row.stopped_by_caller {
            emulator
                .ok("StopDeployment", json!({"deploymentId": deployment_id}))
                .await;
        } else {
            terminal_deployment(&emulator, &deployment_id, row.case).await;
        }
        let before = deployment_info(&emulator, &deployment_id).await;
        assert_eq!(
            before["status"].as_str(),
            Some(row.status),
            "case {:?}: expected the deployment to hold {} before the refusal, got {before:?}",
            row.case,
            row.status
        );

        let expected = format!(
            "The deployment {deployment_id} has already completed, current state {}.",
            row.state
        );
        for operation in row.operations {
            let case = format!("{}/{operation}", row.case);
            let actual = refused(
                &emulator,
                operation,
                json!({"deploymentId": deployment_id}),
                "DeploymentAlreadyCompletedException",
                &case,
            )
            .await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            // The INTERNAL state is what the sentence reports: a stopped
            // deployment is ABORTED there, so its API status must not appear.
            if row.state != row.status {
                assert!(
                    !actual.contains(row.status),
                    "case {case:?}: the sentence must report the internal state {:?} rather than \
                     the API status {:?}, got {actual:?}",
                    row.state,
                    row.status
                );
            }
        }

        let after = deployment_info(&emulator, &deployment_id).await;
        assert_eq!(
            after, before,
            "case {:?}: expected the refusals to leave the record untouched, got {after:?}",
            row.case
        );
    }
}

/// RULE: `UnsupportedActionForDeploymentTypeException` says
/// `Operation is not supported for the deployment <deploymentId> of type <TYPE>.`
/// — the deployment id the request carried and the `DeploymentType` the
/// deployment runs as — for every operation that acts only on a blue/green
/// deployment.
#[tokio::test]
async fn an_action_outside_a_deployments_type_is_refused_with_that_type() {
    // Rows span the ways a deployment ends up in-place — CodeDeploy's default
    // when a group carries no `deploymentStyle`, and an explicit one, under both
    // `deploymentOption` values — and the compute platform it runs on. IN_PLACE
    // and BLUE_GREEN are the whole `DeploymentType` catalogue
    // (`smithy_query(query="shape",shape="DeploymentType")`), and a blue/green
    // deployment is the one these operations serve, so IN_PLACE is the type this
    // refusal reports.
    let rows: &[(&str, &str, Value)] = &[
        ("no deployment style at all", "Server", Value::Null),
        (
            "an explicit in-place style without traffic control",
            "Server",
            json!({"deploymentType": "IN_PLACE", "deploymentOption": "WITHOUT_TRAFFIC_CONTROL"}),
        ),
        (
            "an explicit in-place style with traffic control",
            "Server",
            json!({"deploymentType": "IN_PLACE", "deploymentOption": "WITH_TRAFFIC_CONTROL"}),
        ),
        ("a lambda deployment with no style", "Lambda", Value::Null),
    ];

    for (case, platform, style) in rows {
        let emulator = Emulator::new();
        let application = case.replace([' ', '/'], "-");
        let group = seeded_group(&emulator, &application, platform, style, &Value::Null).await;
        let deployment_id = deploy(&emulator, &application, &group).await;
        let expected = format!(
            "Operation is not supported for the deployment {deployment_id} of type IN_PLACE."
        );

        // Both operations that act only on a blue/green deployment. The wait-skip
        // judges the deployment's PROGRESS first, so it is called here while the
        // deployment the clock has only just created is still in flight.
        for operation in ["SkipWaitTimeForInstanceTermination", "ContinueDeployment"] {
            let case = format!("{case}/{operation}");
            let actual = refused(
                &emulator,
                operation,
                json!({"deploymentId": deployment_id}),
                "UnsupportedActionForDeploymentTypeException",
                &case,
            )
            .await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }

        // The refusals changed nothing: the deployment still reports its type.
        let info = deployment_info(&emulator, &deployment_id).await;
        assert_eq!(
            info["deploymentStyle"]["deploymentType"].as_str(),
            Some("IN_PLACE"),
            "case {case:?}: expected the deployment to be untouched, got {info:?}"
        );
    }
}

/// RULE: the internal state name of a deployment status is the status in upper
/// snake case, with the one state whose internal name differs from its API
/// status — a deployment a caller stopped is `ABORTED` — and that is the token
/// every refusal about a deployment's progress reports.
#[tokio::test]
async fn every_deployment_status_has_its_internal_state_name() {
    // The catalogue is the model's `DeploymentStatus` enum
    // (`smithy_query(query="shape",shape="DeploymentStatus")`: Created | Queued |
    // InProgress | Baking | Succeeded | Failed | Stopped | Ready), including the
    // statuses no operation of this emulator drives a deployment into.
    let cases: &[(&str, &str)] = &[
        ("Created", "CREATED"),
        ("Queued", "QUEUED"),
        ("InProgress", "IN_PROGRESS"),
        ("Baking", "BAKING"),
        ("Succeeded", "SUCCEEDED"),
        ("Failed", "FAILED"),
        ("Stopped", "ABORTED"),
        ("Ready", "READY"),
    ];

    for (status, expected) in cases {
        let actual = deployment_internal_state(status);
        assert_eq!(
            actual, *expected,
            "case {status:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// A `blueGreenDeploymentConfiguration` that terminates the original instances
/// after `minutes` of waiting, which is what holds such a deployment open and
/// gives `SkipWaitTimeForInstanceTermination` a wait to drop.
fn terminating_blue_green(minutes: i64) -> Value {
    json!({
        "terminateBlueInstancesOnDeploymentSuccess": {
            "action": "TERMINATE",
            "terminationWaitTimeInMinutes": minutes,
        }
    })
}

/// The tag key the group of [`terminating_blue_green_group`] selects its
/// original-environment instances by.
const FLEET_TAG_KEY: &str = "fleet";

/// Create an application, register one on-premises instance for it and create a
/// blue/green deployment group that terminates the original instances after
/// `minutes`, selecting that instance only when `with_target`.
///
/// Whether the group selects an instance is what decides the outcome the clock
/// settles its deployment in, so one helper serves both outcomes. The
/// termination option is an EC2/on-premises one
/// (`smithy_query(query="shape",shape="BlueInstanceTerminationOption")`: it
/// "does not apply to Lambda deployments"), so every case here runs on `Server`.
async fn terminating_blue_green_group(
    emulator: &Emulator,
    application: &str,
    minutes: i64,
    with_target: bool,
) -> String {
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": "Server"}),
        )
        .await;
    let instance = format!("{application}-host");
    emulator
        .ok(
            "RegisterOnPremisesInstance",
            json!({
                "instanceName": instance,
                "iamUserArn": format!("arn:aws:iam::123456789012:user/{instance}"),
            }),
        )
        .await;
    emulator
        .ok(
            "AddTagsToOnPremisesInstances",
            json!({
                "instanceNames": [instance],
                "tags": [{"Key": FLEET_TAG_KEY, "Value": application}],
            }),
        )
        .await;
    // The value the group filters on: the instance's own tag value selects it, a
    // value nothing carries selects nothing.
    let selected = if with_target {
        application.to_string()
    } else {
        format!("{application}-no-such-fleet")
    };
    let group = format!("{application}-dg");
    emulator
        .ok(
            "CreateDeploymentGroup",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "serviceRoleArn": SERVICE_ROLE,
                "deploymentStyle": blue_green_style(),
                "blueGreenDeploymentConfiguration": terminating_blue_green(minutes),
                "onPremisesInstanceTagFilters": [
                    {"Key": FLEET_TAG_KEY, "Value": selected, "Type": "KEY_AND_VALUE"},
                ],
            }),
        )
        .await;
    group
}

/// How long a case of [`a_deployment_the_skipped_wait_completed_is_refused_in_the_same_sentence`]
/// is prepared to wait for a transition the clock owes it.
///
/// Deliberately generous: the span this emulator gives such a transition is a
/// few seconds, and the only duration those cases are ABOUT is the
/// minutes-long termination wait they configure, so nothing here is measured
/// against this deadline — it only keeps a case that hangs from hanging forever.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// RULE: `DeploymentAlreadyCompletedException` carries the same sentence —
/// `The deployment <deploymentId> has already completed, current state <STATE>.`
/// — for a deployment that reached its outcome because this very operation
/// dropped the wait holding it open, with the state token read off the status the
/// service itself reports for that deployment.
#[tokio::test]
async fn a_deployment_the_skipped_wait_completed_is_refused_in_the_same_sentence() {
    // Rows differ along the wait the group configured — an hour, and the maximum
    // the member's own documentation gives
    // (`smithy_query(query="doc",shape="BlueInstanceTerminationOption",member="terminationWaitTimeInMinutes")`:
    // "The maximum setting is 2880 minutes (2 days)") — and along the outcome the
    // clock settles the deployment in, which is what the refusal's state token
    // reports: a group that selects an instance reaches its targets and succeeds,
    // one that selects none reaches nothing and fails. Both waits are far longer
    // than any run of this suite, so each case reaches its outcome only because
    // the wait was SKIPPED, whatever speed the host runs at.
    let rows: &[(&str, i64, bool, &str, &str)] = &[
        (
            "an hour long wait, no target",
            60,
            false,
            "Failed",
            "FAILED",
        ),
        (
            "the documented maximum wait, one target",
            2880,
            true,
            "Succeeded",
            "SUCCEEDED",
        ),
    ];

    for (case, minutes, with_target, status, state) in rows {
        let emulator = Emulator::new();
        let application = case.replace([' ', ',', '/'], "-");
        let group =
            terminating_blue_green_group(&emulator, &application, *minutes, *with_target).await;
        let deployment_id = deploy(&emulator, &application, &group).await;

        // The configured wait holds the deployment open once traffic routing is
        // complete, so the call lands on a deployment IN FLIGHT and is answered.
        let waiting = poll_deployment_until(
            &emulator,
            &deployment_id,
            case,
            SETTLE_DEADLINE,
            |info| info["instanceTerminationWaitTimeStarted"].as_bool() == Some(true),
        )
        .await;
        assert!(
            waiting["completeTime"].is_null(),
            "case {case:?}: expected the wait to still hold the deployment open, got {waiting:?}"
        );
        emulator
            .ok(
                "SkipWaitTimeForInstanceTermination",
                json!({"deploymentId": deployment_id}),
            )
            .await;

        // Dropping the wait lets the clock settle the deployment, and from there
        // the SAME call is refused for its progress.
        let settled =
            terminal_deployment_within(&emulator, &deployment_id, case, SETTLE_DEADLINE).await;
        let settled_status = settled["status"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            settled_status, *status,
            "case {case:?}: expected the skipped wait to settle the deployment as {status}, got \
             {settled:?}"
        );

        // The state the sentence reports is the internal name of the status the
        // service itself reports for the deployment, so the expectation is built
        // from what `GetDeployment` just answered rather than from a token
        // written out here.
        let settled_state = deployment_internal_state(&settled_status);
        assert_eq!(
            settled_state, *state,
            "case {case:?}: expected the internal state of {settled_status:?} to be {state:?}, \
             got {settled_state:?}"
        );
        let expected =
            format!("The deployment {deployment_id}{ALREADY_COMPLETED_PHRASE}{settled_state}.");
        let actual = refused(
            &emulator,
            "SkipWaitTimeForInstanceTermination",
            json!({"deploymentId": deployment_id}),
            "DeploymentAlreadyCompletedException",
            case,
        )
        .await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );

        // The refusal left the settled record exactly as it stood.
        let after = deployment_info(&emulator, &deployment_id).await;
        assert_eq!(
            after, settled,
            "case {case:?}: expected the refusal to leave the record untouched, got {after:?}"
        );
    }
}
