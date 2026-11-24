// SPDX-License-Identifier: CC0-1.0

//! That a deployed REST stage gated by a `TOKEN` Lambda authorizer ANSWERS every request it
//! accepts — including the ones it refuses, and including requests that are all in flight at
//! once — and that each answer is counted once in the stage's own series, read where a client
//! reads it: CloudWatch's `GetMetricStatistics`.
//!
//! RULE UNDER TEST, one level above the recorded case: **every request a client addresses to a
//! deployed stage of a REST API is ANSWERED, with the status and body the api's own records and
//! the policy the method's authorizer returned decide — a request is never left open — and the
//! decision is taken without holding this service's store across the calls it makes, so a
//! request answered while nothing else is in flight is answered exactly the same when several
//! arrive at once. Every answered request, the refused ones included, is then published once in
//! the `AWS/ApiGateway` namespace under `{ApiName, Stage}`: one observation of `Count`, one of
//! `4XXError` and one of `5XXError` — 1 for a request whose status is in that class and 0 for
//! one that is not — and one `Latency` sample, so the `403` an authorizer's explicit deny earns
//! is a client error of the stage and no server error at all. The functions behind it run once
//! per request that reached them and no more: the authorizer's for every request presenting its
//! identity source, the integration's only for a request its policy admitted.**
//!
//! A hang is the one answer no client can act on: an SDK call does not fail fast, it blocks
//! until the caller's own socket timeout, so no retry and no error path is ever reached. Every
//! request below is therefore asserted to be answered within [`ANSWER_DEADLINE`] — generous,
//! because what is pinned is that an answer arrives at all and not how fast this host is — and
//! the metric read that follows it is asserted to be answered too, since the series this stage
//! publishes are the only record a client has that the traffic happened.
//!
//! `Method$authorizationType` gives `CUSTOM` "for using a custom authorizer" and
//! `Authorizer$authorizerResultTtlInSeconds` "if it equals 0, authorization caching is
//! disabled", so every request's own token reaches the function and each answer is the policy
//! that function returned for it; `Authorizer$identitySource` names the request value a `TOKEN`
//! authorizer is handed, so a request carrying nothing there has no token to be decided on. The
//! metric vocabulary — the namespace, the dimension set `{ApiName, Stage}` and the metrics
//! `Count`, `4XXError`, `5XXError` and `Latency` — is API Gateway's published
//! metrics-and-dimensions reference, since no modelled operation expresses metric emission.
//!
//! The rows differ along what the rule generalises over: WHAT each request presents at the
//! identity source (a token the returned policy allows, two different tokens it denies, and
//! nothing at all), whether the row's requests arrive ONE AFTER ANOTHER or ALL AT ONCE, whether
//! one batch in flight mixes an admitted request with refused ones, and HOW MANY requests each
//! row drives — a different number per row, so no row's count can stand in for another's.
//!
//! Four crates have to agree for that to hold — CloudFormation archives the inline handlers
//! into packages this emulator can run, Lambda runs the authorizer and the integration and
//! publishes what each was invoked for, API Gateway resolves the stage, takes the decision and
//! publishes what the stage answered, and the store every datapoint lands in is CloudWatch's
//! own — so the suite lives here, on the emulator as it is ASSEMBLED.
//!
//! What the neighbouring assembled-router suites pin and this one does not repeat:
//! [`crate::apigateway_authorizer_deny_counts_as_a_client_error`] pins the CLASSIFICATION of
//! one refusal per request class driven one request at a time,
//! [`crate::apigateway_repeated_stage_requests`] pins that repeated requests to an UNGATED
//! stage each run the integration, and
//! [`crate::apigateway_token_authorizer_deny_and_access_log`] pins the authorizer's decision
//! and the access-log record of it. What only this suite reaches is the gated plane under
//! requests IN FLIGHT TOGETHER: that each of them is answered its own answer, that none of them
//! is left open, and that the stage's counters and both functions' invocation counts come out
//! at exactly one per request afterwards.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling suites do.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region of both functions and of the
/// api's own invoke host.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which a grant's `SourceArn` carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that archives both inline handlers, the status it must reach, the role both
/// functions run as and the name of its one inline document.
const STACK: &str = "xsvc-gated-stage-liveness-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const EXECUTION_ROLE: &str = "xsvc-gated-stage-liveness-runner";
const LOGGING_POLICY: &str = "logging";

/// The two functions: the one the authorizer names, and the one behind the method.
const AUTHORIZER_FUNCTION: &str = "xsvc-gated-stage-liveness-gate";
const BACKEND_FUNCTION: &str = "xsvc-gated-stage-liveness-backend";

/// The api under test, the stage it is deployed to and the resource path of its one method.
const API_UNDER_TEST: &str = "gated-stage-liveness-api";
const STAGE_UNDER_TEST: &str = "live";
const RESOURCE_PATH: &str = "ok";

/// The verb the method is deployed under, and so the verb every request below carries.
const VERB: &str = "GET";

/// The header the authorizer's `identitySource` names, and the tokens the rows present at it:
/// the one the returned policy allows, and two it denies — a second denying spelling, so no row
/// stands for the whole denied class on one value.
const TOKEN_HEADER: &str = "Authorization";
const ALLOWING_TOKEN: &str = "allow";
const DENYING_TOKEN: &str = "deny";
const OTHER_DENYING_TOKEN: &str = "some-other-token";

/// The body the method's own function answers an admitted request with.
const BACKEND_BODY: &str = "GATED-200";

/// The sentences the two refusals carry, and the class an explicit deny names in
/// `x-amzn-errortype`.
const EXPLICIT_DENY: &str = "User is not authorized to access this resource with an explicit \
                             deny in an identity-based policy";
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), which is
/// what routes a call to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace API Gateway publishes what its stages answered in, and the dimensions one of
/// these series is identified by.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_NAME: &str = "ApiName";
const STAGE: &str = "Stage";

/// The counters and the measurement one answered request is observed in.
const COUNT: &str = "Count";
const CLIENT_ERRORS: &str = "4XXError";
const SERVER_ERRORS: &str = "5XXError";
const LATENCY: &str = "Latency";

/// The series that says what RAN: how many invocations Lambda served of one function.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
const FUNCTION_NAME: &str = "FunctionName";
const INVOCATIONS: &str = "Invocations";

/// The units those series carry: a bare count of requests, and a measured duration.
const COUNT_UNIT: &str = "Count";
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The period the series are read at, in seconds, and how far either side of the traffic the
/// read window reaches: generous, because the only thing that decides which window a datapoint
/// falls in is the instant the request was answered at.
const PERIOD: i64 = 60;
const WINDOW_MARGIN_PERIODS: f64 = 2.0;

/// How long one invocation is given, as each function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the answering, not how fast an interpreter
/// gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work another service owes — a stack reaching `CREATE_COMPLETE`, a
/// function reaching `Active`, a published datapoint becoming readable — and how long between
/// two looks. Generous on purpose: everything waited for happens in this process, so a loaded
/// host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long ONE request to the deployed stage is given to be answered. The authorizer's
/// invocation and the integration's are each bounded by this plane's own timeouts, so a request
/// still open after this was never going to be answered at all — which is the symptom this
/// suite exists to pin, so it is asserted rather than waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// How many requests an all-at-once row has in flight together.
const AT_ONCE: usize = 3;

/// The authorizer's function: it decides from the token it was handed, naming the method the
/// call addressed as the policy's resource, exactly as AWS's own Lambda-authorizer blueprint
/// does.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    token = event.get("authorizationToken") or ""
    effect = "Allow" if token == "allow" else "Deny"
    return {
        "principalId": "principal-" + token,
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Action": "execute-api:Invoke",
                    "Effect": effect,
                    "Resource": event["methodArn"],
                }
            ],
        },
    }
"#;

/// The function behind the method: it answers the proxy envelope every admitted request is
/// served with.
const BACKEND_HANDLER: &str = r#"
def handler(event, context):
    return {"statusCode": 200, "body": "GATED-200"}
"#;

/// One answered request: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The sentence a refused answer carries: a gateway response words it under `message` or
    /// under `Message` depending on which response it is, so both spellings are read.
    fn message(&self) -> String {
        let body = self.json();
        for name in ["message", "Message"] {
            if let Some(message) = body.get(name).and_then(Value::as_str) {
                return message.to_string();
            }
        }
        self.text()
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine: both functions run on the Lambda this router
/// mounts, and every datapoint is read from the CloudWatch it mounts.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: the credential scope
    // is what routes an SDK request, and an invoke of the deployed stage carries none at all,
    // as a client of that URL sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`. The signature itself is
/// never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One awsQuery call of `service` that has to be accepted, answered with its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &format!("{service}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(service)),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: {service} {action} must answer 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status,
        expected,
        "setup {what:?}: must answer HTTP {expected}, got {}",
        answered.summary()
    );
    answered.json()
}

/// One CloudWatch call over its `awsJson1_0` wire that has to answer 200 — the read a client
/// learns about this traffic by, which has to be answered as surely as the traffic itself.
async fn monitoring(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let answered = dispatch(
        router,
        "POST",
        &format!("{CLOUDWATCH_SIGNING_NAME}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization(CLOUDWATCH_SIGNING_NAME)),
            (
                "x-amz-target",
                &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
            ),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: CloudWatch {operation} must answer 200, got {}",
        answered.summary()
    );
    answered.json()
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// Create one role trusting `principal`, carrying the log writes a function's own delivery
/// needs. Answers the ARN IAM holds it under.
async fn create_role(router: &MultiServiceRouter, name: &str, principal: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let role_arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name:?}) must answer an ARN"));
    let logging = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    accepted_query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", LOGGING_POLICY),
            ("PolicyDocument", &logging),
        ],
    )
    .await;
    role_arn
}

/// Deploy the stack declaring both functions: the inline sources are archived into deployment
/// packages by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for each, once it reports both `Active`.
async fn deploy_functions(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let declared = |name: &str, source: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role_arn,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": source },
            },
        })
    };
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Gate": declared(AUTHORIZER_FUNCTION, AUTHORIZER_HANDLER),
            "Backend": declared(BACKEND_FUNCTION, BACKEND_HANDLER),
        },
    })
    .to_string();
    accepted_query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described}"
    );
    (
        active_function_arn(router, AUTHORIZER_FUNCTION).await,
        active_function_arn(router, BACKEND_FUNCTION).await,
    )
}

/// The ARN Lambda reports for `name`, once it reports it `Active` — a transition that service
/// owes, so this looks until it reports it rather than sleeping a span on its behalf.
async fn active_function_arn(router: &MultiServiceRouter, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    let configuration = loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await
        .json();
        if reported.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup: Lambda must report {name:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Grant `apigateway.amazonaws.com` the invocation of `name`, scoped to the methods and the
/// authorizers of `api` — the `aws:SourceArn` such a grant is narrowed by, as a client scripts
/// it.
async fn grant(router: &MultiServiceRouter, name: &str, statement: &str, api: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        Some(json!({
            "StatementId": statement,
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/*"),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission({name:?}) must answer 201, got {}",
        granted.summary()
    );
}

/// One series as a reader names it: the namespace, the metric, and the dimensions that identify
/// it together.
struct Series {
    namespace: &'static str,
    metric: &'static str,
    dimensions: Vec<(&'static str, String)>,
}

impl Series {
    /// One STAGE's own series: the requests that stage answered.
    fn of_stage(api_name: &str, metric: &'static str) -> Self {
        Self {
            namespace: NAMESPACE,
            metric,
            dimensions: vec![
                (API_NAME, api_name.to_string()),
                (STAGE, STAGE_UNDER_TEST.to_string()),
            ],
        }
    }

    /// The series of what Lambda was invoked for on one function.
    fn of_function(function: &str) -> Self {
        Self {
            namespace: LAMBDA_NAMESPACE,
            metric: INVOCATIONS,
            dimensions: vec![(FUNCTION_NAME, function.to_string())],
        }
    }

    fn wire(&self) -> Value {
        Value::Array(
            self.dimensions
                .iter()
                .map(|(name, value)| json!({ "Name": name, "Value": value }))
                .collect(),
        )
    }
}

/// What one series holds over the window: how many datapoints a client reads, what they sum to,
/// how many observations they aggregate, and which units they carry.
#[derive(Debug, Default)]
struct Held {
    datapoints: usize,
    sum: f64,
    samples: f64,
    units: BTreeSet<String>,
}

/// Wall-clock now as epoch seconds, the clock the read windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Read ONE series over the whole window and fold what it holds.
///
/// `since` is the instant the suite started driving traffic, taken before the first request: the
/// window reaches from a margin before it to a margin past now, so every datapoint the traffic
/// produced is inside it however long the traffic took on this machine.
async fn held(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) -> Held {
    let margin = WINDOW_MARGIN_PERIODS * PERIOD as f64;
    let answered = monitoring(
        router,
        case,
        "GetMetricStatistics",
        json!({
            "Namespace": series.namespace,
            "MetricName": series.metric,
            "Dimensions": series.wire(),
            "StartTime": since - margin,
            "EndTime": now_secs() + margin,
            "Period": PERIOD,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    let mut folded = Held::default();
    for datapoint in answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        folded.datapoints += 1;
        folded.sum += datapoint["Sum"].as_f64().unwrap_or_default();
        folded.samples += datapoint["SampleCount"].as_f64().unwrap_or_default();
        if let Some(unit) = datapoint["Unit"].as_str() {
            folded.units.insert(unit.to_string());
        }
    }
    folded
}

/// Read `series` until it holds `samples` observations, and answer what it then holds.
///
/// The wait is on the CONDITION — a datapoint the publication owes becoming readable — so it
/// looks again until it holds or [`BUDGET`] passes, and the assertions are made on whatever the
/// last read saw.
async fn awaited(
    router: &MultiServiceRouter,
    case: &str,
    series: &Series,
    since: f64,
    samples: f64,
) -> Held {
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = held(router, case, series, since).await;
        if read.samples >= samples || Instant::now() >= deadline {
            return read;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Assert `series` holds exactly `sum` over `samples` observations, each carrying `unit`, and
/// that a client reads at least one datapoint of it: a counter reading zero is a series that
/// EXISTS and reads zero, which is what tells "nothing of that class happened" from "this stage
/// publishes nothing at all".
async fn assert_series(
    router: &MultiServiceRouter,
    case: &str,
    series: &Series,
    since: f64,
    sum: f64,
    samples: f64,
    unit: &str,
) {
    let read = awaited(router, case, series, since, samples).await;
    assert!(
        read.datapoints > 0,
        "case {case:?}: expected the {} series to exist at all, got {read:?}",
        series.metric
    );
    assert_eq!(
        read.sum, sum,
        "case {case:?}: expected the {} series to sum to {sum}, got {read:?}",
        series.metric
    );
    assert_eq!(
        read.samples, samples,
        "case {case:?}: expected {samples} observations of {}, got {read:?}",
        series.metric
    );
    assert_eq!(
        read.units,
        BTreeSet::from([unit.to_string()]),
        "case {case:?}: expected every datapoint of {} to carry the unit {unit:?}, got {read:?}",
        series.metric
    );
}

/// What ONE request presents at the authorizer's identity source.
#[derive(Debug, Clone, Copy)]
enum Presented {
    /// The identity source's header carries this value.
    Token(&'static str),
    /// The request carries nothing at the identity source at all.
    Nothing,
}

/// How a row's requests arrive at the stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// Each request is sent once the one before it has been answered.
    OneAfterAnother,
    /// Every request of the row is in flight together.
    AllAtOnce,
}

/// What the stage owes ONE request, out of what that request presented.
#[derive(Debug)]
struct Owed {
    status: u16,
    /// The sentence a refusal carries, or `None` for a request the integration serves — whose
    /// body is the function's own.
    message: Option<&'static str>,
    /// The class a refusal names in `x-amzn-errortype`, where the rule fixes one.
    error_type: Option<&'static str>,
    /// Whether the authorizer's own function is invoked for such a request: a `TOKEN` authorizer
    /// is handed the identity source's value and nothing else, so a request carrying none never
    /// reaches it.
    reaches_the_authorizer: bool,
    /// Whether the method's integration is performed for such a request.
    reaches_the_integration: bool,
}

/// What the stage owes a request presenting `presented`, from the rule alone: the policy the
/// function returns allows the invoked method for the one allowing token and denies it for every
/// other, and a request presenting nothing at the identity source has no token to decide on.
fn owed(presented: Presented) -> Owed {
    match presented {
        Presented::Token(token) if token == ALLOWING_TOKEN => Owed {
            status: 200,
            message: None,
            error_type: None,
            reaches_the_authorizer: true,
            reaches_the_integration: true,
        },
        Presented::Token(_) => Owed {
            status: 403,
            message: Some(EXPLICIT_DENY),
            error_type: Some(ACCESS_DENIED_EXCEPTION),
            reaches_the_authorizer: true,
            reaches_the_integration: false,
        },
        Presented::Nothing => Owed {
            status: 401,
            message: Some(UNAUTHORIZED_MESSAGE),
            error_type: None,
            reaches_the_authorizer: false,
            reaches_the_integration: false,
        },
    }
}

/// One row: the requests it drives, in order, and whether they arrive one after another or all
/// at once.
struct Row {
    case: &'static str,
    presented: &'static [Presented],
    arrival: Arrival,
}

/// The table the rule is asserted over.
static ROWS: [Row; 5] = [
    Row {
        case: "admitted requests, one after another",
        presented: &[
            Presented::Token(ALLOWING_TOKEN),
            Presented::Token(ALLOWING_TOKEN),
        ],
        arrival: Arrival::OneAfterAnother,
    },
    Row {
        case: "denied requests, one after another, each presenting its own token",
        presented: &[
            Presented::Token(DENYING_TOKEN),
            Presented::Token(OTHER_DENYING_TOKEN),
            Presented::Token(DENYING_TOKEN),
        ],
        arrival: Arrival::OneAfterAnother,
    },
    Row {
        case: "an admitted request in flight beside two refused ones",
        presented: &[
            Presented::Token(ALLOWING_TOKEN),
            Presented::Token(DENYING_TOKEN),
            Presented::Nothing,
        ],
        arrival: Arrival::AllAtOnce,
    },
    Row {
        case: "three denied requests in flight at once",
        presented: &[
            Presented::Token(DENYING_TOKEN),
            Presented::Token(OTHER_DENYING_TOKEN),
            Presented::Token(DENYING_TOKEN),
        ],
        arrival: Arrival::AllAtOnce,
    },
    Row {
        case: "a request carrying nothing at the identity source",
        presented: &[Presented::Nothing],
        arrival: Arrival::OneAfterAnother,
    },
];

/// How many requests of the whole table are owed an answer satisfying `which`.
fn requests_where(which: impl Fn(&Owed) -> bool) -> f64 {
    ROWS.iter()
        .flat_map(|row| row.presented.iter())
        .map(|presented| owed(*presented))
        .filter(|owed| which(owed))
        .count() as f64
}

/// One request to the stage, answered WITHIN [`ANSWER_DEADLINE`]: a stage that leaves the
/// request open rather than answering it fails here instead of hanging.
async fn answer_within(
    router: &MultiServiceRouter,
    case: &str,
    index: usize,
    host: &str,
    path: &str,
    presented: Presented,
) -> Answered {
    let headers: Vec<(&str, &str)> = match presented {
        Presented::Token(token) => vec![(TOKEN_HEADER, token)],
        Presented::Nothing => Vec::new(),
    };
    let request = dispatch(router, VERB, host, path, &headers, Bytes::new());
    match ::tokio::time::timeout(ANSWER_DEADLINE, request).await {
        Ok(answered) => answered,
        Err(_elapsed) => panic!(
            "case {case:?}: request {index} presenting {presented:?} to {host}{path} was never \
             answered within {ANSWER_DEADLINE:?}"
        ),
    }
}

/// Assert one answer is what the stage owed the request that earned it.
fn assert_answer(case: &str, index: usize, presented: Presented, answered: &Answered) {
    let owed = owed(presented);
    assert_eq!(
        answered.status, owed.status,
        "case {case:?}: request {index} presenting {presented:?} expected HTTP {}, got {}",
        owed.status,
        answered.summary()
    );
    match owed.message {
        Some(sentence) => assert_eq!(
            answered.message(),
            sentence,
            "case {case:?}: request {index} presenting {presented:?} expected the refusal \
             {sentence:?}, got {}",
            answered.summary()
        ),
        None => assert_eq!(
            answered.text(),
            BACKEND_BODY,
            "case {case:?}: request {index} presenting {presented:?} expected the integration's \
             own answer, got {}",
            answered.summary()
        ),
    }
    if let Some(class) = owed.error_type {
        assert_eq!(
            answered.header(ERROR_TYPE_HEADER).as_deref(),
            Some(class),
            "case {case:?}: request {index} presenting {presented:?} expected \
             {ERROR_TYPE_HEADER:?} to name {class:?}, got {:?}",
            answered.header(ERROR_TYPE_HEADER)
        );
    }
}

/// RULE: every request a gated stage accepts is ANSWERED what the api's records and the
/// authorizer's policy decide, whether the requests arrive one at a time or all together, and
/// each of them is counted once in the stage's own series — the refusals as client errors and
/// nothing as a server error.
#[test]
fn every_request_a_gated_stage_accepts_is_answered_and_counted_once() {
    std::thread::Builder::new()
        .name("apigateway-gated-stage-liveness".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on (`core/router/src/main.rs`),
            // because one invoke here drives two nested in-process calls on it — the
            // authorizer's invocation and the integration's — and the rows in flight together
            // drive several of those at once.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a multi-thread runtime");
            runtime.block_on(suite());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}

async fn suite() {
    let router = emulator();

    // 1. The role and the two functions.
    let execution_role = create_role(&router, EXECUTION_ROLE, "lambda.amazonaws.com").await;
    let (authorizer_arn, backend_arn) = deploy_functions(&router, &execution_role).await;

    // 2. The api, exactly as a CLI deployment builds one: the resource, the TOKEN authorizer,
    //    the gated method, the proxy integration, the grants and the deployment that mints the
    //    stage.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": API_UNDER_TEST })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &api_record, "id");
    let api_name = as_str("CreateRestApi", &api_record, "name");
    let root = as_str("CreateRestApi", &api_record, "rootResourceId");
    let resource = apigw(
        &router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource_id = as_str("CreateResource", &resource, "id");
    let authorizer = apigw(
        &router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "token-gate",
            "type": "TOKEN",
            "identitySource": format!("method.request.header.{TOKEN_HEADER}"),
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{authorizer_arn}/\
                 invocations"
            ),
            // No result is held for any identity, so every request's own token reaches the
            // function — including the ones in flight together.
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
    grant(&router, AUTHORIZER_FUNCTION, "apigw-authorizer", &api).await;
    grant(&router, BACKEND_FUNCTION, "apigw-backend", &api).await;
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/{VERB}");
    apigw(
        &router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer_id })),
        201,
    )
    .await;
    apigw(
        &router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{backend_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE_UNDER_TEST })),
        201,
    )
    .await;

    // 3. The traffic, taken from the table: every request is answered, and every answer is the
    //    one the stage owed THAT request — the row in flight together included, where a plane
    //    that can only answer while nothing else is in flight never gets past the deadline.
    let since = now_secs();
    let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let invoke_path = format!("/{STAGE_UNDER_TEST}/{RESOURCE_PATH}");
    for row in &ROWS {
        let case = row.case;
        let answers: Vec<Answered> = match row.arrival {
            Arrival::OneAfterAnother => {
                let mut one_at_a_time: Vec<Answered> = Vec::new();
                for (index, presented) in row.presented.iter().enumerate() {
                    one_at_a_time.push(
                        answer_within(
                            &router,
                            case,
                            index,
                            &invoke_host,
                            &invoke_path,
                            *presented,
                        )
                        .await,
                    );
                }
                one_at_a_time
            }
            Arrival::AllAtOnce => {
                assert_eq!(
                    row.presented.len(),
                    AT_ONCE,
                    "case {case:?}: an all-at-once row has {AT_ONCE} requests in flight, not {}",
                    row.presented.len()
                );
                let (first, second, third) = ::tokio::join!(
                    answer_within(
                        &router,
                        case,
                        0,
                        &invoke_host,
                        &invoke_path,
                        row.presented[0]
                    ),
                    answer_within(
                        &router,
                        case,
                        1,
                        &invoke_host,
                        &invoke_path,
                        row.presented[1]
                    ),
                    answer_within(
                        &router,
                        case,
                        2,
                        &invoke_host,
                        &invoke_path,
                        row.presented[2]
                    ),
                );
                vec![first, second, third]
            }
        };
        for (index, answered) in answers.iter().enumerate() {
            assert_answer(case, index, row.presented[index], answered);
        }
    }

    // 4. What the stage published about all of it. Both counters are published for every
    //    answered request, so each series aggregates one observation per request and the SUM is
    //    how many of them were of that class: the refused requests are client errors, and
    //    nothing at all is a server error — read as a datapoint of zero rather than as a series
    //    that does not exist.
    let answered = requests_where(|_| true);
    let client_errors = requests_where(|owed| (400..500).contains(&owed.status));
    let server_errors = requests_where(|owed| (500..600).contains(&owed.status));
    for (metric, sum) in [
        (COUNT, answered),
        (CLIENT_ERRORS, client_errors),
        (SERVER_ERRORS, server_errors),
    ] {
        assert_series(
            &router,
            &format!("the stage's own series read as {metric}"),
            &Series::of_stage(&api_name, metric),
            since,
            sum,
            answered,
            COUNT_UNIT,
        )
        .await;
    }
    // One LATENCY sample per answered request, in milliseconds: the measurement is of answering
    // the request, so a refusal the stage worded itself is measured like a response an
    // integration produced.
    let case = format!("the stage's own series read as {LATENCY}");
    let read = awaited(
        &router,
        &case,
        &Series::of_stage(&api_name, LATENCY),
        since,
        answered,
    )
    .await;
    assert_eq!(
        read.samples, answered,
        "case {case:?}: expected {answered} {LATENCY} samples, got {read:?}"
    );
    assert_eq!(
        read.units,
        BTreeSet::from([MILLISECONDS_UNIT.to_string()]),
        "case {case:?}: expected every {LATENCY} datapoint to carry the unit \
         {MILLISECONDS_UNIT:?}, got {read:?}"
    );

    // 5. And what RAN, which is the other half of "answered once": the authorizer was invoked
    //    once per request that presented its identity source — no more for the requests in
    //    flight together — and the integration once per request the policy admitted, so the
    //    refusals counted above reached no integration at all.
    for (function, invocations) in [
        (
            AUTHORIZER_FUNCTION,
            requests_where(|owed| owed.reaches_the_authorizer),
        ),
        (
            BACKEND_FUNCTION,
            requests_where(|owed| owed.reaches_the_integration),
        ),
    ] {
        let case = format!("{INVOCATIONS} of {function}");
        let series = Series::of_function(function);
        let read = awaited(&router, &case, &series, since, invocations).await;
        assert_eq!(
            read.sum, invocations,
            "case {case:?}: expected {invocations} invocation(s), got {read:?}"
        );
    }
}
