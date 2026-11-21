// SPDX-License-Identifier: CC0-1.0

//! WHICH values a member whose `@pattern` alternates whole-value branches admits, end to end
//! over HTTP.
//!
//! The rule under test: a `@pattern` whose top-level ALTERNATION is wrapped in the whole-value
//! anchor pair is satisfied only by a value one of its branches matches END TO END, so this
//! model's "an ARN, or nothing at all" expression
//! (`smithy_query(query="shape", shape="ResourceArn,KMSKeyArn")`) admits the empty value and
//! an ARN and nothing else. A request carrying any other value is a CLIENT-side validation
//! failure: `400 ValidationException` whose clause names the member and quotes the expression
//! the service publishes — answered on the REQUEST, before anything is read or written, so it
//! never depends on what the addressed resource's state happens to be and is never reported as
//! a conflict a retry could clear.
//!
//! The rows span what the rule generalises over: every operation of this service that accepts
//! such a member and every member it accepts (at the input's top level and nested inside a
//! structure), the ways a value falls outside the alternation (no ARN at all, an ARN prefix
//! with too few segments, a value that merely CONTAINS an ARN, a partition or a vendor
//! namespace the branch does not admit), the ways one satisfies it (nothing at all, each
//! partition form, a vendor namespace built from each character class the branch names), and
//! a request that breaks the constraint in two members at once.
//!
//! Every row drives the real operations over HTTP and reads what was admitted back through the
//! real `GetFunctionConfiguration`, so a refusal that nevertheless stored something, or an
//! admitted value that was not persisted, fails.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service's signing name (`lambda`), which is what
/// routes a request here. Nothing in it is verified: the harness turns request
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// The wire error code a client-side validation failure carries.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `CreateEventSourceMapping` is bound.
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The expression the refusal quotes: the model's `@pattern` without the whole-value anchor
/// pair, which is how every `@pattern` requirement of this service is stated.
const EXPRESSION: &str = "(arn:(aws[a-zA-Z-]*)?:[a-z0-9-.]+:.*)|()";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "expected a JSON body, got {error}: {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// The wire error code the body names, or the empty string for a success.
    fn error_code(&self) -> String {
        self.json()
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `message` the refusal carries, or the empty string when it carries none.
    fn message(&self) -> String {
        let body = self.json();
        body.get("message")
            .or_else(|| body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The body text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
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

/// The clause a value outside the expression is refused with, at the member `path`.
fn clause(value: &str, path: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {EXPRESSION}"
    )
}

/// The `Code.ZipFile` member of a create: arbitrary bytes, of which only the length and the
/// digest are ever reported.
fn zip_file() -> Value {
    let bytes: Vec<u8> = (0..64_u32)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect();
    json!(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Move every stored function's activation instant into the past, so the next read
/// materialises the `Pending` -> `Active` transition the clock owes. This is what waiting
/// would do, without waiting.
fn make_activation_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// Move every in-flight update's settle instant into the past, so the next read materialises
/// the `InProgress` -> `Successful` transition the clock owes — which is what lets the next
/// row of a table update the same function.
fn make_update_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        if function.last_update_due.is_some() {
            function.last_update_due = Some(past);
        }
    }
}

/// Create one settled `Zip`-package function named `name`, through the real `CreateFunction`.
async fn given_function(
    engine: &Arc<engine::InMemoryEngine>,
    router: &MultiServiceRouter,
    name: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file() },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "the function {name:?} must be created, got {} {}",
        created.status,
        created.text()
    );
    make_activation_due(engine);
}

/// `GetFunctionConfiguration` on the unpublished configuration of `name`.
async fn configuration(router: &MultiServiceRouter, name: &str) -> Value {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "the configuration of {name:?} must be readable, got {} {}",
        read.status,
        read.text()
    );
    read.json()
}

/// One refused request: what it gets wrong, the call, and the clause it must be refused with.
struct Refused {
    label: &'static str,
    method: &'static str,
    uri: String,
    body: Value,
    expected: String,
}

/// The table of refusals, over the function `name` that already exists.
fn refusals(name: &str) -> Vec<Refused> {
    let configuration_uri = format!("{FUNCTIONS_URI}/{name}/configuration");
    let code_uri = format!("{FUNCTIONS_URI}/{name}/code");
    vec![
        // The nested member of the operation the rule was reported against, with a value
        // that is no ARN at all.
        Refused {
            label: "a dead-letter target that is no ARN at all, on an update",
            method: "PUT",
            uri: configuration_uri.clone(),
            body: json!({ "DeadLetterConfig": { "TargetArn": "not-an-arn" } }),
            expected: clause("not-an-arn", "deadLetterConfig.targetArn"),
        },
        // A value that CONTAINS an ARN: the branch is matched against the whole value, so
        // "ends with something a branch matches" is not enough.
        Refused {
            label: "a dead-letter target that merely contains an ARN",
            method: "PUT",
            uri: configuration_uri.clone(),
            body: json!({
                "DeadLetterConfig": {
                    "TargetArn": "queue arn:aws:sqs:us-east-1:000000000000:failed",
                },
            }),
            expected: clause(
                "queue arn:aws:sqs:us-east-1:000000000000:failed",
                "deadLetterConfig.targetArn",
            ),
        },
        // An ARN prefix with too few segments: the branch names a partition, a vendor
        // namespace and a remainder.
        Refused {
            label: "a dead-letter target with no remainder at all",
            method: "PUT",
            uri: configuration_uri.clone(),
            body: json!({ "DeadLetterConfig": { "TargetArn": "arn:aws:sqs" } }),
            expected: clause("arn:aws:sqs", "deadLetterConfig.targetArn"),
        },
        // The member at the input's TOP LEVEL, with a partition the branch does not admit.
        Refused {
            label: "an environment key of a partition the branch does not admit, on an update",
            method: "PUT",
            uri: configuration_uri.clone(),
            body: json!({ "KMSKeyArn": "arn:gcp:kms:us-east-1:000000000000:key/0" }),
            expected: clause("arn:gcp:kms:us-east-1:000000000000:key/0", "kMSKeyArn"),
        },
        // The same member on the CREATE, with a vendor namespace the branch does not admit.
        Refused {
            label: "an environment key whose vendor namespace is uppercase, on a create",
            method: "POST",
            uri: FUNCTIONS_URI.to_string(),
            body: json!({
                "FunctionName": "arn-form-rejected",
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": { "ZipFile": zip_file() },
                "KMSKeyArn": "arn:aws:KMS:us-east-1:000000000000:key/0",
            }),
            expected: clause("arn:aws:KMS:us-east-1:000000000000:key/0", "kMSKeyArn"),
        },
        // The member nested inside the durable configuration, on the create.
        Refused {
            label: "a durable execution's key, on a create",
            method: "POST",
            uri: FUNCTIONS_URI.to_string(),
            body: json!({
                "FunctionName": "arn-form-rejected",
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": { "ZipFile": zip_file() },
                "DurableConfig": { "KMSKeyArn": "key-1", "ExecutionTimeout": 900 },
            }),
            expected: clause("key-1", "durableConfig.kMSKeyArn"),
        },
        // The member nested inside the package of a create.
        Refused {
            label: "the key a package is encrypted under, on a create",
            method: "POST",
            uri: FUNCTIONS_URI.to_string(),
            body: json!({
                "FunctionName": "arn-form-rejected",
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": { "ZipFile": zip_file(), "SourceKMSKeyArn": "arn:" },
            }),
            expected: clause("arn:", "code.sourceKMSKeyArn"),
        },
        // The same member at the top level of the code update.
        Refused {
            label: "the key a package is encrypted under, on a code update",
            method: "PUT",
            uri: code_uri,
            body: json!({ "ZipFile": zip_file(), "SourceKMSKeyArn": "whatever" }),
            expected: clause("whatever", "sourceKMSKeyArn"),
        },
        // The mapping operations: the refusal is answered on the REQUEST, so a mapping
        // request that names an event source nothing holds is still refused for its key.
        Refused {
            label: "an event source mapping's key, on a create",
            method: "POST",
            uri: MAPPINGS_URI.to_string(),
            body: json!({
                "FunctionName": "no-such-function",
                "EventSourceArn": "arn:aws:sqs:us-east-1:000000000000:nothing",
                "KMSKeyArn": "kms-key",
            }),
            expected: clause("kms-key", "kMSKeyArn"),
        },
        Refused {
            label: "an event source mapping's key, on an update",
            method: "PUT",
            uri: format!("{MAPPINGS_URI}/11111111-2222-3333-4444-555555555555"),
            body: json!({ "KMSKeyArn": "kms-key" }),
            expected: clause("kms-key", "kMSKeyArn"),
        },
        // Two members outside the expression in ONE request: one clause per member, in one
        // envelope.
        Refused {
            label: "two members outside the expression at once",
            method: "PUT",
            uri: configuration_uri,
            body: json!({
                "KMSKeyArn": "key-1",
                "DeadLetterConfig": { "TargetArn": "topic-1" },
            }),
            expected: format!(
                "2 validation errors detected: Value 'key-1' at 'kMSKeyArn' failed to \
                 satisfy constraint: Member must satisfy regular expression pattern: \
                 {EXPRESSION}; Value 'topic-1' at 'deadLetterConfig.targetArn' failed to \
                 satisfy constraint: Member must satisfy regular expression pattern: \
                 {EXPRESSION}"
            ),
        },
    ]
}

/// RULE: a value the alternation does not match end to end is a 400 `ValidationException`
/// naming the member and the expression — for every operation and member that carries the
/// pattern — and the request it came in changes nothing.
#[tokio::test]
async fn a_value_outside_the_alternation_is_refused_as_a_validation_error() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&engine, &router, "arn-form-fn").await;

    // What the function holds before any of the refused requests, which none of them may
    // change.
    let before = configuration(&router, "arn-form-fn").await;

    for case in refusals("arn-form-fn") {
        let answer = call(&router, case.method, &case.uri, &case.body).await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {:?}: expected 400 {VALIDATION_EXCEPTION}, got {} {}",
            case.label,
            answer.status,
            answer.text()
        );
        let reported = answer.message();
        assert_eq!(
            reported, case.expected,
            "case {:?}: expected {:?}, got {reported:?}",
            case.label, case.expected
        );
        // A refused create stores no function at all.
        let absent = call(
            &router,
            "GET",
            &format!("{FUNCTIONS_URI}/arn-form-rejected/configuration"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            absent.status,
            404,
            "case {:?}: a refused create must store no function, got {} {}",
            case.label,
            absent.status,
            absent.text()
        );
    }

    // And a refused update left the stored configuration exactly as it was — including the
    // revision, which a change would have minted a new one of.
    let after = configuration(&router, "arn-form-fn").await;
    assert_eq!(
        after, before,
        "a refused request must change nothing, got {after} where it was {before}"
    );
}

/// RULE, the other side: every value one branch of the alternation matches end to end is
/// admitted, stored and reported back.
#[tokio::test]
async fn a_value_the_alternation_matches_is_stored_and_reported() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&engine, &router, "arn-form-admitted").await;

    // Each row: the case, and the dead-letter target and environment key it states — values
    // spanning the branch that matches an ARN, over each partition form and a vendor
    // namespace built from each character class the branch names.
    let rows: &[(&str, &str, &str)] = &[
        (
            "an ARN of the public partition",
            "arn:aws:sqs:us-east-1:000000000000:failed-events",
            "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555",
        ),
        (
            "an ARN whose partition carries letters and a hyphen",
            "arn:aws-us-gov:sns:us-gov-west-1:000000000000:topic",
            "arn:aws-us-gov:kms:us-gov-west-1:000000000000:key/0",
        ),
        (
            "an ARN with no partition at all, which the branch admits",
            "arn::sqs:us-east-1:000000000000:failed-events",
            "arn::kms:us-east-1:000000000000:key/0",
        ),
        (
            "a vendor namespace of lowercase letters, digits, dots and hyphens",
            "arn:aws:s3-outposts.9:us-east-1:000000000000:outpost/op-0",
            "arn:aws:payment-cryptography.1:us-east-1:000000000000:key/0",
        ),
    ];
    for (case, target, key) in rows {
        // The previous row's update settles first, so this one is not refused the conflict
        // that one opened.
        make_update_due(&engine);
        let updated = call(
            &router,
            "PUT",
            &format!("{FUNCTIONS_URI}/arn-form-admitted/configuration"),
            &json!({
                "DeadLetterConfig": { "TargetArn": target },
                "KMSKeyArn": key,
            }),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: expected the update to be served, got {} {}",
            updated.status,
            updated.text()
        );
        // Read back through the real reader, so an answer nothing persisted fails.
        let held = configuration(&router, "arn-form-admitted").await;
        assert_eq!(
            held.pointer("/DeadLetterConfig/TargetArn")
                .and_then(Value::as_str),
            Some(*target),
            "case {case:?}: expected the stored dead-letter target {target:?}, got {held}"
        );
        assert_eq!(
            held.get("KMSKeyArn").and_then(Value::as_str),
            Some(*key),
            "case {case:?}: expected the stored environment key {key:?}, got {held}"
        );
    }

    // The OTHER branch: the empty value is admitted too, and names no resource — which is
    // how a caller clears such a member.
    make_update_due(&engine);
    let cleared = call(
        &router,
        "PUT",
        &format!("{FUNCTIONS_URI}/arn-form-admitted/configuration"),
        &json!({ "DeadLetterConfig": { "TargetArn": "" }, "KMSKeyArn": "" }),
    )
    .await;
    assert_eq!(
        cleared.status,
        200,
        "the empty value must be admitted, got {} {}",
        cleared.status,
        cleared.text()
    );
    let held = configuration(&router, "arn-form-admitted").await;
    for member in ["/DeadLetterConfig/TargetArn", "/KMSKeyArn"] {
        let reported = held.pointer(member).and_then(Value::as_str);
        assert!(
            reported.is_none_or(str::is_empty),
            "expected {member} to name nothing after the empty value was stated, got \
             {reported:?} in {held}"
        );
    }

    // The same values on a CREATE, so the rule is not one operation's: the function is
    // created and reports what it was created with.
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": "arn-form-created",
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(), "SourceKMSKeyArn": "" },
            "DeadLetterConfig": { "TargetArn": "arn:aws:sqs:us-east-1:000000000000:failed" },
            "DurableConfig": { "KMSKeyArn": "arn:aws:kms:us-east-1:000000000000:key/0" },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "expected the create to be served, got {} {}",
        created.status,
        created.text()
    );
    let held = configuration(&router, "arn-form-created").await;
    assert_eq!(
        held.pointer("/DeadLetterConfig/TargetArn")
            .and_then(Value::as_str),
        Some("arn:aws:sqs:us-east-1:000000000000:failed"),
        "expected the created function to report the dead-letter target it was created \
         with, got {held}"
    );
    assert_eq!(
        held.pointer("/DurableConfig/KMSKeyArn").and_then(Value::as_str),
        Some("arn:aws:kms:us-east-1:000000000000:key/0"),
        "expected the created function to report the durable key it was created with, got \
         {held}"
    );
}

/// RULE, where it matters most: the clause is answered on the REQUEST, so a request that
/// could never be valid is refused as a validation error even while the function it names is
/// in a state that refuses every valid update.
#[tokio::test]
async fn the_validation_error_is_answered_before_any_state_conflict() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&engine, &router, "arn-form-busy").await;

    // One update puts the function into the window where "an update is in progress", which
    // is what every LATER update is refused with — a 409 a client retries.
    let first = call(
        &router,
        "PUT",
        &format!("{FUNCTIONS_URI}/arn-form-busy/configuration"),
        &json!({ "Description": "settling" }),
    )
    .await;
    assert_eq!(
        first.status,
        200,
        "expected the first update to be served, got {} {}",
        first.status,
        first.text()
    );
    // A VALID update is indeed refused as that conflict, which is what makes the row below
    // about the order of the two checks rather than about a function that is simply idle.
    let conflicting = call(
        &router,
        "PUT",
        &format!("{FUNCTIONS_URI}/arn-form-busy/configuration"),
        &json!({ "Description": "while the first is still in flight" }),
    )
    .await;
    assert_eq!(
        (conflicting.status, conflicting.error_code().as_str()),
        (409, "ResourceConflictException"),
        "expected a valid update of a function with one in flight to be the conflict, got \
         {} {}",
        conflicting.status,
        conflicting.text()
    );
    // And the request that could never be valid is the validation error, not that conflict.
    let refused = call(
        &router,
        "PUT",
        &format!("{FUNCTIONS_URI}/arn-form-busy/configuration"),
        &json!({ "DeadLetterConfig": { "TargetArn": "not-an-arn" } }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, VALIDATION_EXCEPTION),
        "expected 400 {VALIDATION_EXCEPTION} rather than the in-progress conflict, got {} {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.message(),
        clause("not-an-arn", "deadLetterConfig.targetArn"),
        "expected the clause to name the member and the expression, got {}",
        refused.text()
    );
}
