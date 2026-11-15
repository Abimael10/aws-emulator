// SPDX-License-Identifier: CC0-1.0

//! Attribute-based access control at the TRUST-POLICY boundary of a vend, over a login whose
//! id token the Amazon Cognito user pools service really minted.
//!
//! RULE: where an identity pool holds a principal-tag attribute map for the identity provider
//! a presented login was made under (`SetPrincipalTagAttributeMap`), `GetCredentialsForIdentity`
//! makes its `sts:AssumeRoleWithWebIdentity` a TAGGED assume — each mapped tag key travels as a
//! REQUESTED session tag carrying the value the presented token claims under the attribute that
//! key is mapped to — so the assumed role's trust policy, decided with `aws:RequestTag/<key>`
//! beside `sts:TagSession`, admits exactly the identities whose mapped claim satisfies its
//! condition and refuses every other with the modelled `InvalidIdentityPoolConfigurationException`
//! (HTTP 400, "Invalid identity pool configuration. Check assigned IAM roles for this pool.").
//! The values are the token's own claims and nothing else: a custom user attribute travels as the
//! `custom:<name>` claim the user pool mints it into
//! (`smithy_query(query="doc",shape="SchemaAttributeType",member="Name")` for the prefix,
//! `AuthenticationResultType$IdToken` for the claims), so the value a token carried when it was
//! minted is what decides the assume.
//!
//! What this suite adds over `vended_session_request_tags` — which pins the same rule against a
//! token written by the suite — is the CROSS-SERVICE path the mapping really runs over: the token
//! is minted by `cognito-idp` on the same engine from the user's stored attributes, so the claim
//! names a pool mints and the attribute names a mapping reads have to line up for a single
//! identity to be vended at all.
//!
//! The rows differ along what the rule generalises over: the value the mapped attribute carries
//! (the gated one, another one, another case of it, none at all), how many of the mapped
//! attributes the user holds, and WHICH value the role is gated on — every user is tried against
//! every gate, so a vend can only be served by matching the value rather than by being on some
//! list.

use serde_json::{json, Value};
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;

use crate::identity_pool_flow::{error_code, seed_role, seeded_role_arn};
use crate::server_side_token_check::{
    logins, signed_in, signed_in_user_with_attributes, user_pool_with_schema, Emulator, UserPool,
};

/// The Cognito Identity principal a role's trust policy names to admit an identity pool's
/// users, as `AssumeRoleWithWebIdentityRequest$RoleArn` documents it.
const COGNITO_PRINCIPAL: &str = "cognito-identity.amazonaws.com";

/// The principal tag key the roles below are gated on, and the user attribute the pool maps it
/// from: `PrincipalTags` stores tag key -> the attribute (claim) its value is taken from.
const DEPARTMENT_TAG: &str = "Department";
/// See [`DEPARTMENT_TAG`]. `dept` is declared in the user pool's `Schema`, so the pool stores it
/// as `custom:dept` and its id tokens claim it under that name.
const DEPARTMENT_ATTRIBUTE: &str = "custom:dept";

/// A SECOND mapped tag key, so a vend asking for more than one tag is exercised too.
const TENANT_TAG: &str = "Tenant";
/// See [`TENANT_TAG`].
const TENANT_ATTRIBUTE: &str = "custom:tenant";

/// The modelled error a vend no role of the pool can be assumed for carries — among them an
/// assume the role's own trust policy does not admit.
const INVALID_POOL_CONFIGURATION_CODE: &str = "InvalidIdentityPoolConfigurationException";

/// How AWS words that refusal: entirely fixed text, with no request- or state-derived part.
/// Written out here rather than read back from the crate, so a message that drifted would fail.
const INVALID_POOL_CONFIGURATION: &str =
    "Invalid identity pool configuration. Check assigned IAM roles for this pool.";

/// The status this service answers a modelled client error with.
const REFUSED_STATUS: u16 = 400;

/// A trust policy admitting `audience`'s users ONLY where the assume requests the session tag
/// [`DEPARTMENT_TAG`] with the value `department`: the `Federated` Cognito Identity principal,
/// the web-identity assume beside the `sts:TagSession` permission a tagged assume needs ("To pass
/// session tags, the principal must have the `sts:TagSession` permission", `AssumeRole$Tags`), the
/// `:aud` condition restricting it to this pool's own users, the `:amr` condition the Amazon
/// Cognito Developer Guide writes for an authenticated exchange, and the `aws:RequestTag/<key>`
/// clause the tagged assume is gated on.
fn tag_gated_trust_policy(audience: &str, department: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": COGNITO_PRINCIPAL},
            "Action": ["sts:AssumeRoleWithWebIdentity", "sts:TagSession"],
            "Condition": {
                "StringEquals": {
                    format!("{COGNITO_PRINCIPAL}:aud"): audience,
                    format!("aws:RequestTag/{DEPARTMENT_TAG}"): department,
                },
                "ForAnyValue:StringLike": {
                    format!("{COGNITO_PRINCIPAL}:amr"): "authenticated",
                },
            },
        }],
    })
}

/// An identity pool fronted by `pool`'s user pool, admitting no unauthenticated identity and
/// mapping the two attributes above to principal tags for that provider. Answers its id.
async fn tag_mapping_pool(emulator: &Emulator, name: &str, pool: &UserPool) -> String {
    let created = emulator
        .identity_ok(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": false,
                "CognitoIdentityProviders": [{
                    "ProviderName": &pool.provider_name,
                    "ClientId": &pool.client_id,
                    "ServerSideTokenCheck": false,
                }],
            }),
        )
        .await;
    let pool_id = created["IdentityPoolId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateIdentityPool answers an IdentityPoolId: {created}"))
        .to_string();
    emulator
        .identity_ok(
            "SetPrincipalTagAttributeMap",
            &json!({
                "IdentityPoolId": &pool_id,
                "IdentityProviderName": &pool.provider_name,
                "UseDefaults": false,
                "PrincipalTags": {
                    DEPARTMENT_TAG: DEPARTMENT_ATTRIBUTE,
                    TENANT_TAG: TENANT_ATTRIBUTE,
                },
            }),
        )
        .await;
    pool_id
}

/// Point `pool_id`'s `authenticated` role at `role_arn`, which is the role every vend then
/// assumes.
async fn set_authenticated_role(emulator: &Emulator, pool_id: &str, role_arn: &str) {
    emulator
        .identity_ok(
            "SetIdentityPoolRoles",
            &json!({
                "IdentityPoolId": pool_id,
                "Roles": {"authenticated": role_arn},
            }),
        )
        .await;
}

/// Mint an identity of `pool_id` for `presented`, then vend for it: answers the identity id with
/// the vend's status and body.
async fn exchange(emulator: &Emulator, pool_id: &str, presented: &Value) -> (String, u16, Value) {
    let got = emulator
        .identity_ok(
            "GetId",
            &json!({"IdentityPoolId": pool_id, "Logins": presented}),
        )
        .await;
    let identity_id = got["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("GetId answers an IdentityId: {got}"))
        .to_string();
    let (status, body) = emulator
        .identity(
            "GetCredentialsForIdentity",
            &json!({"IdentityId": &identity_id, "Logins": presented}),
        )
        .await;
    (identity_id, status, body)
}

/// The session tags the STS registry in the same engine holds for the session `access_key_id`
/// names — the pairs `aws:PrincipalTag/<key>` answers for a request that credential signs — in
/// tag-key order.
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

/// Assert a vend was SERVED for `identity_id`, carrying a complete credential set, and answer its
/// access key id.
fn assert_vended(case: &str, identity_id: &str, status: u16, body: &Value) -> String {
    assert_eq!(
        status, 200,
        "case {case:?}: the vend must be served, got {status} with {body}"
    );
    assert_eq!(
        body["IdentityId"].as_str(),
        Some(identity_id),
        "case {case:?}: the vend answers the identity it was made for, got {body}"
    );
    for member in ["AccessKeyId", "SecretKey", "SessionToken", "Expiration"] {
        assert!(
            !body["Credentials"][member].is_null(),
            "case {case:?}: a served vend carries Credentials.{member}, got {body}"
        );
    }
    body["Credentials"]["AccessKeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a served vend carries an AccessKeyId: {body}"))
        .to_string()
}

/// Assert a vend was REFUSED as the pool-configuration refusal a denied assume is reported as,
/// with no credential in the answer.
fn assert_refused(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, REFUSED_STATUS,
        "case {case:?}: the assume must be refused HTTP {REFUSED_STATUS}, got {status} with {body}"
    );
    let code = error_code(body);
    assert!(
        code.ends_with(INVALID_POOL_CONFIGURATION_CODE),
        "case {case:?}: expected {INVALID_POOL_CONFIGURATION_CODE}, got {code:?} in {body}"
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, INVALID_POOL_CONFIGURATION,
        "case {case:?}: expected the message {INVALID_POOL_CONFIGURATION:?}, got {message:?} in \
         {body}"
    );
    assert!(
        body.get("Credentials").is_none(),
        "case {case:?}: a refused assume vends nothing, got {body}"
    );
}

/// One user of the fixture user pool: the values it holds for the two mapped attributes.
struct Member {
    case: &'static str,
    username: &'static str,
    department: Option<&'static str>,
    tenant: Option<&'static str>,
}

impl Member {
    /// The `UserAttributes` the user is created with: only the attributes it holds a value for,
    /// so a row with none holds none.
    fn attributes(&self) -> Vec<Value> {
        [
            (DEPARTMENT_ATTRIBUTE, self.department),
            (TENANT_ATTRIBUTE, self.tenant),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| json!({"Name": name, "Value": value})))
        .collect()
    }

    /// The session tags a vend for this user must request, in tag-key order: one per mapped
    /// attribute the token carries a claim for. A mapped attribute the token says nothing about
    /// contributes none.
    fn expected_tags(&self) -> Vec<(String, String)> {
        let mut tags: Vec<(String, String)> = [
            (DEPARTMENT_TAG, self.department),
            (TENANT_TAG, self.tenant),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_string(), value.to_string())))
        .collect();
        tags.sort();
        tags
    }
}

/// The schema the fixture user pool declares, so the two mapped attributes exist on its users.
/// `Name: "dept"` is stored as `custom:dept`, the name the mapping reads.
fn custom_attribute_schema() -> Vec<Value> {
    vec![
        json!({"Name": "dept", "AttributeDataType": "String", "Mutable": true}),
        json!({"Name": "tenant", "AttributeDataType": "String", "Mutable": true}),
    ]
}

/// RULE: the mapped attribute value the user pool's own id token claims is what the role's
/// `aws:RequestTag/<key>` clause is decided against — so each identity is vended by the role
/// gated on ITS value and refused by every other, and the session it gets carries those pairs.
#[tokio::test]
async fn the_claimed_attribute_value_decides_which_gated_role_vends() {
    let emulator = Emulator::new();
    let pool = user_pool_with_schema(&emulator, "dept-attributes", custom_attribute_schema()).await;
    let pool_id = tag_mapping_pool(&emulator, "dept-gated", &pool).await;

    // The members differ along the value the mapped attribute carries, its case, and how many of
    // the mapped attributes the user holds at all.
    let members = [
        Member {
            case: "the mapped attribute carries 'eng', and the second one a value too",
            username: "alice",
            department: Some("eng"),
            tenant: Some("acme"),
        },
        Member {
            case: "the mapped attribute carries 'finance' and the second one nothing",
            username: "bob",
            department: Some("finance"),
            tenant: None,
        },
        Member {
            case: "the mapped attribute carries 'eng' in another case",
            username: "carol",
            department: Some("ENG"),
            tenant: Some("globex"),
        },
        Member {
            case: "the user holds no value for the mapped attribute",
            username: "dave",
            department: None,
            tenant: Some("acme"),
        },
    ];

    // One sign-in per member: the login every gate below is tried with.
    let mut presented: Vec<(&Member, Value)> = Vec::new();
    for member in &members {
        let id_token = signed_in_user_with_attributes(
            &emulator,
            &pool,
            member.username,
            member.attributes(),
        )
        .await;
        presented.push((member, logins(&pool, &id_token)));
    }

    // Every gate against every member: the whole cross product, so a served vend can only come
    // from the requested tag matching the gate.
    for gate in ["eng", "finance"] {
        let role = seed_role(
            &emulator.engine,
            &seeded_role_arn(&format!("dept-{gate}"), "auth"),
            tag_gated_trust_policy(&pool_id, gate),
        );
        set_authenticated_role(&emulator, &pool_id, &role).await;

        for (member, logins) in &presented {
            let case = format!("{}, against the role gated on {gate:?}", member.case);
            let (identity_id, status, body) = exchange(&emulator, &pool_id, logins).await;
            if member.department == Some(gate) {
                let access_key_id = assert_vended(&case, &identity_id, status, &body);
                let actual = session_tags(&emulator, &access_key_id);
                let expected = member.expected_tags();
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected the session tags {expected:?}, got {actual:?}"
                );
            } else {
                assert_refused(&case, status, &body);
            }
        }
    }
}

/// RULE, the same rule read over time: the requested tag carries what the PRESENTED token
/// claims, so changing the user's attribute moves the vend only for tokens minted after the
/// change — a token still claiming the old value is still decided by that value.
#[tokio::test]
async fn the_requested_tag_follows_the_presented_tokens_own_claim() {
    let emulator = Emulator::new();
    let pool = user_pool_with_schema(&emulator, "dept-updates", custom_attribute_schema()).await;
    let pool_id = tag_mapping_pool(&emulator, "dept-updated-gate", &pool).await;
    let role = seed_role(
        &emulator.engine,
        &seeded_role_arn("dept-updated", "auth"),
        tag_gated_trust_policy(&pool_id, "eng"),
    );
    set_authenticated_role(&emulator, &pool_id, &role).await;

    // The user starts in another department, so the token its sign-in mints claims that value and
    // the eng-gated role refuses it.
    let before = signed_in_user_with_attributes(
        &emulator,
        &pool,
        "erin",
        vec![json!({"Name": DEPARTMENT_ATTRIBUTE, "Value": "finance"})],
    )
    .await;
    let stale = logins(&pool, &before);
    let (_, status, body) = exchange(&emulator, &pool_id, &stale).await;
    assert_refused("the token claims the department it was minted with", status, &body);

    emulator
        .user_pools_ok(
            "AdminUpdateUserAttributes",
            &json!({
                "UserPoolId": &pool.user_pool_id,
                "Username": "erin",
                "UserAttributes": [{"Name": DEPARTMENT_ATTRIBUTE, "Value": "eng"}],
            }),
        )
        .await;

    // A token minted AFTER the change claims the new value, and the same gated role vends for it.
    let after = signed_in(&emulator, &pool, "erin").await;
    let fresh = logins(&pool, &after);
    let case = "a token minted after the attribute changed";
    let (identity_id, status, body) = exchange(&emulator, &pool_id, &fresh).await;
    let access_key_id = assert_vended(case, &identity_id, status, &body);
    let actual = session_tags(&emulator, &access_key_id);
    let expected = vec![(DEPARTMENT_TAG.to_string(), "eng".to_string())];
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the session tags {expected:?}, got {actual:?}"
    );

    // The old token still claims the old value, so it is still refused: the tag is the token's
    // claim rather than the user's current attribute.
    let (_, status, body) = exchange(&emulator, &pool_id, &stale).await;
    assert_refused("the stale token after the attribute changed", status, &body);
}
