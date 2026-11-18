// SPDX-License-Identifier: CC0-1.0

//! What a network ACL entry CARRIES once stored, and what `CreateNetworkAclEntry` refuses as a
//! protocol — driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! Two rules, each asserted over a TABLE rather than over one request:
//!
//! 1. A reported entry a CALLER wrote carries exactly the optional members its RULE has: "the
//!    range of ports the rule applies to … Required if specifying protocol 6 (TCP) or 17 (UDP)"
//!    (`CreateNetworkAclEntryRequest$PortRange`) and "the ICMP or ICMPv6 type and code …
//!    Required if specifying protocol 1 (ICMP) or protocol 58 (ICMPv6) with an IPv6 CIDR block"
//!    (`$IcmpTypeCode`) — so a rule written with ports reports a port range, a rule written with
//!    an ICMP type and code reports one, and a rule whose protocol has neither (every protocol,
//!    or a protocol that is not TCP, UDP or ICMP) reports NEITHER member rather than an empty
//!    one. An empty port range on an all-traffic rule a caller wrote reads as a port
//!    restriction the rule does not carry. An IMPLICIT entry — one the SERVICE wrote onto the
//!    ACL — reports both members as present-but-empty elements instead, which is what an ACL a
//!    create answers carries.
//!
//! 2. "The protocol number. A value of `-1` means all protocols" (`$Protocol`): a value that is
//!    not a protocol number is refused with EC2's own sentence for an unknown IP protocol,
//!    whichever way it fails to be one — a protocol NAME, a number past the single byte a
//!    protocol number occupies, or anything else.
//!
//! The rows of each table differ along what its rule generalises over: the protocol (every
//! protocol, TCP, UDP, ICMP, ICMPv6 over either address family, and one protocol number that is
//! none of them), the direction, the address family, and — for the refusals — the published
//! bounds of a protocol number and one value past each of them.

use ec2::state::{ALL_IPV4_ADDRESSES, ALL_IP_PROTOCOLS, MAX_PROTOCOL_NUMBER, RULE_ACTION_ALLOW};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for. A network ACL carries no Region member.
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

/// A VPC and a network ACL of its own, as the ACL's id — every row writes its rules onto one.
async fn network_acl(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    let vpc_id = element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: no vpcId in {body}"));
    let body = ok(case, router, "CreateNetworkAcl", &[param("VpcId", &vpc_id)]).await;
    element(&body, "networkAclId")
        .unwrap_or_else(|| panic!("case {case:?}: no networkAclId in {body}"))
}

/// Whether the entry at `rule_number` in the direction `egress` names carries the element
/// `member`, and what its two children hold — read off the TREE, so one entry's ports are never
/// read as another's.
///
/// `None` is a member the response left out; `Some((from, to))` one it wrote, with the text of
/// each child it carries.
fn reported_member(
    case: &str,
    body: &str,
    rule_number: i32,
    egress: bool,
    member: &str,
    children: (&str, &str),
) -> Option<(Option<String>, Option<String>)> {
    let parsed = document(case, body);
    let acl = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "entrySet").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no network ACL in {body}"));
    let entry = set_items(acl, "entrySet")
        .into_iter()
        .find(|entry| {
            child_text(*entry, "ruleNumber").as_deref() == Some(rule_number.to_string().as_str())
                && child_text(*entry, "egress").as_deref() == Some(egress.to_string().as_str())
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: no entry {rule_number} (egress {egress}) in {body}")
        });
    child(entry, member).map(|found| {
        (
            child_text(found, children.0),
            child_text(found, children.1),
        )
    })
}

// ---------------------------------------------------------------------------
// Rule 1: a stored entry carries the members its rule has
// ---------------------------------------------------------------------------

/// One row: a rule written over one protocol, and the two optional members it must read back
/// with.
struct MemberCase {
    case: &'static str,
    rule_number: i32,
    egress: bool,
    /// `$Protocol`, as the request spells it.
    protocol: &'static str,
    /// The range the rule matches, and whether it is the IPv6 one.
    cidr_block: &'static str,
    ipv6: bool,
    /// `PortRange.From` / `PortRange.To`, when the row writes a port range.
    ports: Option<(i32, i32)>,
    /// `Icmp.Type` / `Icmp.Code`, when the row writes an ICMP type and code.
    icmp: Option<(i32, i32)>,
}

/// Each rule reads back carrying exactly the members it was written with: the port range of a
/// TCP or UDP rule, the ICMP type and code of an ICMP or ICMPv6 rule, and neither for a rule
/// whose protocol has no ports and no ICMP numbers.
#[tokio::test]
async fn an_entry_reports_only_the_optional_members_its_rule_carries() {
    let router = emulator();
    let acl_id = network_acl("the member table", &router, "10.61.0.0/16").await;

    // The rows: every protocol in each direction, TCP and UDP (which require a port range),
    // ICMP over IPv4 and ICMPv6 over IPv6 (which require a type and code), and a protocol number
    // that is none of those four — which has neither ports nor ICMP numbers.
    let rows: [MemberCase; 7] = [
        MemberCase {
            case: "every protocol, inbound",
            rule_number: 10,
            egress: false,
            protocol: ALL_IP_PROTOCOLS,
            cidr_block: ALL_IPV4_ADDRESSES,
            ipv6: false,
            ports: None,
            icmp: None,
        },
        MemberCase {
            case: "every protocol, outbound",
            rule_number: 20,
            egress: true,
            protocol: ALL_IP_PROTOCOLS,
            cidr_block: ALL_IPV4_ADDRESSES,
            ipv6: false,
            ports: None,
            icmp: None,
        },
        MemberCase {
            case: "tcp over one port",
            rule_number: 30,
            egress: false,
            protocol: "6",
            cidr_block: "10.61.0.0/16",
            ipv6: false,
            ports: Some((22, 22)),
            icmp: None,
        },
        MemberCase {
            case: "udp over a range of ports",
            rule_number: 40,
            egress: true,
            protocol: "17",
            cidr_block: ALL_IPV4_ADDRESSES,
            ipv6: false,
            ports: Some((1024, 65535)),
            icmp: None,
        },
        MemberCase {
            case: "icmp over an IPv4 range",
            rule_number: 50,
            egress: false,
            protocol: "1",
            cidr_block: ALL_IPV4_ADDRESSES,
            ipv6: false,
            ports: None,
            icmp: Some((8, -1)),
        },
        MemberCase {
            case: "icmpv6 over an IPv6 range",
            rule_number: 60,
            egress: false,
            protocol: "58",
            cidr_block: "::/0",
            ipv6: true,
            ports: None,
            icmp: Some((128, 0)),
        },
        MemberCase {
            case: "a protocol that is neither tcp, udp nor icmp",
            rule_number: 70,
            egress: true,
            protocol: "47",
            cidr_block: ALL_IPV4_ADDRESSES,
            ipv6: false,
            ports: None,
            icmp: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut params = vec![
            param("NetworkAclId", &acl_id),
            param("RuleNumber", &row.rule_number.to_string()),
            param("Egress", &row.egress.to_string()),
            param("RuleAction", RULE_ACTION_ALLOW),
            param("Protocol", row.protocol),
            param(
                if row.ipv6 { "Ipv6CidrBlock" } else { "CidrBlock" },
                row.cidr_block,
            ),
        ];
        if let Some((from, to)) = row.ports {
            params.push(param("PortRange.From", &from.to_string()));
            params.push(param("PortRange.To", &to.to_string()));
        }
        if let Some((icmp_type, code)) = row.icmp {
            params.push(param("Icmp.Type", &icmp_type.to_string()));
            params.push(param("Icmp.Code", &code.to_string()));
        }
        let _ = ok(case, &router, "CreateNetworkAclEntry", &params).await;

        let described = ok(
            case,
            &router,
            "DescribeNetworkAcls",
            &indexed("NetworkAclId", &[acl_id.as_str()]),
        )
        .await;
        let ports = reported_member(
            case,
            &described,
            row.rule_number,
            row.egress,
            "portRange",
            ("from", "to"),
        );
        let expected_ports = row
            .ports
            .map(|(from, to)| (Some(from.to_string()), Some(to.to_string())));
        assert_eq!(
            ports, expected_ports,
            "case {case:?}: expected the port range {expected_ports:?}, got {ports:?}"
        );
        let icmp = reported_member(
            case,
            &described,
            row.rule_number,
            row.egress,
            "icmpTypeCode",
            ("type", "code"),
        );
        let expected_icmp = row
            .icmp
            .map(|(icmp_type, code)| (Some(icmp_type.to_string()), Some(code.to_string())));
        assert_eq!(
            icmp, expected_icmp,
            "case {case:?}: expected the ICMP type and code {expected_icmp:?}, got {icmp:?}"
        );
    }

    // And the catch-all deny the ACL was created with — an IMPLICIT entry the SERVICE wrote,
    // over every protocol — reports each member as a present-but-empty element, which is what
    // separates it from a caller's rule over every protocol.
    let described = ok(
        "the catch-all deny",
        &router,
        "DescribeNetworkAcls",
        &indexed("NetworkAclId", &[acl_id.as_str()]),
    )
    .await;
    for egress in [false, true] {
        let case = "the catch-all deny";
        for (member, children) in [("portRange", ("from", "to")), ("icmpTypeCode", ("type", "code"))]
        {
            let reported = reported_member(
                case,
                &described,
                ec2::state::DEFAULT_NETWORK_ACL_DENY_RULE_NUMBER,
                egress,
                member,
                children,
            );
            assert_eq!(
                reported,
                Some((None, None)),
                "case {case:?} (egress {egress}): an implicit rule over every protocol reports \
                 {member} as a present-but-empty element, got {reported:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: what a protocol may be
// ---------------------------------------------------------------------------

/// The protocol numbers a rule may name are accepted — each published bound of the byte a
/// protocol number occupies, and the value that stands for every protocol — and each reads back
/// as the number it names.
#[tokio::test]
async fn every_published_protocol_number_is_stored_as_written() {
    let router = emulator();
    let acl_id = network_acl("the accepted protocols", &router, "10.62.0.0/16").await;

    // The published bounds of `$Protocol` ("the protocol number … between 0 and 255, or -1 for
    // all protocols"), and one number inside the range that is not TCP, UDP or ICMP.
    let accepted = [
        ALL_IP_PROTOCOLS.to_string(),
        "0".to_string(),
        MAX_PROTOCOL_NUMBER.to_string(),
        "47".to_string(),
    ];
    for (index, protocol) in accepted.iter().enumerate() {
        let case = protocol.as_str();
        let rule_number = 100 + i32::try_from(index).unwrap_or(0);
        let _ = ok(
            case,
            &router,
            "CreateNetworkAclEntry",
            &[
                param("NetworkAclId", &acl_id),
                param("RuleNumber", &rule_number.to_string()),
                param("Egress", "false"),
                param("RuleAction", RULE_ACTION_ALLOW),
                param("Protocol", protocol),
                param("CidrBlock", ALL_IPV4_ADDRESSES),
            ],
        )
        .await;
        let described = ok(
            case,
            &router,
            "DescribeNetworkAcls",
            &indexed("NetworkAclId", &[acl_id.as_str()]),
        )
        .await;
        let parsed = document(case, &described);
        let acl = parsed
            .descendants()
            .find(|node| node.is_element() && child(*node, "entrySet").is_some())
            .unwrap_or_else(|| panic!("case {case:?}: no network ACL in {described}"));
        let stored = set_items(acl, "entrySet")
            .into_iter()
            .find(|entry| {
                child_text(*entry, "ruleNumber").as_deref()
                    == Some(rule_number.to_string().as_str())
                    && child_text(*entry, "egress").as_deref() == Some("false")
            })
            .and_then(|entry| child_text(entry, "protocol"));
        assert_eq!(
            stored.as_deref(),
            Some(protocol.as_str()),
            "case {case:?}: expected the rule to report protocol {protocol:?}, got {stored:?}"
        );
    }
}

/// A value that is no protocol number is refused with EC2's own unknown-IP-protocol sentence,
/// whichever way it fails to be one — and nothing is stored for it.
#[tokio::test]
async fn a_value_that_is_no_protocol_number_is_refused_with_the_unknown_protocol_sentence() {
    let router = emulator();
    let acl_id = network_acl("the refused protocols", &router, "10.63.0.0/16").await;

    // The rows: the protocol NAMES a security group rule may carry (which a network ACL entry
    // may not), one value past each published bound of a protocol number, a number with a
    // fraction, and a word that is no protocol at all.
    let refused = [
        "tcp",
        "udp",
        "icmp",
        "icmpv6",
        "256",
        "-2",
        "6.0",
        "all",
    ];
    for value in refused {
        let (status, body) = call(
            &router,
            "CreateNetworkAclEntry",
            &[
                param("NetworkAclId", &acl_id),
                param("RuleNumber", "200"),
                param("Egress", "false"),
                param("RuleAction", RULE_ACTION_ALLOW),
                param("Protocol", value),
                param("CidrBlock", ALL_IPV4_ADDRESSES),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {value:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::INVALID_PARAMETER_VALUE,
            "case {value:?}: expected InvalidParameterValue, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Invalid value '{value}' for IP protocol. Unknown protocol."),
            "case {value:?}: expected EC2's unknown-protocol sentence, got {body}"
        );

        let described = ok(
            value,
            &router,
            "DescribeNetworkAcls",
            &indexed("NetworkAclId", &[acl_id.as_str()]),
        )
        .await;
        let parsed = document(value, &described);
        let acl = parsed
            .descendants()
            .find(|node| node.is_element() && child(*node, "entrySet").is_some())
            .unwrap_or_else(|| panic!("case {value:?}: no network ACL in {described}"));
        let stored_at_200 = set_items(acl, "entrySet")
            .into_iter()
            .any(|entry| child_text(entry, "ruleNumber").as_deref() == Some("200"));
        assert!(
            !stored_at_200,
            "case {value:?}: a refused create must store nothing, got {described}"
        );
    }
}
