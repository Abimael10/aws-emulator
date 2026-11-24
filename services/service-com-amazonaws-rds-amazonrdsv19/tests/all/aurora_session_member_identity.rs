// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH Aurora member a session is served by, as the session itself
//! reports it.
//!
//! The rule under test: an endpoint of an Aurora DB cluster that stands in front of a SET of
//! members serves each session from ONE of them, and an Aurora MySQL session reads the DB
//! instance identifier of that member as the server variable `aurora_server_id` — so a client
//! that dialled a custom endpoint learns which member it landed on, and reads
//! `innodb_read_only` as 1 exactly when that endpoint serves no writes. The variable is
//! Aurora MySQL's own: an RDS for MySQL server has no such variable and answers NULL for it.
//!
//! Everything runs in ONE process: the cluster's, each member's and each custom endpoint's
//! listeners are opened by the control-plane calls below, and the client is the MySQL client
//! the sibling endpoint suites drive ([`crate::db_instance_engine_endpoint`]), written against
//! the wire itself, so what is exercised is what a stock driver speaks.
//!
//! The rows span what the rule generalises over: a custom endpoint of each published
//! `CustomEndpointType`, a membership given as a static member and as none, a membership and a
//! type that admit no member at all, a member whose own endpoint is dialled, both of the
//! cluster's own endpoints, and a non-Aurora MySQL server.
//!
//! Nothing here waits out a span of wall-clock: every wait is the shared poll loop
//! ([`crate::lifecycle_poll_loops::poll_until`]), which re-reads a REPORTING call until the
//! condition the case needs holds.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{mysql_login, MySqlSession, Transport};
use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// The cluster every session below is opened on, and its two members: the one it writes
/// through (created first, so `DBClusterMembers` reports it as the writer) and a reader.
const CLUSTER: &str = "emu-asid-cluster";
const WRITER: &str = "emu-asid-writer";
const READER: &str = "emu-asid-reader";

/// The master user the cluster is created with, and its password: a test credential.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The `DatabaseName` the cluster is created with, which every session here lands in.
const DATABASE: &str = "memberid";

/// Create the cluster and its two members and poll all three up.
async fn an_aurora_mysql_cluster(router: &MultiServiceRouter) {
    expect_ok(
        CLUSTER,
        call(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", CLUSTER),
                ("Engine", "aurora-mysql"),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
                ("DatabaseName", DATABASE),
            ],
        )
        .await,
    );
    for member in [WRITER, READER] {
        expect_ok(
            member,
            call(
                router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", member),
                    ("Engine", "aurora-mysql"),
                    ("DBInstanceClass", "db.r5.large"),
                    ("DBClusterIdentifier", CLUSTER),
                ],
            )
            .await,
        );
    }
    let params = [("DBClusterIdentifier", CLUSTER.to_string())];
    poll_until(
        CLUSTER,
        "the cluster reaching available",
        || call(router, "DescribeDBClusters", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBCluster")
                    .first()
                    .is_some_and(|cluster| text(cluster, "Status") == "available")
        },
    )
    .await;
    for member in [WRITER, READER] {
        let params = [("DBInstanceIdentifier", member.to_string())];
        poll_until(
            member,
            "the member reaching available",
            || call(router, "DescribeDBInstances", &params),
            |status, body| {
                status == 200
                    && subtrees(body, "DBInstance")
                        .first()
                        .is_some_and(|member| text(member, "DBInstanceStatus") == "available")
            },
        )
        .await;
    }
}

/// The `<DBCluster>` the cluster reports, read once the cluster is up.
async fn cluster_report(router: &MultiServiceRouter) -> String {
    let body = expect_ok(
        CLUSTER,
        call(
            router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", CLUSTER)],
        )
        .await,
    );
    subtrees(&body, "DBCluster")
        .into_iter()
        .next()
        .expect("the report carries the cluster")
}

/// Create a custom endpoint and answer with the address it publishes once it is up.
async fn a_custom_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    endpoint_type: &str,
    static_member: Option<&str>,
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterEndpointIdentifier", identifier.to_string()),
        ("DBClusterIdentifier", CLUSTER.to_string()),
        ("EndpointType", endpoint_type.to_string()),
    ];
    if let Some(member) = static_member {
        params.push(("StaticMembers.member.1", member.to_string()));
    }
    expect_ok(case, call(router, "CreateDBClusterEndpoint", &params).await);
    let read = [("DBClusterEndpointIdentifier", identifier.to_string())];
    let (body, _) = poll_until(
        case,
        &format!("{identifier} reaching available"),
        || call(router, "DescribeDBClusterEndpoints", &read),
        |status, body| {
            status == 200
                && subtrees(body, "DBClusterEndpointList")
                    .first()
                    .is_some_and(|endpoint| {
                        optional_text(endpoint, "Status").as_deref() == Some("available")
                    })
        },
    )
    .await;
    let endpoint = subtrees(&body, "DBClusterEndpointList")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the report must carry {identifier}"));
    // The membership the endpoint keeps is the one the create declared: the pinning under
    // test is the endpoint's own record, not a property of the session.
    if let Some(member) = static_member {
        assert!(
            text(&endpoint, "StaticMembers").contains(member),
            "case {case:?}: the endpoint keeps the declared StaticMembers, got {endpoint}"
        );
    }
    assert_eq!(
        optional_text(&endpoint, "CustomEndpointType").as_deref(),
        Some(endpoint_type),
        "case {case:?}: expected the requested type, got {endpoint}"
    );
    text(&endpoint, "Endpoint")
}

/// The `host` and `port` a published `host:port` names, which is the pair a client dials.
fn dialable(case: &str, address: &str) -> (String, u16) {
    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected <host>:<port>, got {address:?}"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("case {case:?}: expected a port, got {port:?} ({error})"));
    (host.trim_matches(['[', ']']).to_string(), port)
}

/// One session on an address, logged in as the cluster's master user.
async fn session(case: &str, host: &str, port: u16, database: &str) -> MySqlSession {
    mysql_login(
        host,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(database),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the master user must be admitted on {host}:{port}, got {refusal:?}")
    })
}

/// The one value a statement answers with over a fresh session on `address`.
async fn value_over(case: &str, address: &str, statement: &str) -> Option<String> {
    let (host, port) = dialable(case, address);
    let mut session = session(case, &host, port, DATABASE).await;
    let answer = session
        .query(statement)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {statement:?} must run, got {refusal:?}"));
    answer.only().map(ToString::to_string)
}

/// How many fresh sessions each row opens: the member a custom endpoint selects is read
/// again for every session, so the pinning has to hold for each of them and not just for the
/// first.
const SESSIONS_PER_CASE: usize = 3;

#[tokio::test]
async fn an_aurora_mysql_session_reports_the_member_it_was_served_by() {
    let router = emulator();
    an_aurora_mysql_cluster(&router).await;
    let cluster = cluster_report(&router).await;
    let cluster_port: u16 = text(&cluster, "Port")
        .parse()
        .unwrap_or_else(|error| panic!("the cluster publishes a port, got {cluster} ({error})"));
    let writer_endpoint = format!("{}:{cluster_port}", text(&cluster, "Endpoint"));

    // The member's own endpoint, as `DescribeDBInstances` publishes it.
    let reader_body = expect_ok(
        READER,
        call(
            &router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", READER)],
        )
        .await,
    );
    let reader_instance = subtrees(&reader_body, "DBInstance")
        .into_iter()
        .next()
        .expect("the report carries the member");
    let reader_own = format!(
        "{}:{}",
        text(&reader_instance, "Address"),
        text(&reader_instance, "Port")
    );

    // One row per address a client can dial: the member it must be served by — `None` where
    // no member of the cluster serves it at all — and whether a session on it serves writes
    // (which is the same state `innodb_read_only` reports).
    let rows: Vec<(&str, String, Option<&str>, bool)> = vec![
        (
            "a custom READER endpoint pinned to the reader",
            a_custom_endpoint(
                &router,
                "a custom READER endpoint pinned to the reader",
                "emu-asid-reader-pinned",
                "READER",
                Some(READER),
            )
            .await,
            Some(READER),
            true,
        ),
        (
            "a custom ANY endpoint pinned to the writer",
            a_custom_endpoint(
                &router,
                "a custom ANY endpoint pinned to the writer",
                "emu-asid-any-pinned",
                "ANY",
                Some(WRITER),
            )
            .await,
            Some(WRITER),
            false,
        ),
        (
            "a custom READER endpoint over every eligible member",
            a_custom_endpoint(
                &router,
                "a custom READER endpoint over every eligible member",
                "emu-asid-reader-any",
                "READER",
                None,
            )
            .await,
            Some(READER),
            true,
        ),
        (
            "a custom WRITER endpoint over every eligible member",
            a_custom_endpoint(
                &router,
                "a custom WRITER endpoint over every eligible member",
                "emu-asid-writer-any",
                "WRITER",
                None,
            )
            .await,
            Some(WRITER),
            false,
        ),
        // A membership and a type that admit NO member between them: a READER endpoint whose
        // group holds only the member the cluster writes through. No member serves such a
        // session — the cluster itself does — so there is no DB instance to name, and naming
        // the member the endpoint leaves out would report a server the session never reached.
        (
            "a custom READER endpoint whose group holds only the writer",
            a_custom_endpoint(
                &router,
                "a custom READER endpoint whose group holds only the writer",
                "emu-asid-reader-writer-only",
                "READER",
                Some(WRITER),
            )
            .await,
            None,
            true,
        ),
        // The cluster's own writer endpoint: "the connection endpoint for the primary
        // instance of the DB cluster", so a session on it is served by the member the cluster
        // writes through.
        (
            "the cluster's own writer endpoint",
            writer_endpoint,
            Some(WRITER),
            false,
        ),
        // And a member's own endpoint, which is served by that member whatever else fronts
        // it.
        (
            "the reader member's own endpoint",
            reader_own,
            Some(READER),
            true,
        ),
    ];

    for (case, address, member, read_only) in rows {
        for attempt in 1..=SESSIONS_PER_CASE {
            let served = value_over(case, &address, "SELECT @@aurora_server_id").await;
            assert_eq!(
                served.as_deref(),
                member,
                "case {case:?} (session {attempt}): expected the member serving the session, \
                 got {served:?}"
            );
            let reported = value_over(case, &address, "SELECT @@innodb_read_only").await;
            assert_eq!(
                reported.as_deref(),
                Some(if read_only { "1" } else { "0" }),
                "case {case:?} (session {attempt}): expected innodb_read_only to report \
                 whether the endpoint serves writes, got {reported:?}"
            );
        }
        // `SHOW VARIABLES` reports the same member as the `@@` reference: one reading of the
        // state, written the way each statement class writes it — and a session no member
        // serves, which has no server to name, is listed by neither.
        let (host, port) = dialable(case, &address);
        let mut session = session(case, &host, port, DATABASE).await;
        let shown = session
            .query("SHOW VARIABLES LIKE 'aurora_server_id'")
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: SHOW VARIABLES, got {refusal:?}"));
        let listed: Vec<Option<&str>> = shown
            .rows
            .iter()
            .filter(|row| row.first().and_then(Option::as_deref) == Some("aurora_server_id"))
            .map(|row| row.get(1).and_then(Option::as_deref))
            .collect();
        assert_eq!(
            listed,
            member.map_or_else(Vec::new, |member| vec![Some(member)]),
            "case {case:?}: SHOW VARIABLES must report the serving member as \
             aurora_server_id, got {shown:?}"
        );
    }
}

#[tokio::test]
async fn an_rds_for_mysql_server_has_no_aurora_server_id() {
    let router = emulator();
    let instance = "emu-asid-rds-mysql";
    expect_ok(
        instance,
        call(
            &router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", instance),
                ("Engine", "mysql"),
                ("DBInstanceClass", "db.t3.micro"),
                ("AllocatedStorage", "20"),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
                ("DBName", DATABASE),
            ],
        )
        .await,
    );
    let params = [("DBInstanceIdentifier", instance.to_string())];
    let (body, _) = poll_until(
        instance,
        "the instance reaching available",
        || call(&router, "DescribeDBInstances", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBInstance")
                    .first()
                    .is_some_and(|record| text(record, "DBInstanceStatus") == "available")
        },
    )
    .await;
    let record = subtrees(&body, "DBInstance")
        .into_iter()
        .next()
        .expect("the report carries the instance");
    let address = format!("{}:{}", text(&record, "Address"), text(&record, "Port"));

    // No Aurora variable on an RDS for MySQL server: the reference answers NULL rather than
    // naming the instance, exactly as a server without that variable does.
    let case = "an RDS for MySQL instance";
    let served = value_over(case, &address, "SELECT @@aurora_server_id").await;
    assert_eq!(
        served, None,
        "case {case:?}: a non-Aurora MySQL server has no aurora_server_id, got {served:?}"
    );
    let (host, port) = dialable(case, &address);
    let mut session = session(case, &host, port, DATABASE).await;
    let shown = session
        .query("SHOW VARIABLES LIKE 'aurora_server_id'")
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: SHOW VARIABLES, got {refusal:?}"));
    assert!(
        shown.rows.is_empty(),
        "case {case:?}: SHOW VARIABLES reports no aurora_server_id for this engine, got {shown:?}"
    );
}
