// SPDX-License-Identifier: CC0-1.0

//! The traffic a client makes AT THE ADDRESS an API Gateway v2 api announces, and the
//! `AWS/ApiGateway` series that traffic raises — both on the emulator as it is ASSEMBLED.
//!
//! RULE: **an api's `ApiEndpoint` is the address a caller hands a client library to reach that
//! api — "the URI of the API … The stage name is typically appended to this URI to form a
//! complete path to a deployed API stage" (`Api$ApiEndpoint`) — so every address an api
//! announces is one this emulator answers that api's OWN protocol on: a request made at a
//! deployed HTTP api's announced address is resolved to the route it matches and answered by
//! that route's integration, or by the refusal the api's own records decide for it, and an RFC
//! 6455 opening handshake made at a deployed WEBSOCKET api's announced address — the socket
//! that api is served on — is answered `101 Switching Protocols` carrying the accept value the
//! offered key derives. A well-formed call at an announced address is never answered by no
//! service at all, and never with a server-error status. The stage that answered it then
//! publishes what it answered in its own `AWS/ApiGateway` series — an HTTP stage's `Count`,
//! `4xx` and `5xx` under `{ApiId}` and under `{ApiId, Stage}`, a WEBSOCKET stage's
//! `MessageCount`, `ConnectCount` and `ClientError` under `{ApiId, Stage}` — so a stage a
//! client reached at the announced address is observable, and a stage that carried no traffic
//! publishes nothing.**
//!
//! Three services have to agree for that to hold — API Gateway V2 mints the api, publishes the
//! address, serves the request or the handshake made there and publishes what the stage
//! answered, the identity provider a gated route's JWT authorizer is configured against is an
//! Amazon Cognito user pool that service really holds, and the store every datapoint lands in is
//! CloudWatch's own — so the suite lives here, on the assembled emulator. Every count is read
//! back through CloudWatch's own
//! `GetMetricStatistics`, the call a dashboard or an alarm resolves a series with: a datapoint
//! that never reached that store cannot pass. The metric vocabulary (the `AWS/ApiGateway`
//! namespace, the dimension sets `{ApiId}` and `{ApiId, Stage}`, and the metric names below) is
//! API Gateway's published metrics-and-dimensions reference, since no modelled operation
//! expresses metric emission at all.
//!
//! The rows span what the rule generalises over: the api's `ProtocolType`, the endpoint the
//! control-plane client was configured with (an IPv4 loopback literal, a loopback NAME, a
//! bracketed IPv6 literal, and one of AWS's own service hosts), the stage the announced address
//! is appended with (a named one, and the reserved stage that serves the api's root), the
//! outcome the api's own records decide (a route that serves the request, a JWT authorizer that
//! refuses a request presenting no token and one that refuses a value that is not a token, a
//! stage with no deployment released to it, a stage name the api does not hold), where the
//! token travels, and how many calls each row makes.
//!
//! What the owning crate's own suites pin and this one does not restate: the FORM the announced
//! address takes
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/api_endpoint_announced_address.rs`),
//! the derivation of `Sec-WebSocket-Accept` over the whole range of offered keys and the
//! per-route split of a stage's series (`.../tests/all/websocket_stage_upgrade_counts.rs`,
//! `.../tests/all/websocket_stage_metrics.rs`), and the session carried behind an upgraded
//! handshake (`.../tests/all/websocket_transport_data_plane.rs`). What only this suite reaches
//! is ONE announced value carrying traffic that a monitoring client then sees.
//!
//! Assembling the router costs stack, so each check runs on an explicitly sized thread for the
//! same reason the sibling router suites do.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The readings this suite shares with the siblings that own them, rather than second copies
// here: how an ANNOUNCED endpoint is split into the address a client dials and the path ahead of
// the stage, the opening handshake a client writes to such an address and the head it is
// answered with, and the key/accept pair RFC 6455 publishes as its own worked example.
use crate::apigatewayv2_published_endpoint_runs_its_lambda_route::announced_parts;
use crate::apigatewayv2_websocket_connect_registers_its_agent::{
    answered_handshake_at, ACCEPT_FIELD, RFC_EXAMPLE_ACCEPT, RFC_EXAMPLE_KEY,
};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every control-plane call below is signed for, and so the Region the apis and
/// their series live in.
const REGION: &str = "us-east-1";

/// The namespace API Gateway publishes what its apis answered in, and the dimensions those
/// series are identified by.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_ID: &str = "ApiId";
const STAGE: &str = "Stage";

/// The series an HTTP api's stage publishes per request it answered.
const COUNT: &str = "Count";
const CLIENT_ERRORS: &str = "4xx";
const SERVER_ERRORS: &str = "5xx";

/// The series a WEBSOCKET api's stage publishes per call it carried.
const MESSAGE_COUNT: &str = "MessageCount";
const CONNECT_COUNT: &str = "ConnectCount";
const CLIENT_ERROR: &str = "ClientError";

/// The reserved stage "that serves an api's root", reached with no stage segment at all.
const ROOT_STAGE: &str = "$default";

/// The route every HTTP row deploys, the path within the stage that matches it, and the
/// upstream its `HTTP_PROXY` integration fronts.
const ROUTE_KEY: &str = "GET /items";
const ROUTE_PATH: &str = "/items";
const UPSTREAM: &str = "https://upstream.example.com/items";

/// The status an `HTTP_PROXY` route answers: the upstream is a URI the CALLER can reach, so the
/// caller is sent there, with the integration's own `IntegrationUri` in `Location`. Reaching it
/// proves the request resolved all the way to the matched route's integration, which is what an
/// announced address being SERVED means.
const SERVED: u16 = 307;

/// The status and sentence a request a JWT authorizer refused is answered with.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The status and sentence an `execute-api` endpoint answers for a stage it does not serve.
const FORBIDDEN: u16 = 403;

/// The front door's error code for a request NO service claimed. An announced address nothing
/// serves is the defect this suite pins, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The user pool a gated row's JWT authorizer is configured against, and the name of the app
/// client whose id is the audience it matches. The pool is a REAL one of the Cognito user pools
/// service mounted beside this one, because a JWT authorizer records "the base URL of the
/// identity provider that issues JSON Web Tokens" (`JWTConfiguration$Issuer`) and an api admits
/// only an issuer that really publishes a discovery document. No row presents a token that pool
/// signed: what is pinned here is the REFUSAL's class and its metering, and the token path
/// itself is owned by `core/router/tests/all/apigatewayv2_cognito_jwt_authorizer.rs`.
const POOL_NAME: &str = "announced-address-pool";
const APP_CLIENT_NAME: &str = "announced-address-client";

/// The status line RFC 6455 §4.2.2 requires of a COMPLETED opening handshake.
const SWITCHING_PROTOCOLS: u16 = 101;
const SWITCHING_PROTOCOLS_LINE: &str = "HTTP/1.1 101 Switching Protocols";

/// How long a case waits for a series to hold the values its traffic owes it, and how often it
/// looks again. Every datapoint waited for is owed by a call that has ALREADY been answered, so
/// what is waited for is a condition rather than anything a clock schedules.
const SERIES_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The window every read below spans, before and after the instant it is made at, and the
/// period it lays its buckets at: wide enough to hold a case's whole traffic whichever minute
/// bucket each datapoint landed in, at the resolution a regular datapoint is stored with.
const WINDOW_SECONDS: f64 = 600.0;
const PERIOD_SECONDS: i64 = 60;

/// The emulator as it is ASSEMBLED, on one engine: the api, the socket it opens and the metric
/// store its stages publish into are all served by this one router.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and a data-plane call carries whatever
    // its own client would send.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// The answer to one dispatched request.
struct Answered {
    status: u16,
    headers: ::http::HeaderMap,
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
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
    }

    /// The whole answer in one line, for a failure message.
    fn summary(&self) -> String {
        format!(
            "{} (x-amzn-errortype {:?}, location {:?}) {:?}",
            self.status,
            self.header("x-amzn-errortype"),
            self.header("location"),
            self.text()
        )
    }
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
    let mut builder = ::http::Request::builder()
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

/// One API Gateway V2 control-plane call that has to be accepted with `expected`, made at the
/// endpoint `endpoint` — the address the client is configured with, which is what decides the
/// address the api is announced on. The V2 endpoint shares the control plane's `apigateway`
/// signing name.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        endpoint,
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status, expected,
        "case {case:?}: expected HTTP {expected} from {method} {path} at {endpoint}, got {}",
        answered.summary()
    );
    answered.json()
}

/// A required string member of an answer.
fn member(case: &str, what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: {what} must carry {name:?}, got {body}"))
}

/// One awsJson1_1 call to the Amazon Cognito user pools service, which must succeed.
async fn cognito(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let host = format!("cognito-idp.{REGION}.amazonaws.com");
    let answered = dispatch(
        router,
        "POST",
        &host,
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            (
                "x-amz-target",
                &format!("AWSCognitoIdentityProviderService.{operation}"),
            ),
            ("authorization", &authorization("cognito-idp")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: cognito-idp {operation} must answer 200, got {}",
        answered.summary()
    );
    answered.json()
}

/// Mint the user pool and the app client a gated row's authorizer is configured against, and
/// answer the issuer that pool publishes and the audience that client's id is.
///
/// "A Cognito user pool has the following format:
/// `https://cognito-idp.{region}.amazonaws.com/{userPoolId}`" (`JWTConfiguration$Issuer`), so the
/// value is built from the Region the pool was created in and the id Cognito minted for it —
/// nothing about the pool is written here.
async fn pool_issuer(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let pool = cognito(
        router,
        case,
        "CreateUserPool",
        json!({ "PoolName": POOL_NAME }),
    )
    .await;
    let pool_id = member(case, "CreateUserPool", &pool["UserPool"], "Id");
    let client = cognito(
        router,
        case,
        "CreateUserPoolClient",
        json!({ "UserPoolId": pool_id, "ClientName": APP_CLIENT_NAME }),
    )
    .await;
    let client_id = member(
        case,
        "CreateUserPoolClient",
        &client["UserPoolClient"],
        "ClientId",
    );
    (
        format!("https://cognito-idp.{REGION}.amazonaws.com/{pool_id}"),
        client_id,
    )
}

/// One series of a stage, the statistic read off it, and the value the traffic owes it. `Sum`
/// reads a counter's total.
struct Series {
    metric: &'static str,
    statistic: &'static str,
    value: f64,
}

/// A counter's total.
fn total(metric: &'static str, value: f64) -> Series {
    Series {
        metric,
        statistic: "Sum",
        value,
    }
}

/// What CloudWatch reports for ONE series over the window this read spans: the statistic folded
/// over every datapoint in it, which is nothing at all — and so zero of whatever was asked for
/// — for a series no call ever published.
async fn observed(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &[(&'static str, String)],
    series: &Series,
) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_secs_f64();
    let host = format!("monitoring.{REGION}.amazonaws.com");
    let body = json!({
        "Namespace": NAMESPACE,
        "MetricName": series.metric,
        "Dimensions": dimensions
            .iter()
            .map(|(name, value)| json!({ "Name": name, "Value": value }))
            .collect::<Vec<Value>>(),
        "StartTime": now - WINDOW_SECONDS,
        "EndTime": now + WINDOW_SECONDS,
        "Period": PERIOD_SECONDS,
        "Statistics": ["Sum", "SampleCount"],
    });
    let answered = dispatch(
        router,
        "POST",
        &host,
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            (
                "x-amz-target",
                "GraniteServiceVersion20100801.GetMetricStatistics",
            ),
            ("authorization", &authorization("monitoring")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: GetMetricStatistics for {:?} must answer 200, got {}",
        series.metric,
        answered.summary()
    );
    answered
        .json()
        .get("Datapoints")
        .and_then(Value::as_array)
        .map(|datapoints| {
            datapoints
                .iter()
                .filter_map(|datapoint| datapoint.get(series.statistic))
                .filter_map(Value::as_f64)
                .sum()
        })
        .unwrap_or(0.0)
}

/// Wait until every expectation of one dimension set holds, failing with the case, the scope
/// and what was actually answered when the deadline passes.
async fn reached(
    router: &MultiServiceRouter,
    case: &str,
    scope: &str,
    dimensions: &[(&'static str, String)],
    expected: &[Series],
) {
    let deadline = Instant::now() + SERIES_DEADLINE;
    loop {
        let mut seen: Vec<String> = Vec::new();
        let mut all = true;
        for want in expected {
            let got = observed(router, case, dimensions, want).await;
            // Counts are whole numbers, so half a unit apart is a different answer; nothing
            // here compares two measured durations.
            if (got - want.value).abs() > 0.5 {
                all = false;
            }
            seen.push(format!(
                "{} {}={got} (expected {})",
                want.metric, want.statistic, want.value
            ));
        }
        if all {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {scope} must publish the series its traffic produced; got {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The dimension set of an api's own series, and of one of its stages'.
fn api_scope(api_id: &str) -> Vec<(&'static str, String)> {
    vec![(API_ID, api_id.to_string())]
}

fn stage_scope(api_id: &str, stage: &str) -> Vec<(&'static str, String)> {
    vec![(API_ID, api_id.to_string()), (STAGE, stage.to_string())]
}

/// WHERE a gated row's token travels, which is the request part the authorizer's
/// `IdentitySource` names, and what the client presents there.
enum Gate {
    /// No authorizer at all: the route is open, so the request reaches its integration.
    Open,
    /// A JWT authorizer reading the `Authorization` header, with the RFC 6750 `Bearer` scheme
    /// in front of the token — the example `Authorizer$IdentitySource` itself gives.
    BearerHeader(Option<&'static str>),
    /// A JWT authorizer reading a query-string parameter, the other form that member documents.
    Query(&'static str, &'static str),
}

impl Gate {
    /// The `IdentitySource` entry the row's authorizer is created with, for a gated row.
    fn identity_source(&self) -> Option<String> {
        match self {
            Self::Open => None,
            Self::BearerHeader(_) => Some("$request.header.Authorization".to_string()),
            Self::Query(name, _) => Some(format!("$request.querystring.{name}")),
        }
    }

    /// The `AuthorizationType` the row's route carries.
    fn authorization_type(&self) -> &'static str {
        match self {
            Self::Open => "NONE",
            Self::BearerHeader(_) | Self::Query(..) => "JWT",
        }
    }

    /// The request the client makes at `path`: the query string it carries and the headers it
    /// sends, as this presentation places the token.
    fn request(&self, path: &str) -> (String, Vec<(String, String)>) {
        match self {
            Self::Open | Self::BearerHeader(None) => (path.to_string(), Vec::new()),
            Self::BearerHeader(Some(token)) => (
                path.to_string(),
                vec![("authorization".to_string(), format!("Bearer {token}"))],
            ),
            Self::Query(name, token) => (format!("{path}?{name}={token}"), Vec::new()),
        }
    }
}

/// One HTTP row: the endpoint the control-plane client is configured with, the stage the
/// announced address is appended with, the gate in front of the route, and the outcome at that
/// address.
struct HttpRow {
    case: &'static str,
    /// The `Host` the control-plane calls carry — the endpoint the client is configured with.
    control_endpoint: &'static str,
    /// Whether that client resolves nothing but the emulator's own endpoint, which is what
    /// decides the address form the api is announced on.
    reaches_only_the_emulator: bool,
    stage: &'static str,
    gate: Gate,
    /// The status each request made at the announced address must be answered with.
    expected_status: u16,
    /// How many requests the row makes: a different number per row, so no row's count can stand
    /// in for another's.
    requests: usize,
}

fn http_rows() -> Vec<HttpRow> {
    vec![
        HttpRow {
            case: "the recorded shape: a client configured with the emulator's loopback \
                   endpoint, an open route on a named stage",
            control_endpoint: "127.0.0.1:45923",
            reaches_only_the_emulator: true,
            stage: "live",
            gate: Gate::Open,
            expected_status: SERVED,
            requests: 1,
        },
        HttpRow {
            case: "the same endpoint named by its loopback NAME, an open route on the stage \
                   that serves the api's root",
            control_endpoint: "localhost:9297",
            reaches_only_the_emulator: true,
            stage: ROOT_STAGE,
            gate: Gate::Open,
            expected_status: SERVED,
            requests: 2,
        },
        HttpRow {
            case: "a bracketed IPv6 loopback literal, a route whose JWT authorizer refuses a \
                   request presenting no token at all",
            control_endpoint: "[::1]:9297",
            reaches_only_the_emulator: true,
            stage: "live",
            gate: Gate::BearerHeader(None),
            expected_status: UNAUTHORIZED,
            requests: 3,
        },
        HttpRow {
            case: "a client that resolves AWS's own hosts, a route whose JWT authorizer \
                   refuses a query-string value that is not a token at all",
            control_endpoint: "apigateway.us-east-1.amazonaws.com",
            reaches_only_the_emulator: false,
            stage: "prod",
            gate: Gate::Query("access_token", "letmein"),
            expected_status: UNAUTHORIZED,
            requests: 4,
        },
    ]
}

/// Create the row's api, the integration its route fronts, the authorizer in front of that
/// route where the row has one, the route and an auto-deploying stage — every call made at the
/// endpoint the row's client is configured with. Answers the api's id and the address it
/// announced.
async fn deploy_http(
    router: &MultiServiceRouter,
    row: &HttpRow,
    name: &str,
    issuer: &str,
    audience: &str,
) -> (String, String) {
    let case = row.case;
    let endpoint = row.control_endpoint;
    let api = apigw(
        router,
        case,
        endpoint,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api_id = member(case, "CreateApi", &api, "apiId");
    let announced = member(case, "CreateApi", &api, "apiEndpoint");

    let integration = apigw(
        router,
        case,
        endpoint,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationMethod": "GET",
            "integrationUri": UPSTREAM,
            "payloadFormatVersion": "1.0"
        })),
        201,
    )
    .await;
    let integration_id = member(case, "CreateIntegration", &integration, "integrationId");

    let mut route = json!({
        "routeKey": ROUTE_KEY,
        "target": format!("integrations/{integration_id}"),
        "authorizationType": row.gate.authorization_type()
    });
    if let Some(identity_source) = row.gate.identity_source() {
        let authorizer = apigw(
            router,
            case,
            endpoint,
            "POST",
            &format!("/v2/apis/{api_id}/authorizers"),
            Some(json!({
                "name": "pool-authorizer",
                "authorizerType": "JWT",
                "identitySource": [identity_source],
                "jwtConfiguration": { "issuer": issuer, "audience": [audience] }
            })),
            201,
        )
        .await;
        route["authorizerId"] = json!(member(case, "CreateAuthorizer", &authorizer, "authorizerId"));
    }
    apigw(
        router,
        case,
        endpoint,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(route),
        201,
    )
    .await;

    apigw(
        router,
        case,
        endpoint,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": row.stage, "autoDeploy": true })),
        201,
    )
    .await;

    // Whichever operation reports the api reports the one address, so a client that reads the
    // value back dials what the minting call announced.
    let fetched = apigw(
        router,
        case,
        endpoint,
        "GET",
        &format!("/v2/apis/{api_id}"),
        None,
        200,
    )
    .await;
    assert_eq!(
        member(case, "GetApi", &fetched, "apiEndpoint"),
        announced,
        "case {case:?}: GetApi must report the address CreateApi announced, got {fetched}"
    );

    (api_id, announced)
}

/// Every request a client makes at the address an HTTP api announced reaches that api's own
/// route, is answered the outcome the api's records decide, and is counted in the stage's own
/// series under both dimension sets.
#[test]
fn an_announced_http_address_is_answered_and_what_it_answered_is_metered() {
    std::thread::Builder::new()
        .name("apigwv2-announced-http".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let router = emulator();
                let (issuer, audience) = pool_issuer(&router, "the pool a gated row names").await;
                for (index, row) in http_rows().iter().enumerate() {
                    let case = row.case;
                    let (api_id, announced) = deploy_http(
                        &router,
                        row,
                        &format!("announced-http-{index}"),
                        &issuer,
                        &audience,
                    )
                    .await;

                    // The address is the one the rule names for this row's client: the endpoint
                    // its own request reached with the api behind the reserved data-plane path,
                    // or the api's own documented `execute-api` host.
                    let (authority, base_path) = announced_parts(&announced);
                    let (expected_authority, expected_base_path) = if row.reaches_only_the_emulator
                    {
                        (
                            row.control_endpoint.to_string(),
                            format!("/_aws/execute-api/{api_id}"),
                        )
                    } else {
                        (
                            format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
                            String::new(),
                        )
                    };
                    assert_eq!(
                        (authority.as_str(), base_path.as_str()),
                        (expected_authority.as_str(), expected_base_path.as_str()),
                        "case {case:?}: the api must be announced on an address this client can \
                         dial, got {announced:?}"
                    );

                    // The invoke URL a client builds from that value: the stage name appended,
                    // and nothing appended at all for the stage that serves the api's root.
                    let invoke_path = if row.stage == ROOT_STAGE {
                        format!("{base_path}{ROUTE_PATH}")
                    } else {
                        format!("{base_path}/{}{ROUTE_PATH}", row.stage)
                    };
                    let (path, headers) = row.gate.request(&invoke_path);
                    let headers: Vec<(&str, &str)> = headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str()))
                        .collect();

                    for attempt in 0..row.requests {
                        let answered = dispatch(
                            &router,
                            "GET",
                            &authority,
                            &path,
                            &headers,
                            Bytes::new(),
                        )
                        .await;
                        assert_ne!(
                            answered.header("x-amzn-errortype").as_deref(),
                            Some(UNKNOWN_SERVICE),
                            "case {case:?}: request {attempt} at the announced address \
                             {authority}{path} must reach this api, but no service claimed it: \
                             {}",
                            answered.summary()
                        );
                        assert!(
                            answered.status < 500,
                            "case {case:?}: request {attempt} at the announced address \
                             {authority}{path} must be answered from the api's own records, not \
                             with a server error: {}",
                            answered.summary()
                        );
                        assert_eq!(
                            answered.status,
                            row.expected_status,
                            "case {case:?}: expected request {attempt} at {authority}{path} to \
                             be answered {}, got {}",
                            row.expected_status,
                            answered.summary()
                        );
                        if row.expected_status == SERVED {
                            assert_eq!(
                                answered.header("location").as_deref(),
                                Some(UPSTREAM),
                                "case {case:?}: a served request must be sent to the upstream \
                                 its integration names, got {}",
                                answered.summary()
                            );
                        } else {
                            assert_eq!(
                                answered.json().get("message").and_then(Value::as_str),
                                Some(UNAUTHORIZED_MESSAGE),
                                "case {case:?}: a refused request must carry the authorizer's \
                                 own sentence, got {}",
                                answered.summary()
                            );
                        }
                    }

                    // What the stage answered, in the series a dashboard reads: every request is
                    // one observation of `Count`, and the status class it ended in decides which
                    // error counter it lands in. Both dimension sets hold it.
                    let made = row.requests as f64;
                    let client_errors = if row.expected_status == SERVED { 0.0 } else { made };
                    let expected = [
                        total(COUNT, made),
                        total(CLIENT_ERRORS, client_errors),
                        total(SERVER_ERRORS, 0.0),
                    ];
                    reached(
                        &router,
                        case,
                        "the api's own series",
                        &api_scope(&api_id),
                        &expected,
                    )
                    .await;
                    reached(
                        &router,
                        case,
                        "the stage's series",
                        &stage_scope(&api_id, row.stage),
                        &expected,
                    )
                    .await;
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the check runs to completion");
}

/// The address a WEBSOCKET api announces is the socket it is served on: a correct handshake made
/// there is upgraded, a handshake the api's own records serve nothing for is refused with that
/// record's definite status, and the stage that carried a connection publishes it.
#[test]
fn an_announced_websocket_socket_upgrades_the_handshake_and_meters_the_connection() {
    std::thread::Builder::new()
        .name("apigwv2-announced-socket".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // `enable_all`: a WEBSOCKET api's data plane is a SOCKET the core opens when the api
            // is created, so the runtime this check runs on has to be able to bind and dial one.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let case = "one WebSocket api, two deployed stages and one stage with no \
                            deployment released to it";
                let router = emulator();
                let endpoint = "127.0.0.1:45923";

                let api = apigw(
                    &router,
                    case,
                    endpoint,
                    "POST",
                    "/v2/apis",
                    Some(json!({
                        "name": "announced-socket-api",
                        "protocolType": "WEBSOCKET",
                        "routeSelectionExpression": "$request.body.action"
                    })),
                    201,
                )
                .await;
                let api_id = member(case, "CreateApi", &api, "apiId");
                let announced = member(case, "CreateApi", &api, "apiEndpoint");
                let fetched = apigw(
                    &router,
                    case,
                    endpoint,
                    "GET",
                    &format!("/v2/apis/{api_id}"),
                    None,
                    200,
                )
                .await;
                assert_eq!(
                    member(case, "GetApi", &fetched, "apiEndpoint"),
                    announced,
                    "case {case:?}: GetApi must report the address CreateApi announced, got \
                     {fetched}"
                );

                // One `$default` route over a MOCK integration, so the api holds a route and the
                // connection request itself is served by the stage rather than by any `$connect`
                // integration.
                let integration = apigw(
                    &router,
                    case,
                    endpoint,
                    "POST",
                    &format!("/v2/apis/{api_id}/integrations"),
                    Some(json!({ "integrationType": "MOCK" })),
                    201,
                )
                .await;
                let integration_id =
                    member(case, "CreateIntegration", &integration, "integrationId");
                apigw(
                    &router,
                    case,
                    endpoint,
                    "POST",
                    &format!("/v2/apis/{api_id}/routes"),
                    Some(json!({
                        "routeKey": "$default",
                        "target": format!("integrations/{integration_id}")
                    })),
                    201,
                )
                .await;

                // Three stages: two with a deployment released to them, one without.
                for (stage, deploy) in [("live", true), ("quiet", true), ("pending", false)] {
                    apigw(
                        &router,
                        case,
                        endpoint,
                        "POST",
                        &format!("/v2/apis/{api_id}/stages"),
                        Some(json!({ "stageName": stage })),
                        201,
                    )
                    .await;
                    if deploy {
                        apigw(
                            &router,
                            case,
                            endpoint,
                            "POST",
                            &format!("/v2/apis/{api_id}/deployments"),
                            Some(json!({ "stageName": stage })),
                            201,
                        )
                        .await;
                    }
                }

                // The handshakes a client writes at the announced address: the path it addresses,
                // how many connections it opens there, and the answer the api's records owe each
                // one. The two deployed stages are dialled a DIFFERENT number of times, so
                // neither stage's counters can stand in for the other's and neither reads as a
                // flag.
                let on_live = 2usize;
                let on_quiet = 1usize;
                let rows: Vec<(&str, String, usize, u16)> = vec![
                    (
                        "a deployed stage upgrades the handshakes",
                        "/live".to_string(),
                        on_live,
                        SWITCHING_PROTOCOLS,
                    ),
                    (
                        "another deployed stage of the same api, dialled once",
                        "/quiet".to_string(),
                        on_quiet,
                        SWITCHING_PROTOCOLS,
                    ),
                    (
                        "a stage with no deployment released to it serves nothing",
                        "/pending".to_string(),
                        1,
                        FORBIDDEN,
                    ),
                    (
                        "a stage name the api does not hold serves nothing",
                        "/no-such-stage".to_string(),
                        1,
                        FORBIDDEN,
                    ),
                ];

                for (row, path, attempts, expected) in &rows {
                    for attempt in 0..*attempts {
                        let head =
                            answered_handshake_at(row, &announced, path, RFC_EXAMPLE_KEY).await;
                        assert!(
                            head.status < 500,
                            "case {row:?}: handshake {attempt} at the announced socket must be \
                             answered from the api's own records, not with a server error, got \
                             {} {:?}",
                            head.line,
                            head.fields
                        );
                        assert_eq!(
                            head.status, *expected,
                            "case {row:?}: expected handshake {attempt} answered {expected}, got \
                             {} {:?}",
                            head.line, head.fields
                        );
                        if *expected == SWITCHING_PROTOCOLS {
                            assert_eq!(
                                head.line, SWITCHING_PROTOCOLS_LINE,
                                "case {row:?}: expected the status line \
                                 {SWITCHING_PROTOCOLS_LINE:?}, got {:?}",
                                head.line
                            );
                            let upgrade = head.field("upgrade").unwrap_or_default();
                            assert!(
                                upgrade.eq_ignore_ascii_case("websocket"),
                                "case {row:?}: expected the Upgrade field to name websocket, got \
                                 {upgrade:?}"
                            );
                            let connection = head.field("connection").unwrap_or_default();
                            assert!(
                                connection.eq_ignore_ascii_case("upgrade"),
                                "case {row:?}: expected the Connection field to name Upgrade, \
                                 got {connection:?}"
                            );
                            assert_eq!(
                                head.field(ACCEPT_FIELD),
                                Some(RFC_EXAMPLE_ACCEPT),
                                "case {row:?}: expected the accept value RFC 6455 publishes for \
                                 the offered key {RFC_EXAMPLE_KEY:?}, got {:?}",
                                head.field(ACCEPT_FIELD)
                            );
                        }
                    }
                }

                // Each stage publishes the connections IT carried: the CONNECTION REQUEST is
                // itself a message the stage carried, so one `MessageCount` per connection opened
                // there — the accounting the owning crate's
                // `tests/all/websocket_stage_metrics.rs` fixes over the whole traffic of a
                // connection (each call dispatched to a route, the connection request and the
                // close included, and each frame written back) — while `ConnectCount` stays at
                // nothing, because it counts "the number of messages sent to the $connect route
                // integration" and this api fronts no `$connect` route. No client error either:
                // nothing refused a client. The two stages were dialled a different number of
                // times, so neither reading can be the other's.
                for (stage, opened) in [("live", on_live), ("quiet", on_quiet)] {
                    reached(
                        &router,
                        case,
                        &format!("the series of stage {stage:?}"),
                        &stage_scope(&api_id, stage),
                        &[
                            total(MESSAGE_COUNT, opened as f64),
                            total(CONNECT_COUNT, 0.0),
                            total(CLIENT_ERROR, 0.0),
                        ],
                    )
                    .await;
                }

                // And the stage no deployment was released to, which upgraded nothing, publishes
                // nothing at all: the series belong to the stage that carried the traffic.
                reached(
                    &router,
                    case,
                    "the series of the stage with no deployment",
                    &stage_scope(&api_id, "pending"),
                    &[total(MESSAGE_COUNT, 0.0), total(CONNECT_COUNT, 0.0)],
                )
                .await;
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the check runs to completion");
}
