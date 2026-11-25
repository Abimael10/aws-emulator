// SPDX-License-Identifier: CC0-1.0

//! A task the caller STOPS keeps publishing `ECS Task State Change` events until it is
//! terminal — including a task whose image fetch is still being waited out — and a
//! `RunTask` that placed nothing publishes none at all.
//!
//! The RULES under test:
//!
//! * Amazon ECS publishes one `ECS Task State Change` envelope onto the account's default
//!   event bus for EVERY move of a task's reported status pair
//!   (`lastStatus`/`desiredStatus`), for as long as the task keeps moving — so a `StopTask`
//!   on a task that is still coming up publishes the statuses that stop takes it through,
//!   down to the terminal `STOPPED` one, with no further Amazon ECS call.
//! * A task Amazon ECS cannot FETCH an image for is one it is still launching while the
//!   agent waits that fetch out: such a task answers the reads a client makes of it as a
//!   task that is coming up, and a stop asked for during the wait reaches it — the stop is
//!   the caller's (`Task$stopCode` `UserInitiated`, with the caller's own `stoppedReason`),
//!   not the launch failure's, and the transitions it earns are published like any other's.
//! * A `RunTask` whose placement FAILED created no task at all, so it has nothing to
//!   publish: the silence of a rule that is supposed to receive nothing is readable only
//!   because the same rule keeps receiving the placed task's own moves.
//!
//! The rows of the first suite span HOW FAR the task had got when the stop arrived — the
//! whole point of the first rule, since a stop that interrupts a task Amazon ECS is still
//! provisioning has more of the lifecycle left to publish than one that stops a task that
//! already runs — crossed with the network mode and launch type it was placed with, because
//! an `awsvpc` task's interface is torn down as part of its stop while a task sharing its
//! host's networking has none, and with a placement that cannot reach the registry the image
//! it runs is held in. The rows of the second suite span the WAYS a placement places
//! nothing: one Amazon ECS refuses outright, and one it answers with a populated `failures`
//! list.
//!
//! EventBridge, Amazon SQS and Amazon EC2 are mounted BESIDE Amazon ECS on the SAME engine,
//! so a publication travels the real in-process callback, through the real bus, to a real
//! queue, and every network resource a placement is resolved against is created through
//! Amazon EC2's own API.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, ec2, ec2_element, list, ok, refused, register_capacity, string,
    ACCOUNT, SIGNED_REGION,
};
use crate::task_state_change_events::{drain, given_queue, given_rule, grant_delivery};

/// The `source` and `detail-type` Amazon ECS publishes a task's state changes under, spelled
/// out here rather than imported, so a wrong constant in the implementation cannot make a
/// test pass.
const EVENT_SOURCE: &str = "aws.ecs";
const TASK_STATE_CHANGE: &str = "ECS Task State Change";

/// The cluster every task below runs in.
const CLUSTER: &str = "stopevents";

/// The rule under test, scoped to that cluster.
const RULE: &str = "ecs-task-state-change";

/// The image the containers start from. No command is given, so nothing of the container's
/// own ends the task: it runs until something stops it.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The statuses read off the delivered envelopes.
const PENDING: &str = "PENDING";
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a stop the CALLER asked for, and of the stop a failed launch earns
/// (`smithy_query(query="shape",shape="TaskStopCode")`).
const USER_INITIATED: &str = "UserInitiated";
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// The attachment status an `awsvpc` task's interface reports once it is in place.
const ATTACHED: &str = "ATTACHED";

/// How often a poll re-reads the target queue.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll keeps trying before it gives up. Generous: every status this suite waits
/// for is one the CLOCK materialises, and a poll stops the instant its condition holds, so
/// the deadline only turns a failure into a failure rather than a hang.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// A router carrying Amazon ECS, EventBridge, Amazon SQS and Amazon EC2 over ONE shared
/// engine — the services a published task event travels through, plus the one that owns the
/// network a placement is resolved against, mounted the way the production router mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The fixed `awsvpc` subnets the rows that place into one name
    // ([`crate::ecs_wire::seed_awsvpc_subnets`]); the row whose placement has no path off its
    // VPC builds that subnet through the Amazon EC2 API instead.
    crate::ecs_wire::seed_awsvpc_subnets(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// One row of the stop table: how far the task is let get before the caller stops it.
struct StopRow {
    case: &'static str,
    /// `RegisterTaskDefinition$networkMode` the revision is registered in.
    network_mode: &'static str,
    /// The launch type the placement names.
    launch_type: &'static str,
    /// Whether the placement goes into a subnet with no path off its VPC, so the image its
    /// container runs cannot be fetched from there at all.
    isolated: bool,
    /// The status the task must have reported through an event before the stop is asked for,
    /// or `None` for a stop asked for as soon as the placement itself is published.
    stop_after: Option<&'static str>,
}

/// The table spans how much of the lifecycle the stop interrupts, the networking the task
/// holds while it is interrupted, and whether the image it runs can be fetched at all.
const STOP_ROWS: &[StopRow] = &[
    StopRow {
        case: "a FARGATE task stopped while Amazon ECS is still provisioning it",
        network_mode: "awsvpc",
        launch_type: "FARGATE",
        isolated: false,
        stop_after: None,
    },
    StopRow {
        case: "a FARGATE task stopped once it is pending",
        network_mode: "awsvpc",
        launch_type: "FARGATE",
        isolated: false,
        stop_after: Some(PENDING),
    },
    StopRow {
        case: "a FARGATE task stopped once it runs",
        network_mode: "awsvpc",
        launch_type: "FARGATE",
        isolated: false,
        stop_after: Some(RUNNING),
    },
    StopRow {
        case: "an EC2 task sharing its host's networking stopped once it runs",
        network_mode: "bridge",
        launch_type: "EC2",
        isolated: false,
        stop_after: Some(RUNNING),
    },
    StopRow {
        case: "an EC2 task sharing its host's networking stopped as soon as it is placed",
        network_mode: "bridge",
        launch_type: "EC2",
        isolated: false,
        stop_after: None,
    },
    StopRow {
        case: "a FARGATE task stopped while the fetch of an unreachable image is waited out",
        network_mode: "awsvpc",
        launch_type: "FARGATE",
        isolated: true,
        stop_after: Some(PENDING),
    },
];

/// The ARN of the cluster these suites run their tasks in.
fn cluster_arn() -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{CLUSTER}")
}

/// The pattern the rule under test carries: this service's own source and detail-type,
/// narrowed to the one cluster through the event's `detail` — the scoping a client uses.
fn task_event_pattern() -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [TASK_STATE_CHANGE],
        "detail": { "clusterArn": [cluster_arn()] },
    })
}

/// A queue, the grant EventBridge needs on it, and the rule under test pointing at it;
/// answers the queue's url.
async fn given_wiring(router: &MultiServiceRouter, case: &str) -> String {
    let (url, queue_arn) = given_queue(router, case, "ecs-stop-events").await;
    grant_delivery(router, case, &url, &queue_arn).await;
    given_rule(router, case, RULE, task_event_pattern(), &queue_arn).await;
    url
}

/// A subnet with NO route for traffic leaving its VPC, created through Amazon EC2's own API:
/// a public registry is no AWS service and publishes no endpoint, so an image held in one
/// cannot be fetched from such a placement at all.
async fn isolated_subnet(router: &MultiServiceRouter, case: &str) -> String {
    let vpc = ec2_element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.91.0.0/16")], case).await,
        "vpcId",
        case,
    );
    let subnet = ec2_element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.91.0.0/20"),
                ("AvailabilityZone", &format!("{SIGNED_REGION}a")),
            ],
            case,
        )
        .await,
        "subnetId",
        case,
    );
    // A route table of the subnet's own, carrying only the local route of its VPC: having no
    // route for traffic leaving the VPC is what makes the subnet isolated.
    let table = ec2_element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())], case).await,
        "routeTableId",
        case,
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
        case,
    )
    .await;
    subnet
}

/// Register one revision of `family`, Fargate-sized when the row is a Fargate one.
async fn given_definition(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    network_mode: &str,
    fargate: bool,
) {
    let mut definition = json!({
        "family": family,
        "networkMode": network_mode,
        "containerDefinitions": [{
            "name": "web",
            "image": IMAGE,
            "memory": 128,
            "essential": true,
        }],
    });
    if fargate {
        definition["requiresCompatibilities"] = json!(["FARGATE"]);
        // The documented Fargate task size the recorded check uses: 256 CPU units, 512 MiB.
        definition["cpu"] = json!("256");
        definition["memory"] = json!("512");
        definition["executionRoleArn"] = json!(format!("arn:aws:iam::{ACCOUNT}:role/ecs-exec"));
    }
    ok(router, "RegisterTaskDefinition", definition, case).await;
}

/// Place ONE task of `family` and answer its ARN.
async fn place(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    network_mode: &str,
    launch_type: &str,
    subnet: Option<&str>,
) -> String {
    let mut request = json!({
        "cluster": CLUSTER,
        "taskDefinition": family,
        "launchType": launch_type,
        "count": 1,
    });
    if network_mode == "awsvpc" {
        request["networkConfiguration"] = match subnet {
            Some(subnet) => json!({
                "awsvpcConfiguration": {
                    "subnets": [subnet],
                    "securityGroups": ["sg-0123456789abcdef0"],
                    "assignPublicIp": "DISABLED",
                }
            }),
            None => awsvpc_network_configuration(),
        };
    }
    let placed = ok(router, "RunTask", request, case).await;
    let tasks = list(&placed, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected RunTask to place one task, got {placed}"
    );
    string(&tasks[0], "taskArn", case)
}

/// Poll the queue — and ONLY the queue — until `enough` holds of the envelopes collected so
/// far, and answer them all.
///
/// Nothing in this loop calls Amazon ECS, which is the point: a client that watches a task
/// through EventBridge makes no further call to the service that publishes.
async fn collect(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    what: &str,
    enough: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, case, url).await);
        if enough(&collected) {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what}, got {collected:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether `events` holds an envelope about `task_arn` reporting `status`.
fn reported(events: &[Value], task_arn: &str, status: &str) -> bool {
    events.iter().any(|event| {
        event["detail"]["taskArn"] == json!(task_arn)
            && event["detail"]["lastStatus"] == json!(status)
    })
}

/// The envelope about `task_arn` reporting `status`, for the assertions on its members.
fn envelope<'a>(events: &'a [Value], task_arn: &str, status: &str, case: &str) -> &'a Value {
    events
        .iter()
        .find(|event| {
            event["detail"]["taskArn"] == json!(task_arn)
                && event["detail"]["lastStatus"] == json!(status)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: no {status} envelope for {task_arn} in {events:?}")
        })
}

/// One task, as `DescribeTasks` reports it.
async fn describe(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
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
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back for {task_arn}"))
}

#[tokio::test]
async fn a_stopped_task_publishes_every_status_the_stop_takes_it_through() {
    for (index, row) in STOP_ROWS.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's tasks cannot decide another's.
        let router = emulator();
        let url = given_wiring(&router, case).await;

        ok(
            &router,
            "CreateCluster",
            json!({ "clusterName": CLUSTER }),
            case,
        )
        .await;
        // The EC2 launch type "runs your tasks on Amazon EC2 instances registered to your
        // cluster", so such a placement needs capacity; a Fargate one needs none.
        if row.launch_type == "EC2" {
            register_capacity(&router, CLUSTER, case).await;
        }
        let subnet = if row.isolated {
            Some(isolated_subnet(&router, case).await)
        } else {
            None
        };
        let family = format!("stopevents{index}");
        given_definition(
            &router,
            case,
            &family,
            row.network_mode,
            row.launch_type == "FARGATE",
        )
        .await;

        let task_arn = place(
            &router,
            case,
            &family,
            row.network_mode,
            row.launch_type,
            subnet.as_deref(),
        )
        .await;

        // The POSITIVE CONTROL: the placement itself is published, so the delivery path is
        // known live before the stop is asked for.
        collect(&router, case, &url, "the placed task's first event", |seen| {
            seen.iter()
                .any(|event| event["detail"]["taskArn"] == json!(task_arn))
        })
        .await;

        // A row that stops the task LATER waits for the status it names to be published
        // first, so the stop really interrupts the task at that point of its lifecycle.
        if let Some(status) = row.stop_after {
            collect(
                &router,
                case,
                &url,
                &format!("an event reporting {status}"),
                |seen| reported(seen, &task_arn, status),
            )
            .await;
        }
        // The row whose image cannot be fetched is read back as a task that is COMING UP
        // while the agent waits that fetch out: its interface is in place and it has not
        // stopped, so the stop below interrupts a task that is still being launched.
        if row.isolated {
            let task = describe(&router, case, &task_arn).await;
            let attachments = list(&task, "attachments", case);
            assert_eq!(
                attachments
                    .first()
                    .map(|attachment| string(attachment, "status", case)),
                Some(ATTACHED.to_string()),
                "case {case:?}: expected the interface of a task that is coming up to be \
                 {ATTACHED}, got {task}"
            );
            assert_ne!(
                string(&task, "lastStatus", case),
                STOPPED,
                "case {case:?}: expected a task whose image fetch is still being waited out \
                 to be coming up, got {task}"
            );
        }

        let reason = "the suite asked for the stop";
        let stopped = ok(
            &router,
            "StopTask",
            json!({ "cluster": CLUSTER, "task": task_arn.clone(), "reason": reason }),
            case,
        )
        .await;
        assert_eq!(
            string(&stopped["task"], "desiredStatus", case),
            STOPPED,
            "case {case:?}: expected StopTask to ask for STOPPED, got {stopped}"
        );

        // THE RULE: the stop's own transitions keep arriving after it, down to the terminal
        // one — with no further Amazon ECS call of any kind.
        let after = collect(
            &router,
            case,
            &url,
            "the stopped task's terminal event",
            |seen| reported(seen, &task_arn, STOPPED),
        )
        .await;
        let terminal = envelope(&after, &task_arn, STOPPED, case);
        assert_eq!(
            terminal["source"],
            json!(EVENT_SOURCE),
            "case {case:?}: expected source {EVENT_SOURCE}, got {terminal}"
        );
        assert_eq!(
            terminal["detail-type"],
            json!(TASK_STATE_CHANGE),
            "case {case:?}: expected detail-type {TASK_STATE_CHANGE}, got {terminal}"
        );
        assert_eq!(
            terminal["resources"],
            json!([task_arn.clone()]),
            "case {case:?}: expected the task's ARN in resources, got {terminal}"
        );
        assert_eq!(
            terminal["detail"]["clusterArn"],
            json!(cluster_arn()),
            "case {case:?}: expected detail.clusterArn {}, got {terminal}",
            cluster_arn()
        );
        // The stop is the CALLER's, whatever the task was doing when it arrived: the reason
        // the caller supplied, under the stop code of a stop somebody asked for.
        assert_eq!(
            terminal["detail"]["stoppedReason"],
            json!(reason),
            "case {case:?}: expected the reason the stop supplied, got {terminal}"
        );
        assert_eq!(
            terminal["detail"]["stopCode"],
            json!(USER_INITIATED),
            "case {case:?}: expected stopCode {USER_INITIATED}, got {terminal}"
        );

        // The event is the task's own state: `DescribeTasks` reports the same terminal
        // status and the same stop for it.
        let task = describe(&router, case, &task_arn).await;
        assert_eq!(
            string(&task, "lastStatus", case),
            STOPPED,
            "case {case:?}: expected DescribeTasks to report the task STOPPED, got {task}"
        );
        assert_eq!(
            string(&task, "stopCode", case),
            USER_INITIATED,
            "case {case:?}: expected the task to carry stopCode {USER_INITIATED} rather than \
             {TASK_FAILED_TO_START}, got {task}"
        );
    }
}

/// One row of the negative table: a `RunTask` that places NOTHING.
struct UnplaceableRow {
    case: &'static str,
    /// Whether the cluster holds a container instance at all.
    capacity: bool,
    /// Whether the unplaceable placement is constrained to a host no candidate satisfies.
    constrained: bool,
    /// The refusal this row's placement earns, and `None` for the row Amazon ECS ANSWERS
    /// with a populated `failures` list instead: "a placement asking for more tasks than
    /// there are free candidates is not refused" — so which of the two shapes a row takes is
    /// stated here rather than read off whatever the emulator answered.
    refusal: Option<(&'static str, &'static str)>,
}

/// The table spans the two ways a placement places nothing: the launch type whose capacity
/// the cluster holds none of, which Amazon ECS refuses outright, and a constraint no
/// candidate host satisfies, which it answers with a populated `failures` list.
const UNPLACEABLE_ROWS: &[UnplaceableRow] = &[
    UnplaceableRow {
        case: "an EC2 placement into a cluster holding no container instance",
        capacity: false,
        constrained: false,
        // The EC2 launch type "runs your tasks on Amazon EC2 instances registered to your
        // cluster", so a cluster holding none has nowhere to put the task at all.
        refusal: Some((
            "InvalidParameterException",
            "No Container Instances were found in your cluster.",
        )),
    },
    UnplaceableRow {
        case: "a placement constrained to a host no candidate satisfies",
        capacity: true,
        constrained: true,
        refusal: None,
    },
];

#[tokio::test]
async fn a_run_task_that_placed_nothing_publishes_no_event() {
    for (index, row) in UNPLACEABLE_ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let url = given_wiring(&router, case).await;

        ok(
            &router,
            "CreateCluster",
            json!({ "clusterName": CLUSTER }),
            case,
        )
        .await;
        if row.capacity {
            register_capacity(&router, CLUSTER, case).await;
        }
        // The CONTROL task, placed on capacity the cluster does hold: Fargate needs none, so
        // every row can place one.
        let control_family = format!("controlevents{index}");
        given_definition(&router, case, &control_family, "awsvpc", true).await;
        let control = place(&router, case, &control_family, "awsvpc", "FARGATE", None).await;
        collect(&router, case, &url, "the control task's first event", |seen| {
            seen.iter()
                .any(|event| event["detail"]["taskArn"] == json!(control))
        })
        .await;

        // The placement that places nothing. Either Amazon ECS refuses it outright or it
        // answers a populated `failures` list — what the row is about is that NO task is
        // created by it.
        let family = format!("unplaceable{index}");
        given_definition(&router, case, &family, "bridge", false).await;
        let mut request = json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "EC2",
            "count": 1,
            "group": format!("family:{family}"),
        });
        if row.constrained {
            // One task of the group already holds the cluster's only host, so a further one
            // of the same group has no candidate left ("each task in a particular group is
            // running on a different container instance").
            ok(&router, "RunTask", request.clone(), case).await;
            request["placementConstraints"] = json!([{ "type": "distinctInstance" }]);
        }
        match row.refusal {
            Some((code, message)) => {
                let refused = refused(&router, "RunTask", request, code, case).await;
                assert_eq!(
                    refused["message"],
                    json!(message),
                    "case {case:?}: expected the refusal to report {message:?}, got {refused}"
                );
            }
            None => {
                let answer = ok(&router, "RunTask", request, case).await;
                assert!(
                    list(&answer, "tasks", case).is_empty()
                        && !list(&answer, "failures", case).is_empty(),
                    "case {case:?}: expected a placement that placed nothing to report failures \
                     and no task, got {answer}"
                );
            }
        }

        // Every task the cluster holds, so the assertion below knows which ARNs a published
        // envelope MAY name.
        let listed = ok(
            &router,
            "ListTasks",
            json!({ "cluster": CLUSTER, "desiredStatus": "RUNNING" }),
            case,
        )
        .await;
        let known: Vec<String> = list(&listed, "taskArns", case)
            .iter()
            .map(|arn| arn.as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            known.contains(&control),
            "case {case:?}: expected the control task to be one of the cluster's own, got \
             {known:?}"
        );

        // THE RULE: the queue keeps receiving the tasks Amazon ECS really placed — the
        // control task goes on moving, which is what makes the silence about the failed
        // placement readable — and never an envelope about a task that was never created.
        let delivered = collect(
            &router,
            case,
            &url,
            "a further event for the control task",
            |seen| reported(seen, &control, RUNNING),
        )
        .await;
        for event in &delivered {
            let named = event["detail"]["taskArn"].as_str().unwrap_or_default();
            assert!(
                known.iter().any(|arn| arn == named),
                "case {case:?}: only the tasks the cluster holds ({known:?}) may be published, \
                 got {event}"
            );
        }
    }
}
