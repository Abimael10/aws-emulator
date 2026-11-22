// SPDX-License-Identifier: CC0-1.0

//! Listing one IAM user's MFA devices and deactivating one, end to end over the awsQuery
//! wire.
//!
//! The rules being asserted:
//!
//! * `ListMFADevices` "lists the MFA devices for an IAM user" — every device that user
//!   holds, of every KIND the account keeps (a virtual MFA device `EnableMFADevice` enabled,
//!   and a FIDO security key enrolled outside IAM's API) — each reported with the user name
//!   the account holds, the serial it is addressed by and when it was enabled, in ascending
//!   order of that serial and paged by `MaxItems` and `Marker`;
//! * with no `UserName`, "IAM determines the user name implicitly based on the Amazon Web
//!   Services access key ID signing the request", so the devices reported are the signing
//!   user's own; a name the account holds no user under is the modelled `NoSuchEntity` 404;
//! * `DeactivateMFADevice` "deactivates the specified MFA device and removes it from
//!   association with the user name for which it was originally enabled", so afterwards the
//!   user holds it no longer and the account reports the device as unassigned — which is what
//!   then lets `DeleteVirtualMFADevice` remove it;
//! * a serial the account holds no device under, and a device the named user does not hold,
//!   are both the entity that does not exist, refused 404 `NoSuchEntity`, and neither
//!   deactivates anything.
//!
//! Each suite is a TABLE whose rows differ along what the rules generalise over: the KIND of
//! each device (virtual, FIDO security key), how many a user holds (none, one, several), the
//! page size, whether the request names the user or leaves it implicit, and — for the
//! refusals — whether the serial names a device the account holds and whose it is.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns; the device fixtures are
//! the ones `tests/all/virtual_mfa_device_enablement.rs` and `tests/all/mfa_device_reads.rs`
//! own, so a device is provisioned, enabled and enrolled exactly one way.

use std::time::SystemTime;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use smithy_server_runtime::MultiServiceRouter;

use crate::identity_policy_enforcement::{principal, statement};
use crate::mfa_device_reads::{record_fido_security_key, ENABLED_MILLIS};
use crate::user_lifecycle::{
    account_id, call, call_signed_by, element, elements, error_code, router, router_with_engine,
};
use crate::virtual_mfa_device_enablement::{current_step, given_device, given_user, pass_code};

/// The IAM user the devices are enabled for.
const USER: &str = "device-operator";

/// A second user, so a row can address a device that is not its own.
const OTHER_USER: &str = "device-bystander";

/// Provision a virtual MFA device named `name` and enable it for `user_name`, answering its
/// serial number.
pub(crate) async fn given_enabled_device(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    name: &str,
) -> String {
    let (serial_number, seed) = given_device(router, name).await;
    let step = current_step(SystemTime::now());
    let (first, second) = (pass_code(&seed, step), pass_code(&seed, step + 1));
    let (status, body) = call(
        router,
        "EnableMFADevice",
        &[
            ("UserName", user_name),
            ("SerialNumber", &serial_number),
            ("AuthenticationCode1", &first),
            ("AuthenticationCode2", &second),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture EnableMFADevice must succeed, got {status}: {body}"
    );
    serial_number
}

/// One `ListMFADevices` call: the user it names when it names one, and the page it asks for.
async fn list(
    router: &MultiServiceRouter,
    akid: Option<&str>,
    user_name: Option<&str>,
    page: &[(&str, &str)],
) -> (u16, String) {
    let mut params: Vec<(&str, &str)> = Vec::new();
    if let Some(user_name) = user_name {
        params.push(("UserName", user_name));
    }
    params.extend_from_slice(page);
    call_signed_by(router, akid, "ListMFADevices", &params).await
}

/// One `DeactivateMFADevice` call.
async fn deactivate(
    router: &MultiServiceRouter,
    user_name: Option<&str>,
    serial_number: &str,
) -> (u16, String) {
    let mut params = vec![("SerialNumber", serial_number)];
    if let Some(user_name) = user_name {
        params.push(("UserName", user_name));
    }
    call(router, "DeactivateMFADevice", &params).await
}

/// RULE: the devices reported are every device the named user holds, of both kinds the
/// account keeps, in ascending order of the serial each is addressed by — and a user holding
/// none is reported with an empty list rather than refused.
#[tokio::test]
async fn every_device_the_user_holds_is_reported_in_serial_order() {
    let case = "a user holding devices of both kinds";
    let (router, engine) = router_with_engine();
    given_user(&router, USER).await;
    given_user(&router, OTHER_USER).await;

    // Three virtual devices, deliberately named so their serials do NOT come out in the
    // order they were enabled: what orders a page is the serial, not the enable.
    let mut expected: Vec<String> = Vec::new();
    for name in ["zulu-phone", "alpha-phone", "mike-phone"] {
        expected.push(given_enabled_device(&router, case, USER, name).await);
    }
    // A FIDO security key of the same user: no operation of this service enrolls one, so
    // the fact is written into the store the handler reads.
    let key_serial = format!("arn:aws:iam::{}:u2f/user/{USER}/key-1", account_id());
    record_fido_security_key(&engine, &key_serial, USER, &[("FIDO", "L1")]);
    expected.push(key_serial.clone());
    // A device of ANOTHER user, which this user's listing must not report.
    let other_serial = given_enabled_device(&router, case, OTHER_USER, "bystander-phone").await;
    expected.sort();

    let (status, body) = list(&router, None, Some(USER), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the devices to be listable, got {status}: {body}"
    );
    assert_eq!(
        elements(&body, "SerialNumber"),
        expected,
        "case {case:?}: expected every device of the user in serial order, got {:?}",
        elements(&body, "SerialNumber")
    );
    assert!(
        !elements(&body, "SerialNumber").contains(&other_serial),
        "case {case:?}: another user's device must not be reported, got {:?}",
        elements(&body, "SerialNumber")
    );
    // Every device reports the user it is enabled for and when it was enabled.
    let users = elements(&body, "UserName");
    assert_eq!(
        users,
        vec![USER.to_string(); expected.len()],
        "case {case:?}: expected each device to report its user, got {users:?}"
    );
    let enabled: Vec<i64> = elements(&body, "EnableDate")
        .iter()
        .map(|text| {
            DateTime::from_str(text, Format::DateTime)
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: expected an ISO-8601 EnableDate, got {text:?}: {error}")
                })
                .to_millis()
                .expect("a representable instant")
        })
        .collect();
    assert_eq!(
        enabled.len(),
        expected.len(),
        "case {case:?}: expected one EnableDate per device, got {enabled:?}"
    );
    // The seeded key's stored instant is reported as stored, which is what says the dates
    // come from the record rather than from the clock of the read.
    let key_position = expected
        .iter()
        .position(|serial| *serial == key_serial)
        .expect("the seeded key is among the expected devices");
    assert_eq!(
        enabled[key_position], ENABLED_MILLIS,
        "case {case:?}: expected the stored EnableDate of the security key, got {:?}",
        enabled[key_position]
    );
    assert_eq!(
        element(&body, "IsTruncated"),
        "false",
        "case {case:?}: one page holds every device, got {body}"
    );

    // A user the account holds that holds no device: an empty list, not a refusal.
    given_user(&router, "no-devices").await;
    let (status, body) = list(&router, None, Some("no-devices"), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a user holding no device must still be listable, got {status}: {body}"
    );
    assert!(
        elements(&body, "SerialNumber").is_empty(),
        "case {case:?}: expected an empty list, got {:?}",
        elements(&body, "SerialNumber")
    );
}

/// RULE: a read pages over the user's devices — each page carries at most `MaxItems` of them
/// and hands back the marker the next page resumes strictly after, so following the markers
/// reports every device exactly once.
#[tokio::test]
async fn a_read_pages_over_the_users_devices() {
    let case = "paging";
    let router = router();
    given_user(&router, USER).await;
    let mut expected: Vec<String> = Vec::new();
    for name in ["phone-a", "phone-b", "phone-c", "phone-d"] {
        expected.push(given_enabled_device(&router, case, USER, name).await);
    }
    expected.sort();

    for page_size in [1usize, 2, 4] {
        let mut reported: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let size = page_size.to_string();
            let mut page = vec![("MaxItems", size.as_str())];
            if let Some(resume) = marker.as_deref() {
                page.push(("Marker", resume));
            }
            let (status, body) = list(&router, None, Some(USER), &page).await;
            assert_eq!(
                status, 200,
                "case {page_size} per page: expected HTTP 200, got {status}: {body}"
            );
            let reported_page = elements(&body, "SerialNumber");
            assert!(
                reported_page.len() <= page_size,
                "case {page_size} per page: expected at most {page_size} devices, got {}: {body}",
                reported_page.len()
            );
            reported.extend(reported_page);
            if element(&body, "IsTruncated") == "true" {
                marker = Some(element(&body, "Marker"));
            } else {
                assert!(
                    elements(&body, "Marker").is_empty(),
                    "case {page_size} per page: expected no marker on the last page: {body}"
                );
                break;
            }
        }
        assert_eq!(
            reported, expected,
            "case {page_size} per page: expected the pages to report every device once"
        );
    }

    // A marker this service's encoding cannot have produced is refused rather than read as
    // "start from the beginning".
    let (status, body) = list(&router, None, Some(USER), &[("Marker", "not-a-marker")]).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a foreign marker to be refused, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationError",
        "case {case:?}: expected ValidationError, got {:?}: {body}",
        error_code(&body)
    );
}

/// RULE: with no `UserName` the devices reported are the SIGNING user's own, and a name the
/// account holds no user under is the modelled `NoSuchEntity` 404.
#[tokio::test]
async fn an_absent_user_name_reports_the_signing_users_devices() {
    let case = "an implicit user name";
    let router = router();
    let access_key_id = principal(
        &router,
        case,
        USER,
        &[(
            "list".to_string(),
            statement("Allow", "iam:ListMFADevices", "*"),
        )],
    )
    .await;
    let serial_number = given_enabled_device(&router, case, USER, "implicit-phone").await;
    // Another user's device, which the signing user's listing must not report.
    given_user(&router, OTHER_USER).await;
    given_enabled_device(&router, case, OTHER_USER, "other-phone").await;

    let (status, body) = list(&router, Some(&access_key_id), None, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the signing user's devices, got {status}: {body}"
    );
    assert_eq!(
        elements(&body, "SerialNumber"),
        vec![serial_number.clone()],
        "case {case:?}: expected exactly the signing user's device, got {:?}",
        elements(&body, "SerialNumber")
    );

    // A name the account holds no user under names no user whose devices could be reported.
    let (status, body) = list(&router, None, Some("no-such-user"), &[]).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404 for an absent user, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "NoSuchEntity",
        "case {case:?}: expected NoSuchEntity, got {:?}: {body}",
        error_code(&body)
    );
    assert_eq!(
        element(&body, "Message"),
        "The user with name no-such-user cannot be found.",
        "case {case:?}: expected the refusal to name the user asked about, got {:?}",
        element(&body, "Message")
    );
}

/// RULE: a deactivation removes the device from association with the user it was enabled for
/// — so the user's listing no longer reports it, the account reports the device as unassigned,
/// and the delete that the association blocked is then served.
#[tokio::test]
async fn a_deactivation_removes_the_association_and_unblocks_the_delete() {
    // Rows: how many devices the user holds when one is deactivated, so what is asserted is
    // that the named device is released rather than every device of the user.
    for held in [1usize, 3] {
        let case = format!("a user holding {held} device(s)");
        let router = router();
        given_user(&router, USER).await;
        let mut serials: Vec<String> = Vec::new();
        for index in 0..held {
            serials.push(given_enabled_device(&router, &case, USER, &format!("phone-{index}")).await);
        }
        let released = serials[0].clone();

        // While the device is the user's, the delete is blocked.
        let (status, body) = call(
            &router,
            "DeleteVirtualMFADevice",
            &[("SerialNumber", &released)],
        )
        .await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected the delete to be blocked while the device is held, \
             got {status}: {body}"
        );

        let (status, body) = deactivate(&router, Some(USER), &released).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the deactivation to be served, got {status}: {body}"
        );

        // The user no longer holds it; the devices it still holds are untouched.
        let (status, body) = list(&router, None, Some(USER), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the user's devices must be listable, got {status}: {body}"
        );
        let mut still_held: Vec<String> = serials[1..].to_vec();
        still_held.sort();
        assert_eq!(
            elements(&body, "SerialNumber"),
            still_held,
            "case {case:?}: expected only the devices still held, got {:?}",
            elements(&body, "SerialNumber")
        );

        // The account reports the released device as unassigned.
        let (status, body) = call(
            &router,
            "ListVirtualMFADevices",
            &[("AssignmentStatus", "Unassigned")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the account's devices must be listable, got {status}: {body}"
        );
        assert!(
            elements(&body, "SerialNumber").contains(&released),
            "case {case:?}: expected the released device to be unassigned, got {:?}",
            elements(&body, "SerialNumber")
        );

        // And the delete the association blocked is served now.
        let (status, body) = call(
            &router,
            "DeleteVirtualMFADevice",
            &[("SerialNumber", &released)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to be served after the deactivation, \
             got {status}: {body}"
        );
    }
}

/// RULE: a serial the account holds no device under, and a device the named user has not
/// enrolled, are both the entity that does not exist — each refused 404 `NoSuchEntity` with
/// the sentence of its own class, the device's absence naming the serial and the
/// ENROLMENT's naming neither the serial nor the user — and neither deactivates anything.
#[tokio::test]
async fn a_device_that_is_not_the_named_users_is_refused_as_a_missing_entity() {
    let case = "deactivating";
    let router = router();
    given_user(&router, USER).await;
    given_user(&router, OTHER_USER).await;
    let serial_number = given_enabled_device(&router, case, USER, "held-phone").await;
    let (free_serial, _) = given_device(&router, "free-phone").await;
    let absent_serial = format!("{serial_number}-absent");
    // The sentence `DeactivateMFADevice` words the absence of an ENROLMENT with, whichever
    // device the request named: entirely fixed text, written out here rather than read from
    // the implementation's constant, because what this suite pins is the text on the WIRE.
    let invalid_for_user = "MFA Device invalid for user.".to_string();

    let rows: [(&str, Option<&str>, &str, String); 4] = [
        (
            "a serial the account holds no device under",
            Some(USER),
            absent_serial.as_str(),
            format!("MFA Device with serial number {absent_serial} does not exist."),
        ),
        (
            "a bare serial naming no device",
            Some(USER),
            "GAHT12345678",
            "MFA Device with serial number GAHT12345678 does not exist.".to_string(),
        ),
        (
            "a device another user holds",
            Some(OTHER_USER),
            serial_number.as_str(),
            invalid_for_user.clone(),
        ),
        (
            "a device nobody holds",
            Some(USER),
            free_serial.as_str(),
            invalid_for_user,
        ),
    ];

    for (row, user_name, serial, expected) in rows {
        let (status, body) = deactivate(&router, user_name, serial).await;
        assert_eq!(
            status, 404,
            "case {row:?}: expected HTTP 404, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {row:?}: expected NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            expected,
            "case {row:?}: expected {expected:?}, got {:?}",
            element(&body, "Message")
        );
    }

    // None of the refusals released anything: the user still holds its device.
    let (status, body) = list(&router, None, Some(USER), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the user's devices must be listable, got {status}: {body}"
    );
    assert_eq!(
        elements(&body, "SerialNumber"),
        vec![serial_number.clone()],
        "case {case:?}: expected the device to stay the user's, got {:?}",
        elements(&body, "SerialNumber")
    );
}
