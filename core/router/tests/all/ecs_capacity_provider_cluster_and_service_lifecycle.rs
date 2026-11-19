// SPDX-License-Identifier: CC0-1.0

//! The EC2-capacity bootstrap's SERVICE and CLUSTER lifecycle: a cluster whose capacity is
//! supplied by a capacity provider over one of the caller's Auto Scaling groups is created,
//! given a service over a `capacityProviderStrategy`, and then torn down again.
//!
//! This suite lives here because the bootstrap spans four services over one engine and only
//! holds if all four agree: Amazon EC2 holds the launch template, Amazon EC2 Auto Scaling
//! holds the group the provider binds, Amazon ECS creates the provider, the cluster and the
//! service over it, and every resource is created through its OWN API — nothing is written
//! into a store by hand.
//!
//! RULE: a lifecycle call on a cluster whose capacity comes from a capacity provider is
//! answered from the state the account holds, not refused. `CreateService` whose
//! `capacityProviderStrategy` names a provider the cluster is associated with answers HTTP
//! 200 with the created `Service` (its `serviceArn`, `serviceName`, `clusterArn`, the
//! strategy it was placed by, its `desiredCount`, its `deploymentConfiguration`, its
//! `deploymentController`, a `PRIMARY` deployment and its `currentServiceRevisions`);
//! `DeleteService` answers 200 with that service in the status the deletion leaves it
//! (`DRAINING` or `INACTIVE`, both of which "can still be viewed with the DescribeServices
//! API operation"); and `DeleteCluster` answers 200 with the cluster at `INACTIVE` — an
//! ACCOUNT-scoped capacity provider still associated with it is no precondition, since
//! `DeleteCluster`'s preconditions are the container instances, the tasks and the services it
//! still holds. HTTP 400 is reserved for the conditions the operation models, each answered
//! with its own `__type` and with the sentence Amazon ECS words for that class.
//!
//! The rows of the lifecycle table differ along what the rule generalises over: how the
//! request NAMES the cluster (short name or full ARN) and the provider (short name or the ARN
//! the create minted), the `base` and `weight` of the strategy entry — the recorded
//! `base: 0`/`weight: 1`, each member's published bounds, and an entry that omits `base`
//! altogether — how MANY entries the strategy carries, and the `desiredCount` the service
//! wants (none, and one the empty group cannot place).
//!
//! Each row also pins the other half of what a client of this bootstrap waits on: the rollout
//! the create started leaves `IN_PROGRESS` for a TERMINAL `Deployment$rolloutState` in bounded
//! time, so a `DescribeServices` poll for one terminates.
//!
//! The refusal table is the other half of the same rule: one row per modelled condition,
//! each asserting the status, the error code and the `message` the operation words that
//! condition with, so a client can tell a bad request from a cluster the emulator cannot
//! find. One of them carries NO message member: a `DeleteService` whose identifier resolves
//! to nothing answers the class alone, while an `UpdateService` of that same identifier
//! states a sentence — the recorded wording of each, which is what the rows pin.
//!
//! Every expected sentence is spelled out here rather than imported from the service crate, so
//! a wrong constant in the implementation cannot make a row pass.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the ec2Query and awsQuery requests below carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_API_VERSION: &str = "2011-01-01";

/// The `X-Amz-Target` prefix Amazon ECS's model fixes (service shape
/// `AmazonEC2ContainerServiceV20141113`); it speaks awsJson1.1.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The access key id every request below presents: the account's own published credential.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The AMI and instance type the launch template names, and the template every Auto Scaling
/// group of this suite launches from.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
const TEMPLATE: &str = "ecscplifecycle-template";

/// The task-definition family every service below runs, and the image its one container
/// names. An `EC2`-compatible `bridge` revision, so a placement needs no network
/// configuration of its own.
const FAMILY: &str = "cplifecycleapp";
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// `Service$status` of a service that is running (`CreateService`'s own answer).
const SERVICE_ACTIVE: &str = "ACTIVE";

/// The two statuses a deleted service is left in: `DeleteService` "Services in the DRAINING
/// or INACTIVE status can still be viewed with the DescribeServices API operation".
const DELETED_SERVICE_STATUSES: [&str; 2] = ["DRAINING", "INACTIVE"];

/// `Cluster$status` of a deleted cluster: "INACTIVE: The cluster has been deleted."
const CLUSTER_INACTIVE: &str = "INACTIVE";

/// `CapacityProvider$status` a provider over one of the caller's own Auto Scaling groups is
/// created under — that member's doc: "Only capacity providers in an ACTIVE state can be used
/// in a cluster."
const CAPACITY_PROVIDER_ACTIVE: &str = "ACTIVE";

/// `CapacityProvider$type` of a provider over such a group.
const CAPACITY_PROVIDER_EC2_AUTOSCALING: &str = "EC2_AUTOSCALING";

/// `Deployment$status` of the rollout a created service is on: "PRIMARY: The most recent
/// deployment of a service."
const DEPLOYMENT_PRIMARY: &str = "PRIMARY";

/// `DeploymentController$type` of a service the Amazon ECS scheduler rolls out.
const DEPLOYMENT_CONTROLLER_ECS: &str = "ECS";

/// `Deployment$rolloutState`'s whole value set (`smithy_query(query="shape",
/// shape="DeploymentRolloutState")` -> COMPLETED, FAILED, IN_PROGRESS) and its two TERMINAL
/// members: "When a service deployment is started, it begins in an IN_PROGRESS state. When
/// the service reaches a steady state, the deployment transitions to a COMPLETED state. If
/// the service fails to reach a steady state and circuit breaker is turned on, the deployment
/// transitions to a FAILED state."
const ROLLOUT_STATES: [&str; 3] = ["COMPLETED", "FAILED", "IN_PROGRESS"];
const TERMINAL_ROLLOUT_STATES: [&str; 2] = ["COMPLETED", "FAILED"];

/// `Cluster$attachmentsStatus` while the capacity-provider attachments a cluster create
/// started are still being made: "Cluster attachments must be in UPDATE_COMPLETE or
/// UPDATE_FAILED state before they can be updated."
const ATTACHMENTS_UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";

/// How often a poll re-reads the cluster it is waiting on, and how long it keeps trying
/// before it gives up: the attachments update is a transition the CLOCK owes, so the wait is
/// for that condition with generous margin for a loaded test machine.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// The published bounds of the two numeric members of a strategy entry
/// (`CapacityProviderStrategyItem$base`: "The valid range is 0 to 100,000";
/// `CapacityProviderStrategyItem$weight`: "The valid range is 0 to 1,000").
const MAX_BASE: i64 = 100_000;
const MAX_WEIGHT: i64 = 1_000;

/// The modelled error codes the refusal rows expect.
const CLUSTER_NOT_FOUND: &str = "ClusterNotFoundException";
const SERVICE_NOT_FOUND: &str = "ServiceNotFoundException";
const CLUSTER_CONTAINS_SERVICES: &str = "ClusterContainsServicesException";
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentences Amazon ECS words those refusals with.
const CLUSTER_NOT_FOUND_MESSAGE: &str = "Cluster not found.";
const SERVICE_NOT_FOUND_MESSAGE: &str = "Service not found.";
const CLUSTER_CONTAINS_SERVICES_MESSAGE: &str =
    "The Cluster cannot be deleted while Services are active.";
const SERVICE_SCALED_ABOVE_ZERO_MESSAGE: &str =
    "The service cannot be stopped while it is scaled above 0.";
const LAUNCH_TYPE_AND_STRATEGY_MESSAGE: &str =
    "Specifying both a launch type and capacity provider strategy is not supported. Remove \
     one and try again.";
const PROVIDER_NOT_ASSOCIATED_MESSAGE: &str =
    "The specified capacity provider strategy cannot contain a capacity provider that is not \
     associated with the cluster. Associate the capacity provider with the cluster or specify \
     a valid capacity provider and try again.";

/// The whole emulator over one engine, exactly as the binary assembles it: Amazon EC2, Amazon
/// EC2 Auto Scaling and Amazon ECS are all reachable through it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope in the `Authorization` header is what names the caller.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to `signing_name` and answer its status and XML body.
async fn send_form(
    router: &MultiServiceRouter,
    signing_name: &str,
    body: String,
) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/\
                 {signing_name}/aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send_form(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Amazon EC2 Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) =
        send_form(router, "autoscaling", form(action, ASG_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Amazon ECS call, answered with its status and parsed body.
async fn ecs_call(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECS_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/ecs/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must answer HTTP 200, with the case it belongs to in the panic.
async fn ecs(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
    case: &str,
) -> Value {
    let (status, body) = ecs_call(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {body}"
    );
    body
}

/// The modelled error code of a call that must answer HTTP 400, and the `message` it words
/// the condition with — `None` for an operation that carries no `message` member at all.
async fn ecs_refusal(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
    case: &str,
) -> (String, Option<String>) {
    let (status, body) = ecs_call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to answer 400, got {status} with {body}"
    );
    let code = body["__type"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a __type naming the refusal class, got {body}")
        })
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string();
    let message = body["message"]
        .as_str()
        .or(body["Message"].as_str())
        .map(str::to_string);
    (code, message)
}

/// A required string member of a response body.
fn string(body: &Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// An integer member whose modelled default is `0`, so an absent member reads as zero.
fn count(body: &Value, member: &str, case: &str) -> i64 {
    match &body[member] {
        Value::Null => 0,
        value => value
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: expected an integer {member}, got {body}")),
    }
}

/// A list member of a response body, absent reading as empty.
fn list(body: &Value, member: &str, case: &str) -> Vec<Value> {
    match &body[member] {
        Value::Null => Vec::new(),
        Value::Array(items) => items.clone(),
        other => panic!("case {case:?}: expected a list {member}, got {other}"),
    }
}

/// Create the launch template every Auto Scaling group of this suite launches from, through
/// Amazon EC2's own API — a group launches from a template THE ACCOUNT HOLDS.
async fn given_launch_template(router: &MultiServiceRouter) {
    ec2(
        router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
}

/// Create one Auto Scaling group holding NO instances (the zero-capacity group of the
/// recorded bootstrap) through Auto Scaling's own API, and answer the ARN that service minted
/// for it.
async fn given_group(router: &MultiServiceRouter, name: &str) -> String {
    let zone = format!("{REGION}a");
    asg(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("MinSize", "0"),
            ("MaxSize", "0"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("AvailabilityZones.member.1", zone.as_str()),
        ],
    )
    .await;
    let described = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    element(&described, "AutoScalingGroupARN")
}

/// Create one capacity provider over the group `group`, and answer the ARN Amazon ECS minted
/// for it — read back through `DescribeCapacityProviders`, so what the strategy below names is
/// a provider that service really holds, at the status and of the type it reports.
async fn given_provider(router: &MultiServiceRouter, name: &str, group: &str, case: &str) -> String {
    given_group(router, group).await;
    ecs(
        router,
        "CreateCapacityProvider",
        &json!({
            "name": name,
            "autoScalingGroupProvider": { "autoScalingGroupArn": group },
        }),
        case,
    )
    .await;
    let described = ecs(
        router,
        "DescribeCapacityProviders",
        &json!({ "capacityProviders": [name] }),
        case,
    )
    .await;
    let provider = list(&described, "capacityProviders", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the provider back, got {described}"));
    assert_eq!(
        (
            string(&provider, "status", case),
            string(&provider, "type", case)
        ),
        (
            CAPACITY_PROVIDER_ACTIVE.to_string(),
            CAPACITY_PROVIDER_EC2_AUTOSCALING.to_string()
        ),
        "case {case:?}: a provider over the caller's own group is usable at once, got {provider}"
    );
    string(&provider, "capacityProviderArn", case)
}

/// Create one cluster associated with `providers`, whose default strategy is the first of
/// them, and answer its ARN — the cluster create of the recorded bootstrap.
async fn given_cluster(
    router: &MultiServiceRouter,
    name: &str,
    providers: &[&str],
    case: &str,
) -> String {
    let created = ecs(
        router,
        "CreateCluster",
        &json!({
            "clusterName": name,
            "capacityProviders": providers,
            "defaultCapacityProviderStrategy": [{
                "capacityProvider": providers[0],
                "weight": 1,
                "base": 0,
            }],
        }),
        case,
    )
    .await;
    string(&created["cluster"], "clusterArn", case)
}

/// Register the one task-definition revision every service below runs.
async fn given_revision(router: &MultiServiceRouter, case: &str) -> String {
    let registered = ecs(
        router,
        "RegisterTaskDefinition",
        &json!({
            "family": FAMILY,
            "networkMode": "bridge",
            "requiresCompatibilities": ["EC2"],
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": 128,
            }],
        }),
        case,
    )
    .await;
    string(&registered["taskDefinition"], "taskDefinitionArn", case)
}

/// Wait until the capacity-provider attachments of `cluster` are no longer being made, so the
/// delete below is weighed on its own preconditions rather than refused for a busy cluster.
async fn attachments_settled(router: &MultiServiceRouter, cluster: &str, case: &str) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let described = ecs(
            router,
            "DescribeClusters",
            &json!({ "clusters": [cluster], "include": ["ATTACHMENTS"] }),
            case,
        )
        .await;
        let status = list(&described, "clusters", case)
            .first()
            .and_then(|cluster| cluster["attachmentsStatus"].as_str().map(str::to_string))
            .unwrap_or_default();
        if status != ATTACHMENTS_UPDATE_IN_PROGRESS {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the cluster's attachments to settle, got {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the lifecycle table: the resources it names, how its request spells them, and
/// the strategy its service is placed by.
struct Row {
    case: &'static str,
    /// What this row's cluster, provider, group and service are named.
    slug: &'static str,
    /// Whether the requests name the cluster by its full ARN rather than its short name.
    cluster_by_arn: bool,
    /// Whether the strategy names the provider by the ARN the create minted.
    provider_by_arn: bool,
    /// The `base` of the strategy's first entry, and `None` for an entry that omits the
    /// member altogether.
    base: Option<i64>,
    /// The `weight` of that entry.
    weight: i64,
    /// The `weight` of a SECOND entry, over another provider of the same cluster — `None` for
    /// a one-entry strategy. Only one entry of a strategy may carry a `base`
    /// (`CapacityProviderStrategyItem$base`: "Only one capacity provider in a capacity
    /// provider strategy can have a base defined"), so the second never does.
    second_weight: Option<i64>,
    /// The `desiredCount` the service is created with.
    desired: i64,
}

const ROWS: [Row; 6] = [
    Row {
        case: "the recorded entry: base 0, weight 1, every name a short name",
        slug: "recorded",
        cluster_by_arn: false,
        provider_by_arn: false,
        base: Some(0),
        weight: 1,
        second_weight: None,
        desired: 0,
    },
    Row {
        case: "the cluster named by its full ARN and the provider by the ARN ECS minted",
        slug: "byarn",
        cluster_by_arn: true,
        provider_by_arn: true,
        base: Some(0),
        weight: 1,
        second_weight: None,
        desired: 0,
    },
    Row {
        case: "an entry that omits base, leaving the member its modelled default",
        slug: "nobase",
        cluster_by_arn: false,
        provider_by_arn: false,
        base: None,
        weight: 1,
        second_weight: None,
        desired: 0,
    },
    Row {
        case: "each member at the top of its published range",
        slug: "maxima",
        cluster_by_arn: false,
        provider_by_arn: false,
        base: Some(MAX_BASE),
        weight: MAX_WEIGHT,
        second_weight: None,
        desired: 0,
    },
    Row {
        case: "a strategy carrying two entries, over both providers of the cluster",
        slug: "twoentries",
        cluster_by_arn: false,
        provider_by_arn: false,
        base: Some(1),
        weight: 1,
        second_weight: Some(2),
        desired: 0,
    },
    Row {
        case: "a service that wants a task the zero-capacity group cannot place",
        slug: "wantsone",
        cluster_by_arn: false,
        provider_by_arn: false,
        base: Some(0),
        weight: 1,
        second_weight: None,
        desired: 1,
    },
];

/// The names this row's resources take.
impl Row {
    fn cluster(&self) -> String {
        format!("cplifecycle-{}", self.slug)
    }

    fn provider(&self) -> String {
        format!("cplifecycleprovider-{}", self.slug)
    }

    fn second_provider(&self) -> String {
        format!("cplifecycleprovider-{}-two", self.slug)
    }

    fn service(&self) -> String {
        format!("cplifecycleservice-{}", self.slug)
    }
}

/// The strategy entries this row's `CreateService` carries, over the references
/// `providers` names its providers by.
fn strategy_of(row: &Row, providers: &[String]) -> Value {
    let mut first = json!({ "capacityProvider": providers[0], "weight": row.weight });
    if let Some(base) = row.base {
        first["base"] = json!(base);
    }
    let mut entries = vec![first];
    if let Some(weight) = row.second_weight {
        entries.push(json!({ "capacityProvider": providers[1], "weight": weight }));
    }
    Value::Array(entries)
}

/// THE RULE, served half: the bootstrap's `CreateService`, `DeleteService` and `DeleteCluster`
/// are each answered HTTP 200 with the resource they acted on.
#[tokio::test]
async fn the_capacity_provider_bootstrap_serves_the_whole_service_and_cluster_lifecycle() {
    let router = router();
    given_launch_template(&router).await;
    given_revision(&router, "setup").await;

    for row in &ROWS {
        let case = row.case;
        let cluster_name = row.cluster();
        let service_name = row.service();

        // The providers the strategy names, each created over a zero-capacity group of the
        // account's — and named by whichever of the two forms the row spells.
        let mut providers: Vec<String> = Vec::new();
        let provider_arn =
            given_provider(&router, &row.provider(), &format!("asg-{}", row.slug), case).await;
        providers.push(if row.provider_by_arn {
            provider_arn
        } else {
            row.provider()
        });
        let mut associated = vec![row.provider()];
        if row.second_weight.is_some() {
            given_provider(
                &router,
                &row.second_provider(),
                &format!("asg-{}-two", row.slug),
                case,
            )
            .await;
            providers.push(row.second_provider());
            associated.push(row.second_provider());
        }
        let associated: Vec<&str> = associated.iter().map(String::as_str).collect();
        let cluster_arn = given_cluster(&router, &cluster_name, &associated, case).await;
        let named_cluster = if row.cluster_by_arn {
            cluster_arn.clone()
        } else {
            cluster_name.clone()
        };

        // CREATE: the step the recorded trajectory takes, answered with the service.
        let strategy = strategy_of(row, &providers);
        let created = ecs(
            &router,
            "CreateService",
            &json!({
                "cluster": named_cluster,
                "serviceName": service_name,
                "taskDefinition": FAMILY,
                "desiredCount": row.desired,
                "capacityProviderStrategy": strategy,
                "deploymentController": { "type": DEPLOYMENT_CONTROLLER_ECS },
            }),
            case,
        )
        .await;
        let service = &created["service"];
        assert_eq!(
            string(service, "serviceArn", case),
            format!(
                "arn:aws:ecs:{REGION}:{}:service/{cluster_name}/{service_name}",
                ::engine::emulator_account_id()
            ),
            "case {case:?}: expected the created service's own ARN, got {service}"
        );
        assert_eq!(
            (
                string(service, "serviceName", case),
                string(service, "clusterArn", case),
                string(service, "status", case),
                count(service, "desiredCount", case),
            ),
            (
                service_name.clone(),
                cluster_arn.clone(),
                SERVICE_ACTIVE.to_string(),
                row.desired
            ),
            "case {case:?}: expected the created service to name itself, its cluster, its \
             status and the count it was asked for, got {service}"
        );
        // The placement it was created with, read back off the answer: one entry per entry
        // the request carried, each naming the provider the request named it by, with the
        // two numeric members it asked for — `base` filled in with its modelled default
        // (`CapacityProviderStrategyItem$base`: "If no value is specified, the default value
        // of 0 is used") for the row that omitted it.
        let reported = list(service, "capacityProviderStrategy", case);
        let expected: Vec<Value> = strategy
            .as_array()
            .expect("the strategy is a list")
            .iter()
            .map(|entry| {
                json!({
                    "capacityProvider": entry["capacityProvider"],
                    "weight": entry["weight"],
                    "base": entry["base"].as_i64().unwrap_or(0),
                })
            })
            .collect();
        let normalized: Vec<Value> = reported
            .iter()
            .map(|entry| {
                json!({
                    "capacityProvider": entry["capacityProvider"],
                    "weight": count(entry, "weight", case),
                    "base": count(entry, "base", case),
                })
            })
            .collect();
        assert_eq!(
            normalized, expected,
            "case {case:?}: expected the strategy the service was placed by, got {service}"
        );
        assert!(
            service["launchType"].is_null(),
            "case {case:?}: a service placed by a strategy carries no launch type, got {service}"
        );
        assert_eq!(
            string(&service["deploymentController"], "type", case),
            DEPLOYMENT_CONTROLLER_ECS,
            "case {case:?}: expected the rolling-update controller, got {service}"
        );
        assert!(
            service["deploymentConfiguration"].is_object(),
            "case {case:?}: expected the deployment configuration the service rolls out \
             under, got {service}"
        );
        let deployment = list(service, "deployments", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {service}"));
        assert_eq!(
            string(&deployment, "status", case),
            DEPLOYMENT_PRIMARY,
            "case {case:?}: the first deployment reported is the current one, got {deployment}"
        );
        let revision = list(service, "currentServiceRevisions", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a current service revision, got {service}")
            });
        assert!(
            !string(&revision, "arn", case).is_empty(),
            "case {case:?}: expected the revision this rollout releases, got {revision}"
        );

        // ROUND TRIP: the created service is the one the account holds.
        let described = ecs(
            &router,
            "DescribeServices",
            &json!({ "cluster": named_cluster, "services": [service_name] }),
            case,
        )
        .await;
        let read = list(&described, "services", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {described}"));
        assert_eq!(
            (
                string(&read, "serviceArn", case),
                string(&read, "status", case),
                read["capacityProviderStrategy"].clone(),
            ),
            (
                string(service, "serviceArn", case),
                SERVICE_ACTIVE.to_string(),
                service["capacityProviderStrategy"].clone(),
            ),
            "case {case:?}: expected the read to agree with the create, got {read}"
        );

        // And the rollout the create started leaves `IN_PROGRESS` for a TERMINAL state, so a
        // client polling this read for one terminates: a service whose desired count the
        // scheduler can hold reaches the steady state it was rolling out, which is the
        // `COMPLETED` `Deployment$rolloutState`'s own contract names. The row that wants a
        // task the zero-capacity group cannot place has no steady state to reach, so it is
        // not waited on — and the breaker that would end such a rollout instead is pinned by
        // the owning crate's own suite, which can make a launch fail.
        if row.desired == 0 {
            let deadline = Instant::now() + POLL_BUDGET;
            let settled = loop {
                let described = ecs(
                    &router,
                    "DescribeServices",
                    &json!({ "cluster": named_cluster, "services": [service_name] }),
                    case,
                )
                .await;
                let read = list(&described, "services", case)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected the service back, got {described}")
                    });
                let deployment = list(&read, "deployments", case)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a deployment, got {read}")
                    });
                let state = string(&deployment, "rolloutState", case);
                assert!(
                    ROLLOUT_STATES.contains(&state.as_str()),
                    "case {case:?}: expected one of {ROLLOUT_STATES:?}, got {deployment}"
                );
                if TERMINAL_ROLLOUT_STATES.contains(&state.as_str()) {
                    break (read, deployment, state);
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the rollout to leave IN_PROGRESS for one of \
                     {TERMINAL_ROLLOUT_STATES:?}, got {deployment}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            let (read, deployment, state) = settled;
            assert_eq!(
                state, "COMPLETED",
                "case {case:?}: a rollout whose count the scheduler holds reaches COMPLETED, \
                 got {deployment}"
            );
            assert_eq!(
                (
                    count(&deployment, "runningCount", case),
                    count(&read, "runningCount", case),
                ),
                (row.desired, row.desired),
                "case {case:?}: expected the counts converged on the desired count, got {read}"
            );
        }

        // The teardown the trajectory takes: scaled to zero, then deleted.
        if row.desired != 0 {
            let updated = ecs(
                &router,
                "UpdateService",
                &json!({
                    "cluster": named_cluster,
                    "service": service_name,
                    "desiredCount": 0,
                }),
                case,
            )
            .await;
            assert_eq!(
                count(&updated["service"], "desiredCount", case),
                0,
                "case {case:?}: expected the update to take the count to zero, got {updated}"
            );
        }
        let deleted = ecs(
            &router,
            "DeleteService",
            &json!({ "cluster": named_cluster, "service": service_name }),
            case,
        )
        .await;
        let gone = &deleted["service"];
        let status = string(gone, "status", case);
        assert!(
            DELETED_SERVICE_STATUSES.contains(&status.as_str()),
            "case {case:?}: expected one of {DELETED_SERVICE_STATUSES:?}, got {gone}"
        );
        assert_eq!(
            string(gone, "serviceArn", case),
            string(service, "serviceArn", case),
            "case {case:?}: expected the delete to answer with the service it removed, got \
             {gone}"
        );
        // "Services in the DRAINING or INACTIVE status can still be viewed with the
        // DescribeServices API operation" — which is what tells a deleted service apart from
        // one that never existed.
        let described = ecs(
            &router,
            "DescribeServices",
            &json!({ "cluster": named_cluster, "services": [service_name] }),
            case,
        )
        .await;
        let read = list(&described, "services", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the deleted service back, got {described}")
            });
        let status = string(&read, "status", case);
        assert!(
            DELETED_SERVICE_STATUSES.contains(&status.as_str()),
            "case {case:?}: expected the deleted service to stay viewable, got {read}"
        );

        // DELETE THE CLUSTER: its capacity provider is still associated with it, and an
        // account-scoped association is no precondition of this delete.
        attachments_settled(&router, &cluster_name, case).await;
        let deleted = ecs(
            &router,
            "DeleteCluster",
            &json!({ "cluster": named_cluster }),
            case,
        )
        .await;
        let tombstone = &deleted["cluster"];
        assert_eq!(
            (
                string(tombstone, "clusterArn", case),
                string(tombstone, "clusterName", case),
                string(tombstone, "status", case),
            ),
            (
                cluster_arn.clone(),
                cluster_name.clone(),
                CLUSTER_INACTIVE.to_string()
            ),
            "case {case:?}: expected the deleted cluster at INACTIVE, got {tombstone}"
        );
        // "Clusters with an INACTIVE status may remain discoverable in your account for a
        // period of time", and the delete is `@idempotent`: asking again answers the same
        // tombstone.
        let described = ecs(
            &router,
            "DescribeClusters",
            &json!({ "clusters": [cluster_name] }),
            case,
        )
        .await;
        let read = list(&described, "clusters", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the deleted cluster back, got {described}")
            });
        assert_eq!(
            string(&read, "status", case),
            CLUSTER_INACTIVE,
            "case {case:?}: expected the tombstone the delete left, got {read}"
        );
        let again = ecs(
            &router,
            "DeleteCluster",
            &json!({ "cluster": cluster_name }),
            case,
        )
        .await;
        assert_eq!(
            string(&again["cluster"], "status", case),
            CLUSTER_INACTIVE,
            "case {case:?}: an idempotent delete answers the same tombstone, got {again}"
        );
    }
}

/// THE RULE, refused half: each modelled condition is answered with its own `__type` and the
/// sentence Amazon ECS words for it, and nothing else is refused.
#[tokio::test]
async fn four_hundred_is_reserved_for_the_conditions_the_operations_model() {
    let router = router();
    given_launch_template(&router).await;
    given_revision(&router, "setup").await;

    let case = "setup";
    let cluster = "cplifecycle-refusals";
    let provider = "cplifecycleprovider-refusals";
    // A provider of the account that the cluster is NOT associated with, for the row about a
    // strategy naming one the cluster does not hold.
    let unassociated = "cplifecycleprovider-unassociated";
    given_provider(&router, provider, "asg-refusals", case).await;
    given_provider(&router, unassociated, "asg-unassociated", case).await;
    let cluster_arn = given_cluster(&router, cluster, &[provider], case).await;
    let strategy = json!([{ "capacityProvider": provider, "weight": 1, "base": 0 }]);

    // One service of that cluster, scaled above zero, so the delete rows have something to
    // refuse: `CreateService` itself is served, which is the served half's rule.
    let service = "cplifecycleservice-refusals";
    ecs(
        &router,
        "CreateService",
        &json!({
            "cluster": cluster,
            "serviceName": service,
            "taskDefinition": FAMILY,
            "desiredCount": 1,
            "capacityProviderStrategy": strategy,
        }),
        case,
    )
    .await;

    // (case, operation, request, the error code, the message — `None` where the operation
    // words this condition with no `message` member at all)
    let rows: Vec<(&str, &str, Value, &str, Option<String>)> = vec![
        (
            "a create naming a cluster the account does not hold",
            "CreateService",
            json!({
                "cluster": "cplifecycle-nosuchcluster",
                "serviceName": "orphan",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "capacityProviderStrategy": strategy,
            }),
            CLUSTER_NOT_FOUND,
            Some(CLUSTER_NOT_FOUND_MESSAGE.to_string()),
        ),
        (
            "a create saying where its tasks run twice",
            "CreateService",
            json!({
                "cluster": cluster,
                "serviceName": "contradictory",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "launchType": "EC2",
                "capacityProviderStrategy": strategy,
            }),
            INVALID_PARAMETER,
            Some(LAUNCH_TYPE_AND_STRATEGY_MESSAGE.to_string()),
        ),
        (
            "a create whose strategy names a provider the cluster is not associated with",
            "CreateService",
            json!({
                "cluster": cluster,
                "serviceName": "unassociated",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "capacityProviderStrategy": [
                    { "capacityProvider": unassociated, "weight": 1, "base": 0 },
                ],
            }),
            INVALID_PARAMETER,
            Some(PROVIDER_NOT_ASSOCIATED_MESSAGE.to_string()),
        ),
        (
            "a create whose strategy names a provider the account holds none of",
            "CreateService",
            json!({
                "cluster": cluster,
                "serviceName": "absentprovider",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "capacityProviderStrategy": [
                    { "capacityProvider": "cplifecycleprovider-absent", "weight": 1 },
                ],
            }),
            INVALID_PARAMETER,
            Some(
                "The specified capacity provider cplifecycleprovider-absent was not found. \
                 Specify a valid capacity provider and try again."
                    .to_string(),
            ),
        ),
        (
            "a create one past the top of the base range",
            "CreateService",
            json!({
                "cluster": cluster,
                "serviceName": "overbase",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "capacityProviderStrategy": [
                    { "capacityProvider": provider, "weight": 1, "base": MAX_BASE + 1 },
                ],
            }),
            INVALID_PARAMETER,
            Some(format!(
                "The specified base value '{}' is invalid. Specify an integer between 0 and \
                 {MAX_BASE} and try again.",
                MAX_BASE + 1
            )),
        ),
        (
            "a create one past the top of the weight range",
            "CreateService",
            json!({
                "cluster": cluster,
                "serviceName": "overweight",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "capacityProviderStrategy": [
                    { "capacityProvider": provider, "weight": MAX_WEIGHT + 1, "base": 0 },
                ],
            }),
            INVALID_PARAMETER,
            Some(format!(
                "The specified weight value '{}' is invalid. Specify an integer between 0 \
                 and {MAX_WEIGHT} and try again.",
                MAX_WEIGHT + 1
            )),
        ),
        (
            "a delete of a service identifier that resolves to nothing",
            "DeleteService",
            json!({ "cluster": cluster, "service": "cplifecycleservice-absent" }),
            SERVICE_NOT_FOUND,
            // RECORDED: a `DeleteService` whose identifier resolves to nothing carries the
            // class and no sentence, where `UpdateService` of the same name states one. The
            // class is what a client branches on to treat the service as already gone.
            None,
        ),
        (
            "an update of the same identifier, which words the condition with its own sentence",
            "UpdateService",
            json!({
                "cluster": cluster,
                "service": "cplifecycleservice-absent",
                "desiredCount": 0,
            }),
            SERVICE_NOT_FOUND,
            Some(SERVICE_NOT_FOUND_MESSAGE.to_string()),
        ),
        (
            "a delete of a service that is still scaled above zero",
            "DeleteService",
            json!({ "cluster": cluster, "service": service }),
            INVALID_PARAMETER,
            Some(SERVICE_SCALED_ABOVE_ZERO_MESSAGE.to_string()),
        ),
        (
            "a delete of a cluster that still holds an active service",
            "DeleteCluster",
            json!({ "cluster": cluster }),
            CLUSTER_CONTAINS_SERVICES,
            Some(CLUSTER_CONTAINS_SERVICES_MESSAGE.to_string()),
        ),
        (
            "a delete of a cluster the account does not hold",
            "DeleteCluster",
            json!({ "cluster": "cplifecycle-nosuchcluster" }),
            CLUSTER_NOT_FOUND,
            Some(CLUSTER_NOT_FOUND_MESSAGE.to_string()),
        ),
        (
            "a delete of a service named by an ARN of a cluster the account does not hold",
            "DeleteService",
            json!({
                "service": format!(
                    "arn:aws:ecs:{REGION}:{}:service/cplifecycle-nosuchcluster/{service}",
                    ::engine::emulator_account_id()
                ),
            }),
            CLUSTER_NOT_FOUND,
            Some(CLUSTER_NOT_FOUND_MESSAGE.to_string()),
        ),
    ];

    for (case, operation, request, code, message) in rows {
        attachments_settled(&router, cluster, case).await;
        let (answered, reported) = ecs_refusal(&router, operation, &request, case).await;
        assert_eq!(
            (answered.as_str(), reported.clone()),
            (code, message),
            "case {case:?}: expected the modelled refusal class and the sentence it words \
             the condition with, got {answered} / {reported:?}"
        );
    }

    // And once the preconditions are cleared the same two deletes are served: the refusals
    // above are the conditions, not the operations.
    let case = "the cleared preconditions are served";
    ecs(
        &router,
        "UpdateService",
        &json!({ "cluster": cluster, "service": service, "desiredCount": 0 }),
        case,
    )
    .await;
    let deleted = ecs(
        &router,
        "DeleteService",
        &json!({ "cluster": cluster, "service": service }),
        case,
    )
    .await;
    let status = string(&deleted["service"], "status", case);
    assert!(
        DELETED_SERVICE_STATUSES.contains(&status.as_str()),
        "case {case:?}: expected the scaled-down service to be deleted, got {deleted}"
    );
    attachments_settled(&router, cluster, case).await;
    let deleted = ecs(
        &router,
        "DeleteCluster",
        &json!({ "cluster": cluster_arn }),
        case,
    )
    .await;
    assert_eq!(
        string(&deleted["cluster"], "status", case),
        CLUSTER_INACTIVE,
        "case {case:?}: expected the emptied cluster to be deleted, got {deleted}"
    );
}
