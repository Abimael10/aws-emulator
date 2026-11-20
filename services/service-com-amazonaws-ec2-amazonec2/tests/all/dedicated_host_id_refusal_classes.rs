// SPDX-License-Identifier: CC0-1.0

//! WHICH CHECK refused a Dedicated Host id, over the two operations that price a reservation
//! for a `HostIdSet` — `GetHostReservationPurchasePreview` and `PurchaseHostReservation`.
//!
//! The rule under test: a value outside the id form EC2 issues for a resource is refused by
//! FORM validation, and carries that resource's malformed code; an id EC2 could have issued
//! reaches the EXISTENCE check, and an id that resolves to no live resource — one the account
//! never held, or one it has given back — carries that resource's not-found code. The two say
//! opposite things to a client ("your input is syntactically wrong" against "that host is
//! gone"), so which one answers is decided by the id's FORM and never by whether this run
//! happened to issue it.
//!
//! Driven over the wire the service speaks (`ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness), so what is asserted is what a client reads.
//!
//! The rows span what the rule generalises over — the id's LENGTH (both published digit
//! counts, one digit short of each, one digit long), its alphabet (uppercase hex, non-hex),
//! its prefix (another resource's, none at all), the all-zero placeholder no host can hold,
//! and, for the well-formed half, HOW the host came to be absent (never allocated, or
//! allocated and released) — and the same table is driven through both operations, so no row
//! and no operation is the specification. The two operations' codes are spelled as each was
//! recorded, which is why the CLASS (the code's `.Malformed` / `.NotFound` half) is what the
//! shared table asserts, with the preview's full codes pinned beside it.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The zone the hosts this suite allocates sit in.
const ZONE: &str = "us-east-1a";

/// The instance family every host and offering here is of, so no row fails on a mismatch
/// between the two.
const FAMILY: &str = "m5";

/// A one-year term, in seconds: the term of the offering this suite prices with.
const ONE_YEAR_SECONDS: i32 = 365 * 24 * 60 * 60;

/// Which check refused an id, as the code says: the half after the code's last `.`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefusedBy {
    /// The id's FORM: EC2 could never have issued it, so there was nothing to look up.
    Form,
    /// The account's HOSTS: the id is one EC2 could have issued, and it names no live host.
    Existence,
}

impl RefusedBy {
    /// The suffix an EC2 refusal of this class carries on its code.
    fn code_suffix(self) -> &'static str {
        match self {
            Self::Form => "Malformed",
            Self::Existence => "NotFound",
        }
    }
}

/// Allocate `quantity` Dedicated Hosts of [`FAMILY`], answering their ids.
async fn allocate(router: &MultiServiceRouter, quantity: usize) -> Vec<String> {
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
async fn release(router: &MultiServiceRouter, host_id: &str) {
    let (status, body) = call(router, REGION, "ReleaseHosts", &indexed("HostId", &[host_id])).await;
    assert_eq!(
        status, 200,
        "ReleaseHosts must succeed, got {status} with body {body}"
    );
}

/// The id of a published offering for [`FAMILY`] — read out of the catalogue the describe
/// publishes, so this suite names no offering id of its own.
async fn published_offering(router: &MultiServiceRouter) -> String {
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
async fn priced(
    router: &MultiServiceRouter,
    action: &str,
    host_ids: &[&str],
    offering_id: &str,
) -> (u16, String) {
    let mut params: Vec<Param> = indexed("HostIdSet", host_ids);
    params.push(param("OfferingId", offering_id));
    call(router, REGION, action, &params).await
}

/// The id cases the rule generalises over, as `(case, ids, refused by)`.
///
/// `held` is a host the account holds and `released` one it has given back, so a row can put
/// an unresolvable id beside a resolvable one.
fn rows<'a>(held: &'a str, released: &'a str) -> Vec<(&'static str, Vec<&'a str>, RefusedBy)> {
    vec![
        // Well formed: `h-` and either published digit count, all lowercase hex — ids EC2
        // could have issued, so each reaches the existence check.
        (
            "the long id form, nothing holds",
            vec!["h-0123456789abcdef0"],
            RefusedBy::Existence,
        ),
        (
            "the short id form, nothing holds",
            vec!["h-89abcdef"],
            RefusedBy::Existence,
        ),
        (
            "every digit at its maximum",
            vec!["h-fffffffffffffffff"],
            RefusedBy::Existence,
        ),
        (
            "a host the account released",
            vec![released],
            RefusedBy::Existence,
        ),
        (
            "a released host beside a held one",
            vec![held, released],
            RefusedBy::Existence,
        ),
        (
            "a held host beside one nothing holds",
            vec![held, "h-0fedcba987654321f"],
            RefusedBy::Existence,
        ),
        // Outside the form: no digit count EC2 publishes, an alphabet it does not issue, no
        // host prefix at all, or the all-zero placeholder the mint counting from one can never
        // produce — none of these could name a host in any account.
        (
            "a digit short of the short form",
            vec!["h-1234567"],
            RefusedBy::Form,
        ),
        (
            "a digit past the short form",
            vec!["h-123456789"],
            RefusedBy::Form,
        ),
        (
            "a digit short of the long form",
            vec!["h-0123456789abcdef"],
            RefusedBy::Form,
        ),
        (
            "a digit past the long form",
            vec!["h-0123456789abcdef01"],
            RefusedBy::Form,
        ),
        (
            "uppercase hexadecimal digits",
            vec!["h-0123456789ABCDEF0"],
            RefusedBy::Form,
        ),
        (
            "digits outside hexadecimal",
            vec!["h-0123456789abcdefg"],
            RefusedBy::Form,
        ),
        (
            "another resource's prefix",
            vec!["i-0123456789abcdef0"],
            RefusedBy::Form,
        ),
        ("no prefix at all", vec!["0123456789abcdef0"], RefusedBy::Form),
        (
            "the all-zero placeholder",
            vec!["h-00000000000000000"],
            RefusedBy::Form,
        ),
        (
            "the all-zero placeholder, short form",
            vec!["h-00000000"],
            RefusedBy::Form,
        ),
        ("not an id at all", vec!["not-a-host"], RefusedBy::Form),
        (
            "a malformed id beside a held host",
            vec![held, "not-a-host"],
            RefusedBy::Form,
        ),
    ]
}

/// The form check and the existence check answer DIFFERENT classes of code, over every way an
/// id can fail to name a live host and over both operations that price a reservation for a set
/// of hosts.
#[tokio::test]
async fn the_form_check_and_the_existence_check_answer_different_codes() {
    let (router, _engine) = super::ec2_query::emulator();
    let held = allocate(&router, 1).await.remove(0);
    let released = allocate(&router, 1).await.remove(0);
    release(&router, &released).await;
    let offering_id = published_offering(&router).await;

    for action in ["GetHostReservationPurchasePreview", "PurchaseHostReservation"] {
        for (case, host_ids, refused_by) in rows(&held, &released) {
            let (status, body) = priced(&router, action, &host_ids, &offering_id).await;
            assert_eq!(
                status, 400,
                "{action} case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            let code = error_code(&body);
            let class = code.rsplit('.').next().unwrap_or_default().to_string();
            assert_eq!(
                class,
                refused_by.code_suffix(),
                "{action} case {case:?}: expected a {refused_by:?} refusal \
                 (code ending in {:?}), got {code:?} in {body}",
                refused_by.code_suffix()
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

/// The preview's own codes, in full: the spellings its recorded refusals carry for the two
/// halves — `InvalidHostId.Malformed` for a value outside the id form, and the code EC2
/// publishes for an absent host for a well-formed id that names none — which is what a client
/// branches on.
#[tokio::test]
async fn the_preview_answers_its_recorded_host_id_codes() {
    let (router, _engine) = super::ec2_query::emulator();
    let held = allocate(&router, 1).await.remove(0);
    let released = allocate(&router, 1).await.remove(0);
    release(&router, &released).await;
    let offering_id = published_offering(&router).await;

    for (case, host_ids, refused_by) in rows(&held, &released) {
        let expected = match refused_by {
            RefusedBy::Form => "InvalidHostId.Malformed",
            RefusedBy::Existence => ec2::state::INVALID_HOST_ID_NOT_FOUND,
        };
        let (status, body) =
            priced(&router, "GetHostReservationPurchasePreview", &host_ids, &offering_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected}, got {:?} in {body}",
            error_code(&body)
        );
    }
}

/// An id the account DOES hold is not refused at all: the existence check is a check and not a
/// blanket refusal, so a preview naming only held hosts is served.
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
        let (status, body) =
            priced(&router, "GetHostReservationPurchasePreview", &host_ids, &offering_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the preview to be served, got {status} with body {body}"
        );
    }
}
