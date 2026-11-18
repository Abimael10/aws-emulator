// SPDX-License-Identifier: CC0-1.0

//! An Application Load Balancer forwarding to an `instance` target answers with the TARGET's
//! own response — the status and the bytes the server inside that instance's guest serves.
//!
//! The rule under test: **a request a client sends to a load balancer's published address and
//! which a `forward` action sends to a healthy target is answered with that target's own
//! status, headers and body; a status the load balancer generates itself (its 502) appears
//! only when the target cannot be reached.** The target is reached at the address its
//! registration names — the private address EC2 holds for an `instance` target
//! (`TargetTypeEnum`: "instance - Targets are specified by instance ID") — on the port the
//! registration receives traffic on (`TargetDescription$Port`).
//!
//! What stands behind the registration is the instance's GUEST, and the server in it exists
//! because the instance's own user data started one: "the user data to make available to the
//! instance" (`RunInstancesRequest$UserData`) is a program the guest RUNS at boot, so a
//! program that installs a web server and starts its unit — or runs a server module directly
//! — leaves a server listening, serving the files that same program wrote. Both halves are
//! driven here through the two services' own APIs, which is why this suite lives in the router
//! crate: the instance is EC2's resource and the load balancer is ELBv2's, and the answer the
//! client gets has to come from the first through the second.
//!
//! The rows span what the rule generalises over: the two program families that start a server
//! (a package's unit started by the init system, and the interpreter's own server module), the
//! port each listens on (the package's documented 80 and a port the program names), a path the
//! guest holds no file at (the TARGET's own 404, not the load balancer's), and a guest whose
//! program started no server at all (the load balancer's own 502, because nothing is
//! listening).
//!
//! The clock-owed moves (the instance to `running`, the load balancer to `active`, the
//! registration to `healthy`) are polled for until they hold, never slept for; every row's
//! answer is taken BOTH over the request this emulator carries in process and over a TCP
//! connection dialled to the `host:port` the load balancer published, which is what a client
//! handed that address does. Assembling the whole router costs stack, so the suite runs on a
//! sized thread, exactly as `elbv2_network_resolution.rs` does — whose wire plumbing it reuses
//! rather than restating.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{
    attach_internet_gateway, ec2, ec2_ok, elbv2, BOOT_STACK_SIZE,
};
use crate::query_wire::element;

/// An AMI id the launches name; this suite asserts nothing about the image.
const AMI: &str = "ami-0123456789abcdef0";

/// The body the guest's own program writes and its server then serves. A marker, so a body
/// anything but that program produced fails the row.
const MARKER: &str = "GUEST-SERVED-BODY";

/// How long a clock-owed move is waited for before the row gives up: generous, because what is
/// under test is that the move HAPPENS, not how fast this host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One row: the user data the instance is launched with, the port its server listens on, the
/// path the client asks for, and the status and body the client must be answered with.
struct Row {
    case: &'static str,
    /// The guest's own program, as a user-data script.
    user_data: &'static str,
    /// The `Type` the load balancer is created with, and the `Protocol` its listener and its
    /// target group carry: an Application Load Balancer answers the client's own request from
    /// its listener's rules, and a Network Load Balancer hands the connection on to what its
    /// target group selects — the answer is the target's own either way.
    lb_type: &'static str,
    protocol: &'static str,
    /// The port the registration receives traffic on.
    port: &'static str,
    /// What the client asks the load balancer for.
    path: &'static str,
    /// The status the client must be answered with.
    status: u16,
    /// The body it must carry, trimmed of the newline a shell's own `echo` writes.
    body: &'static str,
}

/// The rows, in the order they run.
const ROWS: &[Row] = &[
    Row {
        case: "a package's unit started by the init system serves its document root",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                    echo GUEST-SERVED-BODY > /var/www/html/index.html\n\
                    systemctl enable --now httpd\n",
        lb_type: "application",
        protocol: "HTTP",
        port: "80",
        path: "/",
        status: 200,
        body: MARKER,
    },
    Row {
        case: "the same guest answers its own 404 for a path it holds no file at",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                    echo GUEST-SERVED-BODY > /var/www/html/index.html\n\
                    systemctl start httpd\n",
        lb_type: "application",
        protocol: "HTTP",
        port: "80",
        path: "/nothing-here",
        status: 404,
        body: "",
    },
    Row {
        case: "the interpreter's own server module on the port and directory it names",
        user_data: "#!/bin/bash\nmkdir -p /srv/app\n\
                    echo GUEST-SERVED-BODY > /srv/app/index.html\n\
                    python3 -m http.server 8080 --directory /srv/app\n",
        lb_type: "application",
        protocol: "HTTP",
        port: "8080",
        path: "/",
        status: 200,
        body: MARKER,
    },
    Row {
        case: "a network load balancer's listener hands the connection to the same guest",
        user_data: "#!/bin/bash\nmkdir -p /usr/share/nginx/html\nyum install -y nginx\n\
                    echo GUEST-SERVED-BODY > /usr/share/nginx/html/index.html\n\
                    systemctl start nginx\n",
        lb_type: "network",
        protocol: "TCP",
        port: "80",
        path: "/",
        status: 200,
        body: MARKER,
    },
    Row {
        case: "a guest whose program started no server is a target nothing answers at",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\n\
                    echo GUEST-SERVED-BODY > /var/www/html/index.html\n",
        lb_type: "application",
        protocol: "HTTP",
        port: "80",
        path: "/",
        status: 502,
        body: "",
    },
];

/// One answered data-plane request: the status and the body, as the wire carries them.
struct Answered {
    status: u16,
    body: String,
}

/// Send one request to the address the load balancer published, carrying no AWS credential —
/// exactly as a client handed that name sends it.
fn get(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    dns_name: &str,
    path: &str,
) -> Answered {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{dns_name}{path}"))
        .header("host", dns_name)
        .body(bytes::Bytes::new())
        .expect("a well-formed data-plane request");
    let response = runtime.block_on(router.dispatch(request));
    Answered {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// How long one exchange over a dialled connection is given — generous, because what is under
/// test is WHAT is answered, not how fast this host answers it.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(60);

/// DIAL the address the load balancer published and exchange one request on it, as a client
/// handed that address does: a TCP connection in the clear, HTTP/1.1, closed after the answer.
///
/// Only the PORT is taken from the published authority: the core binds its listeners on
/// loopback, so the dial is to this machine.
fn dialed(
    runtime: &tokio::runtime::Runtime,
    case: &str,
    dns_name: &str,
    path: &str,
) -> Answered {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let port = dns_name
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the published address {dns_name:?} carries no port to dial")
        });
    runtime.block_on(async move {
        let mut stream = tokio::time::timeout(
            EXCHANGE_DEADLINE,
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
        )
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: {dns_name} did not accept a connection"))
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {dns_name} refused a connection: {error}")
        });
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {dns_name}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
        let mut answer = Vec::new();
        tokio::time::timeout(EXCHANGE_DEADLINE, stream.read_to_end(&mut answer))
            .await
            .unwrap_or_else(|_| panic!("case {case:?}: {dns_name} did not answer in time"))
            .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
        let raw = String::from_utf8_lossy(&answer).to_string();
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: no status line in {raw:?}"));
        let body = raw
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        Answered { status, body }
    })
}

/// Look again until `holds` answers `Some`, or the deadline passes — what a client waiting for
/// a clock-owed move does.
fn until<T>(case: &str, what: &str, mut holds: impl FnMut() -> Option<T>) -> T {
    let began = Instant::now();
    let mut waited = 0_u32;
    loop {
        if let Some(held) = holds() {
            return held;
        }
        waited += 1;
        assert!(
            began.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: {what} did not happen before the deadline (looked {waited} times)"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[test]
fn a_load_balancer_answers_with_its_instance_targets_own_response() {
    std::thread::Builder::new()
        .name("elbv2-instance-target-response".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // The network every row runs in, created through EC2 itself: one VPC with two
            // subnets, and the internet gateway an internet-facing load balancer needs.
            let vpc = element(
                &ec2_ok(
                    &router,
                    &runtime,
                    "CreateVpc",
                    &[("CidrBlock", "10.70.0.0/16")],
                ),
                "vpcId",
            );
            let mut subnets: Vec<String> = Vec::new();
            for (cidr, zone) in [("10.70.1.0/24", "us-west-2a"), ("10.70.2.0/24", "us-west-2b")] {
                subnets.push(element(
                    &ec2_ok(
                        &router,
                        &runtime,
                        "CreateSubnet",
                        &[
                            ("VpcId", vpc.as_str()),
                            ("CidrBlock", cidr),
                            ("AvailabilityZone", zone),
                        ],
                    ),
                    "subnetId",
                ));
            }
            attach_internet_gateway(&router, &runtime, &vpc);
            // The security group the instances are launched with, admitting the ports their
            // servers listen on from the VPC's own range — the addresses the load balancer
            // nodes' health checks and traffic come from. A load balancer with no security
            // group of its own (a Network Load Balancer created without one) is named by no
            // rule that names a source group, so the range is what admits it.
            let security_group = element(
                &ec2_ok(
                    &router,
                    &runtime,
                    "CreateSecurityGroup",
                    &[
                        ("GroupName", "guest-target-servers"),
                        ("GroupDescription", "the ports the guests serve on"),
                        ("VpcId", vpc.as_str()),
                    ],
                ),
                "groupId",
            );
            for port in ["80", "8080"] {
                ec2_ok(
                    &router,
                    &runtime,
                    "AuthorizeSecurityGroupIngress",
                    &[
                        ("GroupId", security_group.as_str()),
                        ("IpPermissions.1.IpProtocol", "tcp"),
                        ("IpPermissions.1.FromPort", port),
                        ("IpPermissions.1.ToPort", port),
                        ("IpPermissions.1.IpRanges.1.CidrIp", "10.70.0.0/16"),
                    ],
                );
            }

            for (index, row) in ROWS.iter().enumerate() {
                // The instance, launched with the guest's own program as its user data — the
                // API carries it base64-encoded.
                let user_data = aws_smithy_types::base64::encode(row.user_data);
                let instance = element(
                    &ec2_ok(
                        &router,
                        &runtime,
                        "RunInstances",
                        &[
                            ("ImageId", AMI),
                            ("MinCount", "1"),
                            ("MaxCount", "1"),
                            ("SubnetId", subnets[0].as_str()),
                            ("SecurityGroupId.1", security_group.as_str()),
                            ("UserData", user_data.as_str()),
                        ],
                    ),
                    "instanceId",
                );
                // Its guest comes up on the clock: the launch leaves the instance `pending`,
                // and the boot that runs the program happens once it is `running`.
                until(row.case, "the instance reached running", || {
                    let described = ec2(
                        &router,
                        &runtime,
                        "DescribeInstances",
                        &[("InstanceId.1", instance.as_str())],
                    );
                    described
                        .text
                        .contains("<name>running</name>")
                        .then_some(())
                });

                // The target group, and the registration of that instance on the port its
                // server listens on. A health check of an HTTP target group names the path it
                // asks for; a TCP one has none to name.
                let group_name = format!("guest-tg-{index}");
                let mut group_params: Vec<(&str, &str)> = vec![
                    ("Name", group_name.as_str()),
                    ("Protocol", row.protocol),
                    ("Port", row.port),
                    ("VpcId", vpc.as_str()),
                    ("TargetType", "instance"),
                    ("HealthCheckProtocol", row.protocol),
                ];
                if row.protocol == "HTTP" {
                    group_params.push(("HealthCheckPath", "/"));
                } else {
                    // A Network Load Balancer's target group is checked on the cadence and
                    // the thresholds the caller configures, and the two thresholds of such a
                    // group are equal; these are the shortest this suite needs to see the
                    // verdict the checks reach.
                    group_params.push(("HealthCheckIntervalSeconds", "10"));
                    group_params.push(("HealthyThresholdCount", "2"));
                    group_params.push(("UnhealthyThresholdCount", "2"));
                }
                let group = element(
                    &must(
                        row.case,
                        elbv2(&router, &runtime, "CreateTargetGroup", &group_params),
                    ),
                    "TargetGroupArn",
                );
                must(
                    row.case,
                    elbv2(
                        &router,
                        &runtime,
                        "RegisterTargets",
                        &[
                            ("TargetGroupArn", group.as_str()),
                            ("Targets.member.1.Id", instance.as_str()),
                            ("Targets.member.1.Port", row.port),
                        ],
                    ),
                );

                // The load balancer and the listener that forwards to that group.
                let load_balancer_name = format!("guest-lb-{index}");
                let created = must(
                    row.case,
                    elbv2(
                        &router,
                        &runtime,
                        "CreateLoadBalancer",
                        &[
                            ("Name", load_balancer_name.as_str()),
                            ("Type", row.lb_type),
                            ("Subnets.member.1", subnets[0].as_str()),
                            ("Subnets.member.2", subnets[1].as_str()),
                        ],
                    ),
                );
                let load_balancer = element(&created, "LoadBalancerArn");
                let dns_name = element(&created, "DNSName");
                must(
                    row.case,
                    elbv2(
                        &router,
                        &runtime,
                        "CreateListener",
                        &[
                            ("LoadBalancerArn", load_balancer.as_str()),
                            ("Protocol", row.protocol),
                            ("Port", "80"),
                            ("DefaultActions.member.1.Type", "forward"),
                            ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
                        ],
                    ),
                );

                // The load balancer serves once it is active, and it forwards once the
                // registration is healthy: both are clock-owed, so both are waited for.
                until(row.case, "the load balancer reached active", || {
                    let described = elbv2(
                        &router,
                        &runtime,
                        "DescribeLoadBalancers",
                        &[("LoadBalancerArns.member.1", load_balancer.as_str())],
                    );
                    described
                        .text
                        .contains("<Code>active</Code>")
                        .then_some(())
                });
                let began = Instant::now();
                loop {
                    let health = elbv2(
                        &router,
                        &runtime,
                        "DescribeTargetHealth",
                        &[("TargetGroupArn", group.as_str())],
                    );
                    if health.text.contains("<State>healthy</State>") {
                        break;
                    }
                    assert!(
                        began.elapsed() < SETTLE_DEADLINE,
                        "case {:?}: the registration never became healthy; last read {}",
                        row.case,
                        health.text
                    );
                    std::thread::sleep(POLL_INTERVAL);
                }
                // And the answer the client gets, over BOTH the ways a client reaches the
                // address this load balancer published: the request this emulator carries to
                // it in process, and a TCP connection DIALLED to the `host:port` it published
                // — which is what a client handed that address does, and the only way the
                // socket its listeners are served on is exercised.
                for (how, answered) in [
                    ("carried in process", get(&router, &runtime, &dns_name, row.path)),
                    ("dialled over TCP", dialed(&runtime, row.case, &dns_name, row.path)),
                ] {
                    assert_eq!(
                        answered.status, row.status,
                        "case {:?} ({how}): expected HTTP {}, got {} body {:?}",
                        row.case, row.status, answered.status, answered.body
                    );
                    assert_eq!(
                        answered.body.trim(),
                        row.body,
                        "case {:?} ({how}): expected the body {:?}, got {:?}",
                        row.case,
                        row.body,
                        answered.body
                    );
                }
            }
        })
        .expect("the sized thread")
        .join()
        .expect("the suite ran");
}

/// One control-plane call that must be served, answering its body.
fn must(case: &str, answer: crate::elbv2_network_resolution::Answer) -> String {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: setup call must succeed, got {answer:?}"
    );
    answer.text
}
