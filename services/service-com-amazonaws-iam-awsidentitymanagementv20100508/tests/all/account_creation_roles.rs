// SPDX-License-Identifier: CC0-1.0

//! The roles an account HOLDS from the moment it is created, read end to end over the
//! awsQuery wire.
//!
//! RULE: an Amazon Web Services account does not come into existence holding no IAM role —
//! AWS creates the service-linked role of each of its account-wide services with the
//! account itself — so every enumeration of the account's roles reports those roles beside
//! the roles the account created of its own, each under the name AWS publishes for its
//! service, at the path IAM reserves for a service-linked role, carrying the policy that
//! service controls it through. A role enumeration that reported only what the account
//! created would leave them out of every snapshot of the account's permissions, and would
//! answer a different document shape than AWS for an account that created one role.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the
//! service whose role AWS creates, how many roles of its own the account has created (none,
//! one, several), and which enumeration the roles are read through. The awsQuery harness
//! (router, call, element, elements) is the one `tests/all/user_lifecycle.rs` owns, so every
//! suite in this crate builds and reads a request the same way.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::user_lifecycle::{account_id, call, document_element, element, elements, router};

/// A trust policy with the one statement a role's trust document carries, for the roles
/// these suites create of the account's own.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The service principals whose service-linked role AWS creates WITH the account, paired
/// with the role name and the policy AWS publishes for each.
///
/// Written out by hand so these suites pin the CONTENT independently of the tables the
/// service reads. Source: the "Using service-linked roles" page of each service's own user
/// guide — AWS Support's, which says AWS Support creates AWSServiceRoleForSupport when the
/// account is created, and Trusted Advisor's, which says AWSServiceRoleForTrustedAdvisor is
/// created with the account — the same documentation
/// `src/state/account_creation_roles.rs` cites.
const ACCOUNT_CREATION_ROWS: &[(&str, &str, &str)] = &[
    (
        "support.amazonaws.com",
        "AWSServiceRoleForSupport",
        "AWSSupportServiceRolePolicy",
    ),
    (
        "trustedadvisor.amazonaws.com",
        "AWSServiceRoleForTrustedAdvisor",
        "AWSTrustedAdvisorServiceRolePolicy",
    ),
];

/// `names` plus the roles AWS creates with the account, in the order the service enumerates
/// roles: ascending by the case-folded name, which is the key its role store is ordered on.
///
/// The one place a suite in this crate turns the roles a fixture created into the roles the
/// ACCOUNT holds, so every suite that enumerates or counts roles names the account-creation
/// roles the same way (`tests/all/role_listing.rs`,
/// `tests/all/account_authorization_details.rs`).
pub(crate) fn roles_in_account_order(names: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = names
        .iter()
        .copied()
        .chain(state::account_creation_role_names())
        .map(ToString::to_string)
        .collect();
    all.sort_by_key(|name| name.to_ascii_lowercase());
    all
}

/// The path the account holds `role_name` under when AWS created that role with the
/// account: the path IAM reserves for the service-linked role of its service.
///
/// `None` for every other name, so a suite enumerating the account's roles can tell a role
/// AWS created with the account from one a fixture created.
pub(crate) fn account_creation_role_path(role_name: &str) -> Option<String> {
    state::ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS
        .iter()
        .find(|principal| {
            state::published_service_linked_role_name(principal)
                .is_some_and(|published| published.eq_ignore_ascii_case(role_name))
        })
        .map(|principal| format!("/aws-service-role/{principal}/"))
}

/// The KEYS the account's role store holds those roles under: the case-folded name, since
/// IAM names "are not distinguished by case".
///
/// What a suite reading the store directly sees before anything has created a role
/// (`tests/all/role_template_acquisition.rs`).
pub(crate) fn account_creation_role_keys() -> Vec<String> {
    state::account_creation_role_names()
        .into_iter()
        .map(state::name_key)
        .collect()
}

/// The ARNs of the policies those roles carry: the policy AWS publishes for each of their
/// services, under the path IAM reserves for a service-linked role's policy.
///
/// The account USES each of them — they are attached to a role it holds — which is what puts
/// an AWS-owned policy in a report of this account's configuration
/// (`tests/all/account_authorization_details.rs`).
pub(crate) fn account_creation_role_policy_arns() -> Vec<String> {
    account_creation_role_policies()
        .into_iter()
        .map(|(_, arn)| arn)
        .collect()
}

/// Each role AWS creates with the account paired with the ARN of the policy it carries, in
/// the order the account enumerates its roles — what a report that walks the account's roles
/// carries for them (`tests/all/service_linked_role_removal.rs`).
pub(crate) fn account_creation_role_policies() -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = state::ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS
        .iter()
        .filter_map(|principal| {
            let role_name = state::published_service_linked_role_name(principal)?;
            let policy = state::service_linked_role_policy(principal)?;
            Some((role_name.to_string(), policy.arn()))
        })
        .collect();
    pairs.sort_by_key(|(role_name, _)| role_name.to_ascii_lowercase());
    pairs
}

/// The role names one `ListRoles` page reports, in document order.
async fn listed_role_names(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
) -> Vec<String> {
    let (status, body) = call(router, "ListRoles", &[("MaxItems", "1000")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListRoles must succeed, got {status}: {body}"
    );
    elements(&body, "RoleName")
}

/// The role names the account's authorization details report, in document order.
async fn reported_role_names(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
) -> Vec<String> {
    let (status, body) = call(
        router,
        "GetAccountAuthorizationDetails",
        &[("Filter.member.1", "Role"), ("MaxItems", "1000")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetAccountAuthorizationDetails must succeed, got {status}: {body}"
    );
    elements(&body, "RoleName")
}

/// Create one role of the account's own, asserting the fixture create succeeded.
async fn create_role(router: &smithy_server_runtime::MultiServiceRouter, role_name: &str) {
    let (status, body) = call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {role_name:?}: the fixture CreateRole must succeed, got {status}: {body}"
    );
}

/// RULE: the role AWS creates with the account is the one its own documentation names for
/// that service, so a read of the account reports it under that name, at the path IAM
/// reserves for a service-linked role, with the ARN of that path and name and a trust
/// policy whose one statement lets that service assume it.
#[tokio::test]
async fn the_account_holds_the_role_aws_creates_for_each_of_its_account_wide_services() {
    let router = router();

    for (service_principal, published_name, _) in ACCOUNT_CREATION_ROWS {
        let case = *published_name;

        // The row this suite writes out is a row the service reads.
        assert!(
            state::ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS.contains(service_principal),
            "case {case:?}: expected {service_principal:?} among the principals whose role \
             AWS creates with the account, got {:?}",
            state::ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS
        );
        assert_eq!(
            state::published_service_linked_role_name(service_principal),
            Some(*published_name),
            "case {case:?}: expected the published name of {service_principal:?}, got {:?}",
            state::published_service_linked_role_name(service_principal)
        );

        let (status, body) = call(&router, "GetRole", &[("RoleName", published_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the role to be held, got {status}: {body}"
        );
        let expected_path = format!("/aws-service-role/{service_principal}/");
        assert_eq!(
            element(&body, "Path"),
            expected_path,
            "case {case:?}: expected the service-linked role path, got {:?}",
            element(&body, "Path")
        );
        assert_eq!(
            element(&body, "Arn"),
            format!(
                "arn:aws:iam::{}:role{expected_path}{published_name}",
                account_id()
            ),
            "case {case:?}: expected the ARN of that path and name, got {:?}",
            element(&body, "Arn")
        );
        let trust_policy = document_element(&body, "AssumeRolePolicyDocument");
        let document: serde_json::Value = serde_json::from_str(&trust_policy)
            .unwrap_or_else(|error| panic!("case {case:?}: {error} in {trust_policy:?}"));
        assert_eq!(
            document["Statement"][0]["Principal"]["Service"],
            serde_json::json!(*service_principal),
            "case {case:?}: expected the trust policy to name the service, got {:?}",
            document["Statement"][0]["Principal"]["Service"]
        );
    }
}

/// RULE: "the service controls the attached policies" of a service-linked role, so a role
/// AWS created with the account carries the policy AWS files for that service — reported by
/// the role's own attachment listing and by the account's authorization details.
#[tokio::test]
async fn each_account_creation_role_carries_the_policy_its_service_controls_it_through() {
    let router = router();

    for (service_principal, published_name, policy_name) in ACCOUNT_CREATION_ROWS {
        let case = *published_name;

        // The pairing this row writes out is the one the service reads.
        assert_eq!(
            state::service_linked_role_policy(service_principal).map(|policy| policy.policy_name),
            Some(*policy_name),
            "case {case:?}: expected {policy_name:?} to be the policy of {service_principal:?}"
        );

        let (status, body) = call(
            &router,
            "ListAttachedRolePolicies",
            &[("RoleName", published_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {body}"
        );
        // An AWS-managed policy's ARN ends in its path and friendly name, and the path AWS
        // reserves for a service-linked role's policy is `/aws-service-role/`.
        let expected_arn = format!("arn:aws:iam::aws:policy/aws-service-role/{policy_name}");
        assert_eq!(
            elements(&body, "PolicyName"),
            vec![(*policy_name).to_string()],
            "case {case:?}: expected the service's own policy, got {:?}",
            elements(&body, "PolicyName")
        );
        assert_eq!(
            elements(&body, "PolicyArn"),
            vec![expected_arn.clone()],
            "case {case:?}: expected the policy's own ARN, got {:?}",
            elements(&body, "PolicyArn")
        );

        // The attachment resolves a policy a reader of it can report, and the account USES
        // it, so the authorization details report that policy too.
        let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", expected_arn.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the attached policy to resolve, got {status}: {body}"
        );
        let (status, body) = call(
            &router,
            "GetAccountAuthorizationDetails",
            &[
                ("Filter.member.1", "AWSManagedPolicy"),
                ("MaxItems", "1000"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {body}"
        );
        assert!(
            elements(&body, "Arn").contains(&expected_arn),
            "case {case:?}: expected the policy the account uses to be reported, got {:?}",
            elements(&body, "Arn")
        );
    }
}

/// RULE: every enumeration of the account's roles reports the roles AWS created with the
/// account beside the roles the account created of its own — so the set a listing pages over
/// and the set the authorization details report are the same set, whatever the account has
/// created.
#[tokio::test]
async fn every_enumeration_of_the_accounts_roles_reports_them() {
    let router = router();

    // Rows: an account that has created no role of its own, then one, two and three — the
    // cardinalities an enumeration has to follow, including the single-role account whose
    // report would otherwise carry one entry.
    let mut created: Vec<&str> = Vec::new();
    for role_name in ["", "Deployer", "Auditor", "auditor_2"] {
        if !role_name.is_empty() {
            create_role(&router, role_name).await;
            created.push(role_name);
        }
        let case = if created.is_empty() {
            "no role of its own".to_string()
        } else {
            format!("{} role(s) of its own", created.len())
        };
        let expected = roles_in_account_order(&created);
        assert_eq!(
            listed_role_names(&router, &case).await,
            expected,
            "case {case:?}: expected the listing to report every role the account holds"
        );
        assert_eq!(
            reported_role_names(&router, &case).await,
            expected,
            "case {case:?}: expected one report entry per role the account holds"
        );
        // Every one of them resolves on its own, which is what makes the enumeration a
        // report of records rather than a list of names.
        for role_name in &expected {
            let (status, body) = call(&router, "GetRole", &[("RoleName", role_name.as_str())]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {role_name:?} to resolve, got {status}: {body}"
            );
        }
    }
}

/// RULE: a role AWS created with the account is the account's own record of that role, so
/// the account can give it up like any other service-linked role: the submit is accepted,
/// no read reports a role under that name afterwards, and a create under it brings it back.
#[tokio::test]
async fn a_role_aws_created_with_the_account_can_be_given_up_and_created_again() {
    for (service_principal, published_name, _) in ACCOUNT_CREATION_ROWS {
        // A router of its own per row, so one row's deletion never explains another's.
        let router = router();
        let case = *published_name;

        // The name is taken: the account already holds a service role under it, which is
        // what a second create of it is refused for.
        let (status, body) = call(
            &router,
            "CreateServiceLinkedRole",
            &[("AWSServiceName", service_principal)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the taken name to be refused, got {status}: {body}"
        );
        assert_eq!(
            element(&body, "Message"),
            format!(
                "Service role name {published_name} has been taken in this account, please \
                 try a different suffix"
            ),
            "case {case:?}: expected the taken-name refusal, got {:?}",
            element(&body, "Message")
        );

        let (status, body) = call(
            &router,
            "DeleteServiceLinkedRole",
            &[("RoleName", published_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the deletion to be submitted, got {status}: {body}"
        );
        // Twice, because a client POLLS for the role to be gone: the answer is the same
        // every read.
        for _ in 0..2 {
            let (status, body) = call(&router, "GetRole", &[("RoleName", published_name)]).await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected the given-up role to be gone, got {status}: {body}"
            );
        }
        assert!(
            !listed_role_names(&router, case).await.contains(&(*published_name).to_string()),
            "case {case:?}: expected the given-up role to be out of the account's roles, got {:?}",
            listed_role_names(&router, case).await
        );

        // And the name is free again: the service creates its role under it once more.
        let (status, body) = call(
            &router,
            "CreateServiceLinkedRole",
            &[("AWSServiceName", service_principal)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to succeed, got {status}: {body}"
        );
        assert_eq!(
            element(&body, "RoleName"),
            *published_name,
            "case {case:?}: expected the role created under its published name, got {:?}",
            element(&body, "RoleName")
        );
        assert!(
            listed_role_names(&router, case).await.contains(&(*published_name).to_string()),
            "case {case:?}: expected the re-created role to be reported, got {:?}",
            listed_role_names(&router, case).await
        );
    }
}

/// RULE: the roles AWS created with the account are roles of the account like any other, so
/// the enumeration follows the account's own writes: a role it creates joins them and a role
/// it gives up leaves them, and nothing a read does changes the set.
#[tokio::test]
async fn the_enumeration_follows_what_the_account_holds() {
    let router = router();
    let held = roles_in_account_order(&[]);

    // Reading them, twice, stores nothing: the set is the same afterwards.
    for role_name in &held {
        for _ in 0..2 {
            let (status, body) =
                call(&router, "GetRole", &[("RoleName", role_name.as_str())]).await;
            assert_eq!(
                status, 200,
                "case {role_name:?}: expected 200, got {status}: {body}"
            );
        }
    }
    assert_eq!(
        listed_role_names(&router, "after the reads").await,
        held,
        "a read changed the account's roles"
    );

    // A role the account creates joins the set, in the account's enumeration order.
    create_role(&router, "Deployer").await;
    assert_eq!(
        listed_role_names(&router, "with a created role").await,
        roles_in_account_order(&["Deployer"]),
        "expected the created role to be reported beside the account-creation roles"
    );

    // And a role the account gives up leaves it: the service-linked role of a service whose
    // role AWS did NOT create with the account is created and then given up, which is the
    // write that takes a role out of the set.
    let (status, body) = call(
        &router,
        "CreateServiceLinkedRole",
        &[("AWSServiceName", "elasticloadbalancing.amazonaws.com")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture create must succeed, got {status}: {body}"
    );
    let linked = element(&body, "RoleName");
    assert_eq!(
        listed_role_names(&router, "with a service-linked role").await,
        roles_in_account_order(&["Deployer", linked.as_str()]),
        "expected the service-linked role to be reported beside the others"
    );
    let (status, body) = call(
        &router,
        "DeleteServiceLinkedRole",
        &[("RoleName", linked.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the deletion to be submitted, got {status}: {body}"
    );
    assert_eq!(
        listed_role_names(&router, "after the deletion").await,
        roles_in_account_order(&["Deployer"]),
        "expected the given-up role to be out of the account's roles"
    );
}
