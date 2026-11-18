// SPDX-License-Identifier: CC0-1.0

//! WHICH SERIES a deployed REST API stage's request telemetry is readable under, over a stage
//! whose methods are run by a Lambda function through an `AWS_PROXY` integration.
//!
//! RULE: **every request a deployed stage of a REST API answered is published in API Gateway's
//! own `AWS/ApiGateway` namespace keyed by the api's NAME — the `ApiName` dimension alone, and
//! `ApiName` with `Stage` — carrying one `Count` unit, one unit of the error counter its status
//! class belongs to and a published zero of the other, one `Latency` sample and, for a request
//! an integration answered, one `IntegrationLatency` sample. The api's NAME is the whole of its
//! identity in these series, so a read keyed by any other dimension set — above all the
//! `{ApiId, Stage}` pair an HTTP API's series are keyed by — holds no datapoint of a REST api's
//! traffic at all.**
//!
//! The absent half is as much of the rule as the present one. A client's dashboards, its alarms
//! on `5XXError`/`Latency` and its SLO tooling each name ONE series; a stage that published its
//! traffic under an id-keyed set beside the name-keyed one would answer a dashboard querying
//! either, so the wrong query would silently look right here and read empty against AWS. And a
//! counter that goes unpublished for a class with no occurrences cannot be told from a pipeline
//! that never ran: an alarm that treats missing data as breaching fires forever on it.
//!
//! Three crates have to agree for that to hold — IAM holds the function's execution role,
//! CloudFormation archives the inline handler into a package this emulator can run, Lambda runs
//! it, API Gateway resolves the stage, relays the call and publishes what it answered, and the
//! metric store every assertion reads is CloudWatch's own — so the suite lives here, on the
//! emulator as `aws_emulator_router::build_router` assembles it.
//!
//! What the sibling [`crate::apigateway_stage_request_metrics`] suite pins is what a `MOCK`
//! method's answers are OBSERVED as, and the per-method opt-in. What only this suite reaches is
//! the KEYING — that the name-keyed sets hold the traffic and every id-keyed set holds nothing —
//! and the same observations over a method an integration really RAN, whose
//! `IntegrationLatency` is therefore a measurement of a relayed call rather than of an answer
//! the gateway made itself.
//!
//! The traffic rows differ along what the rule generalises over: the STATUS CLASS the handler's
//! envelope carries (a success, a client error, a server error), the DEPTH of the path that
//! matched the greedy proxy resource (one segment, three), and HOW MANY requests each row
//! drives — a different number per row, so no row's count can stand in for another's. The
//! keying rows differ along which dimension set a reader names: the HTTP-API pair, the id
//! alone, each slot filled with the other's value, and the api's own keying with the id added.
//! A second api, deployed the same way and answering only successes, is what shows a counter
//! reading zero rather than reading nothing.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every call is signed for, every ARN names and the apis' invoke hosts carry.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the invoke grant's source ARN names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The access key id this emulator publishes, which speaks for the account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), which is
/// what routes a read of the series to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The function behind every method of both apis, the role it runs as, and the stack that
/// archives its inline handler into a deployment package.
const FUNCTION: &str = "xsvc-rest-metric-keying-proxy";
const EXECUTION_ROLE: &str = "xsvc-rest-metric-keying-exec";
const STACK: &str = "xsvc-rest-metric-keying-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The names of the two apis, and the stage both are deployed to.
const TRAFFIC_API: &str = "rest-metric-keying-api";
const QUIET_API: &str = "rest-metric-keying-quiet-api";
const STAGE: &str = "live";

/// The greedy catch-all resource every path below matches, and the verb its one method is
/// deployed under: "the `ANY` catch-all that matches every method".
const PROXY_PATH_PART: &str = "{proxy+}";
const VERB: &str = "ANY";

/// The namespace API Gateway publishes what its apis answered in.
const NAMESPACE: &str = "AWS/ApiGateway";

/// The dimension a REST api's series are keyed by, the one that separates one stage's traffic
/// from another's, and the one an HTTP API's series are keyed by instead — which is the
/// dimension no REST api may publish under.
const API_NAME_DIMENSION: &str = "ApiName";
const STAGE_DIMENSION: &str = "Stage";
const API_ID_DIMENSION: &str = "ApiId";

/// The counters and the measurements one answered request is observed in.
const COUNT: &str = "Count";
const CLIENT_ERRORS: &str = "4XXError";
const SERVER_ERRORS: &str = "5XXError";
const LATENCY: &str = "Latency";
const INTEGRATION_LATENCY: &str = "IntegrationLatency";

/// Every series one answered request is observed in, which is what each keying row is read
/// across: a dimension set that holds a REST api's traffic holds all five, and one that does
/// not holds none of them.
const EVERY_METRIC: [&str; 5] = [
    COUNT,
    CLIENT_ERRORS,
    SERVER_ERRORS,
    LATENCY,
    INTEGRATION_LATENCY,
];

/// The units those series carry: a bare count of requests, and a measured duration.
const COUNT_UNIT: &str = "Count";
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The period the series are read at, in seconds, and how far either side of the traffic a read
/// window reaches — generous, because the only thing that decides which window a datapoint
/// falls in is the instant its request was answered at, and the traffic takes as long as the
/// machine takes.
const PERIOD: i64 = 60;
const WINDOW_MARGIN_PERIODS: f64 = 3.0;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the series and not how fast an interpreter
/// gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a created function is given to report itself `Active`, and how often to look. The
/// span is Lambda's own, so the wait is on the CONDITION and the deadline is many times over
/// what the transition needs, so a loaded host cannot decide the outcome.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// How long ONE request to a deployed stage is given to be answered. A request still open after
/// this was never going to be answered at all, so it is asserted rather than waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(120);

/// The handler behind every method: the path it was called for decides the `statusCode` of the
/// envelope it returns, so one package serves every row and each row's status is one a RUN
/// returned — "API Gateway ... uses the Lambda function's output as the HTTP response body,
/// status code and headers" (API Gateway Developer Guide, *Lambda proxy integration*).
const PROXY_HANDLER: &str = r#"
def handler(event, context):
    path = event.get("path") or event.get("rawPath") or ""
    if path.endswith("/boom"):
        return {"statusCode": 500, "body": "LAMBDA-500"}
    if path.endswith("/missing"):
        return {"statusCode": 404, "body": "LAMBDA-404"}
    return {"statusCode": 200, "body": "LAMBDA-200"}
"#;

/// One traffic row: a path within the stage, how many requests it drives, and what each of them
/// must be answered with.
struct Row {
    case: &'static str,
    /// What the client asks for within the stage, which the greedy proxy resource matches.
    asked_for: &'static str,
    /// The status the handler's envelope carries for that path.
    status: u16,
    /// The body that envelope carries, which is what shows the integration's own answer was
    /// passed through rather than replaced by one of the gateway's own.
    body: &'static str,
    /// How many requests this row drives.
    requests: usize,
}

/// The table. The counts differ per row so that no row's contribution to a counter can stand in
/// for another's, and the paths differ in depth so the greedy proxy resource is matched at more
/// than one shape.
static ROWS: [Row; 4] = [
    Row {
        case: "a success under a one-segment path",
        asked_for: "/ok",
        status: 200,
        body: "LAMBDA-200",
        requests: 2,
    },
    Row {
        case: "a success under a three-segment path the greedy resource matches",
        asked_for: "/deep/nested/ok",
        status: 200,
        body: "LAMBDA-200",
        requests: 1,
    },
    Row {
        case: "the client-error class, chosen by the handler",
        asked_for: "/missing",
        status: 404,
        body: "LAMBDA-404",
        requests: 3,
    },
    Row {
        case: "the server-error class, chosen by the handler",
        asked_for: "/boom",
        status: 500,
        body: "LAMBDA-500",
        requests: 2,
    },
];

/// How many requests the quiet api is driven with: it answers nothing but successes, so its
/// error counters are what show a counter READING ZERO.
const QUIET_REQUESTS: usize = 2;

/// One keying row: a dimension set a reader might name, and why it names no series of a REST
/// api's traffic.
struct Keying {
    case: &'static str,
    /// The dimension set, built from the api's name and the api's id.
    dimensions: fn(&str, &str) -> Vec<(&'static str, String)>,
}

/// The dimension sets that must hold NO datapoint of a REST api's traffic. Each is built from
/// the api's own name and id, so none of them is a set that merely names some other api.
static NOT_A_REST_APIS_KEYING: [Keying; 5] = [
    Keying {
        case: "the HTTP-API dimension pair",
        dimensions: |_name, id| {
            vec![
                (API_ID_DIMENSION, id.to_string()),
                (STAGE_DIMENSION, STAGE.to_string()),
            ]
        },
    },
    Keying {
        case: "the api's id alone",
        dimensions: |_name, id| vec![(API_ID_DIMENSION, id.to_string())],
    },
    Keying {
        case: "the name-keyed pair with the id in the name's slot",
        dimensions: |_name, id| {
            vec![
                (API_NAME_DIMENSION, id.to_string()),
                (STAGE_DIMENSION, STAGE.to_string()),
            ]
        },
    },
    Keying {
        case: "the id-keyed pair with the name in the id's slot",
        dimensions: |name, _id| {
            vec![
                (API_ID_DIMENSION, name.to_string()),
                (STAGE_DIMENSION, STAGE.to_string()),
            ]
        },
    },
    Keying {
        case: "the api's own keying with the id added to it",
        dimensions: |name, id| {
            vec![
                (API_NAME_DIMENSION, name.to_string()),
                (STAGE_DIMENSION, STAGE.to_string()),
                (API_ID_DIMENSION, id.to_string()),
            ]
        },
    },
];

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of a published stage URL
    // carries no credential at all, as a client of a public method sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
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
        body,
    }
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
        "setup {what:?}: expected HTTP {expected} from API Gateway, got {}",
        answered.summary()
    );
    answered.json()
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

/// One CloudWatch call over its `awsJson1_0` wire that has to answer 200.
async fn monitoring(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
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
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {}",
        answered.summary()
    );
    answered.json()
}

/// One `awsQuery` call of `service` that has to be accepted, answering its XML body.
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
        "setup {service} {action}: expected HTTP 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One unsigned request to a deployed stage's own invoke URL, as a client of a method whose
/// `authorizationType` is `NONE` makes it. The request must be ANSWERED.
async fn invoke(router: &MultiServiceRouter, case: &str, api: &str, asked_for: &str) -> Answered {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let path = format!("/{STAGE}{asked_for}");
    ::tokio::time::timeout(
        ANSWER_DEADLINE,
        dispatch(router, "GET", &host, &path, &[], Bytes::new()),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "case {case:?}: GET {path} must be answered by the stage within {ANSWER_DEADLINE:?}, \
             got no HTTP response at all"
        )
    })
}

/// A required, non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: expected a non-empty {member:?}, got {body}"))
}

/// Wall-clock now as epoch seconds, the clock the read windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// A trust policy naming one AWS service as the principal that may assume the role.
fn service_trust_policy(service_principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service_principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The execution role the function runs as, answering the ARN IAM minted for it.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            (
                "AssumeRolePolicyDocument",
                &service_trust_policy("lambda.amazonaws.com"),
            ),
        ],
    )
    .await;
    opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole({EXECUTION_ROLE:?}): expected the role's ARN"))
}

/// Deploy the stack declaring the function: the inline source is archived into a deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Proxy": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": PROXY_HANDLER },
                },
            },
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
        "setup CreateStack: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );

    // The invoke the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
    let deadline = Instant::now() + ACTIVE_DEADLINE;
    let mut configuration;
    loop {
        configuration = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup GetFunctionConfiguration: expected Lambda to report {FUNCTION:?} Active, got \
         {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// A REGIONAL REST api named `name` whose greedy proxy resource carries one open `ANY` method
/// run by `function_arn` through an `AWS_PROXY` integration, deployed to [`STAGE`]. Answers the
/// api's id.
async fn deployed_api(router: &MultiServiceRouter, name: &str, function_arn: &str) -> String {
    let created = apigw(
        router,
        name,
        "POST",
        "/restapis",
        Some(json!({
            "name": name,
            "endpointConfiguration": { "types": ["REGIONAL"] },
        })),
        201,
    )
    .await;
    let api = as_str(name, &created, "id");
    let root = as_str(name, &created, "rootResourceId");

    let child = apigw(
        router,
        name,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": PROXY_PATH_PART })),
        201,
    )
    .await;
    let proxy = as_str(name, &child, "id");

    let method = format!("/restapis/{api}/resources/{proxy}/methods/{VERB}");
    apigw(
        router,
        name,
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        name,
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that
    // admits the invocation API Gateway makes as its own service principal.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": format!("apigateway-invoke-{api}"),
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*"),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission({name:?}): expected HTTP 201 from Lambda, got {}",
        granted.summary()
    );

    apigw(
        router,
        name,
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// What one series holds over the window: how many datapoints a client reads, what they sum to,
/// how many observations they aggregate, the smallest value any of them reports, and which
/// units they carry.
#[derive(Debug, Default)]
struct Held {
    datapoints: usize,
    sum: f64,
    samples: f64,
    minimum: f64,
    units: BTreeSet<String>,
}

/// Read ONE series of this namespace over the whole window and fold what it holds.
///
/// `since` is the instant the suite started driving traffic, taken before the first request: the
/// window reaches from a margin before it to a margin past now, so every datapoint the traffic
/// produced is inside it however long the traffic took on this machine.
async fn held(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &[(&str, String)],
    since: f64,
) -> Held {
    let margin = WINDOW_MARGIN_PERIODS * PERIOD as f64;
    let answered = monitoring(
        router,
        case,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions
                .iter()
                .map(|(name, value)| json!({ "Name": name, "Value": value }))
                .collect::<Vec<Value>>(),
            "StartTime": since - margin,
            "EndTime": now_secs() + margin,
            "Period": PERIOD,
            "Statistics": ["Sum", "SampleCount", "Minimum"],
        }),
    )
    .await;
    let mut folded = Held {
        minimum: f64::INFINITY,
        ..Held::default()
    };
    for datapoint in answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        folded.datapoints += 1;
        folded.sum += datapoint["Sum"].as_f64().unwrap_or_default();
        folded.samples += datapoint["SampleCount"].as_f64().unwrap_or_default();
        if let Some(minimum) = datapoint["Minimum"].as_f64() {
            folded.minimum = folded.minimum.min(minimum);
        }
        if let Some(unit) = datapoint["Unit"].as_str() {
            folded.units.insert(unit.to_string());
        }
    }
    folded
}

/// The dimension set that keys the api's own series: its NAME alone.
fn of_api(api_name: &str) -> Vec<(&'static str, String)> {
    vec![(API_NAME_DIMENSION, api_name.to_string())]
}

/// The dimension set that keys ONE stage's series: the api's name with the stage beside it.
fn of_stage(api_name: &str) -> Vec<(&'static str, String)> {
    let mut dimensions = of_api(api_name);
    dimensions.push((STAGE_DIMENSION, STAGE.to_string()));
    dimensions
}

/// Assert the counter `metric` of the series `dimensions` names sums to `sum` over `samples`
/// observations, each a bare count of requests.
async fn assert_counter(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &[(&str, String)],
    since: f64,
    sum: f64,
    samples: f64,
) {
    let read = held(router, case, metric, dimensions, since).await;
    assert_eq!(
        read.sum, sum,
        "case {case:?}: expected the {metric} series of {dimensions:?} to sum to {sum}, got \
         {read:?}"
    );
    assert_eq!(
        read.samples, samples,
        "case {case:?}: expected {samples} observations of {metric} under {dimensions:?}, got \
         {read:?}"
    );
    assert_eq!(
        read.units,
        BTreeSet::from([COUNT_UNIT.to_string()]),
        "case {case:?}: expected every datapoint of {metric} to carry the unit {COUNT_UNIT:?}, \
         got {read:?}"
    );
}

/// Assert the measurement `metric` of the series `dimensions` names holds exactly `samples`
/// observations, each a duration in milliseconds.
///
/// How LONG answering a request took is what the machine took, so the value itself is not a
/// requirement — that there is one observation per request, in the unit the series is expressed
/// in and not below zero, is.
async fn assert_measurement(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &[(&str, String)],
    since: f64,
    samples: f64,
) {
    let read = held(router, case, metric, dimensions, since).await;
    assert_eq!(
        read.samples, samples,
        "case {case:?}: expected {samples} observations of {metric} under {dimensions:?}, got \
         {read:?}"
    );
    assert_eq!(
        read.units,
        BTreeSet::from([MILLISECONDS_UNIT.to_string()]),
        "case {case:?}: expected every datapoint of {metric} to carry the unit \
         {MILLISECONDS_UNIT:?}, got {read:?}"
    );
    assert!(
        read.minimum.is_finite() && read.minimum >= 0.0,
        "case {case:?}: expected every {metric} observation under {dimensions:?} to be a \
         duration at or above zero, got {read:?}"
    );
}

/// Assert the series `dimensions` names holds NOTHING AT ALL: not a zero observation, no
/// datapoint.
async fn assert_empty(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &[(&str, String)],
    since: f64,
) {
    let read = held(router, case, metric, dimensions, since).await;
    assert_eq!(
        read.datapoints, 0,
        "case {case:?}: expected no datapoint of {metric} under {dimensions:?} at all, got \
         {read:?}"
    );
}

/// Drive `row`'s requests at `api`, asserting each one is answered with the status AND the body
/// the handler's envelope carries — so a row that counts N requests really made N answered
/// requests, each of them through the integration.
async fn drive(router: &MultiServiceRouter, api: &str, row: &Row) {
    for ordinal in 1..=row.requests {
        let answered = invoke(router, row.case, api, row.asked_for).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?} request {ordinal} of {}: expected HTTP {}, got {}",
            row.case,
            row.requests,
            row.status,
            answered.summary()
        );
        assert_eq!(
            answered.text(),
            row.body,
            "case {:?} request {ordinal} of {}: expected the integration's own body {:?}, got {}",
            row.case,
            row.requests,
            row.body,
            answered.summary()
        );
    }
}

/// How many of the rows' requests were answered in the status class `class` names, and how many
/// they drove in all.
fn counted(class: Option<u16>) -> (f64, f64) {
    let requests: usize = ROWS.iter().map(|row| row.requests).sum();
    let in_class: usize = ROWS
        .iter()
        .filter(|row| class.is_some_and(|class| row.status / 100 == class))
        .map(|row| row.requests)
        .sum();
    let counted = if class.is_none() { requests } else { in_class };
    (counted as f64, requests as f64)
}

#[tokio::test]
async fn a_rest_stages_request_series_are_keyed_by_the_apis_name_and_never_by_its_id() {
    let router = emulator();
    let role_arn = execution_role(&router).await;
    let function_arn = deployed_function(&router, &role_arn).await;
    let api = deployed_api(&router, TRAFFIC_API, &function_arn).await;
    // A SECOND api, deployed the same way, which only ever answers successes: its error
    // counters are what show a counter reading ZERO rather than reading nothing at all.
    let quiet = deployed_api(&router, QUIET_API, &function_arn).await;

    // Taken before the first request and used as the start of every read window below, so a
    // machine that takes minutes to drive the traffic still reads all of it back.
    let since = now_secs();
    for row in &ROWS {
        drive(&router, &api, row).await;
    }
    for ordinal in 1..=QUIET_REQUESTS {
        let answered = invoke(&router, QUIET_API, &quiet, "/ok").await;
        assert_eq!(
            answered.status, 200,
            "case {QUIET_API:?} request {ordinal} of {QUIET_REQUESTS}: expected HTTP 200, got {}",
            answered.summary()
        );
    }

    // The api's own series and its stage's series each hold the whole of the traffic: every
    // request counted once, each error counter summing to the requests of its class and
    // observed for every request, and one latency sample per request beside one integration
    // latency sample for each request the integration answered.
    let (total, requests) = counted(None);
    let (client_errors, _) = counted(Some(4));
    let (server_errors, _) = counted(Some(5));
    for (keyed_by, dimensions) in [
        ("the api's own series", of_api(TRAFFIC_API)),
        ("the stage's series", of_stage(TRAFFIC_API)),
    ] {
        assert_counter(
            &router,
            &format!("{keyed_by} read as {COUNT}"),
            COUNT,
            &dimensions,
            since,
            total,
            requests,
        )
        .await;
        assert_counter(
            &router,
            &format!("{keyed_by} read as {CLIENT_ERRORS}"),
            CLIENT_ERRORS,
            &dimensions,
            since,
            client_errors,
            requests,
        )
        .await;
        assert_counter(
            &router,
            &format!("{keyed_by} read as {SERVER_ERRORS}"),
            SERVER_ERRORS,
            &dimensions,
            since,
            server_errors,
            requests,
        )
        .await;
        assert_measurement(
            &router,
            &format!("{keyed_by} read as {LATENCY}"),
            LATENCY,
            &dimensions,
            since,
            requests,
        )
        .await;
        assert_measurement(
            &router,
            &format!("{keyed_by} read as {INTEGRATION_LATENCY}"),
            INTEGRATION_LATENCY,
            &dimensions,
            since,
            requests,
        )
        .await;
    }

    // The quiet api answered nothing but successes, so BOTH of its error counters are published
    // and both read zero over the datapoints they hold — which is what tells "nothing failed"
    // from "this stage publishes nothing at all".
    let quiet_requests = QUIET_REQUESTS as f64;
    let quiet_dimensions = of_stage(QUIET_API);
    assert_counter(
        &router,
        &format!("the quiet api read as {COUNT}"),
        COUNT,
        &quiet_dimensions,
        since,
        quiet_requests,
        quiet_requests,
    )
    .await;
    for metric in [CLIENT_ERRORS, SERVER_ERRORS] {
        let case = format!("the quiet api read as {metric}");
        assert_counter(
            &router,
            &case,
            metric,
            &quiet_dimensions,
            since,
            0.0,
            quiet_requests,
        )
        .await;
        let read = held(&router, &case, metric, &quiet_dimensions, since).await;
        assert!(
            read.datapoints > 0,
            "case {case:?}: expected the {metric} counter to be PUBLISHED reading zero, over at \
             least one datapoint, got {read:?}"
        );
    }

    // The keying itself: no dimension set built from the api's id names any series of its
    // traffic, in any of the five metrics an answered request is observed in.
    for keying in &NOT_A_REST_APIS_KEYING {
        let dimensions = (keying.dimensions)(TRAFFIC_API, &api);
        for metric in EVERY_METRIC {
            assert_empty(
                &router,
                &format!("{} read as {metric}", keying.case),
                metric,
                &dimensions,
                since,
            )
            .await;
        }
    }
}
