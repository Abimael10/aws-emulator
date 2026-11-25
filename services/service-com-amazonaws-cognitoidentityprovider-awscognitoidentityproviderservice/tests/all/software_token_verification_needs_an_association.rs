// SPDX-License-Identifier: CC0-1.0

//! Verifying a software token the user never set up, over the wire (`POST /`
//! with `X-Amz-Target: AWSCognitoIdentityProviderService.VerifySoftwareToken` /
//! `.AssociateSoftwareToken` / `.AdminDeleteSoftwareToken`).
//!
//! The rule under test: **a verification for a user that holds NO private key
//! has no software token to verify, so it is refused
//! `SoftwareTokenMFANotFoundException` — the factor does not exist for that user
//! — whatever code the request presents; a user that HAS associated a key is
//! judged on the code instead, and a code that key does not emit is the
//! `EnableSoftwareTokenMFAException` mismatch
//! (`smithy_query(query="operation",operation="VerifySoftwareToken")` declares
//! both).** A setup "begins ... with a unique private key that Amazon Cognito
//! generates and returns in the API response" (`AssociateSoftwareToken` in this
//! service's model), so the association is what gives a user a software token at
//! all — and `AdminDeleteSoftwareToken` takes it away again, which puts the user
//! back in exactly the state a user that never associated one is in.
//!
//! The rows differ along whether the user holds a key (never associated, holding
//! one, holding one a delete removed) and along the code presented (one of a key
//! this service never minted, six digits no key emits, and the code the stored
//! key emits now).
//!
//! The one-time passwords are computed here from RFC 6238's definition
//! (`crate::software_token_setup::one_time_password`), so nothing but the service
//! decides whether a code is accepted. Everything else is real: the pool, its app
//! client, the user, its access token and its association all come from this
//! service's own operations.

use std::time::SystemTime;

use serde_json::json;

use super::software_token_setup::{
    GOOD_PASSWORD, Emulator, app_client, current_step, dispatch, emulator, error_code,
    error_message, ok, ok_self_service, one_time_password, pool,
};

/// The user every row sets a token up for.
const USERNAME: &str = "ada";

/// Every refusal of this service is answered 400 and names its class in the
/// body.
const REFUSED: u16 = 400;

/// The class and sentence a verification for a user with no software token
/// earns.
const NOT_FOUND: &str = "SoftwareTokenMFANotFoundException";
const NOT_FOUND_MESSAGE: &str = "Software Token MFA does not exist for the user.";

/// The class and sentence a code the user's stored key does not emit earns.
const CODE_MISMATCH: &str = "EnableSoftwareTokenMFAException";
const CODE_MISMATCH_MESSAGE: &str = "Code mismatch";

/// A key this service never minted, spelled as the base32 a `SecretCode` is
/// spelled in: the row that presents a code of it presents a code no stored key
/// emits, from a key no association handed out.
const FOREIGN_KEY: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";

/// What a row gives the user before it presents a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setup {
    /// Nothing: no `AssociateSoftwareToken` ever ran for this user.
    NeverAssociated,
    /// One association, whose key the row's code may be computed from.
    Associated,
    /// One association an `AdminDeleteSoftwareToken` then removed.
    AssociatedThenDeleted,
}

/// Which code a row presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// The code the key this service stored for the user emits now.
    StoredKeysCode,
    /// The code a key this service never minted emits now.
    ForeignKeysCode,
    /// Six digits no key of this exchange emits.
    SixZeroes,
}

/// One row: the setup the user holds, the code presented, and the answer owed —
/// `None` for a row the verification must SERVE.
struct Row {
    case: &'static str,
    setup: Setup,
    presented: Presented,
    refusal: Option<(&'static str, &'static str)>,
}

/// The rows span the two facts that decide the answer: whether the user holds a
/// key at all, and whether the presented code is one that key emits.
const ROWS: &[Row] = &[
    Row {
        case: "a user that never associated a key, presenting a foreign key's code",
        setup: Setup::NeverAssociated,
        presented: Presented::ForeignKeysCode,
        refusal: Some((NOT_FOUND, NOT_FOUND_MESSAGE)),
    },
    Row {
        case: "a user that never associated a key, presenting six zeroes",
        setup: Setup::NeverAssociated,
        presented: Presented::SixZeroes,
        refusal: Some((NOT_FOUND, NOT_FOUND_MESSAGE)),
    },
    Row {
        case: "a user whose key a delete removed, presenting that key's code",
        setup: Setup::AssociatedThenDeleted,
        presented: Presented::StoredKeysCode,
        refusal: Some((NOT_FOUND, NOT_FOUND_MESSAGE)),
    },
    Row {
        case: "a user holding a key, presenting a foreign key's code",
        setup: Setup::Associated,
        presented: Presented::ForeignKeysCode,
        refusal: Some((CODE_MISMATCH, CODE_MISMATCH_MESSAGE)),
    },
    Row {
        case: "a user holding a key, presenting six zeroes",
        setup: Setup::Associated,
        presented: Presented::SixZeroes,
        refusal: Some((CODE_MISMATCH, CODE_MISMATCH_MESSAGE)),
    },
    Row {
        case: "a user holding a key, presenting that key's own code",
        setup: Setup::Associated,
        presented: Presented::StoredKeysCode,
        refusal: None,
    },
];

/// A pool with TOTP MFA offered, one app client, one user and that user's access
/// token.
struct Fixture {
    user_pool_id: String,
    access_token: String,
}

/// Build [`Fixture`] in a pool named `name`.
async fn fixture(emulator: &Emulator, name: &str) -> Fixture {
    let user_pool_id = pool(emulator, name, json!({})).await;
    ok(
        emulator,
        "SetUserPoolMfaConfig",
        &json!({
            "UserPoolId": user_pool_id,
            "MfaConfiguration": "OPTIONAL",
            "SoftwareTokenMfaConfiguration": { "Enabled": true },
        }),
    )
    .await;
    let client_id = app_client(emulator, &user_pool_id, "verify-client", json!({})).await;
    ok(
        emulator,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": USERNAME }),
    )
    .await;
    ok(
        emulator,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let signed_in = ok_self_service(
        emulator,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    let access_token = signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a sign-in with no factor activated issues a token: {signed_in}"))
        .to_string();
    Fixture {
        user_pool_id,
        access_token,
    }
}

/// Only a user that HOLDS a private key is judged on the code it presents; a
/// user that holds none is told the factor does not exist, and each case reads
/// back through the verification itself.
#[tokio::test]
async fn a_verification_needs_a_key_the_association_minted() {
    for (index, row) in ROWS.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let fixture = fixture(&emulator, &format!("verify-pool-{index}")).await;

        let stored_key = match row.setup {
            Setup::NeverAssociated => None,
            Setup::Associated | Setup::AssociatedThenDeleted => {
                let associated = ok_self_service(
                    &emulator,
                    "AssociateSoftwareToken",
                    &json!({ "AccessToken": fixture.access_token }),
                )
                .await;
                let key = associated["SecretCode"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the association reports a key, got {associated}")
                    })
                    .to_string();
                if row.setup == Setup::AssociatedThenDeleted {
                    ok(
                        &emulator,
                        "AdminDeleteSoftwareToken",
                        &json!({ "UserPoolId": fixture.user_pool_id, "Username": USERNAME }),
                    )
                    .await;
                }
                Some(key)
            }
        };

        let step = current_step(SystemTime::now());
        let code = match row.presented {
            Presented::StoredKeysCode => one_time_password(
                stored_key.as_deref().unwrap_or_else(|| {
                    panic!("case {case:?}: this row associates the key its code comes from")
                }),
                step,
            ),
            Presented::ForeignKeysCode => one_time_password(FOREIGN_KEY, step),
            Presented::SixZeroes => "000000".to_string(),
        };

        let (status, payload) = dispatch(
            &emulator,
            "VerifySoftwareToken",
            &json!({ "AccessToken": fixture.access_token, "UserCode": code }),
            false,
        )
        .await;
        match row.refusal {
            Some((expected_code, expected_message)) => {
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (REFUSED, expected_code, expected_message),
                    "case {case:?}: the verification must be refused, got {status} with {payload}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the stored key's own code registers it, got {status} with \
                     {payload}"
                );
                assert_eq!(
                    payload["Status"].as_str(),
                    Some("SUCCESS"),
                    "case {case:?}: the verification reports its success, got {payload}"
                );
            }
        }
    }
}
