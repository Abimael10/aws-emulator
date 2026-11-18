// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals whose WHOLE sentence is fixed text, or whose only
//! variable parts are values the REQUEST named: the shard configuration of a replication
//! group whose keyspace is not partitioned, a serverless cache whose name the account
//! already holds, an encryption feature asked for on an engine version that has none,
//! automatic failover on a group of one cache cluster, and a value that is not one of this
//! service's identifiers.
//!
//! The rule under test: a refusal that is about the RESOURCE CLASS rather than about a value
//! the request carried states the rule alone — it names neither the identifier the request
//! sent nor the resource it collided with — so a client branching on the message reads the
//! same sentence whatever it asked for; and where the service DOES name values, it names the
//! ones the request supplied and the limit the rule publishes.
//!
//! * `ModifyReplicationGroupShardConfiguration` configures "the Valkey or Redis OSS (cluster
//!   mode ENABLED) cluster (replication group) on which the shards are to be configured": a
//!   group that is not one has a single keyspace in a single shard, so there is no shard
//!   configuration of it to change;
//! * `CreateServerlessCache` refuses a name the account already holds with
//!   `ServerlessCacheAlreadyExistsFault`;
//! * the encryption features belong to the engine BUILD, so a create that asks for one on an
//!   engine version below the first release of its engine that supports them is refused
//!   `InvalidParameterCombination`, naming the version the request sent and the version to
//!   use instead;
//! * automatic failover promotes another cache cluster of the group, so a group of one is
//!   refused `InvalidParameterCombination` with the COUNT such a group needs;
//! * a value outside this service's identifier grammar is refused `InvalidParameterValue`
//!   stating that grammar over the resource identifier the request SPECIFIED.
//!
//! The tests are tables: the rows span the shard count asked for and the direction of the
//! change, the casing and length of the name a second create sends, the engine and version
//! an encryption feature is asked for on, the shape of the group a failover is asked for,
//! and each way a value can miss the identifier grammar at each operation that judges one —
//! so every sentence is pinned as a template rather than as one recorded string.
//!
//! Every assertion goes over the wire through the same router the production emulator
//! mounts, with the shared `harness` request builder and XML reader. The expected sentences
//! are written out here on purpose: a test that built them from the same constant the
//! handler reads would pass whatever that constant said.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// How ElastiCache refuses a shard-configuration change on a group whose keyspace is not
/// partitioned.
const SHARDS_NEED_CLUSTER_MODE: &str =
    "Operation is only applicable for cluster mode enabled replication groups.";

/// How ElastiCache refuses a serverless cache whose name the account already holds: the
/// class alone, with no trailing stop.
const SERVERLESS_CACHE_ALREADY_EXISTS: &str = "Serverless Cache already exists";

/// Create one replication group with the pairs a row sends and let the clock settle it.
async fn create_settled_group(emulator: &Emulator, case: &str, id: &str, extra: &[(&str, &str)]) {
    let mut pairs = params(&[
        ("ReplicationGroupId", id),
        ("ReplicationGroupDescription", "a group to reshard"),
        ("Engine", state::DEFAULT_ENGINE),
        ("CacheNodeType", "cache.m7g.large"),
    ]);
    pairs.extend(params(extra));
    let created = emulator.call("CreateReplicationGroup", &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the create must answer 200, got {} {}",
        created.status, created.body
    );
    // The create leaves the group `creating`, and only a settled group takes a reshard, so
    // the suite waits for that condition rather than for a fixed time.
    poll_until(
        emulator,
        REGION,
        case,
        &format!("the replication group {id} settling at available"),
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", id)]),
        |reply| reply.text("Status") == Some(state::STATUS_AVAILABLE),
    )
    .await;
}

/// `ModifyReplicationGroupShardConfiguration` of `id` to `shards` shards.
async fn reshard(emulator: &Emulator, id: &str, shards: &str) -> Reply {
    emulator
        .call(
            "ModifyReplicationGroupShardConfiguration",
            &params(&[
                ("ReplicationGroupId", id),
                ("NodeGroupCount", shards),
                ("ApplyImmediately", "true"),
            ]),
        )
        .await
}

/// `CreateServerlessCache` for one name.
async fn create_serverless_cache(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "CreateServerlessCache",
            &params(&[("ServerlessCacheName", name), ("Engine", "valkey")]),
        )
        .await
}

/// A reshard of a group whose keyspace is not partitioned is refused with the rule, whatever
/// count it asked for.
#[tokio::test]
async fn a_reshard_of_an_unpartitioned_group_states_the_cluster_mode_rule() {
    let emulator = Emulator::new();
    // Two groups that are not cluster mode enabled: one created with no partitioning member
    // at all, and one that asked for the single shard a group has by default.
    create_settled_group(&emulator, "the default group", "unsharded-default", &[]).await;
    create_settled_group(
        &emulator,
        "the group that asked for one shard",
        "unsharded-one",
        &[("NumNodeGroups", "1")],
    )
    .await;
    // And one that IS cluster mode enabled, so the rule is shown to bite only the groups it
    // is about.
    create_settled_group(
        &emulator,
        "the partitioned group",
        "sharded-group",
        &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "1")],
    )
    .await;

    // The axis: the group the request names and the count it asks for — a scale-out, a
    // bigger scale-out, and the published maximum — none of which a group without a
    // partitioned keyspace can take.
    for (case, id, shards) in [
        ("one shard to two", "unsharded-default", "2"),
        ("one shard to five", "unsharded-default", "5"),
        (
            "one shard to the published maximum",
            "unsharded-one",
            "90",
        ),
    ] {
        let refused = reshard(&emulator, id, shards).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidParameterValue"),
            "case {case:?}: expected 400 InvalidParameterValue, got {} {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            SHARDS_NEED_CLUSTER_MODE,
            "case {case:?}: expected the cluster-mode rule, got {:?}",
            refused.error_message()
        );
    }

    // The same call on a group whose keyspace IS partitioned is served.
    let accepted = reshard(&emulator, "sharded-group", "3").await;
    assert_eq!(
        accepted.status, 200,
        "a partitioned group must take a reshard, got {} {}",
        accepted.status, accepted.body
    );
}

/// A serverless cache whose name the account already holds is refused with the class's own
/// sentence, whatever name it was.
#[tokio::test]
async fn a_second_serverless_cache_of_one_name_states_the_class_alone() {
    let emulator = Emulator::new();

    // The axis: the name a first create took, and how the second create spells it — a
    // serverless cache name "is stored as a lowercase string", so a name in another case is
    // the same cache.
    let rows: &[(&str, &str, &str)] = &[
        ("the same name again", "taken-cache", "taken-cache"),
        ("the same name in upper case", "cased-cache", "CASED-CACHE"),
        (
            "a long name the account already holds",
            "a-serverless-cache-name-of-some-length",
            "A-Serverless-Cache-Name-Of-Some-Length",
        ),
    ];

    for (case, first, second) in rows {
        let created = create_serverless_cache(&emulator, first).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the first create must answer 200, got {} {}",
            created.status, created.body
        );

        let refused = create_serverless_cache(&emulator, second).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "ServerlessCacheAlreadyExistsFault"),
            "case {case:?}: expected 400 ServerlessCacheAlreadyExistsFault, got {} {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            SERVERLESS_CACHE_ALREADY_EXISTS,
            "case {case:?}: expected the class's own sentence, got {:?}",
            refused.error_message()
        );
        // The cache the first create made is untouched: the refusal replaced nothing.
        let described = emulator
            .call(
                "DescribeServerlessCaches",
                &params(&[("ServerlessCacheName", first)]),
            )
            .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: the first cache must still be there, got {} {}",
            described.status, described.body
        );
    }
}

/// How ElastiCache refuses an encryption feature on an engine version that has none: the
/// version the request named, and the first version of that engine which supports it.
fn encryption_unsupported(engine_version: &str, minimum: &str) -> String {
    format!(
        "Encryption features are not supported for engine version {engine_version}. Please use \
         engine version {minimum}"
    )
}

/// How ElastiCache refuses automatic failover on a replication group of ONE cache cluster:
/// wholly fixed text, naming the count such a group needs.
const FAILOVER_NEEDS_TWO_CLUSTERS: &str =
    "When using automatic failover, there must be at least 2 cache clusters in the replication \
     group.";

/// The identifier grammar ElastiCache states over the resource identifier a request
/// specified. `resource` is how the service names that identifier in prose, for the members
/// it words this way.
fn identifier_rule(resource: &str) -> String {
    format!(
        "{resource} specified is not a valid identifier. Identifiers must begin with a letter; \
         must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or \
         contain two consecutive hyphens."
    )
}

/// The same grammar for the members ElastiCache names by its own PARAMETER label instead:
/// the lead-in is per member, not one sentence for the whole service.
fn parameter_identifier_rule(parameter: &str) -> String {
    format!(
        "The parameter {parameter} is not a valid identifier. Identifiers must begin with a \
         letter; must contain only ASCII letters, digits, and hyphens; and must not end with a \
         hyphen or contain two consecutive hyphens."
    )
}

/// One encryption row: the engine, the version the create names, whether it asks for
/// in-transit encryption, and the first encryption-capable version of that engine when the
/// pair is refused.
struct EncryptionRow {
    case: &'static str,
    engine: &'static str,
    engine_version: &'static str,
    transit_encryption: bool,
    /// `Some(minimum)` when the pair is refused, `None` when the create is served.
    refused_with_minimum: Option<&'static str>,
}

/// The rows span what the rule generalises over: which engine, how far below or above its
/// first encryption-capable version the named version is, and whether the create asks for
/// encryption at all. The versions are ones `DescribeCacheEngineVersions` offers (the
/// catalogue in `state::CACHE_ENGINE_VERSIONS`), so no row is refused for naming a version
/// the engine does not have.
const ENCRYPTION_ROWS: &[EncryptionRow] = &[
    EncryptionRow {
        case: "the oldest Memcached version this service offers",
        engine: "memcached",
        engine_version: "1.4.5",
        transit_encryption: true,
        refused_with_minimum: Some("1.6.12"),
    },
    EncryptionRow {
        case: "a Memcached version one release below the first that supports it",
        engine: "memcached",
        engine_version: "1.6.6",
        transit_encryption: true,
        refused_with_minimum: Some("1.6.12"),
    },
    EncryptionRow {
        case: "a Memcached version in the line below",
        engine: "memcached",
        engine_version: "1.5.16",
        transit_encryption: true,
        refused_with_minimum: Some("1.6.12"),
    },
    EncryptionRow {
        case: "the first Memcached version that supports it",
        engine: "memcached",
        engine_version: "1.6.12",
        transit_encryption: true,
        refused_with_minimum: None,
    },
    EncryptionRow {
        case: "a Memcached version past the first that supports it",
        engine: "memcached",
        engine_version: "1.6.22",
        transit_encryption: true,
        refused_with_minimum: None,
    },
    EncryptionRow {
        case: "a Memcached version without encryption asked for at all",
        engine: "memcached",
        engine_version: "1.6.6",
        transit_encryption: false,
        refused_with_minimum: None,
    },
    EncryptionRow {
        case: "the oldest Redis OSS version this service offers",
        engine: "redis",
        engine_version: "4.0.10",
        transit_encryption: true,
        refused_with_minimum: None,
    },
];

/// An encryption feature asked for on an engine version that has none is refused as the
/// combination it is, naming both versions, and nothing is created.
#[tokio::test]
async fn encryption_on_an_engine_version_without_it_names_the_version_to_use() {
    let emulator = Emulator::new();

    for (index, row) in ENCRYPTION_ROWS.iter().enumerate() {
        let case = row.case;
        let cluster = format!("encryption-row-{index}");
        let mut pairs = params(&[
            ("CacheClusterId", cluster.as_str()),
            ("Engine", row.engine),
            ("EngineVersion", row.engine_version),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheNodes", "1"),
        ]);
        if row.transit_encryption {
            pairs.extend(params(&[("TransitEncryptionEnabled", "true")]));
        }
        let answer = emulator.call("CreateCacheCluster", &pairs).await;

        match row.refused_with_minimum {
            Some(minimum) => {
                assert_eq!(
                    (answer.status, answer.error_code()),
                    (400, "InvalidParameterCombination"),
                    "case {case:?}: expected 400 InvalidParameterCombination, got {} {}",
                    answer.status,
                    answer.body
                );
                let expected = encryption_unsupported(row.engine_version, minimum);
                assert_eq!(
                    answer.error_message(),
                    expected,
                    "case {case:?}: expected {expected:?}, got {:?}",
                    answer.error_message()
                );
                // Nothing was created: the refusal came before any state was written.
                let read = emulator
                    .call(
                        "DescribeCacheClusters",
                        &params(&[("CacheClusterId", cluster.as_str())]),
                    )
                    .await;
                assert_eq!(
                    read.error_code(),
                    "CacheClusterNotFound",
                    "case {case:?}: a refused create must store no cluster, got {}",
                    read.body
                );
                assert_eq!(
                    read.error_message(),
                    format!("CacheCluster not found: {cluster}"),
                    "case {case:?}: expected the not-found sentence, got {:?}",
                    read.error_message()
                );
            }
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: the create must be served, got {} {}",
                    answer.status, answer.body
                );
                // And it is really there, with the version it asked for.
                let read = emulator
                    .call(
                        "DescribeCacheClusters",
                        &params(&[("CacheClusterId", cluster.as_str())]),
                    )
                    .await;
                assert_eq!(
                    read.text("EngineVersion"),
                    Some(row.engine_version),
                    "case {case:?}: the cluster must run the version it asked for, got {}",
                    read.body
                );
            }
        }
    }
}

/// Automatic failover on a replication group of one cache cluster is refused with the count
/// such a group needs, at the create and at the modify, and nothing is created.
#[tokio::test]
async fn automatic_failover_on_one_cache_cluster_states_the_two_cluster_rule() {
    let emulator = Emulator::new();

    // The axis: how the request asks for failover on a group of one — by naming no cluster
    // count at all, by naming one, and by asking for Multi-AZ beside it (Multi-AZ IS that
    // promotion, so a group of one cannot have it either).
    let rows: &[(&str, &[(&str, &str)])] = &[
        (
            "a group that named no cluster count",
            &[("AutomaticFailoverEnabled", "true")],
        ),
        (
            "a group of one cache cluster",
            &[
                ("NumCacheClusters", "1"),
                ("AutomaticFailoverEnabled", "true"),
            ],
        ),
        (
            "a group of one cache cluster asking for Multi-AZ too",
            &[
                ("NumCacheClusters", "1"),
                ("AutomaticFailoverEnabled", "true"),
                ("MultiAZEnabled", "true"),
            ],
        ),
    ];

    for (index, (case, extra)) in rows.iter().enumerate() {
        let id = format!("failover-row-{index}");
        let mut pairs = params(&[
            ("ReplicationGroupId", id.as_str()),
            ("ReplicationGroupDescription", "a group of one cluster"),
            ("Engine", state::DEFAULT_ENGINE),
            ("CacheNodeType", "cache.m7g.large"),
        ]);
        pairs.extend(params(extra));
        let refused = emulator.call("CreateReplicationGroup", &pairs).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidParameterCombination"),
            "case {case:?}: expected 400 InvalidParameterCombination, got {} {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            FAILOVER_NEEDS_TWO_CLUSTERS,
            "case {case:?}: expected the two-cluster rule, got {:?}",
            refused.error_message()
        );
        let read = emulator
            .call(
                "DescribeReplicationGroups",
                &params(&[("ReplicationGroupId", id.as_str())]),
            )
            .await;
        assert_eq!(
            read.error_code(),
            "ReplicationGroupNotFoundFault",
            "case {case:?}: a refused create must store no group, got {}",
            read.body
        );
    }

    // A group of TWO cache clusters takes the same request: the rule is about the count.
    let served = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "failover-pair"),
                ("ReplicationGroupDescription", "a group of two clusters"),
                ("Engine", state::DEFAULT_ENGINE),
                ("CacheNodeType", "cache.m7g.large"),
                ("NumCacheClusters", "2"),
                ("AutomaticFailoverEnabled", "true"),
            ]),
        )
        .await;
    assert_eq!(
        served.status, 200,
        "a group of two cache clusters must take automatic failover, got {} {}",
        served.status, served.body
    );

    // And the modify states the same rule: a settled group of one cluster cannot be turned
    // into a failing-over one either.
    create_settled_group(
        &emulator,
        "the group to modify",
        "failover-single",
        &[("NumCacheClusters", "1")],
    )
    .await;
    let modified = emulator
        .call(
            "ModifyReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "failover-single"),
                ("AutomaticFailoverEnabled", "true"),
                ("ApplyImmediately", "true"),
            ]),
        )
        .await;
    assert_eq!(
        (modified.status, modified.error_code()),
        (400, "InvalidParameterCombination"),
        "the modify must be refused 400 InvalidParameterCombination, got {} {}",
        modified.status,
        modified.body
    );
    assert_eq!(
        modified.error_message(),
        FAILOVER_NEEDS_TWO_CLUSTERS,
        "the modify must state the two-cluster rule, got {:?}",
        modified.error_message()
    );
}

/// One identifier row: the operation, the member the bad value is sent as, the rest of a
/// request that operation needs, and how the service names that identifier in the refusal —
/// either in prose (`Prose`) or by the parameter label AWS gives the member (`Parameter`).
struct IdentifierRow {
    action: &'static str,
    member: &'static str,
    rest: &'static [(&'static str, &'static str)],
    names: IdentifierSubject,
}

/// Which lead-in the refusal of one member carries; see [`identifier_rule`] and
/// [`parameter_identifier_rule`].
enum IdentifierSubject {
    /// `<resource> specified is not a valid identifier. …`
    Prose(&'static str),
    /// `The parameter <label> is not a valid identifier. …`
    Parameter(&'static str),
}

impl IdentifierSubject {
    /// The whole sentence this subject's refusal carries.
    fn sentence(&self) -> String {
        match self {
            Self::Prose(resource) => identifier_rule(resource),
            Self::Parameter(parameter) => parameter_identifier_rule(parameter),
        }
    }
}

/// The rows span the operations that judge an identifier of this service — a create and a
/// lookup of each resource whose identifier carries the grammar — so the sentence is pinned
/// as the one rule the service states per member rather than as one operation's string.
const IDENTIFIER_ROWS: &[IdentifierRow] = &[
    IdentifierRow {
        action: "CreateReplicationGroup",
        member: "ReplicationGroupId",
        rest: &[
            ("ReplicationGroupDescription", "a group that is never made"),
            ("Engine", "redis"),
            ("CacheNodeType", "cache.m7g.large"),
        ],
        names: IdentifierSubject::Prose("Replication Group ID"),
    },
    IdentifierRow {
        action: "DescribeReplicationGroups",
        member: "ReplicationGroupId",
        rest: &[],
        names: IdentifierSubject::Prose("Replication Group ID"),
    },
    IdentifierRow {
        action: "CreateCacheCluster",
        member: "CacheClusterId",
        rest: &[("Engine", "memcached"), ("CacheNodeType", "cache.t3.micro")],
        names: IdentifierSubject::Parameter("CacheClusterIdentifier"),
    },
    IdentifierRow {
        action: "DescribeCacheClusters",
        member: "CacheClusterId",
        rest: &[],
        names: IdentifierSubject::Parameter("CacheClusterIdentifier"),
    },
    IdentifierRow {
        action: "DeleteCacheParameterGroup",
        member: "CacheParameterGroupName",
        rest: &[],
        names: IdentifierSubject::Parameter("CacheParameterGroupName"),
    },
];

/// Each way a value can miss the grammar: a leading digit, a trailing hyphen and two
/// consecutive hyphens — the three shapes the sentence itself names.
const MALFORMED_IDENTIFIERS: [(&str, &str); 3] = [
    ("a leading digit", "1cache"),
    ("a trailing hyphen", "cache-"),
    ("two consecutive hyphens", "ca--che"),
];

/// A value outside the identifier grammar is refused with the grammar, stated over the
/// resource identifier the request specified, at every operation that judges one.
#[tokio::test]
async fn a_malformed_identifier_states_the_grammar_over_the_resource_identifier() {
    let emulator = Emulator::new();

    for row in IDENTIFIER_ROWS {
        let expected = row.names.sentence();
        for (shape, value) in MALFORMED_IDENTIFIERS {
            let case = format!("{} with {shape}", row.action);
            let mut pairs = params(&[(row.member, value)]);
            pairs.extend(params(row.rest));
            let refused = emulator.call(row.action, &pairs).await;
            assert_eq!(
                (refused.status, refused.error_code()),
                (400, "InvalidParameterValue"),
                "case {case:?}: expected 400 InvalidParameterValue, got {} {}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.error_message(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                refused.error_message()
            );
        }
        // A well-formed value of the same shape is judged by the resource instead, so the
        // rule above is about the FORM rather than about the operation refusing everything.
        let mut pairs = params(&[(row.member, "a-well-formed-name")]);
        pairs.extend(params(row.rest));
        let answered = emulator.call(row.action, &pairs).await;
        assert_ne!(
            answered.error_message(),
            expected,
            "case {:?}: a well-formed identifier must not be refused for its form, got {}",
            row.action,
            answered.body
        );
    }
}
