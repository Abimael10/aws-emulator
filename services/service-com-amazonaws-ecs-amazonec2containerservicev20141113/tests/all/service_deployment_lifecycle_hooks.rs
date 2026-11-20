// SPDX-License-Identifier: CC0-1.0

//! `ContinueServiceDeployment`, driven end to end over the shared wire harness in
//! [`crate::ecs_wire`].
//!
//! The rule under test: a rollout whose deployment configuration carries a `PAUSE`
//! lifecycle hook waits at each stage that hook names, in the order the deployment passes
//! through those stages, until the action this operation takes on the hook waiting there
//! moves it on — `CONTINUE` to the next such stage, or to the end of the rollout when it
//! has none left, and `ROLLBACK` onto the service revision the rollout before it released —
//! and a request that names a rollout nothing recorded, or a hook that is not the one
//! waiting for an action, is refused and changes nothing.
//!
//! The docs the rows are read off:
//!
//! * `ContinueServiceDeployment`: "Continues or rolls back an Amazon ECS service deployment
//!   that is paused at a lifecycle hook. When a service deployment reaches a lifecycle stage
//!   that has a PAUSE hook configured, the deployment pauses and waits for an explicit
//!   action."
//! * `ContinueServiceDeployment$action`: "CONTINUE - Proceeds the deployment to the next
//!   lifecycle stage. ROLLBACK - Rolls back the deployment to the previous service revision.
//!   If no value is specified, the default action is CONTINUE."
//! * `ContinueServiceDeployment$hookId`: "The ID of the paused lifecycle hook to act on. You
//!   can find the hookId by calling DescribeServiceDeployments and inspecting the
//!   lifecycleHookDetails field of the service deployment."
//! * `DeploymentLifecycleHook$targetType`: "PAUSE - Pauses the deployment at the specified
//!   lifecycle stage until you call ContinueServiceDeployment to continue or roll back."
//! * `DeploymentLifecycleHookDetail`: `$targetArn` ("For PAUSE hooks, this field is not
//!   set"), `$status` ("Valid values include AWAITING_ACTION, IN_PROGRESS, SUCCEEDED,
//!   FAILED, and TIMED_OUT"), `$expiresAt` ("The time when the lifecycle hook times out")
//!   and `$timeoutAction`.
//! * `DeploymentLifecycleHookTimeoutConfiguration`: `$timeoutInMinutes` ("Default: 1440 (24
//!   hours)") and `$action` ("Default: ROLLBACK").
//! * `ServiceDeployment$lifecycleStage`: "The current lifecycle stage of the deployment",
//!   whose doc lists the stages in the order a deployment passes through them.
//! * `Rollback$serviceRevisionArn`: "The ARN of the service revision deployed as part of the
//!   rollback"; `ServiceDeploymentStatus` (smithy_query query="shape"
//!   shape="ServiceDeploymentStatus": PENDING, SUCCESSFUL, STOPPED, STOP_REQUESTED,
//!   IN_PROGRESS, ROLLBACK_REQUESTED, ROLLBACK_IN_PROGRESS, ROLLBACK_SUCCESSFUL,
//!   ROLLBACK_FAILED).
//! * `ClientException`: "These errors are usually caused by a client action. ... it might be
//!   specifying an identifier that isn't valid."
//!
//! Every case goes through the same front door a client uses, so nothing reaches into the
//! state struct and a response that is not computed from persisted state cannot pass.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, refusal, register_definition, service, string,
};

/// Long enough for one clock-owed transition (`state::TRANSITION_SPAN` is 500 ms) to fall
/// due, with margin for a loaded test machine.
const ONE_SPAN: Duration = Duration::from_millis(900);

/// The cluster every service below is created in.
const CLUSTER: &str = "demo";

/// `DeploymentLifecycleHookTimeoutConfiguration$timeoutInMinutes`'s documented default:
/// "Default: 1440 (24 hours)".
const DEFAULT_TIMEOUT_MINUTES: i64 = 1440;

/// That configuration's `action` default: "Default: ROLLBACK".
const DEFAULT_TIMEOUT_ACTION: &str = "ROLLBACK";

/// The `DeploymentLifecycleHookStatus` of a `PAUSE` hook the rollout is waiting at.
const AWAITING_ACTION: &str = "AWAITING_ACTION";

/// The `DeploymentLifecycleHookStatus` of a hook whose action has been taken.
const SUCCEEDED: &str = "SUCCEEDED";

/// One row of the pause table: the stages a hook is configured for, and the stages the
/// rollout has to wait at as a result.
struct PauseCase {
    case: &'static str,
    /// The `lifecycleStages` the request configures, in the order it lists them.
    configured: &'static [&'static str],
    /// The stages the rollout must pause at, in the order it reaches them.
    expected: &'static [&'static str],
    /// The `timeoutConfiguration` the hook carries, or `None` for a hook that configures
    /// none (so the documented defaults apply).
    timeout: Option<(i64, &'static str)>,
}

/// A `PAUSE` lifecycle hook configured for `stages`, with `timeout` when it sets one.
fn pause_hook(stages: &[&str], timeout: Option<(i64, &str)>) -> Value {
    let mut hook = json!({
        "targetType": "PAUSE",
        "lifecycleStages": stages,
    });
    if let Some((minutes, action)) = timeout {
        hook["timeoutConfiguration"] = json!({
            "timeoutInMinutes": minutes,
            "action": action,
        });
    }
    hook
}

/// A blue/green deployment configuration carrying `hooks`.
fn configuration(hooks: Vec<Value>) -> Value {
    json!({
        "strategy": "BLUE_GREEN",
        // "You must provide this parameter when you use the BLUE_GREEN deployment
        // strategy."
        "bakeTimeInMinutes": 1,
        "lifecycleHooks": hooks,
    })
}

/// Create a service whose rollout the ECS scheduler owns, under `deployment_configuration`
/// when one is given, and return the ARN of the service deployment that rollout is recorded
/// as.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    deployment_configuration: Option<Value>,
    case: &str,
) -> String {
    let mut request = json!({
        "cluster": CLUSTER,
        "serviceName": name,
        "taskDefinition": definition,
        "desiredCount": 1,
        "launchType": "EC2",
    });
    if let Some(configuration) = deployment_configuration {
        request["deploymentConfiguration"] = configuration;
    }
    let body = ok(router, "CreateService", request, case).await;
    string(&body["service"], "currentServiceDeployment", case)
}

/// Move `name` onto `definition` under `deployment_configuration`, and return the ARN of
/// the service deployment the update started.
async fn update_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    deployment_configuration: Option<Value>,
    case: &str,
) -> String {
    let mut request = json!({
        "cluster": CLUSTER,
        "service": name,
        "taskDefinition": definition,
    });
    if let Some(configuration) = deployment_configuration {
        request["deploymentConfiguration"] = configuration;
    }
    let body = ok(router, "UpdateService", request, case).await;
    string(&body["service"], "currentServiceDeployment", case)
}

/// The stored service, as `DescribeServices` reports it.
async fn describe_service(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    list(&body, "services", case)
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service to be described"))
        .clone()
}

/// The stored rollout `arn` names, as `DescribeServiceDeployments` reports it.
async fn describe_deployment(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [arn] }),
        case,
    )
    .await;
    list(&body, "serviceDeployments", case)
        .first()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the service deployment {arn} to be described")
        })
        .clone()
}

/// Re-read the stored rollout `arn` until a `PAUSE` hook of it is waiting for an action,
/// and answer it.
///
/// The stage a rollout stops at is one the CLOCK brings it to, so it is polled for rather
/// than timed.
async fn poll_awaiting_hook(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let deployment = describe_deployment(router, arn, case).await;
        let waiting = list(&deployment, "lifecycleHookDetails", case)
            .iter()
            .any(|hook| hook["status"] == json!(AWAITING_ACTION));
        if waiting && !deployment["lifecycleStage"].is_null() {
            return deployment;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the rollout to be waiting at a hook, got {deployment}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Re-read the stored rollout `arn` until its `status` is `wanted`, and answer it.
///
/// The rollout that REDEPLOYS a revision replaces the task set the rollout it undid left
/// behind, which is several scheduler steps (the replacement is started, comes up, and the
/// task it replaces is retired), so its end is polled for rather than timed.
async fn poll_deployment_status(
    router: &MultiServiceRouter,
    arn: &str,
    wanted: &str,
    case: &str,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let deployment = describe_deployment(router, arn, case).await;
        if deployment["status"] == json!(wanted) {
            return deployment;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the rollout to reach {wanted}, got {deployment}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One successful `ContinueServiceDeployment`, with `action` when the request names one.
async fn continue_deployment(
    router: &MultiServiceRouter,
    arn: &str,
    hook_id: &str,
    action: Option<&str>,
    case: &str,
) -> String {
    let mut request = json!({
        "serviceDeploymentArn": arn,
        "hookId": hook_id,
    });
    if let Some(action) = action {
        request["action"] = json!(action);
    }
    let body = ok(router, "ContinueServiceDeployment", request, case).await;
    string(&body, "serviceDeploymentArn", case)
}

/// The hook detail of the rollout that is waiting for an action, with the stage the rollout
/// reports standing at.
fn awaiting_hook(deployment: &Value, case: &str) -> (Value, String) {
    let hooks = list(deployment, "lifecycleHookDetails", case);
    let waiting: Vec<Value> = hooks
        .iter()
        .filter(|hook| hook["status"] == json!(AWAITING_ACTION))
        .cloned()
        .collect();
    assert_eq!(
        waiting.len(),
        1,
        "case {case:?}: expected exactly one hook waiting for an action, got {deployment}"
    );
    let stage = deployment["lifecycleStage"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a lifecycleStage, got {deployment}"))
        .to_string();
    (waiting[0].clone(), stage)
}

/// A service-deployment ARN shaped exactly like `arn` whose deployment-id token names no
/// stored rollout. The token's WIDTH is part of the ARN's shape, so the id is replaced by
/// one of the same width rather than extended — and the counter that mints them starts at
/// one, so the all-zero id belongs to no rollout.
fn unrecorded_deployment_arn(arn: &str) -> String {
    let (prefix, id) = arn
        .rsplit_once('/')
        .expect("a service-deployment ARN ends in its deployment-id token");
    format!("{prefix}/{:0width$}", 0, width = id.len())
}

/// A timestamp member, as the epoch seconds an `awsJson1_1` body renders it as.
fn seconds(body: &Value, member: &str, case: &str) -> f64 {
    body[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a timestamp {member}, got {body}"))
}

/// A rollout pauses at every stage its `PAUSE` hook names, in the documented stage order,
/// and each `CONTINUE` takes it to the next one — the last to the end of the rollout.
#[tokio::test]
async fn pause_hooks_wait_at_each_configured_stage_in_order() {
    let cases = [
        PauseCase {
            case: "one stage",
            configured: &["POST_SCALE_UP"],
            expected: &["POST_SCALE_UP"],
            timeout: None,
        },
        PauseCase {
            case: "two stages, configured in lifecycle order",
            configured: &["PRE_SCALE_UP", "POST_PRODUCTION_TRAFFIC_SHIFT"],
            expected: &["PRE_SCALE_UP", "POST_PRODUCTION_TRAFFIC_SHIFT"],
            timeout: None,
        },
        PauseCase {
            case: "two stages, configured in reverse",
            configured: &["PRODUCTION_TRAFFIC_SHIFT", "RECONCILE_SERVICE"],
            expected: &["RECONCILE_SERVICE", "PRODUCTION_TRAFFIC_SHIFT"],
            timeout: None,
        },
        PauseCase {
            case: "the same stage twice",
            configured: &["TEST_TRAFFIC_SHIFT", "TEST_TRAFFIC_SHIFT"],
            expected: &["TEST_TRAFFIC_SHIFT"],
            timeout: Some((5, "CONTINUE")),
        },
        PauseCase {
            case: "three stages with a configured timeout",
            configured: &[
                "POST_TEST_TRAFFIC_SHIFT",
                "PRE_PRODUCTION_TRAFFIC_SHIFT",
                "RECONCILE_SERVICE",
            ],
            expected: &[
                "RECONCILE_SERVICE",
                "POST_TEST_TRAFFIC_SHIFT",
                "PRE_PRODUCTION_TRAFFIC_SHIFT",
            ],
            timeout: Some((20160, "ROLLBACK")),
        },
    ];

    let router = service();
    cluster_with_capacity(&router, CLUSTER, "capacity").await;

    for (index, expectation) in cases.iter().enumerate() {
        let case = expectation.case;
        let family = format!("hooked-{index}");
        let definition = register_definition(&router, &family, case).await;
        let name = format!("hooked-service-{index}");
        let arn = create_service(
            &router,
            &name,
            &definition,
            Some(configuration(vec![pause_hook(
                expectation.configured,
                expectation.timeout,
            )])),
            case,
        )
        .await;

        let (minutes, timeout_action) = expectation
            .timeout
            .unwrap_or((DEFAULT_TIMEOUT_MINUTES, DEFAULT_TIMEOUT_ACTION));
        let created_at = {
            let deployment = describe_deployment(&router, &arn, case).await;
            seconds(&deployment, "createdAt", case)
        };

        for (step, stage) in expectation.expected.iter().enumerate() {
            tokio::time::sleep(ONE_SPAN).await;
            let deployment = describe_deployment(&router, &arn, case).await;
            assert_eq!(
                deployment["status"],
                json!("IN_PROGRESS"),
                "case {case:?} stage {stage:?}: a rollout waiting at a lifecycle hook is \
                 still under way, got {deployment}"
            );
            assert!(
                deployment["finishedAt"].is_null(),
                "case {case:?} stage {stage:?}: a rollout waiting at a lifecycle hook has \
                 not finished, got {deployment}"
            );
            let (hook, reported_stage) = awaiting_hook(&deployment, case);
            assert_eq!(
                &reported_stage, stage,
                "case {case:?} step {step}: expected the rollout to stand at {stage:?}, got \
                 {reported_stage:?}"
            );
            assert_eq!(
                hook["targetType"],
                json!("PAUSE"),
                "case {case:?} stage {stage:?}: expected a PAUSE hook, got {hook}"
            );
            assert!(
                hook["targetArn"].is_null(),
                "case {case:?} stage {stage:?}: a PAUSE hook sets no targetArn, got {hook}"
            );
            assert_eq!(
                hook["timeoutAction"],
                json!(timeout_action),
                "case {case:?} stage {stage:?}: expected the configured timeout action, got \
                 {hook}"
            );
            // The hook times out `timeoutInMinutes` after the rollout REACHED it, which is
            // one clock-owed span after the call that sent it there.
            let expires_at = seconds(&hook, "expiresAt", case);
            let earliest = created_at + (minutes * 60) as f64;
            assert!(
                (earliest..earliest + 60.0).contains(&expires_at),
                "case {case:?} stage {stage:?}: expected expiresAt in \
                 {earliest}..{}, got {expires_at}",
                earliest + 60.0
            );

            let hook_id = string(&hook, "hookId", case);
            let answered = continue_deployment(&router, &arn, &hook_id, Some("CONTINUE"), case).await;
            assert_eq!(
                answered, arn,
                "case {case:?} stage {stage:?}: expected the continued deployment's own ARN"
            );
        }

        // Past the last hook the rollout runs to its end, which the clock settles.
        tokio::time::sleep(ONE_SPAN).await;
        let deployment = describe_deployment(&router, &arn, case).await;
        assert_eq!(
            deployment["status"],
            json!("SUCCESSFUL"),
            "case {case:?}: expected the resumed rollout to complete, got {deployment}"
        );
        assert!(
            !deployment["finishedAt"].is_null(),
            "case {case:?}: a completed rollout reports when it finished, got {deployment}"
        );
        assert!(
            deployment["lifecycleStage"].is_null(),
            "case {case:?}: a rollout that waits at no hook stands at no stage, got \
             {deployment}"
        );
        let hooks = list(&deployment, "lifecycleHookDetails", case);
        assert_eq!(
            hooks.len(),
            expectation.expected.len(),
            "case {case:?}: expected one hook detail per stage waited at, got {deployment}"
        );
        for hook in &hooks {
            assert_eq!(
                hook["status"],
                json!(SUCCEEDED),
                "case {case:?}: every hook acted on has succeeded, got {hook}"
            );
        }
    }
}

/// A rollout waiting at a `PAUSE` hook does not finish on its own, however long nobody
/// acts: the task set it placed settles, and the rollout stays under way.
#[tokio::test]
async fn a_waiting_rollout_does_not_finish_on_its_own() {
    let case = "waiting rollout";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let definition = register_definition(&router, "patient", case).await;
    let arn = create_service(
        &router,
        "patient-service",
        &definition,
        Some(configuration(vec![pause_hook(&["POST_SCALE_UP"], None)])),
        case,
    )
    .await;

    for round in 0..4 {
        tokio::time::sleep(ONE_SPAN).await;
        let deployment = describe_deployment(&router, &arn, case).await;
        assert_eq!(
            deployment["status"],
            json!("IN_PROGRESS"),
            "case {case:?} round {round}: expected the rollout to keep waiting, got \
             {deployment}"
        );
        let (_, stage) = awaiting_hook(&deployment, case);
        assert_eq!(
            stage, "POST_SCALE_UP",
            "case {case:?} round {round}: expected the rollout to stand at its hooked stage"
        );
    }

    // The service itself is running what it was asked to run while its rollout waits.
    let service = describe_service(&router, "patient-service", case).await;
    assert_eq!(
        service["runningCount"],
        json!(1),
        "case {case:?}: expected the placed task to be running, got {service}"
    );
}

/// A request that names no action takes the documented default, `CONTINUE`.
#[tokio::test]
async fn an_absent_action_continues_the_deployment() {
    let case = "absent action";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let definition = register_definition(&router, "defaulted", case).await;
    let arn = create_service(
        &router,
        "defaulted-service",
        &definition,
        Some(configuration(vec![pause_hook(&["PRE_SCALE_UP"], None)])),
        case,
    )
    .await;

    tokio::time::sleep(ONE_SPAN).await;
    let deployment = describe_deployment(&router, &arn, case).await;
    let (hook, _) = awaiting_hook(&deployment, case);
    let hook_id = string(&hook, "hookId", case);
    continue_deployment(&router, &arn, &hook_id, None, case).await;

    tokio::time::sleep(ONE_SPAN).await;
    let deployment = describe_deployment(&router, &arn, case).await;
    assert_eq!(
        deployment["status"],
        json!("SUCCESSFUL"),
        "case {case:?}: expected the default action to proceed the rollout, got {deployment}"
    );
    assert!(
        deployment["rollback"].is_null(),
        "case {case:?}: a rollout nobody rolled back reports no rollback, got {deployment}"
    );
}

/// One row of the rollback table: where the rollout was waiting when it was rolled back.
struct RollbackCase {
    case: &'static str,
    /// The stages the hook of the rolled-back rollout is configured for.
    configured: &'static [&'static str],
}

/// `ROLLBACK` puts the service back onto the revision the rollout before it released, and
/// ends the rolled-back rollout as rolled back.
#[tokio::test]
async fn rollback_puts_the_service_back_on_the_previous_revision() {
    let cases = [
        RollbackCase {
            case: "rolled back at its only hooked stage",
            configured: &["POST_SCALE_UP"],
        },
        RollbackCase {
            case: "rolled back at the first of two hooked stages",
            configured: &["RECONCILE_SERVICE", "PRODUCTION_TRAFFIC_SHIFT"],
        },
    ];

    let router = service();
    cluster_with_capacity(&router, CLUSTER, "capacity").await;

    for (index, expectation) in cases.iter().enumerate() {
        let case = expectation.case;
        let family = format!("rolled-back-{index}");
        let name = format!("rolled-back-service-{index}");
        // The revision the service runs first, and the one a hooked rollout then releases.
        let first = register_definition(&router, &family, case).await;
        let second = register_definition(&router, &family, case).await;
        let released = create_service(&router, &name, &first, None, case).await;
        let source_revision = {
            let deployment = poll_deployment_status(&router, &released, "SUCCESSFUL", case).await;
            string(&deployment["targetServiceRevision"], "arn", case)
        };

        let rolled_back = update_service(
            &router,
            &name,
            &second,
            Some(configuration(vec![pause_hook(
                expectation.configured,
                None,
            )])),
            case,
        )
        .await;
        let deployment = poll_awaiting_hook(&router, &rolled_back, case).await;
        let (hook, _) = awaiting_hook(&deployment, case);
        let hook_id = string(&hook, "hookId", case);
        let answered =
            continue_deployment(&router, &rolled_back, &hook_id, Some("ROLLBACK"), case).await;
        assert_eq!(
            answered, rolled_back,
            "case {case:?}: expected the rolled-back deployment's own ARN"
        );

        // The rollback is under way the instant it is asked for, and over a span later.
        let deployment = describe_deployment(&router, &rolled_back, case).await;
        assert_eq!(
            deployment["status"],
            json!("ROLLBACK_IN_PROGRESS"),
            "case {case:?}: expected the rollback to be under way, got {deployment}"
        );
        let deployment =
            poll_deployment_status(&router, &rolled_back, "ROLLBACK_SUCCESSFUL", case).await;
        assert_eq!(
            deployment["status"],
            json!("ROLLBACK_SUCCESSFUL"),
            "case {case:?}: expected the rollback to have run its course, got {deployment}"
        );
        assert!(
            !deployment["finishedAt"].is_null(),
            "case {case:?}: a rolled-back rollout reports when it ended, got {deployment}"
        );
        assert_eq!(
            deployment["rollback"]["serviceRevisionArn"],
            json!(source_revision),
            "case {case:?}: expected the rollback to name the previous revision, got \
             {deployment}"
        );
        assert!(
            !deployment["rollback"]["startedAt"].is_null(),
            "case {case:?}: a rollback reports when it started, got {deployment}"
        );
        assert_eq!(
            deployment["lifecycleStage"],
            Value::Null,
            "case {case:?}: a rolled-back rollout waits at no stage, got {deployment}"
        );
        for hook in list(&deployment, "lifecycleHookDetails", case) {
            assert_eq!(
                hook["status"],
                json!(SUCCEEDED),
                "case {case:?}: the hook the rollback was taken on has succeeded, got {hook}"
            );
        }

        // The service runs the previous revision again, under the rollout that put it back.
        let stored = describe_service(&router, &name, case).await;
        assert_eq!(
            stored["taskDefinition"],
            json!(first),
            "case {case:?}: expected the service back on the previous revision, got {stored}"
        );
        let current = string(&stored, "currentServiceDeployment", case);
        assert_ne!(
            current, rolled_back,
            "case {case:?}: expected the redeployment of the previous revision to be the \
             service's current rollout"
        );
        let redeployment = describe_deployment(&router, &current, case).await;
        assert_eq!(
            redeployment["targetServiceRevision"]["arn"],
            json!(source_revision),
            "case {case:?}: expected the redeployment to release the previous revision, got \
             {redeployment}"
        );
        assert!(
            list(&redeployment, "lifecycleHookDetails", case).is_empty(),
            "case {case:?}: the redeployment of the previous revision waits at no hook of \
             the rollout it undid, got {redeployment}"
        );
        let redeployment = poll_deployment_status(&router, &current, "SUCCESSFUL", case).await;
        assert_eq!(
            redeployment["status"],
            json!("SUCCESSFUL"),
            "case {case:?}: expected the redeployment to complete, got {redeployment}"
        );
    }
}

/// One row of the no-waiting-hook table: the ARN and the hook the request names, the action
/// it asks for, the modelled error it earns, and whether that refusal names the rollout the
/// request carried — an ARN that names no stored rollout at all is answered with the fixed
/// sentence for that condition, which carries none of the request's values.
struct RefusedRow {
    case: &'static str,
    arn: String,
    hook_id: String,
    action: Option<&'static str>,
    expected: &'static str,
    names_the_arn: bool,
}

/// Every request that names no hook waiting for an action is refused, and changes nothing.
#[tokio::test]
async fn requests_that_name_no_waiting_hook_are_refused() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "capacity").await;

    // A rollout that configured no lifecycle hook at all: it waits at nothing, which is the
    // state the recorded unit of this issue calls on.
    let plain_definition = register_definition(&router, "plain", "no hook").await;
    let plain = create_service(&router, "plain-service", &plain_definition, None, "no hook").await;

    // A rollout that IS waiting, so the hook id and the rollout can each be wrong on their
    // own.
    let paused_definition = register_definition(&router, "paused", "paused").await;
    let paused = create_service(
        &router,
        "paused-service",
        &paused_definition,
        Some(configuration(vec![pause_hook(&["POST_SCALE_UP"], None)])),
        "paused",
    )
    .await;
    let deployment = poll_awaiting_hook(&router, &paused, "paused").await;
    let (hook, _) = awaiting_hook(&deployment, "paused");
    let waiting_hook_id = string(&hook, "hookId", "paused");

    // A rollout a later one replaced, so it has ENDED while its hook was still waiting.
    let replaced_definition = register_definition(&router, "replaced", "replaced").await;
    let next_definition = register_definition(&router, "replaced", "replaced").await;
    let replaced = create_service(
        &router,
        "replaced-service",
        &replaced_definition,
        Some(configuration(vec![pause_hook(&["PRE_SCALE_UP"], None)])),
        "replaced",
    )
    .await;
    let ended_hook_id = {
        let deployment = poll_awaiting_hook(&router, &replaced, "replaced").await;
        let (hook, _) = awaiting_hook(&deployment, "replaced");
        string(&hook, "hookId", "replaced")
    };
    update_service(
        &router,
        "replaced-service",
        &next_definition,
        None,
        "replaced",
    )
    .await;

    // Each row names the ARN and the hook the request carries; see [`RefusedRow`].
    let cases: Vec<RefusedRow> = vec![
        RefusedRow {
            case: "an ARN no rollout was ever recorded under",
            arn: unrecorded_deployment_arn(&plain),
            hook_id: waiting_hook_id.clone(),
            action: None,
            expected: "ClientException",
            names_the_arn: false,
        },
        RefusedRow {
            case: "a rollout that configured no lifecycle hook",
            arn: plain.clone(),
            hook_id: waiting_hook_id.clone(),
            action: None,
            expected: "InvalidParameterException",
            names_the_arn: true,
        },
        RefusedRow {
            case: "a hook id of another rollout",
            arn: paused.clone(),
            hook_id: ended_hook_id.clone(),
            action: Some("CONTINUE"),
            expected: "InvalidParameterException",
            names_the_arn: true,
        },
        RefusedRow {
            case: "a rollout that already ended",
            arn: replaced.clone(),
            hook_id: ended_hook_id.clone(),
            action: Some("CONTINUE"),
            expected: "InvalidParameterException",
            names_the_arn: true,
        },
        RefusedRow {
            case: "a ROLLBACK of the first rollout of a service",
            arn: paused.clone(),
            hook_id: waiting_hook_id.clone(),
            action: Some("ROLLBACK"),
            expected: "InvalidParameterException",
            names_the_arn: true,
        },
    ];

    for row in cases {
        let case = row.case;
        let mut request = json!({
            "serviceDeploymentArn": row.arn,
            "hookId": row.hook_id,
        });
        if let Some(action) = row.action {
            request["action"] = json!(action);
        }
        let (code, message) = refusal(&router, "ContinueServiceDeployment", request, case).await;
        assert_eq!(
            code, row.expected,
            "case {case:?}: expected a {}, got {code} with {message:?}",
            row.expected
        );
        if row.names_the_arn {
            assert!(
                message.contains(&row.arn),
                "case {case:?}: expected the refusal to name {}, got {message:?}",
                row.arn
            );
        } else {
            assert_eq!(
                message, "The service deployment does not exist.",
                "case {case:?}: expected the not-found refusal, got {message:?}"
            );
        }
    }

    // Nothing the refusals touched moved: the waiting hook is still waiting, and the
    // service still runs the revision it was rolled out on. Read back AFTER the refusals,
    // so a refusal that mutated the rollout would be caught here.
    let deployment = describe_deployment(&router, &paused, "after the refusals").await;
    let (hook, stage) = awaiting_hook(&deployment, "after the refusals");
    assert_eq!(
        string(&hook, "hookId", "after the refusals"),
        waiting_hook_id,
        "case \"after the refusals\": expected the same hook to be waiting still"
    );
    assert_eq!(
        stage, "POST_SCALE_UP",
        "case \"after the refusals\": expected the rollout to stand where it stood"
    );
    let stored = describe_service(&router, "paused-service", "after the refusals").await;
    assert_eq!(
        stored["taskDefinition"],
        json!(paused_definition),
        "case \"after the refusals\": expected the service on its own revision, got {stored}"
    );

    // And the hook the refusals left waiting is still one an action can be taken on, so the
    // refusals were about what they named and not about the hook they left alone.
    let hook_id = string(&hook, "hookId", "after the refusals");
    continue_deployment(
        &router,
        &paused,
        &hook_id,
        Some("CONTINUE"),
        "after the refusals",
    )
    .await;
    let deployment = poll_deployment_status(&router, &paused, "SUCCESSFUL", "after the refusals")
        .await;
    assert_eq!(
        deployment["status"],
        json!("SUCCESSFUL"),
        "case \"after the refusals\": expected the continued rollout to complete, got \
         {deployment}"
    );

    // The rollout a later deployment replaced waits at nothing any more: its hook never got
    // the action it was waiting for.
    let ended = describe_deployment(&router, &replaced, "ended rollout").await;
    assert!(
        ended["lifecycleStage"].is_null(),
        "case \"ended rollout\": a rollout that ended stands at no stage, got {ended}"
    );
    for hook in list(&ended, "lifecycleHookDetails", "ended rollout") {
        assert_eq!(
            hook["status"],
            json!("FAILED"),
            "case \"ended rollout\": expected the unanswered hook to have failed, got {hook}"
        );
    }
}

/// The stage order this service reaches hooked stages in covers exactly the stages a hook
/// may name, which is the generated `DeploymentLifecycleHookStage` value list.
#[test]
fn the_hook_stage_order_covers_every_stage_a_hook_may_name() {
    let order =
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::LIFECYCLE_HOOK_STAGE_ORDER;
    let modelled = service_com_amazonaws_ecs_amazonec2containerservicev20141113::ops::types::DEPLOYMENT_LIFECYCLE_HOOK_STAGE_VALUES;
    let mut ordered: Vec<&str> = order.to_vec();
    ordered.sort_unstable();
    let mut expected: Vec<&str> = modelled.to_vec();
    expected.sort_unstable();
    assert_eq!(
        ordered, expected,
        "case \"stage catalog\": expected the stage order to list every modelled hook \
         stage once, got {order:?} against {modelled:?}"
    );
}
