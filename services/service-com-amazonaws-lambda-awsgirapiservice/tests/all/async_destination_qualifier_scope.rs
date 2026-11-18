// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH asynchronous-invocation configuration decides where the
//! completion of an accepted asynchronous event delivers its invocation record.
//!
//! RULE: the destinations a completion delivers to are read, at the instant the event
//! completes, out of the asynchronous-invocation configuration stored under the QUALIFIER the
//! event was addressed to — the resource `PutFunctionEventInvokeConfig` "configures options
//! for asynchronous invocation on a function, version, or alias" is held per qualifier, so a
//! qualifier that holds no configuration of its own inherits none from another, and a put,
//! update or delete that changed the stored configuration governs every event completing after
//! it.
//!
//! The rows of the first table differ along what the rule generalises over: the qualifier the
//! event is addressed to (none, a published version, an alias) against the qualifiers that
//! carry a configuration, so an emulator that fell back to the unpublished version's
//! destinations, or that resolved an alias to its target version's configuration, fails here.
//! The rows of the second differ along the call that CHANGED the stored configuration (a put
//! that states no destinations, a delete, a delete followed by a put again, an update that
//! states nothing about the slot). The last two suites drive the two remaining forms: an
//! update that redirects one slot while the other stands, and an alias repointed between two
//! events.
//!
//! Every row drives real HTTP requests through one router carrying Lambda and Amazon SQS on
//! ONE engine, so a record is asserted where a client reads it — off the queue the
//! configuration names — and the function's package RUNS, so what each record reports about
//! the response is the handler's own answer.
//!
//! Nothing here waits a span of its own: every wait for a record looks again until it is there
//! or a generous deadline passes, and every "nothing is delivered" row keeps looking at the
//! queues of its row for a window far longer than a delivery that WAS owed takes, after the
//! event itself has settled.
//!
//! What a record CARRIES is `async_destination_record_shape.rs`'s subject and which SLOT an
//! outcome chooses is `async_completion_destinations.rs`'s; this suite is about which stored
//! configuration is consulted at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    AsyncInvocationOutcome, ComAmazonawsLambdaAwsgirapiserviceState,
};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// Where `CreateFunction`, `PublishVersion`, `CreateAlias`, `UpdateAlias` and `Invoke` are
/// bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where the five operations on an asynchronous-invocation configuration are bound
/// (`/2019-09-25/functions/{FunctionName}/event-invoke-config`).
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The Region every call below is made in, and so the Region of every ARN it mints.
const REGION: &str = "us-east-1";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about the role and nothing is refused for it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The request header `Invoke$InvocationType` is bound to, and the value that queues the event
/// instead of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The unpublished version, as the model spells it: the qualifier an event addressed to none
/// belongs to.
const LATEST: &str = "$LATEST";

/// The version every function below publishes first, and the one its alias points at.
const FIRST_VERSION: &str = "1";
/// The version the repoint suite moves that alias to.
const SECOND_VERSION: &str = "2";

/// The alias every row that addresses one uses, inside `Alias`'s own pattern
/// (`^(?!^[0-9]+$)([a-zA-Z0-9-_]+)$`).
const ALIAS: &str = "live";

/// The `MaximumRetryAttempts` every configuration below states: none, the floor of the
/// member's own `@range(min: 0, max: 2)`. So an event whose handler fails has exactly ONE
/// delivery attempt and completes without waiting out a retry ladder.
const NO_RETRIES: i64 = 0;

/// The `requestContext.condition` of a record of an event whose handler RETURNED.
const CONDITION_SUCCESS: &str = "Success";

/// The `requestContext.condition` of a record of an event Lambda gave up on after every
/// attempt its configuration allowed.
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The `responseContext.functionError` a run whose handler raised reports.
const UNHANDLED: &str = "Unhandled";

/// How long a row waits for work the CLOCK owes: the queueing delay of an accepted event, the
/// run of the handler, and the completion that follows it.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until what it waits for is there or this deadline passes, so a loaded host is waited for
/// and only something that never arrives fails a row.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a row keeps looking at the queues that must stay EMPTY before it accepts that
/// nothing was delivered there.
///
/// Generous on purpose: the row has already waited for its event to settle, and a delivery
/// that WAS owed is made in the same step, so a wrong delivery would be seen well inside this
/// window.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler every function below runs: it RAISES for an event asking it to and returns
/// otherwise, so which outcome a completion reached is the handler's own doing.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    if isinstance(event, dict) and event.get("fail"):
        raise RuntimeError("the handler refused " + str(event.get("mark")))
    return {"handled": event}
"#;

/// Lambda beside the queues every delivery of this suite is made to — all on ONE engine, so
/// every record is written where a client reads it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. Nothing
/// in it is verified: the harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda control-plane or data-plane call, the way a client makes it.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the request body serializes")
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(
            builder
                .body(bytes::Bytes::from(payload))
                .expect("a well-formed request"),
        )
        .await;
    Answer {
        status: response.status().as_u16(),
        body: response.body().clone(),
    }
}

/// One Amazon SQS call (`POST /` with `X-Amz-Target: AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A queue of this account, as the queue itself reports it: its url and its ARN.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue {name:?} answered {status} {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A function whose package RUNS [`HANDLER_SOURCE`], carrying `dead_letter` as its
/// `DeadLetterConfig$TargetArn` where a suite asks for one, brought to `Active`.
///
/// The target is stated on the CREATE rather than by a later update, so every published
/// version of the function freezes it and a failed event of any qualifier is dead-lettered.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    dead_letter: Option<&str>,
) {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": RUNTIME,
        "Handler": HANDLER,
        "Timeout": 10,
        "Code": {
            "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(HANDLER_SOURCE)),
        },
    });
    if let Some(target) = dead_letter {
        body["DeadLetterConfig"] = json!({ "TargetArn": target });
    }
    let created = lambda(router, "POST", FUNCTIONS_URI, &body, &[]).await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction {name:?} answered {} {}",
        created.status,
        created.text()
    );
    make_active(engine);
}

/// Move every stored function's activation instant into the past, so the next read
/// materialises the `Pending` -> `Active` transition the clock owes rather than this suite
/// sleeping through it.
fn make_active(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = std::time::SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// `PublishVersion` on `function` with the description `description`, answering the version it
/// published.
///
/// Each call states its own description, because "Lambda doesn't publish a version if the
/// function's configuration and code haven't changed since the last version": a second publish
/// of an unchanged function answers the version already held.
async fn published_version(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    description: &str,
) -> String {
    let answer = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/versions"),
        &json!({ "Description": description }),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: PublishVersion answered {} {}",
        answer.status,
        answer.text()
    );
    answer.json()["Version"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Version in {}", answer.text()))
        .to_string()
}

/// `CreateAlias` naming `version`.
async fn given_alias(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    alias: &str,
    version: &str,
) {
    let answer = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/aliases"),
        &json!({ "Name": alias, "FunctionVersion": version }),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: CreateAlias {alias:?} answered {} {}",
        answer.status,
        answer.text()
    );
}

/// `UpdateAlias`: point `alias` at `version` instead.
async fn repointed_alias(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    alias: &str,
    version: &str,
) {
    let answer = lambda(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{function}/aliases/{alias}"),
        &json!({ "FunctionVersion": version }),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: UpdateAlias {alias:?} answered {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.json()["FunctionVersion"].as_str(),
        Some(version),
        "case {case:?}: UpdateAlias must report the version it now points at, got {}",
        answer.text()
    );
}

/// The `event-invoke-config` URI for the qualifier `qualifier` names, or for the unpublished
/// version when it names none.
fn config_uri(function: &str, qualifier: Option<&str>) -> String {
    match qualifier {
        Some(qualifier) => format!(
            "{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config?Qualifier={qualifier}"
        ),
        None => format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config"),
    }
}

/// `PutFunctionEventInvokeConfig` on that qualifier, stating no retries and the destination
/// slots asked for.
///
/// A put that is asked for NEITHER slot states no `DestinationConfig` at all, which is the
/// request that takes the destinations a qualifier held away: "if you exclude any settings,
/// they are removed".
async fn put_destinations(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: Option<&str>,
    on_success: Option<&str>,
    on_failure: Option<&str>,
) {
    let mut body = json!({ "MaximumRetryAttempts": NO_RETRIES });
    if on_success.is_some() || on_failure.is_some() {
        let mut destinations = json!({});
        if let Some(destination) = on_success {
            destinations["OnSuccess"] = json!({ "Destination": destination });
        }
        if let Some(destination) = on_failure {
            destinations["OnFailure"] = json!({ "Destination": destination });
        }
        body["DestinationConfig"] = destinations;
    }
    let answer = lambda(router, "PUT", &config_uri(function, qualifier), &body, &[]).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: PutFunctionEventInvokeConfig answered {} {}",
        answer.status,
        answer.text()
    );
}

/// `UpdateFunctionEventInvokeConfig` (`POST` on the same resource) carrying exactly `body`.
async fn update_config(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: Option<&str>,
    body: &Value,
) {
    let answer = lambda(router, "POST", &config_uri(function, qualifier), body, &[]).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: UpdateFunctionEventInvokeConfig answered {} {}",
        answer.status,
        answer.text()
    );
}

/// `DeleteFunctionEventInvokeConfig`, answered `204` for the configuration it removed.
async fn delete_config(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: Option<&str>,
) {
    let answer = lambda(
        router,
        "DELETE",
        &config_uri(function, qualifier),
        &Value::Null,
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        204,
        "case {case:?}: DeleteFunctionEventInvokeConfig answered {} {}",
        answer.status,
        answer.text()
    );
}

/// The unqualified ARN of the function `name` names.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// Queue one event for the qualifier `qualifier` names through an asynchronous `Invoke`, which
/// the accepting call answers `202` for.
async fn invoke_async(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: Option<&str>,
    event: &Value,
) {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    let answer = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations{query}"),
        event,
        &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
    )
    .await;
    assert_eq!(
        answer.status,
        202,
        "case {case:?}: an asynchronous Invoke answered {} {}",
        answer.status,
        answer.text()
    );
}

/// Wait until every event accepted for `function` has SETTLED — no entry of its queue is still
/// queued or being delivered — which is what the clock owes before the deliveries a completion
/// makes, and answer what each one came to.
async fn settled(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    function: &str,
) -> Vec<AsyncInvocationOutcome> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let outcomes: Vec<AsyncInvocationOutcome> = {
            let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
            let store = state.lock();
            store
                .functions
                .get(function)
                .map(|held| {
                    held.async_invocations
                        .iter()
                        .map(|entry| entry.outcome.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        let waiting = outcomes.iter().any(|outcome| {
            matches!(
                outcome,
                AsyncInvocationOutcome::Queued | AsyncInvocationOutcome::Delivering
            )
        });
        if !outcomes.is_empty() && !waiting {
            return outcomes;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every event accepted for {function:?} must settle within \
             {BUDGET:?}, got {outcomes:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every message waiting on the queue `url` addresses, taken off it so a later look sees only
/// what arrived since.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {received}"
    );
    let mut bodies = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            let (status, deleted) = sqs(
                router,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteMessage answered {status} {deleted}"
            );
        }
        bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
    }
    bodies
}

/// The messages on the queue `url` addresses once there is at least one, looked for again
/// until they are there or the patience above runs out.
async fn awaited(router: &MultiServiceRouter, case: &str, what: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<String> = Vec::new();
    loop {
        held.extend(drained(router, case, url).await);
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what} on {url} within {BUDGET:?}, got nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Assert the queue `url` addresses holds nothing at all, read ONCE: for a queue read after a
/// delivery the same completion owed has already arrived elsewhere.
async fn held_nothing(router: &MultiServiceRouter, case: &str, what: &str, url: &str) {
    let held = drained(router, case, url).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected nothing on {what} ({url}), got {held:?}"
    );
}

/// Keep looking at every queue `sinks` names until [`QUIET_WINDOW`] has passed, failing the
/// moment anything is delivered to one of them.
async fn stayed_empty(router: &MultiServiceRouter, case: &str, sinks: &[(&str, &str)]) {
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        for (what, url) in sinks {
            held_nothing(router, case, what, url).await;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The ONE invocation record `held` holds, as the document it is.
fn one_record(case: &str, what: &str, held: &[String]) -> Value {
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected exactly one invocation record on {what}, got {held:?}"
    );
    serde_json::from_str(&held[0])
        .unwrap_or_else(|error| panic!("case {case:?}: the record is no document: {error}"))
}

/// Assert that `record` carries the value `expected` at every pointer the table names.
fn record_states(case: &str, record: &Value, stated: &[(&str, Value)]) {
    for (pointer, expected) in stated {
        let read = record.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            read, *expected,
            "case {case:?}: expected {pointer} to be {expected}, got {read} in {record}"
        );
    }
}

/// The event a row queues when it wants the handler to RETURN, naming the row so a record
/// delivered for another row is caught.
fn returning_event(mark: &str) -> Value {
    json!({ "mark": mark })
}

/// The event a row queues when it wants the handler to RAISE; see [`HANDLER_SOURCE`].
fn failing_event(mark: &str) -> Value {
    json!({ "fail": true, "mark": mark })
}

/// What the handler answers for [`returning_event`], which is the `responsePayload` of the
/// record written for it.
fn handled(event: &Value) -> Value {
    json!({ "handled": event.clone() })
}

/// One of the three qualifiers an event of this suite can be addressed to, and a configuration
/// can be held against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressed {
    /// The unpublished version, addressed by an invocation that names no qualifier at all.
    Unqualified,
    /// The one version every row of the first table publishes.
    Version,
    /// The alias pointing at that version.
    Alias,
}

impl Addressed {
    /// The `Qualifier` a request carries to name this one, and `None` for the unqualified
    /// form — which is the request that addresses the unpublished version.
    fn qualifier(self) -> Option<&'static str> {
        match self {
            Self::Unqualified => None,
            Self::Version => Some(FIRST_VERSION),
            Self::Alias => Some(ALIAS),
        }
    }

    /// The qualifier the record's `requestContext.functionArn` is qualified by.
    fn label(self) -> &'static str {
        match self {
            Self::Unqualified => LATEST,
            Self::Version => FIRST_VERSION,
            Self::Alias => ALIAS,
        }
    }

    /// The version an invocation of this qualifier RESOLVES to, which is what the record's
    /// `responseContext.executedVersion` reports.
    fn resolves_to(self) -> &'static str {
        match self {
            Self::Unqualified => LATEST,
            Self::Version | Self::Alias => FIRST_VERSION,
        }
    }

    /// What the queue holding this qualifier's destination is called, in a name and in a
    /// report.
    fn sink_name(self) -> &'static str {
        match self {
            Self::Unqualified => "the unpublished version's destination",
            Self::Version => "the published version's destination",
            Self::Alias => "the alias's destination",
        }
    }

    /// The suffix the queue of this qualifier's destination is NAMED by.
    fn sink_suffix(self) -> &'static str {
        match self {
            Self::Unqualified => "latest",
            Self::Version => "version",
            Self::Alias => "alias",
        }
    }
}

/// Every qualifier a row of the first table configures or addresses.
const ADDRESSED: [Addressed; 3] = [
    Addressed::Unqualified,
    Addressed::Version,
    Addressed::Alias,
];

/// One row of the qualifier table: the qualifier the event is addressed to, and the qualifiers
/// that carry an asynchronous-invocation configuration of their own — each naming a queue of
/// its own, so a record delivered through the wrong one is caught rather than missed.
struct ScopeRow {
    case: &'static str,
    /// The function the row drives, which carries its own three queues.
    function: &'static str,
    addressed: Addressed,
    configured: &'static [Addressed],
}

impl ScopeRow {
    /// The qualifier whose destination is owed this event's record: the addressed one when it
    /// carries a configuration, and NONE at all otherwise — a qualifier inherits no
    /// destination from another.
    fn expected(&self) -> Option<Addressed> {
        self.configured
            .contains(&self.addressed)
            .then_some(self.addressed)
    }
}

/// RULE: a completion delivers its invocation record to the destination the ADDRESSED
/// qualifier's own configuration names, and to no other qualifier's — so a qualifier holding
/// no configuration of its own has no destination at all.
#[tokio::test]
async fn a_record_reaches_only_the_addressed_qualifiers_own_destination() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows: &[ScopeRow] = &[
        ScopeRow {
            case: "an alias configured beside the unpublished version",
            function: "scope-alias-configured",
            addressed: Addressed::Alias,
            configured: &[Addressed::Alias, Addressed::Unqualified],
        },
        ScopeRow {
            case: "an alias with no configuration of its own",
            function: "scope-alias-bare",
            addressed: Addressed::Alias,
            configured: &[Addressed::Unqualified, Addressed::Version],
        },
        ScopeRow {
            case: "a published version configured beside the unpublished version",
            function: "scope-version-configured",
            addressed: Addressed::Version,
            configured: &[Addressed::Version, Addressed::Unqualified],
        },
        ScopeRow {
            case: "a published version with no configuration of its own",
            function: "scope-version-bare",
            addressed: Addressed::Version,
            configured: &[Addressed::Unqualified, Addressed::Alias],
        },
        ScopeRow {
            case: "no qualifier, with its own configuration",
            function: "scope-latest-configured",
            addressed: Addressed::Unqualified,
            configured: &[Addressed::Unqualified],
        },
        ScopeRow {
            case: "no qualifier, while only the version and the alias are configured",
            function: "scope-latest-bare",
            addressed: Addressed::Unqualified,
            configured: &[Addressed::Version, Addressed::Alias],
        },
    ];

    // Every row is wired and its event queued first, so the deliveries happen alongside one
    // another rather than one completion-length wait per row.
    let mut wired: Vec<Vec<(Addressed, String)>> = Vec::new();
    for row in rows {
        let case = row.case;
        let mut sinks: Vec<(Addressed, String)> = Vec::new();
        let mut arns: Vec<(Addressed, String)> = Vec::new();
        for qualifier in ADDRESSED {
            let (url, arn) = given_queue(
                &router,
                case,
                &format!("{}-{}", row.function, qualifier.sink_suffix()),
            )
            .await;
            sinks.push((qualifier, url));
            arns.push((qualifier, arn));
        }
        given_function(&router, &engine, case, row.function, None).await;
        let published =
            published_version(&router, case, row.function, "the version an alias points at").await;
        assert_eq!(
            published, FIRST_VERSION,
            "case {case:?}: expected the first publish to mint version {FIRST_VERSION}, got \
             {published:?}"
        );
        given_alias(&router, case, row.function, ALIAS, FIRST_VERSION).await;
        for qualifier in row.configured {
            let arn = arns
                .iter()
                .find(|(held, _)| held == qualifier)
                .map(|(_, arn)| arn.clone())
                .expect("every qualifier of the table carries a queue");
            put_destinations(
                &router,
                case,
                row.function,
                qualifier.qualifier(),
                Some(&arn),
                None,
            )
            .await;
        }
        invoke_async(
            &router,
            case,
            row.function,
            row.addressed.qualifier(),
            &returning_event(row.function),
        )
        .await;
        wired.push(sinks);
    }

    for (row, sinks) in rows.iter().zip(&wired) {
        let case = row.case;
        let event = returning_event(row.function);
        let outcomes = settled(&engine, case, row.function).await;
        assert!(
            matches!(
                outcomes.as_slice(),
                [AsyncInvocationOutcome::Delivered {
                    function_error: None
                }]
            ),
            "case {case:?}: expected the one event to have been run by the handler and \
             returned, got {outcomes:?}"
        );

        let Some(expected) = row.expected() else {
            // No qualifier the event reached names a destination, so NOTHING is owed to any of
            // this row's queues however long the suite keeps looking.
            let watched: Vec<(&str, &str)> = sinks
                .iter()
                .map(|(qualifier, url)| (qualifier.sink_name(), url.as_str()))
                .collect();
            stayed_empty(&router, case, &watched).await;
            continue;
        };
        for (qualifier, url) in sinks {
            let what = qualifier.sink_name();
            if *qualifier != expected {
                continue;
            }
            let held = awaited(&router, case, "the invocation record", url).await;
            let record = one_record(case, what, &held);
            record_states(
                case,
                &record,
                &[
                    (
                        "/requestContext/functionArn",
                        json!(format!(
                            "{}:{}",
                            function_arn(row.function),
                            row.addressed.label()
                        )),
                    ),
                    ("/requestContext/condition", json!(CONDITION_SUCCESS)),
                    ("/requestContext/approximateInvokeCount", json!(1)),
                    ("/requestPayload", event.clone()),
                    (
                        "/responseContext/executedVersion",
                        json!(row.addressed.resolves_to()),
                    ),
                    ("/responsePayload", handled(&event)),
                ],
            );
        }
        // The queues of the qualifiers this event was NOT addressed to, read once the record
        // the same completion owed has arrived on the one that was.
        for (qualifier, url) in sinks {
            if *qualifier == expected {
                continue;
            }
            held_nothing(&router, case, qualifier.sink_name(), url).await;
        }
    }
}

/// What changed the stored configuration between the put that created it and the event that
/// completes against it.
#[derive(Clone, Copy, Debug)]
enum Mutation {
    /// A second put stating no `DestinationConfig` at all: "if you exclude any settings, they
    /// are removed".
    ClearedByPut,
    /// `DeleteFunctionEventInvokeConfig`, which leaves the qualifier holding nothing.
    Deleted,
    /// That delete followed by a put naming the destination again.
    DeletedThenPutAgain,
    /// An update stating only `MaximumRetryAttempts`: "to set one option without affecting
    /// existing settings for other options, use UpdateFunctionEventInvokeConfig".
    UpdatedRetriesOnly,
}

/// One row of the mutation table: the call that changed the configuration, and whether a
/// record is therefore still owed to the queue the first put named.
struct MutationRow {
    case: &'static str,
    /// The function the row drives, which carries its own queue.
    function: &'static str,
    mutation: Mutation,
    /// Whether the destination the first put named is still in force.
    expect_record: bool,
}

/// RULE: the destinations in force when an event completes are the ones the LAST write to that
/// qualifier's configuration left behind — a put replaces the configuration whole, a delete
/// leaves none, and an update changes only the settings its request carries.
#[tokio::test]
async fn the_destinations_in_force_are_the_ones_the_last_write_left() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows: &[MutationRow] = &[
        MutationRow {
            case: "a put that states no destinations clears the previous ones",
            function: "mutated-cleared",
            mutation: Mutation::ClearedByPut,
            expect_record: false,
        },
        MutationRow {
            case: "a delete stops delivery",
            function: "mutated-deleted",
            mutation: Mutation::Deleted,
            expect_record: false,
        },
        MutationRow {
            case: "a delete followed by a put again resumes delivery",
            function: "mutated-put-again",
            mutation: Mutation::DeletedThenPutAgain,
            expect_record: true,
        },
        MutationRow {
            case: "an update that states nothing about the slot keeps it",
            function: "mutated-retries-only",
            mutation: Mutation::UpdatedRetriesOnly,
            expect_record: true,
        },
    ];

    let mut wired: Vec<String> = Vec::new();
    for row in rows {
        let case = row.case;
        let (record_url, record_arn) =
            given_queue(&router, case, &format!("{}-records", row.function)).await;
        given_function(&router, &engine, case, row.function, None).await;
        put_destinations(&router, case, row.function, None, Some(&record_arn), None).await;
        match row.mutation {
            Mutation::ClearedByPut => {
                put_destinations(&router, case, row.function, None, None, None).await;
            }
            Mutation::Deleted => {
                delete_config(&router, case, row.function, None).await;
            }
            Mutation::DeletedThenPutAgain => {
                delete_config(&router, case, row.function, None).await;
                put_destinations(&router, case, row.function, None, Some(&record_arn), None).await;
            }
            Mutation::UpdatedRetriesOnly => {
                update_config(
                    &router,
                    case,
                    row.function,
                    None,
                    &json!({ "MaximumRetryAttempts": NO_RETRIES }),
                )
                .await;
            }
        }
        invoke_async(
            &router,
            case,
            row.function,
            None,
            &returning_event(row.function),
        )
        .await;
        wired.push(record_url);
    }

    for (row, record_url) in rows.iter().zip(&wired) {
        let case = row.case;
        let event = returning_event(row.function);
        let outcomes = settled(&engine, case, row.function).await;
        assert!(
            matches!(
                outcomes.as_slice(),
                [AsyncInvocationOutcome::Delivered {
                    function_error: None
                }]
            ),
            "case {case:?}: expected the one event to have been run by the handler and \
             returned, got {outcomes:?}"
        );
        if !row.expect_record {
            stayed_empty(
                &router,
                case,
                &[("the destination the first put named", record_url)],
            )
            .await;
            continue;
        }
        let held = awaited(&router, case, "the invocation record", record_url).await;
        let record = one_record(case, "the destination the put named", &held);
        record_states(
            case,
            &record,
            &[
                (
                    "/requestContext/functionArn",
                    json!(format!("{}:{LATEST}", function_arn(row.function))),
                ),
                ("/requestContext/condition", json!(CONDITION_SUCCESS)),
                ("/requestPayload", event.clone()),
                ("/responsePayload", handled(&event)),
            ],
        );
    }
}

/// RULE: an update naming ONE destination slot redirects that slot and leaves the other
/// exactly as it stands, so the next success is recorded at the new on-success destination and
/// the next failure still at the on-failure destination the put named.
#[tokio::test]
async fn an_update_redirects_one_slot_and_leaves_the_other() {
    let case = "an update naming only the on-success slot";
    let function = "redirected-on-success";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    let (first_url, first_arn) = given_queue(&router, case, "redirect-first-success").await;
    let (second_url, second_arn) = given_queue(&router, case, "redirect-second-success").await;
    let (failure_url, failure_arn) = given_queue(&router, case, "redirect-failure").await;
    given_function(&router, &engine, case, function, None).await;
    put_destinations(
        &router,
        case,
        function,
        None,
        Some(&first_arn),
        Some(&failure_arn),
    )
    .await;
    update_config(
        &router,
        case,
        function,
        None,
        &json!({ "DestinationConfig": { "OnSuccess": { "Destination": second_arn } } }),
    )
    .await;

    // The success: its record is owed to the destination the UPDATE named, and the arrival of
    // it there is what makes the two reads below a check on a delivery already made.
    let returning = returning_event(function);
    invoke_async(&router, case, function, None, &returning).await;
    let held = awaited(
        &router,
        case,
        "the record of the successful event",
        &second_url,
    )
    .await;
    let record = one_record(case, "the on-success destination the update named", &held);
    record_states(
        case,
        &record,
        &[
            ("/requestContext/condition", json!(CONDITION_SUCCESS)),
            ("/requestPayload", returning.clone()),
            ("/responsePayload", handled(&returning)),
        ],
    );
    held_nothing(
        &router,
        case,
        "the on-success destination the update replaced",
        &first_url,
    )
    .await;
    held_nothing(
        &router,
        case,
        "the on-failure destination, which no success is recorded at",
        &failure_url,
    )
    .await;

    // The failure: the slot the update said nothing about still carries the destination the
    // put named.
    let failing = failing_event(function);
    invoke_async(&router, case, function, None, &failing).await;
    let held = awaited(
        &router,
        case,
        "the record of the failed event",
        &failure_url,
    )
    .await;
    let record = one_record(case, "the on-failure destination the put named", &held);
    record_states(
        case,
        &record,
        &[
            (
                "/requestContext/condition",
                json!(CONDITION_RETRIES_EXHAUSTED),
            ),
            ("/requestPayload", failing.clone()),
            ("/responseContext/functionError", json!(UNHANDLED)),
        ],
    );
    held_nothing(
        &router,
        case,
        "the on-success destination the update named",
        &second_url,
    )
    .await;
    held_nothing(
        &router,
        case,
        "the on-success destination the update replaced",
        &first_url,
    )
    .await;
}

/// RULE: the record of an event addressed to an ALIAS reports the version the alias resolves
/// to at the instant the event is invoked, so a repoint is what the next event's record names
/// — while the destination stays the alias's own configuration's.
#[tokio::test]
async fn an_alias_record_reports_the_version_the_alias_points_at() {
    let function = "repointed-alias";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    let first_case = "while the alias points at the first version";
    let (record_url, record_arn) = given_queue(&router, first_case, "repoint-records").await;
    given_function(&router, &engine, first_case, function, None).await;
    let first = published_version(&router, first_case, function, "the first version").await;
    let second = published_version(&router, first_case, function, "the second version").await;
    assert_eq!(
        (first.as_str(), second.as_str()),
        (FIRST_VERSION, SECOND_VERSION),
        "case {first_case:?}: expected the two publishes to mint versions {FIRST_VERSION} and \
         {SECOND_VERSION}, got {first:?} and {second:?}"
    );
    given_alias(&router, first_case, function, ALIAS, FIRST_VERSION).await;
    put_destinations(
        &router,
        first_case,
        function,
        Some(ALIAS),
        Some(&record_arn),
        None,
    )
    .await;

    // (the case, the version the alias points at while its event is invoked)
    let legs: &[(&str, &str)] = &[
        (first_case, FIRST_VERSION),
        ("after the alias was repointed", SECOND_VERSION),
    ];
    for (case, points_at) in legs {
        if *points_at != FIRST_VERSION {
            repointed_alias(&router, case, function, ALIAS, points_at).await;
        }
        let event = returning_event(points_at);
        invoke_async(&router, case, function, Some(ALIAS), &event).await;
        let held = awaited(&router, case, "the invocation record", &record_url).await;
        let record = one_record(case, "the alias's own destination", &held);
        record_states(
            case,
            &record,
            &[
                (
                    "/requestContext/functionArn",
                    json!(format!("{}:{ALIAS}", function_arn(function))),
                ),
                ("/requestPayload", event.clone()),
                ("/responseContext/executedVersion", json!(*points_at)),
            ],
        );
    }
}
