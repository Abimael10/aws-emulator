// SPDX-License-Identifier: CC0-1.0

//! The interface an `awsvpc` task runs on while Amazon ECS is still FETCHING the images its
//! containers run.
//!
//! RULE: Amazon ECS attaches the interface such a task is allocated BEFORE it launches the
//! task's containers — "PROVISIONING: Amazon ECS has to perform additional steps before the
//! task is launched. For example, for tasks that use the `awsvpc` network mode, the elastic
//! network interface needs to be provisioned" (the Amazon ECS task lifecycle) — and that
//! interface belongs to the task, so it is torn down WITH it. While a task is being placed and
//! run its `ElasticNetworkInterface` attachment therefore settles at `ATTACHED`, carrying the
//! `subnetId`, `networkInterfaceId`, `macAddress` and `privateIPv4Address` that
//! `Attachment$details` names for an elastic network interface ("For elastic network
//! interfaces, this includes the network interface ID, the MAC address, the subnet ID, and the
//! private IPv4 address"), and the id in that detail resolves to an interface Amazon EC2's own
//! `DescribeNetworkInterfaces` returns. A fetch the placement has no PATH for is answered by
//! NOTHING: it ends in a connection timeout, which is what `Task$stoppedReason` reports for it
//! ("`CannotPullContainerError` ... i/o timeout"), so a task whose image cannot be fetched is
//! still coming up — `PENDING`, with that attachment `ATTACHED` and its interface resolvable —
//! for as long as the agent spends on the attempt, and only the stop it then earns
//! (`Task$stopCode` `TaskFailedToStart`) takes the attachment to `DELETED` and the interface
//! out of Amazon EC2.
//!
//! Which matters because the interface is the JOIN between the two views of a running task: a
//! client that waits for the attachment in order to read the task's address, its security
//! groups or the public address its launch asked for has to find that attachment `ATTACHED`
//! while the task is being brought up, whether or not the image behind the task can be fetched
//! from where it was placed.
//!
//! This suite lives beside the router because the rule spans two services: Amazon ECS places
//! the task and reports the attachment, Amazon EC2 owns the interface, the subnet and the
//! route tables that decide the path, and both are mounted on ONE shared engine here exactly
//! as the production router mounts them. Every id asserted on is one of the two minted.
//!
//! The rows differ along what the rule generalises over: HOW the path from the placement to
//! the registry is closed — a subnet with no route off its VPC at all, and a subnet routed
//! through an internet gateway by a task given NO public address (an internet gateway carries
//! traffic only for an address that is reachable from the internet, which for an `awsvpc` task
//! is `AwsVpcConfiguration$assignPublicIp`: "whether the task's elastic network interface
//! receives a public IP address") — beside a CONTRAST row in the same gateway subnet WITH a
//! public address, whose fetch is made and whose task runs.
//!
//! Sources: `smithy_query(query="doc", shape="Attachment", member="details")` and
//! `member="status"` on the ecs model, `smithy_query(query="shape", shape="TaskStopCode")` for
//! the stop code, and `smithy_query(query="shape", shape="NetworkInterface")` on the ec2 model
//! for the members read back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of Amazon ECS's `awsJson1_1` operations.
const TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The cluster every launch below runs in.
const CLUSTER: &str = "fetchattempt";

/// The two Fargate-capable `awsvpc` families the rows run: one whose container is handed no
/// program of its own, and one whose program does not return — so the CONTRAST row's task is
/// `RUNNING` for as long as its program is.
const PLAIN_FAMILY: &str = "fetchattemptplain";
const SERVING_FAMILY: &str = "fetchattemptserving";

/// The program the contrast row's container runs: one that does not return.
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// The image every container starts from: one held in a PUBLIC registry, which is no AWS
/// service and publishes no VPC endpoint, so it is reachable only over the internet — which is
/// what makes the PATH the rows differ along the thing that decides the fetch.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// `Attachment$type` of the interface an `awsvpc` task is allocated.
const ELASTIC_NETWORK_INTERFACE: &str = "ElasticNetworkInterface";

/// `Attachment$status` of an attachment whose interface is in place, and the one of an
/// attachment whose interface is gone — two of the values that member lists, spelled out here
/// rather than imported so a wrong constant in the implementation cannot make this pass.
const ATTACHED: &str = "ATTACHED";
const DELETED: &str = "DELETED";

/// The details an attachment whose interface is in place reports.
const INTERFACE_DETAILS: [&str; 4] = [
    "subnetId",
    "networkInterfaceId",
    "macAddress",
    "privateIPv4Address",
];

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all, and the
/// error class the container runtime words a failed fetch with.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const CANNOT_PULL: &str = "CannotPullContainerError";

/// How often a poll looks again at the condition it waits on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a poll keeps looking for a transition the START of an `awsvpc` task owes: Amazon
/// ECS creates that task's interface and attaches it one documented step at a time, so what is
/// waited for is the CONDITION, with generous margin for a loaded host.
const START_DEADLINE: Duration = Duration::from_secs(120);

/// How long the agent spends on a fetch nothing answers before it gives the attempt up, which
/// is what the stop of such a task then waits for.
const FETCH_ATTEMPT: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::FAILED_IMAGE_FETCH_TIMEOUT;

/// How long a poll keeps looking for the STOP such a task earns: the attempt itself and the
/// same generous margin, which covers the start before it and the release of the interface
/// after it.
const STOP_DEADLINE: Duration = FETCH_ATTEMPT.saturating_add(START_DEADLINE);

/// How long a row looks again at a task whose fetch is still being attempted, asserting each
/// time that it is still coming up with its interface attached.
///
/// A QUARTER of the attempt, so the window is nowhere near the instant the attempt ends — and
/// never shorter than several task transitions, whatever that attempt is: what the window has
/// to be past is the start such a task would have taken had its fetch been answered, which is
/// ONE transition away ([`TRANSITION_SPAN`]), and a window under that would leave this loop
/// asserting nothing.
const STILL_ATTACHING: Duration = {
    let quarter = FETCH_ATTEMPT.as_millis() as u64 / 4;
    let floor = TRANSITION_SPAN.as_millis() as u64 * 4;
    Duration::from_millis(if quarter > floor { quarter } else { floor })
};

/// How long one lifecycle boundary of a task takes — the span the start this loop watches a
/// task NOT take would have been away.
const TRANSITION_SPAN: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::TRANSITION_SPAN;

/// One emulator carrying Amazon EC2 and Amazon ECS over ONE shared engine, as the production
/// router mounts them.
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
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One ec2Query action, as its HTTP status and its XML body.
async fn ec2_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
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
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One ec2Query action that must be served, as its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = ec2_call(router, action, params).await;
    assert_eq!(
        status, 200,
        "EC2 {action} must answer 200, got {status} with {body}"
    );
    body
}

/// One Amazon ECS call that must be served, as its parsed body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
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
        "case {case:?}: expected ECS {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// The network the rows place into: one subnet routed to an internet gateway, and one with no
/// route off its VPC at all.
struct Network {
    routed_by_gateway: String,
    isolated: String,
}

/// Build that network through Amazon EC2's OWN operations.
async fn ec2_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.88.0.0/16")]).await,
        "vpcId",
    );
    let routed_by_gateway = subnet(router, &vpc, "10.88.1.0/24", "us-west-2a").await;
    let isolated = subnet(router, &vpc, "10.88.2.0/24", "us-west-2b").await;

    // "To use an internet gateway, your subnet's route table must contain a route that directs
    // internet-bound traffic to the internet gateway" (Amazon VPC User Guide).
    let internet_gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", internet_gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let gateway_table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", gateway_table.as_str()),
            ("SubnetId", routed_by_gateway.as_str()),
        ],
    )
    .await;
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", gateway_table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", internet_gateway.as_str()),
        ],
    )
    .await;

    // The isolated subnet gets a route table of its own with no route in it, so nothing
    // carries its traffic off the VPC.
    let isolated_table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", isolated_table.as_str()),
            ("SubnetId", isolated.as_str()),
        ],
    )
    .await;

    Network {
        routed_by_gateway,
        isolated,
    }
}

/// One subnet of `vpc`, created over the wire.
async fn subnet(router: &MultiServiceRouter, vpc: &str, cidr_block: &str, zone: &str) -> String {
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc),
                ("CidrBlock", cidr_block),
                ("AvailabilityZone", zone),
            ],
        )
        .await,
        "subnetId",
    )
}

/// Create the cluster and register the two revisions the rows run.
async fn seed_ecs(router: &MultiServiceRouter) {
    ecs_ok(
        router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        "setup",
    )
    .await;
    for (family, command) in [
        (PLAIN_FAMILY, None),
        (SERVING_FAMILY, Some(STAYS_UP.to_vec())),
    ] {
        let mut container = json!({
            "name": "app",
            "image": IMAGE,
            "memory": 128,
            "essential": true,
        });
        if let Some(command) = command {
            container["command"] = json!(command);
        }
        ecs_ok(
            router,
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "networkMode": "awsvpc",
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "256",
                "memory": "512",
                "containerDefinitions": [container],
            }),
            "setup",
        )
        .await;
    }
}

/// One row: where it places its task, the public address the placement asks for, and whether
/// the image can be fetched from there.
struct Row {
    case: &'static str,
    family: &'static str,
    subnet_id: String,
    assign_public_ip: &'static str,
    reaches_the_registry: bool,
}

/// `RunTask` one task of `row`'s family into `row`'s placement, and answer its ARN.
async fn run(router: &MultiServiceRouter, row: &Row) -> String {
    let placed = ecs_ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": row.family,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [row.subnet_id.clone()],
                    "assignPublicIp": row.assign_public_ip,
                }
            },
        }),
        row.case,
    )
    .await;
    placed["tasks"][0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected RunTask to place one task, got {placed}",
                row.case
            )
        })
        .to_string()
}

/// One task as `DescribeTasks` reports it now — the stored record, not the answer of the call
/// that placed it.
async fn describe(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let described = ecs_ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    described["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {described}"))
}

/// The one `ElasticNetworkInterface` attachment a task holds.
fn interface_attachment(task: &Value, case: &str) -> Value {
    let attachments: Vec<Value> = task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|attachment| attachment["type"] == json!(ELASTIC_NETWORK_INTERFACE))
        .cloned()
        .collect();
    assert_eq!(
        attachments.len(),
        1,
        "case {case:?}: expected the one interface an awsvpc task is allocated, got {task}"
    );
    attachments[0].clone()
}

/// The value of the `Attachment$details` entry named `name`.
fn detail(attachment: &Value, name: &str) -> Option<String> {
    attachment["details"]
        .as_array()?
        .iter()
        .find(|entry| entry["name"] == json!(name))
        .and_then(|entry| entry["value"].as_str().map(str::to_string))
}

/// Poll one task until its interface attachment reports `ATTACHED`, and answer the record it
/// reported that on.
async fn attached(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + START_DEADLINE;
    loop {
        let task = describe(router, task_arn, case).await;
        if interface_attachment(&task, case)["status"] == json!(ATTACHED) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the interface Amazon ECS allocated the task to reach \
             {ATTACHED}, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that `task`'s attachment stands for an interface IN PLACE — `ATTACHED`, with each of
/// the four details such an attachment reports, in the subnet the launch named — and answer
/// the `networkInterfaceId` it names.
fn assert_interface_in_place(task: &Value, subnet_id: &str, case: &str) -> String {
    let attachment = interface_attachment(task, case);
    assert_eq!(
        attachment["status"],
        json!(ATTACHED),
        "case {case:?}: expected the attachment of a task being brought up to report \
         {ATTACHED}, got {attachment}"
    );
    for name in INTERFACE_DETAILS {
        assert!(
            detail(&attachment, name).is_some(),
            "case {case:?}: expected an attached interface to report {name}, got {attachment}"
        );
    }
    assert_eq!(
        detail(&attachment, "subnetId").as_deref(),
        Some(subnet_id),
        "case {case:?}: expected the interface in the subnet the launch named, got {attachment}"
    );
    detail(&attachment, "networkInterfaceId").unwrap_or_default()
}

/// Assert that Amazon EC2 holds the interface `network_interface_id`, in `subnet_id` — the
/// join the attachment's detail is for.
async fn assert_ec2_holds(
    router: &MultiServiceRouter,
    network_interface_id: &str,
    subnet_id: &str,
    case: &str,
) {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", network_interface_id)],
    )
    .await;
    assert_eq!(
        elements(&body, "networkInterfaceId"),
        vec![network_interface_id.to_string()],
        "case {case:?}: expected Amazon EC2 to hold exactly the interface the attachment \
         names, got {body}"
    );
    assert!(
        elements(&body, "subnetId").contains(&subnet_id.to_string()),
        "case {case:?}: expected the interface Amazon EC2 holds to sit in {subnet_id}, got \
         {body}"
    );
}

/// Whether Amazon EC2 still holds an interface under `network_interface_id`, read off the
/// inventory of the account's interfaces: an id nothing holds is not an interface to describe.
async fn ec2_still_holds(router: &MultiServiceRouter, network_interface_id: &str) -> bool {
    let body = ec2(router, "DescribeNetworkInterfaces", &[]).await;
    elements(&body, "networkInterfaceId").contains(&network_interface_id.to_string())
}

/// RULE: the interface an `awsvpc` task is allocated is attached while Amazon ECS brings the
/// task up — including for as long as it spends on a fetch the task's placement has no path
/// for — and is only taken down when the task stops.
#[tokio::test]
async fn an_awsvpc_tasks_interface_stays_attached_while_its_image_fetch_is_attempted() {
    let router = emulator();
    let network = ec2_network(&router).await;
    seed_ecs(&router).await;

    let rows = [
        Row {
            case: "a subnet with no route off its VPC",
            family: PLAIN_FAMILY,
            subnet_id: network.isolated.clone(),
            assign_public_ip: "DISABLED",
            reaches_the_registry: false,
        },
        Row {
            case: "an internet gateway, for a task given no public address",
            family: PLAIN_FAMILY,
            subnet_id: network.routed_by_gateway.clone(),
            assign_public_ip: "DISABLED",
            reaches_the_registry: false,
        },
        Row {
            case: "an internet gateway, for a task given a public address",
            family: SERVING_FAMILY,
            subnet_id: network.routed_by_gateway.clone(),
            assign_public_ip: "ENABLED",
            reaches_the_registry: true,
        },
    ];

    // Every task is launched first and read afterwards: each comes up on its own clock, so the
    // whole table costs one start rather than one per row.
    let mut placed: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        placed.push((row, run(&router, row).await));
    }

    // Every row's interface is ATTACHED while Amazon ECS brings its task up, and the id it
    // names is an interface Amazon EC2 holds in the launch's own subnet — whether or not the
    // image behind the task can be fetched from there.
    let mut launched: Vec<(&Row, String, String)> = Vec::new();
    for (row, task_arn) in placed {
        let task = attached(&router, &task_arn, row.case).await;
        let interface_id = assert_interface_in_place(&task, &row.subnet_id, row.case);
        assert_ec2_holds(&router, &interface_id, &row.subnet_id, row.case).await;
        launched.push((row, task_arn, interface_id));
    }

    // The task whose image CAN be fetched runs, and its interface is attached with it.
    for (row, task_arn, _) in launched.iter().filter(|(row, _, _)| row.reaches_the_registry) {
        let deadline = Instant::now() + START_DEADLINE;
        let task = loop {
            let task = describe(&router, task_arn, row.case).await;
            if task["lastStatus"] == json!("RUNNING") {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected a task whose image can be fetched to run, got {task}",
                row.case
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert!(
            task["stopCode"].is_null(),
            "case {:?}: expected no stopCode on a task that is running, got {task}",
            row.case
        );
        assert_interface_in_place(&task, &row.subnet_id, row.case);
    }

    // The tasks whose image cannot be fetched are STILL COMING UP, with their interface
    // attached, while the agent spends the attempt: looked at again and again over a window
    // far past the one transition the start of a task takes, each look asserting what the
    // client this is for reads.
    for (row, task_arn, _) in launched
        .iter()
        .filter(|(row, _, _)| !row.reaches_the_registry)
    {
        let until = Instant::now() + STILL_ATTACHING;
        while Instant::now() < until {
            let task = describe(&router, task_arn, row.case).await;
            assert!(
                task["stopCode"].is_null(),
                "case {:?}: expected a task whose fetch is still being attempted not to be \
                 stopped, got {task}",
                row.case
            );
            assert_eq!(
                task["lastStatus"],
                json!("PENDING"),
                "case {:?}: expected a task whose fetch is still being attempted to be coming \
                 up, got {task}",
                row.case
            );
            assert_interface_in_place(&task, &row.subnet_id, row.case);
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    // And only the stop that failed fetch earns takes the interface down: the attachment
    // reaches DELETED, the stop names the class the runtime reports a failed fetch under and
    // the image it could not fetch, and Amazon EC2 holds the interface no more.
    for (row, task_arn, interface_id) in launched
        .iter()
        .filter(|(row, _, _)| !row.reaches_the_registry)
    {
        let deadline = Instant::now() + STOP_DEADLINE;
        let task = loop {
            let task = describe(&router, task_arn, row.case).await;
            if task["lastStatus"] == json!("STOPPED") {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected the task to stop once the fetch attempt was over, got \
                 {task}",
                row.case
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            task["stopCode"],
            json!(TASK_FAILED_TO_START),
            "case {:?}: expected the stop code of a task that never started, got {task}",
            row.case
        );
        let reason = task["stoppedReason"].as_str().unwrap_or_default().to_string();
        assert!(
            reason.contains(CANNOT_PULL) && reason.contains(IMAGE),
            "case {:?}: expected the stoppedReason to report {CANNOT_PULL} for {IMAGE}, got \
             {reason:?}",
            row.case
        );
        assert_eq!(
            interface_attachment(&task, row.case)["status"],
            json!(DELETED),
            "case {:?}: expected the attachment of a stopped task to report {DELETED}, got \
             {task}",
            row.case
        );
        // The interface is Amazon VPC's, released with the task that held it — polled for,
        // because that release is owed by the clock rather than by this read.
        let deadline = Instant::now() + START_DEADLINE;
        while ec2_still_holds(&router, interface_id).await {
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected Amazon EC2 to hold no interface {interface_id} once the \
                 task that held it stopped",
                row.case
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
