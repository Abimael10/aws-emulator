// SPDX-License-Identifier: CC0-1.0

//! `DeregisterContainerInstance`: the `force` a host that still holds work needs.
//!
//! The rule under test: a deregistration of a container instance that still holds a task
//! which has not reached `STOPPED` is refused — HTTP 400, `InvalidParameterException`,
//! [`RUNNING_TASKS`] — and leaves the host exactly as it was, unless the call asked for
//! `force`, the member that "forces the container instance to be deregistered. If you have
//! tasks running on the container instance when you deregister it with the `force` option,
//! these tasks remain running ... but they're orphaned". A host that holds no such task —
//! one nothing was ever placed on, one whose tasks have all stopped, one whose cluster-mate
//! is the busy instance — is deregistered by the same force-less call, which takes it out of
//! `ListContainerInstances` and leaves it `INACTIVE`.
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, so nothing reaches into the state struct.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    count, create_cluster, list, ok, poll_until, refusal, register_definition, register_instance,
    string,
};

/// `InvalidParameterException$message` for the force-less deregistration of a host that
/// still holds a task: one fixed sentence, carrying neither the host, nor the cluster, nor
/// how many tasks it holds.
const RUNNING_TASKS: &str = "Found running tasks on the instance.";

/// The modelled error such a refusal carries (`DeregisterContainerInstance` errors include
/// `InvalidParameterException`, HTTP 400).
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The family every case of this suite places.
const FAMILY: &str = "deregforced";

/// `ContainerInstanceStatus` values this suite reads back
/// (`ContainerInstance$status`: "ACTIVE", "DRAINING", "REGISTERING", "DEREGISTERING",
/// "REGISTRATION_FAILED").
const ACTIVE: &str = "ACTIVE";
const INACTIVE: &str = "INACTIVE";

/// `Task$lastStatus`/`$desiredStatus` values this suite reads back.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// One container instance, as `DescribeContainerInstances` reports it. The status of a
/// deregistered host is still readable there, so this read answers for both outcomes.
async fn describe_instance(
    router: &MultiServiceRouter,
    cluster: &str,
    instance_arn: &str,
    case: &str,
) -> Value {
    let body = ok(
        router,
        "DescribeContainerInstances",
        json!({ "cluster": cluster, "containerInstances": [instance_arn] }),
        case,
    )
    .await;
    list(&body, "containerInstances", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the instance back, got {body}"))
}

/// One task, as `DescribeTasks` reports it.
async fn describe_task(
    router: &MultiServiceRouter,
    cluster: &str,
    task_arn: &str,
    case: &str,
) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [task_arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// The container instance ARNs `ListContainerInstances` answers for `cluster` — the listing
/// that leaves a deregistered host out, because it reports the cluster's capacity.
async fn listed_instances(
    router: &MultiServiceRouter,
    cluster: &str,
    case: &str,
) -> Vec<String> {
    let body = ok(
        router,
        "ListContainerInstances",
        json!({ "cluster": cluster }),
        case,
    )
    .await;
    list(&body, "containerInstanceArns", case)
        .iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Place ONE task on `instance_arn` through `StartTask` and answer its ARN.
async fn place_task(
    router: &MultiServiceRouter,
    cluster: &str,
    instance_arn: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "StartTask",
        json!({
            "cluster": cluster,
            "taskDefinition": FAMILY,
            "containerInstances": [instance_arn],
        }),
        case,
    )
    .await;
    let task = list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one task placed, got {body}"));
    string(&task, "taskArn", case)
}

/// Stop the task and look again until it has actually reached `STOPPED`: where a task is
/// left is the CLOCK's, so this waits for the CONDITION rather than for a span.
async fn stop_and_settle(
    router: &MultiServiceRouter,
    cluster: &str,
    task_arn: &str,
    case: &str,
) {
    ok(
        router,
        "StopTask",
        json!({ "cluster": cluster, "task": task_arn, "reason": "making the host idle" }),
        case,
    )
    .await;
    poll_until(
        case,
        "the stopped task to reach STOPPED",
        || describe_task(router, cluster, task_arn, case),
        |task| task["lastStatus"] == json!(STOPPED),
    )
    .await;
}

/// One row of the table: the work the host holds when the call arrives, what the call sends
/// for `force`, and whether that call is refused.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// Tasks placed on the host and LEFT as they are, so each still holds it.
    holding: usize,
    /// Tasks placed on the host and then stopped, so each holds nothing.
    released: usize,
    /// A task left holding ANOTHER container instance of the same cluster.
    neighbour: bool,
    /// What the request spells for `force`: absent, or the boolean it sends.
    force: Option<bool>,
    refused: bool,
}

#[tokio::test]
async fn deregister_container_instance_needs_force_while_the_host_still_holds_a_task() {
    let router = crate::ecs_wire::service();
    register_definition(&router, FAMILY, "setup").await;

    // The rows differ along everything the rule generalises over: whether the host holds
    // work at all, HOW MANY tasks it holds, whether the tasks it held have since stopped,
    // whether a MIXTURE of stopped and holding tasks is on it, whether the holding task is
    // on this host or on a cluster-mate, and how the call spells `force` (absent, false,
    // true).
    let rows = [
        Row {
            case: "a host holding one task, no force member at all",
            holding: 1,
            released: 0,
            neighbour: false,
            force: None,
            refused: true,
        },
        Row {
            case: "a host holding one task, force spelled false",
            holding: 1,
            released: 0,
            neighbour: false,
            force: Some(false),
            refused: true,
        },
        Row {
            case: "a host holding three tasks, no force",
            holding: 3,
            released: 0,
            neighbour: false,
            force: None,
            refused: true,
        },
        Row {
            case: "a host holding one task beside two it has released, no force",
            holding: 1,
            released: 2,
            neighbour: false,
            force: None,
            refused: true,
        },
        Row {
            case: "a host holding one task, forced",
            holding: 1,
            released: 0,
            neighbour: false,
            force: Some(true),
            refused: false,
        },
        Row {
            case: "an idle host nothing was ever placed on, no force",
            holding: 0,
            released: 0,
            neighbour: false,
            force: None,
            refused: false,
        },
        Row {
            case: "an idle host, forced anyway",
            holding: 0,
            released: 0,
            neighbour: false,
            force: Some(true),
            refused: false,
        },
        Row {
            case: "a host whose two tasks have both stopped, no force",
            holding: 0,
            released: 2,
            neighbour: false,
            force: None,
            refused: false,
        },
        Row {
            case: "an idle host whose cluster-mate holds a task, no force",
            holding: 0,
            released: 0,
            neighbour: true,
            force: None,
            refused: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // Each row gets its OWN cluster, so the work one row places is never the work
        // another row's deregistration finds.
        let cluster = format!("forced{index}");
        create_cluster(&router, &cluster, case).await;
        let host =
            register_instance(&router, &cluster, &format!("i-{index:04x}{:013x}", 1), case).await;

        for _ in 0..row.released {
            let task = place_task(&router, &cluster, &host, case).await;
            stop_and_settle(&router, &cluster, &task, case).await;
        }
        let mut holding = Vec::new();
        for _ in 0..row.holding {
            holding.push(place_task(&router, &cluster, &host, case).await);
        }
        if row.neighbour {
            let mate =
                register_instance(&router, &cluster, &format!("i-{index:04x}{:013x}", 2), case)
                    .await;
            place_task(&router, &cluster, &mate, case).await;
        }

        let before = describe_instance(&router, &cluster, &host, case).await;
        assert_eq!(
            string(&before, "status", case),
            ACTIVE,
            "case {case:?}: expected a registered host to start out ACTIVE, got {before}"
        );
        let version_before = count(&before, "version", case);

        let mut request = json!({ "cluster": cluster, "containerInstance": host });
        if let Some(force) = row.force {
            request["force"] = json!(force);
        }

        if row.refused {
            let (code, message) =
                refusal(&router, "DeregisterContainerInstance", request, case).await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?}: expected a {INVALID_PARAMETER}, got {code:?} with {message:?}"
            );
            assert_eq!(
                message, RUNNING_TASKS,
                "case {case:?}: expected the fixed sentence for a host that still holds \
                 work, got {message:?}"
            );

            // The refused call changed NOTHING: the host is still ACTIVE, still has its
            // agent, still carries the version it had, and is still the cluster's capacity.
            let after = describe_instance(&router, &cluster, &host, case).await;
            assert_eq!(
                string(&after, "status", case),
                ACTIVE,
                "case {case:?}: a refused deregistration leaves the host registered, got \
                 {after}"
            );
            assert_eq!(
                after["agentConnected"],
                json!(true),
                "case {case:?}: a refused deregistration leaves the agent connected, got \
                 {after}"
            );
            assert_eq!(
                count(&after, "version", case),
                version_before,
                "case {case:?}: a call that changes nothing does not move the version, got \
                 {after}"
            );
            let listed = listed_instances(&router, &cluster, case).await;
            assert!(
                listed.contains(&host),
                "case {case:?}: a refused deregistration leaves the host in the cluster's \
                 capacity, got {listed:?}"
            );
        } else {
            let body = ok(&router, "DeregisterContainerInstance", request, case).await;
            let reported = &body["containerInstance"];
            assert_eq!(
                string(reported, "containerInstanceArn", case),
                host,
                "case {case:?}: expected the deregistered host back, got {body}"
            );
            assert_eq!(
                string(reported, "status", case),
                INACTIVE,
                "case {case:?}: a deregistered host is INACTIVE, got {body}"
            );

            // The transition is STORED, not just answered: the describe reads it back and
            // the capacity listing no longer selects it.
            let after = describe_instance(&router, &cluster, &host, case).await;
            assert_eq!(
                string(&after, "status", case),
                INACTIVE,
                "case {case:?}: expected the stored host to be INACTIVE, got {after}"
            );
            assert_eq!(
                after["agentConnected"],
                json!(false),
                "case {case:?}: a deregistered host's agent no longer reports, got {after}"
            );
            assert!(
                count(&after, "version", case) > version_before,
                "case {case:?}: a host that changed carries a higher version than {version_before}, got {after}"
            );
            let listed = listed_instances(&router, &cluster, case).await;
            assert!(
                !listed.contains(&host),
                "case {case:?}: a deregistered host is out of the cluster's capacity, got \
                 {listed:?}"
            );
        }

        // Whichever way the call went, the work on the host is left exactly as it stood:
        // the refused call stopped nothing, and the forced one orphans its tasks rather
        // than stopping them ("these tasks remain running until you terminate the instance
        // or the tasks stop through some other means").
        for task_arn in &holding {
            let task = describe_task(&router, &cluster, task_arn, case).await;
            assert_eq!(
                string(&task, "desiredStatus", case),
                RUNNING,
                "case {case:?}: expected the task on the host to still be wanted running, \
                 got {task}"
            );
            assert_ne!(
                string(&task, "lastStatus", case),
                STOPPED,
                "case {case:?}: expected the task on the host not to have been stopped, got \
                 {task}"
            );
        }
    }
}

#[tokio::test]
async fn the_same_force_less_deregistration_is_served_once_every_task_has_stopped() {
    let router = crate::ecs_wire::service();
    let case = "the documented stop-the-tasks-first workflow";
    register_definition(&router, FAMILY, case).await;
    let cluster = "stopthenderegister";
    create_cluster(&router, cluster, case).await;
    let host = register_instance(&router, cluster, "i-0workflow0000001", case).await;
    let task = place_task(&router, cluster, &host, case).await;

    // The identical request, sent twice: refused while the task holds the host, served once
    // it has stopped.
    let request = json!({ "cluster": cluster, "containerInstance": host.clone() });
    let (code, message) = refusal(
        &router,
        "DeregisterContainerInstance",
        request.clone(),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (INVALID_PARAMETER, RUNNING_TASKS),
        "case {case:?}: expected the force-less call to be refused while the task holds the \
         host, got {code:?} with {message:?}"
    );
    let listed = listed_instances(&router, cluster, case).await;
    assert!(
        listed.contains(&host),
        "case {case:?}: expected the host to still be the cluster's capacity, got {listed:?}"
    );

    stop_and_settle(&router, cluster, &task, case).await;

    let body = ok(&router, "DeregisterContainerInstance", request, case).await;
    assert_eq!(
        string(&body["containerInstance"], "status", case),
        INACTIVE,
        "case {case:?}: expected the same call to be served once the task stopped, got {body}"
    );
    let listed = listed_instances(&router, cluster, case).await;
    assert!(
        !listed.contains(&host),
        "case {case:?}: expected the deregistered host to be out of the capacity listing, \
         got {listed:?}"
    );
}
