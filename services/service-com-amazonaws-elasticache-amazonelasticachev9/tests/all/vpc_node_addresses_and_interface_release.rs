// SPDX-License-Identifier: CC0-1.0

//! WHERE a cache node placed in the account's VPC is reached, and what happens to that
//! presence when the cache goes.
//!
//! RULE 1 — a cache placed in a cache subnet group runs in the account's VPC and is reachable
//! only from INSIDE it, and each of its nodes is reached at an endpoint of its OWN: the
//! address the control plane publishes for a node (`Endpoint$Address`, "the information
//! required for client programs to connect to a cache node") is that node's own address on the
//! interface this emulator's listeners are bound on, carrying the port the core assigned that
//! node's socket — so a group created with N node groups publishes N node addresses that
//! resolve to N different backends, and one address wearing every shard's name (the flat
//! keyspace) is exactly what that rules out. Each node also holds one interface of its own in
//! a subnet of the group it was placed in, which is its presence in the account's network. A
//! cache in NO cache subnet group sits in no VPC of the account, so it keeps the address its
//! own socket answers at.
//!
//! RULE 2 — that presence does not outlive the cache. A `DeleteReplicationGroup` gives every
//! member node's interface back as it begins the teardown, with the address it held, so the
//! subnets the group sat in can be deleted again straight away — the same thing a
//! `DeleteCacheCluster` does for a standalone cluster's node.
//!
//! The rows differ along what the rules generalise over: how many node groups the group is
//! partitioned into, how many members each of them holds, whether the keyspace is partitioned
//! at all, whether the cache is a replication group or a standalone cluster, and whether it
//! was placed in a VPC at all.
//!
//! Every assertion goes over a wire: the control-plane reads are `awsQuery` requests through
//! the production router, the interfaces are read through Amazon EC2's OWN
//! `DescribeNetworkInterfaces` (the service that owns them), and every RESP session is opened
//! to the published endpoint through the register of the data planes this emulator serves,
//! which is how a client inside the cache's VPC reaches one.

use std::collections::{BTreeMap, BTreeSet};

use crate::cache_cluster_lifecycle::{account_subnet_ids, subnet_group_params};
use crate::cache_data_plane::{command, node_endpoint, open, open_in_vpc, Resp};
use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// The node type every cache in this suite runs on.
const NODE_TYPE: &str = "cache.t3.micro";

/// The `CacheNodeId` of the one node a cache this emulator serves RESP for runs.
const FIRST_NODE: &str = "0001";

/// `NetworkInterface$RequesterId` of an interface Amazon ElastiCache placed for a cache node
/// of the account, and how that interface names the node it belongs to: the service's own
/// values, read off the handler rather than re-spelled, so the read below cannot drift from
/// what the handler writes.
use service_com_amazonaws_elasticache_amazonelasticachev9::state::ec2_cache_node_interfaces::{
    interface_description, CACHE_INTERFACE_REQUESTER,
};

/// One row: the cache to stand up, and the shape its placement has to produce.
struct Row {
    case: &'static str,
    /// The `ReplicationGroupId`, or the `CacheClusterId` for a row that creates a standalone
    /// cluster instead.
    id: &'static str,
    /// Whether `id` names a replication group; `false` creates a cache cluster.
    group: bool,
    /// What the create sends beside the identity and the node type.
    extra: &'static [(&'static str, &'static str)],
    /// Whether the cache is placed in the suite's cache subnet group.
    placed: bool,
    /// How many member nodes the cache must end up with.
    nodes: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "three node groups, one node each",
        id: "vpc-shards-three",
        group: true,
        extra: &[
            ("Engine", "redis"),
            ("ClusterMode", "enabled"),
            ("NumNodeGroups", "3"),
            ("ReplicasPerNodeGroup", "0"),
        ],
        placed: true,
        nodes: 3,
    },
    Row {
        case: "two node groups with a replica each",
        id: "vpc-shards-pair",
        group: true,
        extra: &[
            ("Engine", "redis"),
            ("ClusterMode", "enabled"),
            ("NumNodeGroups", "2"),
            ("ReplicasPerNodeGroup", "1"),
        ],
        placed: true,
        nodes: 4,
    },
    Row {
        case: "one node group, which is the whole keyspace",
        id: "vpc-unpartitioned",
        group: true,
        extra: &[
            ("Engine", "redis"),
            ("NumCacheClusters", "2"),
        ],
        placed: true,
        nodes: 2,
    },
    Row {
        case: "a standalone cluster in the account's VPC",
        id: "vpc-standalone",
        group: false,
        extra: &[("Engine", "valkey"), ("NumCacheNodes", "1")],
        placed: true,
        nodes: 1,
    },
    Row {
        case: "a cluster in no VPC of the account",
        id: "vpc-less-standalone",
        group: false,
        extra: &[("Engine", "redis"), ("NumCacheNodes", "1")],
        placed: false,
        nodes: 1,
    },
];

/// The cache subnet group every placed row sits in.
const SUBNET_GROUP: &str = "vpc-node-address-subnets";

/// Create the cache of `row` and look again until it reports itself settled.
async fn available_cache(emulator: &Emulator, row: &Row) -> Reply {
    let (action, report, status_path) = match row.group {
        true => (
            "CreateReplicationGroup",
            "DescribeReplicationGroups",
            "ReplicationGroup/Status",
        ),
        false => (
            "CreateCacheCluster",
            "DescribeCacheClusters",
            "CacheCluster/CacheClusterStatus",
        ),
    };
    let identity = match row.group {
        true => params(&[
            ("ReplicationGroupId", row.id),
            ("ReplicationGroupDescription", "a cache of the account's VPC"),
        ]),
        false => params(&[("CacheClusterId", row.id)]),
    };
    let mut pairs = identity;
    pairs.extend(params(&[("CacheNodeType", NODE_TYPE)]));
    pairs.extend(params(row.extra));
    if row.placed {
        pairs.extend(params(&[("CacheSubnetGroupName", SUBNET_GROUP)]));
    }
    let created = emulator.call(action, &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: {action} must answer 200, got {} {}",
        row.case, created.status, created.body
    );
    let read = match row.group {
        true => params(&[("ReplicationGroupId", row.id)]),
        false => params(&[
            ("CacheClusterId", row.id),
            ("ShowCacheNodeInfo", "true"),
        ]),
    };
    poll_until(
        emulator,
        REGION,
        row.case,
        "the cache reaching available",
        report,
        &read,
        |reply| reply.text(status_path) == Some("available"),
    )
    .await
}

/// The member cache clusters of `row`'s cache, in the order the control plane reports them.
fn members_of(row: &Row, reply: &Reply) -> Vec<String> {
    match row.group {
        true => reply
            .all("NodeGroupMember/CacheClusterId")
            .into_iter()
            .map(str::to_string)
            .collect(),
        false => vec![row.id.to_string()],
    }
}

/// The private addresses the interfaces Amazon ElastiCache placed in `vpc` hold, keyed by the
/// description each carries — `ElastiCache <cluster>-<node>`, which is what says WHICH node an
/// interface belongs to.
///
/// Asked of Amazon EC2, the service that owns network interfaces, through its own
/// `DescribeNetworkInterfaces`.
async fn cache_interface_addresses(emulator: &Emulator, vpc: &str) -> BTreeMap<String, String> {
    let in_vpc = ::aws_sdk_ec2::types::Filter::builder()
        .name("vpc-id")
        .values(vpc)
        .build();
    let placed_by_elasticache = ::aws_sdk_ec2::types::Filter::builder()
        .name("requester-id")
        .values(CACHE_INTERFACE_REQUESTER)
        .build();
    let described = emulator
        .ec2(REGION)
        .describe_network_interfaces()
        .filters(in_vpc)
        .filters(placed_by_elasticache)
        .send()
        .await
        .expect("EC2 must report the account's network interfaces");
    described
        .network_interfaces()
        .iter()
        .filter_map(|interface| {
            Some((
                interface.description()?.to_string(),
                interface.private_ip_address()?.to_string(),
            ))
        })
        .collect()
}

#[tokio::test]
async fn every_node_publishes_an_address_of_its_own_in_its_vpc() {
    let emulator = Emulator::new();
    let subnets = account_subnet_ids(&emulator).await;
    let vpc = emulator
        .account_subnets()
        .await
        .first()
        .map(|subnet| subnet.vpc.clone())
        .expect("the account must have a subnet to place a cache in");
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(
                SUBNET_GROUP,
                "the subnets the caches of this suite sit in",
                &subnets[..2],
            ),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "setup: the cache subnet group must be created, got {} {}",
        created.status, created.body
    );

    // Every address a PLACED node published, so no two nodes of the account share one.
    let mut published_by: BTreeMap<String, String> = BTreeMap::new();
    for row in ROWS {
        let described = available_cache(&emulator, row).await;
        let members = members_of(row, &described);
        assert_eq!(
            members.len(),
            row.nodes,
            "case {:?}: the cache must report {} member node(s), got {members:?}",
            row.case,
            row.nodes
        );

        let placed = cache_interface_addresses(&emulator, &vpc).await;
        let mut addresses: BTreeSet<String> = BTreeSet::new();
        for member in &members {
            let (address, port) = node_endpoint(&emulator, row.case, member).await;
            if row.placed {
                // The node really sits in the account's network: it holds one interface of
                // its own in a subnet of the group it was placed in, which is the presence
                // `ec2:DescribeNetworkInterfaces` reports for a cache.
                assert!(
                    placed.contains_key(&interface_description(member, FIRST_NODE)),
                    "case {:?}: {member} must hold an interface in {vpc}, got {placed:?}",
                    row.case
                );
                // And it is published at an address of its OWN on the interface this
                // emulator's listeners are bound on — a loopback address, which is the one
                // interface every spelling of names.
                let parsed: ::std::net::IpAddr = address.parse().unwrap_or_else(|error| {
                    panic!(
                        "case {:?}: {member} must publish an address, got {address:?}: {error}",
                        row.case
                    )
                });
                assert!(
                    parsed.is_loopback(),
                    "case {:?}: {member} must be published on the interface this emulator's \
                     listeners are bound on, got {address:?}",
                    row.case
                );
            } else {
                // A cache in no VPC of the account keeps the address its own socket answers
                // at, which is this machine's own loopback address.
                assert_eq!(
                    address, "127.0.0.1",
                    "case {:?}: a cache in no VPC must publish the address its socket answers \
                     at, got {address:?}",
                    row.case
                );
            }
            assert!(
                addresses.insert(address.clone()),
                "case {:?}: {member} must be reached at an address of its own, but {address} \
                 is already published by another node of this cache",
                row.case
            );
            if row.placed {
                let owner = published_by.insert(address.clone(), member.clone());
                assert!(
                    owner.is_none(),
                    "case {:?}: {address} is published by {member} and by {owner:?}",
                    row.case
                );
            }
            // And the published endpoint is one a client really reaches this node at: a cache
            // of the VPC through the register of served planes, as code inside that VPC does,
            // and a cache of no VPC over a socket of this machine.
            let mut session = match row.placed {
                true => open_in_vpc(&emulator, row.case, &address, &port).await,
                false => open(row.case, &address, &port).await,
            };
            let answered = command(row.case, &mut session, &["PING"]).await;
            assert_eq!(
                answered,
                Resp::Status("PONG".to_string()),
                "case {:?}: {member} must answer at the endpoint it published, {address}:{port}",
                row.case
            );
        }
        assert_eq!(
            addresses.len(),
            row.nodes,
            "case {:?}: the cache's {} nodes must resolve to {} distinct addresses, got \
             {addresses:?}",
            row.case,
            row.nodes,
            row.nodes
        );
    }
}

#[tokio::test]
async fn deleting_a_replication_group_gives_its_nodes_interfaces_back() {
    let emulator = Emulator::new();
    let subnets = account_subnet_ids(&emulator).await;
    let vpc = emulator
        .account_subnets()
        .await
        .first()
        .map(|subnet| subnet.vpc.clone())
        .expect("the account must have a subnet to place a cache in");
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params("teardown-subnets", "the subnets the group sits in", &subnets[..2]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "setup: the cache subnet group must be created, got {} {}",
        created.status, created.body
    );

    // The rows differ along the shape of the group whose interfaces are given back: a
    // partitioned group with a node per shard, and an unpartitioned one with a replica.
    let rows: &[Row] = &[
        Row {
            case: "a partitioned group",
            id: "teardown-shards",
            group: true,
            extra: &[
                ("Engine", "redis"),
                ("ClusterMode", "enabled"),
                ("NumNodeGroups", "2"),
                ("ReplicasPerNodeGroup", "0"),
            ],
            placed: true,
            nodes: 2,
        },
        Row {
            case: "a group with a replica",
            id: "teardown-replica",
            group: true,
            extra: &[("Engine", "redis"), ("NumCacheClusters", "2")],
            placed: true,
            nodes: 2,
        },
    ];

    for row in rows {
        let mut pairs = params(&[
            ("ReplicationGroupId", row.id),
            ("ReplicationGroupDescription", "a group of the account's VPC"),
            ("CacheNodeType", NODE_TYPE),
            ("CacheSubnetGroupName", "teardown-subnets"),
        ]);
        pairs.extend(params(row.extra));
        let created = emulator.call("CreateReplicationGroup", &pairs).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the create must answer 200, got {} {}",
            row.case, created.status, created.body
        );
        let described = poll_until(
            &emulator,
            REGION,
            row.case,
            "the group reaching available",
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", row.id)]),
            |reply| reply.text("ReplicationGroup/Status") == Some("available"),
        )
        .await;
        let members = members_of(row, &described);
        let held = cache_interface_addresses(&emulator, &vpc).await;
        for member in &members {
            assert!(
                held.contains_key(&interface_description(member, FIRST_NODE)),
                "case {:?}: {member} must hold an interface in {vpc} while the group is there, \
                 got {held:?}",
                row.case
            );
        }

        // The delete BEGINS the teardown — the group answers `deleting` — and the presence its
        // nodes had in the account's network is given back with that answer, so nothing of the
        // account's network is still held by a cache that is going away.
        let deleted = emulator
            .call(
                "DeleteReplicationGroup",
                &params(&[("ReplicationGroupId", row.id)]),
            )
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {:?}: the delete must answer 200, got {} {}",
            row.case, deleted.status, deleted.body
        );
        assert_eq!(
            deleted.text("ReplicationGroup/Status"),
            Some("deleting"),
            "case {:?}: the delete must answer the group deleting, got {}",
            row.case,
            deleted.body
        );
        let left = cache_interface_addresses(&emulator, &vpc).await;
        for member in &members {
            assert!(
                !left.contains_key(&interface_description(member, FIRST_NODE)),
                "case {:?}: {member} must hold no interface once the group is deleted, got \
                 {left:?}",
                row.case
            );
        }
    }

    // And the consequence a client depends on: with the interfaces back, the network the group
    // sat in can be taken down again. A subnet "you must terminate all running instances in
    // the subnet before you can delete" cannot be deleted while a cache node's interface is in
    // it, so a cache that kept one would hold the account's subnet for ever.
    let own = emulator
        .ec2(REGION)
        .create_vpc()
        .cidr_block("10.84.0.0/16")
        .send()
        .await
        .expect("EC2 must create the VPC")
        .vpc
        .and_then(|vpc| vpc.vpc_id)
        .expect("CreateVpc must report the id it minted");
    let own_subnet = emulator
        .ec2(REGION)
        .create_subnet()
        .vpc_id(&own)
        .cidr_block("10.84.1.0/24")
        .availability_zone(format!("{REGION}a"))
        .send()
        .await
        .expect("EC2 must create the subnet")
        .subnet
        .and_then(|subnet| subnet.subnet_id)
        .expect("CreateSubnet must report the id it minted");
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(
                "teardown-own-subnets",
                "a subnet of this suite's own VPC",
                std::slice::from_ref(&own_subnet),
            ),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "setup: the cache subnet group over {own_subnet} must be created, got {} {}",
        created.status, created.body
    );
    let created = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "teardown-own-vpc"),
                ("ReplicationGroupDescription", "a group of this suite's own VPC"),
                ("Engine", "redis"),
                ("CacheNodeType", NODE_TYPE),
                ("CacheSubnetGroupName", "teardown-own-subnets"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "setup: the group in {own_subnet} must be created, got {} {}",
        created.status, created.body
    );
    poll_until(
        &emulator,
        REGION,
        "the group of this suite's own VPC",
        "the group reaching available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", "teardown-own-vpc")]),
        |reply| reply.text("ReplicationGroup/Status") == Some("available"),
    )
    .await;
    let blocked = emulator
        .ec2(REGION)
        .delete_subnet()
        .subnet_id(&own_subnet)
        .send()
        .await;
    assert!(
        blocked.is_err(),
        "a subnet holding a cache node's interface must not be deletable, got {blocked:?}"
    );
    let deleted = emulator
        .call(
            "DeleteReplicationGroup",
            &params(&[("ReplicationGroupId", "teardown-own-vpc")]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "the delete must answer 200, got {} {}",
        deleted.status, deleted.body
    );
    let freed = emulator
        .ec2(REGION)
        .delete_subnet()
        .subnet_id(&own_subnet)
        .send()
        .await;
    assert!(
        freed.is_ok(),
        "the subnet must be deletable once the cache that sat in it is deleted, got {freed:?}"
    );
    let gone = emulator.ec2(REGION).delete_vpc().vpc_id(&own).send().await;
    assert!(
        gone.is_ok(),
        "the VPC must be deletable once its subnets are gone, got {gone:?}"
    );
}
