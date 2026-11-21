// SPDX-License-Identifier: CC0-1.0

//! What a security-group rule that REFERENCES a group of another network is answered with,
//! driven over `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a rule may only reference a security group its own network can see —
//! "a security group and the referenced security group must be in the same VPC, or in peered
//! VPCs" (the EC2 User Guide's security group rules). A reference to a group of another VPC
//! that the request does not IDENTIFY as such is refused `InvalidGroup.NotFound`, with the
//! sentence EC2 answers whenever two resources one request names sit in different networks:
//! fixed text that names neither the group nor either network, because what the caller has to
//! correct is the cross-network reference rather than a missing resource.
//!
//! The rows differ along what the reference carries: nothing beside the group id, a `VpcId`
//! naming the wrong network, a `VpcId` naming the group's own network (which makes the
//! reference legitimate), and a group of the rule's own VPC. Both directions of a rule
//! (`AuthorizeSecurityGroupIngress` and `AuthorizeSecurityGroupEgress`) go through the one
//! reference resolution, so every row is driven through both.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The two authorizes that store a rule, which share the one reference resolution.
const AUTHORIZES: [&str; 2] = [
    "AuthorizeSecurityGroupIngress",
    "AuthorizeSecurityGroupEgress",
];

/// The sentence EC2 answers a request naming two resources of different networks, spelled out
/// HERE rather than read from the service: a test that re-asserted the service's own constant
/// would pass however that constant were worded.
const DIFFERENT_NETWORKS_SENTENCE: &str =
    "You have specified two resources that belong to different networks.";

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

/// One VPC of `cidr_block` and one security group inside it: the two ids.
async fn network(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> (String, String) {
    let created = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    let vpc_id = member(case, &created, "vpcId");
    let group = ok(
        case,
        router,
        "CreateSecurityGroup",
        &[
            param("VpcId", &vpc_id),
            param("GroupName", &format!("group-in-{cidr_block}")),
            param("GroupDescription", "a group a rule may reference"),
        ],
    )
    .await;
    let group_id = member(case, &group, "groupId");
    (vpc_id, group_id)
}

/// A rule referencing a group of another VPC is refused with EC2's generic
/// different-networks sentence, and one that identifies that network — or stays inside the
/// rule's own — is stored.
#[tokio::test]
async fn a_reference_across_networks_is_refused_with_the_generic_sentence() {
    let case = "three networks";
    let (router, _engine) = super::ec2_query::emulator();
    let (first_vpc, first_group) = network(case, &router, "10.40.0.0/16").await;
    let (other_vpc, other_group) = network(case, &router, "10.41.0.0/16").await;
    let (third_vpc, _) = network(case, &router, "10.42.0.0/16").await;

    // (case, the referenced group, what the reference says about its network, served?)
    let rows: Vec<(&str, &str, Option<&str>, bool)> = vec![
        (
            "a group of the rule's own VPC",
            first_group.as_str(),
            None,
            true,
        ),
        (
            "a group of another VPC, with nothing identifying it",
            other_group.as_str(),
            None,
            false,
        ),
        (
            "a group of another VPC, naming the rule's own VPC",
            other_group.as_str(),
            Some(first_vpc.as_str()),
            false,
        ),
        (
            "a group of another VPC, naming a third VPC",
            other_group.as_str(),
            Some(third_vpc.as_str()),
            false,
        ),
        (
            "a group of another VPC, naming the VPC it is really in",
            other_group.as_str(),
            Some(other_vpc.as_str()),
            true,
        ),
    ];

    // One port per (row, direction), so a stored rule never duplicates one already stored —
    // a duplicate is refused for being a duplicate, which is a different rule than this one.
    let mut port = 4000;
    let mut stored_ports: Vec<String> = Vec::new();
    for (case, referenced, names_vpc, served) in rows {
        for action in AUTHORIZES {
            port += 1;
            let mut params = vec![
                param("GroupId", &first_group),
                param("IpPermissions.1.IpProtocol", "tcp"),
                param("IpPermissions.1.FromPort", &port.to_string()),
                param("IpPermissions.1.ToPort", &port.to_string()),
                param("IpPermissions.1.Groups.1.GroupId", referenced),
            ];
            if let Some(vpc_id) = names_vpc {
                params.push(param("IpPermissions.1.Groups.1.VpcId", vpc_id));
            }
            let (status, body) = call(&router, action, &params).await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {case:?} through {action}: expected the rule to be stored, got \
                     {status} with body {body}"
                );
                stored_ports.push(port.to_string());
                continue;
            }
            assert_eq!(
                status, 400,
                "case {case:?} through {action}: expected HTTP 400, got {status} with body \
                 {body}"
            );
            assert_eq!(
                error_code(&body).as_str(),
                ec2::state::INVALID_GROUP_NOT_FOUND,
                "case {case:?} through {action}: expected the not-found class EC2 answers a \
                 reference it cannot see, got {body}"
            );
            assert_eq!(
                error_message(&body),
                DIFFERENT_NETWORKS_SENTENCE,
                "case {case:?} through {action}: expected EC2's generic different-networks \
                 sentence, got {body}"
            );
        }
    }

    // Each refusal stored nothing: the rules the group holds are exactly the ones the rows
    // above were served, each identified by the port only it used. The group's own default
    // egress rule covers all traffic and carries no port, which is the `-1` left out here.
    let described = ok(
        case,
        &router,
        "DescribeSecurityGroupRules",
        &[
            param("Filter.1.Name", "group-id"),
            param("Filter.1.Value.1", &first_group),
        ],
    )
    .await;
    let mut found: Vec<String> = super::ec2_query::elements(&described, "fromPort")
        .into_iter()
        .filter(|port| port != "-1")
        .collect();
    found.sort();
    stored_ports.sort();
    assert_eq!(
        found, stored_ports,
        "expected the group to hold exactly the rules that were served, got {described}"
    );
}
