// SPDX-License-Identifier: CC0-1.0

//! The role NAME a hybrid activation's `IamRole` carries: how it addresses a role of this
//! account, and what the create answers when IAM holds no assumable role under it.
//!
//! RULE: `CreateActivation` resolves the `IamRole` value it is handed — whatever form the
//! name takes — to the role of THIS account that value names
//! (`arn:aws:iam::<account>:role/<IamRole>`; an IAM ARN carries no Region) and asks IAM
//! about it before anything is minted, because the machines registering with the activation
//! are managed under that role ("This IAM role must provide AssumeRole permissions for the
//! Amazon Web Services Systems Manager service principal ssm.amazonaws.com",
//! `smithy_query(query="doc", shape="CreateActivationRequest", member="IamRole")`). Where
//! IAM answers that it holds no such assumable role, the create is refused with HTTP 400,
//! wire error `ValidationException` and a `message` of the fixed sentence followed by that
//! ARN, and no activation is minted; where IAM holds a role under that name whose trust
//! policy admits this service, the identical value is served and the activation it minted is
//! reported by `DescribeActivations`.
//!
//! The sibling suite [`crate::hybrid_activation_role`] spans how a trust policy states the
//! grant, and this suite spans the other half of the rule — the FORM of the role name, which
//! is the only part of the refusal that varies: one character, every character class IAM
//! admits in a role name (`RoleName` is `roleNameType`, len 1..64, pattern `^[\w+=,.@-]+$`,
//! per `smithy_query(query="operation", operation="CreateRole")` on the IAM model), the
//! modelled 64-character maximum of this member (`IamRole`, `@length` min 0 max 64, per
//! `smithy_query(query="shape", shape="IamRole")`), and a role IAM holds under a nested
//! path. Each row is asserted BOTH ways: refused while IAM holds nothing under the name,
//! served once IAM holds a role under it that trusts this service. The router, the role
//! creation, the trust document and the refusal template are the sibling suite's, so the one
//! wording is written out in one place.
//!
//! Every role here is created through IAM's OWN `CreateRole`, and every activation through
//! this service's own operations: nothing reaches into either store.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::hybrid_activation_role::{
    create_role, expected_refusal, iam, listed_activations, router_with_iam, trusted_by_ssm,
};
use crate::parameter_store::{call, error_code};

/// The modelled maximum length of `IamRole` (`@length` min 0 max 64).
const IAM_ROLE_MAX_LENGTH: usize = 64;

/// The name-form table: `(case, the IAM path the role is created under, the name IAM holds
/// it under)`.
fn rows() -> Vec<(&'static str, &'static str, String)> {
    let prefix = "ssm-hybrid-";
    let max_length_name = format!("{prefix}{}", "m".repeat(IAM_ROLE_MAX_LENGTH - prefix.len()));
    assert_eq!(
        max_length_name.len(),
        IAM_ROLE_MAX_LENGTH,
        "the maximum-length row must sit exactly on the modelled bound, got {max_length_name:?}"
    );
    vec![
        ("a one-character name", "/", "r".to_string()),
        ("a plain name", "/", "ssm-hybrid-plain-name".to_string()),
        (
            "every character class an IAM role name admits",
            "/",
            "ssm_hybrid+role=1,role.name@edge-9".to_string(),
        ),
        (
            "a name at the modelled 64-character maximum",
            "/",
            max_length_name,
        ),
        (
            "a role IAM holds under a nested path",
            "/org/team/",
            "ssm-hybrid-under-a-nested-path".to_string(),
        ),
    ]
}

/// The `IamRole` value a row's create names: the role name, carrying the path IAM holds it
/// under, which is what this member takes.
fn iam_role_value(path: &str, role_name: &str) -> String {
    match path.trim_matches('/') {
        "" => role_name.to_string(),
        path => format!("{path}/{role_name}"),
    }
}

/// Assert the create naming `iam_role` is refused with the wire payload the rule states, and
/// that it minted nothing.
async fn assert_refused(router: &MultiServiceRouter, case: &str, iam_role: &str) {
    let (status, body) = call(router, "CreateActivation", &json!({ "IamRole": iam_role })).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the create to be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case {case:?}: expected a ValidationException, got {body}"
    );
    let expected = expected_refusal(iam_role);
    let message = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, expected,
        "case {case:?}: expected the refusal {expected:?}, got {message:?}"
    );
    assert!(
        body["ActivationId"].is_null() && body["ActivationCode"].is_null(),
        "case {case:?}: a refused create reports no activation, got {body}"
    );
    let listed = listed_activations(router, case).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: a refused create must mint no activation, got {listed:?}"
    );
}

/// RULE: whatever form the name takes, the create is refused while IAM holds no role under
/// it — with the ARN the value addresses in the message — and served once IAM holds a role
/// under it that this service may assume.
#[tokio::test]
async fn the_name_a_request_supplies_addresses_the_role_the_create_resolves() {
    for (case, path, role_name) in rows() {
        // A router per row, so each row's IAM store holds only its own role.
        let router = router_with_iam();
        let iam_role = iam_role_value(path, &role_name);

        // Nothing created yet: IAM answers that this account holds no such role.
        assert_refused(&router, case, &iam_role).await;

        // The same value, once IAM holds a role under it that trusts this service.
        create_role(&router, case, path, &role_name, &trusted_by_ssm()).await;
        let (status, body) = call(
            &router,
            "CreateActivation",
            &json!({ "IamRole": iam_role, "RegistrationLimit": 1 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served for a role IAM now holds, got \
             {status} with {body}"
        );
        let id = body["ActivationId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a served create reports an ActivationId, got {body}")
            })
            .to_string();
        let listed = listed_activations(&router, case).await;
        assert_eq!(
            listed,
            vec![id],
            "case {case:?}: exactly the activation the served create minted is reported, got \
             {listed:?}"
        );
    }
}

/// RULE, on the entity the name has to address: an `IamRole` value names a ROLE of this
/// account, so a name IAM holds another kind of entity under is still a name it holds no
/// role under, and the create is refused.
#[tokio::test]
async fn a_name_iam_holds_a_user_under_is_no_role() {
    let case = "a name IAM holds a user under, not a role";
    let router = router_with_iam();
    let name = "ssm-hybrid-held-as-a-user";
    iam(
        &router,
        case,
        "CreateUser",
        &[("UserName", name.to_string())],
    )
    .await;
    assert_refused(&router, case, name).await;
}

/// RULE, on what the message varies with: two creates naming different absent roles are
/// refused with the ARN each one's own value addresses, so the variable part comes from the
/// request rather than from anything the service holds.
#[tokio::test]
async fn each_refusal_names_the_role_its_own_request_asked_for() {
    let case = "two absent roles named in turn";
    let router = router_with_iam();
    for iam_role in ["ssm-hybrid-absent-one", "service-role/ssm-hybrid-absent-two"] {
        assert_refused(&router, case, iam_role).await;
    }
}
