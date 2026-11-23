// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `BacktrackDBCluster` REWINDS: the contents the cluster held at
//! the time asked for.
//!
//! The rule under test: backtracking "rewinds the DB cluster to the time you specify", and
//! "if the specified time isn't a consistent time for the DB cluster, Aurora automatically
//! chooses the nearest possible consistent time" — so everything a session committed AT OR
//! BEFORE that instant survives the rewind and only what was committed after it is gone. A
//! table created before the target instant is therefore still there afterwards, holding
//! exactly the rows it held then; a rewind that put the cluster back to the data it came up
//! with would be a reset rather than a rewind.
//!
//! Everything runs in ONE process: the cluster's listener is opened by the `CreateDBCluster`
//! call below and the client is the one the sibling endpoint suites drive
//! ([`crate::db_instance_engine_endpoint`]), written against the MySQL wire itself — Aurora
//! MySQL being the one engine backtracking applies to ("this action applies only to Aurora
//! MySQL DB clusters").
//!
//! The rows span what the rule generalises over: a target before the first write of all, a
//! target between two writes, a target after a schema change but before the rows that
//! followed it, and a target after every write — and each is asserted on the SCHEMA as well
//! as on the rows, because the two are rewound together.
//!
//! Nothing here waits out a span of wall-clock for a transition this service's clock owes:
//! every wait is the shared poll loop ([`crate::lifecycle_poll_loops::poll_until`]).

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{mysql_login, MySqlSession, Transport};
use crate::db_instances::{call, emulator, expect_ok, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// The master user every cluster below is created with, and its password: a test credential,
/// and the only place in this suite it appears.
const MASTER_USER: &str = "sim2real";
const MASTER_PASSWORD: &str = "Sim2RealPassw0rd";

/// The `DatabaseName` the clusters are created with, which every session here lands in.
const DATABASE: &str = "backtracktruth";

/// The `BacktrackWindow` the clusters are created with, in seconds: a day, which is well
/// past the span of one test, so no point this suite records ages out of the window.
const BACKTRACK_WINDOW: &str = "86400";

/// Create an Aurora MySQL cluster with backtracking on, wait for it to report `available`,
/// and answer with the address it publishes.
async fn a_backtrackable_cluster(router: &MultiServiceRouter, identifier: &str) -> (String, u16) {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", "aurora-mysql"),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
                ("DatabaseName", DATABASE),
                ("BacktrackWindow", BACKTRACK_WINDOW),
            ],
        )
        .await,
    );
    let cluster = poll_cluster(router, identifier, "available").await;
    let port = text(&cluster, "Port");
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("case {identifier:?}: expected a port, got {port:?} ({error})"));
    (text(&cluster, "Endpoint"), port)
}

/// Read the cluster until it reports `status`, and answer with its `<DBCluster>`.
async fn poll_cluster(router: &MultiServiceRouter, identifier: &str, status: &str) -> String {
    let params = [("DBClusterIdentifier", identifier.to_string())];
    let (body, _) = poll_until(
        identifier,
        &format!("{identifier} reaching {status}"),
        || call(router, "DescribeDBClusters", &params),
        |http_status, body| {
            http_status == 200
                && subtrees(body, "DBCluster")
                    .first()
                    .is_some_and(|cluster| text(cluster, "Status") == status)
        },
    )
    .await;
    subtrees(&body, "DBCluster")
        .into_iter()
        .next()
        .expect("the poll settled on a report carrying the cluster")
}

/// One session on the cluster's endpoint, logged in as its master user.
async fn session(case: &str, address: &str, port: u16) -> MySqlSession {
    mysql_login(
        address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the master user must be admitted, got {refusal:?}")
    })
}

/// Run one statement that must be served.
async fn must_run(case: &str, session: &mut MySqlSession, sql: &str) {
    let answer = session.query(sql).await;
    assert!(
        answer.is_ok(),
        "case {case:?}: {sql:?} must run, got {answer:?}"
    );
}

/// The instant a client notes between two writes, in the form `BacktrackTo` takes ("must
/// contain a valid ISO 8601 timestamp"), carried out to the subsecond so two writes a moment
/// apart are told apart.
fn now_as_iso8601() -> String {
    ::aws_smithy_types::DateTime::from(std::time::SystemTime::now())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the current instant is an ISO 8601 timestamp")
}

/// How far apart two instants have to be for the point a backtrack picks between them to be
/// decided by their ORDER alone: twice the millisecond this service compares a target with a
/// consistent point at (`BACKTRACK_POINT_RESOLUTION`, the resolution a timestamp carried out
/// to a client and back survives). Two writes closer together than that are one consistent
/// time as far as the API is concerned, which is what "the nearest possible consistent time"
/// allows.
const POINT_RESOLUTION: std::time::Duration = std::time::Duration::from_millis(2);

/// How long the loop below is given before this suite calls the clock stuck: generous beside
/// the span it waits for, so a loaded host cannot fail the suite.
const CLOCK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Drive the engine with READS until the clock has left the instant just noted behind by more
/// than [`POINT_RESOLUTION`], so the next write lands at an instant the API tells apart from
/// it.
///
/// A loop over a CONDITION — the clock having advanced — and never a sleep: each turn runs a
/// statement the engine really serves, and a read records no consistent point, so the
/// cluster's history is exactly the writes the table makes.
async fn past_the_point_resolution(case: &str, session: &mut MySqlSession) {
    let noted = std::time::SystemTime::now();
    let deadline = std::time::Instant::now() + CLOCK_DEADLINE;
    while std::time::SystemTime::now()
        .duration_since(noted)
        .unwrap_or_default()
        <= POINT_RESOLUTION
    {
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the clock must advance past {POINT_RESOLUTION:?}"
        );
        must_run(case, session, "SELECT 1").await;
    }
}

/// One row of the rewind table: which instant of the cluster's history the backtrack names,
/// and what the cluster holds afterwards.
///
/// The rows walk the history from its LATEST instant BACKWARDS, on ONE cluster: a backtrack
/// leaves the cluster holding the data of the instant it rewound to, so a row that names an
/// earlier instant than the row before it is served by a point that is still kept — which is
/// what lets this table drive the whole history over one cluster's storage rather than one
/// cluster's per row, and exercises a second backtrack of a cluster that has already taken
/// one.
struct Row {
    case: &'static str,
    /// How many of the writes below had been committed when the instant the row backtracks to
    /// was noted: 0 is the instant before the table existed at all.
    committed_by_then: usize,
    /// Whether the table exists after the rewind.
    table_exists: bool,
    /// The labels it holds then.
    labels: &'static [&'static str],
}

/// The writes the cluster takes, in order: a schema change and then one row at a time. The
/// instant AFTER each of them is noted, so a row can name any point of this history.
const WRITES: &[&str] = &[
    "CREATE TABLE rewound (label VARCHAR(32) NOT NULL)",
    "INSERT INTO rewound (label) VALUES ('first')",
    "INSERT INTO rewound (label) VALUES ('second')",
];

/// The cluster the table below drives.
const CLUSTER: &str = "emu-rewind-cluster";

const ROWS: &[Row] = &[
    Row {
        case: "an instant after every write",
        committed_by_then: 3,
        table_exists: true,
        labels: &["first", "second"],
    },
    Row {
        case: "an instant between the two rows",
        committed_by_then: 2,
        table_exists: true,
        labels: &["first"],
    },
    Row {
        case: "an instant after the schema change and before the rows",
        committed_by_then: 1,
        table_exists: true,
        labels: &[],
    },
    Row {
        case: "an instant before the first write of all",
        committed_by_then: 0,
        table_exists: false,
        labels: &[],
    },
];

#[tokio::test]
async fn a_backtrack_restores_the_contents_the_cluster_held_at_the_time_asked_for() {
    let router = emulator();
    let (address, port) = a_backtrackable_cluster(&router, CLUSTER).await;

    // The history: every write is committed on its own, and the instant after each one is
    // noted, exactly as a client that means to undo its last change notes one.
    let mut writing = session("the history", &address, port).await;
    let mut noted: Vec<String> = vec![now_as_iso8601()];
    past_the_point_resolution("the history", &mut writing).await;
    for statement in WRITES {
        must_run("the history", &mut writing, statement).await;
        noted.push(now_as_iso8601());
        past_the_point_resolution("the history", &mut writing).await;
    }
    drop(writing);

    for row in ROWS {
        let target = noted[row.committed_by_then].clone();

        let backtracked = expect_ok(
            row.case,
            call(
                &router,
                "BacktrackDBCluster",
                &[
                    ("DBClusterIdentifier", CLUSTER.to_string()),
                    ("BacktrackTo", target.clone()),
                    // "Specifies whether to force the DB cluster to backtrack when binary
                    // logging is enabled", which a client undoing a write asks for.
                    ("Force", "true".to_string()),
                ],
            )
            .await,
        );
        assert_eq!(
            text(&backtracked, "Status"),
            "completed",
            "case {:?}: the backtrack must report what it applied, got {backtracked}",
            row.case
        );

        // A FRESH session, as a client reconnecting after the rewind opens: what it reads is
        // the cluster's contents as of the instant the backtrack named. A table the rewind
        // took away is answered by the engine's own missing-table error (MySQL 1146), which
        // is what a client sees for a table that is not there.
        let mut reading = session(row.case, &address, port).await;
        let read = reading.query("SELECT label FROM rewound ORDER BY label").await;
        if !row.table_exists {
            let refusal = read.expect_err(&format!(
                "case {:?}: expected the table to be gone after the rewind to {target}",
                row.case
            ));
            assert_eq!(
                refusal.code, "1146",
                "case {:?}: expected the engine's missing-table error, got {refusal:?}",
                row.case
            );
            continue;
        }
        let answer = read.unwrap_or_else(|refusal| {
            panic!(
                "case {:?}: the table was created before {target}, so the rewind keeps it, got \
                 {refusal:?}",
                row.case
            )
        });
        let labels: Vec<String> = answer
            .rows
            .iter()
            .map(|row| row.first().cloned().flatten().unwrap_or_default())
            .collect();
        assert_eq!(
            labels,
            row.labels
                .iter()
                .map(|label| (*label).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected the rows committed at or before {target}, got {labels:?}",
            row.case
        );
    }
}
