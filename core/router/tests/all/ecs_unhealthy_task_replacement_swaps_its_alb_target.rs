// SPDX-License-Identifier: CC0-1.0

//! An Application Load Balancer in front of an `awsvpc` Fargate service whose essential
//! container declares a HEALTH CHECK: the task that check condemns is replaced, and the target
//! group follows the replacement.
//!
//! RULE: a service of an `awsvpc`, Fargate placement whose `loadBalancers` bind an `ip` target
//! group brings its `desiredCount` tasks to `lastStatus` `RUNNING`, each at the
//! `privateIPv4Address` of the interface it was allocated and registered in that group on the
//! bound `containerPort`; the `healthCheck` its container definition declares is EXECUTED
//! inside that container, and `DescribeTasks` reports the result as the container's
//! `healthStatus` — `HEALTHY` while the command "exits 0", `UNHEALTHY` once it has failed
//! `retries` times (`HealthCheck$command`: "An exit code of 0 indicates success, and non-zero
//! exit code indicates failure"; `HealthCheck$retries`: "the number of times to retry a failed
//! health check before the container is considered unhealthy"). A task whose ESSENTIAL
//! container reports `UNHEALTHY` is one the service scheduler stops and replaces — a service
//! "replaces tasks that fail or stop" (`CreateService`), and
//! `CreateService$healthCheckGracePeriodSeconds` is "the period of time, in seconds, that the
//! Amazon ECS service scheduler ignores unhealthy Elastic Load Balancing, VPC Lattice, and
//! container health checks after a task has first started", so the stop is held off for that
//! period and then made — and the target group holds the registrations of the LIVE tasks
//! alone: the stopped task's address leaves it and the replacement's address is registered and
//! health-checked in its place, so the published address keeps answering. A failing check on a
//! container the task can live without is `Task$healthStatus`'s own distinction ("determined by
//! the health of the essential containers in the task") and stops nothing.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here: Amazon EC2
//! owns the network and the interfaces the tasks hold, Amazon ECS owns the tasks, the probe and
//! the scheduler that replaces one, and Elastic Load Balancing v2 owns the registrations and
//! the health it reports. Everything is driven through the real operations over ONE shared
//! engine, so what is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: whether the probe passes or fails, HOW
//! it fails (a request for a path the container serves nothing at, a command that simply exits
//! non-zero), how many `retries` the verdict takes, whether a `startPeriod` is declared, how
//! many tasks the service maintains, how long a grace period the scheduler was asked to ignore
//! the checks for, and whether the condemned container is essential at all. The recorded
//! configuration — one task, a probe its own server answers, a 10 second grace period — is a
//! single row.
//!
//! Sibling suites pin the neighbouring facts this one builds on rather than repeating them:
//! `ecs_container_probe_reaches_its_own_server.rs` pins WHICH server answers a probe,
//! `ecs_task_registration_precedes_its_health_verdict.rs` what a RUNNING task's registration
//! owes, and `ecs_scale_in_drains_its_registered_target.rs` the DRAIN a deregistration goes
//! through — which is why the groups here are configured to drain for no time, so what is
//! observed is the swap itself.
//!
//! Every clock-owed move — the tasks' starts, the registrations they owe, the verdicts the
//! probes and the load balancer's checks reach, the stop the scheduler owes at the end of the
//! grace period and the replacement that follows — is POLLED for until it holds, never slept
//! for, and each deadline is generous: a row asserts that the move HAPPENS, not how fast this
//! host runs.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, members_of, opt_element, pairs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The ECS cluster every service below runs in, the essential container every binding names,
/// and the container a task can live without.
const CLUSTER: &str = "replaced";
const CONTAINER: &str = "app";
const SIDECAR: &str = "watcher";

/// The port the containers' servers listen on, which is the `containerPort` the revisions
/// declare, the port every binding names and the target groups' own port.
const CONTAINER_PORT: i64 = 80;

/// The image every container below runs, the document root its own configuration serves, and
/// the marker each container's program writes into that root — so a body anything but that
/// program produced fails the row.
const IMAGE: &str = "public.ecr.aws/docker/library/nginx:alpine";
const DOCUMENT_ROOT: &str = "/usr/share/nginx/html";
const MARKER: &str = "REPLACED-CONTAINER-BODY";

/// The network every task below is placed in.
const VPC_CIDR: &str = "10.93.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.93.1.0/24", "us-east-1a"),
    ("10.93.2.0/24", "us-east-1b"),
];

/// The `HealthCheck$interval` and `HealthCheck$timeout` every probe below declares: the
/// smallest each member documents ("You may specify between 5 and 300 seconds" and "You may
/// specify between 2 and 60 seconds"), so a verdict lands inside a budget without naming a
/// value AWS refuses.
const PROBE_INTERVAL_SECONDS: i64 = 5;
const PROBE_TIMEOUT_SECONDS: i64 = 2;

/// The `HealthCheckIntervalSeconds` and `HealthyThresholdCount` every target group below is
/// created with: the shortest cadence the member admits ("the range is 5-300") and the fewest
/// consecutive successes a verdict takes ("the range is 2-10"), so a row waits the least its
/// configuration allows.
const HEALTH_CHECK_INTERVAL_SECONDS: &str = "5";
const HEALTHY_THRESHOLD_COUNT: &str = "2";

/// The target-group attribute every group below configures, and the value it is configured to:
/// "the amount of time, in seconds, for Elastic Load Balancing to wait before changing the
/// state of a deregistering target from `draining` to `unused`. The range is 0-3600 seconds.
/// The default value is 300 seconds."
/// (`smithy_query(query="doc",shape="TargetGroupAttribute",member="Key")`). The bottom of that
/// range is what makes the SWAP this suite is about observable on its own: the drain itself is
/// the sibling suite's rule.
const DELAY_ATTRIBUTE: &str = "deregistration_delay.timeout_seconds";
const NO_DELAY: &str = "0";

/// The `HealthStatus` values `Container$healthStatus` reports ("`UNKNOWN` — The container
/// health check is being evaluated, there's no container health check defined, or Amazon ECS
/// doesn't have the health status of the container").
const HEALTHY: &str = "HEALTHY";
const UNHEALTHY: &str = "UNHEALTHY";

/// The `TargetHealthStateEnum` value of a registration the load balancer may route to.
const TARGET_HEALTHY: &str = "healthy";

/// The `Task$desiredStatus` values `ListTasks` filters on.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a stop the service scheduler made, and the `Task$stoppedReason` Amazon
/// ECS words a stop for a failed container health check in.
const SERVICE_SCHEDULER_INITIATED: &str = "ServiceSchedulerInitiated";
const HEALTH_CHECK_FAILED_REASON: &str = "Task failed container health checks";

/// The status a served document is answered with.
const OK: u16 = 200;

/// How long a clock-owed move is waited for before a row gives up, how often it looks again,
/// and how many times an invariant is looked at. Generous on purpose: the deadline is here so a
/// failure is a failure rather than a hang, and every wait stops the instant its own condition
/// holds.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(200);
const LOOKS_FOR_AN_INVARIANT: usize = 5;

/// One row: the service it stands up, the probe its containers declare, and what the scheduler
/// and the target group then owe.
struct Row {
    case: &'static str,
    /// The names this row's own resources take, so no two rows collide in the services that
    /// hold them.
    family: &'static str,
    service_name: &'static str,
    group_name: &'static str,
    load_balancer_name: &'static str,
    /// The essential container's `healthCheck` command, as a `CMD-SHELL` program.
    probe: &'static str,
    /// Its `HealthCheck$retries`, and its `HealthCheck$startPeriod` for a row that declares
    /// one.
    retries: i64,
    start_period: Option<i64>,
    /// The probe a NON-essential container of the same task declares, for a row that runs one.
    sidecar_probe: Option<&'static str>,
    /// `CreateService$desiredCount`: how many tasks the service maintains, which is how many
    /// registrations its group holds.
    desired: usize,
    /// `CreateService$healthCheckGracePeriodSeconds`.
    grace_seconds: i64,
    /// Whether the task the service placed must be stopped and replaced.
    replaced: bool,
    /// Whether the row waits for the REPLACEMENT's registration to be reported healthy and for
    /// the published address to answer again: only a row whose grace period leaves the
    /// replacement standing for longer than a verdict takes can observe that.
    awaits_healthy_replacement: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded configuration: one task whose probe its own server answers",
        family: "replacedserving",
        service_name: "replacedserving",
        group_name: "replaced-serving",
        load_balancer_name: "replaced-serving-lb",
        probe: "curl -f http://localhost/ || exit 1",
        retries: 2,
        // "The optional grace period to provide containers time to bootstrap before failed
        // health checks count towards the maximum number of retries" — the probe of this row
        // passes inside it, and "if a health check succeeds within the `startPeriod`, then the
        // container is considered healthy".
        start_period: Some(30),
        sidecar_probe: None,
        desired: 1,
        grace_seconds: 10,
        replaced: false,
        awaits_healthy_replacement: false,
    },
    Row {
        case: "two tasks, each answering its own probe",
        family: "replacedpair",
        service_name: "replacedpair",
        group_name: "replaced-pair",
        load_balancer_name: "replaced-pair-lb",
        probe: "curl -f http://localhost/ || exit 1",
        retries: 1,
        start_period: None,
        sidecar_probe: None,
        desired: 2,
        grace_seconds: 10,
        replaced: false,
        awaits_healthy_replacement: false,
    },
    Row {
        case: "a probe asking for a path the container serves nothing at",
        family: "replacedmissing",
        service_name: "replacedmissing",
        group_name: "replaced-missing",
        load_balancer_name: "replaced-missing-lb",
        probe: "curl -f http://localhost/nothing-here || exit 1",
        retries: 1,
        start_period: None,
        sidecar_probe: None,
        desired: 1,
        grace_seconds: 10,
        replaced: true,
        awaits_healthy_replacement: false,
    },
    Row {
        case: "a probe that exits non-zero, under a longer grace period and two retries",
        family: "replacedfailing",
        service_name: "replacedfailing",
        group_name: "replaced-failing",
        load_balancer_name: "replaced-failing-lb",
        probe: "exit 1",
        retries: 2,
        start_period: None,
        sidecar_probe: None,
        desired: 1,
        grace_seconds: 30,
        replaced: true,
        awaits_healthy_replacement: true,
    },
    Row {
        case: "a failing probe on a container the task can live without",
        family: "replacedsidecar",
        service_name: "replacedsidecar",
        group_name: "replaced-sidecar",
        load_balancer_name: "replaced-sidecar-lb",
        probe: "curl -f http://localhost/ || exit 1",
        retries: 1,
        start_period: None,
        sidecar_probe: Some("exit 1"),
        desired: 1,
        grace_seconds: 10,
        replaced: false,
        awaits_healthy_replacement: false,
    },
];

/// One emulator carrying Amazon EC2, Elastic Load Balancing v2 and Amazon ECS over ONE shared
/// engine, exactly as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
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

/// Send one form-encoded request to the service `signing_name` addresses, and answer its status
/// and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Drive one ec2Query action that must succeed, and answer its body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, OK,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one awsQuery action against the Elastic Load Balancing v2 model that must succeed, and
/// answer its body.
async fn elbv2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, OK,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One awsJson1_1 call to Amazon ECS that must succeed, answered with its parsed body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{REGION}\
                 /ecs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
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
        status, OK,
        "setup: ECS {operation} answered {status} with {parsed}"
    );
    parsed
}

/// The network every task below runs in: a VPC with two subnets, the internet gateway and route
/// an image pull takes, the load balancers' own security group and the group the tasks are
/// placed in.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    load_balancer_group: String,
    task_group: String,
}

/// Create that network through Amazon EC2 itself, so every id used below is one that service
/// minted.
async fn seed_network(router: &MultiServiceRouter) -> Network {
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
    let mut subnets = Vec::new();
    for (cidr, zone) in SUBNET_CIDRS {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    // The path off the VPC a task needs to fetch the image its container runs: Amazon ECS reads
    // the route table that applies to the placement's subnet before it starts a task.
    let route_table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", route_table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    for subnet in &subnets {
        ec2(
            router,
            "AssociateRouteTable",
            &[
                ("RouteTableId", route_table.as_str()),
                ("SubnetId", subnet.as_str()),
            ],
        )
        .await;
    }
    let load_balancer_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "replaced-alb"),
                ("GroupDescription", "the load balancers own group"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    let task_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "replaced-tasks"),
                ("GroupDescription", "the group the tasks are placed in"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // The ONE rule this suite authorizes: the tasks' group takes the load balancers' own group
    // as a source on the port the containers serve on, which is what lets a check of a target
    // in it arrive.
    ec2(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", task_group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", &CONTAINER_PORT.to_string()),
            ("IpPermissions.1.ToPort", &CONTAINER_PORT.to_string()),
            (
                "IpPermissions.1.Groups.1.GroupId",
                load_balancer_group.as_str(),
            ),
        ],
    )
    .await;
    Network {
        vpc,
        subnets,
        load_balancer_group,
        task_group,
    }
}

/// The `healthCheck` one container declares: `probe` as a `CMD-SHELL` program, at the cadence
/// and the timeout every row shares, with the retries and the optional start period the row
/// names.
fn health_check(probe: &str, retries: i64, start_period: Option<i64>) -> Value {
    let mut check = json!({
        "command": ["CMD-SHELL", probe],
        "interval": PROBE_INTERVAL_SECONDS,
        "timeout": PROBE_TIMEOUT_SECONDS,
        "retries": retries,
    });
    if let Some(seconds) = start_period {
        check["startPeriod"] = json!(seconds);
    }
    check
}

/// Register one `awsvpc`, Fargate-compatible revision for `row`: an essential container whose
/// program writes the document its server serves and then runs that server, plus — for a row
/// that asks for one — a container the task can live without, running nothing and declaring a
/// probe of its own.
async fn register_revision(router: &MultiServiceRouter, row: &Row) {
    let mut containers = vec![json!({
        "name": CONTAINER,
        "image": IMAGE,
        "essential": true,
        "memory": 128,
        "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
        "command": [
            "/bin/sh",
            "-c",
            format!("echo {MARKER} > {DOCUMENT_ROOT}/index.html && nginx -g 'daemon off;'"),
        ],
        "healthCheck": health_check(row.probe, row.retries, row.start_period),
    })];
    if let Some(probe) = row.sidecar_probe {
        containers.push(json!({
            "name": SIDECAR,
            "image": IMAGE,
            "essential": false,
            "memory": 128,
            "command": ["/bin/sh", "-c", "sleep 600"],
            "healthCheck": health_check(probe, 1, None),
        }));
    }
    ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": containers,
        }),
    )
    .await;
}

/// Create one row's `ip` target group, configure it to drain for no time, assert it reports that
/// configuration, and put an internet-facing load balancer with a forwarding listener in front
/// of it. Answers the group's ARN and the address the load balancer published.
async fn front_door(router: &MultiServiceRouter, network: &Network, row: &Row) -> (String, String) {
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", row.group_name),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/"),
            ("HealthCheckIntervalSeconds", HEALTH_CHECK_INTERVAL_SECONDS),
            ("HealthyThresholdCount", HEALTHY_THRESHOLD_COUNT),
        ],
    )
    .await;
    let group = opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("case {:?}: no TargetGroupArn in {created}", row.case));
    elbv2(
        router,
        "ModifyTargetGroupAttributes",
        &[
            ("TargetGroupArn", group.as_str()),
            ("Attributes.member.1.Key", DELAY_ATTRIBUTE),
            ("Attributes.member.1.Value", NO_DELAY),
        ],
    )
    .await;
    let described = elbv2(
        router,
        "DescribeTargetGroupAttributes",
        &[("TargetGroupArn", group.as_str())],
    )
    .await;
    let attributes = pairs_of(&described, "Key", "Value");
    assert_eq!(
        attributes.get(DELAY_ATTRIBUTE).map(String::as_str),
        Some(NO_DELAY),
        "case {:?}: expected the group to report a drain of {NO_DELAY} seconds, got \
         {attributes:?}",
        row.case
    );
    let balanced = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", row.load_balancer_name),
            ("Type", "application"),
            ("Scheme", "internet-facing"),
            ("Subnets.member.1", network.subnets[0].as_str()),
            ("Subnets.member.2", network.subnets[1].as_str()),
            (
                "SecurityGroups.member.1",
                network.load_balancer_group.as_str(),
            ),
        ],
    )
    .await;
    let load_balancer = element(&balanced, "LoadBalancerArn");
    let dns_name = element(&balanced, "DNSName");
    elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer.as_str()),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
        ],
    )
    .await;
    (group, dns_name)
}

/// Create one row's service: its tasks across both subnets, bound to `group` on the port the
/// container serves on, with the grace period and the desired count the row names.
async fn create_service(router: &MultiServiceRouter, network: &Network, row: &Row, group: &str) {
    let created = ecs_ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "taskDefinition": row.family,
            "desiredCount": row.desired,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnets[0], network.subnets[1]],
                    "securityGroups": [network.task_group],
                    "assignPublicIp": "ENABLED",
                }
            },
            "loadBalancers": [{
                "targetGroupArn": group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
            "healthCheckGracePeriodSeconds": row.grace_seconds,
            // The configuration the recorded deploy asked for, which is what lets the scheduler
            // stop an unhealthy task before its replacement is up.
            "deploymentConfiguration": {
                "minimumHealthyPercent": 0,
                "maximumPercent": 200,
            },
        }),
    )
    .await;
    assert_eq!(
        created["service"]["desiredCount"].as_u64(),
        u64::try_from(row.desired).ok(),
        "case {:?}: the stored service must carry the count the request asked for, got {created}",
        row.case
    );
}

/// The ARNs of one service's tasks whose `desiredStatus` is `status`.
async fn task_arns(router: &MultiServiceRouter, row: &Row, status: &str) -> Vec<String> {
    let listed = ecs_ok(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "desiredStatus": status,
        }),
    )
    .await;
    listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect()
}

/// The tasks `arns` names, as `DescribeTasks` reports them now.
async fn describe_tasks(router: &MultiServiceRouter, arns: &[String]) -> Vec<Value> {
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs_ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
    )
    .await;
    described["tasks"].as_array().cloned().unwrap_or_default()
}

/// The `privateIPv4Address` the attachments of `task` report, and `None` for a task holding no
/// interface yet.
fn private_address(task: &Value) -> Option<String> {
    task["attachments"]
        .as_array()?
        .iter()
        .flat_map(|attachment| {
            attachment["details"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
        .and_then(|detail| detail["value"].as_str().map(str::to_string))
}

/// The `healthStatus` the container `name` of `task` reports, and the empty string for a task
/// holding no such container.
fn container_health(task: &Value, name: &str) -> String {
    task["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|container| container["name"].as_str() == Some(name))
        .and_then(|container| container["healthStatus"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// `Task$lastStatus`.
fn last_status(task: &Value) -> String {
    task["lastStatus"].as_str().unwrap_or_default().to_string()
}

/// The `(Id, State)` of every registration `group` reports.
async fn registrations(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", group)]).await;
    members_of(&body)
        .into_iter()
        .map(|description| {
            let text = |tag: &str| {
                elements(description, tag)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            };
            (text("Id"), text("State"))
        })
        .collect()
}

/// The addresses `reported` holds registrations for.
fn registered_addresses(reported: &[(String, String)]) -> BTreeSet<String> {
    reported.iter().map(|(id, _)| id.clone()).collect()
}

/// One task of a service: the ARN it is known by and the address its interface holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    arn: String,
    address: String,
}

/// The RUNNING tasks of one row's service, each with its own address.
async fn running_tasks(router: &MultiServiceRouter, row: &Row) -> Vec<Placed> {
    let arns = task_arns(router, row, RUNNING).await;
    let mut placed = Vec::new();
    for task in describe_tasks(router, &arns).await {
        if last_status(&task) != RUNNING {
            continue;
        }
        let (Some(arn), Some(address)) = (task["taskArn"].as_str(), private_address(&task)) else {
            continue;
        };
        placed.push(Placed {
            arn: arn.to_string(),
            address,
        });
    }
    placed.sort_by(|left, right| left.address.cmp(&right.address));
    placed
}

/// Send one request to the address the load balancer published, carrying no AWS credential —
/// exactly as a client handed that name sends it.
async fn get(router: &MultiServiceRouter, dns_name: &str, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{dns_name}{path}"))
        .header("host", dns_name)
        .body(Bytes::new())
        .expect("a well-formed data-plane request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Wait until the service of `row` has its desired count of RUNNING tasks, each at its own
/// address, and `group` holds exactly those addresses — the gate every row below starts from.
async fn await_placement(router: &MultiServiceRouter, row: &Row, group: &str) -> Vec<Placed> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let placed = running_tasks(router, row).await;
        let reported = registrations(router, group).await;
        let addresses: BTreeSet<String> =
            placed.iter().map(|task| task.address.clone()).collect();
        let settled = placed.len() == row.desired
            && addresses.len() == row.desired
            && registered_addresses(&reported) == addresses;
        if settled {
            return placed;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected {} RUNNING task(s), each at its own address and registered in \
             its group, got {placed:?} and {reported:?}",
            row.case,
            row.desired
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Wait until every container of `row`'s tasks has reached the reading its own probe earns and
/// every registration of `group` is `healthy` — the convergence of the two health signals of a
/// serving, load-balanced task.
async fn await_convergence(router: &MultiServiceRouter, row: &Row, group: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let arns = task_arns(router, row, RUNNING).await;
        let tasks = describe_tasks(router, &arns).await;
        let reported = registrations(router, group).await;
        let essential: Vec<String> = tasks
            .iter()
            .map(|task| container_health(task, CONTAINER))
            .collect();
        let sidecars: Vec<String> = tasks
            .iter()
            .map(|task| container_health(task, SIDECAR))
            .collect();
        let converged = essential.len() == row.desired
            && essential.iter().all(|health| health == HEALTHY)
            && row
                .sidecar_probe
                .is_none_or(|_| sidecars.iter().all(|health| health == UNHEALTHY))
            && reported.len() == row.desired
            && reported.iter().all(|(_, state)| state == TARGET_HEALTHY);
        if converged {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected {} container(s) {HEALTHY} with every registration \
             {TARGET_HEALTHY}, got {essential:?}, the sidecars {sidecars:?} and {reported:?}",
            row.case,
            row.desired
        );
        ::tokio::time::sleep(POLL).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_its_container_health_condemns_is_replaced_and_its_alb_target_swapped() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // Every row is stood up FIRST and waited for afterwards: the starts, the probes and the
    // stops they earn are clock-owed, so the rows converge beside each other rather than one
    // budget after another.
    let mut fixtures: Vec<(&Row, String, String)> = Vec::new();
    for row in ROWS {
        register_revision(&router, row).await;
        let (group, dns_name) = front_door(&router, &network, row).await;
        create_service(&router, &network, row, &group).await;
        fixtures.push((row, group, dns_name));
    }

    // 1. The gate the whole chain starts from: each service reaches its desired count of
    //    RUNNING tasks, each at the address of the interface its `awsvpc` placement allocated,
    //    and each registered in the group its binding names.
    let mut placed: Vec<Vec<Placed>> = Vec::new();
    for (row, group, _) in &fixtures {
        placed.push(await_placement(&router, row, group).await);
    }

    // 2. The rows whose essential container answers its own probe: the container reports
    //    HEALTHY, the registration reports healthy, and the published address answers with the
    //    container's own document.
    for ((row, group, dns_name), tasks) in fixtures.iter().zip(&placed) {
        if row.replaced {
            continue;
        }
        await_convergence(&router, row, group).await;
        let (status, body) = get(&router, dns_name, "/").await;
        assert_eq!(
            status, OK,
            "case {:?}: expected the published address to answer {OK}, got {status} with \
             {body:?}",
            row.case
        );
        assert!(
            body.contains(MARKER),
            "case {:?}: expected the container's own document, got {body:?}",
            row.case
        );
        assert_eq!(
            tasks.len(),
            row.desired,
            "case {:?}: expected {} placed task(s), got {tasks:?}",
            row.case,
            row.desired
        );
    }

    // 3. The rows whose essential container fails its probe: that task is stopped — the
    //    scheduler's own stop, worded as Amazon ECS words a failed health check — a replacement
    //    is placed, and the group holds the live tasks' addresses alone, so the stopped task's
    //    address has left it.
    for ((row, group, dns_name), tasks) in fixtures.iter().zip(&placed) {
        if !row.replaced {
            continue;
        }
        let case = row.case;
        let condemned = tasks
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the placement above answers one task"));
        let deadline = Instant::now() + DEADLINE;
        let mut readings: BTreeSet<String> = BTreeSet::new();
        let replacement = loop {
            let stopped = task_arns(&router, row, STOPPED).await;
            let live = running_tasks(&router, row).await;
            let reported = registrations(&router, group).await;
            let addresses: BTreeSet<String> =
                live.iter().map(|task| task.address.clone()).collect();
            for task in describe_tasks(&router, ::std::slice::from_ref(&condemned.arn)).await {
                readings.insert(container_health(&task, CONTAINER));
            }
            let swapped = stopped.contains(&condemned.arn)
                && !live.iter().any(|task| task.arn == condemned.arn)
                && live.len() == row.desired
                && registered_addresses(&reported) == addresses
                && !addresses.contains(&condemned.address);
            if swapped {
                break live;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the condemned task {condemned:?} stopped, replaced, \
                 and its address out of the group, got stopped {stopped:?}, running {live:?} \
                 and {reported:?}; the readings seen were {readings:?}"
            );
            ::tokio::time::sleep(POLL).await;
        };
        // The reading that condemned it is the one the probe produced, reported by
        // `DescribeTasks` itself.
        assert!(
            readings.contains(UNHEALTHY),
            "case {case:?}: expected the condemned container to have reported {UNHEALTHY}, got \
             the readings {readings:?}"
        );
        let described = describe_tasks(&router, ::std::slice::from_ref(&condemned.arn)).await;
        let stopped_task = described
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the stopped task is still described"));
        assert_eq!(
            stopped_task["stopCode"].as_str(),
            Some(SERVICE_SCHEDULER_INITIATED),
            "case {case:?}: expected the stop to be the service scheduler's, got {stopped_task}"
        );
        assert_eq!(
            stopped_task["stoppedReason"].as_str(),
            Some(HEALTH_CHECK_FAILED_REASON),
            "case {case:?}: expected the reason Amazon ECS words a failed health check in, got \
             {stopped_task}"
        );

        if !row.awaits_healthy_replacement {
            continue;
        }
        // And the replacement is a target the load balancer routes to in its turn: its own
        // address reaches `healthy` and the published address answers with the container's
        // document again.
        let deadline = Instant::now() + DEADLINE;
        loop {
            let reported = registrations(&router, group).await;
            let healthy: BTreeSet<String> = reported
                .iter()
                .filter(|(_, state)| state == TARGET_HEALTHY)
                .map(|(id, _)| id.clone())
                .collect();
            if !healthy.is_empty() && !healthy.contains(&condemned.address) {
                let (status, body) = get(&router, dns_name, "/").await;
                if status == OK && body.contains(MARKER) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the published address to answer {OK} with the \
                     replacement's document, got {status} with {body:?}"
                );
            } else {
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the replacement {replacement:?} reported \
                     {TARGET_HEALTHY}, got {reported:?}"
                );
            }
            ::tokio::time::sleep(POLL).await;
        }
    }

    // 4. The rows where no ESSENTIAL container failed its check keep the very tasks they
    //    placed. Read AFTER every replacement above, so each of these services has had far
    //    longer than its own grace period — and than a replacement takes — to lose one.
    for ((row, group, _), tasks) in fixtures.iter().zip(&placed) {
        if row.replaced {
            continue;
        }
        for look in 0..LOOKS_FOR_AN_INVARIANT {
            let live = running_tasks(&router, row).await;
            let stopped = task_arns(&router, row, STOPPED).await;
            let reported = registrations(&router, group).await;
            assert_eq!(
                &live, tasks,
                "case {:?}: expected the tasks the service placed to keep running (look \
                 {look}), got {live:?}",
                row.case
            );
            assert!(
                stopped.is_empty(),
                "case {:?}: expected no task of this service to be stopped (look {look}), got \
                 {stopped:?}",
                row.case
            );
            assert_eq!(
                registered_addresses(&reported),
                tasks
                    .iter()
                    .map(|task| task.address.clone())
                    .collect::<BTreeSet<String>>(),
                "case {:?}: expected the group to hold those tasks' addresses (look {look}), \
                 got {reported:?}",
                row.case
            );
            ::tokio::time::sleep(POLL).await;
        }
    }
}
