// SPDX-License-Identifier: CC0-1.0

//! EventBridge Scheduler's UNIVERSAL target makes the API call its ARN names: here
//! CloudFront's own `CreateInvalidation`, over the assembled router.
//!
//! RULE: a `Target.Arn` of the form `arn:<partition>:scheduler:::aws-sdk:<service>:
//! <operation>` names a CALL rather than a resource — EventBridge Scheduler "supports
//! templated target that invoke common API operations, as well as universal targets that
//! you can customize to invoke over 6,000 API operations across more than 270 services"
//! (`Target`) — so at each occurrence Scheduler acts as `Target.RoleArn` and makes THAT
//! operation's call with `Target.Input` ("the text, or well-formed JSON, passed to the
//! target") as the operation's own request. The effect a client then observes is the one
//! that operation has, read through the invoked service's own API; a call the execution
//! role's policies do not allow is not made at all, and the failed event reaches the
//! queue the schedule names for one.
//!
//! Only a cross-service suite can pin this: the schedule and the occurrence are
//! EventBridge Scheduler's, the invalidation is CloudFront's own record, the execution
//! role is IAM's, and the dead-letter queue is Amazon SQS's.
//!
//! The rows differ along what the rule generalises over: the shape of the request the
//! `Input` carries (one exact object path, several paths at once, a wildcard over a
//! prefix — the list the batch names being what the invoked operation stores), and
//! whether the execution role's policies allow the call the ARN names.
//!
//! Nothing here waits for a fixed span: every schedule names an instant a few seconds
//! ahead, and the suite watches for the CONDITION — the invalidations CloudFront holds,
//! the message the dead-letter queue holds — until a generous deadline.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_handler_invalidates_its_distribution::{
    build_router, call, cloudfront, create_distribution, create_origin, query, API_VERSION,
    REGION,
};
use crate::query_wire::elements;

/// How long this suite watches for the condition it expects, and how often it looks.
/// Every schedule below names an instant seconds ahead, so the deadline is room for the
/// clock's tick and the calls on a loaded host rather than a latency expectation of its
/// own — it is reached only where nothing was invoked at all.
const PATIENCE: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How far ahead each schedule's one occurrence is placed.
const OCCURRENCE_IN: Duration = Duration::from_secs(4);

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The universal target every row names: CloudFront's `CreateInvalidation`, written the
/// way the AWS SDK integration family names an operation — the SDK SERVICE ID and that
/// SDK's own operation name in lowerCamelCase.
const UNIVERSAL_TARGET: &str = "arn:aws:scheduler:::aws-sdk:cloudfront:createInvalidation";

/// The code EventBridge Scheduler reports for a call the execution role's own policies
/// withhold, which the dead-letter message carries as its `ERROR_CODE` attribute: the
/// error CloudFront's own model declares for a call its caller may not make
/// (`AccessDenied`, HTTP 403), since a withheld call is reported as the refusal that
/// service would have answered.
const ACCESS_DENIED_CODE: &str = "AccessDenied";

/// One EventBridge Scheduler control-plane call (`restJson1`), as its status and the
/// parsed body.
async fn scheduler(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|body| body.to_string()).unwrap_or_default();
    let answered = call(
        router,
        "scheduler",
        &format!("scheduler.{REGION}.amazonaws.com"),
        method,
        path,
        "application/json",
        &payload,
    )
    .await;
    let parsed = match answered.body.is_empty() {
        true => Value::Null,
        false => serde_json::from_str(&answered.body).unwrap_or(Value::Null),
    };
    (answered.status, parsed)
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), as its status and the
/// parsed body.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/sqs/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = match response.body().is_empty() {
        true => Value::Null,
        false => serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    };
    (status, parsed)
}

/// The queue the schedules name for their failed events: its url and its ARN.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "setup: CreateQueue must succeed, got {status} with {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetQueueAttributes must succeed, got {status} with {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A role EventBridge Scheduler may assume, granted each `(action, resource)` pair the
/// invocation needs: the pair AWS documents for an execution role — a trust policy for
/// the scheduler service principal, and the permissions the schedule's own calls need.
async fn given_execution_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    grants: &[(&str, &str)],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "scheduler.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole must succeed, got {status} with {created}"
    );
    let statements: Vec<Value> = grants
        .iter()
        .map(|(action, resource)| {
            json!({ "Effect": "Allow", "Action": action, "Resource": resource })
        })
        .collect();
    let policy = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, written) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "invoke-the-target"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy must succeed, got {status} with {written}"
    );
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// `at` as an `at(...)` expression's body states it: a local date and time with no
/// offset, which with no `ScheduleExpressionTimezone` is read in UTC.
fn at_expression_body(at: SystemTime) -> String {
    let rendered = ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders as ISO-8601");
    rendered.trim_end_matches('Z').to_string()
}

/// A whole number of seconds ahead of now, which is the resolution an `at(...)`
/// expression states an instant at.
fn whole_seconds_ahead(ahead: Duration) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(now) + ahead
}

/// A one-time schedule whose universal target carries `input` as the invoked operation's
/// request, with `queue` as the queue its failed events go to.
async fn given_schedule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role: &str,
    queue: &str,
    input: &str,
) {
    let (status, created) = scheduler(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(json!({
            "ScheduleExpression":
                format!("at({})", at_expression_body(whole_seconds_ahead(OCCURRENCE_IN))),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": UNIVERSAL_TARGET,
                "RoleArn": role,
                "Input": input,
                "DeadLetterConfig": { "Arn": queue },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule must succeed, got {status} with {created}"
    );
    // The universal target round-trips, so what is invoked below is the target a client
    // reads back.
    let (status, read) = scheduler(router, "GET", &format!("/schedules/{name}"), None).await;
    assert_eq!(
        (status, read["Target"]["Arn"].as_str(), read["State"].as_str()),
        (200, Some(UNIVERSAL_TARGET), Some("ENABLED")),
        "case {case:?}: GetSchedule must report the universal target enabled, got {status} \
         with {read}"
    );
}

/// Every invalidation CloudFront holds for `distribution`, as the SET of paths each one's
/// batch names — read through CloudFront's own API, which is where an invalidation has to
/// exist.
async fn invalidations(router: &MultiServiceRouter, distribution: &str) -> Vec<Vec<String>> {
    let listed = cloudfront(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution/{distribution}/invalidation"),
        "",
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "ListInvalidations must answer, got {} with {}",
        listed.status, listed.body
    );
    let mut held = Vec::new();
    for id in elements(&listed.body, "Id") {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/distribution/{distribution}/invalidation/{id}"),
            "",
        )
        .await;
        assert_eq!(
            read.status, 200,
            "GetInvalidation({id}) must answer, got {} with {}",
            read.status, read.body
        );
        let mut paths = elements(&read.body, "Path");
        paths.sort();
        held.push(paths);
    }
    held
}

/// Every message the dead-letter queue holds, with its attributes.
async fn dead_lettered(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ReceiveMessage must answer, got {status} with {received}"
    );
    received["Messages"].as_array().cloned().unwrap_or_default()
}

/// One row: the batch the schedule's `Input` names, and whether the execution role's
/// policies allow the call.
struct Row {
    case: &'static str,
    /// The paths the batch names, which are what the invalidation has to hold.
    paths: &'static [&'static str],
    /// Whether the role is granted `cloudfront:CreateInvalidation` on the distribution.
    permitted: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "one exact object path",
        paths: &["/assets/app.js"],
        permitted: true,
    },
    Row {
        case: "several paths in one batch",
        paths: &["/assets/app.js", "/index.html"],
        permitted: true,
    },
    Row {
        case: "a wildcard over a prefix",
        paths: &["/assets/*"],
        permitted: true,
    },
    Row {
        case: "a role whose policies do not allow the invalidation",
        paths: &["/denied/*"],
        permitted: false,
    },
];

/// Every row: the occurrence makes the `CreateInvalidation` call the universal target
/// names, so CloudFront holds an invalidation whose paths are exactly the ones the
/// `Input` named — and the row whose role withholds the action leaves no invalidation at
/// all, its failed event reaching the queue the schedule names instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_universal_target_submits_the_invalidation_its_input_names() {
    let router = build_router();
    create_origin(&router).await;
    let distribution = create_distribution(&router, "scheduler-universal-invalidation").await;
    let distribution_arn = format!("arn:aws:cloudfront::{ACCOUNT}:distribution/{distribution}");
    let (queue_url, queue_arn) = given_queue(&router, "universal-target-failures").await;

    // What each row's schedule asks for, kept so the assertions below read the request
    // the invocation was given rather than rebuilding it.
    let mut requests: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // A failed event is dead-lettered under the execution role too, so every role may
        // write to the queue; what the denied row's role lacks is the invalidation itself.
        let mut grants: Vec<(&str, &str)> = vec![("sqs:SendMessage", queue_arn.as_str())];
        if row.permitted {
            grants.push(("cloudfront:CreateInvalidation", distribution_arn.as_str()));
        }
        let role =
            given_execution_role(&router, case, &format!("universal-role-{index}"), &grants).await;
        let input = json!({
            "DistributionId": distribution,
            "InvalidationBatch": {
                "Paths": { "Quantity": row.paths.len(), "Items": row.paths },
                "CallerReference": format!("universal-reference-{index}"),
            },
        })
        .to_string();
        given_schedule(
            &router,
            case,
            &format!("universal-schedule-{index}"),
            &role,
            &queue_arn,
            &input,
        )
        .await;
        requests.push(input);
    }

    let permitted = ROWS.iter().filter(|row| row.permitted).count();
    let withheld = ROWS.len() - permitted;
    let mut failures: Vec<Value> = Vec::new();
    let mut held = invalidations(&router, &distribution).await;
    let began = Instant::now();
    loop {
        if failures.len() < withheld {
            failures.extend(dead_lettered(&router, &queue_url).await);
        }
        if held.len() >= permitted && failures.len() >= withheld {
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the universal target made no invalidation for every permitted schedule: expected \
             {permitted} invalidation(s) and {withheld} failed event(s), got {held:?} and \
             {failures:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
        held = invalidations(&router, &distribution).await;
    }

    // Each permitted row's batch is held exactly as its `Input` named it, and the denied
    // row left nothing behind: the count is what says so.
    for row in ROWS.iter().filter(|row| row.permitted) {
        let mut expected: Vec<String> = row.paths.iter().map(|path| (*path).to_string()).collect();
        expected.sort();
        let found = held.iter().filter(|paths| **paths == expected).count();
        assert_eq!(
            found, 1,
            "case {:?}: expected exactly one invalidation holding {expected:?}, got {held:?}",
            row.case
        );
    }
    assert_eq!(
        held.len(),
        permitted,
        "expected an invalidation for each permitted schedule and none for the denied one, got \
         {held:?}"
    );

    // The withheld call is not made, and its failed event is the request the invocation
    // would have carried, reported under the refusal's own code and naming the target.
    assert_eq!(
        failures.len(),
        withheld,
        "expected one failed event per withheld call, got {failures:?}"
    );
    let denied = ROWS
        .iter()
        .position(|row| !row.permitted)
        .expect("the table carries a withheld row");
    for message in &failures {
        assert_eq!(
            message["Body"].as_str(),
            Some(requests[denied].as_str()),
            "the failed event must be the request the invocation would have made, got {message}"
        );
        let attribute = |name: &str| {
            message["MessageAttributes"][name]["StringValue"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(
            (attribute("TARGET_ARN"), attribute("ERROR_CODE")),
            (
                UNIVERSAL_TARGET.to_string(),
                ACCESS_DENIED_CODE.to_string()
            ),
            "the failed event must name the target it was for and the refusal's code, got \
             {message}"
        );
    }
}
