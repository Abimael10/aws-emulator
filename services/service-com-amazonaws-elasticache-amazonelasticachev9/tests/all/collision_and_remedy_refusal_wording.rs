// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals whose sentence states the ERROR CLASS and, where the
//! class has one, the remedy — rather than a paraphrase of the gate the handler evaluated.
//!
//! The rule under test: a modelled refusal carries the sentence the service words for that
//! error class, whose fixed text is the whole message apart from the variable parts that
//! template has — a resource-class collision names nothing of the request, a dependency gate
//! names the dependency class that holds the resource, and a refusal whose remedy is another
//! operation says so. So a client branching on the message, or surfacing it, reads one
//! sentence per class whatever it asked for, and never a generic
//! `Invalid value <v> for <member>` lead-in that only restates the request.
//!
//! The three classes here:
//!
//! * `CreateCacheCluster` on a `CacheClusterId` the account already holds —
//!   `CacheClusterAlreadyExistsFault` ("You already have a cluster with the given
//!   identifier") — states the class alone, because the identifier is the one the caller
//!   just sent;
//! * `DeleteCacheSubnetGroup` on a group a cluster is placed in ("You cannot delete … one
//!   that is associated with any clusters") — `CacheSubnetGroupInUse` — names the group and
//!   the dependency class holding it, which is the only place the service reports WHAT
//!   blocks the delete;
//! * `DisassociateGlobalReplicationGroup` naming the datastore's PRIMARY member — the
//!   modelled `InvalidParameterValueException` — states that the primary cannot leave and
//!   that deleting the Global datastore is what releases it, which is the only place the
//!   service reports the remedy.
//!
//! The tests are tables: the rows differ along what each rule generalises over — the id, the
//! engine, the node count and the casing a second create sends; the class of resource
//! holding a subnet group and the casing the delete spells the group in; the shape of the
//! datastore and the ids of its members — so no single recorded request is what makes them
//! pass. Each table also drives one input the rule does NOT cover (a free name, a free
//! subnet group, a member that is a secondary or no member at all), so a sentence asserted
//! here is shown to be this class's rather than every refusal's.
//!
//! Every assertion goes over the wire through the same router the production emulator
//! mounts. The expected sentences are written out here on purpose: a test that built them
//! from the same constant the handler reads would pass whatever that constant said.

use crate::cache_cluster_lifecycle::{account_subnet_ids, create_cluster, subnet_group_params};
use crate::cache_subnet_group_delete::in_use_message;
use crate::harness::{assert_refusal, params, Emulator, REGION};
use crate::modeled_error_surfaces::seed_global_datastore;

/// How ElastiCache words a `CacheClusterId` the account already holds: the class alone, with
/// nothing of the request in it and no trailing stop.
const CACHE_CLUSTER_ALREADY_EXISTS: &str = "Cache cluster already exists";

/// How ElastiCache refuses a `DisassociateGlobalReplicationGroup` that names the datastore's
/// primary member: two wholly fixed sentences, the second naming the operation that releases
/// it.
const PRIMARY_NOT_DISASSOCIABLE: &str =
    "Cannot disassociate primary replication group. Primary Replication Group will be \
     disassociated when you delete the Global Replication Group.";

// How ElastiCache words a cache subnet group a cluster is placed in — the group's STORED
// name (`CacheSubnetGroupName` "is stored as a lowercase string") and the dependency class
// holding it — is stated once, by the suite that owns that operation
// (`crate::cache_subnet_group_delete::in_use_message`), so the two tables cannot drift to
// different expected sentences.

/// `name` with every other character upper-cased, so a row can spell one stored name in a
/// third casing without naming another group.
fn mixed_case(name: &str) -> String {
    name.chars()
        .enumerate()
        .map(|(index, character)| {
            if index % 2 == 0 {
                character.to_ascii_uppercase()
            } else {
                character
            }
        })
        .collect()
}

/// One duplicate-id row: the case, the id the first create takes, the pairs it sends beside
/// it, and the way the second create spells the same id.
struct DuplicateRow {
    case: &'static str,
    id: &'static str,
    extra: &'static [(&'static str, &'static str)],
    again: &'static str,
}

/// The rows span what the rule generalises over: the length and casing of the identifier,
/// the engine the cluster runs and how many nodes it holds. `CacheClusterId` "is stored as a
/// lowercase string", so a second create spelling it in another case collides with the same
/// cluster.
const DUPLICATE_ROWS: &[DuplicateRow] = &[
    DuplicateRow {
        case: "the same short id again",
        id: "dup-a",
        extra: &[("Engine", "redis"), ("CacheNodeType", "cache.t3.micro")],
        again: "dup-a",
    },
    DuplicateRow {
        case: "a memcached cluster of several nodes",
        id: "dup-memcached-cluster",
        extra: &[
            ("Engine", "memcached"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheNodes", "3"),
        ],
        again: "dup-memcached-cluster",
    },
    DuplicateRow {
        case: "a long id spelled in upper case the second time",
        id: "a-cache-cluster-identifier-of-some-length",
        extra: &[("Engine", "valkey"), ("CacheNodeType", "cache.m7g.large")],
        again: "A-Cache-Cluster-Identifier-Of-Some-Length",
    },
    DuplicateRow {
        case: "an id spelled in mixed case the first time",
        id: "Dup-Mixed-Case",
        extra: &[("Engine", "redis"), ("CacheNodeType", "cache.t3.micro")],
        again: "dup-mixed-case",
    },
];

/// A second `CreateCacheCluster` on an identifier the account holds is refused with the
/// class's own sentence, and the cluster the first create made is untouched.
#[tokio::test]
async fn a_duplicate_cache_cluster_id_states_the_class_alone() {
    let emulator = Emulator::new();

    for row in DUPLICATE_ROWS {
        let case = row.case;
        let created = create_cluster(&emulator, row.id, row.extra).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the first create must answer 200, got {} {}",
            created.status, created.body
        );

        // The second create differs from the first in whatever else it sends: the collision
        // is decided on the identifier alone, so the sentence cannot depend on the rest.
        let refused = create_cluster(
            &emulator,
            row.again,
            &[("Engine", "redis"), ("CacheNodeType", "cache.m7g.large")],
        )
        .await;
        assert_refusal(
            case,
            &refused,
            400,
            "CacheClusterAlreadyExists",
            CACHE_CLUSTER_ALREADY_EXISTS,
        );

        // The refusal replaced nothing: the stored cluster still runs what the FIRST create
        // asked for, read back through the describe.
        let described = emulator
            .call(
                "DescribeCacheClusters",
                &params(&[("CacheClusterId", row.id)]),
            )
            .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: the first cluster must still be there, got {} {}",
            described.status, described.body
        );
        let wanted_engine = row
            .extra
            .iter()
            .find(|(member, _)| *member == "Engine")
            .map(|(_, value)| *value);
        assert_eq!(
            described.text("Engine"),
            wanted_engine,
            "case {case:?}: the stored cluster must keep the first create's engine, got {}",
            described.body
        );
    }

    // An identifier nothing holds is not refused for its form: the rule is about the
    // collision, not about creating clusters at all.
    let free = create_cluster(
        &emulator,
        "a-free-cache-cluster-id",
        &[("Engine", "redis"), ("CacheNodeType", "cache.t3.micro")],
    )
    .await;
    assert_eq!(
        free.status, 200,
        "a free identifier must be served, got {} {}",
        free.status, free.body
    );
}

/// A subnet group a cache cluster or a replication group is placed in is refused with the
/// group's name and the dependency class holding it, whatever case the delete spells it in.
#[tokio::test]
async fn a_subnet_group_in_use_names_the_dependency_class() {
    let emulator = Emulator::new();
    let subnets = account_subnet_ids(&emulator).await;

    // Three groups: one a cache cluster is placed in, one a replication group is, and one
    // nothing holds.
    for name in ["held-by-cluster", "held-by-group", "held-by-nobody"] {
        let created = emulator
            .call(
                "CreateCacheSubnetGroup",
                &subnet_group_params(name, "a group to hold", &subnets[..2]),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: the subnet group must be created, got {} {}",
            created.status, created.body
        );
    }

    let cluster = create_cluster(
        &emulator,
        "subnet-holding-cluster",
        &[
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheNodes", "1"),
            ("CacheSubnetGroupName", "held-by-cluster"),
        ],
    )
    .await;
    assert_eq!(
        cluster.status, 200,
        "the cluster must be created, got {} {}",
        cluster.status, cluster.body
    );
    let group = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "subnet-holding-group"),
                ("ReplicationGroupDescription", "a group holding subnets"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
                ("CacheSubnetGroupName", "held-by-group"),
            ]),
        )
        .await;
    assert_eq!(
        group.status, 200,
        "the replication group must be created, got {} {}",
        group.status, group.body
    );

    // The axis: which class of resource holds the group, and how the delete spells the
    // group's name — the stored identity, upper case, and mixed case.
    for (case, group_name) in [
        ("a cache cluster holds it", "held-by-cluster"),
        ("a replication group holds it", "held-by-group"),
    ] {
        for spelling in [
            group_name.to_string(),
            group_name.to_ascii_uppercase(),
            mixed_case(group_name),
        ] {
            let refused = emulator
                .call(
                    "DeleteCacheSubnetGroup",
                    &params(&[("CacheSubnetGroupName", spelling.as_str())]),
                )
                .await;
            assert_refusal(
                &format!("{case}, named {spelling:?}"),
                &refused,
                400,
                "CacheSubnetGroupInUse",
                &in_use_message(&spelling),
            );
        }
    }

    // A group nothing is placed in is deleted: the sentence above is the dependency gate's,
    // not every delete's.
    let deleted = emulator
        .call(
            "DeleteCacheSubnetGroup",
            &params(&[("CacheSubnetGroupName", "held-by-nobody")]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "a group nothing holds must be deleted, got {} {}",
        deleted.status, deleted.body
    );
}

/// One datastore row: the case, the datastore's stored id, how many slice groups it holds,
/// and the secondary members attached to it.
struct DatastoreRow {
    case: &'static str,
    id: &'static str,
    node_groups: i32,
    secondaries: &'static [(&'static str, &'static str)],
}

/// The rows span what the rule generalises over: the datastore's id, whether its keyspace is
/// partitioned, and how many secondaries are attached — none of which the primary-member
/// refusal names.
const DATASTORE_ROWS: &[DatastoreRow] = &[
    DatastoreRow {
        case: "an unpartitioned datastore with one secondary",
        id: "grg-primary-one",
        node_groups: 1,
        secondaries: &[("secondary-one", "us-west-2")],
    },
    DatastoreRow {
        case: "a partitioned datastore with two secondaries",
        id: "grg-primary-two",
        node_groups: 2,
        secondaries: &[("secondary-two", "us-west-2"), ("secondary-three", "eu-west-1")],
    },
    DatastoreRow {
        case: "a datastore with no secondary at all",
        id: "grg-primary-alone",
        node_groups: 1,
        secondaries: &[],
    },
];

/// Disassociating the primary member is refused with the two fixed sentences, whatever the
/// datastore looks like; a secondary leaves, and a pair that is no member at all is refused
/// with the sentence for THAT class instead.
#[tokio::test]
async fn disassociating_the_primary_states_the_remedy() {
    let emulator = Emulator::new();

    for row in DATASTORE_ROWS {
        let case = row.case;
        seed_global_datastore(&emulator, row.id, row.node_groups, row.secondaries);
        let primary = format!("{}-primary", row.id);

        let refused = emulator
            .call(
                "DisassociateGlobalReplicationGroup",
                &params(&[
                    ("GlobalReplicationGroupId", row.id),
                    ("ReplicationGroupId", primary.as_str()),
                    ("ReplicationGroupRegion", REGION),
                ]),
            )
            .await;
        assert_refusal(
            case,
            &refused,
            400,
            "InvalidParameterValue",
            PRIMARY_NOT_DISASSOCIABLE,
        );

        // The primary is still a member: the refusal removed nothing.
        let described = emulator
            .call(
                "DescribeGlobalReplicationGroups",
                &params(&[
                    ("GlobalReplicationGroupId", row.id),
                    ("ShowMemberInfo", "true"),
                ]),
            )
            .await;
        assert!(
            described.all("ReplicationGroupId").contains(&primary.as_str()),
            "case {case:?}: the primary must still be a member, got {}",
            described.body
        );

        // A pair that names no member of this datastore is a different refusal class, so
        // the sentence above is the primary gate's rather than this operation's only one.
        let stranger = emulator
            .call(
                "DisassociateGlobalReplicationGroup",
                &params(&[
                    ("GlobalReplicationGroupId", row.id),
                    ("ReplicationGroupId", "not-a-member-at-all"),
                    ("ReplicationGroupRegion", "ap-south-1"),
                ]),
            )
            .await;
        assert_eq!(
            stranger.error_code(),
            "InvalidParameterValue",
            "case {case:?}: a pair that is no member must be refused InvalidParameterValue, \
             got {}",
            stranger.body
        );
        assert_ne!(
            stranger.error_message(),
            PRIMARY_NOT_DISASSOCIABLE,
            "case {case:?}: a pair that is no member must not get the primary sentence, got {}",
            stranger.body
        );

        // And a SECONDARY leaves: the gate is about the role, not about the operation.
        if let Some((secondary, region)) = row.secondaries.first() {
            let removed = emulator
                .call(
                    "DisassociateGlobalReplicationGroup",
                    &params(&[
                        ("GlobalReplicationGroupId", row.id),
                        ("ReplicationGroupId", secondary),
                        ("ReplicationGroupRegion", region),
                    ]),
                )
                .await;
            assert_eq!(
                removed.status, 200,
                "case {case:?}: a secondary must be disassociated, got {} {}",
                removed.status, removed.body
            );
        }
    }
}
