// SPDX-License-Identifier: CC0-1.0

//! The whole chain a Data API client provisions before it can run a statement — an Aurora DB
//! cluster, its member DB instances, the admin-user secret Amazon RDS manages for it — read
//! back on the ASSEMBLED emulator, and the statement that follows it.
//!
//! RULE: a resource a create answers in a NON-TERMINAL status reaches its terminal status on
//! the CLOCK alone, at a fixed instant after the call that started it — no read advances it
//! and no read is needed to make it due — and sibling resources are owed their transitions
//! CONCURRENTLY, each from its own create. So a client that waits for the step it started
//! LAST finds every step it started earlier already terminal: a chain of independent
//! provisioning steps converges in about one such span altogether, not one span (nor one
//! client poll) per step. `DescribeDBClusters` then reports `Status` `available`,
//! `DescribeDBInstances` reports `DBInstanceStatus` `available` for each member,
//! `MasterUserSecret$SecretStatus` is `active` with the credentials retrievable from Secrets
//! Manager, and `ExecuteStatement` answers on the cluster's HTTP endpoint.
//!
//! Only a suite beside the router can pin this: the chain spans three services on one
//! engine — Amazon RDS holds the cluster and its members, Secrets Manager holds the secret
//! RDS minted through a hop dispatched back into this same router, and the Data API resolves
//! both before it runs a statement. A crate-local suite sees one leg of it at a time.
//!
//! The rows differ along what the rule generalises over: the Aurora engine family, and how
//! many independent children the cluster is provisioned with (a composite with one member
//! and one with two — more children do not cost more spans).
//!
//! The statement at the end is the one a client makes once the chain has converged, which is
//! what shows the chain really did converge: a `typeHint` of `DATE` names the type its
//! `stringValue` is sent as, so a string that is not a date of the documented `YYYY-MM-DD`
//! form is refused with the fixed parse-error sentence of that type and a date of that form
//! is served. The whole table of hints and faults lives in the owning crate
//! (`services/service-com-amazonaws-rdsdata-rdsdataservice/tests/all/sql_data_api.rs`); the
//! few rows here are the end of this chain.
//!
//! Sources: `smithy_query(query="operation", operation="CreateDBCluster")` and
//! `smithy_query(query="operation", operation="DescribeDBInstances")` on the rds model for
//! `EnableHttpEndpoint`, `ManageMasterUserPassword`, `Status`, `DBInstanceStatus` and
//! `MasterUserSecret`; `smithy_query(query="shape", shape="SqlParameter")` on the rdsdata
//! model for `typeHint` and the `DATE` form.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The database every cluster below is created with, and every statement runs against.
const DATABASE: &str = "appdb";

/// The master user every cluster below is created with, which is the user its managed secret
/// holds.
const MASTER_USER: &str = "appadmin";

/// How often a poll looks again, and how long it keeps looking: what the poll waits for is
/// owed by the CLOCK, so it waits for the CONDITION under a generous deadline rather than
/// for a fixed span.
const LOOK: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(120);

/// `DBCluster$Status` / `DBInstance$DBInstanceStatus` once the resource is usable.
const AVAILABLE: &str = "available";

/// `MasterUserSecret$SecretStatus` while the secret is being created, and once it can be
/// used.
const SECRET_CREATING: &str = "creating";
const SECRET_ACTIVE: &str = "active";

/// The error a failure to READ a parameter the request SENT as a value of the type its hint
/// named reaches the caller in, and the two faults the reader of a date reports.
const DATABASE_ERROR: &str = "DatabaseErrorException";
const DATE_INVALID_CHARACTERS: &str = "Parse Error for Date: input contains invalid characters";
const DATE_TRAILING_INPUT: &str = "Parse Error for Date: trailing input";

/// The emulator as it is ASSEMBLED, on one engine: the hop Amazon RDS makes to Secrets
/// Manager to mint the secret it manages, and the hops the Data API makes to both, are
/// dispatched back into this same router.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and names the Region its ARNs are built in.
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

/// One Secrets Manager awsJson1_1 call, answered with the HTTP status and the parsed body.
async fn secrets(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Data API `ExecuteStatement` against one cluster, answered with the HTTP status and
/// the parsed body.
async fn statement(
    router: &MultiServiceRouter,
    cluster_arn: &str,
    secret_arn: &str,
    sql: &str,
    parameters: Value,
) -> (u16, Value) {
    execute(
        router,
        &json!({
            "resourceArn": cluster_arn,
            "secretArn": secret_arn,
            "database": DATABASE,
            "sql": sql,
            "parameters": parameters,
        }),
    )
    .await
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

/// The wire error code of a refusal, without the namespace a `__type` may qualify it with.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A string member of an answered document, or the empty string.
fn text_of(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `DBInstanceStatus` a single `DescribeDBInstances` read reports for `identifier`.
async fn instance_status(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let (status, read) = rds(
        router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeDBInstances({identifier:?}) must succeed, got {status} with {read}"
    );
    element(&read, "DBInstanceStatus")
}

/// One row: the engine family the cluster runs and how many member DB instances it is
/// provisioned with.
struct Row {
    case: &'static str,
    engine: &'static str,
    instance_class: &'static str,
    members: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "an Aurora PostgreSQL cluster with one member",
        engine: "aurora-postgresql",
        instance_class: "db.r6g.large",
        members: 1,
    },
    Row {
        case: "an Aurora MySQL cluster with one member",
        engine: "aurora-mysql",
        instance_class: "db.t3.medium",
        members: 1,
    },
    Row {
        case: "an Aurora PostgreSQL cluster with two members",
        engine: "aurora-postgresql",
        instance_class: "db.r6g.large",
        members: 2,
    },
];

/// One `DATE`-hinted parameter: the string sent, and either the answer it is served as or
/// the sentence it is refused with.
///
/// `DATE` is `YYYY-MM-DD` (`smithy_query(query="shape", shape="SqlParameter")`, member
/// `typeHint`), so the first row is a date of that form and the rest are strings that are
/// not one: another calendar's order, a day the calendar does not have, a month it does not
/// have, and a date of the form with a time left over after it.
const DATE_CASES: &[(&str, Option<&str>)] = &[
    ("2026-10-22", None),
    ("22/10/2026", Some(DATE_INVALID_CHARACTERS)),
    ("2026-02-30", Some(DATE_INVALID_CHARACTERS)),
    ("2026-13-01", Some(DATE_INVALID_CHARACTERS)),
    ("2026-10-22T00:00", Some(DATE_TRAILING_INPUT)),
];

#[tokio::test]
async fn the_aurora_data_api_provisioning_chain_converges_without_a_poll_per_step() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let cluster_id = format!("emu-chain-cluster-{index}");

        // 1. The cluster is created, and the create answers a NON-TERMINAL status: the span
        //    the clock owes is never zero, so a client really does have to wait for it.
        let (status, created) = rds(
            &router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", &cluster_id),
                ("Engine", row.engine),
                ("MasterUsername", MASTER_USER),
                ("ManageMasterUserPassword", "true"),
                ("DatabaseName", DATABASE),
                ("EnableHttpEndpoint", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateDBCluster({cluster_id:?}) must be served, got {status} with \
             {created}"
        );
        let cluster_arn = element(&created, "DBClusterArn");
        let reported = element(&created, "Status");
        assert_ne!(
            reported, AVAILABLE,
            "case {case:?}: expected the create of {cluster_id:?} to answer a status the clock \
             still owes, got {reported:?}"
        );
        let minted = block_of(&created, "MasterUserSecret").unwrap_or_else(|| {
            panic!("case {case:?}: a managed-password cluster reports its secret: {created}")
        });
        let secret_arn = element(minted, "SecretArn");
        assert_eq!(
            opt_element(minted, "SecretStatus").as_deref(),
            Some(SECRET_CREATING),
            "case {case:?}: expected the minted secret of {cluster_id:?} to answer \
             {SECRET_CREATING:?}, got {created}"
        );

        // 2. Its members are created, each answering a non-terminal status of its own. They
        //    are started one after another and NOT waited for in between: their transitions
        //    are owed concurrently.
        let member_ids: Vec<String> = (0..row.members)
            .map(|member| format!("emu-chain-member-{index}-{member}"))
            .collect();
        for member_id in &member_ids {
            let (status, created) = rds(
                &router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", member_id),
                    ("DBClusterIdentifier", &cluster_id),
                    ("Engine", row.engine),
                    ("DBInstanceClass", row.instance_class),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateDBInstance({member_id:?}) must be served, got {status} with \
                 {created}"
            );
            let reported = element(&created, "DBInstanceStatus");
            assert_ne!(
                reported, AVAILABLE,
                "case {case:?}: expected the create of {member_id:?} to answer a status the clock \
                 still owes, got {reported:?}"
            );
        }

        // 3. The client waits for the step it started LAST — the member created last — and
        //    for nothing else.
        let last_member = member_ids
            .last()
            .expect("every row provisions at least one member");
        let deadline = Instant::now() + DEADLINE;
        loop {
            let reported = instance_status(&router, case, last_member).await;
            if reported == AVAILABLE {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {last_member:?} must reach {AVAILABLE:?} on its own, last \
                 reported {reported:?}"
            );
            tokio::time::sleep(LOOK).await;
        }

        // 4. And every step it started EARLIER is already terminal, each read ONCE and
        //    polled for no further: no step of the chain costs a poll of its own.
        for member_id in member_ids.iter().rev().skip(1) {
            let reported = instance_status(&router, case, member_id).await;
            assert_eq!(
                reported, AVAILABLE,
                "case {case:?}: expected {member_id:?}, started before {last_member:?}, to be \
                 {AVAILABLE:?} already, got {reported:?}"
            );
        }
        let (status, read) = rds(
            &router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", &cluster_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeDBClusters({cluster_id:?}) must succeed, got {status} with \
             {read}"
        );
        let reported = element(&read, "Status");
        assert_eq!(
            reported, AVAILABLE,
            "case {case:?}: expected {cluster_id:?}, started before its members, to be \
             {AVAILABLE:?} already, got {reported:?} in {read}"
        );
        let reported = block_of(&read, "MasterUserSecret")
            .and_then(|secret| opt_element(secret, "SecretStatus"))
            .unwrap_or_default();
        assert_eq!(
            reported, SECRET_ACTIVE,
            "case {case:?}: expected the managed secret of {cluster_id:?} to be {SECRET_ACTIVE:?} \
             already, got {reported:?} in {read}"
        );

        // The credentials that secret holds are retrievable from Secrets Manager, which is
        // what the statement below authenticates with: the status it reports is backed by a
        // secret the owning service really stored.
        let (status, held) =
            secrets(&router, "GetSecretValue", &json!({ "SecretId": secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSecretValue({secret_arn:?}) must be served, got {status} with \
             {held}"
        );
        let document: Value =
            serde_json::from_str(&text_of(&held, "SecretString")).unwrap_or_else(|_| {
                panic!("case {case:?}: the managed secret holds a credentials document: {held}")
            });
        assert_eq!(
            text_of(&document, "username"),
            MASTER_USER,
            "case {case:?}: expected the secret to hold the master user the create named, got \
             {document}"
        );
        assert!(
            !text_of(&document, "password").is_empty(),
            "case {case:?}: expected the secret to hold a generated password, got {document}"
        );

        // 5. The Data API endpoint of that cluster answers, which is the end of the chain.
        let (status, answered) = statement(
            &router,
            &cluster_arn,
            &secret_arn,
            "SELECT 1",
            json!([]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a statement on the converged cluster must be served, got {status} \
             with {answered}"
        );

        // And the `DATE` hint decides what the statement's string parameter may be.
        for (sent, refusal) in DATE_CASES {
            let (status, answered) = statement(
                &router,
                &cluster_arn,
                &secret_arn,
                "SELECT :d",
                json!([{ "name": "d", "value": { "stringValue": sent }, "typeHint": "DATE" }]),
            )
            .await;
            match refusal {
                None => {
                    assert_eq!(
                        status, 200,
                        "case {case:?} with DATE {sent:?}: a date of the documented form must be \
                         served, got {status} with {answered}"
                    );
                    assert_eq!(
                        answered["records"],
                        json!([[{ "stringValue": sent }]]),
                        "case {case:?} with DATE {sent:?}: expected the date to be selected back \
                         as it was sent, got {answered}"
                    );
                }
                Some(sentence) => {
                    assert_eq!(
                        status, 400,
                        "case {case:?} with DATE {sent:?}: a string that is no date must be \
                         refused, got {status} with {answered}"
                    );
                    assert_eq!(
                        error_code(&answered),
                        DATABASE_ERROR,
                        "case {case:?} with DATE {sent:?}: expected {DATABASE_ERROR}, got \
                         {answered}"
                    );
                    assert_eq!(
                        text_of(&answered, "message"),
                        *sentence,
                        "case {case:?} with DATE {sent:?}: expected the fixed parse-error \
                         sentence, got {answered}"
                    );
                }
            }
        }
    }
}
