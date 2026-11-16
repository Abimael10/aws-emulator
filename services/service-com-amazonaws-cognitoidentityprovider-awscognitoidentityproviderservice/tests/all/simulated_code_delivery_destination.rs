// SPDX-License-Identifier: CC0-1.0

//! The `CodeDeliveryDetails` a code-sending self-service operation reports for a user
//! the pool does not hold, through an app client that suppresses user-existence errors.
//!
//! The rule under test: a fabricated code-delivery report takes the address it reports
//! from the request's own `Username` — read as the address a code would have gone to and
//! masked exactly as a real destination is — so nothing in the answer is a literal that
//! is the same whatever the request named. The mask discloses the first character of an
//! address's local part and the first character of its domain, so an address-shaped
//! username gives each from its own part and a username carrying no domain stands for
//! both halves of the address.
//!
//! The rows span the forms a `Username` takes (`UsernameType`, length 1..128, pattern
//! `^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$`, from
//! `smithy_query(query="shape",shape="UsernameType")`): both published length bounds,
//! address-shaped and not, ASCII and not, digits and punctuation. Each row names the case
//! it asserts, and the table is driven through BOTH operations that fabricate a delivery.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use cognito_idp::state;

/// This service's SigV4 signing name, the 4th field of a credential scope.
const SIGNING_NAME: &str = "cognito-idp";

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix a
/// client writes.
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// The emulator's own endpoint, which is what an unsigned client is pointed at.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// This service's real regional endpoint host, which a signed call addresses.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The two self-service operations that answer a request naming a user nobody registered
/// with a REPORTED delivery rather than a refusal, once the app client suppresses
/// user-existence errors.
const FABRICATING_OPERATIONS: [&str; 2] = ["ForgotPassword", "ResendConfirmationCode"];

/// Usernames no request in this suite registers, as `(case, username, the `Destination`
/// the fabricated report owes it)`.
const GHOSTS: &[(&str, &str, &str)] = &[
    ("a plain name", "nobody-here", "n***@n***"),
    ("an address-shaped name", "ghost@example.test", "g***@e***"),
    (
        "an address-shaped name with a dotted local part and a subdomain",
        "first.last+tag@mail.corp.example.co.uk",
        "f***@m***",
    ),
    (
        "an address-shaped name in upper case",
        "GHOST@EXAMPLE.TEST",
        "G***@E***",
    ),
    ("the one-character length minimum", "z", "z***@z***"),
    ("a digits-only name", "90210", "9***@9***"),
    ("a phone-shaped name", "+15555550123", "+***@+***"),
    (
        "a name shaped like a generated sub",
        "11111111-2222-3333-4444-555555555555",
        "1***@1***",
    ),
    ("a non-ASCII name", "ユーザー-不在", "ユ***@ユ***"),
];

/// The shared engine every router in one test mounts.
fn engine() -> Arc<engine::InMemoryEngine> {
    Arc::new(engine::InMemoryEngine::new())
}

/// One emulator with just this service mounted over `engine`.
fn router(engine: &Arc<engine::InMemoryEngine>, authenticating: bool) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(engine)));
    router.set_request_authentication(authenticating);
    router
}

/// Whether a request carries a SigV4 credential scope.
#[derive(Clone, Copy, Debug)]
enum Addressing {
    /// A signed call, as the SDK sends every credentialed operation of this service.
    Signed,
    /// An unsigned call to the emulator's own endpoint: how the self-service
    /// code-sending operations are called.
    Anonymous,
}

/// Dispatch `operation` with `body`, addressed as `addressing` says, and return the
/// status and JSON response body.
async fn dispatch(
    router: &MultiServiceRouter,
    addressing: Addressing,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = match addressing {
        Addressing::Signed => ENDPOINT_HOST,
        Addressing::Anonymous => EMULATOR_HOST,
    };
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"));
    if matches!(addressing, Addressing::Signed) {
        request = request.header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=0000"
            ),
        );
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, payload)
}

/// Call `operation` signed, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = dispatch(router, Addressing::Signed, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// Create a pool that auto-verifies `email` and return its minted id.
async fn pool(router: &MultiServiceRouter, pool_name: &str) -> String {
    let created = ok(
        router,
        "CreateUserPool",
        &json!({ "PoolName": pool_name, "AutoVerifiedAttributes": ["email"] }),
    )
    .await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool must mint an id, got {created}"))
        .to_string()
}

/// Create an app client of `user_pool_id` whose `PreventUserExistenceErrors` is
/// `existence_errors`, and return its minted id.
async fn app_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_name: &str,
    existence_errors: &str,
) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": client_name,
            "PreventUserExistenceErrors": existence_errors,
        }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {created}"))
        .to_string()
}

/// Create `username` in `user_pool_id` with `address` as its VERIFIED email and a
/// permanent password, which is what makes it a user a reset message can be addressed to.
async fn admin_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    address: &str,
) {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [
                { "Name": "email", "Value": address },
                { "Name": "email_verified", "Value": "true" },
            ],
        }),
    )
    .await;
    ok(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
}

/// Whether `user_pool_id` holds a user named `username`, read out of the emulator's own
/// state: a fabricated delivery must register nobody.
fn pool_holds(engine: &Arc<engine::InMemoryEngine>, user_pool_id: &str, username: &str) -> bool {
    let service_state = state::service_state(&**engine).expect("this service's state");
    let guard = service_state.lock();
    guard
        .pool(user_pool_id)
        .and_then(|pool| pool.user(username))
        .is_some()
}

#[tokio::test]
async fn a_fabricated_delivery_reports_the_submitted_username_masked() {
    let engine = engine();
    let setup = router(&engine, false);
    let emulator = router(&engine, true);
    let user_pool_id = pool(&setup, "fabrication-pool").await;
    let hiding = app_client(&setup, &user_pool_id, "hiding", "ENABLED").await;

    let mut reported: Vec<(&str, String)> = Vec::new();
    for (case_name, username, destination) in GHOSTS {
        for operation in FABRICATING_OPERATIONS {
            let case = format!("{operation} for {case_name}");
            let (status, payload) = dispatch(
                &emulator,
                Addressing::Anonymous,
                operation,
                &json!({ "ClientId": hiding, "Username": username }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected a reported delivery, got {status} with {payload}"
            );
            let details = &payload["CodeDeliveryDetails"];
            assert_eq!(
                details["Destination"],
                json!(destination),
                "case {case:?}: expected the destination {destination:?} derived from \
                 {username:?}, got {payload}"
            );
            assert_eq!(
                (
                    details["AttributeName"].clone(),
                    details["DeliveryMedium"].clone()
                ),
                (json!("email"), json!("EMAIL")),
                "case {case:?}: expected an email delivery, got {payload}"
            );
            assert!(
                !pool_holds(&engine, &user_pool_id, username),
                "case {case:?}: a fabricated delivery registers nobody, yet {username:?} is \
                 now held by {user_pool_id}"
            );
            reported.push((case_name, details["Destination"].to_string()));
        }
    }

    // No reported destination is independent of the request: every row whose username
    // discloses different characters earns a different answer, so the answer cannot be a
    // literal shared by two usernames.
    for (left_case, left) in &reported {
        for (right_case, right) in &reported {
            if left_case == right_case {
                continue;
            }
            assert_ne!(
                left, right,
                "case {left_case:?} vs {right_case:?}: two usernames disclosing different \
                 characters must not share the destination {left}"
            );
        }
    }
}

#[tokio::test]
async fn a_real_delivery_reports_the_stored_address_and_a_fabricated_one_the_username() {
    let engine = engine();
    let setup = router(&engine, false);
    let emulator = router(&engine, true);
    let user_pool_id = pool(&setup, "comparison-pool").await;
    let hiding = app_client(&setup, &user_pool_id, "hiding", "ENABLED").await;

    // A user that DOES exist, whose stored address shares no disclosed character with
    // its username: the real path reports the address the code went to, so the two
    // paths differ in WHERE the value comes from, not in how it is rendered.
    let username = "registered-user";
    admin_user(&setup, &user_pool_id, username, "owner@mailbox.test").await;

    let payload = dispatch(
        &emulator,
        Addressing::Anonymous,
        "ForgotPassword",
        &json!({ "ClientId": hiding, "Username": username }),
    )
    .await;
    assert_eq!(
        payload.0, 200,
        "a registered user's reset is served, got {} with {}",
        payload.0, payload.1
    );
    assert_eq!(
        payload.1["CodeDeliveryDetails"]["Destination"],
        json!("o***@m***"),
        "a served reset reports the STORED address masked, got {}",
        payload.1
    );

    // The same operation for a username the pool does not hold reports the username
    // masked instead, through the same masking.
    let ghost = "owner-of-nothing";
    let fabricated = dispatch(
        &emulator,
        Addressing::Anonymous,
        "ForgotPassword",
        &json!({ "ClientId": hiding, "Username": ghost }),
    )
    .await;
    assert_eq!(
        fabricated.0, 200,
        "a ghost's reset is answered with a reported delivery, got {} with {}",
        fabricated.0, fabricated.1
    );
    assert_eq!(
        fabricated.1["CodeDeliveryDetails"]["Destination"],
        json!("o***@o***"),
        "a fabricated reset reports the SUBMITTED username masked, got {}",
        fabricated.1
    );

    // And a code is outstanding only for the user that exists: the fabricated answer
    // sent nothing.
    let service_state = state::service_state(&*engine).expect("this service's state");
    let guard = service_state.lock();
    let pool = guard.pool(&user_pool_id).expect("the created pool");
    assert!(
        pool.user(username)
            .and_then(|user| user.outstanding_code(&state::CodePurpose::PasswordReset))
            .is_some(),
        "the served reset mints the code it reported"
    );
    assert!(
        pool.user(ghost).is_none(),
        "the fabricated reset registers nobody, yet {ghost:?} is held by {user_pool_id}"
    );
}
