// SPDX-License-Identifier: CC0-1.0

//! How `EnableMFADevice` words the refusal of an enrolment the account ALREADY holds, end to
//! end over the awsQuery wire.
//!
//! The rule under test: **re-enabling a device that is already enrolled for the named user is
//! an attempt to create a resource that already exists, so it is refused
//! `EntityAlreadyExists` (HTTP 409) with the `MFA Device for user ` … ` already exists.`
//! sentence of IAM's entity-already-exists family — fixed text whose ONE variable part is the
//! user name, with nothing of the device in it — and nothing about the enrolment changes.**
//! `EnableMFADevice` "enables the specified MFA device and associates it with the specified IAM
//! user" and models `EntityAlreadyExistsException` ("it attempted to create a resource that
//! already exists", awsQueryError `EntityAlreadyExists` / HTTP 409), so the entity whose
//! creation the second call attempts is the ENROLMENT — a device held BY a user — which is the
//! entity the sentence names.
//!
//! The table's rows differ along what the rule generalises over: the user name that fills the
//! sentence's one slot (its length, at both ends of the range a user the account holds can
//! carry, and its form over the alphabet `^[\w+=,.@-]+$` — digits, case and each punctuation
//! character the pattern admits) and the device whose serial must NOT appear in it (device
//! names of very different lengths, so the serials they yield differ in length too). The
//! recorded case is one row of it.
//!
//! Every row also pins what the refusal left behind: the user still holds exactly the one
//! device, and once `DeactivateMFADevice` clears the enrolment the identical enable is served
//! again — so the refusal is about the enrolment the account holds and not about the device or
//! the codes.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns; the device fixtures are
//! the ones `tests/all/virtual_mfa_device_enablement.rs` owns, so a device is provisioned and
//! authenticated exactly one way.

use std::time::SystemTime;

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, element, elements, error_code, router};
use crate::virtual_mfa_device_enablement::{current_step, given_device, given_user, pass_code};

/// One `EnableMFADevice` call presenting the pair the device's own seed emits for the current
/// time step and the next one, so a refusal below reports the enrolment and never the codes.
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

/// One row: the user the enrolment belongs to and the device it holds.
struct Row {
    case: &'static str,
    /// The user name, which is the sentence's only variable part.
    user_name: String,
    /// The virtual MFA device's name, which the sentence must say nothing about.
    device_name: String,
}

/// The rows, built here because the bound cases are names of a computed length.
///
/// The lengths come from the model: the name the sentence carries is that of a user the account
/// holds, minted through `CreateUserRequest$UserName` (`userNameType`, `@length 1..64` over
/// `^[\w+=,.@-]+$`) and named by `EnableMFADeviceRequest$UserName` (`existingUserNameType`,
/// `@length 1..128`), so 1 and 64 are the ends of the range a name in the sentence can have; a
/// virtual device's serial is its ARN (`VirtualMFADevice$SerialNumber`), which
/// `serialNumberType`'s `@length 9..256` caps — so the longest device name below keeps the
/// serial it yields inside that bound.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a plain user name",
            user_name: "mfa-reenrolment-operator".to_string(),
            device_name: "reenrolment-phone".to_string(),
        },
        Row {
            case: "a user name of one character, the shortest the model admits",
            user_name: "u".to_string(),
            device_name: "single-character-user-phone".to_string(),
        },
        Row {
            case: "a user name of 64 characters, the longest a user can be created with",
            user_name: "u".repeat(64),
            device_name: "longest-user-phone".to_string(),
        },
        Row {
            case: "a user name spelling every punctuation character the pattern admits",
            user_name: "user+name=with,every.allowed@char-and_digits.0123456789".to_string(),
            device_name: "punctuated-user-phone".to_string(),
        },
        Row {
            case: "a mixed-case user name",
            user_name: "MixedCase.MFA.Operator".to_string(),
            device_name: "mixed-case-user-phone".to_string(),
        },
        Row {
            case: "a device whose name is 200 characters, so its serial is a long one",
            user_name: "long-serial-operator".to_string(),
            device_name: "d".repeat(200),
        },
        Row {
            case: "a device whose name is three characters, so its serial is a short one",
            user_name: "short-serial-operator".to_string(),
            // Spelled from characters the sentence itself never uses, so the check that the
            // device appears nowhere in the message cannot match by accident.
            device_name: "xq7".to_string(),
        },
    ]
}

/// RULE: an enrolment the account already holds is refused `EntityAlreadyExists` (HTTP 409)
/// with `MFA Device for user <UserName> already exists.` — the user name is the sentence's only
/// variable part and the device appears in it nowhere — and the refusal changes nothing: the
/// user keeps that one device, and the identical enable is served again once the enrolment has
/// been deactivated.
#[tokio::test]
async fn re_enabling_an_enrolled_device_is_refused_in_iams_already_exists_wording() {
    for row in rows() {
        let case = row.case;
        let user_name = row.user_name.as_str();
        let router = router();
        given_user(&router, user_name).await;
        let (serial_number, seed) = given_device(&router, &row.device_name).await;

        let (status, body) = enable(&router, user_name, &serial_number, &seed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first enrolment must be served, got {status}: {body}"
        );
        assert_eq!(
            enrolled_serials(&router, user_name).await,
            vec![serial_number.clone()],
            "case {case:?}: the user must hold exactly the device it enrolled"
        );

        // The same device, for the same user, with a freshly computed pair of codes.
        let (status, body) = enable(&router, user_name, &serial_number, &seed).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for the enrolment the account already holds, got \
             {status}: {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "EntityAlreadyExists",
            "case {case:?}: expected the error code EntityAlreadyExists, got {code:?}: {body}"
        );
        let expected = format!("MFA Device for user {user_name} already exists.");
        let message = element(&body, "Message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}: {body}"
        );
        assert!(
            !message.contains(&serial_number) && !message.contains(row.device_name.as_str()),
            "case {case:?}: the sentence names no device, got {message:?}"
        );

        // The refusal left the enrolment exactly as it was.
        assert_eq!(
            enrolled_serials(&router, user_name).await,
            vec![serial_number.clone()],
            "case {case:?}: the refused enable must leave the one enrolment in place"
        );

        // And it was the ENROLMENT the refusal was about: clear it, and the identical call is
        // served again.
        let (status, body) = call(
            &router,
            "DeactivateMFADevice",
            &[("UserName", user_name), ("SerialNumber", &serial_number)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the enrolment must be deactivable, got {status}: {body}"
        );
        let (status, body) = enable(&router, user_name, &serial_number, &seed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the device must be enrollable again once deactivated, got {status}: \
             {body}"
        );
        assert_eq!(
            enrolled_serials(&router, user_name).await,
            vec![serial_number],
            "case {case:?}: the re-enrolment must be the device the user holds"
        );
    }
}
