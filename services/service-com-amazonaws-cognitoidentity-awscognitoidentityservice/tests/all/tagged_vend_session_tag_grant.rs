// SPDX-License-Identifier: CC0-1.0

//! The grant a TAGGED vend needs beside the assume itself.
//!
//! RULE: attaching session tags to a role session is a permission of its own — "To pass
//! session tags, the principal must have the `sts:TagSession` permission" (`AssumeRole$Tags`,
//! the STS model) — so an exchange whose pool maps principal tags for the provider the
//! presented login was made under is served only where the assumed role's trust policy grants
//! the Cognito Identity principal BOTH `sts:AssumeRoleWithWebIdentity` and `sts:TagSession`
//! for that assume. A role that admits only the assume cannot be assumed WITH tags: the
//! request to STS is refused, which is the "STS returned an error response to the request to
//! assume the … role from the identity pool" case the modelled
//! `InvalidIdentityPoolConfigurationException` (HTTP 400) states, and nothing is vended. An
//! exchange that asks for NO tag attaches none and needs no such grant.
//!
//! The rows differ along what the rule generalises over: whether the trust policy grants the
//! tag permission at all, how it grants it (beside the assume in one statement, in a statement
//! of its own, through a wildcard action), whether it DENIES it, how the grant is conditioned
//! (on the tag keys the assume publishes, on a tag's value), and how many tags the exchange
//! asks for — with the untagged exchange as the control row that is served by a role granting
//! the assume alone.
//!
//! The harness is `vended_role_session`'s: this service and STS on ONE shared engine behind
//! the same `MultiServiceRouter` the production router builds, with the IAM roles the assume's
//! decision reads seeded into that same engine. The fixture pool is `mapped_role_resolution`'s
//! — a pool fronted by a user pool, whose `Logins` entry carries the identity token the mapped
//! claims are read out of.

use serde_json::{json, Value};
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;

use crate::mapped_role_resolution::{identity_token, pool, user_pool_claims, USER_POOL_PROVIDER};
use crate::vended_role_session::{vended, Emulator};

/// The Cognito Identity principal a role's trust policy names to admit an identity pool's
/// users, as `AssumeRoleWithWebIdentityRequest$RoleArn` documents it.
const COGNITO_PRINCIPAL: &str = "cognito-identity.amazonaws.com";

/// The assume a pool's exchange makes, and the permission that attaches its session tags.
const ASSUME_ACTION: &str = "sts:AssumeRoleWithWebIdentity";
const TAG_SESSION_ACTION: &str = "sts:TagSession";

/// The principal tag the fixture pool maps, and the user attribute (claim) its value is taken
/// from — the mapping `SetPrincipalTagAttributeMap` stores.
const TENANT_TAG: &str = "Tenant";
const TENANT_ATTRIBUTE: &str = "custom:tenant";
/// A SECOND mapped pair, so an exchange asking for more than one tag is read too.
const DEPARTMENT_TAG: &str = "Department";
const DEPARTMENT_ATTRIBUTE: &str = "custom:dept";

/// The value the mapped tenant attribute carries in every exchange below.
const TENANT: &str = "acme";

/// How AWS words the refusal of a vend whose assume STS would not serve.
const INVALID_POOL_CONFIGURATION: &str =
    "Invalid identity pool configuration. Check assigned IAM roles for this pool.";

/// The modelled error such a refusal carries.
const INVALID_POOL_CONFIGURATION_CODE: &str = "InvalidIdentityPoolConfigurationException";

/// A trust policy admitting this pool's users: one statement granting `actions` to the
/// `Federated` Cognito Identity principal, restricted to `audience`'s own users by the `:aud`
/// condition, with `condition` merged into that clause.
fn trust_policy(audience: &str, actions: Value, condition: Value) -> Value {
    let mut clause = json!({format!("{COGNITO_PRINCIPAL}:aud"): audience});
    for (key, value) in condition.as_object().into_iter().flatten() {
        clause[key] = value.clone();
    }
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": COGNITO_PRINCIPAL},
            "Action": actions,
            "Condition": {"StringEquals": clause},
        }],
    })
}

/// Configure `pool_id`'s authenticated role, which is the role every vend below assumes.
async fn set_authenticated_role(emulator: &Emulator, pool_id: &str, role_arn: &str) {
    emulator
        .ok(
            "SetIdentityPoolRoles",
            &json!({
                "IdentityPoolId": pool_id,
                "Roles": {"authenticated": role_arn},
            }),
        )
        .await;
}

/// Store the principal-tag attribute map `principal_tags` for the fixture pool's user-pool
/// provider, which is what makes its exchanges TAGGED ones.
async fn map_principal_tags(emulator: &Emulator, pool_id: &str, principal_tags: Value) {
    emulator
        .ok(
            "SetPrincipalTagAttributeMap",
            &json!({
                "IdentityPoolId": pool_id,
                "IdentityProviderName": USER_POOL_PROVIDER,
                "UseDefaults": false,
                "PrincipalTags": principal_tags,
            }),
        )
        .await;
}

/// The `Logins` map of one exchange: the user pool's identity token for `subject`, carrying
/// the `claims` the mapped attributes are read out of.
fn login(subject: &str, claims: &Value) -> Value {
    json!({USER_POOL_PROVIDER: identity_token(&user_pool_claims(subject, claims))})
}

/// Vend for a fresh identity of `pool_id` presenting `logins`, and answer the status and body.
async fn exchange(emulator: &Emulator, pool_id: &str, logins: &Value) -> (u16, Value) {
    let identity_id = emulator.identity(pool_id, Some(logins.clone())).await;
    emulator
        .call(
            "GetCredentialsForIdentity",
            &json!({"IdentityId": identity_id, "Logins": logins.clone()}),
        )
        .await
}

/// The session tags the STS registry in the same engine holds for the session `access_key_id`
/// names, in tag-key order.
fn session_tags(emulator: &Emulator, access_key_id: &str) -> Vec<(String, String)> {
    let registry = emulator
        .engine
        .state::<sts::ComAmazonawsStsAwssecuritytokenservicev20110615State>();
    let held = registry.lock();
    let session = held
        .session(access_key_id)
        .unwrap_or_else(|| panic!("the vended session {access_key_id} is in the STS registry"));
    let mut tags = session.tags.clone();
    tags.sort();
    tags
}

/// Assert the vend `status`/`body` is the refusal of an assume STS would not serve, and that
/// it carries no credential.
fn assert_refused(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: the tagged assume must be refused, got {status} with {body}"
    );
    assert_eq!(
        body["__type"].as_str(),
        Some(INVALID_POOL_CONFIGURATION_CODE),
        "case {case:?}: expected {INVALID_POOL_CONFIGURATION_CODE}, got {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(INVALID_POOL_CONFIGURATION),
        "case {case:?}: expected {INVALID_POOL_CONFIGURATION:?}, got {body}"
    );
    assert!(
        body["Credentials"].is_null(),
        "case {case:?}: a refused assume vends nothing, got {body}"
    );
}

/// One row: the trust policy a role carries, and whether the tagged exchange it serves is
/// expected to be vended.
struct Row<'a> {
    case: &'a str,
    /// `(the actions granted, the extra condition clause)`, from which the policy is built.
    actions: Value,
    condition: Value,
    served: bool,
}

/// RULE: a tagged exchange is served only by a role whose trust policy grants the session-tag
/// permission beside the assume.
#[tokio::test]
async fn a_tagged_vend_needs_the_session_tag_grant_on_the_role() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "tag-session-grant").await;
    map_principal_tags(&emulator, &pool_id, json!({TENANT_TAG: TENANT_ATTRIBUTE})).await;

    let rows = [
        Row {
            case: "the assume alone",
            actions: json!(ASSUME_ACTION),
            condition: json!({}),
            served: false,
        },
        Row {
            case: "the assume and the tag permission in one statement",
            actions: json!([ASSUME_ACTION, TAG_SESSION_ACTION]),
            condition: json!({}),
            served: true,
        },
        Row {
            case: "every sts action through a wildcard",
            actions: json!("sts:*"),
            condition: json!({}),
            served: true,
        },
        Row {
            case: "the tag permission granted only for another tag key",
            actions: json!([ASSUME_ACTION, TAG_SESSION_ACTION]),
            condition: json!({"aws:TagKeys": "Project"}),
            served: false,
        },
        Row {
            case: "the tag permission granted for the key the pool maps",
            actions: json!([ASSUME_ACTION, TAG_SESSION_ACTION]),
            condition: json!({"aws:TagKeys": TENANT_TAG}),
            served: true,
        },
        Row {
            case: "the tag permission granted only for another tag value",
            actions: json!([ASSUME_ACTION, TAG_SESSION_ACTION]),
            condition: json!({format!("aws:RequestTag/{TENANT_TAG}"): "globex"}),
            served: false,
        },
        Row {
            case: "the tag permission granted for the mapped tag value",
            actions: json!([ASSUME_ACTION, TAG_SESSION_ACTION]),
            condition: json!({format!("aws:RequestTag/{TENANT_TAG}"): TENANT}),
            served: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let role = emulator.seed_role(
            &format!("tag-session-grant-{index}"),
            Some(trust_policy(
                &pool_id,
                row.actions.clone(),
                row.condition.clone(),
            )),
        );
        set_authenticated_role(&emulator, &pool_id, &role).await;
        let logins = login(
            &format!("tag-session-subject-{index}"),
            &json!({TENANT_ATTRIBUTE: TENANT}),
        );
        let (status, body) = exchange(&emulator, &pool_id, &logins).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: the tagged assume must be served, got {status} with {body}"
            );
            let (access_key_id, _) = vended(&body);
            let actual = session_tags(&emulator, access_key_id);
            let expected = vec![(TENANT_TAG.to_string(), TENANT.to_string())];
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the session tags {expected:?}, got {actual:?}"
            );
        } else {
            assert_refused(case, status, &body);
        }
    }
}

/// RULE: the permission is needed for EVERY tag the exchange asks for, and a statement of its
/// own grants it just as one beside the assume does — while an explicit `Deny` of it withholds
/// it however the assume is granted.
#[tokio::test]
async fn the_session_tag_grant_is_read_wherever_the_trust_policy_states_it() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "tag-session-statements").await;
    // Two mapped pairs, so the exchange below asks for two tags at once.
    map_principal_tags(
        &emulator,
        &pool_id,
        json!({TENANT_TAG: TENANT_ATTRIBUTE, DEPARTMENT_TAG: DEPARTMENT_ATTRIBUTE}),
    )
    .await;
    let claims = json!({TENANT_ATTRIBUTE: TENANT, DEPARTMENT_ATTRIBUTE: "eng"});

    // The grant in a statement of its own, beside the statement that admits the assume.
    let split = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": {"Federated": COGNITO_PRINCIPAL},
                "Action": ASSUME_ACTION,
                "Condition": {"StringEquals": {format!("{COGNITO_PRINCIPAL}:aud"): &pool_id}},
            },
            {
                "Effect": "Allow",
                "Principal": {"Federated": COGNITO_PRINCIPAL},
                "Action": TAG_SESSION_ACTION,
            },
        ],
    });
    let role = emulator.seed_role("tag-session-split", Some(split));
    set_authenticated_role(&emulator, &pool_id, &role).await;
    let case = "the tag permission in a statement of its own";
    let (status, body) = exchange(&emulator, &pool_id, &login("split-subject", &claims)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the tagged assume must be served, got {status} with {body}"
    );
    let (access_key_id, _) = vended(&body);
    let actual = session_tags(&emulator, access_key_id);
    let expected = vec![
        (DEPARTMENT_TAG.to_string(), "eng".to_string()),
        (TENANT_TAG.to_string(), TENANT.to_string()),
    ];
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the session tags {expected:?}, got {actual:?}"
    );

    // The same grant, denied by a second statement: a `Deny` withholds the permission wherever
    // it is granted, so the tagged assume is refused.
    let denied = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": {"Federated": COGNITO_PRINCIPAL},
                "Action": [ASSUME_ACTION, TAG_SESSION_ACTION],
                "Condition": {"StringEquals": {format!("{COGNITO_PRINCIPAL}:aud"): &pool_id}},
            },
            {
                "Effect": "Deny",
                "Principal": {"Federated": COGNITO_PRINCIPAL},
                "Action": TAG_SESSION_ACTION,
            },
        ],
    });
    let role = emulator.seed_role("tag-session-denied", Some(denied));
    set_authenticated_role(&emulator, &pool_id, &role).await;
    let (status, body) = exchange(&emulator, &pool_id, &login("denied-subject", &claims)).await;
    assert_refused("the tag permission explicitly denied", status, &body);
}

/// RULE, the control: an exchange that asks for NO session tag attaches none, so the role that
/// grants the assume alone serves it — which is what makes the refusals above refusals of the
/// missing tag permission rather than of the assume.
#[tokio::test]
async fn an_untagged_vend_needs_no_session_tag_grant() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "tag-session-untagged").await;
    let role = emulator.seed_role(
        "tag-session-untagged-role",
        Some(trust_policy(&pool_id, json!(ASSUME_ACTION), json!({}))),
    );
    set_authenticated_role(&emulator, &pool_id, &role).await;

    // The pool maps no principal tags at all, so the exchange is untagged even though the
    // token carries the claim a mapping would read.
    let logins = login("untagged-subject", &json!({TENANT_ATTRIBUTE: TENANT}));
    let case = "an unmapped pool against a role granting the assume alone";
    let (status, body) = exchange(&emulator, &pool_id, &logins).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the untagged assume must be served, got {status} with {body}"
    );
    let (access_key_id, _) = vended(&body);
    let actual = session_tags(&emulator, access_key_id);
    assert!(
        actual.is_empty(),
        "case {case:?}: an untagged assume attaches no session tags, got {actual:?}"
    );

    // The same role, once the pool maps a tag for the provider: the very same exchange is now
    // a tagged one and is refused.
    map_principal_tags(&emulator, &pool_id, json!({TENANT_TAG: TENANT_ATTRIBUTE})).await;
    let logins = login("mapped-subject", &json!({TENANT_ATTRIBUTE: TENANT}));
    let (status, body) = exchange(&emulator, &pool_id, &logins).await;
    assert_refused(
        "the same role once the pool maps a principal tag",
        status,
        &body,
    );
}
