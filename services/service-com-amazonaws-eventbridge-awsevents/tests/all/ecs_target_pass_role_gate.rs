// SPDX-License-Identifier: CC0-1.0

//! What the policies in force on an AMAZON ECS TASK TARGET's role decide, end to end over
//! the wire through the real `MultiServiceRouter` with Amazon ECS, Amazon EC2, Amazon SQS and
//! IAM mounted beside EventBridge — so the task a delivery ran is read back where a client
//! reads it (`ecs:ListTasks`/`ecs:DescribeTasks`) and the event a delivery could not make is
//! read back off the queue the target's `DeadLetterConfig` names.
//!
//! The RULE asserted here: a rule target EventBridge invokes ACTING AS the role the target
//! names ("the ARN of the IAM role to be used for this target when the rule is triggered",
//! `Target$RoleArn`) is invoked only when the documents in force on that role permit EVERY
//! action the invocation performs — for a task target, `ecs:RunTask` on the TASK DEFINITION
//! the target's `EcsParameters` name, with the cluster carried as the `ecs:cluster` condition
//! key, and `iam:PassRole` on each role the run HANDS Amazon ECS ("to pass a role to a
//! service, a user must have permission to pass the role to the service"), under the
//! `iam:PassedToService` key naming the service they are handed to. A delivery those
//! documents bar runs NOTHING and is sent to the queue that target's `DeadLetterConfig` names
//! carrying EventBridge's own dead-letter attributes — `ERROR_CODE` `NO_PERMISSIONS`,
//! `ERROR_MESSAGE`, and `RULE_ARN`/`TARGET_ARN` naming the rule and the target it failed for
//! — with the transformed input the target would have been invoked with as the message body.
//!
//! The rows differ along what the role is granted and how that grant is written: the action
//! it names, the resource it is scoped to, the condition key it is written under, whether a
//! deny stands beside it, and whether the run hands any role over at all.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    rule_arn, InvocationOutcome, DEFAULT_EVENT_BUS_NAME, NO_PERMISSIONS, SERVICE_PRINCIPAL,
};

use crate::target_dispatch::{
    described, drain, ec2, emulator, fargate_parameters, given_cluster, given_queue,
    given_revision, given_rule, iam, invocations, publish, tasks_started_by, trust_policy,
    xml_text, Emulator, POLL_INTERVAL, REGION, TASK_LAUNCH_BUDGET,
};

/// The two actions one delivery to a task target performs: the run itself, and the hand-over
/// of each role that run gives Amazon ECS.
const RUN_TASK_ACTION: &str = "ecs:RunTask";
const PASS_ROLE_ACTION: &str = "iam:PassRole";

/// The condition key naming the service a role is being handed TO, and the one naming the
/// cluster a run places its task in — the keys IAM and Amazon ECS publish for those two
/// actions.
const PASSED_TO_SERVICE_KEY: &str = "iam:PassedToService";
const CLUSTER_CONDITION_KEY: &str = "ecs:cluster";

/// The service principal Amazon ECS assumes a task's roles as, which is the service a run
/// hands them to.
const TASKS_SERVICE_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// Another service principal entirely, for the row whose grant is scoped to it: a role handed
/// to the tasks service is not the hand-over that grant admits.
const OTHER_SERVICE_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The session name EventBridge opens on a target's role, which is the identity a refusal of
/// the delivery is about: the service's own prefix, the rule and the target id.
const SESSION_NAME_PREFIX: &str = "AWSEvents_";

/// The first subnet of the account, as a client reads them — the networking an `awsvpc` task
/// of these runs is placed into.
async fn a_subnet(emulator: &Emulator, case: &str) -> String {
    let (status, listed) = ec2(emulator, "DescribeSubnets", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSubnets answered {status}: {listed}"
    );
    xml_text(&listed, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: no subnetId in {listed}"))
}

/// A role called `name` trusting `principal`, carrying the inline document `statements` state
/// when a row states any; answers the ARN IAM persisted.
async fn given_role_with(
    emulator: &Emulator,
    case: &str,
    name: &str,
    principal: &str,
    statements: &Value,
) -> String {
    let trust = trust_policy(principal);
    let (status, created) = iam(
        emulator,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole answered {status}: {created}"
    );
    let arn =
        xml_text(&created, "Arn").unwrap_or_else(|| panic!("case {case:?}: no Arn in {created}"));
    let stated = statements.as_array().is_none_or(|list| list.is_empty());
    if !stated {
        let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
        let (status, written) = iam(
            emulator,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "delivery"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRolePolicy answered {status}: {written}"
        );
    }
    arn
}

/// The tasks of `cluster` the rule `rule` ran, in whichever status they are now: EventBridge
/// stamps `RunTask$startedBy` with the rule, and a task on its way out is one the run still
/// placed — what the rows assert about is whether the RUN was made.
async fn launched_by(emulator: &Emulator, case: &str, cluster: &str, rule: &str) -> Vec<String> {
    let mut arns: Vec<String> = Vec::new();
    for desired_status in [None, Some("STOPPED")] {
        for arn in tasks_started_by(emulator, case, cluster, rule, desired_status).await {
            if !arns.contains(&arn) {
                arns.push(arn);
            }
        }
    }
    arns
}

/// [`launched_by`] read again until it holds `wanted` tasks or the budget runs out: a delivery
/// is made inside `PutEvents`, so the first read usually answers, and the loop is what keeps
/// the assertion about the run rather than about the host.
async fn launched_until(
    emulator: &Emulator,
    case: &str,
    cluster: &str,
    rule: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + TASK_LAUNCH_BUDGET;
    let mut seen = launched_by(emulator, case, cluster, rule).await;
    while seen.len() < wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        seen = launched_by(emulator, case, cluster, rule).await;
    }
    seen
}

/// The messages waiting on the queue at `url`, read again until there is at least one or the
/// budget runs out — the condition is looked for, never waited out.
async fn dead_lettered(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + TASK_LAUNCH_BUDGET;
    loop {
        let messages = drain(emulator, url).await;
        if !messages.is_empty() {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the undeliverable event never reached the dead-letter queue at {url}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The value of one message attribute, as a string.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(ToString::to_string)
}

/// One statement allowing `action` on `resource`, written as a client sends it.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// The same, written under one condition key.
fn allow_if(action: &str, resource: &str, key: &str, value: &str) -> Value {
    json!({
        "Effect": "Allow",
        "Action": action,
        "Resource": resource,
        "Condition": { "StringEquals": { key: value } },
    })
}

/// One statement denying `action` on `resource`.
fn deny(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Deny", "Action": action, "Resource": resource })
}

/// The ARNs a row's statements are written about: what the run names, what it places into, the
/// role it hands over, and a role it does not.
struct Scene<'a> {
    task_definition: &'a str,
    cluster: &'a str,
    execution_role: &'a str,
    unrelated_role: &'a str,
}

/// The `InputTransformer` a target of these rules carries: a `TaskOverride` document naming
/// the leg it belongs to and carrying one value selected out of the event — so what a target
/// WOULD have been invoked with is a value distinct from the envelope and from the other
/// target's rendering.
fn leg_transformer(leg: &str) -> Value {
    json!({
        "InputPathsMap": { "order": "$.detail.orderId" },
        "InputTemplate": leg_template(leg),
    })
}

/// The template [`leg_transformer`] renders, with `<order>` standing where the selected value
/// is spliced in.
fn leg_template(leg: &str) -> String {
    format!(
        "{{\"containerOverrides\":[{{\"name\":\"app\",\"environment\":\
         [{{\"name\":\"LEG\",\"value\":\"{leg}\"}},\
         {{\"name\":\"ORDER\",\"value\":\"<order>\"}}]}}]}}"
    )
}

/// What that template renders to for the event these suites publish, whose detail carries the
/// order id `A-1`.
fn leg_rendering(leg: &str) -> Value {
    json!({
        "containerOverrides": [{
            "name": "app",
            "environment": [
                { "name": "LEG", "value": leg },
                { "name": "ORDER", "value": "A-1" },
            ],
        }],
    })
}

/// Whether the overrides `task` runs under carry the environment pair `name`=`value`, which is
/// what says WHICH target's rendering the run was made from.
fn carries_environment(task: &Value, name: &str, value: &str) -> bool {
    task["overrides"]["containerOverrides"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|container| container["environment"].as_array().into_iter().flatten())
        .any(|pair| pair["name"] == json!(name) && pair["value"] == json!(value))
}

/// The sentence a delivery refused for want of a grant reports, which the dead letter's
/// `ERROR_MESSAGE` carries in full: who was refused — the SESSION of the target's role the
/// call would have been made in — which action, on which resource, and the clause naming the
/// document that withheld it. Built from this suite's own literals, so a wrong answer in the
/// implementation cannot make the assertion pass.
fn no_identity_policy_sentence(
    role_name: &str,
    rule: &str,
    target_id: &str,
    action: &str,
    resource: &str,
) -> String {
    format!(
        "User: arn:aws:sts::{account}:assumed-role/{role_name}/{SESSION_NAME_PREFIX}{rule}_\
         {target_id} is not authorized to perform: {action} on resource: {resource} because no \
         identity-based policy allows the {action} action",
        account = engine::emulator_account_id(),
    )
}

/// RULE: of two targets of ONE rule differing only in whether their role may PASS the roles
/// the run hands Amazon ECS, the one whose role may launches a task on the cluster and
/// dead-letters nothing, and the one whose role may not launches nothing and has the
/// transformed input it would have been invoked with parked on its own dead-letter queue under
/// `ERROR_CODE` `NO_PERMISSIONS`, with `RULE_ARN` and `TARGET_ARN` naming that rule and that
/// target.
///
/// The recorded shape of this report, stated as the difference the two legs make: everything
/// else about the two targets — the cluster, the revision, the launch type, the networking,
/// the shape of the input transformation — is the same.
#[tokio::test]
async fn two_task_targets_differing_only_in_pass_role_deliver_differently() {
    let case = "two ECS targets on one rule, differing only in iam:PassRole";
    let rule = "ecs-passrole-legs";
    let emulator = emulator();
    let subnet = a_subnet(&emulator, case).await;
    let cluster = given_cluster(&emulator, case, "passrole-legs").await;
    let execution_role = given_role_with(
        &emulator,
        case,
        "ecs-task-execution",
        TASKS_SERVICE_PRINCIPAL,
        &json!([]),
    )
    .await;
    let revision = given_revision(&emulator, case, "ebecs-legs", Some(&execution_role)).await;
    // The two roles: the same trust, the same ecs:RunTask grant, and ONE difference.
    let control_role = given_role_with(
        &emulator,
        case,
        "events-with-passrole",
        SERVICE_PRINCIPAL,
        &json!([allow(RUN_TASK_ACTION, "*"), allow(PASS_ROLE_ACTION, "*")]),
    )
    .await;
    let refused_role = given_role_with(
        &emulator,
        case,
        "events-without-passrole",
        SERVICE_PRINCIPAL,
        &json!([allow(RUN_TASK_ACTION, "*")]),
    )
    .await;
    let (control_dlq, control_dlq_arn) = given_queue(&emulator, "legs-control-dlq").await;
    let (refused_dlq, refused_dlq_arn) = given_queue(&emulator, "legs-refused-dlq").await;

    given_rule(
        &emulator,
        rule,
        json!([
            {
                "Id": "control",
                "Arn": cluster,
                "RoleArn": control_role,
                "EcsParameters": fargate_parameters(&revision, &subnet),
                "InputTransformer": leg_transformer("control"),
                "DeadLetterConfig": { "Arn": control_dlq_arn },
            },
            {
                "Id": "refused",
                "Arn": cluster,
                "RoleArn": refused_role,
                "EcsParameters": fargate_parameters(&revision, &subnet),
                "InputTransformer": leg_transformer("refused"),
                "DeadLetterConfig": { "Arn": refused_dlq_arn },
            },
        ]),
    )
    .await;

    publish(&emulator).await;

    // The refused leg's dead letter is what says both deliveries have been decided.
    let parked = dead_lettered(&emulator, case, &refused_dlq).await;
    assert_eq!(
        parked.len(),
        1,
        "case {case:?}: expected exactly one dead letter on the refused target's queue, got \
         {parked:?}"
    );
    let message = &parked[0];
    for (name, expected) in [
        ("ERROR_CODE", NO_PERMISSIONS.to_string()),
        ("RULE_ARN", rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, rule)),
        ("TARGET_ARN", cluster.clone()),
        (
            "ERROR_MESSAGE",
            no_identity_policy_sentence(
                "events-without-passrole",
                rule,
                "refused",
                PASS_ROLE_ACTION,
                &execution_role,
            ),
        ),
    ] {
        assert_eq!(
            attribute(message, name).as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the dead letter's {name} to be {expected:?}, got {message}"
        );
    }
    let body: Value = serde_json::from_str(message["Body"].as_str().unwrap_or_default())
        .unwrap_or_else(|error| panic!("case {case:?}: the dead-lettered body is not JSON \
             ({error}): {message}"));
    assert_eq!(
        body,
        leg_rendering("refused"),
        "case {case:?}: expected the dead-lettered body to be the transformed input that target \
         would have been invoked with, got {body}"
    );

    // Exactly one task, and it is the CONTROL target's run: its overrides are that target's
    // own rendering.
    let launched = launched_until(&emulator, case, &cluster, rule, 1).await;
    assert_eq!(
        launched.len(),
        1,
        "case {case:?}: expected exactly one task on the cluster — the control target's — got \
         {launched:?}"
    );
    let tasks = described(&emulator, case, &cluster, &launched).await;
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeTasks reported no task for {launched:?}"));
    assert!(
        carries_environment(task, "LEG", "control"),
        "case {case:?}: expected the launched task to run under the control target's rendering, \
         got {task}"
    );
    assert!(
        carries_environment(task, "ORDER", "A-1"),
        "case {case:?}: expected the launched task's overrides to carry the value the target's \
         InputTransformer selected, got {task}"
    );

    // And the control target dead-letters nothing.
    let control_parked = drain(&emulator, &control_dlq).await;
    assert!(
        control_parked.is_empty(),
        "case {case:?}: expected the control target's dead-letter queue to stay empty, got \
         {control_parked:?}"
    );

    // What EventBridge recorded of the two handovers: one invoked, one refused for want of a
    // permission — and nothing attempted twice.
    let recorded = invocations(&emulator);
    assert_eq!(
        recorded.len(),
        2,
        "case {case:?}: expected one record per target, got {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|(_, _, outcome)| *outcome == InvocationOutcome::Invoked),
        "case {case:?}: expected the control target's handover to be recorded as invoked, got \
         {recorded:?}"
    );
    assert!(
        recorded.iter().any(|(_, _, outcome)| matches!(
            outcome,
            InvocationOutcome::Failed { code, .. } if code == NO_PERMISSIONS
        )),
        "case {case:?}: expected the refused target's handover to be recorded as failed with \
         {NO_PERMISSIONS}, got {recorded:?}"
    );
}

/// One row of the two tables below: what the target's role is granted, whether the revision
/// declares an execution role for the run to hand over, and whether the delivery runs a task.
struct Row {
    case: &'static str,
    /// The statements the role's inline document states, built from the ARNs of the scene.
    statements: fn(&Scene<'_>) -> Value,
    /// Whether the registered revision declares an execution role — a run that hands no role
    /// over performs `iam:PassRole` on nothing.
    declares_execution_role: bool,
    /// Whether the delivery runs a task on the cluster. A delivery that does not is
    /// dead-lettered under `NO_PERMISSIONS`.
    runs: bool,
}

/// RULE (the `iam:PassRole` half): a run is made only when the documents in force on the
/// target's role permit handing Amazon ECS every role that run gives it — the task role its
/// containers assume and the execution role its agent acts as, each taken from the run's
/// overrides where it names one and from the registered revision otherwise — under the
/// `iam:PassedToService` key naming the tasks service. Over a table whose rows differ along
/// the resource the grant is scoped to, the condition key it is written under, whether a deny
/// stands beside it, and whether the run hands any role over at all.
#[tokio::test]
async fn a_task_target_runs_only_when_its_role_may_pass_the_runs_roles() {
    let rows: [Row; 7] = [
        Row {
            case: "iam:PassRole on every resource beside the run grant",
            statements: |_| json!([allow(RUN_TASK_ACTION, "*"), allow(PASS_ROLE_ACTION, "*")]),
            declares_execution_role: true,
            runs: true,
        },
        Row {
            case: "the run grant alone, with no iam:PassRole at all",
            statements: |_| json!([allow(RUN_TASK_ACTION, "*")]),
            declares_execution_role: true,
            runs: false,
        },
        Row {
            case: "iam:PassRole scoped to the execution role the revision declares",
            statements: |scene| {
                json!([
                    allow(RUN_TASK_ACTION, "*"),
                    allow(PASS_ROLE_ACTION, scene.execution_role),
                ])
            },
            declares_execution_role: true,
            runs: true,
        },
        Row {
            case: "iam:PassRole scoped to another role of the account",
            statements: |scene| {
                json!([
                    allow(RUN_TASK_ACTION, "*"),
                    allow(PASS_ROLE_ACTION, scene.unrelated_role),
                ])
            },
            declares_execution_role: true,
            runs: false,
        },
        Row {
            case: "iam:PassRole for roles handed to the tasks service",
            statements: |_| {
                json!([
                    allow(RUN_TASK_ACTION, "*"),
                    allow_if(
                        PASS_ROLE_ACTION,
                        "*",
                        PASSED_TO_SERVICE_KEY,
                        TASKS_SERVICE_PRINCIPAL
                    ),
                ])
            },
            declares_execution_role: true,
            runs: true,
        },
        Row {
            case: "iam:PassRole for roles handed to another service",
            statements: |_| {
                json!([
                    allow(RUN_TASK_ACTION, "*"),
                    allow_if(
                        PASS_ROLE_ACTION,
                        "*",
                        PASSED_TO_SERVICE_KEY,
                        OTHER_SERVICE_PRINCIPAL
                    ),
                ])
            },
            declares_execution_role: true,
            runs: false,
        },
        Row {
            // A revision declaring no execution role hands no role over, so the run performs
            // iam:PassRole on nothing and the grant it never needed is not asked for.
            case: "a revision declaring no role to hand over, and no iam:PassRole granted",
            statements: |_| json!([allow(RUN_TASK_ACTION, "*")]),
            declares_execution_role: false,
            runs: true,
        },
    ];

    for row in rows {
        assert_row(&row).await;
    }
}

/// RULE (the `ecs:RunTask` half): the run itself is the action `ecs:RunTask` performed ON the
/// TASK DEFINITION the target's parameters name — the resource type Amazon ECS publishes for
/// that action — with the cluster the task is placed in carried as the `ecs:cluster` condition
/// key. Over a table whose rows differ along the resource the grant is scoped to and the
/// cluster its condition names.
#[tokio::test]
async fn a_task_target_runs_only_when_its_role_may_run_that_task_definition() {
    let rows: [Row; 5] = [
        Row {
            case: "the run grant on every resource",
            statements: |_| json!([allow(RUN_TASK_ACTION, "*"), allow(PASS_ROLE_ACTION, "*")]),
            declares_execution_role: true,
            runs: true,
        },
        Row {
            case: "the hand-over grant alone, with no ecs:RunTask at all",
            statements: |_| json!([allow(PASS_ROLE_ACTION, "*")]),
            declares_execution_role: true,
            runs: false,
        },
        Row {
            case: "ecs:RunTask scoped to the task definition the parameters name",
            statements: |scene| {
                json!([
                    allow(RUN_TASK_ACTION, scene.task_definition),
                    allow(PASS_ROLE_ACTION, "*"),
                ])
            },
            declares_execution_role: true,
            runs: true,
        },
        Row {
            case: "ecs:RunTask scoped to the cluster rather than to the task definition",
            statements: |scene| {
                json!([
                    allow(RUN_TASK_ACTION, scene.cluster),
                    allow(PASS_ROLE_ACTION, "*"),
                ])
            },
            declares_execution_role: true,
            runs: false,
        },
        Row {
            case: "ecs:RunTask conditioned on another cluster than the one the run places into",
            statements: |scene| {
                json!([
                    allow_if(
                        RUN_TASK_ACTION,
                        "*",
                        CLUSTER_CONDITION_KEY,
                        &format!("{}-elsewhere", scene.cluster)
                    ),
                    allow(PASS_ROLE_ACTION, "*"),
                ])
            },
            declares_execution_role: true,
            runs: false,
        },
    ];

    for row in rows {
        assert_row(&row).await;
    }
}

/// RULE (the condition key the run DECLARES): a grant scoped by `ecs:cluster` to the cluster
/// the run places into authorizes that run, which is what makes the key the run declares
/// decidable rather than merely absent.
#[tokio::test]
async fn a_run_grant_conditioned_on_the_target_cluster_authorizes_the_run() {
    assert_row(&Row {
        case: "ecs:RunTask conditioned on the cluster the run places into",
        statements: |scene| {
            json!([
                allow_if(RUN_TASK_ACTION, "*", CLUSTER_CONDITION_KEY, scene.cluster),
                allow(PASS_ROLE_ACTION, "*"),
            ])
        },
        declares_execution_role: true,
        runs: true,
    })
    .await;
}

/// RULE (deny precedence): an explicit deny of one of the two actions refuses the delivery
/// however broadly the same document grants it.
#[tokio::test]
async fn an_explicit_deny_of_the_hand_over_refuses_the_run() {
    assert_row(&Row {
        case: "a deny of iam:PassRole on the execution role beside an allow of everything",
        statements: |scene| {
            json!([
                allow(RUN_TASK_ACTION, "*"),
                allow(PASS_ROLE_ACTION, "*"),
                deny(PASS_ROLE_ACTION, scene.execution_role),
            ])
        },
        declares_execution_role: true,
        runs: false,
    })
    .await;
}

/// One row, driven end to end: a rule with ONE task target whose role carries the row's
/// document, and the two observations that decide it — the tasks the run placed, and what the
/// target's dead-letter queue holds.
async fn assert_row(row: &Row) {
    let case = row.case;
    let rule = "ecs-role-gate";
    let emulator = emulator();
    let subnet = a_subnet(&emulator, case).await;
    let cluster = given_cluster(&emulator, case, "role-gate").await;
    let execution_role = given_role_with(
        &emulator,
        case,
        "ecs-task-execution",
        TASKS_SERVICE_PRINCIPAL,
        &json!([]),
    )
    .await;
    let unrelated_role = given_role_with(
        &emulator,
        case,
        "ecs-task-execution-other",
        TASKS_SERVICE_PRINCIPAL,
        &json!([]),
    )
    .await;
    let revision = given_revision(
        &emulator,
        case,
        "ebecs-role-gate",
        row.declares_execution_role.then_some(execution_role.as_str()),
    )
    .await;
    let scene = Scene {
        task_definition: &revision,
        cluster: &cluster,
        execution_role: &execution_role,
        unrelated_role: &unrelated_role,
    };
    let role = given_role_with(
        &emulator,
        case,
        "events-run-task",
        SERVICE_PRINCIPAL,
        &(row.statements)(&scene),
    )
    .await;
    let (dlq, dlq_arn) = given_queue(&emulator, "role-gate-dlq").await;
    given_rule(
        &emulator,
        rule,
        json!([{
            "Id": "the-task",
            "Arn": cluster,
            "RoleArn": role,
            "EcsParameters": fargate_parameters(&revision, &subnet),
            "InputTransformer": leg_transformer("row"),
            "DeadLetterConfig": { "Arn": dlq_arn },
        }]),
    )
    .await;

    publish(&emulator).await;

    if row.runs {
        let launched = launched_until(&emulator, case, &cluster, rule, 1).await;
        assert_eq!(
            launched.len(),
            1,
            "case {case:?}: expected the delivery to run one task, got {launched:?}"
        );
        let parked = drain(&emulator, &dlq).await;
        assert!(
            parked.is_empty(),
            "case {case:?}: expected a delivery that ran its task to dead-letter nothing, got \
             {parked:?}"
        );
        return;
    }

    let parked = dead_lettered(&emulator, case, &dlq).await;
    assert_eq!(
        parked.len(),
        1,
        "case {case:?}: expected exactly one dead letter, got {parked:?}"
    );
    let message = &parked[0];
    assert_eq!(
        attribute(message, "ERROR_CODE").as_deref(),
        Some(NO_PERMISSIONS),
        "case {case:?}: expected the dead letter's ERROR_CODE {NO_PERMISSIONS:?}, got {message}"
    );
    assert_eq!(
        attribute(message, "RULE_ARN").as_deref(),
        Some(rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, rule).as_str()),
        "case {case:?}: expected the dead letter's RULE_ARN to name this rule, got {message}"
    );
    assert_eq!(
        attribute(message, "TARGET_ARN").as_deref(),
        Some(cluster.as_str()),
        "case {case:?}: expected the dead letter's TARGET_ARN to name this target, got {message}"
    );
    let launched = launched_by(&emulator, case, &cluster, rule).await;
    assert!(
        launched.is_empty(),
        "case {case:?}: expected a delivery the role's documents barred to run no task, got \
         {launched:?}"
    );
}
