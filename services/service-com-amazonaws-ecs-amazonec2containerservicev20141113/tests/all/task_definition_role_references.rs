// SPDX-License-Identifier: CC0-1.0

//! What a task-definition registration makes of the IAM ROLE references it carries, driven
//! end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! Three rules, all read off the two role members of a registration:
//!
//! * a role member that takes "the short name or full Amazon Resource Name (ARN) of the IAM
//!   role" (`RegisterTaskDefinition$taskRoleArn`) is stored RESOLVED to that role's ARN in
//!   the caller's account, so every later read of the revision reports an ARN whichever form
//!   the registration spelled;
//! * a revision that names a TASK role requires the container-agent capability that serves
//!   those credentials to its containers, reported through `TaskDefinition$requiresAttributes`
//!   ("The container instance attributes required by your task"), and a revision that names
//!   none requires nothing of the kind;
//! * Amazon ECS fetches an image a registry serves only to an authenticated principal as the
//!   task EXECUTION role (`TaskDefinition$executionRoleArn`: "the task execution role that
//!   grants the Amazon ECS container agent permission to make Amazon Web Services API calls
//!   on your behalf"), and a Fargate task runs on capacity Amazon ECS owns rather than on a
//!   container instance of the caller's whose own role the agent could fall back on — so a
//!   revision carrying such an image and no execution role is not Fargate-compatible
//!   (`TaskDefinition$compatibilities`), and a registration that REQUIRES Fargate is refused
//!   outright.
//!
//! The rows differ along what each rule generalises over: the FORM of the role reference (a
//! bare name, a name under a path, a full ARN, both members at once, neither), which role the
//! revision names, and the REGISTRY the container's image resolves to (a repository of this
//! account's Amazon ECR registry, one of another account's, Amazon ECR Public, a bare
//! reference a container runtime resolves against Docker Hub) crossed with whether an
//! execution role is named.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeTaskDefinition` and `DescribeDaemonTaskDefinition`, and the refusal's sentence is
//! spelled out here rather than imported, so a wrong wording in the implementation cannot
//! make a test pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{answered, list, ok, refusal, service, string, ACCOUNT, SIGNED_REGION};
use crate::task_lifecycle::container;

/// The container-agent capability a revision that names a task role requires, as the agent
/// publishes it (Amazon ECS Developer Guide, "IAM roles for tasks": the
/// `com.amazonaws.ecs.capability.task-iam-role` attribute a container instance registers).
const TASK_IAM_ROLE_ATTRIBUTE: &str = "com.amazonaws.ecs.capability.task-iam-role";

/// A legal Fargate task size (one `cpu`/`memory` pair of `RegisterTaskDefinition$memory`'s
/// documented list), so a row that requires Fargate meets the task-size requirement and the
/// only thing left to decide is the image and the execution role.
const TASK_CPU: &str = "512";
const TASK_MEMORY: &str = "1024";

/// An image in a repository of THIS account's Amazon ECR registry, whose registry URI is
/// `<account>.dkr.ecr.<region>.amazonaws.com` (Amazon ECR User Guide, the registry URI a pull
/// uses) — a registry that serves a pull only to an authenticated principal.
fn ecr_image() -> String {
    format!("{ACCOUNT}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/app:1")
}

/// The same form naming ANOTHER account's registry: still Amazon ECR, so still a pull that
/// needs a principal — the row that proves the rule is about the registry and not about the
/// account spelled in the reference.
fn other_account_ecr_image() -> String {
    format!("210987654321.dkr.ecr.{SIGNED_REGION}.amazonaws.com/app:1")
}

/// An image reference carrying the address Amazon ECR announces its registry at on THIS
/// machine — `Repository$repositoryUri` ("You can use this URI for container image push and
/// pull operations"), which here is an address of the emulator the registry is served by
/// rather than the published `<account>.dkr.ecr.<region>.amazonaws.com` host. A client that
/// asked Amazon ECR for the URI pulls from exactly this reference, so it names a repository
/// of the account's own registry too. The two forms such an address takes are a loopback name
/// and a loopback literal, each with the port the emulator is served on.
const ANNOUNCED_REGISTRY_IMAGES: [&str; 2] = ["localhost:9297/app:1", "127.0.0.1:9297/app:1"];

/// An image in Amazon ECR PUBLIC, which serves anyone: `public.ecr.aws/<alias>/<repository>`.
const PUBLIC_IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// A reference that names no registry at all, which a container runtime resolves against
/// Docker Hub — the other form of an image no principal is needed for.
const HUB_IMAGE: &str = "nginx:1.27";

/// The sentence Amazon ECS refuses a Fargate registration whose image needs a principal it
/// named no role for.
const FARGATE_ECR_MESSAGE: &str =
    "Fargate requires task definition to have execution role ARN to support ECR images.";

/// The ARN of a role of this account, as IAM renders one:
/// `arn:aws:iam::<account>:role<path><name>`.
fn role_arn(path: &str, name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role{path}{name}")
}

/// Register one revision and read it back through `DescribeTaskDefinition`, answering BOTH
/// projections of the same stored revision — so a member rendered off the request rather than
/// off the record shows up as a difference between them.
async fn register_and_read(
    router: &MultiServiceRouter,
    request: Value,
    family: &str,
    case: &str,
) -> Vec<(&'static str, Value)> {
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

/// The `name` of every attribute a projection's `requiresAttributes` carries.
fn required_attributes(definition: &Value, case: &str) -> Vec<String> {
    definition["requiresAttributes"]
        .as_array()
        .map(|attributes| {
            attributes
                .iter()
                .map(|attribute| string(attribute, "name", case))
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the role-reference table.
struct RoleRow {
    case: &'static str,
    /// What the registration spells in `taskRoleArn`, or `None` when it names no task role.
    task_role: Option<String>,
    /// What it spells in `executionRoleArn`, or `None` when it names no execution role.
    execution_role: Option<String>,
    /// The ARN the stored revision must report for each member.
    expected_task_role: Option<String>,
    expected_execution_role: Option<String>,
}

#[tokio::test]
async fn a_role_reference_is_stored_as_the_role_arn_whichever_form_it_was_spelled_in() {
    let router = service();

    let rows: Vec<RoleRow> = vec![
        RoleRow {
            case: "bare names for both members",
            task_role: Some("ecs-task".to_string()),
            execution_role: Some("ecs-execution".to_string()),
            expected_task_role: Some(role_arn("/", "ecs-task")),
            expected_execution_role: Some(role_arn("/", "ecs-execution")),
        },
        RoleRow {
            case: "a bare name for the task role alone",
            task_role: Some("task-only".to_string()),
            execution_role: None,
            expected_task_role: Some(role_arn("/", "task-only")),
            expected_execution_role: None,
        },
        RoleRow {
            case: "a bare name for the execution role alone",
            task_role: None,
            execution_role: Some("execution-only".to_string()),
            expected_task_role: None,
            expected_execution_role: Some(role_arn("/", "execution-only")),
        },
        RoleRow {
            case: "full ARNs, which are stored as supplied",
            task_role: Some(role_arn("/", "arn-task")),
            execution_role: Some(role_arn("/", "arn-execution")),
            expected_task_role: Some(role_arn("/", "arn-task")),
            expected_execution_role: Some(role_arn("/", "arn-execution")),
        },
        RoleRow {
            case: "a name carrying the role's path, which the ARN keeps",
            task_role: Some("service-role/pathed-task".to_string()),
            execution_role: Some("/service-role/pathed-execution".to_string()),
            expected_task_role: Some(role_arn("/", "service-role/pathed-task")),
            expected_execution_role: Some(role_arn("/", "service-role/pathed-execution")),
        },
        RoleRow {
            case: "an ARN under a path",
            task_role: Some(role_arn("/service-role/", "arn-pathed")),
            execution_role: None,
            expected_task_role: Some(role_arn("/service-role/", "arn-pathed")),
            expected_execution_role: None,
        },
        RoleRow {
            case: "neither member",
            task_role: None,
            execution_role: None,
            expected_task_role: None,
            expected_execution_role: None,
        },
        RoleRow {
            case: "members present but spelling nothing but whitespace, which name no role",
            task_role: Some("   ".to_string()),
            execution_role: Some(" ".to_string()),
            expected_task_role: None,
            expected_execution_role: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let family = format!("rolefam{index}");
        let mut request = json!({
            "family": family,
            "containerDefinitions": [container("app", PUBLIC_IMAGE)],
        });
        if let Some(reference) = &row.task_role {
            request["taskRoleArn"] = json!(reference);
        }
        if let Some(reference) = &row.execution_role {
            request["executionRoleArn"] = json!(reference);
        }
        for (source, definition) in register_and_read(&router, request, &family, case).await {
            for (member, expected) in [
                ("taskRoleArn", row.expected_task_role.as_deref()),
                ("executionRoleArn", row.expected_execution_role.as_deref()),
            ] {
                let actual = definition[member].as_str();
                assert_eq!(
                    actual, expected,
                    "case {case:?}: {source} expected {member} {expected:?}, got {actual:?} \
                     in {definition}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_daemon_registration_resolves_its_role_references_the_same_way() {
    let router = service();

    // (case, the family, what the registration spells, the ARN it must read back as)
    let rows: [(&str, &str, String, String); 3] = [
        (
            "a bare name",
            "daemonrole0",
            "daemon-task".to_string(),
            role_arn("/", "daemon-task"),
        ),
        (
            "a full ARN",
            "daemonrole1",
            role_arn("/", "daemon-arn"),
            role_arn("/", "daemon-arn"),
        ),
        (
            "a name under a path",
            "daemonrole2",
            "service-role/daemon-pathed".to_string(),
            role_arn("/", "service-role/daemon-pathed"),
        ),
    ];

    for (case, family, reference, expected) in rows {
        ok(
            &router,
            "RegisterDaemonTaskDefinition",
            json!({
                "family": family,
                "taskRoleArn": reference,
                "executionRoleArn": reference,
                "containerDefinitions": [container("agent", PUBLIC_IMAGE)],
            }),
            case,
        )
        .await;
        let body = ok(
            &router,
            "DescribeDaemonTaskDefinition",
            json!({ "daemonTaskDefinition": family }),
            case,
        )
        .await;
        let definition = &body["daemonTaskDefinition"];
        for member in ["taskRoleArn", "executionRoleArn"] {
            let actual = definition[member].as_str();
            assert_eq!(
                actual,
                Some(expected.as_str()),
                "case {case:?}: expected {member} {expected:?}, got {actual:?} in {definition}"
            );
        }
    }
}

/// One row of the requires-attributes table: which roles the registration names, and whether
/// the task-IAM-role capability must then be required.
struct AttributeRow {
    case: &'static str,
    family: &'static str,
    task_role: Option<&'static str>,
    execution_role: Option<&'static str>,
    required: bool,
}

#[tokio::test]
async fn the_task_iam_role_attribute_follows_the_task_role() {
    let router = service();

    let rows: [AttributeRow; 4] = [
        AttributeRow {
            case: "neither role",
            family: "attrfam0",
            task_role: None,
            execution_role: None,
            required: false,
        },
        AttributeRow {
            case: "a task role alone",
            family: "attrfam1",
            task_role: Some("task"),
            execution_role: None,
            required: true,
        },
        AttributeRow {
            case: "an execution role alone, which is a different capability",
            family: "attrfam2",
            task_role: None,
            execution_role: Some("execution"),
            required: false,
        },
        AttributeRow {
            case: "both roles",
            family: "attrfam3",
            task_role: Some("task"),
            execution_role: Some("execution"),
            required: true,
        },
    ];

    for AttributeRow {
        case,
        family,
        task_role,
        execution_role,
        required,
    } in rows
    {
        let mut request = json!({
            "family": family,
            "containerDefinitions": [container("app", PUBLIC_IMAGE)],
        });
        if let Some(reference) = task_role {
            request["taskRoleArn"] = json!(reference);
        }
        if let Some(reference) = execution_role {
            request["executionRoleArn"] = json!(reference);
        }
        for (source, definition) in register_and_read(&router, request, family, case).await {
            let attributes = required_attributes(&definition, case);
            assert_eq!(
                attributes.contains(&TASK_IAM_ROLE_ATTRIBUTE.to_string()),
                required,
                "case {case:?}: {source} expected the {TASK_IAM_ROLE_ATTRIBUTE} requirement \
                 == {required}, got {attributes:?}"
            );
        }
    }
}

/// One row of the image-and-execution-role table: which registry the container's image
/// resolves to, whether the registration names an execution role, and what follows for the
/// revision's Fargate compatibility.
struct PullRow {
    case: &'static str,
    family: &'static str,
    image: String,
    execution_role: Option<&'static str>,
    /// Whether the registration describes a SECOND container, on a public registry: the row
    /// that makes the rule one about the container LIST rather than about one container.
    sidecar: bool,
    /// Whether the revision must report `FARGATE` among its derived compatibilities.
    fargate: bool,
}

/// The rows both pull tests drive: every registry form crossed with the execution role.
fn pull_rows() -> Vec<PullRow> {
    vec![
        PullRow {
            case: "an ECR image of this account with no execution role",
            family: "pullfam0",
            image: ecr_image(),
            execution_role: None,
            sidecar: false,
            fargate: false,
        },
        PullRow {
            case: "an ECR image of this account with an execution role",
            family: "pullfam1",
            image: ecr_image(),
            execution_role: Some("ecs-execution"),
            sidecar: false,
            fargate: true,
        },
        PullRow {
            case: "an ECR image of another account with no execution role",
            family: "pullfam2",
            image: other_account_ecr_image(),
            execution_role: None,
            sidecar: false,
            fargate: false,
        },
        PullRow {
            case: "an Amazon ECR Public image with no execution role",
            family: "pullfam3",
            image: PUBLIC_IMAGE.to_string(),
            execution_role: None,
            sidecar: false,
            fargate: true,
        },
        PullRow {
            case: "a Docker Hub reference with no execution role",
            family: "pullfam4",
            image: HUB_IMAGE.to_string(),
            execution_role: None,
            sidecar: false,
            fargate: true,
        },
        PullRow {
            case: "two containers, one of them an ECR image, with no execution role",
            family: "pullfam5",
            image: ecr_image(),
            execution_role: None,
            sidecar: true,
            fargate: false,
        },
        PullRow {
            case: "the registry address Amazon ECR announces here, by loopback name, with no \
                   execution role",
            family: "pullfam6",
            image: ANNOUNCED_REGISTRY_IMAGES[0].to_string(),
            execution_role: None,
            sidecar: false,
            fargate: false,
        },
        PullRow {
            case: "the registry address Amazon ECR announces here, by loopback literal, with \
                   no execution role",
            family: "pullfam7",
            image: ANNOUNCED_REGISTRY_IMAGES[1].to_string(),
            execution_role: None,
            sidecar: false,
            fargate: false,
        },
        PullRow {
            case: "the registry address Amazon ECR announces here, with an execution role",
            family: "pullfam8",
            image: ANNOUNCED_REGISTRY_IMAGES[1].to_string(),
            execution_role: Some("ecs-execution"),
            sidecar: false,
            fargate: true,
        },
    ]
}

/// The `awsvpc` registration body one pull row asks for, at a legal Fargate task size — so
/// the only thing left deciding Fargate is the image and the execution role.
fn pull_request(row: &PullRow, requires_fargate: bool) -> Value {
    let mut containers = vec![container("app", &row.image)];
    if row.sidecar {
        containers.push(container("sidecar", PUBLIC_IMAGE));
    }
    let mut request = json!({
        "family": row.family,
        "networkMode": "awsvpc",
        "cpu": TASK_CPU,
        "memory": TASK_MEMORY,
        "containerDefinitions": containers,
    });
    if requires_fargate {
        request["requiresCompatibilities"] = json!(["FARGATE"]);
    }
    if let Some(reference) = row.execution_role {
        request["executionRoleArn"] = json!(reference);
    }
    request
}

#[tokio::test]
async fn compatibilities_drop_fargate_for_an_image_no_role_could_pull() {
    let router = service();

    for row in pull_rows() {
        let case = row.case;
        let request = pull_request(&row, false);
        for (source, definition) in register_and_read(&router, request, row.family, case).await {
            let compatibilities: Vec<String> = list(&definition, "compatibilities", case)
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                compatibilities.contains(&"FARGATE".to_string()),
                row.fargate,
                "case {case:?}: {source} expected FARGATE present == {}, got \
                 {compatibilities:?}",
                row.fargate
            );
            // EC2 capacity is the caller's own, and its container instances carry an
            // instance role the agent falls back on, so the rule takes no launch type but
            // Fargate away.
            assert!(
                compatibilities.contains(&"EC2".to_string()),
                "case {case:?}: {source} expected EC2 to stay, got {compatibilities:?}"
            );
        }
    }
}

#[tokio::test]
async fn requiring_fargate_for_an_image_no_role_could_pull_is_refused() {
    let router = service();

    for row in pull_rows() {
        let case = row.case;
        let request = pull_request(&row, true);
        if row.fargate {
            let registered = ok(&router, "RegisterTaskDefinition", request, case).await;
            let revision = registered["taskDefinition"]["revision"].as_i64();
            assert_eq!(
                revision,
                Some(1),
                "case {case:?}: expected the registration to answer the family's first \
                 revision, got {registered}"
            );
            continue;
        }
        let (code, message) = refusal(&router, "RegisterTaskDefinition", request, case).await;
        assert_eq!(
            code, "ClientException",
            "case {case:?}: expected the modelled client refusal, got {code:?} with \
             {message:?}"
        );
        assert_eq!(
            message, FARGATE_ECR_MESSAGE,
            "case {case:?}: expected the refusal {FARGATE_ECR_MESSAGE:?}, got {message:?}"
        );
        // A registration Amazon ECS refused registered nothing, so the family holds no
        // revision to read back.
        let (status, body) = answered(
            &router,
            "DescribeTaskDefinition",
            json!({ "taskDefinition": row.family }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the refused family to hold no revision, got {status} \
             with {body}"
        );
    }
}
