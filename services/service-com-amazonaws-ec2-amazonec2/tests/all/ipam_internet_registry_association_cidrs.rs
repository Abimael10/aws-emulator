// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetIpamInternetRegistryAssociationCidrs`, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * The report lists the IP address CIDRs the IPAM behind the association holds at an
//!   internet registry: the address ranges the account has brought to THAT IPAM — a range
//!   names the pool that manages it (`ByoipCidr$IpamPoolId`) and a pool names its IPAM
//!   (`IpamPool$IpamArn`) — in a state where the range's registration at the registry has been
//!   checked and the account has not finished giving it back. So a range in another IPAM's
//!   pool, one in no IPAM pool at all, and one whose registration was never confirmed or has
//!   already been given back are not reported, while two associations of the same IPAM report
//!   the same CIDRs.
//! * The report is paginated like every other paged EC2 read: at most `MaxResults` CIDRs in a
//!   stable prefix order with a `NextToken` naming where the next page starts, every CIDR in
//!   one answer with no token when the request names no page size ("if not specified, all
//!   available results are returned"), narrowed by each filter the operation resolves — one
//!   per member of the reported shape, spelled in kebab-case, values joined with OR and
//!   filters with AND — and it changes nothing a later caller sees.
//! * An id outside the form this service ISSUES for an internet registry association is
//!   refused with that resource's `.Malformed` before any lookup; an id of that form the
//!   account holds no association for with its `.NotFound`; a filter name or a page token the
//!   operation will not serve is refused rather than answered as "no CIDRs"; and `DryRun`
//!   answers the permission question and reads nothing.
//!
//! Each table row is a distinct input along what those rules generalise over: the address
//! family of the range, its prefix length, which `ByoipCidrState` it sits in, which pool of
//! which IPAM manages it, which member a filter selects on and how large a page is asked for.
//!
//! The ranges used are from the blocks RFC 5737 and RFC 3849 set aside for documentation
//! (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 and 2001:db8::/32), so no row names an
//! address range that belongs to anybody.
//!
//! The IPAM and its pools are created over the wire; the ranges themselves are seeded into the
//! store the way `ipam_route_protection_findings` seeds its own, because these rows need a
//! range sitting in a chosen `ByoipCidrState` under a chosen pool and no single operation puts
//! one there.

use std::collections::BTreeSet;

use ec2::ops::types::BYOIP_CIDR_STATE_VALUES;
use ec2::ops::types::IpamInternetRegistryAssociation;
use ec2::state::{
    ByoipCidrRecord, ComAmazonawsEc2Amazonec2State as Ec2State,
    IPAM_INTERNET_REGISTRY_ASSOCIATION_ID_PREFIX, RESOURCE_TYPE_IPAM_INTERNET_REGISTRY_ASSOCIATION,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, element, elements, emulator, error_code, error_message};

/// The operation under test.
const ACTION: &str = "GetIpamInternetRegistryAssociationCidrs";

/// The Region every call here is signed for. An association reports the Region of the IPAM it
/// belongs to, so the Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The Regional Internet Registry a seeded association is with: the modelled `Rir` catalog's
/// `arin` member.
const RIR_ARIN: &str = "arin";

/// The `IpamInternetRegistryAssociationState` catalog's `enable-complete` member: an
/// association in service.
const STATE_ENABLE_COMPLETE: &str = "enable-complete";

/// How the `AddressFamily` enum spells the two address spaces a pool holds.
const FAMILY_IPV4: &str = "ipv4";
const FAMILY_IPV6: &str = "ipv6";

/// The `ByoipCidrState` words the rows below sit in. Spelled out here rather than imported, so
/// the service cannot make this suite pass by agreeing with itself; every one of them is
/// asserted to be a member of the model's own `ByoipCidrState` catalog.
const PROVISIONED: &str = "provisioned";
const PROVISIONED_NOT_PUBLICLY_ADVERTISABLE: &str = "provisioned-not-publicly-advertisable";
const PENDING_ADVERTISING: &str = "pending-advertising";
const ADVERTISED: &str = "advertised";
const PENDING_WITHDRAWAL: &str = "pending-withdrawal";
const PENDING_DEPROVISION: &str = "pending-deprovision";
const FAILED_DEPROVISION: &str = "failed-deprovision";
const PENDING_PROVISION: &str = "pending-provision";
const FAILED_PROVISION: &str = "failed-provision";
const DEPROVISIONED: &str = "deprovisioned";

/// One form field of an ec2Query request body.
type Param = (String, String);

/// One `(name, value)` form field.
fn param(name: &str, value: &str) -> Param {
    (name.to_string(), value.to_string())
}

/// One call of `action`, signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of the operation under test, as its HTTP status and XML body.
async fn get_cidrs(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    invoke(router, ACTION, params).await
}

/// One call required to succeed, and the body it answered.
async fn get_cidrs_ok(case: &str, router: &MultiServiceRouter, params: &[Param]) -> String {
    let (status, body) = get_cidrs(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
    );
    body
}

/// The request fields that ask for the CIDRs of `association_id`, plus `extra` form fields.
fn cidr_params(association_id: &str, extra: &[Param]) -> Vec<Param> {
    let mut params = vec![param("IpamInternetRegistryAssociationId", association_id)];
    params.extend(extra.iter().cloned());
    params
}

/// The CIDRs a body reports, in the order it reported them.
fn reported_cidrs(body: &str) -> Vec<String> {
    elements(body, "cidr")
}

/// Create one IPAM over the wire and answer its id and the id of its default public scope.
async fn create_ipam(router: &MultiServiceRouter) -> (String, String) {
    let (status, body) = invoke(router, "CreateIpam", &[]).await;
    assert_eq!(
        status, 200,
        "creating an IPAM: expected HTTP 200, got {status} with body {body}"
    );
    let ipam_id = element(&body, "ipamId")
        .unwrap_or_else(|| panic!("the created IPAM reports an id, got {body}"));
    let public_scope = element(&body, "publicDefaultScopeId").unwrap_or_else(|| {
        panic!("the created IPAM reports its default public scope, got {body}")
    });
    (ipam_id, public_scope)
}

/// Create one pool of `family` in `scope_id` and answer the `IpamPoolId` the service minted.
async fn create_pool(router: &MultiServiceRouter, scope_id: &str, family: &str) -> String {
    let (status, body) = invoke(
        router,
        "CreateIpamPool",
        &[param("IpamScopeId", scope_id), param("AddressFamily", family)],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating a {family} pool in {scope_id}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "ipamPoolId")
        .unwrap_or_else(|| panic!("the created pool reports an id, got {body}"))
}

/// Seed one internet registry association of `ipam_id`, and answer its id.
fn seed_association(engine: &engine::InMemoryEngine, ipam_id: &str) -> String {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let association_id = guard
        .insert_ipam_internet_registry_association(IpamInternetRegistryAssociation {
            ipam_id: Some(ipam_id.to_string()),
            ipam_region: Some(REGION.to_string()),
            rir: Some(RIR_ARIN.to_string()),
            organization_handle: Some("EXAMPLE-ORG".to_string()),
            state: Some(STATE_ENABLE_COMPLETE.to_string()),
            ..Default::default()
        })
        .expect("the store holds the seeded association");
    assert!(
        association_id.starts_with(&format!("{IPAM_INTERNET_REGISTRY_ASSOCIATION_ID_PREFIX}-")),
        "the minted association id {association_id:?} must carry the issued prefix \
         {IPAM_INTERNET_REGISTRY_ASSOCIATION_ID_PREFIX:?}"
    );
    association_id
}

/// Seed one BYOIP address range the account brought to AWS, sitting in `cidr_state` and
/// managed by `pool_id` when the row names one.
fn seed_range(
    engine: &engine::InMemoryEngine,
    cidr: &str,
    cidr_state: &str,
    pool_id: Option<&str>,
) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let mut record = ByoipCidrRecord::in_state(cidr, cidr_state);
    record.byoip_cidr.ipam_pool_id = pool_id.map(str::to_string);
    assert!(
        guard.insert_byoip_cidr(record),
        "the store holds the seeded range {cidr} in state {cidr_state} under pool {pool_id:?}"
    );
    // The store keys a range by the CIDR in its own canonical form, which every assertion
    // below spells as the row wrote it, so a row whose literal is not already canonical would
    // otherwise compare against a key nothing reports.
    assert!(
        guard.byoip_cidrs.contains_key(cidr),
        "the seeded range {cidr} must be keyed as the row spells it"
    );
}

/// How many address ranges the store holds right now.
fn stored_range_count(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().byoip_cidrs.len()
}

/// Which pool of the IPAM under test manages a row's range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Managed {
    /// The IPv4 pool of the IPAM under test.
    Ipv4Pool,
    /// The IPv6 pool of the IPAM under test.
    Ipv6Pool,
}

/// One address range the account brought to AWS: a distinct input along its address family,
/// its prefix length, the `ByoipCidrState` it sits in and which pool of the IPAM manages it.
struct Row {
    /// What this row is called in a failure message.
    case: &'static str,
    /// The range itself, as `ByoipCidr$Cidr` spells one.
    cidr: &'static str,
    /// The `ByoipCidrState` the account's record of it sits in.
    cidr_state: &'static str,
    /// The pool of the IPAM under test that manages it.
    managed: Managed,
    /// Whether the internet registry still records the range as the account's, so the report
    /// carries it.
    reported: bool,
}

/// One row per member of the modelled `ByoipCidrState` catalog, over both address families.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a range aws finished provisioning",
            cidr: "198.51.100.0/26",
            cidr_state: PROVISIONED,
            managed: Managed::Ipv4Pool,
            reported: true,
        },
        Row {
            case: "a provisioned range the account asked not to be advertised",
            cidr: "198.51.100.64/26",
            cidr_state: PROVISIONED_NOT_PUBLICLY_ADVERTISABLE,
            managed: Managed::Ipv4Pool,
            reported: true,
        },
        Row {
            case: "a range whose advertisement is being brought up",
            cidr: "198.51.100.128/26",
            cidr_state: PENDING_ADVERTISING,
            managed: Managed::Ipv4Pool,
            reported: true,
        },
        Row {
            case: "a range aws is advertising",
            cidr: "198.51.100.192/26",
            cidr_state: ADVERTISED,
            managed: Managed::Ipv4Pool,
            reported: true,
        },
        Row {
            case: "a range whose advertisement is being withdrawn",
            cidr: "203.0.113.0/26",
            cidr_state: PENDING_WITHDRAWAL,
            managed: Managed::Ipv4Pool,
            reported: true,
        },
        Row {
            case: "a range the account asked back and aws has not finished giving up",
            cidr: "203.0.113.64/26",
            cidr_state: PENDING_DEPROVISION,
            managed: Managed::Ipv4Pool,
            reported: true,
        },
        Row {
            case: "a range whose release did not complete, so it is still the account's",
            cidr: "203.0.113.128/26",
            cidr_state: FAILED_DEPROVISION,
            managed: Managed::Ipv4Pool,
            reported: true,
        },
        Row {
            case: "an ipv6 range aws finished provisioning, in the ipam's ipv6 pool",
            cidr: "2001:db8::/48",
            cidr_state: PROVISIONED,
            managed: Managed::Ipv6Pool,
            reported: true,
        },
        Row {
            case: "a range whose registration at the registry is still being checked",
            cidr: "203.0.113.192/26",
            cidr_state: PENDING_PROVISION,
            managed: Managed::Ipv4Pool,
            reported: false,
        },
        Row {
            case: "a range whose provision did not complete",
            cidr: "192.0.2.0/25",
            cidr_state: FAILED_PROVISION,
            managed: Managed::Ipv4Pool,
            reported: false,
        },
        Row {
            case: "a range the account has finished giving back",
            cidr: "192.0.2.128/25",
            cidr_state: DEPROVISIONED,
            managed: Managed::Ipv4Pool,
            reported: false,
        },
    ]
}

/// The range the OTHER IPAM's pool manages, which no association of the IPAM under test may
/// report.
const OTHER_IPAM_RANGE: &str = "2001:db8:1::/48";

/// A range of the account that no IPAM pool manages at all, which no association may report.
const UNMANAGED_RANGE: &str = "2001:db8:2::/48";

/// The range of [`rows`] the account has finished giving back, which no filter may bring into
/// the report.
const DEPROVISIONED_ROW_RANGE: &str = "192.0.2.128/25";

/// What a suite drives: the router, the engine whose store holds the seeded ranges, and the
/// two IPAMs and three pools the rows are spread over.
struct Fixture {
    router: MultiServiceRouter,
    engine: std::sync::Arc<engine::InMemoryEngine>,
    /// The IPv4 pool of the IPAM under test.
    ipv4_pool: String,
    /// The IPv6 pool of the same IPAM.
    ipv6_pool: String,
    /// One association of the IPAM under test.
    association_id: String,
    /// A second association of that SAME IPAM, which must report the same CIDRs.
    sibling_id: String,
    /// An association of another IPAM of the account, which reports that IPAM's own ranges.
    other_id: String,
}

/// The emulator with two IPAMs created over the wire, a pool of each family in the first and
/// one pool in the second, and one association of each IPAM — but no address range yet.
async fn fixture() -> Fixture {
    let (router, engine) = emulator();
    let (ipam_id, public_scope) = create_ipam(&router).await;
    let ipv4_pool = create_pool(&router, &public_scope, FAMILY_IPV4).await;
    let ipv6_pool = create_pool(&router, &public_scope, FAMILY_IPV6).await;
    let (other_ipam_id, other_scope) = create_ipam(&router).await;
    let other_pool = create_pool(&router, &other_scope, FAMILY_IPV6).await;
    let association_id = seed_association(&engine, &ipam_id);
    let sibling_id = seed_association(&engine, &ipam_id);
    let other_id = seed_association(&engine, &other_ipam_id);
    seed_range(&engine, OTHER_IPAM_RANGE, PROVISIONED, Some(&other_pool));
    seed_range(&engine, UNMANAGED_RANGE, PROVISIONED, None);
    Fixture {
        router,
        engine,
        ipv4_pool,
        ipv6_pool,
        association_id,
        sibling_id,
        other_id,
    }
}

impl Fixture {
    /// Seed every row of [`rows`] under the pool of the IPAM under test it names.
    fn seed_rows(&self) {
        for row in rows() {
            let pool = match row.managed {
                Managed::Ipv4Pool => &self.ipv4_pool,
                Managed::Ipv6Pool => &self.ipv6_pool,
            };
            seed_range(&self.engine, row.cidr, row.cidr_state, Some(pool));
        }
    }
}

/// The CIDRs of [`rows`] the report must carry, in the prefix order it pages in.
fn expected_cidrs() -> Vec<String> {
    let mut expected: Vec<String> = rows()
        .into_iter()
        .filter(|row| row.reported)
        .map(|row| row.cidr.to_string())
        .collect();
    expected.sort();
    expected
}

/// The state words this suite asserts on are the model's own: each is a member of the catalog
/// the model publishes for `ByoipCidrState`, and the suite covers every member of it, so no row
/// pins a word the API does not have and no member is left untested.
#[test]
fn the_asserted_byoip_cidr_state_words_are_the_models_own() {
    for row in rows() {
        assert!(
            BYOIP_CIDR_STATE_VALUES.contains(&row.cidr_state),
            "case {case:?}: expected {state:?} to be a member of the modelled ByoipCidrState \
             catalog, which carries {BYOIP_CIDR_STATE_VALUES:?}",
            case = row.case,
            state = row.cidr_state
        );
    }
    for value in BYOIP_CIDR_STATE_VALUES {
        assert!(
            rows().iter().any(|row| row.cidr_state == *value),
            "expected a row for the ByoipCidrState {value:?}, which the modelled catalog carries"
        );
    }
}

/// The rule: the report lists the address ranges the association's own IPAM holds at the
/// registry — those whose registration there was checked and not given back — and nothing else,
/// with an observation time on each, and reporting changes nothing a later caller sees.
#[tokio::test]
async fn reporting_lists_the_cidrs_the_associations_ipam_holds_at_the_registry() {
    let fixture = fixture().await;

    // An IPAM whose pools hold no range of the account reports the empty set, not a refusal:
    // there is nothing to report and nothing has gone wrong.
    let empty = get_cidrs_ok(
        "an ipam holding no range",
        &fixture.router,
        &cidr_params(&fixture.association_id, &[]),
    )
    .await;
    assert!(
        reported_cidrs(&empty).is_empty(),
        "an ipam whose pools hold no range reports none, got {empty}"
    );
    assert_eq!(
        element(&empty, "nextToken"),
        None,
        "a single short page carries no continuation token, got {empty}"
    );

    fixture.seed_rows();

    let reported = get_cidrs_ok(
        "every range of the ipam",
        &fixture.router,
        &cidr_params(&fixture.association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_cidrs(&reported),
        expected_cidrs(),
        "the report carries exactly the ranges the registry still records for this ipam, in \
         prefix order, got {reported}"
    );
    for row in rows().into_iter().filter(|row| !row.reported) {
        assert!(
            !reported_cidrs(&reported).contains(&row.cidr.to_string()),
            "case {case:?}: a range in state {state:?} is not one the registry records, got \
             {reported}",
            case = row.case,
            state = row.cidr_state
        );
    }
    assert!(
        !reported_cidrs(&reported).contains(&OTHER_IPAM_RANGE.to_string()),
        "a range in another ipam's pool is not reported for this association, got {reported}"
    );
    assert!(
        !reported_cidrs(&reported).contains(&UNMANAGED_RANGE.to_string()),
        "a range no ipam pool manages is not reported for this association, got {reported}"
    );

    // Every reported CIDR carries "the time when the CIDR was last observed at the internet
    // registry": one observation time per CIDR of the answer, each a timestamp a client can
    // read.
    let observed = elements(&reported, "lastObservedAt");
    assert_eq!(
        observed.len(),
        expected_cidrs().len(),
        "every reported range carries its observation time, got {reported}"
    );
    for value in &observed {
        assert!(
            as_timestamp(value).is_some(),
            "the observation time {value:?} must be a timestamp, got {reported}"
        );
    }

    // The report is of the ASSOCIATION's IPAM: another association of the same IPAM reports
    // the same ranges, and an association of a different IPAM reports that IPAM's.
    let sibling = get_cidrs_ok(
        "a second association of the same ipam",
        &fixture.router,
        &cidr_params(&fixture.sibling_id, &[]),
    )
    .await;
    assert_eq!(
        reported_cidrs(&sibling),
        expected_cidrs(),
        "two associations of one ipam report the same ranges, got {sibling}"
    );
    let other = get_cidrs_ok(
        "an association of another ipam",
        &fixture.router,
        &cidr_params(&fixture.other_id, &[]),
    )
    .await;
    assert_eq!(
        reported_cidrs(&other),
        vec![OTHER_IPAM_RANGE.to_string()],
        "an association reports the ranges of its own ipam, got {other}"
    );

    // Reading is not writing: the same answer twice, and nothing added to or removed from the
    // store.
    let again = get_cidrs_ok(
        "reading the ranges twice",
        &fixture.router,
        &cidr_params(&fixture.association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_cidrs(&again),
        reported_cidrs(&reported),
        "reading the ranges twice must report the same ones, got {again}"
    );
    assert_eq!(
        stored_range_count(&fixture.engine),
        rows().len() + 2,
        "a report stores no range and removes none"
    );
}

/// The rule: every filter the operation resolves selects on one member of the reported shape,
/// its values joined with OR and its filters with AND, and a filter name it cannot resolve is
/// refused rather than answered as "no CIDRs".
///
/// The rows span the member catalog — the prefix itself (exactly, as two values, and with a
/// wildcard) and the observation time — plus values that select nothing, including a range the
/// IPAM holds in a state the registry no longer records, so a filter cannot widen the report
/// past the rule.
#[tokio::test]
async fn reporting_selects_by_every_filter_it_resolves() {
    let fixture = fixture().await;
    fixture.seed_rows();

    let every: BTreeSet<String> = expected_cidrs().into_iter().collect();

    // (case, filter name, values, the ranges that match)
    let cases: Vec<(&str, &str, Vec<&str>, BTreeSet<String>)> = vec![
        (
            "by one prefix exactly",
            "cidr",
            vec!["198.51.100.64/26"],
            ["198.51.100.64/26".to_string()].into_iter().collect(),
        ),
        (
            "by two prefixes, joined with OR",
            "cidr",
            vec!["198.51.100.0/26", "2001:db8::/48"],
            ["198.51.100.0/26".to_string(), "2001:db8::/48".to_string()]
                .into_iter()
                .collect(),
        ),
        (
            "by a prefix wildcard",
            "cidr",
            vec!["203.0.113.*"],
            [
                "203.0.113.0/26".to_string(),
                "203.0.113.64/26".to_string(),
                "203.0.113.128/26".to_string(),
            ]
            .into_iter()
            .collect(),
        ),
        (
            "by a range this account has not brought to aws",
            "cidr",
            vec!["198.18.0.0/24"],
            BTreeSet::new(),
        ),
        (
            "by a range the ipam holds in a state the registry no longer records",
            "cidr",
            vec![DEPROVISIONED_ROW_RANGE],
            BTreeSet::new(),
        ),
        (
            "by a range in another ipam's pool",
            "cidr",
            vec![OTHER_IPAM_RANGE],
            BTreeSet::new(),
        ),
        (
            "by a range no ipam pool manages",
            "cidr",
            vec![UNMANAGED_RANGE],
            BTreeSet::new(),
        ),
        (
            "by any observation time",
            "last-observed-at",
            vec!["*"],
            every.clone(),
        ),
        (
            "by an observation time that is not this one",
            "last-observed-at",
            vec!["1999-01-01T00:00:00Z"],
            BTreeSet::new(),
        ),
    ];

    for (case, name, values, expected) in cases {
        let mut params = vec![param("Filter.1.Name", name)];
        for (index, value) in values.iter().enumerate() {
            params.push(param(&format!("Filter.1.Value.{}", index + 1), value));
        }
        let body =
            get_cidrs_ok(case, &fixture.router, &cidr_params(&fixture.association_id, &params))
                .await;
        assert_eq!(
            reported_cidrs(&body)
                .into_iter()
                .collect::<BTreeSet<String>>(),
            expected,
            "case {case:?}: expected the filter {name:?} over {values:?} to select \
             {expected:?}, got {body}"
        );
    }

    // Two filters are joined with an AND: both must hold for the range to be reported.
    let both = get_cidrs_ok(
        "two filters, joined with AND",
        &fixture.router,
        &cidr_params(
            &fixture.association_id,
            &[
                param("Filter.1.Name", "cidr"),
                param("Filter.1.Value.1", "198.51.100.0/26"),
                param("Filter.2.Name", "last-observed-at"),
                param("Filter.2.Value.1", "*"),
            ],
        ),
    )
    .await;
    assert_eq!(
        reported_cidrs(&both),
        vec!["198.51.100.0/26".to_string()],
        "filters are joined with an AND, got {both}"
    );
    let neither = get_cidrs_ok(
        "two filters that select different ranges",
        &fixture.router,
        &cidr_params(
            &fixture.association_id,
            &[
                param("Filter.1.Name", "cidr"),
                param("Filter.1.Value.1", "198.51.100.0/26"),
                param("Filter.2.Name", "cidr"),
                param("Filter.2.Value.1", "203.0.113.0/26"),
            ],
        ),
    )
    .await;
    assert!(
        reported_cidrs(&neither).is_empty(),
        "two filters that can never both hold report no range, got {neither}"
    );

    // A filter name this operation does not resolve is refused.
    let (status, body) = get_cidrs(
        &fixture.router,
        &cidr_params(
            &fixture.association_id,
            &[
                param("Filter.1.Name", "cidr-colour"),
                param("Filter.1.Value.1", "blue"),
            ],
        ),
    )
    .await;
    assert_eq!(
        status, 400,
        "an unresolvable filter name is refused, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "an unresolvable filter name is refused as an invalid value, got {body}"
    );
    assert_eq!(
        error_message(&body),
        "The filter 'cidr-colour' is invalid",
        "the refusal names the filter it could not resolve, got {body}"
    );
}

/// The rule: the report answers at most `MaxResults` CIDRs and a `NextToken` naming where the
/// next page starts, that token continues from the end of the page before it and is absent
/// once nothing follows, while a request naming no page size is not paged at all. A token this
/// service did not mint, or minted while paging ANOTHER association, is refused.
#[tokio::test]
async fn reporting_pages_in_a_stable_order() {
    let fixture = fixture().await;
    fixture.seed_rows();

    let held: Vec<String> = expected_cidrs();
    // The model's `IpamMaxResults` has a minimum of 5, so paging needs more than five reported
    // ranges for a second page to exist at all.
    assert!(
        held.len() > 5,
        "the rows must hold more than one page of reported ranges, got {held:?}"
    );

    for page_size in [5usize, 6, 7] {
        let case = format!("pages of {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut params = vec![param("MaxResults", &page_size.to_string())];
            if let Some(token) = token.as_deref() {
                params.push(param("NextToken", token));
            }
            let body = get_cidrs_ok(
                &case,
                &fixture.router,
                &cidr_params(&fixture.association_id, &params),
            )
            .await;
            let page = reported_cidrs(&body);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page holds at most {page_size} ranges, got {body}"
            );
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= 4,
                "case {case:?}: the paging walk must terminate, got {seen:?}"
            );
            match element(&body, "nextToken") {
                None => break,
                Some(next) => token = Some(next),
            }
        }
        assert_eq!(
            seen, held,
            "case {case:?}: the walk reports every range exactly once, in prefix order, got \
             {seen:?}"
        );
    }

    // A request that names no page size is not paged at all: every range in one answer and no
    // token to follow.
    let unpaged = get_cidrs_ok(
        "no page size asked for",
        &fixture.router,
        &cidr_params(&fixture.association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_cidrs(&unpaged),
        held,
        "a request naming no MaxResults reports every range, got {unpaged}"
    );
    assert_eq!(
        element(&unpaged, "nextToken"),
        None,
        "an unpaged answer carries no continuation token, got {unpaged}"
    );

    let (status, body) = get_cidrs(
        &fixture.router,
        &cidr_params(&fixture.association_id, &[param("NextToken", "not-a-token")]),
    )
    .await;
    assert_eq!(
        status, 400,
        "a token this service did not mint is refused, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidPaginationToken",
        "a bogus token is refused as an invalid pagination token, got {body}"
    );
    assert_eq!(
        error_message(&body),
        ec2::state::INVALID_PAGINATION_TOKEN_MESSAGE,
        "the refusal carries EC2's own pagination-token sentence, got {body}"
    );

    // A token names a position in the walk of ONE association, so presenting it to another
    // association is refused rather than resumed inside that association's own page.
    let first = get_cidrs_ok(
        "the first page of one association",
        &fixture.router,
        &cidr_params(&fixture.association_id, &[param("MaxResults", "5")]),
    )
    .await;
    let token = element(&first, "nextToken")
        .unwrap_or_else(|| panic!("a first page of five of {} ranges", held.len()));
    let (status, body) = get_cidrs(
        &fixture.router,
        &cidr_params(&fixture.other_id, &[param("NextToken", &token)]),
    )
    .await;
    assert_eq!(
        status, 400,
        "a token minted for another association is refused, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidPaginationToken",
        "a token from another association's walk is refused, got {body}"
    );
}

/// The rule: an id outside the form this service ISSUES is refused with this resource's
/// `.Malformed` before any lookup, an id of that form the account holds no association for
/// with its `.NotFound`, and `DryRun` answers the permission question instead of reporting.
#[tokio::test]
async fn reporting_refuses_an_id_it_cannot_resolve() {
    let fixture = fixture().await;
    fixture.seed_rows();

    // (case, the id the request names, the code EC2 answers)
    let refusals: Vec<(&str, &str, &str)> = vec![
        (
            "a suffix of the long form under a prefix this API does not issue",
            "ipam-ira-0000000000000000f",
            "InvalidIpamInternetRegistryAssociationId.Malformed",
        ),
        (
            "an id of another resource kind",
            "ipam-pool-0000000000000000f",
            "InvalidIpamInternetRegistryAssociationId.Malformed",
        ),
        (
            "a suffix that is not hexadecimal",
            "ipam-ir-assoc-0000000000000000g",
            "InvalidIpamInternetRegistryAssociationId.Malformed",
        ),
        (
            "an uppercase suffix",
            "ipam-ir-assoc-0000000000000000F",
            "InvalidIpamInternetRegistryAssociationId.Malformed",
        ),
        (
            "a suffix of neither published length",
            "ipam-ir-assoc-000000000",
            "InvalidIpamInternetRegistryAssociationId.Malformed",
        ),
        (
            "not an id at all",
            "my-association",
            "InvalidIpamInternetRegistryAssociationId.Malformed",
        ),
        (
            "a well-formed long-form id the account holds no association for",
            "ipam-ir-assoc-0000000000000000f",
            "InvalidIpamInternetRegistryAssociationId.NotFound",
        ),
        (
            "a well-formed short-form id the account holds no association for",
            "ipam-ir-assoc-0000000f",
            "InvalidIpamInternetRegistryAssociationId.NotFound",
        ),
    ];

    for (case, requested_id, expected_code) in refusals {
        let (status, body) = get_cidrs(&fixture.router, &cidr_params(requested_id, &[])).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the code {expected_code:?}, got body {body}"
        );
        let expected_message = if expected_code.ends_with(".Malformed") {
            format!(
                "The {RESOURCE_TYPE_IPAM_INTERNET_REGISTRY_ASSOCIATION} ID \
                 '{requested_id}' is malformed"
            )
        } else {
            format!(
                "The {RESOURCE_TYPE_IPAM_INTERNET_REGISTRY_ASSOCIATION} ID \
                 '{requested_id}' does not exist"
            )
        };
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case:?}: expected the refusal to name the id it could not resolve, got {body}"
        );
    }

    // "Checks whether you have the required permissions for the operation, without actually
    // making the request": every caller of this emulator is permitted.
    let (status, body) = get_cidrs(
        &fixture.router,
        &cidr_params(&fixture.association_id, &[param("DryRun", "true")]),
    )
    .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "a dry run answers the permission question, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "a permitted caller's dry run is DryRunOperation, got {body}"
    );

    // The form of the id is checked BEFORE the permission question, so a dry run naming
    // something that is not an id of this kind is told so rather than answered DryRunOperation.
    let (status, body) = get_cidrs(
        &fixture.router,
        &cidr_params("my-association", &[param("DryRun", "true")]),
    )
    .await;
    assert_eq!(
        status, 400,
        "a malformed id is refused before the permission question, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidIpamInternetRegistryAssociationId.Malformed",
        "a malformed id is refused as malformed even on a dry run, got {body}"
    );

    // The ranges the account holds are still reported, unchanged by any refusal above.
    let after = get_cidrs_ok(
        "after the refusals",
        &fixture.router,
        &cidr_params(&fixture.association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_cidrs(&after),
        expected_cidrs(),
        "a refused report changes nothing the account holds, got {after}"
    );
}
