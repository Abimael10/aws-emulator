// SPDX-License-Identifier: CC0-1.0

//! The teardown of a VPC that a VPC endpoint was created in, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: every resource a caller can create in a VPC has an API that removes
//! it, and `DependencyViolation` names only a VPC something still holds. So a
//! `CreateVpcEndpoint` naming route tables succeeds and stores the endpoint; the endpoint,
//! the explicit route-table association, the route table and the internet gateway are each
//! removed by their own operation; and once nothing holds the VPC any more `DeleteVpc`
//! succeeds and `DescribeVpcs` stops reporting it — while a `DeleteVpc` attempted with a
//! gateway still attached is refused with `DependencyViolation` and leaves the VPC in place.
//!
//! The rows differ along what the rule generalises over: which endpoint service the endpoint
//! is created to, which endpoint TYPE it is (a gateway endpoint, which associates route
//! tables and stands up no interface, and an interface endpoint, which stands up one network
//! interface per subnet), how many route tables the create associates (none, one, several),
//! and whether a subnet holds an explicit association that has to be disassociated first.
//! None of them changes the outcome: the VPC is gone at the end of every row.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, emulator, error_code, error_message, indexed};
use super::wire::{child_text, document, is_long_id, param, set_items, Param};

/// The Region every call here is signed for, and the Region of the endpoint services below.
const REGION: &str = "us-east-1";

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

/// One element of a response body, or a panic naming the case and showing the body.
///
/// Named for what it reads rather than `member`, so it is not read as
/// [`super::wire::member`], which is the one VPC a response carries.
fn required_element(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// Which endpoint type a row creates, and so what its create names and its teardown has to
/// take away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A gateway endpoint: reached through route tables, standing up no interface.
    Gateway,
    /// An interface endpoint: an endpoint network interface in each subnet it names.
    Interface,
}

/// One row: an endpoint of one kind to one endpoint service, with some number of route
/// tables, in a VPC that also holds an attached internet gateway.
#[derive(Debug)]
struct TeardownCase {
    case: &'static str,
    /// The CIDR block the row's VPC is created with.
    cidr_block: &'static str,
    /// The endpoint service the endpoint is created to.
    service_name: &'static str,
    kind: Kind,
    /// Whether the create names `VpcEndpointType` or leaves it to the documented default
    /// (`Gateway`).
    names_type: bool,
    /// How many route tables of the VPC the create associates — a gateway endpoint's
    /// `RouteTableIds`.
    route_tables: usize,
    /// Whether the row also associates a subnet with its first route table explicitly, so the
    /// teardown has to `DisassociateRouteTable` before the table can go.
    associates_subnet: bool,
}

/// The rows: both gateway endpoint services the Region publishes a prefix list for, with no,
/// one and several route tables, the defaulted spelling of the type, a subnet association to
/// undo, and an interface endpoint whose teardown has to take an endpoint network interface
/// with it.
///
/// The service names are read off the endpoint-service catalogue
/// `DescribeVpcEndpointServices` reports from, not written for one recorded call.
fn teardown_cases() -> Vec<TeardownCase> {
    vec![
        TeardownCase {
            case: "a gateway endpoint to Amazon S3 with no route table at all",
            cidr_block: "10.20.0.0/16",
            service_name: "com.amazonaws.us-east-1.s3",
            kind: Kind::Gateway,
            names_type: true,
            route_tables: 0,
            associates_subnet: false,
        },
        TeardownCase {
            case: "a gateway endpoint to Amazon S3 associated with one route table",
            cidr_block: "10.21.0.0/16",
            service_name: "com.amazonaws.us-east-1.s3",
            kind: Kind::Gateway,
            names_type: true,
            route_tables: 1,
            associates_subnet: false,
        },
        TeardownCase {
            case: "a gateway endpoint to Amazon DynamoDB associated with three route tables",
            cidr_block: "10.22.0.0/16",
            service_name: "com.amazonaws.us-east-1.dynamodb",
            kind: Kind::Gateway,
            names_type: true,
            route_tables: 3,
            associates_subnet: false,
        },
        TeardownCase {
            case: "a gateway endpoint with the type left to the documented default, its table \
                   used by a subnet",
            cidr_block: "10.23.0.0/16",
            service_name: "com.amazonaws.us-east-1.s3",
            kind: Kind::Gateway,
            names_type: false,
            route_tables: 1,
            associates_subnet: true,
        },
        TeardownCase {
            case: "an interface endpoint to Amazon CloudWatch Logs in one subnet",
            cidr_block: "10.24.0.0/16",
            service_name: "com.amazonaws.us-east-1.logs",
            kind: Kind::Interface,
            names_type: true,
            route_tables: 0,
            associates_subnet: false,
        },
    ]
}

/// Every route of `route_table_id` that points at the gateway `gateway_id`, as the
/// destination prefix list each one matches on — a gateway endpoint's own endpoint route,
/// which the create adds to each table it names and the delete takes away again.
async fn routes_to_gateway(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    gateway_id: &str,
) -> Vec<String> {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &indexed("RouteTableId", &[route_table_id]),
    )
    .await;
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "routeTableSet")
        .into_iter()
        .flat_map(|table| set_items(table, "routeSet"))
        .filter(|route| child_text(*route, "gatewayId").as_deref() == Some(gateway_id))
        .map(|route| child_text(route, "destinationPrefixListId").unwrap_or_default())
        .collect()
}

/// The ids of every association the route table `route_table_id` is reported with — an
/// explicit association is a pair, so it has to stop being reported the moment either side
/// lets go of it.
async fn associations_of(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
) -> Vec<String> {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &indexed("RouteTableId", &[route_table_id]),
    )
    .await;
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "routeTableSet")
        .into_iter()
        .flat_map(|table| set_items(table, "associationSet"))
        .filter_map(|association| child_text(association, "routeTableAssociationId"))
        .collect()
}

/// `DeleteVpc` has to be refused with `DependencyViolation`, in the sentence AWS words that
/// error class with, and leave the VPC describable. `held_by` names what still holds it; it
/// is deliberately NOT expected in the message, AWS's refusal naming the VPC the request
/// named and nothing else.
async fn refused_while_held(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    held_by: &str,
) {
    let (status, body) = call(router, "DeleteVpc", &[param("VpcId", vpc_id)]).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected DeleteVpc to be refused while {held_by} holds {vpc_id}, got \
         {status} with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, "DependencyViolation",
        "case {case:?}: expected DependencyViolation while {held_by} holds {vpc_id}, got \
         {code:?} in {body}"
    );
    let expected = format!("The vpc '{vpc_id}' has dependencies and cannot be deleted.");
    let message = error_message(&body);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
    // The refusal removed nothing: the VPC is still the account's.
    let read = ok(case, router, "DescribeVpcs", &indexed("VpcId", &[vpc_id])).await;
    let reported = elements(&read, "vpcId");
    assert!(
        reported.iter().any(|reported| reported == vpc_id),
        "case {case:?}: expected {vpc_id} to still be describable after the refusal, got \
         {reported:?}"
    );
}

/// A VPC with an endpoint, an attached internet gateway and some route tables can be torn
/// down completely: every resource has an operation that removes it, and the VPC is gone
/// afterwards.
#[tokio::test]
async fn every_resource_a_vpc_endpoint_create_needs_can_be_torn_down_again() {
    for row in teardown_cases() {
        let case = row.case;
        // One emulator per row, so the "nothing of this VPC is left" reads at the end are
        // about this row's resources alone.
        let (router, _engine) = emulator();

        let vpc_id = {
            let body = ok(
                case,
                &router,
                "CreateVpc",
                &[param("CidrBlock", row.cidr_block)],
            )
            .await;
            required_element(case, &body, "vpcId")
        };
        let gateway_id = {
            let body = ok(case, &router, "CreateInternetGateway", &[]).await;
            required_element(case, &body, "internetGatewayId")
        };
        ok(
            case,
            &router,
            "AttachInternetGateway",
            &[
                param("InternetGatewayId", &gateway_id),
                param("VpcId", &vpc_id),
            ],
        )
        .await;

        // A subnet is created for the row that needs one: an interface endpoint is created IN
        // subnets, and an explicit route-table association is a subnet's.
        let subnet_id = if row.kind == Kind::Interface || row.associates_subnet {
            let body = ok(
                case,
                &router,
                "CreateSubnet",
                &[
                    param("VpcId", &vpc_id),
                    param(
                        "CidrBlock",
                        &row.cidr_block.replace("0.0/16", "1.0/24"),
                    ),
                ],
            )
            .await;
            Some(required_element(case, &body, "subnetId"))
        } else {
            None
        };

        let mut tables: Vec<String> = Vec::new();
        for _ in 0..row.route_tables {
            let body = ok(case, &router, "CreateRouteTable", &[param("VpcId", &vpc_id)]).await;
            tables.push(required_element(case, &body, "routeTableId"));
        }

        // The create under test: `RouteTableIds` is an ordinary member of the request, so a
        // create that names them is answered with a stored endpoint rather than refused.
        let mut params = vec![
            param("VpcId", &vpc_id),
            param("ServiceName", row.service_name),
        ];
        if row.names_type {
            params.push(param(
                "VpcEndpointType",
                match row.kind {
                    Kind::Gateway => "Gateway",
                    Kind::Interface => "Interface",
                },
            ));
        }
        let named: Vec<&str> = tables.iter().map(String::as_str).collect();
        params.extend(indexed("RouteTableId", &named));
        if row.kind == Kind::Interface {
            let subnet = subnet_id.as_deref().expect("an interface row has a subnet");
            params.extend(indexed("SubnetId", &[subnet]));
            // "Enabling private DNS requires both enableDnsSupport and enableDnsHostnames VPC
            // attributes set to true", and a VPC a caller created has only the first — so the
            // association is turned off rather than left to the service's default, this row
            // being about the teardown and not about private DNS.
            params.push(param("PrivateDnsEnabled", "false"));
        }
        let created = ok(case, &router, "CreateVpcEndpoint", &params).await;
        let endpoint_id = required_element(case, &created, "vpcEndpointId");
        assert!(
            is_long_id("vpce", &endpoint_id),
            "case {case:?}: expected a vpce- endpoint id, got {endpoint_id:?} in {created}"
        );
        let reported_tables = element(&created, "routeTableIdSet")
            .map(|set| elements(&set, "item"))
            .unwrap_or_default();
        assert_eq!(
            reported_tables, tables,
            "case {case:?}: expected the endpoint to report the tables the create named, got \
             {created}"
        );
        // The endpoint network interfaces the create stood up are the account's: an interface
        // endpoint takes one in each subnet it names, and a gateway endpoint none at all.
        let interfaces = element(&created, "networkInterfaceIdSet")
            .map(|set| elements(&set, "item"))
            .unwrap_or_default();
        let expected_interfaces = match row.kind {
            Kind::Gateway => 0,
            Kind::Interface => 1,
        };
        assert_eq!(
            interfaces.len(),
            expected_interfaces,
            "case {case:?}: expected {expected_interfaces} endpoint network interface(s), got \
             {interfaces:?} in {created}"
        );
        for network_interface_id in &interfaces {
            let reported = ok(
                case,
                &router,
                "DescribeNetworkInterfaces",
                &indexed("NetworkInterfaceId", &[network_interface_id]),
            )
            .await;
            // The interface the create answered is the one the store holds, in the VPC and the
            // subnet the endpoint was created in.
            assert_eq!(
                element(&reported, "networkInterfaceId").as_deref(),
                Some(network_interface_id.as_str()),
                "case {case:?}: expected {network_interface_id} to be reported, got {reported}"
            );
            assert_eq!(
                element(&reported, "vpcId").as_deref(),
                Some(vpc_id.as_str()),
                "case {case:?}: expected {network_interface_id} to sit in {vpc_id}, got \
                 {reported}"
            );
            assert_eq!(
                element(&reported, "subnetId").as_deref(),
                subnet_id.as_deref(),
                "case {case:?}: expected {network_interface_id} to sit in {subnet_id:?}, got \
                 {reported}"
            );
        }

        // The endpoint is the account's: the describe reports the record the create stored.
        let described = ok(
            case,
            &router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
        )
        .await;
        assert_eq!(
            element(&described, "vpcId").as_deref(),
            Some(vpc_id.as_str()),
            "case {case:?}: expected the endpoint to be reported in {vpc_id}, got {described}"
        );
        // A gateway endpoint's own endpoint route is in each table it was associated with,
        // matching on the endpoint service's managed prefix list — the destination AWS reports
        // such a route with, and the reason a gateway endpoint needs no CIDR block of its own.
        for route_table_id in &tables {
            let routes = routes_to_gateway(case, &router, route_table_id, &endpoint_id).await;
            assert_eq!(
                routes.len(),
                1,
                "case {case:?}: expected one endpoint route to {endpoint_id} in \
                 {route_table_id}, got {routes:?}"
            );
            assert!(
                routes[0].starts_with("pl-"),
                "case {case:?}: expected the endpoint route in {route_table_id} to match on a \
                 prefix list, got {routes:?}"
            );
        }

        let association_id = if row.associates_subnet {
            let subnet = subnet_id.as_deref().expect("an associating row has a subnet");
            let body = ok(
                case,
                &router,
                "AssociateRouteTable",
                &[
                    param("RouteTableId", &tables[0]),
                    param("SubnetId", subnet),
                ],
            )
            .await;
            Some(required_element(case, &body, "associationId"))
        } else {
            None
        };

        // With the gateway still attached, the VPC has a dependency — which is the only thing
        // `DependencyViolation` says.
        refused_while_held(case, &router, &vpc_id, &gateway_id).await;

        // The teardown, each resource through its own operation.
        let deleted = ok(
            case,
            &router,
            "DeleteVpcEndpoints",
            &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
        )
        .await;
        let unsuccessful: Vec<String> = {
            let parsed = document(case, &deleted);
            set_items(parsed.root_element(), "unsuccessful")
                .into_iter()
                .map(|item| child_text(item, "resourceId").unwrap_or_default())
                .collect()
        };
        assert!(
            unsuccessful.is_empty(),
            "case {case:?}: expected the endpoint delete to report nothing unsuccessful, got \
             {unsuccessful:?} in {deleted}"
        );
        let (status, body) = call(
            &router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted endpoint to stop resolving, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVpcEndpointId.NotFound",
            "case {case:?}: expected InvalidVpcEndpointId.NotFound for {endpoint_id}, got {body}"
        );
        // The endpoint's route went with it, so the table it was in holds no route to an
        // endpoint that no longer exists.
        for route_table_id in &tables {
            let routes = routes_to_gateway(case, &router, route_table_id, &endpoint_id).await;
            assert!(
                routes.is_empty(),
                "case {case:?}: expected no route to the deleted {endpoint_id} in \
                 {route_table_id}, got {routes:?}"
            );
        }
        // And so did its endpoint network interfaces, which is what frees the subnet they sat
        // in for its own delete below.
        for network_interface_id in &interfaces {
            let (status, body) = call(
                &router,
                "DescribeNetworkInterfaces",
                &indexed("NetworkInterfaceId", &[network_interface_id]),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected {network_interface_id} to stop resolving once the \
                 endpoint was deleted, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidNetworkInterfaceID.NotFound",
                "case {case:?}: expected InvalidNetworkInterfaceID.NotFound for \
                 {network_interface_id}, got {body}"
            );
        }

        if let Some(association_id) = &association_id {
            let before = associations_of(case, &router, &tables[0]).await;
            assert!(
                before.iter().any(|reported| reported == association_id),
                "case {case:?}: expected {} to report the association {association_id}, got \
                 {before:?}",
                tables[0]
            );
            ok(
                case,
                &router,
                "DisassociateRouteTable",
                &[param("AssociationId", association_id)],
            )
            .await;
            // The association is a pair and the disassociate took it away, so the table stops
            // reporting it rather than answering 200 and keeping it.
            let after = associations_of(case, &router, &tables[0]).await;
            assert!(
                !after.iter().any(|reported| reported == association_id),
                "case {case:?}: expected {} to stop reporting the association \
                 {association_id}, got {after:?}",
                tables[0]
            );
        }
        for route_table_id in &tables {
            ok(
                case,
                &router,
                "DeleteRouteTable",
                &[param("RouteTableId", route_table_id)],
            )
            .await;
        }
        if let Some(subnet) = &subnet_id {
            ok(case, &router, "DeleteSubnet", &[param("SubnetId", subnet)]).await;
        }
        ok(
            case,
            &router,
            "DetachInternetGateway",
            &[
                param("InternetGatewayId", &gateway_id),
                param("VpcId", &vpc_id),
            ],
        )
        .await;
        ok(
            case,
            &router,
            "DeleteInternetGateway",
            &[param("InternetGatewayId", &gateway_id)],
        )
        .await;

        // Nothing holds the VPC any more, so the delete succeeds and the VPC stops being the
        // account's: the id resolves to nothing and no unnarrowed report carries it.
        ok(case, &router, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;
        let (status, body) =
            call(&router, "DescribeVpcs", &indexed("VpcId", &[vpc_id.as_str()])).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted {vpc_id} to stop resolving, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVpcID.NotFound",
            "case {case:?}: expected InvalidVpcID.NotFound for {vpc_id}, got {body}"
        );
        let all = ok(case, &router, "DescribeVpcs", &[]).await;
        let reported = elements(&all, "vpcId");
        assert!(
            !reported.contains(&vpc_id),
            "case {case:?}: expected {vpc_id} to be gone from the account's VPCs, got \
             {reported:?}"
        );
        // And the gateway it was attached to is gone as well, so the teardown left nothing
        // behind that the caller has no API to remove.
        let (status, body) = call(
            &router,
            "DescribeInternetGateways",
            &indexed("InternetGatewayId", &[gateway_id.as_str()]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted {gateway_id} to stop resolving, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInternetGatewayID.NotFound",
            "case {case:?}: expected InvalidInternetGatewayID.NotFound for {gateway_id}, got \
             {body}"
        );
    }
}
