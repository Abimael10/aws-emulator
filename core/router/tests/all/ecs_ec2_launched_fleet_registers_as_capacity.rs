// SPDX-License-Identifier: CC0-1.0

//! The capacity a FLEET of Amazon EC2 instances launched into one cluster gives it.
//!
//! RULE under test: every RUNNING Amazon EC2 instance of the account whose user data
//! configures the container agent with a cluster of the account is a container instance of
//! that cluster — `ACTIVE`, with its agent connected, reporting its own `ec2InstanceId` — and
//! each launch is one container instance of its own, however many instances are launched and
//! whichever subnet or Availability Zone of the Region each one sits in. An instance whose
//! user data names a cluster the account does not hold registers nowhere, exactly as its
//! agent would be refused.
//!
//! `RegisterContainerInstance` is the registration itself ("this action is only used by the
//! Amazon ECS agent, and it is not intended for use outside of the agent. Registers an EC2
//! instance into the specified cluster. This instance becomes available to place containers
//! on"); the agent runs ON the instance, so the cluster it joins is the one the launch put in
//! the instance's user data, and `Cluster$registeredContainerInstancesCount` ("the number of
//! container instances registered into the cluster") is what makes it the cluster's capacity.
//!
//! Why the suite lives beside the router: the instances are Amazon EC2's, launched through
//! that service's own `RunInstances` into subnets it minted, and the registration they earn is
//! read back through Amazon ECS's `ListContainerInstances`, `DescribeContainerInstances` and
//! `DescribeClusters` — so the two services have to agree. The Amazon ECS crate's own suite
//! covers the FORMS one instance's user data can carry the configuration in; what this one
//! spans is the FLEET: one host, two hosts in two zones, two hosts in one subnet, and a host
//! configured for a cluster that is not there.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster the launches join, and one name no `CreateCluster` of this suite creates.
const CLUSTER: &str = "fleetcapacity";
const ABSENT_CLUSTER: &str = "fleetabsent";

/// The AMI every launch boots. Well-formed (`ami-<17 hexadecimal digits>`), which is all a
/// launch needs: which AMI carries the container agent is not a fact this emulator publishes.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance type every launch is.
const INSTANCE_TYPE: &str = "t3.micro";

/// The status and the resources a registration reports, spelled out here rather than imported
/// so a wrong constant in the implementation cannot make a row pass.
const ACTIVE: &str = "ACTIVE";
const RESOURCE_CPU: &str = "CPU";
const RESOURCE_MEMORY: &str = "MEMORY";

/// How long one wait keeps looking before it calls the registration stalled, and how long it
/// leaves between looks. The wait ends on the CONDITION; the deadline is only there so a
/// registration that never happens fails the row instead of hanging it.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// How many further looks a row that expects NO registration takes. Together with
/// [`LOOK_AGAIN_AFTER`] a window far longer than the span a registration this emulator does
/// owe falls due in, so the emptiness such a row asserts is one that HELD rather than one
/// read too early.
const QUIET_LOOKS: usize = 20;

/// One row: which zones the row's hosts are launched into (one entry per host, so two
/// entries are two hosts), which cluster their user data configures, and how many container
/// instances the cluster must end up with.
struct Row {
    case: &'static str,
    /// The zone LETTER of each host's subnet. Two hosts sharing a letter share one subnet.
    zones: &'static [char],
    cluster: &'static str,
    registered: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "one host in one zone",
        zones: &['a'],
        cluster: CLUSTER,
        registered: 1,
    },
    Row {
        case: "two hosts in two zones of the Region",
        zones: &['a', 'b'],
        cluster: CLUSTER,
        registered: 2,
    },
    Row {
        case: "two hosts in one subnet",
        zones: &['b', 'b'],
        cluster: CLUSTER,
        registered: 2,
    },
    Row {
        case: "a host configured for a cluster the account does not hold",
        zones: &['a'],
        cluster: ABSENT_CLUSTER,
        registered: 0,
    },
];

/// One emulator carrying Amazon ECS and Amazon EC2 over ONE shared engine, as the production
/// router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; no signature is checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
    )
}

/// One `awsJson1_1` Amazon ECS call that has to succeed, answered with the parsed body.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
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
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// The Amazon EC2 API version an `ec2Query` body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// One `ec2Query` Amazon EC2 call that has to succeed, answered with its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ec2.{REGION}.amazonaws.com/"))
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected ec2 {action} to answer 200, got {status} with {payload}"
    );
    payload
}

/// Create one VPC through Amazon EC2's own API and answer its id.
async fn vpc(router: &MultiServiceRouter, case: &str, block: &str) -> String {
    element(
        &ec2(router, case, "CreateVpc", &[("CidrBlock", block)]).await,
        "vpcId",
    )
}

/// Create one subnet of `vpc_id` in the zone `letter` of the Region and answer its id.
async fn subnet(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    block: &str,
    letter: char,
) -> String {
    let zone = format!("{REGION}{letter}");
    element(
        &ec2(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", vpc_id),
                ("CidrBlock", block),
                ("AvailabilityZone", zone.as_str()),
            ],
        )
        .await,
        "subnetId",
    )
}

/// Launch ONE instance into `subnet_id` with the user data that configures the container
/// agent with `cluster`, and answer the instance id Amazon EC2 minted.
///
/// The documented way to launch an instance into a cluster: the agent's `ECS_CLUSTER`
/// variable, appended to its configuration file by the launch's user data. The text is the one
/// the instance reads — a launch that carries it base64, as a client's does, is covered by the
/// Amazon ECS crate's own suite.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    subnet_id: &str,
    cluster: &str,
) -> String {
    let user_data = format!("#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n");
    let launched = ec2(
        router,
        case,
        "RunInstances",
        &[
            ("ImageId", IMAGE_ID),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", subnet_id),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("UserData", user_data.as_str()),
        ],
    )
    .await;
    element(&launched, "instanceId")
}

/// Every container instance of `cluster`, as `DescribeContainerInstances` reports them —
/// through the listing a client walks first, whose default filter "includes container
/// instances set to all states other than INACTIVE".
async fn capacity_of(router: &MultiServiceRouter, case: &str, cluster: &str) -> Vec<Value> {
    let listed = ecs(
        router,
        case,
        "ListContainerInstances",
        json!({ "cluster": cluster }),
    )
    .await;
    let arns: Vec<Value> = listed["containerInstanceArns"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        case,
        "DescribeContainerInstances",
        json!({ "cluster": cluster, "containerInstances": arns }),
    )
    .await;
    described["containerInstances"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `registeredContainerInstancesCount` `DescribeClusters` reports for `cluster`.
async fn registered_count(router: &MultiServiceRouter, case: &str, cluster: &str) -> i64 {
    let described = ecs(
        router,
        case,
        "DescribeClusters",
        json!({ "clusters": [cluster] }),
    )
    .await;
    described["clusters"][0]["registeredContainerInstancesCount"]
        .as_i64()
        .unwrap_or_default()
}

/// The integer a named resource of a registration reports.
fn integer_resource(instance: &Value, name: &str) -> Option<i64> {
    instance["registeredResources"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|resource| resource["name"].as_str() == Some(name))
        .and_then(|resource| resource["integerValue"].as_i64())
}

#[tokio::test]
async fn every_instance_launched_into_a_cluster_becomes_one_of_its_container_instances() {
    for row in ROWS {
        let case = row.case;
        // A router of its own per row, so what a row reads is the capacity ITS launches gave
        // and nothing another row left behind.
        let router = emulator();
        ecs(&router, case, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
        let network = vpc(&router, case, "10.80.0.0/16").await;
        // One subnet per DISTINCT zone letter, so two hosts of one letter share a subnet.
        let mut subnets: Vec<(char, String)> = Vec::new();
        let mut launched = Vec::new();
        for letter in row.zones {
            let placed = match subnets.iter().find(|(zone, _)| zone == letter) {
                Some((_, placed)) => placed.clone(),
                None => {
                    let block = format!("10.80.{}.0/24", u32::from(*letter) - u32::from('a'));
                    let placed = subnet(&router, case, &network, &block, *letter).await;
                    subnets.push((*letter, placed.clone()));
                    placed
                }
            };
            launched.push(launch(&router, case, &placed, row.cluster).await);
        }

        // The registration is owed by the CLOCK — the agent registers on its own, with no
        // client request between the launch and the cluster reporting the capacity — so the
        // row waits for the condition, which is what a client polling the listing does.
        let deadline = Instant::now() + DEADLINE;
        let mut reported = capacity_of(&router, case, CLUSTER).await;
        while reported.len() != row.registered && Instant::now() < deadline {
            ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
            reported = capacity_of(&router, case, CLUSTER).await;
        }
        // A row that expects none keeps looking, so what it asserts is an emptiness that
        // held rather than one read before the registration would have fallen due.
        if row.registered == 0 {
            for _ in 0..QUIET_LOOKS {
                ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
                reported = capacity_of(&router, case, CLUSTER).await;
                assert!(
                    reported.is_empty(),
                    "case {case:?}: expected the hosts {launched:?} to register nowhere, got \
                     {reported:?}"
                );
            }
        }
        assert_eq!(
            reported.len(),
            row.registered,
            "case {case:?}: expected {} container instance(s) of {CLUSTER} for the hosts \
             {launched:?}, got {reported:?}",
            row.registered
        );

        // Each registration is one of the launched instances, `ACTIVE`, agent connected, and
        // reporting the resources its instance type offers.
        let mut registered_ids: Vec<String> = Vec::new();
        for instance in &reported {
            let id = instance["ec2InstanceId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a registration to name its instance, got \
                            {instance}")
                })
                .to_string();
            assert!(
                launched.contains(&id),
                "case {case:?}: expected the capacity to be one of the launched hosts \
                 {launched:?}, got {id}"
            );
            assert_eq!(
                instance["status"].as_str(),
                Some(ACTIVE),
                "case {case:?}: expected the host {id} to be {ACTIVE}, got {instance}"
            );
            assert_eq!(
                instance["agentConnected"].as_bool(),
                Some(true),
                "case {case:?}: expected the agent of {id} to be connected, got {instance}"
            );
            for resource in [RESOURCE_CPU, RESOURCE_MEMORY] {
                let offered = integer_resource(instance, resource);
                assert!(
                    offered.is_some_and(|value| value > 0),
                    "case {case:?}: expected the host {id} to report the {resource} its \
                     instance type offers, got {offered:?} in {instance}"
                );
            }
            registered_ids.push(id);
        }
        registered_ids.sort();
        registered_ids.dedup();
        assert_eq!(
            registered_ids.len(),
            row.registered,
            "case {case:?}: expected one container instance per launched host, got \
             {registered_ids:?}"
        );

        // And the cluster counts exactly that capacity, which is what a placement is made
        // against.
        let counted = registered_count(&router, case, CLUSTER).await;
        assert_eq!(
            counted,
            row.registered as i64,
            "case {case:?}: expected the cluster to count {} registered instance(s), got \
             {counted}",
            row.registered
        );
    }
}
