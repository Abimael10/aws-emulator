// SPDX-License-Identifier: CC0-1.0

//! What a deployed HTTP api stage fronted by a `REQUEST` (Lambda) authorizer publishes about
//! the traffic it answered, read back where a client reads it — CloudWatch's own
//! `GetMetricStatistics`.
//!
//! RULE: **every request a ROUTE of a deployed HTTP api stage ANSWERED is published once in
//! API Gateway's own `AWS/ApiGateway` namespace under the stage-level dimension sets `{ApiId}`
//! and `{ApiId, Stage}`: one observation of `Count`, one of the lower-case `4xx` counter and
//! one of `5xx` — 1 for a request whose status is in that class and 0 for one that is not, so
//! both counters are published for EVERY answered request and each series' `Sum` is how many
//! requests of that class the stage served — beside one `Latency` sample of how long answering
//! it took. The route the stage matched is what attributes a request to those series and not
//! the answer it ended in, so a request the gateway refused ITSELF before the route's
//! integration was reached — the `403` an authorizer's deny earns, the `401` a request
//! presenting nothing at the authorizer's identity source earns — is counted exactly like one
//! the route's backend answered. `IntegrationLatency` is the one series that is not: it holds
//! one sample per request a BACKEND answered, so a refused request contributes none, which is
//! also readable in `AWS/Lambda` `Invocations` — the authorizer's function is invoked once per
//! request that presented its identity source, and the route's function once per request the
//! authorizer admitted.**
//!
//! `Authorizer$IdentitySource` states the precondition ("only when this is true does the
//! authorizer invoke the authorizer Lambda function. Otherwise, it returns a 401 Unauthorized
//! response without calling the Lambda function") and
//! `Authorizer$AuthorizerResultTtlInSeconds` the caching ("if it equals 0, authorization
//! caching is disabled"), so each request's own identity reaches the function and each answer
//! is that function's. Metric emission is expressed by no modelled operation at all, so the
//! vocabulary — the namespace, the dimension sets `{ApiId}` and `{ApiId, Stage}`, the
//! per-route set `{ApiId, Stage, Method, Resource}` that only a stage asking for
//! `RouteSettings$DetailedMetricsEnabled` publishes, and the metrics `Count`, `4xx`, `5xx`,
//! `Latency` and `IntegrationLatency` — is API Gateway's published metrics-and-dimensions
//! reference.
//!
//! Four crates have to agree for that to hold — CloudFormation archives the inline handlers
//! into packages this emulator can run, Lambda runs the authorizer and the route's backend and
//! publishes what each was invoked for, API Gateway v2 resolves the stage, takes the decision
//! and publishes what the stage answered, and the store every datapoint lands in is
//! CloudWatch's own — so the suite lives here, on the emulator as it is ASSEMBLED. Every
//! request is made at the address the api PUBLISHED and every count is read back through
//! CloudWatch's reader, so an observation that never reached that store cannot pass.
//!
//! The rows differ along what the rule generalises over: what the request presents at the
//! authorizer's identity source (a value the function admits, one it refuses, nothing at all),
//! the status class each of those is answered with, whether the authorizer's function and the
//! route's backend are reached at all, and HOW MANY requests each row drives — a different
//! number per row, so no row's count can stand in for another's and the totals cannot come out
//! right by accident.
//!
//! What the neighbouring suites pin and this one does not restate: which observations ONE
//! answered request contributes to which series, and which requests are counted in none of
//! them (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/`
//! `http_api_request_metrics.rs`); the form of the published address and the status classes a
//! route's own backend answers with
//! ([`super::apigatewayv2_published_endpoint_traffic_accounting`]); the whole identity-source
//! precondition of a `REQUEST` authorizer (that crate's `http_api_request_authorizer.rs`); and
//! the same classification for a REST api's stage
//! ([`super::apigateway_authorizer_deny_counts_as_a_client_error`]). What only this suite
//! reaches is a GATED HTTP api stage whose admitted and whose refused traffic are metered
//! together, in the counters a 4xx-rate dashboard and alarm are built on.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling suites do.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigatewayv2_published_endpoint_runs_its_lambda_route::announced_parts;
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region of both functions and of the
/// api's own invoke host.
const REGION: &str = "us-east-1";

/// The stack that archives both inline handlers, the status it must reach, the role both
/// functions run as and the name of its one inline document.
const STACK: &str = "apigwv2-gated-metrics-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const EXECUTION_ROLE: &str = "apigwv2-gated-metrics-runner";
const LOGGING_POLICY: &str = "logging";

/// The two functions: the one the api's authorizer names, and the one its route fronts.
const AUTHORIZER_FUNCTION: &str = "apigwv2-gated-metrics-gate";
const BACKEND_FUNCTION: &str = "apigwv2-gated-metrics-backend";

/// The api, the stage it is deployed to, and the route the stage serves.
const API_UNDER_TEST: &str = "apigwv2-gated-metrics-api";
const STAGE_UNDER_TEST: &str = "live";
const ROUTE_KEY: &str = "GET /orders";
const ROUTE_PATH: &str = "/orders";

/// The verb every request is made with, which is also the `Method` dimension a route's own
/// series would carry.
const VERB: &str = "GET";

/// The header the authorizer declares as its one identity source, and the two values the rows
/// present at it: the function below admits a value beginning with `allow` and refuses every
/// other one, so WHICH value was presented decides the answer.
const TOKEN_HEADER: &str = "authorization";
const ADMITTED_TOKEN: &str = "allow-this-caller";
const REFUSED_TOKEN: &str = "deny-this-caller";

/// What the route's backend answers, so a served request is told from a refused one by its
/// body alone.
const BACKEND_BODY: &str = "ROUTE-200";

/// The statuses and the sentences the three rows are answered with: the backend's own success,
/// the refusal a decision of deny earns, and the refusal a request presenting nothing at the
/// identity source earns without the function being invoked at all.
const SERVED: u16 = 200;
const FORBIDDEN: u16 = 403;
const FORBIDDEN_MESSAGE: &str = "Forbidden";
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The front door's error code for a request NO service claimed: a published invoke address
/// this emulator serves nothing at would read as this, never as a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), which is
/// what routes a call to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace API Gateway publishes what its apis answered in, and the dimensions one of
/// those series is identified by.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_ID: &str = "ApiId";
const STAGE: &str = "Stage";
const METHOD: &str = "Method";
const RESOURCE: &str = "Resource";

/// The counters and the measurements one answered request is observed in. Spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make this suite pass.
const COUNT: &str = "Count";
const CLIENT_ERRORS: &str = "4xx";
const SERVER_ERRORS: &str = "5xx";
const LATENCY: &str = "Latency";
const INTEGRATION_LATENCY: &str = "IntegrationLatency";

/// The namespace Lambda publishes its own work in, the dimension it is reported per, and the
/// series that counts the invocations a function was handed.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
const FUNCTION_NAME: &str = "FunctionName";
const INVOCATIONS: &str = "Invocations";

/// The units those series carry: a bare count of requests, and a measured duration.
const COUNT_UNIT: &str = "Count";
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The period the series are read at, in seconds, and how far either side of the traffic a
/// read reaches — generous, because what decides which window a datapoint falls in is the
/// instant the request was answered at, and the traffic takes as long as the machine takes.
const PERIOD: i64 = 60;
const WINDOW_MARGIN_PERIODS: f64 = 2.0;

/// How long one invocation is given, as each function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the metering, not how fast an interpreter
/// gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work another service owes — a stack reaching `CREATE_COMPLETE`, a
/// function reaching `Active`, a published datapoint becoming readable — and how long between
/// two looks. Generous on purpose: everything waited for happens in this process, so a loaded
/// host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The authorizer's function, in the simple response format a `2.0` HTTP api authorizer may
/// answer in (`Authorizer$EnableSimpleResponses`): it decides from the value the gateway pulled
/// off the identity source and handed it, so each row's own presentation decides its answer.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    presented = (event.get("identitySource") or [""])[0] or ""
    return {"isAuthorized": presented.startswith("allow")}
"#;

/// The function the route fronts: it answers the proxy envelope every admitted request is
/// served with.
const BACKEND_HANDLER: &str = r#"
def handler(event, context):
    return {"statusCode": 200, "body": "ROUTE-200"}
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

    /// The sentence a refused answer carries, under either spelling a gateway response words
    /// it in.
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

/// The emulator as it is ASSEMBLED, on one engine: both functions run on the Lambda this
/// router mounts, and every datapoint is read from the CloudWatch it mounts.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: the credential scope
    // is what routes an SDK request, and an invoke of the published address carries none at
    // all, as a client of that URL sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`. The signature itself
/// is never checked here.
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

/// One API Gateway v2 control-plane call that has to answer `expected`, addressed at this
/// service's own endpoint host — which is what decides the address a minted api announces.
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

/// One CloudWatch call over its `awsJson1_0` wire that has to answer 200.
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

/// Grant `apigateway.amazonaws.com` the invocation of `name` — the `AddPermission` a client
/// makes beside creating a function an api's route or authorizer names, without which the
/// gateway's invocation is not made at all.
async fn grant(router: &MultiServiceRouter, name: &str, statement: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        Some(json!({
            "StatementId": statement,
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
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

/// The function URI an `Authorizer$AuthorizerUri` and an `AWS_PROXY` integration name a
/// function by: "a well-formed Lambda function URI", which is the gateway's own path form.
fn function_uri(function_arn: &str) -> String {
    format!(
        "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/invocations"
    )
}

/// One series as a reader names it: the namespace, the metric, and the dimensions that
/// identify it together.
struct Series {
    namespace: &'static str,
    metric: &'static str,
    dimensions: Vec<(&'static str, String)>,
}

impl Series {
    /// The api's own series: every request the api answered, whichever stage served it.
    fn of_api(api_id: &str, metric: &'static str) -> Self {
        Self {
            namespace: NAMESPACE,
            metric,
            dimensions: vec![(API_ID, api_id.to_string())],
        }
    }

    /// One STAGE's series: the requests that stage answered.
    fn of_stage(api_id: &str, metric: &'static str) -> Self {
        let mut series = Self::of_api(api_id, metric);
        series
            .dimensions
            .push((STAGE, STAGE_UNDER_TEST.to_string()));
        series
    }

    /// One ROUTE's own series, which only a route whose serving stage asks for detailed
    /// metrics has.
    fn of_route(api_id: &str, metric: &'static str) -> Self {
        let mut series = Self::of_stage(api_id, metric);
        series.dimensions.push((METHOD, VERB.to_string()));
        series
            .dimensions
            .push((RESOURCE, ROUTE_PATH.to_string()));
        series
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

/// What one series holds over the window: how many datapoints a client reads, what they sum
/// to, how many observations they aggregate, and which units they carry.
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
/// `since` is the instant the suite started driving traffic, taken before the first request:
/// the window reaches from a margin before it to a margin past now, so every datapoint the
/// traffic produced is inside it however long the traffic took on this machine.
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
/// looks again until it holds or [`BUDGET`] passes, and the assertions are made on whatever
/// the last read saw.
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
/// EXISTS and reads zero, which is what tells "nothing of that class happened" from "this
/// stage publishes nothing at all".
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

/// Assert `series` holds `samples` observations carrying `unit`, without fixing what they
/// measured: a latency is a duration this machine decides, so what the rule fixes is how many
/// of them there are and what unit they are reported in.
async fn assert_measured(
    router: &MultiServiceRouter,
    case: &str,
    series: &Series,
    since: f64,
    samples: f64,
) {
    let read = awaited(router, case, series, since, samples).await;
    assert_eq!(
        read.samples, samples,
        "case {case:?}: expected {samples} observations of {}, got {read:?}",
        series.metric
    );
    assert_eq!(
        read.units,
        BTreeSet::from([MILLISECONDS_UNIT.to_string()]),
        "case {case:?}: expected every datapoint of {} to carry the unit \
         {MILLISECONDS_UNIT:?}, got {read:?}",
        series.metric
    );
}

/// Assert `series` holds NOTHING AT ALL: not a zero observation, no datapoint.
async fn assert_empty(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) {
    let read = held(router, case, series, since).await;
    assert_eq!(
        read.datapoints, 0,
        "case {case:?}: expected no datapoint of {} at all, got {read:?}",
        series.metric
    );
}

/// One row: what the request presents at the authorizer's identity source, how many such
/// requests the row drives, and what the stage owes each of them.
struct Row {
    case: &'static str,
    /// The value at the authorizer's identity source, or `None` for a request carrying none.
    token: Option<&'static str>,
    requests: usize,
    status: u16,
    /// The sentence a refused answer carries, or `None` for the row the route's backend serves.
    message: Option<&'static str>,
    /// Whether the authorizer's own function is invoked for such a request at all: a request
    /// that presents nothing at the identity source never reaches it.
    reaches_the_authorizer: bool,
    /// Whether the route's integration is performed for such a request.
    reaches_the_integration: bool,
}

/// The table the rule is asserted over: each row presents its own value at the identity source
/// and drives its own number of requests.
static ROWS: [Row; 3] = [
    Row {
        case: "a value the authorizer admits",
        token: Some(ADMITTED_TOKEN),
        requests: 2,
        status: SERVED,
        message: None,
        reaches_the_authorizer: true,
        reaches_the_integration: true,
    },
    Row {
        case: "a value the authorizer refuses",
        token: Some(REFUSED_TOKEN),
        requests: 3,
        status: FORBIDDEN,
        message: Some(FORBIDDEN_MESSAGE),
        reaches_the_authorizer: true,
        reaches_the_integration: false,
    },
    Row {
        case: "a request carrying nothing at the identity source",
        token: None,
        requests: 4,
        status: UNAUTHORIZED,
        message: Some(UNAUTHORIZED_MESSAGE),
        reaches_the_authorizer: false,
        reaches_the_integration: false,
    },
];

/// How many requests of the table satisfy `which`.
fn requests_where(which: impl Fn(&Row) -> bool) -> f64 {
    ROWS.iter()
        .filter(|row| which(row))
        .map(|row| row.requests)
        .sum::<usize>() as f64
}

/// RULE: every request a route of a deployed HTTP api stage answered is counted in the stage's
/// own `AWS/ApiGateway` series, and the STATUS of the answer — not what produced it — decides
/// which error counter it lands in, so the traffic an authorizer refused is metered beside the
/// traffic it admitted instead of going uncounted.
#[test]
fn an_authorizer_gated_http_api_stage_meters_the_traffic_it_refused_and_the_traffic_it_served() {
    std::thread::Builder::new()
        .name("apigatewayv2-gated-stage-metrics".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on (`core/router/src/main.rs`),
            // because one invoke here drives two nested in-process calls on it — the
            // authorizer's invocation and the route's.
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
    grant(&router, AUTHORIZER_FUNCTION, "apigwv2-authorizer").await;
    grant(&router, BACKEND_FUNCTION, "apigwv2-backend").await;

    // 2. The api, as a client scripts one: the `AWS_PROXY` integration over the route's
    //    function, the `REQUEST` authorizer over the gate's, the route that binds them and the
    //    auto-deploying stage. Nothing asks for the route's own series, which is what leaves
    //    the per-route set unpublished while the stage's own are published anyway.
    let api_record = apigw(
        &router,
        "CreateApi",
        "POST",
        "/v2/apis",
        Some(json!({ "name": API_UNDER_TEST, "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api_id = as_str("CreateApi", &api_record, "apiId");
    let announced = as_str("CreateApi", &api_record, "apiEndpoint");

    let integration = apigw(
        &router,
        "CreateIntegration",
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_uri(&backend_arn),
            "payloadFormatVersion": "2.0",
        })),
        201,
    )
    .await;
    let integration_id = as_str("CreateIntegration", &integration, "integrationId");

    let authorizer = apigw(
        &router,
        "CreateAuthorizer",
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(json!({
            "name": "request-gate",
            "authorizerType": "REQUEST",
            "authorizerUri": function_uri(&authorizer_arn),
            "authorizerPayloadFormatVersion": "2.0",
            "enableSimpleResponses": true,
            // "If it equals 0, authorization caching is disabled", so every request's own
            // identity reaches the function and the invocation counts below are per request.
            "authorizerResultTtlInSeconds": 0,
            "identitySource": [format!("$request.header.{TOKEN_HEADER}")],
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "authorizerId");

    apigw(
        &router,
        "CreateRoute",
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": ROUTE_KEY,
            "target": format!("integrations/{integration_id}"),
            "authorizationType": "CUSTOM",
            "authorizerId": authorizer_id,
        })),
        201,
    )
    .await;
    let stage = apigw(
        &router,
        "CreateStage",
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": STAGE_UNDER_TEST, "autoDeploy": true })),
        201,
    )
    .await;
    assert!(
        stage.get("deploymentId").and_then(Value::as_str).is_some(),
        "setup: an auto-deploying stage must carry a deployment, got {stage}"
    );

    // 3. The traffic, taken from the table, made at the address the api PUBLISHED. The window
    //    the series are read over starts here, before the first request.
    let since = now_secs();
    let (authority, base_path) = announced_parts(&announced);
    let invoke_path = format!("{base_path}/{STAGE_UNDER_TEST}{ROUTE_PATH}");
    for row in &ROWS {
        let case = row.case;
        let headers: Vec<(&str, &str)> = match row.token {
            Some(token) => vec![(TOKEN_HEADER, token)],
            None => Vec::new(),
        };
        for attempt in 1..=row.requests {
            let answered = dispatch(
                &router,
                VERB,
                &authority,
                &invoke_path,
                &headers,
                Bytes::new(),
            )
            .await;
            assert_ne!(
                answered.header(ERROR_TYPE_HEADER).as_deref(),
                Some(UNKNOWN_SERVICE),
                "case {case:?} request {attempt}: the published address {authority}{invoke_path} \
                 must be one this emulator serves, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.status, row.status,
                "case {case:?} request {attempt}: expected HTTP {} from the gated stage, got {}",
                row.status,
                answered.summary()
            );
            match row.message {
                Some(sentence) => assert_eq!(
                    answered.message(),
                    sentence,
                    "case {case:?} request {attempt}: expected the refusal {sentence:?}, got {}",
                    answered.summary()
                ),
                None => assert_eq!(
                    answered.text(),
                    BACKEND_BODY,
                    "case {case:?} request {attempt}: expected the route backend's own answer, \
                     got {}",
                    answered.summary()
                ),
            }
        }
    }

    // 4. What the stage published about all of it. Both counters are published for every
    //    answered request, so each series aggregates one observation per request and the SUM is
    //    how many of them were of that class: the refused requests are client errors, and
    //    nothing at all is a server error — read as a datapoint of zero rather than as a series
    //    that does not exist.
    let answered = requests_where(|_| true);
    let client_errors = requests_where(|row| (400..500).contains(&row.status));
    let server_errors = requests_where(|row| (500..600).contains(&row.status));
    let served_by_a_backend = requests_where(|row| row.reaches_the_integration);
    for (level, per_stage) in [
        ("the api's own series", false),
        ("the stage's own series", true),
    ] {
        let of_level = |metric: &'static str| {
            if per_stage {
                Series::of_stage(&api_id, metric)
            } else {
                Series::of_api(&api_id, metric)
            }
        };
        for (metric, sum) in [
            (COUNT, answered),
            (CLIENT_ERRORS, client_errors),
            (SERVER_ERRORS, server_errors),
        ] {
            assert_series(
                &router,
                &format!("{level} read as {metric}"),
                &of_level(metric),
                since,
                sum,
                answered,
                COUNT_UNIT,
            )
            .await;
        }
        // One LATENCY sample per answered request: the measurement is of answering the
        // request, so a refusal the gateway worded itself is measured like a response the
        // route's backend produced.
        assert_measured(
            &router,
            &format!("{level} read as {LATENCY}"),
            &of_level(LATENCY),
            since,
            answered,
        )
        .await;
        // The BACKEND's own latency is the one series a refused request contributes nothing
        // to: it reports how long the route's integration took to answer, and the refusals
        // reached no integration at all.
        assert_measured(
            &router,
            &format!("{level} read as {INTEGRATION_LATENCY}"),
            &of_level(INTEGRATION_LATENCY),
            since,
            served_by_a_backend,
        )
        .await;
    }

    // 5. The per-ROUTE set is the one the opt-in gates, and this stage asks for nothing: no
    //    datapoint of it exists, while every stage-level series above does.
    assert_empty(
        &router,
        "the route's own series, which the stage never asked for",
        &Series::of_route(&api_id, COUNT),
        since,
    )
    .await;

    // 6. And what RAN: the authorizer was invoked once per request that presented its identity
    //    source, and the route's function once per request the authorizer admitted — so the
    //    refused requests, counted above as client errors of the stage, reached no integration.
    for (function, invocations) in [
        (
            AUTHORIZER_FUNCTION,
            requests_where(|row| row.reaches_the_authorizer),
        ),
        (BACKEND_FUNCTION, served_by_a_backend),
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
