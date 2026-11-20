// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Performance Insights members of the `DBCluster` envelope.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on, so each assertion reads the XML the emulator itself produced.
//!
//! The rule under test: an optional `DBCluster` member the model gives no default is
//! reported only once a request has configured it, so a cluster carrying no Performance
//! Insights setting at all reports NO `PerformanceInsightsEnabled`,
//! `PerformanceInsightsKMSKeyId` or `PerformanceInsightsRetentionPeriod` — a value nothing
//! asked for would read as configuration the request never made — while a cluster whose
//! request named the setting reports what it named, `false` included.
//!
//! The table spans what the rule generalises over: both cluster TYPES `CreateDBCluster`
//! documents (Aurora DB clusters and Multi-AZ DB clusters), three engines across two
//! families, a create that names nothing beyond the required members, one that configures
//! Aurora Serverless v2 capacity and no monitoring, and creates that turn Performance
//! Insights explicitly on (with a retention period and a key) and explicitly off. The
//! second suite drives the same rule through `ModifyDBCluster`: a modification of other
//! settings leaves an unconfigured cluster unconfigured, and one that names the setting
//! makes the member appear from then on.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_clusters::{describe_one, the_cluster};
use crate::db_instances::{call, emulator, expect_ok, optional_text};
use crate::lifecycle_poll_loops::poll_until;

/// One row of the create table.
struct InsightsCase {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The parameters this row adds beyond `DBClusterIdentifier` and `Engine`.
    extra: &'static [(&'static str, &'static str)],
    /// The `PerformanceInsightsEnabled` the envelope must carry, or `None` for a cluster
    /// whose request configured no Performance Insights at all, which reports none.
    enabled: Option<&'static str>,
    /// The `PerformanceInsightsRetentionPeriod` the envelope must carry, or `None`.
    retention: Option<&'static str>,
    /// The `PerformanceInsightsKMSKeyId` the envelope must carry, or `None`.
    key: Option<&'static str>,
}

/// The settings a Multi-AZ DB cluster is required to carry, so a row of that cluster type
/// is created rather than refused.
const MULTI_AZ_SETTINGS: [(&str, &str); 4] = [
    ("AllocatedStorage", "100"),
    ("DBClusterInstanceClass", "db.m6gd.xlarge"),
    ("StorageType", "io1"),
    ("Iops", "1000"),
];

/// The clusters the reporting rule is asserted over.
const INSIGHTS_CASES: &[InsightsCase] = &[
    InsightsCase {
        case: "aurora-postgresql, nothing beyond the two required members",
        identifier: "emu-insights-cluster-pg",
        engine: "aurora-postgresql",
        extra: &[],
        enabled: None,
        retention: None,
        key: None,
    },
    InsightsCase {
        case: "aurora-mysql, nothing beyond the two required members",
        identifier: "emu-insights-cluster-mysql",
        engine: "aurora-mysql",
        extra: &[],
        enabled: None,
        retention: None,
        key: None,
    },
    InsightsCase {
        case: "aurora-postgresql with Serverless v2 capacity and no monitoring setting",
        identifier: "emu-insights-cluster-serverless",
        engine: "aurora-postgresql",
        extra: &[
            ("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "1.0"),
            ("StorageEncrypted", "true"),
        ],
        enabled: None,
        retention: None,
        key: None,
    },
    InsightsCase {
        case: "aurora-postgresql with Performance Insights turned on",
        identifier: "emu-insights-cluster-on",
        engine: "aurora-postgresql",
        // 7 is the default retention period `PerformanceInsightsRetentionPeriod`
        // publishes, so it is one of the values the member takes.
        extra: &[
            ("EnablePerformanceInsights", "true"),
            ("PerformanceInsightsRetentionPeriod", "7"),
            ("PerformanceInsightsKMSKeyId", "alias/aws/rds"),
        ],
        enabled: Some("true"),
        retention: Some("7"),
        key: Some("alias/aws/rds"),
    },
    InsightsCase {
        case: "aurora-mysql with Performance Insights turned explicitly off",
        identifier: "emu-insights-cluster-off",
        engine: "aurora-mysql",
        extra: &[("EnablePerformanceInsights", "false")],
        enabled: Some("false"),
        retention: None,
        key: None,
    },
    InsightsCase {
        case: "a Multi-AZ DB cluster with no Performance Insights setting",
        identifier: "emu-insights-cluster-multiaz",
        engine: "postgres",
        extra: &MULTI_AZ_SETTINGS,
        enabled: None,
        retention: None,
        key: None,
    },
    InsightsCase {
        case: "a Multi-AZ DB cluster with Performance Insights turned on",
        identifier: "emu-insights-cluster-multiaz-on",
        engine: "postgres",
        // The required Multi-AZ settings, plus the longest retention period the member
        // publishes ("731").
        extra: &[
            ("AllocatedStorage", "100"),
            ("DBClusterInstanceClass", "db.m6gd.xlarge"),
            ("StorageType", "io1"),
            ("Iops", "1000"),
            ("EnablePerformanceInsights", "true"),
            ("PerformanceInsightsRetentionPeriod", "731"),
        ],
        enabled: Some("true"),
        retention: Some("731"),
        key: None,
    },
];

/// `CreateDBCluster` for one row of the table.
async fn create_cluster(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
    extra: &[(&str, &str)],
) -> (u16, String) {
    let mut request: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
    ];
    request.extend(
        extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    call(router, "CreateDBCluster", &request).await
}

/// Assert the three Performance Insights members of one cluster envelope.
fn assert_reported(
    case: &str,
    label: &str,
    cluster: &str,
    enabled: Option<&str>,
    retention: Option<&str>,
    key: Option<&str>,
) {
    let case = format!("{case} ({label})");
    for (member, expected) in [
        ("PerformanceInsightsEnabled", enabled),
        ("PerformanceInsightsRetentionPeriod", retention),
        ("PerformanceInsightsKMSKeyId", key),
    ] {
        let actual = optional_text(cluster, member);
        assert_eq!(
            actual.as_deref(),
            expected,
            "case {case:?}: expected <{member}> {expected:?}, got {actual:?} in {cluster}"
        );
    }
}

#[tokio::test]
async fn a_cluster_reports_the_performance_insights_settings_its_request_configured() {
    let router = emulator();

    for row in INSIGHTS_CASES {
        let created = the_cluster(
            row.case,
            &expect_ok(
                row.case,
                create_cluster(&router, row.identifier, row.engine, row.extra).await,
            ),
        );
        assert_reported(
            row.case,
            "CreateDBCluster",
            &created,
            row.enabled,
            row.retention,
            row.key,
        );

        // One serializer, one envelope: the read reports exactly what the create did.
        let read = describe_one(&router, row.case, row.identifier).await;
        assert_reported(
            row.case,
            "DescribeDBClusters",
            &read,
            row.enabled,
            row.retention,
            row.key,
        );
    }
}

/// `ModifyDBCluster` with the parameters of one step, answered as the one `<DBCluster>` it
/// reports. The cluster is polled to `available` first, because a modification "can't be
/// performed while the cluster is in this state" otherwise.
async fn modify(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    settings: &[(&str, &str)],
) -> String {
    poll_until(
        case,
        "the cluster reaching available",
        || async {
            call(
                router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier)],
            )
            .await
        },
        |status, body| {
            status == 200
                && optional_text(&the_cluster(case, body), "Status").as_deref() == Some("available")
        },
    )
    .await;

    let mut request: Vec<(&str, String)> = vec![("DBClusterIdentifier", identifier.to_string())];
    request.extend(
        settings
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    the_cluster(
        case,
        &expect_ok(case, call(router, "ModifyDBCluster", &request).await),
    )
}

#[tokio::test]
async fn a_modification_reports_the_performance_insights_setting_only_once_it_names_one() {
    let router = emulator();
    let identifier = "emu-insights-cluster-modified";
    let created = the_cluster(
        "modified",
        &expect_ok(
            "modified",
            create_cluster(&router, identifier, "aurora-postgresql", &[]).await,
        ),
    );
    assert_reported("modified", "CreateDBCluster", &created, None, None, None);

    // A modification of ANOTHER setting configures no Performance Insights, so the cluster
    // still reports none.
    let other = modify(
        &router,
        "a modification naming another setting",
        identifier,
        &[("PreferredBackupWindow", "03:00-03:30")],
    )
    .await;
    assert_reported(
        "a modification naming another setting",
        "ModifyDBCluster",
        &other,
        None,
        None,
        None,
    );
    assert_reported(
        "a modification naming another setting",
        "DescribeDBClusters",
        &describe_one(&router, "a modification naming another setting", identifier).await,
        None,
        None,
        None,
    );

    // Turning it ON makes the member appear where it was absent, and a later modification
    // turning it OFF reports the setting it now carries rather than removing the member.
    for (case, named, expected) in [
        ("turned on by a modification", "true", "true"),
        ("turned off by a modification", "false", "false"),
    ] {
        let modified = modify(
            &router,
            case,
            identifier,
            &[("EnablePerformanceInsights", named)],
        )
        .await;
        assert_reported(case, "ModifyDBCluster", &modified, Some(expected), None, None);
        assert_reported(
            case,
            "DescribeDBClusters",
            &describe_one(&router, case, identifier).await,
            Some(expected),
            None,
            None,
        );
    }
}
