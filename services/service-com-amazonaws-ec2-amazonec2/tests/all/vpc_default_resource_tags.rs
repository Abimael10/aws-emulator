// SPDX-License-Identifier: CC0-1.0

//! The tag plane over the resources EC2 ITSELF puts in a VPC — end to end over the wire the
//! service speaks (`ec2Query`, through the shared [`super::ec2_query`] harness).
//!
//! The rules under test are:
//!
//! 1. a resource the SERVICE created as part of a VPC — the default security group, the main
//!    route table, the default network ACL, the Region's default DHCP option set, a default
//!    subnet — is an ordinary taggable resource: `CreateTags` resolves its id exactly as it
//!    resolves a caller-created resource's, answers HTTP 200 with the envelope's success flag
//!    and nothing else, and the write reads back both through `DescribeTags` under that kind's
//!    `ResourceType` and through the resource's OWN describe;
//! 2. `Invalid<Kind>.NotFound` is reserved for an id that really names no resource of the
//!    account, and because "adds or overwrites only the specified tags for the specified
//!    Amazon EC2 resource or resources" (`CreateTags`) is all-or-nothing, such a refusal
//!    leaves every OTHER resource the same request named untagged.
//!
//! The rows of each table are distinct inputs along what the rules generalise over: which
//! kind of resource the id names, whether the VPC holding it was created by the account or by
//! the service, how many resources one request names, and which kind the unresolvable id
//! belongs to. No row is the specification — the recorded request named five resources of one
//! VPC, and the tables below span the kinds a VPC comes with and both kinds of VPC.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, filters, tag_report, DescribedTag};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for: the account's default VPC and its per-zone
/// default subnets are the ones this Region holds, and the default DHCP option set a VPC
/// inherits is the Region's.
const REGION: &str = "us-east-1";

/// The tag every request here writes, the pair an estate labels its resources with.
const TAG_KEY: &str = "Name";

/// Its value.
const TAG_VALUE: &str = "estate-wide";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One resource a request below tags: the kind it is reported under, its id, and what it is —
/// the case name every assertion about it carries.
#[derive(Debug, Clone)]
struct Resource {
    /// The `ResourceType` `DescribeTags` reports this resource under, as the model's catalog
    /// of taggable kinds spells it (`smithy_query(query="shape",shape="ResourceType")`).
    resource_type: &'static str,
    id: String,
    what: String,
}

/// The describe that reports `resource_type`'s resources, and the request member that narrows
/// it to one of them — the round trip a write is read back through.
fn describe_of(resource_type: &str) -> (&'static str, &'static str) {
    match resource_type {
        "vpc" => ("DescribeVpcs", "VpcId.1"),
        "security-group" => ("DescribeSecurityGroups", "GroupId.1"),
        "security-group-rule" => ("DescribeSecurityGroupRules", "SecurityGroupRuleId.1"),
        "route-table" => ("DescribeRouteTables", "RouteTableId.1"),
        "network-acl" => ("DescribeNetworkAcls", "NetworkAclId.1"),
        "dhcp-options" => ("DescribeDhcpOptions", "DhcpOptionsId.1"),
        "subnet" => ("DescribeSubnets", "SubnetId.1"),
        other => panic!("this suite knows no resource type {other:?}"),
    }
}

/// Call `action`, require HTTP 200, and answer the body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Call `action`, require HTTP 200, and answer the first `id_element` of the response.
async fn reported_id(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
    id_element: &str,
) -> String {
    let body = ok(router, case, action, params).await;
    element(&body, id_element)
        .unwrap_or_else(|| panic!("case {case:?}: expected a <{id_element}> in {body}"))
}

/// The `VpcId` of the VPC the SERVICE created for the account: the one `DescribeVpcs` reports
/// under the `isDefault` filter.
async fn default_vpc(router: &MultiServiceRouter) -> String {
    let body = ok(
        router,
        "the account's default VPC",
        "DescribeVpcs",
        &filters(&[("isDefault", &["true"])]),
    )
    .await;
    element(&body, "vpcId")
        .unwrap_or_else(|| panic!("the account must hold a default VPC in {REGION}: {body}"))
}

/// The id of the MAIN route table reported in `body`: the table one of whose associations is
/// the VPC's main one, read off the tree so the id is the main table's and not a sibling's.
fn main_route_table(case: &str, body: &str) -> String {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "routeTableId").is_some())
        .find(|node| {
            set_items(*node, "associationSet")
                .into_iter()
                .any(|association| child_text(association, "main").as_deref() == Some("true"))
        })
        .and_then(|node| child_text(node, "routeTableId"))
        .unwrap_or_else(|| panic!("case {case:?}: no main route table in {body}"))
}

/// Every resource EC2 itself put in the VPC `vpc_id`, as the account reads them back: the VPC,
/// its default security group, its main route table, its default network ACL and the DHCP
/// option set it inherited — each read through the describe a client would use, so every id
/// tagged below is one the service really reported.
async fn resources_the_vpc_comes_with(
    router: &MultiServiceRouter,
    population: &str,
    vpc_id: &str,
) -> Vec<Resource> {
    let described = ok(
        router,
        population,
        "DescribeVpcs",
        &[p("VpcId.1", vpc_id)],
    )
    .await;
    let dhcp_options_id = element(&described, "dhcpOptionsId").unwrap_or_else(|| {
        panic!("case {population:?}: {vpc_id} must report the option set it inherited: {described}")
    });

    let default_group = reported_id(
        router,
        population,
        "DescribeSecurityGroups",
        &filters(&[("vpc-id", &[vpc_id]), ("group-name", &["default"])]),
        "groupId",
    )
    .await;

    let route_tables = ok(
        router,
        population,
        "DescribeRouteTables",
        &filters(&[("vpc-id", &[vpc_id])]),
    )
    .await;
    let main_table = main_route_table(population, &route_tables);

    let default_acl = reported_id(
        router,
        population,
        "DescribeNetworkAcls",
        &filters(&[("vpc-id", &[vpc_id]), ("default", &["true"])]),
        "networkAclId",
    )
    .await;

    // A rule on that default group, so one request also names a resource whose id this
    // account minted inside a resource the SERVICE created.
    let rule = reported_id(
        router,
        population,
        "AuthorizeSecurityGroupIngress",
        &[
            p("GroupId", &default_group),
            p("IpPermissions.1.IpProtocol", "tcp"),
            p("IpPermissions.1.FromPort", "443"),
            p("IpPermissions.1.ToPort", "443"),
            p("IpPermissions.1.IpRanges.1.CidrIp", "10.0.0.0/8"),
        ],
        "securityGroupRuleId",
    )
    .await;

    [
        ("vpc", vpc_id.to_string(), "the VPC itself"),
        (
            "security-group",
            default_group,
            "the VPC's default security group",
        ),
        ("route-table", main_table, "the VPC's main route table"),
        ("network-acl", default_acl, "the VPC's default network ACL"),
        (
            "dhcp-options",
            dhcp_options_id,
            "the DHCP option set the VPC inherited",
        ),
        (
            "security-group-rule",
            rule,
            "a rule on the VPC's default security group",
        ),
    ]
    .into_iter()
    .map(|(resource_type, id, what)| Resource {
        resource_type,
        id,
        what: format!("{what} of {population}"),
    })
    .collect()
}

/// A default subnet of the account's default VPC — a resource the SERVICE created inside a VPC
/// the service created.
async fn a_default_subnet(router: &MultiServiceRouter, vpc_id: &str) -> Resource {
    let id = reported_id(
        router,
        "a default subnet",
        "DescribeSubnets",
        &filters(&[("vpc-id", &[vpc_id]), ("defaultForAz", &["true"])]),
        "subnetId",
    )
    .await;
    Resource {
        resource_type: "subnet",
        id,
        what: "a default subnet of the account's default VPC".to_string(),
    }
}

/// The `ResourceId.N` fields naming every resource of `resources`, and the one tag to write.
fn tagging_params(resources: &[Resource]) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = resources
        .iter()
        .enumerate()
        .map(|(index, resource)| p(&format!("ResourceId.{}", index + 1), resource.id.clone()))
        .collect();
    params.push(p("Tag.1.Key", TAG_KEY));
    params.push(p("Tag.1.Value", TAG_VALUE));
    params
}

/// Every resource this suite tags: those of the account's default VPC, those of a VPC the
/// caller created, and a default subnet — deduplicated by id, because the DHCP option set the
/// two VPCs inherited is the Region's one set.
async fn every_resource(router: &MultiServiceRouter) -> Vec<Resource> {
    let service_vpc = default_vpc(router).await;
    let caller_vpc = reported_id(
        router,
        "a VPC the caller created",
        "CreateVpc",
        &[p("CidrBlock", "10.20.0.0/16")],
        "vpcId",
    )
    .await;

    let mut resources: Vec<Resource> = Vec::new();
    for (population, vpc_id) in [
        ("the account's default VPC", service_vpc.as_str()),
        ("a VPC the caller created", caller_vpc.as_str()),
    ] {
        resources.extend(resources_the_vpc_comes_with(router, population, vpc_id).await);
    }
    resources.push(a_default_subnet(router, &service_vpc).await);

    let mut seen: BTreeSet<String> = BTreeSet::new();
    resources.retain(|resource| seen.insert(resource.id.clone()));
    resources
}

/// The `DescribeTags` row this suite's write owes one resource: the resource's own id, the
/// kind it is reported under, and the pair that was written.
fn expected_row(resource: &Resource) -> DescribedTag {
    (
        resource.id.clone(),
        resource.resource_type.to_string(),
        TAG_KEY.to_string(),
        TAG_VALUE.to_string(),
    )
}

/// Whether a `DescribeTags` report carries a tag under this suite's key at all — what a
/// refused all-or-nothing write must leave false.
fn carries_the_key(reported: &BTreeSet<DescribedTag>) -> bool {
    reported.iter().any(|(_, _, key, _)| key == TAG_KEY)
}

/// The tags `resource`'s OWN describe reports for it, as `key -> value`.
async fn described_tags(
    router: &MultiServiceRouter,
    resource: &Resource,
) -> Vec<(String, String)> {
    let (action, narrow) = describe_of(resource.resource_type);
    let body = ok(router, &resource.what, action, &[p(narrow, &resource.id)]).await;
    let parsed = document(&resource.what, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "tagSet")
        .flat_map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .collect::<Vec<_>>()
        })
        .map(|item| {
            (
                child_text(item, "key").unwrap_or_default(),
                child_text(item, "value").unwrap_or_default(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rule 1: every resource a VPC comes with is taggable, in one call
// ---------------------------------------------------------------------------

/// ONE `CreateTags` naming every resource EC2 itself put in two VPCs — the account's default
/// VPC and one the caller created — succeeds, and each of them reports the tag afterwards
/// through `DescribeTags` and through its own describe.
#[tokio::test]
async fn one_call_tags_every_resource_a_vpc_comes_with() {
    let router = emulator();
    let resources = every_resource(&router).await;
    // Every kind a VPC comes with is in the one request, so no row is the specification.
    let kinds: BTreeSet<&str> = resources
        .iter()
        .map(|resource| resource.resource_type)
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from([
            "dhcp-options",
            "network-acl",
            "route-table",
            "security-group",
            "security-group-rule",
            "subnet",
            "vpc",
        ]),
        "the request must name every kind a VPC comes with, got {resources:?}"
    );

    let body = ok(
        &router,
        "every resource the two VPCs come with",
        "CreateTags",
        &tagging_params(&resources),
    )
    .await;
    // "Returns an empty body" — the ec2Query envelope's success flag, and no result of its
    // own: a refusal of any id would have been the call's error instead.
    assert!(
        body.contains("<return>true</return>") && !body.contains("<Errors>"),
        "a CreateTags over the resources a VPC comes with answers the success flag, got {body}"
    );

    for resource in &resources {
        let case = &resource.what;
        let reported = tag_report(&router, REGION, case, &resource.id).await;
        assert!(
            reported.contains(&expected_row(resource)),
            "case {case:?}: DescribeTags must report {:?} on {}, got {reported:?}",
            expected_row(resource),
            resource.id
        );
        let own = described_tags(&router, resource).await;
        assert!(
            own.contains(&(TAG_KEY.to_string(), TAG_VALUE.to_string())),
            "case {case:?}: {} must report the tag through its own describe, got {own:?}",
            resource.id
        );
    }
}

/// A second write over the same resources overwrites only the key it names and leaves the
/// first tag in place, so the tag plane keeps acting on the stored resource rather than on a
/// one-off answer.
#[tokio::test]
async fn a_second_write_adds_to_what_a_vpcs_resources_already_carry() {
    let router = emulator();
    let resources = every_resource(&router).await;

    ok(
        &router,
        "the first write",
        "CreateTags",
        &tagging_params(&resources),
    )
    .await;

    let mut second = tagging_params(&resources);
    second.push(p("Tag.2.Key", "Owner"));
    second.push(p("Tag.2.Value", "platform"));
    ok(&router, "the second write", "CreateTags", &second).await;

    for resource in &resources {
        let case = &resource.what;
        let own = described_tags(&router, resource).await;
        for expected in [
            (TAG_KEY.to_string(), TAG_VALUE.to_string()),
            ("Owner".to_string(), "platform".to_string()),
        ] {
            assert!(
                own.contains(&expected),
                "case {case:?}: {} must carry {expected:?} after both writes, got {own:?}",
                resource.id
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a not-found is reserved for an id that names nothing, and drops the request
// ---------------------------------------------------------------------------

/// One row of the refusal table: a well-formed id of a kind a VPC comes with that names no
/// resource of the account, and the refusal that kind answers for it.
struct AbsentRow {
    case: &'static str,
    /// The id the request names beside the resources that DO exist.
    absent_id: &'static str,
    code: &'static str,
    /// The sentence AWS words that code with, around the id the REQUEST named.
    message: &'static str,
}

/// A request naming an id that resolves to nothing is refused with that KIND's own not-found
/// code and sentence, and leaves every resource it named beside it untagged.
///
/// The rows span the kinds a VPC comes with, each with the id form this service mints
/// (`<prefix>-` and 17 lowercase hexadecimal digits) and nothing behind it.
#[tokio::test]
async fn an_id_that_names_nothing_is_refused_and_tags_nothing() {
    let rows = [
        AbsentRow {
            case: "a network ACL id nothing holds",
            absent_id: "acl-00000000000000999",
            code: "InvalidNetworkAclID.NotFound",
            message: "The networkAcl ID 'acl-00000000000000999' does not exist",
        },
        AbsentRow {
            case: "a route table id nothing holds",
            absent_id: "rtb-00000000000000999",
            code: "InvalidRouteTableID.NotFound",
            message: "The routeTable ID 'rtb-00000000000000999' does not exist",
        },
        AbsentRow {
            case: "a security group id nothing holds",
            absent_id: "sg-00000000000000999",
            code: "InvalidGroup.NotFound",
            message: "The security group 'sg-00000000000000999' does not exist",
        },
        AbsentRow {
            case: "a DHCP option set id nothing holds",
            absent_id: "dopt-00000000000000999",
            code: "InvalidDhcpOptionID.NotFound",
            message: "The dhcpOptions ID 'dopt-00000000000000999' does not exist",
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let resources = every_resource(&router).await;

        let mut params = tagging_params(&resources);
        params.push(p(
            &format!("ResourceId.{}", resources.len() + 1),
            row.absent_id,
        ));
        let (status, body) = call(&router, REGION, "CreateTags", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = super::ec2_query::error_code(&body);
        assert_eq!(
            code, row.code,
            "case {case:?}: expected the code {:?}, got {code:?} in {body}",
            row.code
        );
        let message = super::ec2_query::error_message(&body);
        assert_eq!(
            message, row.message,
            "case {case:?}: expected the message {:?}, got {message:?}",
            row.message
        );

        // All-or-nothing: not one of the resources that DO exist was tagged.
        for resource in &resources {
            let reported = tag_report(&router, REGION, case, &resource.id).await;
            assert!(
                !carries_the_key(&reported),
                "case {case:?}: the refused request must leave {} untagged, got {reported:?}",
                resource.id
            );
        }

        // And the same request without the unresolvable id is served, so the refusal was
        // about that id alone.
        ok(
            &router,
            case,
            "CreateTags",
            &tagging_params(&resources),
        )
        .await;
        for resource in &resources {
            let reported = tag_report(&router, REGION, case, &resource.id).await;
            assert!(
                reported.contains(&expected_row(resource)),
                "case {case:?}: {} must carry the tag once the request resolves, got \
                 {reported:?}",
                resource.id
            );
        }
    }
}
