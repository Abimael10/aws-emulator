// SPDX-License-Identifier: CC0-1.0

//! Which RESOURCE a simulated action can be performed on, over the awsQuery wire.
//!
//! The rule under test: an action published with no resource type is performed on no
//! resource, so a simulation of it against ONE named resource ARN reports `implicitDeny`
//! however widely the documents in force are written, while the same action asked about
//! the wildcard resource — the resource element a policy has to use to permit it — is
//! decided from those documents. An action published WITH a resource type is decided from
//! them against a named resource too, so the two kinds of action asked about the same ARN
//! must differ. This is what tells a least-privilege review that a token-minting grant has
//! to stay scoped to `*`.
//!
//! Both simulation operations answer under it: `SimulatePrincipalPolicy`, whose documents
//! come from the principal the request names, and `SimulateCustomPolicy`, whose documents
//! the request supplies — one evaluation, so one rule.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: whether
//! the action is published with a resource type, the service it belongs to, the ARN form of
//! the resource it is asked about (including one of another service), the casing the action
//! is written in, and how many resources one request names. The actions come from
//! `src/state/action_resource_types.rs`, whose catalog records the Service Authorization
//! Reference's rows that list no resource type.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the
//! `SimulatePrincipalPolicy` request builder the one `tests/all/policy_simulation.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::{principal, simulate, ALLOWED, IMPLICIT_DENY};
use crate::user_lifecycle::{call, element, elements, router};

/// The resource a request that names none is decided about, and the one a policy must use
/// to permit an action that takes no resource: "if this parameter is not provided, then the
/// value defaults to * (all resources)" (`ResourceArns`).
const ANY_RESOURCE: &str = "*";

/// Resources the rows are about. "You can simulate resources that don't exist in your
/// account" (`SimulatePrincipalPolicy`), so none of these is created anywhere; the five
/// services and their five ARN forms differ so nothing under test can be keyed to one of
/// them.
const REPOSITORY_ARN: &str = "arn:aws:ecr:us-west-2:000000000000:repository/payments-api";
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/deploy";
const BUCKET_ARN: &str = "arn:aws:s3:::quarterly-reports";
const OBJECT_ARN: &str = "arn:aws:s3:::quarterly-reports/2026/q1.csv";
const KEY_ARN: &str = "arn:aws:kms:eu-west-1:000000000000:key/3f6c7a4e-3333-4444-8888-cccccccccccc";

/// The one grant every row is decided under: each action the rows ask about, permitted on
/// every resource. Written on `*` because that is the only resource element a policy can
/// permit the resourceless half of the list under — so every row starts from a document
/// that DOES grant its action, and the only thing that differs is whether the action can be
/// performed on the resource the request names.
const ALLOW_EVERY_ASKED_ACTION: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ecr:GetAuthorizationToken","ecr:BatchGetImage","sts:GetCallerIdentity","sts:AssumeRole","s3:ListAllMyBuckets","s3:GetObject","kms:ListKeys","kms:Decrypt","iam:ListRoles","iam:GetRole"],"Resource":"*"}]}"#;

/// The name the grant is embedded under.
const GRANT_NAME: &str = "allow-the-asked-actions";

/// One row: an action, the resource one request asks it about, and the decision that pair
/// must be reported with.
struct Case {
    case: &'static str,
    action: &'static str,
    resource: &'static str,
    expected: &'static str,
}

/// Rows pairing each action with a resource, spanning both kinds of action on the same ARN
/// of four services, the wildcard resource, an ARN of another service than the action's,
/// and a catalogued action written in another casing.
const CASES: &[Case] = &[
    Case {
        case: "a registry token mint against a repository",
        action: "ecr:GetAuthorizationToken",
        resource: REPOSITORY_ARN,
        expected: IMPLICIT_DENY,
    },
    Case {
        case: "the same mint against the wildcard resource",
        action: "ecr:GetAuthorizationToken",
        resource: ANY_RESOURCE,
        expected: ALLOWED,
    },
    Case {
        case: "a repository read against that same repository",
        action: "ecr:BatchGetImage",
        resource: REPOSITORY_ARN,
        expected: ALLOWED,
    },
    Case {
        case: "the mint against a resource of another service",
        action: "ecr:GetAuthorizationToken",
        resource: BUCKET_ARN,
        expected: IMPLICIT_DENY,
    },
    Case {
        case: "the mint written in another casing",
        action: "ECR:getauthorizationtoken",
        resource: REPOSITORY_ARN,
        expected: IMPLICIT_DENY,
    },
    Case {
        case: "the same casing against the wildcard resource",
        action: "ECR:getauthorizationtoken",
        resource: ANY_RESOURCE,
        expected: ALLOWED,
    },
    Case {
        case: "an identity read against a role",
        action: "sts:GetCallerIdentity",
        resource: ROLE_ARN,
        expected: IMPLICIT_DENY,
    },
    Case {
        case: "an assume of that same role",
        action: "sts:AssumeRole",
        resource: ROLE_ARN,
        expected: ALLOWED,
    },
    Case {
        case: "an account-wide bucket listing against a bucket",
        action: "s3:ListAllMyBuckets",
        resource: BUCKET_ARN,
        expected: IMPLICIT_DENY,
    },
    Case {
        case: "an object read against an object",
        action: "s3:GetObject",
        resource: OBJECT_ARN,
        expected: ALLOWED,
    },
    Case {
        case: "a key listing against a key",
        action: "kms:ListKeys",
        resource: KEY_ARN,
        expected: IMPLICIT_DENY,
    },
    Case {
        case: "a use of that same key",
        action: "kms:Decrypt",
        resource: KEY_ARN,
        expected: ALLOWED,
    },
    Case {
        case: "an account-wide role listing against a role",
        action: "iam:ListRoles",
        resource: ROLE_ARN,
        expected: IMPLICIT_DENY,
    },
    Case {
        case: "a read of that same role",
        action: "iam:GetRole",
        resource: ROLE_ARN,
        expected: ALLOWED,
    },
];

/// RULE: a simulated `(action, resource)` pair is decided from the documents in force only
/// where the action can be performed on that resource at all; an action published with no
/// resource type reports `implicitDeny` against a named resource, and names no policy as
/// having determined it.
#[tokio::test]
async fn an_action_with_no_resource_type_is_decided_only_on_the_wildcard() {
    let router = router();
    let arn = principal(
        &router,
        "the asked actions",
        "scoped",
        &[(GRANT_NAME, ALLOW_EVERY_ASKED_ACTION)],
    )
    .await;

    for case in CASES {
        let (status, body) =
            simulate(&router, &arn, &[case.action], &[case.resource], &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        let decision = element(&body, "EvalDecision");
        assert_eq!(
            decision, case.expected,
            "case {:?}: expected {} for {} on {}, got {decision:?}: {body}",
            case.case, case.expected, case.action, case.resource
        );
        assert_eq!(
            element(&body, "EvalResourceDecision"),
            case.expected,
            "case {:?}: the resource's own decision must agree with the action's: {body}",
            case.case
        );
        // "Only statements that contributed to the reported decision are included"
        // (`EvaluationResult$MatchedStatements`): nothing contributes to a pair the action
        // cannot be performed as, while the pairs that ARE decided name the policy that
        // granted them.
        let attributed = elements(&body, "SourcePolicyId");
        if case.expected == IMPLICIT_DENY {
            assert!(
                attributed.is_empty(),
                "case {:?}: expected no policy to be named for a pair nothing permitted, \
                 got {attributed:?}: {body}",
                case.case
            );
        } else {
            assert!(
                attributed.iter().any(|named| named.contains(GRANT_NAME)),
                "case {:?}: expected the granting policy {GRANT_NAME:?} to be named, got \
                 {attributed:?}: {body}",
                case.case
            );
        }
    }
}

/// RULE: the pairs of ONE request are decided independently, so a request naming several
/// resources reports the wildcard-only action's decision per resource, while the action's
/// own decision is the most restrictive of the resources the action can be performed on at
/// all — a resource it cannot be performed on is a request that action could not have been
/// made as, reported as such in its own entry and not part of what the action's own result
/// says about the action's resource type.
#[tokio::test]
async fn each_named_resource_of_one_request_is_decided_on_its_own() {
    let router = router();
    let arn = principal(
        &router,
        "several resources",
        "spread",
        &[(GRANT_NAME, ALLOW_EVERY_ASKED_ACTION)],
    )
    .await;

    let (status, body) = simulate(
        &router,
        &arn,
        &["ecr:GetAuthorizationToken", "ecr:BatchGetImage"],
        &[REPOSITORY_ARN, ANY_RESOURCE],
        &[],
    )
    .await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    // Two actions over two resources, in request order: the mint is denied on the
    // repository and allowed on the wildcard, while the repository read is allowed on
    // both.
    assert_eq!(
        elements(&body, "EvalResourceDecision"),
        vec![
            IMPLICIT_DENY.to_string(),
            ALLOWED.to_string(),
            ALLOWED.to_string(),
            ALLOWED.to_string(),
        ],
        "expected one decision per (action, resource) pair, in request order: {body}"
    );
    // The mint cannot be performed on the repository at all, so that pair is not one of the
    // resources its own result is aggregated from: what is left is the wildcard, which the
    // grant covers.
    assert_eq!(
        crate::policy_simulation::reported(&body),
        vec![
            ("ecr:GetAuthorizationToken".to_string(), ALLOWED.to_string()),
            ("ecr:BatchGetImage".to_string(), ALLOWED.to_string()),
        ],
        "expected each action's own decision to be the most restrictive of the resources it \
         can be performed on: {body}"
    );
}

/// One `SimulateCustomPolicy` call over the documents the request itself supplies, with the
/// list members the query protocol flattens into `<member>.member.N` parameters.
async fn simulate_custom(
    router: &MultiServiceRouter,
    documents: &[&str],
    actions: &[&str],
    resources: &[&str],
) -> (u16, String) {
    let mut owned: Vec<(String, String)> = Vec::new();
    for (position, document) in documents.iter().enumerate() {
        owned.push((
            format!("PolicyInputList.member.{}", position + 1),
            (*document).to_string(),
        ));
    }
    for (position, action) in actions.iter().enumerate() {
        owned.push((
            format!("ActionNames.member.{}", position + 1),
            (*action).to_string(),
        ));
    }
    for (position, resource) in resources.iter().enumerate() {
        owned.push((
            format!("ResourceArns.member.{}", position + 1),
            (*resource).to_string(),
        ));
    }
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    call(router, "SimulateCustomPolicy", &params).await
}

/// RULE: the same rule decides a simulation whose documents the REQUEST supplies — the two
/// operations run one evaluation, so neither kind of action can be answered differently by
/// the one that takes no principal.
#[tokio::test]
async fn a_custom_policy_simulation_follows_the_same_rule() {
    let router = router();

    for case in CASES {
        let (status, body) = simulate_custom(
            &router,
            &[ALLOW_EVERY_ASKED_ACTION],
            &[case.action],
            &[case.resource],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        let decision = element(&body, "EvalDecision");
        assert_eq!(
            decision, case.expected,
            "case {:?}: expected {} for {} on {}, got {decision:?}: {body}",
            case.case, case.expected, case.action, case.resource
        );
    }
}
