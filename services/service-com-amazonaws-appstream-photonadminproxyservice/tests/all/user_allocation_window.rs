// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT decides the outcome of a create of a user pool identity the
//! pool already holds.
//!
//! RULE: a create of a `(UserName, AuthenticationType)` the pool already holds claims a
//! username that is taken, so it is refused `ResourceAlreadyExistsException` (400) with
//! AWS's sentence and leaves the stored user exactly as it was — decided by this service's
//! own state alone, never by how many calls a caller made in between, by what they were, or
//! by how fast the host ran them.
//!
//! The rows differ along what the rule generalises over: the number and the kind of calls
//! between the create and the repeat (none, one read, several reads, a write then a read, a
//! create of another identity), the attributes the identity carries, and what the repeat
//! names beside the identity — the same attributes, none, or a different one.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness.

use serde_json::{json, Value};

use crate::refusal_messages::refusal;
use crate::stack_fleet_lifecycle::Emulator;

/// The only authentication type the user pool operations manage, which
/// `CreateUser.AuthenticationType` documents as "You must specify USERPOOL".
const USER_POOL: &str = "USERPOOL";

/// AWS's sentence for a username that is taken, read from the crate so this suite tracks
/// what the service states rather than a copy of it.
const ALREADY_EXISTS: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::USER_ALREADY_EXISTS_MESSAGE;

/// The `CreateUser` request for `user_name` carrying `extra` beside the identity.
fn create_request(user_name: &str, extra: &Value) -> Value {
    let mut request = json!({ "UserName": user_name, "AuthenticationType": USER_POOL });
    for (member, value) in extra.as_object().expect("the row carries members") {
        request[member] = value.clone();
    }
    request
}

/// Every user the pool reports.
async fn user_pool(emulator: &Emulator, case: &str) -> Vec<Value> {
    let payload = emulator
        .ok(
            case,
            "DescribeUsers",
            json!({ "AuthenticationType": USER_POOL }),
        )
        .await;
    payload
        .get("Users")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every user the pool reports for `user_name` — one for an identity allocated once.
async fn reported(emulator: &Emulator, case: &str, user_name: &str) -> Vec<Value> {
    user_pool(emulator, case)
        .await
        .into_iter()
        .filter(|user| user.get("UserName").and_then(Value::as_str) == Some(user_name))
        .collect()
}

/// Assert a create of `request` is refused as a username that is taken, and hand back the
/// sentence that refusal carried.
pub(crate) async fn refused_as_a_username_that_is_taken(
    emulator: &Emulator,
    case: &str,
    request: &Value,
) -> String {
    let message = refusal(
        emulator,
        case,
        "CreateUser",
        request.clone(),
        400,
        "ResourceAlreadyExistsException",
    )
    .await;
    assert_eq!(
        message, ALREADY_EXISTS,
        "case {case:?}: expected AWS's sentence {ALREADY_EXISTS:?}, got {message:?}"
    );
    message
}

/// What a row does between the create that allocates the identity and the repeat of it.
///
/// Each variant is a different amount and kind of work, which is what the rule generalises
/// over: none of it decides the repeat's outcome.
#[derive(Debug, Clone, Copy)]
enum Between {
    /// Nothing at all: the repeat follows the create directly.
    Nothing,
    /// Reads of the pool, which report the identity back to the caller.
    Reads(usize),
    /// A write to the user, then a read of it.
    DisableThenRead,
    /// A create of a DIFFERENT identity, which allocates one of its own.
    AnotherCreate,
}

impl Between {
    async fn apply(self, emulator: &Emulator, case: &str, user_name: &str) {
        match self {
            Between::Nothing => {}
            Between::Reads(times) => {
                for _ in 0..times {
                    user_pool(emulator, case).await;
                }
            }
            Between::DisableThenRead => {
                emulator
                    .ok(
                        case,
                        "DisableUser",
                        json!({ "UserName": user_name, "AuthenticationType": USER_POOL }),
                    )
                    .await;
                user_pool(emulator, case).await;
            }
            Between::AnotherCreate => {
                emulator
                    .ok(
                        case,
                        "CreateUser",
                        create_request("another.identity@example.com", &json!({})),
                    )
                    .await;
            }
        }
    }
}

#[tokio::test]
async fn a_repeat_of_a_held_identity_is_refused_however_much_work_precedes_it() {
    // Rows: what the created identity carries, and what the caller does between the create
    // and the repeat of it. Whatever that work is, the repeat is refused as a username that
    // is taken and the identity is still allocated exactly once, exactly as it was.
    for (case, user_name, created, between) in [
        (
            "nothing in between",
            "straight.after@example.com",
            json!({ "FirstName": "Ada", "LastName": "Lovelace" }),
            Between::Nothing,
        ),
        (
            "one read in between",
            "read.back@example.com",
            json!({ "MessageAction": "SUPPRESS" }),
            Between::Reads(1),
        ),
        (
            "eight reads in between",
            "read.back.again@example.com",
            json!({ "FirstName": "Grace" }),
            Between::Reads(8),
        ),
        (
            "a write and a read in between",
            "disabled.then.repeated@example.com",
            json!({ "LastName": "Hopper" }),
            Between::DisableThenRead,
        ),
        (
            "another identity allocated in between",
            "ユーザー@example.com",
            json!({}),
            Between::AnotherCreate,
        ),
    ] {
        let emulator = Emulator::new();
        emulator
            .ok(case, "CreateUser", create_request(user_name, &created))
            .await;
        let allocated = reported(&emulator, case, user_name).await;
        assert_eq!(
            allocated.len(),
            1,
            "case {case:?}: the create must have allocated exactly the one identity, got \
             {allocated:?}"
        );

        between.apply(&emulator, case, user_name).await;
        let before = reported(&emulator, case, user_name).await;

        let message =
            refused_as_a_username_that_is_taken(&emulator, case, &create_request(user_name, &created))
                .await;
        assert!(
            !message.contains(user_name),
            "case {case:?}: the sentence states that the username is taken without echoing \
             it, got {message:?}"
        );

        let after = reported(&emulator, case, user_name).await;
        assert_eq!(
            after.len(),
            1,
            "case {case:?}: the identity must still be allocated exactly once, got {after:?}"
        );
        assert_eq!(
            after, before,
            "case {case:?}: the refused create must leave the stored user exactly as it was, \
             got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_repeat_that_asks_for_a_different_user_is_refused_too() {
    // Rows: what the identity was created with, and the attribute the repeat names
    // differently — including one the stored record does not carry at all. A create that
    // asks for a user other than the one that is there claims a username that is taken just
    // as an identical one does.
    for (case, created, repeated) in [
        (
            "a different first name",
            json!({ "FirstName": "Ada", "LastName": "Lovelace" }),
            json!({ "FirstName": "Augusta", "LastName": "Lovelace" }),
        ),
        (
            "a different last name",
            json!({ "FirstName": "Ada", "LastName": "Lovelace" }),
            json!({ "LastName": "Byron" }),
        ),
        (
            "both names different",
            json!({ "FirstName": "Ada", "LastName": "Lovelace" }),
            json!({ "FirstName": "Augusta", "LastName": "Byron" }),
        ),
        (
            "a name the stored user does not carry",
            json!({}),
            json!({ "FirstName": "Augusta" }),
        ),
        (
            "nothing beside the identity",
            json!({ "FirstName": "Ada" }),
            json!({}),
        ),
    ] {
        let emulator = Emulator::new();
        let user_name = "conflicting@example.com";
        emulator
            .ok(case, "CreateUser", create_request(user_name, &created))
            .await;
        let before = reported(&emulator, case, user_name).await;

        let message = refused_as_a_username_that_is_taken(
            &emulator,
            case,
            &create_request(user_name, &repeated),
        )
        .await;
        assert!(
            !message.contains(user_name),
            "case {case:?}: the sentence states that the username is taken without echoing \
             it, got {message:?}"
        );

        let after = reported(&emulator, case, user_name).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused create must leave the stored user exactly as it was, \
             got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_create_does_not_free_the_username() {
    // The refusal is a property of the pool's contents, so it repeats: a run of creates of
    // a held identity is refused every time, and the stored user is untouched by all of
    // them.
    let case = "a run of repeated creates";
    let emulator = Emulator::new();
    let user_name = "settles@example.com";
    let request = create_request(user_name, &json!({ "MessageAction": "SUPPRESS" }));
    emulator.ok(case, "CreateUser", request.clone()).await;
    let allocated = reported(&emulator, case, user_name).await;

    for round in 0..3 {
        let message = refused_as_a_username_that_is_taken(&emulator, case, &request).await;
        assert_eq!(
            message, ALREADY_EXISTS,
            "case {case:?}: create {round} must carry AWS's sentence {ALREADY_EXISTS:?}, got \
             {message:?}"
        );
    }
    let settled = reported(&emulator, case, user_name).await;
    assert_eq!(
        settled, allocated,
        "case {case:?}: the refused creates must leave the stored user exactly as it was, \
         got {settled:?}"
    );
}
