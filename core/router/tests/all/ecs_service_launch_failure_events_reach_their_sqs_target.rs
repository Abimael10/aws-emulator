// SPDX-License-Identifier: CC0-1.0

//! A launch the Amazon ECS SERVICE scheduler could not make reaches the Amazon SQS target of
//! the EventBridge rule that matches it.
//!
//! The RULE under test: **a task whose containers Amazon ECS never started is a launch the
//! scheduler that asked for it LOST — the stop such a task earns is `TaskFailedToStart`, with
//! the container runtime's account of what could not be obtained in `Task$stoppedReason` — and
//! losing one is an ACTION of that scheduler, published once onto the account's DEFAULT
//! EventBridge bus as `source` `aws.ecs` / `detail-type` `ECS Service Action`, whose `detail`
//! names the action in `eventName` (`SERVICE_TASK_CONFIGURATION_FAILURE`, the scheduler's error
//! action for a service whose own configuration cannot be launched), how serious it is in
//! `eventType` (`ERROR`), the cluster it happened in in `clusterArn` and the instant it
//! happened at in `createdAt`, and whose `resources` names the SERVICE the scheduler was acting
//! for; EventBridge then routes it by its CONTENT, so a rule scoped to that `eventName`
//! receives it while a rule scoped to another of the scheduler's actions receives none of them
//! — and a service whose tasks the scheduler CAN launch publishes no such action at all.**
//!
//! Sources: the Amazon ECS Developer Guide's service action events for the
//! `eventName`/`eventType` pairs the scheduler publishes (the same table the two steady states
//! are read off), `smithy_query(query="shape", shape="com.amazonaws.ecs#TaskStopCode")` ->
//! EssentialContainerExited, InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption,
//! TaskFailedToStart, TerminationNotice, UserInitiated for the stop a launch that was never
//! made earns, and `smithy_query(query="shape", shape="PutEventsRequestEntry")` for the
//! envelope members a publication fills (`Source`, `DetailType`, `Time`, `Resources`,
//! `Detail`).
//!
//! This suite lives here because the path is cross-service: Amazon ECS publishes through the
//! in-process callback, EventBridge routes the envelope by its content, and Amazon SQS holds
//! what arrived — so what is asserted is what each of the three services persisted, with
//! nothing reading the publisher's own queue.
//!
//! The rows differ along what the rule generalises over: WHICH reference the launch cannot
//! obtain and how it is spelled — a tag the public gallery publishes no release of, the same
//! kind of reference through the registry that gallery mirrors those images from, and that
//! registry's own host-and-namespace spelling of another held repository — and how many tasks
//! the service wants, since each task the scheduler launches is a launch of its own to lose.
//! The last row is the control the differential needs: the same service on a reference the
//! gallery DOES publish, which reaches its steady state and publishes no configuration failure.
//!
//! The second test is the other half of what a client watching such a service receives: the
//! ROLLOUT that cannot launch the revision it is releasing reaches the TERMINAL state of the
//! deployment state machine, and that move is published as an `ECS Deployment State Change`
//! (`Deployment$rolloutState`: "When a service deployment is started, it begins in an
//! `IN_PROGRESS` state. When the service reaches a steady state, the deployment transitions to a
//! `COMPLETED` state. If the service fails to reach a steady state and circuit breaker is turned
//! on, the deployment transitions to a `FAILED` state"), so a rule on that detail type receives
//! the release's start and then its failure, and `DescribeServices` reports the rollout the
//! event named as failed. It drives ONE of the table's rows: what it is about is the SEQUENCE a
//! release goes through, which does not differ by which reference the launch cannot obtain.
//!
//! Every wait below looks again for the CONDITION it waits on until a generous deadline
//! passes, so a loaded machine only polls more often instead of failing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account every emulator ARN carries, which a container-instance registration's identity
/// document has to name.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `source` and `detail-type` Amazon ECS publishes a service action under, spelled out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.ecs";
const SERVICE_ACTION: &str = "ECS Service Action";

/// The `detail-type` a DEPLOYMENT's own state changes are published under, and the `eventName`
/// of each transition the rollout of a revision that cannot be launched makes.
const DEPLOYMENT_STATE_CHANGE: &str = "ECS Deployment State Change";
const DEPLOYMENT_IN_PROGRESS: &str = "SERVICE_DEPLOYMENT_IN_PROGRESS";
const DEPLOYMENT_COMPLETED: &str = "SERVICE_DEPLOYMENT_COMPLETED";
const DEPLOYMENT_FAILED: &str = "SERVICE_DEPLOYMENT_FAILED";

/// The `eventName` of the scheduler's error action for a service whose configuration it cannot
/// launch, the `eventType` that goes with it, and the action the sibling rule is scoped to
/// instead — the scheduler's own steady state.
const TASK_CONFIGURATION_FAILURE: &str = "SERVICE_TASK_CONFIGURATION_FAILURE";
const ERROR: &str = "ERROR";
const SERVICE_STEADY_STATE: &str = "SERVICE_STEADY_STATE";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// The bus an Amazon Web Services service's own events go to: "events generated by Amazon Web
/// Services services go to your account's default event bus".
const DEFAULT_BUS: &str = "default";

/// The service principal EventBridge delivers to an SQS target as, which is what the target
/// queue's own resource policy admits ("for Amazon SQS queues, EventBridge relies on
/// resource-based policies").
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The `X-Amz-Target` prefix of the Amazon ECS model, whose `sigv4Name` is `ecs`; it speaks
/// awsJson1.1.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The cluster every service below runs in.
const CLUSTER: &str = "launchfailures";

/// The container instance the scheduler places this suite's tasks on: "The EC2 launch type runs
/// your tasks on Amazon EC2 instances registered to your cluster."
const CAPACITY_INSTANCE: &str = "i-0c0ffee0c0ffee0c0";

/// How often a poll re-reads a target queue.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll keeps trying before it gives up. Deliberately far longer than the work it
/// waits for: a launch is attempted where the task's own start would have been, the stop it
/// earns falls due a span later, and the scheduler replaces each task it loses — so a service
/// that cannot launch its revision spends several spans before the deployment circuit breaker
/// watching it ends the rollout. Every poll stops the instant its own condition holds, so the
/// budget costs nothing when the action arrives and a host running many suites at once only
/// polls more often instead of failing.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// One row: the reference the service's revision names, how many tasks it wants, and whether a
/// registry this emulator holds serves that reference at all.
struct Row {
    case: &'static str,
    /// The image the revision's one container runs.
    image: &'static str,
    desired: i64,
    /// Whether the launch can obtain the reference — the row that CAN is the control the
    /// differential needs.
    launchable: bool,
}

/// The table spans the rule: which reference the launch cannot obtain, how it is spelled, and
/// how many launches the service asks for.
const ROWS: &[Row] = &[
    Row {
        case: "a tag the gallery publishes no release of",
        image: "public.ecr.aws/docker/library/busybox:no-such-release",
        desired: 1,
        launchable: false,
    },
    Row {
        case: "the same kind of reference through the registry the gallery mirrors",
        image: "busybox:no-such-release",
        desired: 1,
        launchable: false,
    },
    Row {
        case: "two launches of a service naming that registry's own spelling of another \
               repository",
        image: "registry-1.docker.io/library/nginx:no-such-release",
        desired: 2,
        launchable: false,
    },
    Row {
        case: "a release the gallery does publish",
        image: "public.ecr.aws/docker/library/busybox:latest",
        desired: 1,
        launchable: true,
    },
];

/// Amazon ECS, EventBridge and Amazon SQS over ONE shared engine — the three services a
/// published service action travels through, mounted the way the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked: the router above runs with request
/// authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `X-Amz-Target` RPC call, answered with its HTTP status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must succeed, answered with its body.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("{ECS_TARGET_PREFIX}.{operation}");
    let (status, parsed) = json_call(router, "ecs", &target, "1.1", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call that must succeed.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AWSEvents.{operation}");
    let (status, parsed) = json_call(router, "events", &target, "1.1", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call that must succeed.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let (status, parsed) = json_call(router, "sqs", &target, "1.0", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A string member at `pointer`, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// An SQS queue called `name`, created through Amazon SQS itself and admitting EventBridge's own
/// service principal; answers its url and the ARN a rule names it by.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// An ENABLED rule called `name` on the default bus carrying `pattern`, with the queue
/// `queue_arn` as its one target.
async fn rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
    queue_arn: &str,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{ "Id": "sqs-target", "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target must be accepted, got {targeted}"
    );
}

/// The pattern of a rule scoped to ONE of the scheduler's actions in the cluster `cluster_arn`,
/// narrowed through the event's own `detail` — which is what tells a correctly-scoped rule from
/// a broken one.
fn action_pattern(cluster_arn: &str, event_name: &str) -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [SERVICE_ACTION],
        "detail": { "clusterArn": [cluster_arn], "eventName": [event_name] },
    })
}

/// Every message now waiting on the queue at `url`, as the parsed event envelopes their bodies
/// carry.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// Poll the queue at `url` — and ONLY the queue — until what it has delivered satisfies
/// `arrived`, or [`POLL_BUDGET`] passes; answers everything that did arrive.
///
/// Nothing in this loop calls Amazon ECS, which is the point: a client that reacts to a service
/// through EventBridge makes no further call to the service that publishes.
async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    arrived: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, case, url).await);
        if arrived(&collected) || Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether `envelopes` holds the action `event_name` about `service_arn`.
fn holds(envelopes: &[Value], service_arn: &str, event_name: &str) -> bool {
    envelopes.iter().any(|envelope| {
        envelope["resources"] == json!([service_arn])
            && envelope["detail"]["eventName"] == json!(event_name)
    })
}

/// The envelopes of `delivered` that are about `service_arn`.
fn about(delivered: &[Value], service_arn: &str) -> Vec<Value> {
    delivered
        .iter()
        .filter(|envelope| envelope["resources"] == json!([service_arn]))
        .cloned()
        .collect()
}

/// An EC2 instance identity document, as the container agent reads it from the instance
/// metadata service: it carries the `accountId` and the `instanceId` a registration supplies.
fn identity_document(instance_id: &str) -> String {
    json!({
        "accountId": ACCOUNT,
        "instanceId": instance_id,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string()
}

/// The cluster of this suite, with one container instance registered into it through the
/// agent's own operation; answers the cluster's ARN.
async fn cluster_with_capacity(router: &MultiServiceRouter, case: &str) -> String {
    let created = ecs(
        router,
        case,
        "CreateCluster",
        &json!({ "clusterName": CLUSTER }),
    )
    .await;
    ecs(
        router,
        case,
        "RegisterContainerInstance",
        &json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity_document(CAPACITY_INSTANCE),
            // Base64 text beside the document; nothing here signs anything, and what a
            // signature is OF cannot be checked without the key that produced it.
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
    )
    .await;
    member(case, &created, "/cluster/clusterArn")
}

/// Register one revision of `family` whose single, essential container runs `image`, and answer
/// that revision's ARN.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    image: &str,
) -> String {
    let registered = ecs(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": family,
            "containerDefinitions": [{
                "name": "app",
                "image": image,
                "memory": 128,
                "essential": true,
            }],
        }),
    )
    .await;
    member(case, &registered, "/taskDefinition/taskDefinitionArn")
}

/// Create the service of `row` on the revision `family`, and answer its ARN.
///
/// The deployment circuit breaker is turned on, as a client releasing a revision that may not
/// start does: "If the service fails to reach a steady state and circuit breaker is turned on,
/// the deployment transitions to a `FAILED` state. A deployment in `FAILED` state doesn't launch
/// any new tasks" (`Deployment$rolloutState`), so the rollout of a revision that cannot be
/// launched ENDS instead of replacing its tasks for ever.
async fn create_service(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    task_definition: &str,
    service_name: &str,
) -> String {
    let created = ecs(
        router,
        case,
        "CreateService",
        &json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "taskDefinition": task_definition,
            "launchType": "EC2",
            "desiredCount": row.desired,
            "deploymentController": { "type": "ECS" },
            "deploymentConfiguration": {
                "deploymentCircuitBreaker": { "enable": true, "rollback": false },
            },
        }),
    )
    .await;
    member(case, &created, "/service/serviceArn")
}

/// The tasks of `service_name` that are on their way out, as `DescribeTasks` reports them.
async fn stopping_tasks(router: &MultiServiceRouter, case: &str, service_name: &str) -> Vec<Value> {
    let listed = ecs(
        router,
        case,
        "ListTasks",
        &json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "desiredStatus": "STOPPED",
        }),
    )
    .await;
    let arns = listed["taskArns"].as_array().cloned().unwrap_or_default();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        case,
        "DescribeTasks",
        &json!({ "cluster": CLUSTER, "tasks": arns }),
    )
    .await;
    described["tasks"].as_array().cloned().unwrap_or_default()
}

/// The pattern of a rule matching EVERY deployment state change of this bus.
fn deployment_pattern() -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [DEPLOYMENT_STATE_CHANGE],
    })
}

/// Whether `envelopes` holds the transition `event_name` of the rollout `deployment_id`.
fn holds_rollout(envelopes: &[Value], deployment_id: &str, event_name: &str) -> bool {
    envelopes.iter().any(|envelope| {
        envelope["detail"]["deploymentId"] == json!(deployment_id)
            && envelope["detail"]["eventName"] == json!(event_name)
    })
}

/// One service, as `DescribeServices` reports it.
async fn describe_service(router: &MultiServiceRouter, case: &str, service_name: &str) -> Value {
    let described = ecs(
        router,
        case,
        "DescribeServices",
        &json!({ "cluster": CLUSTER, "services": [service_name] }),
    )
    .await;
    described["services"]
        .as_array()
        .and_then(|services| services.first())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {service_name} back, got {described}"))
}

/// The id of the service's CURRENT (`PRIMARY`) rollout, as a service view reports it.
async fn current_deployment(router: &MultiServiceRouter, case: &str, service_name: &str) -> String {
    let service = describe_service(router, case, service_name).await;
    member(case, &service, "/deployments/0/id")
}

/// The rollout `deployment_id` of `service`, as that service view reports it.
fn deployment_of(case: &str, service: &Value, deployment_id: &str) -> Value {
    service["deployments"]
        .as_array()
        .and_then(|deployments| {
            deployments
                .iter()
                .find(|deployment| deployment["id"] == json!(deployment_id))
        })
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout {deployment_id} in {service}"))
}

#[tokio::test]
async fn a_launch_the_scheduler_lost_reaches_the_rule_that_matches_it() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's services cannot decide another's.
        let router = emulator();
        let cluster_arn = cluster_with_capacity(&router, case).await;

        let (failure_url, failure_arn) = queue(&router, case, "ecs-launch-failures").await;
        let (steady_url, steady_arn) = queue(&router, case, "ecs-steady-states").await;
        rule(
            &router,
            case,
            "ecs-service-task-configuration-failure",
            &action_pattern(&cluster_arn, TASK_CONFIGURATION_FAILURE),
            &failure_arn,
        )
        .await;
        rule(
            &router,
            case,
            "ecs-service-steady-state",
            &action_pattern(&cluster_arn, SERVICE_STEADY_STATE),
            &steady_arn,
        )
        .await;

        let family = format!("launchfail{index}");
        let service_name = format!("svc-{family}");
        let revision = register(&router, case, &family, row.image).await;
        let service_arn = create_service(&router, case, row, &revision, &service_name).await;

        if row.launchable {
            // The control: the scheduler reaches its steady state, which is the BARRIER that
            // says the bus has already carried this service's actions — and no configuration
            // failure was published for a service whose tasks it could launch.
            let delivered = collect_until(&router, case, &steady_url, |envelopes| {
                holds(envelopes, &service_arn, SERVICE_STEADY_STATE)
            })
            .await;
            assert!(
                holds(&delivered, &service_arn, SERVICE_STEADY_STATE),
                "case {case:?}: expected the scheduler's steady state for {service_arn}, got \
                 {delivered:?}"
            );
            let failures = about(&drain(&router, case, &failure_url).await, &service_arn);
            assert!(
                failures.is_empty(),
                "case {case:?}: a service whose tasks the scheduler can launch publishes no \
                 configuration failure, got {failures:?}"
            );
            continue;
        }

        let delivered = collect_until(&router, case, &failure_url, |envelopes| {
            holds(envelopes, &service_arn, TASK_CONFIGURATION_FAILURE)
        })
        .await;
        assert!(
            holds(&delivered, &service_arn, TASK_CONFIGURATION_FAILURE),
            "case {case:?}: expected the action {TASK_CONFIGURATION_FAILURE} for {service_arn}, \
             got {delivered:?}"
        );

        // Every envelope that rule delivered is an ECS service action of THIS cluster, about the
        // SERVICE the scheduler was acting for, reported with the severity that action carries
        // and stamped with the instant it happened at.
        for envelope in about(&delivered, &service_arn) {
            assert_eq!(
                envelope["source"],
                json!(EVENT_SOURCE),
                "case {case:?}: expected source {EVENT_SOURCE}, got {envelope}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(SERVICE_ACTION),
                "case {case:?}: expected detail-type {SERVICE_ACTION}, got {envelope}"
            );
            assert_eq!(
                envelope["detail"]["eventName"],
                json!(TASK_CONFIGURATION_FAILURE),
                "case {case:?}: the rule scoped to {TASK_CONFIGURATION_FAILURE} must receive \
                 only that action, got {envelope}"
            );
            assert_eq!(
                envelope["detail"]["eventType"],
                json!(ERROR),
                "case {case:?}: expected eventType {ERROR}, got {envelope}"
            );
            assert_eq!(
                envelope["detail"]["clusterArn"],
                json!(cluster_arn),
                "case {case:?}: expected detail.clusterArn {cluster_arn}, got {envelope}"
            );
            let created_at = envelope["detail"]["createdAt"].as_str().unwrap_or_default();
            assert!(
                created_at.contains('T') && created_at.ends_with('Z'),
                "case {case:?}: expected a UTC date-time in detail.createdAt, got {envelope}"
            );
        }

        // The rule scoped to the scheduler's STEADY STATE receives none of them: the failure
        // above is already on the bus, so an empty queue here is the routing being selective
        // rather than this assertion having looked too early.
        let steady = about(&drain(&router, case, &steady_url).await, &service_arn);
        assert!(
            steady.is_empty(),
            "case {case:?}: the rule scoped to {SERVICE_STEADY_STATE} must receive none of the \
             scheduler's failures, got {steady:?}"
        );

        // ROUND TRIP: the action the event reports is read off the launch the service really
        // lost — a task of that service, stopped under the code a task whose containers were
        // never started earns, naming the reference the launch could not obtain.
        let stopped = stopping_tasks(&router, case, &service_name).await;
        let failed: Vec<&Value> = stopped
            .iter()
            .filter(|task| task["stopCode"] == json!(TASK_FAILED_TO_START))
            .collect();
        assert!(
            !failed.is_empty(),
            "case {case:?}: expected a task of {service_name} stopped under \
             {TASK_FAILED_TO_START}, got {stopped:?}"
        );
        for task in failed {
            let reason = task["stoppedReason"].as_str().unwrap_or_default();
            assert!(
                reason.contains(row.image),
                "case {case:?}: expected the stop to name the reference {} it could not obtain, \
                 got {reason:?}",
                row.image
            );
        }
    }
}

/// The image every service of the test below is CREATED on: a release the gallery publishes, so
/// the service has a rollout that reached its steady state — the one the release under test
/// replaces.
const HEALTHY_IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

#[tokio::test]
async fn a_rollout_onto_a_revision_that_cannot_be_launched_publishes_its_failure() {
    // ONE row of the table above is enough here, and deliberately: this test is about the
    // SEQUENCE a release goes through rather than about which reference it cannot obtain —
    // which the table above spans — and the rollout it drives has to lose a launch for every
    // failure the circuit breaker watching it tolerates before it ends, so running every row
    // through that chain would buy the same assertion several times over.
    for (index, row) in ROWS.iter().filter(|row| !row.launchable).take(1).enumerate() {
        let case = row.case;
        // A fresh emulator per row, so one row's services cannot decide another's.
        let router = emulator();
        cluster_with_capacity(&router, case).await;
        let (url, queue_arn) = queue(&router, case, "ecs-deployment-events").await;
        rule(
            &router,
            case,
            "ecs-deployment-state-change",
            &deployment_pattern(),
            &queue_arn,
        )
        .await;

        let family = format!("rollout{index}");
        let service_name = format!("roll-{family}");
        let healthy_revision = register(&router, case, &family, HEALTHY_IMAGE).await;
        let service_arn =
            create_service(&router, case, row, &healthy_revision, &service_name).await;
        let healthy_rollout = current_deployment(&router, case, &service_name).await;
        let delivered = collect_until(&router, case, &url, |envelopes| {
            holds_rollout(envelopes, &healthy_rollout, DEPLOYMENT_COMPLETED)
        })
        .await;
        assert!(
            holds_rollout(&delivered, &healthy_rollout, DEPLOYMENT_IN_PROGRESS)
                && holds_rollout(&delivered, &healthy_rollout, DEPLOYMENT_COMPLETED),
            "case {case:?}: expected the first rollout of {service_arn} to publish its start and \
             its completion, got {delivered:?}"
        );

        // The release under test: another revision of the same family, whose reference no
        // registry this emulator holds serves.
        let unlaunchable = register(&router, case, &family, row.image).await;
        ecs(
            &router,
            case,
            "UpdateService",
            &json!({
                "cluster": CLUSTER,
                "service": service_name,
                "taskDefinition": unlaunchable,
            }),
        )
        .await;
        let failing_rollout = current_deployment(&router, case, &service_name).await;
        assert_ne!(
            failing_rollout, healthy_rollout,
            "case {case:?}: the release must start a rollout of its own"
        );

        let delivered = collect_until(&router, case, &url, |envelopes| {
            holds_rollout(envelopes, &failing_rollout, DEPLOYMENT_FAILED)
        })
        .await;
        assert!(
            holds_rollout(&delivered, &failing_rollout, DEPLOYMENT_IN_PROGRESS),
            "case {case:?}: expected the release to publish its start, got {delivered:?}"
        );
        assert!(
            holds_rollout(&delivered, &failing_rollout, DEPLOYMENT_FAILED),
            "case {case:?}: expected the rollout {failing_rollout} of a revision that cannot be \
             launched to publish its failure, got {delivered:?}"
        );
        assert!(
            !holds_rollout(&delivered, &failing_rollout, DEPLOYMENT_COMPLETED),
            "case {case:?}: a rollout that cannot launch its revision reaches no steady state, \
             got {delivered:?}"
        );

        // Every envelope that rule delivered about the failing rollout is an ECS deployment state
        // change naming the SERVICE the rollout belongs to.
        for envelope in delivered
            .iter()
            .filter(|envelope| envelope["detail"]["deploymentId"] == json!(failing_rollout))
        {
            assert_eq!(
                envelope["source"],
                json!(EVENT_SOURCE),
                "case {case:?}: expected source {EVENT_SOURCE}, got {envelope}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(DEPLOYMENT_STATE_CHANGE),
                "case {case:?}: expected detail-type {DEPLOYMENT_STATE_CHANGE}, got {envelope}"
            );
            assert_eq!(
                envelope["resources"],
                json!([service_arn]),
                "case {case:?}: expected {service_arn} in resources, got {envelope}"
            );
            let expected_type = if envelope["detail"]["eventName"] == json!(DEPLOYMENT_FAILED) {
                ERROR
            } else {
                "INFO"
            };
            assert_eq!(
                envelope["detail"]["eventType"],
                json!(expected_type),
                "case {case:?}: expected eventType {expected_type}, got {envelope}"
            );
        }

        // ROUND TRIP: the rollout the event names as failed is the one the control plane reports
        // as failed.
        let service = describe_service(&router, case, &service_name).await;
        let rollout = deployment_of(case, &service, &failing_rollout);
        assert_eq!(
            rollout["rolloutState"],
            json!("FAILED"),
            "case {case:?}: expected the rollout to report its failure, got {rollout}"
        );
    }
}
