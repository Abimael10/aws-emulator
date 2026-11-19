// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two cluster-envelope members this front door computes from
//! metadata of the ENGINE and of the REGION rather than from the request:
//! `AutoMinorVersionUpgrade` and `HostedZoneId`.
//!
//! The rules under test:
//!
//! 1. `AutoMinorVersionUpgrade` is the ENGINE's own control-plane default when the request
//!    names nothing — on for the cluster types
//!    `CreateDBClusterMessage$AutoMinorVersionUpgrade` applies to ("Valid for Cluster Type:
//!    Aurora DB clusters and Multi-AZ DB cluster", "by default, minor engine upgrades are
//!    applied automatically"), off for an engine whose own control plane publishes no such
//!    member and whose front door reports a created cluster as not taking them — and a
//!    request that NAMES the member gets the value it asked for, whatever the engine.
//! 2. `HostedZoneId` is "the ID that Amazon Route 53 assigns when you create a hosted zone":
//!    one zone per region AND per the DNS domain of the control plane that owns the engine,
//!    so every cluster of one engine in one region reports the SAME id, two engines served
//!    under different domains report different ids, and where this emulator can cite the id
//!    AWS published for that pair it reports that published id rather than one of its own.
//!
//! Both rules hold whichever operation answers with the cluster, so every row is read
//! through the create, the describe and a modification in turn.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answered with.

use service_com_amazonaws_rds_amazonrdsv19::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_clusters::the_cluster;
use crate::db_instances::{call, call_in, emulator, expect_ok, optional_text, text, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// The status a cluster reaches once the transition the create owes it has settled, which is
/// the state a modification is accepted in.
const AVAILABLE: &str = "available";

/// One row of the automatic-minor-version-upgrade table.
struct UpgradeCase {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// What the create asks for, or `None` for a create that does not name the member at all.
    requested: Option<&'static str>,
    /// What every report of the cluster must carry.
    expected: &'static str,
}

/// The rows span what the rule generalises over: the control plane that owns the engine (the
/// graph plane, the document plane and this door's own Aurora engines), and whether the
/// request names the member, names it ON or names it OFF.
const UPGRADE_CASES: &[UpgradeCase] = &[
    UpgradeCase {
        case: "a graph cluster whose create names nothing",
        identifier: "emu-amvu-neptune",
        engine: "neptune",
        requested: None,
        expected: "false",
    },
    UpgradeCase {
        case: "a graph cluster whose create asks for automatic upgrades",
        identifier: "emu-amvu-neptune-on",
        engine: "neptune",
        requested: Some("true"),
        expected: "true",
    },
    UpgradeCase {
        case: "a graph cluster whose create turns them off explicitly",
        identifier: "emu-amvu-neptune-off",
        engine: "neptune",
        requested: Some("false"),
        expected: "false",
    },
    UpgradeCase {
        case: "a document cluster whose create names nothing",
        identifier: "emu-amvu-docdb",
        engine: "docdb",
        requested: None,
        expected: "true",
    },
    UpgradeCase {
        case: "an Aurora MySQL cluster whose create names nothing",
        identifier: "emu-amvu-aurora",
        engine: "aurora-mysql",
        requested: None,
        expected: "true",
    },
    UpgradeCase {
        case: "an Aurora PostgreSQL cluster whose create turns them off",
        identifier: "emu-amvu-aurora-off",
        engine: "aurora-postgresql",
        requested: Some("false"),
        expected: "false",
    },
];

/// `CreateDBCluster` in `region` for one row's engine, with `extra` beyond the two required
/// members.
async fn create_cluster(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
    engine: &str,
    extra: &[(&str, String)],
) -> (u16, String) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
    ];
    if engine == "docdb" {
        // The document plane's own `CreateDBCluster` requires the master user; the graph
        // plane assigns one and the Aurora engines take this model's defaults.
        params.push(("MasterUsername", "emuadmin".to_string()));
        params.push(("MasterUserPassword", "emu-Secret-99".to_string()));
    }
    params.extend(extra.iter().cloned());
    call_in(router, region, "CreateDBCluster", &params).await
}

/// Poll `DescribeDBClusters` in `region` until the cluster reports `available`, and hand back
/// the `<DBCluster>` it settled on.
async fn available_cluster(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    identifier: &str,
) -> String {
    let params = vec![("DBClusterIdentifier", identifier.to_string())];
    let read = || call_in(router, region, "DescribeDBClusters", &params);
    let (body, _) = poll_until(
        case,
        &format!("{identifier} reaching {AVAILABLE}"),
        read,
        |http_status, body| {
            http_status == 200
                && the_cluster_or_none(body)
                    .is_some_and(|cluster| text(&cluster, "Status") == AVAILABLE)
        },
    )
    .await;
    the_cluster(case, &body)
}

/// The one `<DBCluster>` of a report that carries one, for a poll predicate that must not
/// panic on a report that carries none yet.
fn the_cluster_or_none(xml: &str) -> Option<String> {
    crate::db_instances::subtrees(xml, "DBCluster")
        .into_iter()
        .next()
}

#[tokio::test]
async fn a_cluster_reports_its_engines_automatic_minor_version_upgrade_default() {
    let router = emulator();

    for row in UPGRADE_CASES {
        let extra: Vec<(&str, String)> = row
            .requested
            .map(|value| vec![("AutoMinorVersionUpgrade", value.to_string())])
            .unwrap_or_default();
        let created = the_cluster(
            row.case,
            &expect_ok(
                row.case,
                create_cluster(&router, REGION, row.identifier, row.engine, &extra).await,
            ),
        );
        let reported = optional_text(&created, "AutoMinorVersionUpgrade");
        assert_eq!(
            reported.as_deref(),
            Some(row.expected),
            "case {:?} (CreateDBCluster): expected <AutoMinorVersionUpgrade> {:?}, got \
             {reported:?} in {created}",
            row.case,
            row.expected
        );

        // ROUND TRIP: the setting is PERSISTED, so the read-back reports the same value.
        let described = available_cluster(&router, REGION, row.case, row.identifier).await;
        let read_back = optional_text(&described, "AutoMinorVersionUpgrade");
        assert_eq!(
            read_back.as_deref(),
            Some(row.expected),
            "case {:?} (DescribeDBClusters): expected <AutoMinorVersionUpgrade> {:?}, got \
             {read_back:?} in {described}",
            row.case,
            row.expected
        );

        // A modification of something else leaves the setting standing.
        let modified = the_cluster(
            row.case,
            &expect_ok(
                row.case,
                call(
                    &router,
                    "ModifyDBCluster",
                    &[
                        ("DBClusterIdentifier", row.identifier),
                        ("BackupRetentionPeriod", "7"),
                    ],
                )
                .await,
            ),
        );
        let after_modify = optional_text(&modified, "AutoMinorVersionUpgrade");
        assert_eq!(
            after_modify.as_deref(),
            Some(row.expected),
            "case {:?} (ModifyDBCluster): a modification of another member must leave \
             <AutoMinorVersionUpgrade> {:?}, got {after_modify:?} in {modified}",
            row.case,
            row.expected
        );
    }
}

#[tokio::test]
async fn a_modification_moves_the_automatic_minor_version_upgrade_setting_either_way() {
    // The default is only a default: the setting is the cluster's own state, so a
    // modification moves it on an engine whose default is off and off on one whose default
    // is on, and the next read reports what was written.
    let router = emulator();
    let rows: [(&str, &str, &str, &str); 2] = [
        (
            "a graph cluster turned on",
            "emu-amvu-mod-neptune",
            "neptune",
            "true",
        ),
        (
            "an Aurora MySQL cluster turned off",
            "emu-amvu-mod-aurora",
            "aurora-mysql",
            "false",
        ),
    ];

    for (case, identifier, engine, asked) in rows {
        expect_ok(
            case,
            create_cluster(&router, REGION, identifier, engine, &[]).await,
        );
        available_cluster(&router, REGION, case, identifier).await;

        let modified = the_cluster(
            case,
            &expect_ok(
                case,
                call(
                    &router,
                    "ModifyDBCluster",
                    &[
                        ("DBClusterIdentifier", identifier),
                        ("AutoMinorVersionUpgrade", asked),
                    ],
                )
                .await,
            ),
        );
        let answered = optional_text(&modified, "AutoMinorVersionUpgrade");
        assert_eq!(
            answered.as_deref(),
            Some(asked),
            "case {case:?} (ModifyDBCluster): expected <AutoMinorVersionUpgrade> {asked:?}, \
             got {answered:?} in {modified}"
        );

        let described = available_cluster(&router, REGION, case, identifier).await;
        let read_back = optional_text(&described, "AutoMinorVersionUpgrade");
        assert_eq!(
            read_back.as_deref(),
            Some(asked),
            "case {case:?} (DescribeDBClusters): the modification must be persisted, expected \
             {asked:?}, got {read_back:?} in {described}"
        );
    }
}

/// One row of the hosted-zone table: the case, the region the cluster is created in, the
/// engine it runs, and the id AWS publishes for that region and that engine's endpoint
/// domain — `None` for a pair this emulator cites no published id for.
///
/// The published ids are the "Amazon Relational Database Service (Amazon RDS)" and "Amazon
/// Neptune" rows of the Route 53 hosted zone ID table (Amazon Route 53 Developer Guide,
/// "Values that you specify when you create alias resource record sets"), written out here
/// rather than read from `state::hosted_zone_id` so the assertion is independent of the
/// table under test. The rows cover the published entries of both domains and a region with
/// no published entry at all.
type ZoneRow = (&'static str, &'static str, &'static str, Option<&'static str>);

const ZONE_ROWS: &[ZoneRow] = &[
    (
        "a graph cluster in the region the graph plane publishes a zone for",
        "us-west-2",
        "neptune",
        Some("Z22EZDAJT6W257"),
    ),
    (
        "an Aurora cluster in that same region, under this door's own domain",
        "us-west-2",
        "aurora-mysql",
        Some("Z1PVIF0B656C1W"),
    ),
    (
        "an Aurora cluster in the region this door publishes its first zone for",
        "us-east-1",
        "aurora-postgresql",
        Some("Z2R2ITUGPM61AM"),
    ),
    (
        "a graph cluster in a region no published id is cited for",
        "eu-west-1",
        "neptune",
        None,
    ),
];

/// Assert `zone` is a well-formed Route 53 hosted zone id: the `Z` prefix every one of them
/// carries, followed by the digits-and-uppercase alphabet AWS writes them in.
fn assert_well_formed(case: &str, zone: &str) {
    assert!(
        zone.starts_with('Z')
            && zone.len() > 1
            && zone[1..]
                .chars()
                .all(|character| character.is_ascii_digit() || character.is_ascii_uppercase()),
        "case {case:?}: expected a Z-prefixed hosted zone id, got {zone:?}"
    );
}

#[tokio::test]
async fn every_cluster_of_one_engine_in_one_region_reports_that_regions_published_zone() {
    let router = emulator();

    for (index, (case, region, engine, published)) in ZONE_ROWS.iter().enumerate() {
        // TWO clusters per row, because the invariance is the point: one zone per region and
        // domain, whatever the cluster.
        let identifiers = [format!("emu-zone-{index}-a"), format!("emu-zone-{index}-b")];
        let mut reported: Vec<String> = Vec::new();
        for identifier in &identifiers {
            let created = the_cluster(
                case,
                &expect_ok(
                    case,
                    create_cluster(&router, region, identifier, engine, &[]).await,
                ),
            );
            let zone = optional_text(&created, "HostedZoneId").unwrap_or_else(|| {
                panic!("case {case:?}: the create must report a <HostedZoneId>: {created}")
            });
            assert_well_formed(case, &zone);
            if let Some(expected) = published {
                assert_eq!(
                    zone, *expected,
                    "case {case:?}: expected the published <HostedZoneId> {expected:?}, got \
                     {zone:?} in {created}"
                );
            }

            // ROUND TRIP: the read-back reports the same zone, so a client that re-reads the
            // cluster to build a Route 53 alias record gets one answer.
            let described = available_cluster(&router, region, case, identifier).await;
            let read_back = optional_text(&described, "HostedZoneId");
            assert_eq!(
                read_back.as_deref(),
                Some(zone.as_str()),
                "case {case:?} (DescribeDBClusters): expected {zone:?}, got {read_back:?}"
            );

            // And the modification envelope, the third path that answers with a cluster.
            let modified = the_cluster(
                case,
                &expect_ok(
                    case,
                    call_in(
                        &router,
                        region,
                        "ModifyDBCluster",
                        &[
                            ("DBClusterIdentifier", identifier.as_str()),
                            ("BackupRetentionPeriod", "9"),
                        ],
                    )
                    .await,
                ),
            );
            let after_modify = optional_text(&modified, "HostedZoneId");
            assert_eq!(
                after_modify.as_deref(),
                Some(zone.as_str()),
                "case {case:?} (ModifyDBCluster): expected {zone:?}, got {after_modify:?}"
            );
            reported.push(zone);
        }
        assert_eq!(
            reported.first(),
            reported.last(),
            "case {case:?}: every cluster of one engine in one region sits in ONE hosted zone, \
             got {reported:?}"
        );
    }
}

#[tokio::test]
async fn two_engines_served_under_different_domains_report_different_zones() {
    // The zone is per region AND per the control plane's DNS domain, so two clusters of
    // different engines in the SAME region do not share one.
    let router = emulator();
    let region = "ap-southeast-2";
    let mut zones: Vec<(&str, String)> = Vec::new();
    for (engine, identifier) in [
        ("neptune", "emu-zone-domain-graph"),
        ("aurora-mysql", "emu-zone-domain-aurora"),
    ] {
        let case = format!("{engine} in {region}");
        let created = the_cluster(
            &case,
            &expect_ok(
                &case,
                create_cluster(&router, region, identifier, engine, &[]).await,
            ),
        );
        let zone = optional_text(&created, "HostedZoneId")
            .unwrap_or_else(|| panic!("case {case:?}: the create must report a <HostedZoneId>"));
        assert_well_formed(&case, &zone);
        // The one reading of the pair this service computes the id from.
        assert_eq!(
            zone,
            state::hosted_zone_id(region, state::endpoint_domain(engine)),
            "case {case:?}: the envelope must report the zone of the cluster's region and \
             domain, got {zone:?}"
        );
        zones.push((engine, zone));
    }
    assert_ne!(
        zones[0].1, zones[1].1,
        "case \"two domains in one region\": expected a zone per domain, got {zones:?}"
    );
}
