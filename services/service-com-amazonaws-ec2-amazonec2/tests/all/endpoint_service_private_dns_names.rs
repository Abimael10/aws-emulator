// SPDX-License-Identifier: CC0-1.0

//! The DEFAULT PUBLIC DNS NAME a published endpoint service carries, and the private hosted
//! zone an interface endpoint to it associates — driven over `ec2Query` (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: an endpoint service Amazon Web Services publishes an INTERFACE for has
//! a default public DNS name when the service publishes one — "the default public DNS name for
//! the service for the Region (for example, kinesis.us-east-1.amazonaws.com)"
//! (`smithy_query(query="doc",shape="CreateVpcEndpointRequest",member="PrivateDnsEnabled")`)
//! — and that name is the service's own NAME read as the reverse-domain spelling it is:
//! `com.amazonaws.<region>.<label>` answers on `<host>.<region>.amazonaws.com`, and the two
//! families published under the `aws.api` / `api.aws` labels on `<host>.<region>.api.aws`,
//! where `<host>` is the label's dotted components in reverse order.
//! So `DescribeVpcEndpointServices` reports that name (and `verified` beside it) for any of
//! them, and `CreateVpcEndpoint` associates the private hosted zone it is the record set of
//! rather than refusing the request; `PrivateDnsEnabled` is refused only for a service that
//! really has no such name — one of the account's OWN service configurations created without a
//! `PrivateDnsName`, or a published service that supports no private DNS.
//!
//! The rows span what the rule generalises over: each published name family, a label the
//! blocked recordings did not cite, a dotted label, a gateway-only service (which has no
//! interface and so no private name) and a service the account configured itself.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

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

/// One row: a published endpoint service, and the default public name its family puts it
/// under.
struct Row {
    case: &'static str,
    /// The service name, in the family's own form.
    service_name: &'static str,
    /// The private DNS name the catalogue must report, or `None` for a service with no
    /// interface and therefore no private hosted zone to associate.
    ///
    /// Written out here, independently of the service, so the suite pins the names and not
    /// the code that builds them.
    private_dns_name: Option<&'static str>,
}

/// The rows: the three published families, with a label the issue did not cite and a dotted
/// one among them, and a gateway-only service beside them.
const ROWS: [Row; 6] = [
    Row {
        case: "the EMR session service, under the aws.api prefix",
        service_name: "aws.api.us-east-1.emr-service-cell01",
        private_dns_name: Some("emr-service-cell01.us-east-1.api.aws"),
    },
    Row {
        case: "the Keyspaces streams service, under the api.aws prefix",
        service_name: "api.aws.us-east-1.cassandra-streams",
        private_dns_name: Some("cassandra-streams.us-east-1.api.aws"),
    },
    Row {
        case: "an interface service under com.amazonaws",
        service_name: "com.amazonaws.us-east-1.logs",
        private_dns_name: Some("logs.us-east-1.amazonaws.com"),
    },
    Row {
        case: "a dotted label under com.amazonaws",
        service_name: "com.amazonaws.us-east-1.ecr.dkr",
        private_dns_name: Some("dkr.ecr.us-east-1.amazonaws.com"),
    },
    Row {
        case: "a service reachable through either kind of endpoint",
        service_name: "com.amazonaws.us-east-1.s3",
        private_dns_name: Some("s3.us-east-1.amazonaws.com"),
    },
    Row {
        case: "a gateway-only service",
        service_name: "com.amazonaws.us-east-1.dynamodb",
        private_dns_name: None,
    },
];

/// The catalogue reports the default public name of every published service under the domain
/// its name family names, and `verified` beside it.
#[tokio::test]
async fn the_catalogue_reports_each_familys_own_private_dns_name() {
    let router = emulator();

    for row in &ROWS {
        let case = row.case;
        let body = ok(
            case,
            &router,
            "DescribeVpcEndpointServices",
            &indexed("ServiceName", &[row.service_name]),
        )
        .await;
        assert_eq!(
            element(&body, "serviceName").as_deref(),
            Some(row.service_name),
            "case {case:?}: the catalogue must report the service asked about, got {body}"
        );
        assert_eq!(
            element(&body, "privateDnsName").as_deref(),
            row.private_dns_name,
            "case {case:?}: expected the private DNS name {:?}, got {body}",
            row.private_dns_name
        );
        assert_eq!(
            element(&body, "privateDnsNameVerificationState").as_deref(),
            row.private_dns_name.map(|_| "verified"),
            "case {case:?}: a service's own private name needs nothing verified, got {body}"
        );
    }
}

/// An interface endpoint to any of those services is created WITH private DNS: the zone has
/// the service's own public name as its record set, so the create is served and the endpoint
/// reports the association and that name among its DNS entries.
#[tokio::test]
async fn an_interface_endpoint_of_every_family_takes_private_dns() {
    let router = emulator();
    let case = "the network";
    let body = ok(case, &router, "CreateVpc", &[param("CidrBlock", "10.72.0.0/16")]).await;
    let vpc_id = member(case, &body, "vpcId");
    // A VPC you create has DNS hostnames OFF (Amazon VPC User Guide, "DNS attributes in your
    // VPC"), and a private hosted zone needs them ON.
    ok(
        case,
        &router,
        "ModifyVpcAttribute",
        &[
            param("VpcId", &vpc_id),
            param("EnableDnsHostnames.Value", "true"),
        ],
    )
    .await;
    let body = ok(
        case,
        &router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.72.1.0/24"),
            param("AvailabilityZone", "us-east-1a"),
        ],
    )
    .await;
    let subnet_id = member(case, &body, "subnetId");

    for row in ROWS.iter().filter(|row| row.private_dns_name.is_some()) {
        let case = row.case;
        let private_dns_name = row.private_dns_name.expect("the row carries a name");
        let body = ok(
            case,
            &router,
            "CreateVpcEndpoint",
            &[
                param("VpcId", &vpc_id),
                param("ServiceName", row.service_name),
                param("VpcEndpointType", "Interface"),
                param("PrivateDnsEnabled", "true"),
                param("SubnetId.1", &subnet_id),
            ],
        )
        .await;
        assert_eq!(
            element(&body, "privateDnsEnabled").as_deref(),
            Some("true"),
            "case {case:?}: expected the endpoint to report the private hosted zone, got {body}"
        );
        let endpoint_id = member(case, &body, "vpcEndpointId");
        let names = dns_names(case, &body);
        assert!(
            names.iter().any(|name| name == private_dns_name),
            "case {case:?}: expected the service's own public name {private_dns_name:?} among \
             {names:?}"
        );

        // The create ANSWERED the stored record, so the describe reports the same.
        let described = ok(
            case,
            &router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[&endpoint_id]),
        )
        .await;
        assert_eq!(
            element(&described, "privateDnsEnabled").as_deref(),
            Some("true"),
            "case {case:?}: the stored endpoint must report the association, got {described}"
        );
        let described_names = dns_names(case, &described);
        assert!(
            described_names.iter().any(|name| name == private_dns_name),
            "case {case:?}: expected {private_dns_name:?} among the stored {described_names:?}"
        );
    }
}

/// And `PrivateDnsEnabled` is still refused for a service that really has no default public
/// name: one of the account's own endpoint service configurations, created without a
/// `PrivateDnsName`.
#[tokio::test]
async fn private_dns_is_refused_for_a_service_that_has_no_private_dns_name() {
    let router = emulator();
    let case = "an account service configuration with no private DNS name";
    let body = ok(case, &router, "CreateVpc", &[param("CidrBlock", "10.73.0.0/16")]).await;
    let vpc_id = member(case, &body, "vpcId");
    ok(
        case,
        &router,
        "ModifyVpcAttribute",
        &[
            param("VpcId", &vpc_id),
            param("EnableDnsHostnames.Value", "true"),
        ],
    )
    .await;
    let body = ok(
        case,
        &router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.73.1.0/24"),
            param("AvailabilityZone", "us-east-1a"),
        ],
    )
    .await;
    let subnet_id = member(case, &body, "subnetId");
    // A load balancer of the account's own, which is what an endpoint service configuration
    // is offered through.
    let body = ok(
        case,
        &router,
        "CreateVpcEndpointServiceConfiguration",
        &[param("NetworkLoadBalancerArn.1", &network_load_balancer_arn())],
    )
    .await;
    let service_name = member(case, &body, "serviceName");
    assert_eq!(
        element(&body, "privateDnsName"),
        None,
        "case {case:?}: a configuration created without one must report no private DNS name, \
         got {body}"
    );

    let (status, body) = call(
        &router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", &service_name),
            param("VpcEndpointType", "Interface"),
            param("PrivateDnsEnabled", "true"),
            param("SubnetId.1", &subnet_id),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        ec2::state::INVALID_PARAMETER,
        "case {case:?}: expected InvalidParameter, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "Private DNS can't be enabled because the service {service_name} does not provide \
             a private DNS name."
        ),
        "case {case:?}: the refusal must name the service at fault, got {body}"
    );
}

/// The ARN of a Network Load Balancer of this account, for a service configuration to be
/// offered through.
fn network_load_balancer_arn() -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/net/private-link/0123456789abcdef",
        account = ::engine::emulator_account_id(),
    )
}

/// Every `dnsName` of a response's `dnsEntrySet`, read through the XML parser so the entries
/// of that one set are never confused with another set's.
fn dns_names(case: &str, body: &str) -> Vec<String> {
    let document = super::wire::document(case, body);
    document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "dnsEntrySet")
        .map(|node| {
            node.children()
                .filter(|item| item.is_element() && item.tag_name().name() == "item")
                .filter_map(|item| super::wire::child(item, "dnsName"))
                .map(|found| found.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}
