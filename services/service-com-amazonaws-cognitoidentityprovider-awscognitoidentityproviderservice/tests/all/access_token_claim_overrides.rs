// SPDX-License-Identifier: CC0-1.0

//! What the `accessTokenGeneration` section of a second-contract pre-token-generation answer
//! changes about the ACCESS token a self-service sign-in mints.
//!
//! The rule under test: a pool whose `PreTokenGenerationConfig` names `LambdaVersion`
//! `V2_0` (`PreTokenGenerationVersionConfigType$LambdaVersion`, this service's model) has its
//! function answer with `response.claimsAndScopeOverrideDetails.accessTokenGeneration`, and
//! that section is an INPUT to the minting of the access token rather than decoration it may
//! ignore — a trigger "can modify the outcome of the operations that invoked them"
//! (`LambdaConfigType`, this service's model). So in the token the sign-in hands back, every
//! claim the section gives a value for carries exactly that value in the JSON form the
//! function stated it, every claim it suppresses is gone, and the `scope` claim is the
//! sign-in's own grant plus the scopes it added, less the ones it suppressed; a claim this
//! service reserves for itself is never the function's to rewrite, and the other token of the
//! triple is not customised by this section at all.
//!
//! The dimension the table below spans is the one the rule generalises over and the sibling
//! suite (`crate::pre_token_generation_overrides`, which owns the contract-and-member table
//! over the administrative sign-in) does not: the FORM and the COUNT of what a handler puts
//! in `claimsToAddOrOverride` — text, a number, a boolean, a list, an object, several at
//! once — together with the suppression, scope and reserved-claim rows read on the
//! self-service `InitiateAuth` path, and the same section applied to the access token a
//! REFRESH of that sign-in mints.
//!
//! The scope rows assert an ORDERED string because the order is part of the rule the minting
//! states: the sign-in's own grant first, in the order it authorized the scopes, then the
//! ones the answer added that the claim does not carry yet, in the order the function named
//! them (`TokenOverrides::access_scopes`) — so a row may read the claim as the exact text a
//! verifier splits on rather than as a set.
//!
//! Every row runs a REAL `python3.12` deployment package: Cognito and Lambda are mounted on
//! one engine and the pool's trigger names the function whose handler answers with the
//! document the call's `ClientMetadata` asked it for, so each claim read back out of a
//! decoded token is that function's own return value. Nothing here asserts a value baked into
//! the service: the pool id, the client id, the function ARN, the group and the user all come
//! back from the state the setup calls created.

use serde_json::{json, Value};

use crate::pre_token_generation_overrides::{
    assert_claims, asking_for, claims, grouped_user, ok, pool_with_client, router,
    trigger_function, Expect, GOOD_PASSWORD, GRANTED_SCOPE, STORED_GROUP,
};

/// One row of the table: the section a handler answers with, and what the tokens the
/// generation mints carry because of it.
struct Row {
    case: &'static str,
    /// The `accessTokenGeneration` object of the answer.
    section: Value,
    /// What the minted ACCESS token carries.
    access_token: Vec<(&'static str, Expect)>,
    /// The claims the minted IDENTITY token must NOT carry, since this section customises
    /// the access token alone.
    id_token_absent: Vec<&'static str>,
}

/// The answer document that carries `section` as the second contract's access-token member.
fn answering(section: &Value) -> Value {
    json!({
        "claimsAndScopeOverrideDetails": { "accessTokenGeneration": section },
    })
}

/// The access and identity token of a minted triple, as the sign-in answered with them.
fn minted(case: &str, payload: &Value) -> (Value, Value) {
    let result = &payload["AuthenticationResult"];
    let access = result["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an AccessToken, got {payload}"));
    let id = result["IdToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an IdToken, got {payload}"));
    (
        claims(case, "access", access),
        claims(case, "identity", id),
    )
}

#[tokio::test]
async fn the_access_token_section_of_a_v2_answer_decides_the_minted_access_token() {
    let router = router();
    let function = trigger_function(&router, "pre-token-access-claims").await;
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "access-claims",
        json!({
            "PreTokenGenerationConfig": { "LambdaVersion": "V2_0", "LambdaArn": function },
        }),
    )
    .await;
    let username = "access-claim-signer";
    grouped_user(&router, &user_pool_id, username).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "a claim the handler states as text",
            section: json!({ "claimsToAddOrOverride": { "tenant": "acme" } }),
            access_token: vec![("tenant", Expect::Is(json!("acme")))],
            id_token_absent: vec!["tenant"],
        },
        Row {
            case: "a claim the handler states as a number",
            section: json!({ "claimsToAddOrOverride": { "seats": 42 } }),
            access_token: vec![("seats", Expect::Is(json!(42)))],
            id_token_absent: vec!["seats"],
        },
        Row {
            case: "a claim the handler states as a boolean",
            section: json!({ "claimsToAddOrOverride": { "trial": false } }),
            access_token: vec![("trial", Expect::Is(json!(false)))],
            id_token_absent: vec!["trial"],
        },
        Row {
            case: "a claim the handler states as a list",
            section: json!({
                "claimsToAddOrOverride": { "entitlements": ["reports", "exports"] },
            }),
            access_token: vec![(
                "entitlements",
                Expect::Is(json!(["reports", "exports"])),
            )],
            id_token_absent: vec!["entitlements"],
        },
        Row {
            case: "a claim the handler states as an object",
            section: json!({
                "claimsToAddOrOverride": { "limits": { "daily": 10, "plan": "gold" } },
            }),
            access_token: vec![(
                "limits",
                Expect::Is(json!({ "daily": 10, "plan": "gold" })),
            )],
            id_token_absent: vec!["limits"],
        },
        Row {
            case: "several claims in one answer",
            section: json!({
                "claimsToAddOrOverride": {
                    "tenant": "beta",
                    "seats": 7,
                    "entitlements": ["exports"],
                },
            }),
            access_token: vec![
                ("tenant", Expect::Is(json!("beta"))),
                ("seats", Expect::Is(json!(7))),
                ("entitlements", Expect::Is(json!(["exports"]))),
                // The claims the computed token carries are still there beside them.
                ("token_use", Expect::Is(json!("access"))),
                ("cognito:groups", Expect::Is(json!([STORED_GROUP]))),
                ("scope", Expect::Is(json!(GRANTED_SCOPE))),
            ],
            id_token_absent: vec!["tenant", "seats", "entitlements"],
        },
        Row {
            case: "a claim the same answer both gives a value for and suppresses",
            section: json!({
                "claimsToAddOrOverride": { "tier": "gold" },
                "claimsToSuppress": ["tier"],
            }),
            access_token: vec![("tier", Expect::Is(json!("gold")))],
            id_token_absent: vec!["tier"],
        },
        Row {
            case: "a generated claim the answer suppresses, beside a claim it adds",
            section: json!({
                "claimsToAddOrOverride": { "tenant": "gamma" },
                "claimsToSuppress": ["cognito:groups"],
            }),
            access_token: vec![
                ("tenant", Expect::Is(json!("gamma"))),
                ("cognito:groups", Expect::Absent),
            ],
            // The other token's claims are suppressed on their own, so its membership stands.
            id_token_absent: vec!["tenant"],
        },
        Row {
            case: "a claim name this service reserves for itself",
            section: json!({
                "claimsToAddOrOverride": { "sub": "not-the-user", "scope": "rewritten" },
                "claimsToSuppress": ["token_use"],
            }),
            access_token: vec![
                ("sub", Expect::Other(json!("not-the-user"))),
                ("scope", Expect::Is(json!(GRANTED_SCOPE))),
                ("token_use", Expect::Is(json!("access"))),
            ],
            id_token_absent: vec![],
        },
        Row {
            case: "scopes the answer adds and the grant's own scope suppressed",
            section: json!({
                "scopesToAdd": ["store/read", "store/write"],
                "scopesToSuppress": [GRANTED_SCOPE],
            }),
            access_token: vec![("scope", Expect::Is(json!("store/read store/write")))],
            id_token_absent: vec![],
        },
        Row {
            case: "a scope suppression naming a scope the grant does not carry",
            section: json!({ "scopesToSuppress": ["store/read"] }),
            access_token: vec![("scope", Expect::Is(json!(GRANTED_SCOPE)))],
            id_token_absent: vec![],
        },
        Row {
            case: "an answer whose access-token section asks for nothing",
            section: json!({}),
            access_token: vec![
                ("scope", Expect::Is(json!(GRANTED_SCOPE))),
                ("cognito:groups", Expect::Is(json!([STORED_GROUP]))),
            ],
            id_token_absent: vec![],
        },
    ];

    for row in &rows {
        let signed_in = ok(
            &router,
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
                "ClientMetadata": asking_for(&answering(&row.section)),
            }),
        )
        .await;
        let (access_claims, id_claims) = minted(row.case, &signed_in);
        assert_claims(row.case, "access", &access_claims, &row.access_token);
        let absent: Vec<(&str, Expect)> = row
            .id_token_absent
            .iter()
            .map(|claim| (*claim, Expect::Absent))
            .collect();
        assert_claims(row.case, "identity", &id_claims, &absent);
    }

    // The same section customises the access token of a REFRESH's generation, which is a
    // generation of its own: its own invocation's answer decides its own token.
    let case = "a refresh of the same sign-in";
    let first = ok(
        &router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
            "ClientMetadata": asking_for(&answering(&json!({
                "claimsToAddOrOverride": { "tenant": "before-refresh" },
            }))),
        }),
    )
    .await;
    let (first_access, _) = minted(case, &first);
    assert_claims(
        case,
        "access",
        &first_access,
        &[("tenant", Expect::Is(json!("before-refresh")))],
    );
    let refresh_token = first["AuthenticationResult"]["RefreshToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a refresh token, got {first}"))
        .to_string();
    let refreshed = ok(
        &router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "REFRESH_TOKEN_AUTH",
            "AuthParameters": { "REFRESH_TOKEN": refresh_token },
            "ClientMetadata": asking_for(&answering(&json!({
                "claimsToAddOrOverride": { "tenant": "after-refresh" },
                "claimsToSuppress": ["cognito:groups"],
                "scopesToAdd": ["store/read"],
            }))),
        }),
    )
    .await;
    let (refreshed_access, _) = minted(case, &refreshed);
    assert_claims(
        case,
        "access",
        &refreshed_access,
        &[
            ("tenant", Expect::Is(json!("after-refresh"))),
            ("cognito:groups", Expect::Absent),
            (
                "scope",
                Expect::Is(json!(format!("{GRANTED_SCOPE} store/read"))),
            ),
        ],
    );
}
