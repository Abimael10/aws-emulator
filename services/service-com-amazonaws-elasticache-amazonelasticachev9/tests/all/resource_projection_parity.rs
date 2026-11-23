// SPDX-License-Identifier: CC0-1.0

//! What a `ServerlessCache` and a `UserGroup` PROJECT onto the wire: the member
//! set the call that minted the resource serializes, and the member set every
//! later read serializes.
//!
//! The rule under test: a response carrying a resource structure projects that
//! structure from the stored record, so every modelled member the record holds a
//! value for is serialized — the same members with the same values on the create
//! that minted the resource and on the describe that reads it back, including a
//! value that is itself the answer (`SnapshotRetentionLimit` 0, which a client
//! has to be able to tell from a member the service did not report) — and a
//! member the service DERIVES rather than stores
//! (`UserGroup$MinimumEngineVersion`) is resolved from that resource's own engine
//! against the versions the engine publishes rather than from one fixed number.
//!
//! The member lists below are the members of the `ServerlessCache` and
//! `UserGroup` shapes (`smithy_query(query="shape",
//! shape="ServerlessCache,UserGroup")`) that a stored record always holds a value
//! for; the ones it holds only sometimes (`CacheUsageLimits`, `KmsKeyId`,
//! `UserGroupId`, `Migration`, `PendingChanges`, and the lists naming what a
//! group is attached to) are named by the row or the case that creates them.
//!
//! Every assertion goes over the wire through the same router the production
//! emulator mounts, and nothing reaches into the service's state struct, so a
//! member that is not computed from persisted state cannot pass. The table's axes
//! are what the rule generalises over: the engine the resource runs, which of the
//! optional configurations the request states, how many security groups and
//! subnets it names, and whether the projection is read off the create or off the
//! read-back.

use std::collections::BTreeMap;

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, poll_until, Emulator, Reply, REGION};
use crate::rbac_minimum_engine_version::published_versions;

/// One resource structure as it reached the wire: every text node under the
/// element the structure is serialized at, keyed by its path RELATIVE to that
/// element, with one entry per value a list member carried, in document order.
type Projection = BTreeMap<String, Vec<String>>;

/// The `ServerlessCache` members a stored cache always holds a value for, so
/// every projection of one has to carry them.
///
/// `Description` is not among them because the one a cache created without one
/// carries is a single space (`state::DEFAULT_SERVERLESS_DESCRIPTION`), which the
/// harness' node scanner trims to nothing; the row that states a description of
/// its own asserts it instead.
const CACHE_ALWAYS_PROJECTED: &[&str] = &[
    "ServerlessCacheName",
    "CreateTime",
    "Status",
    "Engine",
    "MajorEngineVersion",
    "FullEngineVersion",
    "StorageEncryptionType",
    "SecurityGroupIds/SecurityGroupId",
    "SubnetIds/SubnetId",
    "Endpoint/Address",
    "Endpoint/Port",
    "ReaderEndpoint/Address",
    "ReaderEndpoint/Port",
    "SnapshotRetentionLimit",
    "DailySnapshotTime",
    "NetworkType",
    "ConnectionType",
    "ARN",
];

/// The `UserGroup` members a stored group always holds a value for.
const GROUP_ALWAYS_PROJECTED: &[&str] = &[
    "UserGroupId",
    "Status",
    "Engine",
    "UserIds/member",
    "MinimumEngineVersion",
    "ARN",
];

/// The member whose value moves on its own while the clock settles the resource,
/// so the two projections are compared without it and it is asserted separately.
const LIFECYCLE_MEMBER: &[&str] = &["Status"];

/// One row of the serverless-cache table: what the request configures, and the
/// members that configuration adds to the projection.
#[derive(Debug, Clone, Copy)]
struct CacheCase {
    /// Names the row in every assertion.
    case: &'static str,
    /// The `ServerlessCacheName` the request sends.
    name: &'static str,
    /// The form pairs sent beside it.
    extra: &'static [(&'static str, &'static str)],
    /// How many security groups of the account's own the request names; `0`
    /// leaves the choice to the service.
    named_groups: usize,
    /// How many of the account's default subnets the request names; `0` leaves
    /// the selection to the service.
    named_subnets: usize,
    /// The retention the request states, or `None` when it states none — where
    /// the cache must still report the posture it was created with.
    retention: Option<&'static str>,
    /// The members this row's configuration adds on top of
    /// [`CACHE_ALWAYS_PROJECTED`].
    also_projected: &'static [&'static str],
}

/// The table: the engines a serverless cache runs, the snapshot configuration,
/// the capacity envelope, and whether the request names its own security groups
/// and placement.
const CACHE_CASES: &[CacheCase] = &[
    CacheCase {
        case: "valkey, nothing but the engine configured",
        name: "projection-valkey",
        extra: &[("Engine", "valkey")],
        named_groups: 0,
        named_subnets: 0,
        retention: None,
        also_projected: &[],
    },
    CacheCase {
        case: "redis, nothing but the engine configured",
        name: "projection-redis",
        extra: &[("Engine", "redis")],
        named_groups: 0,
        named_subnets: 0,
        retention: None,
        also_projected: &[],
    },
    CacheCase {
        // "Available for Valkey, Redis OSS and Serverless Memcached only"
        // (`ServerlessCache$SnapshotRetentionLimit`), so a Memcached cache
        // projects the same snapshot posture as the others.
        case: "memcached, nothing but the engine configured",
        name: "projection-memcached",
        extra: &[("Engine", "memcached")],
        named_groups: 0,
        named_subnets: 0,
        retention: None,
        also_projected: &[],
    },
    CacheCase {
        case: "a snapshot posture the request states",
        name: "projection-snapshots",
        extra: &[
            ("Engine", "valkey"),
            ("SnapshotRetentionLimit", "12"),
            ("DailySnapshotTime", "03:15"),
        ],
        named_groups: 0,
        named_subnets: 0,
        retention: Some("12"),
        also_projected: &[],
    },
    CacheCase {
        case: "the security groups and the placement named by the request",
        name: "projection-network",
        extra: &[("Engine", "redis")],
        named_groups: 2,
        named_subnets: 2,
        retention: None,
        also_projected: &[],
    },
    CacheCase {
        case: "a description the request states",
        name: "projection-described",
        extra: &[("Engine", "valkey"), ("Description", "the described cache")],
        named_groups: 0,
        named_subnets: 0,
        retention: None,
        also_projected: &["Description"],
    },
    CacheCase {
        case: "a capacity envelope and an engine version the request states",
        name: "projection-capacity",
        extra: &[
            ("Engine", "valkey"),
            ("MajorEngineVersion", "8"),
            ("CacheUsageLimits.DataStorage.Unit", "GB"),
            ("CacheUsageLimits.DataStorage.Minimum", "2"),
            ("CacheUsageLimits.DataStorage.Maximum", "20"),
            ("CacheUsageLimits.ECPUPerSecond.Minimum", "1000"),
            ("CacheUsageLimits.ECPUPerSecond.Maximum", "5000"),
        ],
        named_groups: 0,
        named_subnets: 0,
        retention: None,
        also_projected: &[
            "CacheUsageLimits/DataStorage/Unit",
            "CacheUsageLimits/DataStorage/Minimum",
            "CacheUsageLimits/DataStorage/Maximum",
            "CacheUsageLimits/ECPUPerSecond/Minimum",
            "CacheUsageLimits/ECPUPerSecond/Maximum",
        ],
    },
];

/// The resource structure `reply` serialized under the element path `root`, as a
/// [`Projection`].
///
/// Every element under `root` is folded into one map, so a caller that compares
/// two projections names ONE resource in its request (and asserts it got one
/// back) rather than letting a page of several collapse into a single answer.
fn projection(case: &str, where_from: &str, reply: &Reply, root: &str) -> Projection {
    let marker = format!("/{root}/");
    let mut members: Projection = BTreeMap::new();
    for (path, text) in &reply.nodes {
        if let Some(at) = path.find(&marker) {
            members
                .entry(path[at + marker.len()..].to_string())
                .or_default()
                .push(text.clone());
        }
    }
    assert!(
        !members.is_empty(),
        "case {case:?} ({where_from}): the answer must carry a resource under {root}, got {}",
        reply.body
    );
    members
}

/// Assert `members` carries every member in `required`, each with a value.
fn assert_carries(case: &str, where_from: &str, members: &Projection, required: &[&str]) {
    for member in required {
        let values = members.get(*member);
        assert!(
            values.is_some_and(|values| !values.is_empty()
                && values.iter().all(|value| !value.is_empty())),
            "case {case:?} ({where_from}): the projection must carry {member}, got the members \
             {:?}",
            members.keys().collect::<Vec<_>>()
        );
    }
}

/// Assert the create path and the read path projected the SAME resource: the same
/// members with the same values, apart from the ones in `moving`, whose value the
/// clock changes on its own.
fn assert_same_projection(
    case: &str,
    what: &str,
    created: &Projection,
    read: &Projection,
    moving: &[&str],
) {
    let settled = |members: &Projection| -> Projection {
        members
            .iter()
            .filter(|(member, _)| !moving.contains(&member.as_str()))
            .map(|(member, values)| (member.clone(), values.clone()))
            .collect()
    };
    let (from_create, from_read) = (settled(created), settled(read));
    assert_eq!(
        from_create, from_read,
        "case {case:?}: the read must project the same {what} as the create, got {from_read:?} \
         after {from_create:?}"
    );
}

/// Create one serverless cache from already-owned form pairs.
async fn create_cache(emulator: &Emulator, case: &CacheCase, extra: &[(String, String)]) -> Reply {
    let mut pairs = params(&[("ServerlessCacheName", case.name)]);
    pairs.extend(extra.iter().cloned());
    let created = emulator.call("CreateServerlessCache", &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: CreateServerlessCache must answer 200, got {} {}",
        case.case, created.status, created.body
    );
    created
}

/// Read one serverless cache back by name.
async fn describe_cache(emulator: &Emulator, case: &str, name: &str) -> Reply {
    let described = emulator
        .call(
            "DescribeServerlessCaches",
            &params(&[("ServerlessCacheName", name)]),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: DescribeServerlessCaches must answer 200, got {} {}",
        described.status, described.body
    );
    described
}

#[tokio::test]
async fn a_serverless_cache_projects_the_same_members_on_the_create_and_on_the_read() {
    let emulator = Emulator::new();
    // The account's own network, as EC2 reports it: a row that names a placement
    // names subnets the account HAS, so no identifier here is one this suite
    // wrote down.
    let default_subnets: Vec<_> = emulator
        .account_subnets()
        .await
        .into_iter()
        .filter(|subnet| subnet.default_for_az)
        .collect();
    assert!(
        default_subnets.len() >= 2,
        "the account must have at least two default subnets to place a cache in, got \
         {default_subnets:?}"
    );

    for row in CACHE_CASES {
        let mut extra = params(row.extra);
        extra.extend(
            default_subnets[..row.named_subnets]
                .iter()
                .enumerate()
                .map(|(position, subnet)| {
                    (
                        format!("SubnetIds.SubnetId.{}", position + 1),
                        subnet.id.clone(),
                    )
                }),
        );
        extra.extend(
            emulator
                .security_groups(row.name, row.named_groups)
                .await
                .iter()
                .enumerate()
                .map(|(position, id)| {
                    (
                        format!("SecurityGroupIds.SecurityGroupId.{}", position + 1),
                        id.clone(),
                    )
                }),
        );

        let created = create_cache(&emulator, row, &extra).await;
        let from_create = projection(row.case, "create", &created, "ServerlessCache");
        assert_carries(row.case, "create", &from_create, CACHE_ALWAYS_PROJECTED);
        assert_carries(row.case, "create", &from_create, row.also_projected);

        // A posture the request did not state is still the cache's own: the
        // retention the service created it with is reported as a number, which is
        // what lets a client tell automatic snapshots being OFF from a member the
        // service did not report at all.
        let expected_retention = row.retention.map_or_else(
            || state::DEFAULT_SNAPSHOT_RETENTION_LIMIT.to_string(),
            str::to_string,
        );
        assert_eq!(
            created.text("SnapshotRetentionLimit"),
            Some(expected_retention.as_str()),
            "case {:?}: the create must report SnapshotRetentionLimit {expected_retention}, got {}",
            row.case,
            created.body
        );

        let described = describe_cache(&emulator, row.case, row.name).await;
        // The describe names one cache, so its answer carries one — which is what
        // makes the comparison below a comparison of ONE resource's two
        // projections.
        assert_eq!(
            described.all("ServerlessCacheName").len(),
            1,
            "case {:?}: the read must report the one cache it was asked for, got {}",
            row.case,
            described.body
        );
        let from_read = projection(row.case, "describe", &described, "ServerlessCaches/member");
        assert_carries(row.case, "describe", &from_read, CACHE_ALWAYS_PROJECTED);
        assert_carries(row.case, "describe", &from_read, row.also_projected);
        assert_same_projection(
            row.case,
            "serverless cache",
            &from_create,
            &from_read,
            LIFECYCLE_MEMBER,
        );

        // The one member left out of the comparison: a lifecycle value of this
        // service's own vocabulary on both paths, because the clock may have
        // settled the cache between the two calls.
        for (where_from, reported) in [
            ("create", created.text("Status")),
            ("describe", described.text("Status")),
        ] {
            assert!(
                reported.is_some_and(|status| [
                    state::SERVERLESS_STATUS_CREATING,
                    state::SERVERLESS_STATUS_AVAILABLE
                ]
                .contains(&status)),
                "case {:?} ({where_from}): a created cache must report creating or available, got \
                 {reported:?}",
                row.case
            );
        }

        // And the read is a read: looking again projects the same cache rather
        // than a different one.
        let again = describe_cache(&emulator, row.case, row.name).await;
        assert_same_projection(
            row.case,
            "serverless cache on a second read",
            &from_read,
            &projection(row.case, "describe again", &again, "ServerlessCaches/member"),
            LIFECYCLE_MEMBER,
        );
    }
}

#[tokio::test]
async fn a_user_group_projects_the_same_members_on_the_create_and_on_the_read() {
    // The axis: the engine RBAC exists for (`state::USER_ENGINES`) and the
    // spelling the request sends it in.
    for (case, engine, stored_engine) in [
        ("redis", "redis", "redis"),
        ("valkey", "valkey", "valkey"),
        ("the engine in another letter case", "VaLkEy", "valkey"),
    ] {
        let emulator = Emulator::new();
        let request = params(&[("UserGroupId", "projection-group")]);
        let mut create = request.clone();
        create.extend(params(&[
            ("Engine", engine),
            // "Redis user group needs to contain a user with the user name
            // default": the built-in user every account has.
            ("UserIds.member.1", state::DEFAULT_USER_ID),
        ]));
        let created = emulator.call("CreateUserGroup", &create).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateUserGroup must answer 200, got {} {}",
            created.status, created.body
        );
        let from_create = projection(case, "create", &created, "CreateUserGroupResult");
        assert_carries(case, "create", &from_create, GROUP_ALWAYS_PROJECTED);

        let described = emulator.call("DescribeUserGroups", &request).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribeUserGroups must answer 200, got {} {}",
            described.status, described.body
        );
        let from_read = projection(case, "describe", &described, "UserGroups/member");
        assert_carries(case, "describe", &from_read, GROUP_ALWAYS_PROJECTED);
        assert_same_projection(case, "user group", &from_create, &from_read, LIFECYCLE_MEMBER);
        assert_eq!(
            described.all("UserGroupId").len(),
            1,
            "case {case:?}: the read must report the one group it was asked for, got {}",
            described.body
        );

        assert_eq!(
            created.text("Engine"),
            Some(stored_engine),
            "case {case:?}: the group must be listed for {stored_engine}, got {}",
            created.body
        );
        // The derived member: the floor is the oldest version the group's OWN
        // engine publishes at or above the floor the feature arrived at, read out
        // of the catalog this service publishes over the wire — so a group of
        // either engine reports a version a client can actually attach it to,
        // and neither reports one fixed number.
        let published = published_versions(&emulator, stored_engine).await;
        let floor = state::USER_MIN_ENGINE_VERSION;
        let oldest_supported = published
            .iter()
            .filter(|version| state::version_sort_key(version) >= state::version_sort_key(floor))
            .min_by_key(|version| state::version_sort_key(version))
            .unwrap_or_else(|| {
                panic!("case {case:?}: {stored_engine} publishes no version at or above {floor}")
            });
        for (where_from, reply) in [("create", &created), ("describe", &described)] {
            assert_eq!(
                reply.text("MinimumEngineVersion"),
                Some(oldest_supported.as_str()),
                "case {case:?} ({where_from}): the floor must be the oldest version \
                 {stored_engine} publishes at or above {floor} ({oldest_supported}), got {}",
                reply.body
            );
        }
    }
}

#[tokio::test]
async fn an_attached_user_group_and_serverless_cache_each_project_the_other() {
    // The relation is one fact held by two records, so BOTH projections have to
    // carry it: the cache names the group it is attached to, and the group names
    // the cache it is attached to — on the create path and on the read path.
    let emulator = Emulator::new();
    let group_request = params(&[("UserGroupId", "attached-group")]);
    let mut create_group = group_request.clone();
    create_group.extend(params(&[
        ("Engine", "valkey"),
        ("UserIds.member.1", state::DEFAULT_USER_ID),
    ]));
    let created_group = emulator.call("CreateUserGroup", &create_group).await;
    assert_eq!(
        created_group.status, 200,
        "CreateUserGroup must answer 200, got {} {}",
        created_group.status, created_group.body
    );
    assert!(
        !projection("attachment", "create", &created_group, "CreateUserGroupResult")
            .contains_key("ServerlessCaches/member"),
        "a group nothing is attached to must name no serverless cache, got {}",
        created_group.body
    );

    // A cache can only be attached to an ACTIVE group, which the clock owes the
    // created one: the condition is waited for rather than a span of time.
    poll_until(
        &emulator,
        REGION,
        "attachment",
        "the created user group becoming active",
        "DescribeUserGroups",
        &group_request,
        |reply| reply.text("Status") == Some(state::USER_STATUS_ACTIVE),
    )
    .await;

    let created_cache = emulator
        .call(
            "CreateServerlessCache",
            &params(&[
                ("ServerlessCacheName", "attached-cache"),
                ("Engine", "valkey"),
                ("UserGroupId", "attached-group"),
            ]),
        )
        .await;
    assert_eq!(
        created_cache.status, 200,
        "CreateServerlessCache must answer 200, got {} {}",
        created_cache.status, created_cache.body
    );
    let from_create = projection("attachment", "create", &created_cache, "ServerlessCache");
    assert_carries("attachment", "create", &from_create, CACHE_ALWAYS_PROJECTED);
    assert_carries("attachment", "create", &from_create, &["UserGroupId"]);

    let described_cache = describe_cache(&emulator, "attachment", "attached-cache").await;
    let from_read = projection(
        "attachment",
        "describe",
        &described_cache,
        "ServerlessCaches/member",
    );
    assert_same_projection(
        "attachment",
        "serverless cache",
        &from_create,
        &from_read,
        LIFECYCLE_MEMBER,
    );

    // The other side of the same relation, out of the group's own record.
    let described_group = emulator.call("DescribeUserGroups", &group_request).await;
    assert_eq!(
        described_group.status, 200,
        "DescribeUserGroups must answer 200, got {} {}",
        described_group.status, described_group.body
    );
    assert_eq!(
        described_group.all("ServerlessCaches/member"),
        vec!["attached-cache"],
        "the group must name the cache it now serves, got {}",
        described_group.body
    );
}
