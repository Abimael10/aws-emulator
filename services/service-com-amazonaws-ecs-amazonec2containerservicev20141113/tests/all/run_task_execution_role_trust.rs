// SPDX-License-Identifier: CC0-1.0

//! The role a Fargate `RunTask` names has to be a role Amazon ECS can ASSUME, and the call is
//! what is refused when it is not.
//!
//! RULE: Amazon ECS reaches the roles a launch resolves by assuming them, which a role admits
//! only to the principals its own trust policy names. A Fargate placement is provisioned by
//! Amazon ECS ITSELF while the call that asked for it is answered — no agent of the caller's is
//! there to report a failure later — so the assume is taken before anything is placed and a
//! role this account's IAM holds whose trust policy does not let `ecs-tasks.amazonaws.com`
//! perform `sts:AssumeRole` refuses the CALL with the modelled `ClientException` (HTTP 400),
//! naming the role that was passed, leaving no task behind. A launch onto the cluster's own
//! registered capacity is assumed for after it is placed, by the agent on the instance that
//! runs it, so its answer is that task's STOP instead — which `execution_role_trust.rs` pins.
//!
//! Only what IAM ANSWERED is refused: a placement naming no role, one naming a role this
//! account's IAM holds no record for, and one whose trust document admits the principal are
//! each placed and run.
//!
//! The rows differ along what the rule generalises over: WHICH role the placement names (the
//! revision's execution role, its task role, both, the call's own `overrides` execution role,
//! which stands in place of the revision's), which principal the trust document names (this
//! service, another service, several at once, an IAM principal rather than a service), the
//! effect it names (`Allow`, `Deny`), the action it names (`sts:AssumeRole`, a wildcard
//! covering it, another STS action that does not), the case the principal and the action are
//! spelled in, whether IAM holds the role at all — and the second test, which drives the same
//! rule through a Fargate CAPACITY PROVIDER rather than through the `FARGATE` launch type.
//!
//! IAM holds every role through its OWN API, and nothing here reaches into either state
//! struct: what was placed is read back through `ListTasks`, and the refusal's sentence is
//! spelled out here rather than imported, so a wrong wording in the implementation cannot make
//! a test pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, create_cluster, emulator_with_ecr_and_iam, iam, list, ok,
    refusal, string, ACCOUNT,
};

/// The one container every revision below carries.
const CONTAINER: &str = "app";

/// The image its container starts from.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The service principal a task's roles have to admit.
const ECS_TASKS: &str = "ecs-tasks.amazonaws.com";

/// The error the model declares for a client fault of this operation
/// (`smithy_query(query="operation",operation="RunTask")`: `ClientException`, httpError 400).
const CLIENT_EXCEPTION: &str = "ClientException";

/// Where a row puts a role: the member of the revision that names it, or the call's own
/// override, which stands in place of the revision's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// `TaskDefinition$executionRoleArn` — the role Amazon ECS itself acts as.
    Execution,
    /// `TaskDefinition$taskRoleArn` — the role the task's containers are given.
    Task,
    /// `RunTask$overrides.executionRoleArn` — "the task execution role override for the task".
    OverrideExecution,
}

impl Slot {
    /// A suffix for the IAM role name, so each slot of each row holds its own role.
    fn suffix(self) -> &'static str {
        match self {
            Self::Execution => "exec",
            Self::Task => "task",
            Self::OverrideExecution => "override",
        }
    }
}

/// What a row says about one of the roles it names.
enum Role {
    /// This account's IAM holds the role, carrying this trust document.
    Held(String),
    /// The ARN names a role of this account that IAM holds nothing under.
    AbsentFromIam,
}

/// What a row expects of the call it makes.
enum Outcome {
    /// Every role the placement named was assumable (or there was nothing to ask about), so
    /// the call placed a task.
    Placed,
    /// IAM answered that the role in this slot withholds the assume, so the call is refused
    /// naming THAT role and nothing is placed.
    Refused(Slot),
}

/// One row: the roles the placement names, where each sits, and what the call must do.
struct Row {
    case: &'static str,
    roles: Vec<(Slot, Role)>,
    expected: Outcome,
}

/// A trust policy with one statement: `effect` on `actions` for the SERVICE principals
/// `principals`.
fn service_trust(effect: &str, principals: &[&str], actions: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principals },
            "Action": actions,
        }],
    })
    .to_string()
}

/// A trust policy naming an IAM principal rather than a service: a document that admits
/// somebody, but not the principal Amazon ECS assumes a task's roles as.
fn iam_trust(principal_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": principal_arn },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the IAM role `name` carrying `document` as its trust policy — through IAM's own API
/// — and answer the role's ARN.
async fn role(router: &MultiServiceRouter, name: &str, document: &str, case: &str) -> String {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", document.to_string()),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The ARN of a role of this account, whether or not IAM holds it.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Register one FARGATE/`awsvpc` revision of `family` whose `executionRoleArn` and
/// `taskRoleArn` are the roles `slots` puts under those members (each absent when the row
/// names none).
async fn register(router: &MultiServiceRouter, family: &str, slots: &[(Slot, String)], case: &str) {
    let mut definition = json!({
        "family": family,
        "networkMode": "awsvpc",
        "cpu": "256",
        "memory": "512",
        "requiresCompatibilities": ["FARGATE"],
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
        }],
    });
    for (slot, arn) in slots {
        match slot {
            Slot::Execution => definition["executionRoleArn"] = json!(arn),
            Slot::Task => definition["taskRoleArn"] = json!(arn),
            // An override is the call's, not the revision's.
            Slot::OverrideExecution => {}
        }
    }
    ok(router, "RegisterTaskDefinition", definition, case).await;
}

/// The `RunTask` request a row makes: a Fargate placement of `family` into `cluster`, carrying
/// the `overrides` the row's slots ask for.
fn run_request(cluster: &str, family: &str, slots: &[(Slot, String)]) -> Value {
    let mut request = json!({
        "cluster": cluster,
        "taskDefinition": family,
        "launchType": "FARGATE",
        "networkConfiguration": awsvpc_network_configuration(),
        "count": 1,
    });
    for (slot, arn) in slots {
        if matches!(slot, Slot::OverrideExecution) {
            request["overrides"] = json!({ "executionRoleArn": arn });
        }
    }
    request
}

/// The sentence Amazon ECS refuses a placement whose role it could not assume with: fixed
/// text carrying the role the placement resolved.
fn unable_to_assume(role_arn: &str) -> String {
    format!(
        "ECS was unable to assume the role '{role_arn}' that was provided for this task. Please \
         verify that the role being passed has the proper trust relationship and permissions \
         and that your IAM user has permissions to pass this role."
    )
}

/// The rows: one per thing the rule generalises over.
fn rows() -> Vec<Row> {
    let admits = service_trust("Allow", &[ECS_TASKS], &["sts:AssumeRole"]);
    let admits_ec2_only = service_trust("Allow", &["ec2.amazonaws.com"], &["sts:AssumeRole"]);
    vec![
        Row {
            case: "an execution role whose trust policy admits this service",
            roles: vec![(Slot::Execution, Role::Held(admits.clone()))],
            expected: Outcome::Placed,
        },
        Row {
            // The reported misconfiguration: a role that trusts Amazon EC2 and nothing else.
            case: "an execution role admitting another service only",
            roles: vec![(Slot::Execution, Role::Held(admits_ec2_only.clone()))],
            expected: Outcome::Refused(Slot::Execution),
        },
        Row {
            case: "an execution role admitting several services, this one among them",
            roles: vec![(
                Slot::Execution,
                Role::Held(service_trust(
                    "Allow",
                    &["ec2.amazonaws.com", ECS_TASKS],
                    &["sts:AssumeRole"],
                )),
            )],
            expected: Outcome::Placed,
        },
        Row {
            case: "an execution role whose trust policy DENIES this service the assume",
            roles: vec![(
                Slot::Execution,
                Role::Held(service_trust("Deny", &[ECS_TASKS], &["sts:AssumeRole"])),
            )],
            expected: Outcome::Refused(Slot::Execution),
        },
        Row {
            case: "a wildcard action covering the assume",
            roles: vec![(
                Slot::Execution,
                Role::Held(service_trust("Allow", &[ECS_TASKS], &["sts:*"])),
            )],
            expected: Outcome::Placed,
        },
        Row {
            case: "another STS action, which is not the assume",
            roles: vec![(
                Slot::Execution,
                Role::Held(service_trust("Allow", &[ECS_TASKS], &["sts:TagSession"])),
            )],
            expected: Outcome::Refused(Slot::Execution),
        },
        Row {
            // IAM compares the principal and the action case-insensitively, so a document
            // spelled in another case admits exactly the same assume.
            case: "the principal and the action spelled in another case",
            roles: vec![(
                Slot::Execution,
                Role::Held(service_trust(
                    "Allow",
                    &["ECS-Tasks.AmazonAWS.com"],
                    &["STS:assumerole"],
                )),
            )],
            expected: Outcome::Placed,
        },
        Row {
            case: "a trust policy admitting an IAM principal rather than this service",
            roles: vec![(
                Slot::Execution,
                Role::Held(iam_trust(&format!("arn:aws:iam::{ACCOUNT}:root"))),
            )],
            expected: Outcome::Refused(Slot::Execution),
        },
        Row {
            // IAM answered nothing about a role it holds no record for, so nothing is refused:
            // such a role may have been provisioned out of band.
            case: "an execution role this account's IAM holds no record for",
            roles: vec![(Slot::Execution, Role::AbsentFromIam)],
            expected: Outcome::Placed,
        },
        Row {
            case: "a TASK role admitting another service only",
            roles: vec![(Slot::Task, Role::Held(admits_ec2_only.clone()))],
            expected: Outcome::Refused(Slot::Task),
        },
        Row {
            case: "a task role whose trust policy admits this service",
            roles: vec![(Slot::Task, Role::Held(admits.clone()))],
            expected: Outcome::Placed,
        },
        Row {
            // Amazon ECS acts as the execution role before the containers are given the task
            // role, so a placement that names two unassumable roles is refused naming the
            // first of them.
            case: "both roles admitting another service only",
            roles: vec![
                (Slot::Execution, Role::Held(admits_ec2_only.clone())),
                (Slot::Task, Role::Held(admits_ec2_only.clone())),
            ],
            expected: Outcome::Refused(Slot::Execution),
        },
        Row {
            case: "an assumable execution role beside a task role admitting another service",
            roles: vec![
                (Slot::Execution, Role::Held(admits.clone())),
                (Slot::Task, Role::Held(admits_ec2_only.clone())),
            ],
            expected: Outcome::Refused(Slot::Task),
        },
        Row {
            // The override stands in place of the revision's execution role, so it is the
            // override that is put to IAM.
            case: "an overridden execution role admitting another service only",
            roles: vec![
                (Slot::Execution, Role::Held(admits.clone())),
                (Slot::OverrideExecution, Role::Held(admits_ec2_only)),
            ],
            expected: Outcome::Refused(Slot::OverrideExecution),
        },
        Row {
            case: "an assumable override standing in for an unassumable revision role",
            roles: vec![
                (
                    Slot::Execution,
                    Role::Held(service_trust(
                        "Allow",
                        &["ec2.amazonaws.com"],
                        &["sts:AssumeRole"],
                    )),
                ),
                (Slot::OverrideExecution, Role::Held(admits)),
            ],
            expected: Outcome::Placed,
        },
        Row {
            case: "a placement naming no role at all",
            roles: Vec::new(),
            expected: Outcome::Placed,
        },
    ]
}

/// Create the roles one row names and answer the ARN each of its slots resolved to.
async fn roles_of(
    router: &MultiServiceRouter,
    index: usize,
    row_roles: &[(Slot, Role)],
    case: &str,
) -> Vec<(Slot, String)> {
    let mut resolved = Vec::new();
    for (slot, role_case) in row_roles {
        let name = format!("trust-{index}-{}", slot.suffix());
        let arn = match role_case {
            Role::Held(document) => role(router, &name, document, case).await,
            Role::AbsentFromIam => role_arn(&name),
        };
        resolved.push((*slot, arn));
    }
    resolved
}

/// The ARN a row's expected refusal names: the role in the slot the row points at.
fn blamed(slots: &[(Slot, String)], slot: Slot) -> String {
    slots
        .iter()
        .find(|(candidate, _)| *candidate == slot)
        .map(|(_, arn)| arn.clone())
        .unwrap_or_else(|| panic!("the row names a role in {slot:?}"))
}

#[tokio::test]
async fn a_fargate_run_task_is_refused_when_ecs_cannot_assume_a_role_it_names() {
    let router = emulator_with_ecr_and_iam();
    let cluster = "trustfargate";
    create_cluster(&router, cluster, "the suite's cluster").await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let slots = roles_of(&router, index, &row.roles, case).await;
        let family = format!("trustfamily{index}");
        register(&router, &family, &slots, case).await;
        let request = run_request(cluster, &family, &slots);

        match row.expected {
            Outcome::Placed => {
                let body = ok(&router, "RunTask", request, case).await;
                let tasks = list(&body, "tasks", case);
                assert_eq!(
                    tasks.len(),
                    1,
                    "case {case:?}: the placement must place one task, got {body}"
                );
                assert!(
                    list(&body, "failures", case).is_empty(),
                    "case {case:?}: the placement must report no failures, got {body}"
                );
                // The task Amazon ECS placed is the one its own read reports.
                let task_arn = string(&tasks[0], "taskArn", case);
                let described = ok(
                    &router,
                    "DescribeTasks",
                    json!({ "cluster": cluster, "tasks": [&task_arn] }),
                    case,
                )
                .await;
                assert_eq!(
                    list(&described, "tasks", case).len(),
                    1,
                    "case {case:?}: {task_arn} must be a task this cluster holds, got {described}"
                );
            }
            Outcome::Refused(slot) => {
                let before = ok(
                    &router,
                    "ListTasks",
                    json!({ "cluster": cluster }),
                    case,
                )
                .await;
                let held = list(&before, "taskArns", case).len();

                let (code, message) = refusal(&router, "RunTask", request, case).await;
                assert_eq!(
                    code, CLIENT_EXCEPTION,
                    "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code:?} with {message:?}"
                );
                assert_eq!(
                    message,
                    unable_to_assume(&blamed(&slots, slot)),
                    "case {case:?}: the refusal must name the role that was passed"
                );

                // And nothing was placed: a refused call leaves no task behind.
                let after = ok(
                    &router,
                    "ListTasks",
                    json!({ "cluster": cluster }),
                    case,
                )
                .await;
                assert_eq!(
                    list(&after, "taskArns", case).len(),
                    held,
                    "case {case:?}: a refused RunTask must place no task, got {after}"
                );
            }
        }
    }
}

/// The same rule where the placement names no launch type at all and is routed onto Fargate by
/// a capacity provider instead: what decides the assume is WHERE the task runs, not which
/// member said so.
#[tokio::test]
async fn a_placement_routed_onto_fargate_by_a_capacity_provider_is_held_to_the_same_rule() {
    let router = emulator_with_ecr_and_iam();
    let case = "a Fargate capacity provider";
    let cluster = "trustprovider";
    create_cluster(&router, cluster, case).await;
    // A cluster places through the providers it HOLDS, so the predefined Fargate provider is
    // associated with it first.
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": cluster,
            "capacityProviders": ["FARGATE"],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;

    let refused_role = role(
        &router,
        "provider-trust-bad",
        &service_trust("Allow", &["ec2.amazonaws.com"], &["sts:AssumeRole"]),
        case,
    )
    .await;
    let assumable = role(
        &router,
        "provider-trust-good",
        &service_trust("Allow", &[ECS_TASKS], &["sts:AssumeRole"]),
        case,
    )
    .await;

    for (row, arn, placed) in [
        ("the role admits another service only", &refused_role, false),
        ("the role admits this service", &assumable, true),
    ] {
        let family = format!("providerfamily{}", usize::from(placed));
        register(
            &router,
            &family,
            &[(Slot::Execution, arn.clone())],
            row,
        )
        .await;
        let request = json!({
            "cluster": cluster,
            "taskDefinition": &family,
            "capacityProviderStrategy": [{ "capacityProvider": "FARGATE", "weight": 1 }],
            "networkConfiguration": awsvpc_network_configuration(),
            "count": 1,
        });
        if placed {
            let body = ok(&router, "RunTask", request, row).await;
            assert_eq!(
                list(&body, "tasks", row).len(),
                1,
                "case {row:?}: the placement must place one task, got {body}"
            );
        } else {
            let (code, message) = refusal(&router, "RunTask", request, row).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (CLIENT_EXCEPTION, unable_to_assume(arn).as_str()),
                "case {row:?}: a Fargate provider placement must be refused naming the role"
            );
            let listed = ok(&router, "ListTasks", json!({ "cluster": cluster }), row).await;
            assert!(
                list(&listed, "taskArns", row).is_empty(),
                "case {row:?}: a refused RunTask must place no task, got {listed}"
            );
        }
    }
}
