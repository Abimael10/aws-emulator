// SPDX-License-Identifier: CC0-1.0

//! What a placed job PROPAGATES to the Amazon ECS task it runs as, and what it reports when
//! that task performs NOTHING of its container's program — driven over the wire against the
//! services that own the resources.
//!
//! The RULE under test: **a placed job's end is read off the task it was placed as, and a
//! container that task reports a DETAIL for while reporting no exit status has produced the
//! only account of its run there will be — so the job settles `FAILED` carrying that detail,
//! with no exit code invented for it — while the tags a propagating job put on that task stay
//! readable where Amazon ECS holds it.** `AttemptContainerDetail$exitCode` is "the exit code
//! returned from the container … a non-zero exit code is considered failed" and `$reason` is
//! "a short (255 max characters) human-readable string to provide additional details for a
//! running or stopped container", which is the detail Amazon ECS publishes for that container
//! (`Container$reason`); `JobDetail$propagateTags` is "whether to propagate the tags from the
//! job or job definition to the corresponding Amazon ECS task … for tags with the same name,
//! job tags are given priority over job definitions tags". A job whose task owes it no status
//! must not be followed for ever, because `JobStatus` is what every client of AWS Batch waits
//! on — and the propagated set is observable exactly because the job settles.
//!
//! The contrast rows are the programs that DO answer for themselves: one that exits
//! immediately and one that declares a WAIT of its own first. Each ends the job on the status
//! its program exited with, after the time that program declared — a wait a container's
//! program asks for is the program's own, so the job is reported running until it is over.
//!
//! The table over the whole class — the programs a container declares, the statuses they end
//! with, and the propagated tags such a task carries — is
//! `core/router/tests/all/batch_job_settles_whatever_its_container_performed.rs`, driven the
//! way the assembled router mounts every service. The one case here is this crate's own: the
//! boundary at which a job settles on a detail rather than on a status.
//!
//! Sources: `smithy_query(query="shape",shape="AttemptContainerDetail")` for the attempt
//! container's `taskArn`, `exitCode` and `reason`;
//! `smithy_query(query="shape",shape="JobStatus")` for the ladder's members;
//! `smithy_query(query="doc",shape="JobDetail",member="propagateTags")` for the propagation
//! and its tie-break.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under, which the execution role below names.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `/16` this suite's VPC is carved out of, and the `/24` its one subnet takes.
const VPC_CIDR: &str = "10.73.0.0/16";
const SUBNET_CIDR: &str = "10.73.1.0/24";

/// The destination of a default route, which is what gives this suite's subnet a way off its
/// VPC — and so a way to the registry the container's image is fetched from, which the run of
/// its program waits on.
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The image the job definition below runs: a registry this emulator holds none of, so nothing
/// here is decided by the fetch.
const IMAGE: &str = "registry.example.com/simparity/batch-unperformed:1";

/// The program the container declares, which no catalog of this emulator holds — so nothing
/// performs it, and the detail the task reports for the container names it.
const DECLARED_PROGRAM: &str = "simparity-unperformed-worker";

/// The tags the job definition carries and the ones the job carries: each names one key of
/// its own and both name one SHARED key, which is the input the tie-break is read off.
const DEFINITION_TAGS: &[(&str, &str)] = &[
    ("unperformedFromDefinition", "written-by-the-job-definition"),
    ("unperformedShared", "written-by-the-job-definition"),
];
const JOB_TAGS: &[(&str, &str)] = &[
    ("unperformedFromJob", "written-by-the-job"),
    ("unperformedShared", "written-by-the-job"),
];

/// One tag list as the JSON map both Batch tag members take.
fn tag_map(tags: &[(&str, &str)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect(),
    )
}

/// How long a wait below gives the clocks: far above the spans Batch and Amazon ECS give the
/// moves a placed job's task makes. Every wait looks again for the CONDITION.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// AWS Batch, Amazon ECS and Amazon EC2 on ONE shared engine: the task a placed job runs as is
/// Amazon ECS's resource, and the subnet and security group its placement names are Amazon
/// VPC's.
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
        engine,
    ));
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: this harness runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The parsed body of a response, or the text it carried when that is not JSON.
fn decoded(payload: &[u8]) -> Value {
    if payload.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(payload)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(payload).into_owned()))
}

/// One AWS Batch call, as a client makes it: `POST /v1/<operation>` with a JSON body.
async fn batch(router: &MultiServiceRouter, uri: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = decoded(response.into_body().as_ref());
    assert_eq!(
        status, 200,
        "case {uri}: expected HTTP 200, got {status} with {answered}"
    );
    answered
}

/// One Amazon ECS call, as a client makes it.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ecs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = decoded(response.into_body().as_ref());
    assert_eq!(
        status, 200,
        "case ecs {operation}: expected HTTP 200, got {status} with {answered}"
    );
    answered
}

/// One Amazon EC2 call: an `ec2Query` service, so a form body naming the action, answered as
/// XML. Every value below is a literal with nothing form encoding exists for, so the body
/// carries them as they are.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2016-11-15");
    for (name, value) in params {
        assert!(
            !value.contains(['&', '=', '+', '%', ' ']),
            "setup: the value of {name} needs form escaping: {value:?}"
        );
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ec2"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// The text of the first `<name>` element of an XML answer: the id Amazon EC2 minted.
fn element(xml: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let from = xml
        .find(&open)
        .unwrap_or_else(|| panic!("setup: expected a {name} in {xml}"))
        + open.len();
    let rest = &xml[from..];
    let to = rest
        .find(&close)
        .unwrap_or_else(|| panic!("setup: expected a closing {name} in {xml}"));
    rest[..to].to_string()
}

/// Look again until the compute environment `name` reports `VALID`.
async fn until_environment_valid(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(
            router,
            "/v1/describecomputeenvironments",
            json!({"computeEnvironments": [name]}),
        )
        .await;
        if body["computeEnvironments"][0]["status"].as_str() == Some("VALID") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the compute environment never reached VALID, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `job_id` reports a terminal `JobStatus`, and answer its detail.
async fn until_terminal(router: &MultiServiceRouter, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = batch(router, "/v1/describejobs", json!({"jobs": [job_id]})).await;
        let detail = body["jobs"][0].clone();
        let status = detail["status"].as_str().unwrap_or_default();
        if status == "SUCCEEDED" || status == "FAILED" {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a job whose task performed none of its container's program settles `FAILED` on the
/// detail that task reports for the container, with no exit code invented for it — and the
/// tags the propagating job put on that task are readable where Amazon ECS holds it.
#[tokio::test]
async fn a_job_whose_container_performed_nothing_settles_on_the_tasks_own_detail() {
    let case = "a program no catalog of this emulator holds";
    let router = router();

    // The network the placement names, built through Amazon EC2's own API so every id is one
    // that service minted and holds.
    let vpc = element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &router,
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
    // The way off the VPC, so the registry the container's image names is reachable from the
    // subnet its task is placed in: the run of its program waits on that fetch first.
    let gateway = element(
        &ec2(&router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        &router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let table = element(
        &ec2(&router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        &router,
        "AssociateRouteTable",
        &[
            ("RouteTableId", table.as_str()),
            ("SubnetId", subnet.as_str()),
        ],
    )
    .await;
    ec2(
        &router,
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
            &router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "batch-unperformed"),
                ("GroupDescription", "the-group-this-suites-task-runs-under"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );

    batch(
        &router,
        "/v1/createcomputeenvironment",
        json!({
            "computeEnvironmentName": "batch-unperformed-ce",
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
    batch(
        &router,
        "/v1/createjobqueue",
        json!({
            "jobQueueName": "batch-unperformed-queue",
            "priority": 1,
            "computeEnvironmentOrder": [
                {"order": 1, "computeEnvironment": "batch-unperformed-ce"},
            ],
        }),
    )
    .await;
    batch(
        &router,
        "/v1/registerjobdefinition",
        json!({
            "jobDefinitionName": "batch-unperformed-definition",
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "tags": tag_map(DEFINITION_TAGS),
            "containerProperties": container_running(&[DECLARED_PROGRAM, "--once"]),
        }),
    )
    .await;
    batch(
        &router,
        "/v1/registerjobdefinition",
        json!({
            "jobDefinitionName": "batch-performed-definition",
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "tags": tag_map(DEFINITION_TAGS),
            "containerProperties": container_running(&["echo", "the batch job ran this"]),
        }),
    )
    .await;
    batch(
        &router,
        "/v1/registerjobdefinition",
        json!({
            "jobDefinitionName": "batch-waiting-definition",
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "tags": tag_map(DEFINITION_TAGS),
            // The shape a workload that does work for a while takes: a shell program, named by
            // the path a container names its shell by, that WAITS and then writes a line. The
            // wait it declares is the time its own program takes, so the job settles once that
            // program is over — which is what the poll below waits for, never a span of its
            // own.
            "containerProperties": container_running(&["/bin/sh", "-c", "sleep 2; echo done"]),
        }),
    )
    .await;
    until_environment_valid(&router, "batch-unperformed-ce").await;

    // Every job is submitted BEFORE any is waited on, so the spans the clocks owe them run
    // together rather than one after another.
    let unperformed = submit(&router, "batch-unperformed-job", "batch-unperformed-definition").await;
    let performed = submit(&router, "batch-performed-job", "batch-performed-definition").await;
    let waiting = submit(&router, "batch-waiting-job", "batch-waiting-definition").await;

    // The job whose container's program nothing performed SETTLES, and it settles FAILED: no
    // container of its task exited, so there is no status for it to have succeeded on.
    let detail = until_terminal(&router, &unperformed).await;
    assert_eq!(
        detail["status"].as_str(),
        Some("FAILED"),
        "case {case:?}: expected the job to settle FAILED, got {detail}"
    );
    let reason = detail["statusReason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        reason.contains(DECLARED_PROGRAM),
        "case {case:?}: expected the end to name {DECLARED_PROGRAM:?}, got {reason:?}"
    );
    for (where_, found) in [
        ("the job's container", &detail["container"]["exitCode"]),
        (
            "the attempt's container",
            &detail["attempts"][0]["container"]["exitCode"],
        ),
    ] {
        assert!(
            found.as_i64().is_none(),
            "case {case:?}: expected no exitCode on {where_}, got {found} in {detail}"
        );
    }

    // The detail the job reports is the TASK's own: Amazon ECS publishes it for the container
    // of the task the attempt names.
    let task = described_task(&router, &detail, case).await;
    assert_eq!(
        task["containers"][0]["reason"].as_str(),
        Some(reason.as_str()),
        "case {case:?}: expected the job's end to be the detail Amazon ECS reports, got {task}"
    );
    assert!(
        task["containers"][0]["exitCode"].is_null(),
        "case {case:?}: expected no exitCode where nothing ran, got {task}"
    );

    // The CONTRAST, which is also what the propagation is read off: a job whose container's
    // program this emulator does perform ends on the status that program exited with, and the
    // task it ran as — stopped by the exit of its own essential container — still carries the
    // tags the propagation put on it: the definition's and the job's, the job's value winning
    // on the key they share, which is what the chain below leaves since the job's entries are
    // collected last.
    let contrast = "a program this emulator performs";
    let detail = until_terminal(&router, &performed).await;
    assert_eq!(
        detail["status"].as_str(),
        Some("SUCCEEDED"),
        "case {contrast:?}: expected the job to settle SUCCEEDED, got {detail}"
    );
    assert_eq!(
        detail["attempts"][0]["container"]["exitCode"].as_i64(),
        Some(0),
        "case {contrast:?}: expected the status its program exited with, got {detail}"
    );
    let task = described_task(&router, &detail, contrast).await;
    let expected: std::collections::BTreeMap<String, String> = DEFINITION_TAGS
        .iter()
        .chain(JOB_TAGS.iter())
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    assert_eq!(
        task_tags(&task),
        expected,
        "case {contrast:?}: expected the merged tag set on the task, got {task}"
    );

    // And the shape a workload that does work for a while takes: a program that WAITS and then
    // writes a line takes the time it declared and the job settles on the status it exited
    // with — the wait is the PROGRAM's, so a client watching the job sees it running until the
    // program is over and then SUCCEEDED, with the same propagated set on its task.
    let waited = "a program that declares a wait of its own";
    let detail = until_terminal(&router, &waiting).await;
    assert_eq!(
        detail["status"].as_str(),
        Some("SUCCEEDED"),
        "case {waited:?}: expected the job to settle SUCCEEDED, got {detail}"
    );
    assert_eq!(
        detail["attempts"][0]["container"]["exitCode"].as_i64(),
        Some(0),
        "case {waited:?}: expected the status its program exited with, got {detail}"
    );
    let task = described_task(&router, &detail, waited).await;
    assert_eq!(
        task_tags(&task),
        expected,
        "case {waited:?}: expected the merged tag set on the task, got {task}"
    );
}

/// The tags one described Amazon ECS task carries, read into a map so a comparison does not
/// depend on the order the entries came back in.
fn task_tags(task: &Value) -> std::collections::BTreeMap<String, String> {
    task["tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|tag| {
            Some((
                tag["key"].as_str()?.to_string(),
                tag["value"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

/// The `containerProperties` of a Fargate job definition running `command`.
fn container_running(command: &[&str]) -> Value {
    json!({
        "image": IMAGE,
        "command": command,
        // "For jobs that run on Fargate resources, you must provide an execution role"
        // (`ContainerProperties$executionRoleArn`).
        "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/batchExecutionRole"),
        // The public address an internet gateway carries traffic for, which is what lets the
        // task's interface reach the registry the image names.
        "networkConfiguration": {"assignPublicIp": "ENABLED"},
        "resourceRequirements": [
            {"type": "VCPU", "value": "0.25"},
            {"type": "MEMORY", "value": "512"},
        ],
    })
}

/// Submit one job of `definition` to this suite's queue, propagating the tags of both sides,
/// and answer its id.
async fn submit(router: &MultiServiceRouter, name: &str, definition: &str) -> String {
    let submitted = batch(
        router,
        "/v1/submitjob",
        json!({
            "jobName": name,
            "jobQueue": "batch-unperformed-queue",
            "jobDefinition": definition,
            "propagateTags": true,
            "tags": tag_map(JOB_TAGS),
        }),
    )
    .await;
    submitted["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a jobId in {submitted}"))
        .to_string()
}

/// The Amazon ECS task one job's attempt names, as `DescribeTasks` reports it with its tags.
async fn described_task(router: &MultiServiceRouter, detail: &Value, case: &str) -> Value {
    let task_arn = detail["attempts"][0]["container"]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task the job ran as, got {detail}"))
        .to_string();
    let cluster = task_arn
        .split('/')
        .nth(1)
        .unwrap_or_else(|| panic!("case {case:?}: expected a task ARN, got {task_arn}"))
        .to_string();
    let answered = ecs(
        router,
        "DescribeTasks",
        json!({"cluster": &cluster, "tasks": [&task_arn], "include": ["TAGS"]}),
    )
    .await;
    answered["tasks"][0].clone()
}
