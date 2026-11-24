// SPDX-License-Identifier: CC0-1.0

//! How a security group rule's IP-PROTOCOL token is read, and the SCOPE a revoke resolves a
//! `SecurityGroupRuleId` in — driven over `ec2Query` (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! Four rules are under test, and each is asserted over a table whose rows differ along what
//! that rule generalises over:
//!
//! 1. `IpPermission$IpProtocol` is read case-insensitively, and the word for every protocol
//!    is an alias of the number the model gives for it: "use `-1` to specify all protocols".
//!    A rule authorized under any spelling of it is ONE rule, stored and reported as `-1`,
//!    with no port range at all — "specifying `-1` … allows traffic on all ports, regardless
//!    of any port range you specify" — so a second grant of it is the duplicate refusal and a
//!    revoke naming any port range still names it.
//! 2. A token that is neither one of the four protocol NAMES the model lists, nor a protocol
//!    number, nor `-1` (nor the word for it), names no protocol: it is refused
//!    `InvalidParameterValue`, `Invalid value '<token>' for IP protocol. Unknown protocol.`
//! 3. An ICMPv6 rule whose peer is an address RANGE carries no type and code: the port range
//!    is either absent or the pair that stands for every type and code, and anything else is
//!    refused naming the protocol by its own number.
//! 4. A `SecurityGroupRuleId` is resolved across BOTH sides of the group that holds it, so an
//!    id of the group's other side is refused `InvalidParameterValue` as that side's rule,
//!    while `InvalidSecurityGroupRuleId.NotFound` is left for an id the group holds no rule
//!    under at all.
//!
//! Every group here is created in a VPC of its own, which is NOT the Region's default VPC:
//! "for a non-default VPC … an `InvalidPermission.NotFound` client error is returned"
//! (`RevokeSecurityGroupIngress`), so a revoke that fails to match the rule it names is a
//! refusal here rather than a forgiven 200 — which is what makes "the revoke matched the
//! rule" an assertion at all.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, emulator, error_code, error_message};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The two operations that authorize a rule, and the two that revoke one.
const AUTHORIZE_INGRESS: &str = "AuthorizeSecurityGroupIngress";
const AUTHORIZE_EGRESS: &str = "AuthorizeSecurityGroupEgress";
const REVOKE_INGRESS: &str = "RevokeSecurityGroupIngress";
const REVOKE_EGRESS: &str = "RevokeSecurityGroupEgress";

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

/// A VPC of the account, by its minted id — the network every group below is created in.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", cidr_block)],
    )
    .await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no <vpcId> in {body}"))
}

/// A security group of `vpc_id`, by its minted id — with the outbound rule every new group
/// carries already revoked.
///
/// "A security group has an outbound rule that allows all outbound traffic" (Amazon VPC User
/// Guide, "Security group rules"), which on the wire is the `-1` protocol over `0.0.0.0/0`.
/// Revoking it here is what makes every assertion below about the rule its own row
/// authorized, on either side, rather than about that rule beside it.
async fn create_group(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    group_name: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateSecurityGroup",
        &[
            param("VpcId", vpc_id),
            param("GroupName", group_name),
            param("GroupDescription", "the group whose rules are read"),
        ],
    )
    .await;
    let group_id =
        element(&body, "groupId").unwrap_or_else(|| panic!("case {case:?}: no <groupId> in {body}"));
    let mut params = vec![param("GroupId", &group_id)];
    params.extend(permission_params(
        1,
        "-1",
        None,
        &Peer::Ipv4("0.0.0.0/0".to_string()),
    ));
    ok(case, router, REVOKE_EGRESS, &params).await;
    group_id
}

/// The form fields of ONE permission at `position` (counting from 1): the protocol, the port
/// range where the row carries one, and the single peer `peer` names.
fn permission_params(
    position: usize,
    protocol: &str,
    ports: Option<(i32, i32)>,
    peer: &Peer,
) -> Vec<Param> {
    let base = format!("IpPermissions.{position}");
    let mut params = vec![param(&format!("{base}.IpProtocol"), protocol)];
    if let Some((from_port, to_port)) = ports {
        params.push(param(&format!("{base}.FromPort"), &from_port.to_string()));
        params.push(param(&format!("{base}.ToPort"), &to_port.to_string()));
    }
    params.extend(peer.params(&base));
    params
}

/// What a rule's one source names — "you must specify exactly one of the following sources:
/// an IPv4 or IPv6 address range, a prefix list, or a security group"
/// (`AuthorizeSecurityGroupIngress`). The first two are the ADDRESS RANGE forms, which is the
/// peer kind rule 3 above is about.
#[derive(Debug, Clone)]
enum Peer {
    Ipv4(String),
    Ipv6(String),
    GroupById(String),
}

impl Peer {
    /// The form fields naming this source inside the permission at `base`.
    fn params(&self, base: &str) -> Vec<Param> {
        let (list, member, value) = match self {
            Self::Ipv4(cidr) => ("IpRanges", "CidrIp", cidr),
            Self::Ipv6(cidr) => ("Ipv6Ranges", "CidrIpv6", cidr),
            Self::GroupById(group_id) => ("Groups", "GroupId", group_id),
        };
        vec![param(&format!("{base}.{list}.1.{member}"), value)]
    }
}

/// `(ipProtocol, fromPort, toPort)` of every rule `DescribeSecurityGroupRules` reports for
/// `group_id` on the `is_egress` side, in the order it reported them.
async fn described_rules(
    case: &str,
    router: &MultiServiceRouter,
    group_id: &str,
    is_egress: bool,
) -> Vec<(String, Option<String>, Option<String>)> {
    let body = ok(
        case,
        router,
        "DescribeSecurityGroupRules",
        &[
            param("Filter.1.Name", "group-id"),
            param("Filter.1.Value.1", group_id),
        ],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "securityGroupRuleId").is_some())
        .filter(|node| {
            child_text(*node, "isEgress").as_deref() == Some(if is_egress { "true" } else { "false" })
        })
        .map(|node| {
            (
                child_text(node, "ipProtocol").unwrap_or_default(),
                child_text(node, "fromPort"),
                child_text(node, "toPort"),
            )
        })
        .collect()
}

/// `(ipProtocol, fromPort, toPort)` of every permission `DescribeSecurityGroups` reports in
/// `set_name` — `ipPermissions` for the inbound side, `ipPermissionsEgress` for the outbound
/// one — for the group `group_id`.
async fn described_permissions(
    case: &str,
    router: &MultiServiceRouter,
    group_id: &str,
    set_name: &str,
) -> Vec<(String, Option<String>, Option<String>)> {
    let body = ok(
        case,
        router,
        "DescribeSecurityGroups",
        &[param("GroupId.1", group_id)],
    )
    .await;
    let parsed = document(case, &body);
    let group = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "groupDescription").is_some()
                && child_text(*node, "groupId").as_deref() == Some(group_id)
        })
        .unwrap_or_else(|| panic!("case {case:?}: no group {group_id} in {body}"));
    set_items(group, set_name)
        .into_iter()
        .map(|item| {
            (
                child_text(item, "ipProtocol").unwrap_or_default(),
                child_text(item, "fromPort"),
                child_text(item, "toPort"),
            )
        })
        .collect()
}

/// Assert a refusal's status, code and whole message.
fn assert_refusal(case: &str, status: u16, body: &str, code: &str, message: &str) {    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    let actual_code = error_code(body);
    assert_eq!(
        actual_code, code,
        "case {case:?}: expected the code {code:?}, got {actual_code:?} in {body}"
    );
    let actual_message = error_message(body);
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// One row of the all-protocols table: the spelling the rule is authorized under and the
/// range it names, beside the spelling and the range the second grant and the revoke use.
#[derive(Debug, Clone, Copy)]
struct AliasRow {
    case: &'static str,
    authorized: &'static str,
    ports: Option<(i32, i32)>,
    revoked_as: &'static str,
    revoke_ports: Option<(i32, i32)>,
    is_egress: bool,
}

/// One row of the protocol-NAME table: the casing the rule is authorized under, the casing
/// the second grant uses, the spelling it is stored in, and the protocol as the duplicate
/// refusal names it.
#[derive(Debug, Clone)]
struct NameRow {
    case: &'static str,
    authorized: &'static str,
    again_as: &'static str,
    stored: &'static str,
    ports: (i32, i32),
    peer: Peer,
    is_egress: bool,
    descriptor: &'static str,
}

/// One row of the protocol-TOKEN table: the token, and whether it names a protocol at all.
#[derive(Debug, Clone, Copy)]
struct ProtocolRow {
    case: &'static str,
    token: &'static str,
    ports: Option<(i32, i32)>,
    names_a_protocol: bool,
}

impl ProtocolRow {
    /// A token no rule may name.
    fn refused(case: &'static str, token: &'static str) -> Self {
        Self {
            case,
            token,
            ports: None,
            names_a_protocol: false,
        }
    }

    /// A token that names a protocol, with the port range that protocol takes.
    fn served(case: &'static str, token: &'static str, ports: Option<(i32, i32)>) -> Self {
        Self {
            case,
            token,
            ports,
            names_a_protocol: true,
        }
    }
}

/// One row of the ICMPv6 table: the spelling of the protocol, the type and code the request
/// names, the peer it names them for, and whether such a rule is served.
#[derive(Debug, Clone)]
struct Icmpv6Row {
    case: &'static str,
    token: &'static str,
    ports: Option<(i32, i32)>,
    peer: Peer,
    is_egress: bool,
    served: bool,
}

/// The word for every protocol is an alias of `-1`, in any case: the rule is stored and
/// reported as `-1` with NO port range whatever range the request named, a second grant of it
/// under any spelling is the duplicate refusal, and a revoke that names it — under another
/// spelling, with a port range of its own — removes it.
#[tokio::test]
async fn every_protocol_is_named_by_minus_one_and_by_the_word_for_it() {
    let (router, _engine) = emulator();
    let vpc_id = create_vpc("the network", &router, "10.0.0.0/16").await;

    let rows: Vec<AliasRow> = vec![
        AliasRow {
            case: "the word, lower case, with a port range of zeroes",
            authorized: "all",
            ports: Some((0, 0)),
            revoked_as: "-1",
            revoke_ports: Some((0, 0)),
            is_egress: false,
        },
        AliasRow {
            case: "the word, upper case, with a port range of its own",
            authorized: "ALL",
            ports: Some((80, 443)),
            revoked_as: "all",
            revoke_ports: None,
            is_egress: false,
        },
        AliasRow {
            case: "the word, mixed case, with no port range at all",
            authorized: "All",
            ports: None,
            revoked_as: "ALL",
            revoke_ports: Some((1, 65_535)),
            is_egress: true,
        },
        AliasRow {
            case: "the number, with a port range the protocol has none of",
            authorized: "-1",
            ports: Some((22, 22)),
            revoked_as: "aLl",
            revoke_ports: Some((1024, 2048)),
            is_egress: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let &AliasRow {
            case,
            authorized,
            ports,
            revoked_as,
            revoke_ports,
            is_egress,
        } = row;
        let group_id = create_group(case, &router, &vpc_id, &format!("every-protocol-{index}")).await;
        let cidr = format!("10.{index}.0.0/16");
        let peer = Peer::Ipv4(cidr.clone());
        let authorize = if is_egress {
            AUTHORIZE_EGRESS
        } else {
            AUTHORIZE_INGRESS
        };
        let mut params = vec![param("GroupId", &group_id)];
        params.extend(permission_params(1, authorized, ports, &peer));

        // The echo: the rule as it was stored, which is the protocol the model gives for
        // every protocol and the range that stands for every port.
        let body = ok(case, &router, authorize, &params).await;
        for (member, expected) in [("ipProtocol", "-1"), ("fromPort", "-1"), ("toPort", "-1")] {
            let actual = element(&body, member);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected the echoed <{member}> {expected:?}, got {actual:?} \
                 in {body}"
            );
        }

        // The rules read back agree with the echo, and the aggregate view reports the
        // permission with NEITHER port member.
        let rules = described_rules(case, &router, &group_id, is_egress).await;
        assert_eq!(
            rules,
            vec![(
                "-1".to_string(),
                Some("-1".to_string()),
                Some("-1".to_string())
            )],
            "case {case:?}: expected the stored rule to be port-less, got {rules:?}"
        );
        let set_name = if is_egress {
            "ipPermissionsEgress"
        } else {
            "ipPermissions"
        };
        let permissions = described_permissions(case, &router, &group_id, set_name).await;
        assert_eq!(
            permissions,
            vec![("-1".to_string(), None, None)],
            "case {case:?}: expected the reported permission to carry no port member, got \
             {permissions:?}"
        );

        // One rule, so a second grant of it — however it is spelled, whatever range it
        // names — is the duplicate refusal, which names the rule as an all-protocols one.
        let mut again = vec![param("GroupId", &group_id)];
        again.extend(permission_params(1, revoked_as, revoke_ports, &peer));
        let (status, duplicate) = call(&router, authorize, &again).await;
        assert_refusal(
            case,
            status,
            &duplicate,
            ec2::state::INVALID_PERMISSION_DUPLICATE,
            &format!("the specified rule \"peer: {cidr}, ALL, ALLOW\" already exists"),
        );

        // And the rule the revoke names is that same one rule: the port range is no part of
        // it, so a revoke carrying one still matches.
        let revoke = if is_egress {
            REVOKE_EGRESS
        } else {
            REVOKE_INGRESS
        };
        let removed = ok(case, &router, revoke, &again).await;
        assert_eq!(
            element(&removed, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected <return>true</return> from {revoke}, got {removed}"
        );
        let left = described_rules(case, &router, &group_id, is_egress).await;
        assert!(
            left.is_empty(),
            "case {case:?}: expected the revoke to remove the rule, got {left:?}"
        );
    }
}

/// A protocol NAME is matched case-insensitively and stored in the one spelling the model
/// writes it in, with the port range the protocol carries kept exactly as given — so a second
/// grant under another casing is the duplicate refusal for the same rule.
#[tokio::test]
async fn protocol_names_are_read_case_insensitively() {
    let (router, _engine) = emulator();
    let vpc_id = create_vpc("the network", &router, "10.1.0.0/16").await;

    // The four protocol names `IpPermission$IpProtocol` lists (`tcp`, `udp`, `icmp`,
    // `icmpv6`), each authorized under one casing and re-authorized under another:
    // (case, the spelling authorized with, the spelling re-authorized with, the stored
    // spelling, the range, the peer, the direction, the protocol as the refusal names it)
    let rows: Vec<NameRow> = vec![
        NameRow {
            case: "TCP in upper case, inbound",
            authorized: "TCP",
            again_as: "tcp",
            stored: "tcp",
            ports: (22, 22),
            peer: Peer::Ipv4("198.51.100.0/24".to_string()),
            is_egress: false,
            descriptor: "TCP, from port: 22, to port: 22",
        },
        NameRow {
            case: "UDP in mixed case, outbound",
            authorized: "Udp",
            again_as: "UDP",
            stored: "udp",
            ports: (53, 53),
            peer: Peer::Ipv4("203.0.113.0/24".to_string()),
            is_egress: true,
            descriptor: "UDP, from port: 53, to port: 53",
        },
        NameRow {
            case: "ICMP in upper case, inbound",
            authorized: "ICMP",
            again_as: "iCmP",
            stored: "icmp",
            ports: (8, -1),
            peer: Peer::Ipv4("192.0.2.0/24".to_string()),
            is_egress: false,
            descriptor: "ICMP, type: 8, code: -1",
        },
        NameRow {
            case: "ICMPv6 in mixed case, inbound",
            authorized: "ICMPv6",
            again_as: "icmpv6",
            stored: "icmpv6",
            ports: (-1, -1),
            peer: Peer::Ipv6("2001:db8::/32".to_string()),
            is_egress: false,
            descriptor: "ICMPV6, type: -1, code: -1",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let NameRow {
            case,
            authorized,
            again_as,
            stored,
            ports,
            peer,
            is_egress,
            descriptor,
        } = row;
        let (is_egress, ports) = (*is_egress, Some(*ports));
        let group_id = create_group(case, &router, &vpc_id, &format!("by-name-{index}")).await;
        let authorize = if is_egress {
            AUTHORIZE_EGRESS
        } else {
            AUTHORIZE_INGRESS
        };
        let mut params = vec![param("GroupId", &group_id)];
        params.extend(permission_params(1, authorized, ports, peer));
        let body = ok(case, &router, authorize, &params).await;
        let echoed = element(&body, "ipProtocol");
        assert_eq!(
            echoed.as_deref(),
            Some(*stored),
            "case {case:?}: expected the echoed <ipProtocol> {stored:?}, got {echoed:?}"
        );
        let (from_port, to_port) = ports.expect("every row of this table names a port range");
        let rules = described_rules(case, &router, &group_id, is_egress).await;
        assert_eq!(
            rules,
            vec![(
                (*stored).to_string(),
                Some(from_port.to_string()),
                Some(to_port.to_string())
            )],
            "case {case:?}: expected the port range to be kept as given, got {rules:?}"
        );

        let peer_text = match peer {
            Peer::Ipv4(cidr) | Peer::Ipv6(cidr) => cidr.clone(),
            Peer::GroupById(group_id) => group_id.clone(),
        };
        let mut other_casing = vec![param("GroupId", &group_id)];
        other_casing.extend(permission_params(1, again_as, ports, peer));
        let (status, duplicate) = call(&router, authorize, &other_casing).await;
        assert_refusal(
            case,
            status,
            &duplicate,
            ec2::state::INVALID_PERMISSION_DUPLICATE,
            &format!(
                "the specified rule \"peer: {peer_text}, {descriptor}, ALLOW\" already exists"
            ),
        );
    }
}

/// A token that names no IP protocol is refused `InvalidParameterValue`, `Invalid value
/// '<token>' for IP protocol. Unknown protocol.` — and a token that DOES name one is served.
///
/// The catalog and the range come from `IpPermission$IpProtocol`: "the IP protocol name
/// (`tcp`, `udp`, `icmp`, `icmpv6`) or number (see Protocol Numbers). Use `-1` to specify all
/// protocols" — a protocol number being a single byte, so `0` and `255` are the published
/// bounds and `-2` and `256` are one past each of them.
#[tokio::test]
async fn a_token_that_names_no_protocol_is_refused_as_an_unknown_protocol() {
    let (router, _engine) = emulator();
    let vpc_id = create_vpc("the network", &router, "10.2.0.0/16").await;

    let rows: Vec<ProtocolRow> = vec![
        ProtocolRow::refused("a protocol name the model does not list", "esp"),
        ProtocolRow::refused("another IANA name that is not one of the four", "ah"),
        ProtocolRow::refused("a name of a protocol that carries no ports", "gre"),
        ProtocolRow::refused("one past the largest protocol number", "256"),
        ProtocolRow::refused("one below the smallest protocol number", "-2"),
        ProtocolRow::refused("a protocol number that is not a whole number", "6.5"),
        ProtocolRow::refused("a name with a protocol name inside it", "tcpv6"),
        ProtocolRow::refused("a hexadecimal protocol number", "0x06"),
        ProtocolRow::served("the smallest protocol number", "0", None),
        ProtocolRow::served("the largest protocol number", "255", None),
        ProtocolRow::served("a protocol number between the bounds", "47", None),
        ProtocolRow::served("every protocol, as the number", "-1", None),
        ProtocolRow::served("every protocol, as the word", "all", None),
        ProtocolRow::served(
            "a protocol name, with the range it requires",
            "tcp",
            Some((443, 443)),
        ),
        ProtocolRow::served("the fourth name, whose range is optional", "icmpv6", None),
    ];

    for (index, row) in rows.iter().enumerate() {
        let &ProtocolRow {
            case,
            token,
            ports,
            names_a_protocol,
        } = row;
        let group_id = create_group(case, &router, &vpc_id, &format!("protocol-{index}")).await;
        let peer = Peer::Ipv4("172.16.0.0/12".to_string());
        let mut params = vec![param("GroupId", &group_id)];
        params.extend(permission_params(1, token, ports, &peer));
        let (status, body) = call(&router, AUTHORIZE_INGRESS, &params).await;

        if names_a_protocol {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the protocol {token:?} to be served, got {status} \
                 with body {body}"
            );
            let stored = described_rules(case, &router, &group_id, false).await;
            assert_eq!(
                stored.len(),
                1,
                "case {case:?}: expected exactly one stored rule, got {stored:?}"
            );
            continue;
        }

        assert_refusal(
            case,
            status,
            &body,
            ec2::state::INVALID_PARAMETER_VALUE,
            &format!("Invalid value '{token}' for IP protocol. Unknown protocol."),
        );
        // A refused request stores nothing.
        let stored = described_rules(case, &router, &group_id, false).await;
        assert!(
            stored.is_empty(),
            "case {case:?}: expected the refusal to store no rule, got {stored:?}"
        );
    }
}

/// An ICMPv6 rule whose peer is an address RANGE carries no type and code: the port range is
/// either absent or the pair that stands for every type and code, and any other pair is
/// refused — while a peer of another kind keeps the type and code it was given.
#[tokio::test]
async fn icmpv6_from_an_address_range_carries_no_type_or_code() {
    let (router, _engine) = emulator();
    let vpc_id = create_vpc("the network", &router, "10.3.0.0/16").await;
    let peer_group_id = create_group("the referenced group", &router, &vpc_id, "icmpv6-peer").await;

    // The sentence EC2 answers, which names the protocol by its own number.
    let refusal = "When protocol is 58 and remote-ip-range is specified, port range must \
                   either not be specified or must be specified as from=-1 and to=-1.";

    let rows: Vec<Icmpv6Row> = vec![
        Icmpv6Row {
            case: "a type and code of zero from an IPv4 range, inbound",
            token: "icmpv6",
            ports: Some((0, 0)),
            peer: Peer::Ipv4("10.9.0.0/16".to_string()),
            is_egress: false,
            served: false,
        },
        Icmpv6Row {
            case: "an echo-request type from an IPv6 range, inbound",
            token: "icmpv6",
            ports: Some((128, 0)),
            peer: Peer::Ipv6("2001:db8:1::/48".to_string()),
            is_egress: false,
            served: false,
        },
        Icmpv6Row {
            case: "a type and code of one from an IPv6 range, outbound",
            token: "ICMPV6",
            ports: Some((1, 1)),
            peer: Peer::Ipv6("2001:db8:2::/48".to_string()),
            is_egress: true,
            served: false,
        },
        Icmpv6Row {
            case: "the protocol named by its number, from an IPv4 range, outbound",
            token: "58",
            ports: Some((0, 0)),
            peer: Peer::Ipv4("10.10.0.0/16".to_string()),
            is_egress: true,
            served: false,
        },
        Icmpv6Row {
            case: "every type and code from an IPv6 range, inbound",
            token: "icmpv6",
            ports: Some((-1, -1)),
            peer: Peer::Ipv6("2001:db8:3::/48".to_string()),
            is_egress: false,
            served: true,
        },
        Icmpv6Row {
            case: "no port range at all from an IPv4 range, inbound",
            token: "icmpv6",
            ports: None,
            peer: Peer::Ipv4("10.11.0.0/16".to_string()),
            is_egress: false,
            served: true,
        },
        Icmpv6Row {
            case: "a type and code with another group as the peer, inbound",
            token: "icmpv6",
            ports: Some((128, 0)),
            peer: Peer::GroupById(peer_group_id.clone()),
            is_egress: false,
            served: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let Icmpv6Row {
            case,
            token,
            ports,
            peer,
            is_egress,
            served,
        } = row;
        let (is_egress, served, ports) = (*is_egress, *served, *ports);
        let group_id = create_group(case, &router, &vpc_id, &format!("icmpv6-{index}")).await;
        let authorize = if is_egress {
            AUTHORIZE_EGRESS
        } else {
            AUTHORIZE_INGRESS
        };
        let mut params = vec![param("GroupId", &group_id)];
        params.extend(permission_params(1, token, ports, peer));
        let (status, body) = call(&router, authorize, &params).await;

        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the rule to be served, got {status} with body {body}"
            );
            let stored = described_rules(case, &router, &group_id, is_egress).await;
            assert_eq!(
                stored.len(),
                1,
                "case {case:?}: expected exactly one stored rule, got {stored:?}"
            );
            continue;
        }

        assert_refusal(
            case,
            status,
            &body,
            ec2::state::INVALID_PARAMETER_VALUE,
            refusal,
        );
        let stored = described_rules(case, &router, &group_id, is_egress).await;
        assert!(
            stored.is_empty(),
            "case {case:?}: expected the refusal to store no rule, got {stored:?}"
        );
    }
}

/// A `SecurityGroupRuleId` names one rule of the group that holds it, whichever side that
/// rule is on: an id of the group's OTHER side is refused as that side's rule, an id the group
/// holds no rule under is the not-found refusal, and the rules survive both — while the id
/// named in its own operation revokes its rule.
#[tokio::test]
async fn a_rule_id_of_the_other_side_is_refused_as_that_sides_rule() {
    let (router, _engine) = emulator();
    let vpc_id = create_vpc("the network", &router, "10.4.0.0/16").await;
    let group_id = create_group("the group", &router, &vpc_id, "rule-id-scope").await;
    let other_group_id = create_group("the other group", &router, &vpc_id, "rule-id-other").await;

    /// One rule, as the id the group minted for it.
    async fn authorize_one(
        case: &str,
        router: &MultiServiceRouter,
        group_id: &str,
        action: &str,
        cidr: &str,
    ) -> String {
        let mut params = vec![param("GroupId", group_id)];
        params.extend(permission_params(
            1,
            "tcp",
            Some((443, 443)),
            &Peer::Ipv4(cidr.to_string()),
        ));
        let body = ok(case, router, action, &params).await;
        let ids = elements(&body, "securityGroupRuleId");
        assert_eq!(
            ids.len(),
            1,
            "case {case:?}: expected one minted rule id, got {ids:?} in {body}"
        );
        ids[0].clone()
    }

    let inbound_id =
        authorize_one("the inbound rule", &router, &group_id, AUTHORIZE_INGRESS, "10.20.0.0/16")
            .await;
    let outbound_id =
        authorize_one("the outbound rule", &router, &group_id, AUTHORIZE_EGRESS, "10.21.0.0/16")
            .await;
    let other_inbound_id = authorize_one(
        "another group's inbound rule",
        &router,
        &other_group_id,
        AUTHORIZE_INGRESS,
        "10.22.0.0/16",
    )
    .await;
    assert_ne!(
        inbound_id, other_inbound_id,
        "case \"the seeded rules\": one rule of two groups must have two ids"
    );

    // (case, the revoke, the id it names, the code, the message)
    let rows: Vec<(&str, &str, String, &str, String)> = vec![
        (
            "an outbound rule id in a revoke of the inbound side",
            REVOKE_INGRESS,
            outbound_id.clone(),
            ec2::state::INVALID_PARAMETER_VALUE,
            format!(
                "The security group rule ID '{outbound_id}' is an egress rule and may not be \
                 specified in a revoke-security-group-ingress request."
            ),
        ),
        (
            "an inbound rule id in a revoke of the outbound side",
            REVOKE_EGRESS,
            inbound_id.clone(),
            ec2::state::INVALID_PARAMETER_VALUE,
            format!(
                "The security group rule ID '{inbound_id}' is an ingress rule and may not be \
                 specified in a revoke-security-group-egress request."
            ),
        ),
        (
            "another group's rule id, on the side the revoke names",
            REVOKE_INGRESS,
            other_inbound_id.clone(),
            ec2::state::INVALID_SECURITY_GROUP_RULE_ID_NOT_FOUND,
            format!("The security group rule ID '{other_inbound_id}' does not exist"),
        ),
        (
            "a well-formed id no rule was minted under",
            REVOKE_EGRESS,
            "sgr-0000000000000dead".to_string(),
            ec2::state::INVALID_SECURITY_GROUP_RULE_ID_NOT_FOUND,
            "The security group rule ID 'sgr-0000000000000dead' does not exist".to_string(),
        ),
    ];

    for (case, revoke, rule_id, code, message) in &rows {
        let (status, body) = call(
            &router,
            revoke,
            &[
                param("GroupId", &group_id),
                param("SecurityGroupRuleId.1", rule_id),
            ],
        )
        .await;
        assert_refusal(case, status, &body, code, message);

        // Neither side lost a rule to a refusal.
        for (side, is_egress) in [("inbound", false), ("outbound", true)] {
            let stored = described_rules(case, &router, &group_id, is_egress).await;
            assert_eq!(
                stored.len(),
                1,
                "case {case:?}: expected the {side} rule to survive the refusal, got {stored:?}"
            );
        }
    }

    // The same two ids, each in the operation of its own side, revoke their rule.
    for (case, revoke, rule_id, is_egress) in [
        ("the inbound id inbound", REVOKE_INGRESS, &inbound_id, false),
        ("the outbound id outbound", REVOKE_EGRESS, &outbound_id, true),
    ] {
        let body = ok(
            case,
            &router,
            revoke,
            &[
                param("GroupId", &group_id),
                param("SecurityGroupRuleId.1", rule_id),
            ],
        )
        .await;
        assert_eq!(
            element(&body, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected <return>true</return> from {revoke}, got {body}"
        );
        let stored = described_rules(case, &router, &group_id, is_egress).await;
        assert!(
            stored.is_empty(),
            "case {case:?}: expected the rule to be revoked, got {stored:?}"
        );
    }
}
