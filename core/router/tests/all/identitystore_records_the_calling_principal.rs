// SPDX-License-Identifier: CC0-1.0

//! One credential, three services: the principal an identity-store record names as its
//! `CreatedBy`/`UpdatedBy` IS the principal the account's identity service reports for the
//! credential the write was signed with.
//!
//! The rule both halves implement: a request speaks for the principal its CREDENTIAL
//! belongs to, and that principal is identified by its AWS unique id — the value
//! `sts:GetCallerIdentity` answers as `UserId` (`AIDA…` for an IAM user, `AROA…:<session
//! name>` for a role session) — so an identity-store write stamps THAT id on the record and
//! every reader of the record reports it. Two callers that are two identities are therefore
//! two principals, and an update re-stamps `UpdatedBy` with the updating principal while
//! `CreatedBy` keeps the creating one.
//!
//! Only a cross-service suite can pin this: IAM holds the users, their access keys and the
//! roles, STS resolves a presented key (and mints the session credentials) and the identity
//! store records the answer. A crate-local suite can seed none of the other two, so it can
//! assert the SHAPE of the recorded value but never that the three services agree on which
//! principal it is — that half is
//! `services/service-com-amazonaws-identitystore-awsidentitystore/tests/all/audit_principal_identifier.rs`.
//!
//! The rows vary what the rule generalises over: the KIND of credential the write presents
//! (the long-term key the emulator publishes, an access key `iam:CreateAccessKey` minted
//! for a user of the account, and a role session `sts:AssumeRole` vended), and, for each,
//! every resource this API records a principal on (user, group, membership) read back
//! through both its describe and its listing.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery call helpers for the IAM/STS pair and the XML element reader are this test
// crate's own, written once by the suites that first needed them.
use crate::iam_role_id_agrees_with_sts::{iam_as, member, sts, LONG_TERM_KEY};

/// The identity store every call below addresses: the store IAM Identity Center
/// provisioned for the instance `sso-admin` seeds, which is the one `ListInstances` points
/// a client at.
const STORE: &str = service_com_amazonaws_identitystore_awsidentitystore::state::SEED_IDENTITY_STORE_ID;

/// The `X-Amz-Target` prefix of this service's `awsJson1_1` wire (its crate's
/// OPERATIONS.md, "Wire protocol").
const TARGET_PREFIX: &str = "AWSIdentityStore";

/// A trust policy admitting the account's own principals to assume the role, which is what
/// lets the long-term caller below vend a session.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// The emulator with IAM, STS and the identity store mounted on ONE engine, as
/// `core/router/src/lib.rs` mounts them: the access-key registry `iam:CreateAccessKey`
/// writes and the sessions `sts:AssumeRole` mints are the records the identity store
/// resolves a caller against.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_identitystore_awsidentitystore::EmulatedService::new(engine),
    );
    router
}

/// One identity-store call, presenting `access_key_id`: the status and the parsed body.
async fn identity_store(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "identitystore.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/us-east-1\
                 /identitystore/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// An identity-store call that must be served, answering its body.
async fn served(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let (status, parsed) = identity_store(router, access_key_id, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The `UserId` `sts:GetCallerIdentity` answers for a request presenting
/// `access_key_id` — the identifier of the principal that credential speaks for.
async fn caller_principal_id(
    router: &MultiServiceRouter,
    access_key_id: &str,
    case: &str,
) -> String {
    let (status, body) = sts(router, access_key_id, "GetCallerIdentity", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetCallerIdentity, got {status} {body}"
    );
    member(case, &body, "UserId")
}

/// Mint an access key for a NEW IAM user of the account and answer the key id. The answer
/// carries credential material, so nothing here quotes the body.
async fn user_access_key(router: &MultiServiceRouter, user_name: &str, case: &str) -> String {
    let (status, body) = iam_as(
        router,
        LONG_TERM_KEY,
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from iam CreateUser, got {status} {body}"
    );
    let (status, minted) = iam_as(
        router,
        LONG_TERM_KEY,
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from iam CreateAccessKey, got {status}"
    );
    member(case, &minted, "AccessKeyId")
}

/// Vend a role session and answer the access key id of its credential: a role of the
/// account, then `sts:AssumeRole` on it.
async fn role_session_key(
    router: &MultiServiceRouter,
    role_name: &str,
    session_name: &str,
    case: &str,
) -> String {
    let (status, body) = iam_as(
        router,
        LONG_TERM_KEY,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", TRUST_POLICY.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from iam CreateRole, got {status} {body}"
    );
    let role_arn = member(case, &body, "Arn");
    let (status, assumed) = sts(
        router,
        LONG_TERM_KEY,
        "AssumeRole",
        &[
            ("RoleArn", role_arn),
            ("RoleSessionName", session_name.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from sts AssumeRole, got {status}"
    );
    member(case, &assumed, "AccessKeyId")
}

/// One entry of a listing: the element whose `id_member` is `id`.
fn listed(list: &Value, member_name: &str, id_member: &str, id: &str, case: &str) -> Value {
    list[member_name]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry[id_member].as_str() == Some(id))
        })
        .unwrap_or_else(|| panic!("case {case:?}: expected {id} among {member_name}: {list}"))
        .clone()
}

/// Assert both audit members of `body` report `principal`.
fn assert_audited(body: &Value, created_by: &str, updated_by: &str, case: &str) {
    assert_eq!(
        body["CreatedBy"].as_str(),
        Some(created_by),
        "case {case:?}: expected CreatedBy to be the creating principal {created_by:?}, \
         got {body}"
    );
    assert_eq!(
        body["UpdatedBy"].as_str(),
        Some(updated_by),
        "case {case:?}: expected UpdatedBy to be the principal of the last write \
         {updated_by:?}, got {body}"
    );
}

/// Whichever kind of credential a write presents, every identity-store record it creates
/// names the principal STS reports for that credential — read back through both the
/// describe and the listing of each resource — two credentials of two identities are two
/// principals, and an update by another principal moves `UpdatedBy` alone.
#[tokio::test]
async fn an_identity_store_record_names_the_principal_its_credential_speaks_for() {
    let router = build_router();

    // (case, the access key id the write presents)
    let rows: [(&str, String); 3] = [
        (
            "the long-term key the emulator publishes",
            LONG_TERM_KEY.to_string(),
        ),
        (
            "an access key minted for a user of the account",
            user_access_key(&router, "identity-store-writer", "a user's own key").await,
        ),
        (
            "a role session's credential",
            role_session_key(
                &router,
                "identity-store-writer-role",
                "provisioning",
                "a role session",
            )
            .await,
        ),
    ];

    let mut per_row: Vec<(&str, String, String, String)> = Vec::new();
    for (index, (case, key)) in rows.iter().enumerate() {
        // What the identity service says this credential speaks for. Everything below is
        // compared with THIS value, never with a literal.
        let principal = caller_principal_id(&router, key, case).await;

        let created_user = served(
            &router,
            key,
            "CreateUser",
            json!({ "IdentityStoreId": STORE, "UserName": format!("writer-{index}") }),
            case,
        )
        .await;
        let user_id = created_user["UserId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateUser answers a UserId"))
            .to_string();
        let created_group = served(
            &router,
            key,
            "CreateGroup",
            json!({ "IdentityStoreId": STORE, "DisplayName": format!("writers-{index}") }),
            case,
        )
        .await;
        let group_id = created_group["GroupId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateGroup answers a GroupId"))
            .to_string();
        let created_membership = served(
            &router,
            key,
            "CreateGroupMembership",
            json!({
                "IdentityStoreId": STORE,
                "GroupId": group_id.clone(),
                "MemberId": { "UserId": user_id.clone() },
            }),
            case,
        )
        .await;
        let membership_id = created_membership["MembershipId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateGroupMembership answers an id"))
            .to_string();

        // Every reader of every one of the three records, as a client reads them.
        let described_user = served(
            &router,
            LONG_TERM_KEY,
            "DescribeUser",
            json!({ "IdentityStoreId": STORE, "UserId": user_id.clone() }),
            case,
        )
        .await;
        let users = served(
            &router,
            LONG_TERM_KEY,
            "ListUsers",
            json!({ "IdentityStoreId": STORE }),
            case,
        )
        .await;
        let described_group = served(
            &router,
            LONG_TERM_KEY,
            "DescribeGroup",
            json!({ "IdentityStoreId": STORE, "GroupId": group_id.clone() }),
            case,
        )
        .await;
        let groups = served(
            &router,
            LONG_TERM_KEY,
            "ListGroups",
            json!({ "IdentityStoreId": STORE }),
            case,
        )
        .await;
        let described_membership = served(
            &router,
            LONG_TERM_KEY,
            "DescribeGroupMembership",
            json!({ "IdentityStoreId": STORE, "MembershipId": membership_id.clone() }),
            case,
        )
        .await;
        let memberships = served(
            &router,
            LONG_TERM_KEY,
            "ListGroupMemberships",
            json!({ "IdentityStoreId": STORE, "GroupId": group_id.clone() }),
            case,
        )
        .await;

        let readers = [
            ("DescribeUser", described_user),
            ("ListUsers", listed(&users, "Users", "UserId", &user_id, case)),
            ("DescribeGroup", described_group),
            (
                "ListGroups",
                listed(&groups, "Groups", "GroupId", &group_id, case),
            ),
            ("DescribeGroupMembership", described_membership),
            (
                "ListGroupMemberships",
                listed(
                    &memberships,
                    "GroupMemberships",
                    "MembershipId",
                    &membership_id,
                    case,
                ),
            ),
        ];
        for (reader, body) in readers {
            let row = format!("{case} read by {reader}");
            // Nothing has updated the records yet, so the last write is the create.
            assert_audited(&body, &principal, &principal, &row);
        }

        per_row.push((*case, principal, user_id, group_id));
    }

    // Three credentials of three identities are three principals: a value that followed
    // the key TEXT, or one fixed value, would collapse these.
    for (case, principal, _, _) in &per_row {
        for (other_case, other, _, _) in &per_row {
            if case == other_case {
                continue;
            }
            assert_ne!(
                principal, other,
                "case {case:?}: expected a principal of its own beside {other_case:?}, \
                 got {principal:?} twice"
            );
        }
    }

    // An update is a write of its own: `UpdatedBy` becomes the UPDATING principal and
    // `CreatedBy` keeps the creating one, whichever credential made either.
    let (creating_case, creating_principal, user_id, group_id) = per_row[0].clone();
    let (updating_case, updating_principal, _, _) = per_row[2].clone();
    let updating_key = rows[2].1.clone();
    let case = format!("{creating_case} updated by {updating_case}");

    served(
        &router,
        &updating_key,
        "UpdateUser",
        json!({
            "IdentityStoreId": STORE,
            "UserId": user_id.clone(),
            "Operations": [{ "AttributePath": "title", "AttributeValue": "Provisioner" }],
        }),
        &case,
    )
    .await;
    let described_user = served(
        &router,
        LONG_TERM_KEY,
        "DescribeUser",
        json!({ "IdentityStoreId": STORE, "UserId": user_id }),
        &case,
    )
    .await;
    assert_audited(
        &described_user,
        &creating_principal,
        &updating_principal,
        &case,
    );

    served(
        &router,
        &updating_key,
        "UpdateGroup",
        json!({
            "IdentityStoreId": STORE,
            "GroupId": group_id.clone(),
            "Operations": [
                { "AttributePath": "description", "AttributeValue": "provisioned" },
            ],
        }),
        &case,
    )
    .await;
    let described_group = served(
        &router,
        LONG_TERM_KEY,
        "DescribeGroup",
        json!({ "IdentityStoreId": STORE, "GroupId": group_id.clone() }),
        &case,
    )
    .await;
    assert_audited(
        &described_group,
        &creating_principal,
        &updating_principal,
        &case,
    );
    let groups = served(
        &router,
        LONG_TERM_KEY,
        "ListGroups",
        json!({ "IdentityStoreId": STORE }),
        &case,
    )
    .await;
    assert_audited(
        &listed(&groups, "Groups", "GroupId", &group_id, &case),
        &creating_principal,
        &updating_principal,
        &case,
    );
}
