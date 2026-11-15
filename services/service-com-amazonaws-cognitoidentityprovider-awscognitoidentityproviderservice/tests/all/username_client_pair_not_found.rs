// SPDX-License-Identifier: CC0-1.0

//! How an operation that resolves the user it acts on from the `Username` and the `ClientId`
//! of the request TOGETHER refuses a pair it cannot resolve, over the wire.
//!
//! The rule: the self-service operations whose input models a required `Username` beside a
//! required `ClientId` and NO `UserPoolId` — so the app client is what names the pool to
//! look the user up in
//! (`smithy_query(query="operation",operation="ConfirmSignUp,ConfirmForgotPassword,ForgotPassword,ResendConfirmationCode")`)
//! — report a pair that resolves to no user as that COMBINATION not being found, whichever
//! half of the pair failed. An app client no stored pool holds leaves the pair unresolved
//! exactly as a username no pool of that client holds does, so both are answered by the one
//! fixed sentence, which quotes neither half: `ClientIdType` and `UsernameType` are both
//! `@sensitive` (`smithy_query(query="shape",shape="ClientIdType,UsernameType")`), and a
//! sentence naming the id a request carried would both echo it and say which half was wrong.
//!
//! The rows span what the rule generalises over: the operation, and the form of the client
//! id that resolves to nothing — spanning `ClientIdType`'s published bounds (length 1..128)
//! and its `^[\w+]+$`, plus a live app client's id with one more admitted character.
//!
//! The contrast is the boundary: an operation that ADDRESSES an app client as the resource
//! it works on, and the sign-up that MINTS a user rather than resolving one, each name the
//! app client their request asked for, because neither has a pair to resolve.
//!
//! The harness is the one `log_delivery_configuration.rs` builds for this service, reached
//! through the test crate root rather than copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::log_delivery_configuration::{call, error_code, error_message, ok, router};

/// The one sentence an operation that resolves the user from `Username` and `ClientId`
/// together answers for a pair it cannot resolve.
const USERNAME_CLIENT_NOT_FOUND: &str = "Username/client id combination not found.";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// Another one, for the reset half of the recovery flow.
const RESET_PASSWORD: &str = "Res3tPass!2024";

/// A `ConfirmationCodeType` value (length 1..2048, `^[\S]+$`) that no code this emulator
/// minted can be, so a confirmation carrying it resolves nothing.
const UNISSUED_CODE: &str = "000000";

/// A pool with `extra` merged into its configuration, and its minted id.
async fn user_pool(router: &MultiServiceRouter, pool_name: &str, extra: Value) -> String {
    let mut body = json!({ "PoolName": pool_name });
    if let (Some(target), Value::Object(members)) = (body.as_object_mut(), extra) {
        for (member, value) in members {
            target.insert(member, value);
        }
    }
    let created = ok(router, "CreateUserPool", &body).await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool must mint an id, got {created}"))
        .to_string()
}

/// An app client of `user_pool_id`, and its minted id.
async fn app_client(router: &MultiServiceRouter, user_pool_id: &str, client_name: &str) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientName": client_name }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {created}"))
        .to_string()
}

/// The four operations under test, each asked about `username` through `client_id`:
/// (operation, request body).
fn pair_requests(client_id: &str, username: &str) -> [(&'static str, Value); 4] {
    [
        (
            "ConfirmSignUp",
            json!({
                "ClientId": client_id,
                "Username": username,
                "ConfirmationCode": UNISSUED_CODE,
            }),
        ),
        (
            "ResendConfirmationCode",
            json!({ "ClientId": client_id, "Username": username }),
        ),
        (
            "ForgotPassword",
            json!({ "ClientId": client_id, "Username": username }),
        ),
        (
            "ConfirmForgotPassword",
            json!({
                "ClientId": client_id,
                "Username": username,
                "ConfirmationCode": UNISSUED_CODE,
                "Password": GOOD_PASSWORD,
            }),
        ),
    ]
}

#[tokio::test]
async fn a_client_id_no_pool_holds_is_refused_as_an_unresolved_pair() {
    let router = router();
    let user_pool_id = user_pool(
        &router,
        "pair-lookup-pool",
        json!({ "AutoVerifiedAttributes": ["email"] }),
    )
    .await;
    let live = app_client(&router, &user_pool_id, "live").await;

    // The ids span `ClientIdType`: a plain id no create minted, both published length
    // bounds, an id carrying the `+` its expression admits, and the live id with one more
    // admitted character — a near miss that resolves to nothing all the same.
    let ways: Vec<(String, String)> = vec![
        (
            "a plain id no create minted".to_string(),
            "1h57kf5cpq17m0eml12eukdi1o".to_string(),
        ),
        (
            "an id at the published length maximum, 128".to_string(),
            "z".repeat(128),
        ),
        (
            "an id at the published length minimum, 1".to_string(),
            "z".to_string(),
        ),
        (
            "an id carrying the + its expression admits".to_string(),
            "neverminted+1234".to_string(),
        ),
        (
            "the live app client's id with one more character".to_string(),
            format!("{live}z"),
        ),
    ];

    let username = "jane.doe";
    for (way, client_id) in &ways {
        for (operation, body) in pair_requests(client_id, username) {
            let case = format!("{operation} with {way}");
            let (status, payload) = call(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&payload)),
                (400, "ResourceNotFoundException"),
                "case {case:?}: expected a 400 ResourceNotFoundException, got {status} \
                 with {payload}"
            );
            let message = error_message(&payload);
            assert_eq!(
                message, USERNAME_CLIENT_NOT_FOUND,
                "case {case:?}: expected {USERNAME_CLIENT_NOT_FOUND:?}, got {message:?}"
            );
            // The sentence discloses neither half of the pair, so no caller can read the
            // absence of one app client, or of one username, out of it. A one-character id
            // is a substring of ordinary prose, so the echo check is made on the ids long
            // enough to mean something.
            assert!(
                !message.contains(username),
                "case {case:?}: the refusal must quote no username, got {message:?}"
            );
            if client_id.len() >= 4 {
                assert!(
                    !message.contains(client_id.as_str()),
                    "case {case:?}: the refusal must quote no client id, got {message:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_username_half_of_the_pair_earns_the_same_sentence() {
    let router = router();
    let user_pool_id = user_pool(
        &router,
        "pair-username-pool",
        json!({ "AutoVerifiedAttributes": ["email"] }),
    )
    .await;
    let live = app_client(&router, &user_pool_id, "live").await;

    // The other half of the same pair: an app client that DOES resolve, named beside a
    // username no pool of that client holds. The sentence is the one above, so neither half
    // is distinguishable from the other by what the refusal says.
    for username in ["absent-user", "absent@example.test", "ユーザー-不在"] {
        for (operation, body) in pair_requests(&live, username) {
            let case = format!("{operation} with the username {username:?}");
            let (status, payload) = call(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&payload)),
                (400, "UserNotFoundException"),
                "case {case:?}: expected a 400 UserNotFoundException, got {status} with {payload}"
            );
            let message = error_message(&payload);
            assert_eq!(
                message, USERNAME_CLIENT_NOT_FOUND,
                "case {case:?}: expected {USERNAME_CLIENT_NOT_FOUND:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_operation_that_addresses_the_app_client_itself_names_it() {
    let router = router();
    let user_pool_id = user_pool(&router, "pair-contrast-pool", json!({})).await;
    let absent = "neverminted1234";

    // The boundary: a request that works ON the app client resolves no pair, so its
    // refusal reports the resource it was asked for — and so does the sign-up, which mints
    // a user rather than resolving one.
    let rows: Vec<(&str, Value)> = vec![
        (
            "DeleteUserPoolClient",
            json!({ "UserPoolId": &user_pool_id, "ClientId": absent }),
        ),
        (
            "SignUp",
            json!({
                "ClientId": absent,
                "Username": "jane.doe",
                "Password": GOOD_PASSWORD,
            }),
        ),
    ];
    for (operation, body) in rows {
        let (status, payload) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, "ResourceNotFoundException"),
            "case {operation:?}: expected a 400 ResourceNotFoundException, got {status} \
             with {payload}"
        );
        let message = error_message(&payload);
        let expected = format!("User pool client {absent} does not exist.");
        assert_eq!(
            message, expected,
            "case {operation:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn the_pair_a_pool_does_hold_is_still_served() {
    let router = router();
    let user_pool_id = user_pool(
        &router,
        "pair-served-pool",
        json!({ "AutoVerifiedAttributes": ["email"] }),
    )
    .await;
    let live = app_client(&router, &user_pool_id, "live").await;
    let username = "jane.doe";

    // A user that holds its own password and a verified email: the state a self-service
    // reset needs, reached through the administrative operations.
    ok(
        &router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": &user_pool_id,
            "Username": username,
            "UserAttributes": [{ "Name": "email", "Value": "jane@example.test" }],
        }),
    )
    .await;
    ok(
        &router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": &user_pool_id,
            "Username": username,
            "Password": RESET_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    ok(
        &router,
        "AdminUpdateUserAttributes",
        &json!({
            "UserPoolId": &user_pool_id,
            "Username": username,
            "UserAttributes": [{ "Name": "email_verified", "Value": "true" }],
        }),
    )
    .await;

    // The refusals above answer an unresolved pair and not every request: the same
    // operation, asked for a pair this pool DOES hold, reports the delivery it made.
    let requested = ok(
        &router,
        "ForgotPassword",
        &json!({ "ClientId": &live, "Username": username }),
    )
    .await;
    assert_eq!(
        requested["CodeDeliveryDetails"]["AttributeName"],
        json!("email"),
        "a served reset reports the attribute it addressed, got {requested}"
    );

    // And the user is still the one the pool holds, read back through an implemented
    // reader: a refused pair changed nothing and the served one minted no second user.
    let read_back = ok(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": &user_pool_id, "Username": username }),
    )
    .await;
    assert_eq!(
        read_back["UserStatus"],
        json!("CONFIRMED"),
        "the user reads back in the state the administrative writes left, got {read_back}"
    );
}
