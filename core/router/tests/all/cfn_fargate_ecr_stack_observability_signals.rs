// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed Amazon ECS Fargate stack — an image out of this account's Amazon
//! ECR registry, an `awslogs` log group and the metric filters declared over it — makes the
//! workload it deployed OBSERVABLE, inside the window a client's wait-for-logs loop runs in.
//!
//! RULE: a deployed `AWS::ECS::Service` is the service Amazon ECS's own `CreateService` makes,
//! so its scheduler holds the task set its `DesiredCount` asks for and each of those tasks
//! reaches `RUNNING` once the image its container names was fetched; what a running container
//! writes becomes one log event in the group its `awslogs` configuration names; every INGESTED
//! log event is run against each metric filter of that group, and each transformation of a
//! filter whose `filterPattern` matched the event publishes its `metricValue` as a datapoint of
//! the CloudWatch metric it names — while a filter of the same group that matched no event
//! publishes nothing, declaring no `defaultValue` — so an alarm over the first metric leaves
//! `INSUFFICIENT_DATA` and the one over the second cannot; and for as long as the service holds
//! a running task, Amazon ECS publishes its `AWS/ECS` `CPUUtilization` and `MemoryUtilization`
//! in `Percent`, dimensioned on the pair `ClusterName` + `ServiceName`.
//!
//! Six crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes every create call, IAM holds the execution role Amazon ECS pulls as,
//! Amazon ECR holds the repository and the image the reference resolves to, Amazon EC2 holds
//! the network the placement resolves against and the path the pull takes, Amazon ECS owns the
//! service, its tasks and their log delivery, CloudWatch Logs holds the group, what was
//! delivered to it and the filters over it, and CloudWatch holds every series and grades the
//! alarms. Nothing reaches into a store: every fact asserted is read back through the owning
//! service's own API.
//!
//! The rows differ along what the rule generalises over: HOW the revision addresses the pushed
//! image (by tag, and by the digest of that same image), HOW MANY tasks the service holds (one,
//! and two), HOW MANY matching lines each of those tasks writes (one, and two) — so what the
//! filter's metric sums is the number of matching events the group took in rather than a fixed
//! number — and the FORM of the filter pattern (a bare term, and a quoted phrase carrying
//! spaces). A recording of a single-task stack whose filter counts one line is one row.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and output reader this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions a CloudFormation and an ec2Query request carry.
const CFN_API_VERSION: &str = "2010-05-15";
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefixes of the awsJson services this suite drives, and the content type
/// each of their models declares.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const JSON_1_1: &str = "application/x-amz-json-1.1";
const JSON_1_0: &str = "application/x-amz-json-1.0";

/// The one container of every revision below, which is also the `awslogs-stream-prefix` its
/// log configuration states — so a delivered stream is `app/app/<task-id>`.
const CONTAINER: &str = "app";

/// The tag every pushed image carries.
const TAG: &str = "release";

/// A Docker image manifest V2 schema 2 referencing no blob: what the pull depends on is
/// whether the registry HOLDS the image and may serve it to the execution role, not which
/// blobs it is built from — so the layer array its media type requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// The managed policy AWS publishes for a task execution role ("AmazonECSTaskExecutionRolePolicy",
/// the task execution role policy of the Amazon ECS Developer Guide) — attached by the template
/// rather than hand-written, so the pull is decided over the document IAM really holds.
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The service principal Amazon ECS assumes a task's execution role as.
const ECS_TASKS: &str = "ecs-tasks.amazonaws.com";

/// The capability a create has to acknowledge for a template naming the IAM role it creates:
/// "if you have IAM resources with custom names, you must specify CAPABILITY_NAMED_IAM"
/// (`CreateStack$Capabilities`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The stack statuses a create is still in flight in, and the one it finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `Task$lastStatus` of a task that is running — the state its containers write in, and the
/// one its service's utilization is measured in.
const RUNNING: &str = "RUNNING";

/// The namespace every metric filter below publishes into: a client's own namespace, so what
/// this suite reads is what the deployed filters published and nothing else.
const FILTER_NAMESPACE: &str = "Emulator/DeployedFargateStack";

/// The namespace, metric names and dimension names of the `AWS/ECS` service utilization series
/// — spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
const ECS_NAMESPACE: &str = "AWS/ECS";
const CPU_UTILIZATION: &str = "CPUUtilization";
const MEMORY_UTILIZATION: &str = "MemoryUtilization";
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";

/// The `StandardUnit` a percentage is published in.
const PERCENT: &str = "Percent";

/// The alarm states this suite reads, of the three a `StateValue` reports.
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const ALARM: &str = "ALARM";

/// The statistic period every read and every alarm below aggregates over: one minute, the
/// finest an alarm over a regular-resolution metric is evaluated at.
const PERIOD_SECS: i64 = 60;

/// How far back and forward a statistics read windows, in seconds: wide enough to hold every
/// datapoint a series of this suite could carry, so a read never misses one for its window.
const WINDOW_BEHIND_SECS: f64 = 1_800.0;
const WINDOW_AHEAD_SECS: f64 = 600.0;

/// How long one wait keeps looking before it gives up, and how often it looks again. Each wait
/// gets this budget of its OWN: the deadline is only here so a failure is a failure rather than
/// a hang, every wait looks for its CONDITION rather than for a span to pass, and nothing here
/// asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// How many further looks an assertion of EMPTINESS takes. Together with [`POLL`] a window far
/// longer than the span a delivery and the extraction behind it take, so an emptiness a row
/// asserts is one that HELD rather than one read too early.
const QUIET_LOOKS: usize = 30;

/// HOW one row's revision addresses the image its container runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    /// `<repositoryUri>:<tag>`.
    Tag,
    /// `<repositoryUri>@<digest>` of that same image.
    Digest,
}

/// One row: the estate its stack declares, the image reference its revision runs, the lines its
/// container writes, and the two filters the stack puts on the group those lines land in.
struct Row {
    case: &'static str,
    stack: &'static str,
    repository: &'static str,
    role: &'static str,
    log_group: &'static str,
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
    reference: Reference,
    /// The `DesiredCount` the deployed service is created with.
    desired: usize,
    /// The lines ONE task writes that the row's matching filter pattern selects.
    matching_lines: &'static [&'static str],
    /// The lines ONE task writes that NEITHER filter pattern selects — the control that makes
    /// the quiet metric's emptiness a fact about the pattern rather than about the ingestion.
    quiet_lines: &'static [&'static str],
    /// The pattern of the filter that matches, and the metric its transformation publishes.
    matching_pattern: &'static str,
    matching_filter: &'static str,
    matching_metric: &'static str,
    /// The pattern of the filter nothing the task writes matches, and its metric.
    quiet_pattern: &'static str,
    quiet_filter: &'static str,
    quiet_metric: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: one task, the image by tag, a bare-term pattern",
        stack: "fargateobs-tagged",
        repository: "fargateobs/tagged",
        role: "fargateObsTagged",
        log_group: "/emulator/cfn/fargate-observability-tagged",
        cluster: "fargateobstagged",
        family: "fargateobstaggedtask",
        service: "fargateobstaggedsvc",
        reference: Reference::Tag,
        desired: 1,
        matching_lines: &["ERROR the deployed task refused a request"],
        quiet_lines: &["INFO the deployed task served a request"],
        matching_pattern: "ERROR",
        matching_filter: "fargateobs-tagged-errors",
        matching_metric: "TaggedErrorLines",
        quiet_pattern: "CRITICAL",
        quiet_filter: "fargateobs-tagged-critical",
        quiet_metric: "TaggedCriticalLines",
    },
    Row {
        case: "two tasks, the same image by digest, two matching lines each, a quoted phrase",
        stack: "fargateobs-pinned",
        repository: "fargateobs/pinned",
        role: "fargateObsPinned",
        log_group: "/emulator/cfn/fargate-observability-pinned",
        cluster: "fargateobspinned",
        family: "fargateobspinnedtask",
        service: "fargateobspinnedsvc",
        reference: Reference::Digest,
        desired: 2,
        matching_lines: &[
            "ERROR the deployed task refused a request",
            "ERROR the deployed task refused another request",
        ],
        quiet_lines: &["INFO the deployed task served a request"],
        matching_pattern: r#""the deployed task refused""#,
        matching_filter: "fargateobs-pinned-refusals",
        matching_metric: "PinnedRefusalLines",
        quiet_pattern: r#""no task of this stack writes this""#,
        quiet_filter: "fargateobs-pinned-silent",
        quiet_metric: "PinnedSilentLines",
    },
];

/// One emulator carrying every service this path crosses, over ONE shared engine, exactly as
/// the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. The key id
/// and the signature are not credentials: this router runs with request authentication off, so
/// nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one operation of a service whose model declares a form-encoded query wire, answered
/// with its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must succeed, answered with its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "cloudformation", CFN_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// One Amazon EC2 call that must succeed, answered with its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from EC2 {action}, got {status} with {body}"
    );
    body
}

/// One awsJson call that must succeed, answered with its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {signing_name} {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call that must succeed.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    rpc(
        router,
        case,
        "ecs",
        ECS_TARGET_PREFIX,
        JSON_1_1,
        operation,
        body,
    )
    .await
}

/// One Amazon ECR call that must succeed.
async fn ecr(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    rpc(
        router,
        case,
        "ecr",
        ECR_TARGET_PREFIX,
        JSON_1_1,
        operation,
        body,
    )
    .await
}

/// One CloudWatch Logs call that must succeed.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    rpc(
        router,
        case,
        "logs",
        LOGS_TARGET_PREFIX,
        JSON_1_1,
        operation,
        body,
    )
    .await
}

/// One CloudWatch call that must succeed. CloudWatch's `sigv4Name` is `monitoring`.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    rpc(
        router,
        case,
        "monitoring",
        CLOUDWATCH_TARGET_PREFIX,
        JSON_1_0,
        operation,
        body,
    )
    .await
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// A default subnet of the account's own default VPC, read out of Amazon EC2 itself: the
/// network a deployment that creates none of its own places its workload in, and — a default
/// subnet being a public one — the path a task given a public address takes to the registry.
async fn default_subnet(router: &MultiServiceRouter, case: &str) -> String {
    let body = ec2(
        router,
        case,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
    )
    .await;
    let subnets = elements(&body, "subnetId");
    assert!(
        !subnets.is_empty(),
        "case {case:?}: the account's default VPC must hold a default subnet, got {body}"
    );
    subnets[0].clone()
}

/// The image one row's revision runs, and the digest the registry resolved for it.
struct Image {
    /// The reference the template writes, in the form the row addresses it by.
    reference: String,
    /// The `imageDigest` Amazon ECR reported for the pushed manifest.
    digest: String,
}

/// Push one image into one repository of this account's registry through Amazon ECR's own API,
/// and answer the reference this row addresses it by.
async fn seed_image(router: &MultiServiceRouter, row: &Row) -> Image {
    let case = row.case;
    let created = ecr(
        router,
        case,
        "CreateRepository",
        json!({ "repositoryName": row.repository }),
    )
    .await;
    let uri = created["repository"]["repositoryUri"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateRepository must report a repositoryUri, got {created}")
        })
        .to_string();
    let pushed = ecr(
        router,
        case,
        "PutImage",
        json!({
            "repositoryName": row.repository,
            "imageManifest": MANIFEST,
            "imageTag": TAG,
        }),
    )
    .await;
    let digest = pushed["image"]["imageId"]["imageDigest"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: PutImage must report an imageDigest, got {pushed}")
        })
        .to_string();
    let reference = match row.reference {
        Reference::Tag => format!("{uri}:{TAG}"),
        Reference::Digest => format!("{uri}@{digest}"),
    };
    Image { reference, digest }
}

/// The program one row's container runs: every line it writes, then a command that keeps the
/// container up — so a task that is running is still running while its series is read.
fn program(row: &Row) -> String {
    let mut written = String::new();
    for line in row.matching_lines.iter().chain(row.quiet_lines) {
        written.push_str(&format!("echo \"{line}\"; "));
    }
    format!("{written}sleep 3600")
}

/// The template one row deploys: the execution role Amazon ECS pulls as, the log group its
/// container's output is delivered to, the two metric filters over that group, and the cluster,
/// Fargate revision and service that place the tasks.
///
/// The service `DependsOn` both filters, because a metric filter extracts from the events
/// INGESTED while it is installed: a stack that declares the filters beside the workload is
/// what puts them in place before the workload writes.
fn template(row: &Row, subnet: &str, image: &str) -> String {
    format!(
        r#"Resources:
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      RoleName: {role}
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Action: sts:AssumeRole
            Principal:
              Service: {ECS_TASKS}
      ManagedPolicyArns:
        - {EXECUTION_ROLE_POLICY}
  LogGroup:
    Type: AWS::Logs::LogGroup
    Properties:
      LogGroupName: {log_group}
  MatchingFilter:
    Type: AWS::Logs::MetricFilter
    Properties:
      FilterName: {matching_filter}
      LogGroupName: !Ref LogGroup
      FilterPattern: '{matching_pattern}'
      MetricTransformations:
        - MetricName: {matching_metric}
          MetricNamespace: {FILTER_NAMESPACE}
          MetricValue: "1"
  QuietFilter:
    Type: AWS::Logs::MetricFilter
    Properties:
      FilterName: {quiet_filter}
      LogGroupName: !Ref LogGroup
      FilterPattern: '{quiet_pattern}'
      MetricTransformations:
        - MetricName: {quiet_metric}
          MetricNamespace: {FILTER_NAMESPACE}
          MetricValue: "1"
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {cluster}
  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: {family}
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      RequiresCompatibilities:
        - FARGATE
      ExecutionRoleArn: !GetAtt ExecutionRole.Arn
      ContainerDefinitions:
        - Name: {CONTAINER}
          Image: {image}
          Essential: true
          Command:
            - sh
            - -c
            - '{program}'
          LogConfiguration:
            LogDriver: awslogs
            Options:
              awslogs-group: !Ref LogGroup
              awslogs-region: {REGION}
              awslogs-stream-prefix: {CONTAINER}
  Service:
    Type: AWS::ECS::Service
    DependsOn:
      - MatchingFilter
      - QuietFilter
    Properties:
      ServiceName: {service}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: {desired}
      LaunchType: FARGATE
      NetworkConfiguration:
        AwsvpcConfiguration:
          AssignPublicIp: ENABLED
          Subnets:
            - {subnet}
Outputs:
  ClusterArn:
    Value: !GetAtt Cluster.Arn
  ServiceName:
    Value: !GetAtt Service.Name
  LogGroupName:
    Value: !Ref LogGroup
"#,
        role = row.role,
        log_group = row.log_group,
        matching_filter = row.matching_filter,
        matching_pattern = row.matching_pattern,
        matching_metric = row.matching_metric,
        quiet_filter = row.quiet_filter,
        quiet_pattern = row.quiet_pattern,
        quiet_metric = row.quiet_metric,
        cluster = row.cluster,
        family = row.family,
        service = row.service,
        desired = row.desired,
        program = program(row),
    )
}

/// Deploy one row's stack.
async fn create_stack(router: &MultiServiceRouter, row: &Row, subnet: &str, image: &Image) {
    let body = template(row, subnet, &image.reference);
    cloudformation(
        router,
        row.case,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", NAMED_IAM),
        ],
    )
    .await;
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED: looked
/// at again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(router, case, "DescribeStacks", &[("StackName", stack)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        let in_flight = reported == CREATE_IN_PROGRESS || reported == ROLLBACK_IN_PROGRESS;
        if !in_flight || Instant::now() >= deadline {
            return body;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Every `ResourceStatusReason` one stack's event stream carries, which is where the refusal a
/// failing resource earned is reported.
async fn stack_failures(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let body = cloudformation(
        router,
        case,
        "DescribeStackEvents",
        &[("StackName", stack)],
    )
    .await;
    elements(&body, "ResourceStatusReason")
}

/// The task ARNs `ListTasks` reports for one deployed service once it holds as many as the row
/// asked for, looked at again until it does or the deadline passes.
async fn placed_tasks(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = ecs(
            router,
            case,
            "ListTasks",
            json!({ "cluster": row.cluster, "serviceName": row.service }),
        )
        .await;
        let arns: Vec<String> = listed["taskArns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if arns.len() >= row.desired {
            return arns;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {} placed task ARNs, got {arns:?}",
            row.desired
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// One task as `DescribeTasks` reports it once it is `RUNNING`, looked at again until it is or
/// the deadline passes.
async fn running_task(router: &MultiServiceRouter, row: &Row, task_arn: &str) -> Value {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(
            router,
            case,
            "DescribeTasks",
            json!({ "cluster": row.cluster, "tasks": [task_arn] }),
        )
        .await;
        let task = described["tasks"][0].clone();
        if task["lastStatus"].as_str() == Some(RUNNING) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the placed task to reach {RUNNING}, got {task}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Every message CloudWatch Logs holds in the streams of one group under the prefix the row's
/// log configuration states.
async fn delivered(router: &MultiServiceRouter, case: &str, log_group: &str) -> Vec<String> {
    let found = logs(
        router,
        case,
        "FilterLogEvents",
        json!({ "logGroupName": log_group, "logStreamNamePrefix": CONTAINER }),
    )
    .await;
    found["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect()
}

/// How many of `lines` the delivered messages of one group are: one count per distinct line the
/// row's container writes, summed, so a line delivered once per task is counted once per task.
fn occurrences(delivered: &[String], lines: &[&str]) -> usize {
    delivered
        .iter()
        .filter(|message| lines.contains(&message.as_str()))
        .count()
}

/// The sum of every datapoint of `metric` in the filters' namespace, and how many datapoints
/// that sum came from.
async fn published(router: &MultiServiceRouter, case: &str, metric: &str) -> (f64, usize) {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        json!({
            "Namespace": FILTER_NAMESPACE,
            "MetricName": metric,
            "StartTime": now - WINDOW_BEHIND_SECS,
            "EndTime": now + WINDOW_AHEAD_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    let points: Vec<Value> = stats["Datapoints"].as_array().cloned().unwrap_or_default();
    let sum = points.iter().filter_map(|point| point["Sum"].as_f64()).sum();
    (sum, points.len())
}

/// Every datapoint CloudWatch holds for one `AWS/ECS` metric on exactly the pair naming one
/// row's service.
async fn utilization(router: &MultiServiceRouter, row: &Row, metric: &str) -> Vec<Value> {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        row.case,
        "GetMetricStatistics",
        json!({
            "Namespace": ECS_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [
                { "Name": CLUSTER_DIMENSION, "Value": row.cluster },
                { "Name": SERVICE_DIMENSION, "Value": row.service },
            ],
            "StartTime": now - WINDOW_BEHIND_SECS,
            "EndTime": now + WINDOW_AHEAD_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Minimum", "Maximum", "Average", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// Put one alarm over `metric` of the filters' namespace, named `name`: one period of the
/// metric's `Sum` at or above one, which is the condition "a matching event arrived" states.
async fn put_alarm(router: &MultiServiceRouter, case: &str, name: &str, metric: &str) {
    cloudwatch(
        router,
        case,
        "PutMetricAlarm",
        json!({
            "AlarmName": name,
            "Namespace": FILTER_NAMESPACE,
            "MetricName": metric,
            "Statistic": "Sum",
            "Period": PERIOD_SECS,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
        }),
    )
    .await;
}

/// The `StateValue` CloudWatch reports for the alarm `name`.
async fn alarm_state(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let described = cloudwatch(
        router,
        case,
        "DescribeAlarms",
        json!({ "AlarmNames": [name] }),
    )
    .await;
    described["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Deploy every row's stack on one emulator, each over its own ECR image, and answer the
/// router and the images so a caller can assert what each row's deployment produced.
async fn deployed(router: &MultiServiceRouter) -> Vec<Image> {
    let subnet = default_subnet(router, "the shared estate").await;
    let mut images = Vec::new();
    for row in ROWS {
        images.push(seed_image(router, row).await);
    }
    // Every stack is created up front, so the clock-owed moves of all of them fall due together
    // rather than one row's wait after another's.
    for (row, image) in ROWS.iter().zip(&images) {
        create_stack(router, row, &subnet, image).await;
    }
    images
}

/// Assert one row's stack reached `CREATE_COMPLETE` and answer the handles its outputs report.
async fn completed_stack(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let case = row.case;
    let described = settled_stack(router, case, row.stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
        stack_failures(router, case, row.stack).await,
    );
    let outputs = outputs_of(&described);
    let cluster_arn = outputs
        .get("ClusterArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ClusterArn output in {outputs:?}"));
    let service = outputs
        .get("ServiceName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ServiceName output in {outputs:?}"));
    assert_eq!(
        outputs.get("LogGroupName").map(String::as_str),
        Some(row.log_group),
        "case {case:?}: the stack must report the group it declared, got {outputs:?}"
    );
    (cluster_arn, service)
}

#[tokio::test]
async fn a_deployed_fargate_stack_delivers_its_logs_and_publishes_the_metrics_over_them() {
    let router = emulator();
    let images = deployed(&router).await;

    for (row, image) in ROWS.iter().zip(&images) {
        let case = row.case;

        // The stack converged, and it reports the estate a client goes on to address.
        let (cluster_arn, service) = completed_stack(&router, row).await;
        assert!(
            cluster_arn.ends_with(&format!("cluster/{}", row.cluster)),
            "case {case:?}: expected the cluster ARN of {}, got {cluster_arn:?}",
            row.cluster
        );
        assert_eq!(
            service, row.service,
            "case {case:?}: the stack must report the service it created, got {service:?}"
        );

        // The filters the stack declared are the ones CloudWatch Logs really holds on the group.
        let filters = logs(
            &router,
            case,
            "DescribeMetricFilters",
            json!({ "logGroupName": row.log_group }),
        )
        .await;
        let mut patterns: Vec<String> = filters["metricFilters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|filter| filter["filterPattern"].as_str().map(str::to_string))
            .collect();
        patterns.sort();
        let mut declared = vec![
            row.matching_pattern.to_string(),
            row.quiet_pattern.to_string(),
        ];
        declared.sort();
        assert_eq!(
            patterns, declared,
            "case {case:?}: expected the group to hold the declared filters, got {filters}"
        );

        // The scheduler holds the task set the declaration asked for, and each of those tasks
        // ran the image the row addressed — the digest Amazon ECR resolved for it.
        let tasks = placed_tasks(&router, row).await;
        assert_eq!(
            tasks.len(),
            row.desired,
            "case {case:?}: expected {} placed tasks, got {tasks:?}",
            row.desired
        );
        for task_arn in &tasks {
            let task = running_task(&router, row, task_arn).await;
            assert_eq!(
                task["containers"][0]["imageDigest"].as_str(),
                Some(image.digest.as_str()),
                "case {case:?}: expected the container to report the pushed digest, got {task}"
            );
        }

        // Every line every task wrote was delivered to the group the stack declared — the
        // matching ones and, as the control, the ones neither pattern selects.
        let expected_matching = row.desired * row.matching_lines.len();
        let expected_quiet = row.desired * row.quiet_lines.len();
        let deadline = Instant::now() + DEADLINE;
        loop {
            let lines = delivered(&router, case, row.log_group).await;
            let matching = occurrences(&lines, row.matching_lines);
            let quiet = occurrences(&lines, row.quiet_lines);
            if matching >= expected_matching && quiet >= expected_quiet {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {expected_matching} matching and {expected_quiet} \
                 other lines in {}, got {lines:?}",
                row.log_group
            );
            ::tokio::time::sleep(POLL).await;
        }

        // The matching filter published one value per matching event the group took in: the
        // metric's total is the count of those events, whatever that count is.
        let deadline = Instant::now() + DEADLINE;
        loop {
            let lines = delivered(&router, case, row.log_group).await;
            let matching = occurrences(&lines, row.matching_lines);
            let (sum, points) = published(&router, case, row.matching_metric).await;
            #[allow(clippy::cast_precision_loss)]
            let expected = matching as f64;
            if matching >= expected_matching && (sum - expected).abs() < f64::EPSILON {
                assert!(
                    points >= 1,
                    "case {case:?}: expected {} to be reported in datapoints, got {points}",
                    row.matching_metric
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {} to total {expected}, one per matching event, got \
                 {sum} over {points} datapoints",
                row.matching_metric
            );
            ::tokio::time::sleep(POLL).await;
        }

        // ...while the filter of the same group that matched no event published nothing at all,
        // its transformation declaring no defaultValue — asserted over a WINDOW, because a
        // datapoint the ingestion owed would arrive a moment later.
        for _ in 0..QUIET_LOOKS {
            ::tokio::time::sleep(POLL).await;
            let (sum, points) = published(&router, case, row.quiet_metric).await;
            assert_eq!(
                points, 0,
                "case {case:?}: expected no datapoint of {}, got {points} totalling {sum}",
                row.quiet_metric
            );
        }

        // And for as long as the service holds its running tasks, Amazon ECS publishes its own
        // utilization on the pair that names it.
        for metric in [CPU_UTILIZATION, MEMORY_UTILIZATION] {
            let deadline = Instant::now() + DEADLINE;
            let points = loop {
                let points = utilization(&router, row, metric).await;
                if !points.is_empty() {
                    break points;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected {ECS_NAMESPACE} {metric} datapoints for \
                     {}/{}, got none",
                    row.cluster,
                    row.service
                );
                ::tokio::time::sleep(POLL).await;
            };
            for point in &points {
                assert_eq!(
                    point["Unit"].as_str(),
                    Some(PERCENT),
                    "case {case:?}: expected {metric} in {PERCENT}, got {point}"
                );
                let minimum = point["Minimum"].as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a Minimum for {metric}, got {point}")
                });
                let maximum = point["Maximum"].as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a Maximum for {metric}, got {point}")
                });
                // A utilization is a percentage of what the task definition reserves, so no
                // datapoint of either metric is negative or past 100.
                assert!(
                    (0.0..=100.0).contains(&minimum) && (0.0..=100.0).contains(&maximum),
                    "case {case:?}: expected {metric} within 0..100, got {point}"
                );
                assert!(
                    minimum <= maximum,
                    "case {case:?}: expected {metric} Minimum at most Maximum, got {point}"
                );
            }
        }
    }
}

/// The other half of the rule the deployment exists for: an alarm built over the log-derived
/// metric CHANGES STATE on the datapoints the deployed workload's lines published, and the
/// alarm over the filter that matched nothing cannot.
#[tokio::test]
async fn an_alarm_over_the_deployed_filters_metric_reaches_alarm_while_its_quiet_twin_does_not() {
    let router = emulator();
    deployed(&router).await;
    // One row: the contrast here is between two alarms over ONE deployed group, and the test
    // above already spans the image reference, the task count and the pattern form.
    let row = &ROWS[0];
    let case = row.case;
    completed_stack(&router, row).await;

    // The alarms are put before anything is waited for, so neither is evaluated on a series the
    // other had time to collect: what decides their states is what each metric receives.
    put_alarm(&router, case, "deployed-filter-matched", row.matching_metric).await;
    put_alarm(&router, case, "deployed-filter-quiet", row.quiet_metric).await;

    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = alarm_state(&router, case, "deployed-filter-matched").await;
        if state == ALARM {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the alarm over {} to reach {ALARM}, got {state:?}",
            row.matching_metric
        );
        ::tokio::time::sleep(POLL).await;
    }
    for _ in 0..QUIET_LOOKS {
        ::tokio::time::sleep(POLL).await;
        let state = alarm_state(&router, case, "deployed-filter-quiet").await;
        assert_eq!(
            state, INSUFFICIENT_DATA,
            "case {case:?}: expected the alarm over {} to stay {INSUFFICIENT_DATA}, got \
             {state:?}",
            row.quiet_metric
        );
    }
}
