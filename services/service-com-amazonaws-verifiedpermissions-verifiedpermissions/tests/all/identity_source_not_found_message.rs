// SPDX-License-Identifier: CC0-1.0

//! What the operations on a policy store's IDENTITY SOURCES answer when a
//! reference they resolve names nothing.
//!
//! RULE: such an operation's `ResourceNotFoundException` states the KIND of
//! resource that does not exist — `<Resource kind> does not exist.` — rather than
//! the fixed summary the CONTROL-PLANE resources' own not-found answers with, and
//! it reports the identity of what was missing in the modelled `resourceId` and
//! `resourceType` members, so the sentence carries no value out of the request.
//! That is the same sentence form the issuer lookup a token-based authorization
//! call makes already answers with (`Identity Source does not exist for issuer …`)
//! and the same form a reference to a user pool that does not exist is refused
//! with.
//!
//! The rows span every operation that resolves such a reference
//! (`CreateIdentitySource`, `GetIdentitySource`, `UpdateIdentitySource`,
//! `DeleteIdentitySource`, `ListIdentitySources`), the KIND of resource the
//! reference named (`POLICY_STORE`, `POLICY_STORE_ALIAS`, `IDENTITY_SOURCE`) and
//! the FORM the reference took (an id that was never minted, an id at the
//! modelled length bound, an id carrying every punctuation the pattern admits, an
//! alias reference naming no alias, an alias reference whose alias is awaiting
//! deletion, an identity source id of a live store).
//!
//! The other halves of this API keep their own wording, which
//! [`crate::error_messages`] (the control plane's fixed summary) and
//! [`crate::response_fidelity`] (an authorization call naming the resource it was
//! sent) pin; this suite re-checks the control-plane half for every reference it
//! sends, so the two cannot converge unnoticed.

use serde_json::{json, Value};

use crate::harness::{
    call, error_code, fault_message, ok, router, store_with_mode, user_pool, user_pool_arn,
    SIGNING_REGION,
};

/// The prefix every policy store alias name carries, from
/// `CreatePolicyStoreAliasInput$aliasName`.
const ALIAS_PREFIX: &str = "policy-store-alias/";

/// `PolicyStoreId`'s published upper length bound (`length 1..200`).
const MAX_POLICY_STORE_ID_LEN: usize = 200;

/// The sentence the control plane answers a not-found with, whatever was
/// missing — written out here rather than imported, so this suite states the
/// text the wire carries independently of the code under test.
const CONTROL_PLANE_NOT_FOUND: &str = "Resource not found";

/// Every value `ResourceType` admits, from
/// `smithy_query(query="shape", shape="ResourceType")`; a `resourceType` this
/// service reports has to be one of them.
const RESOURCE_TYPE_VALUES: &[&str] = &[
    "IDENTITY_SOURCE",
    "POLICY_STORE",
    "POLICY",
    "POLICY_TEMPLATE",
    "SCHEMA",
    "POLICY_STORE_ALIAS",
];

/// One row: a request on an identity-source operation whose reference resolves to
/// nothing, and what the refusal must say about it.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    /// The reference the request carried. It must NOT appear in the message.
    reference: String,
    /// The `resourceId` the exception must carry.
    resource_id: String,
    /// The `resourceType` the exception must carry.
    resource_type: &'static str,
    /// The whole sentence the message must be, written out per row rather than
    /// rendered from the resource type, so the test states the wording itself.
    message: &'static str,
}

/// A well-formed Cognito identity-source configuration naming `user_pool_arn`, so
/// a write fails on the reference the row is about rather than on its provider.
fn cognito_configuration(user_pool_arn: &str) -> Value {
    json!({ "cognitoUserPoolConfiguration": { "userPoolArn": user_pool_arn } })
}

#[tokio::test]
async fn an_identity_source_not_found_states_the_resource_kind() {
    let router = router();
    // The pool every write row references really exists, so no row fails on its
    // provider reference instead of on the reference under test.
    let pool_arn = user_pool_arn(
        SIGNING_REGION,
        &user_pool(&router, "identity-source-not-found-pool").await,
    );

    // A live store, for the rows whose store resolves and whose identity source
    // does not.
    let live_store = store_with_mode(&router, "OFF").await;

    // An alias of that live store, soft-deleted: `DeletePolicyStoreAlias`
    // documents that a `PendingDeletion` alias used in a `policyStoreId` member
    // "will fail with a `ResourceNotFound` exception".
    let pending_alias = format!("{ALIAS_PREFIX}awaiting-deletion");
    ok(
        &router,
        "CreatePolicyStoreAlias",
        &json!({ "aliasName": pending_alias, "policyStoreId": live_store }),
    )
    .await;
    ok(
        &router,
        "DeletePolicyStoreAlias",
        &json!({ "aliasName": pending_alias }),
    )
    .await;

    let unknown_store = "PSNONEXISTENT00000000".to_string();
    // Exactly at `PolicyStoreId`'s published length bound.
    let longest_store = format!("PS{}", "a".repeat(MAX_POLICY_STORE_ID_LEN - 2));
    // Every punctuation character `PolicyStoreId`'s pattern `^[a-zA-Z0-9-/_]*$`
    // admits, in one reference.
    let punctuated_store = "PS-store_1/2".to_string();
    let unknown_alias = format!("{ALIAS_PREFIX}no-such-alias");
    let unknown_source = "ISDOESNOTEXIST000000".to_string();
    // A second identity source id, differing in every character class the
    // member's pattern `^[a-zA-Z0-9-]*$` admits.
    let other_unknown_source = "IS-ANOTHER-missing-02".to_string();

    const STORE_MISSING: &str = "Policy Store does not exist.";
    const ALIAS_MISSING: &str = "Policy Store Alias does not exist.";
    const SOURCE_MISSING: &str = "Identity Source does not exist.";

    let rows = vec![
        Row {
            case: "CreateIdentitySource, a store id that was never minted",
            operation: "CreateIdentitySource",
            request: json!({
                "policyStoreId": unknown_store,
                "configuration": cognito_configuration(&pool_arn),
            }),
            reference: unknown_store.clone(),
            resource_id: unknown_store.clone(),
            resource_type: "POLICY_STORE",
            message: STORE_MISSING,
        },
        Row {
            case: "CreateIdentitySource, a store id at the modelled length bound",
            operation: "CreateIdentitySource",
            request: json!({
                "policyStoreId": longest_store,
                "configuration": cognito_configuration(&pool_arn),
            }),
            reference: longest_store.clone(),
            resource_id: longest_store.clone(),
            resource_type: "POLICY_STORE",
            message: STORE_MISSING,
        },
        Row {
            // An alias reference names an ALIAS, by its bare name: that is the
            // resource the request referenced, so that is the kind the sentence
            // states.
            case: "CreateIdentitySource, an alias reference naming no alias",
            operation: "CreateIdentitySource",
            request: json!({
                "policyStoreId": unknown_alias,
                "configuration": cognito_configuration(&pool_arn),
            }),
            reference: unknown_alias.clone(),
            resource_id: "no-such-alias".to_string(),
            resource_type: "POLICY_STORE_ALIAS",
            message: ALIAS_MISSING,
        },
        Row {
            case: "CreateIdentitySource, an alias awaiting deletion",
            operation: "CreateIdentitySource",
            request: json!({
                "policyStoreId": pending_alias,
                "configuration": cognito_configuration(&pool_arn),
            }),
            reference: pending_alias.clone(),
            resource_id: "awaiting-deletion".to_string(),
            resource_type: "POLICY_STORE_ALIAS",
            message: ALIAS_MISSING,
        },
        Row {
            case: "GetIdentitySource, an id carrying every admitted punctuation",
            operation: "GetIdentitySource",
            request: json!({
                "policyStoreId": punctuated_store,
                "identitySourceId": unknown_source,
            }),
            reference: punctuated_store.clone(),
            resource_id: punctuated_store.clone(),
            resource_type: "POLICY_STORE",
            message: STORE_MISSING,
        },
        Row {
            case: "GetIdentitySource, a live store and an identity source it does not hold",
            operation: "GetIdentitySource",
            request: json!({
                "policyStoreId": live_store,
                "identitySourceId": unknown_source,
            }),
            reference: unknown_source.clone(),
            resource_id: unknown_source.clone(),
            resource_type: "IDENTITY_SOURCE",
            message: SOURCE_MISSING,
        },
        Row {
            // A second identity source id, in another form: the sentence is the
            // same, which is what makes it a constant rather than a template.
            case: "GetIdentitySource, a live store and another unheld identity source id",
            operation: "GetIdentitySource",
            request: json!({
                "policyStoreId": live_store,
                "identitySourceId": other_unknown_source,
            }),
            reference: other_unknown_source.clone(),
            resource_id: other_unknown_source.clone(),
            resource_type: "IDENTITY_SOURCE",
            message: SOURCE_MISSING,
        },
        Row {
            case: "UpdateIdentitySource, a store id that was never minted",
            operation: "UpdateIdentitySource",
            request: json!({
                "policyStoreId": unknown_store,
                "identitySourceId": unknown_source,
                "updateConfiguration": {
                    "cognitoUserPoolConfiguration": { "userPoolArn": pool_arn },
                },
            }),
            reference: unknown_store.clone(),
            resource_id: unknown_store.clone(),
            resource_type: "POLICY_STORE",
            message: STORE_MISSING,
        },
        Row {
            case: "UpdateIdentitySource, a live store and an identity source it does not hold",
            operation: "UpdateIdentitySource",
            request: json!({
                "policyStoreId": live_store,
                "identitySourceId": unknown_source,
                "updateConfiguration": {
                    "cognitoUserPoolConfiguration": { "userPoolArn": pool_arn },
                },
            }),
            reference: unknown_source.clone(),
            resource_id: unknown_source.clone(),
            resource_type: "IDENTITY_SOURCE",
            message: SOURCE_MISSING,
        },
        Row {
            case: "DeleteIdentitySource, an alias reference naming no alias",
            operation: "DeleteIdentitySource",
            request: json!({
                "policyStoreId": unknown_alias,
                "identitySourceId": unknown_source,
            }),
            reference: unknown_alias.clone(),
            resource_id: "no-such-alias".to_string(),
            resource_type: "POLICY_STORE_ALIAS",
            message: ALIAS_MISSING,
        },
        Row {
            case: "DeleteIdentitySource, a live store and an identity source it does not hold",
            operation: "DeleteIdentitySource",
            request: json!({
                "policyStoreId": live_store,
                "identitySourceId": unknown_source,
            }),
            reference: unknown_source.clone(),
            resource_id: unknown_source.clone(),
            resource_type: "IDENTITY_SOURCE",
            message: SOURCE_MISSING,
        },
        Row {
            case: "ListIdentitySources, a store id that was never minted",
            operation: "ListIdentitySources",
            request: json!({ "policyStoreId": unknown_store }),
            reference: unknown_store.clone(),
            resource_id: unknown_store.clone(),
            resource_type: "POLICY_STORE",
            message: STORE_MISSING,
        },
        Row {
            case: "ListIdentitySources, an alias awaiting deletion",
            operation: "ListIdentitySources",
            request: json!({ "policyStoreId": pending_alias }),
            reference: pending_alias.clone(),
            resource_id: "awaiting-deletion".to_string(),
            resource_type: "POLICY_STORE_ALIAS",
            message: ALIAS_MISSING,
        },
    ];

    for Row {
        case,
        operation,
        request,
        reference,
        resource_id,
        resource_type,
        message: expected,
    } in &rows
    {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException, got {code:?} {body}"
        );

        let message = fault_message(&body);
        assert_eq!(
            message, *expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(reference.as_str()),
            "case {case:?}: the message must not carry the reference {reference:?}, got \
             {message:?}"
        );
        assert!(
            !message.contains(resource_id.as_str()),
            "case {case:?}: the message must not carry the resource id {resource_id:?}, got \
             {message:?}"
        );

        // The identity of what was missing travels in the members that model it,
        // so the constant sentence withholds nothing a client needs.
        assert_eq!(
            body["resourceId"].as_str().unwrap_or_default(),
            resource_id.as_str(),
            "case {case:?}: expected resourceId {resource_id:?}, got {body}"
        );
        assert_eq!(
            body["resourceType"].as_str().unwrap_or_default(),
            *resource_type,
            "case {case:?}: expected resourceType {resource_type:?}, got {body}"
        );
        assert!(
            RESOURCE_TYPE_VALUES.contains(resource_type),
            "case {case:?}: {resource_type:?} is not one of the modelled ResourceType values"
        );

        // The CONTROL PLANE, asked about the same policy-store reference, keeps
        // its own fixed summary: the two halves word their not-found differently,
        // which is the other side of this rule.
        if *resource_type != "IDENTITY_SOURCE" {
            let (control_status, control_body) = call(
                &router,
                "GetPolicyStore",
                &json!({ "policyStoreId": reference }),
            )
            .await;
            assert_eq!(
                control_status, 404,
                "case {case:?}: expected the control-plane read to 404, got {control_status} \
                 {control_body}"
            );
            let control_message = fault_message(&control_body);
            assert_eq!(
                control_message, CONTROL_PLANE_NOT_FOUND,
                "case {case:?}: expected the control plane's {CONTROL_PLANE_NOT_FOUND:?}, got \
                 {control_message:?}"
            );
        }
    }
}

/// The sentence does not vary between two calls, and two DIFFERENT references of
/// the same kind read alike: nothing of the request enters it, so the whole fault
/// differs only in the members that report the reference.
#[tokio::test]
async fn the_identity_source_not_found_sentence_is_the_same_for_every_reference() {
    let router = router();
    let first = json!({ "policyStoreId": "PSMISSINGMISSINGMISS1" });
    let second = json!({ "policyStoreId": "PSMISSINGMISSINGMISS2" });

    let (_, repeated_a) = call(&router, "ListIdentitySources", &first).await;
    let (_, repeated_b) = call(&router, "ListIdentitySources", &first).await;
    assert_eq!(
        repeated_a, repeated_b,
        "case \"the same reference twice\": the whole fault must be identical, got \
         {repeated_a} then {repeated_b}"
    );

    let (_, other) = call(&router, "ListIdentitySources", &second).await;
    assert_eq!(
        fault_message(&other),
        fault_message(&repeated_a),
        "case \"two different references\": the sentence states the resource KIND only, so it \
         must be the same, got {other} and {repeated_a}"
    );
    assert_ne!(
        other["resourceId"], repeated_a["resourceId"],
        "case \"two different references\": the reference must still be reported in \
         resourceId, got {other} and {repeated_a}"
    );
}
