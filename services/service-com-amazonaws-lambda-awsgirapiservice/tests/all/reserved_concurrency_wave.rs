// SPDX-License-Identifier: CC0-1.0

//! How many of a function's invocations RUN AT THE SAME TIME, end to end over the wire with
//! Amazon DynamoDB and IAM mounted beside Lambda on ONE engine — so the invocations register
//! themselves where a client's would, each handler's own SDK calls are made as the function's
//! execution role, and what every run saw of the runs beside it is read out of the item store
//! they share.
//!
//! The RULE asserted over the table below: concurrency is "the number of in-flight requests
//! your function is handling at the same time", so every invocation Lambda admits runs BESIDE
//! the other admitted ones — up to the `ReservedConcurrentExecutions` the function reserves,
//! "the number of simultaneous executions to reserve for the function" — and what each of
//! those runs is configured for does not reduce that: `MemorySize` is "the amount of memory
//! available to the function at runtime", a ceiling on what ONE run may grow to, not capacity
//! taken from the invocations beside it. So a wave of N invocations of a function reserving N
//! is N runs in flight at once, whatever memory the function asks for, and a barrier that
//! needs every one of them running together is reached.
//!
//! Every row's verdict is computed INSIDE the handler, from state the other runs wrote: each
//! invocation registers itself in the table and then looks again until it can see every caller
//! of its own wave registered. A wave whose runs do not overlap cannot report a complete one —
//! every invocation but the last looks in vain and reports what it saw.
//!
//! The rows differ along what the rule generalises over: the `MemorySize` the function is
//! configured for — the smallest the model allows, the memory one run's own heap ceiling is
//! stated at, and a value above that ceiling, which is where a per-run claim on a shared
//! memory envelope used to decide the concurrency — and how large a wave has to be in flight
//! at once.
//!
//! Nothing here waits out a span: the handler LOOKS AGAIN for the condition it waits on until
//! it holds or its own generous look budget is spent, and nothing asserts on how long any of
//! it took.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    run_memory_claim_mb, MAX_CONCURRENT_RUN_MEMORY_MB,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::dynamodb;
use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::event_source_mappings::{call, router_on, LOOK_AGAIN};
use crate::synchronous_invoke_run::package;

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The region every ARN below names.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `PutFunctionConcurrency` is bound: `PUT /2017-10-31/functions/{FunctionName}/concurrency`
/// (`smithy_query(query="operation", operation="PutFunctionConcurrency")`), which is a
/// different prefix from the create's.
const CONCURRENCY_URI_PREFIX: &str = "/2017-10-31/functions";

/// The runtime every function below runs, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `Timeout` every function below carries, in seconds: well above what a wave needs to
/// assemble, so a row reports the concurrency rather than a run its own deadline ended.
const FUNCTION_TIMEOUT: u64 = 30;

/// The partition key the registration table is keyed on.
const KEY: &str = "caller";

/// The two item-store actions the handler performs, which is all its execution role grants it
/// of DynamoDB.
const REGISTRATION_ACTIONS: [&str; 2] = ["PutItem", "Scan"];

/// How many times one handler looks for the rest of its wave before it gives up and reports
/// what it saw.
///
/// A generous ceiling on the LOOKING, not a span a row waits out: a wave really in flight
/// together is complete within the first looks, and this is well inside both the per-run call
/// budget and the function's own `Timeout`.
const LOOKS: u32 = 100;

/// How long the handler waits between two looks, in seconds.
const LOOK_AGAIN_SECONDS: f64 = 0.1;

/// How long a wait for a clock-owed transition is given before a row is a failure.
const BUDGET: Duration = Duration::from_secs(45);

/// The handler every row runs: it registers this invocation in the table and then looks again
/// until every caller of its own wave is registered, reporting how many it came to see.
///
/// The verdict is the handler's own: nothing but the runs of the other invocations can put
/// those registrations there, so a wave whose runs do not overlap cannot report a complete
/// one.
const BARRIER_HANDLER: &str = r#"
import os
import time

import boto3

TABLE = os.environ["TABLE"]
WANTED = int(os.environ["WANTED"])
LOOKS = int(os.environ["LOOKS"])
LOOK_AGAIN = float(os.environ["LOOK_AGAIN"])


def lambda_handler(event, context):
    client = boto3.client("dynamodb")
    wave = str(event["wave"])
    client.put_item(
        TableName=TABLE,
        Item={"caller": {"S": str(event["caller"])}, "wave": {"S": wave}},
    )
    seen = 0
    for _ in range(LOOKS):
        scanned = client.scan(TableName=TABLE)
        seen = len(
            [
                item
                for item in scanned.get("Items", [])
                if item.get("wave", {}).get("S") == wave
            ]
        )
        if seen >= WANTED:
            break
        time.sleep(LOOK_AGAIN)
    return {"caller": event["caller"], "seen": seen}
"#;

/// Lambda, DynamoDB and IAM on ONE engine — the shape `core/router/src/lib.rs` mounts them
/// in: the handler's own calls are dispatched back into this same router, so the table the
/// invocations register in is the one this suite created and the role they act as is the one
/// IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// The row's registration table, waited out to `ACTIVE`.
async fn given_table(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, created) = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": name,
            "KeySchema": [{ "AttributeName": KEY, "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": KEY, "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTable answered {status} {created}"
    );
    let deadline = std::time::Instant::now() + BUDGET;
    loop {
        let (status, read) = dynamodb(router, "DescribeTable", &json!({ "TableName": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTable answered {status} {read}"
        );
        if read["Table"]["TableStatus"] == json!("ACTIVE") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the table {name:?} never became ACTIVE: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The row's function: it runs [`BARRIER_HANDLER`] at `memory_size` MB as `role_arn`, reserves
/// `callers` of the account's concurrency for itself, and is waited out to `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    table: &str,
    memory_size: u32,
    callers: u32,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "MemorySize": memory_size,
            "Environment": { "Variables": {
                "TABLE": table,
                "WANTED": callers.to_string(),
                "LOOKS": LOOKS.to_string(),
                "LOOK_AGAIN": LOOK_AGAIN_SECONDS.to_string(),
            } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(BARRIER_HANDLER)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction answered {} {}",
        created.status,
        created.text()
    );
    // "The number of simultaneous executions to reserve for the function"
    // (`PutFunctionConcurrency$ReservedConcurrentExecutions`): the wave below is exactly that
    // many invocations, so every one of them is inside the reservation.
    let reserved = call(
        router,
        "PUT",
        &format!("{CONCURRENCY_URI_PREFIX}/{name}/concurrency"),
        &json!({ "ReservedConcurrentExecutions": callers }),
    )
    .await;
    assert_eq!(
        reserved.status,
        200,
        "case {case:?}: PutFunctionConcurrency answered {} {}",
        reserved.status,
        reserved.text()
    );
    assert_eq!(
        reserved.json()["ReservedConcurrentExecutions"],
        json!(callers),
        "case {case:?}: the function must report the concurrency it reserved, got {}",
        reserved.text()
    );
    let deadline = std::time::Instant::now() + BUDGET;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        if read.string("State").as_deref() == Some("Active") {
            assert_eq!(
                read.json()["MemorySize"],
                json!(memory_size),
                "case {case:?}: the function must carry the memory it was created with, got {}",
                read.text()
            );
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the function {name:?} never became Active: {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What one invocation of a wave answered.
struct Answered {
    caller: u32,
    status: u16,
    body: Value,
    text: String,
}

/// Fire `callers` invocations of `function` that are all in flight at once, each carrying its
/// own index and the wave it belongs to, and answer what each of them came to.
///
/// A CLONE of the router per caller: they share the one engine and the one service instance,
/// so on this multi-thread runtime the invocations really are dispatched together.
async fn wave(
    router: &MultiServiceRouter,
    function: &str,
    label: &str,
    callers: u32,
) -> Vec<Answered> {
    let mut invoking = tokio::task::JoinSet::new();
    for caller in 0..callers {
        let router = router.clone();
        let function = function.to_string();
        let label = label.to_string();
        invoking.spawn(async move {
            let answered = call(
                &router,
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/invocations"),
                &json!({ "caller": caller, "wave": label }),
            )
            .await;
            Answered {
                caller,
                status: answered.status,
                body: answered.json(),
                text: answered.text(),
            }
        });
    }
    let mut answers: Vec<Answered> = Vec::with_capacity(callers as usize);
    while let Some(joined) = invoking.join_next().await {
        answers.push(joined.expect("an invoking task must not panic"));
    }
    answers.sort_by_key(|answered| answered.caller);
    answers
}

/// One row: the memory the function is configured for, and how many invocations have to be in
/// flight at once.
struct Row {
    case: &'static str,
    /// The `MemorySize` the function carries, in MB.
    memory_size: u32,
    /// How many invocations the wave fires, which is also what the function reserves.
    callers: u32,
}

/// The rows: one per class of configured memory.
///
/// The bounds are the model's own (`smithy_query(query="shape", shape="MemorySize")`:
/// `@range(min: 128, max: 32768)`); the middle row is the memory this emulator states one
/// run's heap ceiling at, and the last is above it, which is where a claim stated per run used
/// to decide how many of them ran at once.
///
/// The last row's wave is WIDER than the runs a function of that memory used to be allowed —
/// twice that count — which is what the rule is about: that the configured memory does not
/// decide the number. It is deliberately NOT as wide as the whole process admits
/// ([`runs_of_the_largest_heap`], asserted below to cover the row): one test of this target
/// must not take every run slot of the host while the other suites of the workspace are
/// running beside it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the smallest MemorySize the model allows",
            memory_size: 128,
            callers: 5,
        },
        Row {
            case: "the memory one run's own heap ceiling is stated at",
            memory_size: 512,
            callers: 5,
        },
        Row {
            case: "a MemorySize above that ceiling, in a wave twice what it used to allow",
            memory_size: 1_024,
            callers: 8,
        },
    ]
}

/// How many runs of the largest per-run heap this emulator serves at once, read from the
/// substrate itself rather than restated here: the envelope it holds together, divided by what
/// one run of a function configured past the per-run heap ceiling claims of it.
///
/// That is the concurrency the rule above promises a function of ANY configured memory, so
/// every row's wave has to fit inside it for the row to be about the rule at all.
fn runs_of_the_largest_heap() -> u32 {
    MAX_CONCURRENT_RUN_MEMORY_MB / run_memory_claim_mb(i32::MAX)
}

/// RULE: every invocation Lambda admits inside the function's reserved concurrency runs beside
/// the others, whatever the function's `MemorySize` — so a wave of N invocations of a function
/// reserving N reaches a barrier that needs all N of them in flight together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reserved_wave_of_invocations_runs_at_the_same_time() {
    for row in rows() {
        let case = row.case;
        let serves = runs_of_the_largest_heap();
        assert!(
            row.callers <= serves,
            "case {case:?}: the row's wave of {} must fit the {serves} runs of the largest \
             per-run heap this emulator serves at once, or the row is about the host's own \
             ceiling rather than the rule",
            row.callers
        );
        let router = emulator();
        let label = format!("barrier-{}", row.memory_size);
        let table = format!("{label}-registrations");
        let function = format!("{label}-fn");
        given_table(&router, case, &table).await;
        let role_arn = given_role(
            &router,
            case,
            &format!("{label}-role"),
            Some(json!([
                logs_only(),
                allow(
                    "dynamodb",
                    &REGISTRATION_ACTIONS,
                    &format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{table}"),
                ),
            ])),
        )
        .await;
        given_function(
            &router,
            case,
            &function,
            &role_arn,
            &table,
            row.memory_size,
            row.callers,
        )
        .await;

        let answers = wave(&router, &function, "one", row.callers).await;
        for answered in &answers {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: invocation {} of {} is inside the reserved concurrency, so it \
                 must be served, got {} {}",
                answered.caller, row.callers, answered.status, answered.text
            );
            assert_eq!(
                answered.body,
                json!({ "caller": answered.caller, "seen": row.callers }),
                "case {case:?}: invocation {} of {} must see all {} invocations of its wave \
                 registered while it runs, got {}",
                answered.caller,
                row.callers,
                row.callers,
                answered.text
            );
        }
    }
}
