// SPDX-License-Identifier: CC0-1.0

//! Amazon ECS publishes an `ECS Service Action` event every time its SERVICE scheduler acts.
//!
//! The RULE under test: every action the service scheduler takes is published onto the
//! account's DEFAULT EventBridge bus as `source` `aws.ecs` / `detail-type` `ECS Service
//! Action`, whose `detail` names the action in `eventName`, how serious it is in `eventType`,
//! the cluster it happened in in `clusterArn` and the instant it happened at in `createdAt`,
//! and whose `resources` names the resource the action is about. EventBridge then routes it,
//! so a rule whose `EventPattern` matches delivers the envelope to that rule's targets and a
//! rule that cannot match receives nothing. The actions are owed by the CLOCK, so they are
//! published with no further Amazon ECS call: a client that reacts to a service through
//! EventBridge makes none.
//!
//! The rows span what the rule generalises over: WHICH action the scheduler takes (the steady
//! state a service's own rollout settles into, and the steady state a task set of a service
//! whose deployments another controller drives reaches), the launch type and network mode the
//! service places its tasks with — and so how long the scheduler takes to get there — and
//! whether it places any task at all.
//!
//! EventBridge and Amazon SQS are mounted BESIDE Amazon ECS on the SAME engine (the harness
//! [`crate::task_state_change_events::emulator`] builds for the task events of the same
//! bus), so a publication travels the real in-process callback, through the real bus, to a
//! real SQS target.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, list, ok, register_capacity, string, ACCOUNT, AWSVPC_START,
    SIGNED_REGION,
};
use crate::task_lifecycle::container;
use crate::task_state_change_events::{
    drain, emulator, given_queue, given_rule, grant_delivery,
};

/// The `source` and `detail-type` Amazon ECS publishes a service action under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make a test
/// pass.
const EVENT_SOURCE: &str = "aws.ecs";
const SERVICE_ACTION: &str = "ECS Service Action";

/// The `eventName` of each action under test, and the `eventType` that goes with it. The
/// names and their severities are the Amazon ECS Developer Guide's service action events:
/// a steady state the scheduler confirms is progress, so it is reported `INFO`.
const SERVICE_STEADY_STATE: &str = "SERVICE_STEADY_STATE";
const TASKSET_STEADY_STATE: &str = "TASKSET_STEADY_STATE";
const INFO: &str = "INFO";

/// The rule that matches a service action of this suite's cluster, and the rule that cannot.
const MATCHING_RULE: &str = "ecs-service-action";
const OTHER_RULE: &str = "unrelated-events";

/// The cluster every service below runs in.
const CLUSTER: &str = "scheduling";

/// The image the containers start from; irrelevant to this rule, so one value.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// One span of the emulated clock, the unit a scheduler confirmation is owed after.
const SPAN: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::TRANSITION_SPAN;

/// How often a poll re-reads the target queue.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll keeps trying before it gives up: the whole start of an `awsvpc` task
/// ([`AWSVPC_START`]) — the scheduler reaches no steady state before the tasks it placed are
/// running — plus the confirmation's own span, with generous margin for a loaded test
/// machine.
const POLL_BUDGET: Duration = AWSVPC_START
    .saturating_add(SPAN)
    .saturating_add(Duration::from_secs(10));

/// Which door of the scheduler a row drives, and with what.
#[derive(Debug, Clone, Copy)]
enum Scheduler {
    /// A service whose deployments the ECS scheduler itself rolls out: it is the SERVICE
    /// that reaches a steady state.
    Rollout {
        launch_type: &'static str,
        network_mode: &'static str,
        desired: i64,
    },
    /// A task set of a service whose deployments the `EXTERNAL` controller drives: it is the
    /// TASK SET that reaches a steady state.
    TaskSet {
        launch_type: &'static str,
        desired: i64,
    },
}

/// One row of the table.
struct Row {
    case: &'static str,
    scheduler: Scheduler,
    /// The `detail.eventName` the action must be published under.
    event_name: &'static str,
}

/// The table spans the rule: which action the scheduler takes, the launch type and network
/// mode its tasks are placed with, and whether it places any task at all.
const ROWS: &[Row] = &[
    Row {
        case: "a service whose task shares its host's networking",
        scheduler: Scheduler::Rollout {
            launch_type: "EC2",
            network_mode: "bridge",
            desired: 1,
        },
        event_name: SERVICE_STEADY_STATE,
    },
    Row {
        case: "a FARGATE service whose tasks are allocated their own interfaces",
        scheduler: Scheduler::Rollout {
            launch_type: "FARGATE",
            network_mode: "awsvpc",
            desired: 2,
        },
        event_name: SERVICE_STEADY_STATE,
    },
    Row {
        case: "a service that wants no tasks at all",
        scheduler: Scheduler::Rollout {
            launch_type: "EC2",
            network_mode: "bridge",
            desired: 0,
        },
        event_name: SERVICE_STEADY_STATE,
    },
    Row {
        case: "a task set of a service another controller deploys",
        scheduler: Scheduler::TaskSet {
            launch_type: "EC2",
            desired: 1,
        },
        event_name: TASKSET_STEADY_STATE,
    },
];

/// The ARN of the cluster this suite's services run in.
fn cluster_arn() -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{CLUSTER}")
}

/// The pattern the matching rule carries: the source and detail type Amazon ECS publishes a
/// service action under, narrowed to ONE cluster through the event's own `detail`.
fn service_action_pattern(cluster_arn: &str) -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [SERVICE_ACTION],
        "detail": { "clusterArn": [cluster_arn] },
    })
}

/// Register one revision of `family` in `network_mode` with a single container, plus the
/// Fargate task size such a revision needs when it is `FARGATE`-compatible.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    network_mode: &str,
    fargate: bool,
    case: &str,
) {
    let mut body = json!({
        "family": family,
        "networkMode": network_mode,
        "containerDefinitions": [container("web", IMAGE)],
    });
    if fargate {
        body["requiresCompatibilities"] = json!(["FARGATE"]);
        // A documented Fargate task size: 256 CPU units, 512 MiB.
        body["cpu"] = json!("256");
        body["memory"] = json!("512");
    }
    ok(router, "RegisterTaskDefinition", body, case).await;
}

/// Drive the scheduler the way `row` asks and answer the ARN of the resource whose action is
/// expected, beside the name of the service it belongs to.
async fn drive(router: &MultiServiceRouter, row: &Row, family: &str, case: &str) -> (String, String) {
    let service_name = format!("svc-{family}");
    match row.scheduler {
        Scheduler::Rollout {
            launch_type,
            network_mode,
            desired,
        } => {
            let mut request = json!({
                "cluster": CLUSTER,
                "serviceName": service_name,
                "taskDefinition": family,
                "launchType": launch_type,
                "desiredCount": desired,
            });
            if network_mode == "awsvpc" {
                request["networkConfiguration"] = awsvpc_network_configuration();
            }
            let created = ok(router, "CreateService", request, case).await;
            (
                string(&created["service"], "serviceArn", case),
                service_name,
            )
        }
        Scheduler::TaskSet {
            launch_type,
            desired,
        } => {
            ok(
                router,
                "CreateService",
                json!({
                    "cluster": CLUSTER,
                    "serviceName": service_name,
                    "desiredCount": desired,
                    "deploymentController": { "type": "EXTERNAL" },
                }),
                case,
            )
            .await;
            let created = ok(
                router,
                "CreateTaskSet",
                json!({
                    "cluster": CLUSTER,
                    "service": service_name,
                    "taskDefinition": family,
                    "launchType": launch_type,
                    "scale": { "value": 100.0, "unit": "PERCENT" },
                }),
                case,
            )
            .await;
            (
                string(&created["taskSet"], "taskSetArn", case),
                service_name,
            )
        }
    }
}

/// Poll the queue at `url` — and ONLY the queue — until what it has delivered satisfies
/// `arrived`, or the generous budget runs out.
///
/// Nothing in this loop calls Amazon ECS, which is the point: a client that watches a service
/// through EventBridge makes no further call to the service that publishes. The loop waits
/// for the CONDITION rather than for a span, so a slow host only polls more often.
async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    arrived: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, case, url).await);
        if arrived(&collected) || Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether `events` holds the action `event_name` about `resource_arn`.
fn holds_action(events: &[Value], resource_arn: &str, event_name: &str) -> bool {
    events.iter().any(|event| {
        event["resources"] == json!([resource_arn])
            && event["detail"]["eventName"] == json!(event_name)
    })
}

/// The delivered events that are about `resource_arn`.
fn about(delivered: &[Value], resource_arn: &str) -> Vec<Value> {
    delivered
        .iter()
        .filter(|event| event["resources"] == json!([resource_arn]))
        .cloned()
        .collect()
}

/// Poll the queue at `url` until the action `event_name` about `resource_arn` has arrived,
/// and answer the events delivered about that resource.
async fn collect_actions(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    resource_arn: &str,
    event_name: &str,
) -> Vec<Value> {
    let delivered = collect_until(router, case, url, |events| {
        holds_action(events, resource_arn, event_name)
    })
    .await;
    about(&delivered, resource_arn)
}

/// Describe one service and return it.
async fn describe_service(router: &MultiServiceRouter, service_name: &str, case: &str) -> Value {
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
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {body}"))
}

/// Describe the one task set of `service_name` and return it.
async fn describe_task_set(router: &MultiServiceRouter, service_name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTaskSets",
        json!({ "cluster": CLUSTER, "service": service_name }),
        case,
    )
    .await;
    list(&body, "taskSets", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task set back, got {body}"))
}

#[tokio::test]
async fn a_scheduler_action_publishes_an_ecs_service_action_event() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's services cannot decide another's.
        let router = emulator();

        let (matching_url, matching_arn) = given_queue(&router, case, "ecs-service-actions").await;
        let (other_url, other_arn) = given_queue(&router, case, "unrelated-events").await;
        given_rule(
            &router,
            case,
            MATCHING_RULE,
            service_action_pattern(&cluster_arn()),
            &matching_arn,
        )
        .await;
        given_rule(
            &router,
            case,
            OTHER_RULE,
            // Another cluster's service actions: nothing this row publishes can match it.
            service_action_pattern(&format!(
                "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/elsewhere"
            )),
            &other_arn,
        )
        .await;
        grant_delivery(&router, case, &matching_url, &matching_arn).await;
        grant_delivery(&router, case, &other_url, &other_arn).await;

        ok(
            &router,
            "CreateCluster",
            json!({ "clusterName": CLUSTER }),
            case,
        )
        .await;
        let launch_type = match row.scheduler {
            Scheduler::Rollout { launch_type, .. } | Scheduler::TaskSet { launch_type, .. } => {
                launch_type
            }
        };
        // The EC2 launch type "runs your tasks on Amazon EC2 instances registered to your
        // cluster", so such a placement needs capacity; a Fargate one needs none.
        if launch_type == "EC2" {
            register_capacity(&router, CLUSTER, case).await;
        }
        let family = format!("actions{index}");
        let network_mode = match row.scheduler {
            Scheduler::Rollout { network_mode, .. } => network_mode,
            Scheduler::TaskSet { .. } => "bridge",
        };
        register(
            &router,
            &family,
            network_mode,
            launch_type == "FARGATE",
            case,
        )
        .await;

        let (resource_arn, service_name) = drive(&router, row, &family, case).await;

        let delivered = collect_actions(&router, case, &matching_url, &resource_arn, row.event_name)
            .await;
        let names: Vec<String> = delivered
            .iter()
            .map(|event| {
                event["detail"]["eventName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert!(
            names.iter().any(|name| name == row.event_name),
            "case {case:?}: expected the action {} for {resource_arn}, got {names:?}",
            row.event_name
        );

        // Every delivered envelope is an ECS service action of THIS cluster, about the
        // resource the action was taken on, reported with the severity that action carries
        // and stamped with the instant it happened at.
        for event in &delivered {
            assert_eq!(
                event["source"],
                json!(EVENT_SOURCE),
                "case {case:?}: expected source {EVENT_SOURCE}, got {event}"
            );
            assert_eq!(
                event["detail-type"],
                json!(SERVICE_ACTION),
                "case {case:?}: expected detail-type {SERVICE_ACTION}, got {event}"
            );
            assert_eq!(
                event["detail"]["eventType"],
                json!(INFO),
                "case {case:?}: expected eventType {INFO}, got {event}"
            );
            assert_eq!(
                event["detail"]["clusterArn"],
                json!(cluster_arn()),
                "case {case:?}: expected detail.clusterArn {}, got {event}",
                cluster_arn()
            );
            assert_eq!(
                event["resources"],
                json!([resource_arn.clone()]),
                "case {case:?}: expected {resource_arn} in resources, got {event}"
            );
            let created_at = event["detail"]["createdAt"].as_str().unwrap_or_default();
            assert!(
                created_at.contains('T') && created_at.ends_with('Z'),
                "case {case:?}: expected a UTC date-time in detail.createdAt, got {event}"
            );
        }

        // ROUND TRIP: the action the event reports is the one the control plane reports —
        // the scheduler's own account of the steady state it reached.
        match row.scheduler {
            Scheduler::Rollout { .. } => {
                let service = describe_service(&router, &service_name, case).await;
                let messages: Vec<String> = list(&service, "events", case)
                    .iter()
                    .map(|event| string(event, "message", case))
                    .collect();
                assert!(
                    messages
                        .iter()
                        .any(|message| message.ends_with("has reached a steady state.")),
                    "case {case:?}: expected the service's own event stream to record the \
                     steady state, got {messages:?}"
                );
            }
            Scheduler::TaskSet { .. } => {
                let task_set = describe_task_set(&router, &service_name, case).await;
                assert_eq!(
                    string(&task_set, "stabilityStatus", case),
                    "STEADY_STATE",
                    "case {case:?}: expected the task set to report its steady state, got \
                     {task_set}"
                );
            }
        }

        // The rule that cannot match this cluster's actions never receives one.
        let unrelated = drain(&router, case, &other_url).await;
        assert!(
            unrelated.is_empty(),
            "case {case:?}: the non-matching rule's target must receive nothing, got \
             {unrelated:?}"
        );
    }
}

/// A service that is READ repeatedly has its steady state published ONCE: the event stream is
/// the scheduler's ACTIONS, not the reads of the service, so polling a settled service does
/// not keep announcing it.
#[tokio::test]
async fn a_scheduler_action_is_published_once_however_often_the_service_is_read() {
    let case = "repeated reads of one settled service";
    let router = emulator();

    let (url, queue_arn) = given_queue(&router, case, "ecs-service-actions").await;
    given_rule(
        &router,
        case,
        MATCHING_RULE,
        service_action_pattern(&cluster_arn()),
        &queue_arn,
    )
    .await;
    grant_delivery(&router, case, &url, &queue_arn).await;

    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    register_capacity(&router, CLUSTER, case).await;
    register(&router, "settled", "bridge", false, case).await;

    let created = ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "svc-settled",
            "taskDefinition": "settled",
            "launchType": "EC2",
            "desiredCount": 1,
        }),
        case,
    )
    .await;
    let service_arn = string(&created["service"], "serviceArn", case);

    // ONLY the queue is polled until the steady state arrives: the action the clock owes has
    // to reach the target without anybody calling Amazon ECS again.
    let delivered = collect_actions(&router, case, &url, &service_arn, SERVICE_STEADY_STATE).await;
    let names: Vec<String> = delivered
        .iter()
        .map(|event| {
            event["detail"]["eventName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        names,
        vec![SERVICE_STEADY_STATE.to_string()],
        "case {case:?}: expected the steady state published exactly once, got {names:?}"
    );

    // Now read the settled service through three doors: a read materialises nothing a write
    // did not already owe, so it announces nothing. What makes that assertable without
    // waiting on the clock is a BARRIER — a second service, created after those reads, whose
    // own steady state travels the same queue behind anything the reads could have queued.
    // Once the barrier has arrived, an announcement the reads made would have arrived too.
    for operation in ["DescribeServices", "DescribeServices", "ListServices"] {
        let body = if operation == "ListServices" {
            json!({ "cluster": CLUSTER })
        } else {
            json!({ "cluster": CLUSTER, "services": ["svc-settled"] })
        };
        ok(&router, operation, body, case).await;
    }
    let barrier = ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "svc-barrier",
            "taskDefinition": "settled",
            "launchType": "EC2",
            "desiredCount": 0,
        }),
        case,
    )
    .await;
    let barrier_arn = string(&barrier["service"], "serviceArn", case);
    let delivered = collect_until(&router, case, &url, |events| {
        holds_action(events, &barrier_arn, SERVICE_STEADY_STATE)
    })
    .await;
    let late = about(&delivered, &service_arn);
    assert!(
        late.is_empty(),
        "case {case:?}: reading a settled service must publish nothing more, got {late:?}"
    );
    assert_eq!(
        about(&delivered, &barrier_arn).len(),
        1,
        "case {case:?}: expected the barrier service's one steady state, got {delivered:?}"
    );
}
