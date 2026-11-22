// SPDX-License-Identifier: CC0-1.0

//! The PARTITION of a cluster mode enabled replication group: one address per shard node,
//! and a keyspace divided across those shards.
//!
//! The rule under test: a cluster mode enabled group serves each shard's slots
//! (`NodeGroup$Slots`, "the keyspace for this node group (shard)") only on that shard's own
//! nodes, and each node is reached at an endpoint of its OWN — the address and port its own
//! socket answers at, so no two nodes of a group share one. So a cluster-aware client that
//! reads `CLUSTER SLOTS` off any node of the group is handed the same partition the control
//! plane reports, naming those same per-node endpoints; a command whose key belongs to
//! another shard is answered the engine's own `-MOVED` naming the node that owns the slot;
//! and a command whose keys straddle the partition is refused `-CROSSSLOT` rather than served
//! in part.
//!
//! Every assertion goes over a wire: the control-plane reads are `awsQuery` requests through
//! the production router, and every RESP session is opened to the endpoint the control plane
//! published, through the register of the data planes this emulator serves — which is how a
//! client INSIDE the cache's VPC reaches one, and the only vantage point a VPC-only cache
//! endpoint is reachable from.
//!
//! The tables span what the rule generalises over: the group's SHAPE (how many shards, how
//! many replicas per shard), and the KEYS a command names (plain keys that spread across the
//! partition, keys sharing one hash tag, a key on the node that owns it and the same key on
//! a node that does not, one key and several).

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use service_common::dialable_data_planes::DialablePlanes;

use crate::cache_cluster_lifecycle::{account_subnet_ids, subnet_group_params};
use crate::cache_data_plane::{command, Resp, Session};
use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// How many hash slots a Valkey / Redis OSS keyspace is divided into, which the partition of
/// a group's shards covers exactly — the service's own figure, so this suite cannot drift
/// from the keyspace size the handlers divide.
const KEYSPACE_SLOTS: u32 = state::KEYSPACE_SLOTS as u32;

/// The node type every group in this suite runs on.
const NODE_TYPE: &str = "cache.t3.micro";

/// One shard of a group, as the control plane reports it.
#[derive(Debug, Clone)]
struct Shard {
    node_group_id: String,
    /// `NodeGroup$Slots`, written `startkey-endkey`.
    slots: String,
    /// The member clusters of the shard, in the order the group reports them.
    members: Vec<String>,
}

impl Shard {
    /// The first and last slot `Slots` names.
    fn range(&self) -> (u32, u32) {
        let (start, end) = self
            .slots
            .split_once('-')
            .unwrap_or_else(|| panic!("a shard's Slots must be written startkey-endkey, got {:?}", self.slots));
        (
            start.parse().expect("the first slot is a number"),
            end.parse().expect("the last slot is a number"),
        )
    }
}

/// Create a cache subnet group over the account's subnets, so the group's nodes are placed
/// in the account's VPC and hold an interface there.
async fn subnet_group(emulator: &Emulator, name: &str) -> Vec<String> {
    let subnets = account_subnet_ids(emulator).await;
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(name, "the subnets a partitioned group sits in", &subnets[..2]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the cache subnet group must be created, got {} {}",
        created.status, created.body
    );
    subnets
}

/// Create one cluster mode enabled replication group and look again until it is `available`.
async fn available_group(
    emulator: &Emulator,
    case: &str,
    id: &str,
    subnet_group_name: &str,
    extra: &[(&str, &str)],
) -> Reply {
    let mut pairs = params(&[
        ("ReplicationGroupId", id),
        ("ReplicationGroupDescription", "a partitioned group"),
        ("Engine", "redis"),
        ("CacheNodeType", NODE_TYPE),
        ("ClusterMode", "enabled"),
        ("CacheSubnetGroupName", subnet_group_name),
    ]);
    pairs.extend(params(extra));
    let created = emulator.call("CreateReplicationGroup", &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the create must answer 200, got {} {}",
        created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        case,
        "the group reaching available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", id)]),
        |reply| reply.text("ReplicationGroup/Status") == Some("available"),
    )
    .await
}

/// The shards of the group `reply` reports, in document order.
fn shards_of(reply: &Reply) -> Vec<Shard> {
    let mut shards: Vec<Shard> = Vec::new();
    for (path, text) in &reply.nodes {
        if path.ends_with("/NodeGroup/NodeGroupId") {
            shards.push(Shard {
                node_group_id: text.clone(),
                slots: String::new(),
                members: Vec::new(),
            });
        } else if path.ends_with("/NodeGroup/Slots") {
            if let Some(shard) = shards.last_mut() {
                shard.slots = text.clone();
            }
        } else if path.ends_with("/NodeGroupMember/CacheClusterId") {
            if let Some(shard) = shards.last_mut() {
                shard.members.push(text.clone());
            }
        }
    }
    shards
}

/// The node endpoint the member cluster `id` publishes: `(address, port)`.
async fn node_endpoint(emulator: &Emulator, case: &str, id: &str) -> (String, u16) {
    let described = emulator
        .call(
            "DescribeCacheClusters",
            &params(&[("CacheClusterId", id), ("ShowCacheNodeInfo", "true")]),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: {id} must be a cluster of this account, got {} {}",
        described.status, described.body
    );
    let address = described
        .text("Endpoint/Address")
        .unwrap_or_else(|| {
            panic!("case {case:?}: {id} must publish a node endpoint, got {}", described.body)
        })
        .to_string();
    let port: u16 = described
        .text("Endpoint/Port")
        .unwrap_or_default()
        .parse()
        .unwrap_or_else(|_| {
            panic!("case {case:?}: {id} must publish a port, got {}", described.body)
        });
    (address, port)
}

/// Open a RESP session on the endpoint the control plane published, as a client INSIDE the
/// cache's VPC reaches it: through the register of the data planes this emulator serves.
async fn session_in_vpc(emulator: &Emulator, case: &str, address: &str, port: u16) -> Session {
    let planes = DialablePlanes::of(&**emulator.engine())
        .expect("the engine vends the register of served data planes");
    let dialed = planes.dialable(address, port).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the published endpoint {address}:{port} must be an address this \
             emulator serves a cache at"
        )
    });
    let conn = dialed
        .plane
        .dial(None)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: {address}:{port} must answer, got {error}"));
    Box::new(conn)
}

/// The `CLUSTER SLOTS` map one node answers: `(first slot, last slot, [(address, port)])`,
/// the shard's primary first.
async fn cluster_slots(case: &str, session: &mut Session) -> Vec<(u32, u32, Vec<(String, u16)>)> {
    let answered = command(case, session, &["CLUSTER", "SLOTS"]).await;
    let Resp::Array(entries) = answered else {
        panic!("case {case:?}: CLUSTER SLOTS must answer an array, got {answered:?}");
    };
    entries
        .into_iter()
        .map(|entry| {
            let Resp::Array(parts) = entry else {
                panic!("case {case:?}: each CLUSTER SLOTS entry must be an array, got {parts:?}",
                    parts = entry);
            };
            let first = match parts.first() {
                Some(Resp::Integer(slot)) => u32::try_from(*slot).unwrap_or_default(),
                other => panic!("case {case:?}: an entry must begin with its first slot, got {other:?}"),
            };
            let last = match parts.get(1) {
                Some(Resp::Integer(slot)) => u32::try_from(*slot).unwrap_or_default(),
                other => panic!("case {case:?}: an entry must carry its last slot, got {other:?}"),
            };
            let nodes = parts
                .iter()
                .skip(2)
                .map(|node| {
                    let Resp::Array(fields) = node else {
                        panic!("case {case:?}: each node must be an array, got {node:?}");
                    };
                    let address = match fields.first() {
                        Some(Resp::Bulk(address)) => address.clone(),
                        other => panic!("case {case:?}: a node must name its address, got {other:?}"),
                    };
                    let port = match fields.get(1) {
                        Some(Resp::Integer(port)) => u16::try_from(*port).unwrap_or_default(),
                        other => panic!("case {case:?}: a node must name its port, got {other:?}"),
                    };
                    // The engine's own node name, which a client keys its map by: 40 hex
                    // characters, distinct per node.
                    match fields.get(2) {
                        Some(Resp::Bulk(name)) => assert!(
                            name.len() == 40 && name.chars().all(|digit| digit.is_ascii_hexdigit()),
                            "case {case:?}: a node must carry the engine's own 40-hex name, got {name:?}"
                        ),
                        other => panic!("case {case:?}: a node must carry its name, got {other:?}"),
                    }
                    (address, port)
                })
                .collect();
            (first, last, nodes)
        })
        .collect()
}

/// The slot the cache itself says `key` belongs to (`CLUSTER KEYSLOT`), which is the one a
/// client's own hashing of it gives.
async fn key_slot(case: &str, session: &mut Session, key: &str) -> u32 {
    let answered = command(case, session, &["CLUSTER", "KEYSLOT", key]).await;
    match answered {
        Resp::Integer(slot) => u32::try_from(slot).unwrap_or_default(),
        other => panic!("case {case:?}: CLUSTER KEYSLOT {key} must answer a slot, got {other:?}"),
    }
}

/// One shape row: the case, the group id, the pairs that create it, and how many nodes the
/// group is made of.
type ShapeCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    usize,
);

#[tokio::test]
async fn every_node_of_a_partitioned_group_publishes_an_endpoint_of_its_own() {
    let emulator = Emulator::new();
    subnet_group(&emulator, "shard-subnets").await;
    // The axis: the group's shape — how many shards the keyspace is divided across and how
    // many nodes each shard holds. Distinct shards sharing one endpoint would be a single
    // backend wearing several names, which is the flat keyspace this rule exists against.
    let cases: [ShapeCase; 3] = [
        (
            "three shards, one node each",
            "shards-three-rg",
            &[("NumNodeGroups", "3"), ("ReplicasPerNodeGroup", "0")],
            3,
        ),
        (
            "two shards with a replica each",
            "shards-pair-rg",
            &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "1")],
            4,
        ),
        (
            "one shard, which is the whole keyspace",
            "shards-one-rg",
            &[("NumNodeGroups", "1"), ("ReplicasPerNodeGroup", "0")],
            1,
        ),
    ];

    for (case, id, extra, nodes) in cases {
        let described = available_group(&emulator, case, id, "shard-subnets", extra).await;
        let shards = shards_of(&described);
        let members: Vec<String> = shards
            .iter()
            .flat_map(|shard| shard.members.iter().cloned())
            .collect();
        assert_eq!(
            members.len(),
            nodes,
            "case {case:?}: the group must report {nodes} member nodes, got {members:?}"
        );

        let mut published: BTreeMap<(String, u16), String> = BTreeMap::new();
        for member in &members {
            let (address, port) = node_endpoint(&emulator, case, member).await;
            let endpoint = (address.clone(), port);
            assert!(
                published.insert(endpoint.clone(), member.clone()).is_none(),
                "case {case:?}: {member} must publish an endpoint of its own, but \
                 {address}:{port} is already published by {:?}",
                published.get(&endpoint)
            );
            // And the published endpoint is one a client inside the VPC really reaches this
            // node at: the session below is opened to exactly what was published.
            let mut session = session_in_vpc(&emulator, case, &address, port).await;
            let answered = command(case, &mut session, &["PING"]).await;
            assert_eq!(
                answered,
                Resp::Status("PONG".to_string()),
                "case {case:?}: {member} must answer at the endpoint it published, \
                 {address}:{port}"
            );
        }
        assert_eq!(
            published.len(),
            nodes,
            "case {case:?}: the {nodes} nodes must publish {nodes} distinct endpoints, got \
             {published:?}"
        );
    }
}

#[tokio::test]
async fn cluster_slots_reports_the_partition_the_control_plane_holds() {
    let emulator = Emulator::new();
    subnet_group(&emulator, "map-subnets").await;
    let described = available_group(
        &emulator,
        "the slot map",
        "map-rg",
        "map-subnets",
        &[("NumNodeGroups", "3"), ("ReplicasPerNodeGroup", "1")],
    )
    .await;
    let shards = shards_of(&described);
    assert_eq!(
        shards.len(),
        3,
        "the group must report three shards, got {shards:?}"
    );

    // The partition the control plane holds: every shard's `Slots`, which together cover the
    // whole keyspace exactly once.
    let mut covered: Vec<(u32, u32)> = shards.iter().map(Shard::range).collect();
    covered.sort_unstable();
    assert_eq!(
        covered.first().map(|range| range.0),
        Some(0),
        "the partition must start at slot 0, got {covered:?}"
    );
    assert_eq!(
        covered.last().map(|range| range.1),
        Some(KEYSPACE_SLOTS - 1),
        "the partition must end at the last slot, got {covered:?}"
    );
    for pair in covered.windows(2) {
        assert_eq!(
            pair[1].0,
            pair[0].1 + 1,
            "the shards must partition the keyspace with no gap and no overlap, got {covered:?}"
        );
    }

    // What every node of the group answers: the same map, naming the same addresses the
    // control plane published for those very nodes.
    let mut endpoints: BTreeMap<String, (String, u16)> = BTreeMap::new();
    for shard in &shards {
        for member in &shard.members {
            endpoints.insert(
                member.clone(),
                node_endpoint(&emulator, "the slot map", member).await,
            );
        }
    }
    for (member, (address, port)) in &endpoints {
        let case = format!("CLUSTER SLOTS on {member}");
        let mut session = session_in_vpc(&emulator, &case, address, *port).await;
        let map = cluster_slots(&case, &mut session).await;
        assert_eq!(
            map.len(),
            shards.len(),
            "case {case:?}: the map must have one entry per shard, got {map:?}"
        );
        for shard in &shards {
            let (start, end) = shard.range();
            let entry = map
                .iter()
                .find(|(first, last, _)| *first == start && *last == end)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the map must carry the shard {} serving {start}-{end}, \
                         got {map:?}",
                        shard.node_group_id
                    )
                });
            let expected: Vec<(String, u16)> = shard
                .members
                .iter()
                .map(|member| endpoints[member].clone())
                .collect();
            let mut named = entry.2.clone();
            named.sort();
            let mut wanted = expected.clone();
            wanted.sort();
            assert_eq!(
                named, wanted,
                "case {case:?}: the entry for {start}-{end} must name that shard's own nodes, \
                 got {named:?}, expected {wanted:?}"
            );
            // The PRIMARY is the first node of the entry, which is where a client sends a
            // write for that shard.
            let primary = endpoints[&shard.members[0]].clone();
            assert_eq!(
                entry.2.first(),
                Some(&primary),
                "case {case:?}: the entry for {start}-{end} must name its primary first, got {:?}",
                entry.2
            );
        }
    }
}

#[tokio::test]
async fn a_key_is_served_by_its_own_shard_and_redirected_everywhere_else() {
    let emulator = Emulator::new();
    subnet_group(&emulator, "routing-subnets").await;
    let described = available_group(
        &emulator,
        "slot routing",
        "routing-rg",
        "routing-subnets",
        &[("NumNodeGroups", "3"), ("ReplicasPerNodeGroup", "0")],
    )
    .await;
    let shards = shards_of(&described);
    let mut endpoints: Vec<(String, (String, u16))> = Vec::new();
    for shard in &shards {
        for member in &shard.members {
            endpoints.push((
                member.clone(),
                node_endpoint(&emulator, "slot routing", member).await,
            ));
        }
    }

    // The keys: plain keys that spread across the partition, and two sharing one hash tag,
    // which the engine hashes by the tag alone so a client can keep related keys together.
    let keys = [
        "alpha",
        "beta",
        "gamma",
        "user:1000",
        "{basket}items",
        "{basket}total",
    ];
    let mut slots: BTreeMap<&str, u32> = BTreeMap::new();
    let mut owners: BTreeMap<&str, usize> = BTreeMap::new();
    for key in keys {
        // The slot is asked of every node, because the hashing is the engine's and not one
        // node's: a client that routes by what one node told it must reach the same node
        // whichever it asked.
        let mut answered: BTreeSet<u32> = BTreeSet::new();
        for (member, (address, port)) in &endpoints {
            let case = format!("CLUSTER KEYSLOT {key} on {member}");
            let mut session = session_in_vpc(&emulator, &case, address, *port).await;
            answered.insert(key_slot(&case, &mut session, key).await);
        }
        assert_eq!(
            answered.len(),
            1,
            "case {key:?}: every node must answer the same slot, got {answered:?}"
        );
        let slot = *answered.iter().next().expect("one slot");
        assert!(
            slot < KEYSPACE_SLOTS,
            "case {key:?}: the slot must be inside the keyspace, got {slot}"
        );
        let owner = shards
            .iter()
            .position(|shard| {
                let (start, end) = shard.range();
                slot >= start && slot <= end
            })
            .unwrap_or_else(|| panic!("case {key:?}: slot {slot} must belong to a shard"));
        slots.insert(key, slot);
        owners.insert(key, owner);
    }
    // The hash tag is what decides the slot: two keys under one tag share it.
    assert_eq!(
        slots["{basket}items"], slots["{basket}total"],
        "keys sharing a hash tag must share a slot, got {slots:?}"
    );
    // And the plain keys really do spread: a partition that put every key on one shard would
    // make the redirect below unreachable.
    let spread: BTreeSet<usize> = keys
        .iter()
        .take(4)
        .map(|key| owners[*key])
        .collect();
    assert!(
        spread.len() > 1,
        "the plain keys must spread across the partition, got {owners:?} over {shards:?}"
    );

    for key in keys {
        let slot = slots[key];
        let owner = owners[key];
        let owning_member = &shards[owner].members[0];
        let (owner_address, owner_port) = endpoints
            .iter()
            .find(|(member, _)| member == owning_member)
            .map(|(_, endpoint)| endpoint.clone())
            .expect("the owning shard's primary publishes an endpoint");
        // On the node that owns the slot the command is SERVED, and read back.
        let case = format!("{key} on the shard that owns slot {slot}");
        let mut session = session_in_vpc(&emulator, &case, &owner_address, owner_port).await;
        assert_eq!(
            command(&case, &mut session, &["SET", key, "held"]).await,
            Resp::Status("OK".to_string()),
            "case {case:?}: the owning shard must serve the write"
        );
        assert_eq!(
            command(&case, &mut session, &["GET", key]).await,
            Resp::Bulk("held".to_string()),
            "case {case:?}: the owning shard must answer what it holds"
        );

        // On every other node it is REDIRECTED, naming the slot and the node that owns it.
        for (member, (address, port)) in &endpoints {
            if member == owning_member {
                continue;
            }
            let case = format!("{key} on {member}, which does not own slot {slot}");
            let mut session = session_in_vpc(&emulator, &case, address, *port).await;
            let expected = format!("MOVED {slot} {owner_address}:{owner_port}");
            for sent in [["GET", key, ""], ["SET", key, "elsewhere"]] {
                let arguments: Vec<&str> =
                    sent.iter().copied().filter(|part| !part.is_empty()).collect();
                let answered = command(&case, &mut session, &arguments).await;
                assert_eq!(
                    answered,
                    Resp::Error(expected.clone()),
                    "case {case:?}: {arguments:?} must be redirected to the owning node"
                );
            }
        }
    }

    // Nothing was written where a command was refused, and every key is held by the shard
    // that owns it and by no other: each shard's keyspace holds exactly the keys above whose
    // slot falls in its own range, which is what the partition IS.
    for (index, shard) in shards.iter().enumerate() {
        let member = &shard.members[0];
        let (address, port) = endpoints
            .iter()
            .find(|(held, _)| held == member)
            .map(|(_, endpoint)| endpoint.clone())
            .expect("the shard's primary publishes an endpoint");
        let case = format!("the keyspace of shard {}", shard.node_group_id);
        let mut session = session_in_vpc(&emulator, &case, &address, port).await;
        let wanted = i64::try_from(keys.iter().filter(|key| owners[**key] == index).count())
            .unwrap_or_default();
        assert_eq!(
            command(&case, &mut session, &["DBSIZE"]).await,
            Resp::Integer(wanted),
            "case {case:?}: the shard must hold exactly the keys whose slot it serves, of \
             {slots:?}"
        );
        // And the keys of another shard are not in it: `KEYS *` is the whole of what this
        // node holds, so a key another shard owns appearing here would be a shared keyspace
        // wearing two names.
        let held = command(&case, &mut session, &["KEYS", "*"]).await;
        let Resp::Array(entries) = held else {
            panic!("case {case:?}: KEYS * must answer an array, got {held:?}");
        };
        for entry in entries {
            let Resp::Bulk(key) = entry else {
                panic!("case {case:?}: each key must be a bulk string, got {entry:?}");
            };
            assert_eq!(
                owners.get(key.as_str()).copied(),
                Some(index),
                "case {case:?}: {key:?} is held by a shard that does not own its slot, of \
                 {slots:?}"
            );
        }
    }
}

/// One multi-key row: the case, the command, and whether the partition serves it.
type MultiKeyCase = (&'static str, &'static [&'static str], bool);

#[tokio::test]
async fn a_command_whose_keys_straddle_the_partition_is_refused() {
    let emulator = Emulator::new();
    subnet_group(&emulator, "crossslot-subnets").await;
    let described = available_group(
        &emulator,
        "cross-slot",
        "crossslot-rg",
        "crossslot-subnets",
        &[("NumNodeGroups", "3"), ("ReplicasPerNodeGroup", "0")],
    )
    .await;
    let shards = shards_of(&described);
    let primaries: Vec<String> = shards
        .iter()
        .map(|shard| shard.members[0].clone())
        .collect();

    // The axis: how many slots the command's keys belong to — several (refused, whatever the
    // command), one by sharing a hash tag (served), and one key only (served).
    let cases: [MultiKeyCase; 5] = [
        ("two keys in different slots", &["MGET", "alpha", "beta"], false),
        (
            "a write over two keys in different slots",
            &["MSET", "alpha", "one", "beta", "two"],
            false,
        ),
        ("three keys in different slots", &["DEL", "alpha", "beta", "gamma"], false),
        (
            "two keys sharing a hash tag",
            &["MSET", "{basket}items", "one", "{basket}total", "two"],
            true,
        ),
        ("one key only", &["MGET", "{basket}items"], true),
    ];

    for (case, arguments, served) in cases {
        // The refusal is the partition's and not one node's, so every primary is asked.
        for member in &primaries {
            let case = format!("{case} on {member}");
            let (address, port) = node_endpoint(&emulator, &case, member).await;
            let mut session = session_in_vpc(&emulator, &case, &address, port).await;
            let answered = command(&case, &mut session, arguments).await;
            match (served, &answered) {
                // A command the partition does serve is either answered or redirected to the
                // node that owns its one slot — never refused for straddling it.
                (true, Resp::Error(error)) => assert!(
                    error.starts_with("MOVED "),
                    "case {case:?}: {arguments:?} belongs to one slot, so it must be served or \
                     redirected, got {error:?}"
                ),
                (true, _) => {}
                (false, Resp::Error(error)) => assert_eq!(
                    error, "CROSSSLOT Keys in request don't hash to the same slot",
                    "case {case:?}: {arguments:?} must be refused for straddling the partition"
                ),
                (false, other) => panic!(
                    "case {case:?}: {arguments:?} must be refused for straddling the partition, \
                     got {other:?}"
                ),
            }
        }
    }
}
