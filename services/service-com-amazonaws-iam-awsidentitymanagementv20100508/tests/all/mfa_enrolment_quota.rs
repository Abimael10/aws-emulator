// SPDX-License-Identifier: CC0-1.0

//! The per-user MFA device quota, and how `DeactivateMFADevice` words a device the named
//! user has not enrolled — both end to end over the awsQuery wire.
//!
//! The rules under test:
//!
//! * ONE IAM user holds a bounded number of MFA devices, the quota AWS publishes as "MFA
//!   devices in use per IAM user", counted over the devices of EVERY kind the account holds
//!   for that user. `EnableMFADevice` models `LimitExceededException` ("it attempted to
//!   create resources beyond the current … account limits. The error message describes the
//!   limit exceeded", HTTP 409 / `LimitExceeded`), so the enrolment past the quota is
//!   refused with the fixed sentence naming that quota, nothing is enrolled, the device
//!   stays unassigned — and the quota is per USER, so another user is unaffected.
//! * `DeactivateMFADevice` "deactivates the specified MFA device and removes it from
//!   association with the user name for which it was originally enabled", so what it
//!   addresses is the ENROLMENT: a serial that names no enrolment of the named user —
//!   a device the account holds unassigned, a device another user holds, or one this user
//!   held until a first deactivation — is `NoSuchEntity` (HTTP 404) with IAM's fixed
//!   sentence for that class, which quotes neither the serial nor the user back. A serial
//!   naming no DEVICE of the account at all is the other class, and keeps the sentence the
//!   serial-addressed family words a missing device with.
//!
//! Each suite is a TABLE whose rows differ along what the rules generalise over: how many
//! devices the user already holds and of which KINDS, which user is at the quota, and which
//! way a serial fails to name the named user's enrolment.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns; the device fixtures
//! are the ones `tests/all/virtual_mfa_device_enablement.rs`, `tests/all/mfa_device_reads.rs`
//! and `tests/all/mfa_device_listing.rs` own, so a device is provisioned, enabled and
//! enrolled exactly one way.

use std::time::SystemTime;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::MAX_MFA_DEVICES_PER_USER;
use smithy_server_runtime::MultiServiceRouter;

use crate::mfa_device_listing::given_enabled_device;
use crate::mfa_device_reads::record_fido_security_key;
use crate::user_lifecycle::{call, element, elements, error_code, router, router_with_engine};
use crate::virtual_mfa_device_enablement::{current_step, given_device, given_user, pass_code};

/// The sentence IAM refuses an enrolment past the per-user quota with, written out here
/// rather than read from the implementation's constant, because what this suite pins is the
/// text on the WIRE: `quota limit for` and the quota's NAME alone, with no value after it.
const QUOTA_EXCEEDED: &str = "Cannot exceed quota limit for MFADevicesPerUser";

/// The sentence `DeactivateMFADevice` words the absence of an ENROLMENT with — entirely
/// fixed text, written out for the same reason.
const INVALID_FOR_USER: &str = "MFA Device invalid for user.";

/// One `EnableMFADevice` call for a device the fixture provisioned, answering the status and
/// body: the codes are the pair the device's own seed emits, so what a refusal below reports
/// is the quota and never the codes.
async fn enable(
    router: &MultiServiceRouter,
    user_name: &str,
    serial_number: &str,
    seed: &str,
) -> (u16, String) {
    let step = current_step(SystemTime::now());
    let (first, second) = (pass_code(seed, step), pass_code(seed, step + 1));
    call(
        router,
        "EnableMFADevice",
        &[
            ("UserName", user_name),
            ("SerialNumber", serial_number),
            ("AuthenticationCode1", &first),
            ("AuthenticationCode2", &second),
        ],
    )
    .await
}

/// The serials `ListMFADevices` reports for `user_name`.
async fn enrolled_serials(router: &MultiServiceRouter, user_name: &str) -> Vec<String> {
    let (status, body) = call(router, "ListMFADevices", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "the devices of {user_name:?} must be listable, got {status}: {body}"
    );
    elements(&body, "SerialNumber")
}

/// RULE: a user may hold at most the per-user quota of MFA devices; the enrolment past it is
/// refused `LimitExceeded` with the quota's own sentence, enrols nothing, and leaves the
/// device free for another user — while the user that is AT the quota keeps exactly the
/// devices it had.
#[tokio::test]
async fn an_enrolment_past_the_per_user_quota_is_refused_and_enrols_nothing() {
    let at_quota = "quota-holder";
    let bystander = "quota-bystander";
    let router = router();
    given_user(&router, at_quota).await;
    given_user(&router, bystander).await;
    // The published quota — "MFA devices in use per IAM user: 8" — written out so the rows
    // below are run against the figure AWS publishes rather than against whatever the
    // implementation happens to hold.
    assert_eq!(
        MAX_MFA_DEVICES_PER_USER, 8,
        "the per-user MFA device quota AWS publishes is 8, got {MAX_MFA_DEVICES_PER_USER}"
    );

    // Fill the quota exactly.
    let mut held: Vec<String> = Vec::new();
    for slot in 1..=MAX_MFA_DEVICES_PER_USER {
        let (serial_number, seed) = given_device(&router, &format!("quota-phone-{slot}")).await;
        let (status, body) = enable(&router, at_quota, &serial_number, &seed).await;
        assert_eq!(
            status, 200,
            "enrolment {slot} of {MAX_MFA_DEVICES_PER_USER} must be served, got {status}: {body}"
        );
        held.push(serial_number);
    }
    held.sort();
    assert_eq!(
        enrolled_serials(&router, at_quota).await,
        held,
        "the user at the quota must hold every device it enrolled"
    );

    // The next one is refused, whichever device it names.
    let (over_serial, over_seed) = given_device(&router, "quota-phone-over").await;
    let (status, body) = enable(&router, at_quota, &over_serial, &over_seed).await;
    assert_eq!(
        status, 409,
        "the enrolment past the quota must be refused with HTTP 409, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "LimitExceeded",
        "expected the error code LimitExceeded, got {:?}: {body}",
        error_code(&body)
    );
    assert_eq!(
        element(&body, "Message"),
        QUOTA_EXCEEDED,
        "expected the quota sentence {QUOTA_EXCEEDED:?}, got {:?}",
        element(&body, "Message")
    );
    assert_eq!(
        enrolled_serials(&router, at_quota).await,
        held,
        "a refused enrolment must leave the user's devices exactly as they were"
    );

    // The refused device was left unassigned, so it can still be enrolled — by another
    // user, because the quota is per user.
    let (status, body) = enable(&router, bystander, &over_serial, &over_seed).await;
    assert_eq!(
        status, 200,
        "the device the refusal left unassigned must be enrollable by another user, got \
         {status}: {body}"
    );
    assert_eq!(
        enrolled_serials(&router, bystander).await,
        vec![over_serial],
        "the other user must hold the device the quota refused for the first"
    );
}

/// RULE: the quota counts the devices of EVERY kind the account holds for the user, so a
/// user whose devices are a FIDO security key the account enrolled outside IAM's API plus
/// virtual devices is at the quota at the same count — the enrolment that would be the
/// quota-th plus one is refused.
#[tokio::test]
async fn the_quota_counts_devices_of_every_kind_the_user_holds() {
    let user_name = "mixed-device-holder";
    let (router, engine) = router_with_engine();
    given_user(&router, user_name).await;

    // One FIDO security key, then virtual devices up to one short of the quota.
    let security_key = "mixed-security-key";
    record_fido_security_key(&engine, security_key, user_name, &[("FIPS-140-2", "L2")]);
    for slot in 1..MAX_MFA_DEVICES_PER_USER {
        let (serial_number, seed) = given_device(&router, &format!("mixed-phone-{slot}")).await;
        let (status, body) = enable(&router, user_name, &serial_number, &seed).await;
        assert_eq!(
            status, 200,
            "enrolment {slot} must be served while the user is under the quota, got {status}: \
             {body}"
        );
    }
    assert_eq!(
        enrolled_serials(&router, user_name).await.len(),
        MAX_MFA_DEVICES_PER_USER,
        "the user must now hold exactly the quota, counting the security key"
    );

    let (serial_number, seed) = given_device(&router, "mixed-phone-over").await;
    let (status, body) = enable(&router, user_name, &serial_number, &seed).await;
    assert_eq!(
        status, 409,
        "with the quota reached across both kinds the next enrolment must be refused, got \
         {status}: {body}"
    );
    assert_eq!(
        element(&body, "Message"),
        QUOTA_EXCEEDED,
        "expected the quota sentence, got {:?}",
        element(&body, "Message")
    );
    assert!(
        !enrolled_serials(&router, user_name)
            .await
            .contains(&serial_number),
        "the refused device must not be enrolled"
    );
}

/// RULE: `DeactivateMFADevice` addresses an ENROLMENT, so every serial that names none of
/// the named user's is refused with the one fixed sentence for that class — and a serial
/// naming no device of the account at all keeps the OTHER class's sentence, which names the
/// serial. Nothing is deactivated by any of them.
#[tokio::test]
async fn deactivating_a_device_the_user_has_not_enrolled_is_refused_as_the_absent_enrolment() {
    let holder = "enrolment-holder";
    let other = "enrolment-other";
    let router = router();
    given_user(&router, holder).await;
    given_user(&router, other).await;

    let held = given_enabled_device(&router, "held", holder, "held-phone").await;
    let others = given_enabled_device(&router, "other", other, "other-phone").await;
    let (unassigned, _) = given_device(&router, "free-phone").await;

    // The first deactivation of the user's OWN device is served, which is what then makes
    // that same serial an absent enrolment for the row below.
    let (status, body) = call(
        &router,
        "DeactivateMFADevice",
        &[("UserName", holder), ("SerialNumber", &held)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the holder's own device must be deactivable, got {status}: {body}"
    );

    let rows: Vec<(&str, String, String)> = vec![
        (
            "a device the account holds that nobody holds",
            unassigned.clone(),
            INVALID_FOR_USER.to_string(),
        ),
        (
            "a device another user holds",
            others.clone(),
            INVALID_FOR_USER.to_string(),
        ),
        (
            "the user's own device, already deactivated",
            held.clone(),
            INVALID_FOR_USER.to_string(),
        ),
        (
            "a serial naming no device of the account",
            format!("{held}-absent"),
            format!("MFA Device with serial number {held}-absent does not exist."),
        ),
        (
            "a bare serial naming no device",
            "GAHT12345678".to_string(),
            "MFA Device with serial number GAHT12345678 does not exist.".to_string(),
        ),
    ];

    for (case, serial_number, expected) in rows {
        let (status, body) = call(
            &router,
            "DeactivateMFADevice",
            &[("UserName", holder), ("SerialNumber", &serial_number)],
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected the error code NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            element(&body, "Message")
        );
    }

    // None of the refusals touched an enrolment: the other user still holds its device.
    assert_eq!(
        enrolled_serials(&router, other).await,
        vec![others],
        "the refusals must leave the other user's enrolment alone"
    );
    assert!(
        enrolled_serials(&router, holder).await.is_empty(),
        "the holder deactivated its only device, so it must hold none"
    );
}
