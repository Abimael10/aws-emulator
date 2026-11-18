// SPDX-License-Identifier: CC0-1.0

//! The PRIVATE DNS of an interface VPC endpoint, and the DOMAIN the endpoint service it is to
//! answers on — driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rules asserted here, each over a TABLE rather than over one request:
//!
//! 1. An endpoint service name is the REVERSE-DOMAIN spelling of the host the service answers
//!    on, so the default public DNS name of `com.amazonaws.<region>.<label>` is the label's
//!    dotted components in reverse order under `<region>.amazonaws.com` — `ecr.dkr` answers on
//!    `dkr.ecr.<region>.amazonaws.com` (Amazon ECR User Guide, "Amazon ECR interface VPC
//!    endpoints") — and the names an endpoint TO it answers on sit under the same host.
//!
//! 2. What a published service OFFERS is a fact of that service: one that supports no endpoint
//!    policy reports `VpcEndpointPolicySupported=false`, and one that publishes no default
//!    public DNS name reports no `PrivateDnsName` — so `PrivateDnsEnabled` defaults to ON for a
//!    service that publishes one and OFF for a service that does not, and asking for it on a
//!    service with none is refused.
//!
//! 3. A private hosted zone answers ONE domain inside a VPC, so it belongs to at most one
//!    endpoint per VPC per service: a second endpoint of that VPC claiming the same service's
//!    domain is refused, whether a create or a modify asks for it, while another service's
//!    domain in the same VPC, the same service's domain in another VPC, and the same domain
//!    again once the endpoint holding it gives it up are all served.
//!
//! 4. A reported endpoint carries every ID LIST it has as a list — the ids the request named for
//!    it, EMPTY where it named none — rather than leaving the member out for an endpoint of a
//!    type that names none.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::{child, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

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

/// A VPC with the two DNS attributes a private hosted zone needs, and one subnet in it: the
/// network every endpoint below is created in.
struct Network {
    vpc_id: String,
    subnet_id: String,
}

/// Build that network through the API, so every endpoint below is created against resources the
/// emulator actually stores.
///
/// "To use a private hosted zone, you must set the following VPC attributes to true:
/// enableDnsHostnames and enableDnsSupport" (`CreateVpcEndpointRequest$PrivateDnsEnabled`), and
/// a VPC a caller creates has hostnames OFF (Amazon VPC User Guide, "DNS attributes in your
/// VPC") — so the network turns them on and leaves the rows free to be about the endpoint.
async fn network(case: &str, router: &MultiServiceRouter, prefix: &str) -> Network {
    let body = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", &format!("{prefix}.0.0/16"))],
    )
    .await;
    let vpc_id = member(case, &body, "vpcId");
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

/// Every `dnsName` of a response's `dnsEntrySet`, read through the XML parser so one set's
/// entries are never read as another's.
fn dns_names(case: &str, body: &str) -> Vec<String> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "dnsEntrySet")
        .map(|set| {
            set.children()
                .filter(|item| item.is_element() && item.tag_name().name() == "item")
                .filter_map(|item| child(item, "dnsName"))
                .map(|found| found.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rule 1 and 2: the domain a service answers on, and what it offers
// ---------------------------------------------------------------------------

/// One row: a published endpoint service, the HOST its label names read as reverse domain, and
/// what the service offers.
struct ServiceRow {
    case: &'static str,
    /// The service's whole name, in its own family's form.
    service_name: &'static str,
    /// The default public DNS name the catalogue must report, or `None` for a service that
    /// publishes none. Written out here rather than derived, so the suite pins the names and not
    /// the code that builds them.
    private_dns_name: Option<&'static str>,
    /// The base endpoint DNS name an interface endpoint's own names sit under, in the
    /// PrivateLink endpoint domain this emulator publishes its endpoints in — the loopback
    /// zone, since a name a consumer resolves has to lead back to this process.
    base_dns_name: &'static str,
    /// Whether an endpoint to it takes an endpoint policy.
    policy_supported: bool,
}

/// The rows: a one-component label, two dotted labels (so the reversal is visible), the AWS IoT
/// Core data-plane service — which publishes no private DNS name and supports no endpoint
/// policy — and a service of the `aws.api` family.
const SERVICE_ROWS: [ServiceRow; 5] = [
    ServiceRow {
        case: "a one-component label",
        service_name: "com.amazonaws.us-west-2.logs",
        private_dns_name: Some("logs.us-west-2.amazonaws.com"),
        base_dns_name: "logs.us-west-2.vpce.localhost",
        policy_supported: true,
    },
    ServiceRow {
        case: "the Docker registry label, reversed",
        service_name: "com.amazonaws.us-west-2.ecr.dkr",
        private_dns_name: Some("dkr.ecr.us-west-2.amazonaws.com"),
        base_dns_name: "dkr.ecr.us-west-2.vpce.localhost",
        policy_supported: true,
    },
    ServiceRow {
        case: "the AppStream streaming label, reversed",
        service_name: "com.amazonaws.us-west-2.appstream.streaming",
        private_dns_name: Some("streaming.appstream.us-west-2.amazonaws.com"),
        base_dns_name: "streaming.appstream.us-west-2.vpce.localhost",
        // The streaming data plane carries sessions rather than API calls, so it supports no
        // endpoint policy even though it publishes a private DNS name of its own — the two
        // facts of a service's offering are independent.
        policy_supported: false,
    },
    ServiceRow {
        case: "the AWS IoT Core data plane, which offers neither",
        service_name: "com.amazonaws.us-west-2.iot.data",
        private_dns_name: None,
        base_dns_name: "data.iot.us-west-2.vpce.localhost",
        policy_supported: false,
    },
    ServiceRow {
        case: "a service of the aws.api family",
        service_name: "aws.api.us-west-2.emr-service-cell01",
        private_dns_name: Some("emr-service-cell01.us-west-2.api.aws"),
        base_dns_name: "emr-service-cell01.us-west-2.vpce.localhost",
        policy_supported: true,
    },
];

/// The catalogue reports each service's own domain and its own offering: the host its name
/// names read as reverse domain, and the endpoint-policy support of that service.
#[tokio::test]
async fn the_catalogue_reports_each_services_own_domain_and_offering() {
    let router = emulator();

    for row in &SERVICE_ROWS {
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
            "case {case:?}: only a service with a name of its own has one to verify, got {body}"
        );
        let base_names: Vec<String> = document(case, &body)
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "baseEndpointDnsNameSet")
            .map(|set| {
                set.children()
                    .filter(|item| item.is_element() && item.tag_name().name() == "item")
                    .map(|item| item.text().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            base_names.iter().any(|name| name == row.base_dns_name),
            "case {case:?}: expected the base endpoint DNS name {:?} among {base_names:?}",
            row.base_dns_name
        );
        assert_eq!(
            element(&body, "vpcEndpointPolicySupported").as_deref(),
            Some(if row.policy_supported { "true" } else { "false" }),
            "case {case:?}: expected the service's own endpoint-policy support, got {body}"
        );
    }
}

/// An omitted `PrivateDnsEnabled` follows the SERVICE: on for one that publishes a default
/// public DNS name — the endpoint then answering on that name too — and off for one that does
/// not, which is also what asking for it explicitly is refused for.
#[tokio::test]
async fn private_dns_defaults_to_what_the_service_publishes() {
    let router = emulator();

    for (index, row) in SERVICE_ROWS.iter().enumerate() {
        let case = row.case;
        // One VPC per row, each with a range of its own, so no row's endpoint can claim a
        // domain another row's holds.
        let built = network(case, &router, &format!("10.{}", 80 + index)).await;
        let body = ok(
            case,
            &router,
            "CreateVpcEndpoint",
            &interface_params(&built, row.service_name, Vec::new()),
        )
        .await;
        let endpoint_id = member(case, &body, "vpcEndpointId");
        let expected = row.private_dns_name.is_some();
        assert_eq!(
            element(&body, "privateDnsEnabled").as_deref(),
            Some(if expected { "true" } else { "false" }),
            "case {case:?}: expected privateDnsEnabled {expected}, got {body}"
        );
        // The endpoint's own names sit under the service's base name, and the service's public
        // name is among them exactly while the private hosted zone is associated.
        let described = ok(
            case,
            &router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
        )
        .await;
        assert_eq!(
            element(&described, "privateDnsEnabled").as_deref(),
            Some(if expected { "true" } else { "false" }),
            "case {case:?}: the stored endpoint must report the same association, got {described}"
        );
        let names = dns_names(case, &described);
        assert!(
            names.iter().any(|name| name
                == &format!(
                    "{endpoint_id}-{suffix}.{base}",
                    suffix = ec2::state::vpc_endpoint_dns_suffix(&endpoint_id),
                    base = row.base_dns_name,
                )),
            "case {case:?}: expected the endpoint's regional name under {:?}, got {names:?}",
            row.base_dns_name
        );
        assert_eq!(
            row.private_dns_name
                .is_some_and(|private| names.iter().any(|name| name == private)),
            expected,
            "case {case:?}: expected the service's public name present only while private DNS \
             is on, got {names:?}"
        );

        // And a request that ASKS for private DNS on a service with none is refused, naming
        // that service.
        if row.private_dns_name.is_none() {
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
                    "Private DNS can't be enabled because the service {service} does not \
                     provide a private DNS name.",
                    service = row.service_name,
                ),
                "case {case:?}: the refusal must name the service at fault, got {body}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3: one private hosted zone per VPC per service
// ---------------------------------------------------------------------------

/// The sentence EC2 refuses a second claim on one domain with, built from the domain and the
/// VPC the request named.
fn conflict_message(private_dns_name: &str, vpc_id: &str) -> String {
    format!(
        "private-dns-enabled cannot be set because there is already a conflicting DNS domain \
         for {private_dns_name} in the VPC {vpc_id}"
    )
}

/// A second endpoint of one VPC claiming one service's domain is refused — while another
/// service's domain in that VPC, and the same service's domain in another VPC, are served.
#[tokio::test]
async fn one_vpc_holds_one_private_dns_endpoint_per_service() {
    let router = emulator();
    let first = network("the first VPC", &router, "10.71").await;
    let second = network("the second VPC", &router, "10.72").await;
    let service = "com.amazonaws.us-west-2.logs";
    let other_service = "com.amazonaws.us-west-2.ecr.dkr";

    // The endpoint that takes the domain. Its private DNS is the service's default.
    let body = ok(
        "the endpoint that holds the domain",
        &router,
        "CreateVpcEndpoint",
        &interface_params(&first, service, Vec::new()),
    )
    .await;
    let holder = member("the endpoint that holds the domain", &body, "vpcEndpointId");

    // (case, the VPC, the service, what the request says about private DNS, the refusal it
    //  earns — or `None` for a request that is served)
    let rows: [(&str, &Network, &str, Option<&str>, bool); 5] = [
        (
            "the same service in the same VPC, saying nothing",
            &first,
            service,
            None,
            true,
        ),
        (
            "the same service in the same VPC, asking for private DNS",
            &first,
            service,
            Some("true"),
            true,
        ),
        (
            "the same service in the same VPC, asking for no private DNS",
            &first,
            service,
            Some("false"),
            false,
        ),
        (
            "another service in the same VPC",
            &first,
            other_service,
            Some("true"),
            false,
        ),
        (
            "the same service in another VPC",
            &second,
            service,
            Some("true"),
            false,
        ),
    ];

    for (case, built, service_name, asked, refused) in rows {
        let extra = asked.map_or_else(Vec::new, |asked| vec![param("PrivateDnsEnabled", asked)]);
        let (status, body) = call(
            &router,
            "CreateVpcEndpoint",
            &interface_params(built, service_name, extra),
        )
        .await;
        if refused {
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
                conflict_message("logs.us-west-2.amazonaws.com", &built.vpc_id),
                "case {case:?}: the refusal must name the domain and the VPC, got {body}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
        }
    }

    // A modify that would take the domain for a second endpoint is refused on the same terms,
    // and the endpoint that asked is left as it was.
    let body = ok(
        "the endpoint without the domain",
        &router,
        "CreateVpcEndpoint",
        &interface_params(
            &first,
            service,
            vec![param("PrivateDnsEnabled", "false")],
        ),
    )
    .await;
    let other = member("the endpoint without the domain", &body, "vpcEndpointId");
    let (status, body) = call(
        &router,
        "ModifyVpcEndpoint",
        &[
            param("VpcEndpointId", &other),
            param("PrivateDnsEnabled", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "a modify claiming the held domain: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_message(&body),
        conflict_message("logs.us-west-2.amazonaws.com", &first.vpc_id),
        "a modify claiming the held domain: expected the conflict sentence, got {body}"
    );

    // Once the endpoint that holds the domain gives it up, the same request is served — and the
    // endpoint that asked for it reports the association and answers on that name.
    let _ = ok(
        "the holder gives the domain up",
        &router,
        "ModifyVpcEndpoint",
        &[
            param("VpcEndpointId", &holder),
            param("PrivateDnsEnabled", "false"),
        ],
    )
    .await;
    let _ = ok(
        "the domain changes hands",
        &router,
        "ModifyVpcEndpoint",
        &[
            param("VpcEndpointId", &other),
            param("PrivateDnsEnabled", "true"),
        ],
    )
    .await;
    let described = ok(
        "the domain changes hands",
        &router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[other.as_str()]),
    )
    .await;
    assert_eq!(
        element(&described, "privateDnsEnabled").as_deref(),
        Some("true"),
        "the endpoint that took the domain must report the association, got {described}"
    );
    let names = dns_names("the domain changes hands", &described);
    assert!(
        names
            .iter()
            .any(|name| name == "logs.us-west-2.amazonaws.com"),
        "the endpoint that took the domain must answer on it, got {names:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 4: every id list is reported as a list
// ---------------------------------------------------------------------------

/// Whether `body`'s one endpoint carries the set `name`, and how many items it holds.
fn reported_set(case: &str, body: &str, name: &str) -> Option<usize> {
    let parsed = document(case, body);
    let endpoint = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "vpcEndpointId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no endpoint in {body}"));
    child(endpoint, name).map(|_| set_items(endpoint, name).len())
}

/// Both kinds of endpoint report every id list they have: an interface endpoint names no route
/// table and reports an EMPTY route-table list, and a gateway endpoint — which is reached
/// through a route rather than through interfaces — reports empty subnet, group and interface
/// lists beside the tables it does name.
#[tokio::test]
async fn an_endpoint_reports_every_id_list_it_has_even_when_it_names_none() {
    let router = emulator();
    let built = network("the id lists", &router, "10.73").await;

    // An interface endpoint: subnets, groups and interfaces of its own, and no route table.
    let created = ok(
        "an interface endpoint",
        &router,
        "CreateVpcEndpoint",
        &interface_params(&built, "com.amazonaws.us-west-2.logs", Vec::new()),
    )
    .await;
    let interface_id = member("an interface endpoint", &created, "vpcEndpointId");
    let described = ok(
        "an interface endpoint",
        &router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[interface_id.as_str()]),
    )
    .await;
    for (body, read) in [(&created, "the create"), (&described, "the describe")] {
        let case = format!("an interface endpoint, {read}");
        assert_eq!(
            reported_set(&case, body, "routeTableIdSet"),
            Some(0),
            "case {case:?}: an interface endpoint names no route table and reports an empty \
             list, got {body}"
        );
        assert_eq!(
            reported_set(&case, body, "subnetIdSet"),
            Some(1),
            "case {case:?}: expected the one subnet the request named, got {body}"
        );
        assert_eq!(
            reported_set(&case, body, "networkInterfaceIdSet"),
            Some(1),
            "case {case:?}: expected one endpoint network interface, got {body}"
        );
    }

    // A gateway endpoint of the same VPC, with one route table named: it is reached through that
    // table's route, so it has no subnet, no group and no interface of its own.
    let table = {
        let body = ok(
            "a gateway endpoint",
            &router,
            "CreateRouteTable",
            &[param("VpcId", &built.vpc_id)],
        )
        .await;
        member("a gateway endpoint", &body, "routeTableId")
    };
    let created = ok(
        "a gateway endpoint",
        &router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &built.vpc_id),
            param("ServiceName", "com.amazonaws.us-west-2.s3"),
            param("VpcEndpointType", "Gateway"),
            param("RouteTableId.1", &table),
        ],
    )
    .await;
    let gateway_id = member("a gateway endpoint", &created, "vpcEndpointId");
    let described = ok(
        "a gateway endpoint",
        &router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[gateway_id.as_str()]),
    )
    .await;
    for (body, read) in [(&created, "the create"), (&described, "the describe")] {
        let case = format!("a gateway endpoint, {read}");
        assert_eq!(
            reported_set(&case, body, "routeTableIdSet"),
            Some(1),
            "case {case:?}: expected the one route table the request named, got {body}"
        );
        for empty in ["subnetIdSet", "groupSet", "networkInterfaceIdSet"] {
            assert_eq!(
                reported_set(&case, body, empty),
                Some(0),
                "case {case:?}: a gateway endpoint reports an empty {empty}, got {body}"
            );
        }
    }
}
