// SPDX-License-Identifier: CC0-1.0

//! The ECS MANAGED DAEMON scheduler: the rollout a `CreateDaemon`/`UpdateDaemon` starts,
//! the revisions it captures, the one daemon task each container instance runs, and the
//! reads that report them.
//!
//! The rules under test, each asserted over a table that spans its class rather than over
//! one request:
//!
//! * "A daemon deployment orchestrates the progressive rollout of daemon task updates
//!   across container instances" — a rollout is reported in a non-terminal
//!   `DaemonDeploymentStatus` when the call that started it answers and reaches `SUCCESSFUL`
//!   on the CLOCK, with `startedAt` and `finishedAt` dating the two steps, for a daemon over
//!   any number of container instances (none, one, several);
//! * "Amazon ECS deploys exactly one daemon task on each container instance of the specified
//!   capacity providers. When a container instance registers with the cluster, Amazon ECS
//!   automatically starts daemon tasks" — so the converged daemon holds exactly one RUNNING
//!   task per instance that accepts a placement, an instance that joins LATER is given one
//!   without any further call, and an instance that stops accepting placements gives its
//!   task up;
//! * "A daemon revision is a snapshot of the daemon's configuration at the time a deployment
//!   was initiated" — an update captures a new revision, rolls the daemon's tasks over onto
//!   it, and leaves the earlier revision readable exactly as it was captured;
//! * `UpdateDaemon` "updates the specified daemon": the call states the WHOLE configuration
//!   the daemon then runs under rather than patching one member of it, so the revision it
//!   captures snapshots exactly the members the request stated, with each optional member it
//!   left out at the default its own documentation gives;
//! * "Amazon ECS daemons are cluster specific and Region specific" — every daemon operation
//!   reads its `daemonArn` as the whole identifier it is, and what it answers for one it
//!   cannot resolve follows what the operation DOES: a call that acts in the daemon's
//!   cluster is refused about that cluster, the read of one daemon is refused about the
//!   daemon, and the listing of a daemon's rollouts selects none of them and answers the
//!   empty page;
//! * `DescribeDaemon` "Describes the specified daemon": the read answers the daemon its
//!   `daemonArn` identifies out of STORED state — the cluster it runs in, the status the
//!   last write put it in, the ARN of its most recent daemon deployment, the instants it was
//!   created and last changed at, and one `currentRevisions` entry per revision it holds
//!   tasks of, whose per-capacity-provider counts add up to the daemon tasks that exist —
//!   and because it only reports, repeating it changes nothing a later caller sees;
//! * `DeleteDaemon`: "Deleting a daemon stops all running daemon tasks on the associated
//!   container instances", the daemon is answered `DELETE_IN_PROGRESS`, and it leaves
//!   `ListDaemons` on the clock;
//! * the same placement rule over the other daemon-scheduled workload this service has:
//!   "The daemon scheduling strategy deploys exactly one task on each active container
//!   instance", so a `schedulingStrategy=DAEMON` SERVICE's `desiredCount` follows the
//!   capacity its cluster holds as instances join and drain.
//!
//! Every case drives the emulator through the shared wire harness in [`crate::ecs_wire`],
//! the same front door a client uses, so nothing reaches into the state struct and a
//! response that is not computed from persisted state cannot pass.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::managed_instances;
use crate::ecs_wire::{
    count, create_cluster, list, ok, refusal, register_instance, service, string, ACCOUNT,
    SIGNED_REGION,
};

/// How long a poll waits for a clock-owed daemon transition before failing. Generous
/// against the spans this emulator owes (a rollout is two of them), so a loaded or slow
/// host does not fail a case about WHETHER the transition happens.
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// How often a poll looks again. Short enough that a settled condition is seen promptly and
/// long enough that the loop is not a spin.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The image the daemon task definitions here run; irrelevant to every rule, so one value.
pub(crate) const IMAGE: &str = "public.ecr.aws/aws-observability/aws-otel-collector:latest";

/// Every `DaemonDeploymentStatus` a rollout that has not ended may be in, from the model's
/// own enum (`smithy_query(query="shape", shape="DaemonDeploymentStatus")`: `PENDING`,
/// `SUCCESSFUL`, `STOPPED`, `STOP_REQUESTED`, `IN_PROGRESS`, `ROLLBACK_IN_PROGRESS`,
/// `ROLLBACK_SUCCESSFUL`, `ROLLBACK_FAILED`) — the two a rollout of this emulator passes
/// through before it ends.
const ONGOING_STATUSES: [&str; 2] = ["PENDING", "IN_PROGRESS"];

/// Create a MANAGED INSTANCES capacity provider scoped to `cluster` and return its ARN.
/// "Managed instances capacity providers are cluster-scoped", so the create names one.
pub(crate) async fn create_managed_provider(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "CreateCapacityProvider",
        json!({
            "name": name,
            "cluster": cluster,
            "managedInstancesProvider": managed_instances(),
        }),
        case,
    )
    .await;
    string(&body["capacityProvider"], "capacityProviderArn", case)
}

/// Register one daemon task-definition revision of `family` running `image`, and return its
/// ARN. The image is what tells two revisions of one family apart in a report.
pub(crate) async fn register_daemon_definition(
    router: &MultiServiceRouter,
    family: &str,
    image: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterDaemonTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [{ "name": "agent", "image": image, "memory": 128 }],
        }),
        case,
    )
    .await;
    string(&body, "daemonTaskDefinitionArn", case)
}

/// Create one daemon over `providers` running `definition`, and return its ARN with the ARN
/// of the initial daemon deployment the create answered.
pub(crate) async fn create_daemon(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    providers: &[String],
    definition: &str,
    case: &str,
) -> (String, String) {
    let body = ok(
        router,
        "CreateDaemon",
        json!({
            "clusterArn": cluster,
            "daemonName": name,
            "daemonTaskDefinitionArn": definition,
            "capacityProviderArns": providers,
        }),
        case,
    )
    .await;
    (
        string(&body, "daemonArn", case),
        string(&body, "deploymentArn", case),
    )
}

/// `DescribeDaemon` for one daemon.
async fn describe_daemon(router: &MultiServiceRouter, daemon_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeDaemon",
        json!({ "daemonArn": daemon_arn }),
        case,
    )
    .await;
    body["daemon"].clone()
}

/// `ListDaemonDeployments` for one request, returning the summaries and the token.
pub(crate) async fn list_deployments_page(
    router: &MultiServiceRouter,
    request: Value,
    case: &str,
) -> (Vec<Value>, Option<String>) {
    let body = ok(router, "ListDaemonDeployments", request, case).await;
    let token = body
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (list(&body, "daemonDeployments", case), token)
}

/// Every deployment of one daemon, newest first, in one unpaged listing.
async fn list_deployments(
    router: &MultiServiceRouter,
    daemon_arn: &str,
    case: &str,
) -> Vec<Value> {
    list_deployments_page(router, json!({ "daemonArn": daemon_arn }), case)
        .await
        .0
}

/// `DescribeDaemonDeployments` for one rollout.
async fn describe_deployment(
    router: &MultiServiceRouter,
    deployment_arn: &str,
    case: &str,
) -> Value {
    let body = ok(
        router,
        "DescribeDaemonDeployments",
        json!({ "daemonDeploymentArns": [deployment_arn] }),
        case,
    )
    .await;
    list(&body, "daemonDeployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout back, got {body}"))
}

/// `DescribeDaemonRevisions` for one revision.
async fn describe_revision(router: &MultiServiceRouter, revision_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeDaemonRevisions",
        json!({ "daemonRevisionArns": [revision_arn] }),
        case,
    )
    .await;
    list(&body, "daemonRevisions", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the revision back, got {body}"))
}

/// Every task of one daemon that Amazon ECS wants running, as `ListTasks`' own `daemonName`
/// filter selects them, described.
async fn daemon_tasks(
    router: &MultiServiceRouter,
    cluster: &str,
    daemon_name: &str,
    case: &str,
) -> Vec<Value> {
    let listed = ok(
        router,
        "ListTasks",
        json!({ "cluster": cluster, "daemonName": daemon_name }),
        case,
    )
    .await;
    let arns = list(&listed, "taskArns", case);
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": arns }),
        case,
    )
    .await;
    list(&described, "tasks", case)
}

/// Poll one daemon until `settled` holds of its `DaemonDetail`, and answer that detail.
async fn poll_daemon(
    router: &MultiServiceRouter,
    daemon_arn: &str,
    case: &str,
    what: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let daemon = describe_daemon(router, daemon_arn, case).await;
        if settled(&daemon) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what}, got {daemon}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll one rollout until `settled` holds of its `DaemonDeployment`, and answer it.
async fn poll_deployment(
    router: &MultiServiceRouter,
    deployment_arn: &str,
    case: &str,
    what: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let deployment = describe_deployment(router, deployment_arn, case).await;
        if settled(&deployment) {
            return deployment;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what}, got {deployment}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The RUNNING tasks of `tasks`, by the container instance each of them is placed on.
fn running_hosts(tasks: &[Value], case: &str) -> Vec<String> {
    let mut hosts: Vec<String> = tasks
        .iter()
        .filter(|task| task["lastStatus"].as_str() == Some("RUNNING"))
        .map(|task| string(task, "containerInstanceArn", case))
        .collect();
    hosts.sort();
    hosts
}

/// The one `currentRevisions` entry of a converged daemon.
fn sole_revision(daemon: &Value, case: &str) -> Value {
    let revisions = list(daemon, "currentRevisions", case);
    assert_eq!(
        revisions.len(),
        1,
        "case {case:?}: a converged daemon runs ONE revision, got {daemon}"
    );
    revisions[0].clone()
}

#[tokio::test]
async fn a_rollout_reaches_its_terminal_status_and_places_one_task_per_container_instance() {
    let router = service();
    // The rows differ along what the rule generalises over: HOW MUCH capacity the daemon's
    // cluster holds — none at all, one instance, and several — because "exactly one daemon
    // task on each container instance" is a statement about every count of them.
    let rows: [(&str, usize); 3] = [
        ("a cluster with no container instance", 0),
        ("a cluster with one container instance", 1),
        ("a cluster with three container instances", 3),
    ];
    for (case, instances) in rows {
        let cluster = format!("daemon-rollout-{instances}");
        create_cluster(&router, &cluster, case).await;
        for ordinal in 0..instances {
            register_instance(
                &router,
                &cluster,
                &format!("i-0daemonrollout{ordinal:04}"),
                case,
            )
            .await;
        }
        let provider =
            create_managed_provider(&router, &cluster, &format!("mi-rollout-{instances}"), case)
                .await;
        let definition =
            register_daemon_definition(&router, "daemonrollout", IMAGE, case).await;
        let (daemon_arn, deployment_arn) = create_daemon(
            &router,
            &cluster,
            "agents",
            std::slice::from_ref(&provider),
            &definition,
            case,
        )
        .await;

        // The rollout the create started has not reached its end when the call answers:
        // "A daemon deployment orchestrates the progressive rollout", so it is reported in
        // one of the statuses a rollout under way carries.
        let opening = describe_deployment(&router, &deployment_arn, case).await;
        let opening_status = string(&opening, "status", case);
        assert!(
            ONGOING_STATUSES.contains(&opening_status.as_str())
                || opening_status == "SUCCESSFUL",
            "case {case:?}: expected the rollout under way or already finished, got \
             {opening_status:?}"
        );
        assert!(
            opening.get("createdAt").is_some(),
            "case {case:?}: a rollout carries the instant it was created, got {opening}"
        );

        // ... and the clock takes it to its terminal, successful end.
        let finished = poll_deployment(
            &router,
            &deployment_arn,
            case,
            "the rollout to reach SUCCESSFUL",
            |deployment| deployment["status"].as_str() == Some("SUCCESSFUL"),
        )
        .await;
        for member in ["startedAt", "finishedAt"] {
            assert!(
                finished.get(member).and_then(Value::as_f64).is_some(),
                "case {case:?}: a finished rollout dates its {member}, got {finished}"
            );
        }
        let started_at = finished["startedAt"].as_f64().unwrap_or_default();
        let created_at = finished["createdAt"].as_f64().unwrap_or_default();
        let finished_at = finished["finishedAt"].as_f64().unwrap_or_default();
        assert!(
            created_at <= started_at && started_at <= finished_at,
            "case {case:?}: expected createdAt <= startedAt <= finishedAt, got \
             {created_at}/{started_at}/{finished_at}"
        );

        // Exactly one RUNNING daemon task per container instance, and each of them on a
        // DIFFERENT instance.
        let daemon = poll_daemon(
            &router,
            &daemon_arn,
            case,
            "the daemon to report one running task per container instance",
            |daemon| {
                list(daemon, "currentRevisions", case)
                    .first()
                    .map(|revision| count(revision, "totalRunningCount", case))
                    == Some(instances as i64)
            },
        )
        .await;
        let tasks = daemon_tasks(&router, &cluster, "agents", case).await;
        let hosts = running_hosts(&tasks, case);
        assert_eq!(
            hosts.len(),
            instances,
            "case {case:?}: expected one running daemon task per instance, got {tasks:?}"
        );
        let mut distinct = hosts.clone();
        distinct.dedup();
        assert_eq!(
            distinct, hosts,
            "case {case:?}: expected each daemon task on its own instance, got {hosts:?}"
        );
        for task in &tasks {
            assert_eq!(
                string(task, "group", case),
                "daemon:agents",
                "case {case:?}: a daemon's task carries its daemon's group, got {task}"
            );
            assert_eq!(
                string(task, "taskDefinitionArn", case),
                definition,
                "case {case:?}: a daemon task runs the daemon's revision, got {task}"
            );
        }

        // The per-capacity-provider counts add up to the totals: the daemon deploys onto the
        // providers it names, so each of them reports the instances it carries.
        let revision = sole_revision(&daemon, case);
        let providers = list(&revision, "capacityProviders", case);
        assert_eq!(
            providers.len(),
            1,
            "case {case:?}: the daemon names one provider, got {revision}"
        );
        assert_eq!(
            string(&providers[0], "arn", case),
            provider,
            "case {case:?}: expected the provider the daemon deploys onto, got {revision}"
        );
        let per_provider: i64 = providers
            .iter()
            .map(|entry| count(entry, "runningCount", case))
            .sum();
        assert_eq!(
            per_provider,
            instances as i64,
            "case {case:?}: expected the provider counts to add up to the total, got \
             {revision}"
        );
    }
}

#[tokio::test]
async fn an_instance_that_joins_later_is_given_a_daemon_task_and_a_draining_one_gives_it_up() {
    let router = service();
    const CLUSTER: &str = "daemon-capacity-moves";
    const CASE: &str = "capacity that moves under a converged daemon";
    create_cluster(&router, CLUSTER, CASE).await;
    let first = register_instance(&router, CLUSTER, "i-0daemonjoinfirst0", CASE).await;
    let provider = create_managed_provider(&router, CLUSTER, "mi-moves", CASE).await;
    let definition = register_daemon_definition(&router, "daemonmoves", IMAGE, CASE).await;
    let (daemon_arn, _) = create_daemon(
        &router,
        CLUSTER,
        "agents",
        &[provider],
        &definition,
        CASE,
    )
    .await;
    poll_daemon(
        &router,
        &daemon_arn,
        CASE,
        "the daemon to run on the instance it was created over",
        |daemon| {
            list(daemon, "currentRevisions", CASE)
                .first()
                .map(|revision| count(revision, "totalRunningCount", CASE))
                == Some(1)
        },
    )
    .await;

    // "When a container instance registers with the cluster, Amazon ECS automatically
    // starts daemon tasks": the join alone is what the task is owed by, with no further
    // daemon call.
    let joined = register_instance(&router, CLUSTER, "i-0daemonjoinlater0", CASE).await;
    poll_daemon(
        &router,
        &daemon_arn,
        CASE,
        "the daemon to run on the instance that joined later too",
        |daemon| {
            list(daemon, "currentRevisions", CASE)
                .first()
                .map(|revision| count(revision, "totalRunningCount", CASE))
                == Some(2)
        },
    )
    .await;
    let tasks = daemon_tasks(&router, CLUSTER, "agents", CASE).await;
    let hosts = running_hosts(&tasks, CASE);
    let mut expected = vec![first.clone(), joined.clone()];
    expected.sort();
    assert_eq!(
        hosts, expected,
        "case {CASE:?}: expected a daemon task on both instances, got {tasks:?}"
    );

    // An instance that no longer accepts a placement — "DRAINING ... prevents new tasks
    // from being scheduled for placement on the container instance" — is not one the daemon
    // runs on, so the task on it is taken off it.
    ok(
        &router,
        "UpdateContainerInstancesState",
        json!({
            "cluster": CLUSTER,
            "containerInstances": [joined.clone()],
            "status": "DRAINING",
        }),
        CASE,
    )
    .await;
    let drained = poll_daemon(
        &router,
        &daemon_arn,
        CASE,
        "the drained instance to stop counting towards the daemon",
        |daemon| {
            list(daemon, "currentRevisions", CASE)
                .first()
                .map(|revision| count(revision, "totalRunningCount", CASE))
                == Some(1)
        },
    )
    .await;
    let remaining = daemon_tasks(&router, CLUSTER, "agents", CASE).await;
    assert_eq!(
        running_hosts(&remaining, CASE),
        vec![first],
        "case {CASE:?}: expected the daemon task only on the instance still accepting \
         placements, got {remaining:?}"
    );
    assert!(
        list(&drained, "currentRevisions", CASE)
            .first()
            .map(|revision| count(revision, "totalRunningCount", CASE))
            == Some(1),
        "case {CASE:?}: expected the running count to follow the capacity, got {drained}"
    );
}

#[tokio::test]
async fn an_update_captures_a_new_revision_and_rolls_the_daemon_tasks_over_onto_it() {
    let router = service();
    const CLUSTER: &str = "daemon-revisions";
    const FAMILY: &str = "daemonrevisions";
    const CASE: &str = "an update over a converged daemon";
    /// The image the SECOND daemon task definition revision runs, so the two revisions a
    /// report carries are told apart by what they run rather than by their ARNs alone.
    const NEXT_IMAGE: &str = "public.ecr.aws/aws-observability/aws-otel-collector:1.32.0";
    /// How many container instances the daemon rolls over ACROSS: more than one, because a
    /// rollout replaces the task of every instance rather than of one.
    const INSTANCES: i64 = 3;
    create_cluster(&router, CLUSTER, CASE).await;
    for ordinal in 0..INSTANCES {
        register_instance(
            &router,
            CLUSTER,
            &format!("i-0daemonrevision{ordinal:03}"),
            CASE,
        )
        .await;
    }
    let provider = create_managed_provider(&router, CLUSTER, "mi-revisions", CASE).await;
    let first_definition = register_daemon_definition(&router, FAMILY, IMAGE, CASE).await;
    let (daemon_arn, first_deployment) = create_daemon(
        &router,
        CLUSTER,
        "agents",
        std::slice::from_ref(&provider),
        &first_definition,
        CASE,
    )
    .await;
    let converged = poll_daemon(
        &router,
        &daemon_arn,
        CASE,
        "the daemon to converge on its first revision across every instance",
        |daemon| {
            list(daemon, "currentRevisions", CASE)
                .first()
                .map(|revision| count(revision, "totalRunningCount", CASE))
                == Some(INSTANCES)
        },
    )
    .await;
    let first_revision = string(&sole_revision(&converged, CASE), "arn", CASE);

    // A second revision of the same family, and the update that points the daemon at it.
    let next_definition = register_daemon_definition(&router, FAMILY, NEXT_IMAGE, CASE).await;
    assert_ne!(
        next_definition, first_definition,
        "case {CASE:?}: a second registration of a family is a second revision"
    );
    let updated = ok(
        &router,
        "UpdateDaemon",
        json!({
            "daemonArn": daemon_arn,
            "daemonTaskDefinitionArn": next_definition,
            "capacityProviderArns": [provider],
        }),
        CASE,
    )
    .await;
    let second_deployment = string(&updated, "deploymentArn", CASE);
    assert_ne!(
        second_deployment, first_deployment,
        "case {CASE:?}: an update triggers a NEW daemon deployment, got {updated}"
    );
    assert_eq!(
        string(&updated, "status", CASE),
        "ACTIVE",
        "case {CASE:?}: an updated daemon is still ACTIVE, got {updated}"
    );
    assert!(
        updated["updatedAt"].as_f64() >= updated["createdAt"].as_f64(),
        "case {CASE:?}: an update is not earlier than the create, got {updated}"
    );

    // The new rollout names the revision it is deploying and the one it is replacing.
    let described = describe_deployment(&router, &second_deployment, CASE).await;
    let target = string(&described["targetDaemonRevision"], "arn", CASE);
    assert_ne!(
        target, first_revision,
        "case {CASE:?}: an update captures a NEW revision, got {described}"
    );
    let sources = list(&described, "sourceDaemonRevisions", CASE);
    assert_eq!(
        sources
            .iter()
            .map(|source| string(source, "arn", CASE))
            .collect::<Vec<String>>(),
        vec![first_revision.clone()],
        "case {CASE:?}: expected the revision being replaced as the source, got {described}"
    );

    // The listing reports both rollouts, NEWEST first, and the one the update replaced has
    // ended.
    let listed = list_deployments(&router, &daemon_arn, CASE).await;
    assert_eq!(
        listed
            .iter()
            .map(|deployment| string(deployment, "daemonDeploymentArn", CASE))
            .collect::<Vec<String>>(),
        vec![second_deployment.clone(), first_deployment.clone()],
        "case {CASE:?}: expected the daemon's rollouts newest first, got {listed:?}"
    );
    let replaced = describe_deployment(&router, &first_deployment, CASE).await;
    let replaced_status = string(&replaced, "status", CASE);
    // The rollout the update replaced has ENDED, whichever end it reached: one still open
    // when the update arrived is STOPPED and names its replacement, and one that had
    // already converged keeps the SUCCESSFUL end it reached — a rollout that ended is never
    // re-ended. Either way it carries the instant it finished at.
    assert!(
        ["STOPPED", "SUCCESSFUL"].contains(&replaced_status.as_str())
            && replaced.get("finishedAt").is_some(),
        "case {CASE:?}: expected the earlier rollout to have ended, got {replaced}"
    );
    if replaced_status == "STOPPED" {
        assert!(
            replaced["statusReason"]
                .as_str()
                .is_some_and(|reason| reason.contains(&second_deployment)),
            "case {CASE:?}: expected a replaced rollout to name its replacement, got \
             {replaced}"
        );
    }

    // The daemon's task on EVERY instance is rolled over onto the new revision, and the
    // rollout then ends.
    poll_deployment(
        &router,
        &second_deployment,
        CASE,
        "the second rollout to reach SUCCESSFUL",
        |deployment| deployment["status"].as_str() == Some("SUCCESSFUL"),
    )
    .await;
    let rolled = poll_daemon(
        &router,
        &daemon_arn,
        CASE,
        "the daemon to run the revision the update captured on every instance",
        |daemon| {
            let revisions = list(daemon, "currentRevisions", CASE);
            revisions.len() == 1
                && revisions[0]["arn"].as_str() == Some(target.as_str())
                && count(&revisions[0], "totalRunningCount", CASE) == INSTANCES
        },
    )
    .await;
    let tasks = daemon_tasks(&router, CLUSTER, "agents", CASE).await;
    let running: Vec<String> = tasks
        .iter()
        .filter(|task| task["lastStatus"].as_str() == Some("RUNNING"))
        .map(|task| string(task, "taskDefinitionArn", CASE))
        .collect();
    assert_eq!(
        running,
        vec![next_definition.clone(); INSTANCES as usize],
        "case {CASE:?}: expected every running task to run the new revision, got {tasks:?}"
    );
    assert_eq!(
        running_hosts(&tasks, CASE).len(),
        INSTANCES as usize,
        "case {CASE:?}: expected one task per instance after the rollout, got {tasks:?}"
    );
    assert_eq!(
        string(&rolled, "deploymentArn", CASE),
        second_deployment,
        "case {CASE:?}: a daemon names its most recent deployment, got {rolled}"
    );
    // "Daemon revisions are immutable": each one reports the configuration it captured,
    // so the earlier revision still names the image it was created over.
    let rows: [(&str, &str, &str); 2] = [
        ("the revision the create captured", &first_revision, IMAGE),
        ("the revision the update captured", &target, NEXT_IMAGE),
    ];
    for (row, revision_arn, image) in rows {
        let revision = describe_revision(&router, revision_arn, row).await;
        assert_eq!(
            string(&revision, "daemonArn", row),
            daemon_arn,
            "case {row:?}: a revision names the daemon it belongs to, got {revision}"
        );
        let images = list(&revision, "containerImages", row);
        assert_eq!(
            images
                .iter()
                .map(|entry| string(entry, "image", row))
                .collect::<Vec<String>>(),
            vec![image.to_string()],
            "case {row:?}: expected the images of the revision as captured, got {revision}"
        );
        assert_eq!(
            string(&images[0], "containerName", row),
            "agent",
            "case {row:?}: expected the container the revision declares, got {revision}"
        );
    }

    // An ARN under a cluster that IS there but names no stored revision is reported as a
    // failure of its own entry rather than refusing the whole read.
    let unknown = format!(
        "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon-revision/{CLUSTER}/agents/\
         0000000000000009999"
    );
    let body = ok(
        &router,
        "DescribeDaemonRevisions",
        json!({ "daemonRevisionArns": [unknown.clone(), first_revision.clone()] }),
        CASE,
    )
    .await;
    assert_eq!(
        list(&body, "daemonRevisions", CASE).len(),
        1,
        "case {CASE:?}: expected the revision that resolves, got {body}"
    );
    let failures = list(&body, "failures", CASE);
    assert_eq!(
        failures.len(),
        1,
        "case {CASE:?}: expected one failure, got {body}"
    );
    assert_eq!(
        (
            string(&failures[0], "arn", CASE),
            string(&failures[0], "reason", CASE)
        ),
        (unknown, "MISSING".to_string()),
        "case {CASE:?}: expected the entry echoed with MISSING, got {body}"
    );
}

#[tokio::test]
async fn a_delete_drains_the_daemon_tasks_and_the_daemon_leaves_the_listing() {
    let router = service();
    const CLUSTER: &str = "daemon-delete";
    const CASE: &str = "a delete over a converged daemon";
    create_cluster(&router, CLUSTER, CASE).await;
    register_instance(&router, CLUSTER, "i-0daemondelete0000", CASE).await;
    let provider = create_managed_provider(&router, CLUSTER, "mi-delete", CASE).await;
    let definition = register_daemon_definition(&router, "daemondelete", IMAGE, CASE).await;
    let (daemon_arn, deployment_arn) = create_daemon(
        &router,
        CLUSTER,
        "agents",
        &[provider],
        &definition,
        CASE,
    )
    .await;
    poll_daemon(
        &router,
        &daemon_arn,
        CASE,
        "the daemon to converge before it is deleted",
        |daemon| {
            list(daemon, "currentRevisions", CASE)
                .first()
                .map(|revision| count(revision, "totalRunningCount", CASE))
                == Some(1)
        },
    )
    .await;

    let deleted = ok(
        &router,
        "DeleteDaemon",
        json!({ "daemonArn": daemon_arn }),
        CASE,
    )
    .await;
    assert_eq!(
        string(&deleted, "status", CASE),
        "DELETE_IN_PROGRESS",
        "case {CASE:?}: \"After you call DeleteDaemon, the status changes to \
         DELETE_IN_PROGRESS\", got {deleted}"
    );
    let drain = string(&deleted, "deploymentArn", CASE);
    assert_ne!(
        drain, deployment_arn,
        "case {CASE:?}: the delete triggers its own daemon deployment, got {deleted}"
    );
    assert!(
        drain.starts_with(&format!(
            "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon-deployment/{CLUSTER}/agents/"
        )),
        "case {CASE:?}: expected a daemon-deployment ARN of this daemon, got {drain:?}"
    );

    // "Deleting a daemon stops all running daemon tasks on the associated container
    // instances": no task of the daemon is wanted running any more.
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let tasks = daemon_tasks(&router, CLUSTER, "agents", CASE).await;
        if tasks.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {CASE:?}: expected the daemon's tasks to stop, got {tasks:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // ... and the daemon itself leaves the listing on the clock, with no further call.
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ok(
            &router,
            "ListDaemons",
            json!({ "clusterArn": CLUSTER }),
            CASE,
        )
        .await;
        let listed = list(&body, "daemonSummariesList", CASE);
        if listed.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {CASE:?}: expected the deleted daemon to leave the listing, got {listed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let (code, message) = refusal(
        &router,
        "DescribeDaemon",
        json!({ "daemonArn": daemon_arn }),
        CASE,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "DaemonNotFoundException",
            format!("Daemon not found: {daemon_arn}").as_str()
        ),
        "case {CASE:?}: expected the deleted daemon to be gone"
    );
}

#[tokio::test]
async fn every_daemon_operation_answers_an_arn_it_cannot_resolve_as_its_own_rule_says() {
    let router = service();
    const CLUSTER: &str = "daemon-refusals";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    let provider = create_managed_provider(&router, CLUSTER, "mi-refusals", CASE).await;
    let definition = register_daemon_definition(&router, "daemonrefusals", IMAGE, CASE).await;

    let daemon_of = |cluster: &str, name: &str| {
        format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon/{cluster}/{name}")
    };
    // The rows differ along WHICH part of the identifier fails to resolve: the cluster the
    // ARN carries, the daemon within a cluster that is there, and the value's being no
    // daemon ARN at all. What each earns follows what the operation DOES with it: a call
    // that ACTS in the daemon's cluster resolves that cluster and is refused about it, the
    // read of the one daemon the ARN identifies answers about the daemon, and the listing of
    // a daemon's rollouts SELECTS them, so an ARN it cannot resolve selects none.
    let identifiers: [(&str, String, bool, bool); 3] = [
        (
            "a daemon ARN under a cluster this account does not hold",
            daemon_of("daemon-nowhere", "agents"),
            true,
            true,
        ),
        (
            "a daemon name no daemon of an existing cluster answers to",
            daemon_of(CLUSTER, "nosuchdaemon"),
            false,
            true,
        ),
        (
            "a value that is no daemon ARN at all",
            "agents".to_string(),
            false,
            false,
        ),
    ];
    for (identifier_case, daemon_arn, cluster_missing, shaped_like_an_arn) in identifiers {
        // Every operation addressed to ONE daemon by ARN, each with the smallest request
        // the model accepts beside it — so the answer is the identifier's and not another
        // member's — paired with what it does with the daemon's cluster and the sentence it
        // words for a daemon that is not there: a READ states the identifier it could not
        // resolve, while the DELETE states that no metadata backs the daemon at all.
        let operations: [(&str, Value, bool, &str); 3] = [
            (
                "DescribeDaemon",
                json!({ "daemonArn": daemon_arn }),
                false,
                "Daemon not found: ",
            ),
            (
                "UpdateDaemon",
                json!({
                    "daemonArn": daemon_arn,
                    "daemonTaskDefinitionArn": definition,
                    "capacityProviderArns": [provider],
                }),
                true,
                "Daemon not found: ",
            ),
            (
                "DeleteDaemon",
                json!({ "daemonArn": daemon_arn }),
                true,
                "No daemon metadata found.",
            ),
        ];
        for (operation, request, acts_in_the_cluster, not_found_sentence) in operations {
            let case = format!("{operation} on {identifier_case}");
            let (code, message) = refusal(&router, operation, request, &case).await;
            let (expected_code, expected_message) = if cluster_missing && acts_in_the_cluster {
                ("ClusterNotFoundException", "Cluster not found.".to_string())
            } else if not_found_sentence.ends_with(' ') {
                (
                    "DaemonNotFoundException",
                    format!("{not_found_sentence}{daemon_arn}"),
                )
            } else {
                ("DaemonNotFoundException", not_found_sentence.to_string())
            };
            assert_eq!(
                (code.as_str(), message.as_str()),
                (expected_code, expected_message.as_str()),
                "case {case:?}: expected the refusal the identifier earns"
            );
        }

        // `ListDaemonDeployments` SELECTS the rollouts of the daemon the ARN names: an ARN
        // of the daemon form that resolves to no stored daemon — whatever part of it does
        // not resolve — selects none of them, and is answered with the empty page it
        // selects. Only a value that is no daemon ARN at all identifies no daemon of any
        // account, and this operation models no not-found error for a daemon, so that is
        // answered as the invalid identifier it is.
        let case = format!("ListDaemonDeployments on {identifier_case}");
        if shaped_like_an_arn {
            let listed = list_deployments(&router, &daemon_arn, &case).await;
            assert!(
                listed.is_empty(),
                "case {case:?}: expected the empty page the ARN selects, got {listed:?}"
            );
        } else {
            let (code, message) = refusal(
                &router,
                "ListDaemonDeployments",
                json!({ "daemonArn": daemon_arn }),
                &case,
            )
            .await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                ("ClientException", format!("Daemon not found: {daemon_arn}").as_str()),
                "case {case:?}: expected the invalid identifier to be answered"
            );
        }
    }
}

#[tokio::test]
async fn a_deployment_listing_filters_by_status_and_creation_window_and_pages() {
    let router = service();
    const CLUSTER: &str = "daemon-listing";
    const CASE: &str = "a daemon with a rollout history";
    create_cluster(&router, CLUSTER, CASE).await;
    let provider = create_managed_provider(&router, CLUSTER, "mi-listing", CASE).await;
    let definition = register_daemon_definition(&router, "daemonlisting", IMAGE, CASE).await;
    let (daemon_arn, first) = create_daemon(
        &router,
        CLUSTER,
        "agents",
        std::slice::from_ref(&provider),
        &definition,
        CASE,
    )
    .await;
    // Two updates, so the daemon holds three rollouts: two ended and one current.
    let mut arns = vec![first];
    for _ in 0..2 {
        let body = ok(
            &router,
            "UpdateDaemon",
            json!({
                "daemonArn": daemon_arn,
                "daemonTaskDefinitionArn": definition,
                "capacityProviderArns": [provider],
            }),
            CASE,
        )
        .await;
        arns.push(string(&body, "deploymentArn", CASE));
    }
    let newest_first: Vec<String> = arns.iter().rev().cloned().collect();

    // Every rollout of the history has ENDED before the filters are read, so what each
    // filter selects is settled: a terminal rollout carries the instant it finished at and
    // its status never moves again, which is what makes the comparisons below about the
    // FILTER rather than about how far the clock has taken each rollout.
    let deadline = Instant::now() + POLL_BUDGET;
    let listed = loop {
        let listed = list_deployments(&router, &daemon_arn, CASE).await;
        if listed.len() == arns.len()
            && listed
                .iter()
                .all(|deployment| deployment.get("finishedAt").is_some())
        {
            break listed;
        }
        assert!(
            Instant::now() < deadline,
            "case {CASE:?}: expected every rollout of the history to end, got {listed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        listed
            .iter()
            .map(|deployment| string(deployment, "daemonDeploymentArn", CASE))
            .collect::<Vec<String>>(),
        newest_first,
        "case {CASE:?}: expected every rollout of the daemon, newest first"
    );
    let created: Vec<f64> = listed
        .iter()
        .map(|deployment| deployment["createdAt"].as_f64().unwrap_or_default())
        .collect();
    // One second either side of the window the history occupies — a margin, so the rows
    // below are about which SIDE of a bound a rollout falls on rather than about how
    // exactly a timestamp survives a round trip through the wire's number form.
    let before_all = created.last().copied().unwrap_or_default() - 1.0;
    let after_all = created.first().copied().unwrap_or_default() + 1.0;

    // The rows differ along WHICH filter narrows the listing, and each is checked against
    // the rollouts it selects out of the same history — the statuses being the ones the
    // unfiltered listing itself reports, so the case is about the FILTER rather than about
    // how far the clock has taken each rollout.
    let mut statuses: Vec<String> = listed
        .iter()
        .map(|deployment| string(deployment, "status", CASE))
        .collect();
    statuses.sort();
    statuses.dedup();
    for status in &statuses {
        let row = format!("the rollouts in {status}");
        let (page, _) = list_deployments_page(
            &router,
            json!({ "daemonArn": daemon_arn, "status": [status] }),
            &row,
        )
        .await;
        let expected: Vec<String> = listed
            .iter()
            .filter(|deployment| string(deployment, "status", CASE) == *status)
            .map(|deployment| string(deployment, "daemonDeploymentArn", CASE))
            .collect();
        assert_eq!(
            page.iter()
                .map(|deployment| string(deployment, "daemonDeploymentArn", &row))
                .collect::<Vec<String>>(),
            expected,
            "case {row:?}: expected exactly the rollouts in that status"
        );
    }
    let filter_rows: [(&str, Value, usize); 5] = [
        (
            "no filter at all",
            json!({ "daemonArn": daemon_arn }),
            arns.len(),
        ),
        (
            "a status no rollout of this daemon is in",
            json!({ "daemonArn": daemon_arn, "status": ["ROLLBACK_FAILED"] }),
            0,
        ),
        (
            "a window that ends before every rollout of the history",
            json!({
                "daemonArn": daemon_arn,
                "createdAt": { "before": before_all },
            }),
            0,
        ),
        (
            "a window that starts after every rollout of the history",
            json!({
                "daemonArn": daemon_arn,
                "createdAt": { "after": after_all },
            }),
            0,
        ),
        (
            "a window that starts before every rollout of the history",
            json!({
                "daemonArn": daemon_arn,
                "createdAt": { "after": before_all },
            }),
            arns.len(),
        ),
    ];
    for (row, request, expected) in filter_rows {
        let (page, _) = list_deployments_page(&router, request, row).await;
        assert_eq!(
            page.len(),
            expected,
            "case {row:?}: expected {expected} rollout(s), got {page:?}"
        );
    }

    // "maxResults ... along with a nextToken response element": the walk reports every
    // rollout exactly once, one page at a time.
    let mut walked = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "daemonArn": daemon_arn, "maxResults": 1 });
        if let Some(token) = token.as_deref() {
            request["nextToken"] = json!(token);
        }
        let (page, next) = list_deployments_page(&router, request, "one rollout per page").await;
        assert!(
            page.len() <= 1,
            "case {CASE:?}: expected at most one rollout a page, got {page:?}"
        );
        walked.extend(
            page.iter()
                .map(|deployment| string(deployment, "daemonDeploymentArn", CASE)),
        );
        match next {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    assert_eq!(
        walked, newest_first,
        "case {CASE:?}: expected the paged walk to report every rollout once"
    );
}

/// One row of the daemon-read rule below: the SHAPE of the daemon whose read is asserted.
///
/// Each row is its own test, over its own emulator, so one row's walk of the clock is not
/// another's wait.
struct ReportedDaemon {
    case: &'static str,
    /// The cluster and the daemon task-definition family this row stands up, so no row
    /// shares another's state.
    cluster: &'static str,
    family: &'static str,
    /// How many container instances the daemon's cluster holds, which is how many running
    /// daemon tasks the read then has to count.
    instances: usize,
    /// How many capacity providers the daemon deploys onto, each of which the read reports
    /// an entry of its own for.
    providers: usize,
    /// Whether an update moves the daemon onto a second revision before it is read, which
    /// is what makes "the most recent daemon deployment" a different rollout from the one
    /// the create answered with.
    updated: bool,
}

/// The image a row's update moves the daemon onto, so the two revisions of its family are
/// told apart by what they run.
const SECOND_IMAGE: &str = "public.ecr.aws/aws-observability/aws-otel-collector:1.33.0";

/// Poll one daemon's own tasks until `instances` of them are RUNNING, every one of them on
/// `definition`, and answer them. The rollover a row's update starts is waited out HERE,
/// off the tasks themselves, so what a report says mid-rollover decides nothing.
async fn poll_daemon_tasks(
    router: &MultiServiceRouter,
    cluster: &str,
    definition: &str,
    instances: usize,
    case: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let tasks = daemon_tasks(router, cluster, "agents", case).await;
        let running: Vec<&Value> = tasks
            .iter()
            .filter(|task| task["lastStatus"].as_str() == Some("RUNNING"))
            .collect();
        if running.len() == instances
            && running
                .iter()
                .all(|task| task["taskDefinitionArn"].as_str() == Some(definition))
        {
            return tasks;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {instances} running daemon task(s) on {definition:?}, got \
             {tasks:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_daemon_read_reports_a_daemon_whose_cluster_holds_no_container_instance() {
    a_daemon_read_reports_the_stored_daemon(&ReportedDaemon {
        case: "a daemon over a cluster that holds no container instance",
        cluster: "daemon-report-empty",
        family: "daemonreportempty",
        instances: 0,
        providers: 1,
        updated: false,
    })
    .await;
}

#[tokio::test]
async fn a_daemon_read_reports_a_daemon_whose_cluster_holds_one_container_instance() {
    a_daemon_read_reports_the_stored_daemon(&ReportedDaemon {
        case: "a daemon over a cluster that holds one container instance",
        cluster: "daemon-report-one",
        family: "daemonreportone",
        instances: 1,
        providers: 1,
        updated: false,
    })
    .await;
}

#[tokio::test]
async fn a_daemon_read_reports_a_daemon_spread_over_two_capacity_providers() {
    a_daemon_read_reports_the_stored_daemon(&ReportedDaemon {
        case: "a daemon over two capacity providers and three container instances",
        cluster: "daemon-report-spread",
        family: "daemonreportspread",
        instances: 3,
        providers: 2,
        updated: false,
    })
    .await;
}

#[tokio::test]
async fn a_daemon_read_reports_a_daemon_an_update_moved_onto_a_second_revision() {
    a_daemon_read_reports_the_stored_daemon(&ReportedDaemon {
        case: "a daemon an update has moved onto a second revision",
        cluster: "daemon-report-updated",
        family: "daemonreportupdated",
        instances: 2,
        providers: 1,
        updated: true,
    })
    .await;
}

/// The daemon-read rule, over ONE row: `DescribeDaemon` "Describes the specified daemon", so
/// every member of the `DaemonDetail` it answers is what the create and the update WROTE —
/// the cluster, the status, the ARN of the most recent daemon deployment, the instants the
/// daemon was created and last changed at, and one `currentRevisions` entry per revision it
/// holds tasks of, whose per-capacity-provider counts add up to the tasks that exist — and,
/// because the call only reports, repeating it changes nothing a later caller sees.
async fn a_daemon_read_reports_the_stored_daemon(row: &ReportedDaemon) {
    let router = service();
    let case = row.case;
    let cluster_arn = create_cluster(&router, row.cluster, case).await;
    for ordinal in 0..row.instances {
        register_instance(
            &router,
            row.cluster,
            &format!("i-0{:015x}", ordinal + 1),
            case,
        )
        .await;
    }
    let mut providers = Vec::new();
    for ordinal in 0..row.providers {
        providers.push(
            create_managed_provider(
                &router,
                row.cluster,
                &format!("{}-mi-{ordinal}", row.cluster),
                case,
            )
            .await,
        );
    }
    let definition = register_daemon_definition(&router, row.family, IMAGE, case).await;
    // The WHOLE create answer is needed here, not just the two ARNs [`create_daemon`] hands
    // back: `createdAt` is one of the members the read has to report back unchanged.
    let created = ok(
        &router,
        "CreateDaemon",
        json!({
            "clusterArn": row.cluster,
            "daemonName": "agents",
            "daemonTaskDefinitionArn": definition,
            "capacityProviderArns": providers,
        }),
        case,
    )
    .await;
    let daemon_arn = string(&created, "daemonArn", case);
    let created_at = created["createdAt"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a createdAt, got {created}"));

    // What the read then owes: the daemon's most recent rollout and the definition its tasks
    // run — the create's, or the update's where the row makes one.
    let (expected_deployment, expected_definition) = if row.updated {
        let next = register_daemon_definition(&router, row.family, SECOND_IMAGE, case).await;
        assert_ne!(
            next, definition,
            "case {case:?}: a second registration of a family is a second revision"
        );
        let updated = ok(
            &router,
            "UpdateDaemon",
            json!({
                "daemonArn": daemon_arn,
                "daemonTaskDefinitionArn": next,
                "capacityProviderArns": providers,
            }),
            case,
        )
        .await;
        let rollout = string(&updated, "deploymentArn", case);
        assert_ne!(
            rollout,
            string(&created, "deploymentArn", case),
            "case {case:?}: an update starts a NEW rollout, got {updated}"
        );
        (rollout, next)
    } else {
        (string(&created, "deploymentArn", case), definition.clone())
    };

    // The clock places the daemon's task on every instance that accepts a placement, and a
    // row that updates rolls those tasks over onto the revision the update captured. Waited
    // out on the TASKS, so the settled state the report is then read in is one the report
    // itself did not decide.
    let instances = row.instances;
    let tasks = poll_daemon_tasks(
        &router,
        row.cluster,
        &expected_definition,
        instances,
        case,
    )
    .await;
    let hosts = running_hosts(&tasks, case);
    let mut distinct = hosts.clone();
    distinct.dedup();
    assert_eq!(
        distinct, hosts,
        "case {case:?}: expected each daemon task on its own instance, got {hosts:?}"
    );
    let instances = instances as i64;
    let daemon = poll_daemon(
        &router,
        &daemon_arn,
        case,
        "the daemon to report the one revision its tasks run",
        |daemon| list(daemon, "currentRevisions", case).len() == 1,
    )
    .await;

    // Every member of the report is what the create and the update WROTE.
    assert_eq!(
        (
            string(&daemon, "daemonArn", case),
            string(&daemon, "clusterArn", case),
            string(&daemon, "status", case),
            string(&daemon, "deploymentArn", case),
            daemon["createdAt"].as_f64()
        ),
        (
            daemon_arn.clone(),
            cluster_arn,
            "ACTIVE".to_string(),
            expected_deployment,
            Some(created_at)
        ),
        "case {case:?}: expected the daemon the writes stored, with its MOST RECENT \
         deployment, got {daemon}"
    );
    assert!(
        daemon["updatedAt"].as_f64() >= Some(created_at),
        "case {case:?}: expected updatedAt not earlier than createdAt, got {daemon}"
    );

    // "The current daemon revision details, including the running task counts per capacity
    // provider": one entry per provider the daemon deploys onto, in the order the daemon
    // names them, and their counts add up to the totals the entry reports — which are the
    // TASKS THAT EXIST, as this service's own task reads report them.
    let revision = sole_revision(&daemon, case);
    let entries = list(&revision, "capacityProviders", case);
    assert_eq!(
        entries
            .iter()
            .map(|entry| string(entry, "arn", case))
            .collect::<Vec<String>>(),
        providers,
        "case {case:?}: expected an entry per provider the daemon deploys onto, got {revision}"
    );
    let running: i64 = entries
        .iter()
        .map(|entry| count(entry, "runningCount", case))
        .sum();
    let without: i64 = entries
        .iter()
        .map(|entry| count(entry, "withoutDaemonCount", case))
        .sum();
    assert_eq!(
        (
            running,
            without,
            count(&revision, "totalRunningCount", case),
            count(&revision, "totalWithoutDaemonCount", case),
            hosts.len() as i64
        ),
        (instances, 0, instances, 0, instances),
        "case {case:?}: expected the provider counts to add up to the totals of a converged \
         daemon and to the running tasks, got {revision} against {tasks:?}"
    );

    // The read REPORTS: repeating it answers the same detail and leaves the daemon, its
    // rollouts and its tasks exactly as they were.
    let rollouts = list_deployments(&router, &daemon_arn, case).await.len();
    for repeat in 0..3 {
        let again = describe_daemon(&router, &daemon_arn, case).await;
        assert_eq!(
            again, daemon,
            "case {case:?}: read {repeat} answered a detail the first read did not"
        );
    }
    assert_eq!(
        list_deployments(&router, &daemon_arn, case).await.len(),
        rollouts,
        "case {case:?}: expected the reads to mint no rollout"
    );
    assert_eq!(
        running_hosts(&daemon_tasks(&router, row.cluster, "agents", case).await, case),
        hosts,
        "case {case:?}: expected the reads to leave the daemon's tasks where they were"
    );
}

#[tokio::test]
async fn a_daemon_scheduled_service_keeps_one_task_per_instance_its_cluster_holds() {
    let router = service();
    const CLUSTER: &str = "daemon-service-capacity";
    const SERVICE: &str = "agents";
    const CASE: &str = "a DAEMON-scheduled service over capacity that moves";
    create_cluster(&router, CLUSTER, CASE).await;
    let first = register_instance(&router, CLUSTER, "i-0daemonservice000", CASE).await;
    let definition = crate::ecs_wire::register_definition(&router, "daemonservice", CASE).await;
    let created = ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": definition,
            "schedulingStrategy": "DAEMON",
        }),
        CASE,
    )
    .await;
    assert_eq!(
        count(&created["service"], "desiredCount", CASE),
        1,
        "case {CASE:?}: \"exactly one task on each active container instance\", got {created}"
    );

    // The rows differ along what the rule generalises over: HOW the capacity of the
    // cluster moves after the service was created — an instance joining, and an instance
    // that stops accepting placements. The count the scheduler keeps follows each move with
    // no call on the service at all.
    let joined = register_instance(&router, CLUSTER, "i-0daemonservice001", CASE).await;
    let rows: [(&str, Option<String>, i64); 2] = [
        ("an instance that joined after the create", None, 2),
        (
            "an instance taken out of service",
            Some(joined.clone()),
            1,
        ),
    ];
    for (row, drain, expected) in rows {
        if let Some(instance) = drain {
            ok(
                &router,
                "UpdateContainerInstancesState",
                json!({
                    "cluster": CLUSTER,
                    "containerInstances": [instance],
                    "status": "DRAINING",
                }),
                row,
            )
            .await;
        }
        let deadline = Instant::now() + POLL_BUDGET;
        loop {
            let body = ok(
                &router,
                "DescribeServices",
                json!({ "cluster": CLUSTER, "services": [SERVICE] }),
                row,
            )
            .await;
            let services = list(&body, "services", row);
            let service = services
                .first()
                .unwrap_or_else(|| panic!("case {row:?}: expected the service back, got {body}"));
            let desired = count(service, "desiredCount", row);
            let running = count(service, "runningCount", row);
            if desired == expected && running == expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {row:?}: expected a desired and running count of {expected}, got \
                 {desired}/{running} in {service}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    // The task the service kept is on the instance that still accepts placements.
    let listed = ok(
        &router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": SERVICE }),
        CASE,
    )
    .await;
    let arns = list(&listed, "taskArns", CASE);
    let described = ok(
        &router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        CASE,
    )
    .await;
    let tasks = list(&described, "tasks", CASE);
    assert_eq!(
        running_hosts(&tasks, CASE),
        vec![first],
        "case {CASE:?}: expected the service's task on the instance still accepting \
         placements, got {tasks:?}"
    );
}

/// A required boolean member of a reported shape, the way [`string`] reads a string one.
fn flag(body: &Value, member: &str, case: &str) -> bool {
    body[member]
        .as_bool()
        .unwrap_or_else(|| panic!("case {case:?}: expected a boolean {member}, got {body}"))
}

/// The configuration one reported `DaemonRevision` snapshotted: `propagateTags`,
/// `enableECSManagedTags`, `enableExecuteCommand` and `critical`
/// (`smithy_query(query="shape", shape="DaemonRevision")`).
fn snapshot(revision: &Value, case: &str) -> (String, bool, bool, bool) {
    (
        string(revision, "propagateTags", case),
        flag(revision, "enableECSManagedTags", case),
        flag(revision, "enableExecuteCommand", case),
        flag(revision, "critical", case),
    )
}

/// The same four values as a table row states them, so a row is written without a `String`.
fn stated_snapshot(expected: (&str, bool, bool, bool)) -> (String, bool, bool, bool) {
    (
        expected.0.to_string(),
        expected.1,
        expected.2,
        expected.3,
    )
}

/// One row of the configuration table in
/// [`an_update_states_the_whole_configuration_and_the_revision_it_captures_snapshots_it`]:
/// what an `UpdateDaemon` states, and the configuration the revision it captures therefore
/// reports.
struct UpdateRow {
    case: &'static str,
    /// Whether the update names its daemon task definition by the FAMILY alone — which
    /// resolves to the latest ACTIVE revision of that family — rather than by a stored ARN.
    by_family: bool,
    /// Which registered revision the update points the daemon at, as an index into the two
    /// registered for the case; ignored when the reference is the family alone.
    definition: usize,
    /// How many of the cluster's managed-instances capacity providers the update deploys
    /// onto.
    providers: usize,
    /// The optional members the request carries beside the three required ones.
    stated: fn() -> Value,
    /// `propagateTags`, `enableECSManagedTags`, `enableExecuteCommand` and `critical` as the
    /// captured revision must report them: the value the row states, or the member's own
    /// documented default where the row leaves it out.
    expected: (&'static str, bool, bool, bool),
}

#[tokio::test]
async fn an_update_states_the_whole_configuration_and_the_revision_it_captures_snapshots_it() {
    let router = service();
    const CLUSTER: &str = "daemon-configuration";
    const FAMILY: &str = "daemonconfiguration";
    const CASE: &str = "setup";
    /// The image the SECOND registered revision of the family runs, so the revision a
    /// reference resolved to is told apart by what it runs rather than by its ARN alone.
    const NEXT_IMAGE: &str = "public.ecr.aws/aws-observability/aws-otel-collector:1.32.0";
    create_cluster(&router, CLUSTER, CASE).await;
    // Two providers, so a row deploys onto one of them or onto both: the list an update
    // states is the capacity the revision it captures is rolled out over.
    let mut providers = Vec::new();
    for slot in 0..2 {
        providers.push(
            create_managed_provider(&router, CLUSTER, &format!("mi-configuration{slot}"), CASE)
                .await,
        );
    }
    // Two revisions of ONE family, so a reference that names the family alone resolves to
    // the later of them while an ARN names exactly the one it carries.
    let definitions = [
        register_daemon_definition(&router, FAMILY, IMAGE, CASE).await,
        register_daemon_definition(&router, FAMILY, NEXT_IMAGE, CASE).await,
    ];
    let images = [IMAGE, NEXT_IMAGE];
    let (daemon_arn, first_deployment) = create_daemon(
        &router,
        CLUSTER,
        "agents",
        std::slice::from_ref(&providers[0]),
        &definitions[0],
        CASE,
    )
    .await;
    let created = describe_daemon(&router, &daemon_arn, CASE).await;
    let created_at = created["createdAt"].as_f64().unwrap_or_else(|| {
        panic!("case {CASE:?}: expected the instant the daemon was created, got {created}")
    });
    let mut updated_at = created["updatedAt"].as_f64().unwrap_or_default();

    // The revision the CREATE captured, with the configuration it snapshotted: the create
    // states no optional member either, so it is the same documented defaults an update
    // that states none has to report.
    let opening = describe_deployment(&router, &first_deployment, CASE).await;
    let first_revision = string(&opening["targetDaemonRevision"], "arn", CASE);
    let first_captured = snapshot(
        &describe_revision(&router, &first_revision, CASE).await,
        CASE,
    );
    assert_eq!(
        first_captured,
        stated_snapshot(("NONE", false, false, true)),
        "case {CASE:?}: expected the documented defaults of the members the create left \
         out, got {first_captured:?}"
    );
    let mut deployments = vec![first_deployment];
    let mut captured = vec![(first_revision, first_captured)];

    // The rows differ along what the rule generalises over: WHICH optional members the
    // update states and with which values, how the daemon task definition is referenced,
    // and how much capacity the update deploys onto. `propagateTags` is taken over the whole
    // published enum (`smithy_query(query="shape", shape="DaemonPropagateTags")`: `DAEMON`,
    // `NONE`), and the deployment configuration's drain percentage over the interval its
    // member documents ("Valid values are between 0.0 and 100.0").
    let rows = [
        UpdateRow {
            case: "an update that states nothing optional",
            by_family: false,
            definition: 1,
            providers: 1,
            stated: || json!({}),
            expected: ("NONE", false, false, true),
        },
        UpdateRow {
            case: "an update that states every optional member, over both providers",
            by_family: false,
            definition: 0,
            providers: 2,
            stated: || {
                json!({
                    "critical": false,
                    "propagateTags": "DAEMON",
                    "enableECSManagedTags": true,
                    "enableExecuteCommand": true,
                    "deploymentConfiguration": {
                        "drainPercent": 25.0,
                        "bakeTimeInMinutes": 5,
                        "alarms": { "alarmNames": ["daemon-errors"], "enable": true },
                    },
                })
            },
            expected: ("DAEMON", true, true, false),
        },
        UpdateRow {
            case: "an update that leaves out every member the one before it stated",
            by_family: false,
            definition: 1,
            providers: 1,
            stated: || json!({}),
            expected: ("NONE", false, false, true),
        },
        UpdateRow {
            case: "an update that states those members explicitly, by family reference",
            by_family: true,
            definition: 1,
            providers: 2,
            stated: || {
                json!({
                    "critical": true,
                    "propagateTags": "NONE",
                    "enableECSManagedTags": false,
                    "enableExecuteCommand": false,
                    "deploymentConfiguration": { "drainPercent": 0.0 },
                })
            },
            expected: ("NONE", false, false, true),
        },
        UpdateRow {
            case: "an update that drains every container instance at once",
            by_family: false,
            definition: 1,
            providers: 1,
            stated: || {
                json!({
                    "propagateTags": "DAEMON",
                    "deploymentConfiguration": {
                        "drainPercent": 100.0,
                        "bakeTimeInMinutes": 0,
                        "alarms": { "alarmNames": [], "enable": false },
                    },
                })
            },
            expected: ("DAEMON", false, false, true),
        },
    ];

    for row in &rows {
        let case = row.case;
        let reference = if row.by_family {
            FAMILY.to_string()
        } else {
            definitions[row.definition].clone()
        };
        // A family alone names the latest ACTIVE revision of it, which is the last one
        // registered above; an ARN names the revision it carries.
        let resolved = if row.by_family {
            definitions.len() - 1
        } else {
            row.definition
        };
        let deploying: Vec<String> = providers[..row.providers].to_vec();
        let stated = (row.stated)();
        let mut request = json!({
            "daemonArn": daemon_arn,
            "daemonTaskDefinitionArn": reference,
            "capacityProviderArns": deploying.clone(),
        });
        for (member, value) in stated
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: the row's stated members are an object"))
        {
            request[member.as_str()] = value.clone();
        }

        let answer = ok(&router, "UpdateDaemon", request, case).await;
        assert_eq!(
            string(&answer, "daemonArn", case),
            daemon_arn,
            "case {case:?}: expected the daemon the update names back, got {answer}"
        );
        assert_eq!(
            string(&answer, "status", case),
            "ACTIVE",
            "case {case:?}: an updated daemon is still ACTIVE, got {answer}"
        );
        assert_eq!(
            answer["createdAt"].as_f64(),
            Some(created_at),
            "case {case:?}: an update never moves the instant the daemon was created, got \
             {answer}"
        );
        let answered_updated_at = answer["updatedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: expected the instant the update was made, got {answer}")
        });
        assert!(
            answered_updated_at >= updated_at,
            "case {case:?}: expected an updatedAt not earlier than {updated_at}, got {answer}"
        );
        updated_at = answered_updated_at;
        let deployment_arn = string(&answer, "deploymentArn", case);
        assert!(
            !deployments.contains(&deployment_arn),
            "case {case:?}: an update triggers a NEW deployment, but {deployment_arn:?} was \
             reported twice"
        );
        deployments.push(deployment_arn.clone());

        // The rollout the update started: it deploys a revision none of the earlier ones
        // was, over exactly the capacity the request stated.
        let deployment = describe_deployment(&router, &deployment_arn, case).await;
        let target = deployment["targetDaemonRevision"].clone();
        let revision_arn = string(&target, "arn", case);
        assert!(
            !captured.iter().any(|(arn, _)| *arn == revision_arn),
            "case {case:?}: an update captures a NEW revision, got {deployment}"
        );
        assert_eq!(
            list(&target, "capacityProviders", case)
                .iter()
                .map(|entry| string(entry, "arn", case))
                .collect::<Vec<String>>(),
            deploying,
            "case {case:?}: expected the rollout over exactly the capacity the update \
             stated, got {deployment}"
        );

        // "Optional deployment parameters that control how the daemon rolls out updates":
        // the rollout runs under the configuration the request carried, member for member —
        // and under none at all where the request carried none.
        match stated.get("deploymentConfiguration") {
            Some(configuration) => {
                let reported = deployment["deploymentConfiguration"].clone();
                assert_eq!(
                    reported["drainPercent"].as_f64(),
                    configuration["drainPercent"].as_f64(),
                    "case {case:?}: expected the drain percentage the update stated, got \
                     {reported}"
                );
                // "The default value is 0" for a configuration that states no bake time.
                assert_eq!(
                    reported["bakeTimeInMinutes"].as_i64(),
                    Some(configuration["bakeTimeInMinutes"].as_i64().unwrap_or(0)),
                    "case {case:?}: expected the bake time the update stated, got {reported}"
                );
                let alarms = deployment["alarms"].clone();
                assert_eq!(
                    list(&alarms, "alarmNames", case),
                    configuration["alarms"]["alarmNames"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                    "case {case:?}: expected the alarms the update stated, got {alarms}"
                );
                // "Amazon ECS is not using alarms for daemon deployment failures when the
                // status is DISABLED": a configuration that enables none leaves them off,
                // and one that enables them is watching the rollout or has finished doing so.
                let status = string(&alarms, "status", case);
                if configuration["alarms"]["enable"].as_bool().unwrap_or(false) {
                    assert!(
                        ["MONITORING", "MONITORING_COMPLETE"].contains(&status.as_str()),
                        "case {case:?}: expected the alarms of the rollout to be watched, \
                         got {alarms}"
                    );
                } else {
                    assert_eq!(
                        status, "DISABLED",
                        "case {case:?}: expected no alarm option on the rollout, got {alarms}"
                    );
                }
            }
            None => assert!(
                deployment
                    .get("deploymentConfiguration")
                    .is_none_or(Value::is_null),
                "case {case:?}: expected no deployment configuration on a rollout the \
                 update stated none for, got {deployment}"
            ),
        }

        // The revision the update captured, read back: the daemon task definition the
        // reference resolved to, and the configuration the request stated with the
        // documented default of every optional member it left out.
        let revision = describe_revision(&router, &revision_arn, case).await;
        assert_eq!(
            string(&revision, "daemonArn", case),
            daemon_arn,
            "case {case:?}: a revision names the daemon it belongs to, got {revision}"
        );
        assert_eq!(
            string(&revision, "daemonTaskDefinitionArn", case),
            definitions[resolved],
            "case {case:?}: expected the revision the reference resolves to, got {revision}"
        );
        assert_eq!(
            list(&revision, "containerImages", case)
                .iter()
                .map(|entry| string(entry, "image", case))
                .collect::<Vec<String>>(),
            vec![images[resolved].to_string()],
            "case {case:?}: expected the images of the definition it runs, got {revision}"
        );
        let reported = snapshot(&revision, case);
        assert_eq!(
            reported,
            stated_snapshot(row.expected),
            "case {case:?}: expected the configuration the update states, got {revision}"
        );

        // The daemon itself follows the update: it names the rollout this call started, is
        // deploying the revision that rollout targets, and dates the two in agreement with
        // what the call answered.
        let daemon = describe_daemon(&router, &daemon_arn, case).await;
        assert_eq!(
            string(&daemon, "deploymentArn", case),
            deployment_arn,
            "case {case:?}: a daemon names its most recent deployment, got {daemon}"
        );
        assert_eq!(
            daemon["createdAt"].as_f64(),
            Some(created_at),
            "case {case:?}: expected the instant the daemon was created, got {daemon}"
        );
        assert_eq!(
            daemon["updatedAt"].as_f64(),
            Some(answered_updated_at),
            "case {case:?}: expected the daemon to date the update as the call did, got \
             {daemon}"
        );
        assert!(
            list(&daemon, "currentRevisions", case)
                .iter()
                .any(|detail| string(detail, "arn", case) == revision_arn),
            "case {case:?}: expected the daemon to be running the revision the update \
             captured, got {daemon}"
        );

        captured.push((revision_arn, reported));
        // "A daemon revision is a snapshot of the daemon's configuration at the time a
        // deployment was initiated": every revision captured up to here still reports
        // exactly the configuration it captured, however the daemon has been updated since.
        for (arn, kept) in &captured {
            let still = describe_revision(&router, arn, case).await;
            assert_eq!(
                &snapshot(&still, case),
                kept,
                "case {case:?}: expected {arn:?} to keep the configuration it captured, \
                 got {still}"
            );
        }
    }
}
