// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series an Application Load Balancer publishes for the traffic it answered.
//!
//! The rule under test: **every request an Application Load Balancer answers is counted into
//! that family's own metric namespace as a side effect of answering it — one request processed
//! under the load balancer's own dimension, and one under the `TargetGroup` + `LoadBalancer`
//! pair of the group that answered — and the status the request ended in is counted into the
//! class series of whichever end PRODUCED it: a status the TARGET answered into
//! `HTTPCode_Target_<n>XX_Count` and into none of the load balancer's own, a status the LOAD
//! BALANCER generated itself into `HTTPCode_ELB_<n>XX_Count` and the series of that one status
//! and into none of the target's. A `lambda` target group's failures of the FUNCTION's own
//! making are counted as `LambdaUserError` rather than as a target status, and a series the
//! request gave no non-zero value is not published at all.**
//!
//! The metric names, their dimensions and their reporting criteria ("there is a non-zero
//! value") are Elastic Load Balancing's published CloudWatch metrics for the Application Load
//! Balancer family; the `error_reason` token each Lambda-target failure class carries is that
//! family's documented access-log format, read here through the crate's own catalog of those
//! classes so the series and the record cannot name different failures.
//!
//! The rows of the first table differ along what the rule generalises over: the CLASS of status
//! the load balancer generated (success, client error, server error, redirection), the
//! terminating ACTION that produced it (`fixed-response`, `redirect`) and whether a rule or the
//! listener's default action matched. The rows of the second differ along what the TARGET did:
//! a success, two different server-error statuses of its own, a run that raised, a payload that
//! is no response object — and which of TWO target groups the request was forwarded to, which
//! is what pins the narrowing to the group that answered.
//!
//! Every request is driven over the data plane by the address the load balancer PUBLISHED, and
//! every datapoint is read out of the CloudWatch store those series live in — the same store
//! `GetMetricStatistics` and the alarm evaluation read — reached on the shared engine, so what
//! a row asserts is what a client's own read would answer. That the function really ran is
//! asserted where nothing of this service can fake it: the handler reports every invocation to
//! an Amazon SQS queue through its own SDK before it decides what to return.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::alb_lambda_target_published_address::{
    activate_functions, execution_role, lambda, reported, sqs, HANDLER, RUNTIME,
};
use crate::provisioning_loop::{action, call, p};

/// The namespace an Application Load Balancer's metrics live in, and the dimensions its series
/// are keyed by — Elastic Load Balancing's published metric identities for that family.
const APPLICATION_NAMESPACE: &str = "AWS/ApplicationELB";
const LOAD_BALANCER_DIMENSION: &str = "LoadBalancer";
const TARGET_GROUP_DIMENSION: &str = "TargetGroup";

/// Every COUNT series this suite windows, written out here rather than read off the
/// implementation: the request counter, the two action counters, the load-balancer-generated
/// class and status counters, the target-generated class counters, the connection error, and
/// the two a `lambda` target group carries. From Elastic Load Balancing's own CloudWatch metric
/// reference for the Application Load Balancer family.
const WATCHED: [&str; 17] = [
    "RequestCount",
    "HTTP_Fixed_Response_Count",
    "HTTP_Redirect_Count",
    "HTTPCode_ELB_3XX_Count",
    "HTTPCode_ELB_4XX_Count",
    "HTTPCode_ELB_5XX_Count",
    "HTTPCode_ELB_500_Count",
    "HTTPCode_ELB_502_Count",
    "HTTPCode_ELB_503_Count",
    "HTTPCode_ELB_504_Count",
    "HTTPCode_Target_2XX_Count",
    "HTTPCode_Target_3XX_Count",
    "HTTPCode_Target_4XX_Count",
    "HTTPCode_Target_5XX_Count",
    "TargetConnectionErrorCount",
    "LambdaUserError",
    "LambdaInternalError",
];

/// The timing series, asserted on its own: its value is how long the target took, which is no
/// count of events.
const TARGET_RESPONSE_TIME: &str = "TargetResponseTime";

/// How long a row waits for the invocation report the handler writes before it returns.
/// Generous on purpose: everything waited for happens in this process, and the deadline is
/// only here so a failure is a failure rather than a hang — the poll loop returns the moment
/// the report is there, so a loaded host cannot decide a verdict.
const BUDGET: Duration = Duration::from_secs(180);

/// The function every lambda row runs, the role it runs as, and the queue it reports to.
const FUNCTION: &str = "alb-request-metrics-target";
const ROLE_NAME: &str = "alb-request-metrics-exec";
const SIDE_CHANNEL: &str = "alb-request-metrics-side-channel";

/// The handler every lambda row runs. It reports the invocation to the side channel FIRST — so
/// a row answered without the handler having run leaves the queue empty — and then decides what
/// to return from the LAST SEGMENT of the path the event carries, so one package serves every
/// row.
const HANDLER_SOURCE: &str = r#"
import os

import boto3


def lambda_handler(event, context):
    path = (event.get("path") or "/").rsplit("/", 1)[-1]
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["QUEUE_URL"], MessageBody=path or "/"
    )
    if path == "nonjson":
        return "not an alb response object at all"
    if path == "raise":
        raise RuntimeError("the handler refused to answer")
    if path == "fail":
        return {"statusCode": 500, "body": "TARGET-FAILED"}
    if path == "busy":
        return {"statusCode": 503, "body": "TARGET-BUSY"}
    return {"statusCode": 200, "body": "TARGET-OK"}
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

/// This service, Lambda, Amazon SQS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the function the load balancer forwards to is run by
/// that Lambda as a session of the role IAM holds, and the handler's own SDK call is dispatched
/// back into this router, so the queue it reports to is the queue this suite reads.
fn lambda_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
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
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// The `app/<name>/<id>` suffix of a load balancer ARN, which is how an Elastic Load Balancing
/// dimension names it.
fn load_balancer_dimension(arn: &str) -> String {
    arn.split_once(":loadbalancer/")
        .map(|(_, suffix)| suffix.to_string())
        .unwrap_or_else(|| panic!("a load balancer ARN names its family and name: {arn}"))
}

/// The `targetgroup/<name>/<id>` suffix of a target group ARN, which is how that dimension
/// names it.
fn target_group_dimension(arn: &str) -> String {
    arn.split_once(":targetgroup/")
        .map(|(_, suffix)| format!("targetgroup/{suffix}"))
        .unwrap_or_else(|| panic!("a target group ARN names its resource type: {arn}"))
}

/// The `Sum` of every datapoint one series holds, as the metric store holds them — and 0.0 for
/// a series the store holds none of, which is what a reader folds for a period no such request
/// fell in.
fn series_sum(
    engine: &Arc<engine::InMemoryEngine>,
    metric: &str,
    dimensions: &[(&str, String)],
) -> f64 {
    let state =
        cloudwatch::service_state(&**engine).expect("the shared engine holds a metric store");
    let store = state.lock();
    let key = cloudwatch::MetricKey {
        namespace: APPLICATION_NAMESPACE.to_string(),
        metric_name: metric.to_string(),
        dimensions: dimensions
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect(),
    };
    store
        .metrics
        .get(&key)
        .map_or(0.0, |series| series.iter().map(|datum| datum.sum).sum())
}

/// How many datapoints one series holds.
fn series_len(
    engine: &Arc<engine::InMemoryEngine>,
    metric: &str,
    dimensions: &[(&str, String)],
) -> usize {
    let state =
        cloudwatch::service_state(&**engine).expect("the shared engine holds a metric store");
    let store = state.lock();
    let key = cloudwatch::MetricKey {
        namespace: APPLICATION_NAMESPACE.to_string(),
        metric_name: metric.to_string(),
        dimensions: dimensions
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect(),
    };
    store.metrics.get(&key).map_or(0, Vec::len)
}

/// Every watched series' `Sum` under one dimension set, so a row asserts the WHOLE catalog at
/// once: a series the rows so far gave no value reads 0.0, which is what pins "nothing else was
/// counted".
fn watched_sums(
    engine: &Arc<engine::InMemoryEngine>,
    dimensions: &[(&str, String)],
) -> BTreeMap<&'static str, f64> {
    WATCHED
        .iter()
        .map(|metric| (*metric, series_sum(engine, metric, dimensions)))
        .collect()
}

/// The expected whole catalog: every watched series at 0.0 except the ones `counted` names.
fn expected(counted: &[(&'static str, f64)]) -> BTreeMap<&'static str, f64> {
    let mut sums: BTreeMap<&'static str, f64> =
        WATCHED.iter().map(|metric| (*metric, 0.0)).collect();
    for (metric, value) in counted {
        sums.insert(metric, *value);
    }
    sums
}

/// Add one `fixed-response` rule at `priority` answering `status` for `path`.
async fn fixed_response_rule(
    router: &MultiServiceRouter,
    listener_arn: &str,
    priority: i32,
    path: &str,
    status: &str,
) {
    let mut form = action("CreateRule");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("Priority", &priority.to_string()));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p("Conditions.member.1.Values.member.1", path));
    form.push(p("Actions.member.1.Type", "fixed-response"));
    form.push(p(
        "Actions.member.1.FixedResponseConfig.StatusCode",
        status,
    ));
    form.push(p(
        "Actions.member.1.FixedResponseConfig.MessageBody",
        "answered by the load balancer itself",
    ));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup rule {path}: expected CreateRule to answer 200, got {} body {}",
        reply.status, reply.body
    );
}

/// One row of the load-balancer-generated table: the path the client asks for, the status the
/// load balancer must answer, and what that request adds to the catalog.
struct GeneratedRow {
    case: &'static str,
    path: &'static str,
    status: u16,
    /// The series this request moves, and by how much — cumulative over the table, so each row
    /// also asserts that the rows before it were counted exactly once.
    counted: &'static [(&'static str, f64)],
}

/// RULE: a status the LOAD BALANCER generated itself is counted into its own class series and
/// into the series of that one status, into no target series at all, and the terminating action
/// that produced it is counted where that action has a series of its own.
#[tokio::test]
async fn a_load_balancer_generated_status_counts_as_an_elb_code() {
    let (router, engine) = crate::provisioning_loop::emulator();
    let (lb_arn, published) = create_load_balancer(&router, "alb-elb-codes").await;

    // The listener's DEFAULT action is the client error, so one row is answered without any
    // rule matching at all.
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "fixed-response"));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.StatusCode",
        "404",
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.MessageBody",
        "no rule matched",
    ));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    fixed_response_rule(&router, &listener_arn, 10, "/ok", "200").await;
    fixed_response_rule(&router, &listener_arn, 20, "/gone", "503").await;
    let mut form = action("CreateRule");
    form.push(p("ListenerArn", &listener_arn));
    form.push(p("Priority", "30"));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p("Conditions.member.1.Values.member.1", "/moved"));
    form.push(p("Actions.member.1.Type", "redirect"));
    form.push(p("Actions.member.1.RedirectConfig.StatusCode", "HTTP_302"));
    form.push(p("Actions.member.1.RedirectConfig.Host", "elsewhere.test"));
    form.push(p("Actions.member.1.RedirectConfig.Path", "/#{path}"));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup redirect rule: expected 200, got {} body {}",
        reply.status, reply.body
    );

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    let dimensions = [(LOAD_BALANCER_DIMENSION, load_balancer_dimension(&lb_arn))];
    assert_eq!(
        watched_sums(&engine, &dimensions),
        expected(&[]),
        "before any traffic: a load balancer that has answered nothing publishes nothing"
    );

    let rows: &[GeneratedRow] = &[
        GeneratedRow {
            case: "a fixed-response success: counted as a request and as the action, in no \
                   class series",
            path: "/ok",
            status: 200,
            counted: &[("RequestCount", 1.0), ("HTTP_Fixed_Response_Count", 1.0)],
        },
        GeneratedRow {
            case: "a client error from the listener's default action",
            path: "/nothing-matches",
            status: 404,
            counted: &[
                ("RequestCount", 2.0),
                ("HTTP_Fixed_Response_Count", 2.0),
                ("HTTPCode_ELB_4XX_Count", 1.0),
            ],
        },
        GeneratedRow {
            case: "a server error the load balancer generated: its class and its own status",
            path: "/gone",
            status: 503,
            counted: &[
                ("RequestCount", 3.0),
                ("HTTP_Fixed_Response_Count", 3.0),
                ("HTTPCode_ELB_4XX_Count", 1.0),
                ("HTTPCode_ELB_5XX_Count", 1.0),
                ("HTTPCode_ELB_503_Count", 1.0),
            ],
        },
        GeneratedRow {
            case: "a redirection: the redirect action's own counter and the 3XX class",
            path: "/moved",
            status: 302,
            counted: &[
                ("RequestCount", 4.0),
                ("HTTP_Fixed_Response_Count", 3.0),
                ("HTTP_Redirect_Count", 1.0),
                ("HTTPCode_ELB_3XX_Count", 1.0),
                ("HTTPCode_ELB_4XX_Count", 1.0),
                ("HTTPCode_ELB_5XX_Count", 1.0),
                ("HTTPCode_ELB_503_Count", 1.0),
            ],
        },
        GeneratedRow {
            case: "the same server error again: the counters add up rather than being replaced",
            path: "/gone",
            status: 503,
            counted: &[
                ("RequestCount", 5.0),
                ("HTTP_Fixed_Response_Count", 4.0),
                ("HTTP_Redirect_Count", 1.0),
                ("HTTPCode_ELB_3XX_Count", 1.0),
                ("HTTPCode_ELB_4XX_Count", 1.0),
                ("HTTPCode_ELB_5XX_Count", 2.0),
                ("HTTPCode_ELB_503_Count", 2.0),
            ],
        },
    ];

    for row in rows {
        let answered = send(&router, "GET", &published, row.path, &[]).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} body {:?}",
            row.case, row.status, answered.status, answered.body
        );
        let sums = watched_sums(&engine, &dimensions);
        assert_eq!(
            sums,
            expected(row.counted),
            "case {:?}: expected the catalog to read {:?}, got {sums:?}",
            row.case,
            expected(row.counted)
        );
    }

    // A status the load balancer generated itself came from no target, so the series that are
    // about a target hold nothing at all: no timing, and no target group to narrow by.
    let timed = series_len(&engine, TARGET_RESPONSE_TIME, &dimensions);
    assert_eq!(
        timed, 0,
        "expected a load balancer that called no target to have timed none, got {timed} \
         datapoint(s)"
    );
}

/// One row of the target table: which group the request is forwarded to, the path the client
/// asks for, the status the client is answered with, and what that request adds to each of the
/// two catalogs.
struct TargetRow {
    case: &'static str,
    /// The index of the target group the path routes to, which is the group whose own catalog
    /// must move.
    group: usize,
    path: &'static str,
    status: u16,
    /// The load balancer's own catalog after this row, cumulative over the table.
    load_balancer: &'static [(&'static str, f64)],
    /// The catalog of the FIRST target group after this row, cumulative over the table — which
    /// a row forwarded to the second group must leave exactly where it was.
    first_group: &'static [(&'static str, f64)],
}

/// RULE: a status the TARGET answered is counted into the target class series — under the load
/// balancer's own dimension and under the `TargetGroup` pair of the group that answered, and
/// never into a load-balancer-generated class — while a failure of the FUNCTION's own making is
/// counted as a Lambda user error beside the 502 the load balancer generated for it.
#[tokio::test]
async fn a_target_status_counts_against_the_target_group_that_answered() {
    let (router, engine) = lambda_emulator();

    // The side channel the handler reports every invocation to, and the role whose one grant
    // admits that report.
    let (status, queue) = sqs(&router, "CreateQueue", &json!({ "QueueName": SIDE_CHANNEL })).await;
    assert_eq!(
        status, 200,
        "setup CreateQueue: expected 200, got {status} {queue}"
    );
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateQueue: no QueueUrl in {queue}"))
        .to_string();
    let (status, attributes) = sqs(
        &router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup GetQueueAttributes: expected 200, got {status} {attributes}"
    );
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup GetQueueAttributes: no QueueArn in {attributes}"))
        .to_string();
    let role = execution_role(&router, ROLE_NAME, &queue_arn).await;

    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": FUNCTION,
            "Role": role,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Environment": { "Variables": { "QUEUE_URL": queue_url } },
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package()) },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected 201, got {status} {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateFunction: no FunctionArn in {created}"))
        .to_string();
    activate_functions(&engine);

    let (lb_arn, published) = create_load_balancer(&router, "alb-target-codes").await;

    // TWO `lambda` target groups registering the same function, each reached by its own rule:
    // what a series is narrowed by is the group the request was FORWARDED to, which two groups
    // behind one function is what pins.
    let mut groups: Vec<String> = Vec::new();
    for name in ["alb-target-codes-one", "alb-target-codes-two"] {
        let mut form = action("CreateTargetGroup");
        form.push(p("Name", name));
        form.push(p("TargetType", "lambda"));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "setup CreateTargetGroup {name}: expected 200, got {} body {}",
            reply.status, reply.body
        );
        let tg_arn = reply.text("TargetGroupArn").unwrap_or_else(|| {
            panic!("setup CreateTargetGroup {name}: no TargetGroupArn in {}", reply.body)
        });

        let mut form = action("RegisterTargets");
        form.push(p("TargetGroupArn", &tg_arn));
        form.push(p("Targets.member.1.Id", &function_arn));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "setup RegisterTargets {name}: expected 200, got {} body {}",
            reply.status, reply.body
        );

        // The grant that admits the invocation is the function's own, narrowed to the target
        // group the request was forwarded to, so each group carries its own statement.
        let (status, granted) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{FUNCTION}/policy"),
            json!({
                "StatementId": format!("elbv2-invoke-{name}"),
                "Action": "lambda:InvokeFunction",
                "Principal": "elasticloadbalancing.amazonaws.com",
                "SourceArn": tg_arn,
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "setup AddPermission {name}: expected 201, got {status} {granted}"
        );
        groups.push(tg_arn);
    }

    // The listener forwards to the FIRST group by default, and a rule sends `/two/*` to the
    // second.
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", &groups[0]));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    let mut form = action("CreateRule");
    form.push(p("ListenerArn", &listener_arn));
    form.push(p("Priority", "10"));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p("Conditions.member.1.Values.member.1", "/two/*"));
    form.push(p("Actions.member.1.Type", "forward"));
    form.push(p("Actions.member.1.TargetGroupArn", &groups[1]));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateRule: expected 200, got {} body {}",
        reply.status, reply.body
    );

    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    let rows: &[TargetRow] = &[
        TargetRow {
            case: "the target answered a success",
            group: 0,
            path: "/one/ok",
            status: 200,
            load_balancer: &[("RequestCount", 1.0), ("HTTPCode_Target_2XX_Count", 1.0)],
            first_group: &[("RequestCount", 1.0), ("HTTPCode_Target_2XX_Count", 1.0)],
        },
        TargetRow {
            case: "the target answered 500: a target 5XX, and no load-balancer 5XX",
            group: 0,
            path: "/one/fail",
            status: 500,
            load_balancer: &[
                ("RequestCount", 2.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 1.0),
            ],
            first_group: &[
                ("RequestCount", 2.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 1.0),
            ],
        },
        TargetRow {
            case: "the target answered another 5XX status of its own",
            group: 0,
            path: "/one/busy",
            status: 503,
            load_balancer: &[
                ("RequestCount", 3.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 2.0),
            ],
            first_group: &[
                ("RequestCount", 3.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 2.0),
            ],
        },
        TargetRow {
            case: "a run that raised: a Lambda user error and the load balancer's own 502",
            group: 0,
            path: "/one/raise",
            status: 502,
            load_balancer: &[
                ("RequestCount", 4.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 2.0),
                ("HTTPCode_ELB_5XX_Count", 1.0),
                ("HTTPCode_ELB_502_Count", 1.0),
                ("LambdaUserError", 1.0),
            ],
            first_group: &[
                ("RequestCount", 4.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 2.0),
                ("LambdaUserError", 1.0),
            ],
        },
        TargetRow {
            case: "a payload that is no response object: the same user-error class",
            group: 0,
            path: "/one/nonjson",
            status: 502,
            load_balancer: &[
                ("RequestCount", 5.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 2.0),
                ("HTTPCode_ELB_5XX_Count", 2.0),
                ("HTTPCode_ELB_502_Count", 2.0),
                ("LambdaUserError", 2.0),
            ],
            first_group: &[
                ("RequestCount", 5.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 2.0),
                ("LambdaUserError", 2.0),
            ],
        },
        TargetRow {
            case: "the SECOND group answered 500: the load balancer's total moves and the \
                   first group's catalog does not",
            group: 1,
            path: "/two/fail",
            status: 500,
            load_balancer: &[
                ("RequestCount", 6.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 3.0),
                ("HTTPCode_ELB_5XX_Count", 2.0),
                ("HTTPCode_ELB_502_Count", 2.0),
                ("LambdaUserError", 2.0),
            ],
            first_group: &[
                ("RequestCount", 5.0),
                ("HTTPCode_Target_2XX_Count", 1.0),
                ("HTTPCode_Target_5XX_Count", 2.0),
                ("LambdaUserError", 2.0),
            ],
        },
    ];

    let load_balancer_dimensions = [(LOAD_BALANCER_DIMENSION, load_balancer_dimension(&lb_arn))];
    let group_dimensions: Vec<Vec<(&str, String)>> = groups
        .iter()
        .map(|arn| {
            vec![
                (TARGET_GROUP_DIMENSION, target_group_dimension(arn)),
                (LOAD_BALANCER_DIMENSION, load_balancer_dimension(&lb_arn)),
            ]
        })
        .collect();

    for row in rows {
        let answered = send(&router, "GET", &published, row.path, &[]).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} body {:?}",
            row.case, row.status, answered.status, answered.body
        );
        // The function RAN, exactly once, whatever the load balancer answered: the handler
        // reports before it returns, so the series below are counted off real invocations.
        let mode = row
            .path
            .rsplit('/')
            .next()
            .expect("a path names a last segment");
        let seen = reported(&router, row.case, &queue_url, BUDGET).await;
        assert_eq!(
            seen,
            vec![mode.to_string()],
            "case {:?}: expected exactly one invocation reported as {mode:?}, got {seen:?}",
            row.case
        );

        let sums = watched_sums(&engine, &load_balancer_dimensions);
        assert_eq!(
            sums,
            expected(row.load_balancer),
            "case {:?}: expected the load balancer's catalog to read {:?}, got {sums:?}",
            row.case,
            expected(row.load_balancer)
        );
        let narrowed = watched_sums(&engine, &group_dimensions[0]);
        assert_eq!(
            narrowed,
            expected(row.first_group),
            "case {:?}: expected the first target group's catalog to read {:?}, got {narrowed:?}",
            row.case,
            expected(row.first_group)
        );

        // The time the target took is a series of the group that answered, and a datapoint of
        // it is published for every request an invocation was made for.
        let timed = series_len(&engine, TARGET_RESPONSE_TIME, &group_dimensions[row.group]);
        assert!(
            timed > 0,
            "case {:?}: expected the group that answered to have timed the target, got {timed} \
             datapoint(s)",
            row.case
        );
    }

    // A series this traffic gave no value is published under NO dimension set: the second
    // group never answered a success, so its own 2XX series holds nothing at all.
    let second_group_successes = series_len(
        &engine,
        "HTTPCode_Target_2XX_Count",
        &group_dimensions[1],
    );
    assert_eq!(
        second_group_successes, 0,
        "expected the group that answered no success to publish no 2XX datapoint, got \
         {second_group_successes}"
    );
}
