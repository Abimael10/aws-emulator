// SPDX-License-Identifier: CC0-1.0

//! Amazon ElastiCache's published default-quota catalog.

use super::*;

/// The `AWS/Usage` display name Amazon ElastiCache publishes its resource counts
/// under, which is the `Service` dimension of every metered ElastiCache quota.
const ELASTICACHE_USAGE_SERVICE: &str = "ElastiCache";

/// The sentence ElastiCache publishes for a plane counted across ONE REGION: what the
/// account may create there, in the service's own voice.
///
/// SOURCE: the recorded default-quota listing of `elasticache`, whose subnet-group plane
/// reads `The maximum number of subnet groups you can create in a Region.` — the account is
/// not named in it and the Region is "a Region", not "the current Region", which is what
/// distinguishes this service's wording from [`super::resource_count_plane`]'s. The class is
/// that one sentence around what the plane counts, so it is built once here.
fn per_region_description(counted: &str) -> String {
    format!("The maximum number of {counted} you can create in a Region.")
}

/// Amazon ElastiCache's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `elasticache` service code (the
/// "Quotas for ElastiCache" page of the Amazon ElastiCache User Guide), and the
/// recorded default-quota listing of that code for which `QuotaCode` names which
/// quota, for the usage metric each metered row carries and for the published wording of
/// each row. The planes ElastiCache counts PER REGION are the ones CloudWatch publishes an
/// `AWS/Usage` resource count for, so those rows carry that metric and the window those
/// counts are published over; a plane counted inside one cluster, cache or group carries
/// the metric of that plane and its own sentence.
///
/// The `Resource` dimension a plane's count is published under is the series CloudWatch
/// carries it as, which for this service is the resource TYPE on a plane of the Region
/// (`SubnetGroup`, `User`) and the plane's own name on a plane counted inside another
/// resource (`UsersPerUserGroup`, `NodesPerCluster`).
///
/// A quota this catalog can cite no code for is left to the default-quota-set gap rather
/// than published under a neighbouring code (see [`super::descriptive_quota_code`]), which
/// is why the replica plane of a shard, the user-group plane and the manual-backup plane of
/// the Region are absent: `L-AF354865`, which this catalog had published the user-group
/// plane under, is the code the per-cluster node plane is published under, and
/// `L-3F15A733` and `L-DFE45DF3`, which it had published the per-cluster Memcached node
/// plane and the manual-backup plane under, are the codes the parameter-group plane and the
/// node plane of the Region are published under. The Memcached node plane the first of
/// those bindings displaced takes the code the parameter-group plane vacated, with that
/// code's published adjustability.
pub(in crate::state) fn elasticache_quotas() -> Vec<QuotaRow> {
    let metered = |code: &str, name: &str, value: f64, resource: &str, counted: &str| {
        resource_count_plane(code, name, value, ELASTICACHE_USAGE_SERVICE, resource, counted)
            .describe(&per_region_description(counted))
    };
    // A plane counted INSIDE one cluster, cache or group: metered like a plane of the
    // Region, under the series CloudWatch publishes that plane's count as.
    let inside = |code: &str, name: &str, value: f64, resource: &str| {
        quota(name, value).code(code).metric(usage_metric(
            ELASTICACHE_USAGE_SERVICE,
            &[
                ("Class", "None"),
                ("Resource", resource),
                ("Type", "Resource"),
            ],
        ))
    };
    vec![
        metered(
            "L-3E7F7726",
            "Subnet groups per Region",
            300.0,
            "SubnetGroup",
            "subnet groups",
        ),
        // The parameter-group plane of the Region, published under the code the recorded
        // listing states for it, with that plane's own resource count and the window those
        // counts are published over.
        metered(
            "L-3F15A733",
            "Parameter groups per Region",
            300.0,
            "ParameterGroup",
            "parameter groups",
        ),
        // The snapshot plane of ONE serverless cache, counted inside the cache rather than
        // across the Region, which is what its own sentence names.
        inside(
            "L-75A7B5A4",
            "Serverless snapshots per day per cache",
            24.0,
            "SnapshotsPerDayPerCache",
        )
        .describe(
            "The maximum number of snapshots that can be taken per day per serverless cache.",
        ),
        // The node plane of ONE Memcached cluster, the plane the parameter-group binding
        // above displaced: it takes the code that binding vacated, and keeps the
        // adjustability the recorded listing reports for THAT code.
        quota("Nodes per cluster (Memcached)", 40.0)
            .code("L-7D6587E6")
            .fixed()
            .describe("The maximum number of nodes in one Memcached cluster."),
        metered(
            "L-80E085C7",
            "Users per Region",
            2000.0,
            "User",
            "users",
        ),
        // The shard plane of ONE cluster with cluster mode enabled, counted inside the
        // cluster and metered under that plane's own series: the recorded listing publishes
        // this code over the window a resource count is published with, which is the window
        // every metered plane of this service carries.
        inside(
            "L-8C334AD1",
            "Shards per cluster (cluster mode enabled)",
            500.0,
            "ShardsPerCluster",
        )
        .describe("The maximum number of shards in one cluster with cluster mode enabled."),
        // The user plane of ONE user group, counted inside the group. The service words
        // this one as the users a client DEFINES for the group.
        inside(
            "L-943F0F1C",
            "Users per User Group",
            100.0,
            "UsersPerUserGroup",
        )
        .describe("The maximum number of users you can define for a user group."),
        inside(
            "L-A87EE522",
            "Subnets per subnet group",
            20.0,
            "SubnetsPerSubnetGroup",
        )
        .describe("The maximum number of subnets you can add to a subnet group."),
        metered(
            "L-AD484FC5",
            "Security groups per Region",
            50.0,
            "SecurityGroupsPerRegion",
            "cache security groups",
        ),
        // The node plane of ONE cluster with cluster mode enabled, counted inside the
        // cluster and metered under that plane's own series, with the limit the recorded
        // listing reports for its code.
        inside(
            "L-AF354865",
            "Nodes per cluster (cluster mode enabled)",
            90.0,
            "NodesPerCluster",
        )
        .describe("The maximum number of nodes in one cluster with cluster mode enabled."),
        metered(
            "L-BBCDAECC",
            "Serverless caches per Region",
            40.0,
            "ServerlessCachesPerRegion",
            "serverless caches",
        ),
        // The node plane of the Region, published under the code the recorded listing
        // states for it; the manual-backup plane this catalog had served here names no
        // other code that listing states, so it is left to the default-quota-set gap.
        metered(
            "L-DFE45DF3",
            "Nodes per Region",
            300.0,
            "NodesPerRegion",
            "nodes",
        ),
    ]
}
