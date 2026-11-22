// SPDX-License-Identifier: CC0-1.0

//! What a container's HTTP client does with the document it fetched: the probe a health check
//! is written as a PIPELINE reads that document, and the container's `healthStatus` follows
//! what the probe made of it.
//!
//! RULE: the clients a container's session provides write what they fetched to the session's
//! own STANDARD OUTPUT whenever the invocation asked for it there — `curl` by default ("`-o`,
//! `--output <file>` — write to file instead of stdout") and `wget` when the file it is given
//! is `-` ("if `-` is used as file, documents will be printed to standard output", the GNU Wget
//! manual's `-O file` / `--output-document=file`) — so whatever reads that stream sees the
//! document: the next command of a pipeline, the file a redirection names, the value of a
//! command substitution. An invocation that names a FILE saves it there instead and writes
//! nothing to the stream. A health check is exactly such a command (`HealthCheck$command` is
//! "the command that the container runs to determine if it is healthy ... An exit code of 0
//! indicates success, and non-zero exit code indicates failure"), so a probe that fetches its
//! own server's document and matches it reports `HEALTHY` when the document matches and
//! `UNHEALTHY` once it has failed `retries` times when it does not.
//!
//! The rows of the first table differ along what the rule generalises over: which client the
//! document is fetched with, how the invocation spells the request for standard output (the
//! operand apart, joined to the option, and inside a cluster of options), what reads the stream
//! (a pipeline, a second stage of one, a redirection, a command substitution), whether the
//! document matches what the probe looks for, and — the contrast that pins the other half of
//! the rule — an invocation that SAVES the document, whose pipeline therefore reads nothing.
//!
//! Two crates have to agree for the rule to hold, which is why this suite lives here: Amazon
//! EC2 owns the network the task is placed in and its interface's address, and Amazon ECS runs
//! the container, its server and the probe. Everything is driven through the real operations
//! over ONE shared engine, so what is asserted is what each service persisted, read back
//! through `DescribeTasks`.
//!
//! The second test is the same rule under a load-balanced service, where the container's own
//! verdict and the load balancer's verdict of the same task have to converge: `DescribeTasks`
//! reports `containers[].healthStatus` `HEALTHY` from the container's own probe while
//! `DescribeTargetHealth` reports the registration of that task's address `healthy`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, items_of, members_of, opt_element};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The cluster and the container name every task below runs.
const CLUSTER: &str = "pipedprobes";
const CONTAINER: &str = "app";

/// The port the containers listen on, which is the port their own command serves on and the
/// `containerPort` the revisions declare.
const CONTAINER_PORT: i64 = 80;

/// The image every container below runs: a multi-call-binary image, whose own `httpd` applet is
/// what the container's command starts.
const IMAGE: &str = "busybox:1.36";

/// The command every container below runs: it writes the document `OK` into `/www`, then serves
/// that directory with the image's own `httpd` applet on the port above — a foreground server,
/// so the command holds the container's program until the task is stopped. The word it writes is
/// the word each probe below looks for in what it fetched, and `MISSING` is a word no document of
/// these containers holds.
const SERVING_COMMAND: &str = "mkdir -p /www && echo OK > /www/index.html && httpd -f -p 80 -h /www";

/// The network every task below is placed in.
const VPC_CIDR: &str = "10.94.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.94.1.0/24", "us-east-1a"),
    ("10.94.2.0/24", "us-east-1b"),
];

/// The `HealthCheck` members the first table's rows declare: the smallest `interval` the member
/// documents ("You may specify between 5 and 300 seconds"), the smallest `timeout` ("You may
/// specify between 2 and 60 seconds") and the fewest `retries` a failure takes to be believed
/// ("You may specify between 1 and 10 retries"), so every row's verdict lands inside the poll
/// budget without naming a value AWS refuses.
const PROBE_INTERVAL_SECONDS: i64 = 5;
const PROBE_TIMEOUT_SECONDS: i64 = 2;
const PROBE_RETRIES: i64 = 1;

/// The `HealthStatus` values `Container$healthStatus` reports, which are the terminal readings a
/// row waits for ("`UNKNOWN` — The container health check is being evaluated").
const HEALTHY: &str = "HEALTHY";
const UNHEALTHY: &str = "UNHEALTHY";

/// The `TargetHealthStateEnum` value of a registration the load balancer may route to.
const TARGET_HEALTHY: &str = "healthy";

/// How long a wait below keeps looking, and how often it looks. Generous on purpose: what is
/// under test is WHICH reading each probe reaches, not how fast — the deadline is only here so a
/// failure is a failure rather than a hang. Every wait stops the instant its condition holds.
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

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. The key id
/// and signature are not credentials: the router above runs with request authentication off, so
/// nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its status
/// and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
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

/// One `awsJson1_1` call to Amazon ECS that must succeed, answered with its parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header("authorization", authorization("ecs"))
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

/// The network every task below runs in: a VPC with an internet gateway and a default route
/// through it — the path the image fetch of a placement takes — two subnets, and one
/// self-referencing security group the tasks and the load balancer share.
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
    // The default route goes into the table the VPC came with, which every subnet below is
    // implicitly associated with — "your VPC automatically comes with a main route table"
    // (Amazon VPC User Guide, "Route tables").
    let tables = ec2(
        router,
        "DescribeRouteTables",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc.as_str()),
        ],
    )
    .await;
    let main = items_of(&tables)
        .into_iter()
        .find(|item| elements(item, "main").iter().any(|main| main == "true"))
        .and_then(|item| elements(item, "routeTableId").into_iter().next())
        .unwrap_or_else(|| panic!("the VPC must come with a main route table, got {tables}"));
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", main.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", gateway.as_str()),
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
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "pipedprobes-shared"),
                (
                    "GroupDescription",
                    "the load balancer and the tasks it checks",
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

/// Register one `awsvpc`, Fargate-compatible revision of `family` whose container serves the
/// document above and declares `probe` as its `CMD-SHELL` health check.
async fn register_revision(
    router: &MultiServiceRouter,
    family: &str,
    probe: &str,
    check: &Value,
) {
    let mut health = check.clone();
    health["command"] = json!(["CMD-SHELL", probe]);
    ecs(
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
                "command": ["sh", "-c", SERVING_COMMAND],
                "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
                "healthCheck": health,
            }],
        }),
    )
    .await;
}

/// The `HealthCheck` members the first table's rows declare, beside the command each row writes.
fn table_check() -> Value {
    json!({
        "interval": PROBE_INTERVAL_SECONDS,
        "timeout": PROBE_TIMEOUT_SECONDS,
        "retries": PROBE_RETRIES,
    })
}

/// Run one task of `family` in `network` and answer its ARN.
async fn run_task(router: &MultiServiceRouter, network: &Network, family: &str) -> String {
    let run = ecs(
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

/// The task `task_arn` names, as `DescribeTasks` reports it now.
async fn describe_task(router: &MultiServiceRouter, task_arn: &str) -> Value {
    let described = ecs(
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
        .filter_map(|attachment| attachment["details"].as_array())
        .flatten()
        .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
        .and_then(|detail| detail["value"].as_str())
        .map(str::to_string)
}

/// Keep reading `task_arn` until its containers report `expected`, and answer the last reading
/// either way — so the caller's own assertion quotes what it really settled on.
async fn health_reaching(
    router: &MultiServiceRouter,
    task_arn: &str,
    expected: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = container_health(&describe_task(router, task_arn).await);
        if reported.iter().all(|reading| reading == expected) && !reported.is_empty() {
            return reported;
        }
        if Instant::now() >= deadline {
            return reported;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// One row of the probe table.
struct Probe {
    case: &'static str,
    /// The task-definition family the row registers.
    family: &'static str,
    /// The health-check command, as a `CMD-SHELL` program.
    probe: &'static str,
    /// The `healthStatus` that container must reach.
    expected: &'static str,
}

/// The rows: every form of the rule's own class, with the recorded configuration as the first.
fn probes() -> Vec<Probe> {
    vec![
        Probe {
            case: "the document piped into a command that matches it",
            family: "pipedjoined",
            probe: "wget -q -O- http://localhost:80/ | grep -q OK || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the same request with the standard-output operand written apart",
            family: "pipedapart",
            probe: "wget -q -O - http://localhost:80/ | grep -q OK || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the same request with the option inside a cluster",
            family: "pipedcluster",
            probe: "wget -qO- http://localhost/ | grep -q OK || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the other client, whose own output is where it writes by default",
            family: "pipedcurl",
            probe: "curl -fsS http://localhost/ | grep -q OK || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the document redirected into a file the next command reads",
            family: "pipedredirect",
            probe: "wget -q -O- http://localhost/ > /tmp/fetched; grep -q OK /tmp/fetched \
                    || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "the document as the value of a command substitution",
            family: "pipedsubstitution",
            probe: "fetched=$(wget -q -O- http://localhost/); echo $fetched | grep -q OK \
                    || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "two stages reading the document after the fetch",
            family: "pipedtwostages",
            probe: "wget -q -O- http://localhost/ | cat | grep -q OK || exit 1",
            expected: HEALTHY,
        },
        Probe {
            case: "a word the fetched document does not hold",
            family: "pipedabsent",
            probe: "wget -q -O- http://localhost/ | grep -q MISSING || exit 1",
            expected: UNHEALTHY,
        },
        Probe {
            case: "a path the container's own server holds nothing at",
            family: "pipedmissingpath",
            probe: "wget -q -O- http://localhost/nothing-here | grep -q OK || exit 1",
            expected: UNHEALTHY,
        },
        Probe {
            case: "the contrast: a fetch that SAVES the document, so the pipe reads nothing",
            family: "pipedsaved",
            probe: "wget -q -O /tmp/held http://localhost/ | grep -q OK || exit 1",
            expected: UNHEALTHY,
        },
    ]
}

/// RULE: a container's HTTP client writes what it fetched to the session's standard output
/// whenever the invocation asked for it there, so a health check written as a pipeline reads
/// that document — and one that asked for the document to be SAVED writes nothing to the
/// stream.
#[tokio::test]
async fn a_probe_reads_the_document_its_client_fetched() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    let rows = probes();
    let check = table_check();
    let mut placed: Vec<(&Probe, String)> = Vec::new();
    for row in &rows {
        register_revision(&router, row.family, row.probe, &check).await;
        let task_arn = run_task(&router, &network, row.family).await;
        placed.push((row, task_arn));
    }

    for (row, task_arn) in &placed {
        let reported = health_reaching(&router, task_arn, row.expected).await;
        assert_eq!(
            reported,
            vec![row.expected.to_string()],
            "case {:?}: expected the container to report {:?}, got {reported:?}",
            row.case,
            row.expected
        );
        let task = describe_task(&router, task_arn).await;
        // "The health status for the task. It's determined by the health of the essential
        // containers in the task" — one essential container, so the task reads the same.
        assert_eq!(
            task["healthStatus"].as_str(),
            Some(row.expected),
            "case {:?}: expected the task to report {:?}, got {task}",
            row.case,
            row.expected
        );
    }
}

/// Keep reading the service `service` until it reports `desired` tasks running, and answer the
/// last count either way.
async fn running_count_reaching(
    router: &MultiServiceRouter,
    service: &str,
    desired: i64,
) -> i64 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(
            router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [service] }),
        )
        .await;
        let reported = described["services"][0]["runningCount"]
            .as_i64()
            .unwrap_or(-1);
        if reported == desired || Instant::now() >= deadline {
            return reported;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The `TargetHealth.State` the group `arn` reports for the registration of `address`, and
/// `None` while it holds none.
async fn target_state(
    router: &MultiServiceRouter,
    arn: &str,
    address: &str,
) -> Option<String> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    members_of(&body).into_iter().find_map(|description| {
        let id = elements(description, "Id").into_iter().next()?;
        (id == address)
            .then(|| elements(description, "State").into_iter().next())
            .flatten()
    })
}

/// Keep reading the group `arn` until the registration of `address` reports `expected`, and
/// answer the last state either way.
async fn target_state_reaching(
    router: &MultiServiceRouter,
    arn: &str,
    address: &str,
    expected: &str,
) -> Option<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = target_state(router, arn, address).await;
        if reported.as_deref() == Some(expected) || Instant::now() >= deadline {
            return reported;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The ARN of one task of `service`, waited for while the service places it.
async fn service_task(router: &MultiServiceRouter, service: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = ecs(
            router,
            "ListTasks",
            json!({
                "cluster": CLUSTER,
                "serviceName": service,
                "desiredStatus": "RUNNING",
            }),
        )
        .await;
        let first = listed["taskArns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|arn| arn.as_str())
            .next()
            .map(str::to_string);
        if let Some(first) = first {
            return first;
        }
        assert!(
            Instant::now() < deadline,
            "case \"the load-balanced service\": expected it to place a task, got {listed}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// RULE: the container's own verdict and the load balancer's verdict of the same task are
/// computed independently and CONVERGE for a task that is serving — `DescribeTasks` reports
/// `containers[].healthStatus` `HEALTHY` from the probe the container ran, while
/// `DescribeTargetHealth` reports the registration of that task's address `healthy`.
#[tokio::test]
async fn a_served_tasks_two_health_verdicts_converge() {
    const FAMILY: &str = "pipedservice";
    const SERVICE: &str = "pipedservicesvc";
    const GROUP: &str = "pipedprobes-group";

    let router = emulator();
    let network = seed_network(&router).await;
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    // The members a deployment behind a load balancer declares: a grace period the probe's
    // first execution falls inside, and the spacing and tolerance of the executions after it.
    register_revision(
        &router,
        FAMILY,
        "wget -q -O- http://localhost:80/ | grep -q OK || exit 1",
        &json!({ "interval": 15, "timeout": 5, "retries": 3, "startPeriod": 10 }),
    )
    .await;

    let created = elbv2(
        &router,
        "CreateTargetGroup",
        &[
            ("Name", GROUP),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/"),
            // The shortest interval the member admits ("the range is 5-300") and the fewest
            // checks a threshold may ask for ("the range is 2-10").
            ("HealthCheckIntervalSeconds", "5"),
            ("HealthyThresholdCount", "2"),
        ],
    )
    .await;
    let group = opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("CreateTargetGroup must report an ARN, got {created}"));
    let load_balancer = element(
        &elbv2(
            &router,
            "CreateLoadBalancer",
            &[
                ("Name", "pipedprobes-alb"),
                ("Subnets.member.1", network.subnets[0].as_str()),
                ("Subnets.member.2", network.subnets[1].as_str()),
                ("SecurityGroups.member.1", network.group.as_str()),
                ("Scheme", "internet-facing"),
            ],
        )
        .await,
        "LoadBalancerArn",
    );
    // A listener forwarding to the group is what makes it one the load balancer forwards to —
    // and what lets an ECS service bind it at all.
    elbv2(
        &router,
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

    ecs(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": FAMILY,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": { "awsvpcConfiguration": {
                "subnets": network.subnets,
                "securityGroups": [network.group],
                "assignPublicIp": "ENABLED",
            }},
            "loadBalancers": [{
                "targetGroupArn": group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
        }),
    )
    .await;

    let running = running_count_reaching(&router, SERVICE, 1).await;
    assert_eq!(
        running, 1,
        "case \"the load-balanced service\": expected one running task, got {running}"
    );
    let task_arn = service_task(&router, SERVICE).await;
    let reported = health_reaching(&router, &task_arn, HEALTHY).await;
    assert_eq!(
        reported,
        vec![HEALTHY.to_string()],
        "case \"the load-balanced service\": expected the container's probe to report \
         {HEALTHY:?}, got {reported:?}"
    );
    let task = describe_task(&router, &task_arn).await;
    let address = private_address(&task).unwrap_or_else(|| {
        panic!("case \"the load-balanced service\": expected one private address, got {task}")
    });
    let state = target_state_reaching(&router, &group, &address, TARGET_HEALTHY).await;
    assert_eq!(
        state.as_deref(),
        Some(TARGET_HEALTHY),
        "case \"the load-balanced service\": expected the registration of {address} to report \
         {TARGET_HEALTHY:?}, got {state:?}"
    );
}
