// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `BacktrackDBCluster` answers for a DB cluster that has no
//! backtracking available.
//!
//! The rule under test: a DB cluster whose backtrack window is zero kept no consistent time
//! it could be rewound to, so it is not in a state the backtrack can be performed in — the
//! modelled `InvalidDBClusterStateFault` at HTTP 400, carrying the one sentence Amazon RDS
//! words for a cluster backtracking is not available to. That sentence states the capability
//! the cluster has not got and nothing else: no identifier, window, status or instant of the
//! request or of the store is written into it.
//!
//! The table spans what the rule generalises over: how the cluster came to have no window (a
//! create that carried none at all, a create that disabled backtracking explicitly, and a
//! window a later modification removed), identifiers of different lengths, and the members
//! the backtrack call itself carries — `Force`, `UseEarliestTimeOnPointInTimeUnavailable`,
//! and `BacktrackTo` instants decades apart — none of which may move the answer. A cluster
//! that DOES keep a window is the control: the same call on it is accepted, so the refusal is
//! read off the cluster's stored state rather than handed to every caller.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on, and every wait is the one
//! poll loop this test target shares (`lifecycle_poll_loops::poll_until`) on a REPORTING
//! call.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The sentence Amazon RDS refuses a backtrack of a cluster backtracking is not available to
/// with. Written out here rather than read from the service, so the wording itself is what
/// the assertions hold the emulator to.
const UNAVAILABLE_MESSAGE: &str = "Backtrack is not currently available to this cluster.";

/// The status a DB cluster reports once the clock has settled its create.
const AVAILABLE: &str = "available";

/// One row of the unavailability table: a DB cluster that ends up with no backtrack window,
/// and the backtrack call made against it.
struct UnavailableCase {
    case: &'static str,
    identifier: &'static str,
    /// The `BacktrackWindow` the create carries, or `None` for a create that carries none at
    /// all — backtracking off is the documented default.
    created_with: Option<i64>,
    /// The window a modification then sets, for a cluster that HAD backtracking and lost it.
    modified_to: Option<i64>,
    /// The members the backtrack carries beyond `DBClusterIdentifier`.
    extra: &'static [(&'static str, &'static str)],
}

const UNAVAILABLE_CASES: &[UnavailableCase] = &[
    UnavailableCase {
        case: "a create that carried no window at all",
        identifier: "emu-bt-default",
        created_with: None,
        modified_to: None,
        extra: &[("BacktrackTo", "2020-01-01T00:00:00Z")],
    },
    UnavailableCase {
        case: "a create that disabled backtracking explicitly",
        identifier: "emu-bt-zero",
        created_with: Some(0),
        modified_to: None,
        // `Force` asks for the backtrack even with binary logging on; it cannot buy a
        // cluster a window it has not got.
        extra: &[("BacktrackTo", "2010-06-30T12:34:56Z"), ("Force", "true")],
    },
    UnavailableCase {
        case: "a window a modification removed",
        identifier: "emu-bt-window-removed",
        created_with: Some(3600),
        modified_to: Some(0),
        // Nor can the member that asks for the earliest time the cluster kept, on a cluster
        // that kept none.
        extra: &[
            ("BacktrackTo", "1970-01-01T00:00:00Z"),
            ("UseEarliestTimeOnPointInTimeUnavailable", "true"),
        ],
    },
    UnavailableCase {
        case: "the longest identifier a DB cluster can be named by",
        identifier: "emu-bt-no-window-on-this-rather-long-aurora-mysql-cluster-name",
        created_with: Some(0),
        modified_to: None,
        extra: &[("BacktrackTo", "1999-12-31T23:59:59Z"), ("Force", "false")],
    },
];

/// `DescribeDBClusters` naming one cluster.
async fn describe_cluster(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBClusters",
        &params!["DBClusterIdentifier" => identifier],
    )
    .await
}

/// Poll `DescribeDBClusters` until the cluster reports [`AVAILABLE`] — the state both a
/// modification and a backtrack need — and answer the one `<DBCluster>` it reported.
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

/// Create an Aurora MySQL DB cluster — the kind the backtrack applies to — with the
/// `BacktrackWindow` the row asks for, when it asks for one at all.
async fn create_cluster(router: &MultiServiceRouter, identifier: &str, window: Option<i64>) {
    let mut settings: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", "aurora-mysql".to_string()),
        ("MasterUsername", "clusteradmin".to_string()),
        ("MasterUserPassword", "a-strong-password-1".to_string()),
    ];
    if let Some(window) = window {
        settings.push(("BacktrackWindow", window.to_string()));
    }
    expect_ok(identifier, call(router, "CreateDBCluster", &settings).await);
}

/// The backtrack call one row makes.
async fn backtrack(
    router: &MultiServiceRouter,
    identifier: &str,
    extra: &[(&'static str, &'static str)],
) -> (u16, String) {
    let mut settings: Vec<(&str, String)> = vec![("DBClusterIdentifier", identifier.to_string())];
    for (member, value) in extra {
        settings.push((member, (*value).to_string()));
    }
    call(router, "BacktrackDBCluster", &settings).await
}

#[tokio::test]
async fn a_cluster_with_no_backtrack_window_is_refused_with_one_fixed_sentence() {
    let router = emulator();
    for row in UNAVAILABLE_CASES {
        create_cluster(&router, row.identifier, row.created_with).await;
    }

    for row in UNAVAILABLE_CASES {
        let case = row.case;
        if let Some(window) = row.modified_to {
            // A modification needs the cluster in a state it can be modified in, so the
            // row's precondition is polled for rather than waited out.
            poll_until_available(&router, case, row.identifier).await;
            expect_ok(
                case,
                call(
                    &router,
                    "ModifyDBCluster",
                    &params![
                        "DBClusterIdentifier" => row.identifier,
                        "BacktrackWindow" => window,
                        "ApplyImmediately" => true,
                    ],
                )
                .await,
            );
        }
        let settled = poll_until_available(&router, case, row.identifier).await;
        assert_eq!(
            text(&settled, "BacktrackWindow"),
            "0",
            "case {case:?}: the cluster under test must keep no backtrack window, got {settled}"
        );
        // A cluster that kept no consistent time reports no earliest one either.
        assert!(
            subtrees(&settled, "EarliestBacktrackTime").is_empty(),
            "case {case:?}: a cluster with no window reports no earliest backtrack time, got \
             {settled}"
        );

        let refused = expect_error(
            case,
            backtrack(&router, row.identifier, row.extra).await,
            400,
            "InvalidDBClusterStateFault",
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, UNAVAILABLE_MESSAGE,
            "case {case:?}: expected the message {UNAVAILABLE_MESSAGE:?}, got {message:?}"
        );
        // The sentence is entirely fixed text: nothing the request carried and nothing the
        // store holds for the cluster is interpolated into it.
        assert!(
            !message.contains(row.identifier),
            "case {case:?}: the sentence must name no cluster identifier, got {message:?}"
        );
        for (_, value) in row.extra {
            assert!(
                !message.contains(value),
                "case {case:?}: the sentence must carry no value of the request, got \
                 {message:?}"
            );
        }

        // The refusal changed nothing: the cluster is still there, still running, and still
        // has no window.
        let read_back = poll_until_available(&router, case, row.identifier).await;
        assert_eq!(
            text(&read_back, "BacktrackWindow"),
            "0",
            "case {case:?}: a refused backtrack must leave the cluster as it was, got \
             {read_back}"
        );
    }
}

#[tokio::test]
async fn a_cluster_that_keeps_a_window_is_backtracked_rather_than_refused() {
    let router = emulator();
    // The control for the table above: the same call, on a cluster whose window makes
    // backtracking available to it, is accepted.
    create_cluster(&router, "emu-bt-control", Some(3600)).await;
    let settled = poll_until_available(&router, "the control cluster", "emu-bt-control").await;
    assert_eq!(
        text(&settled, "BacktrackWindow"),
        "3600",
        "the window the create asked for must read back, got {settled}"
    );
    let earliest = text(&settled, "EarliestBacktrackTime");
    assert!(
        earliest.ends_with('Z'),
        "a cluster backtracking is available to reports the earliest time it kept, got \
         {earliest:?}"
    );

    let backtracked = expect_ok(
        "backtrack a cluster with a window",
        backtrack(
            &router,
            "emu-bt-control",
            &[("BacktrackTo", "1970-01-01T00:00:00Z"), ("UseEarliestTimeOnPointInTimeUnavailable", "true")],
        )
        .await,
    );
    assert_eq!(
        text(&backtracked, "BacktrackTo"),
        earliest,
        "the backtrack must report the consistent time it rewound to, got {backtracked}"
    );
    assert_eq!(
        text(&backtracked, "Status"),
        "completed",
        "a backtrack this service has applied reports completed, got {backtracked}"
    );
}
