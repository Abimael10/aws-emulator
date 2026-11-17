// SPDX-License-Identifier: CC0-1.0

//! What a Data API statement that reads the SCHEMA of its database is answered with, and
//! what a name qualified by a schema resolves to.
//!
//! RULE: the Data API serves Aurora PostgreSQL, so a statement it runs resolves a qualified
//! name the way PostgreSQL does — `public` is "the default schema, in which user objects are
//! created", so `public.<relation>` selects exactly what the bare name does, and the
//! relations of `information_schema` DESCRIBE the database: `information_schema.tables` has
//! "one row for each table or view in the current database" (`table_type` `BASE TABLE` for a
//! table, `VIEW` for a view) and `information_schema.columns` "one row for each column in a
//! table or view", each at the position, with the declared type and with the nullability its
//! relation declares. Every value such a row carries is read out of the database's own
//! schema, so a relation nothing created is in no answer, and a catalog relation this
//! emulator does not serve stays the undefined relation it is (SQLSTATE 42P01) rather than
//! being answered with an invented row.
//!
//! The suite lives beside the router because the Data API's preconditions are other
//! services' facts: Amazon RDS holds the cluster whose `resourceArn` a call names and
//! whether its HTTP endpoint is on, and Secrets Manager holds the credentials the call
//! authenticates with. A crate-local suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: which catalog relation is read,
//! whether the subject is a table or a view, whether the relation is named bare, qualified
//! by `public` or qualified in a column reference, which KIND of statement names it (a
//! read, an insert, an update, a delete), whether the answer is a row or no row at all, and
//! a catalog relation that is not served.
//!
//! Sources: `smithy_query(query="operation", operation="ExecuteStatement")` on the rdsdata
//! model for the call's own members; PostgreSQL's "The Information Schema" for the two
//! relations and their columns.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The database the cluster is created with and every statement below runs against, which
/// is the catalog the relations describing it report.
const DATABASE: &str = "appdb";

/// The master user the cluster is created with.
const MASTER_USER: &str = "appadmin";

/// How often a poll looks again, and how long it keeps looking: what the polls below wait
/// for is owed by the CLOCK, so each waits for the condition under a generous deadline
/// rather than for a fixed span.
const LOOK: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(60);

/// The SQLSTATE PostgreSQL refuses a statement naming a relation the database does not hold
/// with ("undefined_table", its "Error Codes" appendix).
const SQLSTATE_UNDEFINED_TABLE: &str = "42P01";

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

/// The cluster a Data API caller addresses: its ARN and the ARN of the secret holding the
/// credentials a call authenticates with.
struct Cluster {
    arn: String,
    secret_arn: String,
}

impl Cluster {
    /// Create one Aurora PostgreSQL cluster with the Data API turned on and RDS managing the
    /// master user password, waiting until the secret it minted can be used — which is the
    /// credential every statement below authenticates with.
    async fn create(router: &MultiServiceRouter, identifier: &str) -> Self {
        let (status, created) = rds(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", "aurora-postgresql"),
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

    /// Run `sql` and answer the parsed response, which must be served.
    async fn run(&self, router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
        let (status, answered) = self.statement(router, sql).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {sql} must be served, got {status} with {answered}"
        );
        answered
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
}

/// The records of a served answer, each as the list of its column values rendered as text:
/// a string value as itself, a number as its digits, a null as `""`.
fn rows(answered: &Value) -> Vec<Vec<String>> {
    answered
        .get("records")
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .map(|record| {
                    record
                        .as_array()
                        .map(|fields| fields.iter().map(field_text).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One `Field` of a record as text, whichever member of that union carries it.
fn field_text(field: &Value) -> String {
    for member in ["stringValue", "longValue", "doubleValue", "booleanValue"] {
        if let Some(value) = field.get(member) {
            return match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
        }
    }
    String::new()
}

/// The first column of every record, in the order the answer reports them.
fn first_column(answered: &Value) -> Vec<String> {
    rows(answered)
        .into_iter()
        .map(|record| record.into_iter().next().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn the_catalog_relations_report_the_schema_the_database_holds() {
    let router = build_router();
    let cluster = Cluster::create(&router, "emu-xsvc-catalog").await;

    // The schema every row below is read out of, created through the Data API itself: two
    // tables whose columns differ in type and in nullability, and a view over one of them.
    for (case, ddl) in [
        (
            "the widgets table",
            "CREATE TABLE widgets (id serial PRIMARY KEY, name text NOT NULL, price numeric)",
        ),
        ("the gadgets table", "CREATE TABLE gadgets (code text)"),
        (
            "a view over widgets",
            "CREATE VIEW widget_names AS SELECT name FROM widgets",
        ),
    ] {
        cluster.run(&router, case, ddl).await;
    }

    /// One row: a statement that reads the catalog, and the first column of every record it
    /// must answer with.
    struct Row {
        case: &'static str,
        sql: &'static str,
        expected: &'static [&'static str],
    }

    let catalog_rows: &[Row] = &[
        Row {
            case: "every relation of the database, in name order",
            sql: "SELECT table_name FROM information_schema.tables \
                  WHERE table_schema = 'public' ORDER BY table_name",
            expected: &["gadgets", "widget_names", "widgets"],
        },
        Row {
            case: "a table reports itself as a base table",
            sql: "SELECT table_type FROM information_schema.tables \
                  WHERE table_name = 'widgets'",
            expected: &["BASE TABLE"],
        },
        Row {
            case: "a view reports itself as a view",
            sql: "SELECT table_type FROM information_schema.tables \
                  WHERE table_name = 'widget_names'",
            expected: &["VIEW"],
        },
        Row {
            case: "a relation nothing created is in no answer",
            sql: "SELECT table_name FROM information_schema.tables \
                  WHERE table_name = 'sprockets'",
            expected: &[],
        },
        Row {
            case: "the relation reports the database it belongs to",
            sql: "SELECT table_catalog FROM information_schema.tables \
                  WHERE table_name = 'gadgets'",
            expected: &[DATABASE],
        },
        Row {
            case: "a count over the catalog",
            sql: "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
            expected: &["3"],
        },
        Row {
            case: "the relation is addressable by its own name in a column reference",
            sql: "SELECT tables.table_name FROM information_schema.tables \
                  WHERE tables.table_name = 'widgets'",
            expected: &["widgets"],
        },
        Row {
            case: "the relation takes an alias",
            sql: "SELECT held.table_name FROM information_schema.tables AS held \
                  WHERE held.table_name = 'widgets'",
            expected: &["widgets"],
        },
        Row {
            case: "the columns a table declares, in declaration order",
            sql: "SELECT column_name FROM information_schema.columns \
                  WHERE table_name = 'widgets' ORDER BY ordinal_position",
            expected: &["id", "name", "price"],
        },
        Row {
            case: "the type each column was declared with",
            sql: "SELECT data_type FROM information_schema.columns \
                  WHERE table_name = 'gadgets' ORDER BY ordinal_position",
            expected: &["text"],
        },
        Row {
            case: "a column declared NOT NULL admits no null",
            sql: "SELECT is_nullable FROM information_schema.columns \
                  WHERE table_name = 'widgets' AND column_name = 'name'",
            expected: &["NO"],
        },
        Row {
            case: "a column declared without NOT NULL admits one",
            sql: "SELECT is_nullable FROM information_schema.columns \
                  WHERE table_name = 'widgets' AND column_name = 'price'",
            expected: &["YES"],
        },
        Row {
            case: "a column of a relation nothing created is in no answer",
            sql: "SELECT column_name FROM information_schema.columns \
                  WHERE table_name = 'sprockets'",
            expected: &[],
        },
    ];

    for row in catalog_rows {
        let answered = cluster.run(&router, row.case, row.sql).await;
        let reported = first_column(&answered);
        assert_eq!(
            reported, row.expected,
            "case {:?}: expected {:?}, got {reported:?} from {}",
            row.case, row.expected, row.sql
        );
    }

    // The schema the database's own objects are in selects exactly what the bare name does,
    // in every position a statement names a relation in: what it reads from, what it writes
    // to, and a column reference qualified by both.
    cluster
        .run(
            &router,
            "a row to read back",
            "INSERT INTO widgets (name, price) VALUES ('bolt', 2)",
        )
        .await;
    for (case, sql) in [
        ("the bare name", "SELECT name FROM widgets"),
        ("the public schema", "SELECT name FROM public.widgets"),
        (
            "the public schema in a column reference",
            "SELECT public.widgets.name FROM public.widgets",
        ),
        (
            "the public schema in a subquery",
            "SELECT name FROM widgets WHERE id IN (SELECT id FROM public.widgets)",
        ),
    ] {
        let answered = cluster.run(&router, case, sql).await;
        let reported = first_column(&answered);
        assert_eq!(
            reported,
            vec!["bolt".to_string()],
            "case {case:?}: expected the stored row, got {reported:?} from {sql}"
        );
    }
    // The statements that ADDRESS a relation rather than read from one: each is served
    // under the qualification, and the row it left is read back through the bare name.
    for (case, sql, expected) in [
        (
            "an insert into the public schema",
            "INSERT INTO public.widgets (name, price) VALUES ('nut', 1)",
            vec!["bolt".to_string(), "nut".to_string()],
        ),
        (
            "an update of the public schema",
            "UPDATE public.widgets SET name = 'washer' WHERE name = 'nut'",
            vec!["bolt".to_string(), "washer".to_string()],
        ),
        (
            "a delete from the public schema",
            "DELETE FROM public.widgets WHERE name = 'washer'",
            vec!["bolt".to_string()],
        ),
    ] {
        cluster.run(&router, case, sql).await;
        let answered = cluster
            .run(&router, case, "SELECT name FROM widgets ORDER BY name")
            .await;
        let reported = first_column(&answered);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the rows {expected:?} after {sql}, got {reported:?}"
        );
    }

    // A catalog relation this emulator does not serve is the relation the database holds
    // none of, reported with PostgreSQL's own code for exactly that — never an invented row.
    let (status, refused) = cluster
        .statement(
            &router,
            "SELECT constraint_name FROM information_schema.table_constraints",
        )
        .await;
    assert_eq!(
        status, 400,
        "a relation this emulator does not serve must be refused, got {status} with {refused}"
    );
    let message = refused
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("information_schema.table_constraints")
            && message.contains(SQLSTATE_UNDEFINED_TABLE),
        "expected the undefined-relation diagnostic naming the relation and \
         {SQLSTATE_UNDEFINED_TABLE}, got {message:?}"
    );
}
