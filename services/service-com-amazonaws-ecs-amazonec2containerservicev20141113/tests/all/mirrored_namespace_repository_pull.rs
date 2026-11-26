// SPDX-License-Identifier: CC0-1.0

//! Which REFERENCES the registry a bare image name resolves against ANSWERS for, and what a
//! rollout onto one it answers "no such repository" to does — driven end to end over the
//! shared wire harness in [`crate::ecs_wire`], so every value asserted is read back through
//! `DescribeTasks`, `DescribeServices`, `DescribeServiceDeployments` and
//! `ListServiceDeployments`.
//!
//! The rule under test: **a container runs an image, and the image is fetched from the
//! registry its reference names before the container starts, so a registry this emulator
//! holds answers for everything in the namespace it holds — a reference into the mirrored
//! official-images namespace naming a repository that namespace publishes none of resolves to
//! no image at all, exactly as a tag a held repository publishes no release under does, and
//! the task never starts.** A repository that namespace DOES publish whose releases this
//! emulator holds none of is a repository that exists and nothing here knows the releases of,
//! so it is left undecided and the task runs, as a reference into another namespace, another
//! gallery alias or another registry altogether is.
//!
//! `Task$stopCode` is "the stop code indicating why a task was stopped" and the `TaskStopCode`
//! member for a task that failed before its containers started is `TaskFailedToStart`
//! (`smithy_query(query="shape",shape="TaskStopCode")`); `Task$stoppedReason` ("The reason
//! that the task was stopped") carries the container runtime's `CannotPullContainerError` for
//! the reference it could not resolve, and the two conditions are different answers from the
//! registry: a repository it does not publish denies the pull its scope ("pull access denied,
//! repository does not exist"), while a repository it does publish answers a tag it holds no
//! release under with a manifest that is "not found". `Task$startedAt` ("the Unix timestamp
//! for the time when the task started") stays absent for a task that never ran.
//!
//! The rows of the first test differ along what the rule generalises over: HOW the reference
//! spells the registry (no host at all, the namespace written out, the registry's two hosts,
//! the gallery alias that mirrors it), WHICH repository it names (one the namespace publishes
//! no repository under, one it publishes whose releases are held, one it publishes whose
//! releases are not held, one under somebody else's namespace) and WHAT the registry then
//! answers (the image, no such release, no such repository, nothing at all).
//!
//! The second test is the same rule under the DEPLOYMENT CIRCUIT BREAKER: a service rolled
//! onto a revision whose image no registry publishes is one whose every launch fails, so
//! `Deployment$rolloutState` reaches `FAILED` ("if the service fails to reach a steady state
//! and circuit breaker is turned on, the deployment transitions to a `FAILED` state") and
//! `DeploymentCircuitBreaker$rollback` takes the service back to "the last deployment that
//! completed successfully", which the scheduler then runs at the count the service wants.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, count, create_cluster, list, ok, poll_until, present_count,
    service, string,
};

/// The cluster every task and service below runs in.
const CLUSTER: &str = "repositorypull";

/// A repository name the mirrored official-images namespace publishes nothing under: the
/// namespace's catalogue is the list of those images, and no image is published under this
/// name.
const UNPUBLISHED_REPOSITORY: &str = "no-such-official-image";

/// A release of a repository this emulator holds the image of, which is fetched.
const HELD_RELEASE: &str = "busybox:1.36";

/// A tag that same repository publishes no release under.
const UNRELEASED_TAG: &str = "busybox:no-such-release";

/// `Task$lastStatus` members this suite names.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// The error class a container runtime words a failed image fetch with.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// What the runtime reports when the registry publishes no such repository: it grants the pull
/// scope of a repository it holds, so a request for one it holds none of comes back with no
/// such scope rather than with a missing manifest.
const REPOSITORY_DOES_NOT_EXIST: &str =
    "pull access denied, repository does not exist or may require authorization";

/// What it reports for a repository that IS published, under a tag that publishes no release:
/// the manifest the fetch asked for resolves to nothing.
const NOT_FOUND: &str = "not found";

/// A program that keeps the container up for as long as its task lives, so a task whose image
/// WAS fetched is told from one whose was not by the status it settles at rather than by how
/// quickly its program ended.
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// What the registry answers about one row's reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The fetch is refused and the task never starts, with this condition in the reason.
    Refused(&'static str),
    /// Nothing here decides the reference, or the image is held: the task runs.
    Runs,
}

/// One row: the reference a revision names and what the registry answers about it.
struct Row {
    case: &'static str,
    family: &'static str,
    image: &'static str,
    answer: Answer,
}

/// The rows, each its own revision in the one cluster.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bare name the namespace publishes no repository under",
            family: "pullbarename",
            image: UNPUBLISHED_REPOSITORY,
            answer: Answer::Refused(REPOSITORY_DOES_NOT_EXIST),
        },
        Row {
            case: "the same name under the namespace it would live in",
            family: "pullnamespaced",
            image: "library/no-such-official-image:1.0",
            answer: Answer::Refused(REPOSITORY_DOES_NOT_EXIST),
        },
        Row {
            case: "the same under the registry's own host",
            family: "pullhosted",
            image: "docker.io/library/no-such-official-image:1.0",
            answer: Answer::Refused(REPOSITORY_DOES_NOT_EXIST),
        },
        Row {
            case: "the same under that registry's index host",
            family: "pullindexhost",
            image: "index.docker.io/no-such-official-image:latest",
            answer: Answer::Refused(REPOSITORY_DOES_NOT_EXIST),
        },
        Row {
            case: "the same under the gallery alias that mirrors the namespace",
            family: "pullmirroralias",
            image: "public.ecr.aws/docker/library/no-such-official-image:1.0",
            answer: Answer::Refused(REPOSITORY_DOES_NOT_EXIST),
        },
        Row {
            case: "a tag a held repository publishes no release under",
            family: "pullunreleased",
            image: UNRELEASED_TAG,
            answer: Answer::Refused(NOT_FOUND),
        },
        Row {
            case: "a release of a held repository",
            family: "pullheld",
            image: HELD_RELEASE,
            answer: Answer::Runs,
        },
        Row {
            // `maven` and `redis` are repositories that namespace publishes and this emulator
            // holds no image of, so what they publish is not known here and nothing is
            // decided about the reference.
            case: "a repository the namespace publishes whose releases are not held",
            family: "pullunheldrelease",
            image: "maven:3",
            answer: Answer::Runs,
        },
        Row {
            case: "the same under the namespace, by another repository",
            family: "pullunheldnamespaced",
            image: "library/redis:7",
            answer: Answer::Runs,
        },
        Row {
            case: "a repository under somebody else's namespace of that registry",
            family: "pullothernamespace",
            image: "someone/no-such-official-image:1.0",
            answer: Answer::Runs,
        },
        Row {
            case: "a repository of another registry altogether",
            family: "pullotherregistry",
            image: "registry.example.com/no-such-official-image:1.0",
            answer: Answer::Runs,
        },
        Row {
            case: "a gallery alias no image is held of",
            family: "pullotheralias",
            image: "public.ecr.aws/nginx/nginx:latest",
            answer: Answer::Runs,
        },
    ]
}

/// Register one Fargate-compatible `awsvpc` revision of `family` running `image` with
/// `command`, and answer its ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    image: &str,
    command: &[&str],
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "containerDefinitions": [{
                "name": "app",
                "image": image,
                "memory": 128,
                "essential": true,
                "command": command,
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Run one task of `family` and answer its ARN.
async fn run(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "count": 1,
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(task, "taskArn", case)
}

/// One task, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

/// That task once it has SETTLED — running, or stopped — read again until it has.
async fn settled_task(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    poll_until(
        case,
        "the task to settle at RUNNING or STOPPED",
        || describe_task(router, task_arn, case),
        |task| {
            let status = task["lastStatus"].as_str().unwrap_or_default();
            status == RUNNING || status == STOPPED
        },
    )
    .await
}

#[tokio::test]
async fn the_registry_answers_a_reference_into_the_namespace_it_mirrors() {
    let router = service();
    create_cluster(&router, CLUSTER, "repository pull").await;

    let rows = rows();
    for row in &rows {
        register(&router, row.family, row.image, &STAYS_UP, row.case).await;
    }
    // Every task is placed first and read afterwards: each settles on its own clock, so the
    // whole table costs one start rather than one per row.
    let mut placed = Vec::new();
    for row in &rows {
        placed.push(run(&router, row.family, row.case).await);
    }

    for (row, task_arn) in rows.iter().zip(&placed) {
        let case = row.case;
        let task = settled_task(&router, task_arn, case).await;
        match row.answer {
            Answer::Refused(condition) => {
                assert_eq!(
                    string(&task, "lastStatus", case),
                    STOPPED,
                    "case {case:?}: expected a task whose image {} cannot be fetched to be \
                     {STOPPED}, got {task}",
                    row.image
                );
                assert_eq!(
                    string(&task, "stopCode", case),
                    TASK_FAILED_TO_START,
                    "case {case:?}: expected stopCode {TASK_FAILED_TO_START}, got {task}"
                );
                let reason = string(&task, "stoppedReason", case);
                assert!(
                    reason.contains(CANNOT_PULL),
                    "case {case:?}: expected the stoppedReason to report {CANNOT_PULL}, got \
                     {reason:?}"
                );
                assert!(
                    reason.contains(condition),
                    "case {case:?}: expected the stoppedReason to report {condition:?}, got \
                     {reason:?}"
                );
                assert!(
                    reason.contains(row.image),
                    "case {case:?}: expected the stoppedReason to name the reference {:?}, got \
                     {reason:?}",
                    row.image
                );
                assert!(
                    task.get("startedAt").is_none_or(Value::is_null),
                    "case {case:?}: expected no startedAt on a task that never ran, got {task}"
                );
                for container in list(&task, "containers", case) {
                    assert_eq!(
                        present_count(&container, "exitCode", case),
                        None,
                        "case {case:?}: expected no exit code from a container that never ran, \
                         got {container}"
                    );
                }
            }
            Answer::Runs => {
                assert_eq!(
                    string(&task, "lastStatus", case),
                    RUNNING,
                    "case {case:?}: expected a task whose reference {} no registry here refuses \
                     to run, got {task}",
                    row.image
                );
            }
        }
    }
}

/// The service the breaker watches, and the family it rolls out.
const SERVICE: &str = "breakerrepository";
const FAMILY: &str = "breakerrepositoryapp";

/// `Deployment$rolloutState` members this suite names.
const IN_PROGRESS: &str = "IN_PROGRESS";
const COMPLETED: &str = "COMPLETED";
const FAILED: &str = "FAILED";

/// `ServiceDeployment$status` of a rollout the breaker rolled back.
const ROLLBACK_SUCCESSFUL: &str = "ROLLBACK_SUCCESSFUL";

/// What a rollout the circuit breaker failed names in `Deployment$rolloutStateReason`: the
/// monitor that failed it.
const BREAKER: &str = "circuit breaker";

/// The count the service keeps running.
const DESIRED: i64 = 1;

/// One service, as `DescribeServices` reports it.
async fn describe_service(router: &MultiServiceRouter, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [SERVICE] }),
        case,
    )
    .await;
    list(&body, "services", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {body}"))
}

/// The service's current (`PRIMARY`) deployment, which is the first of the ones it reports.
fn primary(service: &Value, case: &str) -> Value {
    list(service, "deployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {service}"))
}

#[tokio::test]
async fn the_breaker_rolls_back_a_rollout_onto_a_repository_the_registry_does_not_publish() {
    let case = "breaker rollback";
    let router = service();
    create_cluster(&router, CLUSTER, case).await;

    // The revision the service CAN run: a release the registry that mirrors it holds, whose
    // container waits for the stop its task is sent, so the rollout it is on reaches a steady
    // state the breaker's rollback has something to put back.
    let first = register(&router, FAMILY, HELD_RELEASE, &["sleep", "3600"], case).await;

    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": first,
            "desiredCount": DESIRED,
            "launchType": "FARGATE",
            "deploymentController": { "type": "ECS" },
            "deploymentConfiguration": {
                "deploymentCircuitBreaker": { "enable": true, "rollback": true },
            },
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;

    let settled = poll_until(
        case,
        "the first rollout to complete with the task count the service wants running",
        || describe_service(&router, case),
        |service| {
            string(&primary(service, "steady"), "rolloutState", "steady") == COMPLETED
                && count(service, "runningCount", "steady") == DESIRED
        },
    )
    .await;
    assert_eq!(
        string(&primary(&settled, case), "taskDefinition", case),
        first,
        "case {case:?}: expected the completed rollout to run the revision it was created on, \
         got {settled}"
    );
    let service_arn = string(&settled, "serviceArn", case);

    // The revision it CANNOT run: a reference into the mirrored namespace that publishes no
    // such repository, so every launch of it is refused.
    let second = register(
        &router,
        FAMILY,
        &format!("{UNPUBLISHED_REPOSITORY}:1.0"),
        &["sleep", "3600"],
        case,
    )
    .await;
    let updated = ok(
        &router,
        "UpdateService",
        json!({ "cluster": CLUSTER, "service": SERVICE, "taskDefinition": second }),
        case,
    )
    .await;
    let rollout = string(&updated["service"], "currentServiceDeployment", case);
    assert_eq!(
        string(&updated["service"], "taskDefinition", case),
        second,
        "case {case:?}: expected the update to move the service onto the revision it asked \
         for, got {updated}"
    );

    // The breaker fails THAT rollout, and the failure is observable as the deployment of the
    // revision that could not be launched.
    let failing = poll_until(
        case,
        "the rollout of the revision that cannot be launched to reach FAILED",
        || describe_service(&router, case),
        |service| {
            list(service, "deployments", "failing").iter().any(|deployment| {
                deployment["rolloutState"] == json!(FAILED)
                    && deployment["taskDefinition"].as_str() == Some(second.as_str())
            })
        },
    )
    .await;
    let failed = list(&failing, "deployments", case)
        .into_iter()
        .find(|deployment| deployment["taskDefinition"].as_str() == Some(second.as_str()))
        .unwrap_or_else(|| panic!("case {case:?}: expected the failed rollout, got {failing}"));
    let reason = string(&failed, "rolloutStateReason", case);
    assert!(
        reason.contains(BREAKER),
        "case {case:?}: expected the failed rollout's reason to name the {BREAKER}, got \
         {reason:?}"
    );
    assert_ne!(
        string(&failed, "rolloutState", case),
        IN_PROGRESS,
        "case {case:?}: a rollout the breaker failed is no longer in progress, got {failed}"
    );

    // And the rollback it configured takes the service back to the revision that completed,
    // which the scheduler runs at the count the service wants.
    let rolled_back = poll_until(
        case,
        "the service to be rolled back to the revision that completed, running the task count \
         it wants",
        || describe_service(&router, case),
        |service| {
            let deployment = primary(service, "rolled back");
            string(service, "taskDefinition", "rolled back") == first
                && string(&deployment, "rolloutState", "rolled back") == COMPLETED
                && count(service, "runningCount", "rolled back") == DESIRED
        },
    )
    .await;
    let restored = primary(&rolled_back, case);
    assert_eq!(
        string(&restored, "taskDefinition", case),
        first,
        "case {case:?}: expected the PRIMARY deployment to run the revision that completed, \
         got {restored}"
    );
    assert_eq!(
        count(&restored, "runningCount", case),
        count(&rolled_back, "desiredCount", case),
        "case {case:?}: expected the rolled-back deployment to run what the service wants, got \
         {rolled_back}"
    );

    // The rollout the breaker failed, as a resource of its own.
    let record = ok(
        &router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [rollout] }),
        case,
    )
    .await;
    let described = list(&record, "serviceDeployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout back, got {record}"));
    assert_eq!(
        string(&described, "status", case),
        ROLLBACK_SUCCESSFUL,
        "case {case:?}: expected the failed rollout to end rolled back, got {described}"
    );
    assert_eq!(
        string(&described, "serviceArn", case),
        service_arn,
        "case {case:?}: expected the rollout to name the service it belongs to, got {described}"
    );

    let listed = ok(
        &router,
        "ListServiceDeployments",
        json!({ "cluster": CLUSTER, "service": SERVICE }),
        case,
    )
    .await;
    assert!(
        list(&listed, "serviceDeployments", case)
            .iter()
            .any(|brief| brief["serviceDeploymentArn"].as_str() == Some(rollout.as_str())),
        "case {case:?}: expected the failed rollout among the service's own, got {listed}"
    );
}
