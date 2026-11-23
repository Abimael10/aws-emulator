// SPDX-License-Identifier: CC0-1.0

//! What an `execute-api` refusal of an HTTP api does to the FUNCTION behind the route it
//! refused — and to the twin the refusal does not reach.
//!
//! RULE: **a request an HTTP api's `execute-api` endpoint refuses ITSELF — the `404`
//! `{"message":"Not Found"}` of an api whose `DisableExecuteApiEndpoint` is set, and the `429`
//! `{"message":"Too Many Requests"}` of a route the serving stage's `RouteSettings` throttle —
//! is answered by the gateway BEFORE the matched route's integration is reached, so the
//! function that integration names is not invoked for it and its log holds no record of it,
//! while the twin the refusal is not scoped to — another api over the same function, another
//! route of the same stage on its `DefaultRouteSettings` — is served by that same function in
//! the same instant; and lifting the refusal (`UpdateApi` clearing the member,
//! `DeleteRouteSettings` returning the route to the stage's default settings) puts the same
//! address back to being served by it.**
//!
//! `Api$DisableExecuteApiEndpoint` is "specifies whether clients can invoke your API by using
//! the default execute-api endpoint … To require that clients use a custom domain name to
//! invoke your API, disable the default endpoint", and `RouteSettings` carries
//! `ThrottlingRateLimit` ("specifies the throttling rate limit") beside `ThrottlingBurstLimit`
//! ("specifies the throttling burst limit"), which `DeleteRouteSettings`
//! (`DELETE /v2/apis/{ApiId}/stages/{StageName}/routesettings/{RouteKey}`) removes again. The
//! statuses and envelopes the data plane answers are recorded behaviour: the execute-api data
//! plane is not modelled in Smithy.
//!
//! Whether the function RAN is never read off an emulator internal here: the handler records
//! the per-request nonce it was given in its own CloudWatch Logs group, so a refused request is
//! shown not to have reached the backend by the ABSENCE of its nonce from that group — asserted
//! only once a nonce from the SAME instant HAS arrived there, so nothing waits on a span for
//! it. Four crates have to agree for that to hold — CloudFormation archives the inline handler,
//! IAM holds the execution role the log is written under, AWS Lambda runs the function and
//! delivers its log, API Gateway v2 refuses or serves the request — so the suite lives here
//! rather than in any one crate.
//!
//! What the owning crate's own suites pin and this one does not restate: the addressings,
//! protocols and stage shapes of the disabled endpoint
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/disabled_execute_api_endpoint.rs`,
//! `.../disabled_execute_api_endpoint_backend.rs`) and the burst arithmetic of a throttled
//! route (`.../http_api_throttle_burst.rs`, `.../src/state/route_throttle.rs`). What is added
//! here is the leg neither can carry: that the function behind the refused route is not invoked
//! at all, read from the function's own log, and that a per-ROUTE throttle leaves the twin route
//! of the same stage untouched and is lifted by `DeleteRouteSettings`.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_lambda_function_log_destination::{
    given_stack_function, logs_policy, messages_until_condition, trust_policy, FUNCTION_TIMEOUT,
};
// One path SEGMENT of a request target is percent-encoded with the crate's own encoder, which
// leaves only RFC 3986's unreserved characters literal — so a stage name and a route key that
// carry `$`, a space and a `/` address the very resource the control plane holds.
use crate::query_wire::form_encode;

/// The Region every call below is signed for, and so the Region the api, the function and the
/// role live in.
const REGION: &str = "us-east-1";

/// The status and sentence an HTTP api's endpoint answers a request it serves nothing for,
/// which is what every request to an api that turned its default endpoint off gets.
const NOT_FOUND: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Not Found";

/// The status and sentence a request the serving stage's route settings shed carries.
const TOO_MANY_REQUESTS: u16 = 429;
const TOO_MANY_REQUESTS_MESSAGE: &str = "Too Many Requests";

/// The status the handler answers with when it RUNS, and the body it returns: an answer
/// carrying them is one the function produced rather than one the gateway decided.
const SERVED: u16 = 200;
const SERVED_BODY: &str = "served-by-the-function";

/// The handler every api below fronts: it RECORDS the nonce of the request it was given in its
/// own log, and answers the backend's own body.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    nonce = (event.get("queryStringParameters") or {}).get("nonce") or "none"
    print("handled %s" % nonce)
    return {"statusCode": 200, "body": "served-by-the-function"}
"#;

/// The line the handler writes for the request carrying `nonce`.
fn handled(nonce: &str) -> String {
    format!("handled {nonce}")
}

/// The stage every api below deploys: the reserved stage "that serves an api's root", reached
/// with no stage segment in the path at all.
const DEFAULT_STAGE: &str = "$default";

/// The logical ids the template declares.
const ROLE_ID: &str = "ExecutionRole";
const FUNCTION_ID: &str = "Backend";

/// This service, the services that hold its route's function and that function's log, and the
/// CloudFormation that provisions them, over ONE engine.
///
/// The five services a case reaches are mounted here rather than the whole emulator
/// (`cloudformation_lambda_function_log_destination::emulator`), as the sibling suites that
/// drive one api's own data plane do: the cases below reach no other service, and assembling
/// every service per case costs stack and time without changing a single answer.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published address
    // carries none, as a client of a published HTTP api endpoint sends none.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One API Gateway v2 control-plane call, as `(status, parsed body)`.
async fn control(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/apigateway\
                 /aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// What an address answered: the status, and the body verbatim.
///
/// A DATA-PLANE answer, which is a different thing from the `Answered` of the Lambda and
/// CloudFormation callers this suite borrows: what a case reads off one is the gateway's
/// sentence or the function's own body, never a `FunctionError` header.
#[derive(Debug, Clone)]
struct Reached {
    status: u16,
    text: String,
}

impl Reached {
    /// The sentence a gateway envelope words its refusal with, or `""` for a body that is not
    /// one.
    fn message(&self) -> String {
        serde_json::from_str::<Value>(&self.text)
            .ok()
            .as_ref()
            .and_then(|body| body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Whether the body is the one the route's function returns.
    fn carries_the_functions_body(&self) -> bool {
        self.text == SERVED_BODY
    }
}

/// One request at the address an api announced, as a client of a published HTTP api endpoint
/// makes it: unsigned, carrying no credential, with the nonce this request is known by.
async fn invoke(
    router: &MultiServiceRouter,
    authority: &str,
    path: &str,
    nonce: &str,
) -> Reached {
    let uri = format!("https://{authority}{path}?nonce={nonce}");
    let request = http::Request::builder()
        .method("GET")
        .uri(&uri)
        .header("host", authority)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Reached {
        status: response.status().as_u16(),
        text: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// The template the backend is deployed as: the execution role, granted the log delivery of
/// AWS's own basic execution policy, and the function itself.
fn function_stack(function: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "basic-execution",
                        "PolicyDocument": logs_policy()
                    }]
                }
            },
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE }
                }
            }
        },
        "Outputs": {
            // "Ref returns the function name", which is the id every call below is made with.
            "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
        }
    })
    .to_string()
}

/// Deploy the backend as a stack and answer `(function name, the ARN its integration names)`,
/// once AWS Lambda reports it invocable. The deploy-and-wait is the crate's own
/// ([`given_stack_function`]); a template is what archives the inline handler into a deployment
/// package this emulator runs, and the function's ARN is composed by the tree's own ARN
/// composer from the name the stack published.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    function: &str,
) -> (String, String) {
    let deployed = given_stack_function(router, case, stack, &function_stack(function)).await;
    let arn = engine::emulator_arn("lambda", REGION, &format!("function:{deployed}"));
    (deployed, arn)
}

/// Grant the gateway the invocation of `function`, narrowed to ONE api's own `execute-api`
/// ARN — the `AddPermission` a client makes beside each api it fronts a route with.
async fn gateway_may_invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    api_id: &str,
) {
    let account = engine::emulator_account_id();
    let granted = crate::cloudformation_lambda_function_log_destination::lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": format!("apigateway-{api_id}"),
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!("arn:aws:execute-api:{REGION}:{account}:{api_id}/*/*/*"),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "case {case:?}: AddPermission for {api_id} must answer 201, got {} with {:?}",
        granted.status,
        granted.text()
    );
}

/// One api of a case: its id, and the authority and base path a client of it addresses.
struct Api {
    api_id: String,
    authority: String,
    base: String,
}

/// An HTTP api whose `routes` all front one `AWS_PROXY` integration over `function_arn`, with
/// the `stage` the case states, granted the invocation of that function.
async fn given_api(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    function: &str,
    function_arn: &str,
    routes: &[&str],
    stage: Value,
) -> Api {
    let (status, api) = control(
        router,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {status} {api}"
    );
    let api_id = member(case, &api, "apiId");
    let announced = member(case, &api, "apiEndpoint");

    let (status, integration) = control(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_arn,
            "payloadFormatVersion": "2.0",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {status} {integration}"
    );
    let integration_id = member(case, &integration, "integrationId");

    for route in routes {
        let (status, created) = control(
            router,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": route,
                "target": format!("integrations/{integration_id}"),
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateRoute {route:?} must answer 201, got {status} {created}"
        );
    }

    let (status, created) = control(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(stage),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {status} {created}"
    );

    gateway_may_invoke(router, case, function, &api_id).await;
    let (authority, base) =
        crate::apigatewayv2_published_endpoint_runs_its_lambda_route::announced_parts(&announced);
    Api {
        api_id,
        authority,
        base,
    }
}

/// One string member of an answer.
fn member(case: &str, answer: &Value, name: &str) -> String {
    answer
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no {name}: {answer}"))
        .to_string()
}

/// Set `DisableExecuteApiEndpoint` on `api_id` and read the member back off the answer, so each
/// step starts from a value the control plane reports.
async fn set_disabled(router: &MultiServiceRouter, case: &str, api_id: &str, disabled: bool) {
    let (status, updated) = control(
        router,
        "PATCH",
        &format!("/v2/apis/{api_id}"),
        Some(json!({ "disableExecuteApiEndpoint": disabled })),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: UpdateApi must succeed, got {status} {updated}"
    );
    assert_eq!(
        updated
            .get("disableExecuteApiEndpoint")
            .and_then(Value::as_bool),
        Some(disabled),
        "case {case:?}: UpdateApi must report the member it was given, got {updated}"
    );
}

/// The group the backend's own log lands in: "Lambda functions send logs to a default log group
/// named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
fn log_group(function: &str) -> String {
    format!("/aws/lambda/{function}")
}

/// Everything the function's log holds, once it holds the line for `nonce` — the request the
/// case already knows the backend served. Waiting for THAT line is what makes a later absence
/// meaningful without waiting on a span for it.
async fn log_once_it_holds(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    nonce: &str,
) -> Vec<String> {
    let line = handled(nonce);
    let held = messages_until_condition(router, &log_group(function), |held| {
        held.iter().any(|message| is_the_line(message, &line))
    })
    .await;
    assert!(
        held.iter().any(|message| is_the_line(message, &line)),
        "case {case:?}: expected the backend's log to hold {line:?} for the request it served, \
         got {held:?}"
    );
    held
}

/// Whether `message` is the WHOLE line the handler wrote for one request, rather than a line
/// that merely starts with it: the nonces of a burst share a prefix (`…-1` and `…-11`), so an
/// absence read with a prefix match would be no absence at all.
fn is_the_line(message: &str, line: &str) -> bool {
    message.trim_end() == line
}

/// Assert the function was NOT invoked for the request known by `nonce`: its log holds no line
/// for it.
fn not_invoked_for(case: &str, held: &[String], nonce: &str) {
    let line = handled(nonce);
    assert!(
        !held.iter().any(|message| is_the_line(message, &line)),
        "case {case:?}: a refused request never reaches the route's function, so its log must \
         hold no {line:?}, got {held:?}"
    );
}

/// The nonce the request of `case` at `label` is known by: unique per request, so one line of
/// the backend's log names one request.
fn nonce(case_index: usize, label: &str, attempt: usize) -> String {
    format!("n{case_index}-{label}-{attempt}")
}

/// One row of the disabled-endpoint table: the path the client asks for on each api.
struct DisabledRow {
    case: &'static str,
    /// The route key both apis of the row carry.
    route_key: &'static str,
    /// The path within the stage a client asks for.
    path: &'static str,
}

/// The rows span what the rule generalises over for this refusal: the route key the apis carry
/// (a literal path, and a greedy one), and so the path the client asks for.
const DISABLED_ROWS: &[DisabledRow] = &[
    DisabledRow {
        case: "a literal route key, asked for at its own path",
        route_key: "GET /hit",
        path: "/hit",
    },
    DisabledRow {
        case: "a greedy route key, asked for at a path it catches",
        route_key: "ANY /{proxy+}",
        path: "/anything/at/all",
    },
];

/// RULE (the disabled endpoint): while the member is set the api's own endpoint answers `404`
/// `{"message":"Not Found"}` and the route's function is NOT invoked for the request, the twin
/// api over the same function keeps being served by it in the same instant, and clearing the
/// member puts the same address back to being served.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disabled_default_endpoint_refuses_without_invoking_the_routes_function() {
    let router = emulator();
    let (function, arn) = given_function(
        &router,
        "setup",
        "apigwv2-refused-invoke-endpoint-stack",
        "apigwv2-refused-invoke-endpoint",
    )
    .await;
    let function = function.as_str();

    for (index, row) in DISABLED_ROWS.iter().enumerate() {
        let case = row.case;
        let stage = json!({ "stageName": DEFAULT_STAGE, "autoDeploy": true });
        let target = given_api(
            &router,
            case,
            "refused-invoke-target",
            function,
            &arn,
            &[row.route_key],
            stage.clone(),
        )
        .await;
        let twin = given_api(
            &router,
            case,
            "refused-invoke-twin",
            function,
            &arn,
            &[row.route_key],
            stage,
        )
        .await;

        // Both apis are served by the same function to begin with, and its log names both
        // requests: the backend is reachable through either endpoint.
        for (label, api) in [("target", &target), ("twin", &twin)] {
            let known = nonce(index, label, 0);
            let answered = invoke(
                &router,
                &api.authority,
                &format!("{}{}", api.base, row.path),
                &known,
            )
            .await;
            assert_eq!(
                answered.status, SERVED,
                "case {case:?}: expected the {label} api served {SERVED} by its function, got \
                 {answered:?}"
            );
            assert!(
                answered.carries_the_functions_body(),
                "case {case:?}: expected the {label} api answered by the function itself, got \
                 {answered:?}"
            );
            log_once_it_holds(&router, case, function, &known).await;
        }

        // The target turns its own default endpoint off.
        set_disabled(&router, case, &target.api_id, true).await;
        let refused_nonce = nonce(index, "target-refused", 1);
        let refused = invoke(
            &router,
            &target.authority,
            &format!("{}{}", target.base, row.path),
            &refused_nonce,
        )
        .await;
        assert_eq!(
            refused.status, NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND} on a disabled default endpoint, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            NOT_FOUND_MESSAGE,
            "case {case:?}: expected the {NOT_FOUND_MESSAGE:?} envelope, got {refused:?}"
        );
        assert!(
            !refused.carries_the_functions_body(),
            "case {case:?}: a refusal carries nothing of the function behind the route, got \
             {refused:?}"
        );

        // The twin over the same function answers in the same instant, and its line arriving
        // in the log is what says the log has caught up with this moment — so the refused
        // request's absence from it is an absence and not a race.
        let twin_nonce = nonce(index, "twin-while-disabled", 1);
        let served = invoke(
            &router,
            &twin.authority,
            &format!("{}{}", twin.base, row.path),
            &twin_nonce,
        )
        .await;
        assert_eq!(
            served.status, SERVED,
            "case {case:?}: expected the twin api still served by the function, got {served:?}"
        );
        assert!(
            served.carries_the_functions_body(),
            "case {case:?}: expected the twin's answer from the function itself, got {served:?}"
        );
        let held = log_once_it_holds(&router, case, function, &twin_nonce).await;
        not_invoked_for(case, &held, &refused_nonce);

        // Clearing the member puts the SAME address back to being served by the function, with
        // no redeployment of the stage.
        set_disabled(&router, case, &target.api_id, false).await;
        let restored_nonce = nonce(index, "target-restored", 2);
        let restored = invoke(
            &router,
            &target.authority,
            &format!("{}{}", target.base, row.path),
            &restored_nonce,
        )
        .await;
        assert_eq!(
            restored.status, SERVED,
            "case {case:?}: expected the restored endpoint served {SERVED}, got {restored:?}"
        );
        assert!(
            restored.carries_the_functions_body(),
            "case {case:?}: expected the restored endpoint answered by the function, got \
             {restored:?}"
        );
        log_once_it_holds(&router, case, function, &restored_nonce).await;
    }
}

/// One row of the throttled-route table: the pair the stage states for the limited route, and
/// how many requests arrive at it together.
struct ThrottleRow {
    case: &'static str,
    rate: f64,
    burst: i32,
    submissions: usize,
}

/// The rows span what the rule generalises over for this refusal: how wide the burst the route's
/// own settings state is, and how many requests meet it.
const THROTTLE_ROWS: &[ThrottleRow] = &[
    ThrottleRow {
        case: "a route metered at one request a second with a burst of one, met by twelve at once",
        rate: 1.0,
        burst: 1,
        submissions: 12,
    },
    ThrottleRow {
        case: "a route metered at one request a second with a burst of two, met by six at once",
        rate: 1.0,
        burst: 2,
        submissions: 6,
    },
];

/// The route the stage throttles, the twin route it leaves on its default settings, and the
/// paths a client reaches them at.
const LIMITED_ROUTE: &str = "GET /limited";
const LIMITED_PATH: &str = "/limited";
const TWIN_ROUTE: &str = "GET /twin";
const TWIN_PATH: &str = "/twin";

/// The `DefaultRouteSettings` the stage states, generous enough that the twin route is never
/// shed by them.
const STAGE_RATE: f64 = 500.0;
const STAGE_BURST: i32 = 200;

/// How many times a case looks again for the throttled route to serve before it gives up, and
/// how long it waits between looks: the budget is far longer than the second a rate of one
/// request a second takes to refill the bucket, so a loaded host is slower without the rule
/// being any different, and nothing here asserts on elapsed time.
const SERVES_AGAIN_LOOKS: usize = 300;
const LOOK_AGAIN_IN: std::time::Duration = std::time::Duration::from_millis(100);

/// Look again at the throttled route until it SERVES — the condition a rate limiter owes once
/// its rate has refilled the bucket, rather than a span — and answer the nonce of the request
/// it served. Every look before that one is shed, which is the only other answer the route has.
async fn serves_again(
    router: &MultiServiceRouter,
    case: &str,
    api: &Api,
    case_index: usize,
) -> String {
    let at = format!("{}{LIMITED_PATH}", api.base);
    let mut last = None;
    for attempt in 0..SERVES_AGAIN_LOOKS {
        let known = nonce(case_index, "served-again", attempt);
        let answered = invoke(router, &api.authority, &at, &known).await;
        if answered.status == SERVED {
            assert!(
                answered.carries_the_functions_body(),
                "case {case:?}: expected the route served again by its function, got {answered:?}"
            );
            return known;
        }
        assert_eq!(
            answered.status, TOO_MANY_REQUESTS,
            "case {case:?}: a throttled route is either served or shed, got {answered:?}"
        );
        last = Some(answered);
        tokio::time::sleep(LOOK_AGAIN_IN).await;
    }
    panic!(
        "case {case:?}: expected the throttled route served again once its rate refilled the \
         bucket, last answer {last:?}"
    )
}

/// Send one request per nonce to `path` AT ONCE, each from its own task, and answer them paired
/// with the nonce the request carried.
async fn at_once(
    router: &Arc<MultiServiceRouter>,
    api: &Api,
    path: &str,
    nonces: &[String],
) -> Vec<(String, Reached)> {
    let mut pending = Vec::with_capacity(nonces.len());
    for known in nonces {
        let router = Arc::clone(router);
        let authority = api.authority.clone();
        let at = format!("{}{path}", api.base);
        let known = known.clone();
        pending.push(tokio::spawn(async move {
            let answered = invoke(&router, &authority, &at, &known).await;
            (known, answered)
        }));
    }
    let mut answers = Vec::with_capacity(nonces.len());
    for task in pending {
        answers.push(task.await.expect("the request answered"));
    }
    answers
}

/// RULE (the throttled route): of the requests arriving together at a route the stage's
/// `RouteSettings` throttle, the excess is shed `429` `{"message":"Too Many Requests"}` without
/// the route's function being invoked for it, the twin route of the same stage sheds nothing in
/// the same instant, and `DeleteRouteSettings` returns the route to the stage's default
/// settings.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_throttled_route_sheds_without_invoking_its_function_while_its_twin_route_serves() {
    let router = Arc::new(emulator());
    let (function, arn) = given_function(
        &router,
        "setup",
        "apigwv2-refused-invoke-throttle-stack",
        "apigwv2-refused-invoke-throttle",
    )
    .await;
    let function = function.as_str();

    for (index, row) in THROTTLE_ROWS.iter().enumerate() {
        let case = row.case;
        let api = given_api(
            &router,
            case,
            "refused-invoke-throttled",
            function,
            &arn,
            &[LIMITED_ROUTE, TWIN_ROUTE],
            json!({
                "stageName": DEFAULT_STAGE,
                "autoDeploy": true,
                "defaultRouteSettings": {
                    "throttlingRateLimit": STAGE_RATE,
                    "throttlingBurstLimit": STAGE_BURST
                },
                "routeSettings": {
                    LIMITED_ROUTE: {
                        "throttlingRateLimit": row.rate,
                        "throttlingBurstLimit": row.burst
                    }
                }
            }),
        )
        .await;

        // The stage serves both routes from the function before any burst: the throttle is what
        // the burst below shows, not an endpoint that serves nothing.
        for (label, path) in [("limited", LIMITED_PATH), ("twin", TWIN_PATH)] {
            let known = nonce(index, label, 0);
            let answered = invoke(
                &router,
                &api.authority,
                &format!("{}{path}", api.base),
                &known,
            )
            .await;
            assert_eq!(
                answered.status, SERVED,
                "case {case:?}: expected {label} served {SERVED} by its function before the \
                 burst, got {answered:?}"
            );
            assert!(
                answered.carries_the_functions_body(),
                "case {case:?}: expected {label} answered by the function itself, got {answered:?}"
            );
            log_once_it_holds(&router, case, function, &known).await;
        }

        // The burst: every request of it arrives at the limited route before any of them is
        // answered, and the same number arrives at the twin route of the same stage.
        let limited_nonces: Vec<String> = (0..row.submissions)
            .map(|at| nonce(index, "limited-burst", at + 1))
            .collect();
        let twin_nonces: Vec<String> = (0..row.submissions)
            .map(|at| nonce(index, "twin-burst", at + 1))
            .collect();
        let limited = at_once(&router, &api, LIMITED_PATH, &limited_nonces).await;
        let twin = at_once(&router, &api, TWIN_PATH, &twin_nonces).await;

        let shed: Vec<&(String, Reached)> = limited
            .iter()
            .filter(|(_, answered)| answered.status == TOO_MANY_REQUESTS)
            .collect();
        let statuses: Vec<u16> = limited
            .iter()
            .map(|(_, answered)| answered.status)
            .collect();
        assert!(
            !shed.is_empty(),
            "case {case:?}: expected the excess of {} requests at once shed \
             {TOO_MANY_REQUESTS}, got {statuses:?}",
            row.submissions
        );
        for (known, answered) in &limited {
            if answered.status == TOO_MANY_REQUESTS {
                assert_eq!(
                    answered.message(),
                    TOO_MANY_REQUESTS_MESSAGE,
                    "case {case:?}: expected the {TOO_MANY_REQUESTS_MESSAGE:?} envelope for the \
                     shed request {known}, got {answered:?}"
                );
                assert!(
                    !answered.carries_the_functions_body(),
                    "case {case:?}: a shed request carries nothing of the route's function, got \
                     {answered:?}"
                );
                continue;
            }
            assert_eq!(
                answered.status, SERVED,
                "case {case:?}: a request the throttle admitted is served by the function, got \
                 {answered:?} for {known}"
            );
            assert!(
                answered.carries_the_functions_body(),
                "case {case:?}: expected the admitted request answered by the function, got \
                 {answered:?}"
            );
        }
        // The throttle is the LIMITED route's: the twin route of the same stage, on its
        // default settings, sheds nothing in the same instant.
        for (known, answered) in &twin {
            assert_eq!(
                answered.status, SERVED,
                "case {case:?}: the twin route of the same stage sheds nothing, got {answered:?} \
                 for {known}"
            );
            assert!(
                answered.carries_the_functions_body(),
                "case {case:?}: expected the twin route answered by the function, got {answered:?}"
            );
        }

        // The function ran for every request that was SERVED and for none that was shed. The
        // log is read once it holds the line of a request the route served AFTER the burst — a
        // throttled route is a rate limiter rather than a closed gate, so looking again until
        // it serves is looking for that condition — and the shed requests' absence from the log
        // it holds then is an absence rather than a race.
        let after_the_burst = serves_again(&router, case, &api, index).await;
        let held = log_once_it_holds(&router, case, function, &after_the_burst).await;
        for (known, answered) in &limited {
            if answered.status == TOO_MANY_REQUESTS {
                not_invoked_for(case, &held, known);
            }
        }

        // DeleteRouteSettings returns the route to the stage's own default settings, which this
        // burst is well inside — so nothing of it is shed, and the function ran for all of it.
        // The route key and the stage name travel in the path, so they are encoded from the very
        // constants the stage was created with rather than spelled a second way.
        let (status, deleted) = control(
            &router,
            "DELETE",
            &format!(
                "/v2/apis/{}/stages/{}/routesettings/{}",
                api.api_id,
                form_encode(DEFAULT_STAGE),
                form_encode(LIMITED_ROUTE)
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 204,
            "case {case:?}: DeleteRouteSettings must answer 204, got {status} {deleted}"
        );
        let after_nonces: Vec<String> = (0..row.submissions)
            .map(|at| nonce(index, "after-delete", at + 1))
            .collect();
        let after = at_once(&router, &api, LIMITED_PATH, &after_nonces).await;
        for (known, answered) in &after {
            assert_eq!(
                answered.status, SERVED,
                "case {case:?}: once the route's own settings are deleted the stage's defaults \
                 shed nothing, got {answered:?} for {known}"
            );
            assert!(
                answered.carries_the_functions_body(),
                "case {case:?}: expected the request answered by the function, got {answered:?}"
            );
        }
        let last = after_nonces
            .last()
            .expect("the burst after the delete carries requests");
        log_once_it_holds(&router, case, function, last).await;
    }
}
