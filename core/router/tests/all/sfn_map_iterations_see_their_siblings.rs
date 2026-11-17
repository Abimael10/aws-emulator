// SPDX-License-Identifier: CC0-1.0

//! What ONE iteration of a Step Functions `Map` state sees of what its SIBLINGS have
//! committed, driven across the two crates the question spans (Step Functions runs the
//! iterations, DynamoDB holds the state they coordinate through), mounted on ONE engine
//! exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: the iterations of a `Map` state run AT THE SAME TIME, up to the state's
//! `MaxConcurrency` — "an upper bound on how many invocations of the Iterator may run in
//! parallel" — and each one issues its own service calls against the one state the
//! service holds, so an iteration's call made at a given point of the run observes every
//! effect a sibling iteration committed at or before that point. An iteration whose own
//! control flow waits on such an effect (a counter, a lock, a ledger, a barrier)
//! therefore takes the branch the siblings' progress unlocks, and the branch it takes is
//! what decides its result and — for a Distributed Map, whose items are child workflow
//! executions — that child's own outcome. `MaxConcurrency` is a real bound either way: a
//! state that runs one item at a time commits nothing while the item that is running
//! polls, so the same definition then takes its give-up branch.
//!
//! The rows differ along what the rule generalises over: the processor's MODE (an inline
//! `Map`, a `DISTRIBUTED` one, whose items are child executions), how many items run at
//! once (`MaxConcurrency` equal to the item count, the member's documented "no limit" of
//! `0`, a limit smaller than the item count, and `1`, which serialises them), WHERE the
//! coordinating item sits among the items (first, in the middle, after a chunk
//! boundary), HOW MANY of them coordinate (one, two), and how many siblings they wait
//! for. Every row runs the same definition shape, so what differs between a row that
//! reaches the released branch and one that gives up is the concurrency alone.
//!
//! The second test asks the same question of the per-item ERROR identity, which is what
//! a client routes on: a `Map` state whose iterations end at a `Fail` state reports, for
//! each failed child workflow execution, the error named on the `Fail` state THAT child
//! reached — so a child released by its siblings' progress carries the released branch's
//! error and never the give-up branch's.
//!
//! The error names, the item roles and the counter key are this suite's own request
//! values; what is asserted about them is only that the outcome names the branch the
//! data decided.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a poll waits for the condition it is waiting on — a created table becoming
/// `ACTIVE`, an execution leaving `RUNNING`. Generous: every loop stops as soon as its
/// condition holds, so this bounds only a failure.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long a poll waits between looks.
const GLANCE: Duration = Duration::from_millis(25);

/// The table the iterations coordinate through, and its one hash key.
const TABLE: &str = "barrier";

/// The `Wait` a coordinating iteration takes between two looks at the counter, in
/// seconds: the walk sleeps for none of it, and it is what moves that iteration's own
/// clock past its siblings' so they run.
const NAP_SECONDS: i64 = 1;

/// How many looks a coordinating iteration takes before it gives up.
const MAX_TRIES: i64 = 2;

/// What a coordinating iteration puts out when the counter reached what it waits for,
/// and when it ran out of looks instead.
const RELEASED: &str = "released";
const GAVE_UP: &str = "gave-up";

/// What a worker iteration puts out, once it has committed its increment.
const BUMPED: &str = "bumped";

/// The two services this question spans, over ONE shared engine. Request authentication
/// is off, as in every other suite here; the in-process calls Step Functions makes sign
/// themselves either way (the credential scope is what routes them).
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One delivery of one `awsJson1_0` call: its status and its body.
async fn deliver(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Step Functions call that must answer 200, as its body.
async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = deliver(router, "states", "AWSStepFunctions", operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation}: expected 200 from Step Functions, got {status} with {parsed:?}"
    );
    parsed
}

/// One DynamoDB call that must answer 200, as its body.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = deliver(router, "dynamodb", "DynamoDB_20120810", operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation}: expected 200 from DynamoDB, got {status} with {parsed:?}"
    );
    parsed
}

/// The text a member of a response carries.
fn string(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a string {name} in {body}"))
        .to_string()
}

/// The table every row coordinates through. "You can perform read and write operations
/// only on an ACTIVE table", so the create is followed by a poll until it is one.
async fn barrier_table(router: &MultiServiceRouter) {
    dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let began = Instant::now();
    loop {
        let described = dynamodb(router, "DescribeTable", &json!({ "TableName": TABLE })).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "expected {TABLE} to report ACTIVE within {PATIENCE:?}, got {described:?}"
        );
        ::tokio::time::sleep(GLANCE).await;
    }
}

/// Seed the counter one row's iterations coordinate through at zero, so the iteration
/// that reads it reads a figure rather than nothing.
async fn seeded_counter(router: &MultiServiceRouter, key: &str) {
    dynamodb(
        router,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": { "pk": { "S": key }, "n": { "N": "0" } },
        }),
    )
    .await;
}

/// The counter's value, as DynamoDB holds it.
async fn counter(router: &MultiServiceRouter, key: &str) -> i64 {
    let read = dynamodb(
        router,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { "pk": { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    read["Item"]["n"]["N"]
        .as_str()
        .unwrap_or_default()
        .parse()
        .unwrap_or_else(|error| panic!("expected a number in {read}, got {error}"))
}

/// Which part an item plays: one that COMMITS its increment, or one that WAITS for the
/// others' increments before it decides what to put out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Worker,
    Coordinator,
}

impl Role {
    fn named(self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Coordinator => "coordinator",
        }
    }
}

/// The `ItemProcessor` every row runs, with the two terminal states written as `kind`
/// says: a `Pass` that puts out which branch was taken, or a `Fail` that names it.
///
/// A worker commits `ADD n :one` against the row's counter and puts out that it did; a
/// coordinator reads the counter with `ConsistentRead` and, once it holds `threshold`
/// increments, takes the released branch — else it naps and looks again, up to
/// [`MAX_TRIES`] times, and then gives up.
fn processor(distributed: bool, threshold: i64, terminal: &dyn Fn(&str) -> Value) -> Value {
    let mut states = json!({
        "Route": {
            "Type": "Choice",
            "Choices": [{
                "Variable": "$.role",
                "StringEquals": Role::Coordinator.named(),
                "Next": "Poll",
            }],
            "Default": "Bump",
        },
        "Bump": {
            "Type": "Task",
            "Resource": "arn:aws:states:::dynamodb:updateItem",
            "Parameters": {
                "TableName": TABLE,
                "Key": { "pk": { "S.$": "$.key" } },
                "UpdateExpression": "ADD n :one",
                "ExpressionAttributeValues": { ":one": { "N": "1" } },
            },
            "ResultPath": "$.wrote",
            "Next": "Bumped",
        },
        "Bumped": { "Type": "Pass", "Result": { "gate": BUMPED }, "End": true },
        "Poll": {
            "Type": "Task",
            "Resource": "arn:aws:states:::dynamodb:getItem",
            "Parameters": {
                "TableName": TABLE,
                "Key": { "pk": { "S.$": "$.key" } },
                "ConsistentRead": true,
            },
            "ResultSelector": { "n.$": "States.StringToJson($.Item.n.N)" },
            "ResultPath": "$.done",
            "Next": "Gate",
        },
        "Gate": {
            "Type": "Choice",
            "Choices": [
                { "Variable": "$.done.n", "NumericGreaterThanEquals": threshold, "Next": "Released" },
                { "Variable": "$.tries.n", "NumericGreaterThanEquals": MAX_TRIES, "Next": "GaveUp" },
            ],
            "Default": "Nap",
        },
        "Nap": { "Type": "Wait", "Seconds": NAP_SECONDS, "Next": "Tick" },
        "Tick": {
            "Type": "Pass",
            "Parameters": {
                "role.$": "$.role",
                "key.$": "$.key",
                "tries": { "n.$": "States.MathAdd($.tries.n, 1)" },
            },
            "Next": "Poll",
        },
    });
    if let Some(members) = states.as_object_mut() {
        members.insert("Released".to_string(), terminal(RELEASED));
        members.insert("GaveUp".to_string(), terminal(GAVE_UP));
    }
    let mut processor = json!({ "StartAt": "Route", "States": states });
    if distributed {
        if let Some(members) = processor.as_object_mut() {
            members.insert(
                "ProcessorConfig".to_string(),
                json!({ "Mode": "DISTRIBUTED", "ExecutionType": "STANDARD" }),
            );
        }
    }
    processor
}

/// A state machine whose one `Map` state iterates the items its input carries.
async fn machine(
    router: &MultiServiceRouter,
    name: &str,
    concurrency: i64,
    processor: Value,
    tolerated_percentage: Option<i64>,
) -> String {
    let mut map_state = json!({
        "Type": "Map",
        "ItemsPath": "$.items",
        "MaxConcurrency": concurrency,
        "ItemSelector": {
            "role.$": "$$.Map.Item.Value.role",
            "key.$": "$.key",
            "tries": { "n": 0 },
        },
        "ItemProcessor": processor,
        "End": true,
    });
    if let (Some(members), Some(percentage)) = (map_state.as_object_mut(), tolerated_percentage) {
        members.insert(
            "ToleratedFailurePercentage".to_string(),
            json!(percentage),
        );
    }
    let definition = json!({ "StartAt": "Fan", "States": { "Fan": map_state } });
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition.to_string(),
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/StepFunctionsRole"),
        }),
    )
    .await;
    string(&created, "stateMachineArn")
}

/// Start one execution of `machine_arn` over `items` and report the `DescribeExecution`
/// that settled it, polled until it has left `RUNNING`.
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    key: &str,
    roles: &[Role],
) -> Value {
    let items: Vec<Value> = roles
        .iter()
        .map(|role| json!({ "role": role.named() }))
        .collect();
    let started = sfn(
        router,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": "only",
            "input": json!({ "key": key, "items": items }).to_string(),
        }),
    )
    .await;
    let execution_arn = string(&started, "executionArn");
    let began = Instant::now();
    loop {
        let described = sfn(
            router,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected a terminal status within {PATIENCE:?}, got {described:?}"
        );
        ::tokio::time::sleep(GLANCE).await;
    }
}

/// One row: the `Map` state's mode and concurrency, the part each of its items plays,
/// how many increments a coordinator waits for, and which branch each item must take.
struct Row {
    case: &'static str,
    /// Whether the processor declares `"Mode": "DISTRIBUTED"`, so each item is a child
    /// workflow execution of its own.
    distributed: bool,
    /// `MaxConcurrency` as the state declares it; `0` is the member's own documented
    /// "no limit".
    concurrency: i64,
    roles: &'static [Role],
    /// How many sibling increments a coordinating item waits for.
    threshold: i64,
    /// What each item must put out, in the order the items are listed.
    expected: &'static [&'static str],
}

/// A `Pass` state that puts out which branch the iteration took.
fn reports(branch: &str) -> Value {
    json!({ "Type": "Pass", "Result": { "gate": branch }, "End": true })
}

/// Every iteration of a `Map` state observes what its siblings have committed, so a
/// coordinating one takes the branch their progress unlocks — and takes the give-up
/// branch exactly where `MaxConcurrency` leaves them nothing to observe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn map_iterations_observe_what_their_siblings_commit() {
    use Role::{Coordinator as C, Worker as W};

    let rows: &[Row] = &[
        Row {
            case: "a distributed map whose coordinator is its first item, every item at once",
            distributed: true,
            concurrency: 4,
            roles: &[C, W, W, W],
            threshold: 3,
            expected: &[RELEASED, BUMPED, BUMPED, BUMPED],
        },
        Row {
            case: "a distributed map with no concurrency limit, coordinating in the middle",
            distributed: true,
            concurrency: 0,
            roles: &[W, W, C, W, W, W],
            threshold: 5,
            expected: &[BUMPED, BUMPED, RELEASED, BUMPED, BUMPED, BUMPED],
        },
        Row {
            case: "an inline map whose coordinator is its first item",
            distributed: false,
            concurrency: 3,
            roles: &[C, W, W],
            threshold: 2,
            expected: &[RELEASED, BUMPED, BUMPED],
        },
        Row {
            case: "an inline map whose two coordinators wait on the same counter",
            distributed: false,
            concurrency: 4,
            roles: &[C, C, W, W],
            threshold: 2,
            expected: &[RELEASED, RELEASED, BUMPED, BUMPED],
        },
        Row {
            case: "a distributed map whose concurrency is smaller than its item count",
            distributed: true,
            concurrency: 2,
            roles: &[W, W, C, W],
            threshold: 3,
            expected: &[BUMPED, BUMPED, RELEASED, BUMPED],
        },
        Row {
            case: "a distributed map whose MaxConcurrency of one serialises its items",
            distributed: true,
            concurrency: 1,
            roles: &[C, W, W, W],
            threshold: 3,
            expected: &[GAVE_UP, BUMPED, BUMPED, BUMPED],
        },
    ];

    let router = emulator();
    barrier_table(&router).await;
    for (index, row) in rows.iter().enumerate() {
        let key = format!("run-{index}");
        seeded_counter(&router, &key).await;
        let machine_arn = machine(
            &router,
            &format!("barrier-{index}"),
            row.concurrency,
            processor(row.distributed, row.threshold, &reports),
            None,
        )
        .await;
        let settled = ran(&router, row.case, &machine_arn, &key, row.roles).await;
        assert_eq!(
            settled.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {:?}: expected the run to succeed, got {settled:?}",
            row.case
        );
        let output: Value = serde_json::from_str(&string(&settled, "output"))
            .unwrap_or_else(|error| panic!("case {:?}: expected JSON output, got {error}", row.case));
        let taken: Vec<String> = output
            .as_array()
            .unwrap_or_else(|| panic!("case {:?}: expected an array, got {output}", row.case))
            .iter()
            .map(|item| item["gate"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            taken,
            row.expected
                .iter()
                .map(|branch| (*branch).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected each item to take its own branch, got {taken:?} from {output}",
            row.case
        );
        // The counter really holds what the workers committed, which is what the
        // coordinators were reading: one increment per worker and no more.
        let committed = i64::try_from(
            row.roles
                .iter()
                .filter(|role| **role == Role::Worker)
                .count(),
        )
        .unwrap_or(i64::MAX);
        let held = counter(&router, &key).await;
        assert_eq!(
            held, committed,
            "case {:?}: expected the counter to hold one increment per worker, got {held}",
            row.case
        );
    }
}

/// A `Fail` state that names the branch the iteration took as the error it fails with.
fn fails_with(branch: &str) -> Value {
    json!({
        "Type": "Fail",
        "Error": format!("Barrier.{branch}"),
        "Cause": format!("the {branch} branch was taken"),
    })
}

/// Every child workflow execution of a Distributed Map reports the error of the `Fail`
/// state THAT child reached, so the child a barrier released reports the released
/// branch's error — and the Map Run's own counts agree with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_child_reports_the_error_of_the_fail_state_it_reached() {
    const WORKERS: usize = 4;
    const KEY: &str = "error-identity";
    const TOLERATED_PERCENTAGE: i64 = 50;

    let router = emulator();
    barrier_table(&router).await;
    seeded_counter(&router, KEY).await;
    let roles: Vec<Role> = std::iter::once(Role::Coordinator)
        .chain(vec![Role::Worker; WORKERS])
        .collect();
    let machine_arn = machine(
        &router,
        "barrier-error-identity",
        i64::try_from(roles.len()).unwrap_or(i64::MAX),
        processor(
            true,
            i64::try_from(WORKERS).unwrap_or(i64::MAX),
            &fails_with,
        ),
        Some(TOLERATED_PERCENTAGE),
    )
    .await;
    let case = "a distributed map whose coordinating child ends at a Fail state";
    let settled = ran(&router, case, &machine_arn, KEY, &roles).await;
    assert_eq!(
        settled.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: one failed item of five is inside the state's tolerance, got {settled:?}"
    );

    // The Map Run counts the items as they turned out, and reports the tolerance it ran
    // under.
    let runs = sfn(
        &router,
        "ListMapRuns",
        &json!({ "executionArn": string(&settled, "executionArn") }),
    )
    .await;
    let listed = runs["mapRuns"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a mapRuns array, got {runs}"));
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: one Distributed Map state is one Map Run, got {runs}"
    );
    let map_run_arn = string(&listed[0], "mapRunArn");
    let described = sfn(&router, "DescribeMapRun", &json!({ "mapRunArn": map_run_arn })).await;
    assert_eq!(
        described["itemCounts"]["total"].as_i64(),
        Some(i64::try_from(roles.len()).unwrap_or(i64::MAX)),
        "case {case:?}: expected every item counted, got {described}"
    );
    assert_eq!(
        described["itemCounts"]["succeeded"].as_i64(),
        Some(i64::try_from(WORKERS).unwrap_or(i64::MAX)),
        "case {case:?}: expected the workers to have succeeded, got {described}"
    );
    assert_eq!(
        described["itemCounts"]["failed"].as_i64(),
        Some(1),
        "case {case:?}: expected the one coordinating item to have failed, got {described}"
    );
    assert_eq!(
        described["toleratedFailurePercentage"].as_f64(),
        Some(TOLERATED_PERCENTAGE as f64),
        "case {case:?}: expected the declared tolerance, got {described}"
    );

    // And the failed child names the branch the data decided, which is the `Fail` state
    // the barrier released into.
    let children = sfn(
        &router,
        "ListExecutions",
        &json!({ "mapRunArn": map_run_arn }),
    )
    .await;
    let listed = children["executions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an executions array, got {children}"));
    assert_eq!(
        listed.len(),
        roles.len(),
        "case {case:?}: one child per item, got {children}"
    );
    let mut failures: Vec<String> = Vec::new();
    for child in listed {
        let child_arn = string(child, "executionArn");
        let described = sfn(
            &router,
            "DescribeExecution",
            &json!({ "executionArn": child_arn }),
        )
        .await;
        if described.get("status").and_then(Value::as_str) == Some("FAILED") {
            failures.push(string(&described, "error"));
        }
    }
    assert_eq!(
        failures,
        vec![format!("Barrier.{RELEASED}")],
        "case {case:?}: expected the one failed child to carry the error of the Fail state the \
         barrier released into, got {failures:?}"
    );
}
