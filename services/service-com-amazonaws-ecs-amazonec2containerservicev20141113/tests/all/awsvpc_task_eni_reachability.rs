// SPDX-License-Identifier: CC0-1.0

//! An `awsvpc` task ANSWERS on the private address of its own elastic network interface —
//! and only while a security group on that interface admits the client that asks.
//!
//! RULE: a workload reaches another workload of the account at the private address of that
//! workload's elastic network interface exactly when an inbound rule of a security group on
//! that interface names the packet — its protocol, its port and its source. There is no deny
//! rule, so a packet no rule names is DROPPED: the client gets no answer at all, which is what
//! revoking the rule that named one client does to that client while a client another rule
//! still names keeps getting through, and what authorizing it again undoes.
//!
//! The grounds. `RegisterTaskDefinition$networkMode`: "If the network mode is `awsvpc`, the
//! task is allocated an elastic network interface", in the subnet and with the security groups
//! the launch names (`AwsVpcConfiguration$subnets`/`$securityGroups`), and `Attachment$details`
//! "for elastic network interfaces ... includes ... the private IPv4 address" — the address the
//! task is reached at. A security group "acts as a virtual firewall ... to control inbound and
//! outbound traffic", its inbound rules "control the incoming traffic that's allowed to reach
//! the instance", and "all rules are permissive" (Amazon VPC User Guide, "Security groups"), so
//! nothing but a rule lets a packet in and no rule keeps one out.
//!
//! Driven end to end over the real operations on ONE shared engine: the network through Amazon
//! EC2's own API, the log group through CloudWatch Logs' own, the execution role through IAM's
//! own, and every task through `RunTask`. What each probing container OBSERVED is read back out
//! of its own `awslogs` stream with CloudWatch Logs' `GetLogEvents` — the client's own report of
//! what came back — so nothing here reads a store by hand.
//!
//! The cases differ along what the rule generalises over: the KIND of source a rule names (the
//! client's own security group, and the address range its address lies in), WHICH client a rule
//! names (one revoked while the other stays), the port a rule names (the one served, and
//! another one), and the direction the change goes (authorized, revoked, authorized again).
//!
//! Every clock-owed move — the interface the task is allocated, the task's own start, the
//! delivery its container's run makes — is POLLED for until it holds, never slept for, and the
//! deadlines are generous: nothing here asserts on how long anything took.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, ec2, ec2_element, iam, list, logs, ok, poll_until, string, ACCOUNT,
    SIGNED_REGION,
};

/// The cluster every task below runs in.
const CLUSTER: &str = "eni-reachable";

/// The one container of the serving revision, and of the probing one.
const SERVER_CONTAINER: &str = "server";
const CLIENT_CONTAINER: &str = "client";

/// The task-definition families those two revisions are registered under.
const SERVER_FAMILY: &str = "eni-reachable-server";
const CLIENT_FAMILY: &str = "eni-reachable-client";

/// The log group both containers deliver their output to, and the `awslogs-stream-prefix`
/// their streams are named under.
const LOG_GROUP: &str = "/emulator/ecs/eni-reachable";
const STREAM_PREFIX: &str = "probe";

/// The image both revisions start from: a reference naming a registry this emulator answers
/// nothing for, so what each container does is its own program's doing and no image's.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The port the server's own program listens on, and the directory it serves.
const SERVED_PORT: i32 = 8080;
const SERVED_ROOT: &str = "/srv/app";

/// The document the server serves at its root, which is what a client that REACHED it writes
/// to its own log stream.
const SERVED_BODY: &str = "ENI-REACHABLE-BODY";

/// The network every task below is placed in.
const VPC_CIDR: &str = "10.72.0.0/16";
const SUBNET_CIDR: &str = "10.72.1.0/24";

/// The IAM role the revisions below name as their task execution role — "the task execution
/// role that grants the Amazon ECS container agent permission to make Amazon Web Services API
/// calls on your behalf" (`TaskDefinition$executionRoleArn`), which a Fargate revision
/// delivering through the `awslogs` driver carries.
const EXECUTION_ROLE: &str = "eniReachableExecutionRole";

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to
/// do it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// A router carrying Amazon ECS, Amazon EC2 — which owns the VPC, the subnet and the security
/// groups this rule is decided by — IAM, which holds the execution role Amazon ECS assumes
/// before it launches a task, and CloudWatch Logs, which each container's output is delivered
/// to, over ONE shared engine, mounted the way the production router mounts them.
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
        service_com_amazonaws_ec2_amazonec2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// The network the tasks below run in: a VPC, one subnet of it whose route table sends
/// internet-bound traffic to an internet gateway — the placement a task with a public address
/// pulls its images over — and nothing else.
struct Network {
    subnet_id: String,
    vpc_id: String,
}

/// Build that network through Amazon EC2's OWN API.
async fn network(router: &MultiServiceRouter, case: &str) -> Network {
    let vpc_id = ec2_element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)], case).await,
        "vpcId",
        case,
    );
    let subnet_id = ec2_element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", &vpc_id),
                ("CidrBlock", SUBNET_CIDR),
                ("AvailabilityZone", &format!("{SIGNED_REGION}a")),
            ],
            case,
        )
        .await,
        "subnetId",
        case,
    );
    // "To use an internet gateway, your subnet's route table must contain a route that directs
    // internet-bound traffic to the internet gateway" (Amazon VPC User Guide) — which is what
    // a task given a public address fetches its container images over.
    let gateway_id = ec2_element(
        &ec2(router, "CreateInternetGateway", &[], case).await,
        "internetGatewayId",
        case,
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway_id), ("VpcId", &vpc_id)],
        case,
    )
    .await;
    let route_table_id = ec2_element(
        &ec2(router, "CreateRouteTable", &[("VpcId", &vpc_id)], case).await,
        "routeTableId",
        case,
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[("RouteTableId", &route_table_id), ("SubnetId", &subnet_id)],
        case,
    )
    .await;
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", &route_table_id),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", &gateway_id),
        ],
        case,
    )
    .await;
    Network { subnet_id, vpc_id }
}

/// One security group of `vpc_id`, created over the wire, carrying no rule of its own.
async fn security_group(
    router: &MultiServiceRouter,
    vpc_id: &str,
    group_name: &str,
    case: &str,
) -> String {
    ec2_element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", group_name),
                ("GroupDescription", "a group this suite decides a reach by"),
                ("VpcId", vpc_id),
            ],
            case,
        )
        .await,
        "groupId",
        case,
    )
}

/// WHAT one inbound rule names as the source it admits.
enum Source<'a> {
    /// Another security group: "the source is the security group itself", so every member of
    /// it is admitted.
    Group(&'a str),
    /// An address range, which admits every address inside it.
    Range(&'a str),
}

/// The `IpPermissions.1.…` parameters naming ONE inbound rule: TCP on `port`, from `source`.
fn permission(port: i32, source: &Source<'_>) -> Vec<(String, String)> {
    let (list, member, value) = match source {
        Source::Group(group_id) => ("Groups", "GroupId", (*group_id).to_string()),
        Source::Range(cidr) => ("IpRanges", "CidrIp", (*cidr).to_string()),
    };
    vec![
        ("IpPermissions.1.IpProtocol".to_string(), "tcp".to_string()),
        ("IpPermissions.1.FromPort".to_string(), port.to_string()),
        ("IpPermissions.1.ToPort".to_string(), port.to_string()),
        (format!("IpPermissions.1.{list}.1.{member}"), value),
    ]
}

/// Authorize — or revoke — that one rule on `group_id`, through Amazon EC2's own API.
async fn change_ingress(
    router: &MultiServiceRouter,
    action: &str,
    group_id: &str,
    port: i32,
    source: &Source<'_>,
    case: &str,
) {
    let mut params: Vec<(String, String)> =
        vec![("GroupId".to_string(), group_id.to_string())];
    params.extend(permission(port, source));
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    ec2(router, action, &borrowed, case).await;
}

/// One CloudWatch Logs call, read through the ONE caller of that service these suites share
/// ([`crate::ecs_wire::logs`]).
async fn create_log_group(router: &MultiServiceRouter, case: &str) {
    let (status, body) = logs(
        router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateLogGroup to answer 200, got {status} with {body}"
    );
}

/// Create the task execution role the revisions name, through IAM's OWN API, with a trust
/// policy that lets Amazon ECS assume it and the two grants its log delivery needs — the
/// driver creates the stream it delivers to and writes each line into it — and answer its ARN.
async fn create_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": [ECS_TASKS_PRINCIPAL] },
            "Action": [ASSUME_ROLE],
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        case,
    )
    .await;
    let delivery = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("PolicyName", "log-delivery".to_string()),
            ("PolicyDocument", delivery),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// Register one Fargate-compatible `awsvpc` revision whose single container `name` runs
/// `command` and delivers its output through the `awslogs` driver.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    name: &str,
    command: Vec<String>,
    execution_role: &str,
    case: &str,
) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role,
            "containerDefinitions": [{
                "name": name,
                "image": IMAGE,
                "essential": true,
                "command": command,
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": LOG_GROUP,
                        "awslogs-region": SIGNED_REGION,
                        "awslogs-stream-prefix": STREAM_PREFIX,
                    },
                },
            }],
        }),
        case,
    )
    .await;
}

/// Launch one task of `family` into `security_group`, with `command` overriding `container`'s
/// own, and answer its ARN.
async fn run_task(
    router: &MultiServiceRouter,
    network: &Network,
    family: &str,
    container: &str,
    security_group: &str,
    command: Option<Vec<String>>,
    case: &str,
) -> String {
    let mut request = json!({
        "cluster": CLUSTER,
        "taskDefinition": family,
        "launchType": "FARGATE",
        "networkConfiguration": {
            "awsvpcConfiguration": {
                "subnets": [network.subnet_id],
                "securityGroups": [security_group],
                // "Whether the task's elastic network interface receives a public IP address"
                // (`AwsVpcConfiguration$assignPublicIp`) — what the images of a task in a
                // public subnet are pulled over.
                "assignPublicIp": "ENABLED",
            },
        },
    });
    if let Some(command) = command {
        request["overrides"] = json!({
            "containerOverrides": [{ "name": container, "command": command }],
        });
    }
    let body = ok(router, "RunTask", request, case).await;
    assert!(
        list(&body, "failures", case).is_empty(),
        "case {case:?}: expected RunTask to place the task, got {body}"
    );
    string(&body["tasks"][0], "taskArn", case)
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming the driver's own `awslogs-stream-prefix`
/// documentation states. Spelled out here rather than imported, so a wrong naming in the
/// implementation cannot make this suite pass.
fn stream_of(container: &str, task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{container}/{task_id}")
}

/// The messages CloudWatch Logs holds in `stream`, oldest first. An unknown stream reads as no
/// messages, which is what a stream nothing has been delivered to yet holds.
async fn delivered(router: &MultiServiceRouter, stream: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
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

/// Wait until the one `RUNNING` task of `task_arn` reports the private address of the
/// interface it was allocated, and answer that address.
///
/// `Attachment$details` "for elastic network interfaces ... includes the network interface ID,
/// the MAC address, the subnet ID, and the private IPv4 address", and the interface is created
/// and attached one clock-owed step at a time, so this is a condition to look again on rather
/// than a reading to take once.
async fn served_address(router: &MultiServiceRouter, task_arn: &str, case: &str) -> String {
    let settled = poll_until(
        case,
        "the task to report RUNNING with the private address of its own interface",
        || {
            ok(
                router,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
                case,
            )
        },
        |body| {
            body["tasks"][0]["lastStatus"].as_str() == Some("RUNNING")
                && private_address(&body["tasks"][0]).is_some()
        },
    )
    .await;
    private_address(&settled["tasks"][0])
        .unwrap_or_else(|| panic!("case {case:?}: no private address in {settled}"))
}

/// The `privateIPv4Address` the elastic network interface attachment of `task` reports, and
/// `None` for a task whose attachment carries none yet.
fn private_address(task: &Value) -> Option<String> {
    task["attachments"]
        .as_array()?
        .iter()
        .filter(|attachment| attachment["type"].as_str() == Some("ElasticNetworkInterface"))
        .flat_map(|attachment| attachment["details"].as_array().into_iter().flatten())
        .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
        .and_then(|detail| detail["value"].as_str())
        .map(str::to_string)
}

/// HOW a probing container asks, and so what lines it writes.
///
/// The two shapes a reachability probe is written in: one that reports the DOCUMENT the server
/// served, and one that reports only the STATUS the transfer came to, which is what
/// `-o /dev/null -w '%{http_code}'` writes — the status of a request nothing answered being
/// [`NO_STATUS`]. Each ends with the status the client itself exited with ([`EXIT_LINE`]).
#[derive(Debug, Clone, Copy)]
enum Probe {
    /// The document the server served, where it served one.
    Document,
    /// The status the transfer came to.
    StatusCode,
}

/// The status a transfer that was never answered reports, which is what the client writes for
/// one: curl's own three-digit `000`.
const NO_STATUS: &str = "000";

/// The status a served document's transfer reports.
const SERVED_STATUS: &str = "200";

/// What a probing client's LAST line states: the status the client exited with, which is how a
/// request that was dropped tells itself from one the server answered — and from an invocation
/// the emulator performed nothing of, which would exit with neither of the two.
const EXIT_LINE: &str = "exit=";

/// The status `curl` exits with for a request that was answered, and the one it exits with when
/// the packets went nowhere and it waited for an answer that never came: "28  Operation
/// timeout" (the curl manual's exit codes).
const CURL_ANSWERED: i32 = 0;
const CURL_TIMED_OUT: i32 = 28;

impl Probe {
    /// The program a client probing `address` on `port` runs, in this probe's own shape. Both
    /// end by reporting the status the CLIENT exited with, so what a row asserts is the client's
    /// own account of the request and not merely the presence of a line.
    fn command(self, address: &str, port: i32) -> Vec<String> {
        let line = match self {
            // The client saves the document and the program writes it out, then the status: a
            // request nothing answered saves nothing, so the body line is absent and the status
            // is the one the client waited out.
            Probe::Document => format!(
                ": > /tmp/body; curl -s -o /tmp/body --max-time 5 \
                 http://{address}:{port}/index.html; S=$?; cat /tmp/body; echo {EXIT_LINE}$S"
            ),
            // The shape a probe that cares only whether it got through is written in: the
            // document is discarded and the status the transfer came to is what the client
            // writes.
            Probe::StatusCode => format!(
                "curl -s -o /dev/null -w '%{{http_code}}\\n' --max-time 5 \
                 http://{address}:{port}/index.html; echo {EXIT_LINE}$?"
            ),
        };
        vec!["/bin/sh".to_string(), "-c".to_string(), line]
    }

    /// The lines its program writes when the request ARRIVED, and the lines it writes when the
    /// packet was dropped.
    fn reached(self) -> Vec<String> {
        let answered = format!("{EXIT_LINE}{CURL_ANSWERED}");
        match self {
            Probe::Document => vec![SERVED_BODY.to_string(), answered],
            Probe::StatusCode => vec![SERVED_STATUS.to_string(), answered],
        }
    }

    fn dropped(self) -> Vec<String> {
        let timed_out = format!("{EXIT_LINE}{CURL_TIMED_OUT}");
        match self {
            // Nothing was saved, so the body line is absent altogether.
            Probe::Document => vec![timed_out],
            Probe::StatusCode => vec![NO_STATUS.to_string(), timed_out],
        }
    }
}

/// Launch one PROBING task in `security_group` that asks `address` for the document the server
/// serves, and answer the lines its program wrote — [`Probe::reached`] where the request was
/// answered, and [`Probe::dropped`] where nothing came back.
///
/// The probe is the client's own report: the lines are read back out of the stream the
/// container's `awslogs` driver delivered them to, WAITED for until the client's own exit line
/// is among them rather than read once.
async fn probe(
    router: &MultiServiceRouter,
    network: &Network,
    security_group: &str,
    address: &str,
    port: i32,
    style: Probe,
    case: &str,
) -> Vec<String> {
    let task_arn = run_task(
        router,
        network,
        CLIENT_FAMILY,
        CLIENT_CONTAINER,
        security_group,
        Some(style.command(address, port)),
        case,
    )
    .await;
    let stream = stream_of(CLIENT_CONTAINER, &task_arn);
    let settled = poll_until(
        case,
        "the probing container to deliver what it observed, and the status it exited with, to \
         its own log stream",
        || async {
            json!({ "delivered": delivered(router, &stream).await })
        },
        |body| {
            body["delivered"].as_array().is_some_and(|lines| {
                lines
                    .last()
                    .and_then(|line| line.as_str())
                    .is_some_and(|line| line.starts_with(EXIT_LINE))
            })
        },
    )
    .await;
    settled["delivered"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Stand the whole topology up: the cluster, the network, the log group, the execution role,
/// both revisions and the SERVING task — answered with the network, the address that task is
/// reached at and the group governing it.
async fn serving_topology(
    router: &MultiServiceRouter,
    case: &str,
) -> (Network, String, String) {
    create_cluster(router, CLUSTER, case).await;
    let network = network(router, case).await;
    create_log_group(router, case).await;
    let execution_role = create_execution_role(router, case).await;
    // The server's program writes its document and then runs the interpreter's own HTTP server
    // module over the directory it is in, which is a server that does not return — so the
    // container stays up and the task stays RUNNING.
    let server_command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "mkdir -p {SERVED_ROOT} && echo {SERVED_BODY} > {SERVED_ROOT}/index.html && \
             python3 -m http.server {SERVED_PORT} --directory {SERVED_ROOT}"
        ),
    ];
    register(
        router,
        SERVER_FAMILY,
        SERVER_CONTAINER,
        server_command,
        &execution_role,
        case,
    )
    .await;
    register(
        router,
        CLIENT_FAMILY,
        CLIENT_CONTAINER,
        vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
        &execution_role,
        case,
    )
    .await;
    let server_group = security_group(router, &network.vpc_id, "eni-reachable-server", case).await;
    let server_task = run_task(
        router,
        &network,
        SERVER_FAMILY,
        SERVER_CONTAINER,
        &server_group,
        None,
        case,
    )
    .await;
    let address = served_address(router, &server_task, case).await;
    (network, address, server_group)
}

/// RULE (the whole of it, in the one order a client changes the rules in): a client reaches the
/// task at its own interface's address while a rule of the task's security group names that
/// client, stops reaching it the moment that rule is revoked — while another client a rule
/// still names keeps getting through — and reaches it again once the rule is authorized anew.
#[tokio::test]
async fn an_awsvpc_task_answers_on_its_eni_address_only_while_its_security_group_admits_the_client()
{
    let case = "two clients of one served task, one of them revoked";
    let router = emulator();
    let (network, address, server_group) = serving_topology(&router, case).await;
    let first = security_group(&router, &network.vpc_id, "eni-reachable-client-one", case).await;
    let second = security_group(&router, &network.vpc_id, "eni-reachable-client-two", case).await;
    // Both clients are named by a rule of the served task's group, each by its own group.
    for client in [&first, &second] {
        change_ingress(
            &router,
            "AuthorizeSecurityGroupIngress",
            &server_group,
            SERVED_PORT,
            &Source::Group(client),
            case,
        )
        .await;
    }
    for (client, which, style) in [
        (&first, "the first client", Probe::Document),
        (&second, "the second client", Probe::StatusCode),
    ] {
        let observed = probe(
            &router,
            &network,
            client,
            &address,
            SERVED_PORT,
            style,
            case,
        )
        .await;
        assert_eq!(
            observed,
            style.reached(),
            "case {case:?}: {which} is named by a rule of the task's group, so its {style:?} \
             probe must be answered by the task, got {observed:?}"
        );
    }
    // The rule naming the FIRST client is revoked; the one naming the second is untouched.
    change_ingress(
        &router,
        "RevokeSecurityGroupIngress",
        &server_group,
        SERVED_PORT,
        &Source::Group(&first),
        case,
    )
    .await;
    let revoked = probe(
        &router,
        &network,
        &first,
        &address,
        SERVED_PORT,
        Probe::Document,
        case,
    )
    .await;
    assert_eq!(
        revoked,
        Probe::Document.dropped(),
        "case {case:?}: no rule of the task's group names the first client any more, so its \
         request must go unanswered, got {revoked:?}"
    );
    // The still-authorized client's probe is the OTHER shape, so the contrast holds however the
    // probe is written.
    let still_admitted = probe(
        &router,
        &network,
        &second,
        &address,
        SERVED_PORT,
        Probe::StatusCode,
        case,
    )
    .await;
    assert_eq!(
        still_admitted,
        Probe::StatusCode.reached(),
        "case {case:?}: the rule naming the second client was not revoked, so that client must \
         still be answered, got {still_admitted:?}"
    );
    // And authorizing it again lets the first client back in, with nothing of the task changed.
    change_ingress(
        &router,
        "AuthorizeSecurityGroupIngress",
        &server_group,
        SERVED_PORT,
        &Source::Group(&first),
        case,
    )
    .await;
    let repaired = probe(
        &router,
        &network,
        &first,
        &address,
        SERVED_PORT,
        Probe::Document,
        case,
    )
    .await;
    assert_eq!(
        repaired,
        Probe::Document.reached(),
        "case {case:?}: the rule naming the first client was authorized again, so that client \
         must be answered once more, got {repaired:?}"
    );
}

/// RULE, over the other two things a rule names: the SOURCE may be an address range rather than
/// a security group, and the PORT a rule names is the only port it admits — a rule on another
/// port leaves the served one unreachable.
#[tokio::test]
async fn a_rule_admits_the_source_range_and_the_port_it_names_and_nothing_else() {
    let case = "a rule naming an address range, and a rule naming another port";
    let router = emulator();
    let (network, address, server_group) = serving_topology(&router, case).await;
    let client = security_group(&router, &network.vpc_id, "eni-reachable-client", case).await;
    // A rule naming the range the client's own address lies in, on a port the server does NOT
    // listen on: the client is admitted there and nowhere else.
    change_ingress(
        &router,
        "AuthorizeSecurityGroupIngress",
        &server_group,
        SERVED_PORT + 1,
        &Source::Range(SUBNET_CIDR),
        case,
    )
    .await;
    let wrong_port = probe(
        &router,
        &network,
        &client,
        &address,
        SERVED_PORT,
        Probe::StatusCode,
        case,
    )
    .await;
    assert_eq!(
        wrong_port,
        Probe::StatusCode.dropped(),
        "case {case:?}: the only rule of the task's group names another port, so a request to \
         the served port must go unanswered, got {wrong_port:?}"
    );
    // The same source range on the port the server listens on admits it.
    change_ingress(
        &router,
        "AuthorizeSecurityGroupIngress",
        &server_group,
        SERVED_PORT,
        &Source::Range(SUBNET_CIDR),
        case,
    )
    .await;
    let served = probe(
        &router,
        &network,
        &client,
        &address,
        SERVED_PORT,
        Probe::StatusCode,
        case,
    )
    .await;
    assert_eq!(
        served,
        Probe::StatusCode.reached(),
        "case {case:?}: a rule naming the range the client's address lies in admits it on the \
         port it names, so the client must be answered by the task, got {served:?}"
    );
}
