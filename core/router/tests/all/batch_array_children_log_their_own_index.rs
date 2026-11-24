// SPDX-License-Identifier: CC0-1.0

//! What each CHILD of an AWS Batch array job writes, and where a client reads it back from —
//! driven end to end across the services that own the facts: Batch, Amazon ECS, Amazon VPC and
//! CloudWatch Logs on one engine.
//!
//! The RULE under test: **every child of an array job runs the ONE program its job definition
//! declares, so the only thing that can tell one child's work from another's is the index its
//! container reads (`ArrayPropertiesDetail$index`, "the job index within the array that's
//! associated with this job"); the lines each child's container writes are delivered to that
//! child's OWN CloudWatch Logs stream — the one `AttemptContainerDetail$logStreamName` names
//! for it, "the name of the CloudWatch Logs log stream that's associated with the container" —
//! one event per line, as the container wrote them; and a `ListJobs` selection by `arrayJobId`
//! "lists all child jobs from within the specified array", whatever status each child is in,
//! while the implicit `RUNNING` default belongs to a listing of a job QUEUE ("the job status
//! used to filter jobs in the specified queue … If you don't specify a status, only `RUNNING`
//! jobs are returned").**
//!
//! Two things a client does with an array job therefore work: it can read the output of a named
//! child, and it can enumerate the children at all.
//!
//! A line of the `NAME=VALUE` form is what makes this observable, and it is the form a container
//! program labels what it computed with: POSIX has it be an ARGUMENT of the command it stands
//! after ("assignment to a variable shall occur only in the command prefix"), so `echo IDX=0`
//! writes `IDX=0`. The word-level rule is pinned in the owning crate's own suite
//! (`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/container_argument_words.rs`);
//! what this suite adds is the whole chain behind it — the index AWS Batch sets in the child's
//! container, the `awslogs` delivery the job's own `logConfiguration` describes, and the stream
//! `DescribeJobs` names for each attempt.
//!
//! Sources: `smithy_query(query="shape",shape="ArrayPropertiesDetail")` for a child's `index`
//! and a parent's `size`/`statusSummary`;
//! `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt's
//! `logStreamName`; `smithy_query(query="doc",shape="ListJobsRequest",member="arrayJobId")` and
//! `member="jobStatus"` for the two listings; `smithy_query(query="shape",shape="JobStatus")`
//! for the statuses.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region of every ARN minted for them.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which the execution role below is named in.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The statuses this suite waits for, spelled out rather than imported so a wrong constant in
/// the implementation cannot make it pass. Source:
/// `smithy_query(query="shape",shape="JobStatus,CEStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const RUNNING: &str = "RUNNING";
const VALID: &str = "VALID";

/// How many children the array below has.
const SIZE: i64 = 3;

/// The image the children run: a registry this emulator holds none of, so nothing is decided
/// against the fetch and the container runs the program the definition declares.
const IMAGE: &str = "registry.example.com/simparity/array-worker:1";

/// The log group the children's `awslogs` configuration delivers to, created through CloudWatch
/// Logs' own API before the array is submitted, and the stream prefix it names.
const LOG_GROUP: &str = "/emulator/batch/array-children";
const STREAM_PREFIX: &str = "shard";

/// The network this suite's compute environment places its tasks in.
const VPC_CIDR: &str = "10.71.0.0/16";
const SUBNET_CIDR: &str = "10.71.1.0/24";
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// How long a wait gives the clocks: far above the spans Batch and Amazon ECS give the moves a
/// placed job's task makes. Every wait looks again for the CONDITION rather than sleeping a
/// fixed stretch, so a loaded machine delays this suite instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(240);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// AWS Batch, Amazon ECS, Amazon EC2 and CloudWatch Logs over ONE shared engine, the way
/// `core/router/src/lib.rs` mounts every service: the task a placed child runs as is Amazon
/// ECS's resource, the subnet and security group its placement names are Amazon VPC's, and the
/// stream its container writes to is CloudWatch Logs'.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: the router above runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The status and the parsed body of one response.
fn answered(response: http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).into_owned()))
    };
    (status, parsed)
}

/// One AWS Batch call: a `restJson1` service, so `POST <uri>` with a JSON body, which must
/// answer 200.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call, which must answer 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("logs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let (status, parsed) = answered(router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon EC2 call: an `ec2Query` service, so a form body naming the action, answered as XML.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={}&Version=2016-11-15", form_encode(action));
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ec2"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// The subnet and security group the placement names, built through Amazon EC2's own API with a
/// way off the VPC — so every id the placement carries is one that service minted and holds.
async fn network_of(router: &MultiServiceRouter) -> (String, String) {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", SUBNET_CIDR),
                ("AvailabilityZone", &format!("{REGION}a")),
            ],
        )
        .await,
        "subnetId",
    );
    let table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
    )
    .await;
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", table.as_str()),
            ("DestinationCidrBlock", DEFAULT_ROUTE),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "batch-array-children"),
                (
                    "GroupDescription",
                    "the group the tasks of this suite run under",
                ),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    (subnet, security_group)
}

/// Look again until the compute environment `name` reports `VALID`.
async fn until_environment_valid(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            case,
            "/v1/describecomputeenvironments",
            &json!({"computeEnvironments": [name]}),
        )
        .await;
        if body["computeEnvironments"][0]["status"].as_str() == Some(VALID) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the compute environment never reached {VALID}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `job_id` reports `SUCCEEDED`, and answer its detail.
async fn until_succeeded(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(router, case, "/v1/describejobs", &json!({"jobs": [job_id]})).await;
        let detail = body["jobs"][0].clone();
        if detail["status"].as_str() == Some(SUCCEEDED) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached {SUCCEEDED}, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The messages CloudWatch Logs holds in `stream`, oldest first.
async fn delivered(router: &MultiServiceRouter, case: &str, stream: &str) -> Vec<String> {
    let body = logs(
        router,
        case,
        "GetLogEvents",
        &json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The job summaries `ListJobs` answers for `request`.
async fn listed(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<Value> {
    let body = batch(router, case, "/v1/listjobs", request).await;
    body["jobSummaryList"].as_array().cloned().unwrap_or_default()
}

/// RULE: each array child's container writes its own lines — labelled by the index AWS Batch
/// sets for it — into the stream `DescribeJobs` names for that child, and `ListJobs` by
/// `arrayJobId` enumerates every child whatever status it is in while a listing of the QUEUE
/// keeps the documented `RUNNING` default.
#[tokio::test]
async fn every_array_child_writes_its_own_index_to_its_own_log_stream() {
    let case = "an array of three children";
    let router = router();
    let (subnet, security_group) = network_of(&router).await;
    logs(
        &router,
        case,
        "CreateLogGroup",
        &json!({"logGroupName": LOG_GROUP}),
    )
    .await;

    batch(
        &router,
        case,
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": "array-children-ce",
            "type": "MANAGED",
            "computeResources": {
                "type": "FARGATE",
                "maxvCpus": 16,
                "subnets": [&subnet],
                "securityGroupIds": [&security_group],
            },
        }),
    )
    .await;
    let queue = batch(
        &router,
        case,
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": "array-children-queue",
            "priority": 1,
            "computeEnvironmentOrder": [
                {"order": 1, "computeEnvironment": "array-children-ce"},
            ],
        }),
    )
    .await["jobQueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the queue's ARN"))
        .to_string();
    until_environment_valid(&router, case, "array-children-ce").await;

    // ONE program for every child: what it writes is decided by the index its container reads
    // ("the array job index … zero-based", the variable AWS Batch sets for a child), both as a
    // label the program echoes and as a value it computes from.
    batch(
        &router,
        case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": "array-children-jd",
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "containerProperties": {
                "image": IMAGE,
                "command": [
                    "sh",
                    "-c",
                    "echo IDX=$AWS_BATCH_JOB_ARRAY_INDEX; \
                     echo DOUBLE=$((AWS_BATCH_JOB_ARRAY_INDEX * 2))",
                ],
                "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/batchExecutionRole"),
                "resourceRequirements": [
                    {"type": "VCPU", "value": "0.25"},
                    {"type": "MEMORY", "value": "512"},
                ],
                "networkConfiguration": {"assignPublicIp": "ENABLED"},
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": LOG_GROUP,
                        "awslogs-stream-prefix": STREAM_PREFIX,
                    },
                },
            },
        }),
    )
    .await;

    let parent_id = batch(
        &router,
        case,
        "/v1/submitjob",
        &json!({
            "jobName": "array-children",
            "jobQueue": &queue,
            "jobDefinition": "array-children-jd",
            "arrayProperties": {"size": SIZE},
        }),
    )
    .await["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the array parent's job id"))
        .to_string();

    // Each child writes ITS OWN lines into ITS OWN stream.
    let mut streams: Vec<String> = Vec::new();
    for index in 0..SIZE {
        let child_case = format!("child {index}");
        let child_id = format!("{parent_id}:{index}");
        let detail = until_succeeded(&router, &child_case, &child_id).await;
        assert_eq!(
            detail["arrayProperties"]["index"].as_i64(),
            Some(index),
            "case {child_case}: expected the child to report its own index, got {detail}"
        );
        let stream = detail["attempts"][0]["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {child_case}: expected the attempt to name its log stream, got {detail}")
            })
            .to_string();
        let expected = vec![format!("IDX={index}"), format!("DOUBLE={}", index * 2)];
        let deadline = Instant::now() + DEADLINE;
        loop {
            let held = delivered(&router, &child_case, &stream).await;
            if held == expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {child_case}: expected {stream} to hold {expected:?}, got {held:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        streams.push(stream);
    }
    let mut unique = streams.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        streams.len(),
        "case {case:?}: each child writes to a stream of its own, got {streams:?}"
    );

    // The parent is what its children add up to.
    let parent = until_succeeded(&router, "the parent", &parent_id).await;
    assert_eq!(
        parent["arrayProperties"]["statusSummary"][SUCCEEDED].as_i64(),
        Some(SIZE),
        "case {case:?}: expected every child counted as {SUCCEEDED}, got {parent}"
    );

    // And the children are ENUMERABLE: a listing by `arrayJobId` lists all of them, whatever
    // status each is in — every one of them is terminal by now, so none is `RUNNING`.
    let children = listed(&router, case, &json!({"arrayJobId": &parent_id})).await;
    let mut indexes: Vec<i64> = children
        .iter()
        .filter_map(|child| child["arrayProperties"]["index"].as_i64())
        .collect();
    indexes.sort_unstable();
    assert_eq!(
        indexes,
        (0..SIZE).collect::<Vec<i64>>(),
        "case {case:?}: expected every child listed by arrayJobId, got {children:?}"
    );

    // A status the caller NAMES selects, in that same listing.
    let succeeded = listed(
        &router,
        case,
        &json!({"arrayJobId": &parent_id, "jobStatus": SUCCEEDED}),
    )
    .await;
    assert_eq!(
        succeeded.len(),
        usize::try_from(SIZE).unwrap_or_default(),
        "case {case:?}: expected the {SUCCEEDED} children, got {succeeded:?}"
    );
    let running = listed(
        &router,
        case,
        &json!({"arrayJobId": &parent_id, "jobStatus": RUNNING}),
    )
    .await;
    assert!(
        running.is_empty(),
        "case {case:?}: no child is {RUNNING} any more, got {running:?}"
    );

    // While a listing of the QUEUE keeps the documented default: only `RUNNING` jobs, which
    // this queue now has none of — and the same listing with a status names every job it holds.
    let queued = listed(&router, case, &json!({"jobQueue": &queue})).await;
    assert!(
        queued.is_empty(),
        "case {case:?}: a queue listing with no status answers only {RUNNING} jobs, got {queued:?}"
    );
    let queued_succeeded = listed(
        &router,
        case,
        &json!({"jobQueue": &queue, "jobStatus": SUCCEEDED}),
    )
    .await;
    assert_eq!(
        queued_succeeded.len(),
        usize::try_from(SIZE + 1).unwrap_or_default(),
        "case {case:?}: expected the parent and its {SIZE} children, got {queued_succeeded:?}"
    );
}
