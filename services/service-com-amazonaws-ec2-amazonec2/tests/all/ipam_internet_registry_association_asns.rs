// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetIpamInternetRegistryAssociationAsns`, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * The report lists the Autonomous System Numbers the IPAM behind the association holds at
//!   an internet registry: the ASNs the account has brought to THAT IPAM (`Byoasn$IpamId`
//!   names it) in a state where the authorization at the registry has been verified and the
//!   account has not finished giving the ASN back — so an ASN of another IPAM, and one whose
//!   registry authorization was never confirmed or has already been given back, are not
//!   reported, and two associations of the same IPAM report the same ASNs.
//! * The report is paginated like every other paged EC2 read: at most `MaxResults` ASNs in a
//!   stable ASN order with a `NextToken` naming where the next page starts, every ASN in one
//!   answer with no token when the request names no page size ("if not specified, all
//!   available results are returned"), narrowed by each filter the operation resolves — one
//!   per member of the reported shape, spelled in kebab-case, values joined with OR and
//!   filters with AND — and it changes nothing a later caller sees.
//! * An id outside the form this service ISSUES for an internet registry association is
//!   refused with that resource's `.Malformed` before any lookup; an id of that form the
//!   account holds no association for with its `.NotFound`; a filter name or a page token the
//!   operation will not serve is refused rather than answered as "no ASNs"; and `DryRun`
//!   answers the permission question and reads nothing.
//!
//! Each table row is a distinct input along what those rules generalise over: the width of
//! the ASN, which `AsnState` it sits in, which IPAM it was brought to, which member a filter
//! selects on and how large a page is asked for.
//!
//! The ASNs used are from the blocks RFC 5398 sets aside for documentation (64496-64511 for
//! 2-byte ASNs, 65536-65551 for 4-byte ASNs), so no row names an ASN that belongs to
//! anybody.
//!
//! The store is seeded directly, the way `ipam_byoasn_associations` seeds its own: these rows
//! need an ASN sitting in a chosen `AsnState`, and `ProvisionIpamByoasn` — the operation that
//! brings an ASN to IPAM — is still a scaffold, so the API has no path to one.

use std::collections::BTreeSet;

use ec2::ops::types::{ASN_STATE_VALUES, IpamInternetRegistryAssociation};
use ec2::state::{
    ByoasnRecord, ComAmazonawsEc2Amazonec2State as Ec2State,
    IPAM_INTERNET_REGISTRY_ASSOCIATION_ID_PREFIX, RESOURCE_TYPE_IPAM_INTERNET_REGISTRY_ASSOCIATION,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, element, elements, emulator, error_code, error_message};

/// The operation under test.
const ACTION: &str = "GetIpamInternetRegistryAssociationAsns";

/// The Region every call here is signed for. An association reports the Region of the IPAM it
/// belongs to, so the Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The IPAM the association under test belongs to.
const IPAM_ID: &str = "ipam-00000000000000001";

/// A second IPAM of the same account, whose ASNs the first association must NOT report.
const OTHER_IPAM_ID: &str = "ipam-00000000000000002";

/// The Regional Internet Registry a seeded association is with: the modelled `Rir` catalog's
/// `arin` member.
const RIR_ARIN: &str = "arin";

/// The `IpamInternetRegistryAssociationState` catalog's `enable-complete` member: an
/// association in service.
const STATE_ENABLE_COMPLETE: &str = "enable-complete";

/// The `AsnState` of an ASN IPAM has finished bringing to the account. Spelled out here
/// rather than imported, so the service cannot make this suite pass by agreeing with itself.
const PROVISIONED: &str = "provisioned";

/// The `AsnState` of an ASN the account has asked back and AWS has not finished giving up.
const PENDING_DEPROVISION: &str = "pending-deprovision";

/// The `AsnState` of an ASN whose release did not complete, so the ASN is still the
/// account's.
const FAILED_DEPROVISION: &str = "failed-deprovision";

/// The `AsnState` of an ASN still being brought to IPAM.
const PENDING_PROVISION: &str = "pending-provision";

/// The `AsnState` of an ASN whose provision did not complete.
const FAILED_PROVISION: &str = "failed-provision";

/// The `AsnState` of an ASN the account has finished giving back.
const DEPROVISIONED: &str = "deprovisioned";

/// One form field of an ec2Query request body.
type Param = (String, String);

/// One `(name, value)` form field.
fn param(name: &str, value: &str) -> Param {
    (name.to_string(), value.to_string())
}

/// One call of the operation under test, as its HTTP status and XML body.
async fn get_asns(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    call(router, REGION, ACTION, params).await
}

/// One call required to succeed, and the body it answered.
async fn get_asns_ok(case: &str, router: &MultiServiceRouter, params: &[Param]) -> String {
    let (status, body) = get_asns(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
    );
    body
}

/// The request fields that ask for the ASNs of `association_id`, plus `extra` form fields.
fn asn_params(association_id: &str, extra: &[Param]) -> Vec<Param> {
    let mut params = vec![param("IpamInternetRegistryAssociationId", association_id)];
    params.extend(extra.iter().cloned());
    params
}

/// The ASNs a body reports, in the order it reported them.
fn reported_asns(body: &str) -> Vec<String> {
    elements(body, "asn")
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

/// Seed one ASN the account brought to `ipam_id`, sitting in `asn_state`.
fn seed_byoasn(engine: &engine::InMemoryEngine, asn: &str, ipam_id: &str, asn_state: &str) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    assert!(
        guard.insert_byoasn(ByoasnRecord::in_state(asn, ipam_id, asn_state)),
        "the store holds the seeded ASN {asn} of {ipam_id} in state {asn_state}"
    );
}

/// How many ASNs the store holds right now.
fn stored_asn_count(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().byoasns.len()
}

/// One ASN the account brought to the IPAM under test: a distinct input along the width of
/// the ASN and the `AsnState` it sits in.
struct Row {
    /// What this row is called in a failure message.
    case: &'static str,
    /// The ASN itself, as the API spells one (`Byoasn$Asn` is a `String`).
    asn: &'static str,
    /// The `AsnState` the account's record of it sits in.
    asn_state: &'static str,
    /// Whether the internet registry still records the ASN as the account's, so the report
    /// carries it.
    reported: bool,
}

/// One row per member of the modelled `AsnState` catalog, over both ASN widths.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a 2-byte asn ipam finished bringing to the account",
            asn: "64496",
            asn_state: PROVISIONED,
            reported: true,
        },
        Row {
            case: "a 4-byte asn ipam finished bringing to the account",
            asn: "65536",
            asn_state: PROVISIONED,
            reported: true,
        },
        Row {
            case: "an asn the account asked back and aws has not finished giving up",
            asn: "64497",
            asn_state: PENDING_DEPROVISION,
            reported: true,
        },
        Row {
            case: "an asn whose release did not complete, so it is still the account's",
            asn: "64498",
            asn_state: FAILED_DEPROVISION,
            reported: true,
        },
        Row {
            case: "an asn whose registry authorization is still being checked",
            asn: "64499",
            asn_state: PENDING_PROVISION,
            reported: false,
        },
        Row {
            case: "an asn whose provision did not complete",
            asn: "64500",
            asn_state: FAILED_PROVISION,
            reported: false,
        },
        Row {
            case: "an asn the account has finished giving back",
            asn: "64501",
            asn_state: DEPROVISIONED,
            reported: false,
        },
    ]
}

/// The ASNs of [`rows`] the report must carry, in the ASN order it pages in.
fn expected_asns() -> Vec<String> {
    let mut expected: Vec<String> = rows()
        .into_iter()
        .filter(|row| row.reported)
        .map(|row| row.asn.to_string())
        .collect();
    expected.sort();
    expected
}

/// Seed every row of [`rows`] under [`IPAM_ID`], and one ASN of another IPAM beside them.
fn seed_rows(engine: &engine::InMemoryEngine) {
    for row in rows() {
        seed_byoasn(engine, row.asn, IPAM_ID, row.asn_state);
    }
    seed_byoasn(engine, OTHER_ASN, OTHER_IPAM_ID, PROVISIONED);
}

/// The ASN the OTHER IPAM holds, which no association of [`IPAM_ID`] may report.
const OTHER_ASN: &str = "65537";

/// The state words this suite asserts on are the model's own: each is a member of the catalog
/// the model publishes for `AsnState`, and the suite covers every member of it, so no row
/// pins a word the API does not have and no member is left untested.
#[test]
fn the_asserted_asn_state_words_are_the_models_own() {
    for row in rows() {
        assert!(
            ASN_STATE_VALUES.contains(&row.asn_state),
            "case {case:?}: expected {state:?} to be a member of the modelled AsnState \
             catalog, which carries {ASN_STATE_VALUES:?}",
            case = row.case,
            state = row.asn_state
        );
    }
    for value in ASN_STATE_VALUES {
        assert!(
            rows().iter().any(|row| row.asn_state == *value),
            "expected a row for the AsnState {value:?}, which the modelled catalog carries"
        );
    }
}

/// The rule: the report lists the ASNs the association's own IPAM holds at the registry —
/// those whose authorization there was verified and not given back — and nothing else, with
/// an observation time on each, and reporting changes nothing a later caller sees.
#[tokio::test]
async fn reporting_lists_the_asns_the_associations_ipam_holds_at_the_registry() {
    let (router, engine) = emulator();
    let association_id = seed_association(&engine, IPAM_ID);
    let sibling_id = seed_association(&engine, IPAM_ID);
    let other_id = seed_association(&engine, OTHER_IPAM_ID);

    // An IPAM that has brought no ASN to IPAM reports the empty set, not a refusal: there is
    // nothing to report and nothing has gone wrong.
    let empty = get_asns_ok(
        "an ipam holding no asn",
        &router,
        &asn_params(&association_id, &[]),
    )
    .await;
    assert!(
        reported_asns(&empty).is_empty(),
        "an ipam that holds no asn reports none, got {empty}"
    );
    assert_eq!(
        element(&empty, "nextToken"),
        None,
        "a single short page carries no continuation token, got {empty}"
    );

    seed_rows(&engine);

    let reported = get_asns_ok(
        "every asn of the ipam",
        &router,
        &asn_params(&association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_asns(&reported),
        expected_asns(),
        "the report carries exactly the asns the registry still records for this ipam, in asn \
         order, got {reported}"
    );
    for row in rows().into_iter().filter(|row| !row.reported) {
        assert!(
            !reported_asns(&reported).contains(&row.asn.to_string()),
            "case {case:?}: an asn in state {state:?} is not one the registry records, got \
             {reported}",
            case = row.case,
            state = row.asn_state
        );
    }
    assert!(
        !reported_asns(&reported).contains(&OTHER_ASN.to_string()),
        "an asn of another ipam is not reported for this association, got {reported}"
    );

    // Every reported ASN carries "the time when the ASN was last observed at the internet
    // registry": one observation time per ASN of the answer, each a timestamp a client can
    // read.
    let observed = elements(&reported, "lastObservedAt");
    assert_eq!(
        observed.len(),
        expected_asns().len(),
        "every reported asn carries its observation time, got {reported}"
    );
    for value in &observed {
        assert!(
            as_timestamp(value).is_some(),
            "the observation time {value:?} must be a timestamp, got {reported}"
        );
    }

    // The report is of the ASSOCIATION's IPAM: another association of the same IPAM reports
    // the same ASNs, and an association of a different IPAM reports that IPAM's.
    let sibling = get_asns_ok(
        "a second association of the same ipam",
        &router,
        &asn_params(&sibling_id, &[]),
    )
    .await;
    assert_eq!(
        reported_asns(&sibling),
        expected_asns(),
        "two associations of one ipam report the same asns, got {sibling}"
    );
    let other = get_asns_ok(
        "an association of another ipam",
        &router,
        &asn_params(&other_id, &[]),
    )
    .await;
    assert_eq!(
        reported_asns(&other),
        vec![OTHER_ASN.to_string()],
        "an association reports the asns of its own ipam, got {other}"
    );

    // Reading is not writing: the same answer twice, and nothing added to or removed from the
    // store.
    let again = get_asns_ok(
        "reading the asns twice",
        &router,
        &asn_params(&association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_asns(&again),
        reported_asns(&reported),
        "reading the asns twice must report the same ones, got {again}"
    );
    assert_eq!(
        stored_asn_count(&engine),
        rows().len() + 1,
        "a report stores no asn and removes none"
    );
}

/// The rule: every filter the operation resolves selects on one member of the reported shape,
/// its values joined with OR and its filters with AND, and a filter name it cannot resolve is
/// refused rather than answered as "no ASNs".
///
/// The rows span the member catalog — the ASN itself (exactly, as two values, and with a
/// wildcard) and the observation time — plus values that select nothing, including an ASN the
/// IPAM holds in a state the registry no longer records, so a filter cannot widen the report
/// past the rule.
#[tokio::test]
async fn reporting_selects_by_every_filter_it_resolves() {
    let (router, engine) = emulator();
    let association_id = seed_association(&engine, IPAM_ID);
    seed_rows(&engine);

    let every: BTreeSet<String> = expected_asns().into_iter().collect();

    // (case, filter name, values, the asns that match)
    let cases: Vec<(&str, &str, Vec<&str>, BTreeSet<String>)> = vec![
        (
            "by one asn exactly",
            "asn",
            vec!["64497"],
            ["64497".to_string()].into_iter().collect(),
        ),
        (
            "by two asns, joined with OR",
            "asn",
            vec!["64496", "65536"],
            ["64496".to_string(), "65536".to_string()]
                .into_iter()
                .collect(),
        ),
        (
            "by an asn wildcard",
            "asn",
            vec!["644*"],
            ["64496".to_string(), "64497".to_string(), "64498".to_string()]
                .into_iter()
                .collect(),
        ),
        (
            "by an asn no ipam of this account holds",
            "asn",
            vec!["64510"],
            BTreeSet::new(),
        ),
        (
            "by an asn the ipam holds in a state the registry no longer records",
            "asn",
            vec![DEPROVISIONED_ROW_ASN],
            BTreeSet::new(),
        ),
        (
            "by an asn of another ipam",
            "asn",
            vec![OTHER_ASN],
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
        let body = get_asns_ok(case, &router, &asn_params(&association_id, &params)).await;
        assert_eq!(
            reported_asns(&body).into_iter().collect::<BTreeSet<String>>(),
            expected,
            "case {case:?}: expected the filter {name:?} over {values:?} to select \
             {expected:?}, got {body}"
        );
    }

    // Two filters are joined with an AND: both must hold for the ASN to be reported.
    let both = get_asns_ok(
        "two filters, joined with AND",
        &router,
        &asn_params(
            &association_id,
            &[
                param("Filter.1.Name", "asn"),
                param("Filter.1.Value.1", "64496"),
                param("Filter.2.Name", "last-observed-at"),
                param("Filter.2.Value.1", "*"),
            ],
        ),
    )
    .await;
    assert_eq!(
        reported_asns(&both),
        vec!["64496".to_string()],
        "filters are joined with an AND, got {both}"
    );
    let neither = get_asns_ok(
        "two filters that select different asns",
        &router,
        &asn_params(
            &association_id,
            &[
                param("Filter.1.Name", "asn"),
                param("Filter.1.Value.1", "64496"),
                param("Filter.2.Name", "asn"),
                param("Filter.2.Value.1", "64497"),
            ],
        ),
    )
    .await;
    assert!(
        reported_asns(&neither).is_empty(),
        "two filters that can never both hold report no asn, got {neither}"
    );

    // A filter name this operation does not resolve is refused.
    let (status, body) = get_asns(
        &router,
        &asn_params(
            &association_id,
            &[
                param("Filter.1.Name", "asn-colour"),
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
        "The filter 'asn-colour' is invalid",
        "the refusal names the filter it could not resolve, got {body}"
    );
}

/// The ASN of the row the account has finished giving back, which no filter may bring into
/// the report.
const DEPROVISIONED_ROW_ASN: &str = "64501";

/// The rule: the report answers at most `MaxResults` ASNs and a `NextToken` naming where the
/// next page starts, that token continues from the end of the page before it and is absent
/// once nothing follows, while a request naming no page size is not paged at all. A token
/// this service did not mint, or minted while paging ANOTHER association, is refused.
#[tokio::test]
async fn reporting_pages_in_a_stable_order() {
    let (router, engine) = emulator();
    let association_id = seed_association(&engine, IPAM_ID);
    let other_id = seed_association(&engine, OTHER_IPAM_ID);

    // The model's `IpamMaxResults` has a minimum of 5, so paging needs more than five ASNs
    // for a second page to exist at all.
    let mut held: BTreeSet<String> = BTreeSet::new();
    for offset in 0..7u32 {
        let asn = (64496 + offset).to_string();
        seed_byoasn(&engine, &asn, IPAM_ID, PROVISIONED);
        held.insert(asn);
    }

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
            let body = get_asns_ok(&case, &router, &asn_params(&association_id, &params)).await;
            let page = reported_asns(&body);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page holds at most {page_size} asns, got {body}"
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
        let walked: Vec<String> = {
            let mut sorted = seen.clone();
            sorted.sort();
            sorted
        };
        assert_eq!(
            walked,
            held.iter().cloned().collect::<Vec<String>>(),
            "case {case:?}: the walk reports every asn exactly once, got {seen:?}"
        );
        assert_eq!(
            seen,
            walked,
            "case {case:?}: the walk reports the asns in asn order, got {seen:?}"
        );
    }

    // A request that names no page size is not paged at all: every ASN in one answer and no
    // token to follow.
    let unpaged = get_asns_ok(
        "no page size asked for",
        &router,
        &asn_params(&association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_asns(&unpaged).into_iter().collect::<BTreeSet<String>>(),
        held,
        "a request naming no MaxResults reports every asn, got {unpaged}"
    );
    assert_eq!(
        element(&unpaged, "nextToken"),
        None,
        "an unpaged answer carries no continuation token, got {unpaged}"
    );

    let (status, body) = get_asns(
        &router,
        &asn_params(&association_id, &[param("NextToken", "not-a-token")]),
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
    let first = get_asns_ok(
        "the first page of one association",
        &router,
        &asn_params(&association_id, &[param("MaxResults", "5")]),
    )
    .await;
    let token = element(&first, "nextToken").expect("a first page of five of seven asns");
    let (status, body) = get_asns(
        &router,
        &asn_params(&other_id, &[param("NextToken", &token)]),
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
    let (router, engine) = emulator();
    let association_id = seed_association(&engine, IPAM_ID);
    seed_rows(&engine);

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
        let (status, body) = get_asns(&router, &asn_params(requested_id, &[])).await;
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
    let (status, body) = get_asns(
        &router,
        &asn_params(&association_id, &[param("DryRun", "true")]),
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
    let (status, body) = get_asns(
        &router,
        &asn_params("my-association", &[param("DryRun", "true")]),
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

    // The ASNs the account holds are still reported, unchanged by any refusal above.
    let after = get_asns_ok(
        "after the refusals",
        &router,
        &asn_params(&association_id, &[]),
    )
    .await;
    assert_eq!(
        reported_asns(&after),
        expected_asns(),
        "a refused report changes nothing the account holds, got {after}"
    );
}
