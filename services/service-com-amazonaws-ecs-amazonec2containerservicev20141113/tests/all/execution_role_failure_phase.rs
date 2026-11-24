// SPDX-License-Identifier: CC0-1.0

//! WHICH phase of the task execution role's work a denial fails a launch in, as the task's
//! `Task$stoppedReason` names it.
//!
//! The rule under test, over the whole class rather than one launch:
//!
//! > Amazon ECS retrieves a task's execution RESOURCES before it creates any container, as the
//! > task's execution role ("the task execution role that grants the Amazon ECS container
//! > agent permission to make Amazon Web Services API calls on your behalf",
//! > `TaskDefinition$executionRoleArn`), and the Amazon ECR registry's authorization token is
//! > one of them: it is obtained on the REGISTRY, before any manifest of any image is asked
//! > for. So a role refused `ecr:GetAuthorizationToken` fails the task in that retrieval step
//! > — the task stops with `Task$stopCode` `TaskFailedToStart`
//! > (`smithy_query(query="shape",shape="TaskStopCode")`) and a `Task$stoppedReason` of the
//! > agent's `ResourceInitializationError` class naming the registry authentication it could
//! > not retrieve, carrying Amazon ECR's own refusal of that registry-level call, which names
//! > the session the role was assumed in for the task. The container runtime's
//! > `CannotPullContainerError` is the NEXT phase's class — a fetch that was made with a token
//! > and refused — so it is absent from such a reason; nothing of the container ran, so it
//! > reports no exit code and the `awslogs` stream its output would have gone to is never
//! > created.
//!
//! The rows span what the rule generalises over: the FORM the image reference takes (the
//! address Amazon ECR announced for the repository, the registry host AWS publishes, a path a
//! pull through cache rule covers, and a digest rather than a tag), and what else is in force
//! on the role (the repository-scoped pull actions, the logger's actions, or neither). Two
//! further rows keep the phases apart: a role that DID obtain the token and was refused the
//! repository's own actions fails at the fetch, under the runtime's class, and a role granted
//! both halves runs and delivers.
//!
//! Every fact is created through the owning service's own API — the repository, the image and
//! the pull through cache rule through Amazon ECR's, the role and its policy through IAM's,
//! the log group through CloudWatch Logs' — and every value asserted is read back over the
//! wire through `DescribeTasks`, `DescribeLogStreams` and `DescribeRepositories`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{ecr, iam, list, ok, register_instance, string, ACCOUNT, SIGNED_REGION};

/// The cluster every task below runs in.
const CLUSTER: &str = "execphase";

/// The EC2 instance id the cluster's one container instance is registered under: a task
/// placed with the default `EC2` launch type runs on the capacity registered to its cluster.
const CAPACITY_INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c3";

/// The repository this account's registry HOLDS, created through Amazon ECR's own API.
const HELD_REPOSITORY: &str = "app";

/// The tag the held repository's one image is pushed under.
const HELD_TAG: &str = "1";

/// The prefix the pull through cache rule below covers, and the path under it a reference
/// addresses — the form a cache rule gives a repository of an upstream registry's image.
const CACHE_PREFIX: &str = "hub";
const CACHED_PATH: &str = "hub/docker/library/busybox";

/// The Secrets Manager secret the pull through cache rule reads its upstream credentials
/// from, in the namespace Amazon ECR's `CredentialArn` constrains one to
/// (`^arn:aws(-\w+)*:secretsmanager:[a-zA-Z0-9-:]+:secret:ecr\-pullthroughcache\/[a-zA-Z0-9\/_+=.@-]+$`).
const UPSTREAM_CREDENTIAL_ARN: &str =
    "arn:aws:secretsmanager:us-west-2:000000000000:secret:ecr-pullthroughcache/docker-hub-AbCdEf";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is which phase
/// a denial fails the launch in, not which blobs an image is built from.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#
);

/// The log group every container below delivers through the `awslogs` driver to, created
/// through CloudWatch Logs' own API.
const LOG_GROUP: &str = "/emulator/ecs/execution-role-phase";

/// The `awslogs-stream-prefix` every configuration names, and so the first segment of the
/// stream a container that RAN delivered to.
const STREAM_PREFIX: &str = "ecs";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The program that container runs: it ends at once, so a task that was launched at all
/// stops for the work it did rather than being still running when it is read.
const PROGRAM_THAT_ENDS: [&str; 3] = ["sh", "-c", "exit 0"];

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to
/// do it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The registry-level call the agent makes to retrieve a task's registry authentication,
/// granted on no resource narrower than the registry (IAM Service Authorization Reference,
/// "Actions defined by Amazon Elastic Container Registry").
const REGISTRY_TOKEN_ACTION: &str = "ecr:GetAuthorizationToken";

/// The calls a pull makes on the REPOSITORY the reference names — the other half of a pull's
/// authorization, and the half a cache rule's import is covered by (IAM Service Authorization
/// Reference, "Actions defined by Amazon Elastic Container Registry").
const REPOSITORY_PULL_ACTIONS: [&str; 4] = [
    "ecr:BatchCheckLayerAvailability",
    "ecr:GetDownloadUrlForLayer",
    "ecr:BatchGetImage",
    "ecr:BatchImportUpstreamImage",
];

/// The calls the `awslogs` driver makes under the execution role before the container it is
/// for runs (IAM Service Authorization Reference, "Actions defined by Amazon CloudWatch
/// Logs").
const LOGGER_ACTIONS: [&str; 2] = ["logs:CreateLogStream", "logs:PutLogEvents"];

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// `Task$stopCode` of a task whose essential container exited — the end of a task that ran.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// How the agent's account of the execution resource it could not retrieve OPENS for the
/// Amazon ECR registry authentication, followed by the class Amazon ECR refuses an
/// unauthorized call with. Spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make this suite pass.
const REGISTRY_AUTH_RETRIEVAL_FAILED: &str =
    "ResourceInitializationError: unable to pull secrets or registry auth: execution resource \
     retrieval failed: unable to retrieve ecr registry auth: service call has been retried 1 \
     time(s): AccessDeniedException: ";

/// The phrase that names WHICH execution resource could not be retrieved — the registry
/// authentication, not an image.
const REGISTRY_AUTH_RESOURCE: &str = "unable to retrieve ecr registry auth";

/// The refusal class Amazon ECR answers an unauthorized call with, carried inside that
/// reason.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The error class the container RUNTIME reports a failed image fetch under: the next phase's
/// class, which a task that never obtained a token has not reached.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// How the class of a failed fetch opens, so a reason of the retrieval phase can be asserted
/// not to be one.
const RESOURCE_INITIALIZATION_ERROR: &str = "ResourceInitializationError:";

/// How the agent's account of a logger it could not validate opens — the phase AFTER the
/// resource retrieval, which a task refused its registry token never reaches.
const LOGGER_FAILURE: &str = "failed to validate logger args";

/// The HTTP status a registry answers a manifest request no authorization covers with, which
/// is what the runtime reports for a fetch that WAS made.
const FORBIDDEN_STATUS: &str = "403 Forbidden";

/// How often a poll re-reads a task.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying before it gives up. Generous: a task that fails to launch is
/// stopped at the instant it would otherwise have started, one that runs is stopped when the
/// work it was placed to do is over, and this suite runs beside every other test of the
/// workspace.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// A router carrying Amazon ECS and the three services a launch of these rows is decided
/// across — Amazon ECR, which serves the pull and answers for the registry's authorization
/// token; IAM, which holds the policies in force on the execution role; and CloudWatch Logs,
/// which owns the log group and the stream the `awslogs` driver creates — over ONE shared
/// engine, mounted the way the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One CloudWatch Logs call, the way a client makes it.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /logs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The names of every stream of the log group above, and an empty list for a group this
/// account does not hold at all.
async fn streams(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = logs(
        router,
        "DescribeLogStreams",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["logStreams"]
        .as_array()
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// An identity-based document with one `Allow` statement per `(actions, resource)` pair.
fn document(statements: &[(&[&str], &str)]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": statements
            .iter()
            .map(|(actions, resource)| json!({
                "Effect": "Allow",
                "Action": actions,
                "Resource": resource,
            }))
            .collect::<Vec<Value>>(),
    })
    .to_string()
}

/// Create the role `name`, trusted by Amazon ECS and carrying `policy` as its one inline
/// document — through IAM's own API — and answer the role's ARN.
async fn role(router: &MultiServiceRouter, name: &str, policy: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": [ECS_TASKS_PRINCIPAL] },
            "Action": [ASSUME_ROLE],
        }],
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
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Which phase a row's launch must fail in — or that it fails in none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The execution-resource retrieval: the registry's authorization token was withheld, so
    /// no image was ever asked for.
    RegistryAuthRetrieval,
    /// The image FETCH: a token was obtained and the registry refused the manifest request
    /// the pull made.
    ImageFetch,
    /// Neither: the task launched, ran and ended for the work it did.
    None,
}

/// One row: a revision running `image` as the execution role `role_arn`, and the phase its
/// launch must fail in.
struct Row {
    case: &'static str,
    family: &'static str,
    image: String,
    role_arn: String,
    phase: Phase,
}

/// Register one revision of `row`'s family — one container running `row`'s image under
/// `row`'s execution role, delivering through the `awslogs` driver — and run one task of it,
/// answering the task's ARN.
async fn run(router: &MultiServiceRouter, row: &Row) -> String {
    let definition = json!({
        "family": row.family,
        "executionRoleArn": row.role_arn,
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": row.image,
            "memory": 128,
            "essential": true,
            "command": PROGRAM_THAT_ENDS,
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": LOG_GROUP,
                    "awslogs-region": SIGNED_REGION,
                    "awslogs-stream-prefix": STREAM_PREFIX,
                },
            },
        }],
    });
    ok(router, "RegisterTaskDefinition", definition, row.case).await;
    let body = ok(
        router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": row.family }),
        row.case,
    )
    .await;
    let tasks = list(&body, "tasks", row.case);
    let case = row.case;
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(task, "taskArn", row.case)
}

/// Read one task back through `DescribeTasks` again until it reaches the terminal `STOPPED`
/// status, and answer the record it stopped as.
async fn stopped(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ok(
            router,
            "DescribeTasks",
            json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
            case,
        )
        .await;
        let task = list(&body, "tasks", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"));
        if task["lastStatus"].as_str() == Some("STOPPED") {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to stop, it was still {:?}: {task}",
            task["lastStatus"]
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The container `CONTAINER` of one described task.
fn container_of(task: &Value, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(CONTAINER))
        .unwrap_or_else(|| panic!("case {case:?}: expected the container {CONTAINER:?} on {task}"))
}

/// RULE: a denial of the registry-level authorization token fails a task in the
/// execution-ROLE resource-initialization phase, whatever spelling the reference carries and
/// whatever the registry holds for it; the runtime's `CannotPullContainerError` is reserved
/// for a fetch that a token was obtained for.
#[tokio::test]
async fn a_denied_registry_token_fails_the_task_in_the_resource_retrieval_phase() {
    let router = emulator();
    let case = "the facts every row is decided against";
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    register_instance(&router, CLUSTER, CAPACITY_INSTANCE_ID, case).await;
    let (status, created) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the log group every row delivers into to be created, got \
         {status} with {created}"
    );

    // One repository holding one tagged image, and one pull through cache rule covering a
    // prefix nothing has been cached under yet — both created through Amazon ECR's own API.
    let repository = ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": HELD_REPOSITORY }),
        case,
    )
    .await;
    // The ADDRESS that registry announced for the repository, which is what a client writes
    // into a container definition: `Repository$repositoryUri` is "The URI for the repository.
    // You can use this URI for container image push and pull operations".
    let announced = string(&repository["repository"], "repositoryUri", case);
    let authority = announced
        .split_once('/')
        .map(|(authority, _)| authority.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: expected an authority on {announced:?}"));
    let pushed = ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": HELD_REPOSITORY,
            "imageManifest": MANIFEST,
            "imageTag": HELD_TAG,
        }),
        case,
    )
    .await;
    let digest = string(&pushed["image"]["imageId"], "imageDigest", case);
    ecr(
        &router,
        "CreatePullThroughCacheRule",
        json!({
            "ecrRepositoryPrefix": CACHE_PREFIX,
            "upstreamRegistryUrl": "registry-1.docker.io",
            "credentialArn": UPSTREAM_CREDENTIAL_ARN,
        }),
        case,
    )
    .await;

    // The roles, each holding a different part of what a launch takes. The repository-scoped
    // grants are written on the repository ARNs a pull of these references names, which is how
    // such a policy is scoped — and none of them says anything about the registry-level call.
    let held_repository_arn =
        format!("arn:aws:ecr:{SIGNED_REGION}:{ACCOUNT}:repository/{HELD_REPOSITORY}");
    let cached_repository_arn =
        format!("arn:aws:ecr:{SIGNED_REGION}:{ACCOUNT}:repository/{CACHE_PREFIX}/*");
    let repository_and_logger = role(
        &router,
        "repositoryandlogger",
        &document(&[
            (&REPOSITORY_PULL_ACTIONS, &held_repository_arn),
            (&REPOSITORY_PULL_ACTIONS, &cached_repository_arn),
            (&LOGGER_ACTIONS, "*"),
        ]),
    )
    .await;
    let repository_only = role(
        &router,
        "repositoryonly",
        &document(&[(&REPOSITORY_PULL_ACTIONS, &held_repository_arn)]),
    )
    .await;
    let token_and_logger = role(
        &router,
        "tokenandlogger",
        &document(&[(&[REGISTRY_TOKEN_ACTION], "*"), (&LOGGER_ACTIONS, "*")]),
    )
    .await;
    let everything = role(
        &router,
        "everything",
        &document(&[
            (&[REGISTRY_TOKEN_ACTION], "*"),
            (&REPOSITORY_PULL_ACTIONS, "*"),
            (&LOGGER_ACTIONS, "*"),
        ]),
    )
    .await;

    // The rows differ along the FORM the reference takes, what else is in force on the role,
    // and which phase the launch must therefore fail in.
    let rows = vec![
        Row {
            case: "the address the registry announced for the repository",
            family: "announcedref",
            image: format!("{announced}:{HELD_TAG}"),
            role_arn: repository_and_logger.clone(),
            phase: Phase::RegistryAuthRetrieval,
        },
        Row {
            case: "the registry host AWS publishes",
            family: "publishedref",
            image: format!(
                "{ACCOUNT}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/{HELD_REPOSITORY}:{HELD_TAG}"
            ),
            role_arn: repository_and_logger.clone(),
            phase: Phase::RegistryAuthRetrieval,
        },
        Row {
            case: "a path a pull through cache rule covers",
            family: "cachedref",
            image: format!("{authority}/{CACHED_PATH}:latest"),
            role_arn: repository_and_logger.clone(),
            phase: Phase::RegistryAuthRetrieval,
        },
        Row {
            case: "a reference addressing a digest rather than a tag",
            family: "digestref",
            image: format!("{announced}@{digest}"),
            role_arn: repository_and_logger.clone(),
            phase: Phase::RegistryAuthRetrieval,
        },
        // The retrieval is the FIRST phase: a role that holds neither the token nor the
        // logger's actions fails on the token, not on the logger it never got to.
        Row {
            case: "a role holding neither the registry token nor the logger's actions",
            family: "repositoryonlyref",
            image: format!("{announced}:{HELD_TAG}"),
            role_arn: repository_only.clone(),
            phase: Phase::RegistryAuthRetrieval,
        },
        // The OTHER phase: the token WAS obtained, so what failed is the fetch itself.
        Row {
            case: "a role granted the registry token but not the repository's own actions",
            family: "tokenonlyref",
            image: format!("{announced}:{HELD_TAG}"),
            role_arn: token_and_logger.clone(),
            phase: Phase::ImageFetch,
        },
        // And the row nothing is withheld from.
        Row {
            case: "a role granted both halves of the pull and the logger's actions",
            family: "fullgrantref",
            image: format!("{announced}:{HELD_TAG}"),
            role_arn: everything.clone(),
            phase: Phase::None,
        },
    ];

    // Every task is launched first and read back afterwards: each settles on its own clock,
    // so the whole table costs one wait rather than one per row.
    let mut launched = Vec::new();
    for row in &rows {
        let task_arn = run(&router, row).await;
        launched.push((row, task_arn));
    }

    for (row, task_arn) in &launched {
        let case = row.case;
        let task = stopped(&router, task_arn, case).await;
        let stop_code = string(&task, "stopCode", case);
        let reason = string(&task, "stoppedReason", case);
        match row.phase {
            Phase::RegistryAuthRetrieval => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task whose registry authentication was withheld \
                     to stop with {TASK_FAILED_TO_START}, got {stop_code:?}: {task}"
                );
                // The agent's account of the execution resource it could not retrieve, with
                // Amazon ECR's refusal of the registry-level call inside it.
                assert!(
                    reason.starts_with(REGISTRY_AUTH_RETRIEVAL_FAILED),
                    "case {case:?}: expected the stoppedReason to begin \
                     {REGISTRY_AUTH_RETRIEVAL_FAILED:?}, got {reason:?}"
                );
                for expected in [REGISTRY_AUTH_RESOURCE, ACCESS_DENIED, REGISTRY_TOKEN_ACTION] {
                    assert!(
                        reason.contains(expected),
                        "case {case:?}: expected the stoppedReason to carry {expected:?}, got \
                         {reason:?}"
                    );
                }
                // The principal that refusal names: the agent retrieves the token AS the
                // execution role, so the caller is an assumed-role session of that role named
                // after the task it is for.
                let role_name = row.role_arn.rsplit('/').next().unwrap_or_default();
                let task_id = task_arn.rsplit('/').next().unwrap_or_default();
                let session = format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/{task_id}");
                assert!(
                    reason.contains(&session),
                    "case {case:?}: expected the stoppedReason to name the session \
                     {session:?}, got {reason:?}"
                );
                // The next phase's class, and the phase after that one, are both absent: no
                // fetch was made and no logger was validated.
                for absent in [CANNOT_PULL, LOGGER_FAILURE] {
                    assert!(
                        !reason.contains(absent),
                        "case {case:?}: expected no {absent:?} in the stoppedReason of a task \
                         that never obtained a registry token, got {reason:?}"
                    );
                }
                assert!(
                    task["startedAt"].is_null(),
                    "case {case:?}: expected no startedAt on a task that never ran, got {task}"
                );
                let container = container_of(&task, case);
                assert!(
                    container["exitCode"].is_null(),
                    "case {case:?}: expected no exitCode on a container that never ran, got \
                     {container}"
                );
            }
            Phase::ImageFetch => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task whose image could not be fetched to stop \
                     with {TASK_FAILED_TO_START}, got {stop_code:?}: {task}"
                );
                // A token WAS obtained, so the failure is the runtime's account of the
                // request it made and the status the registry answered it with.
                for expected in [CANNOT_PULL, FORBIDDEN_STATUS, row.image.as_str()] {
                    assert!(
                        reason.contains(expected),
                        "case {case:?}: expected the stoppedReason to carry {expected:?}, got \
                         {reason:?}"
                    );
                }
                for absent in [RESOURCE_INITIALIZATION_ERROR, REGISTRY_AUTH_RESOURCE] {
                    assert!(
                        !reason.contains(absent),
                        "case {case:?}: expected no {absent:?} in the stoppedReason of a fetch \
                         that was made with a token, got {reason:?}"
                    );
                }
            }
            Phase::None => {
                assert_eq!(
                    stop_code, ESSENTIAL_CONTAINER_EXITED,
                    "case {case:?}: expected a task nothing was withheld from to end with \
                     {ESSENTIAL_CONTAINER_EXITED}, got {stop_code:?}: {task}"
                );
                assert!(
                    !task["startedAt"].is_null(),
                    "case {case:?}: expected a startedAt on a task that ran, got {task}"
                );
            }
        }
    }

    // ONE stream exists in the group: the container of the row that RAN delivered to it, and
    // no task that never obtained a registry token had a logger initialised at all.
    let case = "the streams the group holds";
    let held = streams(&router).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the one container that ran to have created the only stream, \
         got {held:?}"
    );
    assert!(
        held[0].starts_with(&format!("{STREAM_PREFIX}/{CONTAINER}/")),
        "case {case:?}: expected the stream of the container that ran, got {held:?}"
    );

    // A pull that was never made imported nothing: the repository the covered reference names
    // is still absent from the registry, read back through Amazon ECR's own report.
    let case = "the covered repository";
    let listed = ecr(&router, "DescribeRepositories", json!({}), case).await;
    let names: Vec<String> = list(&listed, "repositories", case)
        .iter()
        .map(|repository| string(repository, "repositoryName", case))
        .collect();
    assert!(
        !names.iter().any(|name| name == CACHED_PATH),
        "case {case:?}: expected no repository to be cached for a task whose registry \
         authentication was withheld, got {names:?}"
    );
}
