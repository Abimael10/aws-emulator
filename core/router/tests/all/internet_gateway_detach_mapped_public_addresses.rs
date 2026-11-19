// SPDX-License-Identifier: CC0-1.0

//! Detaching a VPC's internet gateway is REFUSED while a public address is still mapped in
//! that VPC, whatever holds it.
//!
//! RULE: a public IPv4 address mapped in a VPC reaches the internet through that VPC's
//! internet gateway, so `DetachInternetGateway` is refused `DependencyViolation` — "Network
//! <vpc> has some mapped public address(es). Please unmap those public address(es) before
//! detaching the gateway." — for as long as one is mapped onto something that is using it,
//! and the refused call changes nothing: the attachment is still reported `available`. The
//! operation's own doc names the case a client meets first ("the VPC must not contain any
//! running instances with Elastic IP addresses or public IPv4 addresses",
//! `DetachInternetGateway`), but an instance is not the only thing an address is mapped onto:
//! EC2 maps one onto a NETWORK INTERFACE, on that interface's own
//! `NetworkInterface$Association`, so the service-managed nodes another service places in the
//! VPC hold the gateway exactly as an instance's interface does — "the nodes of an
//! Internet-facing load balancer have public IP addresses" (`CreateLoadBalancerInput$Scheme`),
//! and each of those nodes is one of those interfaces. Once the address is unmapped — the load
//! balancer deleted, the Elastic IP disassociated — the identical detach is served and the
//! gateway reads back attached to nothing.
//!
//! Two crates have to agree for that to hold, which is why this suite lives here: the load
//! balancer is Elastic Load Balancing v2's resource, the interfaces and their addresses are
//! EC2's inventory, and the refusal is EC2 reading what the other service placed in it.
//! Everything is driven through the real operations over one shared engine, so what is
//! asserted is what each service persisted.
//!
//! The rows span what the rule generalises over — WHAT holds the address, and whether it is
//! holding it at all: the nodes of an internet-facing Application and Network Load Balancer
//! (the recorded case is the first), an internal load balancer whose nodes carry only private
//! addresses, a running instance holding an Elastic IP address, an Elastic IP address on an
//! interface nothing holds (`available`, the `NetworkInterface$Status` of an interface no
//! attachment and no service holds, so the address is mapped onto nothing in the network), and
//! a VPC holding nothing at all.
//!
//! The two SERVED rows carrying an address are the emulator's existing answers, pinned here
//! against the same gateway so one reading governs both: an internal load balancer's nodes hold
//! no public address at all (`core/router/tests/all/elbv2_public_nodes_carry_public_addresses.rs`
//! reads that off `DescribeNetworkInterfaces`), and an Elastic IP associated with an interface
//! nothing holds leaves the gateway detachable — which the EC2 crate's own
//! `tests/all/elastic_ip_needs_an_internet_gateway.rs` already asserts end to end, by detaching
//! the gateway of a VPC whose standalone interface carries an association and then reading the
//! `Gateway.NotAttached` that detach causes.
//!
//! The request plumbing below is this suite's own, as `ecs_cross_vpc_target_group_health.rs`'s
//! is: those suites mount the services they drive directly on one engine and run as async
//! tests, where `elbv2_network_resolution.rs`'s `pub(crate)` drivers are the SYNC form built on
//! the whole router behind a sized thread. The one part that must not be written twice — the
//! form encoder and the XML element readers — is taken from `all/query_wire.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, element, form_encode, items_of, opt_element};

/// The region every request below is addressed to.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The zones the two subnets of each row's VPC are created in — an Application Load Balancer
/// is placed in at least two ("you must specify subnets from at least two Availability
/// Zones", `CreateLoadBalancer$Subnets`).
const ZONES: [&str; 2] = ["us-east-1a", "us-east-1b"];

/// An AMI id the one launch below names; `RunInstances` only has to accept it, and no row
/// asserts anything about the image.
const AMI: &str = "ami-0123456789abcdef0";

/// How long a row waits for a clock-owed move it depends on — a load balancer's move to
/// `active`, an instance's to `running` — and how often it looks. Generous on purpose: the
/// deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// The error code AWS refuses a detach the VPC still depends on with.
const DEPENDENCY_VIOLATION: &str = "DependencyViolation";

/// The state an internet gateway's attachment to a VPC is reported in: "for an internet
/// gateway, the state is `available` when attached to a VPC"
/// (`InternetGatewayAttachment$State`).
const ATTACHMENT_AVAILABLE: &str = "available";

/// What holds a public address in one row's VPC.
#[derive(Debug, Clone, Copy)]
enum Dependent {
    /// Nothing at all.
    Nothing,
    /// A load balancer of this family and scheme, placed in both subnets of the VPC and
    /// polled to `active` before the detach is tried.
    LoadBalancer {
        lb_type: &'static str,
        scheme: &'static str,
    },
    /// A running instance holding an Elastic IP address — the case the operation's own doc
    /// names.
    RunningInstanceWithAnElasticIp,
    /// An Elastic IP address associated with an interface nothing holds.
    ElasticIpOnAnUnheldInterface,
}

/// One row: what the VPC holds, and whether the detach of its gateway is refused.
struct Row {
    case: &'static str,
    /// The `/16` of this row's own VPC, so no two rows share an address range.
    cidr: &'static str,
    dependent: Dependent,
    refused: bool,
}

const ROWS: [Row; 6] = [
    Row {
        case: "an active internet-facing Application Load Balancer",
        cidr: "10.120.0.0/16",
        dependent: Dependent::LoadBalancer {
            lb_type: "application",
            scheme: "internet-facing",
        },
        refused: true,
    },
    Row {
        case: "an active internet-facing Network Load Balancer",
        cidr: "10.121.0.0/16",
        dependent: Dependent::LoadBalancer {
            lb_type: "network",
            scheme: "internet-facing",
        },
        refused: true,
    },
    Row {
        case: "an active internal Application Load Balancer, whose nodes carry only private \
               addresses",
        cidr: "10.122.0.0/16",
        dependent: Dependent::LoadBalancer {
            lb_type: "application",
            scheme: "internal",
        },
        refused: false,
    },
    Row {
        case: "a running instance holding an Elastic IP address",
        cidr: "10.123.0.0/16",
        dependent: Dependent::RunningInstanceWithAnElasticIp,
        refused: true,
    },
    Row {
        // Served, and pinned as such by the EC2 crate's own
        // `tests/all/elastic_ip_needs_an_internet_gateway.rs`: an interface no attachment and
        // no service holds carries its address for nothing in the network.
        case: "an Elastic IP address on an interface nothing holds",
        cidr: "10.124.0.0/16",
        dependent: Dependent::ElasticIpOnAnUnheldInterface,
        refused: false,
    },
    Row {
        case: "a VPC holding nothing",
        cidr: "10.125.0.0/16",
        dependent: Dependent::Nothing,
        refused: false,
    },
];

/// What a row placed, as the handle its own unmap needs.
enum Placed {
    Nothing,
    /// The `LoadBalancerArn` of the load balancer to delete.
    LoadBalancer(String),
    /// The `AssociationId` of the Elastic IP association to end.
    Association(String),
}

/// One network a row runs in, as EC2 minted it.
struct Network {
    vpc: String,
    gateway: String,
    subnets: Vec<String>,
}

/// One emulator carrying EC2 and Elastic Load Balancing v2 over ONE shared engine, exactly as
/// the production router builds them.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
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
    (router, engine)
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

/// Send one form-encoded request to the service `signing_name` addresses, and answer its
/// status and body.
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

/// Drive one ec2Query action, as its status and body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    send(router, "ec2", form(action, EC2_API_VERSION, params)).await
}

/// Drive one ec2Query action that must succeed, and answer its body.
async fn ec2_ok(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = ec2(router, action, params).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one awsQuery action against the ELBv2 model that must succeed, and answer its body.
async fn elbv2_ok(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
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

/// Create one row's network through EC2 itself — a VPC, two subnets and the internet gateway
/// attached to it — so every id asserted against is one EC2 minted.
async fn seed_network(router: &MultiServiceRouter, row: &Row) -> Network {
    let case = row.case;
    let vpc = element(
        &ec2_ok(router, "CreateVpc", &[("CidrBlock", row.cidr)]).await,
        "vpcId",
    );
    let gateway = element(
        &ec2_ok(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2_ok(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let prefix = row
        .cidr
        .split_once(".0.0/16")
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: the rows' VPC blocks are /16s, got {}", row.cidr));
    let mut subnets = Vec::new();
    for (index, zone) in ZONES.iter().enumerate() {
        subnets.push(element(
            &ec2_ok(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", &format!("{prefix}.{}.0/24", index + 1)),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    Network {
        vpc,
        gateway,
        subnets,
    }
}

/// Place one row's dependent in its network, and answer what the row's unmap needs.
async fn place_dependent(
    router: &MultiServiceRouter,
    row: &Row,
    index: usize,
    network: &Network,
) -> Placed {
    match row.dependent {
        Dependent::Nothing => Placed::Nothing,
        Dependent::LoadBalancer { lb_type, scheme } => {
            let name = format!("detach-dep-{index}");
            let mut params: Vec<(&str, &str)> = vec![
                ("Name", name.as_str()),
                ("Type", lb_type),
                ("Scheme", scheme),
            ];
            let keys = ["Subnets.member.1", "Subnets.member.2"];
            for (key, subnet) in keys.iter().zip(network.subnets.iter()) {
                params.push((key, subnet.as_str()));
            }
            let created = elbv2_ok(router, "CreateLoadBalancer", &params).await;
            let arn = element(&created, "LoadBalancerArn");
            reaches_active(router, row.case, &arn).await;
            Placed::LoadBalancer(arn)
        }
        Dependent::RunningInstanceWithAnElasticIp => {
            let launched = ec2_ok(
                router,
                "RunInstances",
                &[
                    ("ImageId", AMI),
                    ("MinCount", "1"),
                    ("MaxCount", "1"),
                    ("SubnetId", network.subnets[0].as_str()),
                ],
            )
            .await;
            let instance = element(&launched, "instanceId");
            reaches_running(router, row.case, &instance).await;
            let allocation = element(
                &ec2_ok(router, "AllocateAddress", &[]).await,
                "allocationId",
            );
            let associated = ec2_ok(
                router,
                "AssociateAddress",
                &[
                    ("AllocationId", allocation.as_str()),
                    ("InstanceId", instance.as_str()),
                ],
            )
            .await;
            Placed::Association(element(&associated, "associationId"))
        }
        Dependent::ElasticIpOnAnUnheldInterface => {
            let interface = element(
                &ec2_ok(
                    router,
                    "CreateNetworkInterface",
                    &[("SubnetId", network.subnets[0].as_str())],
                )
                .await,
                "networkInterfaceId",
            );
            let allocation = element(
                &ec2_ok(router, "AllocateAddress", &[]).await,
                "allocationId",
            );
            let associated = ec2_ok(
                router,
                "AssociateAddress",
                &[
                    ("AllocationId", allocation.as_str()),
                    ("NetworkInterfaceId", interface.as_str()),
                ],
            )
            .await;
            Placed::Association(element(&associated, "associationId"))
        }
    }
}

/// Unmap what a row placed — the call a client makes to clear the dependency the refusal
/// names: the load balancer whose nodes hold the addresses is deleted, the Elastic IP
/// association is ended.
async fn unmap(router: &MultiServiceRouter, case: &str, placed: &Placed) {
    match placed {
        Placed::Nothing => panic!("case {case:?}: a row holding nothing has nothing to unmap"),
        Placed::LoadBalancer(arn) => {
            elbv2_ok(router, "DeleteLoadBalancer", &[("LoadBalancerArn", arn)]).await;
        }
        Placed::Association(association_id) => {
            ec2_ok(
                router,
                "DisassociateAddress",
                &[("AssociationId", association_id)],
            )
            .await;
        }
    }
}

/// Wait for the load balancer `arn` to come into service: the `LoadBalancerState$Code`
/// `active`, which the create answers `provisioning` before the clock brings it to.
async fn reaches_active(router: &MultiServiceRouter, case: &str, arn: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = elbv2_ok(
            router,
            "DescribeLoadBalancers",
            &[("LoadBalancerArns.member.1", arn)],
        )
        .await;
        let code = opt_element(&body, "Code").unwrap_or_default();
        if code == "active" || Instant::now() >= deadline {
            assert_eq!(
                code, "active",
                "case {case:?}: expected the load balancer to come into service, got {body}"
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Wait for the instance `instance_id` to reach `running` — the state the dependency the
/// operation's doc names is about.
async fn reaches_running(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = ec2_ok(
            router,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        )
        .await;
        let state = block_of(&body, "instanceState")
            .map(|state| element(state, "name"))
            .unwrap_or_default();
        if state == "running" || Instant::now() >= deadline {
            assert_eq!(
                state, "running",
                "case {case:?}: expected {instance_id} to reach running, got {body}"
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// One `DetachInternetGateway` call for the pair, as its status and body.
async fn detach(router: &MultiServiceRouter, network: &Network) -> (u16, String) {
    ec2(
        router,
        "DetachInternetGateway",
        &[
            ("InternetGatewayId", network.gateway.as_str()),
            ("VpcId", network.vpc.as_str()),
        ],
    )
    .await
}

/// The attachments `DescribeInternetGateways` reports for the row's gateway, as
/// `(vpcId, state)` pairs — what a client reads to tell a refused detach from a served one.
async fn attachments(router: &MultiServiceRouter, network: &Network) -> Vec<(String, String)> {
    let body = ec2_ok(
        router,
        "DescribeInternetGateways",
        &[("InternetGatewayId.1", network.gateway.as_str())],
    )
    .await;
    let set = opt_element(&body, "attachmentSet").unwrap_or_default();
    items_of(&set)
        .into_iter()
        .map(|item| (element(item, "vpcId"), element(item, "state")))
        .collect()
}

/// The `Code` and `Message` of a refusal.
fn refusal(body: &str) -> (String, String) {
    (
        opt_element(body, "Code").unwrap_or_default(),
        opt_element(body, "Message").unwrap_or_default(),
    )
}

/// The sentence AWS words this refusal with, for the network the REQUEST named — everything
/// but the id is the error class's fixed text.
fn expected_message(vpc_id: &str) -> String {
    format!(
        "Network {vpc_id} has some mapped public address(es). Please unmap those public \
         address(es) before detaching the gateway."
    )
}

#[tokio::test]
async fn a_gateway_detach_waits_for_every_mapped_public_address_of_its_vpc() {
    let (router, _engine) = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let network = seed_network(&router, row).await;
        let placed = place_dependent(&router, row, index, &network).await;

        let (status, body) = detach(&router, &network).await;
        if !row.refused {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the detach to be served, got {status} with body {body}"
            );
            let left = attachments(&router, &network).await;
            assert!(
                left.is_empty(),
                "case {case:?}: expected the served detach to leave no attachment, got {left:?}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the detach to be refused, got {status} with body {body}"
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            (code.as_str(), message.as_str()),
            (DEPENDENCY_VIOLATION, expected_message(&network.vpc).as_str()),
            "case {case:?}: expected the documented refusal, got {body}"
        );
        // The refused call is a NO-OP: the attachment the client was told to keep is still
        // there, in the one state an internet gateway's attachment is reported in.
        let held = attachments(&router, &network).await;
        assert_eq!(
            held,
            vec![(network.vpc.clone(), ATTACHMENT_AVAILABLE.to_string())],
            "case {case:?}: expected the refused detach to leave the attachment in place, got \
             {held:?}"
        );

        // And once the address is unmapped, the IDENTICAL call is served — polled, because
        // what the client waits for is the dependency clearing, not a span of time.
        unmap(&router, case, &placed).await;
        let deadline = Instant::now() + DEADLINE;
        loop {
            let (status, body) = detach(&router, &network).await;
            if status == 200 || Instant::now() >= deadline {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the detach to be served once the address was \
                     unmapped, got {status} with body {body}"
                );
                break;
            }
            ::tokio::time::sleep(POLL).await;
        }
        let left = attachments(&router, &network).await;
        assert!(
            left.is_empty(),
            "case {case:?}: expected the gateway to read back attached to nothing, got {left:?}"
        );
    }
}
