// SPDX-License-Identifier: CC0-1.0

//! What a replication group's TEARDOWN gives back to the account's VPC.
//!
//! RULE: a cache node's elastic network interface is torn down with that node, so a delete
//! that takes the nodes releases every interface they held — and the subnets, the security
//! groups and the VPC those interfaces sat in become deletable again. It holds for the delete
//! of a REPLICATION GROUP as well as of a standalone cluster, even though the group's record
//! and the records of its member clusters stay readable in `deleting` while the teardown the
//! clock owes runs: the interface belongs to the node, not to the record of it, and the call
//! that begins the teardown is the last call a client tearing its network down makes to this
//! service.
//!
//! Why it matters: an interface left behind for a node nothing runs for holds its subnet
//! against `ec2:DeleteSubnet` and its VPC against `ec2:DeleteVpc` for ever — "You must
//! terminate all running instances in the subnet before you can delete the subnet"
//! (`DeleteSubnet`) — so a client that deletes its caches and then tears its network down
//! never converges and is answered `DependencyViolation` on every pass.
//!
//! The rows span what the rule generalises over: how many MEMBER CLUSTERS the group has (one
//! primary alone, a primary with a replica, a group whose keyspace is partitioned over two
//! shards), and how many subnets of how many zones the group's placement spans.
//!
//! This suite lives beside the router because the fact spans two services: the subnets, the
//! security groups and the interfaces are Amazon EC2's resources, minted and read through
//! EC2's own operations on the same engine that serves ElastiCache, and the caches are
//! ElastiCache's. Nothing reaches into either service's state struct; every assertion is a
//! wire answer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region the resources belong to.
const REGION: &str = "us-east-1";

/// The API version an ec2Query client sends beside `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version an ElastiCache awsQuery client sends beside `Action`.
const ELASTICACHE_API_VERSION: &str = "2015-02-02";

/// `NetworkInterface$RequesterId` of an interface Amazon ElastiCache created and manages for a
/// cache node of the account, which is what a client filters its VPC's interfaces by to find
/// the network presence of its caches.
const CACHE_REQUESTER: &str = "amazon-elasticache";

/// The node type every group below runs on.
const NODE_TYPE: &str = "cache.t3.micro";

/// How long this suite waits for something the CLOCK owes — a group settling, a teardown
/// carrying through — and how often it looks again while it waits. Generous on purpose: it
/// waits for the CONDITION rather than for a fixed time, so what it asserts does not depend
/// on how fast or how loaded the host is.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLE_DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator carrying Amazon EC2 and Amazon ElastiCache over ONE shared engine, exactly as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticache_amazonelasticachev9::EmulatedService::new(engine),
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

/// One query-protocol answer, reduced to what a client branches on.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    /// The query error envelope's `Code`, or `""` when the answer carries no envelope.
    fn error_code(&self) -> String {
        crate::query_wire::blocks_of(&self.body, "Error")
            .first()
            .and_then(|envelope| opt_element(envelope, "Code"))
            .unwrap_or_default()
    }
}

/// Call one query-protocol operation of `signing_name`, at `version`, with these form members.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Reply {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Reply {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// Call one Amazon EC2 action.
async fn ec2_call(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Reply {
    query(router, "ec2", EC2_API_VERSION, action, members).await
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let reply = ec2_call(router, action, members).await;
    assert_eq!(
        reply.status, 200,
        "EC2 {action} must succeed, got {} with {}",
        reply.status, reply.body
    );
    reply.body
}

/// Call one Amazon ElastiCache operation over its awsQuery wire.
async fn elasticache(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Reply {
    query(
        router,
        "elasticache",
        ELASTICACHE_API_VERSION,
        action,
        members,
    )
    .await
}

/// One VPC of the account: its id and the subnets EC2 minted in it, in zone order.
struct Vpc {
    id: String,
    subnets: Vec<String>,
}

/// Create one VPC with one subnet per entry of `zones`, through EC2's own operations, so every
/// identifier this suite hands ElastiCache is one the owner of subnets handed out.
async fn ec2_vpc(router: &MultiServiceRouter, second_octet: u8, zones: &[&str]) -> Vpc {
    let cidr = format!("10.{second_octet}.0.0/16");
    let id = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", cidr.as_str())]).await,
        "vpcId",
    );
    let mut subnets = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
        let body = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", id.as_str()),
                ("CidrBlock", subnet_cidr.as_str()),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&body, "subnetId"));
    }
    Vpc { id, subnets }
}

/// Mint one VPC security group in `vpc`, answered with the id EC2 reported for it.
async fn ec2_security_group(router: &MultiServiceRouter, vpc: &str, name: &str) -> String {
    let body = ec2(
        router,
        "CreateSecurityGroup",
        &[
            ("GroupName", name),
            ("GroupDescription", "a group guarding a cache of this suite"),
            ("VpcId", vpc),
        ],
    )
    .await;
    element(&body, "groupId")
}

/// Every interface EC2 reports in `vpc` that ElastiCache created and manages — the read a
/// client does to find the network presence of its caches.
async fn cache_interface_ids(router: &MultiServiceRouter, vpc: &str) -> Vec<String> {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc),
            ("Filter.2.Name", "requester-id"),
            ("Filter.2.Value.1", CACHE_REQUESTER),
        ],
    )
    .await;
    elements(&body, "networkInterfaceId")
}

/// Look again until `holds` is true of the cache interfaces of `vpc`, and answer the ids it
/// held then.
///
/// Bounded by [`SETTLE_DEADLINE`]; a deadline that passes fails naming the ids last seen.
async fn interfaces_until(
    router: &MultiServiceRouter,
    case: &str,
    description: &str,
    vpc: &str,
    holds: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = cache_interface_ids(router, vpc).await;
        if holds(&held) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {description} never came about; the VPC still holds {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Look again until `holds` is true of the answer to `action`, and answer with that reply.
async fn poll_until(
    router: &MultiServiceRouter,
    case: &str,
    description: &str,
    action: &str,
    members: &[(&str, &str)],
    holds: impl Fn(&Reply) -> bool,
) -> Reply {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reply = elasticache(router, action, members).await;
        if holds(&reply) {
            return reply;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {description} never came about; the last {action} answered {} {}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The owned form members as the borrowed pairs [`query`] takes.
fn borrowed(members: &[(String, String)]) -> Vec<(&str, &str)> {
    members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// One row: the shape of the replication group, and so how many member clusters — and
/// interfaces — that shape has.
struct Row {
    case: &'static str,
    /// The zones the group's placement spans, one subnet per zone.
    zones: &'static [&'static str],
    /// `NumNodeGroups`, the shards the keyspace is partitioned over.
    shards: usize,
    /// `ReplicasPerNodeGroup`, the read replicas of each shard.
    replicas: usize,
}

impl Row {
    /// How many member clusters the group is made of: one primary per shard plus its
    /// replicas, each one cache node and so one interface.
    fn members(&self) -> usize {
        self.shards * (1 + self.replicas)
    }
}

/// The rows span the member count and the placement: a single node, a primary with a replica,
/// and a partitioned keyspace whose members spread over two subnets.
const ROWS: &[Row] = &[
    Row {
        case: "a group of one primary in a one-subnet placement",
        zones: &["us-east-1a"],
        shards: 1,
        replicas: 0,
    },
    Row {
        case: "a group of a primary and one replica over two subnets",
        zones: &["us-east-1a", "us-east-1b"],
        shards: 1,
        replicas: 1,
    },
    Row {
        case: "a group whose keyspace is partitioned over two shards",
        zones: &["us-east-1a", "us-east-1b"],
        shards: 2,
        replicas: 0,
    },
];

#[tokio::test]
async fn deleting_a_replication_group_gives_its_interfaces_and_its_network_back() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let vpc = ec2_vpc(&router, 80 + u8::try_from(index).unwrap_or(0), row.zones).await;
        let group_name = format!("rg-teardown-subnets-{index}");
        let group_id = format!("rg-teardown-{index}");
        let security_group =
            ec2_security_group(&router, &vpc.id, &format!("rg-teardown-{index}")).await;

        let mut placement: Vec<(String, String)> = vec![
            ("CacheSubnetGroupName".to_string(), group_name.clone()),
            (
                "CacheSubnetGroupDescription".to_string(),
                format!("the placement of {group_id}"),
            ),
        ];
        for (position, subnet) in vpc.subnets.iter().enumerate() {
            placement.push((
                format!("SubnetIds.SubnetIdentifier.{}", position + 1),
                subnet.clone(),
            ));
        }
        let created = elasticache(&router, "CreateCacheSubnetGroup", &borrowed(&placement)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the cache subnet group must be created, got {} {}",
            created.status, created.body
        );

        // Before the group exists the account's VPC holds no interface of this service, so
        // what the teardown gives back below is what the create placed.
        let before = cache_interface_ids(&router, &vpc.id).await;
        assert!(
            before.is_empty(),
            "case {case:?}: no cache interface must exist before the group, got {before:?}"
        );

        let group: Vec<(String, String)> = vec![
            ("ReplicationGroupId".to_string(), group_id.clone()),
            (
                "ReplicationGroupDescription".to_string(),
                format!("the group {group_id} of this suite"),
            ),
            ("Engine".to_string(), "redis".to_string()),
            ("CacheNodeType".to_string(), NODE_TYPE.to_string()),
            ("NumNodeGroups".to_string(), row.shards.to_string()),
            (
                "ReplicasPerNodeGroup".to_string(),
                row.replicas.to_string(),
            ),
            ("CacheSubnetGroupName".to_string(), group_name.clone()),
            (
                "SecurityGroupIds.SecurityGroupId.1".to_string(),
                security_group.clone(),
            ),
        ];
        let placed = elasticache(&router, "CreateReplicationGroup", &borrowed(&group)).await;
        assert_eq!(
            placed.status, 200,
            "case {case:?}: the replication group must be created, got {} {}",
            placed.status, placed.body
        );
        poll_until(
            &router,
            case,
            &format!("the group {group_id} settling at available"),
            "DescribeReplicationGroups",
            &[("ReplicationGroupId", group_id.as_str())],
            |reply| opt_element(&reply.body, "Status").as_deref() == Some("available"),
        )
        .await;

        // ONE interface per member cluster's node while the group runs.
        let running = interfaces_until(
            &router,
            case,
            &format!("one interface per member cluster ({})", row.members()),
            &vpc.id,
            |held| held.len() == row.members(),
        )
        .await;
        assert_eq!(
            running.len(),
            row.members(),
            "case {case:?}: expected {} interfaces while the group runs, got {running:?}",
            row.members()
        );

        // The delete BEGINS the teardown: the group answers it and keeps reporting itself
        // while the clock carries the removal through …
        let deleted = elasticache(
            &router,
            "DeleteReplicationGroup",
            &[("ReplicationGroupId", group_id.as_str())],
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must be accepted, got {} {}",
            deleted.status, deleted.body
        );

        // … and the interfaces its nodes held are given back, so the account's VPC holds no
        // managed interface of this service any more.
        let released = interfaces_until(
            &router,
            case,
            "every cache interface being given back",
            &vpc.id,
            <[String]>::is_empty,
        )
        .await;
        assert!(
            released.is_empty(),
            "case {case:?}: the teardown must release every interface, got {released:?}"
        );

        // Which is what lets the client tear the network down: once the group's own record is
        // gone, its placement, its subnets, its security group and its VPC all delete with no
        // dependency left behind.
        poll_until(
            &router,
            case,
            &format!("the group {group_id} going away"),
            "DescribeReplicationGroups",
            &[("ReplicationGroupId", group_id.as_str())],
            |reply| reply.error_code() == "ReplicationGroupNotFoundFault",
        )
        .await;
        let dropped = elasticache(
            &router,
            "DeleteCacheSubnetGroup",
            &[("CacheSubnetGroupName", group_name.as_str())],
        )
        .await;
        assert_eq!(
            dropped.status, 200,
            "case {case:?}: the cache subnet group must delete, got {} {}",
            dropped.status, dropped.body
        );
        for subnet in &vpc.subnets {
            let removed = ec2_call(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
            assert_eq!(
                removed.status, 200,
                "case {case:?}: {subnet} must delete once no cache node sits in it, got {} {}",
                removed.status, removed.body
            );
        }
        let ungoverned = ec2_call(
            &router,
            "DeleteSecurityGroup",
            &[("GroupId", security_group.as_str())],
        )
        .await;
        assert_eq!(
            ungoverned.status, 200,
            "case {case:?}: {security_group} must delete once no interface carries it, got {} {}",
            ungoverned.status, ungoverned.body
        );
        let torn_down = ec2_call(&router, "DeleteVpc", &[("VpcId", vpc.id.as_str())]).await;
        assert_eq!(
            torn_down.status, 200,
            "case {case:?}: the VPC must delete once nothing of the cache is left in it, got {} \
             {}",
            torn_down.status, torn_down.body
        );
    }
}
