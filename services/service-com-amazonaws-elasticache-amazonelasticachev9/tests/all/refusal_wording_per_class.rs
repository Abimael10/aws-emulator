// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE each of these refusal classes carries, which is the whole difference a
//! client matching on message text sees.
//!
//! Every assertion goes over the wire through the shared [`crate::harness`]: an `awsQuery`
//! form-urlencoded request is dispatched through the production router and only the HTTP
//! status and the XML body are inspected, so nothing here can pass by reaching into the
//! service's state.
//!
//! The rules under test, each over a SET of inputs rather than one request:
//!
//! 1. ENGINE — a value that names no cache engine is refused `InvalidParameterValue` with
//!    `Invalid Cache engine: <value>.`, the value as the request spelled it, wherever an
//!    operation takes an `Engine` for a cache to RUN.
//! 2. NODE COUNT — a `NumCacheNodes` below the floor every cluster has is refused with the
//!    floor sentence, whatever engine the cluster runs; a count above an engine's own
//!    ceiling is the other refusal and keeps the ceiling's own sentence.
//! 3. SNAPSHOTS — `SnapshotFeatureNotSupportedFault` for a cache whose ENGINE has no
//!    snapshots carries the engine sentence, with nothing from the request in it, while the
//!    node type that supports none keeps its own; `InvalidCacheClusterStateFault` for a
//!    snapshot of a cluster that is not `available` states the status such a call ADMITS,
//!    not the one the cluster carries.
//! 4. TAKEN NAMES — a `CopySnapshot` whose destination name is taken names it as the
//!    DESTINATION, which is a different sentence from the one a snapshot being TAKEN under a
//!    taken name carries.
//! 5. SUBNET GROUP — each operation that resolves a `CacheSubnetGroupName` words a name it
//!    cannot resolve its own way, over the name's stored identity, and the three that answer
//!    `Cache Subnet Group <name> does not exist.` go through one template.
//! 6. IDENTIFIERS — the lead-in of the identifier-grammar refusal is per MEMBER: the members
//!    AWS names by its own parameter label carry `The parameter <label> is not a valid
//!    identifier. …`, and the members it names in prose keep `<prose> specified is not a
//!    valid identifier. …`.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::cache_cluster_lifecycle::{create_available_cluster, create_cluster};
use crate::harness::{assert_refusal, params, poll_until, Emulator, REGION};

/// The grammar sentence every identifier refusal of this service ends with, whichever
/// lead-in it carries.
const GRAMMAR: &str = "Identifiers must begin with a letter; must contain only ASCII letters, \
                       digits, and hyphens; and must not end with a hyphen or contain two \
                       consecutive hyphens.";

/// Take a snapshot of `cluster` under `name` and look again until the clock has carried it
/// to `available`, which is the only state a copy may be made from.
async fn available_snapshot(emulator: &Emulator, case: &str, name: &str, cluster: &str) {
    let created = emulator
        .call(
            "CreateSnapshot",
            &params(&[("SnapshotName", name), ("CacheClusterId", cluster)]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the snapshot {name} must be taken, got {} {}",
        created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        case,
        &format!("the snapshot {name} reporting available"),
        "DescribeSnapshots",
        &params(&[("SnapshotName", name)]),
        |reply| reply.text("Snapshot/SnapshotStatus") == Some("available"),
    )
    .await;
}

/// An `Engine` naming no cache engine is refused with the service's own sentence for it, at
/// every operation that takes one for a cache to run — and the engines it DOES run are
/// accepted whatever case they are spelled in, so the refusal is about the vocabulary.
#[tokio::test]
async fn an_unknown_engine_is_refused_with_the_engine_sentence() {
    let emulator = Emulator::new();
    create_available_cluster(&emulator, "engine-modify", &[("Engine", "redis")]).await;

    // The axis: how the value misses the vocabulary — another product's engine, a name with
    // a version glued on, a near-miss spelling, a name of the right family in the wrong
    // form, and one carrying a character no engine name has.
    for value in [
        "not-an-engine",
        "mysql",
        "redis-7",
        "memcache",
        "valkeyy",
        "Redis OSS",
    ] {
        let expected = format!("Invalid Cache engine: {value}.");
        let created = create_cluster(
            &emulator,
            "engine-create",
            &[
                ("Engine", value),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheNodes", "1"),
            ],
        )
        .await;
        assert_refusal(
            &format!("CreateCacheCluster with {value:?}"),
            &created,
            400,
            "InvalidParameterValue",
            &expected,
        );

        let modified = emulator
            .call(
                "ModifyCacheCluster",
                &params(&[
                    ("CacheClusterId", "engine-modify"),
                    ("Engine", value),
                    ("ApplyImmediately", "true"),
                ]),
            )
            .await;
        assert_refusal(
            &format!("ModifyCacheCluster with {value:?}"),
            &modified,
            400,
            "InvalidParameterValue",
            &expected,
        );
    }

    // The accepting half: every engine this service runs, in a case a request may spell it
    // in, is served — so the rows above are refused for naming no engine.
    for (index, engine) in ["memcached", "REDIS", "Valkey"].iter().enumerate() {
        let created = create_cluster(
            &emulator,
            &format!("engine-ok-{index}"),
            &[("Engine", engine), ("CacheNodeType", "cache.t3.micro")],
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {engine:?}: an engine this service runs must be accepted, got {} {}",
            created.status, created.body
        );
    }
}

/// A `NumCacheNodes` below the floor every cluster has is refused with the floor sentence,
/// whatever engine the cluster runs and whether the count arrives on a create or a modify;
/// a count above an engine's ceiling is the OTHER refusal.
#[tokio::test]
async fn a_node_count_below_the_floor_states_the_floor() {
    let emulator = Emulator::new();
    create_available_cluster(
        &emulator,
        "floor-memcached",
        &[("Engine", "memcached"), ("NumCacheNodes", "2")],
    )
    .await;

    // The axis: how far below the floor the count is — the first value below it, the
    // smallest negative, and the wire type's own floor.
    for count in ["0", "-1", "-2147483648"] {
        for engine in ["memcached", "redis", "valkey"] {
            let created = create_cluster(
                &emulator,
                "floor-create",
                &[
                    ("Engine", engine),
                    ("CacheNodeType", "cache.t3.micro"),
                    ("NumCacheNodes", count),
                ],
            )
            .await;
            assert_refusal(
                &format!("CreateCacheCluster {engine} with {count}"),
                &created,
                400,
                "InvalidParameterValue",
                state::NUM_CACHE_NODES_FLOOR_MESSAGE,
            );
        }

        // A modify that would leave the cluster with that count is the same refusal, with
        // or without the change being applied straight away.
        for apply in ["true", "false"] {
            let modified = emulator
                .call(
                    "ModifyCacheCluster",
                    &params(&[
                        ("CacheClusterId", "floor-memcached"),
                        ("NumCacheNodes", count),
                        ("ApplyImmediately", apply),
                    ]),
                )
                .await;
            assert_refusal(
                &format!("ModifyCacheCluster with {count} / ApplyImmediately={apply}"),
                &modified,
                400,
                "InvalidParameterValue",
                state::NUM_CACHE_NODES_FLOOR_MESSAGE,
            );
        }
    }

    // The contrast: a count past what ONE CLUSTER may hold is refused as the quota it is,
    // which is a different class of refusal — so the floor sentence is the floor's alone.
    // A Memcached count merely past the adjustable figure the member's prose names is
    // SERVED, because that figure is a quota and not a bound the API validates against.
    let past_the_quota = (state::MAX_CACHE_NODES_PER_CLUSTER + 1).to_string();
    for (case, engine, count, code) in [
        (
            "one past what a cluster may hold",
            "memcached",
            past_the_quota.as_str(),
            "NodeQuotaForClusterExceeded",
        ),
        (
            "the wire type's ceiling",
            "memcached",
            "2147483647",
            "NodeQuotaForClusterExceeded",
        ),
        (
            "more than one node on redis",
            "redis",
            "2",
            "InvalidParameterValue",
        ),
    ] {
        let created = create_cluster(
            &emulator,
            "ceiling-create",
            &[
                ("Engine", engine),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheNodes", count),
            ],
        )
        .await;
        assert_eq!(
            (created.status, created.error_code()),
            (400, code),
            "case {case:?}: expected 400 {code}, got {} {}",
            created.status,
            created.body
        );
        assert_ne!(
            created.error_message(),
            state::NUM_CACHE_NODES_FLOOR_MESSAGE,
            "case {case:?}: a count above the floor must not state the floor, got {:?}",
            created.error_message()
        );
    }

    // The accepting half: the floor itself is a legal count for every engine, and a
    // Memcached cluster past the figure the prose names is provisioned with the count it
    // asked for.
    for (index, engine) in ["memcached", "redis", "valkey"].iter().enumerate() {
        let created = create_cluster(
            &emulator,
            &format!("floor-ok-{index}"),
            &[
                ("Engine", engine),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheNodes", "1"),
            ],
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {engine:?}: one cache node must be accepted, got {} {}",
            created.status, created.body
        );
    }
    for count in ["40", "41", "50"] {
        let created = create_cluster(
            &emulator,
            &format!("quota-ok-{count}"),
            &[
                ("Engine", "memcached"),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheNodes", count),
            ],
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {count}: a Memcached cluster within the quota must be accepted, got {} {}",
            created.status, created.body
        );
        assert_eq!(
            created.text("CacheCluster/NumCacheNodes"),
            Some(count),
            "case {count}: the stored count is what was asked for, got {}",
            created.body
        );
    }
}

/// The two gates of `SnapshotFeatureNotSupportedFault` and the state gate of
/// `CreateSnapshot`: the engine gate and the state gate each carry a sentence with nothing
/// from the request in it, and the node-type gate keeps its own.
#[tokio::test]
async fn a_snapshot_that_cannot_be_taken_states_its_own_reason() {
    let emulator = Emulator::new();
    create_available_cluster(&emulator, "snap-redis", &[("Engine", "redis")]).await;
    create_available_cluster(
        &emulator,
        "snap-memcached-1",
        &[("Engine", "memcached"), ("NumCacheNodes", "1")],
    )
    .await;
    create_available_cluster(
        &emulator,
        "snap-memcached-3",
        &[("Engine", "memcached"), ("NumCacheNodes", "3")],
    )
    .await;

    // The ENGINE gate: whatever the cluster's node count or id, the refusal is the engine's
    // own sentence and names neither.
    for cluster in ["snap-memcached-1", "snap-memcached-3"] {
        let refused = emulator
            .call(
                "CreateSnapshot",
                &params(&[("SnapshotName", "never-taken"), ("CacheClusterId", cluster)]),
            )
            .await;
        assert_refusal(
            &format!("CreateSnapshot of {cluster}"),
            &refused,
            400,
            "SnapshotFeatureNotSupportedFault",
            state::SNAPSHOT_ENGINE_UNSUPPORTED_MESSAGE,
        );
        assert!(
            !refused.error_message().contains(cluster),
            "case {cluster}: the sentence carries no part of the request, got {:?}",
            refused.error_message()
        );
    }

    // The contrast: the node type that supports no snapshot is the same fault with its own
    // sentence, so the engine sentence belongs to the engine gate.
    create_available_cluster(
        &emulator,
        "snap-tiny",
        &[("Engine", "redis"), ("CacheNodeType", "cache.t1.micro")],
    )
    .await;
    let tiny = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("SnapshotName", "never-taken"),
                ("CacheClusterId", "snap-tiny"),
            ]),
        )
        .await;
    assert_eq!(
        (tiny.status, tiny.error_code()),
        (400, "SnapshotFeatureNotSupportedFault"),
        "case the unsnapshottable node type: expected 400 SnapshotFeatureNotSupportedFault, \
         got {} {}",
        tiny.status,
        tiny.body
    );
    assert_ne!(
        tiny.error_message(),
        state::SNAPSHOT_ENGINE_UNSUPPORTED_MESSAGE,
        "case the unsnapshottable node type: the node-type gate keeps its own sentence, got {:?}",
        tiny.error_message()
    );

    // The STATE gate: a cluster the clock has not settled yet, and one left `snapshotting`
    // by a snapshot just taken, are both refused with the status such a call ADMITS.
    let creating = create_cluster(&emulator, "snap-creating", &[("Engine", "redis")]).await;
    assert_eq!(
        creating.status, 200,
        "the cluster must be created, got {} {}",
        creating.status, creating.body
    );
    let first = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("SnapshotName", "first-of-redis"),
                ("CacheClusterId", "snap-redis"),
            ]),
        )
        .await;
    assert_eq!(
        first.status, 200,
        "the first snapshot must be taken, got {} {}",
        first.status, first.body
    );
    for (case, cluster) in [
        ("a cluster the clock has not settled", "snap-creating"),
        ("a cluster left snapshotting", "snap-redis"),
    ] {
        let refused = emulator
            .call(
                "CreateSnapshot",
                &params(&[("SnapshotName", "never-taken"), ("CacheClusterId", cluster)]),
            )
            .await;
        assert_refusal(
            case,
            &refused,
            400,
            "InvalidCacheClusterState",
            state::SNAPSHOT_SOURCE_NOT_AVAILABLE_MESSAGE,
        );
        assert!(
            !refused.error_message().contains(cluster),
            "case {case:?}: the sentence carries no part of the request, got {:?}",
            refused.error_message()
        );
    }
}

/// A taken name is refused differently depending on whether the snapshot is being TAKEN or
/// COPIED: the copy names the taken name as its DESTINATION, and neither sentence carries
/// the name itself.
#[tokio::test]
async fn a_taken_snapshot_name_is_worded_by_how_the_snapshot_arrives() {
    let emulator = Emulator::new();
    create_available_cluster(&emulator, "taken-source", &[("Engine", "redis")]).await;
    available_snapshot(&emulator, "the source", "origin", "taken-source").await;

    // The axis: which name the copy's destination collides with — another snapshot, the
    // copy's own source, and a name spelled in another case (a name "is stored as a
    // lowercase string", so it is one identity).
    available_snapshot(&emulator, "a second snapshot", "second", "taken-source").await;
    for (case, target) in [
        ("another snapshot's name", "second"),
        ("the source's own name", "origin"),
        ("a taken name in another case", "SECOND"),
    ] {
        let refused = emulator
            .call(
                "CopySnapshot",
                &params(&[
                    ("SourceSnapshotName", "origin"),
                    ("TargetSnapshotName", target),
                ]),
            )
            .await;
        assert_refusal(
            case,
            &refused,
            400,
            "SnapshotAlreadyExistsFault",
            state::COPY_DESTINATION_SNAPSHOT_ALREADY_EXISTS_MESSAGE,
        );
        assert!(
            !refused
                .error_message()
                .contains(&target.to_ascii_lowercase()),
            "case {case:?}: the sentence names no snapshot, got {:?}",
            refused.error_message()
        );
    }

    // The contrast: a snapshot being TAKEN under a taken name keeps the other sentence, so
    // the destination wording belongs to the copy.
    for (case, name) in [
        ("a snapshot taken under a taken name", "origin"),
        ("the same in another case", "Second"),
    ] {
        let refused = emulator
            .call(
                "CreateSnapshot",
                &params(&[("SnapshotName", name), ("CacheClusterId", "taken-source")]),
            )
            .await;
        assert_refusal(
            case,
            &refused,
            400,
            "SnapshotAlreadyExistsFault",
            state::SNAPSHOT_ALREADY_EXISTS_MESSAGE,
        );
    }
}

/// A `CacheSubnetGroupName` that resolves to nothing is worded per operation: the create of
/// a CACHE CLUSTER answers the template the group's own writes answer, and the describe and
/// the replication-group create each keep their own.
#[tokio::test]
async fn a_cache_subnet_group_that_resolves_to_nothing_is_worded_per_operation() {
    let emulator = Emulator::new();

    // The axis: the form of the name a request may send — a plain name, one spelled in
    // another case (the name's identity is its lowercased form), one made only of digits
    // after its first letter, and the longest a name may be (`CacheSubnetGroupName`: "Must
    // contain no more than 255 alphanumeric characters or hyphens", which
    // `state::is_valid_subnet_group_name` enforces).
    let longest = format!("g{}", "a".repeat(254));
    for name in [
        "no-such-group",
        "Mixed-Case-Group",
        "group-0123456789",
        longest.as_str(),
    ] {
        let stored = name.to_ascii_lowercase();
        // Every one of these reports the name's STORED identity: a name "is stored as a
        // lowercase string", so the service names the group it resolved under that one
        // identity whatever case the request spelled.
        let placed = emulator
            .call(
                "CreateCacheCluster",
                &params(&[
                    ("CacheClusterId", "subnet-placed"),
                    ("CacheNodeType", "cache.t3.micro"),
                    ("CacheSubnetGroupName", name),
                ]),
            )
            .await;
        assert_refusal(
            &format!("CreateCacheCluster naming {name:?}"),
            &placed,
            400,
            "CacheSubnetGroupNotFoundFault",
            &state::subnet_group_does_not_exist_message(&stored),
        );

        let described = emulator
            .call(
                "DescribeCacheSubnetGroups",
                &params(&[("CacheSubnetGroupName", name)]),
            )
            .await;
        assert_refusal(
            &format!("DescribeCacheSubnetGroups naming {name:?}"),
            &described,
            400,
            "CacheSubnetGroupNotFoundFault",
            &state::describe_subnet_group_not_found_message(&stored),
        );

        // The two that word it their own way: the create of a replication group quotes the
        // name, and the delete of the group itself answers the template the cache-cluster
        // create above answers.
        let group = emulator
            .call(
                "CreateReplicationGroup",
                &params(&[
                    ("ReplicationGroupId", "subnet-placed-group"),
                    ("ReplicationGroupDescription", "a group never made"),
                    ("CacheNodeType", "cache.t3.micro"),
                    ("CacheSubnetGroupName", name),
                ]),
            )
            .await;
        assert_refusal(
            &format!("CreateReplicationGroup naming {name:?}"),
            &group,
            400,
            "CacheSubnetGroupNotFoundFault",
            &state::placement_subnet_group_not_found_message(&stored),
        );

        let deleted = emulator
            .call(
                "DeleteCacheSubnetGroup",
                &params(&[("CacheSubnetGroupName", name)]),
            )
            .await;
        assert_refusal(
            &format!("DeleteCacheSubnetGroup naming {name:?}"),
            &deleted,
            400,
            "CacheSubnetGroupNotFoundFault",
            &state::subnet_group_does_not_exist_message(&stored),
        );
    }
}

/// One identifier row: the case name, the `Action`, the member the malformed value is sent
/// as, the rest of a request that operation needs, and the LEAD-IN sentence the service
/// words that member's identifier refusal with.
struct IdentifierRow {
    case: &'static str,
    action: &'static str,
    member: &'static str,
    rest: &'static [(&'static str, &'static str)],
    lead_in: &'static str,
}

/// The lead-in of the identifier-grammar refusal is the MEMBER's: the members AWS names by
/// its own parameter label carry that label, and the members it names in prose keep the
/// prose. Both lead-ins end in the same grammar sentence.
#[tokio::test]
async fn an_identifier_refusal_names_the_member_the_way_aws_names_it() {
    let emulator = Emulator::new();

    // The axis: which clause of the grammar the value breaks, and which member carries it.
    let rows: [IdentifierRow; 6] = [
        IdentifierRow {
            case: "a cache cluster create",
            action: "CreateCacheCluster",
            member: "CacheClusterId",
            rest: &[("Engine", "redis"), ("CacheNodeType", "cache.t3.micro")],
            lead_in: "The parameter CacheClusterIdentifier is not a valid identifier.",
        },
        IdentifierRow {
            case: "a cache cluster lookup",
            action: "DescribeCacheClusters",
            member: "CacheClusterId",
            rest: &[],
            lead_in: "The parameter CacheClusterIdentifier is not a valid identifier.",
        },
        IdentifierRow {
            case: "the same member on the snapshot describe",
            action: "DescribeSnapshots",
            member: "CacheClusterId",
            rest: &[],
            lead_in: "The parameter CacheClusterIdentifier is not a valid identifier.",
        },
        IdentifierRow {
            case: "a parameter group delete",
            action: "DeleteCacheParameterGroup",
            member: "CacheParameterGroupName",
            rest: &[],
            lead_in: "The parameter CacheParameterGroupName is not a valid identifier.",
        },
        IdentifierRow {
            case: "a parameter group reset",
            action: "ResetCacheParameterGroup",
            member: "CacheParameterGroupName",
            rest: &[("ResetAllParameters", "true")],
            lead_in: "The parameter CacheParameterGroupName is not a valid identifier.",
        },
        IdentifierRow {
            case: "a replication group lookup",
            action: "DescribeReplicationGroups",
            member: "ReplicationGroupId",
            rest: &[],
            lead_in: "Replication Group ID specified is not a valid identifier.",
        },
    ];

    // Every way a value can miss the grammar: a leading digit, a leading hyphen, a trailing
    // hyphen, two consecutive hyphens, a character outside the set, and the '.' a family's
    // own default group name carries.
    let malformed = [
        "9starts-with-a-digit",
        "-leading-hyphen",
        "trailing-hyphen-",
        "two--hyphens",
        "under_score",
        "default.redis7",
    ];

    for row in rows {
        let case = row.case;
        let expected = format!("{} {GRAMMAR}", row.lead_in);
        for value in malformed {
            let mut pairs = params(&[(row.member, value)]);
            pairs.extend(params(row.rest));
            let refused = emulator.call(row.action, &pairs).await;
            assert_refusal(
                &format!("{case} with {value:?}"),
                &refused,
                400,
                "InvalidParameterValue",
                &expected,
            );
        }
        // A value the grammar ADMITS is judged by the resource instead, so the rows above
        // are refused for their form rather than by an operation refusing everything.
        let mut pairs = params(&[(row.member, "a-well-formed-name")]);
        pairs.extend(params(row.rest));
        let answered = emulator.call(row.action, &pairs).await;
        assert_ne!(
            answered.error_message(),
            expected,
            "case {case:?}: a well-formed value must not be refused for its form, got {}",
            answered.body
        );
    }
}
