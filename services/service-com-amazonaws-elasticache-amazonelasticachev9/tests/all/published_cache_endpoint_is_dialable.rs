// SPDX-License-Identifier: CC0-1.0

//! That the endpoint a cache publishes is an address a CONNECTION to it is accepted at — and
//! what a client dialling exactly that address is served.
//!
//! RULE: an endpoint a service publishes for a resource is an address the resource's own socket
//! accepts a connection at, so a client that dials the `host:port` the control plane published
//! — over an ordinary socket of the machine this emulator runs on, with nothing resolving,
//! rewriting or re-spelling it on the way — reaches that resource's own server. A socket bound
//! on ONE address accepts a connection addressed to that address and to no other, so a cache
//! whose socket the core bound on one address is published AT that address, carrying the port
//! of its own socket: what keeps one node's endpoint apart from another's is the port the core
//! assigned that node, not a second address of the interface that nothing would answer at.
//!
//! The rows span what the rule generalises over: WHICH kind of cache published the endpoint (a
//! cache cluster's node endpoint, a replication group's primary and its members' node
//! endpoints, a cluster-mode group's per-shard node endpoints, a serverless cache's endpoint),
//! whether the cache was PLACED in a VPC of the account (a cache subnet group over the
//! account's own subnets, guarded by a security group Amazon EC2 minted) or in no VPC at all,
//! and whether the wire is encrypted.
//!
//! The two flows at the end are the client sequences this rule exists for, each driven over the
//! published endpoint dialled as a client dials it: a cache cluster written to, snapshotted,
//! deleted and restored from that snapshot — the data surviving the round trip — and a user
//! withdrawn from the user group attached to a replication group, whose `AUTH` the running
//! engine then refuses until the group holds it again.
//!
//! The sibling suites assert the same endpoints reached through the register of the data planes
//! this emulator serves ([`crate::cache_data_plane`], [`crate::rbac_revocation_flow`],
//! [`crate::cache_data_consequences`]), which is how the code of a function attached to the
//! cache's VPC reaches one; this suite is about the OTHER client of the same address, the one
//! that opens a socket. The socket and the TLS handshake are this crate's own one client
//! ([`crate::cache_data_plane::open`] / [`crate::cache_data_plane::open_tls`]), which asserts
//! that the published host is the interface the core binds its listeners on and then dials the
//! port that was published — so a published endpoint no socket answers at fails there.

use std::collections::BTreeSet;

use crate::cache_cluster_lifecycle::{account_subnet_ids, subnet_group_params};
use crate::cache_data_plane::{
    command, endpoint_of, node_endpoint, open, open_tls, Resp, Session,
};
use crate::harness::{assert_own_endpoint, params, poll_until, Emulator, Reply, REGION};

/// The node type every cache in this suite runs on.
const NODE_TYPE: &str = "cache.t3.micro";

/// The cache subnet group the placed caches of this suite sit in.
const SUBNET_GROUP: &str = "dialable-subnets";

/// Which kind of cache a row creates, which decides the call that mints it, the read that
/// reports it and where that read carries its endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Cluster,
    Group,
    Serverless,
}

/// One row: the cache to stand up, and how many endpoints it has to publish.
struct Row {
    case: &'static str,
    kind: Kind,
    /// The `CacheClusterId` / `ReplicationGroupId` / `ServerlessCacheName` it is created with.
    id: &'static str,
    /// What the create sends beside the identity, the node type and the placement.
    extra: &'static [(&'static str, &'static str)],
    /// Whether the cache is placed in [`SUBNET_GROUP`], which puts it in the account's VPC.
    placed: bool,
    /// Whether a client of this cache speaks TLS from the first byte.
    tls: bool,
    /// How many nodes the cache runs, and so how many distinct endpoints it must publish.
    nodes: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "a cache cluster in the account's VPC",
        kind: Kind::Cluster,
        id: "dialable-cluster",
        extra: &[("Engine", "redis"), ("NumCacheNodes", "1")],
        placed: true,
        tls: false,
        nodes: 1,
    },
    Row {
        case: "a cache cluster in no VPC of the account",
        kind: Kind::Cluster,
        id: "dialable-vpc-less",
        extra: &[("Engine", "valkey"), ("NumCacheNodes", "1")],
        placed: false,
        tls: false,
        nodes: 1,
    },
    Row {
        case: "a replication group with a replica, in the account's VPC",
        kind: Kind::Group,
        id: "dialable-group",
        extra: &[("Engine", "redis"), ("NumCacheClusters", "2")],
        placed: true,
        tls: false,
        nodes: 2,
    },
    Row {
        case: "a replication group with in-transit encryption",
        kind: Kind::Group,
        id: "dialable-tls-group",
        extra: &[
            ("Engine", "redis"),
            ("NumCacheClusters", "1"),
            ("TransitEncryptionEnabled", "true"),
        ],
        placed: true,
        tls: true,
        nodes: 1,
    },
    Row {
        case: "a cluster-mode group of three shards",
        kind: Kind::Group,
        id: "dialable-shards",
        extra: &[
            ("Engine", "redis"),
            ("ClusterMode", "enabled"),
            ("NumNodeGroups", "3"),
            ("ReplicasPerNodeGroup", "0"),
        ],
        placed: true,
        tls: false,
        nodes: 3,
    },
    Row {
        case: "a serverless cache",
        kind: Kind::Serverless,
        id: "dialable-serverless",
        extra: &[("Engine", "valkey")],
        placed: false,
        tls: true,
        nodes: 1,
    },
];

/// Open a session on `address`:`port` the way a client of this machine opens one: a socket to
/// exactly what the control plane published, and the TLS handshake on top of it for a cache
/// whose wire is encrypted.
///
/// The dial is the assertion: this crate's own client ([`open`]) requires the published host to
/// be the interface the core binds its listeners on and then connects to the published port, so
/// an endpoint carrying an address nothing answers at — or a port nothing accepts a connection on
/// — fails there, which is the one thing a published endpoint must never be. `what` names the
/// member the endpoint was read from, so such a failure says WHICH endpoint was unreachable.
async fn dialed(case: &str, what: &str, address: &str, port: &str, tls: bool) -> Session {
    let dialing = format!("{case}: {what}");
    match tls {
        true => open_tls(&dialing, address, port).await,
        false => open(&dialing, address, port).await,
    }
}

/// The cache subnet group the placed rows sit in, over the account's own subnets.
async fn subnet_group(emulator: &Emulator) {
    let subnets = account_subnet_ids(emulator).await;
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
}

/// Create the cache of `row` and read it back until it reports itself settled.
async fn available_cache(emulator: &Emulator, row: &Row, security_group: &str) -> Reply {
    let (create, read, request, status_path) = match row.kind {
        Kind::Cluster => (
            "CreateCacheCluster",
            "DescribeCacheClusters",
            params(&[("CacheClusterId", row.id), ("ShowCacheNodeInfo", "true")]),
            "CacheCluster/CacheClusterStatus",
        ),
        Kind::Group => (
            "CreateReplicationGroup",
            "DescribeReplicationGroups",
            params(&[("ReplicationGroupId", row.id)]),
            "ReplicationGroup/Status",
        ),
        Kind::Serverless => (
            "CreateServerlessCache",
            "DescribeServerlessCaches",
            params(&[("ServerlessCacheName", row.id)]),
            "Status",
        ),
    };
    let mut pairs = match row.kind {
        Kind::Cluster => params(&[("CacheClusterId", row.id), ("CacheNodeType", NODE_TYPE)]),
        Kind::Group => params(&[
            ("ReplicationGroupId", row.id),
            ("ReplicationGroupDescription", "a cache of this suite"),
            ("CacheNodeType", NODE_TYPE),
        ]),
        Kind::Serverless => params(&[("ServerlessCacheName", row.id)]),
    };
    pairs.extend(params(row.extra));
    if row.placed {
        pairs.extend(params(&[
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("SecurityGroupIds.SecurityGroupId.1", security_group),
        ]));
    }
    let created = emulator.call(create, &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: {create} must answer 200, got {} {}",
        row.case, created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        row.case,
        "the cache reaching available",
        read,
        &request,
        |reply| reply.text(status_path) == Some("available"),
    )
    .await
}

/// Every endpoint `row`'s settled cache publishes: what each one is, and the `host:port` it
/// carries — read exactly where a client reads it.
///
/// Whatever the read reports is collected, so a member the projection publishes an endpoint for
/// is dialled whether it is a primary, a reader or a shard's node, and an endpoint a row did not
/// expect is asserted rather than skipped.
async fn published_endpoints(
    emulator: &Emulator,
    row: &Row,
    described: &Reply,
) -> Vec<(String, String, String)> {
    let mut endpoints: Vec<(String, String, String)> = Vec::new();
    // The endpoints the read itself carries, each under the member the model names it with.
    for member in [
        "Endpoint",
        "PrimaryEndpoint",
        "ReaderEndpoint",
        "ConfigurationEndpoint",
    ] {
        let addresses = described.all(&format!("{member}/Address"));
        let ports = described.all(&format!("{member}/Port"));
        for (index, (address, port)) in addresses.iter().zip(ports.iter()).enumerate() {
            endpoints.push((
                format!("{member}[{index}]"),
                (*address).to_string(),
                (*port).to_string(),
            ));
        }
    }
    // And the node endpoint each member cluster of the cache publishes for itself.
    let members: Vec<String> = match row.kind {
        Kind::Cluster => vec![row.id.to_string()],
        Kind::Group => described
            .all("NodeGroupMember/CacheClusterId")
            .into_iter()
            .map(str::to_string)
            .collect(),
        Kind::Serverless => Vec::new(),
    };
    for member in members {
        let (address, port) = node_endpoint(emulator, row.case, &member).await;
        endpoints.push((format!("the node endpoint of {member}"), address, port));
    }
    endpoints
}

#[tokio::test]
async fn every_endpoint_a_cache_publishes_is_dialled_as_published_and_answers_resp() {
    let emulator = Emulator::new();
    subnet_group(&emulator).await;
    let security_group = emulator.security_group_named("dialable-cache").await;

    for row in ROWS {
        let described = available_cache(&emulator, row, &security_group).await;
        let endpoints = published_endpoints(&emulator, row, &described).await;
        assert!(
            !endpoints.is_empty(),
            "case {:?}: the settled cache must publish an endpoint, got {}",
            row.case,
            described.body
        );

        // Every published endpoint is dialled as it was published, and what answers there is a
        // RESP server: the reply to `PING` is the engine's own `+PONG`.
        let mut distinct: BTreeSet<(String, String)> = BTreeSet::new();
        for (what, address, port) in &endpoints {
            assert_own_endpoint(row.case, what, address, Some(port));
            let mut session = dialed(row.case, what, address, port, row.tls).await;
            let answered = command(row.case, &mut session, &["PING"]).await;
            assert_eq!(
                answered,
                Resp::Status("PONG".to_string()),
                "case {:?}: {what}, published at {address}:{port}, must answer PING, got \
                 {answered:?}",
                row.case
            );
            if what.starts_with("the node endpoint") {
                distinct.insert((address.clone(), port.clone()));
            }
        }
        // And each NODE of the cache is reached at an endpoint of its own, so a group of N
        // nodes is N backends rather than one wearing N names.
        if row.kind == Kind::Group {
            assert_eq!(
                distinct.len(),
                row.nodes,
                "case {:?}: the cache's {} nodes must publish {} distinct endpoints, got \
                 {distinct:?}",
                row.case,
                row.nodes,
                row.nodes
            );
        }
    }
}

/// The values the snapshot flow below writes: the axis is the SHAPE of what a client stores,
/// because what has to survive the round trip is the bytes and not one recorded payload.
const STORED: &[(&str, &str)] = &[
    ("plain-text", "a value a client stored"),
    ("empty", ""),
    ("multi-byte", "zürich-αβγ"),
    ("digits", "40900"),
];

#[tokio::test]
async fn a_cluster_restored_from_a_snapshot_serves_what_the_source_held_at_the_endpoint_it_publishes(
) {
    let emulator = Emulator::new();
    subnet_group(&emulator).await;
    let security_group = emulator.security_group_named("restore-flow-cache").await;
    let placed = |id: &str, extra: &[(&str, &str)]| {
        let mut pairs = params(&[
            ("CacheClusterId", id),
            ("Engine", "redis"),
            ("CacheNodeType", NODE_TYPE),
            ("NumCacheNodes", "1"),
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("SecurityGroupIds.SecurityGroupId.1", security_group.as_str()),
        ]);
        pairs.extend(params(extra));
        pairs
    };

    // The source cluster, and the data a client writes to the endpoint it published.
    let created = emulator
        .call("CreateCacheCluster", &placed("restore-source", &[]))
        .await;
    assert_eq!(
        created.status, 200,
        "the source cluster must be created, got {} {}",
        created.status, created.body
    );
    cluster_available(&emulator, "restore-source").await;
    let (address, port) = node_endpoint(&emulator, "the source cluster", "restore-source").await;
    let mut source = dialed(
        "the source cluster",
        "CacheNodes[0].Endpoint",
        &address,
        &port,
        false,
    )
    .await;
    for (key, value) in STORED {
        let stored = command("the source cluster", &mut source, &["SET", key, value]).await;
        assert_eq!(
            stored,
            Resp::Status("OK".to_string()),
            "the source cluster must take {key:?}, got {stored:?}"
        );
    }

    // The snapshot of it, which settles `available` for the first read past its due instant.
    let snapped = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("CacheClusterId", "restore-source"),
                ("SnapshotName", "restore-snap"),
            ]),
        )
        .await;
    assert_eq!(
        snapped.status, 200,
        "CreateSnapshot must answer 200, got {} {}",
        snapped.status, snapped.body
    );
    let settled = poll_until(
        &emulator,
        REGION,
        "the snapshot",
        "the snapshot reaching available",
        "DescribeSnapshots",
        &params(&[("SnapshotName", "restore-snap")]),
        |reply| reply.text("Snapshot/SnapshotStatus") == Some("available"),
    )
    .await;
    assert_eq!(
        settled.text("Snapshot/SnapshotName"),
        Some("restore-snap"),
        "the snapshot must be reported under the name it was taken with, got {}",
        settled.body
    );

    // The source is torn down and stops resolving, exactly as a client tearing a cache down
    // sees it.
    let deleted = emulator
        .call(
            "DeleteCacheCluster",
            &params(&[("CacheClusterId", "restore-source")]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "DeleteCacheCluster must answer 200, got {} {}",
        deleted.status, deleted.body
    );
    let gone = poll_until(
        &emulator,
        REGION,
        "the deleted cluster",
        "the deleted cluster ceasing to resolve",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", "restore-source")]),
        |reply| reply.status == 404,
    )
    .await;
    assert_eq!(
        gone.error_code(),
        "CacheClusterNotFound",
        "a deleted cluster must answer the modelled not-found, got {} {}",
        gone.status,
        gone.body
    );

    // And the cluster restored from that snapshot serves what the source held, at the endpoint
    // IT publishes.
    let restored = emulator
        .call(
            "CreateCacheCluster",
            &placed("restore-target", &[("SnapshotName", "restore-snap")]),
        )
        .await;
    assert_eq!(
        restored.status, 200,
        "a create naming the snapshot must answer 200, got {} {}",
        restored.status, restored.body
    );
    cluster_available(&emulator, "restore-target").await;
    let (address, port) = node_endpoint(&emulator, "the restored cluster", "restore-target").await;
    let mut target = dialed(
        "the restored cluster",
        "CacheNodes[0].Endpoint",
        &address,
        &port,
        false,
    )
    .await;
    for (key, value) in STORED {
        let read = command("the restored cluster", &mut target, &["GET", key]).await;
        assert_eq!(
            read,
            Resp::Bulk((*value).to_string()),
            "the restored cluster must serve {key:?} as the snapshot captured it, got {read:?}"
        );
    }
    let never = command("the restored cluster", &mut target, &["GET", "never-written"]).await;
    assert_eq!(
        never,
        Resp::Nil,
        "a key the snapshot never held must be absent from the restored cluster, got {never:?}"
    );
}

/// Read one cache cluster back until the clock has made it `available` with its node reported.
async fn cluster_available(emulator: &Emulator, id: &str) -> Reply {
    poll_until(
        emulator,
        REGION,
        id,
        "the cluster reaching available with its node provisioned",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", id), ("ShowCacheNodeInfo", "true")]),
        |reply| reply.text("CacheCluster/CacheClusterStatus") == Some("available"),
    )
    .await
}

/// The members the revocation flow below withdraws and restores: the axis is the FORM of
/// credential the member authenticates with, because what decides the session is the group's
/// membership and not the secret.
const MEMBERS: &[(&str, &str, &str)] = &[
    (
        "a member with a password",
        "dialable-member",
        "a-url-safe_fixture-for-this-member",
    ),
    (
        "a member that needs no password",
        "dialable-open-member",
        "any-credential-at-all",
    ),
];

#[tokio::test]
async fn withdrawing_a_user_from_the_attached_group_refuses_its_auth_at_the_published_endpoint() {
    let emulator = Emulator::new();
    subnet_group(&emulator).await;
    let security_group = emulator.security_group_named("revocation-flow-cache").await;

    for (case, id, secret) in MEMBERS {
        let mut request = params(&[
            ("UserId", id),
            ("UserName", id),
            ("Engine", "redis"),
            ("AccessString", "on ~* +@all"),
        ]);
        match *case == MEMBERS[0].0 {
            true => request.extend(params(&[("Passwords.member.1", secret)])),
            false => request.extend(params(&[("NoPasswordRequired", "true")])),
        }
        let created = emulator.call("CreateUser", &request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateUser must answer 200, got {} {}",
            created.status, created.body
        );
    }
    let mut group = params(&[
        ("UserGroupId", "dialable-group-rbac"),
        ("Engine", "redis"),
        ("UserIds.member.1", "default"),
    ]);
    for (index, (_, id, _)) in MEMBERS.iter().enumerate() {
        group.push((format!("UserIds.member.{}", index + 2), (*id).to_string()));
    }
    let created = emulator.call("CreateUserGroup", &group).await;
    assert_eq!(
        created.status, 200,
        "CreateUserGroup must answer 200, got {} {}",
        created.status, created.body
    );

    let created = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "dialable-rbac-rg"),
                ("ReplicationGroupDescription", "the group access is revoked on"),
                ("Engine", "redis"),
                ("CacheNodeType", NODE_TYPE),
                ("NumCacheClusters", "1"),
                ("TransitEncryptionEnabled", "true"),
                ("CacheSubnetGroupName", SUBNET_GROUP),
                ("SecurityGroupIds.SecurityGroupId.1", &security_group),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "CreateReplicationGroup must answer 200, got {} {}",
        created.status, created.body
    );
    poll_until(
        &emulator,
        REGION,
        "the replication group",
        "the replication group reaching available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", "dialable-rbac-rg")]),
        |reply| reply.text("ReplicationGroup/Status") == Some("available"),
    )
    .await;
    let attached = emulator
        .call(
            "ModifyReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "dialable-rbac-rg"),
                ("UserGroupIdsToAdd.member.1", "dialable-group-rbac"),
                ("ApplyImmediately", "true"),
            ]),
        )
        .await;
    assert_eq!(
        attached.status, 200,
        "ModifyReplicationGroup must answer 200, got {} {}",
        attached.status, attached.body
    );
    let described = poll_until(
        &emulator,
        REGION,
        "the attachment",
        "the replication group reaching available with the group attached",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", "dialable-rbac-rg")]),
        |reply| {
            reply.text("ReplicationGroup/Status") == Some("available")
                && reply
                    .all("UserGroupIds/member")
                    .contains(&"dialable-group-rbac")
        },
    )
    .await;
    let (address, port) = endpoint_of(
        &described,
        "NodeGroup/PrimaryEndpoint/Address",
        "NodeGroup/PrimaryEndpoint/Port",
        "the attached replication group",
    );
    assert_own_endpoint(
        "the attached replication group",
        "NodeGroups[0].PrimaryEndpoint",
        &address,
        Some(&port),
    );

    for (case, id, secret) in MEMBERS {
        // The positive control: the member authenticates on the published endpoint.
        let accepted = auth(&address, &port, case, id, secret).await;
        assert_eq!(
            accepted,
            Resp::Status("OK".to_string()),
            "case {case:?}: a member of the attached group must authenticate, got {accepted:?}"
        );

        // Withdrawn from the group, the IDENTICAL credential is refused by the engine.
        change_membership(&emulator, case, "UserIdsToRemove", id, false).await;
        let refused = auth(&address, &port, case, id, secret).await;
        assert!(
            matches!(&refused, Resp::Error(message) if message.starts_with("WRONGPASS")),
            "case {case:?}: a user the attached group no longer holds must authenticate \
             nobody, got {refused:?}"
        );

        // And restored to it, the same credential opens a session again.
        change_membership(&emulator, case, "UserIdsToAdd", id, true).await;
        let reaccepted = auth(&address, &port, case, id, secret).await;
        assert_eq!(
            reaccepted,
            Resp::Status("OK".to_string()),
            "case {case:?}: the identical credential must be accepted once the group holds \
             the member again, got {reaccepted:?}"
        );
    }
}

/// `AUTH <user> <secret>` on a fresh TLS session to the endpoint the control plane published.
async fn auth(address: &str, port: &str, case: &str, user: &str, secret: &str) -> Resp {
    let mut session = dialed(case, "NodeGroups[0].PrimaryEndpoint", address, port, true).await;
    command(case, &mut session, &["AUTH", user, secret]).await
}

/// Withdraw `user` from the flow's user group, or restore it, and wait until the group reads
/// back settled with the membership that change leaves — which is what a client polls for.
async fn change_membership(
    emulator: &Emulator,
    case: &str,
    member: &str,
    user: &str,
    present: bool,
) {
    let changed = emulator
        .call(
            "ModifyUserGroup",
            &params(&[
                ("UserGroupId", "dialable-group-rbac"),
                (&format!("{member}.member.1"), user),
            ]),
        )
        .await;
    assert_eq!(
        changed.status, 200,
        "case {case:?}: ModifyUserGroup({member}) must answer 200, got {} {}",
        changed.status, changed.body
    );
    let user = user.to_string();
    poll_until(
        emulator,
        REGION,
        case,
        "the user group reaching active with the membership the change leaves",
        "DescribeUserGroups",
        &params(&[("UserGroupId", "dialable-group-rbac")]),
        move |reply| {
            reply.text("member/Status") == Some("active")
                && reply.all("UserIds/member").contains(&user.as_str()) == present
        },
    )
    .await;
    poll_until(
        emulator,
        REGION,
        case,
        "the attached replication group reading available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", "dialable-rbac-rg")]),
        |reply| reply.text("ReplicationGroup/Status") == Some("available"),
    )
    .await;
}
