// SPDX-License-Identifier: CC0-1.0

//! A client that walks a SEQUENCE of statements, polling each one to a verdict before it
//! submits the next, driven over the same router the emulator boots.
//!
//! THE RULE UNDER TEST: a query execution is a unit of WORK, so the state a read reports
//! moves to a terminal one as soon as that work is over. The span between the submission
//! and the completion a read reports is therefore a FLOOR this service owes — long enough
//! that a caller reading straight back still observes a transient state and the refusals
//! that go with it ("QUEUED indicates that the query has been submitted to the service",
//! `smithy_query(query="shape",shape="QueryExecutionState", model_path=
//! "services/service-com-amazonaws-athena-amazonathena/smithy-model/
//! com-amazonaws-athena-amazonathena.json")`) — and NOT a provisioning wait held above the
//! work: a statement whose work is a definition against the Data Catalog or a scan of a few
//! objects settles inside a second, as the service itself settles such a statement, so a
//! walk of such statements reaches a verdict for every one of them inside a budget a client
//! driving a deployment holds the whole walk to.
//!
//! The rows differ along what the rule generalises over: the KIND of statement submitted
//! (a database definition, a table definition over seeded objects, a scan of those objects,
//! a bare projection, a utility statement, and the drops that undo the definitions) and
//! therefore along how much work the statement does — each of them is walked in turn, by a
//! caller that holds nothing in flight.
//!
//! The spans asserted are ABSOLUTE — [`SETTLE_CEILING`] and [`WALK_BUDGET`] are written as
//! the spans the service answers such statements in, not in terms of the span this crate
//! owes a transient state — so a floor raised back above the work a statement does fails
//! here however that floor is spelled.
//!
//! Nothing here asserts how long the HOST took: every span asserted is read off the
//! instants the service itself reports for the execution (`SubmissionDateTime` and
//! `CompletionDateTime`), and every wait is a poll for the state the execution reaches.
//!
//! The mounting, the Athena caller, the bucket and the polls are the ones the suites beside
//! this one share (`crate::query_result_objects`, `crate::harness`): Athena and Amazon S3
//! on ONE engine, as `core/router/src/lib.rs` mounts them, because the walk spans both —
//! the objects the scan reads and the results each statement saves are the object store's,
//! while the databases and tables the statements define are the Data Catalog's, which this
//! service reaches through the shared engine.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_EXECUTION_COMPLETION, QUERY_STATE_QUEUED, QUERY_STATE_RUNNING, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{execution, settled};
use crate::query_result_objects::{athena, create_bucket, emulator, start};

/// The bucket the walk's objects and query results live in.
const BUCKET: &str = "athena-statement-walk";

/// Where the walk's statements write their results.
const OUTPUT_LOCATION: &str = "s3://athena-statement-walk/athena-results/";

/// The database the walk defines its table in.
const DATABASE: &str = "statement_walk";

/// The database every statement of the walk is submitted against: the one an unqualified
/// name resolves in, which is there before the walk defines anything.
const CONTEXT_DATABASE: &str = "default";

/// The name this service reaches the account's AWS Glue Data Catalog by.
const GLUE_CATALOG: &str = "AwsDataCatalog";

/// The prefixes of [`BUCKET`] the walk seeds, each holding its own CSV objects under a
/// nested prefix — so the table defined over the bucket's data prefix scans objects that
/// sit deeper than the location itself.
const PREFIXES: [&str; 3] = ["orders", "items", "ledger"];

/// How many CSV objects each prefix of [`PREFIXES`] holds.
const OBJECTS_PER_PREFIX: usize = 2;

/// How many DATA rows each seeded object carries, under its one header line.
const ROWS_PER_OBJECT: usize = 3;

/// The longest ONE statement of this walk may take to settle, as the execution's own
/// instants report the span between its submission and its completion.
///
/// ABSOLUTE, and deliberately not written in terms of any span this crate owes a transient
/// state: it is the span the service answers a statement of this work in — a definition
/// against the Data Catalog, or a scan of a handful of small objects, is sub-second — so a
/// completion held above the work fails here whatever that hold is spelled as. The work
/// itself is milliseconds, so the value leaves a loaded host room while staying well inside
/// the span such a statement answers in.
const SETTLE_CEILING: Duration = Duration::from_millis(1_500);

/// The longest the WHOLE walk's reported spans may add up to, which is what a client that
/// polls each statement to a verdict before submitting the next waits in all.
///
/// ABSOLUTE for the reason [`SETTLE_CEILING`] is, and generous next to the span these
/// statements add up to, so what fails here is an order of magnitude rather than a slow
/// host: the walk is what a deployment's setup, query and reset phases do, and a client
/// holds all of them together to a budget of its own.
const WALK_BUDGET: Duration = Duration::from_secs(7);

/// A SigV4 `Authorization` header for Amazon S3, which is how the router claims the write
/// below. Nothing is verified: the shared mounting turns request authentication off.
fn s3_authorization() -> String {
    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/s3/aws4_request, \
     SignedHeaders=host, Signature=not-verified"
        .to_string()
}

/// Write one object into [`BUCKET`] — `PutObject` over Amazon S3's own data plane, which the
/// shared caller beside this one does not make (it reads objects rather than writing them).
async fn put_object(router: &MultiServiceRouter, key: &str, body: Vec<u8>) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}/{key}"))
        .header("host", "s3.us-east-1.amazonaws.com")
        .header("authorization", s3_authorization())
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "writing s3://{BUCKET}/{key} must succeed, got {}",
        response.status()
    );
}

/// The bucket of the walk, holding one header line and [`ROWS_PER_OBJECT`] data rows in each
/// of its [`OBJECTS_PER_PREFIX`] objects per prefix.
async fn given_the_seeded_bucket(router: &MultiServiceRouter) {
    create_bucket(router, BUCKET).await;
    let mut row = 0;
    for prefix in PREFIXES {
        for object in 0..OBJECTS_PER_PREFIX {
            let mut body = String::from("id,label,amount\n");
            for _ in 0..ROWS_PER_OBJECT {
                row += 1;
                body.push_str(&format!("{row},row-{row},{}\n", row * 10));
            }
            put_object(
                router,
                &format!("data/{prefix}/part-{object}.csv"),
                body.into_bytes(),
            )
            .await;
        }
    }
}

/// How many data rows the seeded bucket holds in all, which is what a scan of the table
/// defined over its data prefix must count.
fn seeded_rows() -> usize {
    PREFIXES.len() * OBJECTS_PER_PREFIX * ROWS_PER_OBJECT
}

/// One statement of the walk: what it is about, the SQL submitted, and the rows a completed
/// run of it reports (`None` for a statement whose result is the engine's text output rather
/// than a result set this walk pins).
struct Step {
    /// What this step is about, named in every failure.
    case: String,
    /// The statement submitted.
    sql: String,
    /// The rows `GetQueryResults` must report, header row first, when the walk pins them.
    rows: Option<Vec<Vec<String>>>,
}

/// The walk: a definition of the database, a definition of a table over the seeded objects,
/// a scan of them, a bare projection, a utility statement, and the drops that undo the two
/// definitions — submitted in this order, each polled to a verdict before the next.
fn walk() -> Vec<Step> {
    let total = seeded_rows().to_string();
    vec![
        Step {
            case: "a database definition".to_string(),
            sql: format!("CREATE DATABASE IF NOT EXISTS {DATABASE}"),
            rows: None,
        },
        Step {
            case: "a table definition over the seeded objects".to_string(),
            sql: format!(
                "CREATE EXTERNAL TABLE IF NOT EXISTS {DATABASE}.seeded \
                 (id bigint, label string, amount bigint) \
                 ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' \
                 LOCATION 's3://{BUCKET}/data/' \
                 TBLPROPERTIES ('skip.header.line.count'='1')"
            ),
            rows: None,
        },
        Step {
            case: "a scan of every object under that location".to_string(),
            sql: format!("SELECT count(*) AS rows_read FROM {DATABASE}.seeded"),
            rows: Some(vec![vec!["rows_read".to_string()], vec![total.clone()]]),
        },
        Step {
            case: "a bare projection".to_string(),
            sql: "SELECT 'walked' AS marker".to_string(),
            rows: Some(vec![
                vec!["marker".to_string()],
                vec!["walked".to_string()],
            ]),
        },
        Step {
            case: "a utility statement".to_string(),
            sql: format!("SHOW TABLES IN {DATABASE}"),
            rows: None,
        },
        Step {
            case: "the drop of the table".to_string(),
            sql: format!("DROP TABLE {DATABASE}.seeded"),
            rows: None,
        },
        Step {
            case: "the drop of the database".to_string(),
            sql: format!("DROP DATABASE {DATABASE}"),
            rows: None,
        },
    ]
}

/// Submit `sql` and answer the `QueryExecutionId` Athena minted for it.
async fn submitted(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    start(
        router,
        case,
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": CONTEXT_DATABASE, "Catalog": GLUE_CATALOG },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await
}

/// Every row `GetQueryResults` reports for `id`, each row as the values of its `Datum`s.
async fn reported_rows(router: &MultiServiceRouter, case: &str, id: &str) -> Vec<Vec<String>> {
    let (status, answered) = athena(
        router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {answered:?}"
    );
    answered["ResultSet"]["Rows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| {
            row["Data"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect()
}

/// The span between the submission and the completion of `execution`, as the execution's own
/// instants report them.
fn reported_span(case: &str, execution: &Value) -> Duration {
    let status = &execution["Status"];
    let submitted = status["SubmissionDateTime"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: a submitted statement reports SubmissionDateTime, got {status:?}")
    });
    let completed = status["CompletionDateTime"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: a terminal statement reports CompletionDateTime, got {status:?}")
    });
    assert!(
        completed >= submitted,
        "case {case:?}: expected the completion at or after the submission, got {submitted} \
         then {completed}"
    );
    Duration::from_secs_f64(completed - submitted)
}

/// A client that walks a sequence of statements gets a verdict for every one of them: each
/// statement is transient when it is read straight back, settles SUCCEEDED, reports the rows
/// it produced, and its completion falls inside the span the service answers that work in —
/// so the walk as a whole fits a budget a client holds it to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sequence_of_statements_reaches_a_verdict_for_every_one_of_them() {
    let router = emulator();
    given_the_seeded_bucket(&router).await;

    let mut walked = Duration::ZERO;
    for step in walk() {
        let case = step.case.as_str();
        let id = submitted(&router, case, &step.sql).await;

        // Read straight back: the floor is never zero, so a caller that acts at once still
        // sees a transient state and the refusal that goes with it.
        let immediate = execution(&router, &id).await;
        let state = immediate["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            state == QUERY_STATE_QUEUED || state == QUERY_STATE_RUNNING,
            "case {case:?}: expected {QUERY_STATE_QUEUED} or {QUERY_STATE_RUNNING} from the \
             first read, got {state:?} in {immediate:?}"
        );
        let (refused_status, refused) = athena(
            &router,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            refused_status, 400,
            "case {case:?}: expected the results of an unfinished statement to be refused, \
             got {refused_status} with {refused:?}"
        );

        let found = settled(&router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {case:?}: expected {QUERY_STATE_SUCCEEDED}, got {found:?}"
        );
        let span = reported_span(case, &found);
        // The FLOOR this service owes a transient state, which the read above observed.
        assert!(
            span >= QUERY_EXECUTION_COMPLETION,
            "case {case:?}: expected a completion no sooner than \
             {QUERY_EXECUTION_COMPLETION:?} after the submission, got {span:?}"
        );
        // The CEILING the work puts on it, as an absolute span.
        assert!(
            span <= SETTLE_CEILING,
            "case {case:?}: expected a statement of this work to complete within \
             {SETTLE_CEILING:?} of its submission, got {span:?}"
        );
        walked += span;
        if let Some(expected) = step.rows {
            let rows = reported_rows(&router, case, &id).await;
            assert_eq!(
                rows, expected,
                "case {case:?}: expected the rows the statement produced, got {rows:?}"
            );
        }
    }

    assert!(
        walked <= WALK_BUDGET,
        "expected the whole walk's reported spans to add up to no more than {WALK_BUDGET:?}, \
         got {walked:?}"
    );
}
