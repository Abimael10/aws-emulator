// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the members PCS DOCUMENTS AS SET: what a cluster — and a compute
//! node group — reports for a member the request named nothing for.
//!
//! RULE under test: where a member's own documentation states the value the service applies
//! when the request names none, the record a create or an update leaves behind carries that
//! value, so the call itself and every later read report the member SET to it; a member the
//! request names is reported exactly as it was sent, whatever it names; and a member whose
//! documentation states no value of its own is left absent.
//!
//! The members this model documents that way, and where each sentence lives
//! (`smithy_query(query="shape",shape="NetworkingRequest,ClusterSlurmConfigurationRequest,
//! AccountingRequest,SpotOptions,CreateComputeNodeGroupRequest")`, worded identically on each
//! response shape):
//!
//! * `networkType` — "The IP address version the cluster uses. The default is `IPV4`", which
//!   is also the address family the cluster's own endpoints are published on, so the two
//!   cannot disagree;
//! * `scaleDownIdleTimeInSeconds` (the CLUSTER's) — "Default: `600`";
//! * `defaultPurgeTimeInDays` — "The default value for `defaultPurgeTimeInDays` is `-1`";
//! * `allocationStrategy` — "If you don't provide this option, it defaults to price capacity
//!   optimized";
//! * `purchaseOption` — "If you don't provide this option, it defaults to On-Demand".
//!
//! A compute node group's `scaleDownIdleTimeInSeconds` documents no value of its own — "If
//! not specified, the cluster-level setting applies" — so it is the member pinned ABSENT
//! here. `mode` on accounting and on the Slurm REST API is documented the same way and was
//! already applied; the values the two members ADMIT are pinned in
//! `cluster_update_and_endpoints/accounting_purge_time.rs`, which this suite does not
//! repeat.
//!
//! Driven over the awsJson1_0 wire through the same router the emulator serves from, on the
//! harness [`crate::cluster_lifecycle`] builds. Nothing here waits on the wall clock: a
//! transition this service owes is materialised before the next request is answered.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{call, create_compute_node_group, emulator};

/// The `NetworkType` member the request's documentation names as the default
/// (`NetworkingRequest$networkType`), spelled as `NetworkType` spells it
/// (`smithy_query(query="shape",shape="NetworkType")`).
const DOCUMENTED_NETWORK_TYPE: &str = "IPV4";

/// The idle time the cluster's Slurm member documents as its default
/// (`ClusterSlurmConfigurationRequest$scaleDownIdleTimeInSeconds`: "Default: `600`").
const DOCUMENTED_SCALE_DOWN_SECONDS: i64 = 600;

/// The purge time the accounting member documents as its default
/// (`AccountingRequest$defaultPurgeTimeInDays`: "The default value ... is `-1`").
const DOCUMENTED_PURGE_TIME_IN_DAYS: i64 = -1;

/// The strategy the spot member documents as its default (`SpotOptions$allocationStrategy`:
/// "it defaults to price capacity optimized"), spelled as `SpotAllocationStrategy` spells it
/// (`smithy_query(query="shape",shape="SpotAllocationStrategy")`).
const DOCUMENTED_ALLOCATION_STRATEGY: &str = "price-capacity-optimized";

/// Every `SpotAllocationStrategy` member the model declares, one of which is the documented
/// default (source: `smithy_query(query="shape",shape="SpotAllocationStrategy")`).
const ALLOCATION_STRATEGIES: &[&str] = &[
    "lowest-price",
    "capacity-optimized",
    "price-capacity-optimized",
];

/// The purchasing option the member documents as its default
/// (`CreateComputeNodeGroupRequest$purchaseOption`, worded identically on
/// `UpdateComputeNodeGroupRequest` and on `ComputeNodeGroup`: "If you don't provide this
/// option, it defaults to On-Demand"), spelled as `PurchaseOption` spells it
/// (`smithy_query(query="shape",shape="PurchaseOption")`).
const DOCUMENTED_PURCHASE_OPTION: &str = "ONDEMAND";

/// Every `PurchaseOption` member the model declares, one of which is the documented default
/// (source: `smithy_query(query="shape",shape="PurchaseOption")`).
const PURCHASE_OPTIONS: &[&str] = &[
    "ONDEMAND",
    "SPOT",
    "CAPACITY_BLOCK",
    "INTERRUPTIBLE_CAPACITY_RESERVATION",
];

/// The published bounds of the cluster's `scaleDownIdleTimeInSeconds`
/// (`@range(min: 1, max: 10000000)` on
/// `ClusterSlurmConfigurationRequest$scaleDownIdleTimeInSeconds`).
const SCALE_DOWN_PUBLISHED_MIN: i64 = 1;
const SCALE_DOWN_PUBLISHED_MAX: i64 = 10_000_000;

/// One case of the table below: the request members under test and what every read of the
/// cluster must then report.
struct ClusterCase {
    /// How an assertion names this row.
    case: &'static str,
    /// The `networking` member the create carries.
    networking: Value,
    /// The `slurmConfiguration` member the create carries, when it carries one.
    slurm: Option<Value>,
    /// The `networking.networkType` every read must report.
    network_type: &'static str,
    /// The `slurmConfiguration.scaleDownIdleTimeInSeconds` every read must report, or `None`
    /// when the cluster holds no Slurm configuration at all.
    scale_down: Option<i64>,
    /// The `slurmConfiguration.accounting.defaultPurgeTimeInDays` every read must report, or
    /// `None` when the cluster holds no accounting configuration.
    purge_time: Option<i64>,
}

/// The `networking` member, with `networkType` named only when `network_type` is `Some`.
fn networking(network_type: Option<&str>) -> Value {
    let mut body = json!({
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
    });
    if let Some(network_type) = network_type {
        body["networkType"] = json!(network_type);
    }
    body
}

/// The table: one row per way a request can leave a documented member unnamed, and one per
/// way it can name it — every address family the enum declares, both published bounds of the
/// idle time, and an accounting configuration with and without a purge time.
fn cases() -> Vec<ClusterCase> {
    vec![
        ClusterCase {
            case: "nothing optional named at all",
            networking: networking(None),
            slurm: None,
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: None,
            purge_time: None,
        },
        ClusterCase {
            case: "the documented address family named explicitly",
            networking: networking(Some("IPV4")),
            slurm: None,
            network_type: "IPV4",
            scale_down: None,
            purge_time: None,
        },
        ClusterCase {
            case: "the other address family named explicitly",
            networking: networking(Some("IPV6")),
            slurm: None,
            network_type: "IPV6",
            scale_down: None,
            purge_time: None,
        },
        ClusterCase {
            case: "a Slurm configuration that names nothing",
            networking: networking(None),
            slurm: Some(json!({})),
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: Some(DOCUMENTED_SCALE_DOWN_SECONDS),
            purge_time: None,
        },
        ClusterCase {
            case: "accounting enabled, no purge time named",
            networking: networking(None),
            slurm: Some(json!({"accounting": {"mode": "STANDARD"}})),
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: Some(DOCUMENTED_SCALE_DOWN_SECONDS),
            purge_time: Some(DOCUMENTED_PURGE_TIME_IN_DAYS),
        },
        ClusterCase {
            case: "accounting off, no purge time named",
            networking: networking(Some("IPV6")),
            slurm: Some(json!({"accounting": {"mode": "NONE"}})),
            network_type: "IPV6",
            scale_down: Some(DOCUMENTED_SCALE_DOWN_SECONDS),
            purge_time: Some(DOCUMENTED_PURGE_TIME_IN_DAYS),
        },
        ClusterCase {
            case: "the Slurm REST API named, nothing else",
            networking: networking(None),
            slurm: Some(json!({"slurmRest": {"mode": "STANDARD"}})),
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: Some(DOCUMENTED_SCALE_DOWN_SECONDS),
            purge_time: None,
        },
        ClusterCase {
            case: "the idle time at its published minimum",
            networking: networking(None),
            slurm: Some(json!({"scaleDownIdleTimeInSeconds": SCALE_DOWN_PUBLISHED_MIN})),
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: Some(SCALE_DOWN_PUBLISHED_MIN),
            purge_time: None,
        },
        ClusterCase {
            case: "the idle time at its published maximum",
            networking: networking(None),
            slurm: Some(json!({"scaleDownIdleTimeInSeconds": SCALE_DOWN_PUBLISHED_MAX})),
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: Some(SCALE_DOWN_PUBLISHED_MAX),
            purge_time: None,
        },
        ClusterCase {
            case: "both Slurm members named",
            networking: networking(Some("IPV4")),
            slurm: Some(json!({
                "scaleDownIdleTimeInSeconds": 900,
                "accounting": {"mode": "STANDARD", "defaultPurgeTimeInDays": 45},
            })),
            network_type: "IPV4",
            scale_down: Some(900),
            purge_time: Some(45),
        },
        ClusterCase {
            case: "the purge time named as the value it documents",
            networking: networking(None),
            slurm: Some(json!({
                "accounting": {"mode": "STANDARD", "defaultPurgeTimeInDays": -1},
            })),
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: Some(DOCUMENTED_SCALE_DOWN_SECONDS),
            purge_time: Some(DOCUMENTED_PURGE_TIME_IN_DAYS),
        },
        ClusterCase {
            case: "the purge time at its published maximum",
            networking: networking(None),
            slurm: Some(json!({
                "accounting": {"mode": "STANDARD", "defaultPurgeTimeInDays": 10_000},
            })),
            network_type: DOCUMENTED_NETWORK_TYPE,
            scale_down: Some(DOCUMENTED_SCALE_DOWN_SECONDS),
            purge_time: Some(10_000),
        },
    ]
}

/// A `CreateCluster` request carrying every `@required` member and this row's optional ones.
fn create_request(name: &str, row: &ClusterCase) -> Value {
    let mut body = json!({
        "clusterName": name,
        "scheduler": {"type": "SLURM", "version": "24.11"},
        "size": "SMALL",
        "networking": row.networking.clone(),
    });
    if let Some(slurm) = row.slurm.as_ref() {
        body["slurmConfiguration"] = slurm.clone();
    }
    body
}

/// Every documented member `cluster` must report for `row`, whichever call answered it.
fn assert_documented_members(source: &str, cluster: &Value, row: &ClusterCase) {
    let case = row.case;
    assert_eq!(
        cluster["networking"]["networkType"],
        json!(row.network_type),
        "case {case:?} ({source}): expected networkType {:?}, got {cluster}",
        row.network_type
    );
    match row.scale_down {
        Some(expected) => assert_eq!(
            cluster["slurmConfiguration"]["scaleDownIdleTimeInSeconds"],
            json!(expected),
            "case {case:?} ({source}): expected scaleDownIdleTimeInSeconds {expected}, got \
             {cluster}"
        ),
        None => assert!(
            cluster["slurmConfiguration"].is_null(),
            "case {case:?} ({source}): expected no slurmConfiguration at all, got {cluster}"
        ),
    }
    match row.purge_time {
        Some(expected) => assert_eq!(
            cluster["slurmConfiguration"]["accounting"]["defaultPurgeTimeInDays"],
            json!(expected),
            "case {case:?} ({source}): expected defaultPurgeTimeInDays {expected}, got {cluster}"
        ),
        None => assert!(
            cluster["slurmConfiguration"]["accounting"].is_null(),
            "case {case:?} ({source}): expected no accounting configuration, got {cluster}"
        ),
    }
}

/// The address family the cluster's published endpoints are on agrees with the
/// `networkType` the same record reports: "For clusters configured to use IPv6, this is an
/// empty string" (`Endpoint$privateIpAddress`), and an IPv4 cluster reports no
/// `ipv6Address`.
fn assert_endpoints_match_the_network_type(case: &str, cluster: &Value) {
    let network_type = cluster["networking"]["networkType"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let endpoints = cluster["endpoints"].as_array().cloned().unwrap_or_default();
    assert!(
        !endpoints.is_empty(),
        "case {case:?}: expected a settled cluster to publish its scheduler endpoints, got \
         {cluster}"
    );
    for endpoint in &endpoints {
        if network_type == "IPV6" {
            assert_eq!(
                endpoint["privateIpAddress"],
                json!(""),
                "case {case:?}: expected an IPv6 cluster to publish no private IPv4 address, \
                 got {endpoint}"
            );
            assert!(
                endpoint["ipv6Address"]
                    .as_str()
                    .is_some_and(|address| address.contains(':')),
                "case {case:?}: expected an IPv6 cluster to publish an IPv6 address, got \
                 {endpoint}"
            );
        } else {
            assert!(
                endpoint["privateIpAddress"]
                    .as_str()
                    .is_some_and(|address| address.contains('.')),
                "case {case:?}: expected an IPv4 cluster to publish a private IPv4 address, \
                 got {endpoint}"
            );
            assert!(
                endpoint["ipv6Address"].is_null(),
                "case {case:?}: expected an IPv4 cluster to publish no IPv6 address, got \
                 {endpoint}"
            );
        }
    }
}

#[tokio::test]
async fn a_create_reports_every_documented_member_as_set_and_a_read_answers_the_same() {
    let router = emulator();

    for (index, row) in cases().iter().enumerate() {
        let name = format!("defaults-create-{index}");
        let (status, created) = call(&router, "CreateCluster", &create_request(&name, row)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the create to be served, got {status} with {created}",
            row.case
        );
        assert_documented_members("CreateCluster", &created["cluster"], row);

        // The read that follows the create settles it, so the record it answers publishes
        // the endpoints the same record's address family asks for.
        let (status, read) = call(
            &router,
            "GetCluster",
            &json!({"clusterIdentifier": name}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected GetCluster to be served, got {status} with {read}",
            row.case
        );
        assert_documented_members("GetCluster", &read["cluster"], row);
        assert_endpoints_match_the_network_type(row.case, &read["cluster"]);
    }
}

#[tokio::test]
async fn a_list_reports_the_same_documented_members_the_create_answered() {
    let router = emulator();

    // One cluster per row, then ONE list: a documented default is part of the stored record,
    // so the member a list reports is the member the create answered.
    let rows = cases();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("defaults-list-{index}");
        let (status, created) = call(&router, "CreateCluster", &create_request(&name, row)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the create to be served, got {status} with {created}",
            row.case
        );
    }

    let (status, listed) = call(&router, "ListClusters", &json!({"maxResults": 100})).await;
    assert_eq!(status, 200, "ListClusters got {status} with {listed}");
    let names: Vec<String> = listed["clusters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|cluster| cluster["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        names.len(),
        rows.len(),
        "expected every created cluster listed, got {names:?}"
    );

    for (index, row) in rows.iter().enumerate() {
        let name = format!("defaults-list-{index}");
        let (status, read) = call(
            &router,
            "GetCluster",
            &json!({"clusterIdentifier": name}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected GetCluster to be served, got {status} with {read}",
            row.case
        );
        assert_documented_members("GetCluster after the list", &read["cluster"], row);
    }
}

#[tokio::test]
async fn an_idle_time_outside_the_members_published_range_is_refused_on_both_paths() {
    let router = emulator();
    let host = "defaults-range-host";
    let (status, created) = call(
        &router,
        "CreateCluster",
        &create_request(host, &cases()[0]),
    )
    .await;
    assert_eq!(status, 200, "case setup: got {status} with {created}");

    // One past each published bound, so the default this suite pins is never read as
    // permission to store a value the range excludes.
    for (case, seconds) in [
        ("one below the published minimum", SCALE_DOWN_PUBLISHED_MIN - 1),
        ("one above the published maximum", SCALE_DOWN_PUBLISHED_MAX + 1),
    ] {
        let mut row = cases().remove(0);
        row.case = case;
        row.slurm = Some(json!({"scaleDownIdleTimeInSeconds": seconds}));
        let (status, refused) = call(
            &router,
            "CreateCluster",
            &create_request("defaults-range-refused", &row),
        )
        .await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ValidationException")),
            "case {case:?} on CreateCluster: expected {seconds} refused, got {status} with \
             {refused}"
        );

        let (status, refused) = call(
            &router,
            "UpdateCluster",
            &json!({
                "clusterIdentifier": host,
                "slurmConfiguration": {"scaleDownIdleTimeInSeconds": seconds},
            }),
        )
        .await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ValidationException")),
            "case {case:?} on UpdateCluster: expected {seconds} refused, got {status} with \
             {refused}"
        );
    }

    // Not one of the refused calls wrote anything: the only cluster stored is the setup's,
    // and it carries no Slurm configuration because its create named none.
    let (status, read) = call(
        &router,
        "GetCluster",
        &json!({"clusterIdentifier": host}),
    )
    .await;
    assert_eq!(status, 200, "GetCluster got {status} with {read}");
    assert!(
        read["cluster"]["slurmConfiguration"].is_null(),
        "expected the refused calls to have stored no Slurm configuration, got {read}"
    );
}

/// The Slurm configuration `GetCluster` reports for `identifier`, once the transitions the
/// service owes have been materialised by this very read.
async fn stored_slurm_configuration(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Value {
    let (status, read) = call(
        router,
        "GetCluster",
        &json!({"clusterIdentifier": identifier}),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: GetCluster got {status} with {read}");
    read["cluster"]["slurmConfiguration"].clone()
}

#[tokio::test]
async fn an_update_leaves_every_documented_slurm_member_set() {
    let router = emulator();
    let name = "defaults-update-host";

    // A cluster created with no Slurm configuration at all holds none, so the update below is
    // what brings one into being: the documented members are set by whichever call writes it.
    let (status, created) = call(
        &router,
        "CreateCluster",
        &create_request(name, &cases()[0]),
    )
    .await;
    assert_eq!(status, 200, "case setup: got {status} with {created}");
    assert!(
        created["cluster"]["slurmConfiguration"].is_null(),
        "case setup: expected no Slurm configuration, got {created}"
    );

    // Each row updates one member and states what the whole configuration must then be: the
    // member the request named, and every documented member, whether named or not.
    let rows: [(&str, Value, Value); 4] = [
        (
            "accounting turned on by the update",
            json!({"accounting": {"mode": "STANDARD"}}),
            json!({
                "scaleDownIdleTimeInSeconds": DOCUMENTED_SCALE_DOWN_SECONDS,
                "accounting": {
                    "mode": "STANDARD",
                    "defaultPurgeTimeInDays": DOCUMENTED_PURGE_TIME_IN_DAYS,
                },
            }),
        ),
        (
            "the idle time named by the update",
            json!({"scaleDownIdleTimeInSeconds": 1_200}),
            json!({
                "scaleDownIdleTimeInSeconds": 1_200,
                "accounting": {
                    "mode": "STANDARD",
                    "defaultPurgeTimeInDays": DOCUMENTED_PURGE_TIME_IN_DAYS,
                },
            }),
        ),
        (
            "accounting turned off again",
            json!({"accounting": {"mode": "NONE"}}),
            json!({
                "scaleDownIdleTimeInSeconds": 1_200,
                "accounting": {
                    "mode": "NONE",
                    "defaultPurgeTimeInDays": DOCUMENTED_PURGE_TIME_IN_DAYS,
                },
            }),
        ),
        (
            "a purge time named by the update",
            json!({"accounting": {"mode": "STANDARD", "defaultPurgeTimeInDays": 30}}),
            json!({
                "scaleDownIdleTimeInSeconds": 1_200,
                "accounting": {"mode": "STANDARD", "defaultPurgeTimeInDays": 30},
            }),
        ),
    ];

    for (case, patch, expected) in rows {
        let (status, updated) = call(
            &router,
            "UpdateCluster",
            &json!({"clusterIdentifier": name, "slurmConfiguration": patch}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be served, got {status} with {updated}"
        );
        assert_eq!(
            updated["cluster"]["slurmConfiguration"], expected,
            "case {case:?}: expected the update to answer {expected}, got {updated}"
        );

        // The read settles the cluster back to ACTIVE, so the next row starts from a cluster
        // an update can be served against without anything waiting on the wall clock.
        let stored = stored_slurm_configuration(&router, case, name).await;
        assert_eq!(
            stored, expected,
            "case {case:?}: expected {expected} to persist, got {stored}"
        );
    }
}

/// A `CreateComputeNodeGroup` request for `cluster`, carrying `members` beside every
/// `@required` one.
fn create_group_request(cluster: &str, name: &str, members: &Value) -> Value {
    let mut body = create_compute_node_group(cluster, name);
    if let (Some(target), Some(extra)) = (body.as_object_mut(), members.as_object()) {
        for (member, value) in extra {
            target.insert(member.clone(), value.clone());
        }
    }
    body
}

/// The compute node group `GetComputeNodeGroup` reports.
async fn stored_group(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    group: &str,
) -> Value {
    let (status, read) = call(
        router,
        "GetComputeNodeGroup",
        &json!({"clusterIdentifier": cluster, "computeNodeGroupIdentifier": group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetComputeNodeGroup got {status} with {read}"
    );
    read["computeNodeGroup"].clone()
}

#[tokio::test]
async fn a_spot_configuration_carries_the_allocation_strategy_the_member_documents() {
    let router = emulator();
    let cluster = "defaults-spot-cluster";
    let (status, created) = call(
        &router,
        "CreateCluster",
        &create_request(cluster, &cases()[0]),
    )
    .await;
    assert_eq!(status, 200, "case setup: got {status} with {created}");

    // Row 1: a spot configuration that names no strategy carries the documented one. The
    // rest: every strategy the enum declares is stored as it was sent.
    let mut rows: Vec<(String, Value, Option<&str>)> = vec![(
        "no strategy named".to_string(),
        json!({"purchaseOption": "SPOT", "spotOptions": {}}),
        Some(DOCUMENTED_ALLOCATION_STRATEGY),
    )];
    for strategy in ALLOCATION_STRATEGIES {
        rows.push((
            format!("the {strategy} strategy named"),
            json!({"purchaseOption": "SPOT", "spotOptions": {"allocationStrategy": strategy}}),
            Some(strategy),
        ));
    }
    // And a group whose request carries no spot configuration at all stores none: the
    // documented value belongs to a member of a structure, not to a structure nobody asked
    // for.
    rows.push((
        "no spot configuration at all".to_string(),
        json!({"purchaseOption": "ONDEMAND"}),
        None,
    ));

    let mut group_ids: Vec<String> = Vec::new();
    for (index, (case, members, expected)) in rows.iter().enumerate() {
        let name = format!("defaults-spot-{index}");
        let (status, created) = call(
            &router,
            "CreateComputeNodeGroup",
            &create_group_request(cluster, &name, members),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served, got {status} with {created}"
        );
        group_ids.push(
            created["computeNodeGroup"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        for (source, group) in [
            ("CreateComputeNodeGroup", created["computeNodeGroup"].clone()),
            (
                "GetComputeNodeGroup",
                stored_group(&router, case, cluster, &name).await,
            ),
        ] {
            match expected {
                Some(strategy) => assert_eq!(
                    group["spotOptions"]["allocationStrategy"],
                    json!(strategy),
                    "case {case:?} ({source}): expected allocationStrategy {strategy:?}, got \
                     {group}"
                ),
                None => assert!(
                    group["spotOptions"].is_null(),
                    "case {case:?} ({source}): expected no spotOptions, got {group}"
                ),
            }
        }
    }

    // An UPDATE that names a spot configuration without a strategy leaves the same
    // documented member set — here over a group whose create named a DIFFERENT strategy, so
    // which call stored the configuration does not change what a read reports.
    let (status, updated) = call(
        &router,
        "UpdateComputeNodeGroup",
        &json!({
            "clusterIdentifier": cluster,
            "computeNodeGroupIdentifier": group_ids.get(1).cloned().unwrap_or_default(),
            "spotOptions": {},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case update with no strategy named: got {status} with {updated}"
    );
    assert_eq!(
        updated["computeNodeGroup"]["spotOptions"]["allocationStrategy"],
        json!(DOCUMENTED_ALLOCATION_STRATEGY),
        "case update with no strategy named: expected allocationStrategy \
         {DOCUMENTED_ALLOCATION_STRATEGY:?}, got {updated}"
    );
}

#[tokio::test]
async fn a_compute_node_groups_idle_time_stays_absent_because_its_member_documents_none() {
    let router = emulator();
    let cluster = "defaults-group-cluster";
    let (status, created) = call(
        &router,
        "CreateCluster",
        &create_request(cluster, &cases()[4]),
    )
    .await;
    assert_eq!(status, 200, "case setup: got {status} with {created}");

    // `ComputeNodeGroupSlurmConfigurationRequest$scaleDownIdleTimeInSeconds` states no value
    // of its own — "If not specified, the cluster-level setting applies" — so a group that
    // names none reports none, and the cluster's own documented 600 is not copied into it.
    for (index, (case, members)) in [
        (
            "a Slurm configuration that names nothing",
            json!({"slurmConfiguration": {}}),
        ),
        (
            "a Slurm configuration that names only custom settings",
            json!({
                "slurmConfiguration": {
                    "slurmCustomSettings": [
                        {"parameterName": "Weight", "parameterValue": "10"},
                    ],
                },
            }),
        ),
        ("no Slurm configuration at all", json!({})),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("defaults-group-{index}");
        let (status, created) = call(
            &router,
            "CreateComputeNodeGroup",
            &create_group_request(cluster, &name, &members),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served, got {status} with {created}"
        );
        for (source, group) in [
            ("CreateComputeNodeGroup", created["computeNodeGroup"].clone()),
            (
                "GetComputeNodeGroup",
                stored_group(&router, case, cluster, &name).await,
            ),
        ] {
            assert!(
                group["slurmConfiguration"]["scaleDownIdleTimeInSeconds"].is_null(),
                "case {case:?} ({source}): expected the group's idle time left absent, got \
                 {group}"
            );
        }
    }

    // The CLUSTER's member, by contrast, is the one the model documents as set: the group's
    // absence is not the cluster's.
    let stored = stored_slurm_configuration(&router, "the cluster behind the groups", cluster).await;
    assert_eq!(
        stored["scaleDownIdleTimeInSeconds"],
        json!(DOCUMENTED_SCALE_DOWN_SECONDS),
        "expected the cluster to report the idle time its own member documents, got {stored}"
    );
}

#[tokio::test]
async fn a_compute_node_group_carries_the_purchase_option_its_member_documents() {
    let router = emulator();
    let cluster = "defaults-purchase-cluster";
    let (status, created) = call(
        &router,
        "CreateCluster",
        &create_request(cluster, &cases()[0]),
    )
    .await;
    assert_eq!(status, 200, "case setup: got {status} with {created}");

    // Row 1: a create that names no purchasing option reports the documented one. The rest:
    // every member the enum declares is stored exactly as it was sent, including the ones
    // nothing cited.
    let mut rows: Vec<(String, Value, &str)> = vec![(
        "no purchase option named".to_string(),
        json!({}),
        DOCUMENTED_PURCHASE_OPTION,
    )];
    for option in PURCHASE_OPTIONS {
        rows.push((
            format!("the {option} option named"),
            json!({"purchaseOption": option}),
            option,
        ));
    }

    let mut group_ids: Vec<String> = Vec::new();
    for (index, (case, members, expected)) in rows.iter().enumerate() {
        let name = format!("defaults-purchase-{index}");
        let (status, created) = call(
            &router,
            "CreateComputeNodeGroup",
            &create_group_request(cluster, &name, members),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served, got {status} with {created}"
        );
        group_ids.push(
            created["computeNodeGroup"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        for (source, group) in [
            ("CreateComputeNodeGroup", created["computeNodeGroup"].clone()),
            (
                "GetComputeNodeGroup",
                stored_group(&router, case, cluster, &name).await,
            ),
        ] {
            assert_eq!(
                group["purchaseOption"],
                json!(expected),
                "case {case:?} ({source}): expected purchaseOption {expected:?}, got {group}"
            );
        }
    }

    // And over an UPDATE of the group whose create named NO option: one that names another
    // member replaces the stored option, and one that names none leaves it where it is — the
    // documented value is carried by the record, not re-derived per call.
    let group_identifier = group_ids.first().cloned().unwrap_or_default();
    for (case, patch, expected) in [
        (
            "an update that names another option",
            json!({"purchaseOption": "SPOT"}),
            "SPOT",
        ),
        (
            "an update that names no option at all",
            json!({"amiId": "ami-0123456789abcdef0"}),
            "SPOT",
        ),
    ] {
        let mut body = json!({
            "clusterIdentifier": cluster,
            "computeNodeGroupIdentifier": group_identifier,
        });
        if let (Some(target), Some(extra)) = (body.as_object_mut(), patch.as_object()) {
            for (member, value) in extra {
                target.insert(member.clone(), value.clone());
            }
        }
        let (status, updated) = call(&router, "UpdateComputeNodeGroup", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be served, got {status} with {updated}"
        );
        for (source, group) in [
            ("UpdateComputeNodeGroup", updated["computeNodeGroup"].clone()),
            (
                "GetComputeNodeGroup",
                stored_group(&router, case, cluster, &group_identifier).await,
            ),
        ] {
            assert_eq!(
                group["purchaseOption"],
                json!(expected),
                "case {case:?} ({source}): expected purchaseOption {expected:?}, got {group}"
            );
        }
    }
}
