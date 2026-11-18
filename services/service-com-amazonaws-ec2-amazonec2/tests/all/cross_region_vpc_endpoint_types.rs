// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the Region half of the endpoint-type rule `CreateVpcEndpoint` holds a
//! request to, driven over `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: the types a VPC endpoint may be created as are the ones the endpoint
//! service it names is available through ("the type of service", `ServiceDetail$ServiceType`,
//! as `DescribeVpcEndpointServices` reports it), narrowed — when that service is hosted in
//! ANOTHER Region than the one the endpoint is created in ("the Region where the service is
//! hosted", `CreateVpcEndpointRequest$ServiceRegion`, which defaults to the Region the service
//! NAME carries) — to the types that reach a service across Regions: the interface endpoint,
//! which reaches its service over endpoint network interfaces in this VPC's own subnets. A
//! gateway endpoint reaches its service only through that service's prefix list in the route
//! tables of the service's OWN Region, and a Gateway Load Balancer endpoint only through a load
//! balancer of its own Region, so neither can carry traffic to a service of another Region: a
//! create asking for one is refused with `InvalidParameter` naming the requested type and the
//! types available, and stores nothing.
//!
//! Each row of the table below is a distinct input along what that rule generalises over: which
//! type was asked for (named, and defaulted), which Region hosts the service (this one, another
//! one, and another one named through `ServiceRegion` rather than through the name), and which
//! types the service itself offers — one reachable both ways, one gateway-only and one
//! interface-only — so the narrowing is exercised where it leaves one type, where it leaves the
//! type asked for, and where it leaves none at all.
//!
//! Every row states its own expected outcome and the types the refusal must name; each of those
//! lists is cross-checked against the types `DescribeVpcEndpointServices` reports for the
//! service, so no row can claim a type the catalogue does not hold, and a service of this Region
//! is asserted to have every reported type available to it — the narrowing a service of another
//! Region is the departure from.

use std::collections::BTreeSet;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for — the Region the endpoints are created in.
const REGION: &str = "us-west-2";

/// An Availability Zone of [`REGION`], for the subnet the interface endpoints sit in.
const ZONE: &str = "us-west-2a";

/// The Region whose services are reached ACROSS Regions: any Region of the catalogue other than
/// [`REGION`].
const OTHER_REGION: &str = "us-east-1";

/// The endpoint types that reach a service hosted in another Region: the interface endpoint,
/// which reaches its service over endpoint network interfaces in the endpoint's own VPC rather
/// than over anything the service's own Region publishes for it. Written out here, so this suite
/// states the rule rather than restating the implementation.
const CROSS_REGION_TYPES: &[&str] = &["Interface"];

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

/// The VPC, subnet and route table every endpoint below is created with.
struct Network {
    vpc_id: String,
    subnet_id: String,
    route_table_id: String,
}

/// Build that network through the API, so each create below names resources this emulator
/// really holds.
async fn network(router: &MultiServiceRouter) -> Network {
    let case = "the network";
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", "10.80.0.0/16")]).await;
    let vpc_id = member(case, &body, "vpcId");
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.80.1.0/24"),
            param("AvailabilityZone", ZONE),
        ],
    )
    .await;
    let subnet_id = member(case, &body, "subnetId");
    let body = ok(
        case,
        router,
        "CreateRouteTable",
        &[param("VpcId", &vpc_id)],
    )
    .await;
    let route_table_id = member(case, &body, "routeTableId");
    Network {
        vpc_id,
        subnet_id,
        route_table_id,
    }
}

/// The types `DescribeVpcEndpointServices` reports for the one service `service_name` of
/// `service_region`, in the order it reports them — ASKED of the catalogue rather than assumed
/// by this suite, so the expected half of every message below comes from the same catalogue the
/// create resolves against.
async fn reported_service_types(
    case: &str,
    router: &MultiServiceRouter,
    service_region: &str,
    service_name: &str,
) -> Vec<String> {
    let mut params = indexed("ServiceName", &[service_name]);
    params.extend(indexed("ServiceRegion", &[service_region]));
    let body = ok(case, router, "DescribeVpcEndpointServices", &params).await;
    // The member is a LIST of structures, so the set element and each entry's own member share
    // the one name: the tree is walked rather than the text scanned, and only the entries that
    // carry text — the leaves — are the service's types.
    let document = super::wire::document(case, &body);
    document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "serviceType")
        .filter_map(|node| node.text().map(str::to_string))
        .filter(|service_type| !service_type.trim().is_empty())
        .collect()
}

/// The message EC2 refuses an endpoint of a type that cannot reach its service with.
fn mismatch_message(endpoint_type: &str, available: &[&str]) -> String {
    format!(
        "Endpoint type ({endpoint_type}) does not match available service types ([{types}]).",
        types = available.join(", "),
    )
}

/// One row of the table: an endpoint type asked of a service of some Region, and the outcome
/// this suite expects for it.
struct Row {
    case: &'static str,
    /// `VpcEndpointType`, or `None` to leave the member out — "Default: Gateway".
    endpoint_type: Option<&'static str>,
    /// The Region hosting the service the create names.
    service_region: &'static str,
    /// Whether that Region is named through `ServiceRegion` as well as through the service name.
    names_service_region: bool,
    /// The service's label under its Region — `s3` in `com.amazonaws.us-east-1.s3`.
    service_label: &'static str,
    /// The types an endpoint of THIS Region may be created as to that service, in the order the
    /// catalogue reports them: every type the service is available through for a service of this
    /// Region, and only the ones that reach it across Regions for a service of another one.
    /// Written out per row rather than computed, so the row states the rule rather than
    /// restating the implementation — and checked against the types
    /// `DescribeVpcEndpointServices` reports for the service, which no row may claim a type
    /// outside.
    available: &'static [&'static str],
    /// Whether the create is expected to succeed: exactly whether `available` holds the type the
    /// row asks for.
    created: bool,
}

/// The rows.
///
/// The services are entries of the published catalogue every Region has, which
/// `DescribeVpcEndpointServices` reports and this suite asks for each service's types rather
/// than hard-coding them: Amazon S3 is reachable both ways, Amazon DynamoDB only through a
/// gateway endpoint, and Amazon CloudWatch Logs only through an interface endpoint.
const ROWS: &[Row] = &[
    Row {
        case: "a gateway endpoint to this Region's S3 service",
        endpoint_type: Some("Gateway"),
        service_region: REGION,
        names_service_region: false,
        service_label: "s3",
        available: &["Gateway", "Interface"],
        created: true,
    },
    Row {
        case: "a gateway endpoint to another Region's S3 service",
        endpoint_type: Some("Gateway"),
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "s3",
        available: &["Interface"],
        created: false,
    },
    Row {
        case: "the defaulted type, which is Gateway, to another Region's S3 service",
        endpoint_type: None,
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "s3",
        available: &["Interface"],
        created: false,
    },
    Row {
        case: "a gateway endpoint to another Region named through ServiceRegion too",
        endpoint_type: Some("Gateway"),
        service_region: OTHER_REGION,
        names_service_region: true,
        service_label: "s3",
        available: &["Interface"],
        created: false,
    },
    Row {
        case: "a Gateway Load Balancer endpoint to another Region's S3 service",
        endpoint_type: Some("GatewayLoadBalancer"),
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "s3",
        available: &["Interface"],
        created: false,
    },
    Row {
        case: "an interface endpoint to another Region's S3 service",
        endpoint_type: Some("Interface"),
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "s3",
        available: &["Interface"],
        created: true,
    },
    Row {
        case: "an interface endpoint to another Region named through ServiceRegion too",
        endpoint_type: Some("Interface"),
        service_region: OTHER_REGION,
        names_service_region: true,
        service_label: "s3",
        available: &["Interface"],
        created: true,
    },
    Row {
        case: "an interface endpoint to this Region's S3 service",
        endpoint_type: Some("Interface"),
        service_region: REGION,
        names_service_region: false,
        service_label: "s3",
        available: &["Gateway", "Interface"],
        created: true,
    },
    Row {
        case: "a gateway endpoint to this Region's gateway-only DynamoDB service",
        endpoint_type: Some("Gateway"),
        service_region: REGION,
        names_service_region: false,
        service_label: "dynamodb",
        available: &["Gateway"],
        created: true,
    },
    Row {
        case: "an interface endpoint to this Region's gateway-only DynamoDB service",
        endpoint_type: Some("Interface"),
        service_region: REGION,
        names_service_region: false,
        service_label: "dynamodb",
        available: &["Gateway"],
        created: false,
    },
    Row {
        case: "a gateway endpoint to another Region's gateway-only DynamoDB service",
        endpoint_type: Some("Gateway"),
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "dynamodb",
        available: &[],
        created: false,
    },
    Row {
        case: "an interface endpoint to another Region's gateway-only DynamoDB service",
        endpoint_type: Some("Interface"),
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "dynamodb",
        available: &[],
        created: false,
    },
    Row {
        case: "an interface endpoint to another Region's interface-only Logs service",
        endpoint_type: Some("Interface"),
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "logs",
        available: &["Interface"],
        created: true,
    },
    Row {
        case: "a gateway endpoint to another Region's interface-only Logs service",
        endpoint_type: Some("Gateway"),
        service_region: OTHER_REGION,
        names_service_region: false,
        service_label: "logs",
        available: &["Interface"],
        created: false,
    },
];

impl Row {
    /// The endpoint service name this row names: `com.amazonaws.<region>.<label>`, the form the
    /// `ServiceName` doc's own examples take.
    fn service_name(&self) -> String {
        format!("com.amazonaws.{}.{}", self.service_region, self.service_label)
    }

    /// The type the create asks for, defaulted the way the member documents it.
    fn requested_type(&self) -> &'static str {
        self.endpoint_type.unwrap_or("Gateway")
    }
}

/// The form fields one create sends: the route tables a gateway endpoint is reached through, and
/// a subnet for the types that stand up an endpoint network interface — an endpoint type that
/// stands up none refuses `SubnetIds` outright, and every other type refuses `RouteTableIds`.
fn create_params(row: &Row, built: &Network) -> Vec<Param> {
    let mut params = vec![
        param("VpcId", &built.vpc_id),
        param("ServiceName", &row.service_name()),
        // These rows are about the endpoint TYPE a service of some Region is reached through, so
        // they ask for no private hosted zone: a VPC you create has DNS hostnames off, which is
        // what an endpoint associating one needs.
        param("PrivateDnsEnabled", "false"),
    ];
    if let Some(endpoint_type) = row.endpoint_type {
        params.push(param("VpcEndpointType", endpoint_type));
    }
    if row.names_service_region {
        params.push(param("ServiceRegion", row.service_region));
    }
    if row.requested_type() == "Gateway" {
        params.push(param("RouteTableId.1", &built.route_table_id));
    } else {
        params.push(param("SubnetId.1", &built.subnet_id));
    }
    params
}

/// Every row: a type that can reach the service it names is created and reads back, and a type
/// that cannot — because the service is hosted in another Region, or because the service does
/// not offer it at all — is refused with `InvalidParameter` naming the requested type and the
/// types available, leaving no endpoint behind.
#[tokio::test]
async fn an_endpoint_is_created_only_for_a_type_that_reaches_its_service() {
    let router = emulator();
    let built = network(&router).await;

    for row in ROWS {
        let case = row.case;
        let requested = row.requested_type();
        let service_name = row.service_name();
        // The row's own expectation, cross-checked against the catalogue the create resolves
        // against: a type available to an endpoint is one the SERVICE is available through, so no
        // row may claim one `DescribeVpcEndpointServices` does not report for it — and for a
        // service of this Region every reported type is available, which is what the narrowing
        // below is a departure from.
        let service_types =
            reported_service_types(case, &router, row.service_region, &service_name).await;
        for available in row.available {
            assert!(
                service_types.iter().any(|reported| reported == available),
                "case {case:?}: {available:?} is not one of the types the catalogue reports for \
                 {service_name}, which are {service_types:?}"
            );
        }
        if row.service_region == REGION {
            let reported: Vec<&str> = service_types.iter().map(String::as_str).collect();
            assert_eq!(
                row.available,
                reported.as_slice(),
                "case {case:?}: every type a service of this Region is available through is \
                 available to an endpoint of it, the catalogue reports {service_types:?}"
            );
        }
        assert_eq!(
            row.created,
            row.available.contains(&requested),
            "case {case:?}: the row expects {created} for a type {requested:?} its available \
             types {available:?} {hold}",
            created = if row.created { "a create" } else { "a refusal" },
            available = row.available,
            hold = if row.available.contains(&requested) {
                "hold"
            } else {
                "do not hold"
            },
        );

        let before = ok(case, &router, "DescribeVpcEndpoints", &[]).await;
        let before = elements(&before, "vpcEndpointId");
        let (status, body) = call(&router, "CreateVpcEndpoint", &create_params(row, &built)).await;
        let after = ok(case, &router, "DescribeVpcEndpoints", &[]).await;
        let after = elements(&after, "vpcEndpointId");

        if !row.created {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for a type that cannot reach the service, \
                 got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_PARAMETER,
                "case {case:?}: expected {}, got {body}",
                ec2::state::INVALID_PARAMETER
            );
            assert_eq!(
                error_message(&body),
                mismatch_message(requested, row.available),
                "case {case:?}: expected the message to name the requested type and the types \
                 available {available:?}, got {body}",
                available = row.available
            );
            assert_eq!(
                after, before,
                "case {case:?}: a refused create must store nothing, the account holds \
                 {after:?} where it held {before:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for a type that reaches the service, got {status} \
             with body {body}"
        );
        let endpoint_id = member(case, &body, "vpcEndpointId");
        // The create ANSWERS the stored record, and the Region hosting the service is part of
        // it: an endpoint to another Region's service reports that Region.
        assert_eq!(
            member(case, &body, "serviceRegion"),
            row.service_region,
            "case {case:?}: the endpoint reports the Region hosting its service, got {body}"
        );
        // The round trip: the created endpoint is the record the store holds, read back by the
        // id the create minted.
        let read = ok(
            case,
            &router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[&endpoint_id]),
        )
        .await;
        for element_name in ["serviceName", "serviceRegion", "vpcEndpointType"] {
            assert_eq!(
                element(&read, element_name),
                element(&body, element_name),
                "case {case:?}: the describe must report what the create answered for \
                 <{element_name}>: {read}"
            );
        }
        assert!(
            after.contains(&endpoint_id),
            "case {case:?}: expected the account to hold {endpoint_id:?}, it holds {after:?}"
        );
    }
}

/// The same narrowing holds for an endpoint service this ACCOUNT configured: it is hosted in the
/// Region it was created in, so an endpoint of another Region to it is held to the cross-Region
/// types — and a `ServiceRegion` naming a Region that does not host it names no service at all,
/// which is `InvalidServiceName` rather than a type mismatch.
#[tokio::test]
async fn a_service_name_is_resolved_in_the_region_said_to_host_it() {
    let router = emulator();
    let built = network(&router).await;
    let case = "a Gateway Load Balancer service of this account";

    // The service, configured through the API: "before you create an endpoint service, you must
    // create one of the following for your service" — the load balancer behind it, whose KIND is
    // what decides the service's type.
    let body = ok(
        case,
        &router,
        "CreateVpcEndpointServiceConfiguration",
        &[param(
            "GatewayLoadBalancerArn.1",
            &format!(
                "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/gwy/mirror/\
                 0123456789abcdef",
                account = engine::emulator_account_id(),
            ),
        )],
    )
    .await;
    let service_name = member(case, &body, "serviceName");

    // `(the case, the Region named for the service, the type asked for, what it answers)`.
    // A configuration is hosted in the Region it was created in, so the name resolves there and
    // nowhere else; the one type its load balancer offers is not a cross-Region type, so naming
    // another Region leaves no type at all.
    let rows: [(&str, Option<&str>, &str, &str); 4] = [
        (
            "the type its load balancer offers, in its own Region",
            None,
            "GatewayLoadBalancer",
            "created",
        ),
        (
            "that same type with its own Region named explicitly",
            Some(REGION),
            "GatewayLoadBalancer",
            "created",
        ),
        (
            "that same type with another Region named for the service",
            Some(OTHER_REGION),
            "GatewayLoadBalancer",
            ec2::state::INVALID_SERVICE_NAME,
        ),
        (
            "an interface endpoint to it in its own Region",
            None,
            "Interface",
            ec2::state::INVALID_PARAMETER,
        ),
    ];

    let mut created: BTreeSet<String> = BTreeSet::new();
    for (case, service_region, endpoint_type, expected) in rows {
        let mut params = vec![
            param("VpcId", &built.vpc_id),
            param("ServiceName", &service_name),
            param("VpcEndpointType", endpoint_type),
            param("SubnetId.1", &built.subnet_id),
        ];
        if let Some(service_region) = service_region {
            params.push(param("ServiceRegion", service_region));
        }
        let (status, body) = call(&router, "CreateVpcEndpoint", &params).await;
        if expected != "created" {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                expected,
                "case {case:?}: expected {expected}, got {body}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            member(case, &body, "serviceRegion"),
            REGION,
            "case {case:?}: the endpoint reports the Region hosting its service, got {body}"
        );
        assert!(
            created.insert(member(case, &body, "vpcEndpointId")),
            "case {case:?}: expected a fresh endpoint id, got one this suite already holds"
        );
    }
}

/// A name of the PARTNER form (`com.amazonaws.vpce.<region>.<service-id>`) names a service
/// another account or a Marketplace partner offers, which this emulator holds no catalogue of: in
/// its OWN Region there is no type list to hold the create to, so it is read for its form alone.
/// Which Region HOSTS it is still known — the name carries it — so the narrowing holds for one of
/// another Region all the same: only an interface endpoint reaches it, whatever types its owner
/// may offer.
#[tokio::test]
async fn a_partner_service_of_another_region_is_held_to_the_cross_region_types() {
    let router = emulator();
    let built = network(&router).await;

    // `(the case, the Region hosting the service, the type asked for, whether it is created)`.
    let rows: [(&str, &str, &str, bool); 5] = [
        (
            "a Gateway Load Balancer endpoint to a partner service of this Region",
            REGION,
            "GatewayLoadBalancer",
            true,
        ),
        (
            "an interface endpoint to a partner service of this Region",
            REGION,
            "Interface",
            true,
        ),
        (
            "an interface endpoint to a partner service of another Region",
            OTHER_REGION,
            "Interface",
            true,
        ),
        (
            "a Gateway Load Balancer endpoint to a partner service of another Region",
            OTHER_REGION,
            "GatewayLoadBalancer",
            false,
        ),
        (
            "a gateway endpoint to a partner service of another Region",
            OTHER_REGION,
            "Gateway",
            false,
        ),
    ];

    for (case, service_region, endpoint_type, created) in rows {
        let service_name =
            format!("com.amazonaws.vpce.{service_region}.vpce-svc-0a1b2c3d4e5f6a7b8");
        let mut params = vec![
            param("VpcId", &built.vpc_id),
            param("ServiceName", &service_name),
            param("VpcEndpointType", endpoint_type),
            param("PrivateDnsEnabled", "false"),
        ];
        // A gateway endpoint takes no subnet; every other type is reached through one.
        if endpoint_type != "Gateway" {
            params.push(param("SubnetId.1", &built.subnet_id));
        }
        let before = ok(case, &router, "DescribeVpcEndpoints", &[]).await;
        let before = elements(&before, "vpcEndpointId");
        let (status, body) = call(&router, "CreateVpcEndpoint", &params).await;
        let after = ok(case, &router, "DescribeVpcEndpoints", &[]).await;
        let after = elements(&after, "vpcEndpointId");

        if !created {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for a type that cannot reach a service of \
                 another Region, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_PARAMETER,
                "case {case:?}: expected {}, got {body}",
                ec2::state::INVALID_PARAMETER
            );
            assert_eq!(
                error_message(&body),
                mismatch_message(endpoint_type, CROSS_REGION_TYPES),
                "case {case:?}: expected the message to name the requested type and the types \
                 that reach a service of another Region, got {body}"
            );
            assert_eq!(
                after, before,
                "case {case:?}: a refused create must store nothing, the account holds {after:?} \
                 where it held {before:?}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            member(case, &body, "serviceRegion"),
            service_region,
            "case {case:?}: the endpoint reports the Region hosting its service, got {body}"
        );
        assert_eq!(
            member(case, &body, "serviceName"),
            service_name,
            "case {case:?}: the endpoint names the service it was created to, got {body}"
        );
    }
}
