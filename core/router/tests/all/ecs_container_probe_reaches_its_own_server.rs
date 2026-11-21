// SPDX-License-Identifier: CC0-1.0

//! A container's HEALTH CHECK is a request to the server that same container is RUNNING, and
//! the health it reaches converges with the health the load balancer's own check of that task
//! reports.
//!
//! RULE: `HealthCheck$command` is "the command that the container runs to determine if it is
//! healthy ... An exit code of 0 indicates success", and Amazon ECS runs it INSIDE the
//! container; the command such a check is written with is an HTTP request to the container's own
//! server, as the member's own example `[ "CMD-SHELL", "curl -f http://localhost/ || exit 1" ]`
//! is. A task that uses the `awsvpc` network mode "is allocated its own elastic network
//! interface" and its containers "can communicate over the `localhost` interface", so that
//! request reaches the servers of its OWN task — the loopback inside it and the address of its
//! interface are the same servers — and nothing else: a request to any other host is one such a
//! container has no network for. So a container serving the path its probe asks for reports
//! `healthStatus` `HEALTHY`, and one whose probe asks for a path, a port or a host nothing of its
//! task answers reports `UNHEALTHY` once that check has failed `retries` times.
//!
//! The second rule, which is what makes the first observable from outside ECS: the two health
//! signals of one load-balanced task are computed independently and CONVERGE for a task that is
//! serving — `DescribeTasks` reports `containers[].healthStatus` `HEALTHY` from the container's
//! own probe, while `DescribeTargetHealth` reports the registration of that task's address
//! `healthy` from the load balancer's own checks of it — and a target group no service's
//! `loadBalancers` names stays empty throughout.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here: Amazon EC2
//! owns the network the task is placed in, Amazon ECS owns the task, its interface's address and
//! the container's probe, and Elastic Load Balancing v2 holds the registration and reports its
//! health. Everything is driven through the real operations over ONE shared engine, so what is
//! asserted is what each service persisted.
//!
//! The rows of the first table differ along what the rule generalises over: which client the
//! probe is written with (both the clients a container's image ships), which form of its own
//! task the request names (the loopback by name, the loopback by address with the port named,
//! and a host outside the task), what it asks for (the document the container's program wrote,
//! another document it wrote, a path nothing wrote, a port nothing listens on), and whether the
//! container runs a server at all. The recorded configuration — the documented request over a
//! container that serves it — is one row of it.
//!
//! The sibling suite `ecs_task_registration_precedes_its_health_verdict.rs` pins that the
//! REGISTRATION does not follow from the health verdict; this one pins what the two verdicts of
//! a serving task are.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, members_of, opt_element};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The ECS cluster and the container name every task below runs.
const CLUSTER: &str = "probedcluster";
const CONTAINER: &str = "app";

/// The port the containers listen on, which is the port the bindings below name.
const CONTAINER_PORT: i64 = 80;

/// The document root the image's own server serves, and the two documents a container's program
/// writes into it: its root page and the one a probe of another path asks for.
const DOCUMENT_ROOT: &str = "/usr/share/nginx/html";
const SERVED_BODY: &str = "PROBED-CONTAINER-BODY";
const HEALTH_DOCUMENT: &str = "health";

/// The image every container below runs: a web server image, whose own command starts the server
/// its configuration ships.
const IMAGE: &str = "public.ecr.aws/docker/library/nginx:alpine";

/// The network every task below is placed in.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.91.1.0/24", "us-east-1a"),
    ("10.91.2.0/24", "us-east-1b"),
];

/// The `HealthCheck$interval` every probe below runs at: the smallest the member documents
/// ("You may specify between 5 and 300 seconds"), so a row's verdict lands inside the budget
/// without naming a value AWS refuses. `retries` is one, the fewest a failure takes to be
/// believed, for the same reason.
const PROBE_INTERVAL_SECONDS: i64 = 5;
const PROBE_RETRIES: i64 = 1;

/// The `HealthCheckIntervalSeconds` the target group below is created with: the shortest the
/// member admits ("the range is 5-300").
const HEALTH_CHECK_INTERVAL_SECONDS: &str = "5";

/// The `HealthStatus` values `Container$healthStatus` reports, which are the terminal readings a
/// row waits for ("`UNKNOWN` — The container health check is being evaluated").
const HEALTHY: &str = "HEALTHY";
const UNHEALTHY: &str = "UNHEALTHY";
const UNKNOWN: &str = "UNKNOWN";

/// The `TargetHealthStateEnum` value of a registration the load balancer may route to.
const TARGET_HEALTHY: &str = "healthy";

/// How long a wait below keeps looking, and how often it looks. Generous on purpose: what is
/// under test is WHICH health is reached, not how fast — the deadline is only here so a failure
/// is a failure rather than a hang. Every wait stops the instant its own condition holds.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

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
        status, 200,
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
        status, 200,
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
        status, 200,
        "setup: ECS {operation} answered {status} with {parsed}"
    );
    parsed
}

/// The network every task below runs in: a VPC with two subnets, the internet gateway and route
/// the image pull of a placement takes, and the one security group the load balancer and the
/// tasks share — which admits the checks of anything in it, as a self-referencing group does.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
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
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "probed-shared"),
                (
                    "GroupDescription",
                    "the group the load balancer and the tasks share",
                ),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // The ONE rule this suite authorizes: the group takes ITSELF as a source on every port, so a
    // check performed from a load balancer in it reaches a target in it.
    ec2(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", group.as_str()),
            ("IpPermissions.1.IpProtocol", "-1"),
            ("IpPermissions.1.Groups.1.GroupId", group.as_str()),
        ],
    )
    .await;
    Network {
        vpc,
        subnets,
        group,
    }
}

/// The container command that writes the documents below into the image's document root and then
/// runs the image's OWN command, which is what starts the server its configuration ships.
fn serving_command() -> Value {
    json!([
        "sh",
        "-c",
        format!(
            "echo {SERVED_BODY} > {DOCUMENT_ROOT}/index.html && \
             echo {SERVED_BODY} > {DOCUMENT_ROOT}/{HEALTH_DOCUMENT} && \
             nginx -g 'daemon off;'"
        ),
    ])
}

/// The command of a container that runs NO server, so nothing of its task is listening at all.
fn silent_command() -> Value {
    json!(["sh", "-c", "sleep 300"])
}

/// Register one `awsvpc`, Fargate-compatible revision of `family` whose container runs `command`
/// and declares `probe` as its health check.
async fn register_revision(
    router: &MultiServiceRouter,
    family: &str,
    command: Value,
    probe: &str,
) {
    ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "essential": true,
                "command": command,
                "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
                "healthCheck": {
                    "command": ["CMD-SHELL", probe],
                    "interval": PROBE_INTERVAL_SECONDS,
                    "timeout": 2,
                    "retries": PROBE_RETRIES,
                },
            }],
        }),
    )
    .await;
}

/// One row of the probe table.
struct Probe {
    case: &'static str,
    /// The task-definition family the row registers.
    family: &'static str,
    /// Whether its container runs the server, or nothing at all.
    serving: bool,
    /// The health-check command, as a `CMD-SHELL` program.
    probe: &'static str,
    /// The `healthStatus` that container must reach.
    expected: &'static str,
}

/// The rows: every form of the rule's own class, with the recorded configuration as the first.
fn probes() -> Vec<Probe> {
    vec![
        Probe {
            case: "the documented request to the container's own server",
            family: "probedlocalhost",
            serving: true,
            probe: "curl -f http://localhost/ || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the loopback address with the port the container listens on named",
            family: "probedloopbackport",
            serving: true,
            probe: "curl -fsS http://127.0.0.1:80/health || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the other client, asked only whether the document is there",
            family: "probedspider",
            serving: true,
            probe: "wget -q --spider http://localhost/ || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the other client, writing the document to its own output",
            family: "probedwget",
            serving: true,
            probe: "wget -q -O - http://localhost/health || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the other client's options written as one cluster",
            family: "probedwgetcluster",
            serving: true,
            probe: "wget -qO- http://localhost/health || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the client invoked by its absolute path",
            family: "probedabsolutepath",
            serving: true,
            probe: "/usr/bin/curl -f http://localhost/ || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the headers of the document alone",
            family: "probedheaders",
            serving: true,
            probe: "curl -f -I http://localhost/ || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "a path nothing of the container wrote",
            family: "probedmissingpath",
            serving: true,
            probe: "curl -f http://localhost/nothing-here || exit 1",
            expected: UNHEALTHY,
        },
        Probe {
            case: "a port nothing of the task listens on",
            family: "probedotherport",
            serving: true,
            probe: "curl -f http://localhost:8080/ || exit 1",
            expected: UNHEALTHY,
        },
        Probe {
            case: "a host outside the task, which such a container has no network for",
            family: "probedforeignhost",
            serving: true,
            probe: "curl -f http://example.invalid/ || exit 1",
            expected: UNHEALTHY,
        },
        Probe {
            case: "a container that runs no server at all",
            family: "probedsilent",
            serving: false,
            probe: "curl -f http://localhost/ || exit 1",
            expected: UNHEALTHY,
        },
    ]
}

/// Run one task of `family` in `network` and answer its ARN.
async fn run_task(router: &MultiServiceRouter, network: &Network, family: &str) -> String {
    let run = ecs_ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnets[0]],
                    "securityGroups": [network.group],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    run["tasks"][0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the run must place one task, got {run}"))
        .to_string()
}

/// The task as `DescribeTasks` reports it now.
async fn describe_task(router: &MultiServiceRouter, task_arn: &str) -> Value {
    let described = ecs_ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
    )
    .await;
    described["tasks"][0].clone()
}

/// The `healthStatus` the containers of `task` report, in order.
fn container_health(task: &Value) -> Vec<String> {
    task["containers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|container| {
            container["healthStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
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

#[tokio::test(flavor = "multi_thread")]
async fn a_containers_probe_is_answered_by_the_server_of_its_own_task() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let rows = probes();
    // Every row's task is placed FIRST and waited for afterwards: the probes are clock-owed, so
    // the rows converge beside each other rather than one budget after another.
    let mut placed: Vec<(&Probe, String)> = Vec::new();
    for row in &rows {
        let command = if row.serving {
            serving_command()
        } else {
            silent_command()
        };
        register_revision(&router, row.family, command, row.probe).await;
        let task_arn = run_task(&router, &network, row.family).await;
        placed.push((row, task_arn));
    }
    for (row, task_arn) in &placed {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let task = describe_task(&router, task_arn).await;
            let reported = container_health(&task);
            // A reading that is still being evaluated is not a verdict: the row waits for the
            // terminal one its probe reaches.
            let settled = !reported.is_empty() && reported.iter().all(|health| health != UNKNOWN);
            if settled || Instant::now() >= deadline {
                assert_eq!(
                    reported,
                    vec![row.expected.to_string()],
                    "case {:?}: expected {:?}, got {reported:?} from {task}",
                    row.case,
                    row.expected
                );
                break;
            }
            ::tokio::time::sleep(POLL).await;
        }
    }
}

/// The service and the groups the convergence test below creates.
const SERVICE_NAME: &str = "probedservice";
const WIRED_GROUP: &str = "probed-wired";
const UNREFERENCED_GROUP: &str = "probed-unreferenced";
const LOAD_BALANCER_NAME: &str = "probed-alb";

/// Create one `ip` target group named `name` in `network`, health-checking `/` over HTTP, and
/// answer its ARN.
async fn target_group(router: &MultiServiceRouter, network: &Network, name: &str) -> String {
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", name),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/"),
            ("HealthCheckIntervalSeconds", HEALTH_CHECK_INTERVAL_SECONDS),
            ("HealthyThresholdCount", "2"),
            ("Matcher.HttpCode", "200"),
        ],
    )
    .await;
    opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("no TargetGroupArn for {name} in {created}"))
}

/// The `(Id, Port, State)` of every registration `group` reports.
async fn target_health(router: &MultiServiceRouter, group: &str) -> Vec<(String, String, String)> {
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
            (text("Id"), text("Port"), text("State"))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_container_health_and_the_target_health_of_one_serving_task_converge() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    register_revision(
        &router,
        "probedconverging",
        serving_command(),
        "curl -f http://localhost/ || exit 1",
    )
    .await;
    let unreferenced = target_group(&router, &network, UNREFERENCED_GROUP).await;
    let wired = target_group(&router, &network, WIRED_GROUP).await;
    let balanced = elbv2(
        &router,
        "CreateLoadBalancer",
        &[
            ("Name", LOAD_BALANCER_NAME),
            ("Type", "application"),
            ("Scheme", "internet-facing"),
            ("Subnets.member.1", network.subnets[0].as_str()),
            ("Subnets.member.2", network.subnets[1].as_str()),
            ("SecurityGroups.member.1", network.group.as_str()),
        ],
    )
    .await;
    let load_balancer = element(&balanced, "LoadBalancerArn");
    elbv2(
        &router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer.as_str()),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", wired.as_str()),
        ],
    )
    .await;
    ecs_ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE_NAME,
            "taskDefinition": "probedconverging",
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnets[0], network.subnets[1]],
                    "securityGroups": [network.group],
                    "assignPublicIp": "ENABLED",
                }
            },
            "loadBalancers": [{
                "targetGroupArn": wired,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
            "healthCheckGracePeriodSeconds": 120,
        }),
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs_ok(
            &router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [SERVICE_NAME] }),
        )
        .await;
        let running = described["services"][0]["runningCount"].as_i64().unwrap_or(0);
        let listed = ecs_ok(
            &router,
            "ListTasks",
            json!({ "cluster": CLUSTER, "serviceName": SERVICE_NAME }),
        )
        .await;
        let arns: Vec<String> = listed["taskArns"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|arn| arn.as_str().map(str::to_string))
            .collect();
        let task = match arns.first() {
            Some(arn) => describe_task(&router, arn).await,
            None => Value::Null,
        };
        let health = container_health(&task);
        let address = private_address(&task);
        let registered = target_health(&router, &wired).await;
        // The group NO service references is read on every pass, so its emptiness holds
        // throughout the window the service's task comes up in rather than once at the end.
        let unwired = target_health(&router, &unreferenced).await;
        assert!(
            unwired.is_empty(),
            "a target group no service's loadBalancers names must stay empty, got {unwired:?}"
        );
        let converged = running == 1
            && health == vec![HEALTHY.to_string()]
            && address.is_some()
            && registered.len() == 1
            && registered[0].2 == TARGET_HEALTHY;
        if converged || Instant::now() >= deadline {
            assert_eq!(
                running, 1,
                "the service must report its one task running, got {described}"
            );
            assert_eq!(
                health,
                vec![HEALTHY.to_string()],
                "the container's own probe must report it healthy, got {health:?} from {task}"
            );
            let address = address
                .unwrap_or_else(|| panic!("the task must report its interface's address: {task}"));
            assert_eq!(
                registered,
                vec![(
                    address.clone(),
                    CONTAINER_PORT.to_string(),
                    TARGET_HEALTHY.to_string()
                )],
                "the wired group must report the task's address {address} on port \
                 {CONTAINER_PORT} as {TARGET_HEALTHY}, got {registered:?}"
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}
