// SPDX-License-Identifier: CC0-1.0

//! The ORDER the two prerequisites of private DNS are read in, driven over the wire the service
//! speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md
//! / OPERATIONS.md "Wire protocol").
//!
//! Associating a private hosted zone with a VPC needs two things to hold: the VPC has to resolve
//! private names at all — "to use a private hosted zone, you must set the following VPC
//! attributes to true: enableDnsHostnames and enableDnsSupport"
//! (`CreateVpcEndpointRequest$PrivateDnsEnabled`) — and the endpoint service has to publish the
//! default public DNS name the zone's record set is for.
//!
//! The rule this suite pins: a request that asks to enable private DNS is held to the VPC's own
//! attributes FIRST, so a VPC with either attribute off earns the sentence naming both
//! attributes and that VPC whatever the named service publishes; only a VPC that has both on
//! reaches the question of what the service publishes. The same order holds for the create and
//! for the modify that turns the association on.
//!
//! Each rule is asserted over a TABLE: both kinds of endpoint service — one that publishes a
//! default public DNS name and one that publishes none — against each attribute state a VPC can
//! be in.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// An endpoint service that publishes a default public DNS name of its own, so private DNS has
/// a record set to put in the zone.
const SERVICE_WITH_NAME: &str = "com.amazonaws.us-west-2.logs";

/// An endpoint service that publishes NONE: a recorded `DescribeVpcEndpointServices` reports the
/// AWS IoT Core data plane with no `privateDnsName` at all (the suite
/// `vpc_endpoint_private_dns_domains` pins that reading of the catalogue).
const SERVICE_WITHOUT_NAME: &str = "com.amazonaws.us-west-2.iot.data";

/// The emulator the tests drive, reached only over the wire.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The DNS attributes a VPC is put in for a row.
#[derive(Debug, Clone, Copy)]
struct VpcDns {
    /// `enableDnsSupport`: "whether the DNS resolution is supported for the VPC".
    support: bool,
    /// `enableDnsHostnames`: whether instances launched in it get public DNS hostnames.
    hostnames: bool,
}

/// A VPC in a given attribute state, and one subnet in it.
struct Network {
    vpc_id: String,
    subnet_id: String,
}

/// Build that network through the API, so every endpoint below is created against resources the
/// emulator actually stores.
///
/// A VPC a caller creates has `enableDnsSupport` on and `enableDnsHostnames` off (Amazon VPC
/// User Guide, "DNS attributes in your VPC"), so only the attributes that differ from that are
/// written — and `enableDnsHostnames` is only settable while support is on, which is why
/// hostnames are turned on before support is turned off.
async fn network(case: &str, router: &MultiServiceRouter, prefix: &str, dns: VpcDns) -> Network {
    let body = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", &format!("{prefix}.0.0/16"))],
    )
    .await;
    let vpc_id = member(case, &body, "vpcId");
    if dns.hostnames {
        let _ = ok(
            case,
            router,
            "ModifyVpcAttribute",
            &[
                param("VpcId", &vpc_id),
                param("EnableDnsHostnames.Value", "true"),
            ],
        )
        .await;
    }
    if !dns.support {
        let _ = ok(
            case,
            router,
            "ModifyVpcAttribute",
            &[
                param("VpcId", &vpc_id),
                param("EnableDnsSupport.Value", "false"),
            ],
        )
        .await;
    }
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", &format!("{prefix}.1.0/24")),
            param("AvailabilityZone", &format!("{REGION}a")),
        ],
    )
    .await;
    let subnet_id = member(case, &body, "subnetId");
    Network { vpc_id, subnet_id }
}

/// The form fields of a `CreateVpcEndpoint` for an interface endpoint of `built` to
/// `service_name`, with whatever `extra` the row varies.
fn interface_params(built: &Network, service_name: &str, extra: Vec<Param>) -> Vec<Param> {
    let mut params = vec![
        param("VpcId", &built.vpc_id),
        param("ServiceName", service_name),
        param("VpcEndpointType", "Interface"),
        param("SubnetId.1", &built.subnet_id),
    ];
    params.extend(extra);
    params
}

/// The sentence EC2 refuses a private-DNS association in a VPC that resolves no private names
/// with: it names both attributes that have to be on and the VPC they are off for.
fn vpc_refusal(vpc_id: &str) -> String {
    format!(
        "Enabling private DNS requires both enableDnsSupport and enableDnsHostnames VPC \
         attributes set to true for {vpc_id}"
    )
}

/// The sentence EC2 refuses a private-DNS association to a service publishing no default public
/// DNS name with: it names the service at fault.
fn service_refusal(service_name: &str) -> String {
    format!(
        "Private DNS can't be enabled because the service {service_name} does not provide a \
         private DNS name."
    )
}

/// What a row expects of the request that asks for private DNS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Refused for the VPC's attributes, naming both of them and the VPC.
    VpcAttributes,
    /// Refused for the service publishing no name of its own, naming the service.
    ServiceHasNoName,
    /// Served: the endpoint carries the association.
    Associated,
}

/// One row: the service the endpoint is to, the state its VPC's attributes are in, and what the
/// request asking for private DNS earns.
struct Row {
    case: &'static str,
    /// The /16 prefix this row's VPC takes, so no row's endpoint can claim a domain another
    /// row's holds.
    prefix: &'static str,
    service_name: &'static str,
    dns: VpcDns,
    expected: Expected,
}

/// The rows: both kinds of service against each attribute state — the default state a created
/// VPC is in (support on, hostnames off), both attributes off, and both on.
const ROWS: [Row; 6] = [
    Row {
        case: "a service publishing a name, in a VPC with hostnames off",
        prefix: "10.81",
        service_name: SERVICE_WITH_NAME,
        dns: VpcDns { support: true, hostnames: false },
        expected: Expected::VpcAttributes,
    },
    Row {
        case: "a service publishing a name, in a VPC with both attributes off",
        prefix: "10.82",
        service_name: SERVICE_WITH_NAME,
        dns: VpcDns { support: false, hostnames: false },
        expected: Expected::VpcAttributes,
    },
    Row {
        case: "a service publishing a name, in a VPC with both attributes on",
        prefix: "10.83",
        service_name: SERVICE_WITH_NAME,
        dns: VpcDns { support: true, hostnames: true },
        expected: Expected::Associated,
    },
    Row {
        case: "a service publishing no name, in a VPC with hostnames off",
        prefix: "10.84",
        service_name: SERVICE_WITHOUT_NAME,
        dns: VpcDns { support: true, hostnames: false },
        expected: Expected::VpcAttributes,
    },
    Row {
        case: "a service publishing no name, in a VPC with both attributes off",
        prefix: "10.85",
        service_name: SERVICE_WITHOUT_NAME,
        dns: VpcDns { support: false, hostnames: false },
        expected: Expected::VpcAttributes,
    },
    Row {
        case: "a service publishing no name, in a VPC with both attributes on",
        prefix: "10.86",
        service_name: SERVICE_WITHOUT_NAME,
        dns: VpcDns { support: true, hostnames: true },
        expected: Expected::ServiceHasNoName,
    },
];

/// Assert one refusal: its status, its code and the sentence the row expects.
fn assert_refused(case: &str, status: u16, body: &str, message: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(body),
        ec2::state::INVALID_PARAMETER,
        "case {case:?}: expected InvalidParameter, got {body}"
    );
    assert_eq!(
        error_message(body),
        message,
        "case {case:?}: expected the sentence {message:?}, got {body}"
    );
}

/// A CREATE that asks for private DNS is held to the VPC's attributes before the service's own
/// published name: the VPC's sentence whatever the service publishes, and the service's sentence
/// only once the VPC has both attributes on.
#[tokio::test]
async fn a_create_reads_the_vpcs_attributes_before_the_services_name() {
    let router = emulator();

    for row in &ROWS {
        let case = row.case;
        let built = network(case, &router, row.prefix, row.dns).await;
        let (status, body) = call(
            &router,
            "CreateVpcEndpoint",
            &interface_params(
                &built,
                row.service_name,
                vec![param("PrivateDnsEnabled", "true")],
            ),
        )
        .await;
        match row.expected {
            Expected::VpcAttributes => {
                assert_refused(case, status, &body, &vpc_refusal(&built.vpc_id));
            }
            Expected::ServiceHasNoName => {
                assert_refused(case, status, &body, &service_refusal(row.service_name));
            }
            Expected::Associated => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} with body {body}"
                );
                assert_eq!(
                    element(&body, "privateDnsEnabled").as_deref(),
                    Some("true"),
                    "case {case:?}: the endpoint must report the association, got {body}"
                );
            }
        }
        // A refused create writes no endpoint at all.
        if row.expected != Expected::Associated {
            let reported = ok(
                case,
                &router,
                "DescribeVpcEndpoints",
                &super::ec2_query::filters(&[("vpc-id", &[built.vpc_id.as_str()])]),
            )
            .await;
            assert!(
                element(&reported, "vpcEndpointId").is_none(),
                "case {case:?}: a refused create must leave the VPC without an endpoint, got \
                 {reported}"
            );
        }
    }
}

/// And a MODIFY that turns the association ON is held to the same two questions in the same
/// order, on an endpoint created without it.
#[tokio::test]
async fn a_modify_reads_the_vpcs_attributes_before_the_services_name() {
    let router = emulator();

    for row in &ROWS {
        let case = row.case;
        let built = network(case, &router, row.prefix, row.dns).await;
        let created = ok(
            case,
            &router,
            "CreateVpcEndpoint",
            &interface_params(
                &built,
                row.service_name,
                vec![param("PrivateDnsEnabled", "false")],
            ),
        )
        .await;
        let endpoint_id = member(case, &created, "vpcEndpointId");
        let turn_on = [
            param("VpcEndpointId", &endpoint_id),
            param("PrivateDnsEnabled", "true"),
        ];
        let (status, body) = call(&router, "ModifyVpcEndpoint", &turn_on).await;
        match row.expected {
            Expected::VpcAttributes => {
                assert_refused(case, status, &body, &vpc_refusal(&built.vpc_id));
            }
            Expected::ServiceHasNoName => {
                assert_refused(case, status, &body, &service_refusal(row.service_name));
            }
            Expected::Associated => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} with body {body}"
                );
            }
        }
        // The endpoint holds the association only where the modify was served.
        let reported = ok(
            case,
            &router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
        )
        .await;
        let expected_association = row.expected == Expected::Associated;
        assert_eq!(
            element(&reported, "privateDnsEnabled").as_deref(),
            Some(if expected_association { "true" } else { "false" }),
            "case {case:?}: expected privateDnsEnabled {expected_association}, got {reported}"
        );
    }
}
