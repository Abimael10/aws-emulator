// SPDX-License-Identifier: CC0-1.0

//! Deleting one of the account's virtual MFA devices, end to end over the awsQuery wire.
//!
//! The rules being asserted:
//!
//! * `DeleteVirtualMFADevice` removes the device its `SerialNumber` names from the account —
//!   whatever path it was created under and whatever it carries — so the account's own
//!   listing no longer reports it and the serial names nothing again;
//! * a device still ENABLED for a user "has attached subordinate entities": "you must
//!   deactivate a user's virtual MFA device before you can delete it", so the delete is
//!   refused HTTP 409 `DeleteConflict` with the fixed sentence IAM words that class of
//!   blocking dependent with, and the device stays as the account held it — which is the
//!   same contract every other IAM delete with a dependent answers
//!   (`tests/all/group_deletion.rs`, `tests/all/instance_profiles.rs`);
//! * a serial the account holds no device under is the entity that does not exist, refused
//!   HTTP 404 `NoSuchEntity` with the message naming the serial the request supplied.
//!
//! Each suite is a TABLE whose rows differ along what the rules generalise over: the path a
//! device was created under, the character classes of its name and how many tags it carries
//! (the table `tests/all/virtual_mfa_devices.rs` owns, so the fixture is written once), and
//! for the absent serials the FORM of the value (a device ARN of this account naming nothing,
//! one of another account and another partition, a bare hardware token serial) and both
//! published length bounds of `serialNumberType`.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites in
//! this crate share.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{account_id, call, element, elements, error_code, router};
use crate::virtual_mfa_device_enablement::{current_step, given_user, pass_code};
use crate::virtual_mfa_devices::{provision, Provisioned, PROVISIONED};

/// The IAM user the enabled-device rows hold a device.
const USER: &str = "device-holder";

/// The sentence IAM refuses a delete blocked by a device a user still holds with: fixed
/// text, naming what has to change and neither the device nor its holder.
const DELETE_CONFLICT: &str = "MFA VirtualDevice in use. Must deactivate first.";

/// The published bounds of `serialNumberType`: "len 9..256, pattern `^[\w+=/:,.@-]+$`"
/// (`smithy_query(query="operation", operation="DeleteVirtualMFADevice")`).
const MIN_SERIAL_CHARACTERS: usize = 9;
const MAX_SERIAL_CHARACTERS: usize = 256;

/// One `DeleteVirtualMFADevice` call.
async fn delete(router: &MultiServiceRouter, serial_number: &str) -> (u16, String) {
    call(
        router,
        "DeleteVirtualMFADevice",
        &[("SerialNumber", serial_number)],
    )
    .await
}

/// The serial numbers the account reports under the `Any` assignment status — every device
/// it holds, which is what a delete has to remove one from.
async fn held_serials(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = call(router, "ListVirtualMFADevices", &[("AssignmentStatus", "Any")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's devices must be listable, got {status}: {body}"
    );
    elements(&body, "SerialNumber")
}

/// Enable the device `serial_number`, provisioned with `seed`, for `USER`: the association
/// that blocks a delete.
async fn enable(router: &MultiServiceRouter, case: &str, serial_number: &str, seed: &str) {
    let step = current_step(std::time::SystemTime::now());
    let (first, second) = (pass_code(seed, step), pass_code(seed, step + 1));
    let (status, body) = call(
        router,
        "EnableMFADevice",
        &[
            ("UserName", USER),
            ("SerialNumber", serial_number),
            ("AuthenticationCode1", &first),
            ("AuthenticationCode2", &second),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture EnableMFADevice must succeed, got {status}: {body}"
    );
}

/// RULE: a device no user holds is deleted, whatever path it was created under and whatever
/// it carries — the account's listing no longer reports it, and the serial names nothing
/// again, so a second delete of it is a missing entity.
#[tokio::test]
async fn a_device_no_user_holds_is_deleted() {
    for row in PROVISIONED {
        let router = router();
        // The account holds EVERY row's device, so what a delete removes is the one it
        // names rather than the only device there was.
        for provisioned in PROVISIONED {
            provision(&router, provisioned).await;
        }
        let serial_number = row.serial_number();

        let (status, body) = delete(&router, &serial_number).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the delete to be served, got {status}: {body}",
            row.case
        );

        let remaining = held_serials(&router, row.case).await;
        assert!(
            !remaining.contains(&serial_number),
            "case {:?}: expected the deleted device to be gone, got {remaining:?}",
            row.case
        );
        let mut expected: Vec<String> = PROVISIONED
            .iter()
            .map(Provisioned::serial_number)
            .filter(|held| *held != serial_number)
            .collect();
        expected.sort();
        assert_eq!(
            remaining, expected,
            "case {:?}: expected every other device to be left alone, got {remaining:?}",
            row.case
        );

        // The serial names nothing again: the second delete is a missing entity.
        let (status, body) = delete(&router, &serial_number).await;
        assert_eq!(
            status, 404,
            "case {:?}: expected the second delete to be refused, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {:?}: expected NoSuchEntity, got {:?}: {body}",
            row.case,
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            format!("MFA Device with serial number {serial_number} does not exist."),
            "case {:?}: expected the refusal to name the serial the request supplied, got {:?}",
            row.case,
            element(&body, "Message")
        );
    }
}

/// RULE: a device still enabled for a user is refused 409 `DeleteConflict` with the fixed
/// sentence of that class, and is NOT deleted — the account still holds it, assigned to the
/// same user.
#[tokio::test]
async fn a_device_a_user_still_holds_is_refused_as_a_delete_conflict() {
    for row in PROVISIONED {
        let router = router();
        given_user(&router, USER).await;
        let (serial_number, seed, _) = provision(&router, row).await;
        enable(&router, row.case, &serial_number, &seed).await;

        let (status, body) = delete(&router, &serial_number).await;
        assert_eq!(
            status, 409,
            "case {:?}: expected HTTP 409, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "DeleteConflict",
            "case {:?}: expected DeleteConflict, got {:?}: {body}",
            row.case,
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            DELETE_CONFLICT,
            "case {:?}: expected {DELETE_CONFLICT:?}, got {:?}",
            row.case,
            element(&body, "Message")
        );

        // The refused delete removed nothing: the device is still the user's.
        let held = held_serials(&router, row.case).await;
        assert_eq!(
            held,
            vec![serial_number.clone()],
            "case {:?}: expected the device to stay held, got {held:?}",
            row.case
        );
        let (status, listed) = call(
            &router,
            "ListVirtualMFADevices",
            &[("AssignmentStatus", "Assigned")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the assigned devices must be listable, got {status}: {listed}",
            row.case
        );
        assert_eq!(
            element(&listed, "UserName"),
            USER,
            "case {:?}: expected the device to stay assigned to its user, got {:?}: {listed}",
            row.case,
            element(&listed, "UserName")
        );
    }
}

/// The serials the absent-serial suite spans: values of every form a `SerialNumber` can
/// take, none of them naming a device the account holds, and both published length bounds of
/// `serialNumberType`.
fn absent_serials() -> Vec<(&'static str, String)> {
    let account = account_id();
    vec![
        (
            "a virtual MFA device ARN of this account naming no device",
            format!("arn:aws:iam::{account}:mfa/never-created"),
        ),
        (
            "a virtual MFA device ARN under a path naming no device",
            format!("arn:aws:iam::{account}:mfa/engineering/never-created"),
        ),
        (
            "a virtual MFA device ARN of another account",
            "arn:aws:iam::111122223333:mfa/other-account-device".to_string(),
        ),
        (
            "a virtual MFA device ARN in another partition",
            format!("arn:aws-cn:iam::{account}:mfa/other-partition-device"),
        ),
        (
            "a bare hardware token serial",
            "GAHT12345678".to_string(),
        ),
        (
            "the shortest serial the member admits",
            "A+=/:,.@-".to_string(),
        ),
        (
            "the longest serial the member admits",
            "s".repeat(MAX_SERIAL_CHARACTERS),
        ),
    ]
}

/// RULE: a serial the account holds no device under references a resource entity that does
/// not exist, so the delete is refused 404 `NoSuchEntity` with the message naming the serial
/// the request supplied, and the devices the account does hold are untouched.
#[tokio::test]
async fn a_serial_the_account_holds_no_device_under_is_refused_as_a_missing_entity() {
    let router = router();
    let mut expected: Vec<String> = Vec::new();
    for row in PROVISIONED {
        provision(&router, row).await;
        expected.push(row.serial_number());
    }
    expected.sort();

    for (case, serial_number) in absent_serials() {
        assert!(
            serial_number.chars().count() >= MIN_SERIAL_CHARACTERS,
            "case {case:?}: a row must carry a serial the member admits, got {serial_number:?}"
        );
        let (status, body) = delete(&router, &serial_number).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            format!("MFA Device with serial number {serial_number} does not exist."),
            "case {case:?}: expected the refusal to name the serial the request supplied, \
             got {:?}",
            element(&body, "Message")
        );
    }

    // None of the refusals removed anything.
    let held = held_serials(&router, "after the refusals").await;
    assert_eq!(
        held, expected,
        "a refused delete removes nothing, got {held:?}"
    );
}
