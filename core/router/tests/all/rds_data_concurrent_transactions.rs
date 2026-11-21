// SPDX-License-Identifier: CC0-1.0

//! Two RDS Data API TRANSACTIONS open at once on one Aurora cluster, and which of their
//! statements contend.
//!
//! RULE: a transaction of this API holds the rows it CHANGED against every other transaction
//! of the cluster until it ends — and a row it INSERTED is not one of them: that row exists
//! for nobody else until the transaction is installed, so no other transaction can be holding,
//! reading or writing it. Two transactions inserting into one table are therefore independent,
//! each served at once and each keeping its own rows, exactly as on either engine this API
//! answers for; a row two transactions contend for is one that was already COMMITTED when both
//! of them reached it.
//!
//! Only a cross-service suite can pin this: the rows a Data API transaction holds are held in
//! the LOCK TABLE of the cluster whose storage the Data API shares with the cluster's own
//! engine endpoint, and a cluster has that storage only once Amazon RDS has provisioned it —
//! so the control plane (RDS), the credential the call authenticates with (Secrets Manager)
//! and the Data API are three services over ONE engine, as `core/router/src/lib.rs` mounts
//! them. Driven through the Data API alone, with no volume behind the cluster, nothing would
//! be held at all and the rule would be untested.
//!
//! The rows span what the rule generalises over: whether the two transactions write the same
//! table or different ones, how many rows each writes, and whether the row a statement changes
//! was inserted by its own transaction or was already committed — the last being the contended
//! case, which is what shows the lock table is engaged rather than switched off.
//!
//! Every control-plane call goes over the wire on a router with request authentication off, as
//! every suite in this crate runs it, and every wait is a poll for the CONDITION under a
//! generous deadline. Assembling the whole router costs stack (see `boot_serve.rs`), so it is
//! built — and the requests driven — on a sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

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

/// The database the cluster is created with, which a Data API call naming none addresses.
const DATABASE: &str = "ledgerdb";

/// The engine version the cluster runs.
const ENGINE_VERSION: &str = "14.12";

/// How long a poll waits for a state the RDS control plane owes before giving up: generous, so
/// a loaded host does not fail a condition that does arrive, and far short of a hang.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads. The condition, not the clock, is what ends the
/// loop; this only keeps it from spinning.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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

    /// The body as JSON.
    fn json(&self, case: &str) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("case {case:?}: JSON was expected, got {error}"))
    }
}

/// One call of `method` to the service `signing_name` at `path`, with `body`.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: String,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = ::http::Request::builder()
        .method("POST")
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
async fn rds(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> Answer {
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
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body,
    )
    .await
}

/// One call to the cluster's Data API HTTP endpoint at `path`.
async fn data_api(router: &MultiServiceRouter, path: &str, call: &Value) -> Answer {
    dispatch(
        router,
        "rds-data",
        path,
        &[("content-type", "application/json")],
        call.to_string(),
    )
    .await
}

/// The cluster this suite opens transactions on: its ARN and the credential its Data API
/// calls authenticate with.
struct Provisioned {
    cluster_arn: String,
    secret_arn: String,
}

impl Provisioned {
    /// `ExecuteStatement` of `sql`, inside `transaction` when it names one.
    async fn execute(
        &self,
        router: &MultiServiceRouter,
        transaction: Option<&str>,
        sql: &str,
    ) -> Answer {
        let mut call = json!({
            "resourceArn": self.cluster_arn,
            "secretArn": self.secret_arn,
            "sql": sql,
        });
        if let Some(transaction) = transaction {
            call["transactionId"] = Value::String(transaction.to_string());
        }
        data_api(router, "/Execute", &call).await
    }

    /// `BeginTransaction`, answering the id it minted.
    async fn begin(&self, router: &MultiServiceRouter, case: &str) -> String {
        let answer = data_api(
            router,
            "/BeginTransaction",
            &json!({ "resourceArn": self.cluster_arn, "secretArn": self.secret_arn }),
        )
        .await;
        let body = answer.json(case);
        answer.served(case);
        body["transactionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: BeginTransaction reports its id: {body}"))
            .to_string()
    }

    /// `CommitTransaction` of `transaction`.
    async fn commit(&self, router: &MultiServiceRouter, transaction: &str) -> Answer {
        data_api(
            router,
            "/CommitTransaction",
            &json!({
                "resourceArn": self.cluster_arn,
                "secretArn": self.secret_arn,
                "transactionId": transaction,
            }),
        )
        .await
    }

    /// The `id` column of a `SELECT`, outside any transaction.
    async fn committed_ids(&self, router: &MultiServiceRouter, case: &str, table: &str) -> Vec<i64> {
        let answer = self
            .execute(router, None, &format!("SELECT id FROM {table} ORDER BY id"))
            .await;
        let body = answer.json(case);
        answer.served(case);
        body["records"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|record| {
                record
                    .as_array()
                    .and_then(|fields| fields.first())
                    .and_then(|field| field.get("longValue"))
                    .and_then(Value::as_i64)
            })
            .collect()
    }
}

/// Create the cluster with its Data API HTTP endpoint on and a secret carrying its master
/// credential, and wait for the cluster to report `available`.
async fn provisioned(router: &MultiServiceRouter, identifier: &str, secret: &str) -> Provisioned {
    let created = rds(
        router,
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", identifier),
            ("Engine", "aurora-postgresql"),
            ("EngineVersion", ENGINE_VERSION),
            ("MasterUsername", MASTER_USER),
            ("MasterUserPassword", MASTER_PASSWORD),
            ("DatabaseName", DATABASE),
            ("EnableHttpEndpoint", "true"),
        ],
    )
    .await
    .served("create the DB cluster");
    let cluster_arn = element(&created, "DBClusterArn");

    // The Data API answers for a cluster the control plane reports `available`, which the
    // clock makes due: looked for again until it holds rather than waited out.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = rds(
            router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", identifier)],
        )
        .await
        .served("describe the DB cluster");
        if element(&described, "Status") == "available" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the cluster must reach `available`, last answer was {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let minted = dispatch(
        router,
        "secretsmanager",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", "secretsmanager.CreateSecret"),
        ],
        json!({
            "Name": secret,
            // An SDK generates this idempotency token itself; a raw request carries its own.
            "ClientRequestToken": "0f2c7a16-9b3d-4e58-8a71-6c4d2f9e5b80",
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
    }
}

/// One row of the independence table: what the two open transactions each insert.
struct Pair {
    /// What this row is a case of.
    case: &'static str,
    /// The table the first transaction inserts into, and the ids it writes.
    first_table: &'static str,
    first_ids: &'static [i64],
    /// The same for the second transaction, which is open at the same time.
    second_table: &'static str,
    second_ids: &'static [i64],
}

/// The pairs: one table or two, one row each or several, and a second transaction whose ids
/// fall below the first's as well as above them — none of which makes the two contend.
const PAIRS: &[Pair] = &[
    Pair {
        case: "one row each, in one table",
        first_table: "shared_rows",
        first_ids: &[10],
        second_table: "shared_rows",
        second_ids: &[20],
    },
    Pair {
        case: "several rows each, in one table",
        first_table: "shared_rows",
        first_ids: &[30, 31, 32],
        second_table: "shared_rows",
        second_ids: &[40, 41],
    },
    Pair {
        case: "the second transaction's ids below the first's, in one table",
        first_table: "shared_rows",
        first_ids: &[60],
        second_table: "shared_rows",
        second_ids: &[50],
    },
    Pair {
        case: "one table each",
        first_table: "shared_rows",
        first_ids: &[70],
        second_table: "other_rows",
        second_ids: &[70],
    },
];

/// Two open transactions inserting rows of their own are INDEPENDENT: each insert is served
/// while the other transaction is still open, and each commit keeps its own rows. The
/// contended case — two transactions changing a row that was already committed — is the
/// control that shows the rows a transaction holds are held at all.
#[test]
fn two_open_transactions_inserting_their_own_rows_do_not_contend() {
    std::thread::Builder::new()
        .name("rds-data-concurrent-transactions".to_string())
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
                    provisioned(&router, "concurrent-txn-cluster", "concurrent-txn-secret").await;

                for table in ["shared_rows", "other_rows"] {
                    cluster
                        .execute(
                            &router,
                            None,
                            &format!(
                                "CREATE TABLE {table} (id INT PRIMARY KEY, \
                                 holder VARCHAR(40) NOT NULL)"
                            ),
                        )
                        .await
                        .served("create the table both transactions write");
                }

                for pair in PAIRS {
                    let case = pair.case;
                    let first = cluster.begin(&router, case).await;
                    let second = cluster.begin(&router, case).await;
                    assert_ne!(
                        first, second,
                        "case {case:?}: two transactions are two transactions"
                    );

                    // Interleaved, so each insert of one runs while the other is open and
                    // holding whatever it has written so far.
                    let steps = pair.first_ids.len().max(pair.second_ids.len());
                    for step in 0..steps {
                        for (table, transaction, ids) in [
                            (pair.first_table, &first, pair.first_ids),
                            (pair.second_table, &second, pair.second_ids),
                        ] {
                            let Some(id) = ids.get(step) else {
                                continue;
                            };
                            let answer = cluster
                                .execute(
                                    &router,
                                    Some(transaction),
                                    &format!(
                                        "INSERT INTO {table} (id, holder) \
                                         VALUES ({id}, 'row-{id}')"
                                    ),
                                )
                                .await;
                            assert_eq!(
                                answer.status, 200,
                                "case {case:?}: insert {id} (step {step}) must be served while \
                                 the other transaction is open — a row a transaction inserts \
                                 is nobody else's — got HTTP {} with {}",
                                answer.status, answer.body
                            );
                        }
                    }

                    cluster
                        .commit(&router, &first)
                        .await
                        .served(&format!("{case}: commit the first transaction"));
                    cluster
                        .commit(&router, &second)
                        .await
                        .served(&format!("{case}: commit the second transaction"));

                    for (table, written) in [
                        (pair.first_table, pair.first_ids),
                        (pair.second_table, pair.second_ids),
                    ] {
                        let committed = cluster.committed_ids(&router, case, table).await;
                        for id in written {
                            assert!(
                                committed.contains(id),
                                "case {case:?}: the commit keeps the row {id} its own \
                                 transaction wrote, got {committed:?} in {table}"
                            );
                        }
                    }
                }

                // THE CONTROL: a row that was already COMMITTED when both transactions
                // reached it IS contended, so the rows a transaction changes are held — the
                // independence above is the inserted row being nobody else's, not the lock
                // table being switched off.
                let case = "two transactions changing one committed row";
                cluster
                    .execute(
                        &router,
                        None,
                        "INSERT INTO shared_rows (id, holder) VALUES (900, 'committed')",
                    )
                    .await
                    .served(case);
                let holder = cluster.begin(&router, case).await;
                let waiter = cluster.begin(&router, case).await;
                cluster
                    .execute(
                        &router,
                        Some(&holder),
                        "UPDATE shared_rows SET holder = 'held' WHERE id = 900",
                    )
                    .await
                    .served(case);
                let refused = cluster
                    .execute(
                        &router,
                        Some(&waiter),
                        "UPDATE shared_rows SET holder = 'waited' WHERE id = 900",
                    )
                    .await;
                assert_eq!(
                    refused.status, 400,
                    "case {case:?}: a row another transaction holds must be refused, got \
                     HTTP {} with {}",
                    refused.status, refused.body
                );
                let body = refused.json(case);
                assert_eq!(
                    body["message"].as_str(),
                    Some("ERROR: canceling statement due to lock timeout; SQLState: 55P03"),
                    "case {case:?}: expected PostgreSQL's own sentence for a wait that ran \
                     out, got {body}"
                );

                // And the holder's own transaction is unharmed by the refusal of the other.
                cluster
                    .commit(&router, &holder)
                    .await
                    .served(&format!("{case}: commit the holder"));
                let committed = cluster.committed_ids(&router, case, "shared_rows").await;
                assert!(
                    committed.contains(&900),
                    "case {case:?}: the held row is still there, got {committed:?}"
                );
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}
