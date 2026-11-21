// SPDX-License-Identifier: CC0-1.0

//! WHAT the Dedicated Host reservation preview's id validation is grounded in: the host ids
//! this ACCOUNT was issued, not the spelling of the id in the request.
//!
//! `GetHostReservationPurchasePreview` answers two different classes of refusal for a
//! `HostIdSet` member it cannot price a reservation for, and this suite pins the line between
//! them with ONE id at a time: a host id this run was handed is priced while the account holds
//! it, and answers the published not-found code once the account has given it back — while an
//! id of exactly the same shape the account was never handed is refused as a malformed id, the
//! class EC2's own id validation answers before any host is looked up.
//!
//! So the class cannot be read off the id's spelling: the two refusals below differ in a
//! single hexadecimal digit, and it is the account's own host records that decide which one
//! answers.
//!
//! Driven over the wire the service speaks, through the crate's shared [`super::ec2_query`]
//! harness, and the released host is read back through `DescribeHosts` so the not-found half
//! is pinned against a host record that really exists.

use super::dedicated_host_id_refusal_classes::{
    REGION, allocate, not_valid_sentence, priced, published_offering, release,
};
use super::ec2_query::{call, elements, error_code, error_message};
use super::wire::param;
use service_com_amazonaws_ec2_amazonec2 as ec2;

/// The operation under test.
const PREVIEW: &str = "GetHostReservationPurchasePreview";

/// The code the preview answers for an id this account was never issued, as its recorded
/// refusals spell the resource (`InvalidHostId`, mixed case).
const MALFORMED: &str = "InvalidHostId.Malformed";

/// An id in the same form and of the same length as `issued`, differing in its last
/// hexadecimal digit, that is NOT one of the ids `minted` carries.
///
/// The point of the suite: a value this account was never handed, as close in spelling to one
/// it was as the id form allows.
fn unissued_twin(issued: &str, minted: &[String]) -> String {
    let head = issued
        .get(..issued.len() - 1)
        .unwrap_or_else(|| panic!("the issued host id {issued:?} must carry a last digit"));
    for digit in "0123456789abcdef".chars() {
        let twin = format!("{head}{digit}");
        if !minted.contains(&twin) {
            return twin;
        }
    }
    panic!("no unissued twin of {issued:?} exists beside {minted:?}")
}

/// What `DescribeHosts` reports as the state of `host_id`, so a claim about a host is made
/// against the record the service itself serves.
async fn described_state(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    host_id: &str,
) -> Vec<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeHosts",
        &[param("HostId.1", host_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeHosts must report {host_id}, got {status} with body {body}"
    );
    elements(&body, "state")
}

/// One host at a time: priced while held, not found once released, malformed for the twin id
/// the account was never issued.
#[tokio::test]
async fn the_class_follows_the_account_s_host_records_and_not_the_id_s_spelling() {
    let (router, _engine) = super::ec2_query::emulator();
    // Several hosts, so the claim is made of more than one minted id — and each is driven
    // through the whole progression on its own.
    let minted = allocate(&router, 3).await;
    let offering_id = published_offering(&router).await;

    for (position, host_id) in minted.iter().enumerate() {
        let case = format!("host {position} ({host_id})");
        let twin = unissued_twin(host_id, &minted);

        // Held: the preview prices it.
        let (status, body) = priced(&router, PREVIEW, &[host_id.as_str()], &offering_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a held host must be priced, got {status} with body {body}"
        );

        // Never issued, one digit away: EC2's id validation refuses it, before any host is
        // looked up.
        let (status, body) = priced(&router, PREVIEW, &[twin.as_str()], &offering_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the unissued twin {twin} must be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            MALFORMED,
            "case {case:?}: the unissued twin {twin} must be refused {MALFORMED}, got {:?} \
             in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            not_valid_sentence(&[twin.as_str()]),
            "case {case:?}: the unissued twin {twin} must name the requested set, got {:?}",
            error_message(&body)
        );

        // Given back: the record is still there — `DescribeHosts` reports it released — so
        // the preview answers the EXISTENCE half for it, not the validation half.
        release(&router, host_id).await;
        let states = described_state(&router, &case, host_id).await;
        assert!(
            states
                .iter()
                .any(|state| state == ec2::state::HOST_STATE_RELEASED),
            "case {case:?}: DescribeHosts must report the host as \
             {released:?}, got {states:?}",
            released = ec2::state::HOST_STATE_RELEASED
        );
        let (status, body) = priced(&router, PREVIEW, &[host_id.as_str()], &offering_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a released host must be refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::INVALID_HOST_ID_NOT_FOUND,
            "case {case:?}: a released host must be refused {expected:?}, got {actual:?} \
             in {body}",
            expected = ec2::state::INVALID_HOST_ID_NOT_FOUND,
            actual = error_code(&body)
        );
    }
}

/// A set carrying both classes answers the VALIDATION one: an id the account was never issued
/// is a defect of the request, which is settled before any host's state is read.
#[tokio::test]
async fn an_unissued_id_beside_a_released_one_is_refused_by_validation() {
    let (router, _engine) = super::ec2_query::emulator();
    let minted = allocate(&router, 2).await;
    let offering_id = published_offering(&router).await;
    for host_id in &minted {
        release(&router, host_id).await;
    }
    let unissued = unissued_twin(&minted[0], &minted);

    // Rows differ in where the unissued id sits in the set and in how many released ids it
    // sits beside, so nothing here depends on the order the request listed them in.
    let rows: Vec<(&str, Vec<&str>)> = vec![
        (
            "the unissued id first",
            vec![unissued.as_str(), minted[0].as_str()],
        ),
        (
            "the unissued id last",
            vec![minted[0].as_str(), unissued.as_str()],
        ),
        (
            "the unissued id between two released ones",
            vec![minted[0].as_str(), unissued.as_str(), minted[1].as_str()],
        ),
    ];

    for (case, host_ids) in rows {
        let (status, body) = priced(&router, PREVIEW, &host_ids, &offering_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            MALFORMED,
            "case {case:?}: expected {MALFORMED}, got {:?} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            not_valid_sentence(&host_ids),
            "case {case:?}: expected the requested set to be named, got {:?}",
            error_message(&body)
        );
    }
}
