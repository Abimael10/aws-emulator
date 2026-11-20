// SPDX-License-Identifier: CC0-1.0

//! An Amazon RDS DB cluster's ENGINE LOG EXPORT, across the two services that have to agree
//! for a client to read it.
//!
//! RULE: a DB resource created with a log type in `EnableCloudwatchLogsExports` streams that
//! engine log to the CloudWatch Logs group Amazon RDS publishes for it —
//! `/aws/rds/cluster/<DBClusterIdentifier>/<log type>`, in a stream named after the DB
//! instance whose engine wrote the lines — and every message the engine writes at or above
//! the severity its own configuration logs at is delivered there as a log event. A resource
//! that has not turned a log type on has nothing delivered for it, so a filter over the
//! group sees exactly the lines the engine wrote and nothing else.
//!
//! Only a cross-service suite can pin this: the lines are written by the ENGINE behind the
//! cluster's published endpoint (Amazon RDS) and read back through CloudWatch Logs' own
//! readers, over ONE engine, exactly as `core/router/src/lib.rs` mounts them. The client is
//! written against the PostgreSQL protocol itself — the startup message, the cleartext
//! password and the simple-query protocol — so what drives the engine here is what a stock
//! driver sends.
//!
//! The rows span what the rule generalises over: the log type enabled on its own, the same
//! type among several, a log type that is NOT the engine's own, and no export at all; the
//! messages span the severities (a `LOG` the server logs and does not send, a `NOTICE` it
//! sends and does not log, and the two `ERROR`s — a statement the engine refused and a
//! `RAISE EXCEPTION` — that it both logs and sends).

use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::query_wire::{element, form_encode};

/// Keep this at or above the stack the other whole-router suites build on: the same
/// assembly runs here.
pub(crate) const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The master user every cluster below is created with, and its password — a test
/// credential, and the only place it appears.
pub(crate) const MASTER_USER: &str = "pgadmin";
pub(crate) const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The database every cluster below carries.
pub(crate) const DATABASE: &str = "obsdb";

/// The engine version the clusters run.
///
/// Amazon RDS publishes `rds.force_ssl`'s default as 1 for the PostgreSQL 15 and later
/// release lines and 0 before them, so a cluster of this line admits the PLAINTEXT session
/// this suite's client opens — the in-band TLS upgrade is the subject of the RDS crate's own
/// endpoint suites, not of this one.
const ENGINE_VERSION: &str = "14.12";

/// The log type the PostgreSQL engines' server log is exported under.
pub(crate) const POSTGRESQL_LOG_TYPE: &str = "postgresql";

/// How long the suite waits for a condition it polls for (a cluster reaching `available`,
/// an event reaching the log group). Generous: a loaded machine may take far longer than
/// this emulator's own transition span, and nothing here is timed.
pub(crate) const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long it waits between looks while polling.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// Assert the call was served, and hand back its body.
    fn served(self, case: &str) -> String {
        assert_eq!(
            self.status, 200,
            "case {case:?}: the call must be served, got HTTP {} with {}",
            self.status, self.body
        );
        self.body
    }
}

/// `[(name, value.to_string()), ..]` — the parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One call of `method` to the service `signing_name` at `path`, with `body`.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: String,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            // The signature is never checked: this router runs with authentication off.
            // What the scope carries is the Region and the service a handler reads.
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// One awsQuery call to Amazon RDS, as an SDK client sends one.
async fn rds(router: &MultiServiceRouter, action: &str, params: &[(&str, String)]) -> Answer {
    let mut body = format!("Action={action}&Version=2014-10-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "rds",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body,
    )
    .await
}

/// One CloudWatch Logs call that must be served, as its JSON client sends one.
pub(crate) async fn logs(
    router: &MultiServiceRouter,
    operation: &str,
    request: Value,
    case: &str,
) -> Value {
    let target = format!("Logs_20140328.{operation}");
    let body = dispatch(
        router,
        "logs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target.as_str()),
        ],
        request.to_string(),
    )
    .await
    .served(case);
    serde_json::from_str(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: CloudWatch Logs answers JSON, got {error} in {body}")
    })
}

/// Create the row's cluster, wait for it to report `available`, and answer with the address
/// it published.
pub(crate) async fn available_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    exports: &[&str],
) -> (String, u16) {
    let mut request: Vec<(&str, String)> = params![
        "DBClusterIdentifier" => identifier,
        "Engine" => "aurora-postgresql",
        "EngineVersion" => ENGINE_VERSION,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "DatabaseName" => DATABASE,
    ]
    .to_vec();
    for (index, log_type) in exports.iter().enumerate() {
        request.push((
            // The awsQuery member name of one entry of `EnableCloudwatchLogsExports`,
            // which is a list of strings and so is sent as `<member>.<n>`.
            Box::leak(format!("EnableCloudwatchLogsExports.member.{}", index + 1).into_boxed_str()),
            (*log_type).to_string(),
        ));
    }
    let created = rds(router, "CreateDBCluster", &request)
        .await
        .served(&format!("{case}: create the cluster"));
    let address = element(&created, "Endpoint");
    let port: u16 = element(&created, "Port")
        .parse()
        .unwrap_or_else(|error| panic!("case {case:?}: the cluster publishes a port: {error}"));

    // A client connects once the cluster reports `available`, which the clock makes due:
    // looked for again until it holds rather than waited out.
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let described = rds(
            router,
            "DescribeDBClusters",
            &params!["DBClusterIdentifier" => identifier],
        )
        .await
        .served(&format!("{case}: describe the cluster"));
        let status = element(&described, "Status");
        if status == "available" {
            assert_eq!(
                (element(&described, "Endpoint"), element(&described, "Port")),
                (address.clone(), port.to_string()),
                "case {case:?}: the endpoint the create published must be the one the \
                 describe reports"
            );
            return (address, port);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the cluster must reach `available`, last status {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A refusal the server sent: the SQLSTATE a client branches on and the sentence it reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: String,
    pub(crate) message: String,
}

/// What one statement answered: what the server SAID about it, and whether it was refused.
#[derive(Debug, Clone, Default)]
pub(crate) struct Said {
    pub(crate) notices: Vec<Refusal>,
}

/// A PostgreSQL session opened on an engine endpoint of this emulator, in the clear.
pub(crate) struct Session {
    conn: TcpStream,
}

impl Session {
    /// Open a session on `address:port` as libpq does: the startup message, the cleartext
    /// password, and then the parameters the server reports.
    pub(crate) async fn open(case: &str, address: &str, port: u16) -> Self {
        assert_eq!(
            address, "127.0.0.1",
            "case {case:?}: the published endpoint must be this machine's loopback address, \
             got {address:?}"
        );
        let mut conn = TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the published endpoint {address}:{port} must accept a \
                     connection, got {error}"
                )
            });
        let mut body = Vec::new();
        // Protocol version 3.0, the user and the database.
        body.extend_from_slice(&196_608i32.to_be_bytes());
        put_cstring(&mut body, "user");
        put_cstring(&mut body, MASTER_USER);
        put_cstring(&mut body, "database");
        put_cstring(&mut body, DATABASE);
        body.push(0);
        let mut startup = Vec::new();
        startup.extend_from_slice(
            &i32::try_from(body.len() + 4)
                .expect("the startup message fits")
                .to_be_bytes(),
        );
        startup.extend_from_slice(&body);
        conn.write_all(&startup)
            .await
            .expect("the startup is sent");
        let mut session = Self { conn };
        loop {
            let (kind, body) = session.read_message(case).await;
            match kind {
                // AuthenticationRequest: this endpoint asks for a cleartext password.
                b'R' => {
                    let code = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                    if code == 0 {
                        continue;
                    }
                    assert_eq!(
                        code, 3,
                        "case {case:?}: expected a cleartext password request, got {code}"
                    );
                    let mut message = Vec::new();
                    put_cstring(&mut message, MASTER_PASSWORD);
                    session.send(b'p', &message).await;
                }
                b'E' => panic!(
                    "case {case:?}: the master user must be admitted, got {:?}",
                    refusal_of(&body)
                ),
                // ReadyForQuery: the session is open.
                b'Z' => return session,
                // ParameterStatus, BackendKeyData, NoticeResponse.
                _ => {}
            }
        }
    }

    /// Run one simple query and read everything the server answers for it.
    pub(crate) async fn query(&mut self, case: &str, sql: &str) -> Result<Said, Refusal> {
        let mut message = Vec::new();
        put_cstring(&mut message, sql);
        self.send(b'Q', &message).await;
        let mut said = Said::default();
        let mut refusal = None;
        loop {
            let (kind, body) = self.read_message(case).await;
            match kind {
                b'E' => refusal = Some(refusal_of(&body)),
                // NoticeResponse: what the server has to say about the statement.
                b'N' => said.notices.push(refusal_of(&body)),
                // ReadyForQuery ends the cycle.
                b'Z' => break,
                // RowDescription, DataRow, CommandComplete, EmptyQueryResponse.
                _ => {}
            }
        }
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(said),
        }
    }

    /// Write one protocol message: its type byte, its length and its body.
    async fn send(&mut self, kind: u8, body: &[u8]) {
        let mut message = vec![kind];
        message.extend_from_slice(
            &i32::try_from(body.len() + 4)
                .expect("the message fits")
                .to_be_bytes(),
        );
        message.extend_from_slice(body);
        self.conn
            .write_all(&message)
            .await
            .expect("the message is sent");
    }

    /// Read one protocol message.
    async fn read_message(&mut self, case: &str) -> (u8, Vec<u8>) {
        let mut header = [0u8; 5];
        self.conn
            .read_exact(&mut header)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the server answers, got {error}"));
        let length = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let body_length = usize::try_from(length - 4).expect("a message length fits");
        let mut body = vec![0u8; body_length];
        self.conn
            .read_exact(&mut body)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the server answers, got {error}"));
        (header[0], body)
    }
}

/// The SQLSTATE and the message an `ErrorResponse` or `NoticeResponse` carries.
fn refusal_of(body: &[u8]) -> Refusal {
    let mut refusal = Refusal {
        code: String::new(),
        message: String::new(),
    };
    let mut at = 0usize;
    while at < body.len() && body[at] != 0 {
        let field = body[at];
        let (value, next) = read_cstring(body, at + 1);
        match field {
            b'C' => refusal.code = value,
            b'M' => refusal.message = value,
            _ => {}
        }
        at = next;
    }
    refusal
}

/// Append a NUL-terminated string.
fn put_cstring(body: &mut Vec<u8>, text: &str) {
    body.extend_from_slice(text.as_bytes());
    body.push(0);
}

/// The NUL-terminated string at `at`, and the index after it.
fn read_cstring(body: &[u8], at: usize) -> (String, usize) {
    let rest = &body[at.min(body.len())..];
    let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
    (
        String::from_utf8_lossy(&rest[..end]).into_owned(),
        at + end + 1,
    )
}

/// The messages the log group carries, filtered by `pattern` where the row names one.
pub(crate) async fn filtered_events(
    router: &MultiServiceRouter,
    case: &str,
    log_group: &str,
    pattern: Option<&str>,
) -> Vec<(String, String)> {
    let mut request = json!({ "logGroupName": log_group });
    if let Some(pattern) = pattern {
        request["filterPattern"] = Value::String(pattern.to_string());
    }
    let answer = logs(router, "FilterLogEvents", request, case).await;
    answer["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|event| {
            Some((
                event["logStreamName"].as_str()?.to_string(),
                event["message"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Whether CloudWatch Logs holds a group of exactly `log_group`.
async fn holds_group(router: &MultiServiceRouter, case: &str, log_group: &str) -> bool {
    let answer = logs(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": log_group }),
        case,
    )
    .await;
    answer["logGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|group| group["logGroupName"].as_str() == Some(log_group))
}

/// One cluster this suite provisions and drives.
struct Row {
    case: &'static str,
    identifier: &'static str,
    /// The log types the create turns on.
    exports: &'static [&'static str],
    /// Whether the engine's own log — `postgresql` — is among them, which is what decides
    /// whether anything is delivered.
    exported: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the engine's log type on its own",
        identifier: "obs-export-only-postgresql",
        exports: &["postgresql"],
        exported: true,
    },
    Row {
        case: "the engine's log type among several",
        identifier: "obs-export-several",
        exports: &["postgresql", "upgrade"],
        exported: true,
    },
    Row {
        case: "a log type that is not the engine's own",
        identifier: "obs-export-other-type",
        exports: &["upgrade"],
        exported: false,
    },
    Row {
        case: "no export at all",
        identifier: "obs-export-none",
        exports: &[],
        exported: false,
    },
];

/// The marker a `RAISE LOG` writes, which is the line a client polls the group for.
const LOG_MARKER: &str = "export marker for this cluster";

/// The sentence a `RAISE EXCEPTION` raises, with its `%` placeholder filled by the engine.
const EXCEPTION_MARKER: &str = "the export is broken at row 7";

/// The sentence a `RAISE WARNING` raises in the block that then aborts: a message the server
/// produced BEFORE the failure, which its log carries all the same.
const WARNING_BEFORE_THE_ABORT: &str = "the export is about to fail";

/// Drive one row's engine and answer what the server said about each statement.
async fn drive_the_engine(router: &MultiServiceRouter, row: &Row) -> Vec<Refusal> {
    let (address, port) = available_cluster(router, row.case, row.identifier, row.exports).await;
    let mut session = Session::open(row.case, &address, port).await;
    let mut refusals = Vec::new();

    // A `RAISE LOG`: the engine writes it to its log and — at the default
    // `client_min_messages` — does not send it to the client.
    let said = session
        .query(
            row.case,
            &format!("DO $$ BEGIN RAISE LOG '{LOG_MARKER}'; END $$"),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!(
                "case {:?}: a code block that raises a LOG must be served, got {refusal:?}",
                row.case
            )
        });
    assert!(
        said.notices.is_empty(),
        "case {:?}: a LOG message is not sent to the client at the default \
         client_min_messages, got {:?}",
        row.case,
        said.notices
    );

    // A `RAISE NOTICE`, whose `%` the engine fills: sent to the client, and NOT logged.
    let said = session
        .query(
            row.case,
            "DO $$ BEGIN RAISE NOTICE 'counted % of them', 1 + 1; END $$",
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!(
                "case {:?}: a code block that raises a NOTICE must be served, got {refusal:?}",
                row.case
            )
        });
    assert_eq!(
        said.notices
            .iter()
            .map(|notice| notice.message.as_str())
            .collect::<Vec<_>>(),
        vec!["counted 2 of them"],
        "case {:?}: the NOTICE must reach the client with its format filled by the engine",
        row.case
    );

    // A statement the engine refuses, which is an `ERROR` line of its log carrying the very
    // sentence the client is told.
    let refusal = session
        .query(row.case, "SELECT * FROM a_relation_that_is_not_there")
        .await
        .expect_err("a query over a relation that does not exist is refused");
    assert_eq!(
        refusal.code, "42P01",
        "case {:?}: an undefined relation is refused under 42P01, got {refusal:?}",
        row.case
    );
    refusals.push(refusal);

    // And a `RAISE EXCEPTION`, which aborts the block under `raise_exception` — after a
    // WARNING the same block raised, which the engine's log carries even though the statement
    // it belongs to failed.
    let refusal = session
        .query(
            row.case,
            &format!(
                "DO $$ BEGIN RAISE WARNING '{WARNING_BEFORE_THE_ABORT}'; \
                 RAISE EXCEPTION 'the export is broken at row %', 3 + 4; END $$"
            ),
        )
        .await
        .expect_err("a code block that raises an exception is refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ("P0001", EXCEPTION_MARKER),
        "case {:?}: a RAISE EXCEPTION is reported under P0001 with its format filled, got \
         {refusal:?}",
        row.case
    );
    refusals.push(refusal);
    refusals
}

/// What the engine wrote reaches the group of every cluster that exports that log type, and
/// nothing is created for one that does not.
#[test]
fn the_engine_log_of_each_exporting_cluster_reaches_its_cloudwatch_logs_group() {
    std::thread::Builder::new()
        .name("rds-engine-log-export".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: a cluster
            // create opens the engine's listener through the core, and binding a socket
            // needs the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                for row in ROWS {
                    let refusals = drive_the_engine(&router, row).await;
                    let log_group =
                        format!("/aws/rds/cluster/{}/{POSTGRESQL_LOG_TYPE}", row.identifier);
                    if !row.exported {
                        assert!(
                            !holds_group(&router, row.case, &log_group).await,
                            "case {:?}: a cluster that has not turned this log type on has \
                             no group {log_group}",
                            row.case
                        );
                        continue;
                    }
                    // The lines are delivered before the statement that wrote them is
                    // answered, so they are already there; looked for again anyway, so a
                    // loaded machine cannot turn this into a timing assertion.
                    let deadline = std::time::Instant::now() + POLL_DEADLINE;
                    let events = loop {
                        let events =
                            filtered_events(&router, row.case, &log_group, None).await;
                        if events.len() >= 3 {
                            break events;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "case {:?}: the engine's lines must reach {log_group}, got \
                             {events:?}",
                            row.case
                        );
                        tokio::time::sleep(POLL_INTERVAL).await;
                    };
                    let messages: Vec<&str> =
                        events.iter().map(|(_, message)| message.as_str()).collect();

                    // The stream is named after the DB instance whose engine wrote the
                    // lines; this cluster has no member instance, so it is the cluster's own
                    // engine that wrote them.
                    for (stream, message) in &events {
                        assert_eq!(
                            stream, row.identifier,
                            "case {:?}: the line {message:?} must be in the stream of the \
                             engine that wrote it",
                            row.case
                        );
                    }

                    // The `LOG` message, under the prefix Amazon RDS's PostgreSQL log lines
                    // carry: the instant, the session's client, user and database, the
                    // backend's pid, and then the severity.
                    let logged = messages
                        .iter()
                        .find(|message| message.contains(LOG_MARKER))
                        .unwrap_or_else(|| {
                            panic!(
                                "case {:?}: the LOG message must be in the group, got \
                                 {messages:?}",
                                row.case
                            )
                        });
                    assert!(
                        logged.contains(&format!("{MASTER_USER}@{DATABASE}"))
                            && logged.ends_with(&format!(":LOG:  {LOG_MARKER}")),
                        "case {:?}: expected a PostgreSQL log line naming the session and \
                         the severity, got {logged:?}",
                        row.case
                    );

                    // The `NOTICE` is NOT logged: it is below `log_min_messages`' default.
                    assert!(
                        !messages
                            .iter()
                            .any(|message| message.contains("counted 2 of them")),
                        "case {:?}: a NOTICE is not written to the engine's log, got \
                         {messages:?}",
                        row.case
                    );

                    // The `WARNING` the aborting block raised BEFORE it failed is written
                    // all the same: a message the server produced is not lost because the
                    // statement it belongs to then failed.
                    assert!(
                        messages.iter().any(|message| {
                            message.ends_with(&format!(":WARNING:  {WARNING_BEFORE_THE_ABORT}"))
                        }),
                        "case {:?}: the WARNING raised before the block aborted must be a \
                         line of the log, got {messages:?}",
                        row.case
                    );

                    // Each refusal is an `ERROR` line carrying the refusal's own sentence,
                    // and a filter over the group — what a metric filter counting the
                    // group's errors matches on — matches exactly those lines.
                    let errors =
                        filtered_events(&router, row.case, &log_group, Some("ERROR")).await;
                    let error_messages: Vec<&str> =
                        errors.iter().map(|(_, message)| message.as_str()).collect();
                    assert_eq!(
                        error_messages.len(),
                        refusals.len(),
                        "case {:?}: the group's ERROR lines are the statements the engine \
                         refused, got {error_messages:?}",
                        row.case
                    );
                    for refusal in &refusals {
                        assert!(
                            error_messages.iter().any(|message| {
                                message.ends_with(&format!(":ERROR:  {}", refusal.message))
                            }),
                            "case {:?}: the refusal {:?} must be an ERROR line of the \
                             group, got {error_messages:?}",
                            row.case,
                            refusal.message
                        );
                    }
                }
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}

/// A MEMBER instance's engine log goes to its CLUSTER's group, in the stream named after
/// that instance: the export is configured on the cluster, and the stream names the DB
/// instance whose engine wrote the line.
#[test]
fn a_member_instances_engine_log_goes_to_its_clusters_group_in_its_own_stream() {
    std::thread::Builder::new()
        .name("rds-member-log-export".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                const CASE: &str = "a member instance of an exporting cluster";
                const CLUSTER: &str = "obs-export-member-cluster";
                const MEMBER: &str = "obs-export-member-writer";
                available_cluster(&router, CASE, CLUSTER, &["postgresql"]).await;
                rds(
                    &router,
                    "CreateDBInstance",
                    &params![
                        "DBInstanceIdentifier" => MEMBER,
                        "DBClusterIdentifier" => CLUSTER,
                        "Engine" => "aurora-postgresql",
                        "DBInstanceClass" => "db.r6g.large",
                    ],
                )
                .await
                .served(CASE);
                // The member's own endpoint, once the instance reports `available`.
                let deadline = std::time::Instant::now() + POLL_DEADLINE;
                let (address, port) = loop {
                    let described = rds(
                        &router,
                        "DescribeDBInstances",
                        &params!["DBInstanceIdentifier" => MEMBER],
                    )
                    .await
                    .served(CASE);
                    if element(&described, "DBInstanceStatus") == "available" {
                        let port: u16 = element(&described, "Port")
                            .parse()
                            .unwrap_or_else(|error| {
                                panic!("case {CASE:?}: the member publishes a port: {error}")
                            });
                        break (element(&described, "Address"), port);
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "case {CASE:?}: the member instance must reach `available`"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                };

                let mut session = Session::open(CASE, &address, port).await;
                session
                    .query(CASE, "DO $$ BEGIN RAISE LOG 'the member wrote this'; END $$")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {CASE:?}: the block must be served, got {refusal:?}")
                    });

                let log_group = format!("/aws/rds/cluster/{CLUSTER}/{POSTGRESQL_LOG_TYPE}");
                let deadline = std::time::Instant::now() + POLL_DEADLINE;
                let events = loop {
                    let events = filtered_events(&router, CASE, &log_group, None).await;
                    if !events.is_empty() {
                        break events;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "case {CASE:?}: the member's line must reach {log_group}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                };
                assert!(
                    events.iter().any(|(stream, message)| {
                        stream == MEMBER && message.contains("the member wrote this")
                    }),
                    "case {CASE:?}: the member's line must be in the stream named after it, \
                     got {events:?}"
                );
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}
