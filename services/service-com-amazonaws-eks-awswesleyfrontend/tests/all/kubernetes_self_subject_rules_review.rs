// SPDX-License-Identifier: CC0-1.0

//! What a client is told when it asks a cluster's Kubernetes API server WHAT it may
//! do in a namespace.
//!
//! The RULE under test: the API server serves `authorization.k8s.io/v1`'s
//! `selfsubjectrulesreviews` resource — a `SelfSubjectRulesReview` "enumerates the
//! set of actions the current user can perform within a namespace", so the answer is
//! the rules in force for the asking identity THERE, collected from the same state
//! the authorizer decides a request by: the roles Kubernetes publishes for the
//! groups the identity is in, and the roles the cluster's own bindings bind it to.
//! The rules of an authorizer that is asked one request at a time cannot be
//! enumerated — which is what Amazon EKS serves a cluster's ACCESS ENTRIES by — so a
//! listing taken while that authorizer is in the cluster's chain leaves its rules
//! out and reports itself `incomplete`, and one taken on a cluster without it is
//! complete. A review that names no namespace asks about nothing and is a bad
//! request.
//!
//! The suites drive that over a TABLE whose rows differ along what the rule
//! generalises over: the cluster's authorizer chain (a cluster whose access entries
//! that authorizer serves, and one that authenticates through its `aws-auth`
//! ConfigMap instead), the namespace asked about (one a binding reaches and one it
//! does not), the permissions the identity holds (the cluster's own creator, and an
//! identity holding only the read-only access policy scoped to one namespace), the
//! source of the rule (an access policy, the groups every authenticated client
//! carries, and a `Role` the client itself created) and the kind of rule (a resource
//! rule and a non-resource one).
//!
//! An incomplete listing also names the error the authorizer it could not enumerate
//! answered with, which is the sentence a webhook authorizer reports when it is
//! asked to resolve a user's rules.
//!
//! Every row asserts the listing AGAINST the server: a rule it reports must be
//! allowed by the `SelfSubjectAccessReview` the same server answers — a listing that
//! claims a permission the server refuses is the defect the resource would have —
//! and a listing that reports itself COMPLETE must leave no allowed action out.

use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_rbac::WEBHOOK_RULE_RESOLUTION_ERROR;
use service_com_amazonaws_eks_awswesleyfrontend::state::{
    access_policy_arn, ACCESS_SCOPE_TYPE_NAMESPACE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, kubeconfig_of, poll_until_active, Kubeconfig};
use crate::wire::{call, emulator, encode_label, Answer};

/// The path a rules review is created at.
const RULES_REVIEWS_PATH: &str = "/apis/authorization.k8s.io/v1/selfsubjectrulesreviews";

/// The path an access review — the one-action question — is created at, which every
/// row cross-checks the reported rules against.
const ACCESS_REVIEWS_PATH: &str = "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews";

/// The access policy AWS publishes as granting read-only access to a cluster.
const VIEW_POLICY: &str = "AmazonEKSViewPolicy";

/// The authentication mode of a cluster whose access entries the access-entry
/// authorizer serves, and the one of a cluster that authenticates through its
/// `aws-auth` ConfigMap instead — the two ends of `AuthenticationMode`.
const API_MODE: &str = "API";
const CONFIG_MAP_MODE: &str = "CONFIG_MAP";

/// The namespace a namespace-scoped association and the created `Role` are confined
/// to.
const SCOPED_NAMESPACE: &str = "default";

/// Another namespace, which neither reaches.
const OTHER_NAMESPACE: &str = "kube-system";

/// Create a cluster whose Kubernetes API this suite dials, under `mode`, with the
/// creator's bootstrap admin permissions as the case asks for them.
async fn provisioned_cluster(
    router: &MultiServiceRouter,
    name: &str,
    mode: &str,
    bootstrap_creator_admin: bool,
) -> Kubeconfig {
    let body = serde_json::json!({
        "name": name,
        "roleArn": "arn:aws:iam::000000000000:role/eksClusterRole",
        "resourcesVpcConfig": { "subnetIds": ["subnet-aaaa1111", "subnet-bbbb2222"] },
        "accessConfig": {
            "authenticationMode": mode,
            "bootstrapClusterCreatorAdminPermissions": bootstrap_creator_admin,
        },
    });
    let created = call(router, REGION, "POST", "/clusters", &body.to_string()).await;
    assert_eq!(
        created.status, 200,
        "setup: expected 200 from CreateCluster({name}), got {created:?}"
    );
    let described = poll_until_active(router, REGION, name).await;
    kubeconfig_of(name, &described)
}

/// `CreateAccessEntry` for the IAM principal the dialling credential speaks for,
/// then `AssociateAccessPolicy` of `policy_name` over `namespaces`.
async fn grant_scoped_policy(
    router: &MultiServiceRouter,
    cluster: &str,
    principal_arn: &str,
    policy_name: &str,
    namespaces: &[&str],
) {
    let entry: Answer = call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/access-entries", encode_label(cluster)),
        &serde_json::json!({ "principalArn": principal_arn }).to_string(),
    )
    .await;
    assert_eq!(
        entry.status, 200,
        "setup: expected 200 from CreateAccessEntry, got {entry:?}"
    );
    let body = serde_json::json!({
        "policyArn": access_policy_arn(policy_name),
        "accessScope": {
            "type": ACCESS_SCOPE_TYPE_NAMESPACE,
            "namespaces": namespaces,
        },
    });
    let associated: Answer = call(
        router,
        REGION,
        "POST",
        &format!(
            "/clusters/{}/access-entries/{}/access-policies",
            encode_label(cluster),
            encode_label(principal_arn)
        ),
        &body.to_string(),
    )
    .await;
    assert_eq!(
        associated.status, 200,
        "setup: expected 200 from AssociateAccessPolicy, got {associated:?}"
    );
}

/// The `status` of the rules review of `namespace`, asserted to be the answered
/// object: `201 Created`, the review's own kind, the spec echoed back, and the
/// completeness `incomplete` states.
async fn reviewed_rules(
    config: &Kubeconfig,
    case: &str,
    namespace: &str,
    incomplete: bool,
) -> serde_json::Value {
    let spec = serde_json::json!({ "namespace": namespace });
    let answered = dial(
        config,
        case,
        "POST",
        RULES_REVIEWS_PATH,
        Some(&serde_json::json!({
            "apiVersion": "authorization.k8s.io/v1",
            "kind": "SelfSubjectRulesReview",
            "spec": spec,
        })),
    )
    .await;
    assert_eq!(
        answered.status, 201,
        "case {case:?}: a review must be created, got {answered:?}"
    );
    assert_eq!(
        answered.body["kind"],
        serde_json::json!("SelfSubjectRulesReview"),
        "case {case:?}: the answer is the review object, got {answered:?}"
    );
    assert_eq!(
        answered.body["spec"], spec,
        "case {case:?}: the review echoes the spec it was asked about, got {answered:?}"
    );
    assert_eq!(
        answered.body["status"]["incomplete"],
        serde_json::json!(incomplete),
        "case {case:?}: expected incomplete={incomplete}, got {answered:?}"
    );
    // An incomplete listing names the error the authorizer it could not enumerate
    // answered with; a complete one reports none.
    let expected_error = if incomplete {
        serde_json::json!(WEBHOOK_RULE_RESOLUTION_ERROR)
    } else {
        serde_json::Value::Null
    };
    assert_eq!(
        answered.body["status"]["evaluationError"], expected_error,
        "case {case:?}: expected evaluationError {expected_error:?}, got {answered:?}"
    );
    answered.body["status"].clone()
}

/// The reported `resourceRules`.
fn resource_rules(case: &str, status: &serde_json::Value) -> Vec<serde_json::Value> {
    status["resourceRules"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a review reports resourceRules, got {status:?}"))
        .clone()
}

/// Whether `list` names `wanted`: the wildcard RBAC gives every member of a rule, or
/// the value itself. Written here rather than read from the emulator, so the test
/// judges the reported rules by RBAC's own reading of them.
fn names(list: Option<&Vec<serde_json::Value>>, wanted: &str) -> bool {
    list.is_some_and(|values| {
        values
            .iter()
            .any(|value| value == &serde_json::json!("*") || value == &serde_json::json!(wanted))
    })
}

/// Whether the reported rules admit `verb` on `resource` of `group`.
fn rules_admit(rules: &[serde_json::Value], verb: &str, group: &str, resource: &str) -> bool {
    rules.iter().any(|rule| {
        names(rule["verbs"].as_array(), verb)
            && names(rule["apiGroups"].as_array(), group)
            && names(rule["resources"].as_array(), resource)
    })
}

/// Whether the API server's one-action review allows `verb` on `resource` of `group`
/// in `namespace`.
async fn access_review_allows(
    config: &Kubeconfig,
    case: &str,
    namespace: &str,
    verb: &str,
    group: &str,
    resource: &str,
) -> bool {
    let answered = dial(
        config,
        case,
        "POST",
        ACCESS_REVIEWS_PATH,
        Some(&serde_json::json!({
            "apiVersion": "authorization.k8s.io/v1",
            "kind": "SelfSubjectAccessReview",
            "spec": { "resourceAttributes": {
                "verb": verb,
                "group": group,
                "resource": resource,
                "namespace": namespace,
            } },
        })),
    )
    .await;
    assert_eq!(
        answered.status, 201,
        "case {case:?}: an access review must be created, got {answered:?}"
    );
    answered.body["status"]["allowed"]
        .as_bool()
        .unwrap_or_else(|| panic!("case {case:?}: expected status.allowed, got {answered:?}"))
}

/// The actions every row is cross-checked over: reads and writes, in the core group
/// and in `apps`.
const PROBES: [(&str, &str, &str); 6] = [
    ("list", "", "pods"),
    ("get", "", "configmaps"),
    ("create", "", "configmaps"),
    ("list", "apps", "deployments"),
    ("create", "apps", "deployments"),
    ("deletecollection", "", "secrets"),
];

/// Assert the listing for `namespace` AGREES with the decision the same server takes
/// on each probe: a reported rule is always allowed, and a COMPLETE listing reports
/// every allowed action.
async fn assert_agrees(
    config: &Kubeconfig,
    case: &str,
    namespace: &str,
    status: &serde_json::Value,
) {
    let rules = resource_rules(case, status);
    let complete = status["incomplete"] == serde_json::json!(false);
    for (verb, group, resource) in PROBES {
        let probe = format!("{case}: {verb} {resource:?} in group {group:?} in {namespace}");
        let reported = rules_admit(&rules, verb, group, resource);
        let allowed = access_review_allows(config, &probe, namespace, verb, group, resource).await;
        if reported {
            assert!(
                allowed,
                "case {probe:?}: a reported rule must be one the server allows, got \
                 allowed={allowed} over {rules:?}"
            );
        }
        if complete {
            assert_eq!(
                reported, allowed,
                "case {probe:?}: a complete listing reports every allowed action, got \
                 reported={reported} allowed={allowed} over {rules:?}"
            );
        }
    }
}

/// RULE: the group serves the resource — its resource list names it — and a review
/// that names no namespace is refused as a bad request.
#[tokio::test]
async fn the_authorization_group_serves_the_rules_review() {
    let router = emulator();
    let config = provisioned_cluster(&router, "rules-discovery", API_MODE, true).await;

    let resources = dial(
        &config,
        "the group's resources",
        "GET",
        "/apis/authorization.k8s.io/v1",
        None,
    )
    .await;
    assert_eq!(
        resources.status, 200,
        "the group's resource list must be served, got {resources:?}"
    );
    let row = resources.body["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("expected resources, got {resources:?}"))
        .iter()
        .find(|row| row["name"] == serde_json::json!("selfsubjectrulesreviews"))
        .cloned()
        .unwrap_or_else(|| panic!("expected the rules review resource, got {resources:?}"));
    assert_eq!(
        row["kind"],
        serde_json::json!("SelfSubjectRulesReview"),
        "the resource carries its kind, got {row:?}"
    );
    assert_eq!(
        row["namespaced"],
        serde_json::json!(false),
        "the namespace is a member of the review's spec, not of its path, got {row:?}"
    );
    assert_eq!(
        row["verbs"],
        serde_json::json!(["create"]),
        "a review is created, got {row:?}"
    );

    for (case, spec) in [
        ("a review naming no namespace", serde_json::json!({})),
        (
            "a review naming the empty namespace",
            serde_json::json!({ "namespace": "" }),
        ),
    ] {
        let refused = dial(
            &config,
            case,
            "POST",
            RULES_REVIEWS_PATH,
            Some(&serde_json::json!({
                "apiVersion": "authorization.k8s.io/v1",
                "kind": "SelfSubjectRulesReview",
                "spec": spec,
            })),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected the bad-request Status, got {refused:?}"
        );
        assert_eq!(
            refused.body["message"],
            serde_json::json!("no namespace on the request"),
            "case {case:?}: the refusal names what was missing, got {refused:?}"
        );
    }
}

/// RULE: on a cluster whose access entries the access-entry authorizer serves, the
/// listing is INCOMPLETE and carries none of that authorizer's rules — the
/// permissions an associated access policy grants are decided one request at a time
/// — while the rules Kubernetes itself holds for the identity are listed, and
/// everything listed is allowed.
#[tokio::test]
async fn a_cluster_served_by_the_access_entry_authorizer_reports_an_incomplete_listing() {
    let router = emulator();
    let config = provisioned_cluster(&router, "rules-incomplete", API_MODE, true).await;

    for namespace in [SCOPED_NAMESPACE, OTHER_NAMESPACE] {
        let case = format!("the cluster creator in {namespace}");
        let status = reviewed_rules(&config, &case, namespace, true).await;
        let rules = resource_rules(&case, &status);
        // The cluster-admin policy the creator's entry carries is that authorizer's,
        // so its wildcard rule is not in the listing even though the server allows
        // every action.
        assert!(
            !rules.iter().any(|rule| rule["resources"] == serde_json::json!(["*"])),
            "case {case:?}: the access-entry authorizer's rules cannot be enumerated, got \
             {rules:?}"
        );
        assert!(
            access_review_allows(&config, &case, namespace, "create", "", "configmaps").await,
            "case {case:?}: the creator's policy still admits every action"
        );
        // What Kubernetes itself holds for every authenticated client IS listed: the
        // reviews it may create, and the server's own paths.
        assert!(
            rules_admit(&rules, "create", "authorization.k8s.io", "selfsubjectrulesreviews"),
            "case {case:?}: every authenticated client may ask this very question, got {rules:?}"
        );
        let non_resource = status["nonResourceRules"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a review reports nonResourceRules, got {status:?}")
            })
            .clone();
        assert!(
            non_resource
                .iter()
                .any(|rule| names(rule["nonResourceURLs"].as_array(), "/version")),
            "case {case:?}: an authenticated client may read the server's own paths, got \
             {non_resource:?}"
        );
        assert_agrees(&config, &case, namespace, &status).await;
    }

    // The same on an identity whose only permissions are an access policy scoped to
    // one namespace: the policy admits the read where it applies, and neither
    // listing carries it.
    let cluster = "rules-scoped-policy";
    let scoped = provisioned_cluster(&router, cluster, API_MODE, false).await;
    let principal_arn = service_common::caller_principal::ambient_user_arn();
    grant_scoped_policy(
        &router,
        cluster,
        &principal_arn,
        VIEW_POLICY,
        &[SCOPED_NAMESPACE],
    )
    .await;
    for (namespace, reads) in [(SCOPED_NAMESPACE, true), (OTHER_NAMESPACE, false)] {
        let case = format!("a reader confined to {SCOPED_NAMESPACE}, asking about {namespace}");
        let status = reviewed_rules(&scoped, &case, namespace, true).await;
        let rules = resource_rules(&case, &status);
        assert!(
            !rules_admit(&rules, "list", "", "pods"),
            "case {case:?}: the policy's rules are the other authorizer's, got {rules:?}"
        );
        assert_eq!(
            access_review_allows(&scoped, &case, namespace, "list", "", "pods").await,
            reads,
            "case {case:?}: the policy admits the read only where it applies, expected {reads}"
        );
        assert_agrees(&scoped, &case, namespace, &status).await;
    }
}

/// RULE: on a cluster that authenticates through its `aws-auth` ConfigMap — no
/// access-entry authorizer in its chain — the listing is COMPLETE: every rule that
/// decides a request is enumerated, so the listing and the server's decision agree
/// in both directions.
#[tokio::test]
async fn a_cluster_without_that_authorizer_reports_a_complete_listing() {
    let router = emulator();
    let config = provisioned_cluster(&router, "rules-complete", CONFIG_MAP_MODE, true).await;

    for namespace in [SCOPED_NAMESPACE, OTHER_NAMESPACE] {
        let case = format!("a cluster on {CONFIG_MAP_MODE}, asked about {namespace}");
        let status = reviewed_rules(&config, &case, namespace, false).await;
        let rules = resource_rules(&case, &status);
        assert!(
            rules.iter().any(|rule| {
                rule["verbs"] == serde_json::json!(["*"])
                    && rule["apiGroups"] == serde_json::json!(["*"])
                    && rule["resources"] == serde_json::json!(["*"])
            }),
            "case {case:?}: a complete listing carries the rule that admits everything, got \
             {rules:?}"
        );
        assert_agrees(&config, &case, namespace, &status).await;
    }
}

/// RULE: the cluster's OWN bindings are a source of rules the listing always
/// carries — a `Role` the client created and bound to a group it is in is listed in
/// that namespace and in no other, whichever authorizer serves the cluster's access
/// entries.
#[tokio::test]
async fn a_role_the_client_created_is_listed_in_its_namespace() {
    let router = emulator();
    let config = provisioned_cluster(&router, "rules-bound-role", API_MODE, true).await;
    let rule = serde_json::json!({
        "apiGroups": [""],
        "resources": ["resourcequotas"],
        "verbs": ["deletecollection"],
    });

    let case = "the Role the client created";
    let role = dial(
        &config,
        case,
        "POST",
        &format!("/apis/rbac.authorization.k8s.io/v1/namespaces/{SCOPED_NAMESPACE}/roles"),
        Some(&serde_json::json!({
            "metadata": { "name": "reviewed-role" },
            "rules": [rule],
        })),
    )
    .await;
    assert_eq!(
        role.status, 201,
        "case {case:?}: the Role must be created, got {role:?}"
    );
    let binding = dial(
        &config,
        case,
        "POST",
        &format!("/apis/rbac.authorization.k8s.io/v1/namespaces/{SCOPED_NAMESPACE}/rolebindings"),
        Some(&serde_json::json!({
            "metadata": { "name": "reviewed-binding" },
            "roleRef": {
                "apiGroup": "rbac.authorization.k8s.io",
                "kind": "Role",
                "name": "reviewed-role",
            },
            // Every authenticated client is in this group, so the binding binds the
            // identity this suite dials as.
            "subjects": [{ "kind": "Group", "name": "system:authenticated" }],
        })),
    )
    .await;
    assert_eq!(
        binding.status, 201,
        "case {case:?}: the RoleBinding must be created, got {binding:?}"
    );

    for (namespace, bound) in [(SCOPED_NAMESPACE, true), (OTHER_NAMESPACE, false)] {
        let case = format!("the bound Role, asked about in {namespace}");
        let status = reviewed_rules(&config, &case, namespace, true).await;
        let rules = resource_rules(&case, &status);
        assert_eq!(
            rules.contains(&rule),
            bound,
            "case {case:?}: expected the Role's own rule to be listed={bound}, got {rules:?}"
        );
        assert_agrees(&config, &case, namespace, &status).await;
    }
}
