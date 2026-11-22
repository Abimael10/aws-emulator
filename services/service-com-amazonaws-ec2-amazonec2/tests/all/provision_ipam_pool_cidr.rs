// SPDX-License-Identifier: CC0-1.0

//! What `ProvisionIpamPoolCidr` puts into an IPAM pool, and what it refuses — driven over
//! `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! One rule: a provision stores ONE range against the pool the request names and answers it as
//! the store then holds it, so every later reader of the store reports the same range. The
//! members the service owns are derived rather than taken from the request — the
//! `IpamPoolCidrId`, the `pending-provision` state of the `IpamPoolCidrState` catalog the range
//! starts in (with the clock owing it `provisioned`), and the netmask length, which is the
//! range's own — and the range is read in the family the POOL holds, with its host bits
//! cleared, so one range cannot be spelled two ways in one pool. A request that names a SIZE
//! instead of a range asks the service to choose the range, which it can only do out of space
//! the pool holds: the source pool's for a pool within one, Amazon's own IPv6 space for a
//! top-level IPv6 pool. Anything the pool could never hand out — a range of the other family,
//! a range meeting one the pool already holds, a range outside the source pool's space — is
//! refused before anything is stored.
//!
//! The rows of each table differ along what that rule generalises over: the address family,
//! the size of the range, how the request spells it, whether the request names a range or a
//! size, whether the pool is top-level or sits within another, and how the requested range
//! relates to the space the pool already holds.
//!
//! The suite keeps its own `call`/`ok`/`member` wrappers over the shared [`super::ec2_query`]
//! harness, which is how every other suite in this crate is arranged.

use std::net::Ipv6Addr;
use std::time::{Duration, Instant};

use ec2::state::{
    ADDRESS_FAMILY_IPV4, ADDRESS_FAMILY_IPV6, DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE,
    DRY_RUN_OPERATION_STATUS, INVALID_IPAM_POOL_ID_MALFORMED, INVALID_IPAM_POOL_ID_NOT_FOUND,
    INVALID_PARAMETER_COMBINATION, INVALID_PARAMETER_VALUE,
    IPAM_POOL_CIDR_FAILURE_CODE_NOT_AVAILABLE, IPAM_POOL_CIDR_ID_PREFIX,
    IPAM_POOL_CIDR_STATE_FAILED_PROVISION, IPAM_POOL_CIDR_STATE_PENDING_PROVISION,
    IPAM_POOL_CIDR_STATE_PROVISIONED, MAX_CLIENT_TOKEN_CHARS, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, emulator, error_code, error_message};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// The provision this suite drives.
const PROVISION: &str = "ProvisionIpamPoolCidr";

/// How many hexadecimal digits an `IpamPoolCidr$IpamPoolCidrId` carries after its prefix:
/// written out here rather than read off the service's own constant, so a change to the width
/// the service mints fails this suite instead of being re-asserted by it.
const POOL_CIDR_ID_DIGITS: usize = 32;

/// The read every provision of this suite is proved through.
const GET_CIDRS: &str = "GetIpamPoolCidrs";

/// The space Amazon advertises an Amazon-provided IPv6 CIDR block from, which is the only
/// space a TOP-LEVEL IPv6 pool asking for a size can be given a range out of.
const AMAZON_IPV6_SPACE: &str = "2600:1f00::/24";

/// The prefix length of an Amazon-provided IPv6 CIDR block: "an Amazon-provided IPv6 CIDR block
/// with a /56 prefix length" (`smithy_query(query="doc",shape="CreateVpcRequest",
/// member="AmazonProvidedIpv6CidrBlock")`), so no LARGER range can be taken out of one.
const AMAZON_IPV6_PREFIX_LENGTH: u32 = 56;

/// The deadline the poll below gives up at: a large multiple of the service's own transition
/// span, so an assertion is about the state the clock owes and never about how fast the host
/// runs.
fn settle_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// How long one look waits before looking again — a fraction of that span, so a poll inside the
/// deadline above costs many looks rather than one.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
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

/// Assert that `action` was refused HTTP 400 with `code` and exactly `message`.
async fn refused(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    code: &str,
    message: &str,
) {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} from {action}, got {status} with body \
         {body}"
    );
    let actual_code = error_code(&body);
    assert_eq!(
        actual_code, code,
        "case {case:?}: expected the code {code:?} from {action}, got {actual_code:?} in {body}"
    );
    let actual_message = error_message(&body);
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// Create one IPAM and answer its default private scope id, the scope every pool of this suite
/// is created in.
async fn create_scope(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "CreateIpam", &[]).await;
    member(case, &body, "privateDefaultScopeId")
}

/// Create one IPAM pool of `family` in `scope_id`, drawing on `source` when there is one, and
/// answer the `IpamPoolId` the service minted.
async fn create_pool(
    case: &str,
    router: &MultiServiceRouter,
    scope_id: &str,
    family: &str,
    source: Option<&str>,
) -> String {
    let mut params = vec![
        param("IpamScopeId", scope_id),
        param("AddressFamily", family),
    ];
    if let Some(source_pool_id) = source {
        params.push(param("SourceIpamPoolId", source_pool_id));
    }
    let body = ok(case, router, "CreateIpamPool", &params).await;
    member(case, &body, "ipamPoolId")
}

/// One CIDR as a reader of the store reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedCidr {
    /// The `ipamPoolCidrId` the service minted for it.
    id: String,
    /// The range, as the store spells it.
    cidr: String,
    /// The `IpamPoolCidrState` member it is in.
    state: String,
    /// The `netmaskLength` reported for it, as text.
    netmask_length: Option<String>,
    /// The `failureReason`'s `code`, for a CIDR whose provisioning settled as a failure.
    failure_code: Option<String>,
    /// The `failureReason`'s `message`, for the same.
    failure_message: Option<String>,
}

/// One CIDR read out of `body`: the element carrying an `ipamPoolCidrId`, so the members read
/// are that CIDR's own rather than any of the envelope's.
fn reported_one(case: &str, body: &str) -> ReportedCidr {
    let mut all = reported_all(case, body);
    assert_eq!(
        all.len(),
        1,
        "case {case:?}: expected exactly one reported CIDR, got {all:?} in {body}"
    );
    all.remove(0)
}

/// Every CIDR `body` carries, in the order it reported them.
fn reported_all(case: &str, body: &str) -> Vec<ReportedCidr> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "ipamPoolCidrId").is_some())
        .map(|node| ReportedCidr {
            id: child_text(node, "ipamPoolCidrId").unwrap_or_default(),
            cidr: child_text(node, "cidr").unwrap_or_default(),
            state: child_text(node, "state").unwrap_or_default(),
            netmask_length: child_text(node, "netmaskLength"),
            failure_code: child(node, "failureReason")
                .and_then(|reason| child_text(reason, "code")),
            failure_message: child(node, "failureReason")
                .and_then(|reason| child_text(reason, "message")),
        })
        .collect()
}

/// Every CIDR `GetIpamPoolCidrs` reports for the pool, sorted by range.
async fn held(case: &str, router: &MultiServiceRouter, pool_id: &str) -> Vec<ReportedCidr> {
    let body = ok(case, router, GET_CIDRS, &[param("IpamPoolId", pool_id)]).await;
    let mut reported = reported_all(case, &body);
    reported.sort_by(|left, right| left.cidr.cmp(&right.cidr));
    reported
}

/// Look at the pool's CIDRs AGAIN until the record `names` picks is reported in `wanted`, or a
/// deadline far past the service's own transition span passes. Answers the record as it was
/// last seen, or `None` while the pool reports no record `names` picks at all.
///
/// The ONE poller of this suite, so the deadline and the interval have a single implementation
/// and a caller only says how the record it waits on is NAMED: by its range, or — for a
/// provision that could not be given space, which never chose a range — by its id.
///
/// The CONDITION is what is waited for, never a span of wall clock: the move is one the clock
/// owes, so a loaded host costs more looks rather than a different answer.
async fn poll_until(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
    wanted: &str,
    names: impl Fn(&ReportedCidr) -> bool,
) -> Option<ReportedCidr> {
    let deadline = Instant::now() + settle_deadline();
    loop {
        let record = held(case, router, pool_id)
            .await
            .into_iter()
            .find(&names);
        let settled = record
            .as_ref()
            .is_some_and(|record| record.state == wanted);
        if settled || Instant::now() >= deadline {
            return record;
        }
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The state the range `cidr` of the pool is reported in once it is `wanted`, or the state it
/// was last seen in — the empty string while the pool reports no such range.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
    cidr: &str,
    wanted: &str,
) -> String {
    poll_until(case, router, pool_id, wanted, |reported| {
        reported.cidr == cidr
    })
    .await
    .map(|reported| reported.state)
    .unwrap_or_default()
}

/// The record `id` names once it is `wanted`, or as it was last seen.
///
/// The by-id counterpart of [`poll_until_state`], for a record that carries NO range of its
/// own: a provision that could not be given space never chose one, so its id is the only name
/// it has.
async fn poll_until_record_state(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
    id: &str,
    wanted: &str,
) -> ReportedCidr {
    poll_until(case, router, pool_id, wanted, |reported| reported.id == id)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the pool no longer reports {id}"))
}

/// Provision `cidr` to the pool, asserting the provision took, and answer the CIDR it reported.
async fn provision(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
    cidr: &str,
) -> ReportedCidr {
    let body = ok(
        case,
        router,
        PROVISION,
        &[param("IpamPoolId", pool_id), param("Cidr", cidr)],
    )
    .await;
    reported_one(case, &body)
}

/// A well-formed IPAM pool id the account holds no pool for: the prefix the service mints with
/// and 17 lowercase hexadecimal digits, which is the long id form every EC2 id takes.
const UNKNOWN_POOL_ID: &str = "ipam-pool-0123456789abcdef0";

// The containment and overlap arithmetic below is computed HERE rather than through the
// crate's own `cidr_block_contains_block` / `cidr_blocks_overlap`, deliberately: what these
// cases check is the range the SERVICE chose, and a check that asks the code under test
// whether its own answer is inside the space it drew on cannot fail when that code is wrong.
// It is the only place in this suite that reads a range numerically; every other expectation
// is a literal.

/// `value` as the numeric address and prefix length of an IPv6 CIDR block.
fn ipv6_block(case: &str, value: &str) -> (u128, u32) {
    let (address, prefix_length) = value
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {value:?} is not a CIDR block"));
    let address: Ipv6Addr = address
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {value:?} does not carry an IPv6 address"));
    let prefix_length: u32 = prefix_length
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {value:?} does not carry a prefix length"));
    (u128::from(address), prefix_length)
}

/// Whether every address of the IPv6 block `inner` is an address of the IPv6 block `outer`.
fn ipv6_contains(case: &str, outer: &str, inner: &str) -> bool {
    let (outer_base, outer_length) = ipv6_block(case, outer);
    let (inner_base, inner_length) = ipv6_block(case, inner);
    if inner_length < outer_length {
        return false;
    }
    let mask = u128::MAX
        .checked_shl(128 - outer_length)
        .unwrap_or_default();
    outer_base & mask == inner_base & mask
}

/// The first and last address of an IPv4 CIDR block, as numbers.
fn ipv4_bounds(case: &str, value: &str) -> (u64, u64) {
    let (address, prefix_length) = value
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {value:?} is not a CIDR block"));
    let address: std::net::Ipv4Addr = address
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {value:?} carries no IPv4 address"));
    let prefix_length: u32 = prefix_length
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {value:?} carries no prefix length"));
    assert!(
        prefix_length <= 32,
        "case {case:?}: {value:?} is not an IPv4 block — no IPv4 prefix is longer than 32"
    );
    let base = u64::from(u32::from(address));
    let size = 1u64 << (32 - prefix_length);
    (base, base + size - 1)
}

/// Whether every address of the IPv4 block `inner` is an address of the IPv4 block `outer`.
fn ipv4_contains(case: &str, outer: &str, inner: &str) -> bool {
    let (outer_first, outer_last) = ipv4_bounds(case, outer);
    let (inner_first, inner_last) = ipv4_bounds(case, inner);
    outer_first <= inner_first && inner_last <= outer_last
}

/// Whether the two IPv4 blocks share an address, read off their numeric bounds.
fn ipv4_blocks_overlap(case: &str, left: &str, right: &str) -> bool {
    let (left_first, left_last) = ipv4_bounds(case, left);
    let (right_first, right_last) = ipv4_bounds(case, right);
    left_first <= right_last && right_first <= left_last
}

/// A provision stores the range the request names and every reader of the store reports it:
/// the call answers it in the transient state of the `IpamPoolCidrState` catalog with a minted
/// id and the range's OWN netmask length, a read reports exactly that, and the clock owes it
/// `provisioned` one span later.
///
/// The rows differ along the address family, the size of the range and how the request SPELLS
/// it — a range is the addresses it covers, so a spelling with host bits set names the range it
/// lies in, and the store holds the one canonical spelling.
#[tokio::test]
async fn a_provisioned_range_is_stored_and_read_back_as_the_pool_holds_it() {
    // (case, the pool's family, the spelling the request names, the range the pool holds)
    let rows: [(&str, &str, &str, &str); 8] = [
        (
            "an IPv4 /8",
            ADDRESS_FAMILY_IPV4,
            "10.0.0.0/8",
            "10.0.0.0/8",
        ),
        (
            "an IPv4 /16 named exactly",
            ADDRESS_FAMILY_IPV4,
            "10.1.0.0/16",
            "10.1.0.0/16",
        ),
        (
            "an IPv4 /24 named with its host bits set",
            ADDRESS_FAMILY_IPV4,
            "10.2.3.37/24",
            "10.2.3.0/24",
        ),
        (
            "an IPv4 /20 named with its host bits set",
            ADDRESS_FAMILY_IPV4,
            "10.3.19.200/20",
            "10.3.16.0/20",
        ),
        (
            "an IPv4 /32",
            ADDRESS_FAMILY_IPV4,
            "192.0.2.7/32",
            "192.0.2.7/32",
        ),
        (
            "an IPv6 /32",
            ADDRESS_FAMILY_IPV6,
            "2001:db8::/32",
            "2001:db8::/32",
        ),
        (
            "an IPv6 /56 named with its host bits set",
            ADDRESS_FAMILY_IPV6,
            "2001:db8:1:ff01:2::9/56",
            "2001:db8:1:ff00::/56",
        ),
        (
            "an IPv6 /128",
            ADDRESS_FAMILY_IPV6,
            "2001:db8::1/128",
            "2001:db8::1/128",
        ),
    ];

    for (case, family, spelling, canonical) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let pool_id = create_pool(case, &router, &scope, family, None).await;

        let answered = provision(case, &router, &pool_id, spelling).await;
        assert_eq!(
            answered.cidr, canonical,
            "case {case:?}: expected the provision to answer the range {canonical:?}, got \
             {answered:?}"
        );
        assert_eq!(
            answered.state, IPAM_POOL_CIDR_STATE_PENDING_PROVISION,
            "case {case:?}: expected the transient provision state, got {answered:?}"
        );
        // The netmask length is the RANGE's own, not a figure the request carried: this
        // request named none at all.
        let expected_length = canonical
            .split_once('/')
            .map(|(_, length)| length.to_string());
        assert_eq!(
            answered.netmask_length, expected_length,
            "case {case:?}: expected the netmask length {expected_length:?} of the range \
             itself, got {answered:?}"
        );
        assert!(
            super::wire::is_hex_id(IPAM_POOL_CIDR_ID_PREFIX, &answered.id, POOL_CIDR_ID_DIGITS),
            "case {case:?}: expected a minted {IPAM_POOL_CIDR_ID_PREFIX} id of \
             {POOL_CIDR_ID_DIGITS} hexadecimal digits, got {:?}",
            answered.id
        );

        // Read back out of the store straight away: the same record, with the state either the
        // one the call left it in or the one the clock has since made due — never a third
        // thing, and never a different id, range or netmask length.
        let reported = held(case, &router, &pool_id).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the pool to report exactly the provisioned range, got \
             {reported:?}"
        );
        assert_eq!(
            (
                reported[0].id.as_str(),
                reported[0].cidr.as_str(),
                reported[0].netmask_length.clone(),
            ),
            (
                answered.id.as_str(),
                answered.cidr.as_str(),
                answered.netmask_length.clone(),
            ),
            "case {case:?}: expected the read to report what the provision answered \
             ({answered:?}), got {reported:?}"
        );
        assert!(
            [
                IPAM_POOL_CIDR_STATE_PENDING_PROVISION,
                IPAM_POOL_CIDR_STATE_PROVISIONED,
            ]
            .contains(&reported[0].state.as_str()),
            "case {case:?}: expected the read to report the provision's own state or the one \
             the clock owes, got {reported:?}"
        );

        // The clock owes it the terminal state, which it reaches whether or not anybody
        // called.
        let settled = poll_until_state(
            case,
            &router,
            &pool_id,
            canonical,
            IPAM_POOL_CIDR_STATE_PROVISIONED,
        )
        .await;
        assert_eq!(
            settled, IPAM_POOL_CIDR_STATE_PROVISIONED,
            "case {case:?}: expected the clock to settle {canonical:?} as \
             {IPAM_POOL_CIDR_STATE_PROVISIONED:?}, got {settled:?}"
        );
        // Settling moved the state and nothing else: the id and the range are the ones the
        // provision minted.
        let settled_record = held(case, &router, &pool_id).await;
        assert_eq!(
            settled_record,
            vec![ReportedCidr {
                state: IPAM_POOL_CIDR_STATE_PROVISIONED.to_string(),
                ..answered.clone()
            }],
            "case {case:?}: expected only the state to have moved, got {settled_record:?}"
        );
    }
}

/// A pool holds every range it was given, each with its own id, and the store reports them all.
///
/// The rows differ along how MANY ranges the pool holds and their sizes, which is what "the
/// CIDRs provisioned to a pool" generalises over.
#[tokio::test]
async fn a_pool_holds_every_range_it_was_given() {
    // (case, the ranges provisioned to the pool, in the order they were asked for)
    let rows: [(&str, &[&str]); 3] = [
        ("one range", &["10.20.0.0/16"]),
        ("two ranges of different sizes", &["10.21.0.0/16", "10.22.0.0/24"]),
        (
            "four ranges, asked for out of order",
            &["10.33.0.0/16", "10.31.0.0/20", "10.34.0.0/32", "10.32.0.0/24"],
        ),
    ];

    for (case, ranges) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let pool_id = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;

        let mut minted: Vec<ReportedCidr> = Vec::new();
        for cidr in ranges {
            minted.push(provision(case, &router, &pool_id, cidr).await);
        }

        let mut ids: Vec<String> = minted.iter().map(|record| record.id.clone()).collect();
        ids.sort();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(
            ids.len(),
            unique,
            "case {case:?}: expected one id per range, got {minted:?}"
        );

        let reported = held(case, &router, &pool_id).await;
        let mut expected = minted;
        expected.sort_by(|left, right| left.cidr.cmp(&right.cidr));
        let reported_ranges: Vec<&str> = reported
            .iter()
            .map(|record| record.cidr.as_str())
            .collect();
        let expected_ranges: Vec<&str> = expected
            .iter()
            .map(|record| record.cidr.as_str())
            .collect();
        assert_eq!(
            reported_ranges, expected_ranges,
            "case {case:?}: expected the pool to hold {expected_ranges:?}, got \
             {reported_ranges:?}"
        );
        let reported_ids: Vec<&str> = reported.iter().map(|record| record.id.as_str()).collect();
        let expected_ids: Vec<&str> = expected.iter().map(|record| record.id.as_str()).collect();
        assert_eq!(
            reported_ids, expected_ids,
            "case {case:?}: expected the ids the provisions minted, got {reported_ids:?}"
        );
    }
}

/// A request that names a SIZE rather than a range asks the service to choose the range, and a
/// pool within another is given space out of its source pool: the chosen range is of the asked
/// for size, inside the source pool's space, and never one already handed out of it.
///
/// The rows differ along the size asked for and the size of the source pool's own space.
#[tokio::test]
async fn a_netmask_length_is_answered_with_free_space_of_the_source_pool() {
    // (case, the source pool's range, the netmask length asked for, the two blocks given out)
    let rows: [(&str, &str, &str, [&str; 2]); 4] = [
        (
            "a /24 out of a /16",
            "10.40.0.0/16",
            "24",
            ["10.40.0.0/24", "10.40.1.0/24"],
        ),
        (
            "a /20 out of a /16",
            "10.41.0.0/16",
            "20",
            ["10.41.0.0/20", "10.41.16.0/20"],
        ),
        (
            "a /32 out of a /24",
            "10.42.7.0/24",
            "32",
            ["10.42.7.0/32", "10.42.7.1/32"],
        ),
        (
            "a /16 out of an /8",
            "10.0.0.0/8",
            "16",
            ["10.0.0.0/16", "10.1.0.0/16"],
        ),
    ];

    for (case, source_range, netmask_length, expected) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let source = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;
        provision(case, &router, &source, source_range).await;
        let child = create_pool(
            case,
            &router,
            &scope,
            ADDRESS_FAMILY_IPV4,
            Some(source.as_str()),
        )
        .await;

        let mut given: Vec<String> = Vec::new();
        for (index, expected_block) in expected.iter().enumerate() {
            let body = ok(
                case,
                &router,
                PROVISION,
                &[
                    param("IpamPoolId", &child),
                    param("NetmaskLength", netmask_length),
                ],
            )
            .await;
            let answered = reported_one(case, &body);
            assert_eq!(
                answered.netmask_length.as_deref(),
                Some(netmask_length),
                "case {case:?}: provision {index} must report the netmask length asked for, \
                 got {answered:?}"
            );
            assert_eq!(
                &answered.cidr, expected_block,
                "case {case:?}: provision {index} must be given the first free block \
                 {expected_block:?} of the source pool's space, got {answered:?}"
            );
            given.push(answered.cidr.clone());
        }

        // Whatever the chosen ranges are, they come out of the source pool's space and the two
        // calls never hand the same address out twice.
        for block in &given {
            assert!(
                ipv4_contains(case, source_range, block),
                "case {case:?}: expected {block:?} to come out of the source pool's space \
                 {source_range:?}"
            );
        }
        assert!(
            !ipv4_blocks_overlap(case, &given[0], &given[1]),
            "case {case:?}: expected two calls to hand out disjoint space, got {given:?}"
        );

        let reported = held(case, &router, &child).await;
        let reported_ranges: Vec<&str> = reported
            .iter()
            .map(|record| record.cidr.as_str())
            .collect();
        let mut expected_ranges = expected.to_vec();
        expected_ranges.sort();
        assert_eq!(
            reported_ranges, expected_ranges,
            "case {case:?}: expected the child pool to hold {expected_ranges:?}, got \
             {reported_ranges:?}"
        );
    }
}

/// A TOP-LEVEL IPv6 pool asking for a size is given a block of the space Amazon advertises its
/// provided IPv6 CIDRs from, of exactly the size asked for.
///
/// The rows differ along the size asked for, from the whole Amazon-provided block down to a
/// single address.
#[tokio::test]
async fn a_netmask_length_on_a_top_level_ipv6_pool_takes_amazon_space() {
    // (case, the netmask length asked for)
    let rows: [(&str, u32); 4] = [
        ("the whole Amazon-provided block", AMAZON_IPV6_PREFIX_LENGTH),
        ("a /60 of it", 60),
        ("a /64 of it", 64),
        ("a single address of it", 128),
    ];

    for (case, netmask_length) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let pool_id = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV6, None).await;

        let mut given: Vec<String> = Vec::new();
        for index in 0..2 {
            let body = ok(
                case,
                &router,
                PROVISION,
                &[
                    param("IpamPoolId", &pool_id),
                    param("NetmaskLength", &netmask_length.to_string()),
                ],
            )
            .await;
            let answered = reported_one(case, &body);
            assert_eq!(
                answered.netmask_length,
                Some(netmask_length.to_string()),
                "case {case:?}: provision {index} must report the netmask length asked for, \
                 got {answered:?}"
            );
            assert!(
                ipv6_contains(case, AMAZON_IPV6_SPACE, &answered.cidr),
                "case {case:?}: provision {index} must come out of the Amazon space \
                 {AMAZON_IPV6_SPACE}, got {answered:?}"
            );
            given.push(answered.cidr);
        }
        assert!(
            !ipv6_contains(case, &given[0], &given[1])
                && !ipv6_contains(case, &given[1], &given[0]),
            "case {case:?}: expected two calls to hand out disjoint space, got {given:?}"
        );

        let reported = held(case, &router, &pool_id).await;
        let mut expected = given;
        expected.sort();
        let reported_ranges: Vec<String> = reported
            .iter()
            .map(|record| record.cidr.clone())
            .collect();
        assert_eq!(
            reported_ranges, expected,
            "case {case:?}: expected the pool to hold the ranges it was given, got \
             {reported_ranges:?}"
        );
    }
}

/// A request the pool cannot be given the range it asks for is refused, and NOTHING is stored.
///
/// The rows differ along the family the pool holds, the form of the request, and how the
/// requested range relates to the space the pool already holds — the three things "space this
/// pool can hand out" generalises over.
#[tokio::test]
async fn a_range_the_pool_cannot_hand_out_is_refused_and_stored_nowhere() {
    // (case, the pool's family, the ranges it already holds, the provision's own parameters,
    //  the code, the message — built from the pool id the service minted)
    type Refusal = (
        &'static str,
        &'static str,
        &'static [&'static str],
        &'static [(&'static str, &'static str)],
        &'static str,
        fn(&str) -> String,
    );
    let rows: [Refusal; 10] = [
        (
            "neither a cidr nor a netmask length",
            ADDRESS_FAMILY_IPV4,
            &[],
            &[],
            INVALID_PARAMETER_COMBINATION,
            |_| {
                "The request must contain either a CIDR or Netmask Length.".to_string()
            },
        ),
        (
            "both a cidr and a netmask length",
            ADDRESS_FAMILY_IPV4,
            &[],
            &[("Cidr", "10.50.0.0/16"), ("NetmaskLength", "24")],
            INVALID_PARAMETER_COMBINATION,
            |_| {
                "The request cannot contain both CIDR and Netmask Length. Exactly one of \
                 them should be provided."
                    .to_string()
            },
        ),
        (
            "an IPv6 range for a pool that holds IPv4 space",
            ADDRESS_FAMILY_IPV4,
            &[],
            &[("Cidr", "2001:db8::/32")],
            INVALID_PARAMETER_VALUE,
            |_| {
                "Invalid value '2001:db8::/32' for cidr. It must be an ipv4 CIDR block"
                    .to_string()
            },
        ),
        (
            "an IPv4 range for a pool that holds IPv6 space",
            ADDRESS_FAMILY_IPV6,
            &[],
            &[("Cidr", "10.51.0.0/16")],
            INVALID_PARAMETER_VALUE,
            |_| "Invalid value '10.51.0.0/16' for cidr. It must be an ipv6 CIDR block".to_string(),
        ),
        (
            "an address with no prefix length at all",
            ADDRESS_FAMILY_IPV4,
            &[],
            &[("Cidr", "10.52.0.0")],
            INVALID_PARAMETER_VALUE,
            |_| "Invalid value '10.52.0.0' for cidr. It must be an ipv4 CIDR block".to_string(),
        ),
        (
            "a prefix length no IPv4 block has",
            ADDRESS_FAMILY_IPV4,
            &[],
            &[("Cidr", "10.53.0.0/33")],
            INVALID_PARAMETER_VALUE,
            |_| "Invalid value '10.53.0.0/33' for cidr. It must be an ipv4 CIDR block".to_string(),
        ),
        (
            "the range the pool already holds",
            ADDRESS_FAMILY_IPV4,
            &["10.54.0.0/16"],
            &[("Cidr", "10.54.0.0/16")],
            INVALID_PARAMETER_VALUE,
            |pool_id| {
                format!(
                    "Invalid value '10.54.0.0/16' for cidr. The IPAM pool '{pool_id}' already \
                     holds that CIDR, in state 'provisioned'"
                )
            },
        ),
        (
            "a spelling of the range the pool already holds",
            ADDRESS_FAMILY_IPV4,
            &["10.55.0.0/16"],
            &[("Cidr", "10.55.1.2/16")],
            INVALID_PARAMETER_VALUE,
            |pool_id| {
                format!(
                    "Invalid value '10.55.0.0/16' for cidr. The IPAM pool '{pool_id}' already \
                     holds that CIDR, in state 'provisioned'"
                )
            },
        ),
        (
            "a block inside a range the pool holds",
            ADDRESS_FAMILY_IPV4,
            &["10.56.0.0/16"],
            &[("Cidr", "10.56.4.0/24")],
            INVALID_PARAMETER_VALUE,
            |pool_id| {
                format!(
                    "Invalid value '10.56.4.0/24' for cidr. It overlaps with the CIDR \
                     '10.56.0.0/16' already provisioned to the IPAM pool '{pool_id}'"
                )
            },
        ),
        (
            "a block a range the pool holds lies inside",
            ADDRESS_FAMILY_IPV4,
            &["10.57.0.0/16"],
            &[("Cidr", "10.0.0.0/8")],
            INVALID_PARAMETER_VALUE,
            |pool_id| {
                format!(
                    "Invalid value '10.0.0.0/8' for cidr. It overlaps with the CIDR \
                     '10.57.0.0/16' already provisioned to the IPAM pool '{pool_id}'"
                )
            },
        ),
    ];

    for (case, family, already, extra, code, message) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let pool_id = create_pool(case, &router, &scope, family, None).await;
        for cidr in already {
            provision(case, &router, &pool_id, cidr).await;
            // The state a refusal quotes is the one the store is ACTUALLY in, so the range is
            // let settle first: what is under test is the refusal, not the clock.
            let settled = poll_until_state(
                case,
                &router,
                &pool_id,
                cidr,
                IPAM_POOL_CIDR_STATE_PROVISIONED,
            )
            .await;
            assert_eq!(
                settled, IPAM_POOL_CIDR_STATE_PROVISIONED,
                "case {case:?}: the setup range {cidr:?} must settle before the refusal is \
                 asked for, got {settled:?}"
            );
        }

        let before = held(case, &router, &pool_id).await;
        let mut params = vec![param("IpamPoolId", &pool_id)];
        params.extend(extra.iter().map(|(name, value)| param(name, value)));
        refused(
            case,
            &router,
            PROVISION,
            &params,
            code,
            &message(&pool_id),
        )
        .await;

        let after = held(case, &router, &pool_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused call to store nothing, got {after:?}"
        );
    }
}

/// A pool id the request names that this service cannot resolve is refused before the range is
/// read at all: a value that is not an IPAM pool id is malformed, and a well-formed id the
/// account holds no pool for does not exist.
///
/// The rows differ along WHY the id does not resolve and how it fails to: the two questions are
/// asked in that order, so "not an id of this kind" is never reported as "no such pool".
#[tokio::test]
async fn a_pool_id_that_resolves_to_nothing_is_refused_in_its_own_words() {
    // (case, the id the request names, the code, the message)
    let rows: [(&str, &str, &str, String); 6] = [
        (
            "a pool id with no digits at all",
            "ipam-pool-",
            INVALID_IPAM_POOL_ID_MALFORMED,
            "Invalid id: \"ipam-pool-\"".to_string(),
        ),
        (
            "a value of another resource kind",
            "vpc-0123456789abcdef0",
            INVALID_IPAM_POOL_ID_MALFORMED,
            "Invalid id: \"vpc-0123456789abcdef0\"".to_string(),
        ),
        (
            "an IPAM scope id",
            "ipam-scope-0123456789abcdef0",
            INVALID_IPAM_POOL_ID_MALFORMED,
            "Invalid id: \"ipam-scope-0123456789abcdef0\"".to_string(),
        ),
        (
            "a pool id with too few digits",
            "ipam-pool-0123",
            INVALID_IPAM_POOL_ID_MALFORMED,
            "Invalid id: \"ipam-pool-0123\"".to_string(),
        ),
        (
            "a pool id whose digits are not hexadecimal",
            "ipam-pool-0123456789abcdefg",
            INVALID_IPAM_POOL_ID_MALFORMED,
            "Invalid id: \"ipam-pool-0123456789abcdefg\"".to_string(),
        ),
        (
            "a well-formed pool id the account holds no pool for",
            UNKNOWN_POOL_ID,
            INVALID_IPAM_POOL_ID_NOT_FOUND,
            format!("The ipam-pool ID '{UNKNOWN_POOL_ID}' does not exist"),
        ),
    ];

    for (case, pool_id, code, message) in rows {
        let (router, _engine) = emulator();
        refused(
            case,
            &router,
            PROVISION,
            &[param("IpamPoolId", pool_id), param("Cidr", "10.60.0.0/16")],
            code,
            &message,
        )
        .await;
    }
}

/// A pool within another may only be given space its SOURCE pool holds, and a size it has no
/// free block for is refused: "transfer a CIDR from a top-level pool to a pool within it"
/// (`smithy_query(query="doc",shape="ProvisionIpamPoolCidr")`).
///
/// The rows differ along whether the request names a range outside the source pool's space or a
/// size the source pool holds no free block of, and along the size of that space.
#[tokio::test]
async fn space_the_source_pool_does_not_hold_is_refused() {
    // (case, the source pool's range, the provision's parameters, the code, the message)
    type Refusal = (
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static str,
        fn(&str) -> String,
    );
    let rows: [Refusal; 5] = [
        (
            // A SIZE no address of the pool's family has names no block at all, which is a
            // property of the request rather than of the space the source pool holds — so it
            // is refused here instead of being accepted and settled as a failed provision.
            "a size one bit past the longest IPv4 netmask",
            "10.68.0.0/16",
            &[("NetmaskLength", "33")],
            INVALID_PARAMETER_VALUE,
            |_| {
                "Invalid value '33' for netmaskLength. An ipv4 CIDR has a netmask length \
                 between 0 and 32"
                    .to_string()
            },
        ),
        (
            "a size no IPv4 address has at all",
            "10.69.0.0/16",
            &[("NetmaskLength", "40")],
            INVALID_PARAMETER_VALUE,
            |_| {
                "Invalid value '40' for netmaskLength. An ipv4 CIDR has a netmask length \
                 between 0 and 32"
                    .to_string()
            },
        ),
        (
            "a range of altogether different space",
            "10.70.0.0/16",
            &[("Cidr", "192.0.2.0/24")],
            INVALID_PARAMETER_VALUE,
            |source| {
                format!(
                    "Invalid value '192.0.2.0/24' for cidr. It is not within a CIDR \
                     provisioned to the source IPAM pool '{source}'"
                )
            },
        ),
        (
            "a range the source pool's own space lies inside",
            "10.71.0.0/16",
            &[("Cidr", "10.0.0.0/8")],
            INVALID_PARAMETER_VALUE,
            |source| {
                format!(
                    "Invalid value '10.0.0.0/8' for cidr. It is not within a CIDR provisioned \
                     to the source IPAM pool '{source}'"
                )
            },
        ),
        (
            "a range straddling the edge of the source pool's space",
            "10.72.0.0/16",
            &[("Cidr", "10.72.0.0/15")],
            INVALID_PARAMETER_VALUE,
            |source| {
                format!(
                    "Invalid value '10.72.0.0/15' for cidr. It is not within a CIDR \
                     provisioned to the source IPAM pool '{source}'"
                )
            },
        ),
    ];

    for (case, source_range, extra, code, message) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let source = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;
        provision(case, &router, &source, source_range).await;
        let child = create_pool(
            case,
            &router,
            &scope,
            ADDRESS_FAMILY_IPV4,
            Some(source.as_str()),
        )
        .await;

        let mut params = vec![param("IpamPoolId", &child)];
        params.extend(extra.iter().map(|(name, value)| param(name, value)));
        refused(case, &router, PROVISION, &params, code, &message(&source)).await;

        let after = held(case, &router, &child).await;
        assert!(
            after.is_empty(),
            "case {case:?}: expected the refused call to store nothing, got {after:?}"
        );
    }
}

/// A SIZE the source pool holds no free block of is ACCEPTED, and the range it would have
/// created settles as a failed provision — the refusal is the state of the range, not the
/// answer to the call.
///
/// The request names a size the pool's own rules admit; what is missing is SPACE, which is a
/// property of the source pool at that instant rather than of the request. So the provision is
/// served, the record starts `pending-provision` carrying the size asked for and no range at
/// all, and the clock settles it as `failed-provision` with the failure catalog's own code for
/// a range that could not be had.
///
/// The rows differ along WHY there is no free block: a size larger than the whole of the source
/// pool's space, a size one bit larger than it, and a size that would fit if another pool had
/// not already been given every address of it.
#[tokio::test]
async fn a_size_the_source_pool_has_no_free_block_for_is_accepted_and_settles_as_failed() {
    // (case, the source pool's range, the netmask length asked for, whether another pool is
    //  given the whole of the source pool's space first)
    let rows: [(&str, &str, &str, bool); 3] = [
        (
            "a size larger than the whole of the source pool's space",
            "10.73.0.0/24",
            "16",
            false,
        ),
        (
            "a size one bit larger than the source pool's space",
            "10.74.0.0/24",
            "23",
            false,
        ),
        (
            "a size the source pool has already given every address of away",
            "10.75.0.0/24",
            "24",
            true,
        ),
    ];

    for (case, source_range, netmask_length, already_taken) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let source = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;
        provision(case, &router, &source, source_range).await;
        if already_taken {
            let first = create_pool(
                case,
                &router,
                &scope,
                ADDRESS_FAMILY_IPV4,
                Some(source.as_str()),
            )
            .await;
            provision(case, &router, &first, source_range).await;
        }
        let child = create_pool(
            case,
            &router,
            &scope,
            ADDRESS_FAMILY_IPV4,
            Some(source.as_str()),
        )
        .await;

        let body = ok(
            case,
            &router,
            PROVISION,
            &[
                param("IpamPoolId", &child),
                param("NetmaskLength", netmask_length),
            ],
        )
        .await;
        let answered = reported_one(case, &body);
        assert_eq!(
            answered.state, IPAM_POOL_CIDR_STATE_PENDING_PROVISION,
            "case {case:?}: expected the accepted provision to answer \
             {IPAM_POOL_CIDR_STATE_PENDING_PROVISION:?}, got {answered:?}"
        );
        assert_eq!(
            answered.netmask_length.as_deref(),
            Some(netmask_length),
            "case {case:?}: expected the size asked for to be reported, got {answered:?}"
        );
        assert!(
            answered.cidr.is_empty(),
            "case {case:?}: expected no range at all on a provision nothing could be chosen \
             for, got {answered:?}"
        );
        assert!(
            answered.failure_code.is_none(),
            "case {case:?}: expected no failure reason while the provision is still under \
             way, got {answered:?}"
        );

        let settled = poll_until_record_state(
            case,
            &router,
            &child,
            &answered.id,
            IPAM_POOL_CIDR_STATE_FAILED_PROVISION,
        )
        .await;
        assert_eq!(
            settled.state, IPAM_POOL_CIDR_STATE_FAILED_PROVISION,
            "case {case:?}: expected the clock to settle the provision as \
             {IPAM_POOL_CIDR_STATE_FAILED_PROVISION:?}, got {settled:?}"
        );
        assert_eq!(
            settled.failure_code.as_deref(),
            Some(IPAM_POOL_CIDR_FAILURE_CODE_NOT_AVAILABLE),
            "case {case:?}: expected the failure to carry \
             {IPAM_POOL_CIDR_FAILURE_CODE_NOT_AVAILABLE:?}, got {settled:?}"
        );
        assert!(
            settled
                .failure_message
                .as_deref()
                .is_some_and(|message| message.contains(&source)),
            "case {case:?}: expected the failure to say which pool had no space, got \
             {settled:?}"
        );

        // The failed provision gave the pool no space: a range it holds is one a later
        // allocation could come out of, and this one holds none.
        let after = held(case, &router, &child).await;
        assert!(
            after.iter().all(|reported| reported.cidr.is_empty()),
            "case {case:?}: expected the pool to hold no range, got {after:?}"
        );
    }
}

/// A TOP-LEVEL pool has no source pool to take space out of, so a request that names a SIZE is
/// refused unless the family has space the service itself provides — Amazon's own IPv6 space,
/// which is a block of one published size.
///
/// The rows differ along the family of the top-level pool and, for IPv6, along how the size
/// asked for relates to the published size of an Amazon-provided block: each bound and one past
/// it.
#[tokio::test]
async fn a_size_a_top_level_pool_has_no_space_for_is_refused() {
    // (case, the pool's family, the netmask length asked for, the code, the message)
    let rows: [(&str, &str, &str, &str, String); 5] = [
        (
            "an IPv4 top-level pool, which has no Amazon-provided space",
            ADDRESS_FAMILY_IPV4,
            "24",
            INVALID_PARAMETER_COMBINATION,
            "netmaskLength cannot be used to provision BYOIP CIDRs to top-level pools; name \
             the cidr instead"
                .to_string(),
        ),
        (
            "an IPv4 top-level pool asked for its whole address space",
            ADDRESS_FAMILY_IPV4,
            "32",
            INVALID_PARAMETER_COMBINATION,
            "netmaskLength cannot be used to provision BYOIP CIDRs to top-level pools; name \
             the cidr instead"
                .to_string(),
        ),
        (
            "an IPv6 range larger than an Amazon-provided block",
            ADDRESS_FAMILY_IPV6,
            "55",
            INVALID_PARAMETER_VALUE,
            format!(
                "Invalid value '55' for netmaskLength. An Amazon-provided IPv6 CIDR is a \
                 /{AMAZON_IPV6_PREFIX_LENGTH} block, so no range of that size can be taken \
                 from it"
            ),
        ),
        (
            "an IPv6 range far larger than an Amazon-provided block",
            ADDRESS_FAMILY_IPV6,
            "32",
            INVALID_PARAMETER_VALUE,
            format!(
                "Invalid value '32' for netmaskLength. An Amazon-provided IPv6 CIDR is a \
                 /{AMAZON_IPV6_PREFIX_LENGTH} block, so no range of that size can be taken \
                 from it"
            ),
        ),
        (
            "an IPv6 netmask length no address has",
            ADDRESS_FAMILY_IPV6,
            "129",
            INVALID_PARAMETER_VALUE,
            format!(
                "Invalid value '129' for netmaskLength. An Amazon-provided IPv6 CIDR is a \
                 /{AMAZON_IPV6_PREFIX_LENGTH} block, so no range of that size can be taken \
                 from it"
            ),
        ),
    ];

    for (case, family, netmask_length, code, message) in rows {
        let (router, _engine) = emulator();
        let scope = create_scope(case, &router).await;
        let pool_id = create_pool(case, &router, &scope, family, None).await;

        refused(
            case,
            &router,
            PROVISION,
            &[
                param("IpamPoolId", &pool_id),
                param("NetmaskLength", netmask_length),
            ],
            code,
            &message,
        )
        .await;

        let after = held(case, &router, &pool_id).await;
        assert!(
            after.is_empty(),
            "case {case:?}: expected the refused call to store nothing, got {after:?}"
        );
    }
}

/// A `ClientToken` longer than the service keeps is refused, and the pool is left untouched.
#[tokio::test]
async fn a_client_token_longer_than_the_service_keeps_is_refused() {
    let case = "a client token one character past the bound";
    let (router, _engine) = emulator();
    let scope = create_scope(case, &router).await;
    let pool_id = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;

    // The bound itself is accepted; one character past it is not.
    let at_the_bound = "t".repeat(MAX_CLIENT_TOKEN_CHARS);
    let body = ok(
        case,
        &router,
        PROVISION,
        &[
            param("IpamPoolId", &pool_id),
            param("Cidr", "10.80.0.0/16"),
            param("ClientToken", &at_the_bound),
        ],
    )
    .await;
    let accepted = reported_one(case, &body);
    assert_eq!(
        accepted.cidr, "10.80.0.0/16",
        "case {case:?}: a token of exactly the bound must be accepted, got {accepted:?}"
    );

    let past_the_bound = "t".repeat(MAX_CLIENT_TOKEN_CHARS + 1);
    refused(
        case,
        &router,
        PROVISION,
        &[
            param("IpamPoolId", &pool_id),
            param("Cidr", "10.81.0.0/16"),
            param("ClientToken", &past_the_bound),
        ],
        INVALID_PARAMETER_VALUE,
        &format!("clientToken accepts at most {MAX_CLIENT_TOKEN_CHARS} characters"),
    )
    .await;

    let after = held(case, &router, &pool_id).await;
    let ranges: Vec<&str> = after.iter().map(|record| record.cidr.as_str()).collect();
    assert_eq!(
        ranges,
        vec!["10.80.0.0/16"],
        "case {case:?}: expected the refused call to store nothing, got {after:?}"
    );
}

/// `DryRun` asks whether the caller MAY provision, without actually provisioning: the answer is
/// the dry-run refusal and the pool is left exactly as it was.
#[tokio::test]
async fn a_dry_run_provisions_nothing() {
    let case = "a dry run against a pool that holds one range";
    let (router, _engine) = emulator();
    let scope = create_scope(case, &router).await;
    let pool_id = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;
    provision(case, &router, &pool_id, "10.90.0.0/16").await;
    let before = held(case, &router, &pool_id).await;

    let (status, body) = call(
        &router,
        PROVISION,
        &[
            param("IpamPoolId", &pool_id),
            param("Cidr", "10.91.0.0/16"),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_OPERATION_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_OPERATION_STATUS} from a dry run, got {status} \
         with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, DRY_RUN_OPERATION,
        "case {case:?}: expected the code {DRY_RUN_OPERATION:?}, got {code:?} in {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, DRY_RUN_OPERATION_MESSAGE,
        "case {case:?}: expected the message {DRY_RUN_OPERATION_MESSAGE:?}, got {message:?}"
    );

    let after = held(case, &router, &pool_id).await;
    let before_ranges: Vec<&str> = before.iter().map(|record| record.cidr.as_str()).collect();
    let after_ranges: Vec<&str> = after.iter().map(|record| record.cidr.as_str()).collect();
    assert_eq!(
        after_ranges, before_ranges,
        "case {case:?}: expected the dry run to store nothing, got {after:?}"
    );
}

/// A range provisioned to one pool is not one another pool holds: two pools of the same scope
/// can each be given the SAME range, and each reports only its own.
#[tokio::test]
async fn a_range_is_held_by_the_pool_it_was_provisioned_to() {
    let case = "two top-level pools given the same range";
    let (router, _engine) = emulator();
    let scope = create_scope(case, &router).await;
    let first = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;
    let second = create_pool(case, &router, &scope, ADDRESS_FAMILY_IPV4, None).await;
    let shared = "10.100.0.0/16";

    let first_record = provision(case, &router, &first, shared).await;
    let second_record = provision(case, &router, &second, shared).await;
    assert_ne!(
        first_record.id, second_record.id,
        "case {case:?}: each pool's record of the range must be its own, got \
         {first_record:?} and {second_record:?}"
    );

    for (pool_id, expected) in [(&first, &first_record), (&second, &second_record)] {
        let reported = held(case, &router, pool_id).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the pool {pool_id} to hold one range, got {reported:?}"
        );
        assert_eq!(
            reported[0].id, expected.id,
            "case {case:?}: expected the pool {pool_id} to report its own record, got \
             {reported:?}"
        );
    }
}
