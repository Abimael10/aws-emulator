// SPDX-License-Identifier: CC0-1.0

//! What a refusal SAYS about the value it could not use, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=<operation>`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! Three rules, each asserted over a table whose rows differ along what the rule generalises
//! over rather than around any one recorded call:
//!
//! 1. A customer-owned address pool id that resolves to no pool is answered by the FORM of
//!    the value: a value of the pool-id form (the prefix plus lowercase hexadecimal digits,
//!    however many) names a pool that could exist, so it is reported not found, while a value
//!    outside that form is refused under the shape the member targets (`Ipv4PoolCoipId`),
//!    `InvalidIpv4PoolCoipId.Malformed`, with the crate's one malformed-id sentence for the
//!    resource kind. The digit COUNT is not part of the question.
//! 2. A prefix list id that names no prefix list the caller can see is refused with the
//!    not-found code that surface answers — `InvalidPrefixListID.NotFound` for the managed
//!    prefix list surface, `InvalidPrefixListId.NotFound` for the legacy read of a Region's
//!    AWS service prefix lists — and the operations that resolve a list to REPORT ON or REMOVE
//!    it all answer one sentence — `The prefix list (<id>) does not exist.` — while the
//!    revise (`ModifyManagedPrefixList`) answers its own.
//! 3. A network ACL rule whose protocol needs a port range, or an ICMP type and code, and
//!    carries none names the LEAF member the caller has to fill in (`portRange.from`) rather
//!    than the structure around it, and nothing else.
//!
//! The id forms and the resource token come from the crate's own readers and constants
//! (`ec2::state::coip_pool_id_reads_as_one`, `ec2::state::COIP_POOL_ID_PREFIX`), so a row
//! names no number of its own.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::coip_pools::seed as seed_coip_pools;
use super::ec2_query::{call, element, error_code, error_message};
use super::network_acl_entries::{create_entry, vpc_and_acl};
use super::wire::param;

/// The Region every call of this suite is signed for. The coip-pool fixtures are seeded for
/// this Region's store, which is the one the network ACL suite drives too.
const REGION: &str = "us-east-1";

/// How long a poll loop keeps looking for the condition it waits on before giving up:
/// generous, because only the condition is under test and never how fast the host reaches it.
const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a poll loop waits between looks.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// One form field of an ec2Query body.
type Field = (String, String);

/// One row of the pool-id table: the operation, the code it answers for a value OF the
/// pool-id form that names no pool, and the form fields it needs beside that id.
type PoolIdCaller = (&'static str, &'static str, Vec<Field>);

/// One row of the required-leaf table: the case, the protocol the rule names, the port or
/// ICMP fields the request carries, the address range it matches, and the leaf member the
/// refusal must name.
type RequiredLeafCase<'a> = (&'a str, &'a str, Vec<Field>, (&'a str, &'a str), &'a str);

/// A value that READS as a customer-owned address pool id: the prefix, then `digits`
/// hexadecimal digits. The forms a mint issues are 8 and 17 digits long, and
/// `ec2::state::coip_pool_id_reads_as_one` accepts any count — which is exactly what rule 1
/// is about, so the rows below use counts on both sides of the mintable ones.
fn readable_pool_id(digits: usize) -> String {
    format!(
        "{prefix}{suffix}",
        prefix = ec2::state::COIP_POOL_ID_PREFIX,
        suffix = "0".repeat(digits),
    )
}

/// The operations that resolve the ONE customer-owned address pool their request names, with
/// the form fields each needs beside that id, and the code each answers for a value of the
/// pool-id form that names no pool of the account.
///
/// `DeleteCoipPool` answers its own recorded code for such a value — the delete cannot act on
/// it — where the operations that read or revise a pool report it absent; the MALFORMED half
/// of rule 1 is the same for all of them, which is what the table asserts.
fn pool_id_callers() -> Vec<PoolIdCaller> {
    vec![
        (
            "CreateCoipCidr",
            ec2::state::INVALID_POOL_ID_NOT_FOUND,
            vec![param("Cidr", "10.99.0.0/24")],
        ),
        (
            "DeleteCoipCidr",
            ec2::state::INVALID_POOL_ID_NOT_FOUND,
            vec![param("Cidr", "10.99.0.0/24")],
        ),
        (
            "GetCoipPoolUsage",
            ec2::state::INVALID_POOL_ID_NOT_FOUND,
            Vec::new(),
        ),
        (
            "DeleteCoipPool",
            ec2::state::INVALID_PARAMETER_VALUE,
            Vec::new(),
        ),
    ]
}

/// The member each of those operations names the pool with: `GetCoipPoolUsage` spells it
/// `PoolId`, the rest `CoipPoolId` (`smithy_query(query="operation",operation="DeleteCoipCidr,
/// GetCoipPoolUsage")`).
fn pool_id_member(action: &str) -> &'static str {
    if action == "GetCoipPoolUsage" {
        "PoolId"
    } else {
        "CoipPoolId"
    }
}

/// Rule 1: a value OF the pool-id form that names no pool is reported absent, whatever digit
/// count it carries — the two counts a mint issues and the counts on either side of them — and
/// the sentence is the surface's one not-found sentence. Asserted from every operation that
/// resolves one pool, so the reading is the member's and not one handler's.
#[tokio::test]
async fn a_readable_pool_id_naming_no_pool_is_reported_absent() {
    // Rows: the two counts EC2 mints (8 and 17), one digit, one between the two, one past the
    // longer. None of them is a pool the fixtures create.
    let values = [
        ("the short mintable form", readable_pool_id(8)),
        ("the long mintable form", readable_pool_id(17)),
        ("one digit", readable_pool_id(1)),
        ("between the two forms", readable_pool_id(12)),
        ("past the longer form", readable_pool_id(24)),
    ];
    for (action, expected_code, extra) in pool_id_callers() {
        for (case, pool_id) in &values {
            let (router, engine) = super::ec2_query::emulator();
            let seeded = seed_coip_pools(&engine);
            assert!(
                !seeded.contains(pool_id),
                "case {action:?} {case:?}: the fixtures must hold no pool under {pool_id:?}"
            );

            let mut params = vec![param(pool_id_member(action), pool_id)];
            params.extend(extra.iter().cloned());
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {action:?} {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                expected_code,
                "case {action:?} {case:?}: expected {expected_code:?}, got {body}"
            );
            let message = error_message(&body);
            let expected_message = format!("The pool ID '{pool_id}' does not exist.");
            assert_eq!(
                message, expected_message,
                "case {action:?} {case:?}: expected {expected_message:?}, got {message:?}"
            );
        }
    }
}

/// Rule 1, the other half: a value that is NO pool id at all has nothing to look up, so every
/// operation that resolves one pool refuses it under the shape its member targets, with the
/// crate's one malformed-id sentence for a customer-owned pool. The rows are each way a value
/// can fall outside the form — another prefix, another resource's id, an upper-case suffix, a
/// non-hexadecimal suffix, the bare prefix, free text and the empty value.
#[tokio::test]
async fn a_value_that_is_no_pool_id_is_refused_under_the_shape_the_member_targets() {
    let prefix = ec2::state::COIP_POOL_ID_PREFIX;
    let values = [
        ("another prefix", "pool-0a1b2c3d".to_string()),
        (
            "the public pool prefix",
            "ipv4pool-ec2-0a1b2c3d".to_string(),
        ),
        ("an upper-case suffix", format!("{prefix}0ABCDEF01")),
        ("a non-hexadecimal suffix", format!("{prefix}0a1b2c3g")),
        ("the bare prefix", prefix.to_string()),
        ("another resource's id", "subnet-0a1b2c3d".to_string()),
        ("free text", "no pool at all".to_string()),
        ("the empty value", String::new()),
    ];
    for (action, _expected_code, extra) in pool_id_callers() {
        for (case, pool_id) in &values {
            let (router, engine) = super::ec2_query::emulator();
            let _seeded = seed_coip_pools(&engine);

            let mut params = vec![param(pool_id_member(action), pool_id)];
            params.extend(extra.iter().cloned());
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {action:?} {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_IPV4_POOL_COIP_ID_MALFORMED,
                "case {action:?} {case:?}: expected the malformed class of the shape the \
                 member targets, got {body}"
            );
            let message = error_message(&body);
            let expected_message = format!("The coip-pool ID '{pool_id}' is malformed");
            assert_eq!(
                message, expected_message,
                "case {action:?} {case:?}: expected {expected_message:?}, got {message:?}"
            );
        }
    }
}

/// One prefix list of the account, created over the wire with one entry; answers the id it was
/// given, so a later read is asserted against a list this store really minted.
async fn create_prefix_list(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let params = vec![
        param("PrefixListName", name),
        param("AddressFamily", "IPv4"),
        param("MaxEntries", "5"),
        param("Entry.1.Cidr", "10.0.0.0/24"),
    ];
    let (status, body) = call(router, REGION, "CreateManagedPrefixList", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must succeed, got {status} with body {body}"
    );
    element(&body, "prefixListId")
        .unwrap_or_else(|| panic!("case {case:?}: the create reported no prefixListId: {body}"))
}

/// The operations that resolve ONE prefix list to report on or remove it, with the field each
/// names it through and the CODE each answers: the reads of a list's entries and of what
/// references it, the two describes that take a prefix-list-id selector, and the delete.
///
/// EC2 is not uniform about the spelling of this code: the managed-prefix-list surface answers
/// `InvalidPrefixListID.NotFound` and the legacy read of a Region's AWS service prefix lists
/// answers `InvalidPrefixListId.NotFound`, so the code is a column of the table rather than one
/// value for every row.
const PREFIX_LIST_READERS: [(&str, &str, &str); 5] = [
    (
        "GetManagedPrefixListEntries",
        "PrefixListId",
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
    ),
    (
        "GetManagedPrefixListAssociations",
        "PrefixListId",
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
    ),
    (
        "DescribeManagedPrefixLists",
        "PrefixListId.1",
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
    ),
    (
        "DescribePrefixLists",
        "PrefixListId.1",
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND_SERVICE_READ,
    ),
    (
        "DeleteManagedPrefixList",
        "PrefixListId",
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
    ),
];

/// Rule 2: an id no prefix list the caller can see carries is refused with ONE sentence from
/// every operation that resolves a list to report on or remove it, built from the id the
/// request named, under that surface's own not-found code. The rows are the operations, over an
/// id in each of EC2's two published id forms and over the id of a list this store minted and
/// then deleted — so the rule holds for an id that never existed and for one that stopped
/// existing.
#[tokio::test]
async fn a_prefix_list_read_names_the_list_it_could_not_resolve() {
    let never_existed = ["pl-0f1e2d3c", "pl-0123456789abcdef0"];
    let (router, _engine) = super::ec2_query::emulator();

    // An id of a list this store really minted, deleted again so a read has to refuse it: the
    // same sentence as an id that was never minted at all. Both the create reaching a state a
    // delete is allowed in and the removal falling due are the CLOCK's, so each is waited for
    // by looking again until it holds rather than by sleeping a span.
    let deleted = create_prefix_list(&router, "the minted list", "to-be-deleted").await;
    let started = std::time::Instant::now();
    loop {
        let (status, body) = call(
            &router,
            REGION,
            "DeleteManagedPrefixList",
            &[param("PrefixListId", &deleted)],
        )
        .await;
        if status == 200 {
            break;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "the minted list: gave up waiting for it to become deletable, last answer {status} \
             with body {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let started = std::time::Instant::now();
    loop {
        let (status, body) = call(
            &router,
            REGION,
            "DescribeManagedPrefixLists",
            &[param("PrefixListId.1", &deleted)],
        )
        .await;
        if status == 400 {
            break;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "the deleted list: gave up waiting for it to stop being reported, last answer \
             {status} with body {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (action, member, expected_code) in PREFIX_LIST_READERS {
        for unknown in never_existed
            .iter()
            .map(|id| (*id).to_string())
            .chain([deleted.clone()])
        {
            let (status, body) = call(&router, REGION, action, &[param(member, &unknown)]).await;
            assert_eq!(
                status, 400,
                "case {action:?} with {unknown:?}: expected HTTP 400, got {status} with body \
                 {body}"
            );
            assert_eq!(
                error_code(&body),
                expected_code,
                "case {action:?} with {unknown:?}: expected {expected_code}, got {body}"
            );
            let message = error_message(&body);
            let expected_message = format!("The prefix list ({unknown}) does not exist.");
            assert_eq!(
                message, expected_message,
                "case {action:?} with {unknown:?}: expected {expected_message:?}, got \
                 {message:?}"
            );
        }
    }
}

/// Rule 2's echo: `PrefixListResourceId` is a bare string in the model, so an unresolvable id
/// is named only up to the ceiling this crate quotes a caller's id to
/// (`ec2::state::MAX_ECHOED_RESOURCE_ID_CHARS`). Rows: an id AT the bound, named in full, and
/// one past it, named up to the bound — asserted through the describe, whose id list resolves
/// a value of any length.
#[tokio::test]
async fn an_over_long_prefix_list_id_is_named_only_up_to_the_echo_bound() {
    let bound = ec2::state::MAX_ECHOED_RESOURCE_ID_CHARS;
    for (case, length) in [("at the bound", bound), ("past the bound", bound + 16)] {
        let (router, _engine) = super::ec2_query::emulator();
        let unknown = format!("pl-{}", "0".repeat(length - "pl-".len()));
        assert_eq!(
            unknown.chars().count(),
            length,
            "case {case:?}: the row must build an id of {length} characters"
        );

        let (status, body) = call(
            &router,
            REGION,
            "DescribeManagedPrefixLists",
            &[param("PrefixListId.1", &unknown)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        let echoed: String = unknown.chars().take(bound).collect();
        let message = error_message(&body);
        let expected_message = format!("The prefix list ({echoed}) does not exist.");
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected {expected_message:?}, got {message:?}"
        );
    }
}

/// Rule 3: a rule whose protocol needs a port range, or an ICMP type and code, and carries
/// none is refused naming the LEAF member the caller has to supply — and a range that carries
/// only one of its two ports names the leaf that is missing from it. The rows are the two
/// port-based protocols the operation documents (TCP and UDP), each with no range at all and
/// with each half of a range on its own, plus the two protocols that need a type and code.
#[tokio::test]
async fn a_rule_missing_a_required_leaf_names_that_leaf() {
    let tcp = ec2::state::NETWORK_ACL_PROTOCOL_TCP.to_string();
    let udp = ec2::state::NETWORK_ACL_PROTOCOL_UDP.to_string();
    let icmp = ec2::state::NETWORK_ACL_PROTOCOL_ICMP.to_string();
    let icmpv6 = ec2::state::NETWORK_ACL_PROTOCOL_ICMPV6.to_string();
    // (case, protocol, the range fields the request carries, the address range it matches,
    //  the leaf the refusal must name)
    let cases: Vec<RequiredLeafCase<'_>> = vec![
        (
            "TCP with no port range",
            &tcp,
            Vec::new(),
            ("CidrBlock", "0.0.0.0/0"),
            "portRange.from",
        ),
        (
            "UDP with no port range",
            &udp,
            Vec::new(),
            ("CidrBlock", "0.0.0.0/0"),
            "portRange.from",
        ),
        (
            "TCP with only the last port",
            &tcp,
            vec![param("PortRange.To", "443")],
            ("CidrBlock", "0.0.0.0/0"),
            "portRange.from",
        ),
        (
            "TCP with only the first port",
            &tcp,
            vec![param("PortRange.From", "443")],
            ("CidrBlock", "0.0.0.0/0"),
            "portRange.to",
        ),
        (
            "UDP with only the first port",
            &udp,
            vec![param("PortRange.From", "53")],
            ("CidrBlock", "0.0.0.0/0"),
            "portRange.to",
        ),
        (
            "ICMP with no type and code",
            &icmp,
            Vec::new(),
            ("CidrBlock", "0.0.0.0/0"),
            "icmpTypeCode.type",
        ),
        (
            "ICMPv6 over an IPv6 range with no type and code",
            &icmpv6,
            Vec::new(),
            ("Ipv6CidrBlock", "::/0"),
            "icmpTypeCode.type",
        ),
    ];

    // Both operations that write ONE rule onto an ACL hold a request to the same members, so
    // each row is asserted from both.
    for action in ["CreateNetworkAclEntry", "ReplaceNetworkAclEntry"] {
        for (case, protocol, range_fields, (range_member, range_value), leaf) in &cases {
            let (router, _engine) = super::ec2_query::emulator();
            let (_vpc_id, acl_id) = vpc_and_acl(&router, "10.7.0.0/16").await;
            // The rule number is one the ACL holds no entry at, so a replace and a create
            // reach the same member reading rather than one of them failing on the entry.
            let mut params = vec![
                param("NetworkAclId", &acl_id),
                param("RuleNumber", "120"),
                param("Egress", "false"),
                param("RuleAction", ec2::state::RULE_ACTION_ALLOW),
                param("Protocol", protocol),
                param(range_member, range_value),
            ];
            params.extend(range_fields.iter().cloned());

            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {action:?} {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::MISSING_PARAMETER,
                "case {action:?} {case:?}: expected a missing-parameter refusal, got {body}"
            );
            let message = error_message(&body);
            let expected_message = format!("The request must contain the parameter {leaf}");
            assert_eq!(
                message, expected_message,
                "case {action:?} {case:?}: expected {expected_message:?}, got {message:?}"
            );
        }
    }
}

/// A rule whose protocol needs a port range and carries BOTH of its ports is created, so the
/// refusals above are about the missing leaf and not about the protocol: the entry is read
/// back through the ACL's own describe. One row per port-based protocol.
#[tokio::test]
async fn a_port_based_rule_with_both_ports_is_created() {
    for protocol in [
        ec2::state::NETWORK_ACL_PROTOCOL_TCP,
        ec2::state::NETWORK_ACL_PROTOCOL_UDP,
    ] {
        let (router, _engine) = super::ec2_query::emulator();
        let (_vpc_id, acl_id) = vpc_and_acl(&router, "10.8.0.0/16").await;
        let case = format!("protocol {protocol}");

        let (status, body) = create_entry(
            &router,
            &[
                param("NetworkAclId", &acl_id),
                param("RuleNumber", "130"),
                param("Egress", "false"),
                param("RuleAction", ec2::state::RULE_ACTION_ALLOW),
                param("Protocol", &protocol.to_string()),
                param("CidrBlock", "0.0.0.0/0"),
                param("PortRange.From", "80"),
                param("PortRange.To", "443"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to succeed, got {status} with body {body}"
        );

        let (status, described) = call(
            &router,
            REGION,
            "DescribeNetworkAcls",
            &[param("NetworkAclId.1", &acl_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the describe to succeed, got {status} with body {described}"
        );
        assert!(
            described.contains("<from>80</from>") && described.contains("<to>443</to>"),
            "case {case:?}: expected the stored rule to report both ports, got {described}"
        );
    }
}
