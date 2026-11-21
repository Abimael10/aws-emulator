// SPDX-License-Identifier: CC0-1.0

//! WHEN each step of a task's launch and of its stop happens: the schedule the clock owes a
//! task, read off the instants the task itself reports.
//!
//! RULE under test: every step the CLOCK owes on a task's own lifecycle takes ONE of this
//! emulator's task-transition spans (`state::TRANSITION_SPAN`) — the elastic network interface
//! Amazon ECS creates and attaches for a task in the `awsvpc` network mode included. That
//! interface is a step of the task's own LAUNCH ("If the network mode is `awsvpc`, the task is
//! allocated an elastic network interface", and the task is `PROVISIONING` until it is in
//! place), and its teardown is a step of the task's own STOP, so each documented status of
//! `Attachment$status` ("Valid values are `PRECREATED`, `CREATED`, `ATTACHING`, `ATTACHED`,
//! `DETACHING`, `DETACHED`, `DELETED`, and `FAILED`") falls one span after the one before it,
//! on the way up exactly as on the way down. The whole lifecycle of one task is therefore a
//! handful of spans — which is what lets a client run SEVERAL tasks one after another, each
//! waiting out the one before it (the shape of a scenario that hands data from one task to the
//! next over a shared volume), and get its answer inside an ordinary poll budget.
//!
//! The rows differ along what the rule generalises over: whether the task is allocated an
//! interface at all (`awsvpc` against `bridge`, so the launch has the extra steps or has none)
//! and which launch type places it (`FARGATE`, and the `EC2` capacity registered to the
//! cluster) — because the steps are the NETWORK MODE's, not the launch type's.
//!
//! Nothing here measures the host's clock. Every interval asserted is the difference between
//! two instants the emulator REPORTS for the task — `Task$createdAt` (the placement),
//! `Task$connectivityAt` ("the time when the task last went into `CONNECTED` status", which
//! for a task of its own interface is the instant that interface is in place),
//! `Task$startedAt` ("when the task transitioned from the `PENDING` state to the `RUNNING`
//! state"), `Task$stoppingAt` and `Task$stoppedAt` — and those are the stored instants each
//! transition was owed at, so what the rows assert does not depend on how fast or how loaded
//! the host is. The only waiting is for the CONDITION (the task reporting `STOPPED`), with a
//! deadline so a move that never happens fails the test instead of hanging it.
//!
//! Why this suite lives beside the router: the interface those steps attach and release is
//! AMAZON VPC's resource — created in a subnet Amazon EC2 holds, and gone from that service
//! once the release has finished — so the rule spans two services. Every resource below is
//! created through the real operation of the service that owns it on ONE shared engine, and
//! every assertion is a wire answer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

use service_com_amazonaws_ecs_amazonec2containerservicev20141113::state as ecs_state;

/// The Region every request below is signed for, and so the Region the resources belong to.
const REGION: &str = "us-west-2";

/// The API version an ec2Query client sends beside `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The ECS cluster every task below runs in.
const CLUSTER: &str = "launchschedule";

/// The EC2 instance the cluster's own capacity is registered as, so the rows placed with the
/// `EC2` launch type have somewhere to run.
const INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c0";

/// The account id every emulator ARN carries, which a container-instance registration's
/// identity document has to name.
const ACCOUNT: &str = "000000000000";

/// The image every container starts from, held in a public registry reached over the internet
/// gateway the network below carries, so the image fetch succeeds and the task runs.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The destination of the route that carries the VPC's traffic to its internet gateway.
const INTERNET_DESTINATION: &str = "0.0.0.0/0";

/// `Attachment$type` of the interface an `awsvpc` task is allocated, from that member's own
/// doc ("such as `ElasticNetworkInterface`, `Service Connect`, and
/// `AmazonElasticBlockStorage`").
const ELASTIC_NETWORK_INTERFACE: &str = "ElasticNetworkInterface";

/// The terminal `Attachment$status` of an attachment whose resource is gone — one of the
/// values that member's own doc lists. Spelled out rather than imported from the service's
/// `state` (which exports it), so a wrong wording in the implementation cannot make this
/// suite pass; the SPANS below are imported, because what they assert is that the schedule
/// is the one the crate publishes.
const DELETED: &str = "DELETED";

/// How long this suite waits for a transition the CLOCK owes, and how often it looks again
/// while it waits. Generous on purpose: it waits for the CONDITION rather than for a fixed
/// time, so a loaded host cannot fail a row the emulator would satisfy.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// How far apart two instants the emulator reports may be from the span between them and
/// still count as that span: HALF a transition span.
///
/// Not a measurement of the host — the instants are the stored ones each transition was owed
/// at — but the placement reads the clock more than once while it records the task, so two
/// instants owed "from the placement" can be a tick either side of a millisecond.
const TOLERANCE: Duration = Duration::from_millis(ecs_state::TRANSITION_SPAN.as_millis() as u64 / 2);

/// One emulator carrying Amazon EC2 and Amazon ECS over ONE shared engine, exactly as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "EC2 {action} must succeed, got {status} with {payload}"
    );
    payload
}

/// One awsJson1_1 call to Amazon ECS that must succeed, answered with the parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "ECS {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// The network every task below is placed into: a VPC with one subnet, an internet gateway
/// attached to it with a default route, and a security group of its own — each created
/// through the operation a caller calls.
struct Network {
    subnet: String,
    security_group: String,
}

async fn network_of(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.91.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.91.4.0/24"),
                ("AvailabilityZone", &format!("{REGION}a")),
            ],
        )
        .await,
        "subnetId",
    );
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let table = element(
        &ec2(
            router,
            "DescribeRouteTables",
            &[
                ("Filter.1.Name", "vpc-id"),
                ("Filter.1.Value.1", vpc.as_str()),
            ],
        )
        .await,
        "routeTableId",
    );
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", table.as_str()),
            ("DestinationCidrBlock", INTERNET_DESTINATION),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "schedule-tasks"),
                ("GroupDescription", "the tasks own group"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        subnet,
        security_group,
    }
}

/// Create the cluster and register ONE container instance into it through the agent's own
/// operation, so a row placed with the `EC2` launch type has capacity to run on.
async fn cluster_with_capacity(router: &MultiServiceRouter) {
    ecs(router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let identity = json!({
        "accountId": ACCOUNT,
        "instanceId": INSTANCE_ID,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string();
    ecs(
        router,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity,
            // A registration presents the document's signature beside it; nothing here signs
            // anything, so this is base64 text of the right shape.
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
    )
    .await;
}

/// One row: the shape of the launch, and so how many clock-owed steps its lifecycle has.
struct Row {
    case: &'static str,
    family: &'static str,
    /// `RegisterTaskDefinition$networkMode`: `awsvpc` allocates the task its own interface,
    /// every other mode shares the host's networking.
    network_mode: &'static str,
    /// Whether the launch names the `FARGATE` launch type; a row that does not is placed on
    /// the `EC2` capacity registered to the cluster.
    fargate: bool,
    /// Whether the CALLER stops the task with `StopTask`. A row that does not is left to
    /// finish the work it was placed to do, which the exit of its essential container ends;
    /// a row that does gives its container no program at all, so nothing but the call ends
    /// it.
    stopped_by_the_caller: bool,
}

const ROWS: [Row; 3] = [
    Row {
        case: "a Fargate task allocated its own interface, finishing its own work",
        family: "schedulefargate",
        network_mode: "awsvpc",
        fargate: true,
        stopped_by_the_caller: false,
    },
    Row {
        case: "a Fargate task allocated its own interface, stopped by the caller",
        family: "schedulestopped",
        network_mode: "awsvpc",
        fargate: true,
        stopped_by_the_caller: true,
    },
    Row {
        case: "a bridge task sharing its host's networking",
        family: "schedulebridge",
        network_mode: "bridge",
        fargate: false,
        stopped_by_the_caller: false,
    },
];

impl Row {
    /// Whether this row's task is allocated an elastic network interface of its own.
    fn holds_interface(&self) -> bool {
        self.network_mode == "awsvpc"
    }

    /// How many steps the clock owes before the interface is IN PLACE: one per status of
    /// `state::INTERFACE_ATTACH_LIFECYCLE` after the one the attachment is created at, and
    /// none at all for a task allocated no interface.
    fn attach_steps(&self) -> u32 {
        if self.holds_interface() {
            ecs_state::INTERFACE_ATTACH_LIFECYCLE.len() as u32 - 1
        } else {
            0
        }
    }

    /// How many steps the clock owes before the interface is GONE: one per status of
    /// `state::INTERFACE_DETACH_LIFECYCLE` after the one it is released from. A task that
    /// holds no interface waits on nothing but its own stop, which is one span.
    fn stop_steps(&self) -> u32 {
        if self.holds_interface() {
            ecs_state::INTERFACE_DETACH_LIFECYCLE.len() as u32 - 1
        } else {
            1
        }
    }
}

/// Read ONE task back through `DescribeTasks` — the stored record, not the answer of the call
/// that placed it.
async fn describe(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
    )
    .await;
    body["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

/// Every `ElasticNetworkInterface` attachment a task holds.
fn interface_attachments(task: &Value) -> Vec<Value> {
    task["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|attachment| attachment["type"] == json!(ELASTIC_NETWORK_INTERFACE))
        .collect()
}

/// One `Attachment$details` entry, by name.
fn detail(attachment: &Value, name: &str) -> Option<String> {
    attachment["details"]
        .as_array()?
        .iter()
        .find(|entry| entry["name"] == json!(name))
        .and_then(|entry| entry["value"].as_str().map(str::to_string))
}

/// Every interface Amazon EC2 reports in `subnet` — the read a client makes to find what its
/// subnet still holds.
async fn interfaces_in(router: &MultiServiceRouter, subnet: &str) -> Vec<String> {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("Filter.1.Name", "subnet-id"), ("Filter.1.Value.1", subnet)],
    )
    .await;
    elements(&body, "networkInterfaceId")
}

/// One instant the task reports, in milliseconds since the epoch.
fn instant_of(task: &Value, member: &str, case: &str) -> i64 {
    let seconds = task[member].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected the task to report {member}, got {task}")
    });
    (seconds * 1_000.0).round() as i64
}

/// `span` in milliseconds.
fn millis(span: Duration) -> i64 {
    span.as_millis() as i64
}

/// Assert that `later - earlier` is `spans` transition spans, naming the case and what the
/// task actually reported.
fn assert_spans(case: &str, what: &str, earlier: i64, later: i64, spans: u32, task: &Value) {
    let expected = millis(ecs_state::TRANSITION_SPAN) * i64::from(spans);
    let actual = later - earlier;
    assert!(
        (actual - expected).abs() <= millis(TOLERANCE),
        "case {case:?}: expected {what} to be {spans} transition span(s), {expected}ms, got \
         {actual}ms in {task}"
    );
}

/// RULE: every step the clock owes on a task's lifecycle is one transition span — the steps
/// that create and attach the interface an `awsvpc` task is allocated, and the steps that
/// detach and delete it, included — so the instants the task reports for its own launch and
/// stop are exactly those spans apart.
#[tokio::test]
async fn every_clock_owed_step_of_a_tasks_lifecycle_is_one_transition_span() {
    let router = emulator();
    let network = network_of(&router).await;
    cluster_with_capacity(&router).await;

    // Every row is launched before any wait, so the rows' clocks run together rather than one
    // after another.
    let mut placed: Vec<(&Row, String)> = Vec::new();
    for row in &ROWS {
        let mut container = json!({ "name": "app", "image": IMAGE, "essential": true });
        if !row.stopped_by_the_caller {
            // The row left to finish its own work gives its container a program that EXITS,
            // because a task is stopped as one whose essential container exited only once
            // that container really exits (`Container$exitCode` is "the exit code returned
            // from the container"). The row the CALLER stops names no program at all, so
            // nothing but the call ends it.
            container["command"] = json!(["sh", "-c", "exit 0"]);
        }
        ecs(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": row.family,
                "networkMode": row.network_mode,
                "cpu": "256",
                "memory": "512",
                "requiresCompatibilities": if row.fargate {
                    json!(["FARGATE"])
                } else {
                    json!(["EC2"])
                },
                "containerDefinitions": [container],
            }),
        )
        .await;
        let mut request = json!({
            "cluster": CLUSTER,
            "taskDefinition": row.family,
            "count": 1,
        });
        if row.fargate {
            request["launchType"] = json!("FARGATE");
        }
        if row.holds_interface() {
            let mut awsvpc = json!({
                "subnets": [network.subnet.clone()],
                "securityGroups": [network.security_group.clone()],
            });
            if row.fargate {
                // `AwsVpcConfiguration$assignPublicIp` is "whether the task's elastic network
                // interface receives a public IP address": a Fargate task reaches the public
                // registry its image is held in over the gateway the subnet routes to, and
                // without an address of its own there is no path to it. The option is for
                // that launch type alone ("Assign public IP is not supported for this launch
                // type"); a task on the cluster's own capacity is reached through the
                // instance that hosts it.
                awsvpc["assignPublicIp"] = json!("ENABLED");
            }
            request["networkConfiguration"] = json!({ "awsvpcConfiguration": awsvpc });
        }
        let run = ecs(&router, "RunTask", request).await;
        let task_arn = run["tasks"][0]["taskArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {:?}: RunTask answers a task ARN, got {run}", row.case)
            })
            .to_string();
        placed.push((row, task_arn));
    }

    for (row, task_arn) in placed {
        let case = row.case;
        // The row whose task the CALLER stops waits for it to be RUNNING — the status a task
        // whose interface is in place reports — and then stops it; nothing else would end a
        // task whose container runs no program of its own.
        if row.stopped_by_the_caller {
            let deadline = Instant::now() + SETTLE_DEADLINE;
            loop {
                let task = describe(&router, &task_arn, case).await;
                if task["lastStatus"] == json!("RUNNING") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the clock to start the task, got {task}"
                );
                tokio::time::sleep(LOOK_AGAIN_AFTER).await;
            }
            ecs(
                &router,
                "StopTask",
                json!({
                    "cluster": CLUSTER,
                    "task": task_arn.clone(),
                    "reason": "the client is finished with it",
                }),
            )
            .await;
        }
        // The CONDITION the row waits on: the task has reached the end of its lifecycle, so
        // every instant of it is recorded.
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let task = loop {
            let task = describe(&router, &task_arn, case).await;
            if task["lastStatus"] == json!("STOPPED") {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the clock to take the task through its lifecycle, \
                 got {task}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        };
        assert_ne!(
            task["stopCode"],
            json!("TaskFailedToStart"),
            "case {case:?}: the row reads the lifecycle of a task that RAN, got {task}"
        );

        let created = instant_of(&task, "createdAt", case);
        let connected = instant_of(&task, "connectivityAt", case);
        let started = instant_of(&task, "startedAt", case);
        let stopping = instant_of(&task, "stoppingAt", case);
        let stopped = instant_of(&task, "stoppedAt", case);

        // THE LAUNCH. A task of its own interface is on the network the instant that
        // interface is in place, one span per documented step of the attach; one that shares
        // its host's networking is on it once it has started there. Either way its own
        // `PENDING` -> `RUNNING` is one more span.
        if row.holds_interface() {
            assert_spans(
                case,
                "the attach of the task's own interface",
                created,
                connected,
                row.attach_steps(),
                &task,
            );
            assert_spans(
                case,
                "the start of a task whose interface is in place",
                connected,
                started,
                1,
                &task,
            );
        } else {
            assert_eq!(
                connected, started,
                "case {case:?}: a task that shares its host's networking is on the network \
                 once it has started there, got {task}"
            );
        }
        assert_spans(
            case,
            "the whole launch, from the placement to the task running",
            created,
            started,
            row.attach_steps() + 1,
            &task,
        );

        // THE WORK, which is a step of neither half: a task placed on its own runs for the
        // span it is given to do what it was placed for (`state::CONTAINER_WORK_SPAN`), and
        // its own stop is owed from the end of that — so a row left to finish its work is
        // RUNNING for at least that span. A row the CALLER stops is ended by the call
        // instead, whenever that call arrives.
        if !row.stopped_by_the_caller {
            assert!(
                stopping - started >= millis(ecs_state::CONTAINER_WORK_SPAN) - millis(TOLERANCE),
                "case {case:?}: expected the task to be RUNNING for the span it was placed to \
                 work for, {}ms, got {}ms in {task}",
                millis(ecs_state::CONTAINER_WORK_SPAN),
                stopping - started
            );
        }

        // THE STOP. Amazon ECS detaches and deletes the interface before the task is
        // stopped, one span per documented step of the release; a task that holds none is
        // stopped one span after it began stopping.
        assert_spans(
            case,
            "the stop, which waits for the release of the task's own interface",
            stopping,
            stopped,
            row.stop_steps(),
            &task,
        );

        // And the release really released it: the attachment reports the terminal status, and
        // the interface it named is no longer one the service that OWNS interfaces holds.
        let attachments = interface_attachments(&task);
        if !row.holds_interface() {
            assert!(
                attachments.is_empty(),
                "case {case:?}: a task outside the awsvpc network mode is allocated no \
                 interface, got {task}"
            );
            continue;
        }
        assert_eq!(
            attachments.len(),
            1,
            "case {case:?}: expected the one interface an awsvpc task is allocated, got {task}"
        );
        assert_eq!(
            attachments[0]["status"],
            json!(DELETED),
            "case {case:?}: a stopped task's interface has been released, got {task}"
        );
        let interface_id = detail(&attachments[0], "networkInterfaceId").unwrap_or_else(|| {
            panic!(
                "case {case:?}: the attachment keeps the id of the interface it held, got \
                 {task}"
            )
        });
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let held = interfaces_in(&router, &network.subnet).await;
            if !held.contains(&interface_id) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the interface {interface_id} of the stopped task to \
                 be gone from Amazon EC2, it still reports {held:?}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        }
    }
}
