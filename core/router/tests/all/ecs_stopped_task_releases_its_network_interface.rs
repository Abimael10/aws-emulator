// SPDX-License-Identifier: CC0-1.0

//! The elastic network interface an `awsvpc` ECS task runs on, as the service that OWNS
//! network interfaces holds it — while the task runs, and once it has stopped.
//!
//! RULE: `RegisterTaskDefinition$networkMode` says a task in the `awsvpc` network mode "is
//! allocated an elastic network interface", and that interface is an Amazon VPC resource:
//! Amazon ECS creates it in the subnet the launch named, the task holds it while it runs, and
//! Amazon ECS tears it down with the task. `Attachment$status` is "the status of the
//! attachment. Valid values are `PRECREATED`, `CREATED`, `ATTACHING`, `ATTACHED`,
//! `DETACHING`, `DETACHED`, `DELETED`, and `FAILED`", so the attachment of a task that has
//! stopped leaves `ATTACHED` and reaches the terminal `DELETED` — and that terminal status
//! stands for a resource that is really GONE: the interface the attachment named is no
//! longer one Amazon EC2's own `DescribeNetworkInterfaces` reports, which is what lets a
//! client that tears a subnet or a security group down after its tasks stop stop waiting.
//!
//! This suite lives beside the router because that one rule spans two services: the VPC, the
//! subnet and the interface are Amazon EC2's resources, minted and read through EC2's OWN
//! operations on the same engine that serves Amazon ECS, and the task and its attachment are
//! Amazon ECS's. Nothing reaches into either service's state struct; every assertion is a
//! wire answer.
//!
//! The rows differ along what the rule generalises over: how the task stopped — a `StopTask`
//! of the caller's and the end of the work a task placed on its own was placed to do. The
//! STEPS the attachment passes through on its way to the terminal status are pinned by the
//! owning crate's own suite, which can watch one task closely:
//! `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/task_interface_attach_and_connectivity.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region the resources belong to.
const REGION: &str = "us-west-2";

/// The API version an ec2Query client sends beside `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The ECS cluster every task below runs in.
const CLUSTER: &str = "interfacerelease";

/// The container every revision runs. Held in a PUBLIC registry, reached over the internet
/// gateway the network below carries, so the image fetch succeeds and the task runs — which
/// is when its interface is in place. An image this emulator holds no program for, so a task
/// of it finishes its own work, which one row below is about: an image whose own command is a
/// server in the foreground would hold such a task `RUNNING`
/// (`container_servers::image_declared_command`).
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The destination of the route that carries the VPC's traffic to its internet gateway.
const INTERNET_DESTINATION: &str = "0.0.0.0/0";

/// `Attachment$type` of the interface an `awsvpc` task is allocated, from that member's own
/// doc ("such as `ElasticNetworkInterface`, `Service Connect`, and
/// `AmazonElasticBlockStorage`").
const ELASTIC_NETWORK_INTERFACE: &str = "ElasticNetworkInterface";

/// The terminal `Attachment$status` of an attachment whose resource is gone.
const DELETED: &str = "DELETED";

/// How long this suite waits for a transition the CLOCK owes, and how often it looks again
/// while it waits. Generous on purpose: it waits for the CONDITION rather than for a fixed
/// time, so what it asserts does not depend on how fast or how loaded the host is.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

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
/// attached to it and a default route to that gateway, plus a security group of its own —
/// every one created through the operations a caller calls.
struct Network {
    subnet: String,
    security_group: String,
}

async fn network_of(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.90.7.0/24"),
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
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc.as_str())],
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
                ("GroupName", "release-tasks"),
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

/// Read ONE task back through `DescribeTasks` — the stored record, not the answer of the call
/// that changed it.
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

/// The one `ElasticNetworkInterface` attachment an `awsvpc` task holds.
fn only_attachment(task: &Value, case: &str) -> Value {
    let attachments: Vec<Value> = task["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|attachment| attachment["type"] == json!(ELASTIC_NETWORK_INTERFACE))
        .collect();
    assert_eq!(
        attachments.len(),
        1,
        "case {case:?}: expected the one interface an awsvpc task is allocated, got {task}"
    );
    attachments[0].clone()
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
/// subnet still holds before tearing the network down.
async fn interfaces_in(router: &MultiServiceRouter, subnet: &str) -> Vec<String> {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("Filter.1.Name", "subnet-id"), ("Filter.1.Value.1", subnet)],
    )
    .await;
    elements(&body, "networkInterfaceId")
}

/// The private IPv4 address Amazon EC2 reports for ONE interface, or `None` when that store
/// holds no such interface.
///
/// Read one interface at a time on purpose: an ec2Query answer nests `<item>` lists inside
/// each interface, so a reader that split the whole answer would tear one interface into
/// several.
async fn ec2_address_of(router: &MultiServiceRouter, subnet: &str, id: &str) -> Option<String> {
    if !interfaces_in(router, subnet)
        .await
        .iter()
        .any(|held| held == id)
    {
        return None;
    }
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", id)],
    )
    .await;
    opt_element(&body, "privateIpAddress")
}

/// One row: how its task stopped, and the names its own resources take.
struct Row {
    case: &'static str,
    family: &'static str,
    /// Whether the caller stops the task with `StopTask`; a row that does not is left to
    /// finish the work it was placed to do.
    stopped_by_the_caller: bool,
    /// The argument vector its container runs: a program that ends for the row whose task
    /// finishes its own work, and none for the row the caller stops.
    program: &'static [&'static str],
}

const ROWS: [Row; 2] = [
    Row {
        case: "a task the caller stopped",
        family: "releasestopped",
        stopped_by_the_caller: true,
        program: &[],
    },
    Row {
        case: "a task whose own work finished",
        family: "releasefinished",
        stopped_by_the_caller: false,
        program: &["sh", "-c", "exit 0"],
    },
];

/// The interface one row's task reports once it is in place: the id and the address the
/// attachment names.
struct Interface {
    id: String,
    address: String,
}

/// Look again until the row's task is RUNNING with its interface in place, and answer what
/// the attachment reports for it.
async fn running_interface(
    router: &MultiServiceRouter,
    task_arn: &str,
    case: &str,
) -> Interface {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let task = describe(router, task_arn, case).await;
        let attachment = only_attachment(&task, case);
        if task["lastStatus"] == json!("RUNNING") {
            assert_eq!(
                attachment["status"],
                json!("ATTACHED"),
                "case {case:?}: a running task's interface is in place, got {attachment}"
            );
            let id = detail(&attachment, "networkInterfaceId").unwrap_or_else(|| {
                panic!("case {case:?}: an interface in place reports its id, got {attachment}")
            });
            let address = detail(&attachment, "privateIPv4Address").unwrap_or_else(|| {
                panic!("case {case:?}: an interface in place reports its address, got {attachment}")
            });
            return Interface { id, address };
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the clock to attach the interface and run the task, got \
             {task}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

#[tokio::test]
async fn a_stopped_awsvpc_tasks_interface_is_gone_from_the_service_that_owns_it() {
    let router = emulator();
    let network = network_of(&router).await;
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // Every row is launched before any wait, so the clocks of the rows run together rather
    // than one after another.
    let mut placed: Vec<(&Row, String)> = Vec::new();
    for row in &ROWS {
        let mut app = json!({ "name": "app", "image": IMAGE, "memory": 128 });
        if !row.program.is_empty() {
            // The row whose task ends ON ITS OWN gives its container a program that EXITS,
            // because that exit is what ends such a task: a task is stopped as one whose
            // essential container exited only once that container really exits
            // (`Container$exitCode` is "the exit code returned from the container"). The row
            // the CALLER stops needs none.
            app["command"] = json!(row.program);
        }
        ecs(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": row.family,
                "networkMode": "awsvpc",
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "256",
                "memory": "512",
                "containerDefinitions": [app],
            }),
        )
        .await;
        let run = ecs(
            &router,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": row.family,
                "count": 1,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": [network.subnet.clone()],
                        "securityGroups": [network.security_group.clone()],
                        // A Fargate task reaches the public registry its container's image
                        // is held in over the gateway the subnet routes to, and
                        // `AwsVpcConfiguration$assignPublicIp` is "whether the task's
                        // elastic network interface receives a public IP address" — without
                        // one there is no path to it, and the task would be stopped before
                        // it ran.
                        "assignPublicIp": "ENABLED",
                    }
                },
            }),
        )
        .await;
        let task_arn = run["tasks"][0]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: RunTask answers a task ARN, got {run}", row.case))
            .to_string();
        placed.push((row, task_arn));
    }

    // While the task runs, the interface its attachment names is one the OWNING service
    // reports, in the subnet the launch named and at the address the attachment reports: two
    // views of one record.
    let mut running: Vec<(&Row, String, Interface)> = Vec::new();
    for (row, task_arn) in placed {
        let interface = running_interface(&router, &task_arn, row.case).await;
        assert_eq!(
            ec2_address_of(&router, &network.subnet, &interface.id).await,
            Some(interface.address.clone()),
            "case {:?}: expected Amazon EC2 to hold the interface {} the running task is \
             reached on at {}",
            row.case,
            interface.id,
            interface.address
        );
        if row.stopped_by_the_caller {
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
        running.push((row, task_arn, interface));
    }

    // Once the attachment reports the terminal status, the interface it stood for is GONE
    // from the service that owns it — so a client that waits for the release before tearing
    // its network down is not racing an interface the account still holds.
    for (row, task_arn, interface) in &running {
        let case = row.case;
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let task = describe(&router, task_arn, case).await;
            let attachment = only_attachment(&task, case);
            if attachment["status"] == json!(DELETED) {
                assert_eq!(
                    task["lastStatus"],
                    json!("STOPPED"),
                    "case {case:?}: the interface is released once the task has stopped, got \
                     {task}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the stopped task's interface to be released, got \
                 {attachment}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        }
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let held = interfaces_in(&router, &network.subnet).await;
            if !held.contains(&interface.id) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the interface {} of the stopped task to be gone from \
                 Amazon EC2, it still reports {held:?}",
                interface.id
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        }
    }

    // And nothing of either task is left behind in the subnet: both interfaces were the
    // tasks' own, so the subnet holds none once both have stopped.
    let left = interfaces_in(&router, &network.subnet).await;
    assert!(
        left.is_empty(),
        "expected the subnet to hold no interface once every task of it had stopped, got \
         {left:?}"
    );
}
