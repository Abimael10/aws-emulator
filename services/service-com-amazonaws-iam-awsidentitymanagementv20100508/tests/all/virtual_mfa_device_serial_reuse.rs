// SPDX-License-Identifier: CC0-1.0

//! What a deleted virtual MFA device leaves behind, end to end over the awsQuery wire:
//! nothing at all.
//!
//! RULE: `DeleteVirtualMFADevice` removes the whole record of the device its `SerialNumber`
//! names — "deletes a virtual MFA device" (`DeleteVirtualMFADevice`) — and a virtual device
//! is identified by its path and name together, that pair being the `mfa` ARN which IS the
//! serial ("for virtual MFA devices, the serial number is the same as the ARN",
//! `DeleteVirtualMFADeviceRequest$SerialNumber`). So after a delete that pair names nothing:
//! a create under it is served again rather than refused as the conflict a held pair is, and
//! what it mints is a NEW device — provisioned with its own seed, so the pass codes the
//! released device emitted authenticate nothing and only the new seed's pair enables it.
//!
//! This is the part of the delete that a listing alone cannot show: that the record went,
//! secret and all, rather than being hidden from the account's reads.
//!
//! The table's rows differ along what the rule generalises over: the path the device was
//! created under, the character classes of its name and how many tags it carries (the table
//! `tests/all/virtual_mfa_devices.rs` owns, so the fixture is written once), and HOW the
//! device came to be deletable — never enabled for anyone, or enabled for a user and then
//! deactivated, which is the state `DeactivateMFADevice` leaves a device in.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites in
//! this crate share.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, element, elements, error_code, router};
use crate::virtual_mfa_device_enablement::{current_step, given_user, pass_code};
use crate::virtual_mfa_devices::{borrowed, provision, Provisioned, PROVISIONED};

/// The IAM user the rows that enable a device before releasing it hold it as.
const USER: &str = "serial-reuser";

/// HOW a row's device came to be free of dependents before it was deleted.
#[derive(Debug, Clone, Copy)]
enum Released {
    /// No user ever held the device, so nothing had to be cleared.
    NeverEnabled,
    /// A user held the device and `DeactivateMFADevice` cleared the association, which is
    /// what "you must deactivate a user's virtual MFA device before you can delete it"
    /// (`DeleteVirtualMFADevice`) asks for.
    Deactivated,
}

/// The two codes a device provisioned with `seed` emits in consecutive time steps — the pair
/// `EnableMFADevice` authenticates against the seed the create answered with.
fn consecutive_codes(seed: &str) -> (String, String) {
    let step = current_step(std::time::SystemTime::now());
    (pass_code(seed, step), pass_code(seed, step + 1))
}

/// One `EnableMFADevice` call presenting the pair `seed` emits.
async fn enable(
    router: &MultiServiceRouter,
    serial_number: &str,
    seed: &str,
) -> (u16, String) {
    let (first, second) = consecutive_codes(seed);
    call(
        router,
        "EnableMFADevice",
        &[
            ("UserName", USER),
            ("SerialNumber", serial_number),
            ("AuthenticationCode1", &first),
            ("AuthenticationCode2", &second),
        ],
    )
    .await
}

/// The serial numbers the account holds, under the status that selects both kinds.
async fn held_serials(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = call(router, "ListVirtualMFADevices", &[("AssignmentStatus", "Any")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's devices must be listable, got {status}: {body}"
    );
    elements(&body, "SerialNumber")
}

/// Bring `row`'s device to the state `released` names, and answer its serial number and the
/// seed it was provisioned with. The account already holds [`USER`].
async fn given_released_device(
    router: &MultiServiceRouter,
    case: &str,
    row: &Provisioned,
    released: Released,
) -> (String, String) {
    let (serial_number, seed, _) = provision(router, row).await;
    if matches!(released, Released::Deactivated) {
        let (status, body) = enable(router, &serial_number, &seed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture EnableMFADevice must succeed, got {status}: {body}"
        );
        let (status, body) = call(
            router,
            "DeactivateMFADevice",
            &[("UserName", USER), ("SerialNumber", &serial_number)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture DeactivateMFADevice must succeed, got {status}: {body}"
        );
    }
    (serial_number, seed)
}

/// RULE: the path and name a delete released name nothing, so a create under them is served
/// again — not the `EntityAlreadyExists` conflict a pair the account still holds answers —
/// and the device it mints carries the same serial with a seed of its own.
#[tokio::test]
async fn a_deleted_path_and_name_can_be_created_again_as_a_new_device() {
    for row in PROVISIONED {
        for released in [Released::NeverEnabled, Released::Deactivated] {
            let case = format!("{} / {released:?}", row.case);
            let router = router();
            given_user(&router, USER).await;
            let (serial_number, first_seed) =
                given_released_device(&router, &case, row, released).await;

            // While the account holds the pair, a create under it is the conflict.
            let params = row.params();
            let (status, body) = call(&router, "CreateVirtualMFADevice", &borrowed(&params)).await;
            assert_eq!(
                status, 409,
                "case {case:?}: expected the create to conflict while the pair is held, \
                 got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "EntityAlreadyExists",
                "case {case:?}: expected EntityAlreadyExists, got {:?}: {body}",
                error_code(&body)
            );

            let (status, body) = call(
                &router,
                "DeleteVirtualMFADevice",
                &[("SerialNumber", &serial_number)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the delete to be served, got {status}: {body}"
            );
            let held = held_serials(&router, &case).await;
            assert!(
                held.is_empty(),
                "case {case:?}: expected the account to hold no device, got {held:?}"
            );

            // The pair names nothing, so the create is served again — `provision` is the
            // shared fixture that asserts the 200 and reads the serial and the seed back.
            let (recreated, second_seed, _) = provision(&router, row).await;
            assert_eq!(
                recreated, serial_number,
                "case {case:?}: expected the same path and name to make the same serial, \
                 got {recreated:?}"
            );
            assert_ne!(
                second_seed, first_seed,
                "case {case:?}: expected the new device to carry a seed of its own, \
                 got the released device's seed again"
            );
            let held = held_serials(&router, &case).await;
            assert_eq!(
                held,
                vec![serial_number.clone()],
                "case {case:?}: expected the account to hold exactly the new device, got {held:?}"
            );
        }
    }
}

/// RULE: the seed went with the record, so the released device's pass codes authenticate
/// nothing — the pair it emitted is refused `InvalidAuthenticationCode` on the device created
/// under the same path and name, which only the NEW seed's pair enables.
#[tokio::test]
async fn the_released_devices_pass_codes_do_not_enable_the_new_device() {
    for row in PROVISIONED {
        for released in [Released::NeverEnabled, Released::Deactivated] {
            let case = format!("{} / {released:?}", row.case);
            let router = router();
            given_user(&router, USER).await;
            let (serial_number, first_seed) =
                given_released_device(&router, &case, row, released).await;

            let (status, body) = call(
                &router,
                "DeleteVirtualMFADevice",
                &[("SerialNumber", &serial_number)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the delete to be served, got {status}: {body}"
            );

            let (_, second_seed, _) = provision(&router, row).await;

            // The released device's own pair proves nothing about the device now held under
            // that serial.
            let (status, body) = enable(&router, &serial_number, &first_seed).await;
            assert_eq!(
                status, 403,
                "case {case:?}: expected the released device's codes to be refused, \
                 got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidAuthenticationCode",
                "case {case:?}: expected InvalidAuthenticationCode, got {:?}: {body}",
                error_code(&body)
            );
            let (status, listed) = call(
                &router,
                "ListVirtualMFADevices",
                &[("AssignmentStatus", "Assigned")],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the account's devices must be listable, got {status}: {listed}"
            );
            assert!(
                elements(&listed, "SerialNumber").is_empty(),
                "case {case:?}: expected the refused enable to assign nothing, got {:?}",
                elements(&listed, "SerialNumber")
            );

            // The new seed's pair is the one that enables it.
            let (status, body) = enable(&router, &serial_number, &second_seed).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the new device's own codes to enable it, \
                 got {status}: {body}"
            );
            let (status, listed) = call(
                &router,
                "ListVirtualMFADevices",
                &[("AssignmentStatus", "Assigned")],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the account's devices must be listable, got {status}: {listed}"
            );
            assert_eq!(
                elements(&listed, "SerialNumber"),
                vec![serial_number.clone()],
                "case {case:?}: expected the new device to be the assigned one, got {:?}",
                elements(&listed, "SerialNumber")
            );
            assert_eq!(
                element(&listed, "UserName"),
                USER,
                "case {case:?}: expected the device to report its holder, got {:?}: {listed}",
                element(&listed, "UserName")
            );

            // And the device a user now holds is blocked from deletion again, which is the
            // same dependent the first delete had to be free of.
            let (status, body) = call(
                &router,
                "DeleteVirtualMFADevice",
                &[("SerialNumber", &serial_number)],
            )
            .await;
            assert_eq!(
                status, 409,
                "case {case:?}: expected the held device's delete to be blocked, \
                 got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "DeleteConflict",
                "case {case:?}: expected DeleteConflict, got {:?}: {body}",
                error_code(&body)
            );
        }
    }
}
