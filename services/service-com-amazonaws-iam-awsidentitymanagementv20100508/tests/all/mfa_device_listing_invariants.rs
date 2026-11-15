// SPDX-License-Identifier: CC0-1.0

//! The invariants `ListMFADevices` holds beside WHICH devices it reports, end to end over
//! the awsQuery wire.
//!
//! The rules being asserted:
//!
//! * the user whose devices are reported is resolved WITHOUT regard to the case the request
//!   spells its name in — IAM entity names are not distinguished by case — and every device
//!   is reported under the spelling the account holds the user by, not the one the request
//!   happened to use;
//! * the operation is a READ ("lists the MFA devices for an IAM user"), so no call of it,
//!   served or refused, changes what a later caller sees: the devices, their users and their
//!   enable dates are the same after a walk of every page as before it, and the account's own
//!   view of the same devices is untouched;
//! * `MaxItems` carries the published range of `maxItemsType` (1..1000,
//!   `smithy_query(query="shape",shape="maxItemsType")`), so each published bound is served
//!   and one past either end is refused 400 `ValidationError` naming the member and the
//!   bound.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the case the
//! `UserName` is spelled in (as created, folded down, folded up, alternating), the KIND of
//! the devices held (virtual MFA devices and a FIDO security key), the page size a read asks
//! for, and — for the bounds — which end of the published range a value sits on and which
//! side of it.
//!
//! `tests/all/mfa_device_listing.rs` owns WHICH devices a page reports and in what order;
//! this suite owns what a read must not disturb and the bounds it accepts. The awsQuery
//! harness is the one `tests/all/user_lifecycle.rs` owns, the enabled-device fixture is the
//! one `tests/all/mfa_device_listing.rs` owns, and the FIDO security key is seeded the one
//! way `tests/all/mfa_device_reads.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use crate::mfa_device_listing::given_enabled_device;
use crate::mfa_device_reads::record_fido_security_key;
use crate::user_lifecycle::{account_id, call, element, elements, error_code, router_with_engine};
use crate::virtual_mfa_device_enablement::given_user;

/// The IAM user the devices belong to, deliberately spelled in MIXED case so a row that
/// folds the name is really asking for the same user under another spelling.
const USER: &str = "Device-Holder";

/// One `ListMFADevices` call naming `user_name`, asking for the page `page` describes.
async fn list(
    router: &MultiServiceRouter,
    user_name: &str,
    page: &[(&str, &str)],
) -> (u16, String) {
    let mut params = vec![("UserName", user_name)];
    params.extend_from_slice(page);
    call(router, "ListMFADevices", &params).await
}

/// Provision the fixture: the user, two virtual MFA devices enabled for it and one FIDO
/// security key enrolled for it, answering the serials in the order a page reports them.
async fn given_devices_of_both_kinds(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &str,
) -> Vec<String> {
    given_user(router, USER).await;
    let mut serials = Vec::new();
    for name in ["holder-phone", "holder-tablet"] {
        serials.push(given_enabled_device(router, case, USER, name).await);
    }
    // No operation of this service enrolls a FIDO security key, so the fact is written into
    // the store the handler reads (see `tests/all/mfa_device_reads.rs`).
    let key_serial = format!("arn:aws:iam::{}:u2f/user/{USER}/holder-key", account_id());
    record_fido_security_key(engine, &key_serial, USER, &[("FIDO", "L1")]);
    serials.push(key_serial);
    serials.sort();
    serials
}

/// RULE: the user whose devices are reported is resolved without regard to the case its name
/// is spelled in, and each device reports the spelling the account holds the user by.
#[tokio::test]
async fn the_named_user_is_resolved_without_regard_to_case() {
    let case = "a user named in another case";
    let (router, engine) = router_with_engine();
    let expected = given_devices_of_both_kinds(&router, &engine, case).await;

    // Rows: the spelling the user was created with, folded all the way down, all the way up,
    // and alternating — every one of them names the same user.
    for spelling in [
        USER.to_string(),
        USER.to_ascii_lowercase(),
        USER.to_ascii_uppercase(),
        USER.chars()
            .enumerate()
            .map(|(position, letter)| {
                if position % 2 == 0 {
                    letter.to_ascii_uppercase()
                } else {
                    letter.to_ascii_lowercase()
                }
            })
            .collect::<String>(),
    ] {
        let (status, body) = list(&router, &spelling, &[]).await;
        assert_eq!(
            status, 200,
            "case {spelling:?}: expected the devices to be listable, got {status}: {body}"
        );
        assert_eq!(
            elements(&body, "SerialNumber"),
            expected,
            "case {spelling:?}: expected every device of the user, got {:?}",
            elements(&body, "SerialNumber")
        );
        let reported = elements(&body, "UserName");
        assert_eq!(
            reported,
            vec![USER.to_string(); expected.len()],
            "case {spelling:?}: expected each device to report the held spelling {USER:?}, \
             got {reported:?}"
        );
    }
}

/// RULE: the listing is a READ, so no call of it — a whole page, a walk of every page, or a
/// refused one — changes what a later caller sees, of the devices themselves or of the
/// account's own view of them.
#[tokio::test]
async fn a_read_leaves_what_a_later_caller_sees_unchanged() {
    let case = "reading repeatedly";
    let (router, engine) = router_with_engine();
    let expected = given_devices_of_both_kinds(&router, &engine, case).await;

    /// What a later caller sees: the devices of the user, and the account's own view of the
    /// devices it holds.
    async fn observed(router: &MultiServiceRouter, case: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
        let (status, body) = list(router, USER, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the user's devices must be listable, got {status}: {body}"
        );
        let (account_status, account_body) = call(router, "ListVirtualMFADevices", &[]).await;
        assert_eq!(
            account_status, 200,
            "case {case:?}: the account's devices must be listable, got {account_status}: \
             {account_body}"
        );
        (
            elements(&body, "SerialNumber"),
            elements(&body, "EnableDate"),
            elements(&account_body, "SerialNumber"),
        )
    }

    let before = observed(&router, case).await;
    assert_eq!(
        before.0, expected,
        "case {case:?}: the fixture must report every device, got {:?}",
        before.0
    );

    // Rows: the page sizes a caller reads with, walked to the end through the markers, plus
    // the two refusals a read can answer.
    for page_size in ["1", "2", "1000"] {
        let mut marker: Option<String> = None;
        loop {
            let mut page = vec![("MaxItems", page_size)];
            if let Some(resume) = marker.as_deref() {
                page.push(("Marker", resume));
            }
            let (status, body) = list(&router, USER, &page).await;
            assert_eq!(
                status, 200,
                "case {page_size} per page: expected HTTP 200, got {status}: {body}"
            );
            if element(&body, "IsTruncated") == "true" {
                marker = Some(element(&body, "Marker"));
            } else {
                break;
            }
        }
    }
    let (status, body) = list(&router, USER, &[("Marker", "not-a-marker")]).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a foreign marker must be refused, got {status}: {body}"
    );
    let (status, body) = list(&router, "no-such-holder", &[]).await;
    assert_eq!(
        status, 404,
        "case {case:?}: an absent user must be refused, got {status}: {body}"
    );

    let after = observed(&router, case).await;
    assert_eq!(
        after, before,
        "case {case:?}: expected the reads to leave the devices, their enable dates and the \
         account's own view untouched, got {after:?}"
    );
}

/// RULE: `MaxItems` carries the published range of `maxItemsType` (1..1000), so each
/// published bound is served and one past either end is refused 400 `ValidationError` naming
/// the member and the bound it broke.
#[tokio::test]
async fn the_published_page_size_range_is_served_at_its_bounds_and_refused_past_them() {
    let case = "page-size bounds";
    let (router, engine) = router_with_engine();
    let expected = given_devices_of_both_kinds(&router, &engine, case).await;

    // Each published bound of the range, served.
    for (row, page_size, reported) in [
        ("the published minimum", "1", 1usize),
        ("the published maximum", "1000", expected.len()),
    ] {
        let (status, body) = list(&router, USER, &[("MaxItems", page_size)]).await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected HTTP 200, got {status}: {body}"
        );
        assert_eq!(
            elements(&body, "SerialNumber").len(),
            reported,
            "case {row:?}: expected {reported} device(s) on the page, got {:?}",
            elements(&body, "SerialNumber")
        );
    }

    // One past either end of the range.
    for (row, page_size, requirement) in [
        (
            "one below the published minimum",
            "0",
            "Member must have value greater than or equal to 1",
        ),
        (
            "one above the published maximum",
            "1001",
            "Member must have value less than or equal to 1000",
        ),
    ] {
        let (status, body) = list(&router, USER, &[("MaxItems", page_size)]).await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationError",
            "case {row:?}: expected ValidationError, got {:?}: {body}",
            error_code(&body)
        );
        let expected_message = format!(
            "1 validation error detected: Value at 'maxItems' failed to satisfy constraint: \
             {requirement}"
        );
        assert_eq!(
            element(&body, "Message"),
            expected_message,
            "case {row:?}: expected {expected_message:?}, got {:?}",
            element(&body, "Message")
        );
    }
}
