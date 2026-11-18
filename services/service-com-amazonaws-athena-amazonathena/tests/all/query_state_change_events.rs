// SPDX-License-Identifier: CC0-1.0

//! The `Athena Query State Change` event every state a query execution moves into is
//! published as, driven end to end over the wire.
//!
//! THE RULE under test, stated over the class and not over one query: Athena is an event
//! SOURCE as well as a control plane, so every state a query execution moves into is
//! published onto the account's default EventBridge bus as `source` `aws.athena` /
//! `detail-type` `Athena Query State Change`, carrying in the event's `detail` the state the
//! query reached, the state it came from, the execution, the workgroup, the kind of statement
//! and which event of that query's sequence it is — and the `athenaError` that query reports
//! ONLY where the state it reached is `FAILED`. EventBridge then routes it: a rule whose
//! `EventPattern` matches delivers the envelope to that rule's SQS target, and a rule
//! patterned on another state receives nothing.
//!
//! The rows span what the rule generalises over: the terminal state a query settles in
//! (`SUCCEEDED`, `FAILED`), the kind of statement submitted (one that queries data, one that
//! defines something, one that asks the engine about a statement) and what made a query fail
//! (a relation nothing holds, a column a relation does not have). The recorded case — one
//! query driven to `SUCCEEDED` and one driven to `FAILED`, read off an SQS target — is two
//! rows among them.
//!
//! Nothing here is evidence of anything but the real publication: the queue and the rule are
//! Amazon SQS's and EventBridge's own resources, created through their own APIs on one engine
//! beside Athena, and every envelope is read back off the queue with `ReceiveMessage`. The
//! `athenaError` an envelope carries is compared against the `AthenaError` the same
//! execution's own `GetQueryExecution` reports, so neither side can be a literal this suite
//! carries.
//!
//! One suite asserts the publication happens with NO further Athena call at all: a client
//! that keys off these events polls nothing, so the envelopes have to arrive while nobody
//! asks. Every wait here waits for the CONDITION — the envelope being on the queue, the
//! query reporting a terminal state — under a deadline far above the span a completion is
//! due after, so a loaded host cannot turn a slow run into a failure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The `source` Athena publishes under, spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make a test pass.
const ATHENA_EVENT_SOURCE: &str = "aws.athena";

/// The `detail-type` a query execution's state change is published under; see
/// [`ATHENA_EVENT_SOURCE`].
const QUERY_STATE_CHANGE: &str = "Athena Query State Change";

/// The `QueryExecutionState` a submitted query waits for resources in — the state every
/// query starts in, and so the first state published for one.
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

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where these queries write their results, which every submission has to name.
const OUTPUT_LOCATION: &str = "s3://athena-results/query-events/";

/// How long a poll keeps asking before it gives up.
///
/// Generous: what a case asserts is the state a query reaches and the envelope that reports
/// it, never how fast the host gets there, so the deadline is only here to fail a wait that
/// will never end.
const BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between asks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many `ReceiveMessage` rounds ONE drain makes: a receive answers at most ten messages,
/// and this is well above the rounds a whole table's events need.
const MAX_RECEIVE_ROUNDS: usize = 16;

/// How many events one query execution's own transitions produce: the state it was submitted
/// in, the state it runs in, and the terminal state it settles in.
const EVENTS_PER_QUERY: usize = 3;

/// One emulator carrying Athena, EventBridge and Amazon SQS over one shared engine: the bus a
/// query's events are published onto and the queue a rule delivers them to are those
/// services' own.
struct Pairing {
    router: MultiServiceRouter,
}

impl Pairing {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One `awsJson` RPC call against the service whose signing name is `signing_name`.
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
                    "AWS4-HMAC-SHA256 Credential=AKIDQUERYEVENTS/20261110/{REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=00"
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
            .unwrap_or_else(|| panic!("GetQueueAttributes must answer a QueueArn, got {attributes}"))
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

    /// A default-bus rule matching `pattern`, delivering to the queue `queue_arn`.
    async fn rule(&self, name: &str, pattern: Value, queue_arn: &str) {
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

    /// Start one query and answer the `QueryExecutionId` it minted.
    async fn start(&self, case: &str, sql: &str) -> String {
        let answer = self
            .athena(
                "StartQueryExecution",
                json!({
                    "QueryString": sql,
                    "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
                }),
            )
            .await;
        answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueryExecutionId, got {answer}"))
            .to_string()
    }

    /// The stored `QueryExecution` of `id`.
    async fn execution(&self, id: &str) -> Value {
        self.athena("GetQueryExecution", json!({ "QueryExecutionId": id })).await
            ["QueryExecution"]
            .clone()
    }

    /// Poll `GetQueryExecution` until `id` reports a terminal state, answering the execution
    /// as it settled.
    async fn settled(&self, case: &str, id: &str) -> Value {
        let deadline = Instant::now() + BUDGET;
        loop {
            let execution = self.execution(id).await;
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

    /// Every envelope waiting on the queue at `url` right now, as parsed bodies.
    ///
    /// Each message is DELETED as it is read, so no message is reported twice, and the
    /// receives stop at the first one that answers none.
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
    /// The CONDITION is what is waited on: the envelopes are delivered as the transitions
    /// they report fall due, so a drain that finds fewer looks again rather than sleeping out
    /// a span.
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

/// One row of the table: a statement, the kind it parses as and the terminal state it
/// settles in.
struct Row {
    case: &'static str,
    sql: &'static str,
    /// The `StatementType` the submitted SQL parses as, which the event reports as its
    /// `statementType`.
    statement_type: &'static str,
    /// The terminal `QueryExecutionState` the query reaches.
    terminal: &'static str,
}

/// The rows: a query differs from the next along the state it settles in, the kind of
/// statement it carries and what made it fail.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a projection the engine computes",
            sql: "SELECT 1 AS one",
            statement_type: "DML",
            terminal: SUCCEEDED,
        },
        Row {
            case: "a projection over several rows",
            sql: "SELECT n FROM (SELECT 2 AS n UNION ALL SELECT 1) ORDER BY n",
            statement_type: "DML",
            terminal: SUCCEEDED,
        },
        Row {
            case: "a definition statement",
            sql: "CREATE TABLE query_events_ddl (id integer, name varchar)",
            statement_type: "DDL",
            terminal: SUCCEEDED,
        },
        Row {
            case: "a statement that asks the engine about a statement",
            sql: "EXPLAIN SELECT 1",
            statement_type: "UTILITY",
            terminal: SUCCEEDED,
        },
        Row {
            case: "a relation no catalog holds",
            sql: "SELECT * FROM not_a_table",
            statement_type: "DML",
            terminal: FAILED,
        },
        Row {
            case: "a column the relation does not have",
            sql: "SELECT missing FROM (SELECT 1 AS n)",
            statement_type: "DML",
            terminal: FAILED,
        },
    ]
}

/// THE RULE over the table: every state each query moved into is delivered to the rule
/// matching Athena's state changes, as one envelope per transition, naming the execution, the
/// workgroup, the kind of statement and the state it came from — and carrying the error the
/// execution itself reports exactly where the state it reached is FAILED.
#[tokio::test]
async fn every_state_a_query_reaches_is_published_and_only_a_failure_carries_the_error() {
    let pairing = Pairing::new();
    let (url, arn) = pairing.queue("athena-query-state-changes").await;
    pairing
        .rule(
            "athena-query-state-changes",
            json!({ "source": [ATHENA_EVENT_SOURCE], "detail-type": [QUERY_STATE_CHANGE] }),
            &arn,
        )
        .await;

    // Started first, all of them, so the span a completion is owed after is crossed once for
    // the whole table rather than per row.
    let table = rows();
    let mut started: Vec<String> = Vec::new();
    for row in &table {
        started.push(pairing.start(row.case, row.sql).await);
    }

    let mut settled: Vec<Value> = Vec::new();
    for (row, id) in table.iter().zip(&started) {
        let execution = pairing.settled(row.case, id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(row.terminal),
            "case {:?}: expected the query to settle {}, got {execution}",
            row.case,
            row.terminal
        );
        settled.push(execution);
    }

    let envelopes = pairing
        .drain_until(
            "the whole table",
            &url,
            EVENTS_PER_QUERY * table.len(),
        )
        .await;
    assert_eq!(
        envelopes.len(),
        EVENTS_PER_QUERY * table.len(),
        "expected one envelope per transition of every query and no more, got {envelopes:?}"
    );

    for ((row, id), execution) in table.iter().zip(&started).zip(&settled) {
        let case = row.case;
        let details = details_of(case, &envelopes, id);
        // The states a query passes through, in order: submitted, running, settled.
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
            "case {case:?}: expected one envelope per state the query moved into, got \
             {details:?}"
        );
        // Each event says where the query came FROM, which is the state the event before it
        // reported — and for the first event the state the submission created the query in.
        let mut previous = QUEUED.to_string();
        for (position, detail) in details.iter().enumerate() {
            assert_eq!(
                detail["previousState"].as_str(),
                Some(previous.as_str()),
                "case {case:?}: envelope {position} must report {previous} as the previous \
                 state, got {detail}"
            );
            assert_eq!(
                detail["sequenceNumber"].as_str(),
                Some((position + 1).to_string().as_str()),
                "case {case:?}: envelope {position} must carry sequence number {}, got {detail}",
                position + 1
            );
            assert_eq!(
                detail["queryExecutionId"].as_str(),
                Some(id.as_str()),
                "case {case:?}: the envelope names the execution, got {detail}"
            );
            assert_eq!(
                detail["workgroupName"].as_str(),
                Some(PRIMARY_WORK_GROUP),
                "case {case:?}: the envelope names the workgroup the query ran in, got {detail}"
            );
            assert_eq!(
                detail["statementType"].as_str(),
                Some(row.statement_type),
                "case {case:?}: the envelope reports the kind of statement, got {detail}"
            );
            previous = detail["currentState"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }

        // The ERROR, where there is one: only the envelope whose state is FAILED carries it,
        // and what it carries is the error the execution itself reports.
        let reported = execution["Status"]["AthenaError"].clone();
        for detail in &details {
            let state = detail["currentState"].as_str().unwrap_or_default();
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
            assert_eq!(
                carried["retryable"].as_bool(),
                Some(false),
                "case {case:?}: the FAILED state is terminal with no automatic retry, got \
                 {carried}"
            );
        }
    }
}

/// A rule patterned on ONE state receives the queries that reached that state and nothing
/// else: the failed query's envelope lands on the rule for failures, carrying its error, and
/// the succeeded query's on the rule for successes, carrying none.
#[tokio::test]
async fn a_rule_patterned_on_one_state_receives_only_the_queries_that_reached_it() {
    let pairing = Pairing::new();
    let (failed_url, failed_arn) = pairing.queue("athena-failed-queries").await;
    let (succeeded_url, succeeded_arn) = pairing.queue("athena-succeeded-queries").await;
    for (name, state, arn) in [
        ("athena-query-failed", FAILED, &failed_arn),
        ("athena-query-succeeded", SUCCEEDED, &succeeded_arn),
    ] {
        pairing
            .rule(
                name,
                json!({
                    "source": [ATHENA_EVENT_SOURCE],
                    "detail-type": [QUERY_STATE_CHANGE],
                    "detail": { "currentState": [state] },
                }),
                arn,
            )
            .await;
    }

    let failing = pairing
        .start("a query driven to FAILED", "SELECT * FROM not_a_table")
        .await;
    let succeeding = pairing
        .start("a query driven to SUCCEEDED", "SELECT 1 AS one")
        .await;
    let failed_execution = pairing.settled("a query driven to FAILED", &failing).await;
    pairing
        .settled("a query driven to SUCCEEDED", &succeeding)
        .await;

    let on_failure = pairing
        .drain_until("a query driven to FAILED", &failed_url, 1)
        .await;
    let on_success = pairing
        .drain_until("a query driven to SUCCEEDED", &succeeded_url, 1)
        .await;

    let failed_details = details_of("a query driven to FAILED", &on_failure, &failing);
    assert_eq!(
        failed_details.len(),
        1,
        "case {:?}: expected exactly one FAILED envelope for {failing}, got {on_failure:?}",
        "a query driven to FAILED"
    );
    let failed_detail = failed_details[0].clone();
    assert_eq!(
        failed_detail["athenaError"]["errorMessage"].as_str(),
        failed_execution["Status"]["AthenaError"]["ErrorMessage"].as_str(),
        "case {:?}: the FAILED envelope carries the error the execution reports, got \
         {failed_detail}",
        "a query driven to FAILED"
    );
    assert!(
        details_of("a query driven to FAILED", &on_failure, &succeeding).is_empty(),
        "case {:?}: the rule for failures receives nothing about a query that succeeded, got \
         {on_failure:?}",
        "a query driven to FAILED"
    );

    let succeeded_details = details_of("a query driven to SUCCEEDED", &on_success, &succeeding);
    assert_eq!(
        succeeded_details.len(),
        1,
        "case {:?}: expected exactly one SUCCEEDED envelope for {succeeding}, got \
         {on_success:?}",
        "a query driven to SUCCEEDED"
    );
    assert!(
        succeeded_details[0]["athenaError"].is_null(),
        "case {:?}: a SUCCEEDED envelope carries no athenaError, got {:?}",
        "a query driven to SUCCEEDED",
        succeeded_details[0]
    );
    assert!(
        details_of("a query driven to SUCCEEDED", &on_success, &failing).is_empty(),
        "case {:?}: the rule for successes receives nothing about a query that failed, got \
         {on_success:?}",
        "a query driven to SUCCEEDED"
    );
}

/// The events arrive with NO further Athena call: a client that keys off them polls nothing,
/// so every state the query reaches is published as it falls due rather than when somebody
/// next asks.
#[tokio::test]
async fn the_states_of_a_query_nobody_polls_are_published_as_they_fall_due() {
    let case = "a query nobody polls";
    let pairing = Pairing::new();
    let (url, arn) = pairing.queue("athena-unpolled-queries").await;
    pairing
        .rule(
            "athena-unpolled-queries",
            json!({ "source": [ATHENA_EVENT_SOURCE], "detail-type": [QUERY_STATE_CHANGE] }),
            &arn,
        )
        .await;

    let id = pairing.start(case, "SELECT 1 AS one").await;

    // Nothing is asked of Athena from here on: the only thing waited for is the queue.
    let envelopes = pairing.drain_until(case, &url, EVENTS_PER_QUERY).await;
    let details = details_of(case, &envelopes, &id);
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
            SUCCEEDED.to_string()
        ],
        "case {case:?}: every state the query reached must be published without a further \
         Athena call, got {details:?}"
    );
}

/// A transition is published ONCE however often the query is read: the states a query moved
/// through are each delivered a single time, and reading a settled query publishes nothing
/// more.
#[tokio::test]
async fn a_transition_is_published_once_however_often_the_query_is_read() {
    let case = "a settled query read again";
    let pairing = Pairing::new();
    let (url, arn) = pairing.queue("athena-queries-read-again").await;
    pairing
        .rule(
            "athena-queries-read-again",
            json!({ "source": [ATHENA_EVENT_SOURCE], "detail-type": [QUERY_STATE_CHANGE] }),
            &arn,
        )
        .await;

    let id = pairing.start(case, "SELECT 1 AS one").await;
    pairing.settled(case, &id).await;

    // Read the settled query several more times, through each operation that reports one:
    // none of them materialises a transition, so none of them publishes an event.
    for _ in 0..3 {
        pairing.execution(&id).await;
        pairing
            .athena(
                "BatchGetQueryExecution",
                json!({ "QueryExecutionIds": [&id] }),
            )
            .await;
        pairing
            .athena(
                "ListQueryExecutions",
                json!({ "WorkGroup": PRIMARY_WORK_GROUP, "MaxResults": 50 }),
            )
            .await;
        pairing
            .athena("StopQueryExecution", json!({ "QueryExecutionId": &id }))
            .await;
    }

    let envelopes = pairing.drain_until(case, &url, EVENTS_PER_QUERY).await;
    let details = details_of(case, &envelopes, &id);
    assert_eq!(
        details.len(),
        EVENTS_PER_QUERY,
        "case {case:?}: expected one envelope per transition however often the query is read, \
         got {details:?}"
    );
    let sequences: Vec<String> = details
        .iter()
        .map(|detail| {
            detail["sequenceNumber"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        sequences,
        vec!["1".to_string(), "2".to_string(), "3".to_string()],
        "case {case:?}: the envelopes of one query are numbered from one without repeating, \
         got {details:?}"
    );
}
