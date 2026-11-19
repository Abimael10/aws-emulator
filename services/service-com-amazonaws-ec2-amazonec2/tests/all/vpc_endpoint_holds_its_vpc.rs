// SPDX-License-Identifier: CC0-1.0

//! A VPC ENDPOINT holds the VPC it was created in, driven over `ec2Query` (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! RULE: "You must detach or delete all gateways and resources that are associated with the
//! VPC before you can delete it" (`DeleteVpc`) — a VPC endpoint of the VPC is such a
//! resource, whatever type it is of, because `DeleteVpc` does not remove it (only
//! `DeleteVpcEndpoints` does). So while the account holds an endpoint whose `VpcId` is the
//! target, `DeleteVpc` refuses with HTTP 400 `DependencyViolation` and the sentence AWS words
//! that class with, the VPC and the endpoint both keep describing, and the identical delete
//! succeeds once the endpoints are gone.
//!
//! The rows differ along what the rule generalises over: the endpoint TYPE (a `Gateway`
//! endpoint, which has no network interface of its own and holds the VPC through a route
//! table, and an `Interface` one, which sits in a subnet), the endpoint service, WHICH table
//! of the VPC it was associated with (the one it came with or one the caller created), and how
//! many endpoints the VPC holds at once.
//!
//! Siblings: [`super::vpc_network_lifecycle`] covers the other dependencies of a VPC (a
//! subnet, a security group, the internet gateway), and [`super::delete_vpc`] what the delete
//! takes with the VPC once nothing holds it.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The zone the subnet of the interface-endpoint row sits in.
const SUBNET_ZONE: &str = "us-east-1a";

/// The error code EC2 answers a delete something still holds.
const DEPENDENCY_VIOLATION: &str = "DependencyViolation";

/// The emulator the suite drives.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call, as its HTTP status and XML body.
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

/// One element of a body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// A VPC with `cidr`, as the id EC2 minted.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    member(case, &body, "vpcId")
}

/// The MAIN route table of `vpc_id` — the one the VPC came with, selected by the association
/// flag EC2 reports for exactly that table.
async fn main_route_table(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &super::ec2_query::filters(&[("vpc-id", &[vpc_id]), ("association.main", &["true"])]),
    )
    .await;
    member(case, &body, "routeTableId")
}

/// A route table of `vpc_id` the caller created, as its id.
async fn create_route_table(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateRouteTable",
        &[param("VpcId", vpc_id)],
    )
    .await;
    member(case, &body, "routeTableId")
}

/// A subnet of `vpc_id` with `cidr`, as its id.
async fn create_subnet(case: &str, router: &MultiServiceRouter, vpc_id: &str, cidr: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", vpc_id),
            param("CidrBlock", cidr),
            param("AvailabilityZone", SUBNET_ZONE),
        ],
    )
    .await;
    member(case, &body, "subnetId")
}

/// Whether `DescribeVpcEndpoints` still reports `endpoint_id`.
async fn endpoint_describes(router: &MultiServiceRouter, endpoint_id: &str) -> bool {
    let (status, body) = call(
        router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[endpoint_id]),
    )
    .await;
    status == 200
        && elements(&body, "vpcEndpointId")
            .iter()
            .any(|reported| reported == endpoint_id)
}

/// Whether `DescribeVpcs` still reports `vpc_id`.
async fn vpc_describes(router: &MultiServiceRouter, vpc_id: &str) -> bool {
    let (status, body) = call(router, "DescribeVpcs", &indexed("VpcId", &[vpc_id])).await;
    status == 200
        && elements(&body, "vpcId")
            .iter()
            .any(|reported| reported == vpc_id)
}

/// `DeleteVpc` has to be refused `DependencyViolation` while something still holds the VPC,
/// with the sentence AWS words that class with: fixed text around the `VpcId` the REQUEST
/// named and nothing else.
async fn expect_dependency_violation(case: &str, router: &MultiServiceRouter, vpc_id: &str) {
    let (status, body) = call(router, "DeleteVpc", &[param("VpcId", vpc_id)]).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 while the endpoint holds the VPC, got {status} \
         with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DEPENDENCY_VIOLATION,
        "case {case:?}: expected {DEPENDENCY_VIOLATION}, got {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message,
        format!("The vpc '{vpc_id}' has dependencies and cannot be deleted."),
        "case {case:?}: expected the sentence naming the VPC the request named, got {message:?}"
    );
}

/// What one row stands up: the endpoints of one VPC, and the records that have to be taken
/// away BESIDE them before the VPC deletes (a subnet of an interface endpoint's own).
struct Row {
    /// The case name every assertion carries.
    case: &'static str,
    /// The VPC's block, distinct per row so the rows share no address space.
    vpc_cidr: &'static str,
    /// Whether the endpoints are associated with a table the caller created rather than the
    /// one the VPC came with.
    created_table: bool,
    /// The endpoint services a `Gateway` endpoint is created for, one endpoint each.
    gateway_services: &'static [&'static str],
    /// Whether the row also creates an `Interface` endpoint, which needs a subnet.
    interface_endpoint: bool,
}

/// RULE: every VPC endpoint of the VPC holds it, so `DeleteVpc` refuses with
/// `DependencyViolation` until every one of them is deleted — and then succeeds.
#[tokio::test]
async fn delete_vpc_refuses_while_the_vpc_still_holds_an_endpoint() {
    let router = emulator();
    let rows: [Row; 5] = [
        Row {
            case: "one gateway endpoint on the table the VPC came with",
            vpc_cidr: "10.87.0.0/16",
            created_table: false,
            gateway_services: &["s3"],
            interface_endpoint: false,
        },
        Row {
            case: "one gateway endpoint to another service, on a table the caller created",
            vpc_cidr: "10.88.0.0/16",
            created_table: true,
            gateway_services: &["dynamodb"],
            interface_endpoint: false,
        },
        Row {
            case: "two gateway endpoints at once: each one holds the VPC on its own",
            vpc_cidr: "10.89.0.0/16",
            created_table: false,
            gateway_services: &["s3", "dynamodb"],
            interface_endpoint: false,
        },
        Row {
            case: "an interface endpoint, which sits in a subnet",
            vpc_cidr: "10.90.0.0/16",
            created_table: false,
            gateway_services: &[],
            interface_endpoint: true,
        },
        Row {
            case: "a gateway endpoint and an interface endpoint together",
            vpc_cidr: "10.91.0.0/16",
            created_table: false,
            gateway_services: &["s3"],
            interface_endpoint: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let vpc_id = create_vpc(case, &router, row.vpc_cidr).await;
        let route_table_id = if row.created_table {
            create_route_table(case, &router, &vpc_id).await
        } else {
            main_route_table(case, &router, &vpc_id).await
        };
        let subnet_id = if row.interface_endpoint {
            let octet = row.vpc_cidr.split('.').nth(1).unwrap_or("0");
            Some(create_subnet(case, &router, &vpc_id, &format!("10.{octet}.1.0/24")).await)
        } else {
            None
        };

        let mut endpoint_ids: Vec<String> = Vec::new();
        for service in row.gateway_services {
            let body = ok(
                case,
                &router,
                "CreateVpcEndpoint",
                &[
                    param("VpcId", &vpc_id),
                    param("ServiceName", &format!("com.amazonaws.{REGION}.{service}")),
                    param("VpcEndpointType", "Gateway"),
                    param("RouteTableId.1", &route_table_id),
                ],
            )
            .await;
            endpoint_ids.push(member(case, &body, "vpcEndpointId"));
        }
        if let Some(subnet_id) = subnet_id.as_deref() {
            let body = ok(
                case,
                &router,
                "CreateVpcEndpoint",
                &[
                    param("VpcId", &vpc_id),
                    param("ServiceName", &format!("com.amazonaws.{REGION}.logs")),
                    param("VpcEndpointType", "Interface"),
                    param("SubnetId.1", subnet_id),
                    // A VPC a client created has DNS hostnames off, and private DNS needs
                    // them, so the endpoint asks for none: what this row is about is the
                    // endpoint's hold on the VPC.
                    param("PrivateDnsEnabled", "false"),
                ],
            )
            .await;
            endpoint_ids.push(member(case, &body, "vpcEndpointId"));
        }
        assert!(
            !endpoint_ids.is_empty(),
            "case {case:?}: the row must create at least one endpoint"
        );

        // Every endpoint holds the VPC on its own: the delete is refused while any one of
        // them is left, so the endpoints are taken away one at a time and the refusal re-read
        // after each.
        for (position, endpoint_id) in endpoint_ids.iter().enumerate() {
            expect_dependency_violation(case, &router, &vpc_id).await;
            assert!(
                vpc_describes(&router, &vpc_id).await,
                "case {case:?}: the refused delete must leave the VPC describable"
            );
            assert!(
                endpoint_describes(&router, endpoint_id).await,
                "case {case:?}: the refused delete must leave endpoint {position} \
                 ({endpoint_id}) describable"
            );
            ok(
                case,
                &router,
                "DeleteVpcEndpoints",
                &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
            )
            .await;
            assert!(
                !endpoint_describes(&router, endpoint_id).await,
                "case {case:?}: endpoint {endpoint_id} must stop describing once deleted"
            );
        }

        // What is left of the VPC beside the endpoints: an interface endpoint's subnet is a
        // dependency of its own, so a row that created one reads the refusal once more until
        // the subnet is gone — which is what shows the endpoint was counted independently of
        // its network interface, that interface having gone with the endpoint.
        if let Some(subnet_id) = subnet_id.as_deref() {
            expect_dependency_violation(case, &router, &vpc_id).await;
            ok(
                case,
                &router,
                "DeleteSubnet",
                &[param("SubnetId", subnet_id)],
            )
            .await;
        }

        // And with nothing left holding it, the identical delete succeeds and the VPC stops
        // describing.
        ok(case, &router, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;
        assert!(
            !vpc_describes(&router, &vpc_id).await,
            "case {case:?}: the VPC must stop describing once deleted"
        );
    }
}
