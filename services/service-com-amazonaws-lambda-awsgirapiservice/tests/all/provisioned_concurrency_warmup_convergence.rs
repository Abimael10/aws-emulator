// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a provisioned concurrency allocation CONVERGING, driven by nothing
//! but the clock: `PutProvisionedConcurrencyConfig`
//! (`PUT /2019-09-30/functions/{FunctionName}/provisioned-concurrency`) followed by
//! `GetProvisionedConcurrencyConfig` and `ListProvisionedConcurrencyConfigs` polled until the
//! allocation reports itself ready.
//!
//! The rule under test: `PutProvisionedConcurrencyConfig` ACCEPTS an allocation rather than
//! finishing one — it answers 202 with `Status` `IN_PROGRESS` and nothing allocated, "the
//! status of the allocation process" — and the allocation then CONVERGES on its own, with no
//! further call made and nothing of the client's doing: a successful one reaches `Status`
//! `READY` with `AllocatedProvisionedConcurrentExecutions` ==
//! `AvailableProvisionedConcurrentExecutions` == `RequestedProvisionedConcurrentExecutions`,
//! which is the state a client waits for before it cuts traffic over to the allocation. So a
//! client that merely polls a reader gets there, and gets there inside the patience such a
//! poll has. Nothing here moves the stored due instant, which is what makes this the suite
//! that would notice an allocation settling only when a test reaches into the store.
//!
//! The rows differ along what the rule generalises over: how much concurrency is requested
//! (one unit, a few, and more units than this suite has functions) and which kind of qualifier
//! holds the allocation (a published version and an alias), since a configuration "is added to
//! a function's alias or version".
//!
//! What is NOT asserted here, because it is asserted elsewhere:
//! `crate::provisioned_concurrency` pins what every reader reports for a stored configuration,
//! its refusals and its deletion, and `crate::provisioned_concurrency_invocation_series` which
//! invocations a ready allocation serves.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `State` a function settles in, from the model's `State` enum — the state a client waits
/// for before it configures or invokes the function.
const STATE_ACTIVE: &str = "Active";

/// The `ProvisionedConcurrencyStatusEnum` values an allocation moves through, from
/// `smithy_query(query="shape", shape="ProvisionedConcurrencyStatusEnum")`:
/// `IN_PROGRESS | READY | FAILED`.
const STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
/// See [`STATUS_IN_PROGRESS`].
const STATUS_READY: &str = "READY";

/// How long a poll may look for its condition before the case gives up — generous, so a loaded
/// host cannot turn a condition that holds into a failure. Every poll here waits for the
/// CONDITION and never for a fixed span, and nothing asserts how long one took.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A handler that answers, so a function of this suite is one a client could really provision
/// concurrency for.
const SOURCE: &str = "def lambda_handler(event, context):\n    return {'ok': True}\n";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn number(&self, pointer: &str) -> Option<i64> {
        self.json().pointer(pointer).and_then(Value::as_i64)
    }

    fn string(&self, pointer: &str) -> Option<String> {
        self.json()
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one JSON request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// Where every provisioned concurrency operation of one function is bound.
fn concurrency_uri(function: &str) -> String {
    format!("/2019-09-30/functions/{function}/provisioned-concurrency")
}

/// Create the function `name`, then WAIT for it by polling its configuration until it reports
/// itself `Active`: the transition is one the clock owes, so nothing sleeps for a fixed span.
async fn create_active_function(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "MemorySize": 512,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        if read.string("/State").as_deref() == Some(STATE_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "function {name:?} never reported {STATE_ACTIVE}, last {}",
            read.text()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Publish a version of `name` and answer its version label.
async fn publish_version(router: &MultiServiceRouter, name: &str) -> String {
    let published = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/versions"),
        &json!({}),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "PublishVersion on {name:?} must succeed, got {} {}",
        published.status,
        published.text()
    );
    published
        .string("/Version")
        .expect("a published version carries its label")
}

/// Point a new alias `alias` of `name` at `version`.
async fn create_alias(router: &MultiServiceRouter, name: &str, alias: &str, version: &str) {
    let created = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &json!({ "Name": alias, "FunctionVersion": version }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateAlias {alias:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
}

/// The four members a reader reports for one configuration, as a comparable value.
fn reported(answer: &Answer) -> Value {
    json!({
        "Requested": answer.number("/RequestedProvisionedConcurrentExecutions"),
        "Allocated": answer.number("/AllocatedProvisionedConcurrentExecutions"),
        "Available": answer.number("/AvailableProvisionedConcurrentExecutions"),
        "Status": answer.string("/Status"),
    })
}

/// What a configuration of `requested` reports in each of the two statuses: nothing allocated
/// while the allocation is being made, and all of it once it is ready.
fn expected(requested: i64, status: &str) -> Value {
    let allocated = if status == STATUS_READY { requested } else { 0 };
    json!({
        "Requested": requested,
        "Allocated": allocated,
        "Available": allocated,
        "Status": status,
    })
}

/// Poll `GetProvisionedConcurrencyConfig` until the allocation reports itself `READY`, and
/// answer that reading — or fail once [`POLL_DEADLINE`] has passed. Nothing but the CLOCK moves
/// it: no call is made that could settle it and nothing reaches into the store.
async fn poll_until_ready(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: &str,
) -> Answer {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{}?Qualifier={qualifier}", concurrency_uri(function)),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetProvisionedConcurrencyConfig must answer 200, got {} {}",
            read.status,
            read.text()
        );
        if read.string("/Status").as_deref() == Some(STATUS_READY) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the allocation never became {STATUS_READY}; last {}",
            read.text()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An accepted allocation converges on `READY` with everything it asked for allocated and
/// available, with nothing done to it but reading it.
///
/// One row per requested amount and kind of qualifier.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accepted_allocation_converges_on_ready_with_everything_it_asked_for() {
    // (case, whether an alias holds the allocation, how much concurrency the row asks for)
    let rows: &[(&str, bool, i64)] = &[
        ("one unit on a published version", false, 1),
        ("a few units on a published version", false, 3),
        ("more units than there are functions here", false, 25),
        ("one unit on an alias", true, 1),
        ("a few units on an alias", true, 5),
    ];
    for (index, (case, on_alias, requested)) in rows.iter().enumerate() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let function = &format!("converging-allocation-{index}");
        create_active_function(&router, function).await;
        let version = publish_version(&router, function).await;
        let qualifier = if *on_alias {
            let alias = "live";
            create_alias(&router, function, alias, &version).await;
            alias.to_string()
        } else {
            version
        };

        // The call ACCEPTS the allocation: 202, with the amount requested and nothing
        // allocated yet, which is what "the status of the allocation process" reports.
        let accepted = call(
            &router,
            "PUT",
            &format!("{}?Qualifier={qualifier}", concurrency_uri(function)),
            &json!({ "ProvisionedConcurrentExecutions": requested }),
        )
        .await;
        assert_eq!(
            accepted.status,
            202,
            "case {case:?}: PutProvisionedConcurrencyConfig must be accepted, got {} {}",
            accepted.status,
            accepted.text()
        );
        assert_eq!(
            reported(&accepted),
            expected(*requested, STATUS_IN_PROGRESS),
            "case {case:?}: the accepting call must answer the allocation it started, got {}",
            accepted.text()
        );

        // And then it converges, with no further call of the client's: polling a reader is
        // what a client waiting for the allocation does, and it gets there.
        let ready = poll_until_ready(&router, case, function, &qualifier).await;
        assert_eq!(
            reported(&ready),
            expected(*requested, STATUS_READY),
            "case {case:?}: a converged allocation must report all of {requested} allocated \
             and available, got {}",
            ready.text()
        );

        // Reading it again changes nothing: the convergence was the clock's, not the read's.
        let again = call(
            &router,
            "GET",
            &format!("{}?Qualifier={qualifier}", concurrency_uri(function)),
            &Value::Null,
        )
        .await;
        assert_eq!(
            reported(&again),
            reported(&ready),
            "case {case:?}: a second read must report the same converged allocation, got {}",
            again.text()
        );

        // The listing of the function's allocations agrees with that reader, so what
        // converged is the stored record and not one reader's answer.
        let listed = call(
            &router,
            "GET",
            &format!("{}?List=ALL", concurrency_uri(function)),
            &Value::Null,
        )
        .await;
        assert_eq!(
            listed.status,
            200,
            "case {case:?}: ListProvisionedConcurrencyConfigs must answer 200, got {} {}",
            listed.status,
            listed.text()
        );
        let entries = listed
            .json()
            .get("ProvisionedConcurrencyConfigs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: the function holds exactly the one allocation, got {}",
            listed.text()
        );
        let entry = &entries[0];
        assert_eq!(
            json!({
                "Requested": entry.get("RequestedProvisionedConcurrentExecutions"),
                "Allocated": entry.get("AllocatedProvisionedConcurrentExecutions"),
                "Available": entry.get("AvailableProvisionedConcurrentExecutions"),
                "Status": entry.get("Status"),
            }),
            expected(*requested, STATUS_READY),
            "case {case:?}: the listing must report the converged allocation, got {}",
            listed.text()
        );
    }
}
