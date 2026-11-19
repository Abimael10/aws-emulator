// SPDX-License-Identifier: CC0-1.0

//! The HEALTH CHECK of a `lambda` TARGET GROUP: the invocation one check of such a group is,
//! what the function is handed, and what its answer decides.
//!
//! The rule under test: **a load balancer health-checks a `lambda` target group by INVOKING
//! the function the group registers — a `GET` of the group's own `HealthCheckPath`, reported
//! in the request's user-agent field as the load balancer's health checker — on the group's own
//! `HealthCheckIntervalSeconds` cadence and only while its health checks are enabled, which for
//! such a group is off unless the caller turned it on. The function therefore RUNS, with every
//! effect its run has: the records of the run reach the log group it writes to, which is the
//! group CloudWatch Logs then holds and `DescribeLogStreams` answers for. The check succeeds
//! while the status the function returned is one the group's `Matcher` counts as success, and a
//! target whose checks answer outside it is reported unhealthy.**
//!
//! `TargetGroup$TargetType` gives `lambda` as "Register a single Lambda function as a target",
//! `HealthCheckEnabled` states "if the target type is lambda, health checks are disabled by
//! default but can be enabled", `HealthCheckPath` is "the destination for health checks on the
//! targets" and `Matcher` "the HTTP or gRPC codes to use when checking for a successful
//! response from a target".
//!
//! Nothing here is observed from a record this suite invented. The function is a real
//! deployment package created through Lambda's OWN API and RUN by the Lambda mounted on this
//! suite's engine, and that it ran is asserted where nothing of this service can fake it: the
//! handler PRINTS what it was handed, so the only way the event reaches this suite is through
//! the log group the run's records were delivered to — read back through CloudWatch Logs' own
//! `DescribeLogStreams` and `GetLogEvents`, which is how a client finds what a run wrote.
//!
//! The rows differ along what the rule generalises over: whether the group's health checks are
//! enabled at all, the destination the check requests (the documented default and a configured
//! one), and the status the function answers (inside the group's matcher and outside it).

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, settle};
use crate::provisioning_loop::{account, action, call, p, REGION};

/// The module and attribute every package carries its handler as, and the runtime it is
/// created for.
const HANDLER: &str = "lambda_function.lambda_handler";
const RUNTIME: &str = "python3.12";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about the role and nothing it does is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The cadence every checked group is created with: the shortest
/// `HealthCheckIntervalSeconds` the member admits ("the range is 5-300"), so a check comes due
/// without the suite waiting out the 35 seconds a `lambda` target group defaults to.
const INTERVAL_SECONDS: &str = "5";

/// The `HealthCheckTimeoutSeconds` those groups carry, because the service holds the pair to
/// "health check interval must be greater than the timeout" and a `lambda` target group
/// defaults that member to 30 seconds — longer than the cadence above. The longest wait that
/// pairs with that cadence, so a first run of the function that takes a moment is still
/// answered inside the span the check waits.
const TIMEOUT_SECONDS: &str = "4";

/// The marker the handler prints the event under, which is what a log record of the run is
/// found by.
const MARKER: &str = "alb-health-check";

/// How long a wait for work the clock owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: everything waited for happens in this process, so a
/// loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler every row runs. It PRINTS what the event carried — the method, the destination,
/// the user-agent field and the target group the check belongs to — so the only record of the
/// invocation is the one the run's log delivers, and then answers the status the row's own
/// environment names. One package serves every row, which is what the rule generalises over.
const HANDLER_SOURCE: &str = r#"
import json
import os


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    context_elb = (event.get("requestContext") or {}).get("elb") or {}
    print(
        "alb-health-check "
        + json.dumps(
            {
                "method": event.get("httpMethod"),
                "path": event.get("path"),
                "user_agent": headers.get("user-agent"),
                "target_group": context_elb.get("targetGroupArn"),
            },
            sort_keys=True,
        )
    )
    return {
        "statusCode": int(os.environ["ANSWER_STATUS"]),
        "headers": {"content-type": "text/plain"},
        "body": "OK",
    }
"#;

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `lambda_function.py`, written in
/// memory the way a caller uploads one.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// This service, Lambda and CloudWatch Logs over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them.
///
/// Its own builder rather than the one `all/alb_lambda_target.rs` shares, for the service this
/// suite cannot do without: the records of a run reach CloudWatch Logs through Lambda's own
/// delivery, so the log group a health check's invocation creates is only a group a client can
/// read where that service is mounted beside the other two.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// One Lambda `restJson1` call.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str, body: Value) -> (u16, Value) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).expect("the request body serializes")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{path}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/lambda/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One CloudWatch Logs call (`POST /` with `X-Amz-Target: Logs_20140328.<Operation>`), the way
/// a client makes it.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/logs/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The error code an awsJson refusal carries, which the wire writes as the shape's id.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The default log group of the function `name`: "Lambda functions send logs to a default log
/// group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
fn default_log_group(name: &str) -> String {
    format!("/aws/lambda/{name}")
}

/// Create the function `name` through Lambda's own API, answering the status its handler
/// returns, and answer its ARN.
async fn create_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    answer_status: u16,
) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Environment": { "Variables": { "ANSWER_STATUS": answer_status.to_string() } },
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package()) },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction {name}: expected 201, got {status} {created}"
    );
    let arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateFunction {name}: no FunctionArn in {created}"))
        .to_string();
    // The activation span Lambda owes a freshly created function, placed rather than slept for
    // — the same reading every suite that invokes one uses.
    {
        let state = engine
            .state::<service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState>();
        let mut store = state.lock();
        let past = SystemTime::now() - Duration::from_secs(1);
        for record in store.functions.values_mut() {
            record.active_due = past;
        }
    }
    arn
}

/// One row: the target group's health-check configuration, the function behind it, and what
/// the checks of that group must then do.
struct Row {
    case: &'static str,
    /// The function the group registers, which is also the name of its default log group.
    function: &'static str,
    /// The target group, and the listener port that routes to it.
    target_group: &'static str,
    port: &'static str,
    /// Whether the group is created with its health checks enabled.
    health_checks: bool,
    /// The `HealthCheckPath` the group is created with; `None` leaves the member out, so the
    /// check requests the documented default.
    path: Option<&'static str>,
    /// The status the function answers a check with.
    answer_status: u16,
    /// What the checks of this group must then produce: the destination the event carries and
    /// the health the target is reported in, or nothing at all.
    expected: Expected,
}

/// What one row's checks must produce.
enum Expected {
    /// The function is invoked: the event carries this destination, and the target is reported
    /// in this health.
    Checked {
        path: &'static str,
        health: &'static str,
    },
    /// Nothing invokes the function, so CloudWatch Logs holds no group of its own at all.
    Unchecked,
}

/// Every row's group is created, registered and routed the same way; what differs is the
/// health-check configuration and the answer the function gives.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "health checks enabled at the documented default destination",
            function: "alb-health-check-default",
            target_group: "alb-hc-default",
            port: "80",
            health_checks: true,
            path: None,
            answer_status: 200,
            expected: Expected::Checked {
                path: "/",
                health: "healthy",
            },
        },
        Row {
            case: "health checks enabled at a configured destination",
            function: "alb-health-check-configured",
            target_group: "alb-hc-configured",
            port: "81",
            health_checks: true,
            path: Some("/ready"),
            answer_status: 200,
            expected: Expected::Checked {
                path: "/ready",
                health: "healthy",
            },
        },
        Row {
            case: "a function answering outside the group's matcher",
            function: "alb-health-check-refusing",
            target_group: "alb-hc-refusing",
            port: "82",
            health_checks: true,
            path: Some("/health"),
            // The matcher a `lambda` target group is created with counts 200 as success, so a
            // function answering this is a target whose checks are failing.
            answer_status: 503,
            expected: Expected::Checked {
                path: "/health",
                health: "unhealthy",
            },
        },
        Row {
            case: "health checks disabled, which is a lambda target group's default",
            function: "alb-health-check-off",
            target_group: "alb-hc-off",
            port: "83",
            health_checks: false,
            path: None,
            answer_status: 200,
            expected: Expected::Unchecked,
        },
    ]
}

/// Look at `DescribeLogStreams` until CloudWatch Logs answers 200 with at least one stream of
/// `group`, and answer the streams it reported — or panic once the budget is spent.
///
/// The wait is for the CONDITION, not for a span: a check comes due on the target group's own
/// cadence and the records of the run it makes reach the group after it, so what this waits for
/// is the group holding a stream.
async fn streams_of(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, answered) = logs(
            router,
            "DescribeLogStreams",
            &json!({ "logGroupName": group }),
        )
        .await;
        if status == 200 {
            let streams = answered["logStreams"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !streams.is_empty() {
                return streams;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: DescribeLogStreams of {group:?} never answered 200 with a stream of \
             the health check's invocation, last answer {status} {answered}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look at `DescribeTargetHealth` until the registration of `target_group` reports `health`, or
/// panic once the budget is spent. The verdict is a number of the group's own checks away, so
/// what this waits for is the state rather than a span.
async fn until_health(
    router: &MultiServiceRouter,
    case: &str,
    target_group: &str,
    health: &str,
) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let mut form = action("DescribeTargetHealth");
        form.push(p("TargetGroupArn", target_group));
        let reply = call(router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: DescribeTargetHealth must answer 200, got {} body {}",
            reply.status, reply.body
        );
        let reported = reply.text("State").unwrap_or_default();
        if reported == health {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the registration to be reported {health:?}, got \
             {reported:?} in {}",
            reply.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every message the stream `stream` of `group` holds, as `GetLogEvents` reports them.
async fn messages_of(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
) -> Vec<String> {
    let (status, answered) = logs(
        router,
        "GetLogEvents",
        &json!({ "logGroupName": group, "logStreamName": stream }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetLogEvents of {group:?}/{stream:?} must answer 200, got {status} \
         {answered}"
    );
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| event["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: a `lambda` target group's health check is an INVOCATION of the registered function, on
/// the group's own cadence and only while its health checks are enabled; the function is handed
/// a `GET` of the group's destination from the load balancer's health checker, its run's records
/// reach its log group, and the status it answers decides the target's health.
#[tokio::test]
async fn a_lambda_target_groups_health_check_invokes_the_function_and_its_run_is_logged() {
    let (router, engine) = emulator();
    let (lb_arn, _dns_name) = create_load_balancer(&router, "lambda-health-check-alb").await;

    // Every row's function, target group, grant, registration and listener — each through the
    // owning service's own API.
    let mut target_groups: Vec<(String, String)> = Vec::new();
    for row in rows() {
        let function_arn = create_function(&router, &engine, row.function, row.answer_status).await;

        let mut form = action("CreateTargetGroup");
        form.push(p("Name", row.target_group));
        form.push(p("TargetType", "lambda"));
        form.push(p("HealthCheckEnabled", &row.health_checks.to_string()));
        form.push(p("HealthCheckIntervalSeconds", INTERVAL_SECONDS));
        form.push(p("HealthCheckTimeoutSeconds", TIMEOUT_SECONDS));
        if let Some(path) = row.path {
            form.push(p("HealthCheckPath", path));
        }
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: CreateTargetGroup must answer 200, got {} body {}",
            row.case, reply.status, reply.body
        );
        let tg_arn = reply.text("TargetGroupArn").unwrap_or_else(|| {
            panic!(
                "case {:?}: no TargetGroupArn in {}",
                row.case, reply.body
            )
        });

        // The grant that admits the load balancer's invocation — the function's own
        // resource-based policy, narrowed by `aws:SourceArn` to this target group.
        let (status, granted) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{}/policy", row.function),
            json!({
                "StatementId": "elbv2-health-check",
                "Action": "lambda:InvokeFunction",
                "Principal": "elasticloadbalancing.amazonaws.com",
                "SourceArn": tg_arn,
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: AddPermission must answer 201, got {status} {granted}",
            row.case
        );

        let mut form = action("RegisterTargets");
        form.push(p("TargetGroupArn", &tg_arn));
        form.push(p("Targets.member.1.Id", &function_arn));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: RegisterTargets must answer 200, got {} body {}",
            row.case, reply.status, reply.body
        );

        // The listener that routes to the group, which is what starts its health checking.
        let mut form = action("CreateListener");
        form.push(p("LoadBalancerArn", &lb_arn));
        form.push(p("Protocol", "HTTP"));
        form.push(p("Port", row.port));
        form.push(p("DefaultActions.member.1.Type", "forward"));
        form.push(p("DefaultActions.member.1.TargetGroupArn", &tg_arn));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: CreateListener must answer 200, got {} body {}",
            row.case, reply.status, reply.body
        );
        target_groups.push((row.target_group.to_string(), tg_arn));
    }
    // The settling span the clock owes the load balancer and every registration, placed rather
    // than slept for, so what a read reports is the verdict the checks reached.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    for row in rows() {
        let group = default_log_group(row.function);
        let tg_arn = target_groups
            .iter()
            .find(|(name, _)| name == row.target_group)
            .map(|(_, arn)| arn.clone())
            .unwrap_or_else(|| panic!("case {:?}: the target group was not created", row.case));
        let Expected::Checked { path, health } = row.expected else {
            // Nothing checks this group's registration, so nothing ever invoked its function
            // — and a log group no run created is a group CloudWatch Logs genuinely does not
            // hold, which is the one thing `ResourceNotFoundException` is for. Asserted after
            // every checked row has been observed, so the cadence has had several of its
            // intervals to invoke something.
            let (status, answered) = logs(
                &router,
                "DescribeLogStreams",
                &json!({ "logGroupName": group }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {:?}: expected HTTP 400 for the group of an unchecked function, got \
                 {status} {answered}",
                row.case
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {:?}: expected ResourceNotFoundException, got {answered}",
                row.case
            );
            assert_eq!(
                answered["message"].as_str(),
                Some("The specified log group does not exist."),
                "case {:?}: unexpected refusal message in {answered}",
                row.case
            );
            // And its registration is reported as the model words a group whose health checks
            // are off, rather than as a health no check determined.
            let reported = until_health(&router, row.case, &tg_arn, "unavailable").await;
            assert_eq!(
                reported, "unavailable",
                "case {:?}: expected the registration of a group with health checks off to be \
                 reported unavailable, got {reported:?}",
                row.case
            );
            continue;
        };

        // The group CloudWatch Logs now holds, because the health check's invocation RAN the
        // function and the run's records were delivered to it.
        let streams = streams_of(&router, row.case, &group).await;
        for stream in &streams {
            let name = stream["logStreamName"].as_str().unwrap_or_default();
            assert!(
                !name.is_empty(),
                "case {:?}: every reported stream names itself, got {stream}",
                row.case
            );
            assert_eq!(
                stream["arn"].as_str(),
                Some(
                    format!(
                        "arn:aws:logs:{REGION}:{}:log-group:{group}:log-stream:{name}",
                        account()
                    )
                    .as_str()
                ),
                "case {:?}: unexpected stream ARN in {stream}",
                row.case
            );
            // "As of June 17, 2019, this parameter is no longer supported for log streams, and
            // is always reported as zero."
            assert_eq!(
                stream["storedBytes"].as_i64(),
                Some(0),
                "case {:?}: expected storedBytes 0, got {stream}",
                row.case
            );
            assert!(
                stream["creationTime"].as_i64().unwrap_or_default() > 0,
                "case {:?}: expected the stream's creationTime, got {stream}",
                row.case
            );
        }

        // A group that exists but holds no stream matching the prefix is still a 200 with an
        // empty list, never a refusal.
        let (status, answered) = logs(
            &router,
            "DescribeLogStreams",
            &json!({ "logGroupName": group, "logStreamNamePrefix": "no-stream-of-this-name" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: a prefix nothing matches is still a 200, got {status} {answered}",
            row.case
        );
        assert_eq!(
            answered["logStreams"].as_array().map(Vec::len),
            Some(0),
            "case {:?}: expected an empty logStreams for a prefix nothing matches, got \
             {answered}",
            row.case
        );

        // What the function was HANDED, read back where only a run can have put it: the records
        // of that run in its own log group.
        let mut printed: Vec<String> = Vec::new();
        for stream in &streams {
            let name = stream["logStreamName"].as_str().unwrap_or_default();
            printed.extend(
                messages_of(&router, row.case, &group, name)
                    .await
                    .into_iter()
                    .filter(|message| message.contains(MARKER)),
            );
        }
        assert!(
            !printed.is_empty(),
            "case {:?}: the run of the health check's invocation printed nothing to {group:?}",
            row.case
        );
        for message in &printed {
            let event: Value = message
                .trim()
                .trim_start_matches(MARKER)
                .trim()
                .parse::<Value>()
                .unwrap_or_else(|error| {
                    panic!(
                        "case {:?}: the record of the event is not readable ({error}): \
                         {message:?}",
                        row.case
                    )
                });
            assert_eq!(
                event["method"].as_str(),
                Some("GET"),
                "case {:?}: expected the check to request GET, got {event}",
                row.case
            );
            assert_eq!(
                event["path"].as_str(),
                Some(path),
                "case {:?}: expected the check to request {path:?}, got {event}",
                row.case
            );
            assert_eq!(
                event["user_agent"].as_str(),
                Some("ELB-HealthChecker/2.0"),
                "case {:?}: expected the load balancer's own health checker to be named, got \
                 {event}",
                row.case
            );
            assert_eq!(
                event["target_group"].as_str(),
                Some(tg_arn.as_str()),
                "case {:?}: expected the event to name the target group the check belongs to, \
                 got {event}",
                row.case
            );
        }

        // And the status the function answered is what the health reported for the target is
        // decided by.
        let reported = until_health(&router, row.case, &tg_arn, health).await;
        assert_eq!(
            reported, health,
            "case {:?}: expected the registration to be reported {health:?}, got {reported:?}",
            row.case
        );
    }
}
