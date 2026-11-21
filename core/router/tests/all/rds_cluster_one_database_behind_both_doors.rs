// SPDX-License-Identifier: CC0-1.0

//! A DB CLUSTER's Data API HTTP endpoint and its own ENGINE endpoint address ONE database.
//!
//! RULE: a DB cluster is one database behind several doors — the engine endpoints a client
//! dials with its ordinary driver, and, for an Aurora cluster whose HTTP endpoint is on, the
//! RDS Data API, whose `ExecuteStatement` "runs a SQL statement against a database" of that
//! same cluster. So a statement committed through either door is committed IN THE CLUSTER: a
//! table one door created, and the rows it holds, are what a session on the other door reads
//! back, under the same database name, and neither door sees a table nothing created.
//!
//! Only a cross-service suite can pin this: the cluster, its published endpoints and the
//! engine behind them are Amazon RDS's, its Data API HTTP endpoint is served by the RDS Data
//! API service, and the credential that call authenticates with is Secrets Manager's — three
//! services over ONE engine, exactly as `core/router/src/lib.rs` mounts them. The native
//! client is written against the PostgreSQL protocol itself (the startup message, the
//! cleartext password and the simple-query protocol), so what drives the engine from that
//! side is what a stock driver sends; it reads result ROWS and logs in on a database the row
//! names, which is why it is this suite's own rather than the sibling log-export suite's.
//!
//! The rows span what the rule generalises over: WHICH door wrote the table (the Data API and
//! a native session), WHICH database it is (the one the cluster was created with, named by
//! the Data API call and named by none — "the connection's default database" — and the
//! database the PostgreSQL engine itself comes with), and what is read back (a row's value
//! and how many rows the table holds). Beside them: the writes of a Data API TRANSACTION,
//! which the other door sees exactly once that transaction commits — and which are ADDED to
//! what the other door committed while it was open, not installed over it — the ROW such a
//! transaction holds while it is open, which a native `SELECT … FOR UPDATE NOWAIT` is refused
//! and a neighbouring row of the same table is not, and the refusal both doors answer for a
//! relation nothing created.
//!
//! Every control-plane call goes over the wire, on a router with request authentication off
//! as every suite in this crate runs it, and every wait is a poll for the CONDITION under a
//! generous deadline. Assembling the whole router costs stack (see `boot_serve.rs`), so it is
//! built — and the requests driven — on a sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::query_wire::{element, form_encode};

/// Keep this at or above the stack the other whole-router suites build on: the same assembly
/// runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
const REGION: &str = "us-west-2";

/// The master user the cluster is created with, and the password the secret carries — a test
/// credential, and the only place it appears.
const MASTER_USER: &str = "clusteradmin";
/// See [`MASTER_USER`].
const MASTER_PASSWORD: &str = "emu-Secret-77";

/// The database the cluster is created with: its `DatabaseName`, which is the database a Data
/// API call that names none addresses.
const DATABASE: &str = "shareddb";

/// The database the PostgreSQL engine itself comes with, which a cluster running it therefore
/// has as well — the second database name both doors address.
const ENGINE_DATABASE: &str = "postgres";

/// The engine version the cluster runs.
///
/// Amazon RDS publishes `rds.force_ssl`'s default as 1 for the PostgreSQL 15 and later release
/// lines and 0 before them, so a cluster of this line admits the PLAINTEXT session this
/// suite's client opens — the in-band TLS upgrade is the subject of the RDS crate's own
/// endpoint suites, not of this one.
const ENGINE_VERSION: &str = "14.12";

/// How long a poll waits for a state the RDS control plane owes before giving up: generous, so
/// a loaded host does not fail a condition that does arrive, and far short of a hang.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads. The condition, not the clock, is what ends the
/// loop; this only keeps it from spinning.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `[(name, value.to_string()), ..]` — the parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One table seeded through ONE door of the cluster and read back through the OTHER.
struct Crossing {
    /// What this row is a case of.
    case: &'static str,
    /// The `database` member the Data API calls of this row carry, or `None` for a call that
    /// names none — which addresses the database the cluster was created with.
    database: Option<&'static str>,
    /// The database the native session of this row logs in on, which is that same database of
    /// the cluster.
    native_database: &'static str,
    /// The table this row creates, which no other row names.
    table: &'static str,
    /// Whether the DATA API is the door that wrote the table; else the native session wrote it
    /// and the Data API reads it back.
    written_through_the_data_api: bool,
}

/// The crossings: each database name both doors address, each door as the writer.
const CROSSINGS: &[Crossing] = &[
    Crossing {
        case: "a table the Data API created in the database its calls name",
        database: Some(DATABASE),
        native_database: DATABASE,
        table: "api_named_rows",
        written_through_the_data_api: true,
    },
    Crossing {
        case: "a table the Data API created in the cluster's own database, named by no call",
        database: None,
        native_database: DATABASE,
        table: "api_default_rows",
        written_through_the_data_api: true,
    },
    Crossing {
        case: "a table a native session created in the cluster's own database",
        database: Some(DATABASE),
        native_database: DATABASE,
        table: "native_named_rows",
        written_through_the_data_api: false,
    },
    Crossing {
        case: "a table the Data API created in the engine's own database",
        database: Some(ENGINE_DATABASE),
        native_database: ENGINE_DATABASE,
        table: "api_engine_rows",
        written_through_the_data_api: true,
    },
    Crossing {
        case: "a table a native session created in the engine's own database",
        database: Some(ENGINE_DATABASE),
        native_database: ENGINE_DATABASE,
        table: "native_engine_rows",
        written_through_the_data_api: false,
    },
];

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

/// One call to the cluster's Data API HTTP endpoint at `path`, as its JSON client sends one.
async fn data_api(router: &MultiServiceRouter, path: &str, call: &Value) -> Answer {
    dispatch(
        router,
        "rds-data",
        "POST",
        path,
        &[("content-type", "application/json")],
        call.to_string(),
    )
    .await
}

/// One `ExecuteStatement` on the cluster `resource_arn`, in the database `database` (or in the
/// cluster's own, for a call that names none) and inside `transaction_id` where it names one.
async fn execute_statement(
    router: &MultiServiceRouter,
    resource_arn: &str,
    secret_arn: &str,
    database: Option<&str>,
    transaction_id: Option<&str>,
    sql: &str,
) -> Answer {
    let mut call = json!({
        "resourceArn": resource_arn,
        "secretArn": secret_arn,
        "sql": sql,
    });
    if let Some(database) = database {
        call["database"] = Value::String(database.to_string());
    }
    if let Some(transaction_id) = transaction_id {
        call["transactionId"] = Value::String(transaction_id.to_string());
    }
    data_api(router, "/Execute", &call).await
}

/// The values of one column of an `ExecuteStatement` answer, as the `Field` members it reports
/// them in: a string as it stands, a number and a boolean as the client would print them.
fn api_column(case: &str, body: &str) -> Vec<String> {
    let answer: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("case {case:?}: the Data API answers JSON, got {error}"));
    let records = answer["records"].as_array().cloned().unwrap_or_default();
    records
        .iter()
        .map(|record| {
            let field = &record.as_array().and_then(|fields| fields.first().cloned())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every record carries its column, got {answer}")
                });
            for member in ["stringValue", "longValue", "doubleValue", "booleanValue"] {
                if let Some(value) = field.get(member) {
                    return match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                }
            }
            panic!("case {case:?}: the field carries a value this suite reads, got {field}")
        })
        .collect()
}

/// A refusal the PostgreSQL server sent: the SQLSTATE a client branches on and the sentence it
/// reports.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    code: String,
    message: String,
}

/// A PostgreSQL session opened on the cluster's engine endpoint, in the clear.
///
/// It reads result ROWS, which is what this suite asserts on, and logs in on the database the
/// case names.
struct Session {
    conn: TcpStream,
}

impl Session {
    /// Open a session on `address:port` as libpq does: the startup message naming the user and
    /// the database, the cleartext password, and then the parameters the server reports.
    async fn open(case: &str, address: &str, port: u16, database: &str) -> Self {
        assert_eq!(
            address, "127.0.0.1",
            "case {case:?}: the published endpoint must be this machine's loopback address, \
             got {address:?}"
        );
        let conn = TcpStream::connect(("127.0.0.1", port))
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
        put_cstring(&mut body, database);
        body.push(0);
        let mut session = Self { conn };
        let mut startup = Vec::new();
        startup.extend_from_slice(
            &i32::try_from(body.len() + 4)
                .expect("the startup message fits")
                .to_be_bytes(),
        );
        startup.extend_from_slice(&body);
        session
            .conn
            .write_all(&startup)
            .await
            .expect("the startup is sent");
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
                    "case {case:?}: the master user must be admitted on {database}, got {:?}",
                    refusal_of(&body)
                ),
                // ReadyForQuery: the session is open.
                b'Z' => return session,
                // ParameterStatus, BackendKeyData, NoticeResponse.
                _ => {}
            }
        }
    }

    /// Run one simple query and answer with its rows, each field as the text the server sent
    /// (`None` for a NULL).
    async fn query(
        &mut self,
        case: &str,
        sql: &str,
    ) -> Result<Vec<Vec<Option<String>>>, Refusal> {
        let mut message = Vec::new();
        put_cstring(&mut message, sql);
        self.send(b'Q', &message).await;
        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        let mut refusal = None;
        loop {
            let (kind, body) = self.read_message(case).await;
            match kind {
                b'E' => refusal = Some(refusal_of(&body)),
                // DataRow: the fields of one row, in the text format a simple query returns.
                b'D' => rows.push(data_row(&body)),
                // ReadyForQuery ends the cycle.
                b'Z' => break,
                // RowDescription, CommandComplete, NoticeResponse, EmptyQueryResponse.
                _ => {}
            }
        }
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(rows),
        }
    }

    /// Run one statement that must be served, and answer with its rows.
    async fn served(&mut self, case: &str, sql: &str) -> Vec<Vec<Option<String>>> {
        self.query(case, sql).await.unwrap_or_else(|refusal| {
            panic!("case {case:?}: {sql:?} must be served, got {refusal:?}")
        })
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

/// The fields of one `DataRow`: a 16-bit count, then each field's length and bytes, with -1
/// standing for a NULL.
fn data_row(body: &[u8]) -> Vec<Option<String>> {
    let mut fields = Vec::new();
    let count = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let mut at = 2usize;
    for _ in 0..count {
        let length = i32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
        at += 4;
        if length < 0 {
            fields.push(None);
            continue;
        }
        let length = usize::try_from(length).expect("a field length fits");
        fields.push(Some(
            String::from_utf8_lossy(&body[at..at + length]).into_owned(),
        ));
        at += length;
    }
    fields
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

/// The cluster this suite serves both doors of: its ARN, the credential its Data API calls
/// authenticate with, and the address its engine endpoint publishes.
struct Provisioned {
    cluster_arn: String,
    secret_arn: String,
    address: String,
    port: u16,
}

/// Create the cluster with its Data API HTTP endpoint on and a secret carrying its master
/// credential, and wait for the cluster to report `available`.
async fn provisioned(router: &MultiServiceRouter, identifier: &str, secret: &str) -> Provisioned {
    let created = rds(
        router,
        "CreateDBCluster",
        &params![
            "DBClusterIdentifier" => identifier,
            "Engine" => "aurora-postgresql",
            "EngineVersion" => ENGINE_VERSION,
            "MasterUsername" => MASTER_USER,
            "MasterUserPassword" => MASTER_PASSWORD,
            "DatabaseName" => DATABASE,
            "EnableHttpEndpoint" => "true",
        ],
    )
    .await
    .served("create the DB cluster");
    let cluster_arn = element(&created, "DBClusterArn");

    // A client connects once the cluster reports `available`, which the clock makes due:
    // looked for again until it holds rather than waited out.
    let deadline = Instant::now() + POLL_DEADLINE;
    let (address, port) = loop {
        let described = rds(
            router,
            "DescribeDBClusters",
            &params!["DBClusterIdentifier" => identifier],
        )
        .await
        .served("describe the DB cluster");
        if element(&described, "Status") == "available" {
            let port: u16 = element(&described, "Port").parse().unwrap_or_else(|error| {
                panic!("the cluster publishes a port: {error}");
            });
            break (element(&described, "Endpoint"), port);
        }
        assert!(
            Instant::now() < deadline,
            "the cluster must reach `available`, last answer was {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    let minted = dispatch(
        router,
        "secretsmanager",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", "secretsmanager.CreateSecret"),
        ],
        json!({
            "Name": secret,
            // An SDK generates this idempotency token itself; a raw request carries its own.
            "ClientRequestToken": "6b1d4c90-5f21-4a7e-9c30-2d8e5f7a1b43",
            "SecretString":
                format!("{{\"username\":\"{MASTER_USER}\",\"password\":\"{MASTER_PASSWORD}\"}}"),
        })
        .to_string(),
    )
    .await
    .served("create the cluster's credential secret");
    let minted: Value = serde_json::from_str(&minted).expect("Secrets Manager answers JSON");
    let secret_arn = minted["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("Secrets Manager reports the secret it minted: {minted}"))
        .to_string();

    Provisioned {
        cluster_arn,
        secret_arn,
        address,
        port,
    }
}

/// A table created through either door of the cluster is a table OF THE CLUSTER: the other
/// door reads it, and its rows, back under the same database name — and the writes of a Data
/// API transaction reach it exactly when that transaction commits.
#[test]
fn a_table_created_through_either_door_of_a_cluster_is_read_through_the_other() {
    std::thread::Builder::new()
        .name("rds-cluster-one-database".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: a cluster
            // create opens the cluster's engine listener through the core, and binding a
            // socket needs the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                let cluster =
                    provisioned(&router, "one-database-cluster", "one-database-secret").await;

                for crossing in CROSSINGS {
                    let case = crossing.case;
                    let table = crossing.table;
                    let create = format!(
                        "CREATE TABLE {table} (id INT PRIMARY KEY, holder VARCHAR(40) NOT NULL)"
                    );
                    let seed = format!(
                        "INSERT INTO {table} (id, holder) VALUES (1, 'first'), (2, 'second')"
                    );
                    let mut session = Session::open(
                        case,
                        &cluster.address,
                        cluster.port,
                        crossing.native_database,
                    )
                    .await;
                    if crossing.written_through_the_data_api {
                        for statement in [&create, &seed] {
                            execute_statement(
                                &router,
                                &cluster.cluster_arn,
                                &cluster.secret_arn,
                                crossing.database,
                                None,
                                statement,
                            )
                            .await
                            .served(case);
                        }
                        // The other door: the native session on the cluster's writer
                        // endpoint, which must hold the table and its rows.
                        let read = session
                            .served(case, &format!("SELECT holder FROM {table} WHERE id = 2"))
                            .await;
                        assert_eq!(
                            read,
                            vec![vec![Some("second".to_string())]],
                            "case {case:?}: the native session must read the row the Data API \
                             committed, got {read:?}"
                        );
                        let counted = session
                            .served(case, &format!("SELECT count(*) FROM {table}"))
                            .await;
                        assert_eq!(
                            counted,
                            vec![vec![Some("2".to_string())]],
                            "case {case:?}: the native session must read every row the table \
                             holds, got {counted:?}"
                        );
                    } else {
                        session.served(case, &create).await;
                        session.served(case, &seed).await;
                        // The other door: an `ExecuteStatement` on the same cluster, which
                        // must hold the table and its rows.
                        let read = execute_statement(
                            &router,
                            &cluster.cluster_arn,
                            &cluster.secret_arn,
                            crossing.database,
                            None,
                            &format!("SELECT holder FROM {table} WHERE id = 2"),
                        )
                        .await
                        .served(case);
                        assert_eq!(
                            api_column(case, &read),
                            vec!["second".to_string()],
                            "case {case:?}: the Data API must read the row the native session \
                             committed, got {read}"
                        );
                        let counted = execute_statement(
                            &router,
                            &cluster.cluster_arn,
                            &cluster.secret_arn,
                            crossing.database,
                            None,
                            &format!("SELECT count(*) FROM {table}"),
                        )
                        .await
                        .served(case);
                        assert_eq!(
                            api_column(case, &counted),
                            vec!["2".to_string()],
                            "case {case:?}: the Data API must read every row the table holds, \
                             got {counted}"
                        );
                    }
                }

                // The writes of a Data API TRANSACTION: invisible to the other door while the
                // transaction is open, and its own once it commits — which is what a
                // transaction of the cluster is, whichever door opened it.
                const IN_TRANSACTION: &str = "a row a Data API transaction wrote";
                let opened = data_api(
                    &router,
                    "/BeginTransaction",
                    &json!({
                        "resourceArn": cluster.cluster_arn,
                        "secretArn": cluster.secret_arn,
                        "database": DATABASE,
                    }),
                )
                .await
                .served(IN_TRANSACTION);
                let opened: Value =
                    serde_json::from_str(&opened).expect("the Data API answers JSON");
                let transaction_id = opened["transactionId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("BeginTransaction reports its id: {opened}"))
                    .to_string();
                execute_statement(
                    &router,
                    &cluster.cluster_arn,
                    &cluster.secret_arn,
                    None,
                    Some(&transaction_id),
                    "INSERT INTO api_named_rows (id, holder) VALUES (3, 'in a transaction')",
                )
                .await
                .served(IN_TRANSACTION);
                let mut session =
                    Session::open(IN_TRANSACTION, &cluster.address, cluster.port, DATABASE).await;
                let uncommitted = session
                    .served(
                        IN_TRANSACTION,
                        "SELECT count(*) FROM api_named_rows WHERE id = 3",
                    )
                    .await;
                assert_eq!(
                    uncommitted,
                    vec![vec![Some("0".to_string())]],
                    "case {IN_TRANSACTION:?}: a row no commit has installed is invisible to \
                     the other door, got {uncommitted:?}"
                );
                // And a row the OTHER door commits while that transaction is open is a row of
                // the cluster: the commit that follows adds this transaction's writes to it
                // rather than installing a database that never held it.
                session
                    .served(
                        IN_TRANSACTION,
                        "INSERT INTO api_named_rows (id, holder) VALUES (4, 'from the session')",
                    )
                    .await;
                data_api(
                    &router,
                    "/CommitTransaction",
                    &json!({
                        "resourceArn": cluster.cluster_arn,
                        "secretArn": cluster.secret_arn,
                        "transactionId": transaction_id,
                    }),
                )
                .await
                .served(IN_TRANSACTION);
                let committed = session
                    .served(
                        IN_TRANSACTION,
                        "SELECT id, holder FROM api_named_rows WHERE id IN (3, 4) ORDER BY id",
                    )
                    .await;
                assert_eq!(
                    committed,
                    vec![
                        vec![Some("3".to_string()), Some("in a transaction".to_string())],
                        vec![Some("4".to_string()), Some("from the session".to_string())],
                    ],
                    "case {IN_TRANSACTION:?}: the commit must ADD the transaction's row to \
                     what the other door committed meanwhile, got {committed:?}"
                );

                // And the ROWS such a transaction has written are rows of the cluster it
                // HOLDS: a native session asking for one of them without waiting is refused
                // the lock, a neighbouring row of the same table is free, and the commit
                // hands the row over with the value the transaction wrote.
                const UNDER_LOCK: &str = "a row a Data API transaction holds";
                let opened = data_api(
                    &router,
                    "/BeginTransaction",
                    &json!({
                        "resourceArn": cluster.cluster_arn,
                        "secretArn": cluster.secret_arn,
                        "database": DATABASE,
                    }),
                )
                .await
                .served(UNDER_LOCK);
                let opened: Value =
                    serde_json::from_str(&opened).expect("the Data API answers JSON");
                let transaction_id = opened["transactionId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("BeginTransaction reports its id: {opened}"))
                    .to_string();
                execute_statement(
                    &router,
                    &cluster.cluster_arn,
                    &cluster.secret_arn,
                    None,
                    Some(&transaction_id),
                    "UPDATE api_named_rows SET holder = 'held' WHERE id = 1",
                )
                .await
                .served(UNDER_LOCK);
                let free = session
                    .served(
                        UNDER_LOCK,
                        "SELECT holder FROM api_named_rows WHERE id = 2 FOR UPDATE NOWAIT",
                    )
                    .await;
                assert_eq!(
                    free,
                    vec![vec![Some("second".to_string())]],
                    "case {UNDER_LOCK:?}: a row the transaction did not write is uncontended, \
                     got {free:?}"
                );
                let contended = session
                    .query(
                        UNDER_LOCK,
                        "SELECT holder FROM api_named_rows WHERE id = 1 FOR UPDATE NOWAIT",
                    )
                    .await
                    .expect_err(&format!(
                        "case {UNDER_LOCK:?}: a row another transaction holds must be refused \
                         to a statement that asked not to wait"
                    ));
                assert_eq!(
                    contended,
                    Refusal {
                        code: "55P03".to_string(),
                        message:
                            "could not obtain lock on row in relation \"api_named_rows\""
                                .to_string(),
                    },
                    "case {UNDER_LOCK:?}: expected PostgreSQL's own lock refusal, got \
                     {contended:?}"
                );
                data_api(
                    &router,
                    "/CommitTransaction",
                    &json!({
                        "resourceArn": cluster.cluster_arn,
                        "secretArn": cluster.secret_arn,
                        "transactionId": transaction_id,
                    }),
                )
                .await
                .served(UNDER_LOCK);
                let handed_over = session
                    .served(
                        UNDER_LOCK,
                        "SELECT holder FROM api_named_rows WHERE id = 1 FOR UPDATE NOWAIT",
                    )
                    .await;
                assert_eq!(
                    handed_over,
                    vec![vec![Some("held".to_string())]],
                    "case {UNDER_LOCK:?}: once the transaction commits the row is free and \
                     carries what it wrote, got {handed_over:?}"
                );
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}

/// A relation NOTHING created is held by neither door, and each refuses the statement naming
/// it in the words of the engine the cluster runs: PostgreSQL's `relation "<name>" does not
/// exist`, under the SQLSTATE of an undefined table.
///
/// The rows differ along the name the statement reads, including one a case above creates in
/// ANOTHER database of the cluster — so what is refused is the relation the addressed database
/// does not hold rather than a name nobody ever used.
#[test]
fn a_relation_neither_door_created_is_refused_in_postgresqls_own_words() {
    std::thread::Builder::new()
        .name("rds-cluster-missing-relation".to_string())
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
                let cluster =
                    provisioned(&router, "missing-relation-cluster", "missing-relation-secret")
                        .await;
                // The relation of another database of the cluster, so one row reads a name
                // that exists somewhere and not here.
                execute_statement(
                    &router,
                    &cluster.cluster_arn,
                    &cluster.secret_arn,
                    Some(ENGINE_DATABASE),
                    None,
                    "CREATE TABLE elsewhere_rows (id INT PRIMARY KEY)",
                )
                .await
                .served("the relation of another database");

                let mut session =
                    Session::open("a missing relation", &cluster.address, cluster.port, DATABASE)
                        .await;
                for (case, relation) in [
                    ("a relation nothing created", "nothing_here"),
                    ("a relation of another database of the cluster", "elsewhere_rows"),
                ] {
                    let refusal = session
                        .query(case, &format!("SELECT * FROM {relation}"))
                        .await
                        .expect_err(&format!(
                            "case {case:?}: a relation the database does not hold must be \
                             refused"
                        ));
                    assert_eq!(
                        refusal,
                        Refusal {
                            code: "42P01".to_string(),
                            message: format!("relation \"{relation}\" does not exist"),
                        },
                        "case {case:?}: expected PostgreSQL's own refusal, got {refusal:?}"
                    );
                    // And the SAME condition through the other door, in the same words with
                    // the same SQLSTATE beside them.
                    let through_the_api = execute_statement(
                        &router,
                        &cluster.cluster_arn,
                        &cluster.secret_arn,
                        Some(DATABASE),
                        None,
                        &format!("SELECT * FROM {relation}"),
                    )
                    .await;
                    assert_eq!(
                        through_the_api.status, 400,
                        "case {case:?}: the Data API refuses the same statement, got HTTP {} \
                         with {}",
                        through_the_api.status, through_the_api.body
                    );
                    let reported: Value = serde_json::from_str(&through_the_api.body)
                        .unwrap_or_else(|error| {
                            panic!("case {case:?}: the Data API answers JSON, got {error}")
                        });
                    let message = reported["message"].as_str().unwrap_or_default().to_string();
                    assert_eq!(
                        message,
                        format!("ERROR: relation \"{relation}\" does not exist; SQLState: 42P01"),
                        "case {case:?}: expected the same condition in the engine's words, got \
                         {}",
                        through_the_api.body
                    );
                }
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}
