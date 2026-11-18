// SPDX-License-Identifier: CC0-1.0

//! Amazon ElastiCache's presence INSIDE the account's VPC, as the service that owns the VPC
//! holds it.
//!
//! A cache placed in a cache subnet group lives in the account's own network, and the facts
//! about that placement belong to Amazon EC2 rather than to ElastiCache. Three rules, each
//! driven over a SET of inputs rather than one recorded request:
//!
//! RULE 1 — a cache node has an ADDRESS in its subnet. Each node of a cluster placed in a
//! cache subnet group holds exactly one elastic network interface in one of that group's
//! subnets: an interface ElastiCache created and manages (`RequesterManaged` true, the
//! service's own `RequesterId`), carrying the cluster's VPC security groups and a private
//! address out of that subnet's own CIDR block, in the subnet whose Availability Zone the
//! node was created in. Deleting the cluster gives every one of those interfaces back. The
//! rows differ along what the rule generalises over: how many nodes the cluster has, how
//! many subnets the group spans, how the nodes are spread over zones, how many security
//! groups the cluster carries, and which engine it runs.
//!
//! RULE 2 — a cache subnet group's membership is DERIVED from the subnets that still exist.
//! `Subnets` and `SupportedNetworkTypes` re-derive from what EC2 holds now, so a subnet
//! deleted after the group was created drops out of the group, the group survives with its
//! name and its now-dangling `VpcId`, and a group left holding no subnet reports an empty
//! subnet list and an empty network-type list. The rows differ along how many subnets the
//! group holds and the order they are deleted in.
//!
//! RULE 3 — a cache subnet group's NAME is judged before its subnets are resolved. A create
//! whose name the account already holds is refused `CacheSubnetGroupAlreadyExists` even when
//! the same request names a subnet that does not exist; once the name is free the identical
//! request is refused for the subnet instead. The rows differ along how the taken name is
//! spelled and which subnet of the list is the absent one.
//!
//! This suite lives beside the router because every one of those facts spans two services:
//! the subnets, the security groups and the interfaces are Amazon EC2's resources, minted
//! and read through EC2's OWN operations on the same engine that serves ElastiCache, and the
//! caches are ElastiCache's. Nothing reaches into either service's state struct; every
//! assertion is a wire answer.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_contains, element, elements, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region the resources belong to.
const REGION: &str = "us-east-1";

/// The API version an ec2Query client sends beside `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version an ElastiCache awsQuery client sends beside `Action`.
const ELASTICACHE_API_VERSION: &str = "2015-02-02";

/// `NetworkInterface$RequesterId` of an interface Amazon ElastiCache created and manages for
/// a cache node of the account — the alias the service is published under, which is what a
/// client filters its own VPC's interfaces by to find its caches.
const CACHE_REQUESTER: &str = "amazon-elasticache";

/// How long this suite waits for a status change the CLOCK owes, and how often it looks
/// again while it waits. Generous on purpose: it waits for the CONDITION rather than for a
/// fixed time, so what it asserts does not depend on how fast or how loaded the host is.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
/// See [`SETTLE_DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator carrying Amazon EC2 and Amazon ElastiCache over ONE shared engine, exactly
/// as `core/router/src/lib.rs` mounts them.
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
/// what routes the request; the signature is never checked.
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
    /// The awsQuery error envelope's `Code`, or `""` when the answer carries no envelope.
    fn error_code(&self) -> String {
        crate::query_wire::blocks_of(&self.body, "Error")
            .first()
            .and_then(|envelope| opt_element(envelope, "Code"))
            .unwrap_or_default()
    }

    /// The error envelope's `Message`, or `""` when the answer carries no envelope.
    fn error_message(&self) -> String {
        crate::query_wire::blocks_of(&self.body, "Error")
            .first()
            .and_then(|envelope| opt_element(envelope, "Message"))
            .unwrap_or_default()
    }
}

/// Call one query-protocol operation of `signing_name`, at `version`, with these form
/// members.
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

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let reply = query(router, "ec2", EC2_API_VERSION, action, members).await;
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

/// One subnet of the account, as EC2 minted it.
#[derive(Debug, Clone)]
struct Subnet {
    id: String,
    zone: String,
    cidr: String,
}

/// One VPC of the account: its id and the subnets EC2 minted in it, in zone order.
#[derive(Debug, Clone)]
struct Vpc {
    id: String,
    subnets: Vec<Subnet>,
}

/// Create one VPC with one subnet per entry of `zones`, through EC2's own operations, so
/// every identifier this suite hands ElastiCache is one the owner of subnets handed out.
async fn ec2_vpc(router: &MultiServiceRouter, second_octet: u8, zones: &[&str]) -> Vpc {
    let cidr = format!("10.{second_octet}.0.0/16");
    let id = element(&ec2(router, "CreateVpc", &[("CidrBlock", &cidr)]).await, "vpcId");
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
        subnets.push(Subnet {
            id: element(&body, "subnetId"),
            zone: (*zone).to_string(),
            cidr: subnet_cidr,
        });
    }
    Vpc { id, subnets }
}

/// Mint `count` VPC security groups in `vpc` through `ec2:CreateSecurityGroup`, answered
/// with the ids EC2 reported for them.
async fn ec2_security_groups(
    router: &MultiServiceRouter,
    vpc: &str,
    label: &str,
    count: usize,
) -> Vec<String> {
    let mut minted = Vec::with_capacity(count);
    for index in 0..count {
        let name = format!("{label}-{index}");
        let body = ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", name.as_str()),
                (
                    "GroupDescription",
                    "a group guarding a cache of this suite",
                ),
                ("VpcId", vpc),
            ],
        )
        .await;
        minted.push(element(&body, "groupId"));
    }
    minted
}

/// `CreateCacheSubnetGroup` for `name` over `subnets`.
async fn create_subnet_group(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[String],
) -> Reply {
    let mut members: Vec<(String, String)> = vec![
        ("CacheSubnetGroupName".to_string(), name.to_string()),
        (
            "CacheSubnetGroupDescription".to_string(),
            format!("the placement of {name}"),
        ),
    ];
    for (index, subnet) in subnets.iter().enumerate() {
        members.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            subnet.clone(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    elasticache(router, "CreateCacheSubnetGroup", &borrowed).await
}

/// `DescribeCacheSubnetGroups` for one group.
async fn describe_subnet_group(router: &MultiServiceRouter, name: &str) -> Reply {
    elasticache(
        router,
        "DescribeCacheSubnetGroups",
        &[("CacheSubnetGroupName", name)],
    )
    .await
}

/// One network interface of the account, reduced to what these rows assert over.
#[derive(Debug, Clone)]
struct Interface {
    id: String,
    subnet: String,
    address: String,
    description: String,
    requester: String,
    requester_managed: String,
    groups: Vec<String>,
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

/// One interface as EC2 describes it when asked for exactly it, which is how a client reads
/// an address off an id it found through a filter.
///
/// Read one interface at a time on purpose: an ec2Query answer nests an `<item>` list inside
/// each interface (its security groups, its addresses), so a reader that split the whole
/// answer on `<item>` would tear one interface into several.
async fn describe_interface(router: &MultiServiceRouter, id: &str) -> Interface {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", id)],
    )
    .await;
    Interface {
        id: id.to_string(),
        subnet: opt_element(&body, "subnetId").unwrap_or_default(),
        address: opt_element(&body, "privateIpAddress").unwrap_or_default(),
        description: opt_element(&body, "description").unwrap_or_default(),
        requester: opt_element(&body, "requesterId").unwrap_or_default(),
        requester_managed: opt_element(&body, "requesterManaged").unwrap_or_default(),
        groups: elements(&body, "groupId"),
    }
}

/// Every cache interface of `vpc`, each read back through the describe that reports one.
async fn cache_interfaces(router: &MultiServiceRouter, vpc: &str) -> Vec<Interface> {
    let mut described = Vec::new();
    for id in cache_interface_ids(router, vpc).await {
        described.push(describe_interface(router, &id).await);
    }
    described
}

/// The interfaces EC2 reports for ONE subnet and ONE security group — the two filters the
/// recorded diagnosis uses to find a cache node's interface.
async fn interfaces_in(
    router: &MultiServiceRouter,
    subnet: &str,
    security_group: &str,
) -> Vec<String> {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "subnet-id"),
            ("Filter.1.Value.1", subnet),
            ("Filter.2.Name", "group-id"),
            ("Filter.2.Value.1", security_group),
        ],
    )
    .await;
    elements(&body, "networkInterfaceId")
}

/// Whether the IPv4 address `address` falls inside the IPv4 block `cidr`.
///
/// The shared reader (`query_wire::block_contains`), in this suite's argument order: the
/// judgement — an address the OWNER chose against the block the subnet was created with — is
/// read ONE way across the suites that make it.
fn address_in_cidr(address: &str, cidr: &str) -> bool {
    block_contains(cidr, address)
}

/// Wait until `holds` is true of the answer to `action`, and answer with that reply.
///
/// The one place this suite waits for a status change the clock owes: it looks again until
/// the condition holds or [`SETTLE_DEADLINE`] passes, and a deadline that passes fails with
/// the last answer quoted.
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

/// One row of rule 1: the cluster's shape and the placement it is given.
struct NodeRow {
    case: &'static str,
    /// The zones the group's subnets sit in, one subnet per zone.
    zones: &'static [&'static str],
    /// How many cache nodes the cluster has.
    nodes: usize,
    /// How many VPC security groups the cluster carries.
    security_groups: usize,
    /// The engine the cluster runs.
    engine: &'static str,
    /// `AZMode`, where the row sets one: a `cross-az` cluster spreads its nodes over zones.
    az_mode: Option<&'static str>,
}

/// The rows span what rule 1 generalises over: the node count, the number of subnets the
/// group spans, how the nodes are spread over zones, the number of security groups and the
/// engine.
const NODE_ROWS: &[NodeRow] = &[
    NodeRow {
        case: "one memcached node in a group of one subnet",
        zones: &["us-east-1a"],
        nodes: 1,
        security_groups: 1,
        engine: "memcached",
        az_mode: None,
    },
    NodeRow {
        case: "three memcached nodes in one zone of a two-subnet group",
        zones: &["us-east-1a", "us-east-1b"],
        nodes: 3,
        security_groups: 1,
        engine: "memcached",
        az_mode: Some("single-az"),
    },
    NodeRow {
        case: "two memcached nodes spread across the group's two subnets",
        zones: &["us-east-1a", "us-east-1b"],
        nodes: 2,
        security_groups: 2,
        engine: "memcached",
        az_mode: Some("cross-az"),
    },
    NodeRow {
        case: "one redis node in a group of three subnets",
        zones: &["us-east-1a", "us-east-1b", "us-east-1c"],
        nodes: 1,
        security_groups: 1,
        engine: "redis",
        az_mode: None,
    },
];

#[tokio::test]
async fn each_cache_node_holds_one_managed_interface_in_its_own_subnet() {
    for (index, row) in NODE_ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let vpc = ec2_vpc(&router, 60 + u8::try_from(index).unwrap_or(0), row.zones).await;
        let group_name = format!("vpc-presence-{index}");
        let cluster_id = format!("vpc-presence-cluster-{index}");
        let subnet_ids: Vec<String> = vpc.subnets.iter().map(|subnet| subnet.id.clone()).collect();
        let security_groups = ec2_security_groups(
            &router,
            &vpc.id,
            &format!("vpc-presence-{index}"),
            row.security_groups,
        )
        .await;

        let created = create_subnet_group(&router, &group_name, &subnet_ids).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the cache subnet group must be created, got {} {}",
            created.status, created.body
        );
        // Before the cluster exists, the account's VPC holds no interface of this service:
        // what the assertions below find was placed by the create, not by the setup.
        let before = cache_interfaces(&router, &vpc.id).await;
        assert!(
            before.is_empty(),
            "case {case:?}: no cache interface must exist before the cluster, got {before:?}"
        );

        let nodes = row.nodes.to_string();
        let mut members: Vec<(String, String)> = vec![
            ("CacheClusterId".to_string(), cluster_id.clone()),
            ("Engine".to_string(), row.engine.to_string()),
            ("CacheNodeType".to_string(), "cache.t3.micro".to_string()),
            ("NumCacheNodes".to_string(), nodes),
            ("CacheSubnetGroupName".to_string(), group_name.clone()),
        ];
        if let Some(az_mode) = row.az_mode {
            members.push(("AZMode".to_string(), az_mode.to_string()));
        }
        for (position, group) in security_groups.iter().enumerate() {
            members.push((
                format!("SecurityGroupIds.SecurityGroupId.{}", position + 1),
                group.clone(),
            ));
        }
        let borrowed: Vec<(&str, &str)> = members
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let placed = elasticache(&router, "CreateCacheCluster", &borrowed).await;
        assert_eq!(
            placed.status, 200,
            "case {case:?}: the cluster must be created, got {} {}",
            placed.status, placed.body
        );
        // The recorded diagnosis reads the interfaces of an AVAILABLE cluster, so the suite
        // waits for that condition.
        poll_until(
            &router,
            case,
            &format!("the cluster {cluster_id} settling at available"),
            "DescribeCacheClusters",
            &[("CacheClusterId", cluster_id.as_str())],
            |reply| opt_element(&reply.body, "CacheClusterStatus").as_deref() == Some("available"),
        )
        .await;

        // ONE interface per node, each in a subnet of the group, managed by ElastiCache,
        // carrying the cluster's groups and an address of its own subnet's block.
        let interfaces = cache_interfaces(&router, &vpc.id).await;
        assert_eq!(
            interfaces.len(),
            row.nodes,
            "case {case:?}: expected one interface per cache node ({}), got {interfaces:?}",
            row.nodes
        );
        let blocks: BTreeMap<&str, &str> = vpc
            .subnets
            .iter()
            .map(|subnet| (subnet.id.as_str(), subnet.cidr.as_str()))
            .collect();
        let mut addresses: Vec<String> = Vec::new();
        for interface in &interfaces {
            assert_eq!(
                interface.requester_managed, "true",
                "case {case:?}: {} must be managed by the service, got {interface:?}",
                interface.id
            );
            assert_eq!(
                interface.requester, CACHE_REQUESTER,
                "case {case:?}: {} must name the service that created it, got {interface:?}",
                interface.id
            );
            let Some(cidr) = blocks.get(interface.subnet.as_str()) else {
                panic!(
                    "case {case:?}: {} sits in {:?}, which is no subnet of the group {:?}",
                    interface.id, interface.subnet, subnet_ids
                );
            };
            assert!(
                address_in_cidr(&interface.address, cidr),
                "case {case:?}: {} must hold an address of {cidr}, got {:?}",
                interface.id,
                interface.address
            );
            let mut reported = interface.groups.clone();
            reported.sort();
            let mut expected = security_groups.clone();
            expected.sort();
            assert_eq!(
                reported, expected,
                "case {case:?}: {} must carry the cluster's security groups, got {reported:?}",
                interface.id
            );
            assert!(
                interface.description.contains(&cluster_id),
                "case {case:?}: {} must describe the cache it serves, got {:?}",
                interface.id,
                interface.description
            );
            addresses.push(interface.address.clone());
        }
        // Each node holds its OWN address and its own interface: no two share either.
        addresses.sort();
        let distinct = addresses.len();
        addresses.dedup();
        assert_eq!(
            addresses.len(),
            distinct,
            "case {case:?}: every node's address must be its own, got {addresses:?}"
        );

        // A node sits in the subnet whose ZONE it was created in: the zones of the subnets
        // the interfaces occupy are exactly the zones the cluster reports for its nodes, so
        // a cross-az cluster of two nodes holds one interface in each of the group's two
        // subnets and a single-az cluster holds all of them in the one zone it was placed
        // in.
        let zone_of: BTreeMap<&str, &str> = vpc
            .subnets
            .iter()
            .map(|subnet| (subnet.id.as_str(), subnet.zone.as_str()))
            .collect();
        let mut interface_zones: Vec<String> = interfaces
            .iter()
            .map(|interface| {
                zone_of
                    .get(interface.subnet.as_str())
                    .map_or_else(String::new, |zone| (*zone).to_string())
            })
            .collect();
        interface_zones.sort();
        let read = elasticache(
            &router,
            "DescribeCacheClusters",
            &[
                ("CacheClusterId", cluster_id.as_str()),
                // "An optional flag … to retrieve information about the individual cache
                // nodes" (`DescribeCacheClusters$ShowCacheNodeInfo`), which is where the
                // zone each node was created in is reported.
                ("ShowCacheNodeInfo", "true"),
            ],
        )
        .await;
        let mut node_zones = elements(&read.body, "CustomerAvailabilityZone");
        node_zones.sort();
        assert_eq!(
            interface_zones, node_zones,
            "case {case:?}: the interfaces must sit in the zones the cluster reports for \
             its nodes, got {interface_zones:?} against {node_zones:?}"
        );
        let mut occupied: Vec<String> = interfaces
            .iter()
            .map(|interface| interface.subnet.clone())
            .collect();
        occupied.sort();
        occupied.dedup();
        let expected_subnets = match row.az_mode {
            Some("cross-az") => row.nodes.min(row.zones.len()),
            _ => 1,
        };
        assert_eq!(
            occupied.len(),
            expected_subnets,
            "case {case:?}: expected the nodes to occupy {expected_subnets} subnet(s), got \
             {occupied:?}"
        );

        // The same interfaces are what the recorded diagnosis finds: filtered by the
        // group's subnet and the cluster's security group.
        let found = interfaces_in(&router, &vpc.subnets[0].id, &security_groups[0]).await;
        let expected_in_first: Vec<String> = interfaces
            .iter()
            .filter(|interface| interface.subnet == vpc.subnets[0].id)
            .map(|interface| interface.id.clone())
            .collect();
        assert_eq!(
            found.len(),
            expected_in_first.len(),
            "case {case:?}: the subnet-and-group filter must find the node interfaces of \
             {}, expected {expected_in_first:?}, got {found:?}",
            vpc.subnets[0].id
        );

        // The interfaces do not outlive the cluster: deleting it gives every one of them,
        // and the addresses they held, back.
        let deleted = elasticache(
            &router,
            "DeleteCacheCluster",
            &[("CacheClusterId", cluster_id.as_str())],
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must answer 200, got {} {}",
            deleted.status, deleted.body
        );
        let after = cache_interfaces(&router, &vpc.id).await;
        assert!(
            after.is_empty(),
            "case {case:?}: a deleted cluster must hold no interface, got {after:?}"
        );
    }
}

/// One row of rule 2: how many subnets the group holds and the order they are deleted in.
struct MembershipRow {
    case: &'static str,
    zones: &'static [&'static str],
    /// The positions of the group's subnets, in the order this row deletes them.
    delete_order: &'static [usize],
}

const MEMBERSHIP_ROWS: &[MembershipRow] = &[
    MembershipRow {
        case: "a group of one subnet",
        zones: &["us-east-1a"],
        delete_order: &[0],
    },
    MembershipRow {
        case: "a group of two subnets, the first deleted first",
        zones: &["us-east-1a", "us-east-1b"],
        delete_order: &[0, 1],
    },
    MembershipRow {
        case: "a group of two subnets, the last deleted first",
        zones: &["us-east-1a", "us-east-1b"],
        delete_order: &[1, 0],
    },
    MembershipRow {
        case: "a group of three subnets, the middle one deleted first",
        zones: &["us-east-1a", "us-east-1b", "us-east-1c"],
        delete_order: &[1, 2, 0],
    },
];

#[tokio::test]
async fn a_cache_subnet_group_reports_only_the_subnets_ec2_still_holds() {
    for (index, row) in MEMBERSHIP_ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let vpc = ec2_vpc(&router, 80 + u8::try_from(index).unwrap_or(0), row.zones).await;
        let group_name = format!("membership-{index}");
        let subnet_ids: Vec<String> = vpc.subnets.iter().map(|subnet| subnet.id.clone()).collect();

        let created = create_subnet_group(&router, &group_name, &subnet_ids).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the group must be created, got {} {}",
            created.status, created.body
        );

        // Every subnet of an IPv4-only VPC offers `ipv4`, so a group over them does too —
        // the member is derived from the group's membership, whatever that membership is.
        let mut still_there: Vec<String> = subnet_ids.clone();
        for (step, position) in row.delete_order.iter().enumerate() {
            let read = describe_subnet_group(&router, &group_name).await;
            assert_eq!(
                read.status, 200,
                "case {case:?} / step {step}: the group must be readable, got {} {}",
                read.status, read.body
            );
            assert_eq!(
                elements(&read.body, "SubnetIdentifier"),
                still_there,
                "case {case:?} / step {step}: the group must report the subnets EC2 still \
                 holds, got {}",
                read.body
            );
            assert_eq!(
                elements(&read.body, "VpcId"),
                vec![vpc.id.clone()],
                "case {case:?} / step {step}: the group keeps the VPC it was created in, got {}",
                read.body
            );
            let network_types = group_network_types(&read.body);
            let expected: Vec<String> = if still_there.is_empty() {
                Vec::new()
            } else {
                vec!["ipv4".to_string()]
            };
            assert_eq!(
                network_types, expected,
                "case {case:?} / step {step}: expected {expected:?}, got {network_types:?} in {}",
                read.body
            );

            let gone = &vpc.subnets[*position];
            ec2(&router, "DeleteSubnet", &[("SubnetId", gone.id.as_str())]).await;
            still_there.retain(|subnet| *subnet != gone.id);
        }

        // Every subnet gone, and then the VPC itself: the group OUTLIVES both, keeps its
        // name and its now-dangling VpcId, and reports an empty membership and an empty
        // network-type list.
        ec2(&router, "DeleteVpc", &[("VpcId", vpc.id.as_str())]).await;
        let orphaned = describe_subnet_group(&router, &group_name).await;
        assert_eq!(
            orphaned.status, 200,
            "case {case:?}: the group must outlive its subnets, got {} {}",
            orphaned.status, orphaned.body
        );
        assert_eq!(
            elements(&orphaned.body, "CacheSubnetGroupName"),
            vec![group_name.clone()],
            "case {case:?}: the group keeps its name, got {}",
            orphaned.body
        );
        assert_eq!(
            elements(&orphaned.body, "VpcId"),
            vec![vpc.id.clone()],
            "case {case:?}: the group keeps the VPC it was created in, got {}",
            orphaned.body
        );
        assert!(
            elements(&orphaned.body, "SubnetIdentifier").is_empty(),
            "case {case:?}: a group whose subnets are gone holds none, got {}",
            orphaned.body
        );
        assert!(
            group_network_types(&orphaned.body).is_empty(),
            "case {case:?}: a group holding no subnet offers no network type, got {}",
            orphaned.body
        );
        // The member is REPORTED, as the empty list it is, rather than withheld: a client
        // reading it to decide ipv6 or dual-stack eligibility finds a list either way.
        assert!(
            crate::query_wire::block_of(&orphaned.body, "SupportedNetworkTypes").is_some(),
            "case {case:?}: the network types must be reported as a list, got {}",
            orphaned.body
        );
        // And EC2 itself answers for none of those subnets any more, which is what makes
        // the group's membership above a DERIVED answer rather than a stored one.
        for subnet in &vpc.subnets {
            let described = query(
                &router,
                "ec2",
                EC2_API_VERSION,
                "DescribeSubnets",
                &[("SubnetId.1", subnet.id.as_str())],
            )
            .await;
            assert_eq!(
                described.status, 400,
                "case {case:?}: EC2 must hold no subnet {} any more, got {} {}",
                subnet.id, described.status, described.body
            );
        }
    }
}

/// The network types the one `CacheSubnetGroup` of a reply publishes for ITSELF: the members
/// of the group's own `SupportedNetworkTypes`, not of a subnet's.
fn group_network_types(body: &str) -> Vec<String> {
    let group = crate::query_wire::block_of(body, "CacheSubnetGroup").unwrap_or_default();
    // The group's own list comes AFTER its `Subnets`, so the group's block is read from the
    // end of that list: a subnet's own `SupportedNetworkTypes` lives inside `Subnets`.
    let tail = group
        .rsplit_once("</Subnets>")
        .map_or(group, |(_, tail)| tail);
    crate::query_wire::block_of(tail, "SupportedNetworkTypes")
        .map(|list| elements(list, "member"))
        .unwrap_or_default()
}

/// One row of rule 3: how the taken name is spelled on the second create, and which
/// position of the subnet list carries the absent subnet.
struct NameRow {
    case: &'static str,
    created_as: &'static str,
    second_as: &'static str,
    /// Where the absent subnet id goes in the second create's list.
    absent_at: usize,
}

const NAME_ROWS: &[NameRow] = &[
    NameRow {
        case: "the same name again, with the absent subnet first",
        created_as: "taken-group-one",
        second_as: "taken-group-one",
        absent_at: 0,
    },
    NameRow {
        case: "the same name in upper case, with the absent subnet second",
        created_as: "taken-group-two",
        second_as: "TAKEN-GROUP-TWO",
        absent_at: 1,
    },
];

#[tokio::test]
async fn a_taken_cache_subnet_group_name_is_refused_before_the_subnets_are_resolved() {
    let router = emulator();
    let vpc = ec2_vpc(&router, 70, &["us-east-1a", "us-east-1b"]).await;
    let subnet_ids: Vec<String> = vpc.subnets.iter().map(|subnet| subnet.id.clone()).collect();
    // A subnet of the account that was minted and then DELETED: an id EC2 answers nothing
    // for, which is what the second create names beside the taken name.
    let deleted = ec2_vpc(&router, 71, &["us-east-1a"]).await;
    ec2(
        &router,
        "DeleteSubnet",
        &[("SubnetId", deleted.subnets[0].id.as_str())],
    )
    .await;

    for row in NAME_ROWS {
        let case = row.case;
        let created = create_subnet_group(&router, row.created_as, &subnet_ids).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the first create must be accepted, got {} {}",
            created.status, created.body
        );

        let mut named = subnet_ids.clone();
        named[row.absent_at] = deleted.subnets[0].id.clone();
        let refused = create_subnet_group(&router, row.second_as, &named).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, "CacheSubnetGroupAlreadyExists"),
            "case {case:?}: the taken name must be reported first, got {} {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            format!(
                "Cache subnet group {} already exists.",
                row.created_as.to_ascii_lowercase()
            ),
            "case {case:?}: expected the taken-name sentence, got {:?}",
            refused.error_message()
        );

        // Once the name is FREE the identical request is refused for the subnet instead, so
        // the precedence above is an ordering rather than the subnet going unjudged.
        let removed = elasticache(
            &router,
            "DeleteCacheSubnetGroup",
            &[("CacheSubnetGroupName", row.created_as)],
        )
        .await;
        assert_eq!(
            removed.status, 200,
            "case {case:?}: the group must be deletable, got {} {}",
            removed.status, removed.body
        );
        let on_the_subnet = create_subnet_group(&router, row.second_as, &named).await;
        assert_eq!(
            (on_the_subnet.status, on_the_subnet.error_code().as_str()),
            (400, "InvalidParameterValue"),
            "case {case:?}: with the name free the subnet must be refused, got {} {}",
            on_the_subnet.status,
            on_the_subnet.body
        );
        assert!(
            on_the_subnet
                .error_message()
                .contains(&deleted.subnets[0].id),
            "case {case:?}: the refusal must name the subnet that resolves to nothing, got {:?}",
            on_the_subnet.error_message()
        );
    }
}
