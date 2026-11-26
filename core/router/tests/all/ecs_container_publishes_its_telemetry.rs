// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING container's own telemetry calls do, across the services the decision spans.
//!
//! RULE under test: a container of a running Amazon ECS task runs the program its launch names,
//! and the AWS CLI its session provides really makes the call each command line names — signed
//! with a session of the TASK ROLE the launch resolved (`TaskDefinition$taskRoleArn` is "the IAM
//! role that grants containers in the task permission to call Amazon Web Services APIs on your
//! behalf"). So `aws cloudwatch put-metric-data`, which "publishes metric data to Amazon
//! CloudWatch", leaves a datapoint of the metric it named; `aws xray put-trace-segments`, which
//! "uploads segment documents to Amazon Web Services X-Ray", leaves a trace of the service that
//! emitted the segment; and `aws ssm get-parameter`, which "gets information about a single
//! parameter", answers the value the account holds. Each call is authorized against the policies
//! in force on that role, so a role withholding one of those actions is refused exactly that call
//! and the program's `if` takes its other branch — which is how a container reports its own
//! refusals instead of its frames.
//!
//! Why this suite lives beside the router: nothing about the decision belongs to one service. The
//! role and its policies are IAM's records, the session is the token service's, the parameter is
//! Systems Manager's, the datapoint is CloudWatch's, the trace is X-Ray's, and what the container
//! WROTE is CloudWatch Logs' — so all of them have to agree for the rule to hold. Every resource
//! below is created through the real operation of the service that owns it on ONE shared engine,
//! and everything asserted is read back through a real read: the container's `exitCode` through
//! `DescribeTasks`, its lines through `GetLogEvents`, the counter through CloudWatch's own
//! `GetMetricStatistics` and the trace through X-Ray's own `GetTraceSummaries`.
//!
//! The rows differ along what the rule generalises over: WHICH actions the task role carries
//! (every one the program needs, one withheld, and no task role at all — so no credential to call
//! with), and therefore which branch the program takes and which of the three services answers
//! it. Every row's container runs the SAME program, so what differs between them is the
//! permissions alone.
//!
//! Member and doc source: `smithy_query(query="shape", shape="TaskDefinition")` for
//! `taskRoleArn`, `smithy_query(query="operation", operation="PutMetricData")` for the publish's
//! own members, `smithy_query(query="operation", operation="PutTraceSegments")` for the segment
//! document's required fields and `smithy_query(query="operation", operation="GetTraceSummaries")`
//! for the read that resolves it.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster every task below runs in.
const CLUSTER: &str = "containertelemetry";

/// The one container of every task definition below.
const CONTAINER: &str = "publisher";

/// The image the containers start from: the official AWS CLI image, which is what a workload
/// whose program is a sequence of `aws` commands is routinely built on. Its own entry point is
/// that program, so every revision below states the `entryPoint` the shell program it runs needs
/// — exactly as a task definition carrying a script does.
const IMAGE: &str = "public.ecr.aws/aws-cli/aws-cli:2.36.44";

/// The log group every container delivers its output to, created through CloudWatch Logs' own API
/// before any task is launched, and the `awslogs-stream-prefix` every revision configures.
const LOG_GROUP: &str = "/emulator/ecs/container-telemetry";
const STREAM_PREFIX: &str = "telemetry";

/// The parameter every row's program reads its configuration from, and the value the account
/// holds for it — put there through Systems Manager's own `PutParameter`.
const PARAMETER: &str = "/flint/prod/quartz-latency-mode";
const PARAMETER_VALUE: &str = "degraded";

/// The metric every row's program publishes its own counter as, and the dimension it reports the
/// outcome of its submission under.
const METRIC: &str = "TraceWrites";
const RESULT_DIMENSION: &str = "Result";

/// The two outcomes that dimension carries: the submission X-Ray accepted, and the one it refused.
const RESULT_OK: &str = "ok";
const RESULT_DENIED: &str = "denied";

/// How long a poll keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. Generous on purpose: the wait ends on the CONDITION, and the deadline is
/// only there so a move that never happens fails the test instead of hanging it.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(150);

/// The period a metric read aggregates into, and the span every read asks for around the publish —
/// both generous, so a read is never a race with the clock.
const PERIOD_SECS: i64 = 60;
const WINDOW_SECS: i64 = 300;

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// The services this decision spans over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them, with the engine handle the network records are written through.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
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
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One RPC call to the service `signing_name` under `target_prefix`, in the JSON protocol
/// `content_type` names, answered with the HTTP status and the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that has to succeed.
async fn ecs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(
        router,
        "ecs",
        "application/x-amz-json-1.1",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ecs {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One CloudWatch Logs call that has to succeed.
async fn logs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(
        router,
        "logs",
        "application/x-amz-json-1.1",
        "Logs_20140328",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: logs {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One Systems Manager call that has to succeed.
async fn ssm_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(
        router,
        "ssm",
        "application/x-amz-json-1.1",
        "AmazonSSM",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ssm {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One CloudWatch call that has to succeed, over that service's `x-amz-json-1.0` wire.
async fn cloudwatch_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answer) = rpc(
        router,
        "monitoring",
        "application/x-amz-json-1.0",
        "GraniteServiceVersion20100801",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: cloudwatch {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One X-Ray call that has to succeed: that service serves `restJson1`, so the operation is its
/// own URI and the body is JSON.
async fn xray_ok(router: &MultiServiceRouter, case: &str, uri: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("xray.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("xray"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: xray {uri} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM that has to succeed, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: iam {action} must answer 200, got {status} with {text}"
    );
    text
}

/// One ec2Query action that has to succeed, answered with the first `element` of its XML body —
/// the identifier Amazon EC2 minted for the resource the action created.
async fn ec2_element(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
    element: &str,
) -> String {
    let mut body = format!("Action={action}&Version=2016-11-15");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: ec2 {action} must answer 200, got {status} with {text}"
    );
    crate::query_wire::opt_element(&text, element)
        .unwrap_or_else(|| panic!("case {case:?}: no <{element}> in {text}"))
}

/// The trust policy a role Amazon ECS assumes for a task carries: the service principal it
/// assumes a task's roles as.
fn ecs_tasks_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the role `name`, trusted by that principal, through IAM's own API; answer its ARN.
async fn role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &ecs_tasks_trust()),
        ],
    )
    .await;
    crate::query_wire::opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: no <Arn> in {created}"))
}

/// Attach an inline policy to `role_name` allowing `actions` on every resource, through IAM's own
/// `PutRolePolicy` — the call a client makes to give a task role a grant.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy_name: &str,
    actions: &[&str],
) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", policy_name),
            ("PolicyDocument", &document),
        ],
    )
    .await;
}

/// One row: the task role a launch names, what that role may do, and what the program it runs
/// therefore reports.
struct Row {
    case: &'static str,
    /// The family its revision is registered under.
    family: &'static str,
    /// The task role the revision names, and `None` for a revision that names none — whose
    /// containers hold no credential at all.
    task_role: Option<&'static str>,
    /// Whether that role carries `xray:PutTraceSegments`.
    traces_allowed: bool,
    /// The namespace the program publishes its counter into, and the `name` its segment carries —
    /// one per row, so each row's reads are about its own task alone.
    namespace: &'static str,
    service_name: &'static str,
    /// The outcome the program reports: `ok` where the submission was made, `denied` where it was
    /// not.
    result: &'static str,
    /// Whether the program's calls were made at all — false for a container holding no credential,
    /// whose CLI locates none and calls nothing.
    calls: bool,
    /// The status the program ends with.
    exit_code: i64,
}

const ROWS: &[Row] = &[
    Row {
        case: "a task role carrying every action the program's commands need",
        family: "telemetryallowed",
        task_role: Some("containertelemetryallowed"),
        traces_allowed: true,
        namespace: "flint/allowed",
        service_name: "FlintMotionDetectionAllowed",
        result: RESULT_OK,
        calls: true,
        exit_code: 41,
    },
    Row {
        case: "a task role withholding exactly the trace action",
        family: "telemetrydenied",
        task_role: Some("containertelemetrydenied"),
        traces_allowed: false,
        namespace: "flint/denied",
        service_name: "FlintMotionDetectionDenied",
        result: RESULT_DENIED,
        calls: true,
        exit_code: 41,
    },
    Row {
        case: "a revision naming no task role, whose containers hold no credential",
        family: "telemetryroleless",
        task_role: None,
        traces_allowed: false,
        namespace: "flint/roleless",
        service_name: "FlintMotionDetectionRoleless",
        result: RESULT_DENIED,
        calls: false,
        exit_code: 41,
    },
];

/// The segment document one row's program submits: the fields `PutTraceSegments` documents as
/// required — `name`, `id`, `trace_id`, `start_time` and `end_time` — with the instants taken from
/// the clock this suite runs on, so the trace falls inside the window the read below asks for.
fn segment_document(row: &Row, at: i64) -> String {
    // "The version number, for instance, 1", then "the time of the original request, in Unix epoch
    // time, in 8 hexadecimal digits", then "a 96-bit identifier ... in 24 hexadecimal digits"
    // (`PutTraceSegments`, trace ID format), and a segment id "in 16 hexadecimal digits".
    let unique = u64::try_from(at).unwrap_or_default();
    let trace_id = format!("1-{at:08x}-{:024x}", 0x5110_0000_0000_u64 + unique);
    let segment_id = format!("{:016x}", 0x7ab0_0000_u64 + unique);
    json!({
        "name": row.service_name,
        "id": segment_id,
        "trace_id": trace_id,
        "start_time": at,
        "end_time": at + 1,
    })
    .to_string()
}

/// The program one row's container runs: it reads the parameter the account holds and reports what
/// it read, submits one segment, and publishes its own counter under the OUTCOME of that
/// submission — the shape an instrumented workload's own program has.
///
/// The read asks for the answer as text — "the output is formatted as multiple lines of
/// tab-separated string values" — so the value the account holds is among the words the command
/// wrote, which is what the case below looks for in the line the program echoed.
///
/// Written as ONE program for every row, so what differs between rows is the task role's
/// permissions and nothing about the program.
fn program(row: &Row, at: i64) -> String {
    format!(
        "MODE=$(aws ssm get-parameter --name {PARAMETER} --output text); \
         echo \"MODE=$MODE\"; \
         if aws xray put-trace-segments --trace-segment-documents '{segment}' --output text; \
         then RESULT={RESULT_OK}; else RESULT={RESULT_DENIED}; fi; \
         aws cloudwatch put-metric-data --namespace {namespace} --metric-name {METRIC} \
         --value 1 --unit Count --dimensions {RESULT_DIMENSION}=$RESULT; \
         echo \"RESULT=$RESULT\"; \
         exit {exit_code}",
        segment = segment_document(row, at),
        namespace = row.namespace,
        exit_code = row.exit_code,
    )
}

/// Register one revision of `row`'s family whose single essential container runs `program` through
/// a shell and delivers its output through the `awslogs` driver.
///
/// `entryPoint` is stated because the image's own entry point is the CLI itself: "the entry point
/// that's passed to the container" (`ContainerDefinition$entryPoint`) is what the runtime exec's,
/// and a task definition carrying a script states the shell there exactly like this.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    execution_role_arn: &str,
    program: &str,
) {
    let mut body = json!({
        "family": row.family,
        "networkMode": "awsvpc",
        "cpu": "256",
        "memory": "512",
        "executionRoleArn": execution_role_arn,
        "requiresCompatibilities": ["FARGATE"],
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
            "essential": true,
            "entryPoint": ["sh", "-c"],
            "command": [program],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": LOG_GROUP,
                    "awslogs-region": REGION,
                    "awslogs-stream-prefix": STREAM_PREFIX,
                },
            },
        }],
    });
    if let Some(task_role) = row.task_role {
        body["taskRoleArn"] = json!(format!(
            "arn:aws:iam::{}:role/{task_role}",
            engine::emulator_account_id()
        ));
    }
    ecs_ok(router, case, "RegisterTaskDefinition", body).await;
}

/// `RunTask` one Fargate task of `family` in `subnet`, and answer its ARN.
async fn run_task(router: &MultiServiceRouter, case: &str, family: &str, subnet: &str) -> String {
    let answer = ecs_ok(
        router,
        case,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": { "subnets": [subnet], "assignPublicIp": "ENABLED" }
            },
        }),
    )
    .await;
    let tasks = answer["tasks"].as_array().cloned().unwrap_or_default();
    let task = tasks.first().unwrap_or_else(|| {
        panic!("case {case:?}: expected RunTask to place one task, got {answer}")
    });
    task["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a taskArn, got {task}"))
        .to_string()
}

/// Look again until the task `task_arn` reports `STOPPED`, then answer the lines its container
/// delivered to its own `awslogs` stream and the task as `DescribeTasks` reports it.
///
/// The CONDITION is polled rather than a span slept for: the task's own end is what makes its
/// container's run certainly over, so the lines read afterwards are the whole of what the program
/// wrote.
async fn stopped_task(
    router: &MultiServiceRouter,
    case: &str,
    task_arn: &str,
) -> (Vec<String>, Value) {
    let deadline = Instant::now() + DEADLINE;
    let task = loop {
        let body = ecs_ok(
            router,
            case,
            "DescribeTasks",
            json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        )
        .await;
        let task = body["tasks"][0].clone();
        if task["lastStatus"].as_str() == Some("STOPPED") {
            break task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task never stopped, last read {task}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    };
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    let read = logs_ok(
        router,
        case,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}"),
            "startFromHead": true,
        }),
    )
    .await;
    let lines = read["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    (lines, task)
}

/// The value a line `NAME=<value>` carries, and `None` when no line names it.
fn reported(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

/// How many observations the metric `METRIC` of `namespace` holds under `Result=<result>`, as a
/// client reads them: every datapoint's `SampleCount`, summed over the window around `at`.
async fn published_samples(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    result: &str,
    at: i64,
) -> f64 {
    let read = cloudwatch_ok(
        router,
        case,
        "GetMetricStatistics",
        json!({
            "Namespace": namespace,
            "MetricName": METRIC,
            "Dimensions": [{ "Name": RESULT_DIMENSION, "Value": result }],
            "StartTime": at - WINDOW_SECS,
            "EndTime": at + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["SampleCount", "Sum"],
        }),
    )
    .await;
    read["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["SampleCount"].as_f64())
        .sum()
}

/// The traces X-Ray holds for the service `name`, as a client asks for them: the summaries a
/// `service("<name>")` filter answers over the window around `at`.
async fn traced_summaries(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    at: i64,
) -> Vec<Value> {
    let read = xray_ok(
        router,
        case,
        "/TraceSummaries",
        json!({
            "StartTime": at - WINDOW_SECS,
            "EndTime": at + WINDOW_SECS,
            "FilterExpression": format!("service(\"{name}\")"),
        }),
    )
    .await;
    read["TraceSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Everything a launch below needs, each created through the API of the service that owns it: the
/// network the Fargate tasks are placed in (with the route off the VPC a task that fetches an image
/// needs), the log group the containers deliver to, the parameter their programs read, the cluster
/// they run in, and the EXECUTION role the agent acts as. Answers the subnet and that role's ARN.
async fn given_the_account(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
) -> (String, String) {
    let vpc = ec2_element(
        router,
        case,
        "CreateVpc",
        &[("CidrBlock", "10.0.0.0/16")],
        "vpcId",
    )
    .await;
    let subnet = ec2_element(
        router,
        case,
        "CreateSubnet",
        &[
            ("VpcId", vpc.as_str()),
            ("CidrBlock", "10.0.0.0/24"),
            ("AvailabilityZone", &format!("{REGION}a")),
        ],
        "subnetId",
    )
    .await;
    crate::ec2_network::register_internet_route(engine, &vpc);
    logs_ok(
        router,
        case,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    ssm_ok(
        router,
        case,
        "PutParameter",
        json!({ "Name": PARAMETER, "Value": PARAMETER_VALUE, "Type": "String" }),
    )
    .await;
    ecs_ok(
        router,
        case,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
    )
    .await;
    let execution = role(router, case, "containertelemetryexec").await;
    grant(
        router,
        case,
        "containertelemetryexec",
        "deliver-container-logs",
        &["logs:CreateLogStream", "logs:PutLogEvents"],
    )
    .await;
    (subnet, execution)
}

/// RULE: the AWS CLI a container's session provides really makes the call each of its command
/// lines names — the parameter read, the segment submission and the metric publish — signed as the
/// task role, so what the program published is readable from the service that owns it and a role
/// withholding one action is refused exactly that call.
#[tokio::test]
async fn a_containers_program_publishes_its_telemetry_as_the_task_role() {
    let case = "the container's own telemetry calls";
    let (router, engine) = emulator();
    let (subnet, execution) = given_the_account(&router, &engine, case).await;

    for row in ROWS {
        if let Some(task_role) = row.task_role {
            role(&router, row.case, task_role).await;
            grant(
                &router,
                row.case,
                task_role,
                "publish-and-read",
                &["cloudwatch:PutMetricData", "ssm:GetParameter"],
            )
            .await;
            if row.traces_allowed {
                grant(
                    &router,
                    row.case,
                    task_role,
                    "submit-segments",
                    &["xray:PutTraceSegments", "xray:PutTelemetryRecords"],
                )
                .await;
            }
        }
    }

    // The instant every row's segment is stamped with, read once so the reads below ask for the
    // same window the programs wrote in.
    let at = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_secs(),
    )
    .expect("the epoch seconds fit");

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel in the
    // one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for row in ROWS {
        register(&router, row.case, row, &execution, &program(row, at)).await;
        let task_arn = run_task(&router, row.case, row.family, &subnet).await;
        launched.push((row, task_arn));
    }

    for (row, task_arn) in &launched {
        let case = row.case;
        let (lines, task) = stopped_task(&router, case, task_arn).await;

        // The program ran to its end and reported the status it chose.
        assert_eq!(
            task["containers"][0]["exitCode"].as_i64(),
            Some(row.exit_code),
            "case {case:?}: expected the container's own exit status {}, got {task}",
            row.exit_code
        );

        // What the program READ: the value the parameter holds is among the words the read
        // answered, and a container that holds no credential to read it with read nothing.
        let reported_mode = reported(&lines, "MODE");
        match row.calls {
            true => assert!(
                reported_mode
                    .as_deref()
                    .is_some_and(|read| read.contains(PARAMETER_VALUE)),
                "case {case:?}: expected the program to report the parameter's value \
                 {PARAMETER_VALUE:?} among what the read answered, got {lines:?}"
            ),
            false => assert_eq!(
                reported_mode.as_deref(),
                Some(""),
                "case {case:?}: expected the program to report nothing read, got {lines:?}"
            ),
        }

        // Which BRANCH the submission's status decided.
        assert_eq!(
            reported(&lines, "RESULT").as_deref(),
            Some(row.result),
            "case {case:?}: expected the program to report {:?}, got {lines:?}",
            row.result
        );

        // What it PUBLISHED: the counter under the outcome it reported, and nothing under the
        // other one.
        let published = published_samples(&router, case, row.namespace, row.result, at).await;
        let expected = if row.calls { 1.0 } else { 0.0 };
        assert_eq!(
            published, expected,
            "case {case:?}: expected {expected} observation(s) of {METRIC} under \
             {RESULT_DIMENSION}={:?} in {:?}, got {published}",
            row.result, row.namespace
        );
        let other = if row.result == RESULT_OK {
            RESULT_DENIED
        } else {
            RESULT_OK
        };
        let unpublished = published_samples(&router, case, row.namespace, other, at).await;
        assert_eq!(
            unpublished, 0.0,
            "case {case:?}: expected nothing published under {RESULT_DIMENSION}={other:?} in {:?}, \
             got {unpublished}",
            row.namespace
        );

        // What it TRACED: the segment it submitted is retrievable as a trace of the service it
        // named, and a submission the service refused left none.
        let summaries = traced_summaries(&router, case, row.service_name, at).await;
        if row.traces_allowed {
            assert_eq!(
                summaries.len(),
                1,
                "case {case:?}: expected the submitted segment to be retrievable as one trace of \
                 {:?}, got {summaries:?}",
                row.service_name
            );
        } else {
            assert!(
                summaries.is_empty(),
                "case {case:?}: expected no trace for {:?}, whose submission was not made, got \
                 {summaries:?}",
                row.service_name
            );
        }
    }
}
