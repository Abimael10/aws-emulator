// SPDX-License-Identifier: CC0-1.0

//! The image PULL a launched task's containers need, served by Amazon ECR, and the
//! terminal status a task that ran reaches — driven end to end over the wire, with Amazon
//! ECR and IAM mounted beside Amazon ECS on one engine so every repository, image, pull
//! through cache rule, role and policy is created through the OWNING service's own API
//! rather than written into a store by hand.
//!
//! The rules under test:
//!
//! * a container's image is fetched before the container starts, and for a reference that
//!   names a repository of this account's Amazon ECR registry the fetch is the registry's
//!   to serve: Amazon ECS pulls as the task's EXECUTION ROLE
//!   (`TaskDefinition$executionRoleArn`), so the registry decides the two calls a pull
//!   makes (`ecr:GetAuthorizationToken` on the registry and `ecr:BatchGetImage` on the
//!   repository) over the policies in force on that role and the repository's own policy,
//!   resolves the reference, and caches the upstream image where a pull through cache rule
//!   covers the repository — and a task whose image it will not serve never runs: it stops
//!   with `Task$stopCode` `TaskFailedToStart` and a `CannotPullContainerError`
//!   `stoppedReason`, with no `startedAt`;
//! * a task that DID pull records the instants the fetch took (`Task$pullStartedAt` and
//!   `pullStoppedAt`) and runs, and a task placed on its own then reaches the terminal
//!   `STOPPED` status with the stop code for an essential container that exited — while a
//!   task a service's scheduler placed is kept running by that scheduler;
//! * that same stop is what a SERVICE's scheduler produces for a reference the registry
//!   resolves to nothing, and it is reachable the way a caller diagnosing the service finds
//!   it: listed by `ListTasks` under the stopped desired status and then described, with the
//!   failure reported both as the task's `stoppedReason` and as the `Container$reason` ("a
//!   short (1024 max characters) human-readable string to provide additional details about a
//!   running or stopped container") of the container whose image it was — while the
//!   service's `Service$runningCount` stays `0`.
//!
//! The rows differ along what the rule generalises over: whether the registry holds the
//! image, whether a cache rule covers the repository, which half of the pull the role is
//! granted, which document grants it, whether the reference names this registry at all,
//! and — for a service's deployment — the FORM the reference that resolves to nothing
//! takes. Nothing here reaches into either state struct: every value asserted is read back
//! through `DescribeTasks`, `ListTasks`, `DescribeServices` and Amazon ECR's
//! `DescribeImages`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, count, ecr, emulator_with_ecr_and_iam, iam, list, ok,
    register_instance, string, ACCOUNT, AWSVPC_START, SIGNED_REGION,
};
use crate::task_lifecycle::container;

/// The cluster every task below runs in.
const CLUSTER: &str = "registrypull";

/// The Secrets Manager secret the pull through cache rule below reads its upstream
/// credentials from, in the namespace Amazon ECR's `CredentialArn` constrains one to
/// (`^arn:aws(-\w+)*:secretsmanager:[a-zA-Z0-9-:]+:secret:ecr\-pullthroughcache\/[a-zA-Z0-9\/_+=.@-]+$`,
/// length 50..612).
const UPSTREAM_CREDENTIAL_ARN: &str =
    "arn:aws:secretsmanager:us-east-1:000000000000:secret:ecr-pullthroughcache/docker-hub-AbCdEf";

/// The EC2 instance id the cluster's one container instance is registered under: a task
/// placed with the default `EC2` launch type runs on the capacity registered to its
/// cluster, so every cluster below has one.
const CAPACITY_INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c1";

/// How often a poll re-reads a task.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a poll keeps trying before it gives up: the whole run of a standalone task —
/// its start, the work it was placed to do (`state::CONTAINER_WORK_SPAN`) and the stop that
/// follows — with margin for a loaded test machine. Built by `Duration` arithmetic, so no
/// figure here is a cast.
const POLL_BUDGET: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::CONTAINER_WORK_SPAN
        .saturating_add(
            service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::TRANSITION_SPAN
                .saturating_mul(4),
        )
        .saturating_add(Duration::from_secs(4));

/// How long a poll of a SERVICE's task keeps trying: the whole start of an `awsvpc` task
/// ([`AWSVPC_START`], which covers the interface steps Amazon ECS takes and the start owed
/// after them) plus the further spans a failed launch takes — the fetch is decided where the
/// start would have been, and `STOPPED` falls due one span after that, once the release of
/// the interface the task was allocated has finished (one
/// `state::INTERFACE_RELEASE_STEP_SPAN` per step of `state::INTERFACE_DETACH_LIFECYCLE`) —
/// with margin for a loaded test machine. Built by `Duration` arithmetic, so no figure here
/// is a cast.
const SERVICE_POLL_BUDGET: Duration = AWSVPC_START
    .saturating_add(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::INTERFACE_RELEASE_STEP_SPAN
            .saturating_mul(3),
    )
    .saturating_add(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::TRANSITION_SPAN
            .saturating_mul(4)
            .saturating_add(Duration::from_secs(4)),
    );

/// `Task$lastStatus` of a task that has stopped.
const STOPPED: &str = "STOPPED";

/// `Task$lastStatus` of a task that is running.
const RUNNING: &str = "RUNNING";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of
/// (`smithy_query(query="shape",shape="TaskStopCode")`).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// `Task$stopCode` of a task whose essential container exited — the end of a task that
/// ran.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The argument vector a standalone row's container runs: a program that ends at once, so
/// the container really exits and the task is stopped for that exit.
const PROGRAM_THAT_ENDS: [&str; 3] = ["sh", "-c", "exit 0"];

/// The error class a container runtime reports a failed image fetch under, which is the
/// fixed part of such a `stoppedReason`.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is who may
/// pull an image and whether the registry holds one, not which blobs an image is built
/// from.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#
);

/// Create the IAM role `name`, trusted by Amazon ECS, carrying `policy` as its one inline
/// policy when a document is given — through IAM's own API — and answer the role's ARN.
async fn role(router: &MultiServiceRouter, name: &str, policy: Option<&str>) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }]
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        name,
    )
    .await;
    if let Some(policy) = policy {
        iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
            name,
        )
        .await;
    }
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Create the cluster the tasks below run in, with ONE container instance registered into
/// it: "The EC2 launch type runs your tasks on Amazon EC2 instances registered to your
/// cluster", so a cluster with none has nowhere to place a task.
async fn cluster(router: &MultiServiceRouter) {
    ok(router, "CreateCluster", json!({ "clusterName": CLUSTER }), "cluster").await;
    register_instance(router, CLUSTER, CAPACITY_INSTANCE_ID, "capacity").await;
}

/// Register one revision of `family` running `image` as `execution_role`, and run one task
/// of it — answering the task's ARN.
///
/// The container runs a program that ENDS: a task is stopped as one whose essential
/// container exited only once that container really exits
/// (`Container$exitCode` is "the exit code returned from the container"), so a row that
/// reads the stop of a task that RAN gives its container something to run to the end. What
/// the program does is beside the point here — every row is about the PULL that precedes it.
async fn run(
    router: &MultiServiceRouter,
    family: &str,
    image: &str,
    execution_role: Option<&str>,
    case: &str,
) -> String {
    let mut app = container("app", image);
    app["command"] = json!(PROGRAM_THAT_ENDS);
    let mut definition = json!({
        "family": family,
        "containerDefinitions": [app],
    });
    if let Some(role) = execution_role {
        definition["executionRoleArn"] = json!(role);
    }
    ok(router, "RegisterTaskDefinition", definition, case).await;
    let body = ok(
        router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": family }),
        case,
    )
    .await;
    let tasks = body["tasks"].as_array().cloned().unwrap_or_default();
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(task, "taskArn", case)
}

/// Read ONE task back through `DescribeTasks` — the stored record, not the answer of the
/// call that launched it.
async fn describe(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    body["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

/// Poll one task until it reaches the terminal `STOPPED` status, and answer the record it
/// stopped as.
async fn stopped(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, task_arn, case).await;
        if string(&task, "lastStatus", case) == STOPPED {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {STOPPED}, it was still {:?}: {task}",
            string(&task, "lastStatus", case)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An image reference naming the repository `path` of this account's registry in the Region
/// the requests are signed for: `<account>.dkr.ecr.<region>.amazonaws.com/<path>` (Amazon
/// ECR User Guide, the registry URI a pull uses).
fn ecr_image(path: &str) -> String {
    format!("{ACCOUNT}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/{path}")
}

/// An identity-based document allowing `actions` on every resource.
fn grant(actions: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
    })
    .to_string()
}

/// What a row expects of the task it launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The registry served the image, so the task ran and then ended.
    Ran,
    /// The registry would not serve it, so the task never started.
    FailedToStart,
    /// The registry withheld the authorization TOKEN the agent retrieves before any fetch, so
    /// the task failed in the execution-resource retrieval having reached no image
    /// ([`REGISTRY_AUTH_RETRIEVAL_FAILED`]).
    RegistryAuthWithheld,
}

/// One row of the table: a revision running `image` as `execution_role`, and the terminal
/// record the task it places must reach.
struct Row {
    case: &'static str,
    family: &'static str,
    image: String,
    execution_role: Option<String>,
    expected: Outcome,
    /// A clause the task's `stoppedReason` must carry, for a row whose stop says WHAT the
    /// registry answered about the pull.
    says: Option<String>,
}

impl Row {
    /// This row with `expected` in place of the outcome it was built with.
    fn with_outcome(self, expected: Outcome) -> Self {
        Self { expected, ..self }
    }
}

/// One task launched by the table, waiting to be read back.
struct Launched {
    case: &'static str,
    image: String,
    task_arn: String,
    expected: Outcome,
    says: Option<String>,
    /// The execution role the task pulls as, which a refusal of one of the pull's calls names
    /// the session of, and `None` for a task that names none.
    execution_role: Option<String>,
}

#[tokio::test]
async fn a_task_pulls_its_ecr_image_as_its_execution_role_and_then_settles() {
    let router = emulator_with_ecr_and_iam();
    cluster(&router).await;
    let case = "the registry's records";

    // The registry's own records: one repository holding one tagged image, and one pull
    // through cache rule whose prefix nothing has been cached under yet.
    ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": "app" }),
        case,
    )
    .await;
    ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": "app",
            "imageManifest": MANIFEST,
            "imageTag": "1",
        }),
        case,
    )
    .await;
    ecr(
        &router,
        "CreatePullThroughCacheRule",
        json!({
            "ecrRepositoryPrefix": "hub",
            "upstreamRegistryUrl": "registry-1.docker.io",
            // The Docker Hub upstream authenticates its callers, so the rule carries the
            // Secrets Manager secret the registry reads its credentials from — a rule
            // without one is refused by `CreatePullThroughCacheRule`.
            "credentialArn": UPSTREAM_CREDENTIAL_ARN,
        }),
        case,
    )
    .await;
    // A repository with a policy that shares its images with ONE role, which is the other
    // document a pull can be allowed by.
    ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": "shared" }),
        case,
    )
    .await;
    ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": "shared",
            "imageManifest": MANIFEST,
            "imageTag": "1",
        }),
        case,
    )
    .await;

    // The roles, each carrying a different part of what a pull takes.
    let full = role(
        &router,
        "pullall",
        Some(&grant(&["ecr:GetAuthorizationToken", "ecr:BatchGetImage"])),
    )
    .await;
    let none = role(&router, "pullnone", Some(&grant(&["ecr:ListImages"]))).await;
    let token_only = role(
        &router,
        "pulltoken",
        Some(&grant(&["ecr:GetAuthorizationToken"])),
    )
    .await;
    let unknown = format!("arn:aws:iam::{ACCOUNT}:role/nosuchrole");
    ecr(
        &router,
        "SetRepositoryPolicy",
        json!({
            "repositoryName": "shared",
            "policyText": json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": token_only },
                    "Action": "ecr:BatchGetImage",
                    // A repository policy is attached to the repository its statements
                    // apply to, so it names no resource of its own — Amazon ECR refuses a
                    // repository policy that does.
                }],
            }).to_string(),
        }),
        case,
    )
    .await;
    // A third repository, holding the same image, whose policy DENIES the pull outright: an
    // explicit deny on either side refuses whatever the identity documents grant.
    ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": "denied" }),
        case,
    )
    .await;
    ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": "denied",
            "imageManifest": MANIFEST,
            "imageTag": "1",
        }),
        case,
    )
    .await;
    ecr(
        &router,
        "SetRepositoryPolicy",
        json!({
            "repositoryName": "denied",
            "policyText": json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": "ecr:BatchGetImage",
                }],
            }).to_string(),
        }),
        case,
    )
    .await;

    let row = |case: &'static str,
               family: &'static str,
               image: String,
               execution_role: Option<&String>,
               expected: Outcome| Row {
        case,
        family,
        image,
        execution_role: execution_role.cloned(),
        expected,
        says: None,
    };
    /// The clause a refusal carries when nothing grants a call: IAM's default deny, naming
    /// the action (`service_common::iam_authorization::no_identity_policy_allows`).
    fn nothing_allows(action: &str) -> String {
        format!("because no identity-based policy allows the {action} action")
    }
    /// The clause a refusal carries when the REGISTRY answered the manifest request the fetch
    /// made: a `docker pull` is refused with an HTTP status, which is what the runtime
    /// reports.
    fn refused_manifest(repository: &str, reference: &str) -> String {
        format!(
            "unexpected status from HEAD request to \
             https://{ACCOUNT}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/v2/{repository}\
             /manifests/{reference}: 403 Forbidden"
        )
    }
    let rows = vec![
        row(
            "an image the registry holds, pulled by a granted role",
            "held",
            ecr_image("app:1"),
            Some(&full),
            Outcome::Ran,
        ),
        row(
            "a repository a pull through cache rule covers, cached on this pull",
            "cached",
            ecr_image("hub/library/busybox:latest"),
            Some(&full),
            Outcome::Ran,
        ),
        row(
            "a repository the registry does not hold and no rule covers",
            "missingrepo",
            ecr_image("absent:1"),
            Some(&full),
            Outcome::FailedToStart,
        ),
        row(
            "a tag the repository does not hold",
            "missingtag",
            ecr_image("app:2"),
            Some(&full),
            Outcome::FailedToStart,
        ),
        Row {
            says: Some(nothing_allows("ecr:GetAuthorizationToken")),
            ..row(
                "a role no document grants an ECR pull to",
                "nogrant",
                ecr_image("app:1"),
                Some(&none),
                // The agent never reaches the image: the registry's authorization token is
                // the execution resource it retrieves first, and this role is refused it
                // (the table in
                // `a_task_whose_role_cannot_get_the_registry_token_fails_the_resource_retrieval`).
                Outcome::RegistryAuthWithheld,
            )
        },
        Row {
            says: Some(refused_manifest("app", "1")),
            ..row(
                "a role granted the registry token but not the manifest",
                "tokenonly",
                ecr_image("app:1"),
                Some(&token_only),
                Outcome::FailedToStart,
            )
        },
        row(
            "a role the repository's own policy shares the image with",
            "sharedrepo",
            ecr_image("shared:1"),
            Some(&token_only),
            Outcome::Ran,
        ),
        Row {
            says: Some(refused_manifest("denied", "1")),
            ..row(
                "a repository whose own policy denies the pull a role is granted",
                "deniedrepo",
                ecr_image("denied:1"),
                Some(&full),
                Outcome::FailedToStart,
            )
        },
        row(
            "a role this account's IAM holds no record for",
            "unknownrole",
            ecr_image("app:1"),
            Some(&unknown),
            Outcome::Ran,
        ),
        row(
            "no execution role at all",
            "norole",
            ecr_image("app:1"),
            None,
            Outcome::Ran,
        ),
        row(
            "a reference naming another registry altogether",
            "publicimage",
            "public.ecr.aws/nginx/nginx:latest".to_string(),
            Some(&none),
            Outcome::Ran,
        ),
    ];

    // Every task is launched first and polled afterwards: each settles on its own clock,
    // so the whole table costs one run rather than one per row.
    let mut launched = Vec::new();
    for row in &rows {
        let task_arn = run(
            &router,
            row.family,
            &row.image,
            row.execution_role.as_deref(),
            row.case,
        )
        .await;
        launched.push(Launched {
            case: row.case,
            image: row.image.clone(),
            task_arn,
            expected: row.expected,
            says: row.says.clone(),
            execution_role: row.execution_role.clone(),
        });
    }

    for Launched {
        case,
        image,
        task_arn,
        expected,
        says,
        execution_role,
    } in launched
    {
        let task = stopped(&router, &task_arn, case).await;
        let stop_code = string(&task, "stopCode", case);
        // The pull is ATTEMPTED either way, so both instants are reported either way.
        for member in ["pullStartedAt", "pullStoppedAt"] {
            assert!(
                !task[member].is_null(),
                "case {case:?}: expected the attempted pull's {member}, got {task}"
            );
        }
        match expected {
            Outcome::Ran => {
                assert_eq!(
                    stop_code, ESSENTIAL_CONTAINER_EXITED,
                    "case {case:?}: expected a task that pulled {image} to end with \
                     {ESSENTIAL_CONTAINER_EXITED}, got {task}"
                );
                assert!(
                    !task["startedAt"].is_null(),
                    "case {case:?}: expected a startedAt on a task that ran, got {task}"
                );
            }
            Outcome::FailedToStart => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task that could not pull {image} to stop with \
                     {TASK_FAILED_TO_START}, got {task}"
                );
                let reason = string(&task, "stoppedReason", case);
                assert!(
                    reason.contains(CANNOT_PULL),
                    "case {case:?}: expected the stoppedReason to report {CANNOT_PULL}, got \
                     {reason:?}"
                );
                assert!(
                    reason.contains(&image),
                    "case {case:?}: expected the stoppedReason to name the image {image}, got \
                     {reason:?}"
                );
                // A row that is about what the registry ANSWERED asserts that too: a manifest
                // request it refused is answered with an HTTP status, which is not the same
                // answer as a reference it resolves to nothing.
                if let Some(says) = says.as_deref() {
                    assert!(
                        reason.contains(says),
                        "case {case:?}: expected the stoppedReason to carry {says:?}, got \
                         {reason:?}"
                    );
                }
                assert!(
                    task["startedAt"].is_null(),
                    "case {case:?}: expected no startedAt on a task that never ran, got {task}"
                );
            }
            Outcome::RegistryAuthWithheld => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task whose registry authentication was \
                     withheld to stop with {TASK_FAILED_TO_START}, got {task}"
                );
                assert_registry_auth_withheld(
                    case,
                    &task,
                    &image,
                    says.as_deref(),
                    execution_role.as_deref(),
                );
            }
        }
        // What each container reports follows from whether it RAN: the one of a task that
        // ran performed the program its launch named and reports the status that program
        // ended with ([`PROGRAM_THAT_ENDS`] exits zero), while a task that never started a
        // container has none to report an exit status for.
        let expected_exit = match expected {
            Outcome::Ran => Some(0),
            Outcome::FailedToStart | Outcome::RegistryAuthWithheld => None,
        };
        for container in task["containers"].as_array().cloned().unwrap_or_default() {
            assert_eq!(
                container["exitCode"].as_i64(),
                expected_exit,
                "case {case:?}: expected exitCode {expected_exit:?}, got {container}"
            );
        }
    }

    // The import the cached row made, read back through the REGISTRY's own report of what
    // the cache repository holds: the upstream image is in it under the tag that was
    // pulled, with a digest of the manifest the registry stored.
    let case = "the cached image";
    let described = ecr(
        &router,
        "DescribeImages",
        json!({ "repositoryName": "hub/library/busybox" }),
        case,
    )
    .await;
    let details = described["imageDetails"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let detail = details
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the cached image, got {described}"));
    assert!(
        detail["imageTags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some("latest"))),
        "case {case:?}: expected the pulled tag on the cached image, got {detail}"
    );
    assert!(
        string(detail, "imageDigest", case).starts_with("sha256:"),
        "case {case:?}: expected a content digest on the cached image, got {detail}"
    );

    // And nothing was cached under a prefix no rule covers: the repository the missing-row
    // reference named is still absent from the registry.
    let listed = ecr(&router, "DescribeRepositories", json!({}), case).await;
    let names: Vec<String> = listed["repositories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|repository| string(repository, "repositoryName", case))
        .collect();
    assert!(
        !names.iter().any(|name| name == "absent"),
        "case \"a repository no rule covers\": expected no repository to be created for it, \
         got {names:?}"
    );
}

#[tokio::test]
async fn a_service_keeps_a_running_task_past_the_span_a_standalone_task_ends_in() {
    let router = emulator_with_ecr_and_iam();
    cluster(&router).await;
    let case = "a service's task";
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": "servicefamily",
            "containerDefinitions": [container("app", "public.ecr.aws/nginx/nginx:latest")],
        }),
        case,
    )
    .await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "web",
            "taskDefinition": "servicefamily",
            "desiredCount": 1,
        }),
        case,
    )
    .await;
    // A task placed ON ITS OWN in the same cluster, at the same instant, is the clock this
    // row is read against: once IT has stopped, the span a standalone task's work takes has
    // passed — so nothing here sleeps for a fixed stretch.
    let standalone = run(
        &router,
        "standalonefamily",
        "public.ecr.aws/nginx/nginx:latest",
        None,
        "a standalone task",
    )
    .await;
    stopped(&router, &standalone, "a standalone task").await;

    let described = ok(
        &router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": ["web"] }),
        case,
    )
    .await;
    let service = described["services"]
        .as_array()
        .and_then(|services| services.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {described}"));
    assert_eq!(
        service["runningCount"].as_i64().unwrap_or_default(),
        1,
        "case {case:?}: expected the scheduler to keep one task running, got {service}"
    );
    let listed = ok(
        &router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": "web" }),
        case,
    )
    .await;
    let task_arn = listed["taskArns"]
        .as_array()
        .and_then(|arns| arns.first().cloned())
        .and_then(|arn| arn.as_str().map(ToString::to_string))
        .unwrap_or_else(|| panic!("case {case:?}: expected the service's task, got {listed}"));
    let task = describe(&router, &task_arn, case).await;
    assert_eq!(
        string(&task, "lastStatus", case),
        "RUNNING",
        "case {case:?}: expected a service's task to still be running, got {task}"
    );
    assert!(
        task["stopCode"].is_null(),
        "case {case:?}: expected no stopCode on a running task, got {task}"
    );
}

/// The repository the service rows below pull from, which holds ONE tagged image.
const HELD_REPOSITORY: &str = "serviceapp";

/// The tag that repository's one image carries.
const HELD_TAG: &str = "1";

/// A repository that is created and then left EMPTY — nothing is ever pushed to it, which is
/// the state a repository is in between its create and its first push.
const EMPTY_REPOSITORY: &str = "servicestaging";

/// A repository name the registry is never asked to create.
const ABSENT_REPOSITORY: &str = "nosuchrepository";

/// A manifest digest no image of this registry was stored under: a well-formed `sha256`
/// digest, so what the registry answers about the reference is that it resolves to nothing
/// rather than that it cannot be read at all.
const UNHELD_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// Create `name` in this account's registry through Amazon ECR's own API, and answer the
/// `Repository$repositoryUri` that registry published for it — "The URI for the repository.
/// You can use this URI for container image push and pull operations", which is the value a
/// client composes the image reference in its container definition from.
async fn repository(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let created = ecr(
        router,
        "CreateRepository",
        json!({ "repositoryName": name }),
        case,
    )
    .await;
    string(&created["repository"], "repositoryUri", case)
}

/// Push one tagged image into the repository `name`, through Amazon ECR's own API.
async fn push(router: &MultiServiceRouter, name: &str, tag: &str, case: &str) {
    ecr(
        router,
        "PutImage",
        json!({
            "repositoryName": name,
            "imageManifest": MANIFEST,
            "imageTag": tag,
        }),
        case,
    )
    .await;
}

/// One row of the service table: a service whose revision runs `containers`, and the
/// container its launch must fail at — `None` for a row whose every image the registry
/// serves.
struct ServiceRow {
    case: &'static str,
    /// The service's name, which is also its revision's family and the `serviceName` the row
    /// is listed and described by.
    name: &'static str,
    /// `(container name, image reference)` per container of the revision, in the order the
    /// revision declares them.
    containers: Vec<(&'static str, String)>,
    /// The name of the container whose image cannot be fetched, and `None` for a row that
    /// runs.
    fails_at: Option<&'static str>,
}

impl ServiceRow {
    /// The image reference the container `name` of this row runs.
    fn image_of(&self, name: &str) -> String {
        self.containers
            .iter()
            .find(|(container, _)| *container == name)
            .map(|(_, image)| image.clone())
            .unwrap_or_else(|| {
                panic!("case {:?}: the row declares the container {name:?}", self.case)
            })
    }
}

/// Register the `awsvpc`, Fargate-compatible revision of `row` as `execution_role` and
/// create the service whose scheduler places it, with one task desired.
///
/// The revision names an execution role because the Fargate launch type requires one of a
/// revision whose containers run Amazon ECR images (`RegisterTaskDefinition` refuses one
/// without), and because the pull is made AS that role.
async fn create_service(router: &MultiServiceRouter, row: &ServiceRow, execution_role: &str) {
    let definitions: Vec<Value> = row
        .containers
        .iter()
        .map(|(name, image)| container(name, image))
        .collect();
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.name,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "executionRoleArn": execution_role,
            "containerDefinitions": definitions,
        }),
        row.case,
    )
    .await;
    ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.name,
            "taskDefinition": row.name,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        row.case,
    )
    .await;
}

/// The tasks of `row`'s service that `ListTasks` reports in `desired_status` (every desired
/// status when `None`), read back through `DescribeTasks` — the stored records, not the
/// answer of the call that placed them.
async fn service_tasks(
    router: &MultiServiceRouter,
    row: &ServiceRow,
    desired_status: Option<&str>,
) -> Vec<Value> {
    let mut request = json!({ "cluster": CLUSTER, "serviceName": row.name });
    if let Some(status) = desired_status {
        request["desiredStatus"] = json!(status);
    }
    let listed = ok(router, "ListTasks", request, row.case).await;
    let arns = list(&listed, "taskArns", row.case);
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        row.case,
    )
    .await;
    list(&described, "tasks", row.case)
}

/// The `runningCount` `DescribeServices` reports for `row`'s service.
async fn running_count(router: &MultiServiceRouter, row: &ServiceRow) -> i64 {
    let described = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.name] }),
        row.case,
    )
    .await;
    let service = list(&described, "services", row.case)
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the service back, got {described}",
                row.case
            )
        });
    count(&service, "runningCount", row.case)
}

/// The container `name` of `task`.
fn container_of(task: &Value, name: &str, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("case {case:?}: expected the container {name:?} on {task}"))
}

/// Assert everything the stop a failed fetch earns carries, on the task `task` of `row`.
fn assert_failed_fetch(row: &ServiceRow, task: &Value, failing: &str) {
    let case = row.case;
    let image = row.image_of(failing);
    let reason = string(task, "stoppedReason", case);
    for expected in [CANNOT_PULL, image.as_str()] {
        assert!(
            reason.contains(expected),
            "case {case:?}: expected the stoppedReason to carry {expected:?}, got {reason:?}"
        );
    }
    assert!(
        task["startedAt"].is_null(),
        "case {case:?}: expected no startedAt on a task that never ran, got {task}"
    );
    // The failure belongs to the container whose image it was, and to that container alone.
    for (name, _) in &row.containers {
        let container = container_of(task, name, case);
        let carried = container["reason"].as_str();
        if *name == failing {
            let carried = carried.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the failed fetch on the container {name:?}, got \
                     {container}"
                )
            });
            for expected in [CANNOT_PULL, image.as_str()] {
                assert!(
                    carried.contains(expected),
                    "case {case:?}: expected the reason of {name:?} to carry {expected:?}, got \
                     {carried:?}"
                );
            }
        } else {
            assert_eq!(
                carried, None,
                "case {case:?}: expected no reason on the container {name:?}, whose image was \
                 fetched, got {container}"
            );
        }
        assert_eq!(
            container["lastStatus"].as_str(),
            Some(STOPPED),
            "case {case:?}: expected the container {name:?} of a stopped task to be stopped, \
             got {container}"
        );
    }
}

#[tokio::test]
async fn a_service_whose_image_the_registry_resolves_to_nothing_stops_a_task_that_says_so() {
    let router = emulator_with_ecr_and_iam();
    cluster(&router).await;

    // The registry's own records, written through Amazon ECR's API: one repository holding
    // one tagged image, and one created and left empty.
    let case = "the registry's records";
    let held = repository(&router, HELD_REPOSITORY, case).await;
    push(&router, HELD_REPOSITORY, HELD_TAG, case).await;
    let empty = repository(&router, EMPTY_REPOSITORY, case).await;
    // The repository nothing creates is named in the SAME registry the two above are in, so
    // the rows differ in what that registry holds rather than in which registry they address.
    let absent = held.replace(HELD_REPOSITORY, ABSENT_REPOSITORY);
    // The task execution role every row pulls as, granted both calls a pull makes, so what
    // the rows differ in is what the registry RESOLVES rather than what it authorizes.
    let execution_role = role(
        &router,
        "servicepull",
        Some(&grant(&["ecr:GetAuthorizationToken", "ecr:BatchGetImage"])),
    )
    .await;

    let one = |case: &'static str, name: &'static str, image: String, fails: bool| ServiceRow {
        case,
        name,
        containers: vec![("app", image)],
        fails_at: fails.then_some("app"),
    };
    let rows = vec![
        one(
            "a tag the repository never held",
            "missingtagservice",
            format!("{held}:does-not-exist"),
            true,
        ),
        // A reference that names no tag at all asks for the registry's default tag, which
        // nothing was pushed under either: what decides the launch is what the reference
        // RESOLVES to, not how much of the pull form the caller wrote
        // (`<account>.dkr.ecr.<region>.<suffix>/<repository>[:<tag>|@<digest>]`, the pull
        // reference form of the Amazon ECR User Guide).
        one(
            "no tag at all, so the default tag, which the repository never held",
            "defaulttagservice",
            held.clone(),
            true,
        ),
        one(
            "a repository created and left empty",
            "emptyrepservice",
            format!("{empty}:{HELD_TAG}"),
            true,
        ),
        one(
            "a repository this registry does not hold",
            "absentrepservice",
            format!("{absent}:{HELD_TAG}"),
            true,
        ),
        one(
            "a digest the repository cannot resolve",
            "missingdigestservice",
            format!("{held}@{UNHELD_DIGEST}"),
            true,
        ),
        ServiceRow {
            case: "a second container whose image the registry holds nothing for",
            name: "sidecarmissingservice",
            containers: vec![
                ("app", format!("{held}:{HELD_TAG}")),
                ("sidecar", format!("{held}:2")),
            ],
            fails_at: Some("sidecar"),
        },
        one(
            "the tag the repository holds",
            "heldtagservice",
            format!("{held}:{HELD_TAG}"),
            false,
        ),
    ];

    // Every service is created first and polled afterwards: each converges on its own clock,
    // so the whole table costs one run rather than one per row.
    for row in &rows {
        create_service(&router, row, &execution_role).await;
    }

    for row in &rows {
        let case = row.case;
        let deadline = Instant::now() + SERVICE_POLL_BUDGET;
        loop {
            let settled = match row.fails_at {
                Some(failing) => {
                    // The task is reached exactly as a caller diagnosing the service reaches
                    // it: listed under the stopped desired status, then described.
                    let failed = service_tasks(&router, row, Some(STOPPED))
                        .await
                        .into_iter()
                        .find(|task| {
                            task["lastStatus"].as_str() == Some(STOPPED)
                                && task["stopCode"].as_str() == Some(TASK_FAILED_TO_START)
                        });
                    match failed {
                        Some(task) => {
                            assert_failed_fetch(row, &task, failing);
                            // A service whose scheduler cannot launch a task runs none.
                            let running = running_count(&router, row).await;
                            assert_eq!(
                                running, 0,
                                "case {case:?}: expected the service to run no task, got \
                                 runningCount {running}"
                            );
                            true
                        }
                        None => false,
                    }
                }
                None => {
                    // The row the registry serves: its task runs, and nothing of it is ever
                    // stopped for a fetch that failed.
                    let running = service_tasks(&router, row, None)
                        .await
                        .into_iter()
                        .any(|task| task["lastStatus"].as_str() == Some(RUNNING));
                    if running {
                        let reported = running_count(&router, row).await;
                        assert_eq!(
                            reported, 1,
                            "case {case:?}: expected the service to run its one task, got \
                             runningCount {reported}"
                        );
                        let stopped = service_tasks(&router, row, Some(STOPPED)).await;
                        assert!(
                            !stopped.iter().any(|task| {
                                task["stopCode"].as_str() == Some(TASK_FAILED_TO_START)
                            }),
                            "case {case:?}: expected no failed launch for an image the registry \
                             serves, got {stopped:?}"
                        );
                    }
                    running
                }
            };
            if settled {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the service's task to settle on what the registry \
                 answered about its image, got the tasks {:?} with runningCount {}",
                service_tasks(&router, row, None).await,
                running_count(&router, row).await
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The fixed text of the `stoppedReason` the agent words for the Amazon ECR registry
/// AUTHENTICATION it could not retrieve — the execution resource it obtains before any
/// manifest is fetched — followed by the class Amazon ECR refuses an unauthorized call with.
/// Spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
const REGISTRY_AUTH_RETRIEVAL_FAILED: &str =
    "ResourceInitializationError: unable to pull secrets or registry auth: execution resource \
     retrieval failed: unable to retrieve ecr registry auth: service call has been retried 1 \
     time(s): AccessDeniedException: ";

/// What the refusal inside that reason names: the registry-level authorization token, decided
/// on no resource narrower than the registry — not the manifest call the fetch itself makes.
const WITHHELD_REGISTRY_CALL: &str =
    "is not authorized to perform: ecr:GetAuthorizationToken on resource: *";

/// Assert everything the stop of a task whose execution role could not obtain the registry's
/// authorization token carries: the agent's account of the execution resource it could not
/// retrieve, Amazon ECR's own refusal of the registry-level call inside it — naming the
/// SESSION the agent assumed `execution_role` in for this task, which is the principal AWS
/// reports for a call made under one — `says` where the row states WHY the grant was
/// withheld, and nothing of an image, because none was reached.
fn assert_registry_auth_withheld(
    case: &str,
    task: &Value,
    image: &str,
    says: Option<&str>,
    execution_role: Option<&str>,
) {
    let reason = string(task, "stoppedReason", case);
    assert!(
        reason.starts_with(REGISTRY_AUTH_RETRIEVAL_FAILED),
        "case {case:?}: expected the stoppedReason to begin \
         {REGISTRY_AUTH_RETRIEVAL_FAILED:?}, got {reason:?}"
    );
    assert!(
        reason.contains(WITHHELD_REGISTRY_CALL),
        "case {case:?}: expected the stoppedReason to carry {WITHHELD_REGISTRY_CALL:?}, got \
         {reason:?}"
    );
    // The principal the refusal names: the agent retrieves the token AS the execution role,
    // so the caller is an assumed-role session of that role named after the task it is for.
    if let Some(role_arn) = execution_role {
        let role_name = role_arn.rsplit('/').next().unwrap_or_default();
        let task_arn = string(task, "taskArn", case);
        let task_id = task_arn.rsplit('/').next().unwrap_or_default().to_string();
        let expected = format!(
            "User: arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/{task_id} \
             {WITHHELD_REGISTRY_CALL}"
        );
        assert!(
            reason.contains(&expected),
            "case {case:?}: expected the stoppedReason to carry {expected:?}, got {reason:?}"
        );
    }
    if let Some(says) = says {
        assert!(
            reason.contains(says),
            "case {case:?}: expected the stoppedReason to carry {says:?}, got {reason:?}"
        );
    }
    // A fetch that was never made is not reported: the token is retrieved before any image,
    // so neither the runtime's class for a failed fetch nor the reference itself is in it.
    for absent in [CANNOT_PULL, image] {
        assert!(
            !reason.contains(absent),
            "case {case:?}: expected no {absent:?} in the stoppedReason of a task that \
             reached no image, got {reason:?}"
        );
    }
    assert!(
        task["startedAt"].is_null(),
        "case {case:?}: expected no startedAt on a task that never ran, got {task}"
    );
    // The resource the agent retrieves is the TASK's, not one container's, so the failure is
    // reported on the task alone — the way every other execution resource's is.
    for container in list(task, "containers", case) {
        assert_eq!(
            container["reason"].as_str(),
            None,
            "case {case:?}: expected no reason on a container whose image was never fetched, \
             got {container}"
        );
    }
}

/// The repository a cache rule covers in the table below, under the rule's own prefix: the
/// registry holds nothing for it until a pull caches it.
const CACHED_REPOSITORY: &str = "hub/library/alpine";

#[tokio::test]
async fn a_task_whose_role_cannot_get_the_registry_token_fails_the_resource_retrieval() {
    let router = emulator_with_ecr_and_iam();
    cluster(&router).await;
    let case = "the registry's records";

    // One repository holding one tagged image, and one pull through cache rule whose prefix
    // nothing has been cached under yet.
    ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": "app" }),
        case,
    )
    .await;
    let pushed = ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": "app",
            "imageManifest": MANIFEST,
            "imageTag": "1",
        }),
        case,
    )
    .await;
    let digest = string(&pushed["image"]["imageId"], "imageDigest", case);
    ecr(
        &router,
        "CreatePullThroughCacheRule",
        json!({
            "ecrRepositoryPrefix": "hub",
            "upstreamRegistryUrl": "registry-1.docker.io",
            "credentialArn": UPSTREAM_CREDENTIAL_ARN,
        }),
        case,
    )
    .await;
    // A repository whose own policy shares its images with a role: a resource-based policy is
    // the document a repository can be pulled from by, and it is never the document a
    // registry-level call is granted on.
    ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": "shared" }),
        case,
    )
    .await;
    ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": "shared",
            "imageManifest": MANIFEST,
            "imageTag": "1",
        }),
        case,
    )
    .await;

    // The roles, each carrying a different part of what a pull takes — and one whose document
    // DENIES the registry-level call the others are simply not granted.
    let token_and_manifest = role(
        &router,
        "tokenpull",
        Some(&grant(&["ecr:GetAuthorizationToken", "ecr:BatchGetImage"])),
    )
    .await;
    let manifest_only = role(&router, "manifestonly", Some(&grant(&["ecr:BatchGetImage"]))).await;
    let token_only = role(
        &router,
        "tokenonlypull",
        Some(&grant(&["ecr:GetAuthorizationToken"])),
    )
    .await;
    let unrelated = role(&router, "unrelated", Some(&grant(&["ecr:ListImages"]))).await;
    let denied = role(
        &router,
        "tokendenied",
        Some(
            &json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "ecr:*", "Resource": "*" },
                    {
                        "Effect": "Deny",
                        "Action": "ecr:GetAuthorizationToken",
                        "Resource": "*",
                    },
                ],
            })
            .to_string(),
        ),
    )
    .await;
    ecr(
        &router,
        "SetRepositoryPolicy",
        json!({
            "repositoryName": "shared",
            "policyText": json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": manifest_only },
                    "Action": "ecr:BatchGetImage",
                }],
            }).to_string(),
        }),
        case,
    )
    .await;

    /// The clause a refusal carries when nothing grants the call: IAM's default deny, naming
    /// the action (`service_common::iam_authorization::no_identity_policy_allows`).
    fn nothing_allows(action: &str) -> String {
        format!("because no identity-based policy allows the {action} action")
    }
    /// The clause it carries when a document of the role's own DENIES the call.
    const IDENTITY_DENY: &str = "with an explicit deny in an identity-based policy";
    let withheld = |case: &'static str, family: &'static str, image: String, role: &String| Row {
        case,
        family,
        image,
        execution_role: Some(role.clone()),
        expected: Outcome::RegistryAuthWithheld,
        says: Some(nothing_allows("ecr:GetAuthorizationToken")),
    };
    // The rows differ along what the rule generalises over: which document withholds the
    // token, which other half of the pull the role DOES hold, what the registry holds for the
    // reference, and the form the reference takes — none of which the registry-level call is
    // decided on.
    let rows = vec![
        withheld(
            "a role granted the manifest call but not the registry token",
            "manifestonlytask",
            ecr_image("app:1"),
            &manifest_only,
        ),
        withheld(
            "a role granted nothing of the pull at all",
            "unrelatedtask",
            ecr_image("app:1"),
            &unrelated,
        ),
        withheld(
            "a role the repository's own policy shares the image with",
            "sharedreptask",
            ecr_image("shared:1"),
            &manifest_only,
        ),
        withheld(
            "a reference this registry holds no repository for",
            "absentreptask",
            ecr_image("absent:1"),
            &manifest_only,
        ),
        withheld(
            "a reference a pull through cache rule covers",
            "cachedtask",
            ecr_image(&format!("{CACHED_REPOSITORY}:latest")),
            &manifest_only,
        ),
        withheld(
            "a reference addressing a digest rather than a tag",
            "digesttask",
            ecr_image(&format!("app@{digest}")),
            &manifest_only,
        ),
        Row {
            says: Some(IDENTITY_DENY.to_string()),
            ..withheld(
                "a role whose own document denies the registry token",
                "tokendeniedtask",
                ecr_image("app:1"),
                &denied,
            )
        },
        // The OTHER half of the pull, to keep the two steps apart: a role that obtained the
        // token and was refused the manifest fails at the FETCH, which the runtime reports as
        // the request it made and the status the registry answered it with.
        Row {
            says: Some(format!(
                "unexpected status from HEAD request to \
                 https://{ACCOUNT}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/v2/app/manifests/1: \
                 403 Forbidden"
            )),
            ..withheld(
                "a role granted the registry token but not the manifest",
                "tokenonlytask",
                ecr_image("app:1"),
                &token_only,
            )
        }
        .with_outcome(Outcome::FailedToStart),
        // And the rows nothing is withheld from: both calls granted, no role at all to decide
        // anything against, and a reference into a registry this emulator does not hold, for
        // which no token of ours is retrieved.
        Row {
            says: None,
            ..withheld(
                "a role granted both calls a pull makes",
                "fullgranttask",
                ecr_image("app:1"),
                &token_and_manifest,
            )
        }
        .with_outcome(Outcome::Ran),
        Row {
            case: "no execution role at all",
            family: "noroletask",
            image: ecr_image("app:1"),
            execution_role: None,
            expected: Outcome::Ran,
            says: None,
        },
        Row {
            case: "a registry this emulator holds nothing of, pulled by an ungranted role",
            family: "publictask",
            image: "public.ecr.aws/nginx/nginx:latest".to_string(),
            execution_role: Some(unrelated.clone()),
            expected: Outcome::Ran,
            says: None,
        },
    ];

    // Every task is launched first and polled afterwards: each settles on its own clock, so
    // the whole table costs one run rather than one per row.
    let mut launched = Vec::new();
    for row in &rows {
        let task_arn = run(
            &router,
            row.family,
            &row.image,
            row.execution_role.as_deref(),
            row.case,
        )
        .await;
        launched.push((row, task_arn));
    }

    for (row, task_arn) in launched {
        let case = row.case;
        let task = stopped(&router, &task_arn, case).await;
        let stop_code = string(&task, "stopCode", case);
        match row.expected {
            Outcome::RegistryAuthWithheld => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task whose registry authentication was \
                     withheld to stop with {TASK_FAILED_TO_START}, got {task}"
                );
                assert_registry_auth_withheld(
                    case,
                    &task,
                    &row.image,
                    row.says.as_deref(),
                    row.execution_role.as_deref(),
                );
            }
            Outcome::FailedToStart => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task that could not pull {} to stop with \
                     {TASK_FAILED_TO_START}, got {task}",
                    row.image
                );
                let reason = string(&task, "stoppedReason", case);
                // The token WAS obtained, so what failed is the fetch: the runtime's class,
                // naming the reference it could not resolve.
                for expected in [CANNOT_PULL, row.image.as_str()] {
                    assert!(
                        reason.contains(expected),
                        "case {case:?}: expected the stoppedReason to carry {expected:?}, got \
                         {reason:?}"
                    );
                }
                assert!(
                    !reason.starts_with("ResourceInitializationError"),
                    "case {case:?}: expected a fetch failure rather than a resource \
                     retrieval, got {reason:?}"
                );
                if let Some(says) = row.says.as_deref() {
                    assert!(
                        reason.contains(says),
                        "case {case:?}: expected the stoppedReason to carry {says:?}, got \
                         {reason:?}"
                    );
                }
            }
            Outcome::Ran => {
                assert_eq!(
                    stop_code, ESSENTIAL_CONTAINER_EXITED,
                    "case {case:?}: expected a task that pulled {} to end with \
                     {ESSENTIAL_CONTAINER_EXITED}, got {task}",
                    row.image
                );
                assert!(
                    !task["startedAt"].is_null(),
                    "case {case:?}: expected a startedAt on a task that ran, got {task}"
                );
            }
        }
    }

    // A pull that was never made cached nothing: the repository the covered reference names
    // is still absent from the registry, read back through Amazon ECR's own report.
    let case = "the covered repository";
    let listed = ecr(&router, "DescribeRepositories", json!({}), case).await;
    let names: Vec<String> = list(&listed, "repositories", case)
        .iter()
        .map(|repository| string(repository, "repositoryName", case))
        .collect();
    assert!(
        !names.iter().any(|name| name == CACHED_REPOSITORY),
        "case {case:?}: expected no repository to be cached for a task whose registry \
         authentication was withheld, got {names:?}"
    );
}
