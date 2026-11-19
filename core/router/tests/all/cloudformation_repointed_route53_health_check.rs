// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack UPDATE that repoints its `AWS::Route53::HealthCheck` at another path
//! of the API Gateway endpoint the same stack deploys moves what every Route 53 health checker
//! reports about that check.
//!
//! The rule under test: **`HealthCheckConfig$ResourcePath` is "the path, if any, that you want
//! Amazon Route 53 to request when performing health checks" — a PROBE TARGET re-requested
//! every `RequestInterval`, not a string the service only stores — so every
//! `HealthCheckObservations[].StatusReport.Status` describes the LATEST probe of the path the
//! check's CURRENT configuration names: the description opens with the fixed text `Success: `
//! while that path answers inside the range the request types accept (`HealthCheckConfig$Type`
//! — Route 53 "submits an HTTP request and waits for an HTTP status code of 200 or greater and
//! less than 400") and with the fixed text `Failure: ` when it answers outside it, naming in
//! both cases the status the probe observed. Any write that changes `ResourcePath` — including
//! the `UpdateHealthCheck` CloudFormation applies a changed declaration of that property
//! through — therefore moves the probe, so one health check reports one fact about one endpoint
//! across `GetHealthCheckStatus`, `GetHealthCheckLastFailureReason` and the `AWS/Route53`
//! `HealthCheckStatus` series.**
//!
//! Four crates have to agree for that to hold — CloudFormation provisions the health check
//! beside the api and applies a changed `ResourcePath` to the live one, API Gateway serves the
//! probed endpoint, Route 53 probes it and reports what it observed, and CloudWatch answers for
//! the series — so the suite lives here and drives the emulator as it is ASSEMBLED. A
//! crate-local suite can see at most one side of it: the sibling
//! `service-…-route53-…/tests/all/health_check_repointed_endpoint` pins the same rule for a
//! check repointed by a direct `UpdateHealthCheck` over a static website, and what only this
//! suite reaches is the deployment path a client actually takes — the health check and the
//! endpoint it monitors declared in ONE stack, and the repointing applied as a stack update of
//! a template parameter.
//!
//! The phases differ along what the rule generalises over: whether the probed path is one the
//! deployed api serves or one it does not define, and WHICH status the served path answers —
//! the lowest status the request types accept, another inside the range, and one outside it
//! that the endpoint answers perfectly well. They ALTERNATE between accepted and rejected
//! answers, so the description a phase waits for can only come from a probe of the path that
//! phase's stack update stored, never from the phase before it.
//!
//! Every expected description is computed from what the deployed api answers an ordinary
//! anonymous client at that path — fetched here exactly as a checker's probe fetches it — and
//! never from a value written into the suite; the `200` phase is additionally held to the whole
//! wording the service is recorded reporting for an answered 200.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources belong
/// to and the health checkers probe.
const REGION: &str = "us-east-1";

/// The stack statuses this suite waits on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The stack, and the template parameter whose value is the health check's `ResourcePath`: the
/// one declaration a phase's stack update changes.
const STACK: &str = "route53-cfn-repointed-health-check";
const PROBE_PATH_PARAMETER: &str = "ProbePath";

/// The stage the api is deployed to, and the verb its methods answer.
const STAGE: &str = "prod";
const VERB: &str = "GET";

/// The probe cadence the health check is declared with: the fastest published `RequestInterval`
/// (`range 10..30`) and the smallest `FailureThreshold` (`range 1..10`), so one probe decides
/// both what the checkers report and the status the series carries.
const REQUEST_INTERVAL: i64 = 10;
const FAILURE_THRESHOLD: i64 = 1;

/// The lowest and the first excluded HTTP status of an answer the request types accept:
/// `HealthCheckConfig$Type` — Route 53 "waits for an HTTP status code of 200 or greater and
/// less than 400".
const LOWEST_ACCEPTED_STATUS: u16 = 200;
const FIRST_REJECTED_STATUS: u16 = 400;

/// What a checker's `StatusReport$Status` opens with when the probe it describes met the
/// criterion its type sets, and what it opens with when the probe did not. Fixed text, and the
/// one requirement the model's free-text `StatusReport$Status` leaves a client to branch on.
const SUCCESS_OPENING: &str = "Success: ";
const FAILURE_OPENING: &str = "Failure: ";

/// The whole description the service is recorded reporting for a probe that was answered the
/// lowest status the request types accept.
const ANSWERED_OK_DESCRIPTION: &str = "Success: HTTP Status Code 200, OK";

/// The series one health check's aggregate assessment is published as.
const NAMESPACE: &str = "AWS/Route53";
const METRIC: &str = "HealthCheckStatus";
const DIMENSION: &str = "HealthCheckId";

/// The two values that series carries: `1` while the check is considered healthy and `0` while
/// it is considered unhealthy.
const HEALTHY: f64 = 1.0;
const UNHEALTHY: f64 = 0.0;

/// How long a phase waits for the condition it is about — the checkers reporting the probe of
/// the path that phase stored, and the series carrying the assessment folded from it — and how
/// often it looks.
///
/// Generous on purpose: what is waited for is the CONDITION, and the deadline is only here so a
/// failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The emulator as it is ASSEMBLED, on one engine: the api CloudFormation deploys is the api
/// Route 53's checkers probe.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Epoch seconds, the unit a CloudWatch timestamp is read and written in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs_f64()
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Route 53 operation over its restXml wire, requiring HTTP 200.
async fn route53(router: &MultiServiceRouter, case: &str, uri: &str) -> String {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "route53.amazonaws.com")
        .header("content-type", "application/xml")
        .header("authorization", authorization("route53"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let read = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Route 53 GET {uri}, got {status} with body {read}"
    );
    read
}

/// Call one CloudWatch operation over its awsJson wire, requiring HTTP 200.
async fn cloudwatch(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header("authorization", authorization("monitoring"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with body \
         {parsed}"
    );
    parsed
}

/// One ANONYMOUS request to the deployed api's own invoke endpoint — the endpoint the health
/// check monitors, fetched here exactly as a client reaches AWS and exactly as a checker's probe
/// does: the host is `{rest-api-id}.execute-api.{region}.amazonaws.com` and no credential is
/// carried at all.
async fn invoke(router: &MultiServiceRouter, api_id: &str, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(VERB)
        .uri(path)
        .header(
            "host",
            format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        )
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// What the deployed api answers an anonymous client asking for `path`, once the stage answers
/// it at all, with the phase's own expectation about that answer checked first: the oracle this
/// suite compares the checkers against is the endpoint's REAL answer, so it is pinned before it
/// is used.
async fn answered_status(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    path: &str,
    accepted: bool,
) -> u16 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = invoke(router, api_id, path).await;
        let inside = (LOWEST_ACCEPTED_STATUS..FIRST_REJECTED_STATUS).contains(&status);
        if inside == accepted || Instant::now() >= deadline {
            assert_eq!(
                inside, accepted,
                "case {case:?}: expected the deployed api to answer {path:?} {} the range the \
                 request types accept, got {status} with body {body}",
                if accepted { "inside" } else { "outside" }
            );
            return status;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The description a checker reports for an answer of `status`: the opening the probe's verdict
/// is reported under, and the status as the status line names it.
fn description_of(status: u16) -> String {
    let opening = if (LOWEST_ACCEPTED_STATUS..FIRST_REJECTED_STATUS).contains(&status) {
        SUCCESS_OPENING
    } else {
        FAILURE_OPENING
    };
    match http::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
    {
        Some(reason) => format!("{opening}HTTP Status Code {status}, {reason}"),
        None => format!("{opening}HTTP Status Code {status}"),
    }
}

/// The statuses every checker of `id` is reporting right now (`GetHealthCheckStatus`).
async fn reported_statuses(router: &MultiServiceRouter, case: &str, id: &str) -> Vec<String> {
    let read = route53(router, case, &format!("/2013-04-01/healthcheck/{id}/status")).await;
    elements(&read, "Status")
}

/// The statuses the checkers of `id` are reporting as a last failure reason
/// (`GetHealthCheckLastFailureReason`).
async fn last_failure_statuses(router: &MultiServiceRouter, case: &str, id: &str) -> Vec<String> {
    let read = route53(
        router,
        case,
        &format!("/2013-04-01/healthcheck/{id}/lastfailurereason"),
    )
    .await;
    elements(&read, "Status")
}

/// The `ResourcePath` and `HealthCheckVersion` `GetHealthCheck` reports for `id`: what says the
/// stack update's reconfiguration LANDED, independently of what the checkers report.
async fn configured_path_and_version(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> (String, i64) {
    let read = route53(router, case, &format!("/2013-04-01/healthcheck/{id}")).await;
    let path = element(&read, "ResourcePath");
    let version = element(&read, "HealthCheckVersion")
        .parse()
        .unwrap_or_else(|_| {
            panic!("case {case:?}: HealthCheckVersion must be a number; body {read}")
        });
    (path, version)
}

/// Wait until every checker of `id` reports `expected`, and answer the statuses they report.
async fn statuses_settling_on(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    expected: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = reported_statuses(router, case, id).await;
        let settled = !reported.is_empty() && reported.iter().all(|status| status == expected);
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected every checker to report {expected:?}, got {reported:?}"
            );
            return reported;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The value of the LATEST datapoint of `id`'s status series, or `None` while the series holds
/// none.
///
/// The period is the health check's own `RequestInterval`, so one window holds one assessment,
/// which is asserted through the window's own `Minimum` and `Maximum` rather than assumed.
async fn latest_assessment(router: &MultiServiceRouter, case: &str, id: &str) -> Option<f64> {
    let now = now_secs();
    let reported = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": DIMENSION, "Value": id}],
            "StartTime": now - 1_800.0,
            "EndTime": now + 1_800.0,
            "Period": REQUEST_INTERVAL,
            "Statistics": ["Minimum", "Maximum"],
        }),
    )
    .await;
    let points = reported["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut latest: Option<(f64, f64)> = None;
    for point in &points {
        let stamp = point["Timestamp"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: every datapoint carries a Timestamp, got {reported}")
        });
        let minimum = point["Minimum"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: every datapoint carries a Minimum, got {reported}")
        });
        let maximum = point["Maximum"].as_f64().unwrap_or_default();
        assert_eq!(
            minimum, maximum,
            "case {case:?}: one assessment's Minimum and Maximum are the same value, got {point}"
        );
        if latest.is_none_or(|(held, _)| stamp > held) {
            latest = Some((stamp, minimum));
        }
    }
    latest.map(|(_, value)| value)
}

/// Wait until the latest datapoint of `id`'s series reads `expected`.
async fn series_settling_on(router: &MultiServiceRouter, case: &str, id: &str, expected: f64) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let latest = latest_assessment(router, case, id).await;
        if latest == Some(expected) || Instant::now() >= deadline {
            assert_eq!(
                latest,
                Some(expected),
                "case {case:?}: expected the latest {NAMESPACE} {METRIC} datapoint of the health \
                 check to read {expected}, got {latest:?}"
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The resource one served path is declared as, under the api's root.
fn declared_resource(path_part: &str) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Resource",
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
            "PathPart": path_part
        }
    })
}

/// The method that answers one served path with `status`.
///
/// A `MOCK` is the one integration type that needs no backend — "for testing without actually
/// invoking the backend" — and the `statusCode` its rendered request template names is the
/// result the integration response is selected by, so the api answers each path the status the
/// phase probing it expects of the endpoint.
fn declared_method(resource_id: &str, status: u16) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Method",
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "ResourceId": { "Ref": resource_id },
            "HttpMethod": VERB,
            "AuthorizationType": "NONE",
            "MethodResponses": [{ "StatusCode": status.to_string() }],
            "Integration": {
                "Type": "MOCK",
                "RequestTemplates": {
                    "application/json": format!(r#"{{"statusCode":{status}}}"#)
                },
                "IntegrationResponses": [{
                    "StatusCode": status.to_string(),
                    "ResponseTemplates": {
                        "application/json": r#"{"answered":"the mock"}"#
                    }
                }]
            }
        }
    })
}

/// The template the stack is deployed and updated from: the api whose paths the phases probe,
/// its deployment and stage, and the health check that monitors it — whose `ResourcePath` is
/// the one template PARAMETER, which is how a client's stack update repoints a live check.
fn template() -> String {
    let mut resources = json!({
        "Api": {
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{STACK}-api"),
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        }
    });
    let declarations = resources.as_object_mut().expect("an object");
    let mut methods = Vec::new();
    for (path_part, status) in SERVED_PATHS {
        let resource_id = format!("Resource{path_part}");
        let method_id = format!("Method{path_part}");
        declarations.insert(resource_id.clone(), declared_resource(path_part));
        declarations.insert(method_id.clone(), declared_method(&resource_id, *status));
        methods.push(method_id);
    }
    // A deployment is the snapshot taken when it is created, so every method has to exist
    // before that snapshot is taken to be in it.
    declarations.insert(
        "Deployment".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": methods,
            "Properties": { "RestApiId": { "Ref": "Api" } }
        }),
    );
    declarations.insert(
        "Stage".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": "Deployment" },
                "StageName": STAGE
            }
        }),
    );
    declarations.insert(
        "Check".to_string(),
        json!({
            "Type": "AWS::Route53::HealthCheck",
            // The endpoint has to be serving before the checkers are pointed at it, which is
            // what a client declaring both in one stack states with the dependency.
            "DependsOn": ["Stage"],
            "Properties": {
                "HealthCheckConfig": {
                    "Type": "HTTPS",
                    "FullyQualifiedDomainName": {
                        "Fn::Sub": format!("${{Api}}.execute-api.{REGION}.amazonaws.com")
                    },
                    "Port": 443,
                    "ResourcePath": { "Ref": PROBE_PATH_PARAMETER },
                    "RequestInterval": REQUEST_INTERVAL,
                    "FailureThreshold": FAILURE_THRESHOLD
                }
            }
        }),
    );
    json!({
        "Parameters": { PROBE_PATH_PARAMETER: { "Type": "String" } },
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "HealthCheckId": { "Value": { "Ref": "Check" } }
        }
    })
    .to_string()
}

/// The paths the deployed api serves, each with the status its `MOCK` method answers: the
/// lowest status the request types accept, another status inside that range, and one outside it
/// the endpoint answers perfectly well.
const SERVED_PATHS: &[(&str, u16)] = &[("healthy", 200), ("accepted", 202), ("refused", 503)];

/// One phase: the `ResourcePath` the stack states for the health check, and whether the api
/// answers that path inside the range the request types accept.
struct Phase {
    case: &'static str,
    path: &'static str,
    accepted: bool,
}

/// The phases, in the order one stack is driven through them. They ALTERNATE between an
/// accepted and a rejected answer, so the description a phase waits for can only come from a
/// probe of the path that phase's update stored. Nothing about the api changes across them: the
/// only declaration a phase's update changes is the health check's `ResourcePath`.
const PHASES: &[Phase] = &[
    Phase {
        case: "created on a path the deployed api answers 200",
        path: "/prod/healthy",
        accepted: true,
    },
    Phase {
        case: "repointed at a path the deployed api does not define",
        path: "/prod/sim2real-absent",
        accepted: false,
    },
    Phase {
        case: "repointed at a path answered another status inside the accepted range",
        path: "/prod/accepted",
        accepted: true,
    },
    Phase {
        case: "repointed at a path the api answers outside the accepted range",
        path: "/prod/refused",
        accepted: false,
    },
    Phase {
        case: "repointed back at the path answered 200",
        path: "/prod/healthy",
        accepted: true,
    },
];

#[tokio::test]
async fn a_stack_update_repointing_the_resource_path_moves_what_every_checker_reports() {
    let router = build_router();
    let body = template();

    let (first, rest) = PHASES
        .split_first()
        .expect("the suite drives at least one phase");

    // The stack as it is DEPLOYED: the api, the stage serving it, and the health check pointed
    // at the path the parameter states.
    let case = first.case;
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            ("Parameters.member.1.ParameterKey", PROBE_PATH_PARAMETER),
            ("Parameters.member.1.ParameterValue", first.path),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let (reached, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described} with the \
         failure reasons {:?}",
        elements(&stack_events(&router, STACK).await, "ResourceStatusReason")
    );
    let outputs = outputs_of(&described);
    let api_id = outputs
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no ApiId in {described}"));
    let id = outputs.get("HealthCheckId").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: the stack published no HealthCheckId in {described}")
    });

    let answered = answered_status(&router, case, &api_id, first.path, first.accepted).await;
    assert_eq!(
        description_of(answered),
        ANSWERED_OK_DESCRIPTION,
        "case {case:?}: the api answers this path the lowest accepted status, which the service \
         is recorded reporting in this wording"
    );
    assert_phase(&router, case, &id, answered, first.accepted).await;
    let (path, mut version) = configured_path_and_version(&router, case, &id).await;
    assert_eq!(
        path, first.path,
        "case {case:?}: the stack's create stores the path the probe requests"
    );

    for phase in rest {
        let case = phase.case;
        // What the probed path answers an ordinary client, read BEFORE the check is repointed at
        // it: the oracle for this phase, and a fact about the api rather than about the check.
        let answered = answered_status(&router, case, &api_id, phase.path, phase.accepted).await;

        let (status, body_text) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", STACK),
                ("TemplateBody", &body),
                ("Parameters.member.1.ParameterKey", PROBE_PATH_PARAMETER),
                ("Parameters.member.1.ParameterValue", phase.path),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body_text}"
        );
        let (reached, described) = wait_for_stack_status(&router, STACK, UPDATE_COMPLETE).await;
        assert_eq!(
            reached, UPDATE_COMPLETE,
            "case {case:?}: expected the update to reach {UPDATE_COMPLETE}, got {described} with \
             the failure reasons {:?}",
            elements(&stack_events(&router, STACK).await, "ResourceStatusReason")
        );

        // The reconfiguration LANDED on the live check: Route 53 reports the new path and an
        // advanced version, the health check keeping its identity.
        let (reported_path, reported_version) =
            configured_path_and_version(&router, case, &id).await;
        assert_eq!(
            reported_path, phase.path,
            "case {case:?}: GetHealthCheck must report the path the update declared"
        );
        assert!(
            reported_version > version,
            "case {case:?}: expected the HealthCheckVersion to advance past {version}, got \
             {reported_version}"
        );
        version = reported_version;

        assert_phase(&router, case, &id, answered, phase.accepted).await;
    }
}

/// Everything one phase asserts about what the checkers observed: the description every checker
/// reports, which checkers report a last failure reason, and the assessment the series carries —
/// three readers of ONE probe of the path this phase stored.
async fn assert_phase(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    answered: u16,
    accepted: bool,
) {
    let expected = description_of(answered);
    assert_eq!(
        expected.starts_with(SUCCESS_OPENING),
        accepted,
        "case {case:?}: an answer of {answered} is reported under the opening its verdict takes, \
         got {expected:?}"
    );
    let reported = statuses_settling_on(router, case, id, &expected).await;
    assert!(
        !reported.is_empty(),
        "case {case:?}: every health-checking Region reports on this endpoint, got {reported:?}"
    );

    let failures = last_failure_statuses(router, case, id).await;
    if accepted {
        assert!(
            failures.is_empty(),
            "case {case:?}: a checker whose latest probe passed has no failure to report, got \
             {failures:?}"
        );
    } else {
        assert_eq!(
            failures, reported,
            "case {case:?}: every checker whose probe failed reports that failure as its last \
             failure reason, got {failures:?} against {reported:?}"
        );
    }

    // The aggregate plane cannot disagree with the checkers about the same probe.
    let folded = if accepted { HEALTHY } else { UNHEALTHY };
    series_settling_on(router, case, id, folded).await;
}

/// The stack's own events, which is where a case that did not end where it should reads the
/// reason a declaration could not be applied.
async fn stack_events(router: &MultiServiceRouter, stack: &str) -> String {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    events
}
