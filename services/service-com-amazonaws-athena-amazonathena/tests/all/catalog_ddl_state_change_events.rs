// SPDX-License-Identifier: CC0-1.0

//! A statement whose whole effect is on the DATA CATALOG, submitted through
//! `StartQueryExecution` and followed all the way to the `Athena Query State Change` event
//! its transitions are published as.
//!
//! THE RULE under test, stated over the class and not over one statement: Athena performs a
//! statement whose whole effect is on the data catalog AGAINST that catalog — not on the
//! engine that runs statements over data — so such a statement settles `SUCCEEDED`, reports
//! the kind of statement it is, and carries neither `Status.AthenaError` nor
//! `Status.StateChangeReason`; where the catalog's own metastore refuses it the statement
//! settles `FAILED` carrying THAT refusal's classification and wording, never a diagnostic of
//! the engine the emulator runs data queries on. The `Athena Query State Change` event
//! published for each state such a query moved into carries `athenaError` only where the state
//! it reached is `FAILED`, and what it carries there is the error the execution itself
//! reports.
//!
//! The rows span what the rule generalises over: which catalog statement is submitted (a
//! database definition, a drop, a listing), the grammar and spelling it is written in
//! (`DATABASE` against `SCHEMA`), the quoting of the name (bare, and back-quoted carrying a
//! character a bare name cannot), which clauses it carries (none, and the clause sequence only
//! Hive's DDL grammar reads), and the terminal state it settles in together with the class of
//! refusal that settled it. The recorded case — one back-quoted `CREATE DATABASE` polled to
//! `SUCCEEDED` while a rule delivers its events — is one row among them.
//!
//! Nothing here is evidence of anything but the real catalog and the real publication: the
//! database each definition leaves is read back through AWS Glue's own `GetDatabase`, the queue
//! and the rule are Amazon SQS's and EventBridge's own resources created through their own
//! APIs on one shared engine, and the `athenaError` an envelope carries is compared against the
//! `AthenaError` the same execution's `GetQueryExecution` reports, so neither side can be a
//! literal this suite carries.
//!
//! Every wait waits for the CONDITION — the query reporting a terminal state, the envelope
//! being on the queue — under a deadline far above the span a completion is due after, so a
//! loaded host cannot turn a slow run into a failure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The name Athena reaches the account's AWS Glue Data Catalog by.
const GLUE_CATALOG: &str = "AwsDataCatalog";

/// The database a statement that qualifies no name runs in.
const CONTEXT_DATABASE: &str = "default";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where these statements write their results, which every submission has to name.
const OUTPUT_LOCATION: &str = "s3://athena-ddl-events/results/";

/// The `source` Athena publishes under, spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make a test pass.
const ATHENA_EVENT_SOURCE: &str = "aws.athena";

/// The `detail-type` a query execution's state change is published under; see
/// [`ATHENA_EVENT_SOURCE`].
const QUERY_STATE_CHANGE: &str = "Athena Query State Change";

/// The `QueryExecutionState` a submitted query waits for resources in.
const QUEUED: &str = "QUEUED";

/// The `QueryExecutionState` of a query in its execution phase.
const RUNNING: &str = "RUNNING";

/// The `QueryExecutionState` of a query that completed without errors.
const SUCCEEDED: &str = "SUCCEEDED";

/// The `QueryExecutionState` of a query that experienced an error and did not complete.
const FAILED: &str = "FAILED";

/// The workgroup a submission that names none runs in, which the event reports as its
/// `workgroupName`.
const PRIMARY_WORK_GROUP: &str = "primary";

/// The `AthenaError$ErrorCategory` of a failure the submitted statement caused: the
/// documented list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// The `AthenaError$ErrorType` Athena publishes for a DDL task the catalog's metastore
/// refused.
const ERROR_TYPE_DDL_TASK: i64 = 1006;

/// The `AthenaError$ErrorType` Athena publishes for a name the catalog does not hold.
const ERROR_TYPE_ENTITY_NOT_FOUND: i64 = 1301;

/// What the Hive DDL engine writes in front of the metastore exception a failed task
/// wrapped.
const DDL_TASK_FAILED: &str =
    "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask.";

/// The shape a diagnostic of the SQL engine this emulator runs DATA queries on takes. No
/// failure of a catalog statement may read like one: the catalog is what refuses such a
/// statement, and it says so in its own words.
const ENGINE_DIAGNOSTIC: &str = "syntax error";

/// How long a poll keeps asking before it gives up.
///
/// Generous: what a case asserts is the state a query reaches and the envelopes that report
/// it, never how fast the host gets there, so the deadline is only here to fail a wait that
/// will never end.
const BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between asks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many `ReceiveMessage` rounds ONE drain makes: a receive answers at most ten messages,
/// and this is well above the rounds a whole table's events need.
const MAX_RECEIVE_ROUNDS: usize = 32;

/// How many events one query execution's own transitions produce: the state it was submitted
/// in, the state it runs in, and the terminal state it settles in.
const EVENTS_PER_QUERY: usize = 3;

/// One emulator carrying Athena, AWS Glue, EventBridge and Amazon SQS over ONE shared engine:
/// the catalog a statement is performed against, the bus its events are published onto and the
/// queue a rule delivers them to are those services' own.
struct Pairing {
    router: MultiServiceRouter,
}

impl Pairing {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        // Athena verifies the resolved output location before it accepts a submission, so
        // the bucket this suite's location names is held before its first query runs.
        crate::results_bucket::hold_results_bucket(&engine, OUTPUT_LOCATION);
        router.register_service(
            service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One `awsJson` RPC call against the service whose signing name is `signing_name`.
    ///
    /// Neither the key id nor the signature is a credential: this harness turns request
    /// authentication off, so nothing is verified.
    async fn call(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, payload)
    }

    /// One Athena call that must answer 200.
    async fn athena(&self, operation: &str, body: Value) -> Value {
        let (status, payload) = self
            .call("athena", "AmazonAthena", "1.1", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation} must answer 200, got {status} with {payload}"
        );
        payload
    }

    /// One AWS Glue call, and its HTTP status and parsed body.
    async fn glue(&self, operation: &str, body: Value) -> (u16, Value) {
        self.call("glue", "AWSGlue", "1.1", operation, body).await
    }

    /// One EventBridge call that must answer 200.
    async fn events(&self, operation: &str, body: Value) -> Value {
        let (status, payload) = self
            .call("events", "AWSEvents", "1.1", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation} must answer 200, got {status} with {payload}"
        );
        payload
    }

    /// One Amazon SQS call that must answer 200.
    async fn sqs(&self, operation: &str, body: Value) -> Value {
        let (status, payload) = self.call("sqs", "AmazonSQS", "1.0", operation, body).await;
        assert_eq!(
            status, 200,
            "{operation} must answer 200, got {status} with {payload}"
        );
        payload
    }

    /// A queue called `name`, created through Amazon SQS itself and carrying the grant a
    /// rule's target needs — EventBridge makes the target's `sqs:SendMessage` as its own
    /// service principal and relies on the queue's resource-based policy — answering its url
    /// and the ARN a rule names it by.
    async fn queue(&self, name: &str) -> (String, String) {
        let created = self.sqs("CreateQueue", json!({ "QueueName": name })).await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateQueue must answer a QueueUrl, got {created}"))
            .to_string();
        let attributes = self
            .sqs(
                "GetQueueAttributes",
                json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("GetQueueAttributes must answer a QueueArn, got {attributes}")
            })
            .to_string();
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "events.amazonaws.com" },
                "Action": "sqs:SendMessage",
                "Resource": &arn,
            }],
        })
        .to_string();
        self.sqs(
            "SetQueueAttributes",
            json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
        )
        .await;
        (url, arn)
    }

    /// A default-bus rule matching every state change Athena publishes, delivering to the
    /// queue `queue_arn`.
    async fn rule(&self, name: &str, queue_arn: &str) {
        let pattern =
            json!({ "source": [ATHENA_EVENT_SOURCE], "detail-type": [QUERY_STATE_CHANGE] });
        self.events(
            "PutRule",
            json!({ "Name": name, "State": "ENABLED", "EventPattern": pattern.to_string() }),
        )
        .await;
        self.events(
            "PutTargets",
            json!({ "Rule": name, "Targets": [{ "Id": "t1", "Arn": queue_arn }] }),
        )
        .await;
    }

    /// Start one statement and answer the `QueryExecutionId` it minted.
    async fn start(&self, case: &str, sql: &str) -> String {
        let answer = self
            .athena(
                "StartQueryExecution",
                json!({
                    "QueryString": sql,
                    "QueryExecutionContext": {
                        "Database": CONTEXT_DATABASE,
                        "Catalog": GLUE_CATALOG,
                    },
                    "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
                }),
            )
            .await;
        answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueryExecutionId, got {answer}"))
            .to_string()
    }

    /// Poll `GetQueryExecution` until `id` reports a terminal state, answering the execution
    /// as it settled.
    async fn settled(&self, case: &str, id: &str) -> Value {
        let deadline = Instant::now() + BUDGET;
        loop {
            let execution = self
                .athena("GetQueryExecution", json!({ "QueryExecutionId": id }))
                .await["QueryExecution"]
                .clone();
            let state = execution["Status"]["State"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state != QUEUED && state != RUNNING {
                return execution;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the query {id} must reach a terminal state, last saw \
                 {state:?} in {execution}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The `Database` AWS Glue holds under `name`, or `None` when it holds none.
    async fn stored_database(&self, name: &str) -> Option<Value> {
        let (status, body) = self.glue("GetDatabase", json!({ "Name": name })).await;
        (status == 200).then(|| body["Database"].clone())
    }

    /// Every envelope waiting on the queue at `url` right now, as parsed bodies.
    ///
    /// Each message is DELETED as it is read, so no message is reported twice, and the
    /// receives stop at the first round that answers none.
    async fn drain(&self, url: &str) -> Vec<Value> {
        let mut collected: Vec<Value> = Vec::new();
        for _ in 0..MAX_RECEIVE_ROUNDS {
            let received = self
                .sqs(
                    "ReceiveMessage",
                    json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
                )
                .await;
            let messages = received["Messages"].as_array().cloned().unwrap_or_default();
            if messages.is_empty() {
                break;
            }
            for message in messages {
                if let Some(handle) = message["ReceiptHandle"].as_str() {
                    self.sqs(
                        "DeleteMessage",
                        json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                    )
                    .await;
                }
                collected.push(
                    message["Body"]
                        .as_str()
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                        .unwrap_or(Value::Null),
                );
            }
        }
        collected
    }

    /// Drain the queue at `url` until it has answered `wanted` envelopes, looking again until
    /// it has or the deadline passes.
    ///
    /// The CONDITION is what is waited on: the envelopes are delivered as the transitions they
    /// report fall due, so a drain that finds fewer looks again rather than sleeping out a
    /// span.
    async fn drain_until(&self, case: &str, url: &str, wanted: usize) -> Vec<Value> {
        let deadline = Instant::now() + BUDGET;
        let mut collected: Vec<Value> = Vec::new();
        loop {
            collected.extend(self.drain(url).await);
            if collected.len() >= wanted {
                return collected;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {wanted} envelopes on {url}, got {}: {collected:?}",
                collected.len()
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The `detail` of one delivered envelope, having asserted the envelope's own identity.
fn detail(case: &str, envelope: &Value) -> Value {
    assert_eq!(
        envelope["source"].as_str(),
        Some(ATHENA_EVENT_SOURCE),
        "case {case:?}: expected source {ATHENA_EVENT_SOURCE}, got {envelope}"
    );
    assert_eq!(
        envelope["detail-type"].as_str(),
        Some(QUERY_STATE_CHANGE),
        "case {case:?}: expected detail-type {QUERY_STATE_CHANGE}, got {envelope}"
    );
    envelope["detail"].clone()
}

/// The details of the envelopes about the execution `id`, in the order the events were
/// published (their own `sequenceNumber`).
fn details_of(case: &str, envelopes: &[Value], id: &str) -> Vec<Value> {
    let mut details: Vec<Value> = envelopes
        .iter()
        .map(|envelope| detail(case, envelope))
        .filter(|detail| detail["queryExecutionId"] == json!(id))
        .collect();
    details.sort_by_key(|detail| {
        detail["sequenceNumber"]
            .as_str()
            .and_then(|sequence| sequence.parse::<u32>().ok())
            .unwrap_or_default()
    });
    details
}

/// What the catalog's metastore answered a refused statement with: the error type of that
/// class, the message it carries and the line the DDL task reports as the state change reason.
struct Refusal {
    error_type: i64,
    message: String,
    reason: String,
}

/// One row of the table: a catalog statement, the kind it is, the state it settles in and —
/// where the catalog refuses it — the refusal it settles with.
struct Row {
    case: &'static str,
    sql: &'static str,
    /// The `StatementType` Athena classifies the statement in.
    statement_type: &'static str,
    /// The `SubstatementType` naming the statement's own kind.
    substatement_type: &'static str,
    /// The terminal `QueryExecutionState` it reaches.
    terminal: &'static str,
    /// The refusal it settles with, for a row the catalog refuses.
    refusal: Option<Refusal>,
}

/// The database a row defines, drops or names, so the names a row writes are stated once.
const SHOP: &str = "ddl_event_shop";

/// The database created by one row and dropped by a later one.
const DEPOT: &str = "ddl_event_depot";

/// The back-quoted database of the clause-carrying row: a name holding a character a bare
/// name cannot.
const LAKE: &str = "ddl-event-lake";

/// A database no row ever creates, which is what makes a drop of it a refusal.
const ABSENT: &str = "ddl_event_absent";

/// The rows, in the order they are SUBMITTED: a statement runs as its submission is taken, so
/// a row sees the catalog every row before it left — which is what makes the definition of a
/// database already defined, and the drop of one that is there, the statements they are.
///
/// They differ along which catalog statement is submitted, the grammar and spelling it is
/// written in, the quoting of the name, the clauses it carries, and the terminal state and
/// refusal class it settles with.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bare database definition",
            sql: "CREATE DATABASE ddl_event_shop",
            statement_type: "DDL",
            substatement_type: "CREATE_DATABASE",
            terminal: SUCCEEDED,
            refusal: None,
        },
        Row {
            case: "the Presto spelling of the same catalog object",
            sql: "CREATE SCHEMA ddl_event_depot",
            statement_type: "DDL",
            substatement_type: "CREATE_DATABASE",
            terminal: SUCCEEDED,
            refusal: None,
        },
        Row {
            case: "a back-quoted name with the clauses only the Hive grammar reads",
            sql: "CREATE DATABASE IF NOT EXISTS `ddl-event-lake` COMMENT 'the lake' \
                  LOCATION 's3://athena-ddl-events/lake/' \
                  WITH DBPROPERTIES ('owner' = 'lake')",
            statement_type: "DDL",
            substatement_type: "CREATE_DATABASE",
            terminal: SUCCEEDED,
            refusal: None,
        },
        Row {
            case: "a statement that reports what the catalog holds",
            sql: "SHOW DATABASES",
            statement_type: "UTILITY",
            substatement_type: "SHOW_DATABASES",
            terminal: SUCCEEDED,
            refusal: None,
        },
        Row {
            case: "a drop of a database the catalog holds",
            sql: "DROP DATABASE ddl_event_depot",
            statement_type: "DDL",
            substatement_type: "DROP_DATABASE",
            terminal: SUCCEEDED,
            refusal: None,
        },
        Row {
            case: "a definition of a database the catalog already holds",
            sql: "CREATE DATABASE ddl_event_shop",
            statement_type: "DDL",
            substatement_type: "CREATE_DATABASE",
            terminal: FAILED,
            refusal: Some(Refusal {
                error_type: ERROR_TYPE_DDL_TASK,
                message: format!("Database {SHOP} already exists."),
                reason: format!("{DDL_TASK_FAILED} Database {SHOP} already exists"),
            }),
        },
        Row {
            case: "a drop of a database the catalog does not hold",
            sql: "DROP SCHEMA ddl_event_absent",
            statement_type: "DDL",
            substatement_type: "DROP_DATABASE",
            terminal: FAILED,
            refusal: Some(Refusal {
                error_type: ERROR_TYPE_ENTITY_NOT_FOUND,
                message: format!("Cannot drop database {ABSENT}, as it does not exist."),
                reason: format!(
                    "FAILED: SemanticException [Error 10072]: Database does not exist: {ABSENT}"
                ),
            }),
        },
    ]
}

/// THE RULE over the table: every catalog statement is performed against the catalog, so each
/// settles in the state that catalog's answer puts it in, reporting its own kind and carrying
/// an error exactly where the catalog refused it — and every state it moved into is delivered
/// to the rule matching Athena's state changes, carrying `athenaError` only on the envelope
/// whose state is FAILED.
#[tokio::test]
async fn a_catalog_statement_settles_on_the_catalogs_answer_and_publishes_it() {
    let pairing = Pairing::new();
    let (url, arn) = pairing.queue("athena-catalog-ddl-events").await;
    pairing.rule("athena-catalog-ddl-events", &arn).await;

    // Submitted in row order, all of them, so the span a completion is owed after is crossed
    // once for the whole table rather than per row.
    let table = rows();
    let mut started: Vec<String> = Vec::new();
    for row in &table {
        started.push(pairing.start(row.case, row.sql).await);
    }

    let mut settled: Vec<Value> = Vec::new();
    for (row, id) in table.iter().zip(&started) {
        let case = row.case;
        let execution = pairing.settled(case, id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(row.terminal),
            "case {case:?}: expected {} to settle {}, got {execution}",
            row.sql,
            row.terminal
        );
        assert_eq!(
            execution["StatementType"].as_str(),
            Some(row.statement_type),
            "case {case:?}: unexpected statement type, got {execution}"
        );
        assert_eq!(
            execution["SubstatementType"].as_str(),
            Some(row.substatement_type),
            "case {case:?}: unexpected substatement type, got {execution}"
        );
        match &row.refusal {
            // A statement the catalog PERFORMED reports nothing about an error: no
            // classification and no reason it reached the state it did.
            None => {
                assert!(
                    execution["Status"]["AthenaError"].is_null(),
                    "case {case:?}: a performed statement carries no AthenaError, got \
                     {execution}"
                );
                assert!(
                    execution["Status"]["StateChangeReason"].is_null(),
                    "case {case:?}: a performed statement carries no StateChangeReason, got \
                     {execution}"
                );
            }
            // A statement the catalog REFUSED carries that refusal's own classification and
            // wording — the caller is told what the catalog said, never how the engine that
            // runs data queries reads the text.
            Some(refusal) => {
                let error = execution["Status"]["AthenaError"].clone();
                assert_eq!(
                    error["ErrorCategory"].as_i64(),
                    Some(ERROR_CATEGORY_USER),
                    "case {case:?}: the submitted statement caused the failure, got {error}"
                );
                assert_eq!(
                    error["ErrorType"].as_i64(),
                    Some(refusal.error_type),
                    "case {case:?}: unexpected error type, got {error}"
                );
                assert_eq!(
                    error["Retryable"].as_bool(),
                    Some(false),
                    "case {case:?}: a refused statement is not retried, got {error}"
                );
                assert_eq!(
                    error["ErrorMessage"].as_str(),
                    Some(refusal.message.as_str()),
                    "case {case:?}: unexpected error message, got {error}"
                );
                assert_eq!(
                    execution["Status"]["StateChangeReason"].as_str(),
                    Some(refusal.reason.as_str()),
                    "case {case:?}: unexpected state change reason, got {execution}"
                );
            }
        }
        let reported = execution["Status"].to_string();
        assert!(
            !reported.contains(ENGINE_DIAGNOSTIC),
            "case {case:?}: a catalog statement is never answered with a diagnostic of the \
             engine that runs data queries, got {reported}"
        );
        settled.push(execution);
    }

    let envelopes = pairing
        .drain_until("the whole table", &url, EVENTS_PER_QUERY * table.len())
        .await;
    assert_eq!(
        envelopes.len(),
        EVENTS_PER_QUERY * table.len(),
        "expected one envelope per transition of every statement and no more, got {envelopes:?}"
    );

    for ((row, id), execution) in table.iter().zip(&started).zip(&settled) {
        let case = row.case;
        let details = details_of(case, &envelopes, id);
        let states: Vec<String> = details
            .iter()
            .map(|detail| {
                detail["currentState"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            states,
            vec![
                QUEUED.to_string(),
                RUNNING.to_string(),
                row.terminal.to_string()
            ],
            "case {case:?}: expected one envelope per state the statement moved into, got \
             {details:?}"
        );
        let reported = execution["Status"]["AthenaError"].clone();
        for detail in &details {
            let state = detail["currentState"].as_str().unwrap_or_default();
            assert_eq!(
                detail["workgroupName"].as_str(),
                Some(PRIMARY_WORK_GROUP),
                "case {case:?}: the envelope names the workgroup the statement ran in, got \
                 {detail}"
            );
            assert_eq!(
                detail["statementType"].as_str(),
                Some(row.statement_type),
                "case {case:?}: the envelope reports the kind of statement, got {detail}"
            );
            if state != FAILED {
                assert!(
                    detail["athenaError"].is_null(),
                    "case {case:?}: an envelope whose state is {state} carries no athenaError, \
                     got {detail}"
                );
                continue;
            }
            let carried = detail["athenaError"].clone();
            assert_eq!(
                carried["errorCategory"].as_i64(),
                reported["ErrorCategory"].as_i64(),
                "case {case:?}: the envelope reports the category the execution reports, got \
                 {carried} against {reported}"
            );
            assert_eq!(
                carried["errorType"].as_i64(),
                reported["ErrorType"].as_i64(),
                "case {case:?}: the envelope reports the error type the execution reports, got \
                 {carried} against {reported}"
            );
            assert_eq!(
                carried["errorMessage"].as_str(),
                reported["ErrorMessage"].as_str(),
                "case {case:?}: the envelope reports the message the execution reports, got \
                 {carried} against {reported}"
            );
        }
    }

    // What the statements LEFT, read back through the catalog's own API: a definition that
    // succeeded leaves a database Glue reports with the clauses it declared, a drop that
    // succeeded leaves none, a refused drop leaves the catalog without the name it could not
    // find, and a refused definition leaves the database that was already there.
    let shop = pairing
        .stored_database(SHOP)
        .await
        .unwrap_or_else(|| panic!("the catalog must hold {SHOP} after its definition"));
    assert_eq!(
        shop["Name"].as_str(),
        Some(SHOP),
        "unexpected stored database, got {shop}"
    );
    let lake = pairing
        .stored_database(LAKE)
        .await
        .unwrap_or_else(|| panic!("the catalog must hold {LAKE} after its definition"));
    assert_eq!(
        lake["Description"].as_str(),
        Some("the lake"),
        "the description the definition declared must be the catalog's, got {lake}"
    );
    assert_eq!(
        lake["Parameters"]["owner"].as_str(),
        Some("lake"),
        "the properties the definition declared must be the catalog's, got {lake}"
    );
    assert!(
        pairing.stored_database(DEPOT).await.is_none(),
        "the dropped database must be gone from the catalog"
    );
    assert!(
        pairing.stored_database(ABSENT).await.is_none(),
        "a refused drop defines nothing: the catalog must still hold no {ABSENT}"
    );
}
