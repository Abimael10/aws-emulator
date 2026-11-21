// SPDX-License-Identifier: CC0-1.0

//! The calls and the polls an Aurora DB cluster's lifecycle is driven by on the ASSEMBLED
//! emulator, written ONCE for the suites beside this one that drive it.
//!
//! `rds_cluster_snapshot_restore_lifecycle.rs` and
//! `rds_restored_cluster_vpc_scaling_and_managed_secret.rs` assert two sides of one rule —
//! every transient status of the snapshot-and-restore lifecycle settles, and a restored
//! cluster serves the data its snapshot held — the first over the snapshot INSTANTS and the
//! databases a cluster holds, the second over the MEMBERS a restore carries and the order the
//! client drives them in. Both reach the lifecycle through exactly these calls, so they live
//! here rather than once per suite: a suite that writes its own poll drifts from the one
//! beside it, and a poll that stops at a different condition changes what the suite is
//! actually asserting.
//!
//! Only what both suites reach the service THROUGH is here. What a suite is about stays in
//! that suite: the snapshot instants and the second database of the first, the VPC placement,
//! the capacity range, the tags and the post-restore orderings of the second.
//!
//! This module holds no test of its own; it is a suite's helper module, like
//! [`crate::query_wire`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, form_encode, opt_element};

/// The Region every request below is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The engine the whole lifecycle runs on, the one whose clusters serve the Data API.
pub(crate) const ENGINE: &str = "aurora-postgresql";

/// The database every cluster below is created with, and the one most statements run
/// against.
pub(crate) const DATABASE: &str = "simparitydb";

/// The master user every cluster below is created with.
pub(crate) const MASTER_USER: &str = "simparity";

/// The DB instance class an Aurora Serverless v2 writer is provisioned with
/// (`smithy_query(query="doc", shape="CreateDBInstanceMessage", member="DBInstanceClass")`).
pub(crate) const SERVERLESS_CLASS: &str = "db.serverless";

/// How often a poll looks again, and how long it keeps looking: every condition below is
/// owed by the CLOCK, so each poll waits for the CONDITION under a generous deadline rather
/// than for a fixed span.
pub(crate) const LOOK: Duration = Duration::from_millis(25);
pub(crate) const DEADLINE: Duration = Duration::from_secs(120);

/// The terminal values this lifecycle's statuses settle on: `DBClusterSnapshot$Status` and
/// `DBCluster$Status` (`available`), `MasterUserSecret$SecretStatus` (`active`) and
/// `DBInstance$DBInstanceStatus` (`available`).
pub(crate) const STATUS_AVAILABLE: &str = "available";
pub(crate) const SECRET_ACTIVE: &str = "active";

/// PostgreSQL's own code for a statement naming a relation the database does not hold
/// ("undefined_table", its "Error Codes" appendix) — what a restored cluster answers for a
/// table its snapshot did not carry.
pub(crate) const SQLSTATE_UNDEFINED_TABLE: &str = "42P01";

/// The table the rows are written to, in each database.
pub(crate) const TABLE: &str = "sim2real_snap_restore";

/// The Aurora Serverless v2 capacity range [`create_source_cluster`] gives a source cluster,
/// as the request spells it.
pub(crate) const SOURCE_MIN_CAPACITY: &str = "0.5";
pub(crate) const SOURCE_MAX_CAPACITY: &str = "1.0";

/// The classes the RDS Data API refuses a call on a cluster that is still STARTING UP with,
/// each of them transient, so a poll that waits for the Data API to answer retries exactly
/// these and fails fast on anything else (`smithy_query(query="operation",
/// operation="ExecuteStatement")` on the rdsdata model).
pub(crate) const DATA_API_START_UP_CLASSES: &[&str] = &[
    "DatabaseResumingException",
    "HttpEndpointNotEnabledException",
    "DatabaseNotFoundException",
    "ServiceUnavailableError",
    "DatabaseUnavailableException",
];

/// The emulator as it is ASSEMBLED, on one engine: the hops Amazon RDS makes to Secrets
/// Manager and to Amazon EC2, and the ones the Data API makes back to Amazon RDS, are
/// dispatched into this same router.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request.
pub(crate) fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One query-protocol call to `service`, answered with the HTTP status and the XML body.
pub(crate) async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon RDS awsQuery call, answered with the HTTP status and the XML body.
pub(crate) async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "rds", "2014-10-31", action, members).await
}

/// One Amazon RDS call that must be served, answered with its XML body.
pub(crate) async fn served(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = rds(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with {body}"
    );
    body
}

/// One Data API `ExecuteStatement`, answered with the HTTP status and the parsed body.
pub(crate) async fn execute(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/Execute")
        .header("host", format!("rds-data.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("rds-data"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The wire error class of a Data API refusal, without the namespace `__type` may qualify it
/// with.
pub(crate) fn error_class(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A cluster a Data API caller addresses: its ARN and the ARN of the secret holding the
/// credentials a call authenticates with.
#[derive(Debug, Clone)]
pub(crate) struct Addressed {
    pub(crate) arn: String,
    pub(crate) secret_arn: String,
}

impl Addressed {
    /// Run `sql` against `database` and answer the HTTP status and the parsed response.
    pub(crate) async fn statement_in(
        &self,
        router: &MultiServiceRouter,
        database: &str,
        sql: &str,
    ) -> (u16, Value) {
        execute(
            router,
            &json!({
                "resourceArn": self.arn,
                "secretArn": self.secret_arn,
                "database": database,
                "sql": sql,
            }),
        )
        .await
    }

    /// Run `sql` against `database`; it must be served.
    pub(crate) async fn run_in(
        &self,
        router: &MultiServiceRouter,
        case: &str,
        database: &str,
        sql: &str,
    ) -> Value {
        let (status, answered) = self.statement_in(router, database, sql).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {sql} on {database:?} must be served, got {status} with {answered}"
        );
        answered
    }

    /// Run `sql` against [`DATABASE`]; it must be served.
    pub(crate) async fn run(&self, router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
        self.run_in(router, case, DATABASE, sql).await
    }

    /// Poll until the Data API ANSWERS a statement on this cluster: the classes a cluster
    /// that is still starting up is refused with are transient, so the call is retried under
    /// a generous deadline — and ONLY those, so a refusal that will never clear fails the
    /// suite where it happens instead of as a timeout.
    pub(crate) async fn await_data_api(&self, router: &MultiServiceRouter, case: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let (status, answered) = self.statement_in(router, DATABASE, "SELECT 1").await;
            if status == 200 {
                return;
            }
            let class = error_class(&answered);
            assert!(
                DATA_API_START_UP_CLASSES.contains(&class.as_str()),
                "case {case:?}: the Data API must answer on {:?} or be starting up, got {status} \
                 with {answered}",
                self.arn
            );
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the Data API must answer on {:?}, last got {status} with \
                 {answered}",
                self.arn
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// The ids [`TABLE`] holds in `database`, in key order.
    pub(crate) async fn ids_in(
        &self,
        router: &MultiServiceRouter,
        case: &str,
        database: &str,
    ) -> Vec<i64> {
        let answered = self
            .run_in(
                router,
                case,
                database,
                &format!("SELECT id FROM {TABLE} ORDER BY id"),
            )
            .await;
        ids(&answered)
    }

    /// Assert that this cluster serves NO such table in `database`: the data a snapshot did
    /// not carry is data the restored cluster does not have, which is a refusal and never an
    /// empty answer.
    pub(crate) async fn assert_no_table_in(
        &self,
        router: &MultiServiceRouter,
        case: &str,
        database: &str,
    ) {
        let (status, refused) = self
            .statement_in(router, database, &format!("SELECT id FROM {TABLE}"))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a table the snapshot did not carry must be refused in {database:?}, \
             got {status} with {refused}"
        );
        let message = refused
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains(TABLE) || message.contains(database),
            "case {case:?}: expected the refusal to name the relation or the database \
             ({SQLSTATE_UNDEFINED_TABLE} for a relation nothing created), got {message:?}"
        );
    }
}

/// The `id` column of every record of an answered `SELECT`, in the order the answer reports
/// them.
pub(crate) fn ids(answered: &Value) -> Vec<i64> {
    answered
        .get("records")
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .filter_map(|record| {
                    record
                        .as_array()
                        .and_then(|fields| fields.first())
                        .and_then(|field| field.get("longValue"))
                        .and_then(Value::as_i64)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Poll `DescribeDBClusters` until `pick` answers for what the cluster reports, answering
/// what it picked.
///
/// The ONE cluster poll the three below are: each differs only in the condition it waits for
/// and in what it hands back, so the loop, the deadline and the failure are written once.
/// `awaited` completes the sentence "the DB cluster <identifier> must …".
async fn await_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    awaited: &str,
    pick: impl Fn(&str) -> Option<String>,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = served(
            router,
            case,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", identifier)],
        )
        .await;
        if let Some(picked) = pick(&read) {
            return picked;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the DB cluster {identifier:?} must {awaited}, last read {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// What a cluster reports its managed master user secret's ARN as once that secret is
/// `active`, or `None` while it is not.
fn active_secret_arn(read: &str) -> Option<String> {
    let secret = block_of(read, "MasterUserSecret")?;
    (opt_element(secret, "SecretStatus").unwrap_or_default() == SECRET_ACTIVE)
        .then(|| element(secret, "SecretArn"))
}

/// Poll `DescribeDBClusters` until the cluster reports `available`, answering what it then
/// reports.
pub(crate) async fn await_available_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    await_cluster(
        router,
        case,
        identifier,
        &format!("reach {STATUS_AVAILABLE:?}"),
        |read| {
            (opt_element(read, "Status").unwrap_or_default() == STATUS_AVAILABLE)
                .then(|| read.to_string())
        },
    )
    .await
}

/// Poll `DescribeDBClusters` until the cluster's managed master user secret reports
/// `active`, answering that secret's ARN.
pub(crate) async fn await_active_secret(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    await_cluster(
        router,
        case,
        identifier,
        &format!("report its managed master user secret {SECRET_ACTIVE:?}"),
        active_secret_arn,
    )
    .await
}

/// Poll `DescribeDBClusters` until the cluster reports `available` AND its managed master
/// user secret reports `active` — the single condition a client waits on after asking for the
/// managed password — answering that secret's ARN.
pub(crate) async fn await_settled_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    await_cluster(
        router,
        case,
        identifier,
        &format!("reach {STATUS_AVAILABLE:?} with its managed secret {SECRET_ACTIVE:?}"),
        |read| {
            (opt_element(read, "Status").unwrap_or_default() == STATUS_AVAILABLE)
                .then(|| active_secret_arn(read))
                .flatten()
        },
    )
    .await
}

/// Poll `DescribeDBInstances` until the DB instance reports `available`, answering what it
/// then reports.
pub(crate) async fn await_available_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = served(
            router,
            case,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        )
        .await;
        let reported = opt_element(&read, "DBInstanceStatus").unwrap_or_default();
        if reported == STATUS_AVAILABLE {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the DB instance {identifier:?} must reach {STATUS_AVAILABLE:?}, last \
             reported {reported:?} in {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Poll `DescribeDBClusterSnapshots` until the snapshot reports `available`.
pub(crate) async fn await_available_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = served(
            router,
            case,
            "DescribeDBClusterSnapshots",
            &[("DBClusterSnapshotIdentifier", identifier)],
        )
        .await;
        let reported = opt_element(&read, "Status").unwrap_or_default();
        if reported == STATUS_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the DB cluster snapshot {identifier:?} must reach \
             {STATUS_AVAILABLE:?}, last reported {reported:?} in {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Create an Aurora Serverless v2 cluster with RDS managing the master user password and the
/// Data API turned on, carrying `extra` beside the members every such cluster is created
/// with, and wait until that credential is usable.
pub(crate) async fn create_source_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    extra: &[(&str, &str)],
) -> Addressed {
    let mut members: Vec<(&str, &str)> = vec![
        ("DBClusterIdentifier", identifier),
        ("Engine", ENGINE),
        ("DatabaseName", DATABASE),
        ("MasterUsername", MASTER_USER),
        ("ManageMasterUserPassword", "true"),
        ("EnableHttpEndpoint", "true"),
        ("BackupRetentionPeriod", "1"),
        ("ServerlessV2ScalingConfiguration.MinCapacity", SOURCE_MIN_CAPACITY),
        ("ServerlessV2ScalingConfiguration.MaxCapacity", SOURCE_MAX_CAPACITY),
    ];
    members.extend_from_slice(extra);
    let created = served(router, case, "CreateDBCluster", &members).await;
    let addressed = Addressed {
        arn: element(&created, "DBClusterArn"),
        secret_arn: await_active_secret(router, case, identifier).await,
    };
    await_available_cluster(router, case, identifier).await;
    addressed.await_data_api(router, case).await;
    addressed
}

/// Add a writer DB instance to `cluster` and wait until it is available, reported as a member
/// of that cluster.
pub(crate) async fn add_writer(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    instance: &str,
) {
    served(
        router,
        case,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", instance),
            ("DBClusterIdentifier", cluster),
            ("Engine", ENGINE),
            ("DBInstanceClass", SERVERLESS_CLASS),
        ],
    )
    .await;
    let reported = await_available_instance(router, case, instance).await;
    assert_eq!(
        opt_element(&reported, "DBClusterIdentifier").as_deref(),
        Some(cluster),
        "case {case:?}: expected {instance:?} to report its cluster, got {reported}"
    );
}

/// Take a DB cluster snapshot of `cluster` and wait until it is available.
pub(crate) async fn take_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    snapshot: &str,
) {
    served(
        router,
        case,
        "CreateDBClusterSnapshot",
        &[
            ("DBClusterSnapshotIdentifier", snapshot),
            ("DBClusterIdentifier", cluster),
        ],
    )
    .await;
    await_available_snapshot(router, case, snapshot).await;
}
