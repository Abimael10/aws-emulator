// SPDX-License-Identifier: CC0-1.0

//! The service-linked role a registration brings into the account.
//!
//! The rule: registering a scalable target in a namespace creates, if it is not there
//! already, the service-linked role Application Auto Scaling scales that namespace's
//! resources through — "Application Auto Scaling uses a service-linked role, which it creates
//! if it does not yet exist" — so the `RoleARN` a scalable target reports names a role a
//! client can read back in IAM, with the permissions policy that service attaches to it.
//!
//! What identifies the role is published per namespace, not per account: the name is
//! `AWSServiceRoleForApplicationAutoScaling_<resource type>`, it is filed under the path IAM
//! reserves for service-linked roles followed by the namespace's own service principal
//! (`<namespace>.application-autoscaling.amazonaws.com`), and the policy attached to it is
//! `AWSApplicationAutoscaling<resource type>Policy` in AWS's own managed-policy namespace.
//! The suites below walk a table of namespaces rather than the one the issue cited, and read
//! everything back over the wire through IAM's own operations: `GetRole` on the published
//! name, `ListAttachedRolePolicies` for the policy, and `ListRoles` under the path the role is
//! filed at — the three lookups a client makes on the `RoleARN` it read off a scalable
//! target.

use serde_json::json;

use super::emulator::{resource_id_for, service_linked_role_suffix, Emulator, ACCOUNT};

/// One namespace a registration is made in, and the dimension it is made on.
///
/// The rows span the namespaces whose resource type labels differ most from the namespace
/// itself (`ecs` -> `ECSService`, `dynamodb` -> `DynamoDBTable`, `ec2` ->
/// `EC2SpotFleetRequest`, `elasticache` -> `ElastiCacheRG`), including ones the issue did not
/// cite, so what is pinned is the published-name rule rather than one row of it.
const NAMESPACE_ROWS: &[(&str, &str)] = &[
    ("dynamodb", "dynamodb:table:ReadCapacityUnits"),
    ("dynamodb", "dynamodb:index:WriteCapacityUnits"),
    ("ecs", "ecs:service:DesiredCount"),
    ("ec2", "ec2:spot-fleet-request:TargetCapacity"),
    ("elasticache", "elasticache:replication-group:Replicas"),
    ("rds", "rds:cluster:ReadReplicaCount"),
];

#[tokio::test]
async fn registering_a_target_creates_the_namespace_s_service_linked_role() {
    for (namespace, dimension) in NAMESPACE_ROWS {
        let case = *dimension;
        let emulator = Emulator::empty();

        // Before the registration the account holds no such role: the role is something the
        // registration brings into existence, not something IAM always reports.
        let role_name = format!(
            "AWSServiceRoleForApplicationAutoScaling_{}",
            service_linked_role_suffix(namespace)
        );
        let (status, body) = emulator
            .call_iam("ListAttachedRolePolicies", &[("RoleName", &role_name)])
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected IAM to hold no role under {role_name:?} before the \
             registration, got {status} {body}"
        );

        emulator.given_target(dimension, 1, 10).await;

        // The role is there now, and the ARN the scalable target reports is its ARN.
        let expected_arn = format!(
            "arn:aws:iam::{ACCOUNT}:role/aws-service-role/\
             {namespace}.application-autoscaling.amazonaws.com/{role_name}"
        );
        let (status, body) = emulator
            .call_iam("GetRole", &[("RoleName", &role_name)])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRole to answer 200 after the registration, got {status} \
             {body}"
        );
        assert!(
            body.contains(&expected_arn),
            "case {case:?}: expected the role to carry {expected_arn:?}, got {body}"
        );
        // Its trust policy names the namespace's own service principal: that is the service
        // the role is linked to, and the only one that can assume it.
        let principal = format!("{namespace}.application-autoscaling.amazonaws.com");
        assert!(
            body.contains(&principal.replace('/', "%2F")) || body.contains(&principal),
            "case {case:?}: expected the trust policy to name {principal:?}, got {body}"
        );

        let (status, described) = emulator
            .call(
                "DescribeScalableTargets",
                json!({"ServiceNamespace": namespace,
                       "ResourceIds": [resource_id_for(dimension)],
                       "ScalableDimension": dimension}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeScalableTargets to answer 200, got {described:?}"
        );
        assert_eq!(
            described["ScalableTargets"][0]["RoleARN"],
            json!(expected_arn),
            "case {case:?}: expected the target to report the role it is scaled through, got \
             {described:?}"
        );

        // And the role carries the permissions policy the service attaches to its own role,
        // which is what a client inspects to see what auto scaling may do.
        let expected_policy = format!(
            "AWSApplicationAutoscaling{}Policy",
            service_linked_role_suffix(namespace)
        );
        let (status, body) = emulator
            .call_iam("ListAttachedRolePolicies", &[("RoleName", &role_name)])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListAttachedRolePolicies to answer 200, got {status} {body}"
        );
        assert!(
            body.contains(&expected_policy),
            "case {case:?}: expected the attached policy {expected_policy:?}, got {body}"
        );
        assert!(
            body.contains(&format!(
                "arn:aws:iam::aws:policy/aws-service-role/{expected_policy}"
            )),
            "case {case:?}: expected the policy's ARN in AWS's own namespace, got {body}"
        );
    }
}

#[tokio::test]
async fn a_second_registration_leaves_the_role_it_already_created_alone() {
    const NAMESPACE: &str = "dynamodb";
    const DIMENSION: &str = "dynamodb:table:ReadCapacityUnits";

    let emulator = Emulator::empty();
    let role_name = format!(
        "AWSServiceRoleForApplicationAutoScaling_{}",
        service_linked_role_suffix(NAMESPACE)
    );
    let target = emulator.given_target(DIMENSION, 1, 10).await;
    let (status, first) = emulator
        .call_iam("GetRole", &[("RoleName", &role_name)])
        .await;
    assert_eq!(
        status, 200,
        "case \"the first registration\": expected GetRole to answer 200, got {status} {first}"
    );

    // A second registration of the same target, and one of ANOTHER target in the same
    // namespace: the role is per namespace, so neither creates a second one and neither
    // replaces the one that is there (its id and creation date are what a client caches).
    let mut again = target.clone();
    again["MinCapacity"] = json!(2);
    let (status, body) = emulator.call("RegisterScalableTarget", again).await;
    assert_eq!(
        status, 200,
        "case \"the same target again\": expected 200, got {status} {body:?}"
    );
    let other = emulator
        .given_target("dynamodb:table:WriteCapacityUnits", 1, 10)
        .await;
    assert_eq!(
        other["ServiceNamespace"],
        json!(NAMESPACE),
        "case \"another target of the namespace\": expected the same namespace, got {other:?}"
    );

    let (status, again) = emulator
        .call_iam("GetRole", &[("RoleName", &role_name)])
        .await;
    assert_eq!(
        status, 200,
        "case \"after both registrations\": expected GetRole to answer 200, got {status} {again}"
    );
    for member in ["RoleId", "CreateDate"] {
        let before = element(&first, member);
        let after = element(&again, member);
        assert!(
            !before.is_empty(),
            "case \"after both registrations\": expected the role to report a {member}, got \
             {first}"
        );
        assert_eq!(
            before, after,
            "case \"after both registrations\": expected the same {member}, so the role was \
             neither re-created nor replaced, got {again}"
        );
    }
}

/// One namespace a registration is made in, and the dimension it is made on, for the suites
/// that read the minted role back through IAM's own `ListRoles`.
///
/// The rows differ along how the namespace's resource is resolved — `lambda` and `cassandra`
/// are reached by calling the service that owns the resource, `dynamodb` reads that service's
/// store, and `sagemaker` and `workspaces` resolve against a service this emulator does not
/// hold — and along the form the resource id takes, so what the rows pin is the per-namespace
/// minting rule rather than one namespace of it. `lambda` is the row the issue cited.
const LISTED_NAMESPACE_ROWS: &[(&str, &str)] = &[
    ("lambda", "lambda:function:ProvisionedConcurrency"),
    ("dynamodb", "dynamodb:table:ReadCapacityUnits"),
    ("cassandra", "cassandra:table:WriteCapacityUnits"),
    ("sagemaker", "sagemaker:inference-component:DesiredCopyCount"),
    ("workspaces", "workspaces:workspacespool:DesiredUserSessions"),
];

/// The path IAM files a service-linked role of `namespace` under: the prefix it reserves for
/// such roles followed by the service principal the role is linked to.
fn service_linked_role_path(namespace: &str) -> String {
    format!("/aws-service-role/{namespace}.application-autoscaling.amazonaws.com/")
}

/// The published name of the service-linked role `namespace`'s targets are scaled through.
fn role_name_of(namespace: &str) -> String {
    format!(
        "AWSServiceRoleForApplicationAutoScaling_{}",
        service_linked_role_suffix(namespace)
    )
}

/// RULE: the role a registration brings into the account is a role IAM HOLDS, so it is among
/// the roles `ListRoles` reports under the path that namespace's service-linked role is filed
/// at — which is where a client that read the target's `RoleARN` looks that role up, and where
/// nothing was reported before the registration made it exist.
#[tokio::test]
async fn list_roles_under_a_namespaces_service_linked_path_reports_the_minted_role() {
    for (namespace, dimension) in LISTED_NAMESPACE_ROWS {
        let case = *dimension;
        let emulator = Emulator::empty();
        let path = service_linked_role_path(namespace);
        let role_name = role_name_of(namespace);

        // Before the registration the account holds no role under that path at all: the role
        // is something the registration creates, not something IAM always lists.
        let (status, body) = emulator
            .call_iam("ListRoles", &[("PathPrefix", &path)])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListRoles to answer 200, got {status} {body}"
        );
        let before = elements(&body, "RoleName");
        assert!(
            before.is_empty(),
            "case {case:?}: expected IAM to list no role under {path:?} before the \
             registration, got {before:?}"
        );

        emulator.given_target(dimension, 1, 10).await;

        // Now the namespace's own role is listed there, filed at that path, with the ARN the
        // scalable target reports.
        let (status, body) = emulator
            .call_iam("ListRoles", &[("PathPrefix", &path)])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListRoles to answer 200, got {status} {body}"
        );
        let listed = elements(&body, "RoleName");
        assert_eq!(
            listed,
            vec![role_name.clone()],
            "case {case:?}: expected ListRoles under {path:?} to report the namespace's own \
             role {role_name:?}, got {listed:?}"
        );
        let paths = elements(&body, "Path");
        assert_eq!(
            paths,
            vec![path.clone()],
            "case {case:?}: expected the listed role to be filed under {path:?}, got {paths:?}"
        );
        let expected_arn = format!("arn:aws:iam::{ACCOUNT}:role{path}{role_name}");
        let arns = elements(&body, "Arn");
        assert_eq!(
            arns,
            vec![expected_arn.clone()],
            "case {case:?}: expected the listed role to carry {expected_arn:?}, got {arns:?}"
        );

        // And that ARN is the one the registered target claims to be scaled through, so the
        // string a client reads off the target resolves to the role IAM just listed.
        let (status, described) = emulator
            .call(
                "DescribeScalableTargets",
                json!({"ServiceNamespace": namespace,
                       "ResourceIds": [resource_id_for(dimension)],
                       "ScalableDimension": dimension}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeScalableTargets to answer 200, got {described:?}"
        );
        assert_eq!(
            described["ScalableTargets"][0]["RoleARN"],
            json!(expected_arn),
            "case {case:?}: expected the target to report the role IAM holds, got {described:?}"
        );
    }
}

/// RULE: the role is one PER NAMESPACE, so a registration in one namespace creates that
/// namespace's role and no other — no other namespace's service-linked path answers for it,
/// and the account holds exactly the one role the registration brought in.
#[tokio::test]
async fn a_registration_creates_no_other_namespaces_service_linked_role() {
    // Rows: the namespaces whose role must NOT be there — the `dynamodb` twin the adjacency
    // note names, and three more whose resource is resolved differently again.
    const OTHER_NAMESPACES: &[&str] = &["dynamodb", "ecs", "cassandra", "workspaces"];
    const REGISTERED: &str = "lambda:function:ProvisionedConcurrency";

    let emulator = Emulator::empty();
    emulator.given_target(REGISTERED, 2, 10).await;
    let minted = role_name_of("lambda");

    for namespace in OTHER_NAMESPACES {
        let case = *namespace;
        let path = service_linked_role_path(namespace);
        let (status, body) = emulator
            .call_iam("ListRoles", &[("PathPrefix", &path)])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListRoles to answer 200, got {status} {body}"
        );
        let listed = elements(&body, "RoleName");
        assert!(
            listed.is_empty(),
            "case {case:?}: expected no role under {path:?}, since nothing was registered in \
             that namespace, got {listed:?}"
        );
    }

    // Listing the whole account finds that one role and nothing else: a registration creates
    // one role, not a role per namespace the service knows of.
    let (status, body) = emulator.call_iam("ListRoles", &[]).await;
    assert_eq!(
        status, 200,
        "case {REGISTERED:?}: expected ListRoles to answer 200, got {status} {body}"
    );
    let listed = elements(&body, "RoleName");
    assert_eq!(
        listed,
        vec![minted.clone()],
        "case {REGISTERED:?}: expected the account to hold exactly {minted:?}, got {listed:?}"
    );
}

/// The text of every `<name>` element of an awsQuery XML response, in document order.
fn elements(body: &str, name: &str) -> Vec<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    body.split(open.as_str())
        .skip(1)
        .filter_map(|rest| {
            rest.split_once(close.as_str())
                .map(|(value, _)| value.to_string())
        })
        .collect()
}

/// The text of the first `<name>` element of an awsQuery XML response.
fn element(body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    body.split_once(&open)
        .and_then(|(_, rest)| rest.split_once(&close))
        .map(|(value, _)| value.to_string())
        .unwrap_or_default()
}
