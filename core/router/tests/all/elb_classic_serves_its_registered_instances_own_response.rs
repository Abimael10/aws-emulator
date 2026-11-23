// SPDX-License-Identifier: CC0-1.0

//! A Classic load balancer answers a client with the REGISTERED INSTANCE'S OWN response — the
//! status, the media type and the bytes the server inside that instance's guest serves.
//!
//! The rule under test: **a request the load balancer selected a registered instance for is
//! sent to that instance on the port its listener forwards traffic to, and the client is
//! answered with that instance's own status and body — a status the instance itself denies the
//! request with included; the load balancer's own statuses appear only where it produced no
//! backend answer at all: the one it words for having no instance to send the request to, and
//! the one it words for a selected instance whose connection cannot be established.**
//!
//! `Listener$InstancePort` is "the port on which the instance is listening" and
//! `Listener$InstanceProtocol` "the protocol to use for routing traffic to instances", so what
//! answers is the server the instance RUNS there: "the user data to make available to the
//! instance" (`RunInstancesRequest$UserData`) is a program the guest runs at boot, so a program
//! that installs a web server and starts its unit — or runs a server module directly — leaves a
//! server listening and serving the files that same program wrote. Which instances may be sent
//! the request at all is the outcome of this load balancer's OWN health checks, so each row
//! waits for `DescribeInstanceHealth` to report the triple its backend's checks earn — the
//! service's own constants, not sentences retyped here.
//!
//! Both halves are driven through the two services' own APIs — the instance is Amazon EC2's
//! resource and the load balancer is Elastic Load Balancing's — which is why this suite lives in
//! the router crate.
//!
//! The rows span what the rule generalises over: the program families that start a server (a
//! package's unit started by the init system, the interpreter's own server module, the same
//! module left running in the background, and a `#cloud-config` document that writes the files
//! and runs the program), the port each listens on (the package's documented 80 and a port the
//! program names), the ping path the health check asks for, the request METHOD (the one that
//! asks for a document, the one that asks for everything but its body, and one the server serves
//! no document for), a path the guest holds no file at (the INSTANCE's own 404), a backend in
//! service whose listener port nothing listens on (the load balancer's own gateway-timeout
//! status) and a backend whose checks fail, so the load balancer has nothing to send the request
//! to (its own no-capacity status).
//!
//! Every row's answer is taken BOTH over the request this emulator carries in process to the
//! published name and over a TCP connection DIALLED to the `host:port` that name is — which is
//! what a client handed that address does, and the only way the socket a load balancer's
//! listeners are served on is exercised. The two sibling suites below read the same answer for
//! the two rules that decide WHICH registered instance a request is sent to and are only
//! observable once the instance's own body comes back: the `AWSELB` pin a duration-based policy
//! mints, and the zone a node serves while `CrossZoneLoadBalancing` is off. Every clock-owed move
//! (the instance to `running`, the run of health checks behind a verdict) is polled for until it
//! holds, never slept for. Assembling the whole router costs stack, so each test runs on a sized
//! thread, exactly as `elbv2_serves_an_instance_targets_own_response.rs` does — whose wire
//! plumbing this suite reuses rather than restating.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::state::{
    InstanceHealth, HEALTH_FAILED_HEALTH_CHECKS, HEALTH_IN_SERVICE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{
    attach_internet_gateway, ec2, ec2_ok, form, send, Answer, BOOT_STACK_SIZE, REGION,
};
use crate::query_wire::{element, elements};

/// The API version an awsQuery request naming the Classic model carries, which is what the
/// shared Elastic Load Balancing endpoint dispatches on.
const CLASSIC_VERSION: &str = "2012-06-01";

/// An AMI id the launches name; this suite asserts nothing about the image.
const AMI: &str = "ami-0123456789abcdef0";

/// The body each guest's own program writes and its server then serves. A marker per backend,
/// so a body anything but that program produced fails the row — and so the instance that
/// answered is READ off the answer.
const MARKER_A: &str = "GUEST-A-BODY";
const MARKER_B: &str = "GUEST-B-BODY";
const MARKER_C: &str = "GUEST-C-BODY";
const MARKER_D: &str = "GUEST-D-BODY";
const MARKER_E: &str = "GUEST-E-BODY";
const MARKER_F: &str = "GUEST-F-BODY";

/// The port every listener below is reached on.
const LISTENER_PORT: &str = "80";

/// The status a load balancer with no registered instance it may send the request to answers,
/// from the Classic Load Balancer's own `HTTP 503: Service unavailable`.
const NO_INSTANCE: u16 = 503;

/// The status it answers once it HAS selected a registered instance and the connection to that
/// instance cannot be established, from its own `HTTP 504: Gateway Timeout`.
const INSTANCE_UNREACHABLE: u16 = 504;

/// How long a clock-owed move is waited for before a row gives up: generous, because what is
/// under test is that the move HAPPENS, not how fast this host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long one exchange over a dialled connection is given — generous for the same reason.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(60);

/// One backend a row's load balancer registers: the program its guest runs, the health check
/// that program's server is checked by, and the verdict those checks reach.
struct Backend {
    /// What this backend is, as a report names it.
    case: &'static str,
    /// The guest's own program, as a user-data script.
    user_data: &'static str,
    /// The `HealthCheck$Target` the load balancer checks it with.
    target: &'static str,
    /// The port the listener forwards traffic to on it — `Listener$InstancePort`.
    instance_port: &'static str,
    /// The health `DescribeInstanceHealth` must report for it before the row sends traffic.
    settles_at: InstanceHealth,
}

/// The backends, each launched once and registered with one load balancer of its own.
const BACKENDS: &[Backend] = &[
    Backend {
        case: "a package's unit started by the init system",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                    echo GUEST-A-BODY > /var/www/html/index.html\n\
                    systemctl enable --now httpd\n",
        target: "HTTP:80/index.html",
        instance_port: "80",
        settles_at: HEALTH_IN_SERVICE,
    },
    Backend {
        case: "the interpreter's own server module on the port and directory it names",
        user_data: "#!/bin/bash\nmkdir -p /srv/app\n\
                    echo GUEST-B-BODY > /srv/app/ok\n\
                    python3 -m http.server 8080 --directory /srv/app\n",
        target: "HTTP:8080/ok",
        instance_port: "8080",
        settles_at: HEALTH_IN_SERVICE,
    },
    Backend {
        case: "a backend serving the port its checks name and nothing on its listener's",
        user_data: "#!/bin/bash\nmkdir -p /srv/probe\n\
                    echo GUEST-C-BODY > /srv/probe/ping\n\
                    python3 -m http.server 9000 --directory /srv/probe\n",
        target: "HTTP:9000/ping",
        instance_port: "8080",
        settles_at: HEALTH_IN_SERVICE,
    },
    Backend {
        case: "a guest whose program started no server at all",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\n\
                    echo GUEST-A-BODY > /var/www/html/index.html\n",
        target: "HTTP:80/index.html",
        instance_port: "80",
        settles_at: HEALTH_FAILED_HEALTH_CHECKS,
    },
    Backend {
        case: "a package's server checked on the ping path its own program wrote",
        user_data: "#!/bin/bash -xe\nyum update -y\nyum install -y httpd\n\
                    echo GUEST-D-BODY > /var/www/html/ping\n\
                    systemctl enable httpd\nsystemctl start httpd\n",
        target: "HTTP:80/ping",
        instance_port: "80",
        settles_at: HEALTH_IN_SERVICE,
    },
    Backend {
        case: "the server module left running in the background by the program that started it",
        user_data: "#!/bin/bash\nmkdir -p /srv/site\n\
                    echo GUEST-E-BODY > /srv/site/ok\n\
                    nohup python3 -m http.server 80 --directory /srv/site \
                    > /var/log/server.log 2>&1 &\n",
        target: "HTTP:80/ok",
        instance_port: "80",
        settles_at: HEALTH_IN_SERVICE,
    },
    Backend {
        case: "a cloud-config document that writes the document and runs the server",
        user_data: "#cloud-config\nwrite_files:\n  - path: /srv/pages/ping\n    \
                    content: |\n      GUEST-F-BODY\nruncmd:\n  - \
                    python3 -m http.server 8080 --directory /srv/pages\n",
        target: "HTTP:8080/ping",
        instance_port: "8080",
        settles_at: HEALTH_IN_SERVICE,
    },
];

/// One row: the request a client makes of one backend's load balancer, and the answer it must
/// get back.
struct Row {
    case: &'static str,
    /// The backend of [`BACKENDS`] whose load balancer the request is sent to.
    backend: usize,
    method: &'static str,
    path: &'static str,
    status: u16,
    /// The body the answer must carry, trimmed of the newline a shell's own `echo` writes.
    body: &'static str,
    /// Whether the answer must carry a media type: the INSTANCE states one for the document it
    /// served, and a status the load balancer produced itself carries no document at all.
    served_document: bool,
}

/// The rows, in the order they run.
const ROWS: &[Row] = &[
    Row {
        case: "the package's own document root answers the root path",
        backend: 0,
        method: "GET",
        path: "/",
        status: 200,
        body: MARKER_A,
        served_document: true,
    },
    Row {
        case: "the same instance answers its OWN not-found for a path it holds no file at",
        backend: 0,
        method: "GET",
        path: "/nothing-here",
        status: 404,
        body: "",
        served_document: false,
    },
    Row {
        case: "a HEAD asks the same instance for everything about the document but its body",
        backend: 0,
        method: "HEAD",
        path: "/index.html",
        status: 200,
        body: "",
        served_document: true,
    },
    Row {
        case: "the instance's own refusal of a method it serves no document for",
        backend: 0,
        method: "POST",
        path: "/",
        status: 405,
        body: "",
        served_document: false,
    },
    Row {
        case: "the server module answers on the port the listener forwards traffic to",
        backend: 1,
        method: "GET",
        path: "/ok",
        status: 200,
        body: MARKER_B,
        served_document: true,
    },
    Row {
        case: "an in-service instance listening on nothing at its listener's port",
        backend: 2,
        method: "GET",
        path: "/ping",
        status: INSTANCE_UNREACHABLE,
        body: "",
        served_document: false,
    },
    Row {
        case: "a load balancer whose only registration failed its checks",
        backend: 3,
        method: "GET",
        path: "/",
        status: NO_INSTANCE,
        body: "",
        served_document: false,
    },
    Row {
        case: "the package's server answers the ping path its own program wrote",
        backend: 4,
        method: "GET",
        path: "/ping",
        status: 200,
        body: MARKER_D,
        served_document: true,
    },
    Row {
        case: "the backgrounded server module answers the document it serves",
        backend: 5,
        method: "GET",
        path: "/ok",
        status: 200,
        body: MARKER_E,
        served_document: true,
    },
    Row {
        case: "the cloud-config document's own server answers the file it wrote",
        backend: 6,
        method: "GET",
        path: "/ping",
        status: 200,
        body: MARKER_F,
        served_document: true,
    },
];

/// One answered data-plane request: the status, the body, and the two response fields a row
/// reads — the media type of the document the instance served, and the stickiness cookie the
/// load balancer minted.
#[derive(Debug)]
struct Answered {
    status: u16,
    body: String,
    content_type: Option<String>,
    set_cookie: Option<String>,
}

impl Answered {
    /// The `AWSELB` value this answer minted, or `None` when it minted no cookie.
    fn minted_pin(&self) -> Option<String> {
        let field = self.set_cookie.as_deref()?;
        let value = field.split(';').next()?.trim().strip_prefix("AWSELB=")?;
        Some(value.to_string())
    }
}

/// Drive one awsQuery action against the Classic model.
fn elb(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, &str)],
) -> Answer {
    send(
        router,
        runtime,
        "elasticloadbalancing",
        form(action, CLASSIC_VERSION, params),
    )
}

/// The same call, asserted to have been served, answering its body.
fn elb_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let answer = elb(router, runtime, action, params);
    assert_eq!(
        answer.status, 200,
        "case {case:?}: {action} must be served, got {answer:?}"
    );
    answer.text
}

/// Send one request to the address the load balancer published, carrying no AWS credential —
/// exactly as a client handed that name sends it.
fn carried(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    dns_name: &str,
    method: &str,
    path: &str,
    cookie: Option<&str>,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://{dns_name}{path}"))
        .header("host", dns_name);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let request = builder
        .body(bytes::Bytes::new())
        .expect("a well-formed data-plane request");
    let response = runtime.block_on(router.dispatch(request));
    let field = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    Answered {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
        content_type: field("content-type"),
        set_cookie: field("set-cookie"),
    }
}

/// DIAL the address the load balancer published and exchange one request on it, as a client
/// handed that address does: a TCP connection in the clear, HTTP/1.1, closed after the answer.
///
/// Only the PORT is taken from the published authority: the core binds its listeners on
/// loopback, so the dial is to this machine.
fn dialled(
    runtime: &tokio::runtime::Runtime,
    case: &str,
    dns_name: &str,
    method: &str,
    path: &str,
    cookie: Option<&str>,
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
        .unwrap_or_else(|error| panic!("case {case:?}: {dns_name} refused a connection: {error}"));
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {dns_name}\r\nAccept: */*\r\nConnection: close\r\n"
        );
        if let Some(cookie) = cookie {
            request.push_str(&format!("Cookie: {cookie}\r\n"));
        }
        request.push_str("\r\n");
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
        let (head, body) = raw
            .split_once("\r\n\r\n")
            .map(|(head, body)| (head.to_string(), body.to_string()))
            .unwrap_or((raw.clone(), String::new()));
        let field = |wanted: &str| {
            head.split("\r\n")
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
                .map(|(_, value)| value.trim().to_string())
        };
        Answered {
            status,
            body,
            content_type: field("content-type"),
            set_cookie: field("set-cookie"),
        }
    })
}

/// Look again until `holds` answers `Some`, or the deadline passes — what a client waiting for
/// a clock-owed move does.
fn until<T>(case: &str, what: &str, mut holds: impl FnMut() -> Option<T>) -> T {
    let began = Instant::now();
    let mut looked = 0_u32;
    loop {
        if let Some(held) = holds() {
            return held;
        }
        looked += 1;
        assert!(
            began.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: {what} did not happen before the deadline (looked {looked} times)"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The network every load balancer and instance below is placed in.
struct Network {
    /// The subnets of one VPC, in the order created.
    subnets: Vec<String>,
    security_group: String,
}

/// Create that network through EC2 itself, so every placement below is one EC2 reported.
fn seed_network(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    cidr: &str,
    subnets_in: &[(&str, &str)],
) -> Network {
    let vpc = element(
        &ec2_ok(router, runtime, "CreateVpc", &[("CidrBlock", cidr)]),
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (subnet_cidr, zone) in subnets_in {
        subnets.push(element(
            &ec2_ok(
                router,
                runtime,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", subnet_cidr),
                    ("AvailabilityZone", zone),
                ],
            ),
            "subnetId",
        ));
    }
    // An internet-facing load balancer — the scheme `CreateLoadBalancer` documents as its
    // default — belongs in a VPC that has an internet gateway.
    attach_internet_gateway(router, runtime, &vpc);
    let security_group = element(
        &ec2_ok(
            router,
            runtime,
            "CreateSecurityGroup",
            &[
                ("GroupName", "classic-guest-servers"),
                ("GroupDescription", "the ports the guests serve on"),
                ("VpcId", vpc.as_str()),
            ],
        ),
        "groupId",
    );
    for port in ["80", "8080", "9000"] {
        ec2_ok(
            router,
            runtime,
            "AuthorizeSecurityGroupIngress",
            &[
                ("GroupId", security_group.as_str()),
                ("IpPermissions.1.IpProtocol", "tcp"),
                ("IpPermissions.1.FromPort", port),
                ("IpPermissions.1.ToPort", port),
                ("IpPermissions.1.IpRanges.1.CidrIp", cidr),
            ],
        );
    }
    Network {
        subnets,
        security_group,
    }
}

/// Launch one instance carrying `user_data` as the program its guest runs, and answer the id
/// EC2 minted for it.
fn launch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    network: &Network,
    subnet: usize,
    user_data: &str,
) -> String {
    let encoded = aws_smithy_types::base64::encode(user_data);
    element(
        &ec2_ok(
            router,
            runtime,
            "RunInstances",
            &[
                ("ImageId", AMI),
                ("MinCount", "1"),
                ("MaxCount", "1"),
                ("SubnetId", network.subnets[subnet].as_str()),
                ("SecurityGroupId.1", network.security_group.as_str()),
                ("UserData", encoded.as_str()),
            ],
        ),
        "instanceId",
    )
}

/// Wait until EC2 reports `instance_id` `running`, which is when the boot that runs its own
/// program has happened.
fn until_running(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) {
    until(case, "the instance reached running", || {
        ec2(
            router,
            runtime,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        )
        .text
        .contains("<name>running</name>")
        .then_some(())
    });
}

/// One load balancer this suite creates: its name, the port its listener forwards traffic to,
/// and the health check target it checks its registrations with.
struct Placed<'a> {
    name: &'a str,
    instance_port: &'a str,
    target: &'a str,
}

/// Create one Classic load balancer with one HTTP listener, configure the health check its
/// target describes, register `instances` with it, and answer the address it published.
fn load_balancer(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    network: &Network,
    case: &str,
    placed: &Placed<'_>,
    instances: &[String],
) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("LoadBalancerName", placed.name),
        ("Scheme", "internet-facing"),
        ("Listeners.member.1.Protocol", "HTTP"),
        ("Listeners.member.1.LoadBalancerPort", LISTENER_PORT),
        ("Listeners.member.1.InstanceProtocol", "HTTP"),
        ("Listeners.member.1.InstancePort", placed.instance_port),
        ("SecurityGroups.member.1", network.security_group.as_str()),
    ];
    let subnet_keys = ["Subnets.member.1", "Subnets.member.2"];
    for (key, subnet) in subnet_keys.iter().zip(&network.subnets) {
        params.push((key, subnet.as_str()));
    }
    let created = elb_ok(router, runtime, case, "CreateLoadBalancer", &params);
    let dns_name = element(&created, "DNSName");
    elb_ok(
        router,
        runtime,
        case,
        "ConfigureHealthCheck",
        &[
            ("LoadBalancerName", placed.name),
            ("HealthCheck.Target", placed.target),
            ("HealthCheck.Interval", "5"),
            ("HealthCheck.Timeout", "2"),
            ("HealthCheck.HealthyThreshold", "2"),
            ("HealthCheck.UnhealthyThreshold", "2"),
        ],
    );
    let mut registration: Vec<(String, String)> = vec![(
        "LoadBalancerName".to_string(),
        placed.name.to_string(),
    )];
    for (index, instance_id) in instances.iter().enumerate() {
        registration.push((
            format!("Instances.member.{}.InstanceId", index + 1),
            instance_id.clone(),
        ));
    }
    let registration: Vec<(&str, &str)> = registration
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    elb_ok(
        router,
        runtime,
        case,
        "RegisterInstancesWithLoadBalancer",
        &registration,
    );
    dns_name
}

/// The `(State, ReasonCode, Description)` the load balancer `name` reports for `instance_id`
/// now — the three members `InstanceState` carries.
fn reported_health(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    name: &str,
    instance_id: &str,
) -> (String, String, String) {
    let reported = elb_ok(
        router,
        runtime,
        case,
        "DescribeInstanceHealth",
        &[
            ("LoadBalancerName", name),
            ("Instances.member.1.InstanceId", instance_id),
        ],
    );
    let read = |member: &str| {
        elements(&reported, member)
            .first()
            .cloned()
            .unwrap_or_default()
    };
    (read("State"), read("ReasonCode"), read("Description"))
}

/// Wait until that triple is the one `wanted` carries, which is the outcome of the run of
/// checks the clock owed — polled for, never slept through.
fn until_health(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    name: &str,
    instance_id: &str,
    wanted: InstanceHealth,
) {
    let expected = (
        wanted.state.to_string(),
        wanted.reason_code.to_string(),
        wanted.description.to_string(),
    );
    until(
        case,
        &format!("the registration reported {expected:?}"),
        || {
            let reported = reported_health(router, runtime, case, name, instance_id);
            (reported == expected).then_some(())
        },
    );
}

#[test]
fn a_classic_load_balancer_answers_with_its_registered_instances_own_response() {
    std::thread::Builder::new()
        .name("elb-classic-instance-response".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            let zone_a = format!("{REGION}a");
            let zone_b = format!("{REGION}b");
            let network = seed_network(
                &router,
                &runtime,
                "10.71.0.0/16",
                &[
                    ("10.71.1.0/24", zone_a.as_str()),
                    ("10.71.2.0/24", zone_b.as_str()),
                ],
            );

            // Every backend is launched and registered FIRST, so the runs of health checks the
            // clock owes them all happen at once rather than one row at a time.
            let instances: Vec<String> = BACKENDS
                .iter()
                .map(|backend| launch(&router, &runtime, &network, 0, backend.user_data))
                .collect();
            for (backend, instance_id) in BACKENDS.iter().zip(&instances) {
                until_running(&router, &runtime, backend.case, instance_id);
            }
            let names: Vec<String> = (0..BACKENDS.len())
                .map(|index| format!("classic-guest-elb-{index}"))
                .collect();
            for ((backend, instance_id), name) in BACKENDS.iter().zip(&instances).zip(&names) {
                load_balancer(
                    &router,
                    &runtime,
                    &network,
                    backend.case,
                    &Placed {
                        name,
                        instance_port: backend.instance_port,
                        target: backend.target,
                    },
                    std::slice::from_ref(instance_id),
                );
            }
            // The addresses they published, read back where a client that did not keep the
            // create's answer reads them.
            let published: Vec<String> = names
                .iter()
                .map(|name| {
                    element(
                        &elb_ok(
                            &router,
                            &runtime,
                            name,
                            "DescribeLoadBalancers",
                            &[("LoadBalancerNames.member.1", name.as_str())],
                        ),
                        "DNSName",
                    )
                })
                .collect();
            for ((backend, instance_id), name) in BACKENDS.iter().zip(&instances).zip(&names) {
                until_health(
                    &router,
                    &runtime,
                    backend.case,
                    name,
                    instance_id,
                    backend.settles_at,
                );
            }

            for row in ROWS {
                let dns_name = &published[row.backend];
                for (how, answered) in [
                    (
                        "carried in process",
                        carried(&router, &runtime, dns_name, row.method, row.path, None),
                    ),
                    (
                        "dialled over TCP",
                        dialled(&runtime, row.case, dns_name, row.method, row.path, None),
                    ),
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
                    assert_eq!(
                        answered.content_type.is_some(),
                        row.served_document,
                        "case {:?} ({how}): the media type must be the instance's own, stated \
                         for a document it served and for nothing else, got {:?}",
                        row.case,
                        answered.content_type
                    );
                }
            }
        })
        .expect("the sized thread")
        .join()
        .expect("the suite ran");
}

/// How many cookie-less requests the spread below draws, and how many a held pin is carried
/// over. Far above what it takes to see both instances: one draw per request over two eligible
/// instances misses one with probability 2^-23 at this count.
const DRAWS: usize = 24;

#[test]
fn a_duration_policy_pins_a_client_to_the_instance_that_served_it() {
    std::thread::Builder::new()
        .name("elb-classic-stickiness-response".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            let case = "a duration-based policy over two serving backends";
            const NAME: &str = "classic-sticky-elb";

            // ONE subnet, so both backends are in the zone of the node that takes the request:
            // `CrossZoneLoadBalancing` is at its modelled default (false) here, and what the
            // rows below are about is the PIN rather than the placement.
            let zone = format!("{REGION}a");
            let network = seed_network(
                &router,
                &runtime,
                "10.72.0.0/16",
                &[("10.72.1.0/24", zone.as_str())],
            );
            // Two backends whose servers serve DIFFERENT bodies, so which instance answered a
            // request is read off the answer itself.
            let programs = [
                (
                    "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                     echo GUEST-B-BODY > /var/www/html/index.html\n\
                     systemctl enable --now httpd\n",
                    MARKER_B,
                ),
                (
                    "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                     echo GUEST-C-BODY > /var/www/html/index.html\n\
                     systemctl enable --now httpd\n",
                    MARKER_C,
                ),
            ];
            let instances: Vec<String> = programs
                .iter()
                .map(|(user_data, _)| launch(&router, &runtime, &network, 0, user_data))
                .collect();
            for instance_id in &instances {
                until_running(&router, &runtime, case, instance_id);
            }
            let dns_name = load_balancer(
                &router,
                &runtime,
                &network,
                case,
                &Placed {
                    name: NAME,
                    instance_port: "80",
                    target: "HTTP:80/index.html",
                },
                &instances,
            );
            for instance_id in &instances {
                until_health(
                    &router,
                    &runtime,
                    case,
                    NAME,
                    instance_id,
                    HEALTH_IN_SERVICE,
                );
            }
            // The policy, and the listener it governs: a duration-based policy is what makes
            // the listener mint the `AWSELB` cookie at all.
            elb_ok(
                &router,
                &runtime,
                case,
                "CreateLBCookieStickinessPolicy",
                &[
                    ("LoadBalancerName", NAME),
                    ("PolicyName", "pin"),
                    ("CookieExpirationPeriod", "300"),
                ],
            );
            elb_ok(
                &router,
                &runtime,
                case,
                "SetLoadBalancerPoliciesOfListener",
                &[
                    ("LoadBalancerName", NAME),
                    ("LoadBalancerPort", LISTENER_PORT),
                    ("PolicyNames.member.1", "pin"),
                ],
            );

            // A cookie-less client is spread across the instances the load balancer may send
            // requests to, and every answer it gets is one of those instances' own body,
            // carrying the pin that names the instance which served it.
            let mut spread: BTreeSet<String> = BTreeSet::new();
            let mut drawn: Vec<(String, String)> = Vec::new();
            for draw in 0..DRAWS {
                let answered = carried(&router, &runtime, &dns_name, "GET", "/", None);
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}, draw {draw}: a request to an in-service backend must be \
                     answered by it, got {answered:?}"
                );
                let body = answered.body.trim().to_string();
                let pin = answered.minted_pin().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}, draw {draw}: a listener carrying the policy must mint \
                         the pin, got {:?}",
                        answered.set_cookie
                    )
                });
                spread.insert(body.clone());
                drawn.push((pin, body));
            }
            let served: BTreeSet<String> =
                programs.iter().map(|(_, body)| body.to_string()).collect();
            assert_eq!(
                spread, served,
                "case {case:?}: a cookie-less client must be spread across both instances' own \
                 bodies, got {spread:?}"
            );

            // And a client that carries a pin back is answered by the instance that pin names,
            // every time, with no new cookie minted — over both ways it can reach the address.
            let (pin, body) = drawn.first().cloned().expect("a drawn request minted a pin");
            let cookie = format!("AWSELB={pin}");
            for attempt in 0..DRAWS {
                for (how, answered) in [
                    (
                        "carried in process",
                        carried(&router, &runtime, &dns_name, "GET", "/", Some(&cookie)),
                    ),
                    (
                        "dialled over TCP",
                        dialled(&runtime, case, &dns_name, "GET", "/", Some(&cookie)),
                    ),
                ] {
                    assert_eq!(
                        (answered.status, answered.body.trim()),
                        (200, body.as_str()),
                        "case {case:?}, pinned request {attempt} ({how}): the pin must be \
                         honoured by the instance it names, got {answered:?}"
                    );
                    assert_eq!(
                        answered.set_cookie, None,
                        "case {case:?}, pinned request {attempt} ({how}): a client that holds \
                         the pin must be handed no new one, got {:?}",
                        answered.set_cookie
                    );
                }
            }
        })
        .expect("the sized thread")
        .join()
        .expect("the suite ran");
}

/// The program each backend of the cross-zone rows runs: one body per zone, so WHICH zone's
/// instance answered is read off the answer.
fn zone_program(marker: &str) -> String {
    format!(
        "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
         echo {marker} > /var/www/html/index.html\nsystemctl enable --now httpd\n"
    )
}

#[test]
fn cross_zone_load_balancing_decides_whose_body_the_published_address_answers_with() {
    std::thread::Builder::new()
        .name("elb-classic-cross-zone-response".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            let case = "one backend per zone behind one published address";
            const NAME: &str = "classic-cross-zone-elb";

            // Two subnets in two zones, one serving backend in each: the node that takes a
            // request is in the FIRST of the load balancer's zones, which is the only zone it
            // serves while `CrossZoneLoadBalancing$Enabled` is at its modelled default.
            let zone_a = format!("{REGION}a");
            let zone_b = format!("{REGION}b");
            let network = seed_network(
                &router,
                &runtime,
                "10.73.0.0/16",
                &[
                    ("10.73.1.0/24", zone_a.as_str()),
                    ("10.73.2.0/24", zone_b.as_str()),
                ],
            );
            let node_zone_program = zone_program(MARKER_B);
            let other_zone_program = zone_program(MARKER_C);
            let instances: Vec<String> = [&node_zone_program, &other_zone_program]
                .iter()
                .enumerate()
                .map(|(subnet, program)| launch(&router, &runtime, &network, subnet, program))
                .collect();
            for instance_id in &instances {
                until_running(&router, &runtime, case, instance_id);
            }
            let dns_name = load_balancer(
                &router,
                &runtime,
                &network,
                case,
                &Placed {
                    name: NAME,
                    instance_port: "80",
                    target: "HTTP:80/index.html",
                },
                &instances,
            );
            for instance_id in &instances {
                until_health(&router, &runtime, case, NAME, instance_id, HEALTH_IN_SERVICE);
            }

            // Cross-zone load balancing off, which is the attribute's modelled default: every
            // answer is the body of the instance in the node's own zone.
            let mut own_zone: BTreeSet<String> = BTreeSet::new();
            for draw in 0..DRAWS {
                let answered = carried(&router, &runtime, &dns_name, "GET", "/", None);
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}, draw {draw} with cross-zone off: the node's own zone has an \
                     in-service instance, so its body must come back, got {answered:?}"
                );
                own_zone.insert(answered.body.trim().to_string());
            }
            assert_eq!(
                own_zone,
                BTreeSet::from([MARKER_B.to_string()]),
                "case {case:?}: with cross-zone load balancing off only the node's own zone may \
                 answer, got {own_zone:?}"
            );

            // Turned ON through the attribute's own operation: the same node now answers out of
            // the instances in every zone the load balancer is in.
            elb_ok(
                &router,
                &runtime,
                case,
                "ModifyLoadBalancerAttributes",
                &[
                    ("LoadBalancerName", NAME),
                    (
                        "LoadBalancerAttributes.CrossZoneLoadBalancing.Enabled",
                        "true",
                    ),
                ],
            );
            let mut spread: BTreeSet<String> = BTreeSet::new();
            for draw in 0..(DRAWS * 2) {
                let answered = carried(&router, &runtime, &dns_name, "GET", "/", None);
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}, draw {draw} with cross-zone on: every in-service instance \
                     may answer, got {answered:?}"
                );
                spread.insert(answered.body.trim().to_string());
            }
            assert_eq!(
                spread,
                BTreeSet::from([MARKER_B.to_string(), MARKER_C.to_string()]),
                "case {case:?}: with cross-zone load balancing on both zones' instances must \
                 answer, got {spread:?}"
            );
        })
        .expect("the sized thread")
        .join()
        .expect("the suite ran");
}
