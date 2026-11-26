// SPDX-License-Identifier: CC0-1.0

//! What the SERVICE SCHEDULER reports about the tasks it starts and the tasks it retires, and
//! the name of a service a delete is still draining away — driven end to end over the shared
//! wire harness in [`crate::ecs_wire`].
//!
//! The rules under test, each over the whole class rather than one request:
//!
//! > 1. `Service$events` is "The event stream for your service", and the scheduler's account
//! >    of a launch it made belongs there: when tasks of a service have started, that
//! >    service's stream carries an entry naming each of them — `(service <name>) has started
//! >    <n> tasks: (task <id>).` — beside the deployment and steady-state entries the rollout
//! >    earns. So the task ids the start entries name are exactly the tasks the service holds
//! >    RUNNING, each named once, however the count was reached: placed by the create, or by a
//! >    later raise of `desiredCount`.
//! > 2. A task the scheduler takes out of a service's task set carries
//! >    `stopCode: ServiceSchedulerInitiated` and ONE sentence — `Scaling activity initiated
//! >    by (deployment <id>)` — naming the deployment that STARTED the task
//! >    (`Task$startedBy`). The sentence is the same whether the task is retired because the
//! >    service wants fewer tasks or because a newer deployment is replacing it, so on a
//! >    rolling update the retired task names its OWN deployment and not the replacement; and
//! >    the retirement is the scheduler's own work, so the service's event stream names the
//! >    task it stopped — `(service <name>) has stopped <n> running tasks: (task <id>).`
//! > 3. A `DeleteService` leaves the service `DRAINING` until the tasks it was cleaning up
//! >    have stopped ("After all tasks have transitioned to either STOPPING or STOPPED status,
//! >    the service status moves from DRAINING to INACTIVE"), and the name belongs to it until
//! >    then: a create of the same name while it drains is refused with
//! >    `InvalidParameterException` and the sentence that states the drain, and the same create
//! >    succeeds once the drain has finished.
//!
//! The tables span how the task set was reached (the create's own count and a later raise),
//! the two retirements a scheduler makes (scale-in and rolling replacement), and whether the
//! draining service still holds a task. Nothing here reaches into the state struct: every
//! value is read back through `DescribeServices`, `ListTasks` and `DescribeTasks`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, poll_until, refusal, register_definition, service, string,
};

/// The cluster every service of this suite runs in.
const CLUSTER: &str = "schedulerreports";

/// The task-definition family they run.
const FAMILY: &str = "schedulerreportsfamily";

/// The refusal class a create of a draining name is reported under, and the sentence it
/// carries (`state`'s `SERVICE_STILL_DRAINING_MESSAGE`).
const INVALID_PARAMETER: &str = "InvalidParameterException";
const STILL_DRAINING: &str = "Unable to Start a service that is still Draining.";

/// `Task$stopCode` for a task the service scheduler stopped.
const SERVICE_SCHEDULER_INITIATED: &str = "ServiceSchedulerInitiated";

/// One service as `DescribeServices` reports it.
async fn describe(router: &MultiServiceRouter, service_name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [service_name] }),
        case,
    )
    .await;
    list(&body, "services", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service {service_name} back"))
}

/// The messages of a service's event stream, newest first.
fn event_messages(service_view: &Value, case: &str) -> Vec<String> {
    list(service_view, "events", case)
        .iter()
        .map(|event| string(event, "message", case))
        .collect()
}

/// The task ids the START entries of `messages` name, in the order the entries carry them.
///
/// The entry's shape is the scheduler's own — `... has started <n> tasks: (task <id>) ...` —
/// so the ids are read out of the `(task <id>)` clauses of the entries that announce a start.
fn started_task_ids(messages: &[String]) -> Vec<String> {
    let mut ids = Vec::new();
    for message in messages.iter().filter(|message| message.contains("has started")) {
        for clause in message.split("(task ").skip(1) {
            if let Some((id, _)) = clause.split_once(')') {
                ids.push(id.to_string());
            }
        }
    }
    ids.sort();
    ids
}

/// The ARNs of the tasks of `service_name` the cluster still wants running.
async fn live_task_arns(
    router: &MultiServiceRouter,
    service_name: &str,
    case: &str,
) -> Vec<String> {
    let listed = ok(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "desiredStatus": "RUNNING",
        }),
        case,
    )
    .await;
    let mut arns: Vec<String> = list(&listed, "taskArns", case)
        .into_iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect();
    arns.sort();
    arns
}

/// The tasks `arns` names, as `DescribeTasks` reports them.
async fn describe_tasks(
    router: &MultiServiceRouter,
    arns: &[String],
    case: &str,
) -> Vec<Value> {
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    list(&described, "tasks", case)
}

/// ONE task, as `DescribeTasks` reports it — the empty object when the task is no longer
/// reported at all, so a poll over it reads a value either way.
async fn describe_task(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    describe_tasks(router, &[arn.to_string()], case)
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| json!({}))
}

/// The ids of the tasks of `service_name` that are RUNNING, sorted.
async fn running_task_ids(
    router: &MultiServiceRouter,
    service_name: &str,
    case: &str,
) -> Vec<String> {
    let arns = live_task_arns(router, service_name, case).await;
    let mut ids: Vec<String> = describe_tasks(router, &arns, case)
        .await
        .iter()
        .filter(|task| task["lastStatus"] == json!("RUNNING"))
        .map(|task| {
            string(task, "taskArn", case)
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

/// Create one service of `desired` tasks.
async fn create_service(
    router: &MultiServiceRouter,
    service_name: &str,
    desired: i64,
    case: &str,
) -> Value {
    let created = ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "taskDefinition": FAMILY,
            "desiredCount": desired,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    created["service"].clone()
}

/// Ask the service for `desired` tasks.
async fn scale_to(router: &MultiServiceRouter, service_name: &str, desired: i64, case: &str) {
    ok(
        router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": service_name,
            "desiredCount": desired,
        }),
        case,
    )
    .await;
}

/// The service's `PRIMARY` deployment id.
fn primary_deployment_id(service_view: &Value, case: &str) -> String {
    list(service_view, "deployments", case)
        .into_iter()
        .find(|deployment| deployment["status"] == json!("PRIMARY"))
        .map(|deployment| string(&deployment, "id", case))
        .unwrap_or_else(|| panic!("case {case:?}: expected a PRIMARY deployment"))
}

#[tokio::test]
async fn the_start_entries_name_each_task_the_scheduler_started() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "setup").await;
    register_definition(&router, FAMILY, "setup").await;

    // (case, the count the create asks for, the count a later update raises it to)
    let rows: &[(&str, i64, Option<i64>)] = &[
        ("a service created with no tasks and raised to one", 0, Some(1)),
        ("a service created with two tasks", 2, None),
        ("a service created with one task and raised to three", 1, Some(3)),
    ];
    for (index, (case, created_with, raised_to)) in rows.iter().enumerate() {
        let service_name = format!("started{index}");
        let created = create_service(&router, &service_name, *created_with, case).await;
        // The create has placed tasks but none has STARTED, so the scheduler has announced
        // nothing yet.
        assert!(
            started_task_ids(&event_messages(&created, case)).is_empty(),
            "case {case:?}: a create announces no start of its own, got {created}"
        );
        let wanted = raised_to.unwrap_or(*created_with);
        if let Some(raised_to) = raised_to {
            scale_to(&router, &service_name, *raised_to, case).await;
        }

        // The starts are the CLOCK's, so the stream is polled for the condition rather than
        // timed: every task the service holds running is named by a start entry.
        let settled = poll_until(
            case,
            &format!("{wanted} start entries on the service's event stream"),
            || describe(&router, &service_name, case),
            |view| started_task_ids(&event_messages(view, case)).len() as i64 == wanted,
        )
        .await;
        let named = started_task_ids(&event_messages(&settled, case));
        let running = running_task_ids(&router, &service_name, case).await;
        assert_eq!(
            named, running,
            "case {case:?}: expected the start entries to name each RUNNING task exactly \
             once, got {named:?} named and {running:?} running"
        );
        let messages = event_messages(&settled, case);
        let announced: Vec<&String> = messages
            .iter()
            .filter(|message| message.contains("has started"))
            .collect();
        assert_eq!(
            announced.len(),
            wanted as usize,
            "case {case:?}: expected one start entry per task the scheduler started, got \
             {messages:?}"
        );
        for message in announced {
            assert!(
                message.starts_with(&format!("(service {service_name}) has started 1 tasks: ("))
                    && message.ends_with(").",),
                "case {case:?}: expected the scheduler's own entry shape, got {message:?}"
            );
        }

        // Reading again neither duplicates an entry nor announces a start twice.
        let again = describe(&router, &service_name, case).await;
        assert_eq!(
            started_task_ids(&event_messages(&again, case)),
            named,
            "case {case:?}: a second read announces no further start, got {again}"
        );
    }
}

#[tokio::test]
async fn a_task_the_scheduler_retires_names_the_deployment_that_started_it() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "setup").await;
    register_definition(&router, FAMILY, "setup").await;

    // Both retirements a service's scheduler makes: the service wants fewer tasks, and a
    // newer deployment replaces the task set the service holds. The sentence is one template
    // either way, and the deployment it names is the one that STARTED the retired task.
    for (index, case) in ["a scale-in", "a rolling replacement"].iter().enumerate() {
        let service_name = format!("retired{index}");
        create_service(&router, &service_name, 1, case).await;
        let settled = poll_until(
            case,
            "the service to hold one running task",
            || describe(&router, &service_name, case),
            |view| view["runningCount"] == json!(1),
        )
        .await;
        let first_deployment = primary_deployment_id(&settled, case);
        let retired = live_task_arns(&router, &service_name, case).await;
        let retired = retired
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the service's task"))
            .clone();

        if index == 0 {
            scale_to(&router, &service_name, 0, case).await;
        } else {
            // A new revision of the same family is a new deployment, which replaces the task
            // set the service holds.
            let next = ok(
                &router,
                "RegisterTaskDefinition",
                json!({
                    "family": FAMILY,
                    "containerDefinitions": [
                        crate::task_lifecycle::container(
                            "web",
                            "public.ecr.aws/nginx/nginx:stable",
                        ),
                    ],
                }),
                case,
            )
            .await;
            let revision = string(&next["taskDefinition"], "taskDefinitionArn", case);
            ok(
                &router,
                "UpdateService",
                json!({
                    "cluster": CLUSTER,
                    "service": &service_name,
                    "taskDefinition": revision,
                }),
                case,
            )
            .await;
        }

        // The stop is the CLOCK's, so the task is polled for the condition.
        let stopped = poll_until(
            case,
            "the retired task to carry the scheduler's stop",
            || describe_task(&router, &retired, case),
            |task| task["stoppedReason"].is_string(),
        )
        .await;
        let started_by = string(&stopped, "startedBy", case);
        assert_eq!(
            started_by, first_deployment,
            "case {case:?}: the retired task was started by the first deployment, got \
             {stopped}"
        );
        assert_eq!(
            string(&stopped, "stopCode", case),
            SERVICE_SCHEDULER_INITIATED,
            "case {case:?}: expected the scheduler's own stop code, got {stopped}"
        );
        assert_eq!(
            string(&stopped, "stoppedReason", case),
            format!("Scaling activity initiated by (deployment {started_by})"),
            "case {case:?}: expected the scheduler's one sentence naming the task's own \
             deployment, got {stopped}"
        );
        // The retirement is the scheduler's own work, so the service's event stream carries
        // its account of it, naming the task it stopped.
        let task_id = string(&stopped, "taskArn", case)
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let announced = format!(
            "(service {service_name}) has stopped 1 running tasks: (task {task_id})."
        );
        let reported = poll_until(
            case,
            "the scheduler's account of the task it stopped",
            || describe(&router, &service_name, case),
            |view| event_messages(view, case).contains(&announced),
        )
        .await;
        assert!(
            event_messages(&reported, case).contains(&announced),
            "case {case:?}: expected the stream to carry {announced:?}, got {reported}"
        );
        if index == 1 {
            // The replacement's deployment is a DIFFERENT one, which the retired task does
            // not name: that is what makes the sentence the retired task's own.
            let rolled = describe(&router, &service_name, case).await;
            let replacement = primary_deployment_id(&rolled, case);
            assert_ne!(
                replacement, first_deployment,
                "case {case:?}: a new revision starts a new deployment, got {rolled}"
            );
            assert!(
                !string(&stopped, "stoppedReason", case).contains(&replacement),
                "case {case:?}: the retired task names its own deployment and not the \
                 replacement, got {stopped}"
            );
        }
    }
}

#[tokio::test]
async fn a_name_a_draining_service_still_holds_is_refused_for_the_drain() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "setup").await;
    register_definition(&router, FAMILY, "setup").await;

    // Rows differ along what the deleted service still holds: a task that has to stop before
    // the drain can finish, and no task at all. The name is held by the DRAINING record
    // either way.
    for (index, (case, desired)) in [
        ("a draining service that held a running task", 1_i64),
        ("a draining service that held no task", 0),
    ]
    .iter()
    .enumerate()
    {
        let service_name = format!("draining{index}");
        create_service(&router, &service_name, *desired, case).await;
        if *desired > 0 {
            poll_until(
                case,
                "the service to hold its running task",
                || describe(&router, &service_name, case),
                |view| view["runningCount"] == json!(*desired),
            )
            .await;
        }
        ok(
            &router,
            "DeleteService",
            json!({
                "cluster": CLUSTER,
                "service": &service_name,
                // "You can delete a service if ... it has no running tasks ... If the
                // service is actively maintaining tasks, you can't delete it ... you must
                // use the force option."
                "force": *desired > 0,
            }),
            case,
        )
        .await;

        // The create of the taken name, made while the predecessor drains: the read AFTER it
        // is what shows the attempt landed inside the drain, which is the service's own
        // transition span rather than anything this suite waits out.
        let (code, message) = refusal(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": &service_name,
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "launchType": "EC2",
            }),
            case,
        )
        .await;
        let during = describe(&router, &service_name, case).await;
        assert_eq!(
            string(&during, "status", case),
            "DRAINING",
            "case {case:?}: the refused create was made while the predecessor drained, got \
             {during}"
        );
        if *desired > 0 {
            // The delete stopped a RUNNING task, which is the scheduler's work: the service's
            // stream names it, exactly as it does for a task retired from a task set.
            let stopped = event_messages(&during, case)
                .into_iter()
                .filter(|message| message.contains("has stopped"))
                .collect::<Vec<String>>();
            assert_eq!(
                stopped.len(),
                1,
                "case {case:?}: expected the scheduler's account of the task the delete \
                 stopped, got {during}"
            );
            assert!(
                stopped[0].starts_with(&format!(
                    "(service {service_name}) has stopped 1 running tasks: (task "
                )) && stopped[0].ends_with(")."),
                "case {case:?}: expected the scheduler's own entry shape, got {stopped:?}"
            );
        }
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected an {INVALID_PARAMETER}, got {code:?} with {message:?}"
        );
        assert_eq!(
            message, STILL_DRAINING,
            "case {case:?}: expected the fixed sentence {STILL_DRAINING:?}, got {message:?}"
        );

        // The name becomes free on its own: once the drain has finished the SAME create is
        // answered with a new service.
        poll_until(
            case,
            "the drain to finish",
            || describe(&router, &service_name, case),
            |view| view["status"] == json!("INACTIVE"),
        )
        .await;
        let recreated = create_service(&router, &service_name, 0, case).await;
        assert_eq!(
            string(&recreated, "status", case),
            "ACTIVE",
            "case {case:?}: a drained name is free to create again, got {recreated}"
        );
    }
}
