// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the filters `DescribeGlobalClusters` accepts.
//!
//! The rule under test: a global-database report accepts the filters this operation
//! publishes — `region`, which selects a global cluster with a member in one of the named
//! Amazon Web Services Regions, and `db-cluster-id`, which selects the global cluster a
//! named member DB CLUSTER belongs to — and a filter that matches nothing is an empty report
//! rather than a refusal. A filter name the operation does not publish is accepted too, and
//! narrows nothing: this report honours only the names above, so a name outside them is a
//! parameter it does not apply rather than one it refuses.
//!
//! The table spans what the rule generalises over: each form a DB cluster is named in (the
//! bare identifier, another case of it, and the ARN), a value naming a cluster that is not a
//! member and one naming no cluster at all, several values in one filter, two filters that
//! have to hold together, and the member in the region the request is signed for beside a
//! secondary in ANOTHER region.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly as
//! the production router mounts it, each request is a real awsQuery form POST carrying the
//! SigV4 credential scope the router keys on, and every assertion reads the XML the emulator
//! answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, call_in, emulator, expect_error, expect_ok, text, texts, REGION};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The region the secondary member of the global database lives in.
const OTHER_REGION: &str = "eu-west-1";

/// The engine the global database and its members run.
const ENGINE: &str = "aurora-postgresql";

/// A DB cluster's ARN, which carries the region the cluster lives in.
fn db_cluster_arn(region: &str, identifier: &str) -> String {
    format!("arn:aws:rds:{region}:{ACCOUNT}:cluster:{identifier}")
}

/// `DescribeGlobalClusters` carrying `filters` as the awsQuery form encodes a list of
/// structures: each entry's members carry the entry's 1-based index.
async fn described(
    router: &MultiServiceRouter,
    filters: &[(&str, &[&str])],
) -> (u16, String) {
    let mut params: Vec<(String, String)> = Vec::new();
    for (index, (name, values)) in filters.iter().enumerate() {
        params.push((
            format!("Filters.Filter.{}.Name", index + 1),
            (*name).to_string(),
        ));
        for (position, value) in values.iter().enumerate() {
            params.push((
                format!("Filters.Filter.{}.Values.Value.{}", index + 1, position + 1),
                (*value).to_string(),
            ));
        }
    }
    call(router, "DescribeGlobalClusters", &params).await
}

/// The global cluster identifiers a filtered report names.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    filters: &[(&str, &[&str])],
) -> Vec<String> {
    let body = expect_ok(case, described(router, filters).await);
    texts(&body, "GlobalClusterIdentifier")
}

/// A global database with two members — one in the region the requests are signed for and a
/// secondary in another — is found by the `db-cluster-id` filter under every form a member is
/// named in, and a value that names no member of it reports nothing.
#[tokio::test]
async fn the_db_cluster_id_filter_selects_the_global_database_a_member_belongs_to() {
    let router = emulator();
    let global = "emu-global-filtered";
    let primary = "emu-global-primary";
    let secondary = "emu-global-secondary";
    let standalone = "emu-standalone-cluster";

    expect_ok(
        "create the global database",
        call(
            &router,
            "CreateGlobalCluster",
            &[("GlobalClusterIdentifier", global), ("Engine", ENGINE)],
        )
        .await,
    );
    for (region, identifier) in [(REGION, primary), (OTHER_REGION, secondary)] {
        expect_ok(
            "create a member cluster",
            call_in(
                &router,
                region,
                "CreateDBCluster",
                &[
                    ("DBClusterIdentifier", identifier),
                    ("Engine", ENGINE),
                    ("GlobalClusterIdentifier", global),
                ],
            )
            .await,
        );
    }
    // A cluster of its own, member of no global database: the filter must not report the
    // global database for it.
    expect_ok(
        "create a standalone cluster",
        call(
            &router,
            "CreateDBCluster",
            &[("DBClusterIdentifier", standalone), ("Engine", ENGINE)],
        )
        .await,
    );

    let primary_arn = db_cluster_arn(REGION, primary);
    let secondary_arn = db_cluster_arn(OTHER_REGION, secondary);
    // (case, the values the `db-cluster-id` filter carries, the global databases reported)
    let rows: [(&str, &[&str], bool); 8] = [
        ("the member's own identifier", &[primary], true),
        ("the member's identifier in another case", &["EMU-GLOBAL-PRIMARY"], true),
        ("the member's ARN", &[primary_arn.as_str()], true),
        ("a secondary in another region, by identifier", &[secondary], true),
        ("a secondary in another region, by ARN", &[secondary_arn.as_str()], true),
        // A filter carrying several values selects a global database any one of them names.
        ("one value that matches among two", &["emu-no-such-cluster", secondary], true),
        ("a cluster that is a member of nothing", &[standalone], false),
        ("a value that names no cluster at all", &["emu-no-such-cluster"], false),
    ];

    for (case, values, matches) in rows {
        let found = reported(&router, case, &[("db-cluster-id", values)]).await;
        let expected: Vec<String> = if matches {
            vec![global.to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            found, expected,
            "case {case:?}: expected {expected:?} reported, got {found:?}"
        );
    }

    // Two filters have to hold TOGETHER: the member and a region it has a member in select
    // the global database, and a region it has no member in selects nothing.
    let found = reported(
        &router,
        "both filters hold",
        &[("db-cluster-id", &[primary]), ("region", &[OTHER_REGION])],
    )
    .await;
    assert_eq!(
        found,
        vec![global.to_string()],
        "case {:?}: expected the global database reported, got {found:?}",
        "both filters hold"
    );
    let found = reported(
        &router,
        "one filter does not hold",
        &[("db-cluster-id", &[primary]), ("region", &["ap-south-1"])],
    )
    .await;
    assert_eq!(
        found,
        Vec::<String>::new(),
        "case {:?}: expected no global database reported, got {found:?}",
        "one filter does not hold"
    );
}

/// With no global database at all, a supported filter is still a 200 with an empty list — the
/// report an empty result set is — and a filter name this operation does not support is
/// accepted too and narrows nothing.
#[tokio::test]
async fn a_supported_filter_that_matches_nothing_reports_an_empty_list() {
    let router = emulator();

    for (case, filter) in [
        ("the member filter", "db-cluster-id"),
        ("the region filter", "region"),
    ] {
        let body = expect_ok(case, described(&router, &[(filter, &["emu-nothing"])]).await);
        assert!(
            body.contains("<DescribeGlobalClustersResult>")
                && texts(&body, "GlobalClusterIdentifier").is_empty(),
            "case {case:?}: expected the empty success envelope, got {body}"
        );
    }

    for name in ["engine", "global-cluster-id", "Region", "db-cluster-name"] {
        let case = format!("the unsupported filter {name:?}");
        let body = expect_ok(&case, described(&router, &[(name, &["anything"])]).await);
        assert!(
            body.contains("<DescribeGlobalClustersResult>"),
            "case {case:?}: expected the success envelope, got {body}"
        );
    }
}

/// A filter name this operation does not support is ACCEPTED and does not narrow the report:
/// the global clusters it answers with are the ones it answers without any filter at all,
/// whatever values the filter carries — while the `GlobalClusterIdentifier` PARAMETER still
/// matches exactly and faults for an identifier no global cluster answers to.
///
/// The rows span what the rule generalises over: the filter name the generic `Filter` shape
/// would narrow another report by, one in another case than a supported name, one that names
/// no member of anything, and a value that names the owned global cluster beside one that was
/// never created.
#[tokio::test]
async fn a_filter_name_this_report_does_not_support_narrows_nothing() {
    let router = emulator();
    let global = "emu-global-tolerated";

    expect_ok(
        "create the global database",
        call(
            &router,
            "CreateGlobalCluster",
            &[("GlobalClusterIdentifier", global), ("Engine", ENGINE)],
        )
        .await,
    );

    // The report with no filter at all, which every filtered report below must match.
    let unfiltered = reported(&router, "no filter", &[]).await;
    assert_eq!(
        unfiltered,
        vec![global.to_string()],
        "case {:?}: expected the owned global database reported, got {unfiltered:?}",
        "no filter"
    );

    // (case, the unsupported filter name, the values it carries)
    let rows: [(&str, &str, &[&str]); 6] = [
        ("the identifier filter, naming the owned global database", "global-cluster-id", &[global]),
        ("the identifier filter, naming one never created", "global-cluster-id", &["emu-no-such-global"]),
        ("a supported name in another case", "Region", &[REGION]),
        ("a filter of another report", "engine", &[ENGINE]),
        ("a name that names no member of anything", "not-a-filter", &["anything"]),
        ("several values, none of which names anything", "global-cluster-id", &["one", "two"]),
    ];

    for (case, name, values) in rows {
        let found = reported(&router, case, &[(name, values)]).await;
        assert_eq!(
            found, unfiltered,
            "case {case:?}: an unsupported filter must not narrow the report: expected \
             {unfiltered:?}, got {found:?}"
        );
    }

    // An unsupported filter beside a SUPPORTED one leaves the supported one's narrowing
    // alone: the region filter still selects on the regions the global database has members
    // in, so a region it has no member in reports nothing.
    let found = reported(
        &router,
        "an unsupported filter beside a region the global database has no member in",
        &[("global-cluster-id", &[global]), ("region", &["ap-south-1"])],
    )
    .await;
    assert_eq!(
        found,
        Vec::<String>::new(),
        "case {:?}: expected no global database reported, got {found:?}",
        "an unsupported filter beside a region the global database has no member in"
    );

    // The PARAMETER is unchanged by any of that: it matches exactly and faults for an
    // identifier that names no global cluster.
    let refused = expect_error(
        "the identifier parameter naming nothing",
        call(
            &router,
            "DescribeGlobalClusters",
            &[("GlobalClusterIdentifier", "emu-no-such-global")],
        )
        .await,
        404,
        "GlobalClusterNotFoundFault",
    );
    assert_eq!(
        text(&refused, "Message"),
        "Global cluster 'emu-no-such-global' not found",
        "case {:?}: expected the not-found fault naming the identifier, got {refused}",
        "the identifier parameter naming nothing"
    );
}
