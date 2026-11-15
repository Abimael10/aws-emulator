// SPDX-License-Identifier: CC0-1.0

//! The container-instance capabilities a registered revision requires, and the role
//! references a registration will not store — both driven end to end over the shared wire
//! harness in [`crate::ecs_wire`].
//!
//! Two rules, read back through BOTH projections of the stored revision
//! (`RegisterTaskDefinition` and `DescribeTaskDefinition`), so a member rendered off the
//! request rather than off the record shows up as a difference between them:
//!
//! * `TaskDefinition$requiresAttributes` ("The container instance attributes required by
//!   your task") is DERIVED from the features the revision declares, one container-agent
//!   capability per feature, reported in the order the derivation takes them — each
//!   container's own features, container by container, then the revision's own — and
//!   deduplicated. The features tabled here are the ones an IMAGE PULL and a LOG
//!   CONFIGURATION need: an image a registry serves only to an authenticated principal
//!   needs the ECR-authentication capability, such an image together with an
//!   `executionRoleArn` needs the capability for pulling AS that role, a
//!   `logConfiguration` needs the Docker remote-API version that carries one, and its
//!   `options` the version that carries driver options. The other features of the same
//!   list are tabled in [`crate::task_definition_rendering`].
//! * a role member takes "the short name or full Amazon Resource Name (ARN) of the IAM
//!   role" (`RegisterTaskDefinition$taskRoleArn`), so a value that names no IAM role of
//!   this account is refused with the modelled `ClientException` and stores nothing — the
//!   family's revision counter does not move for a registration Amazon ECS refused.
//!
//! The capability names and the refusal's sentence are spelled out here rather than
//! imported, so a wrong name or wording in the implementation cannot make a test pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{ok, refusal, service, ACCOUNT, SIGNED_REGION};
use crate::task_lifecycle::container;

/// The Amazon ECS container-agent capability names this suite asserts on, as the agent
/// publishes them (Amazon ECS Developer Guide, the container-instance attributes a
/// registered agent reports).
const ECR_AUTH: &str = "com.amazonaws.ecs.capability.ecr-auth";
const EXECUTION_ROLE_ECR_PULL: &str = "ecs.capability.execution-role-ecr-pull";
const EXECUTION_ROLE_AWSLOGS: &str = "ecs.capability.execution-role-awslogs";
const DOCKER_1_18: &str = "com.amazonaws.ecs.capability.docker-remote-api.1.18";
const DOCKER_1_19: &str = "com.amazonaws.ecs.capability.docker-remote-api.1.19";
const TASK_ENI: &str = "ecs.capability.task-eni";
const TASK_IAM_ROLE: &str = "com.amazonaws.ecs.capability.task-iam-role";
const DRIVER_AWSLOGS: &str = "com.amazonaws.ecs.capability.logging-driver.awslogs";
const DRIVER_FLUENTD: &str = "com.amazonaws.ecs.capability.logging-driver.fluentd";
const DRIVER_SPLUNK: &str = "com.amazonaws.ecs.capability.logging-driver.splunk";

/// How Amazon ECS words a role member that names no IAM role.
const ROLE_NOT_VALID: &str = "Role is not valid";

/// An image in a repository of THIS account's Amazon ECR registry, whose registry URI is
/// `<account>.dkr.ecr.<region>.amazonaws.com` (Amazon ECR User Guide, the registry URI a
/// pull uses) — a registry that serves a pull only to an authenticated principal.
fn ecr_image() -> String {
    format!("{ACCOUNT}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/app:1")
}

/// The same form naming ANOTHER account's registry: still Amazon ECR, so still a pull that
/// needs a principal.
fn other_account_ecr_image() -> String {
    format!("210987654321.dkr.ecr.{SIGNED_REGION}.amazonaws.com/app:1")
}

/// The address Amazon ECR announces its registry at on THIS machine
/// (`Repository$repositoryUri`), which is the other form a reference to this account's
/// registry takes.
const ANNOUNCED_REGISTRY_IMAGE: &str = "127.0.0.1:9297/app:1";

/// An image in Amazon ECR PUBLIC, which serves anyone.
const PUBLIC_IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// A reference that names no registry at all, which a container runtime resolves against
/// Docker Hub — the other form of an image no principal is needed for.
const HUB_IMAGE: &str = "nginx:1.27";

/// The ARN of a role of this account, as IAM renders one:
/// `arn:aws:iam::<account>:role<path><name>`.
fn role_arn(path: &str, name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role{path}{name}")
}

/// The `name` of every attribute a projection's `requiresAttributes` carries, in the order
/// the projection reports them, and the empty list when the member is ABSENT.
fn required_attributes(definition: &Value, case: &str) -> Vec<String> {
    definition["requiresAttributes"]
        .as_array()
        .map(|attributes| {
            attributes
                .iter()
                .map(|attribute| {
                    attribute["name"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: expected a named attribute, got {attribute}")
                        })
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Register `request` under `family` and read the revision back through
/// `DescribeTaskDefinition`, answering both projections of the same stored revision.
async fn register_and_read(
    router: &MultiServiceRouter,
    family: &str,
    mut request: Value,
    case: &str,
) -> Vec<(&'static str, Value)> {
    request["family"] = json!(family);
    let registered = ok(router, "RegisterTaskDefinition", request, case).await;
    let described = ok(
        router,
        "DescribeTaskDefinition",
        json!({ "taskDefinition": family }),
        case,
    )
    .await;
    vec![
        (
            "RegisterTaskDefinition",
            registered["taskDefinition"].clone(),
        ),
        ("DescribeTaskDefinition", described["taskDefinition"].clone()),
    ]
}

/// One row of the image-pull table.
struct PullRow {
    case: &'static str,
    /// The images the revision's containers run, in the order it declares them.
    images: Vec<String>,
    /// Whether the registration names an `executionRoleArn`.
    execution_role: bool,
    /// The capability names the revision must require, in order.
    expected: Vec<&'static str>,
}

#[tokio::test]
async fn image_pull_capabilities_follow_the_registry_and_the_execution_role() {
    let router = service();

    // The rows differ along what the rule generalises over: the REGISTRY the reference
    // resolves to (a repository of this account's Amazon ECR registry, one of another
    // account's, the address that registry is announced at here, Amazon ECR Public, a
    // bare reference a runtime resolves against Docker Hub), the NUMBER of containers
    // that run such an image, and whether an execution role is named. Every row stays in
    // the default `bridge` network mode, so the list is the pull capabilities and nothing
    // else.
    let rows: Vec<PullRow> = vec![
        PullRow {
            case: "an ECR image of this account with no execution role",
            images: vec![ecr_image()],
            execution_role: false,
            expected: vec![ECR_AUTH],
        },
        PullRow {
            case: "an ECR image of this account with an execution role",
            images: vec![ecr_image()],
            execution_role: true,
            expected: vec![ECR_AUTH, EXECUTION_ROLE_ECR_PULL],
        },
        PullRow {
            case: "an ECR image of another account with an execution role",
            images: vec![other_account_ecr_image()],
            execution_role: true,
            expected: vec![ECR_AUTH, EXECUTION_ROLE_ECR_PULL],
        },
        PullRow {
            case: "the address Amazon ECR announces its registry at here",
            images: vec![ANNOUNCED_REGISTRY_IMAGE.to_string()],
            execution_role: false,
            expected: vec![ECR_AUTH],
        },
        PullRow {
            case: "an Amazon ECR Public image, which serves anyone",
            images: vec![PUBLIC_IMAGE.to_string()],
            execution_role: true,
            expected: vec![],
        },
        PullRow {
            case: "a Docker Hub reference, which names no registry at all",
            images: vec![HUB_IMAGE.to_string()],
            execution_role: true,
            expected: vec![],
        },
        PullRow {
            case: "two containers, the second of them an ECR image",
            images: vec![PUBLIC_IMAGE.to_string(), ecr_image()],
            execution_role: true,
            expected: vec![ECR_AUTH, EXECUTION_ROLE_ECR_PULL],
        },
        PullRow {
            case: "two ECR images require each capability once",
            images: vec![ecr_image(), other_account_ecr_image()],
            execution_role: true,
            expected: vec![ECR_AUTH, EXECUTION_ROLE_ECR_PULL],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let containers: Vec<Value> = row
            .images
            .iter()
            .enumerate()
            .map(|(slot, image)| container(&format!("app{slot}"), image))
            .collect();
        let mut request = json!({ "containerDefinitions": containers });
        if row.execution_role {
            request["executionRoleArn"] = json!(role_arn("/", "ecsTaskExecutionRole"));
        }
        let family = format!("pullcap{index}");
        for (source, definition) in register_and_read(&router, &family, request, case).await {
            let actual = required_attributes(&definition, case);
            assert_eq!(
                actual, row.expected,
                "case {case:?}: {source} expected requiresAttributes {:?}, got {actual:?} \
                 in {definition}",
                row.expected
            );
            if row.expected.is_empty() {
                // "Absent" and "empty" are different answers: a revision that requires
                // nothing carries no member at all.
                assert!(
                    definition.get("requiresAttributes").is_none(),
                    "case {case:?}: {source} expected requiresAttributes to stay absent, \
                     got {definition}"
                );
            }
        }
    }
}

/// One row of the log-configuration table.
struct LogRow {
    case: &'static str,
    network_mode: &'static str,
    /// Whether the registration names an `executionRoleArn`.
    execution_role: bool,
    /// Whether it names a `taskRoleArn`.
    task_role: bool,
    /// Each container beyond its name, image and memory, and the image it runs.
    containers: Vec<(String, Value)>,
    expected: Vec<&'static str>,
}

#[tokio::test]
async fn a_log_configuration_requires_the_docker_api_versions_its_members_need() {
    let router = service();

    // The rows differ along what the rule generalises over: WHICH driver is configured,
    // whether it is passed `options` (and whether that map is empty), how many containers
    // configure logging, and the features of the same revision that contribute their own
    // capabilities — so the ORDER the list is reported in is read too.
    let rows: Vec<LogRow> = vec![
        LogRow {
            case: "a revision that declares no such feature requires nothing",
            network_mode: "bridge",
            execution_role: true,
            task_role: false,
            containers: vec![(PUBLIC_IMAGE.to_string(), json!({}))],
            expected: vec![],
        },
        LogRow {
            case: "a driver passed no options",
            network_mode: "bridge",
            execution_role: false,
            task_role: false,
            containers: vec![(
                PUBLIC_IMAGE.to_string(),
                json!({ "logConfiguration": { "logDriver": "splunk" } }),
            )],
            expected: vec![DOCKER_1_18, DRIVER_SPLUNK],
        },
        LogRow {
            case: "a driver passed an empty options map asks for no option",
            network_mode: "bridge",
            execution_role: false,
            task_role: false,
            containers: vec![(
                PUBLIC_IMAGE.to_string(),
                json!({ "logConfiguration": { "logDriver": "fluentd", "options": {} } }),
            )],
            expected: vec![DOCKER_1_18, DRIVER_FLUENTD],
        },
        LogRow {
            case: "a driver passed its own options",
            network_mode: "bridge",
            execution_role: false,
            task_role: false,
            containers: vec![(
                PUBLIC_IMAGE.to_string(),
                json!({
                    "logConfiguration": {
                        "logDriver": "fluentd",
                        "options": { "fluentd-address": "127.0.0.1:24224" },
                    },
                }),
            )],
            expected: vec![DOCKER_1_18, DRIVER_FLUENTD, DOCKER_1_19],
        },
        LogRow {
            case: "awslogs with options on a revision that names an execution role",
            network_mode: "bridge",
            execution_role: true,
            task_role: false,
            containers: vec![(
                PUBLIC_IMAGE.to_string(),
                json!({
                    "logConfiguration": {
                        "logDriver": "awslogs",
                        "options": { "awslogs-group": "/ecs/capabilities" },
                    },
                }),
            )],
            expected: vec![
                DOCKER_1_18,
                DRIVER_AWSLOGS,
                DOCKER_1_19,
                EXECUTION_ROLE_AWSLOGS,
            ],
        },
        LogRow {
            case: "two containers configuring logging report each version once",
            network_mode: "bridge",
            execution_role: true,
            task_role: false,
            containers: vec![
                (
                    PUBLIC_IMAGE.to_string(),
                    json!({
                        "logConfiguration": {
                            "logDriver": "awslogs",
                            "options": { "awslogs-group": "/ecs/capabilities" },
                        },
                    }),
                ),
                (
                    PUBLIC_IMAGE.to_string(),
                    json!({
                        "logConfiguration": {
                            "logDriver": "fluentd",
                            "options": { "fluentd-address": "127.0.0.1:24224" },
                        },
                    }),
                ),
            ],
            expected: vec![
                DOCKER_1_18,
                DRIVER_AWSLOGS,
                DOCKER_1_19,
                EXECUTION_ROLE_AWSLOGS,
                DRIVER_FLUENTD,
            ],
        },
        LogRow {
            case: "an awsvpc revision with an ECR image, both roles and awslogs",
            network_mode: "awsvpc",
            execution_role: true,
            task_role: true,
            containers: vec![(
                ecr_image(),
                json!({
                    "logConfiguration": {
                        "logDriver": "awslogs",
                        "options": { "awslogs-stream-prefix": "app" },
                    },
                }),
            )],
            expected: vec![
                DOCKER_1_18,
                DRIVER_AWSLOGS,
                DOCKER_1_19,
                EXECUTION_ROLE_AWSLOGS,
                ECR_AUTH,
                TASK_IAM_ROLE,
                EXECUTION_ROLE_ECR_PULL,
                TASK_ENI,
            ],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let containers: Vec<Value> = row
            .containers
            .iter()
            .enumerate()
            .map(|(slot, (image, declared))| {
                let mut definition = container(&format!("app{slot}"), image);
                for (member, value) in declared.as_object().into_iter().flatten() {
                    definition[member.as_str()] = value.clone();
                }
                definition
            })
            .collect();
        let mut request = json!({
            "networkMode": row.network_mode,
            "containerDefinitions": containers,
        });
        if row.execution_role {
            request["executionRoleArn"] = json!(role_arn("/", "ecsTaskExecutionRole"));
        }
        if row.task_role {
            request["taskRoleArn"] = json!(role_arn("/", "appTaskRole"));
        }
        let family = format!("logcap{index}");
        for (source, definition) in register_and_read(&router, &family, request, case).await {
            let actual = required_attributes(&definition, case);
            assert_eq!(
                actual, row.expected,
                "case {case:?}: {source} expected requiresAttributes {:?}, got {actual:?} \
                 in {definition}",
                row.expected
            );
        }
    }
}

/// The registration body one role-reference row sends, with `member` set to `reference`.
fn role_request(family: &str, member: &str, reference: &str) -> Value {
    let mut request = json!({
        "family": family,
        "containerDefinitions": [container("app", PUBLIC_IMAGE)],
    });
    request[member] = json!(reference);
    request
}

#[tokio::test]
async fn a_role_member_that_names_an_iam_role_is_stored_for_either_member() {
    let router = service();

    // Every FORM the member's doc admits — "the short name or full Amazon Resource Name
    // (ARN) of the IAM role", with "the full role ARN (this is recommended) or ... the
    // role name prefixed with the path" for a role under a path — plus the value that
    // names no role at all, which is no mistake.
    let accepted: [(&str, &str, Option<String>); 5] = [
        ("a bare name", "app-task", Some(role_arn("/", "app-task"))),
        (
            "a name under a path",
            "service-role/app-task",
            Some(role_arn("/", "service-role/app-task")),
        ),
        (
            "a full ARN",
            "arn:aws:iam::000000000000:role/app-task",
            Some(role_arn("/", "app-task")),
        ),
        (
            "an ARN under a path",
            "arn:aws:iam::000000000000:role/service-role/app-task",
            Some(role_arn("/service-role/", "app-task")),
        ),
        ("whitespace names no role", "   ", None),
    ];

    for (member, prefix) in [("taskRoleArn", "taskrole"), ("executionRoleArn", "execrole")] {
        for (index, (case, reference, expected)) in accepted.iter().enumerate() {
            let family = format!("{prefix}ok{index}");
            let request = role_request(&family, member, reference);
            let registered = ok(&router, "RegisterTaskDefinition", request, case).await;
            let described = ok(
                &router,
                "DescribeTaskDefinition",
                json!({ "taskDefinition": family }),
                case,
            )
            .await;
            for (source, definition) in [
                ("RegisterTaskDefinition", &registered["taskDefinition"]),
                ("DescribeTaskDefinition", &described["taskDefinition"]),
            ] {
                let actual = definition[member].as_str();
                assert_eq!(
                    actual,
                    expected.as_deref(),
                    "case {case:?}: {source} expected {member} {expected:?}, got \
                     {actual:?} in {definition}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_role_member_that_names_no_iam_role_is_refused_and_consumes_no_revision() {
    let router = service();
    let long_name = "a".repeat(65);

    // The rows differ along HOW the value fails to name a role of this account: it is an
    // ARN of another service, an ARN of another IAM entity type, an ARN of another
    // account, an ARN with a Region (an IAM ARN is partition-global), an ARN with no role
    // name, or a name IAM could hold no role under — a character outside `[\w+=,.@-]`, or
    // more than the 64 characters a role name may carry.
    let refused: Vec<(&str, String)> = vec![
        (
            "an ARN of another service",
            format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:task-definition/app:1"),
        ),
        (
            "an IAM ARN naming a user",
            format!("arn:aws:iam::{ACCOUNT}:user/developer"),
        ),
        (
            "an IAM ARN naming an instance profile",
            format!("arn:aws:iam::{ACCOUNT}:instance-profile/app"),
        ),
        (
            "a role ARN of another account",
            "arn:aws:iam::210987654321:role/app".to_string(),
        ),
        (
            "an IAM ARN carrying a Region",
            format!("arn:aws:iam:{SIGNED_REGION}:{ACCOUNT}:role/app"),
        ),
        (
            "an ARN with no role name",
            format!("arn:aws:iam::{ACCOUNT}:role/"),
        ),
        ("a value that is not an ARN at all", "arn:aws".to_string()),
        (
            "a name carrying a character IAM does not admit",
            "app task".to_string(),
        ),
        ("a name past the 64 characters IAM holds", long_name.clone()),
    ];

    for (member, prefix) in [("taskRoleArn", "taskrole"), ("executionRoleArn", "execrole")] {
        // One family for every refused row of this member: nothing is stored under it, so
        // the revision the FIRST accepted registration gets is the family's first.
        let family = format!("{prefix}refused");
        for (case, reference) in &refused {
            let request = role_request(&family, member, reference);
            let (code, message) = refusal(&router, "RegisterTaskDefinition", request, case).await;
            assert_eq!(
                code, "ClientException",
                "case {case:?}: {member} expected the modelled client refusal, got \
                 {code:?} with {message:?}"
            );
            assert_eq!(
                message, ROLE_NOT_VALID,
                "case {case:?}: {member} expected the refusal {ROLE_NOT_VALID:?}, got \
                 {message:?}"
            );
        }

        // Every registration above was refused, so the counter never moved: the next one
        // Amazon ECS accepts is revision 1 of the family.
        let accepted = ok(
            &router,
            "RegisterTaskDefinition",
            role_request(&family, member, &role_arn("/", "app-task")),
            "after the refusals",
        )
        .await;
        let revision = accepted["taskDefinition"]["revision"].as_i64();
        assert_eq!(
            revision,
            Some(1),
            "{member}: a refused registration consumes no revision, so the first \
             accepted one is revision 1, got {accepted}"
        );
        let arn = accepted["taskDefinition"]["taskDefinitionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            arn.ends_with(&format!("/{family}:1")),
            "{member}: expected the ARN to name revision 1 of {family}, got {arn:?}"
        );
    }
}

#[tokio::test]
async fn a_daemon_registration_refuses_a_role_reference_the_same_way() {
    let router = service();
    let case = "an IAM ARN naming a user";
    let reference = format!("arn:aws:iam::{ACCOUNT}:user/developer");

    let (code, message) = refusal(
        &router,
        "RegisterDaemonTaskDefinition",
        json!({
            "family": "daemonrolecap",
            "taskRoleArn": reference,
            "containerDefinitions": [container("agent", PUBLIC_IMAGE)],
        }),
        case,
    )
    .await;
    assert_eq!(
        code, "ClientException",
        "case {case:?}: expected the modelled client refusal, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, ROLE_NOT_VALID,
        "case {case:?}: expected the refusal {ROLE_NOT_VALID:?}, got {message:?}"
    );
}
