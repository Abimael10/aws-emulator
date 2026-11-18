// SPDX-License-Identifier: CC0-1.0

//! The ENDPOINT NETWORK INTERFACES an interface VPC endpoint is served by: who holds them,
//! what they report, and what releases them — driven over the wire the service speaks
//! (`ec2Query`: an `application/x-www-form-urlencoded` POST answered with XML, README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! ONE RULE is under test: a network interface the SERVICE that created it manages for a
//! resource of the account is held by that resource for as long as the resource exists. "When
//! you create an interface endpoint, we create an endpoint network interface in the subnet"
//! (the Amazon Web Services PrivateLink User Guide's "Interface endpoints"), so such an
//! interface reports `RequesterManaged` true and the `in-use` status of an interface something
//! holds, and a caller's own `DeleteNetworkInterface` of it is refused `InvalidParameterValue`
//! — "Network interface '<id>' is currently in use." — leaving it exactly where it was. Only
//! deleting the ENDPOINT releases it: "when you delete an interface endpoint, we delete its
//! endpoint network interfaces" (`DeleteVpcEndpoints`).
//!
//! The rows span what that generalises over: how many subnets the endpoint is served from (so
//! how many interfaces it holds), which endpoint type stood them up, and an endpoint type that
//! stands up none at all. A caller's OWN interface is the control row: nothing holds it, so it
//! is `available` and the same delete takes it away — the rule refuses an interface a service
//! holds, not every interface.

use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{child, child_text, document, param, Param};

/// The text of every `<item>` of the first set element named `set`.
///
/// Read through the XML parser rather than by substring, so the items of ONE set are never
/// confused with the identically-shaped items of another set of the same response.
fn set_items(case: &str, body: &str, set: &str) -> Vec<String> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == set)
        .map(|node| {
            node.children()
                .filter(|item| item.is_element() && item.tag_name().name() == "item")
                .map(|item| item.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The VPC the endpoints are created in, and the subnets they are served from.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNET_CIDRS: [&str; 2] = ["10.91.1.0/24", "10.91.2.0/24"];

/// A gateway endpoint is reached through a service's prefix list rather than through an
/// interface, so this row names a service of the Region's own catalogue that offers one.
const GATEWAY_SERVICE: &str = "com.amazonaws.us-east-1.s3";

/// How long a poll keeps looking: far past the span the service moves a record over
/// ([`ec2::state::TRANSITION_DELAY`]), so a loaded host only waits longer.
fn poll_deadline() -> Instant {
    Instant::now() + ec2::state::TRANSITION_DELAY * 40
}

/// How long the poll waits between two looks.
fn poll_interval() -> Duration {
    ec2::state::TRANSITION_DELAY / 20
}

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The ARN of a load balancer of `kind` behind one of the services: `net` for a Network Load
/// Balancer, `gwy` for a Gateway Load Balancer. A load balancer is a resource of another
/// service, so what this crate stores is the ARN the request named.
fn load_balancer_arn(kind: &str, name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/{kind}/{name}/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// What one interface reports: its status, whether a service manages it, and its description.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InterfaceView {
    status: String,
    requester_managed: String,
    description: String,
}

/// What `DescribeNetworkInterfaces` reports for `network_interface_id`, or `None` when the
/// account holds no such interface any more.
async fn described(
    case: &str,
    router: &MultiServiceRouter,
    network_interface_id: &str,
) -> Option<InterfaceView> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeNetworkInterfaces",
        &[param("NetworkInterfaceId.1", network_interface_id)],
    )
    .await;
    if status != 200 {
        assert_eq!(
            error_code(&body),
            ec2::state::INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
            "case {case:?}: an interface the account no longer holds is reported not found, \
             got {body}"
        );
        return None;
    }
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())?;
    Some(InterfaceView {
        status: child_text(interface, "status").unwrap_or_default(),
        requester_managed: child_text(interface, "requesterManaged").unwrap_or_default(),
        description: child_text(interface, "description").unwrap_or_default(),
    })
}

/// Look again until `endpoint_id` reports `wanted`, or a generous deadline passes.
async fn until_endpoint_state(
    case: &str,
    router: &MultiServiceRouter,
    endpoint_id: &str,
    wanted: &str,
) {
    let deadline = poll_deadline();
    loop {
        let body = ok(
            case,
            router,
            "DescribeVpcEndpoints",
            &[param("VpcEndpointId.1", endpoint_id)],
        )
        .await;
        let parsed = document(case, &body);
        let state = parsed
            .descendants()
            .find(|node| node.is_element() && child(*node, "vpcEndpointId").is_some())
            .and_then(|endpoint| child_text(endpoint, "state"))
            .unwrap_or_default();
        if state == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {endpoint_id} must reach {wanted:?}, got {state:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// One row: the endpoint it creates, and how many interfaces that endpoint must hold.
struct Row {
    case: &'static str,
    /// The endpoint type, as `CreateVpcEndpoint` spells it.
    endpoint_type: &'static str,
    /// How many of this suite's subnets the endpoint is served from.
    subnets: usize,
    /// Whether the service behind it is reached through a Gateway Load Balancer.
    gateway_load_balancer: bool,
    /// How many endpoint network interfaces the endpoint must hold.
    interfaces: usize,
}

/// RULE: every endpoint network interface is held by the endpoint it serves — it reports
/// `in-use` and `RequesterManaged`, a caller's own delete of it is refused, and only deleting
/// the endpoint releases it.
#[tokio::test]
async fn an_endpoints_own_interfaces_are_held_by_it_until_the_endpoint_is_deleted() {
    let (router, _engine) = super::ec2_query::emulator();

    let vpc_id = {
        let body = ok("the VPC", &router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
        element(&body, "vpcId").unwrap_or_default()
    };
    let mut subnet_ids: Vec<String> = Vec::new();
    // One subnet per zone: "VPC endpoint subnets should be in different availability zones",
    // so the two-subnet row needs two zones of the Region.
    let zones = ec2::state::availability_zones_in_region(REGION);
    for (position, cidr_block) in SUBNET_CIDRS.iter().enumerate() {
        let zone = zones
            .get(position)
            .map(|zone| zone.zone_name.clone())
            .expect("a Region reports more zones than this suite needs");
        let body = ok(
            "the subnet",
            &router,
            "CreateSubnet",
            &[
                param("VpcId", &vpc_id),
                param("CidrBlock", cidr_block),
                param("AvailabilityZone", &zone),
            ],
        )
        .await;
        subnet_ids.push(element(&body, "subnetId").unwrap_or_default());
    }

    // One service behind each kind of load balancer, each waiving acceptance so the endpoints
    // to them reach `available` on the clock alone.
    let service_name = async |case: &str, kind: &str, name: &str| -> String {
        let body = ok(
            case,
            &router,
            "CreateVpcEndpointServiceConfiguration",
            &[
                param(
                    &format!("{}LoadBalancerArn.1", if kind == "net" { "Network" } else { "Gateway" }),
                    &load_balancer_arn(kind, name),
                ),
                param("AcceptanceRequired", "false"),
            ],
        )
        .await;
        element(&body, "serviceName").unwrap_or_default()
    };
    let interface_service = service_name("the interface service", "net", "behind-interface").await;
    let load_balancer_service = service_name("the gwlb service", "gwy", "behind-gwlb").await;

    let rows: Vec<Row> = vec![
        Row {
            case: "an interface endpoint in one subnet",
            endpoint_type: ec2::state::VPC_ENDPOINT_TYPE_INTERFACE,
            subnets: 1,
            gateway_load_balancer: false,
            interfaces: 1,
        },
        Row {
            case: "an interface endpoint in two subnets",
            endpoint_type: ec2::state::VPC_ENDPOINT_TYPE_INTERFACE,
            subnets: 2,
            gateway_load_balancer: false,
            interfaces: 2,
        },
        Row {
            case: "a Gateway Load Balancer endpoint",
            endpoint_type: ec2::state::VPC_ENDPOINT_TYPE_GATEWAY_LOAD_BALANCER,
            subnets: 1,
            gateway_load_balancer: true,
            interfaces: 1,
        },
    ];

    for row in rows {
        let case = row.case;
        let mut params = vec![
            param("VpcId", &vpc_id),
            param("VpcEndpointType", row.endpoint_type),
            param(
                "ServiceName",
                if row.gateway_load_balancer {
                    &load_balancer_service
                } else {
                    &interface_service
                },
            ),
        ];
        for (position, subnet_id) in subnet_ids.iter().take(row.subnets).enumerate() {
            params.push(param(&format!("SubnetId.{}", position + 1), subnet_id));
        }
        let body = ok(case, &router, "CreateVpcEndpoint", &params).await;
        let endpoint_id = element(&body, "vpcEndpointId")
            .unwrap_or_else(|| panic!("case {case:?}: a create reports its endpoint: {body}"));
        let held: Vec<String> = set_items(case, &body, "networkInterfaceIdSet");
        assert_eq!(
            held.len(),
            row.interfaces,
            "case {case:?}: expected {interfaces} endpoint network interfaces, got {held:?}",
            interfaces = row.interfaces,
        );

        // Held from the moment the endpoint exists, and still held once it is in service: the
        // endpoint owns them for its whole life.
        for stage in ["while the endpoint is coming up", "once it is in service"] {
            if stage == "once it is in service" {
                until_endpoint_state(
                    case,
                    &router,
                    &endpoint_id,
                    ec2::state::VPC_ENDPOINT_STATE_AVAILABLE,
                )
                .await;
            }
            for network_interface_id in &held {
                let view = described(case, &router, network_interface_id).await;
                assert_eq!(
                    view,
                    Some(InterfaceView {
                        status: ec2::state::NETWORK_INTERFACE_STATUS_IN_USE.to_string(),
                        requester_managed: "true".to_string(),
                        description: format!("VPC Endpoint Interface {endpoint_id}"),
                    }),
                    "case {case:?} ({stage}): the endpoint holds {network_interface_id}, so it \
                     must report in-use and managed by the service, got {view:?}"
                );

                // The caller's own delete of it is refused, and the interface stays.
                let (status, body) = call(
                    &router,
                    REGION,
                    "DeleteNetworkInterface",
                    &[param("NetworkInterfaceId", network_interface_id)],
                )
                .await;
                assert_eq!(
                    status, 400,
                    "case {case:?} ({stage}): deleting an interface the endpoint holds must be \
                     refused, got {status} with body {body}"
                );
                assert_eq!(
                    error_code(&body),
                    ec2::state::INVALID_PARAMETER_VALUE,
                    "case {case:?} ({stage}): expected the parameter-value refusal, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    format!("Network interface '{network_interface_id}' is currently in use."),
                    "case {case:?} ({stage}): expected EC2's own sentence for an interface in \
                     use, got {body}"
                );
                assert!(
                    described(case, &router, network_interface_id)
                        .await
                        .is_some(),
                    "case {case:?} ({stage}): a refused delete leaves {network_interface_id} \
                     where it was"
                );
            }
        }

        // Deleting the ENDPOINT is what releases them.
        ok(
            case,
            &router,
            "DeleteVpcEndpoints",
            &[param("VpcEndpointId.1", &endpoint_id)],
        )
        .await;
        for network_interface_id in &held {
            let view = described(case, &router, network_interface_id).await;
            assert_eq!(
                view, None,
                "case {case:?}: deleting the endpoint deletes its interfaces, got {view:?}"
            );
        }
    }
}

/// RULE, the other half: nothing holds an interface the CALLER created for itself, so it is
/// `available` and the same delete takes it away — and an endpoint type that stands up no
/// interface at all holds none.
#[tokio::test]
async fn an_interface_nothing_holds_is_available_and_deletable() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = {
        let body = ok("the VPC", &router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
        element(&body, "vpcId").unwrap_or_default()
    };
    let subnet_id = {
        let body = ok(
            "the subnet",
            &router,
            "CreateSubnet",
            &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDRS[0])],
        )
        .await;
        element(&body, "subnetId").unwrap_or_default()
    };

    // The caller's own interface: nothing created it for a resource of the account.
    let body = ok(
        "a caller's own interface",
        &router,
        "CreateNetworkInterface",
        &[param("SubnetId", &subnet_id)],
    )
    .await;
    let network_interface_id = element(&body, "networkInterfaceId").unwrap_or_default();
    let view = described("a caller's own interface", &router, &network_interface_id).await;
    assert_eq!(
        view.as_ref().map(|held| held.status.as_str()),
        Some(ec2::state::NETWORK_INTERFACE_STATUS_AVAILABLE),
        "an interface nothing holds is available, got {view:?}"
    );
    assert_eq!(
        view.as_ref().map(|held| held.requester_managed.as_str()),
        Some("false"),
        "no service manages an interface the caller created, got {view:?}"
    );
    ok(
        "a caller's own interface",
        &router,
        "DeleteNetworkInterface",
        &[param("NetworkInterfaceId", &network_interface_id)],
    )
    .await;
    assert_eq!(
        described("a caller's own interface", &router, &network_interface_id).await,
        None,
        "the delete takes away an interface nothing holds"
    );

    // A gateway endpoint is reached through its service's prefix list in a route table, so it
    // stands up no interface and holds none.
    let body = ok(
        "a gateway endpoint",
        &router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("VpcEndpointType", ec2::state::VPC_ENDPOINT_TYPE_GATEWAY),
            param("ServiceName", GATEWAY_SERVICE),
        ],
    )
    .await;
    assert_eq!(
        set_items("a gateway endpoint", &body, "networkInterfaceIdSet"),
        Vec::<String>::new(),
        "a gateway endpoint stands up no endpoint network interface, got {body}"
    );
}
