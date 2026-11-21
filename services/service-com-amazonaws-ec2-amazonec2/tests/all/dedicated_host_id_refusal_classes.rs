// SPDX-License-Identifier: CC0-1.0

//! WHICH CHECK refused a Dedicated Host id, over the two operations that price a reservation
//! for a `HostIdSet` — `GetHostReservationPurchasePreview` and `PurchaseHostReservation`.
//!
//! The rule under test, for the PREVIEW: a host id this account was never issued — a value
//! outside the id form EC2 publishes, and a value in that form the account was never handed —
//! fails the preview's id validation and carries its malformed code; only an id the account
//! really held and has given back gets past validation to the existence answer, which carries
//! the published not-found code. The two say opposite things to a client ("that is not a host
//! id of yours" against "that host is gone"), so which one answers is decided by whether the
//! account was EVER ISSUED the id and never by the request's spelling alone.
//!
//! The purchase beside it is recorded splitting the same two classes at a different place and
//! spelling both codes differently, so every expectation here is per operation
//! ([`expected_code`]) and neither operation is the specification for the other.
//!
//! Driven over the wire the service speaks (`ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness), so what is asserted is what a client reads.
//!
//! The rows span what the rule generalises over — the id's LENGTH (both published digit
//! counts, one digit short of each, one digit long), its alphabet (uppercase hex, non-hex),
//! its prefix (another resource's, none at all), the all-zero placeholder no host can hold,
//! how many ids the set carries, and, for the ids EC2 could have issued, HOW the host came to
//! be absent (never allocated, or allocated by this run and released) — so no row is the
//! specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for.
pub(super) const REGION: &str = "us-east-1";

/// The zone the hosts this suite allocates sit in.
const ZONE: &str = "us-east-1a";

/// The instance family every host and offering here is of, so no row fails on a mismatch
/// between the two.
const FAMILY: &str = "m5";

/// A one-year term, in seconds: the term of the offering this suite prices with.
const ONE_YEAR_SECONDS: i32 = 365 * 24 * 60 * 60;

/// How an id the request named fails to name a live Dedicated Host of this account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unresolvable {
    /// Outside the id form EC2 issues: no account could ever have been handed it.
    OutsideForm,
    /// In one of the printed id forms, but a value this account was NEVER ISSUED — no host of
    /// it has ever carried the id.
    NeverIssued,
    /// An id this account really held and has given back, so the host it names is gone.
    Released,
}

/// The full code `action` answers for an id that is unresolvable in way `kind`, as each
/// operation's recorded refusals spell it.
///
/// The preview answers its own `InvalidHostId.Malformed` for every id the account was never
/// issued, whatever its spelling, and the code EC2 publishes for an absent host
/// ([`ec2::state::INVALID_HOST_ID_NOT_FOUND`]) for a host the account released; the purchase is
/// recorded answering `InvalidHostID.Malformed` for a value outside the id form and
/// `InvalidHostId.NotFound` for an id EC2 could have issued.
fn expected_code(action: &str, kind: Unresolvable) -> &'static str {
    const PREVIEW: &str = "GetHostReservationPurchasePreview";
    match (action, kind) {
        (PREVIEW, Unresolvable::Released) => ec2::state::INVALID_HOST_ID_NOT_FOUND,
        (PREVIEW, _) => "InvalidHostId.Malformed",
        (_, Unresolvable::OutsideForm) => ec2::state::INVALID_HOST_ID_MALFORMED,
        (_, _) => "InvalidHostId.NotFound",
    }
}

/// Allocate `quantity` Dedicated Hosts of [`FAMILY`], answering their ids.
pub(super) async fn allocate(router: &MultiServiceRouter, quantity: usize) -> Vec<String> {
    let params = vec![
        param("AvailabilityZone", ZONE),
        param("InstanceFamily", FAMILY),
        param("Quantity", &quantity.to_string()),
    ];
    let (status, body) = call(router, REGION, "AllocateHosts", &params).await;
    assert_eq!(
        status, 200,
        "AllocateHosts must succeed, got {status} with body {body}"
    );
    let ids = super::ec2_query::elements(&body, "item");
    assert_eq!(
        ids.len(),
        quantity,
        "AllocateHosts must report the {quantity} host(s) it allocated: {body}"
    );
    ids
}

/// Give `host_id` back to EC2, so its id names no live host of the account any more.
pub(super) async fn release(router: &MultiServiceRouter, host_id: &str) {
    let (status, body) = call(router, REGION, "ReleaseHosts", &indexed("HostId", &[host_id])).await;
    assert_eq!(
        status, 200,
        "ReleaseHosts must succeed, got {status} with body {body}"
    );
}

/// The id of a published offering for [`FAMILY`] — read out of the catalogue the describe
/// publishes, so this suite names no offering id of its own.
pub(super) async fn published_offering(router: &MultiServiceRouter) -> String {
    let mut params = vec![
        param("MinDuration", &ONE_YEAR_SECONDS.to_string()),
        param("MaxDuration", &ONE_YEAR_SECONDS.to_string()),
    ];
    params.extend(super::ec2_query::filters(&[
        ("instance-family", &[FAMILY]),
        ("payment-option", &["AllUpfront"]),
    ]));
    let (status, body) = call(router, REGION, "DescribeHostReservationOfferings", &params).await;
    assert_eq!(
        status, 200,
        "DescribeHostReservationOfferings must succeed, got {status} with body {body}"
    );
    super::ec2_query::elements(&body, "offeringId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the catalogue must publish an offering for {FAMILY}: {body}"))
}

/// One call of `action` naming `host_ids` and `offering_id` — the two members both the preview
/// and the purchase take the `HostIdSet` and the offering through.
pub(super) async fn priced(
    router: &MultiServiceRouter,
    action: &str,
    host_ids: &[&str],
    offering_id: &str,
) -> (u16, String) {
    let mut params: Vec<Param> = indexed("HostIdSet", host_ids);
    params.push(param("OfferingId", offering_id));
    call(router, REGION, action, &params).await
}

/// The sentence EC2 refuses a SET of Dedicated Host ids it cannot resolve with, over the ids
/// the REQUEST carried — the one wording both halves of the preview's refusal use.
pub(super) fn not_valid_sentence(host_ids: &[&str]) -> String {
    let quoted: Vec<String> = host_ids.iter().map(|id| format!("'{id}'")).collect();
    format!(
        "The specified Dedicated host IDs [{ids}] are not valid.",
        ids = quoted.join(", ")
    )
}

/// The id cases the rule generalises over, as `(case, ids, how it is unresolvable)`.
///
/// `held` is a host the account holds and `released` one it has given back, so a row can put
/// an unresolvable id beside a resolvable one.
fn rows<'a>(held: &'a str, released: &'a str) -> Vec<(&'static str, Vec<&'a str>, Unresolvable)> {
    vec![
        // In a published form, so EC2 could have issued them — but this account never was:
        // nothing of it has ever carried these ids.
        (
            "the long id form, never issued",
            vec!["h-0123456789abcdef0"],
            Unresolvable::NeverIssued,
        ),
        (
            "the short id form, never issued",
            vec!["h-89abcdef"],
            Unresolvable::NeverIssued,
        ),
        (
            "every digit at its maximum",
            vec!["h-fffffffffffffffff"],
            Unresolvable::NeverIssued,
        ),
        (
            "a held host beside one never issued",
            vec![held, "h-0fedcba987654321f"],
            Unresolvable::NeverIssued,
        ),
        // Ids this account really held and gave back: the host is gone rather than the id
        // being one of someone else's.
        (
            "a host the account released",
            vec![released],
            Unresolvable::Released,
        ),
        (
            "a released host beside a held one",
            vec![held, released],
            Unresolvable::Released,
        ),
        // Outside the form: no digit count EC2 publishes, an alphabet it does not issue, no
        // host prefix at all, or the all-zero placeholder the mint counting from one can never
        // produce — none of these could name a host in any account.
        (
            "a digit short of the short form",
            vec!["h-1234567"],
            Unresolvable::OutsideForm,
        ),
        (
            "a digit past the short form",
            vec!["h-123456789"],
            Unresolvable::OutsideForm,
        ),
        (
            "a digit short of the long form",
            vec!["h-0123456789abcdef"],
            Unresolvable::OutsideForm,
        ),
        (
            "a digit past the long form",
            vec!["h-0123456789abcdef01"],
            Unresolvable::OutsideForm,
        ),
        (
            "uppercase hexadecimal digits",
            vec!["h-0123456789ABCDEF0"],
            Unresolvable::OutsideForm,
        ),
        (
            "digits outside hexadecimal",
            vec!["h-0123456789abcdefg"],
            Unresolvable::OutsideForm,
        ),
        (
            "another resource's prefix",
            vec!["i-0123456789abcdef0"],
            Unresolvable::OutsideForm,
        ),
        (
            "no prefix at all",
            vec!["0123456789abcdef0"],
            Unresolvable::OutsideForm,
        ),
        (
            "the all-zero placeholder",
            vec!["h-00000000000000000"],
            Unresolvable::OutsideForm,
        ),
        (
            "the all-zero placeholder, short form",
            vec!["h-00000000"],
            Unresolvable::OutsideForm,
        ),
        (
            "not an id at all",
            vec!["not-a-host"],
            Unresolvable::OutsideForm,
        ),
        (
            "a malformed id beside a held host",
            vec![held, "not-a-host"],
            Unresolvable::OutsideForm,
        ),
    ]
}

/// Validation and the existence check answer DIFFERENT codes, over every way an id can fail to
/// name a live host and over both operations that price a reservation for a set of hosts.
#[tokio::test]
async fn validation_and_the_existence_check_answer_different_codes() {
    let (router, _engine) = super::ec2_query::emulator();
    let held = allocate(&router, 1).await.remove(0);
    let released = allocate(&router, 1).await.remove(0);
    release(&router, &released).await;
    let offering_id = published_offering(&router).await;

    for action in ["GetHostReservationPurchasePreview", "PurchaseHostReservation"] {
        for (case, host_ids, kind) in rows(&held, &released) {
            let (status, body) = priced(&router, action, &host_ids, &offering_id).await;
            assert_eq!(
                status, 400,
                "{action} case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            let code = error_code(&body);
            let expected = expected_code(action, kind);
            assert_eq!(
                code, expected,
                "{action} case {case:?} ({kind:?}): expected {expected:?}, got {code:?} in {body}"
            );
            // Whichever check refused it, the code names the Dedicated Host id as the thing
            // it could not accept, so a client can tell this refusal from one about the
            // offering or the request's shape.
            assert!(
                code.to_ascii_lowercase().starts_with("invalidhostid"),
                "{action} case {case:?}: expected a Dedicated-Host-id code, got {code:?} in {body}"
            );
        }
    }
}

/// The preview's refusal names the SET the request asked a price for, in both its halves: the
/// preview prices the whole `HostIdSet`, so it is that set that is reported not valid, in the
/// order and the spelling the request carried.
#[tokio::test]
async fn the_preview_names_the_requested_host_id_set() {
    let (router, _engine) = super::ec2_query::emulator();
    let held = allocate(&router, 1).await.remove(0);
    let released = allocate(&router, 1).await.remove(0);
    release(&router, &released).await;
    let offering_id = published_offering(&router).await;

    for (case, host_ids, _kind) in rows(&held, &released) {
        let (status, body) = priced(
            &router,
            "GetHostReservationPurchasePreview",
            &host_ids,
            &offering_id,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        let expected = not_valid_sentence(&host_ids);
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected {expected:?}, got {:?} in {body}",
            error_message(&body)
        );
    }
}

/// An id the account DOES hold is not refused at all: validation is a check and not a blanket
/// refusal, so a preview naming only held hosts is served.
#[tokio::test]
async fn a_held_host_is_still_priced() {
    let (router, _engine) = super::ec2_query::emulator();
    let hosts = allocate(&router, 2).await;
    let offering_id = published_offering(&router).await;

    // Rows differ in how many held hosts the reservation would cover, so what is pinned is
    // that resolvable ids pass the check rather than one particular set of them.
    let rows: [(&str, Vec<&str>); 3] = [
        ("one held host", vec![hosts[0].as_str()]),
        ("the other held host", vec![hosts[1].as_str()]),
        ("both held hosts", vec![hosts[0].as_str(), hosts[1].as_str()]),
    ];

    for (case, host_ids) in rows {
        let (status, body) = priced(
            &router,
            "GetHostReservationPurchasePreview",
            &host_ids,
            &offering_id,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the preview to be served, got {status} with body {body}"
        );
    }
}
