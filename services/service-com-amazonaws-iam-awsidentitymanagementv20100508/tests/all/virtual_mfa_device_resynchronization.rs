// SPDX-License-Identifier: CC0-1.0

//! Resynchronising one of the account's virtual MFA devices, end to end over the awsQuery
//! wire.
//!
//! The rules being asserted:
//!
//! * `ResyncMFADevice` synchronises the device its `SerialNumber` names with the clock the
//!   presented pass codes were emitted by — "synchronizes the specified MFA device with its
//!   IAM resource object" — and only when the two codes are the pass codes that device emits
//!   in two consecutive time steps, looked for over the resynchronisation span, which is
//!   wider than the allowance `EnableMFADevice` authenticates within: a device is
//!   resynchronised exactly because its clock has drifted out of that one;
//! * a pair the device emits in no step of that span authenticates nothing: the call is
//!   refused HTTP 403 `InvalidAuthenticationCode` with AWS's fixed sentence for the class,
//!   and the device stays associated with the user it was associated with;
//! * a pass code is a ONE-TIME password, so a pair that has already synchronised the device
//!   does not synchronise it a second time — which is what resolves two simultaneous resyncs
//!   of one device into one success and one refusal;
//! * both modelled outcomes are ANSWERED by the call that made them, one after the other, in
//!   the sequence a client drives: a refusal leaves nothing pending and the success that
//!   follows it needs no wall-clock span to have passed;
//! * what the request ADDRESSES has to exist and be that user's: a user the account does not
//!   hold, a serial it holds no device under, and a device another user (or nobody) holds are
//!   all the entity that does not exist, refused HTTP 404 `NoSuchEntity`.
//!
//! The suite computes the pass codes itself, from RFC 6238's definition over the seed the
//! create answered with — the arithmetic a holder of the device does — so a wrong algorithm
//! in the service cannot be the reason a row passes. Those helpers, and the user and device
//! fixtures, are the ones `tests/all/virtual_mfa_device_enablement.rs` owns.
//!
//! The code table's rows differ along what the rule generalises over: how far the device's
//! clock stands from this one (five offsets inside the resynchronisation span, in both
//! directions, and three far outside it), and the shape of a pair that is not two codes in a
//! row (the same code twice, the pair reversed, a pair of another device's seed).
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites in
//! this crate share.

use std::time::SystemTime;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, element, elements, error_code, router, router_with_engine};
use crate::virtual_mfa_device_enablement::{current_step, given_device, given_user, pass_code};

/// The IAM user the devices are enabled for.
const USER: &str = "mfa-holder";

/// A second user, so a row can address a device that is not its own.
const OTHER_USER: &str = "mfa-bystander";

/// The sentence IAM refuses a pair of codes a device's seed does not answer with: entirely
/// fixed text, quoting neither code and naming neither the device nor the user.
const INVALID_AUTHENTICATION_CODE: &str = "Authentication code for device is not valid.";

/// One `ResyncMFADevice` call.
async fn resync(
    router: &MultiServiceRouter,
    user_name: &str,
    serial_number: &str,
    codes: (&str, &str),
) -> (u16, String) {
    call(
        router,
        "ResyncMFADevice",
        &[
            ("UserName", user_name),
            ("SerialNumber", serial_number),
            ("AuthenticationCode1", codes.0),
            ("AuthenticationCode2", codes.1),
        ],
    )
    .await
}

/// Provision a virtual MFA device named `name` and enable it for `user_name`, answering its
/// serial number and seed: the state a resynchronisation acts on.
async fn given_enabled_device(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    name: &str,
) -> (String, String) {
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
    (serial_number, seed)
}

/// The account's ASSIGNED devices as the listing reports them, which is where a device's
/// association with its user is observable: the body, so a caller can read both the serials
/// and the users out of it.
async fn assigned_devices(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = call(
        router,
        "ListVirtualMFADevices",
        &[("AssignmentStatus", "Assigned")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's devices must be listable, got {status}: {body}"
    );
    body
}

/// WHICH pair of codes a row presents.
#[derive(Debug, Clone, Copy)]
enum Codes {
    /// The pair the device emits in the two consecutive steps starting `offset` steps from
    /// the one this instant falls in: a device whose clock runs that far ahead (positive) or
    /// behind (negative).
    DriftedBy(i64),
    /// The same code twice, which is one code rather than two in a row.
    Repeated,
    /// The pair in the wrong order.
    Reversed,
    /// A pair of ANOTHER device's seed, emitted in this very step: a valid pass code pair,
    /// of the wrong device.
    OfAnotherDevice,
}

impl Codes {
    /// The pair as this row presents it, for a device provisioned with `seed` and a second
    /// device provisioned with `other_seed`.
    fn pair(self, seed: &str, other_seed: &str) -> (String, String) {
        let step = current_step(SystemTime::now());
        match self {
            Self::DriftedBy(offset) => {
                let step = step.saturating_add_signed(offset);
                (pass_code(seed, step), pass_code(seed, step + 1))
            }
            Self::Repeated => {
                let code = pass_code(seed, step);
                (code.clone(), code)
            }
            Self::Reversed => (pass_code(seed, step + 1), pass_code(seed, step)),
            Self::OfAnotherDevice => (
                pass_code(other_seed, step),
                pass_code(other_seed, step + 1),
            ),
        }
    }
}

/// One row of the code table: what is presented, and whether the device is synchronised by
/// it.
struct Row {
    case: &'static str,
    codes: Codes,
    /// Whether the resynchronisation is served (HTTP 200) rather than refused 403.
    served: bool,
}

/// The rows. Offsets inside the resynchronisation span are served; a pair the device emits
/// far outside it, or a pair that is not two of its codes in a row, is not.
///
/// The offsets rows are held two steps clear of the span's edge in both directions, so a
/// step boundary crossed between computing the codes and the call deciding them cannot move
/// a row from one side of the span to the other.
const ROWS: &[Row] = &[
    Row {
        case: "the pair the device emits now",
        codes: Codes::DriftedBy(0),
        served: true,
    },
    Row {
        case: "a device one step ahead",
        codes: Codes::DriftedBy(1),
        served: true,
    },
    Row {
        case: "a device one step behind",
        codes: Codes::DriftedBy(-1),
        served: true,
    },
    Row {
        case: "a device two steps ahead",
        codes: Codes::DriftedBy(2),
        served: true,
    },
    Row {
        case: "a device two steps behind",
        codes: Codes::DriftedBy(-2),
        served: true,
    },
    Row {
        case: "a device ten steps ahead",
        codes: Codes::DriftedBy(10),
        served: false,
    },
    Row {
        case: "a device ten steps behind",
        codes: Codes::DriftedBy(-10),
        served: false,
    },
    Row {
        case: "a pair from an unrelated instant",
        codes: Codes::DriftedBy(500),
        served: false,
    },
    Row {
        case: "the same code twice",
        codes: Codes::Repeated,
        served: false,
    },
    Row {
        case: "the pair in the wrong order",
        codes: Codes::Reversed,
        served: false,
    },
    Row {
        case: "a pair of another device's seed",
        codes: Codes::OfAnotherDevice,
        served: false,
    },
];

/// RULE: a device is synchronised only by two of ITS pass codes in a row, emitted within the
/// resynchronisation span of this instant; any other pair is refused 403
/// `InvalidAuthenticationCode` with AWS's fixed sentence, and either way the device stays
/// associated with its user.
#[tokio::test]
async fn only_a_pair_the_device_emits_in_the_resynchronisation_span_synchronises_it() {
    for (row_index, row) in ROWS.iter().enumerate() {
        let router = router();
        given_user(&router, USER).await;
        let (serial_number, seed) =
            given_enabled_device(&router, row.case, USER, &format!("device-{row_index}")).await;
        // The second device a row of another device's seed takes its codes from. It is
        // created but never enabled, so the account's ASSIGNED devices stay exactly the one
        // under test.
        let (_, other_seed) = given_device(&router, &format!("other-device-{row_index}")).await;
        let (first, second) = row.codes.pair(&seed, &other_seed);

        let (status, body) = resync(&router, USER, &serial_number, (&first, &second)).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {:?}: expected the resynchronisation to be served, got {status}: {body}",
                row.case
            );
        } else {
            assert_eq!(
                status, 403,
                "case {:?}: expected HTTP 403, got {status}: {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                "InvalidAuthenticationCode",
                "case {:?}: expected InvalidAuthenticationCode, got {:?}: {body}",
                row.case,
                error_code(&body)
            );
            assert_eq!(
                element(&body, "Message"),
                INVALID_AUTHENTICATION_CODE,
                "case {:?}: expected {INVALID_AUTHENTICATION_CODE:?}, got {:?}",
                row.case,
                element(&body, "Message")
            );
        }

        // Served or refused, the device is still the user's: a resynchronisation changes
        // where the clock is known to stand, not whose device it is.
        let listed = assigned_devices(&router, row.case).await;
        assert_eq!(
            elements(&listed, "SerialNumber"),
            vec![serial_number.clone()],
            "case {:?}: expected the device to stay assigned, got {:?}",
            row.case,
            elements(&listed, "SerialNumber")
        );
        assert_eq!(
            element(&listed, "UserName"),
            USER,
            "case {:?}: expected the device to stay the user's, got {:?}: {listed}",
            row.case,
            element(&listed, "UserName")
        );
    }
}

/// RULE: a pass code is a ONE-TIME password, so the pair that synchronised a device does not
/// synchronise it again — the second call presenting it is refused in the invalid-code class
/// while a pair the device emits LATER is served. That is what makes two simultaneous
/// resyncs of one device resolve as one success and one refusal, and it is observable in the
/// step the account records for the device.
#[tokio::test]
async fn the_pair_that_synchronised_a_device_does_not_synchronise_it_again() {
    let case = "a repeated pair";
    let (router, engine) = router_with_engine();
    given_user(&router, USER).await;
    let (serial_number, seed) = given_enabled_device(&router, case, USER, "repeat-device").await;

    let step = current_step(SystemTime::now());
    let (first, second) = (pass_code(&seed, step), pass_code(&seed, step + 1));

    // The device carries no synchronisation yet: nothing has resynchronised it.
    assert_eq!(
        synchronized_step(&engine, &serial_number),
        None,
        "case {case:?}: a device no resynchronisation has been made for carries no \
         synchronised step, got {:?}",
        synchronized_step(&engine, &serial_number)
    );

    let (status, body) = resync(&router, USER, &serial_number, (&first, &second)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the first resynchronisation to be served, got {status}: {body}"
    );
    // Both codes of the pair are consumed, so the device is synchronised THROUGH the step
    // the second one was emitted in.
    assert_eq!(
        synchronized_step(&engine, &serial_number),
        Some(step + 1),
        "case {case:?}: expected the device synchronised through step {}, got {:?}",
        step + 1,
        synchronized_step(&engine, &serial_number)
    );

    // The same pair again: already-consumed codes synchronise nothing.
    let (status, body) = resync(&router, USER, &serial_number, (&first, &second)).await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected the repeated pair to be refused, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidAuthenticationCode",
        "case {case:?}: expected InvalidAuthenticationCode, got {:?}: {body}",
        error_code(&body)
    );
    assert_eq!(
        element(&body, "Message"),
        INVALID_AUTHENTICATION_CODE,
        "case {case:?}: expected {INVALID_AUTHENTICATION_CODE:?}, got {:?}",
        element(&body, "Message")
    );
    assert_eq!(
        synchronized_step(&engine, &serial_number),
        Some(step + 1),
        "case {case:?}: a refused resynchronisation records nothing, got {:?}",
        synchronized_step(&engine, &serial_number)
    );

    // A pair the device emits LATER is served, and moves the recorded step on: what is
    // refused is the reuse, not every further resynchronisation.
    let later = step + 2;
    let (third, fourth) = (pass_code(&seed, later), pass_code(&seed, later + 1));
    let (status, body) = resync(&router, USER, &serial_number, (&third, &fourth)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a later pair to be served, got {status}: {body}"
    );
    assert_eq!(
        synchronized_step(&engine, &serial_number),
        Some(later + 1),
        "case {case:?}: expected the device synchronised through step {}, got {:?}",
        later + 1,
        synchronized_step(&engine, &serial_number)
    );
}

/// RULE: both modelled outcomes of a resynchronisation are ANSWERED, one after the other,
/// in the sequence a client drives: a wrong pair earns the modelled refusal and a pair the
/// device emits now is served — each answered by the call that made it, with nothing left
/// pending, and the device stays its user's across both.
///
/// The whole operation contract in one sequence, because what a client sees is the SEQUENCE:
/// the refusal does not leave the next call unanswerable, and the success does not depend on
/// any wall-clock span having passed first.
#[tokio::test]
async fn the_refusal_and_the_success_are_each_answered_in_one_sequence() {
    let case = "the contract sequence";
    let router = router();
    given_user(&router, USER).await;
    let (serial_number, seed) = given_enabled_device(&router, case, USER, "sequence-device").await;

    // A pair of this device's codes from an unrelated instant: valid six-digit codes that no
    // step of the resynchronisation span emits, so the refusal is about the codes alone.
    let step = current_step(SystemTime::now());
    let stale = step.saturating_add(500);
    let (wrong_first, wrong_second) = (pass_code(&seed, stale), pass_code(&seed, stale + 1));
    let (status, refused) = resync(
        &router,
        USER,
        &serial_number,
        (&wrong_first, &wrong_second),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused)),
        (403, "InvalidAuthenticationCode".to_string()),
        "case {case:?}: expected the modelled refusal, got {status}: {refused}"
    );
    assert_eq!(
        element(&refused, "Message"),
        INVALID_AUTHENTICATION_CODE,
        "case {case:?}: expected {INVALID_AUTHENTICATION_CODE:?}, got {:?}",
        element(&refused, "Message")
    );

    // And the very next call, presenting the pair the device emits now, is served — the
    // refusal left nothing behind that a later caller has to wait out.
    let step = current_step(SystemTime::now());
    let (first, second) = (pass_code(&seed, step), pass_code(&seed, step + 1));
    let (status, served) = resync(&router, USER, &serial_number, (&first, &second)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the resynchronisation to be served, got {status}: {served}"
    );

    // Across both answers the device is still the user's.
    let listed = assigned_devices(&router, case).await;
    assert_eq!(
        (
            elements(&listed, "SerialNumber"),
            element(&listed, "UserName")
        ),
        (vec![serial_number.clone()], USER.to_string()),
        "case {case:?}: expected the device to stay the user's, got {listed}"
    );
}

/// The step the account records the device under `serial_number` as synchronised through, read
/// from the service's OWN store: the operation's output shape is `Unit` and no read of a
/// device reports the step, so this is where the synchronisation it wrote is observable
/// beyond the refusal of a reused pair.
fn synchronized_step(engine: &engine::InMemoryEngine, serial_number: &str) -> Option<u64> {
    let store = state::service_state(engine).expect("the IAM store");
    let recorded = store
        .lock()
        .virtual_mfa_device(serial_number)
        .and_then(|device| device.synchronized_step);
    recorded
}

/// RULE: what the request ADDRESSES has to exist and be the named user's — a user the account
/// does not hold, a serial it holds no device under, a device another user holds and a device
/// nobody holds are all the entity that does not exist, each refused 404 `NoSuchEntity` with
/// the sentence of its own class, and nothing is synchronised.
#[tokio::test]
async fn an_absent_entity_and_another_user_s_device_are_refused_as_missing_entities() {
    let case = "addressing";
    let (router, engine) = router_with_engine();
    given_user(&router, USER).await;
    given_user(&router, OTHER_USER).await;
    let (serial_number, seed) = given_enabled_device(&router, case, USER, "addressed-device").await;
    // A device the account holds but nobody has enabled.
    let (free_serial, _) = given_device(&router, "unassigned-device").await;
    let step = current_step(SystemTime::now());
    let (first, second) = (pass_code(&seed, step), pass_code(&seed, step + 1));

    let absent_serial = format!("{serial_number}-absent");
    let rows: [(&str, &str, &str, String); 4] = [
        (
            "a user the account does not hold",
            "nobody",
            serial_number.as_str(),
            "The user with name nobody cannot be found.".to_string(),
        ),
        (
            "a serial the account holds no device under",
            USER,
            absent_serial.as_str(),
            format!("MFA Device with serial number {absent_serial} does not exist."),
        ),
        (
            "a device another user holds",
            OTHER_USER,
            serial_number.as_str(),
            format!("MFA Device {serial_number} cannot be found for user {OTHER_USER}."),
        ),
        (
            "a device nobody holds",
            USER,
            free_serial.as_str(),
            format!("MFA Device {free_serial} cannot be found for user {USER}."),
        ),
    ];

    for (row, user_name, serial, expected) in rows {
        let (status, body) = resync(&router, user_name, serial, (&first, &second)).await;
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

    // None of the refusals synchronised anything, and the device the rows addressed is
    // still its user's.
    assert_eq!(
        synchronized_step(&engine, &serial_number),
        None,
        "case {case:?}: a refused resynchronisation records nothing, got {:?}",
        synchronized_step(&engine, &serial_number)
    );
    let listed = assigned_devices(&router, case).await;
    assert_eq!(
        elements(&listed, "SerialNumber"),
        vec![serial_number.clone()],
        "case {case:?}: expected the device to stay assigned, got {:?}",
        elements(&listed, "SerialNumber")
    );
}
