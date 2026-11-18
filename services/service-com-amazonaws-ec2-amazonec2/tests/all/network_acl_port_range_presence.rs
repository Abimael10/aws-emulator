// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHICH network ACL entries carry a `portRange`, driven over the wire
//! the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rule under test: an entry's `portRange` and `icmpTypeCode` are decided by the RULE and
//! by nothing else — "the range of ports the rule applies to … Required if specifying
//! protocol 6 (TCP) or 17 (UDP)" (`CreateNetworkAclEntryRequest$PortRange`), "the ICMP or
//! ICMPv6 type and code … Required if specifying protocol 1 (ICMP) or protocol 58 (ICMPv6)
//! with an IPv6 CIDR block" (`$IcmpTypeCode`) — so an entry over EVERY protocol reports
//! neither member, whether a caller wrote it or the service wrote it onto the ACL, because a
//! port range on an all-traffic rule reads as a port restriction that rule does not carry.
//!
//! The rows span what the rule generalises over: the protocol (TCP and UDP, which require
//! ports; ICMP over an IPv6 range, which requires a type and code; a protocol that has
//! neither; and every protocol), the direction, which ACL the entries are on (one a caller
//! created and the default ACL its VPC came with, whose implicit entries the service wrote),
//! and which reader reports them (`CreateNetworkAclEntry`'s own answer and
//! `DescribeNetworkAcls`).
//!
//! The ec2Query harness ([`super::ec2_query`]) and the XML tree readers ([`super::wire`]) are
//! this crate's own, reused rather than stood up again.

use ec2::state::{
    ALL_IPV4_ADDRESSES, ALL_IP_PROTOCOLS, DEFAULT_NETWORK_ACL_DENY_RULE_NUMBER,
    NETWORK_ACL_PROTOCOL_ICMP, NETWORK_ACL_PROTOCOL_TCP, NETWORK_ACL_PROTOCOL_UDP,
    RULE_ACTION_ALLOW, RULE_ACTION_DENY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, indexed};
use super::wire::{child, child_text, document, param, set_items};

/// The Region every call here is signed for. A network ACL carries no Region member.
const REGION: &str = "us-east-1";

/// An IPv6 range, for the row whose rule needs one: ICMPv6 and ICMP numbers are required
/// together only over an IPv6 CIDR block.
const IPV6_RANGE: &str = "2001:db8::/32";

/// One rule a row writes, and what its two optional members must read back as.
struct Row {
    /// What the row is, named in every assertion.
    case: &'static str,
    rule_number: i32,
    egress: bool,
    rule_action: &'static str,
    /// The protocol number the rule matches, as a request spells it.
    protocol: String,
    /// `true` for a rule written over an IPv6 range rather than an IPv4 one.
    ipv6: bool,
    /// "The range of ports the rule applies to", where the rule names one.
    ports: Option<(i32, i32)>,
    /// "The ICMP or ICMPv6 type and code", where the rule names one.
    icmp: Option<(i32, i32)>,
}

/// The rules a caller writes: one per protocol class the rule distinguishes, in both
/// directions, including the all-traffic rule the regression is about.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a TCP rule, which names ports",
            rule_number: 50,
            egress: false,
            rule_action: RULE_ACTION_DENY,
            protocol: NETWORK_ACL_PROTOCOL_TCP.to_string(),
            ipv6: false,
            ports: Some((22, 22)),
            icmp: None,
        },
        Row {
            case: "an all-traffic allow, which names neither",
            rule_number: 100,
            egress: false,
            rule_action: RULE_ACTION_ALLOW,
            protocol: ALL_IP_PROTOCOLS.to_string(),
            ipv6: false,
            ports: None,
            icmp: None,
        },
        Row {
            case: "a UDP rule over every port",
            rule_number: 150,
            egress: true,
            rule_action: RULE_ACTION_ALLOW,
            protocol: NETWORK_ACL_PROTOCOL_UDP.to_string(),
            ipv6: false,
            ports: Some((0, 65_535)),
            icmp: None,
        },
        Row {
            case: "an all-traffic allow in the other direction",
            rule_number: 200,
            egress: true,
            rule_action: RULE_ACTION_ALLOW,
            protocol: ALL_IP_PROTOCOLS.to_string(),
            ipv6: false,
            ports: None,
            icmp: None,
        },
        Row {
            case: "an ICMP rule over an IPv6 range, which names a type and code",
            rule_number: 250,
            egress: true,
            rule_action: RULE_ACTION_ALLOW,
            protocol: NETWORK_ACL_PROTOCOL_ICMP.to_string(),
            ipv6: true,
            ports: None,
            icmp: Some((-1, -1)),
        },
        Row {
            // 47 is GRE: a protocol that has neither ports nor ICMP numbers.
            case: "a rule over a protocol with neither ports nor ICMP numbers",
            rule_number: 300,
            egress: false,
            rule_action: RULE_ACTION_ALLOW,
            protocol: "47".to_string(),
            ipv6: false,
            ports: None,
            icmp: None,
        },
    ]
}

/// The form fields of the `CreateNetworkAclEntry` request that writes `row` onto
/// `network_acl_id`.
fn create_params(network_acl_id: &str, row: &Row) -> Vec<(String, String)> {
    let mut params = vec![
        param("NetworkAclId", network_acl_id),
        param("RuleNumber", &row.rule_number.to_string()),
        param("Egress", &row.egress.to_string()),
        param("RuleAction", row.rule_action),
        param("Protocol", &row.protocol),
    ];
    if row.ipv6 {
        params.push(param("Ipv6CidrBlock", IPV6_RANGE));
    } else {
        params.push(param("CidrBlock", ALL_IPV4_ADDRESSES));
    }
    if let Some((from, to)) = row.ports {
        params.push(param("PortRange.From", &from.to_string()));
        params.push(param("PortRange.To", &to.to_string()));
    }
    if let Some((icmp_type, code)) = row.icmp {
        params.push(param("Icmp.Type", &icmp_type.to_string()));
        params.push(param("Icmp.Code", &code.to_string()));
    }
    params
}

/// `(rule number, egress)` of every entry of `network_acl_id` in `body` that carries a
/// `portRange` element at all, and the same for `icmpTypeCode`.
///
/// Read off the TREE: an entry's members are children of its own `<item>`, so scanning the
/// whole body for an element name would mix two entries together.
type Carried = (Vec<(i32, bool)>, Vec<(i32, bool)>);
fn members_carried(case: &str, body: &str, network_acl_id: &str) -> Carried {
    let parsed = document(case, body);
    let acl = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkAclId").as_deref() == Some(network_acl_id)
                && child(*node, "entrySet").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no network ACL {network_acl_id} in {body}"));
    let mut ported = Vec::new();
    let mut icmped = Vec::new();
    for entry in set_items(acl, "entrySet") {
        let rule_number = child_text(entry, "ruleNumber")
            .and_then(|text| text.parse::<i32>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: an entry carries no ruleNumber in {body}"));
        let egress = child_text(entry, "egress").as_deref() == Some("true");
        if child(entry, "portRange").is_some() {
            ported.push((rule_number, egress));
        }
        if child(entry, "icmpTypeCode").is_some() {
            icmped.push((rule_number, egress));
        }
    }
    ported.sort_unstable();
    icmped.sort_unstable();
    (ported, icmped)
}

/// A VPC created over the wire, by id.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let params = [param("CidrBlock", cidr_block)];
    let (status, body) = super::ec2_query::call(router, REGION, "CreateVpc", &params).await;
    assert_eq!(
        status, 200,
        "CreateVpc {cidr_block}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// A network ACL created in `vpc_id`, by id.
async fn create_acl(router: &MultiServiceRouter, vpc_id: &str) -> String {
    let params = [param("VpcId", vpc_id)];
    let (status, body) =
        super::ec2_query::call(router, REGION, "CreateNetworkAcl", &params).await;
    assert_eq!(
        status, 200,
        "CreateNetworkAcl in {vpc_id}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "networkAclId")
        .unwrap_or_else(|| panic!("CreateNetworkAcl answered no networkAclId: {body}"))
}

/// `DescribeNetworkAcls` for the one ACL `network_acl_id` names.
async fn describe(router: &MultiServiceRouter, network_acl_id: &str) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeNetworkAcls",
        &indexed("NetworkAclId", &[network_acl_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeNetworkAcls {network_acl_id}: expected HTTP 200, got {status} with body {body}"
    );
    body
}

#[tokio::test]
async fn only_a_rule_whose_protocol_has_ports_carries_a_port_range() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(&router, "10.80.0.0/16").await;
    let network_acl_id = create_acl(&router, &vpc_id).await;

    // The ACL a create answers carries the implicit catch-all deny in each direction: an
    // entry over every protocol, so neither member is reported on it either.
    let created_body = describe(&router, &network_acl_id).await;
    let (ported, icmped) = members_carried("the created ACL", &created_body, &network_acl_id);
    assert_eq!(
        ported,
        Vec::<(i32, bool)>::new(),
        "case the created ACL: the catch-all deny matches every protocol, so no entry carries \
         a portRange, got {ported:?}"
    );
    assert_eq!(
        icmped,
        Vec::<(i32, bool)>::new(),
        "case the created ACL: the catch-all deny carries no icmpTypeCode either, got \
         {icmped:?}"
    );

    for row in rows() {
        let (status, body) = super::ec2_query::call(
            &router,
            REGION,
            "CreateNetworkAclEntry",
            &create_params(&network_acl_id, &row),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from CreateNetworkAclEntry, got {status} with body \
             {body}",
            row.case
        );
    }

    // What every reader must now report: a portRange exactly on the rules that name ports,
    // an icmpTypeCode exactly on the rule that names a type and code, and neither on any
    // all-traffic rule — the caller's and the two the service wrote alike.
    let expected_ported: Vec<(i32, bool)> = {
        let mut expected: Vec<(i32, bool)> = rows()
            .iter()
            .filter(|row| row.ports.is_some())
            .map(|row| (row.rule_number, row.egress))
            .collect();
        expected.sort_unstable();
        expected
    };
    let expected_icmped: Vec<(i32, bool)> = {
        let mut expected: Vec<(i32, bool)> = rows()
            .iter()
            .filter(|row| row.icmp.is_some())
            .map(|row| (row.rule_number, row.egress))
            .collect();
        expected.sort_unstable();
        expected
    };
    assert!(
        !expected_ported.contains(&(DEFAULT_NETWORK_ACL_DENY_RULE_NUMBER, false)),
        "the table must not expect ports on the implicit catch-all, got {expected_ported:?}"
    );

    let body = describe(&router, &network_acl_id).await;
    let (ported, icmped) = members_carried("the written rules", &body, &network_acl_id);
    assert_eq!(
        ported, expected_ported,
        "case the written rules: expected a portRange only on the rules that name ports, got \
         {ported:?}"
    );
    assert_eq!(
        icmped, expected_icmped,
        "case the written rules: expected an icmpTypeCode only on the rule that names one, \
         got {icmped:?}"
    );
}

#[tokio::test]
async fn the_entries_a_vpcs_default_acl_comes_with_carry_neither_member() {
    // The VPC's own default ACL is written by the SERVICE and carries the allow-everything
    // pair beside the catch-all deny: every one of them is a rule over every protocol, so
    // none of them reports a portRange or an icmpTypeCode.
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(&router, "10.81.0.0/16").await;
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "DescribeNetworkAcls",
        &super::ec2_query::filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the default ACL: expected HTTP 200, got {status} with body {body}"
    );
    let network_acl_id = element(&body, "networkAclId")
        .unwrap_or_else(|| panic!("case the default ACL: no ACL for {vpc_id} in {body}"));
    let (ported, icmped) = members_carried("the default ACL", &body, &network_acl_id);
    assert_eq!(
        ported,
        Vec::<(i32, bool)>::new(),
        "case the default ACL: an all-traffic rule carries no portRange, got {ported:?}"
    );
    assert_eq!(
        icmped,
        Vec::<(i32, bool)>::new(),
        "case the default ACL: an all-traffic rule carries no icmpTypeCode, got {icmped:?}"
    );
}
