// SPDX-License-Identifier: CC0-1.0

//! What a Data API statement that asks the cluster's engine for ITS OWN CLOCK is answered
//! with, and how a function that engine does not have is refused.
//!
//! RULE: `ExecuteStatement` "runs a SQL statement against a database" of the DB cluster the
//! call addresses, so the statement is read in the dialect of the engine THAT CLUSTER RUNS: a
//! function that engine's own reference documents as a built-in is computed by the engine and
//! answered as a result (HTTP 200), and a function it does not define — one belonging to the
//! other engine family, or one neither family has — is refused as that engine refuses an
//! undefined function, in its own sentence with its own vendor error number and SQLSTATE
//! (MySQL `ER_SP_DOES_NOT_EXIST`, number 1305, SQLSTATE `42000`, "FUNCTION
//! `<database>.<name>` does not exist"; PostgreSQL SQLSTATE `42883`, "function `<name>()`
//! does not exist"). The backing engine of this emulator is neither of those products, so
//! none of its own wording — and nothing of the statement it was parsing — reaches the
//! caller.
//!
//! The DATE AND TIME built-ins are where a client meets that rule first: a workflow timestamps
//! a write, computes a window or compares its own clock to the engine's by asking the server
//! for the time. The value is the engine's clock at the instant the statement ran, so what is
//! asserted here is its FORM — the fields the engine's reference says that function answers
//! with, at the fractional-seconds precision the call asked for — and never one run's digits.
//! The SYSTEM INFORMATION built-ins each family documents for "which database am I in" are the
//! second kind of the same rule, and their value is the database the CALL addressed.
//!
//! The rows span what the rule generalises over: which engine the addressed cluster runs,
//! which family documents the function, what the function reads (a timestamp, a date, a time
//! of day, whole seconds since the epoch), the fractional-seconds precision the call asks
//! for, and a function neither family defines.
//!
//! The suite lives beside the router because the facts the rule turns on are other services':
//! Amazon RDS holds the cluster whose `resourceArn` a call names and the ENGINE it runs, and
//! Secrets Manager holds the credentials the call authenticates with.
//!
//! Sources: `smithy_query(query="operation", operation="ExecuteStatement")` on the rdsdata
//! model for the call's own members and its `DatabaseErrorException`; the MySQL Reference
//! Manual's "Date and Time Functions" and its "Server Error Message Reference" for
//! `ER_SP_DOES_NOT_EXIST`; the PostgreSQL documentation's "Date/Time Functions and Operators"
//! and its "Error Codes" appendix.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The database every cluster below is created with, which every statement runs against —
/// and which the MySQL refusal names the function in.
const DATABASE: &str = "appdb";

/// The master user every cluster is created with.
const MASTER_USER: &str = "appadmin";

/// How often a poll looks again, and how long it keeps looking: what the poll waits for is
/// owed by the CLOCK, so it waits for the condition under a generous deadline rather than for
/// a fixed span.
const LOOK: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(60);

/// The engine families a Data API cluster runs, and the `Engine` each is created with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    MySql,
    PostgreSql,
}

impl Family {
    /// The `Engine` a cluster of this family is created with.
    fn engine(self) -> &'static str {
        match self {
            Self::MySql => "aurora-mysql",
            Self::PostgreSql => "aurora-postgresql",
        }
    }

    /// The sentence THIS engine refuses a function it does not have with, for the function
    /// `called` as the statement named it.
    fn undefined_function(self, called: &str) -> String {
        match self {
            // `ER_SP_DOES_NOT_EXIST`: MySQL qualifies the routine by the schema it looked for
            // it in, and carries its own error number and SQLSTATE.
            Self::MySql => format!(
                "FUNCTION {DATABASE}.{called} does not exist; Error code: 1305; SQLState: 42000"
            ),
            Self::PostgreSql => {
                format!("ERROR: function {called}() does not exist; SQLState: 42883")
            }
        }
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the hops the Data API makes to Amazon RDS
/// and to Secrets Manager are dispatched back into this same router.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon RDS awsQuery call, answered with the HTTP status and the XML body.
async fn rds(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2014-10-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("rds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("rds"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Data API `ExecuteStatement`, answered with the HTTP status and the parsed body.
async fn execute(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
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

/// The cluster a Data API caller addresses: the engine it runs, its ARN and the ARN of the
/// secret holding the credentials a call authenticates with.
struct Cluster {
    family: Family,
    arn: String,
    secret_arn: String,
}

impl Cluster {
    /// Create one cluster of `family` with the Data API turned on and RDS managing the master
    /// user password, waiting until the secret it minted can be used — which is the credential
    /// every statement below authenticates with.
    async fn create(router: &MultiServiceRouter, identifier: &str, family: Family) -> Self {
        let (status, created) = rds(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", family.engine()),
                ("MasterUsername", MASTER_USER),
                ("ManageMasterUserPassword", "true"),
                ("DatabaseName", DATABASE),
                ("EnableHttpEndpoint", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateDBCluster({identifier:?}) must succeed, got {status} with {created}"
        );
        let secret = block_of(&created, "MasterUserSecret").unwrap_or_else(|| {
            panic!("setup: a managed-password cluster reports its secret: {created}")
        });
        let held = Self {
            family,
            arn: element(&created, "DBClusterArn"),
            secret_arn: element(secret, "SecretArn"),
        };
        held.await_usable_secret(router, identifier).await;
        held
    }

    /// Poll the cluster until the secret RDS minted for it reports `active`: the credentials
    /// are being created when the call that asked for them answers, and only the clock
    /// settles them.
    async fn await_usable_secret(&self, router: &MultiServiceRouter, identifier: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let (status, read) = rds(
                router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier)],
            )
            .await;
            assert_eq!(
                status, 200,
                "setup: DescribeDBClusters({identifier:?}) must succeed, got {status} with {read}"
            );
            let reported = block_of(&read, "MasterUserSecret")
                .and_then(|secret| opt_element(secret, "SecretStatus"))
                .unwrap_or_default();
            if reported == "active" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "setup: the managed secret of {identifier:?} must become usable, last reported \
                 {reported:?} in {read}"
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Run `sql` and answer the HTTP status and the parsed response, whatever it is.
    async fn statement(&self, router: &MultiServiceRouter, sql: &str) -> (u16, Value) {
        execute(
            router,
            &json!({
                "resourceArn": self.arn,
                "secretArn": self.secret_arn,
                "database": DATABASE,
                "sql": sql,
            }),
        )
        .await
    }

    /// The ONE value a statement that must be served answered with.
    async fn value(&self, router: &MultiServiceRouter, case: &str, sql: &str) -> String {
        let (status, answered) = self.statement(router, sql).await;
        assert_eq!(
            status, 200,
            "case {case:?} on {}: {sql:?} must be served, got {status} with {answered}",
            self.family.engine()
        );
        let records = answered["records"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            records.len(),
            1,
            "case {case:?} on {}: {sql:?} answers exactly one row, got {answered}",
            self.family.engine()
        );
        let fields = records[0].as_array().cloned().unwrap_or_default();
        assert_eq!(
            fields.len(),
            1,
            "case {case:?} on {}: {sql:?} answers exactly one column, got {answered}",
            self.family.engine()
        );
        for member in ["stringValue", "longValue"] {
            if let Some(value) = fields[0].get(member) {
                return match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
            }
        }
        panic!(
            "case {case:?} on {}: the field carries a value this suite reads, got {answered}",
            self.family.engine()
        )
    }

    /// The MESSAGE a statement that must be refused was refused with.
    async fn refusal(&self, router: &MultiServiceRouter, case: &str, sql: &str) -> String {
        let (status, refused) = self.statement(router, sql).await;
        assert_eq!(
            status, 400,
            "case {case:?} on {}: {sql:?} must be refused as a database error, got {status} \
             with {refused}",
            self.family.engine()
        );
        refused["message"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?} on {}: a refusal carries its message, got {refused}",
                    self.family.engine()
                )
            })
            .to_string()
    }
}

/// Whether `value` has the shape `mask` spells: a `0` stands for one decimal digit and every
/// other character for itself.
fn shaped_like(value: &str, mask: &str) -> bool {
    value.chars().count() == mask.chars().count()
        && value.chars().zip(mask.chars()).all(|(held, expected)| {
            if expected == '0' {
                held.is_ascii_digit()
            } else {
                held == expected
            }
        })
}

/// One clock built-in: the statement a client sends, which family documents it, and the shape
/// of the value it answers with (empty for one that answers a number).
struct Builtin {
    case: &'static str,
    /// The call as the client writes it, and the function's own name, which is what a refusal
    /// names.
    call: &'static str,
    name: &'static str,
    family: Family,
    /// The mask the answered value must have, or `None` for a reading that is a number.
    shape: Option<&'static str>,
}

/// The built-ins each family's reference documents, with the shape each answers in.
///
/// MySQL renders a date and time with a space between the two and the fractional seconds the
/// `fsp` asked for (none at all when the call asks for none); PostgreSQL's `timestamp with
/// time zone` carries the microseconds it holds. The ZONE such a value carries is not part of
/// what a Data API caller reads back — the API displays a date-and-time value without a zone
/// designator — so no mask below carries one.
const BUILTINS: &[Builtin] = &[
    Builtin {
        case: "the engine's UTC clock to microseconds",
        call: "SELECT UTC_TIMESTAMP(6)",
        name: "utc_timestamp",
        family: Family::MySql,
        shape: Some("0000-00-00 00:00:00.000000"),
    },
    Builtin {
        case: "the engine's UTC clock to milliseconds",
        call: "SELECT UTC_TIMESTAMP(3)",
        name: "utc_timestamp",
        family: Family::MySql,
        shape: Some("0000-00-00 00:00:00.000"),
    },
    Builtin {
        case: "the engine's UTC clock in whole seconds",
        call: "SELECT UTC_TIMESTAMP()",
        name: "utc_timestamp",
        family: Family::MySql,
        shape: Some("0000-00-00 00:00:00"),
    },
    Builtin {
        case: "the engine's own clock",
        call: "SELECT NOW()",
        name: "now",
        // `NOW()` is documented by BOTH families, so it is served on either cluster; the row
        // below spans the PostgreSQL rendering of it.
        family: Family::MySql,
        shape: Some("0000-00-00 00:00:00"),
    },
    Builtin {
        case: "the engine's clock as the other family renders it",
        call: "SELECT now()",
        name: "now",
        family: Family::PostgreSql,
        shape: Some("0000-00-00 00:00:00.000000"),
    },
    Builtin {
        case: "the engine's clock read at the moment of the call",
        call: "SELECT clock_timestamp()",
        name: "clock_timestamp",
        family: Family::PostgreSql,
        shape: Some("0000-00-00 00:00:00.000000"),
    },
    Builtin {
        case: "the instant the transaction began",
        call: "SELECT transaction_timestamp()",
        name: "transaction_timestamp",
        family: Family::PostgreSql,
        shape: Some("0000-00-00 00:00:00.000000"),
    },
    Builtin {
        case: "the engine's own date",
        call: "SELECT CURDATE()",
        name: "curdate",
        family: Family::MySql,
        shape: Some("0000-00-00"),
    },
    Builtin {
        case: "the engine's UTC date",
        call: "SELECT UTC_DATE()",
        name: "utc_date",
        family: Family::MySql,
        shape: Some("0000-00-00"),
    },
    Builtin {
        case: "the engine's own time of day",
        call: "SELECT CURTIME()",
        name: "curtime",
        family: Family::MySql,
        shape: Some("00:00:00"),
    },
    Builtin {
        case: "the engine's UTC time of day to microseconds",
        call: "SELECT UTC_TIME(6)",
        name: "utc_time",
        family: Family::MySql,
        shape: Some("00:00:00.000000"),
    },
    Builtin {
        case: "the engine's clock as whole seconds since the epoch",
        call: "SELECT UNIX_TIMESTAMP()",
        name: "unix_timestamp",
        family: Family::MySql,
        shape: None,
    },
];

/// A function NEITHER family documents, which both engines refuse in their own words.
const NO_SUCH_BUILTIN: &str = "not_a_builtin";

/// The backing engine's OWN wording for a function it does not have, which no client of
/// either Aurora engine may ever see.
const BACKING_ENGINE_SENTENCE: &str = "no such function";

#[tokio::test]
async fn a_clusters_engine_computes_its_own_clock_builtins_and_refuses_the_others() {
    let router = build_router();
    let mysql = Cluster::create(&router, "emu-xsvc-clock-mysql", Family::MySql).await;
    let postgres = Cluster::create(&router, "emu-xsvc-clock-pg", Family::PostgreSql).await;

    for builtin in BUILTINS {
        let case = builtin.case;
        let (serves, refuses) = match builtin.family {
            Family::MySql => (&mysql, &postgres),
            Family::PostgreSql => (&postgres, &mysql),
        };

        // The cluster whose engine documents the built-in COMPUTES it: one row, one column,
        // carrying the reading that function answers with, in the form its own engine renders
        // it.
        let answered = serves.value(&router, case, builtin.call).await;
        match builtin.shape {
            Some(mask) => assert!(
                shaped_like(&answered, mask),
                "case {case:?} on {}: expected a value shaped {mask:?} from {}, got {answered:?}",
                serves.family.engine(),
                builtin.call
            ),
            // A reading that is a count of seconds: a number the engine computed from its
            // clock, which is what its own reference says the function answers.
            None => {
                let seconds: i64 = answered.parse().unwrap_or_else(|error| {
                    panic!(
                        "case {case:?} on {}: expected whole seconds from {}, got \
                         {answered:?} ({error})",
                        serves.family.engine(),
                        builtin.call
                    )
                });
                assert!(
                    seconds > 0,
                    "case {case:?} on {}: expected an instant past the epoch, got {seconds}",
                    serves.family.engine()
                );
            }
        }

        // ... and the one whose engine does NOT have it refuses it in that engine's own
        // sentence, with that engine's own code — unless both families document it, which is
        // the one row below that is served on either cluster.
        if builtin.name == "now" {
            continue;
        }
        let refused = refuses.refusal(&router, case, builtin.call).await;
        assert_eq!(
            refused,
            refuses.family.undefined_function(builtin.name),
            "case {case:?} on {}: expected the engine's own undefined-function sentence for \
             {}, got {refused:?}",
            refuses.family.engine(),
            builtin.call
        );
        assert!(
            !refused.contains(BACKING_ENGINE_SENTENCE) && !refused.contains(builtin.call),
            "case {case:?} on {}: a refusal carries neither the backing engine's wording nor \
             the statement it was parsing, got {refused:?}",
            refuses.family.engine()
        );
    }

    // A function NEITHER family documents is refused by each cluster in its own engine's
    // words, for the same reason: what the caller is told is the engine the cluster runs.
    for cluster in [&mysql, &postgres] {
        let case = "a function neither family documents";
        let refused = cluster
            .refusal(&router, case, &format!("SELECT {NO_SUCH_BUILTIN}()"))
            .await;
        assert_eq!(
            refused,
            cluster.family.undefined_function(NO_SUCH_BUILTIN),
            "case {case:?} on {}: expected the engine's own undefined-function sentence, got \
             {refused:?}",
            cluster.family.engine()
        );
    }

    // The same rule over the SYSTEM INFORMATION built-ins each family documents for "which
    // database am I in", whose value is the database the CALL addressed: the family's own is
    // computed by the engine, and the other family's is refused in that engine's own words.
    for (case, cluster, call, expected) in [
        (
            "PostgreSQL's own name for the current database",
            &postgres,
            "SELECT current_database()",
            DATABASE,
        ),
        (
            "PostgreSQL's current schema",
            &postgres,
            "SELECT current_schema()",
            "public",
        ),
        (
            "MySQL's own name for the current database",
            &mysql,
            "SELECT DATABASE()",
            DATABASE,
        ),
        (
            "MySQL's synonym for it",
            &mysql,
            "SELECT SCHEMA()",
            DATABASE,
        ),
    ] {
        let answered = cluster.value(&router, case, call).await;
        assert_eq!(
            answered, expected,
            "case {case:?} on {}: expected {expected:?} from {call}, got {answered:?}",
            cluster.family.engine()
        );
    }
    for (case, cluster, call, name) in [
        (
            "PostgreSQL's information function on a MySQL cluster",
            &mysql,
            "SELECT current_database()",
            "current_database",
        ),
        (
            "MySQL's information function on a PostgreSQL cluster",
            &postgres,
            "SELECT database()",
            "database",
        ),
    ] {
        let refused = cluster.refusal(&router, case, call).await;
        assert_eq!(
            refused,
            cluster.family.undefined_function(name),
            "case {case:?} on {}: expected the engine's own undefined-function sentence, got \
             {refused:?}",
            cluster.family.engine()
        );
    }

    // The built-ins are the ENGINE's, so a statement may compute WITH one: the clock a row is
    // timestamped with server-side is the value the engine wrote, read back through the API.
    mysql
        .value(
            &router,
            "a row timestamped by the engine",
            "SELECT 1 FROM (SELECT UTC_TIMESTAMP(6) AS at) AS reading WHERE at IS NOT NULL",
        )
        .await;
    for (case, ddl) in [
        (
            "a table to timestamp a row in",
            "CREATE TABLE events (id INT PRIMARY KEY, at VARCHAR(32) NOT NULL)",
        ),
        (
            "a row the engine timestamps",
            "INSERT INTO events (id, at) VALUES (1, UTC_TIMESTAMP(6))",
        ),
    ] {
        let (status, answered) = mysql.statement(&router, ddl).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {ddl:?} must be served, got {status} with {answered}"
        );
    }
    let stored = mysql
        .value(
            &router,
            "the stored timestamp",
            "SELECT at FROM events WHERE id = 1",
        )
        .await;
    assert!(
        shaped_like(&stored, "0000-00-00 00:00:00.000000"),
        "case \"the stored timestamp\": the row carries the instant the engine computed, got \
         {stored:?}"
    );
}
