// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FLOW a client that relies on an Aurora PostgreSQL cluster's
//! engine drives, and for the WAIT a blocked session of that engine reports.
//!
//! The rule under test: a server reports the wait a session is IN. PostgreSQL's
//! `pg_stat_activity.wait_event_type` and `wait_event` are "the type of event for which the
//! backend is waiting" and the event itself — `Lock` / `transactionid` for a backend waiting
//! on a row another transaction holds — and are NULL only for a backend that waits for none.
//! That pair is how a client finds the session that blocks it, which is what a flow
//! co-ordinating two transactions over one row needs before it can close the cycle.
//!
//! Around it, the flow itself, end to end in one process: an Aurora PostgreSQL cluster plus
//! its writer instance, each polled to `available`, a TLS session opened on the pair the
//! control plane published, the engine's own refusal of a duplicate key and of an orphan child
//! row (SQLSTATE 23505 and 23503, the condition names of PostgreSQL's "Appendix A. PostgreSQL
//! Error Codes"), and the teardown a client retries. The catalogues of constraint and lock
//! refusals this engine serves are tabled over in [`crate::aurora_engine_row_locks`] and the
//! address the cluster publishes in [`crate::served_cluster_endpoint_address`], both of which
//! drive the same kind of cluster endpoint; what is asserted here is the flow that REACHES
//! that engine and what it can observe about a blocked session.

use crate::db_clusters::the_cluster;
use crate::db_instance_engine_endpoint::{
    postgres_login, PostgresSession, Transport, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator, expect_ok, text};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The instance class a member of an Aurora cluster is created with.
const AURORA_MEMBER_CLASS: &str = "db.r5.large";

/// Create the row's cluster and the member instances it asks for.
async fn create_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
    members: usize,
    extra: &[(&str, &str)],
) -> String {
    let mut request: Vec<(&str, String)> = params![
        "DBClusterIdentifier" => identifier,
        "Engine" => engine,
        "DatabaseName" => DATABASE,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "BackupRetentionPeriod" => 1,
    ]
    .to_vec();
    request.extend(
        extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    let created = the_cluster(
        case,
        &expect_ok(case, call(router, "CreateDBCluster", &request).await),
    );
    for member in 0..members {
        expect_ok(
            case,
            call(
                router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => format!("{identifier}-w{member}"),
                    "DBClusterIdentifier" => identifier,
                    "Engine" => engine,
                    "DBInstanceClass" => AURORA_MEMBER_CLASS,
                    "PubliclyAccessible" => false,
                ],
            )
            .await,
        );
    }
    created
}

/// Poll the cluster until it reports `available` — the state a client dials it in — and answer
/// with the envelope it settled on.
///
/// The CONDITION is waited for rather than a span: the loop is the one this test target
/// shares, whose deadline is generous beside the span a transition this service owes becomes
/// due after.
async fn available_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let request = [("DBClusterIdentifier", identifier)];
    let (body, _polls) = poll_until(
        case,
        "the cluster",
        || call(router, "DescribeDBClusters", &request),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
    the_cluster(case, &body)
}

/// Poll the member DB instance until `DBInstanceStatus` reports `available`, which is the
/// order a client provisions a cluster's writer in: the writer first, then the cluster.
async fn available_instance(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) {
    let request = [("DBInstanceIdentifier", identifier)];
    poll_until(
        case,
        "the writer instance",
        || call(router, "DescribeDBInstances", &request),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
}

/// The `Endpoint` and `Port` a cluster envelope publishes, asserting on the way that the
/// endpoint member carries no port of its own.
fn published_pair(case: &str, envelope: &str) -> (String, u16) {
    let endpoint = text(envelope, "Endpoint");
    let port = text(envelope, "Port");
    assert!(
        !endpoint.is_empty() && !endpoint.contains(':'),
        "case {case:?}: expected a host carrying no port of its own as the endpoint, got \
         {endpoint:?}"
    );
    let parsed = port.parse::<u16>().unwrap_or_else(|_| {
        panic!("case {case:?}: expected the port the engine listens on, got {port:?}")
    });
    assert!(
        parsed != 0,
        "case {case:?}: expected a port a client can dial, got {parsed}"
    );
    (endpoint, parsed)
}

/// `unique_violation` and `foreign_key_violation`: the conditions the two refusals this
/// flow's own beats assert carry.
///
/// Source: the condition names of PostgreSQL's "Appendix A. PostgreSQL Error Codes". The
/// catalogues of constraint and LOCK refusals this engine serves are tabled over in
/// [`crate::aurora_engine_row_locks`] (and its parameterised sibling), which drive the same
/// Aurora PostgreSQL cluster endpoint: what is asserted HERE is that the cluster-plus-writer
/// flow reaches that engine at all through the address the control plane published.
const UNIQUE_VIOLATION: &str = "23505";
const FOREIGN_KEY_VIOLATION: &str = "23503";

/// One refusal the engine behind the cluster's endpoint must answer.
struct RefusalRow {
    case: &'static str,
    /// The schema and rows the case needs, each of which must run.
    schema: &'static [&'static str],
    /// The statement the engine must refuse.
    refused: &'static str,
    code: &'static str,
    message: &'static str,
}

/// The two beats the flow itself asserts: a duplicate key on a UNIQUE constraint the client
/// named, and an orphan child row on a FOREIGN KEY it named — each refused with the condition
/// PostgreSQL gives that class and a sentence naming the constraint the row violated.
const REFUSAL_ROWS: &[RefusalRow] = &[
    RefusalRow {
        case: "a duplicate key on a UNIQUE constraint the client named",
        schema: &[
            "CREATE TABLE swe_users (id integer PRIMARY KEY, email text, CONSTRAINT \
             swe_users_email_uq UNIQUE (email))",
            "INSERT INTO swe_users (id, email) VALUES (1, 'first@example.com')",
        ],
        refused: "INSERT INTO swe_users (id, email) VALUES (2, 'first@example.com')",
        code: UNIQUE_VIOLATION,
        message: "duplicate key value violates unique constraint \"swe_users_email_uq\"",
    },
    RefusalRow {
        case: "an orphan child row on a FOREIGN KEY the client named",
        schema: &[
            "CREATE TABLE swe_parent (id integer PRIMARY KEY)",
            "CREATE TABLE swe_child (id integer PRIMARY KEY, parent integer, CONSTRAINT \
             swe_child_parent_fk FOREIGN KEY (parent) REFERENCES swe_parent (id))",
            "INSERT INTO swe_parent (id) VALUES (1)",
        ],
        refused: "INSERT INTO swe_child (id, parent) VALUES (10, 999)",
        code: FOREIGN_KEY_VIOLATION,
        message: "insert or update on table \"swe_child\" violates foreign key constraint \
                  \"swe_child_parent_fk\"",
    },
];

/// One session on the published pair, logged in as the cluster's master user over the in-band
/// TLS upgrade a current PostgreSQL endpoint negotiates.
async fn session(case: &str, address: &str, port: u16) -> PostgresSession {
    postgres_login(
        address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the master user must be admitted on {address}:{port}, got {refusal:?}")
    })
}

/// Run one statement that must succeed.
async fn must_run(case: &str, session: &mut PostgresSession, sql: &str) {
    let answer = session.query(sql).await;
    assert!(
        answer.is_ok(),
        "case {case:?}: {sql:?} must run, got {answer:?}"
    );
}

/// The cluster of the engine-truth flow and its writer instance, polled to `available`, with
/// the pair the control plane published for it.
async fn cluster_with_a_writer(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> (String, u16) {
    create_cluster(router, case, identifier, "aurora-postgresql", 1, &[]).await;
    available_instance(router, case, &format!("{identifier}-w0")).await;
    let settled = available_cluster(router, case, identifier).await;
    published_pair(case, &settled)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cluster_and_its_writer_are_torn_down_by_the_deletes_a_client_retries() {
    let router = emulator();
    let case = "the teardown of a cluster and its writer instance";
    let identifier = "emu-swe-teardown";
    let writer = format!("{identifier}-w0");
    cluster_with_a_writer(&router, case, identifier).await;

    // The writer goes first: a cluster member holds no storage of its own, so its delete
    // names no final snapshot, and the instance is gone once the clock has made the delete
    // due — the CONDITION a client polls for.
    expect_ok(
        case,
        call(
            &router,
            "DeleteDBInstance",
            &params!["DBInstanceIdentifier" => writer.clone()],
        )
        .await,
    );
    let instance_request = [("DBInstanceIdentifier", writer.as_str())];
    poll_until(
        case,
        "the writer instance to be gone",
        || call(&router, "DescribeDBInstances", &instance_request),
        |status, _body| status == 404,
    )
    .await;

    // Then the cluster itself. A client RETRIES the delete, because a cluster that is still
    // busy refuses it, and polls the describe until the cluster is gone.
    let cluster_request = [("DBClusterIdentifier", identifier)];
    let delete = params![
        "DBClusterIdentifier" => identifier,
        "SkipFinalSnapshot" => true,
    ];
    let (_last, _polls) = poll_until(
        case,
        "the cluster delete to be accepted",
        || call(&router, "DeleteDBCluster", &delete),
        |status, _body| status == 200,
    )
    .await;
    poll_until(
        case,
        "the cluster to be gone",
        || call(&router, "DescribeDBClusters", &cluster_request),
        |status, _body| status == 404,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_on_the_published_pair_reads_the_engines_own_refusals() {
    let router = emulator();
    let case = "an Aurora PostgreSQL cluster and its writer instance";
    let (address, port) = cluster_with_a_writer(&router, case, "emu-swe-truth").await;
    let mut client = session(case, &address, port).await;

    for row in REFUSAL_ROWS {
        for statement in row.schema {
            must_run(row.case, &mut client, statement).await;
        }
        let refusal = client
            .query(row.refused)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {:?}: {:?} must be refused", row.case, row.refused));
        assert_eq!(
            refusal.code, row.code,
            "case {:?}: expected SQLSTATE {}, got {refusal:?}",
            row.case, row.code
        );
        assert_eq!(
            refusal.message, row.message,
            "case {:?}: expected the engine's own sentence, got {refusal:?}",
            row.case
        );
    }
}

/// How long this suite gives a condition it polls for before calling it unobservable:
/// generous beside the engine's own statement budget, which is what bounds a waiting
/// statement.
const OBSERVE_DEADLINE: std::time::Duration = std::time::Duration::from_millis(4_000);

/// The gap between two polls of the same loop.
const OBSERVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// How many backends `pg_stat_activity` reports as waiting for a row lock right now.
async fn backends_waiting_for_a_row_lock(case: &str, observer: &mut PostgresSession) -> usize {
    let counted = observer
        .query(
            "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND \
             wait_event = 'transactionid'",
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: the session view must be readable, got {refusal:?}")
        });
    counted
        .only()
        .unwrap_or("0")
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a count, got {counted:?}"))
}

/// Poll `pg_stat_activity` until the number of waiting backends is `wanted`, answering
/// whether it was reached: the CONDITION is waited for, never a span.
async fn waiting_backends_settle_at(
    case: &str,
    observer: &mut PostgresSession,
    wanted: usize,
) -> usize {
    let deadline = std::time::Instant::now() + OBSERVE_DEADLINE;
    let mut seen = backends_waiting_for_a_row_lock(case, observer).await;
    while seen != wanted && std::time::Instant::now() < deadline {
        tokio::time::sleep(OBSERVE_INTERVAL).await;
        seen = backends_waiting_for_a_row_lock(case, observer).await;
    }
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backend_blocked_on_a_held_row_is_reported_as_waiting_for_that_lock() {
    let router = emulator();
    let case = "a backend blocked on a row another transaction holds";
    let (address, port) = cluster_with_a_writer(&router, case, "emu-swe-waiting").await;
    let mut holder = session(case, &address, port).await;
    for statement in [
        "CREATE TABLE swe_waits (id integer PRIMARY KEY, n integer)",
        "INSERT INTO swe_waits (id, n) VALUES (1, 10)",
    ] {
        must_run(case, &mut holder, statement).await;
    }
    must_run(case, &mut holder, "BEGIN").await;
    must_run(case, &mut holder, "UPDATE swe_waits SET n = 11 WHERE id = 1").await;

    // Nothing is blocked yet, so the view reports no wait at all: the members are reported
    // for a backend that IS waiting and for no other.
    let mut observer = session(case, &address, port).await;
    let idle = backends_waiting_for_a_row_lock(case, &mut observer).await;
    assert_eq!(
        idle, 0,
        "case {case:?}: expected no backend to be waiting for a lock, got {idle}"
    );

    // A second session asks for the row the holder has: it waits, and while it waits the
    // view reports the event it is waiting for.
    let mut blocked = session(case, &address, port).await;
    let waiter = tokio::spawn(async move {
        let answer = blocked
            .query("UPDATE swe_waits SET n = 12 WHERE id = 1")
            .await;
        (blocked, answer)
    });
    let seen = waiting_backends_settle_at(case, &mut observer, 1).await;
    assert_eq!(
        seen, 1,
        "case {case:?}: expected exactly one backend waiting for the row lock, got {seen}"
    );

    // Once the holder ends its transaction the waiter gets the row, and the view reports no
    // wait again — so what a client reads is the backends blocked at that instant.
    must_run(case, &mut holder, "COMMIT").await;
    let (mut served, answer) = waiter.await.expect("the blocked session's own task");
    assert!(
        answer.is_ok(),
        "case {case:?}: the waiting statement must be served once the holder ends, got \
         {answer:?}"
    );
    let settled = waiting_backends_settle_at(case, &mut observer, 0).await;
    assert_eq!(
        settled, 0,
        "case {case:?}: expected no backend waiting once the row was granted, got {settled}"
    );
    let row = served.query("SELECT n FROM swe_waits WHERE id = 1").await;
    assert_eq!(
        row.as_ref().map(|rows| rows.only().map(str::to_string)),
        Ok(Some("12".to_string())),
        "case {case:?}: expected the row the waiter wrote, got {row:?}"
    );
}


