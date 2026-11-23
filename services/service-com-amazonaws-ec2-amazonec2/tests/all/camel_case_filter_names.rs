// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the SPELLING an EC2 filter name may be given in, driven over the
//! wire the service speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test is that a VPC resource's describe SERVES A DOCUMENTED FILTER UNDER
//! THE CAMELCASE SPELLING OF ITS NAME AS WELL AS THE HYPHENATED ONE, both selecting on the
//! same attribute — the spelling these filters were first published under, which several
//! `Filters` members still publish beside the hyphenated name ("You can also use
//! `availabilityZone` as the filter name", "You can also use `cidr` or `cidrBlock` as the
//! filter names" — `DescribeSubnetsRequest$Filters`) and which callers still select on.
//! A spelling that is NOT the camelCase of a name the operation documents stays refused
//! with EC2's `InvalidParameterValue`, so the allowance is a spelling of a documented
//! attribute rather than a hole in the filter table.
//!
//! Every table row is a distinct input along what the rule generalises over — which
//! operation answers, which attribute is selected on, and whether the name is one word, a
//! hyphenated run of words or a member path with a `.` in it — so no one row is the
//! specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, filters, unescaped};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The IPv4 block the VPC these rows select on is created with.
const VPC_CIDR: &str = "10.42.0.0/16";

/// The IPv4 block its subnet is created with.
const SUBNET_CIDR: &str = "10.42.7.0/24";

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it.
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

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The `reported` elements of what `action` answers for the one filter `name`=`value`, in
/// ascending order and without repeats: an id that appears twice in a projection (a route
/// table's id inside its own association, say) is one selected resource either way.
async fn selected(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    reported: &str,
    name: &str,
    value: &str,
) -> Vec<String> {
    let body = ok(case, router, action, &filters(&[(name, &[value])])).await;
    let mut ids = elements(&body, reported);
    ids.sort();
    ids.dedup();
    ids
}

/// What one row selects on: the describe, the element naming each selected resource, the
/// documented (hyphenated) filter name, that name's camelCase spelling, and the value both
/// select with.
struct Row {
    case: &'static str,
    action: &'static str,
    reported: &'static str,
    documented: &'static str,
    camel_case: &'static str,
    value: String,
}

/// The VPC network the rows select over: a VPC with a subnet, the main route table and
/// default network ACL that VPC comes with, and an internet gateway attached to it.
struct Network {
    vpc_id: String,
    subnet_id: String,
    route_table_id: String,
    network_acl_id: String,
    internet_gateway_id: String,
    dhcp_options_id: String,
    owner_id: String,
    availability_zone: String,
}

/// Create that network, reading every id back off the service rather than writing one here.
async fn network(router: &MultiServiceRouter) -> Network {
    let case = "the network the rows select over";
    let vpc = ok(case, router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
    let vpc_id = member(case, &vpc, "vpcId");
    let dhcp_options_id = member(case, &vpc, "dhcpOptionsId");
    let owner_id = member(case, &vpc, "ownerId");

    let subnet = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    let subnet_id = member(case, &subnet, "subnetId");
    let availability_zone = member(case, &subnet, "availabilityZone");

    let tables = ok(
        case,
        router,
        "DescribeRouteTables",
        &filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    let route_table_id = member(case, &tables, "routeTableId");

    let acls = ok(
        case,
        router,
        "DescribeNetworkAcls",
        &filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    let network_acl_id = member(case, &acls, "networkAclId");

    let gateway = ok(case, router, "CreateInternetGateway", &[]).await;
    let internet_gateway_id = member(case, &gateway, "internetGatewayId");
    let _ = ok(
        case,
        router,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", &internet_gateway_id),
            param("VpcId", &vpc_id),
        ],
    )
    .await;

    Network {
        vpc_id,
        subnet_id,
        route_table_id,
        network_acl_id,
        internet_gateway_id,
        dhcp_options_id,
        owner_id,
        availability_zone,
    }
}

/// The rows: six describes of VPC resources, and per describe the attributes that differ in
/// the shape of their name — one word (`state`-like `key`), a hyphenated run (`vpc-id`,
/// `map-public-ip-on-launch`) and a member path (`route.destination-cidr-block`,
/// `cidr-block-association.cidr-block`) — selected on by value, by id and by flag.
fn rows(network: &Network) -> Vec<Row> {
    let Network {
        vpc_id,
        subnet_id,
        route_table_id,
        network_acl_id,
        internet_gateway_id,
        dhcp_options_id,
        owner_id,
        availability_zone,
    } = network;
    vec![
        Row {
            case: "a subnet by its VPC",
            action: "DescribeSubnets",
            reported: "subnetId",
            documented: "vpc-id",
            camel_case: "vpcId",
            value: vpc_id.clone(),
        },
        Row {
            case: "a subnet by its own id",
            action: "DescribeSubnets",
            reported: "subnetId",
            documented: "subnet-id",
            camel_case: "subnetId",
            value: subnet_id.clone(),
        },
        Row {
            case: "a subnet by its block",
            action: "DescribeSubnets",
            reported: "subnetId",
            documented: "cidr-block",
            camel_case: "cidrBlock",
            value: SUBNET_CIDR.to_string(),
        },
        Row {
            case: "a subnet by its zone",
            action: "DescribeSubnets",
            reported: "subnetId",
            documented: "availability-zone",
            camel_case: "availabilityZone",
            value: availability_zone.clone(),
        },
        Row {
            case: "a subnet by a launch-time flag",
            action: "DescribeSubnets",
            reported: "subnetId",
            documented: "map-public-ip-on-launch",
            camel_case: "mapPublicIpOnLaunch",
            value: "false".to_string(),
        },
        Row {
            case: "a subnet by whether it is its zone's default",
            action: "DescribeSubnets",
            reported: "subnetId",
            documented: "default-for-az",
            camel_case: "defaultForAz",
            value: "false".to_string(),
        },
        Row {
            case: "a VPC by whether it is the default one",
            action: "DescribeVpcs",
            reported: "vpcId",
            documented: "is-default",
            camel_case: "isDefault",
            value: "false".to_string(),
        },
        Row {
            case: "a VPC by its option set",
            action: "DescribeVpcs",
            reported: "vpcId",
            documented: "dhcp-options-id",
            camel_case: "dhcpOptionsId",
            value: dhcp_options_id.clone(),
        },
        Row {
            case: "a VPC by an association's block",
            action: "DescribeVpcs",
            reported: "vpcId",
            documented: "cidr-block-association.cidr-block",
            camel_case: "cidrBlockAssociation.cidrBlock",
            value: VPC_CIDR.to_string(),
        },
        Row {
            case: "a VPC by its owner",
            action: "DescribeVpcs",
            reported: "vpcId",
            documented: "owner-id",
            camel_case: "ownerId",
            value: owner_id.clone(),
        },
        Row {
            case: "a route table by its own id",
            action: "DescribeRouteTables",
            reported: "routeTableId",
            documented: "route-table-id",
            camel_case: "routeTableId",
            value: route_table_id.clone(),
        },
        Row {
            case: "a route table by a route's destination",
            action: "DescribeRouteTables",
            reported: "routeTableId",
            documented: "route.destination-cidr-block",
            camel_case: "route.destinationCidrBlock",
            value: VPC_CIDR.to_string(),
        },
        Row {
            case: "a network ACL by its VPC",
            action: "DescribeNetworkAcls",
            reported: "networkAclId",
            documented: "vpc-id",
            camel_case: "vpcId",
            value: vpc_id.clone(),
        },
        Row {
            case: "a network ACL by an entry's action",
            action: "DescribeNetworkAcls",
            reported: "networkAclId",
            documented: "entry.rule-action",
            camel_case: "entry.ruleAction",
            value: "allow".to_string(),
        },
        Row {
            case: "a network ACL by its own id",
            action: "DescribeNetworkAcls",
            reported: "networkAclId",
            documented: "network-acl-id",
            camel_case: "networkAclId",
            value: network_acl_id.clone(),
        },
        Row {
            case: "an internet gateway by its own id",
            action: "DescribeInternetGateways",
            reported: "internetGatewayId",
            documented: "internet-gateway-id",
            camel_case: "internetGatewayId",
            value: internet_gateway_id.clone(),
        },
        Row {
            case: "an internet gateway by the VPC it is attached to",
            action: "DescribeInternetGateways",
            reported: "internetGatewayId",
            documented: "attachment.vpc-id",
            camel_case: "attachment.vpcId",
            value: vpc_id.clone(),
        },
        Row {
            case: "an option set by its own id",
            action: "DescribeDhcpOptions",
            reported: "dhcpOptionsId",
            documented: "dhcp-options-id",
            camel_case: "dhcpOptionsId",
            value: dhcp_options_id.clone(),
        },
        Row {
            case: "an option set by its owner",
            action: "DescribeDhcpOptions",
            reported: "dhcpOptionsId",
            documented: "owner-id",
            camel_case: "ownerId",
            value: owner_id.clone(),
        },
    ]
}

/// RULE: the camelCase spelling of a documented filter name selects exactly what the
/// hyphenated name selects — the same attribute, compared against the same value.
#[tokio::test]
async fn a_camel_case_spelling_selects_what_the_hyphenated_name_selects() {
    let router = emulator();
    let network = network(&router).await;

    for row in rows(&network) {
        let Row {
            case,
            action,
            reported,
            documented,
            camel_case,
            value,
        } = row;

        let under_documented =
            selected(case, &router, action, reported, documented, &value).await;
        assert!(
            !under_documented.is_empty(),
            "case {case:?}: {action} must report something for {documented}={value:?}, got \
             nothing"
        );

        let under_camel_case =
            selected(case, &router, action, reported, camel_case, &value).await;
        assert_eq!(
            under_camel_case, under_documented,
            "case {case:?}: {action} selected {under_camel_case:?} for {camel_case}={value:?} \
             and {under_documented:?} for {documented}={value:?}"
        );
    }
}

/// A spelling to refuse, and the describe it is sent to.
struct Refused {
    case: &'static str,
    action: &'static str,
    name: &'static str,
}

/// RULE: only the camelCase spelling of a name the operation itself documents resolves —
/// every other spelling, and the camelCase of a name another operation documents, is
/// refused with EC2's `InvalidParameterValue` naming the filter.
#[tokio::test]
async fn a_spelling_that_is_not_the_camel_case_of_a_documented_name_is_refused() {
    let router = emulator();

    // Rows: a leading capital, a capital following a capital, a capital following a
    // separator, the underscore spelling, the squashed all-lowercase spelling, a camelCase
    // name of a DIFFERENT operation's filter table, and the tag filter without a key.
    let cases = [
        Refused {
            case: "a leading capital",
            action: "DescribeSubnets",
            name: "VpcId",
        },
        Refused {
            case: "a capital after a capital",
            action: "DescribeSubnets",
            name: "vpcID",
        },
        Refused {
            case: "a capital after a hyphen",
            action: "DescribeSubnets",
            name: "vpc-Id",
        },
        Refused {
            case: "the underscore spelling",
            action: "DescribeSubnets",
            name: "vpc_id",
        },
        Refused {
            case: "the squashed spelling",
            action: "DescribeSubnets",
            name: "vpcid",
        },
        Refused {
            case: "another operation's filter",
            action: "DescribeSubnets",
            name: "isDefault",
        },
        Refused {
            case: "a VPC filter naming no VPC attribute",
            action: "DescribeVpcs",
            name: "subnetId",
        },
        Refused {
            case: "a gateway filter naming an attribute only its attachment has",
            action: "DescribeInternetGateways",
            name: "vpcId",
        },
        Refused {
            case: "a route-table filter with a capitalised path step",
            action: "DescribeRouteTables",
            name: "route.DestinationCidrBlock",
        },
        Refused {
            case: "the tag filter without a key",
            action: "DescribeNetworkAcls",
            name: "tag:",
        },
    ];

    for Refused { case, action, name } in cases {
        let (status, body) = call(&router, action, &filters(&[(name, &["whatever"])])).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action} for {name:?}, got {status} with \
             body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue from {action} for {name:?}, got \
             {code:?} in {body}"
        );
        // The EC2 error envelope is XML, so the message's apostrophes arrive escaped.
        assert!(
            unescaped(&body).contains(&format!("The filter '{name}' is invalid")),
            "case {case:?}: the refusal must name the filter {name:?}, got {body}"
        );
    }
}
