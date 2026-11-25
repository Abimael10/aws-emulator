// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of the cluster-not-found refusal of the BACKTRACK API:
//! `BacktrackDBCluster`, which performs a backtrack on an Aurora MySQL DB cluster, and
//! `DescribeDBClusterBacktracks`, which reports the backtracks performed on one.
//!
//! The rule under test: every call of this API whose `DBClusterIdentifier` resolves to no DB
//! cluster of the region the request was signed for is refused with the modelled
//! `DBClusterNotFoundFault` — HTTP 404, wire code `DBClusterNotFoundFault` — carrying this
//! API's single fixed sentence, which has NO variable part: neither the identifier the
//! caller wrote nor anything else of the request is interpolated into it. That sets the API
//! apart from this front door's other cluster readers, which name the identifier they could
//! not resolve, so a client matching the sentence must see one text and not one per
//! identifier.
//!
//! The table spans what the rule generalises over: both operations of the API; an identifier
//! of one character, a plain lower-case name, a mixed-case spelling (which the store folds
//! for the lookup), one carrying digits and hyphens, one at the longest length a DB cluster
//! reference may carry (`MAX_DB_CLUSTER_LOOKUP_IDENTIFIER_LEN`, the published bound) and the
//! ARN form the members also accept; a cluster that EXISTS in another region, so the absence
//! under test is a regional one rather than a spelling; and a cluster that exists in the
//! signed-for region, which both calls SERVE rather than refuse — so the sentence reports
//! what the store holds rather than a shape of the request.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST, and every wait is the one poll loop this test target shares
//! (`lifecycle_poll_loops::poll_until`) on a REPORTING call.

use service_com_amazonaws_rds_amazonrdsv19::state::MAX_DB_CLUSTER_LOOKUP_IDENTIFIER_LEN;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, subtrees, text, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// A second region, so a cluster that exists is absent from the scope of a call signed for
/// another region.
const OTHER_REGION: &str = "eu-west-1";

/// The sentence Amazon RDS answers a `DBClusterNotFoundFault` with across the backtrack API.
/// The whole sentence is fixed text: nothing of the request appears in it.
const BACKTRACK_CLUSTER_NOT_FOUND: &str = "Could not find specified DB Cluster";

/// The status a DB cluster reports once the clock has settled its create.
const AVAILABLE: &str = "available";

/// A `BacktrackWindow` that turns backtracking on, well inside the published range.
const WINDOW: i64 = 3600;

/// The identifiers the table sends for a cluster nothing created: one character, a plain
/// lower-case name, a mixed-case spelling, one carrying digits and hyphens, and one at the
/// longest length a DB cluster reference may carry.
fn absent_identifiers() -> Vec<String> {
    vec![
        "a".to_string(),
        "emu-bkt-absent".to_string(),
        "EMU-BKT-Absent".to_string(),
        "emu-bkt-7-42".to_string(),
        at_the_longest("emu-bkt-long-"),
    ]
}

/// `prefix` grown to the longest DB cluster reference this service admits
/// (`MAX_DB_CLUSTER_LOOKUP_IDENTIFIER_LEN`), so the row sends the published bound without a
/// hand-counted literal.
fn at_the_longest(prefix: &str) -> String {
    let mut named = prefix.to_string();
    while named.len() < MAX_DB_CLUSTER_LOOKUP_IDENTIFIER_LEN {
        named.push('x');
    }
    assert_eq!(
        named.len(),
        MAX_DB_CLUSTER_LOOKUP_IDENTIFIER_LEN,
        "the padded identifier must sit at the published bound, got {named:?}"
    );
    named
}

/// `CreateDBCluster` for an Aurora MySQL cluster with backtracking on, in `region`.
async fn create_cluster(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", identifier),
            ("Engine", "aurora-mysql"),
            ("MasterUsername", "clusteradmin"),
            ("MasterUserPassword", "a-strong-password-1"),
            ("BacktrackWindow", &WINDOW.to_string()),
        ],
    )
    .await
}

/// `DescribeDBClusterBacktracks` for one cluster reference, in `region`.
async fn report_in(router: &MultiServiceRouter, region: &str, named: &str) -> (u16, String) {
    call_in(
        router,
        region,
        "DescribeDBClusterBacktracks",
        &[("DBClusterIdentifier", named)],
    )
    .await
}

/// `BacktrackDBCluster` for one cluster reference, in `region`.
///
/// `UseEarliestTimeOnPointInTimeUnavailable` is what lets one fixed instant stand for "the
/// earliest time the cluster kept", whatever the clock says when the test runs.
async fn backtrack_in(router: &MultiServiceRouter, region: &str, named: &str) -> (u16, String) {
    call_in(
        router,
        region,
        "BacktrackDBCluster",
        &[
            ("DBClusterIdentifier", named),
            ("BacktrackTo", "1970-01-01T00:00:00Z"),
            ("UseEarliestTimeOnPointInTimeUnavailable", "true"),
        ],
    )
    .await
}

/// Poll `DescribeDBClusters` until the cluster reports [`AVAILABLE`], and answer the one
/// `<DBCluster>` it reported.
async fn poll_until_available(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let params = [("DBClusterIdentifier", identifier)];
    let read = || call(router, "DescribeDBClusters", &params);
    let (body, _polls) = poll_until(
        case,
        &format!("{identifier} reporting {AVAILABLE}"),
        read,
        |http, body| http == 200 && optional_text(body, "Status").as_deref() == Some(AVAILABLE),
    )
    .await;
    let found = subtrees(&body, "DBCluster");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one DBCluster, got {found:?}"
    );
    found.into_iter().next().expect("checked just above")
}

/// Assert the modelled refusal of the backtrack API and its single fixed sentence.
fn expect_backtrack_cluster_refusal(case: &str, response: (u16, String), named: &str) {
    let body = expect_error(case, response, 404, "DBClusterNotFoundFault");
    let message = text(&body, "Message");
    assert_eq!(
        message, BACKTRACK_CLUSTER_NOT_FOUND,
        "case {case:?}: expected the backtrack API's fixed sentence, got {message:?} in {body}"
    );
    assert!(
        !message.contains(named),
        "case {case:?}: the sentence has no variable part, so it must not carry {named:?}, \
         got {message:?}"
    );
}

#[tokio::test]
async fn both_backtrack_calls_refuse_an_unresolved_cluster_with_one_fixed_sentence() {
    let router = emulator();

    for named in absent_identifiers() {
        expect_backtrack_cluster_refusal(
            &format!("DescribeDBClusterBacktracks for {named:?}"),
            report_in(&router, REGION, &named).await,
            &named,
        );
        expect_backtrack_cluster_refusal(
            &format!("BacktrackDBCluster for {named:?}"),
            backtrack_in(&router, REGION, &named).await,
            &named,
        );
    }

    // A cluster that EXISTS in another region is absent from the scope of a call signed for
    // this one, and is reported absent in the very same words.
    expect_ok(
        "the cluster of the other region",
        create_cluster(&router, OTHER_REGION, "emu-bkt-elsewhere").await,
    );
    expect_backtrack_cluster_refusal(
        "DescribeDBClusterBacktracks for a cluster of another region",
        report_in(&router, REGION, "emu-bkt-elsewhere").await,
        "emu-bkt-elsewhere",
    );
    expect_backtrack_cluster_refusal(
        "BacktrackDBCluster for a cluster of another region",
        backtrack_in(&router, REGION, "emu-bkt-elsewhere").await,
        "emu-bkt-elsewhere",
    );
}

#[tokio::test]
async fn the_arn_form_of_an_unresolved_cluster_carries_the_same_sentence() {
    let router = emulator();
    expect_ok(
        "the cluster the ARN form is read off",
        create_cluster(&router, REGION, "emu-bkt-live").await,
    );
    let live = poll_until_available(&router, "the created cluster", "emu-bkt-live").await;
    let arn = text(&live, "DBClusterArn");

    // The members accept "the user-supplied DB cluster identifier or the Amazon Resource
    // Name (ARN)", so an ARN naming a cluster nothing created resolves to nothing too — read
    // off a real ARN this service minted, rather than hand-written.
    let absent_arn = arn.replace("emu-bkt-live", "emu-bkt-absent-arn");
    assert_ne!(
        absent_arn, arn,
        "the absent ARN must differ from the one the cluster published, got {absent_arn:?}"
    );
    expect_backtrack_cluster_refusal(
        "DescribeDBClusterBacktracks for an ARN naming no cluster",
        report_in(&router, REGION, &absent_arn).await,
        &absent_arn,
    );
    expect_backtrack_cluster_refusal(
        "BacktrackDBCluster for an ARN naming no cluster",
        backtrack_in(&router, REGION, &absent_arn).await,
        &absent_arn,
    );

    // The ARN of the cluster that EXISTS is SERVED by both calls, so the refusal above
    // reports what the store holds rather than the form of the reference.
    let served = expect_ok(
        "the report addressed by the cluster's own ARN",
        report_in(&router, REGION, &arn).await,
    );
    assert!(
        subtrees(&served, "DBClusterBacktrack").is_empty(),
        "a cluster nothing backtracked reports no backtrack, got {served}"
    );
    let performed = expect_ok(
        "the backtrack addressed by the cluster's own ARN",
        backtrack_in(&router, REGION, &arn).await,
    );
    assert_eq!(
        text(&performed, "DBClusterIdentifier"),
        "emu-bkt-live",
        "the backtrack must name the cluster the ARN resolved to, got {performed}"
    );
    let after = expect_ok(
        "the report after the backtrack",
        report_in(&router, REGION, "emu-bkt-live").await,
    );
    assert_eq!(
        subtrees(&after, "DBClusterBacktrack").len(),
        1,
        "the report must carry the backtrack just performed, got {after}"
    );
}
