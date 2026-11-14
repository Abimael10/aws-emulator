// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what a REPEATED `CreateNetworkAclEntry` does: the write path's
//! idempotency, read back through `DescribeNetworkAcls`. Driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//! ec2Query is selected by the `ec2` signing name in the request's SigV4 credential scope, so
//! every call here carries one.
//!
//! The rule under test: a create that asks for a resource the service ALREADY holds exactly
//! as asked for has nothing left to do, so it succeeds and writes nothing — a create of the
//! entry an ACL already holds at that rule number and direction, member for member, answers
//! the operation's empty 200 and leaves that one entry, where only a create naming a
//! DIFFERENT rule at a number the ACL has an entry at is the conflict
//! `NetworkAclEntryAlreadyExists` refuses ("After you add an entry, you can't modify it; you
//! must either replace it, or create an entry and delete the old one",
//! `CreateNetworkAclEntry`, whose modelled output is `Unit` and which declares no error
//! shapes).
//!
//! What follows from it, asserted over the table below:
//!
//! 1. The identical re-create answers HTTP 200, and the ACL still reports exactly ONE entry
//!    at that rule number in that direction, with the contents the first create stored.
//! 2. Repeating it again changes nothing, and the store is charged for the entry ONCE: a
//!    no-op write may not grow the emulator's byte budget.
//! 3. A re-create that differs in ANY member of the rule — the range, the action, the ports,
//!    the ICMP numbers, the protocol — is refused `NetworkAclEntryAlreadyExists` with AWS's
//!    sentence for it, naming the rule number from the request, and the stored entry is left
//!    as it was.
//! 4. A `DryRun` of the identical re-create reports the outcome it would have had
//!    (`DryRunOperation`) and writes nothing.
//!
//! Every table row is a distinct input along what the rule generalises over — the direction,
//! the rule number (both published bounds among them), the protocol, the address family, the
//! action and the port or ICMP numbers — so no single recorded request is the specification.

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use ec2::state::{
    ALL_IP_PROTOCOLS, MAX_ICMP_TYPE, NETWORK_ACL_ENTRY_RULE_NUMBERS, NETWORK_ACL_PROTOCOL_ICMP,
    NETWORK_ACL_PROTOCOL_ICMPV6, NETWORK_ACL_PROTOCOL_TCP, NETWORK_ACL_PROTOCOL_UDP,
    RULE_ACTION_ALLOW, RULE_ACTION_DENY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{error_code, error_message};
use super::network_acl_entries::{
    EntryCase, ReportedEntry, acl_body, create_entry, entry_params, expected_entry,
    reported_entries, vpc_and_acl,
};
use super::wire::param;

/// The code AWS refuses a DIFFERENT rule at an occupied rule number with.
const CONFLICT_CODE: &str = "NetworkAclEntryAlreadyExists";

/// How many bytes of caller-supplied data the store holds — what a no-op write may not grow.
fn stored_bytes(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().stored_bytes()
}

/// The entries the ACL reports at `rule_number` in the direction `egress` names. Exactly one
/// of them is the whole point of the rule, so the count is asserted by the caller.
fn entries_at(case: &str, body: &str, rule_number: i32, egress: bool) -> Vec<ReportedEntry> {
    reported_entries(case, body)
        .into_iter()
        .filter(|entry| {
            entry.rule_number == rule_number.to_string() && entry.egress == egress.to_string()
        })
        .collect()
}

/// The rows the rule is asserted over: one per kind of rule a caller can write.
fn rows() -> Vec<EntryCase> {
    vec![
        // TCP over one port, ingress — the shape the recorded converging client re-applies.
        EntryCase {
            case: "TCP ingress over one port",
            rule_number: 100,
            egress: false,
            rule_action: RULE_ACTION_ALLOW,
            protocol: int_text(NETWORK_ACL_PROTOCOL_TCP),
            requested_cidr: "10.0.0.0/8",
            ipv6: false,
            reported_cidr: "10.0.0.0/8",
            port_range: Some((80, 80)),
            icmp_type_code: None,
        },
        // UDP egress over a span of ports, at the HIGHEST rule number a caller may write
        // ("Constraints: Positive integer from 1 to 32766", `$RuleNumber`).
        EntryCase {
            case: "UDP egress over a span of ports at the highest rule number",
            rule_number: *NETWORK_ACL_ENTRY_RULE_NUMBERS.end(),
            egress: true,
            rule_action: RULE_ACTION_DENY,
            protocol: int_text(NETWORK_ACL_PROTOCOL_UDP),
            requested_cidr: "172.16.0.0/12",
            ipv6: false,
            reported_cidr: "172.16.0.0/12",
            port_range: Some((1024, 65535)),
            icmp_type_code: None,
        },
        // ICMP with a type and code, at the LOWEST rule number a caller may write.
        EntryCase {
            case: "ICMP with a type and code at the lowest rule number",
            rule_number: *NETWORK_ACL_ENTRY_RULE_NUMBERS.start(),
            egress: false,
            rule_action: RULE_ACTION_ALLOW,
            protocol: int_text(NETWORK_ACL_PROTOCOL_ICMP),
            requested_cidr: "192.0.2.0/24",
            ipv6: false,
            reported_cidr: "192.0.2.0/24",
            port_range: None,
            icmp_type_code: Some((8, 0)),
        },
        // Every protocol, which carries neither ports nor ICMP numbers.
        EntryCase {
            case: "every protocol, egress, no ports and no ICMP numbers",
            rule_number: 200,
            egress: true,
            rule_action: RULE_ACTION_DENY,
            protocol: ALL_IP_PROTOCOLS,
            requested_cidr: "0.0.0.0/0",
            ipv6: false,
            reported_cidr: "0.0.0.0/0",
            port_range: None,
            icmp_type_code: None,
        },
        // The other address family: an IPv6 range rather than an IPv4 one.
        EntryCase {
            case: "TCP ingress over an IPv6 range",
            rule_number: 150,
            egress: false,
            rule_action: RULE_ACTION_ALLOW,
            protocol: int_text(NETWORK_ACL_PROTOCOL_TCP),
            requested_cidr: "2001:db8:1234:1a00::/64",
            ipv6: true,
            reported_cidr: "2001:db8:1234:1a00::/64",
            port_range: Some((443, 443)),
            icmp_type_code: None,
        },
        // ICMPv6 over an IPv6 range, where "you must specify an ICMP type and code"
        // (`$Protocol`), with the all-types, all-codes pair.
        EntryCase {
            case: "ICMPv6 egress over an IPv6 range, all types and codes",
            rule_number: 300,
            egress: true,
            rule_action: RULE_ACTION_ALLOW,
            protocol: int_text(NETWORK_ACL_PROTOCOL_ICMPV6),
            requested_cidr: "2001:db8::/32",
            ipv6: true,
            reported_cidr: "2001:db8::/32",
            port_range: None,
            icmp_type_code: Some((-1, -1)),
        },
    ]
}

/// How a row's re-create is made to ask for a DIFFERENT rule at the same number and
/// direction: one member at a time, so each member of the comparison is exercised.
///
/// `None` for a change a row cannot carry — a port range on a protocol that has none, ICMP
/// numbers on one that takes none — which is what keeps the table honest rather than
/// asserting a change the service would have refused outright.
fn conflicting_variants(row: &EntryCase) -> Vec<(&'static str, EntryCase)> {
    let mut variants: Vec<(&'static str, EntryCase)> = Vec::new();

    // A different range of the same family.
    let mut other_range = row.clone();
    if row.ipv6 {
        other_range.requested_cidr = "2001:db8:aaaa::/48";
        other_range.reported_cidr = "2001:db8:aaaa::/48";
    } else {
        other_range.requested_cidr = "198.51.100.0/24";
        other_range.reported_cidr = "198.51.100.0/24";
    }
    variants.push(("a different range", other_range));

    // The other action.
    let mut other_action = row.clone();
    other_action.rule_action = if row.rule_action == RULE_ACTION_ALLOW {
        RULE_ACTION_DENY
    } else {
        RULE_ACTION_ALLOW
    };
    variants.push(("the other action", other_action));

    // A different port range, for the protocols that carry one — and dropping it altogether
    // is a different rule too, which is what the protocols that carry none test instead.
    if row.port_range.is_some() {
        let mut other_ports = row.clone();
        other_ports.port_range = Some((8080, 8080));
        variants.push(("a different port range", other_ports));
    }

    // Different ICMP numbers, for the protocols that carry them.
    if let Some((icmp_type, _)) = row.icmp_type_code {
        let mut other_icmp = row.clone();
        other_icmp.icmp_type_code = Some((
            if icmp_type == MAX_ICMP_TYPE {
                0
            } else {
                MAX_ICMP_TYPE
            },
            0,
        ));
        variants.push(("different ICMP numbers", other_icmp));
    }

    variants
}

/// A create of the entry the ACL already holds succeeds and writes nothing, while a create of
/// a different rule at the same number and direction is refused.
#[tokio::test]
async fn an_identical_re_create_succeeds_and_writes_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let (_vpc_id, acl_id) = vpc_and_acl(&router, "10.101.0.0/16").await;

    for row in rows() {
        let case = row.case;
        let params = entry_params(&acl_id, &row);

        // The first create stores the entry.
        let (status, body) = create_entry(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 creating the entry, got {status} with body \
             {body}"
        );
        let charged = stored_bytes(&engine);

        // The identical re-create, twice: each answers success and leaves the ONE entry the
        // first create stored, with nothing added to the store's byte budget.
        for attempt in 1..=2 {
            let (status, body) = create_entry(&router, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the identical re-create #{attempt} to succeed, got \
                 {status} with body {body}"
            );
            let described = acl_body(&router, case, &acl_id).await;
            let at_number = entries_at(case, &described, row.rule_number, row.egress);
            assert_eq!(
                at_number,
                vec![expected_entry(&row)],
                "case {case:?}: after the identical re-create #{attempt} the ACL must report \
                 exactly the one entry the first create stored, got {at_number:?}"
            );
            assert_eq!(
                stored_bytes(&engine),
                charged,
                "case {case:?}: the identical re-create #{attempt} must charge the store \
                 nothing, got {} against {charged}",
                stored_bytes(&engine)
            );
        }

        // A DryRun of that same re-create reports the outcome and writes nothing either.
        let mut dry_run = params.clone();
        dry_run.push(param("DryRun", "true"));
        let (status, body) = create_entry(&router, &dry_run).await;
        assert_eq!(
            status, 412,
            "case {case:?}: expected the dry run to be answered 412, got {status} with body \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "case {case:?}: expected DryRunOperation, got {body}"
        );

        // A re-create that asks for a DIFFERENT rule at the same number and direction is the
        // conflict, member by member.
        for (change, variant) in conflicting_variants(&row) {
            let (status, body) = create_entry(&router, &entry_params(&acl_id, &variant)).await;
            assert_eq!(
                status, 400,
                "case {case:?} with {change}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                CONFLICT_CODE,
                "case {case:?} with {change}: expected {CONFLICT_CODE}, got {body}"
            );
            assert_eq!(
                error_message(&body),
                format!(
                    "The network acl entry identified by {} already exists.",
                    row.rule_number
                ),
                "case {case:?} with {change}: expected the rule number from the request in \
                 the sentence, got {body}"
            );
            let described = acl_body(&router, case, &acl_id).await;
            let at_number = entries_at(case, &described, row.rule_number, row.egress);
            assert_eq!(
                at_number,
                vec![expected_entry(&row)],
                "case {case:?} with {change}: expected the refused conflict to leave the \
                 stored entry alone, got {at_number:?}"
            );
            assert_eq!(
                stored_bytes(&engine),
                charged,
                "case {case:?} with {change}: a refused create must charge nothing, got {} \
                 against {charged}",
                stored_bytes(&engine)
            );
        }
    }
}

/// Dropping a member the stored entry carries, or adding one it does not, is a DIFFERENT rule
/// at that number: the comparison is over the whole entry rather than over the members the
/// request happens to send.
#[tokio::test]
async fn a_re_create_that_drops_or_adds_a_member_is_the_conflict() {
    let (router, _engine) = super::ec2_query::emulator();
    let (_vpc_id, acl_id) = vpc_and_acl(&router, "10.102.0.0/16").await;

    // Each pair stores one rule and then asks for another at the SAME number and direction,
    // differing in exactly one member — and the pairs differ in WHICH member that is: a port
    // range dropped, ICMP numbers added, the protocol, and the address family of the range the
    // rule matches. Every pair carries a rule number of its own, so one pair cannot answer
    // for another.
    let all_protocols_with_ports = EntryCase {
        case: "every protocol with a port range",
        rule_number: 110,
        egress: false,
        rule_action: RULE_ACTION_ALLOW,
        protocol: ALL_IP_PROTOCOLS,
        requested_cidr: "10.0.0.0/8",
        ipv6: false,
        reported_cidr: "10.0.0.0/8",
        port_range: Some((80, 80)),
        icmp_type_code: None,
    };
    let all_protocols_without_icmp = EntryCase {
        case: "every protocol with no ICMP numbers",
        rule_number: 120,
        egress: true,
        rule_action: RULE_ACTION_DENY,
        protocol: ALL_IP_PROTOCOLS,
        requested_cidr: "0.0.0.0/0",
        ipv6: false,
        reported_cidr: "0.0.0.0/0",
        port_range: None,
        icmp_type_code: None,
    };
    let tcp = EntryCase {
        case: "TCP with a port range",
        rule_number: 130,
        egress: false,
        rule_action: RULE_ACTION_ALLOW,
        protocol: int_text(NETWORK_ACL_PROTOCOL_TCP),
        requested_cidr: "10.1.0.0/16",
        ipv6: false,
        reported_cidr: "10.1.0.0/16",
        port_range: Some((443, 443)),
        icmp_type_code: None,
    };
    let icmp_over_ipv4 = EntryCase {
        case: "ICMP over an IPv4 range",
        rule_number: 140,
        egress: true,
        rule_action: RULE_ACTION_DENY,
        protocol: int_text(NETWORK_ACL_PROTOCOL_ICMP),
        requested_cidr: "198.51.100.0/24",
        ipv6: false,
        reported_cidr: "198.51.100.0/24",
        port_range: None,
        icmp_type_code: Some((3, 1)),
    };

    let pairs: Vec<(EntryCase, EntryCase)> = vec![
        // The port range dropped.
        (
            all_protocols_with_ports.clone(),
            EntryCase {
                case: "the same rule with the port range dropped",
                port_range: None,
                ..all_protocols_with_ports
            },
        ),
        // ICMP numbers added.
        (
            all_protocols_without_icmp.clone(),
            EntryCase {
                case: "the same rule with ICMP numbers added",
                icmp_type_code: Some((3, 1)),
                ..all_protocols_without_icmp
            },
        ),
        // Another protocol, with everything else the same.
        (
            tcp.clone(),
            EntryCase {
                case: "the same rule over UDP rather than TCP",
                protocol: int_text(NETWORK_ACL_PROTOCOL_UDP),
                ..tcp
            },
        ),
        // The other address family, with everything else the same. ICMPv6 needs the type and
        // code "with an IPv6 CIDR block" (`$Protocol`), which this rule already carries.
        (
            icmp_over_ipv4.clone(),
            EntryCase {
                case: "the same rule over an IPv6 range",
                protocol: int_text(NETWORK_ACL_PROTOCOL_ICMPV6),
                requested_cidr: "2001:db8::/32",
                ipv6: true,
                reported_cidr: "2001:db8::/32",
                ..icmp_over_ipv4
            },
        ),
    ];

    for (stored, variant) in pairs {
        let case = stored.case;
        let (status, body) = create_entry(&router, &entry_params(&acl_id, &stored)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 creating the entry, got {status} with body \
             {body}"
        );

        let change = variant.case;
        let (status, body) = create_entry(&router, &entry_params(&acl_id, &variant)).await;
        assert_eq!(
            status, 400,
            "case {case:?} against {change:?}: expected HTTP 400, got {status} with body \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            CONFLICT_CODE,
            "case {case:?} against {change:?}: expected {CONFLICT_CODE}, got {body}"
        );

        // And the entry the first create stored is still the one the ACL reports.
        let described = acl_body(&router, case, &acl_id).await;
        let at_number = entries_at(case, &described, stored.rule_number, stored.egress);
        assert_eq!(
            at_number,
            vec![expected_entry(&stored)],
            "case {case:?} against {change:?}: expected the stored entry to be left alone, \
             got {at_number:?}"
        );
    }
}

/// One protocol number as the request spells it, mapped from this crate's own protocol
/// constants so each row names the protocol it means rather than a bare number. Every
/// protocol is [`ALL_IP_PROTOCOLS`], which the crate already spells as the text a request
/// sends.
fn int_text(protocol: i32) -> &'static str {
    match protocol {
        NETWORK_ACL_PROTOCOL_TCP => "6",
        NETWORK_ACL_PROTOCOL_UDP => "17",
        NETWORK_ACL_PROTOCOL_ICMP => "1",
        NETWORK_ACL_PROTOCOL_ICMPV6 => "58",
        other => panic!("no text for protocol {other}"),
    }
}
