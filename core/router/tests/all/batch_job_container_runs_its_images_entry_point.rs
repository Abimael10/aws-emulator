// SPDX-License-Identifier: CC0-1.0

//! What an AWS Batch job's container RUNS when its `command` names no program, driven end to
//! end across the services that own the facts: Batch, Amazon ECS, Amazon VPC, IAM, the token
//! service, Amazon S3 and CloudWatch Logs on one engine.
//!
//! The RULE under test: **a container's program is the ENTRY POINT followed by the COMMAND, so
//! a container that declares only a `command` on an image whose own entry point is a program
//! runs THAT program with the declared command as its arguments.**
//! `ContainerProperties$command` is "the command that's passed to the container. This
//! parameter maps to `Cmd` in the Create a container section of the Docker Remote API and the
//! `COMMAND` parameter to docker run", and Batch's container properties declare no entry point
//! at all (`smithy_query(query="shape", shape="ContainerProperties")`) — so the image's own
//! entry point is what the words a job definition carries are arguments TO. The AWS CLI's
//! official image is such an image: its entry point is the `aws` program, which is why a job
//! definition of it carries `["s3", "cp", …]` rather than a vector naming a program.
//!
//! Everything downstream of that follows: the transfer the CLI asks for is really made, as the
//! JOB ROLE — `ContainerProperties$jobRoleArn` is "the IAM role that the container can assume
//! for Amazon Web Services permissions" — so Amazon S3 decides it on the policies in force on
//! that role, and the job's own terminal `JobStatus` is the status the program exited with:
//! `SUCCEEDED` for a transfer that was made, `FAILED` for one the store refused.
//!
//! Why this suite lives beside the router: no one service owns the decision. The job and its
//! status are Batch's, the task and the container's run are Amazon ECS's, the role and its
//! policies are IAM's, the session is the token service's, the object is Amazon S3's and the
//! line the CLI wrote is CloudWatch Logs'. Every resource below is created through the real
//! operation of the service that owns it on ONE shared engine, and everything asserted is read
//! back through a real read: the job through `DescribeJobs`, the object through Amazon S3's own
//! `GetObject`, and the container's output through `GetLogEvents`.
//!
//! The rows differ along what the rule generalises over: the REPOSITORY the image is named by
//! (the public gallery's own namespace and the registry a reference naming no host resolves
//! against), the command the CLI is given (`cp` in each direction, `rm`), whether the line
//! names the Region explicitly, and whether the role carries the action on the resource — plus
//! the boundary row, an image whose entry point this emulator knows none of, for which no
//! program is invented.
//!
//! The SECOND half of configuring that process is pinned by the sibling suite
//! [`super::zz_array_probe`], which drives this one's harness over an ARRAY job: the variables
//! AWS Batch itself SETS in a job's container, the index among them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The bucket every transfer addresses, created through Amazon S3's own API.
pub(crate) const BUCKET: &str = "batch-cli-entry-point-objects";

/// The key the source object is written at, under the prefix the job role is granted the read
/// on, and the bytes it holds.
pub(crate) const SOURCE_KEY: &str = "src/seed.txt";
pub(crate) const PAYLOAD: &str = "the bytes the job's container carries";

/// The prefix the job role is granted the WRITE on, which is where an in-scope destination
/// lives.
pub(crate) const DESTINATION_PREFIX: &str = "dst";

/// A prefix the job role carries no grant for at all.
const OUT_OF_SCOPE_PREFIX: &str = "elsewhere";

/// The AWS CLI's official image, under the two repositories it is published in: its own
/// namespace in the public gallery, and the registry a reference naming no host resolves
/// against.
const GALLERY_CLI_IMAGE: &str = "public.ecr.aws/aws-cli/aws-cli:2.36.44";
const MIRRORED_CLI_IMAGE: &str = "amazon/aws-cli:2";
/// An image whose own entry point this emulator knows nothing of, for which no program is
/// invented.
const UNKNOWN_IMAGE: &str = "registry.example.com/simparity/batch-worker:1";

/// The `CEStatus`/`JQStatus` a compute environment and a job queue come to rest in. Source:
/// `smithy_query(query="shape", shape="CEStatus,JQStatus")`.
const VALID: &str = "VALID";

/// The terminal `JobStatus` members, written out rather than imported from a service crate, so
/// a wrong constant in the implementation cannot make this suite pass. Source:
/// `smithy_query(query="shape", shape="JobStatus")`.
pub(crate) const SUCCEEDED: &str = "SUCCEEDED";
pub(crate) const FAILED: &str = "FAILED";

/// The `stoppedReason` the compute plane words for a task whose essential container exited,
/// which is the `statusReason` the job that ran as it carries. Fixed text with no part of the
/// request in it.
const ESSENTIAL_CONTAINER_EXITED: &str = "Essential container in task exited";

/// The code Amazon S3 refuses a request no policy allows with — fixed by the Amazon S3 API, so
/// a literal here is what catches a wrong wire code.
const ACCESS_DENIED: &str = "AccessDenied";

/// The log group AWS Batch delivers a job container's output to when the job's configuration
/// names none: "the log group for Batch jobs is `/aws/batch/job`"
/// (`AttemptContainerDetail$logStreamName`).
const JOB_LOG_GROUP: &str = "/aws/batch/job";

/// The XML namespace an Amazon S3 request body carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// How long a wait gives the clock before it calls a move stalled, and how long one look leaves
/// before the next. Generous on purpose: every wait ends on the CONDITION, and the deadline is
/// only there so a move that never happens fails the test instead of hanging it.
const DEADLINE: Duration = Duration::from_secs(240);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The status the AWS CLI exits with when its transfer was not made: "1 -- Limited errors …
/// For `s3` commands, this means at least one or more s3 transfers failed" (AWS CLI return
/// codes).
const TRANSFER_FAILED: i64 = 1;

/// A SigV4 credential scope naming `service`, which is what routes the request; the router
/// below runs with request authentication off, so the key id and signature are not credentials.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The services this rule spans over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them.
pub(crate) fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
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

/// One AWS Batch call — a `restJson1` service, so `POST <uri>` with a JSON body — which must
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

/// One CloudWatch Logs call, answered with the HTTP status and the parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("logs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    answered(router.dispatch(request).await)
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket and the
/// key live in the URI and the body is the payload.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::copy_from_slice(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One awsQuery call to IAM that has to succeed, answered with its XML body.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, parameters: &[(&str, &str)]) -> String {
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
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: iam {action} must answer 200, got {status} with {text}"
    );
    text
}

/// One ec2Query action that has to succeed, answered with the first `element` of its XML body.
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
        .expect("the test request is well formed");
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

/// The trust policy a role Batch's tasks assume carries.
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

/// Create the role `name` through IAM's own API and answer its ARN.
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

/// Attach an inline policy to `role_name` allowing each `(actions, resource)` pair, through
/// IAM's own `PutRolePolicy`.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy_name: &str,
    statements: &[(&[&str], String)],
) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": statements
            .iter()
            .map(|(actions, resource)| json!({
                "Effect": "Allow",
                "Action": actions,
                "Resource": resource,
            }))
            .collect::<Vec<Value>>(),
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

/// What this suite's jobs need, each created through the API of the service that owns it: the
/// network their Fargate tasks are placed in, the bucket and source object their containers
/// address, and the two roles — the EXECUTION role the agent acts as, and the JOB role scoped
/// to the read under the source prefix and the write under the destination prefix.
pub(crate) struct Account {
    subnet: String,
    security_group: String,
    execution_role: String,
    job_role: String,
}

/// Build it.
pub(crate) async fn given_the_account(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
) -> Account {
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
            ("CidrBlock", "10.0.1.0/24"),
            ("AvailabilityZone", &format!("{REGION}a")),
        ],
        "subnetId",
    )
    .await;
    // The route off the VPC a task that fetches its image from a public registry needs.
    crate::ec2_network::register_internet_route(engine, &vpc);
    let security_group = ec2_element(
        router,
        case,
        "CreateSecurityGroup",
        &[
            ("GroupName", "batch-cli-entry-point"),
            ("GroupDescription", "the group the job tasks of this suite run under"),
            ("VpcId", vpc.as_str()),
        ],
        "groupId",
    )
    .await;
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let (bucket_status, bucket_body) =
        s3(router, "PUT", &format!("/{BUCKET}"), configuration.as_bytes()).await;
    assert_eq!(
        bucket_status, 200,
        "case {case:?}: the bucket must be created, got {bucket_status} with {bucket_body}"
    );
    for key in [SOURCE_KEY, "src/removable.txt"] {
        let (put_status, put_body) =
            s3(router, "PUT", &format!("/{BUCKET}/{key}"), PAYLOAD.as_bytes()).await;
        assert_eq!(
            put_status, 200,
            "case {case:?}: the source object {key} must be written, got {put_status} with \
             {put_body}"
        );
    }
    let execution_role = role(router, case, "batchCliEntryPointExecution").await;
    grant(
        router,
        case,
        "batchCliEntryPointExecution",
        "deliver-container-logs",
        &[(
            &["logs:CreateLogStream", "logs:PutLogEvents"],
            "*".to_string(),
        )],
    )
    .await;
    let job_role = role(router, case, "batchCliEntryPointJob").await;
    // The job role's whole scope: the read under the source prefix and the write under the
    // destination prefix, and nothing else — no removal, and nothing outside those prefixes.
    grant(
        router,
        case,
        "batchCliEntryPointJob",
        "copy-within-scope",
        &[
            (&["s3:GetObject"], format!("arn:aws:s3:::{BUCKET}/src/*")),
            (
                &["s3:PutObject"],
                format!("arn:aws:s3:::{BUCKET}/{DESTINATION_PREFIX}/*"),
            ),
        ],
    )
    .await;
    Account {
        subnet,
        security_group,
        execution_role,
        job_role,
    }
}

/// Create one MANAGED Fargate compute environment and the queue in front of it, and wait until
/// both report `VALID`.
pub(crate) async fn given_the_queue(
    router: &MultiServiceRouter,
    case: &str,
    account: &Account,
    name: &str,
    max_vcpus: i64,
) -> String {
    let environment = format!("{name}-ce");
    let queue = format!("{name}-queue");
    batch(
        router,
        case,
        "/v1/createcomputeenvironment",
        &json!({
            "computeEnvironmentName": &environment,
            "type": "MANAGED",
            "computeResources": {
                "type": "FARGATE",
                "maxvCpus": max_vcpus,
                "subnets": [&account.subnet],
                "securityGroupIds": [&account.security_group],
            },
        }),
    )
    .await;
    batch(
        router,
        case,
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": &queue,
            "priority": 1,
            "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": &environment}],
        }),
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let environments = batch(
            router,
            case,
            "/v1/describecomputeenvironments",
            &json!({"computeEnvironments": [&environment]}),
        )
        .await;
        let queues = batch(
            router,
            case,
            "/v1/describejobqueues",
            &json!({"jobQueues": [&queue]}),
        )
        .await;
        if environments["computeEnvironments"][0]["status"].as_str() == Some(VALID)
            && queues["jobQueues"][0]["status"].as_str() == Some(VALID)
        {
            return queue;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the capacity never reached {VALID}, last {environments} {queues}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Register one job definition whose container runs `image` with `command`, under the job role,
/// and answer its name.
pub(crate) async fn register(
    router: &MultiServiceRouter,
    case: &str,
    account: &Account,
    name: &str,
    image: &str,
    command: &[String],
    vcpus: &str,
) -> String {
    batch(
        router,
        case,
        "/v1/registerjobdefinition",
        &json!({
            "jobDefinitionName": name,
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "containerProperties": {
                "image": image,
                "command": command,
                "jobRoleArn": &account.job_role,
                "executionRoleArn": &account.execution_role,
                "resourceRequirements": [
                    {"type": "VCPU", "value": vcpus},
                    {"type": "MEMORY", "value": "512"},
                ],
                "networkConfiguration": {"assignPublicIp": "ENABLED"},
            },
        }),
    )
    .await;
    name.to_string()
}

/// Submit one job and answer its id.
pub(crate) async fn submit(
    router: &MultiServiceRouter,
    case: &str,
    queue: &str,
    definition: &str,
    name: &str,
    array_size: Option<i64>,
) -> String {
    let mut body = json!({
        "jobName": name,
        "jobQueue": queue,
        "jobDefinition": definition,
    });
    if let Some(size) = array_size {
        body["arrayProperties"] = json!({"size": size});
    }
    let answer = batch(router, case, "/v1/submitjob", &body).await;
    answer["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a jobId in {answer}"))
        .to_string()
}

/// The one `JobDetail` `DescribeJobs` reports for `job_id`.
pub(crate) async fn described(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let body = batch(router, case, "/v1/describejobs", &json!({"jobs": [job_id]})).await;
    let jobs = body["jobs"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        jobs.len(),
        1,
        "case {case:?}: expected exactly one described job for {job_id}, got {body}"
    );
    jobs[0].clone()
}

/// Look again until `job_id` reports a terminal `JobStatus`, and answer the detail it reports
/// then. Nothing here sleeps for a span: it waits for the CONDITION.
pub(crate) async fn until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let detail = described(router, case, job_id).await;
        let status = detail["status"].as_str().unwrap_or_default().to_string();
        if status == SUCCEEDED || status == FAILED {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What the object store holds at `key`: `Some(bytes)` for an object it holds, and `None` when
/// it answers that there is none — read through Amazon S3's own `GetObject`.
pub(crate) async fn held_object(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (status == 200).then_some(body)
}

/// The lines the container of `detail`'s attempt delivered to its own log stream.
async fn container_lines(router: &MultiServiceRouter, case: &str, detail: &Value) -> Vec<String> {
    let stream = detail["attempts"][0]["container"]["logStreamName"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if stream.is_empty() {
        return Vec::new();
    }
    let (status, read) = logs(
        router,
        "GetLogEvents",
        &json!({
            "logGroupName": JOB_LOG_GROUP,
            "logStreamName": &stream,
            "startFromHead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetLogEvents must answer 200, got {status} with {read}"
    );
    read["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect()
}

/// Look again until the container of `detail`'s attempt has delivered a line naming `name`,
/// and answer every line it delivered then.
///
/// A container writes its output as it runs and the driver delivers it, so the lines are
/// waited FOR rather than assumed to be there the instant the job settles.
pub(crate) async fn lines_until_written(
    router: &MultiServiceRouter,
    case: &str,
    detail: &Value,
    name: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let lines = container_lines(router, case, detail).await;
        if lines
            .iter()
            .any(|line| line.starts_with(&format!("{name}:")))
        {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a line naming {name} in the job's own stream, got {lines:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How one row's job must end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The CLI made the transfer and exited 0, so the job succeeded and the object named below
    /// holds the payload.
    Transferred,
    /// The object store refused the call, so the CLI exited [`TRANSFER_FAILED`], the job failed
    /// and nothing was stored.
    Refused,
    /// NOTHING was performed for the container: the image declares no entry point this emulator
    /// knows, so the words the definition carried name no program at all. No exit status is
    /// claimed.
    NothingPerformedIt,
}

/// One row: the image, the CLI invocation its command carries, and how the job ends.
struct Row {
    case: &'static str,
    image: &'static str,
    command: Vec<String>,
    /// The key the row's transfer addresses, which the assertions read back.
    key: String,
    ends: Ends,
}

/// The table. Its rows differ along what the rule generalises over — see the module header.
fn rows() -> Vec<Row> {
    let destination = |name: &str| format!("{DESTINATION_PREFIX}/{name}");
    vec![
        Row {
            case: "the gallery's own image, copying within the role's scope, naming the Region",
            image: GALLERY_CLI_IMAGE,
            command: vec![
                "s3".to_string(),
                "cp".to_string(),
                format!("s3://{BUCKET}/{SOURCE_KEY}"),
                format!("s3://{BUCKET}/{}", destination("gallery.txt")),
                "--region".to_string(),
                REGION.to_string(),
            ],
            key: destination("gallery.txt"),
            ends: Ends::Transferred,
        },
        Row {
            case: "the same image on the mirrored registry, naming no Region",
            image: MIRRORED_CLI_IMAGE,
            command: vec![
                "s3".to_string(),
                "cp".to_string(),
                format!("s3://{BUCKET}/{SOURCE_KEY}"),
                format!("s3://{BUCKET}/{}", destination("mirrored.txt")),
            ],
            key: destination("mirrored.txt"),
            ends: Ends::Transferred,
        },
        Row {
            case: "a destination outside the role's scope",
            image: GALLERY_CLI_IMAGE,
            command: vec![
                "s3".to_string(),
                "cp".to_string(),
                format!("s3://{BUCKET}/{SOURCE_KEY}"),
                format!("s3://{BUCKET}/{OUT_OF_SCOPE_PREFIX}/denied.txt"),
                "--region".to_string(),
                REGION.to_string(),
            ],
            key: format!("{OUT_OF_SCOPE_PREFIX}/denied.txt"),
            ends: Ends::Refused,
        },
        Row {
            case: "a removal the role carries no action for",
            image: GALLERY_CLI_IMAGE,
            command: vec![
                "s3".to_string(),
                "rm".to_string(),
                format!("s3://{BUCKET}/src/removable.txt"),
            ],
            key: "src/removable.txt".to_string(),
            ends: Ends::Refused,
        },
        Row {
            case: "an image whose entry point this emulator knows none of",
            image: UNKNOWN_IMAGE,
            command: vec![
                "s3".to_string(),
                "cp".to_string(),
                format!("s3://{BUCKET}/{SOURCE_KEY}"),
                format!("s3://{BUCKET}/{}", destination("unknown.txt")),
            ],
            key: destination("unknown.txt"),
            ends: Ends::NothingPerformedIt,
        },
    ]
}

/// RULE: a container that declares only a `command` runs the ENTRY POINT its image declares
/// with that command as its arguments, so a Batch job on the AWS CLI's own image really makes
/// the transfer its command asks for — as the job role, which is what decides it.
#[tokio::test]
async fn a_batch_jobs_container_runs_the_entry_point_its_image_declares() {
    let case = "the image's declared entry point";
    let (router, engine) = emulator();
    let account = given_the_account(&router, &engine, case).await;
    let queue = given_the_queue(&router, case, &account, "batch-cli", 16).await;
    let rows = rows();

    // Every definition is registered and every job submitted BEFORE any of them is waited on,
    // so the spans the clocks owe them run together rather than one after another.
    let mut submitted = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let definition = register(
            &router,
            row.case,
            &account,
            &format!("batch-cli-definition-{index}"),
            row.image,
            &row.command,
            "0.25",
        )
        .await;
        submitted.push(
            submit(
                &router,
                row.case,
                &queue,
                &definition,
                &format!("batch-cli-job-{index}"),
                None,
            )
            .await,
        );
    }

    for (row, job_id) in rows.iter().zip(&submitted) {
        let case = row.case;
        let detail = until_terminal(&router, case, job_id).await;
        let expected_status = match row.ends {
            Ends::Transferred => SUCCEEDED,
            Ends::Refused | Ends::NothingPerformedIt => FAILED,
        };
        assert_eq!(
            detail["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected {expected_status}, got {detail}"
        );
        let expected_exit = match row.ends {
            Ends::Transferred => Some(0),
            Ends::Refused => Some(TRANSFER_FAILED),
            Ends::NothingPerformedIt => None,
        };
        assert_eq!(
            detail["container"]["exitCode"].as_i64(),
            expected_exit,
            "case {case:?}: expected {expected_exit:?} as the container's exitCode, got {detail}"
        );
        let reason = detail["statusReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        match row.ends {
            Ends::Transferred | Ends::Refused => assert_eq!(
                reason, ESSENTIAL_CONTAINER_EXITED,
                "case {case:?}: a job whose container exited carries the sentence the compute \
                 plane words for it, got {reason:?}"
            ),
            // No entry point is invented for an image this emulator knows none of, so the
            // first word the definition carried is the program nothing performed — and the
            // detail the job reports NAMES it.
            Ends::NothingPerformedIt => assert!(
                reason.contains(&row.command[0]),
                "case {case:?}: expected the detail to name the program nothing performed, got \
                 {reason:?}"
            ),
        }

        // The EFFECT, read back through Amazon S3's own reader: the object the transfer carried,
        // or nothing at all where the store refused it.
        let held = held_object(&router, &row.key).await;
        match row.ends {
            Ends::Transferred => assert_eq!(
                held.as_deref(),
                Some(PAYLOAD),
                "case {case:?}: expected the destination object to hold the source bytes, got \
                 {held:?}"
            ),
            // The refused removal leaves its object where it was; the refused write stored
            // nothing at the key it addressed.
            Ends::Refused => {
                if row.command[1] == "rm" {
                    assert_eq!(
                        held.as_deref(),
                        Some(PAYLOAD),
                        "case {case:?}: a refused removal must leave the object, got {held:?}"
                    );
                } else {
                    assert!(
                        held.is_none(),
                        "case {case:?}: a refused write must have stored nothing, got {held:?}"
                    );
                }
            }
            Ends::NothingPerformedIt => assert!(
                held.is_none(),
                "case {case:?}: a program nothing performed must have stored nothing, got \
                 {held:?}"
            ),
        }

        // And what the CLI WROTE is in the stream the attempt names: a refusal reports the
        // store's own code, which is what a client reads to tell a denial from a failure.
        if row.ends == Ends::Refused {
            let lines = container_lines(&router, case, &detail).await;
            assert!(
                lines.iter().any(|line| line.contains(ACCESS_DENIED)),
                "case {case:?}: expected the CLI's account of the refusal in the job's stream, \
                 got {lines:?}"
            );
        }
    }
}

/// An image whose own entry point this emulator knows none of, so the `command` a definition
/// carries is the program itself — which is how a job declares a SHELL program.
pub(crate) const SHELL_IMAGE: &str = "registry.example.com/simparity/shard-worker:1";

