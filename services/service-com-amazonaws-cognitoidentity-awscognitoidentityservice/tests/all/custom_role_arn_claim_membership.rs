// SPDX-License-Identifier: CC0-1.0

//! What becomes of a vend naming a `CustomRoleArn` the exchange's identity provider did not
//! offer it.
//!
//! RULE: `CustomRoleArn` is "the Amazon Resource Name (ARN) of the role to be assumed when
//! MULTIPLE ROLES WERE RECEIVED IN THE TOKEN from the identity provider"
//! (`GetCredentialsForIdentityInput$CustomRoleArn`, this service's model), so the roles such
//! a request may pick from are the roles the presented token itself states — the
//! `cognito:roles` and `cognito:preferred_role` claims a Cognito identity-provider token
//! carries its roles in (`RoleMapping$Type`) — and naming any other role, a role the POOL
//! stores as its own `authenticated` or `unauthenticated` role included, is a role this
//! caller may not assume. That is an authorization decision about the token presented, so it
//! is refused with the modelled `NotAuthorizedException`
//! (`smithy_query(query="operation",operation="GetCredentialsForIdentity")` declares it with
//! `httpError` 403) and the message names the role the request asked for. It is NOT
//! `InvalidIdentityPoolConfigurationException`, which is for a pool that "has no
//! authenticated role configured, or STS returned an error response" (that error's own doc):
//! the controls below show the same pool vending for an in-claim role and resolving its
//! mapping for the bare call, so its configuration is usable.
//!
//! The decision is the request's own, not the mapping's: it is taken before the pool's
//! `RoleMappings` resolve anything, so a mapping stating `Deny` and one stating
//! `AuthenticatedRole` answer the same `CustomRoleArn` the same way.
//!
//! The rows differ along what the rule generalises over: WHERE the named role comes from (a
//! single-valued group claim, one of several values of it, the preference claim, the pool's
//! own two roles, a role no side names), the COUNT of roles the token states (none, one,
//! two, three), the resolution the mapping states, and the ARN named — each refused row
//! names a different one, which is the message's one variable part.
//!
//! Every refusal of this service travels with HTTP 400 whatever `@httpError` its shape is
//! annotated with (`crate::service`'s own status rule owns that), the error class travelling
//! in the body's `__type`; these rows assert the class and the sentence.
//!
//! The fixture is `mapped_role_resolution`'s, driven through `vended_role_session`'s
//! harness: this service and STS on ONE shared engine behind the same `MultiServiceRouter`
//! the production router builds, with the IAM roles the assume's decision reads seeded into
//! that same engine.

use serde_json::{json, Value};

use crate::mapped_role_resolution::{
    identity_token, pool, seeded_roles, set_roles, user_pool_claims, Role, USER_POOL_PROVIDER,
};
use crate::vended_role_session::{element, expected_session_arn, vended, Emulator};

/// The values `AmbiguousRoleResolutionType` admits, from the model
/// (`smithy_query(query="shape",shape="AmbiguousRoleResolutionType")`). Spelled as literals
/// here so the wire-visible values are pinned by the test, not by the code under test.
const AUTHENTICATED_ROLE: &str = "AuthenticatedRole";
const DENY: &str = "Deny";

/// The status every refusal of this service travels with.
const REFUSED_STATUS: u16 = 400;

/// The message the refusal of a `CustomRoleArn` the presented token did not state carries —
/// spelled out here, so the wire-visible wording is pinned by the test rather than by the
/// code under test.
///
/// The fixed text is `CustomRoleARN: ` … ` is not present in the token in the cognito:roles
/// claim` and the one variable part is the role ARN the request named.
fn not_in_token_message(role_arn: &str) -> String {
    format!("CustomRoleARN: {role_arn} is not present in the token in the cognito:roles claim")
}

/// The message the refusal of a vend a role mapping DENIED carries, as the sibling suite on
/// this fixture states it: the control that tells a denied RESOLUTION from a refused
/// `CustomRoleArn`.
fn denied_message(provider: &str) -> String {
    format!("The ambiguous role mapping rules for: {provider} denied this request.")
}

/// A `Token` role mapping of the user-pool provider, stating `resolution`: the type that
/// "will use `cognito:roles` and `cognito:preferred_role` claims from the Cognito identity
/// provider token to map groups to roles" (`RoleMapping$Type`, this service's model).
fn token_mapping(resolution: &str) -> Value {
    json!({USER_POOL_PROVIDER: {"Type": "Token", "AmbiguousRoleResolution": resolution}})
}

/// What a row expects of its vend.
enum Outcome<'a> {
    /// A session of this role is vended.
    Session(&'a Role),
    /// Nothing is vended: the named role is one the presented token did not state, so the
    /// call is refused `NotAuthorizedException` naming that role.
    NotAuthorized,
}

/// One row: the case, the resolution the mapping states, the group claims the presented
/// token carries, the `CustomRoleArn` the request names, and what the vend must come to.
struct Row<'a> {
    case: &'a str,
    resolution: &'a str,
    claims: Value,
    custom_role_arn: &'a str,
    outcome: Outcome<'a>,
}

/// Vend for a fresh identity of `pool_id` presenting `logins`, naming `custom_role_arn` when
/// the row names one.
async fn vend(
    emulator: &Emulator,
    pool_id: &str,
    logins: &Value,
    custom_role_arn: Option<&str>,
) -> (u16, Value) {
    let identity_id = emulator.identity(pool_id, Some(logins.clone())).await;
    let mut request = json!({"IdentityId": &identity_id, "Logins": logins.clone()});
    if let Some(custom_role_arn) = custom_role_arn {
        request["CustomRoleArn"] = json!(custom_role_arn);
    }
    emulator.call("GetCredentialsForIdentity", &request).await
}

/// The error class an awsJson failure names in its body.
fn error_code(body: &Value) -> String {
    body["__type"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn a_custom_role_arn_the_token_does_not_state_is_refused_not_authorized() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "custom-role-claim-membership").await;
    let roles = seeded_roles(
        &emulator,
        &pool_id,
        &[
            "claim-auth",
            "claim-unauth",
            "claim-group-a",
            "claim-group-b",
            "claim-group-c",
            "claim-ungrouped",
        ],
    );
    let (authenticated, unauthenticated) = (&roles[0], &roles[1]);
    let (group_a, group_b, group_c) = (&roles[2], &roles[3], &roles[4]);
    // A role of the same account, with the same trust policy, that no group of the user
    // grants and the pool's configuration does not name.
    let ungrouped = &roles[5];

    let rows = [
        Row {
            case: "the token's single cognito:roles entry, under Deny",
            resolution: DENY,
            claims: json!({"cognito:roles": [&group_a.arn]}),
            custom_role_arn: &group_a.arn,
            outcome: Outcome::Session(group_a),
        },
        Row {
            case: "one of two cognito:roles entries: the documented several-roles case",
            resolution: DENY,
            claims: json!({"cognito:roles": [&group_a.arn, &group_b.arn]}),
            custom_role_arn: &group_b.arn,
            outcome: Outcome::Session(group_b),
        },
        Row {
            case: "the last of three cognito:roles entries, under AuthenticatedRole",
            resolution: AUTHENTICATED_ROLE,
            claims: json!({"cognito:roles": [&group_a.arn, &group_b.arn, &group_c.arn]}),
            custom_role_arn: &group_c.arn,
            outcome: Outcome::Session(group_c),
        },
        Row {
            case: "a role the token states only in cognito:preferred_role",
            resolution: DENY,
            claims: json!({"cognito:preferred_role": &group_c.arn}),
            custom_role_arn: &group_c.arn,
            outcome: Outcome::Session(group_c),
        },
        Row {
            case: "a role no group of the user grants, under Deny",
            resolution: DENY,
            claims: json!({"cognito:roles": [&group_a.arn, &group_b.arn]}),
            custom_role_arn: &ungrouped.arn,
            outcome: Outcome::NotAuthorized,
        },
        Row {
            case: "that same role under AuthenticatedRole: the resolution does not decide it",
            resolution: AUTHENTICATED_ROLE,
            claims: json!({"cognito:roles": [&group_a.arn, &group_b.arn]}),
            custom_role_arn: &ungrouped.arn,
            outcome: Outcome::NotAuthorized,
        },
        Row {
            case: "the pool's own default authenticated role, absent from the claim",
            resolution: DENY,
            claims: json!({"cognito:roles": [&group_a.arn, &group_b.arn]}),
            custom_role_arn: &authenticated.arn,
            outcome: Outcome::NotAuthorized,
        },
        Row {
            case: "the pool's own unauthenticated role, absent from the claim",
            resolution: DENY,
            claims: json!({"cognito:roles": [&group_a.arn, &group_b.arn]}),
            custom_role_arn: &unauthenticated.arn,
            outcome: Outcome::NotAuthorized,
        },
        Row {
            case: "a role the token states, named beside a preference for another",
            resolution: DENY,
            claims: json!({
                "cognito:roles": [&group_a.arn, &group_b.arn],
                "cognito:preferred_role": &group_a.arn,
            }),
            custom_role_arn: &group_b.arn,
            outcome: Outcome::Session(group_b),
        },
        Row {
            case: "a role absent from a single-valued claim, under Deny",
            resolution: DENY,
            claims: json!({"cognito:roles": [&group_a.arn]}),
            custom_role_arn: &group_c.arn,
            outcome: Outcome::NotAuthorized,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        set_roles(
            &emulator,
            &pool_id,
            authenticated,
            unauthenticated,
            token_mapping(row.resolution),
        )
        .await;
        let logins = json!({USER_POOL_PROVIDER: identity_token(&user_pool_claims(
            &format!("claim-membership-subject-{index}"),
            &row.claims,
        ))});
        let (status, body) = vend(&emulator, &pool_id, &logins, Some(row.custom_role_arn)).await;

        match &row.outcome {
            Outcome::Session(role) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the vend must be served, got {status} with {body}"
                );
                let (access_key_id, _) = vended(&body);
                let (caller_status, caller_body) = emulator.caller_identity(access_key_id).await;
                assert_eq!(
                    caller_status, 200,
                    "case {case:?}: GetCallerIdentity under the vended credential expected \
                     200, got {caller_status} with {caller_body}"
                );
                let arn = element(&caller_body, "Arn");
                assert_eq!(
                    arn,
                    expected_session_arn(&role.name),
                    "case {case:?}: expected a session of {}, got {arn}",
                    role.name
                );
            }
            Outcome::NotAuthorized => {
                assert_eq!(
                    status, REFUSED_STATUS,
                    "case {case:?}: expected HTTP {REFUSED_STATUS}, got {status} with {body}"
                );
                let code = error_code(&body);
                assert!(
                    code.ends_with("NotAuthorizedException"),
                    "case {case:?}: expected NotAuthorizedException, got {code:?} in {body}"
                );
                let expected = not_in_token_message(row.custom_role_arn);
                assert_eq!(
                    body["message"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the message {expected:?}, got {body}"
                );
                assert!(
                    body.get("Credentials").is_none(),
                    "case {case:?}: a refused vend carries no credential, got {body}"
                );
            }
        }
    }
}

/// RULE, the controls: the refusal above is about the ROLE the request named and not about
/// the pool, so the same pool, the same mapping and the same token answer the bare call —
/// the one that names no role — out of the mapping's own resolution, and answer a named
/// in-claim role with a session of it.
#[tokio::test]
async fn the_same_pool_still_resolves_the_bare_call_out_of_its_mapping() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "custom-role-claim-controls").await;
    let roles = seeded_roles(
        &emulator,
        &pool_id,
        &[
            "control-auth",
            "control-unauth",
            "control-group-a",
            "control-group-b",
        ],
    );
    let (authenticated, unauthenticated) = (&roles[0], &roles[1]);
    let (group_a, group_b) = (&roles[2], &roles[3]);
    // Several roles with no preference among them: the exchange a `Token` mapping resolves
    // no one role for, which is what its `AmbiguousRoleResolution` decides.
    let claims = json!({"cognito:roles": [&group_a.arn, &group_b.arn]});
    let logins = json!({USER_POOL_PROVIDER: identity_token(&user_pool_claims(
        "claim-control-subject",
        &claims,
    ))});

    // Under `Deny` the bare call is refused for the AMBIGUOUS MAPPING, naming the provider —
    // a different sentence from the one a refused `CustomRoleArn` carries.
    set_roles(
        &emulator,
        &pool_id,
        authenticated,
        unauthenticated,
        token_mapping(DENY),
    )
    .await;
    let (status, body) = vend(&emulator, &pool_id, &logins, None).await;
    assert_eq!(
        status, REFUSED_STATUS,
        "the bare call under Deny expected HTTP {REFUSED_STATUS}, got {status} with {body}"
    );
    let code = error_code(&body);
    assert!(
        code.ends_with("NotAuthorizedException"),
        "the bare call under Deny expected NotAuthorizedException, got {code:?} in {body}"
    );
    let expected = denied_message(USER_POOL_PROVIDER);
    assert_eq!(
        body["message"].as_str(),
        Some(expected.as_str()),
        "the bare call under Deny expected the message {expected:?}, got {body}"
    );

    // The same pool, the same token: a role the token DID state is vended for, which is how
    // the refusals above are known not to be a report that the pool is misconfigured.
    let (status, body) = vend(&emulator, &pool_id, &logins, Some(&group_a.arn)).await;
    assert_eq!(
        status, 200,
        "an in-claim CustomRoleArn must be served, got {status} with {body}"
    );
    let (access_key_id, _) = vended(&body);
    let (caller_status, caller_body) = emulator.caller_identity(access_key_id).await;
    assert_eq!(
        caller_status, 200,
        "GetCallerIdentity under the vended credential expected 200, got {caller_status} \
         with {caller_body}"
    );
    let arn = element(&caller_body, "Arn");
    assert_eq!(
        arn,
        expected_session_arn(&group_a.name),
        "expected a session of {}, got {arn}",
        group_a.name
    );

    // Under `AuthenticatedRole` the same bare call falls back to the pool's own
    // authenticated role: the very role a request may not NAME, because the token does not
    // state it.
    set_roles(
        &emulator,
        &pool_id,
        authenticated,
        unauthenticated,
        token_mapping(AUTHENTICATED_ROLE),
    )
    .await;
    let (status, body) = vend(&emulator, &pool_id, &logins, None).await;
    assert_eq!(
        status, 200,
        "the bare call under AuthenticatedRole must be served, got {status} with {body}"
    );
    let (access_key_id, _) = vended(&body);
    let (caller_status, caller_body) = emulator.caller_identity(access_key_id).await;
    assert_eq!(
        caller_status, 200,
        "GetCallerIdentity under the vended credential expected 200, got {caller_status} \
         with {caller_body}"
    );
    let arn = element(&caller_body, "Arn");
    assert_eq!(
        arn,
        expected_session_arn(&authenticated.name),
        "expected a session of {}, got {arn}",
        authenticated.name
    );
}

/// RULE, the line between the two refusals: a pool that stores NO role configuration at all
/// has no role for any exchange — "the identity pool has no authenticated role configured"
/// (`InvalidIdentityPoolConfigurationException`, this service's model) — so a request naming
/// a `CustomRoleArn` against such a pool is answered for the POOL, not told about a claim.
#[tokio::test]
async fn a_pool_that_stores_no_role_at_all_answers_for_its_own_configuration() {
    let emulator = Emulator::new();
    // A SAML provider is one of the two kinds whose exchange may name a `CustomRoleArn` at
    // all ("Only SAML providers and providers with RoleMappings support custom role ARN"),
    // and it needs no stored `RoleMappings` to be one — so this pool reaches the role
    // selection while storing no role configuration whatsoever.
    let saml_provider = "arn:aws:iam::000000000000:saml-provider/claimless-corp";
    let created = emulator
        .ok(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": "custom-role-unconfigured",
                "AllowUnauthenticatedIdentities": true,
                "SamlProviderARNs": [saml_provider],
            }),
        )
        .await;
    let pool_id = created["IdentityPoolId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateIdentityPool answers an IdentityPoolId: {created}"))
        .to_string();
    let logins = json!({saml_provider: "a-saml-assertion"});
    let role_arn = emulator.seed_role("unconfigured-pool-role", None);

    let (status, body) = vend(&emulator, &pool_id, &logins, Some(&role_arn)).await;
    assert_eq!(
        status, REFUSED_STATUS,
        "a pool with no role configuration expected HTTP {REFUSED_STATUS}, got {status} with \
         {body}"
    );
    let code = error_code(&body);
    assert!(
        code.ends_with("InvalidIdentityPoolConfigurationException"),
        "a pool with no role configuration is answered for its own configuration, got \
         {code:?} in {body}"
    );
}

/// RULE, the other side of it: where NO presented token states a role at all — a provider
/// whose answer this emulator cannot read roles out of — the roles the pool's own
/// configuration names are the offer a `CustomRoleArn` is drawn from, and a role the pool
/// does not name either is refused the same way.
#[tokio::test]
async fn a_token_stating_no_role_leaves_the_pools_own_roles_to_pick_from() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "custom-role-claimless").await;
    let roles = seeded_roles(
        &emulator,
        &pool_id,
        &["claimless-auth", "claimless-unauth", "claimless-other"],
    );
    let (authenticated, unauthenticated, other) = (&roles[0], &roles[1], &roles[2]);
    set_roles(
        &emulator,
        &pool_id,
        authenticated,
        unauthenticated,
        token_mapping(AUTHENTICATED_ROLE),
    )
    .await;
    // A token carrying no role claim at all: nothing here states which roles the provider
    // offered this exchange.
    let logins = json!({USER_POOL_PROVIDER: identity_token(&user_pool_claims(
        "claimless-subject",
        &json!({"custom:dept": "engineering"}),
    ))});

    // (case, the role named, whether the pool names it)
    let rows = [
        ("the pool's own authenticated role", authenticated, true),
        ("the pool's own unauthenticated role", unauthenticated, true),
        ("a role the pool does not name", other, false),
    ];

    for (case, role, named) in rows {
        let (status, body) = vend(&emulator, &pool_id, &logins, Some(&role.arn)).await;
        if named {
            assert_eq!(
                status, 200,
                "case {case:?}: the vend must be served, got {status} with {body}"
            );
            let (access_key_id, _) = vended(&body);
            let (caller_status, caller_body) = emulator.caller_identity(access_key_id).await;
            assert_eq!(
                caller_status, 200,
                "case {case:?}: GetCallerIdentity under the vended credential expected 200, \
                 got {caller_status} with {caller_body}"
            );
            let arn = element(&caller_body, "Arn");
            assert_eq!(
                arn,
                expected_session_arn(&role.name),
                "case {case:?}: expected a session of {}, got {arn}",
                role.name
            );
            continue;
        }
        assert_eq!(
            status, REFUSED_STATUS,
            "case {case:?}: expected HTTP {REFUSED_STATUS}, got {status} with {body}"
        );
        let code = error_code(&body);
        assert!(
            code.ends_with("NotAuthorizedException"),
            "case {case:?}: expected NotAuthorizedException, got {code:?} in {body}"
        );
        let expected = not_in_token_message(&role.arn);
        assert_eq!(
            body["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body}"
        );
    }
}
