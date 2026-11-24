// SPDX-License-Identifier: CC0-1.0

//! WHICH principal-tag attribute map a vend builds its session tags from, read over a map that
//! is REPLACED between vends.
//!
//! RULE: `SetPrincipalTagAttributeMap` states one `(IdentityPoolId, IdentityProviderName)`
//! pair's WHOLE mapping — it takes `UseDefaults` and `PrincipalTags` and answers the mapping
//! now in force, which is the one `GetPrincipalTagAttributeMap` then reports
//! (`smithy_query(query="operation",operation="SetPrincipalTagAttributeMap,GetPrincipalTagAttributeMap")`)
//! — so each call REPLACES what the pair had, and every vend asks its assume for the tags of
//! the map in force AT THAT VEND: one tag per key the current map names, carrying the value the
//! presented token claims under the attribute that key is mapped to, and no tag for a key the
//! replacement dropped. The mapping is the pool's configuration rather than a property of the
//! token or of the identity, so one token presented for one identity is tagged differently
//! before and after a replacement.
//!
//! What a client reads and what a credential carries are therefore one fact: the reporting call
//! and the vend answer from the SAME record
//! (`crate::state::…::principal_tag_attribute_map`), so a map read back as in force is the map
//! the next session is tagged by.
//!
//! The rows differ along what the rule generalises over: which tag key the map names, whether a
//! replacement renames a key, adds one, drops one, maps a second attribute, asks for the
//! DEFAULT mappings instead of a custom map (`client` from the `aud` claim and `username` from
//! `sub`, Amazon Cognito's published default pair) or states an EMPTY map, which leaves the
//! exchange untagged. Every row is driven with the SAME identity and the SAME token, so the only
//! thing that decides the session's tags is the map in force.
//!
//! The harness is `vended_role_session`'s — this service and STS on ONE shared engine behind the
//! router the production one builds, with the role the assume reads seeded into that engine —
//! and the fixture pool and token are `mapped_role_resolution`'s.

use serde_json::{json, Value};
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;

use crate::mapped_role_resolution::{
    identity_token, pool, user_pool_claims, CLIENT_ID, USER_POOL_PROVIDER,
};
use crate::vended_role_session::{vended, Emulator};

/// The Cognito Identity principal a role's trust policy names to admit an identity pool's
/// users, as `AssumeRoleWithWebIdentityRequest$RoleArn` documents it.
const COGNITO_PRINCIPAL: &str = "cognito-identity.amazonaws.com";

/// The two user attributes the maps below map onto session tags, and the tag keys they are
/// mapped onto: the key the first map names, the key a replacement re-points the same attribute
/// at, and a key for the second attribute.
const TENANT_ATTRIBUTE: &str = "custom:tenant";
const DEPARTMENT_ATTRIBUTE: &str = "custom:dept";
const TENANT_TAG: &str = "tenant";
const ORG_TAG: &str = "org";
const UNIT_TAG: &str = "unit";

/// The values the presented token claims under those two attributes.
const TENANT_VALUE: &str = "tenant-a";
const DEPARTMENT_VALUE: &str = "eng";

/// The `sub` of the one identity every row below is vended for, which is also what the DEFAULT
/// mappings' `username` tag is taken from.
const SUBJECT: &str = "replaced-map-subject";

/// A trust policy that admits this pool's users and lets the exchange ATTACH whatever tags it
/// asks for: the `Federated` Cognito Identity principal, the web-identity assume, the
/// `sts:TagSession` permission a tagged assume needs beside it ("To pass session tags, the
/// principal must have the `sts:TagSession` permission", `AssumeRole$Tags`), and the `:aud`
/// condition restricting it to `audience`'s own users — and no clause over a tag, so what the
/// rows below read is the tags the vend asked for rather than a gate on them.
fn tagging_trust_policy(audience: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": COGNITO_PRINCIPAL},
            "Action": ["sts:AssumeRoleWithWebIdentity", "sts:TagSession"],
            "Condition": {"StringEquals": {format!("{COGNITO_PRINCIPAL}:aud"): audience}},
        }],
    })
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

/// One row: the mapping members a `SetPrincipalTagAttributeMap` puts in force, and the session
/// tags every vend made after it must then carry, in tag-key order.
struct Row<'a> {
    case: &'a str,
    mapping: Value,
    expected_tags: Vec<(&'a str, &'a str)>,
}

/// RULE: every vend asks for the tags of the map in force at that vend, so replacing the map
/// re-tags the sessions minted after it — the same identity presenting the same token.
#[tokio::test]
async fn a_replaced_map_decides_the_tags_of_the_sessions_vended_after_it() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "replaced-principal-tags").await;
    let role = emulator.seed_role("replaced-map-reader", Some(tagging_trust_policy(&pool_id)));
    emulator
        .ok(
            "SetIdentityPoolRoles",
            &json!({"IdentityPoolId": &pool_id, "Roles": {"authenticated": &role}}),
        )
        .await;

    // ONE login and ONE identity for every row: the token's claims never change, so a tag that
    // moves between the rows moved because the pool's mapping did.
    let logins = json!({
        USER_POOL_PROVIDER: identity_token(&user_pool_claims(
            SUBJECT,
            &json!({TENANT_ATTRIBUTE: TENANT_VALUE, DEPARTMENT_ATTRIBUTE: DEPARTMENT_VALUE}),
        )),
    });
    let identity_id = emulator.identity(&pool_id, Some(logins.clone())).await;

    let rows = [
        Row {
            case: "the first custom mapping, onto one tag key",
            mapping: json!({"UseDefaults": false, "PrincipalTags": {TENANT_TAG: TENANT_ATTRIBUTE}}),
            expected_tags: vec![(TENANT_TAG, TENANT_VALUE)],
        },
        Row {
            case: "a replacement re-pointing the same attribute at another tag key",
            mapping: json!({"UseDefaults": false, "PrincipalTags": {ORG_TAG: TENANT_ATTRIBUTE}}),
            expected_tags: vec![(ORG_TAG, TENANT_VALUE)],
        },
        Row {
            case: "a replacement that adds a key, mapping the second attribute",
            mapping: json!({
                "UseDefaults": false,
                "PrincipalTags": {
                    ORG_TAG: TENANT_ATTRIBUTE,
                    UNIT_TAG: DEPARTMENT_ATTRIBUTE,
                },
            }),
            expected_tags: vec![(ORG_TAG, TENANT_VALUE), (UNIT_TAG, DEPARTMENT_VALUE)],
        },
        Row {
            case: "a replacement that drops the first key again",
            mapping: json!({"UseDefaults": false, "PrincipalTags": {UNIT_TAG: DEPARTMENT_ATTRIBUTE}}),
            expected_tags: vec![(UNIT_TAG, DEPARTMENT_VALUE)],
        },
        Row {
            case: "a replacement asking for the default mappings instead of a custom map",
            mapping: json!({"UseDefaults": true}),
            // The default pair: `client` from the token's audience, `username` from its subject.
            expected_tags: vec![("client", CLIENT_ID), ("username", SUBJECT)],
        },
        Row {
            case: "a replacement stating an empty map, which maps nothing at all",
            mapping: json!({"UseDefaults": false, "PrincipalTags": {}}),
            expected_tags: Vec::new(),
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut request = json!({
            "IdentityPoolId": &pool_id,
            "IdentityProviderName": USER_POOL_PROVIDER,
        });
        for (member, value) in row.mapping.as_object().expect("the row states a mapping") {
            request[member] = value.clone();
        }
        let set = emulator.ok("SetPrincipalTagAttributeMap", &request).await;

        // What the reporting call says is in force, which has to be the mapping the write
        // answered: one record, so the map a client reads is the map the vend below is tagged
        // by.
        let reported = emulator
            .ok(
                "GetPrincipalTagAttributeMap",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "IdentityProviderName": USER_POOL_PROVIDER,
                }),
            )
            .await;
        for member in ["PrincipalTags", "UseDefaults"] {
            assert_eq!(
                reported[member], set[member],
                "case {case:?}: the mapping reported in force must be the one just written; \
                 expected {member} {}, got {}",
                set[member], reported[member]
            );
        }

        // The vend that follows: a fresh session of the same identity, tagged by the map that
        // is in force now.
        let (status, body) = emulator
            .call(
                "GetCredentialsForIdentity",
                &json!({"IdentityId": &identity_id, "Logins": logins.clone()}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the vend must be served, got {status} with {body}"
        );
        let (access_key_id, _) = vended(&body);
        let actual = session_tags(&emulator, access_key_id);
        let expected: Vec<(String, String)> = row
            .expected_tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the session tags {expected:?}, got {actual:?}"
        );
    }
}
