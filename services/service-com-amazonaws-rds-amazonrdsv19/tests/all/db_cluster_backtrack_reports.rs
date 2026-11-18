// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeDBClusterBacktracks`: the report of the backtracks
//! `BacktrackDBCluster` performed on an Aurora MySQL DB cluster.
//!
//! The rules under test:
//!
//! * A backtrack is KEPT by the cluster it was performed on, so the identifier the backtrack
//!   answered with resolves to a record that carries the same instants and status a later
//!   read reports — a report of a cluster nothing was backtracked on is empty rather than
//!   invented, and one cluster's backtracks are never reported under another's.
//! * The report is narrowed by what the model says narrows it: `BacktrackIdentifier` selects
//!   the one backtrack it names and is the modelled 404 when it names none, each published
//!   filter selects on the member it names, a filter name the report does not publish is
//!   refused, and the page is cut by `MaxRecords` with a `Marker` that resumes beyond the
//!   record it was issued for.
//! * A `DBClusterIdentifier` that names no cluster of the signed-for region is the modelled
//!   `DBClusterNotFoundFault`, and one naming a cluster of another engine is refused as a
//!   value the parameter does not accept, since the action "only applies to Aurora MySQL DB
//!   clusters".
//!
//! The tables span what those rules generalise over: a report with none, one and several
//! backtracks, both published filter names with a value that matches and one that does not,
//! the three refusals the narrowing members have, and a page boundary with and without a
//! marker.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST, and every wait is the one poll loop this test target shares
//! (`lifecycle_poll_loops::poll_until`) on a REPORTING call.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The status a DB cluster reports once the clock has settled its create.
const AVAILABLE: &str = "available";

/// The status a backtrack this service has applied reports.
const COMPLETED: &str = "completed";

/// A `BacktrackWindow` that turns backtracking on, well inside the published range.
const WINDOW: i64 = 3600;

/// Create an Aurora MySQL DB cluster with backtracking on, and poll it to [`AVAILABLE`].
async fn a_backtrackable_cluster(router: &MultiServiceRouter, identifier: &str) -> String {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "Engine" => "aurora-mysql",
                "MasterUsername" => "clusteradmin",
                "MasterUserPassword" => "a-strong-password-1",
                "BacktrackWindow" => WINDOW,
            ],
        )
        .await,
    );
    poll_until_available(router, identifier, identifier).await
}

/// `DescribeDBClusters` naming one cluster.
async fn describe_cluster(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBClusters",
        &params!["DBClusterIdentifier" => identifier],
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
    let (body, _polls) = poll_until(
        case,
        &format!("{identifier} reporting {AVAILABLE}"),
        || describe_cluster(router, identifier),
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

/// Backtrack `identifier` to the earliest consistent time it kept, and answer the
/// `<BacktrackDBClusterResult>` body.
///
/// `UseEarliestTimeOnPointInTimeUnavailable` is what lets one fixed instant stand for "the
/// earliest time the cluster kept", whatever the clock says when the test runs.
async fn backtrack(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    expect_ok(
        case,
        call(
            router,
            "BacktrackDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "BacktrackTo" => "1970-01-01T00:00:00Z",
                "UseEarliestTimeOnPointInTimeUnavailable" => true,
            ],
        )
        .await,
    )
}

/// `DescribeDBClusterBacktracks` with `extra` beyond the required cluster identifier.
async fn report(
    router: &MultiServiceRouter,
    identifier: &str,
    extra: &[(&str, String)],
) -> (u16, String) {
    let mut settings: Vec<(&str, String)> = vec![("DBClusterIdentifier", identifier.to_string())];
    settings.extend(extra.iter().cloned());
    call(router, "DescribeDBClusterBacktracks", &settings).await
}

/// The `<DBClusterBacktrack>` entries of a report.
fn entries(body: &str) -> Vec<String> {
    subtrees(body, "DBClusterBacktrack")
}

/// One backtrack's `BacktrackIdentifier`.
fn identifier_of(entry: &str) -> String {
    text(entry, "BacktrackIdentifier")
}

#[tokio::test]
async fn a_performed_backtrack_reads_back_through_the_report_of_its_cluster() {
    let router = emulator();
    a_backtrackable_cluster(&router, "emu-btr-reported").await;
    // A cluster nothing has backtracked reports no backtrack at all, and no marker: there is
    // nothing to resume.
    let empty = expect_ok(
        "a cluster nothing backtracked",
        report(&router, "emu-btr-reported", &[]).await,
    );
    assert!(
        entries(&empty).is_empty(),
        "a cluster nothing backtracked must report no backtrack, got {empty}"
    );
    assert_eq!(
        optional_text(&empty, "Marker"),
        None,
        "a report that served every record must carry no marker, got {empty}"
    );

    // Two backtracks, so the report is held to more than the one record.
    let first = backtrack(&router, "the first backtrack", "emu-btr-reported").await;
    let second = backtrack(&router, "the second backtrack", "emu-btr-reported").await;
    let performed = [&first, &second];

    let listed = expect_ok(
        "the report after two backtracks",
        report(&router, "emu-btr-reported", &[]).await,
    );
    let reported = entries(&listed);
    assert_eq!(
        reported.len(),
        performed.len(),
        "the report must carry one record per backtrack performed, got {listed}"
    );
    // Each record carries what the call that performed it answered: a backtrack that
    // reported nothing of the kind would not survive this.
    for (case, (answered, entry)) in performed.iter().zip(reported.iter()).enumerate() {
        for member in [
            "BacktrackIdentifier",
            "DBClusterIdentifier",
            "BacktrackTo",
            "BacktrackedFrom",
            "BacktrackRequestCreationTime",
            "Status",
        ] {
            let kept = text(entry, member);
            assert_eq!(
                kept,
                text(answered, member),
                "case {case:?}: the report must carry the {member} the backtrack answered, \
                 got {kept:?} in {entry}"
            );
        }
        assert_eq!(
            text(entry, "Status"),
            COMPLETED,
            "case {case:?}: a backtrack this service applied reports completed, got {entry}"
        );
        assert_eq!(
            text(entry, "DBClusterIdentifier"),
            "emu-btr-reported",
            "case {case:?}: the record names the cluster it was performed on, got {entry}"
        );
    }
    // The two backtracks are distinct records, each under its own identifier.
    assert_ne!(
        identifier_of(&reported[0]),
        identifier_of(&reported[1]),
        "each backtrack carries its own identifier, got {listed}"
    );

    // Another cluster's backtrack is reported under that cluster and nowhere else.
    a_backtrackable_cluster(&router, "emu-btr-other").await;
    let elsewhere = backtrack(&router, "the other cluster's backtrack", "emu-btr-other").await;
    let other = expect_ok(
        "the other cluster's report",
        report(&router, "emu-btr-other", &[]).await,
    );
    let other_entries = entries(&other);
    assert_eq!(
        other_entries.len(),
        1,
        "the other cluster reports only its own backtrack, got {other}"
    );
    assert_eq!(
        identifier_of(&other_entries[0]),
        text(&elsewhere, "BacktrackIdentifier"),
        "the other cluster reports the backtrack performed on it, got {other}"
    );
    let unchanged = expect_ok(
        "the first cluster after the other was backtracked",
        report(&router, "emu-btr-reported", &[]).await,
    );
    assert_eq!(
        entries(&unchanged).len(),
        performed.len(),
        "a backtrack of another cluster must not appear under this one, got {unchanged}"
    );
}

#[tokio::test]
async fn the_report_is_narrowed_by_the_identifier_and_by_each_published_filter() {
    let router = emulator();
    a_backtrackable_cluster(&router, "emu-btr-narrowed").await;
    let first = identifier_of(&backtrack(&router, "the first", "emu-btr-narrowed").await);
    let second = identifier_of(&backtrack(&router, "the second", "emu-btr-narrowed").await);

    // `BacktrackIdentifier` reports the one backtrack it names.
    for wanted in [&first, &second] {
        let one = expect_ok(
            "one backtrack by identifier",
            report(
                &router,
                "emu-btr-narrowed",
                &params!["BacktrackIdentifier" => wanted],
            )
            .await,
        );
        let found = entries(&one);
        assert_eq!(
            found.len(),
            1,
            "case {wanted:?}: naming a backtrack reports that one alone, got {one}"
        );
        assert_eq!(
            identifier_of(&found[0]),
            *wanted,
            "case {wanted:?}: the record reported must be the one named, got {one}"
        );
    }

    // The published filters: `db-cluster-backtrack-id` accepts backtrack identifiers and
    // `db-cluster-backtrack-status` the status values — with a value that matches, and one
    // that is of the same kind and matches nothing.
    let cases: &[(&str, &str, usize)] = &[
        ("db-cluster-backtrack-id", first.as_str(), 1),
        ("db-cluster-backtrack-status", COMPLETED, 2),
        // "pending" is a published status value, which no backtrack here is in.
        ("db-cluster-backtrack-status", "pending", 0),
    ];
    for (name, value, expected) in cases {
        let filtered = expect_ok(
            name,
            report(
                &router,
                "emu-btr-narrowed",
                &params![
                    "Filters.Filter.1.Name" => name,
                    "Filters.Filter.1.Values.Value.1" => value,
                ],
            )
            .await,
        );
        assert_eq!(
            entries(&filtered).len(),
            *expected,
            "case {name:?}={value:?}: expected {expected} record(s), got {filtered}"
        );
    }

    // A filter name this report does not publish is refused rather than ignored.
    expect_error(
        "a filter the report does not publish",
        report(
            &router,
            "emu-btr-narrowed",
            &params![
                "Filters.Filter.1.Name" => "db-cluster-id",
                "Filters.Filter.1.Values.Value.1" => "emu-btr-narrowed",
            ],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    // `MaxRecords` outside "Minimum 20, maximum 100", on each side of the window.
    for value in ["19", "101"] {
        expect_error(
            "MaxRecords outside the published window",
            report(
                &router,
                "emu-btr-narrowed",
                &params!["MaxRecords" => value],
            )
            .await,
            400,
            "InvalidParameterValue",
        );
    }
    // And a `Marker` no prior request handed out.
    expect_error(
        "a marker this report could not have issued",
        report(
            &router,
            "emu-btr-narrowed",
            &params!["Marker" => "not-a-marker-this-service-issued"],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
}

#[tokio::test]
async fn a_page_is_cut_by_max_records_and_resumed_beyond_the_marker() {
    let router = emulator();
    a_backtrackable_cluster(&router, "emu-btr-paged").await;
    // One more backtrack than the smallest page the report serves ("Minimum 20"), so the
    // first page cannot hold them all.
    let page_size = 20usize;
    let mut performed = Vec::new();
    for index in 0..=page_size {
        performed.push(identifier_of(
            &backtrack(&router, &format!("backtrack {index}"), "emu-btr-paged").await,
        ));
    }

    let first = expect_ok(
        "the first page",
        report(
            &router,
            "emu-btr-paged",
            &params!["MaxRecords" => page_size],
        )
        .await,
    );
    let mut served: Vec<String> = entries(&first).iter().map(|e| identifier_of(e)).collect();
    assert_eq!(
        served.len(),
        page_size,
        "the first page holds at most MaxRecords records, got {first}"
    );
    let marker = text(&first, "Marker");
    assert!(
        !marker.is_empty(),
        "a report with records left over must hand out a marker, got {first}"
    );

    let second = expect_ok(
        "the second page",
        report(
            &router,
            "emu-btr-paged",
            &params!["MaxRecords" => page_size, "Marker" => &marker],
        )
        .await,
    );
    let rest: Vec<String> = entries(&second).iter().map(|e| identifier_of(e)).collect();
    assert_eq!(
        rest.len(),
        performed.len() - page_size,
        "the second page holds the records beyond the marker, got {second}"
    );
    assert_eq!(
        optional_text(&second, "Marker"),
        None,
        "the last page carries no marker, got {second}"
    );
    served.extend(rest);
    assert_eq!(
        served.len(),
        performed.len(),
        "paging must serve every record exactly once, got {served:?}"
    );
    let mut sorted = served.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        served.len(),
        "no record may be served on two pages, got {served:?}"
    );
    let mut expected = performed.clone();
    expected.sort();
    assert_eq!(
        sorted, expected,
        "the pages together must hold the backtracks performed, got {served:?}"
    );
}

#[tokio::test]
async fn the_report_refuses_what_the_model_declares_a_fault_for() {
    let router = emulator();
    a_backtrackable_cluster(&router, "emu-btr-refusals").await;

    // A cluster the store does not hold: "DBClusterIdentifier doesn't refer to an existing
    // DB cluster."
    let missing = expect_error(
        "a cluster the store does not hold",
        report(&router, "emu-btr-absent", &[]).await,
        404,
        "DBClusterNotFoundFault",
    );
    assert!(
        text(&missing, "Message").contains("emu-btr-absent"),
        "the fault must name the cluster the request wrote, got {missing}"
    );

    // A backtrack identifier of this cluster's: the UUID the model's own example names, which
    // nothing minted here, and a value that is no UUID at all. Neither refers to an existing
    // backtrack.
    for wanted in ["123e4567-e89b-12d3-a456-426655440000", "not-a-uuid"] {
        let refused = expect_error(
            wanted,
            report(
                &router,
                "emu-btr-refusals",
                &params!["BacktrackIdentifier" => wanted],
            )
            .await,
            404,
            "DBClusterBacktrackNotFoundFault",
        );
        assert!(
            text(&refused, "Message").contains(wanted),
            "case {wanted:?}: the fault must name the identifier the request wrote, got \
             {refused}"
        );
    }

    // A cluster of an engine that cannot backtrack at all: the action "only applies to
    // Aurora MySQL DB clusters", so the identifier is a value the parameter does not accept.
    expect_ok(
        "an Aurora PostgreSQL cluster",
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => "emu-btr-postgres",
                "Engine" => "aurora-postgresql",
                "MasterUsername" => "clusteradmin",
                "MasterUserPassword" => "a-strong-password-1",
            ],
        )
        .await,
    );
    let wrong_kind = expect_error(
        "a cluster of an engine that cannot backtrack",
        report(&router, "emu-btr-postgres", &[]).await,
        400,
        "InvalidParameterValue",
    );
    assert!(
        text(&wrong_kind, "Message").contains("aurora-postgresql"),
        "the refusal must name the engine the cluster runs, got {wrong_kind}"
    );
}
