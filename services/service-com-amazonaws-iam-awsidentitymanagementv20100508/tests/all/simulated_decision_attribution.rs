// SPDX-License-Identifier: CC0-1.0

//! Why `SimulatePrincipalPolicy` decided as it did: the policies each result names as
//! having determined its decision, and how the action's own result aggregates the
//! resources the request asked about.
//!
//! The two rules under test:
//!
//! 1. every result EXPLAINS its decision. `MatchedStatements` is "a list of the
//!    statements in the input policies that determine the result for this scenario …
//!    only statements that contributed to the reported decision are included"
//!    (`EvaluationResult$MatchedStatements`), and each entry names the policy
//!    (`SourcePolicyId`) and of which kind it is (`SourcePolicyType`) — on the action's
//!    own result and on each `ResourceSpecificResults` entry alike. So an allow reached
//!    through the simulated principal's OWN identity-based policies is told apart from
//!    one reached through the resource-based policy the request supplied, WHEREVER the
//!    deciding identity document comes from: embedded in the principal, embedded in a
//!    group it belongs to, the default version of a managed policy attached to it, or
//!    supplied in `PolicyInputList`.
//! 2. the action's own result AGGREGATES the resources, it is not the first of them:
//!    "the top-level `EvalDecision` reflects the most restrictive decision across all
//!    resources (for example, if any resource produces `explicitDeny`, the top-level
//!    decision is `explicitDeny`)" (`EvaluationResult`), with the permissions-boundary
//!    detail beside it aggregated the same way — while each resource keeps its own
//!    decision in `ResourceSpecificResults`.
//!
//! Three more rules, about WHAT each part of that report is and which detail it carries,
//! are stated where they are tested further down: the resource the action's own result is
//! about, the resources an action is decided on at all, and the two details a cross-account
//! simulation reports.
//!
//! `tests/all/simulated_resource_policy.rs` is the sibling suite that spans the
//! principal element's forms with a user's inline policy as the only identity source;
//! the rows here differ along the axes it holds fixed — WHERE the deciding identity
//! document comes from, the service the action belongs to, and how many resources one
//! request asks about — and read a response through that suite's own attribution
//! readers, so both suites read one the same way.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the
//! simulation request spelling the one `tests/all/policy_simulation.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::{simulate, ALLOWED, EXPLICIT_DENY, IMPLICIT_DENY};
use crate::simulated_resource_policy::{
    attribution, inline_policy_id, parts, user_inline_policy_id, IDENTITY_POLICY, RESOURCE_POLICY,
    RESOURCE_POLICY_ID,
};
use crate::user_lifecycle::{account_id, call, element, elements, router};

/// How a `MatchedStatements` entry identifies the FIRST document a request supplied in
/// `PolicyInputList`: that member and the document's position in it, counted from one.
const SUPPLIED_POLICY_ID: &str = "PolicyInputList.1";

/// Resources the simulation is about. "You can simulate resources that don't exist in
/// your account" (`SimulatePrincipalPolicy`), so none of these is created anywhere; the
/// four services and their four ARN forms differ so nothing under test can be keyed to
/// one of them.
const OBJECT_ARN: &str = "arn:aws:s3:::quarterly-reports/2026/q1.csv";
const QUEUE_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:orders";
const KEY_ARN: &str = "arn:aws:kms:eu-west-1:000000000000:key/3f6c7a4e-3333-4444-8888-cccccccccccc";
const TABLE_ARN: &str = "arn:aws:dynamodb:ap-southeast-2:000000000000:table/ledger";

/// Where the document that grants the simulated request comes from.
#[derive(Clone, Copy, Debug)]
enum Grant {
    /// An inline policy embedded in the simulated user (`PutUserPolicy`).
    UserInline,
    /// The default version of a managed policy attached to it (`CreatePolicy` +
    /// `AttachUserPolicy`).
    UserManaged,
    /// An inline policy embedded in a GROUP the user belongs to: "if you specify a user,
    /// then the simulation also includes all of the policies that are attached to groups
    /// that the user belongs to" (`SimulatePrincipalPolicy`).
    GroupInline,
    /// A document the REQUEST supplied in `PolicyInputList`.
    Supplied,
    /// The resource-based policy the request supplied in `ResourcePolicy`, naming the
    /// simulated principal.
    ResourcePolicy,
}

/// One row of the attribution table.
struct Case {
    case: &'static str,
    grant: Grant,
    /// The name the granting policy is stored under, for a grant the account holds: an
    /// inline policy's own name (which the response identifies together with the entity
    /// that embeds it), or a managed policy's friendly name. Unused by a grant the request
    /// itself supplies, which the response identifies by the member it arrived in.
    policy_name: &'static str,
    action: &'static str,
    resource: &'static str,
}

/// Rows spanning every place a document that grants one request can come from, each on a
/// different service's action and ARN form, with the policy names spanning the
/// `^[\w+=,.@-]+$` alphabet and both name cases.
const CASES: &[Case] = &[
    Case {
        case: "the principal's own inline policy",
        grant: Grant::UserInline,
        policy_name: "read-objects",
        action: "s3:GetObject",
        resource: OBJECT_ARN,
    },
    Case {
        case: "a managed policy attached to the principal",
        grant: Grant::UserManaged,
        policy_name: "SendToOrders",
        action: "sqs:SendMessage",
        resource: QUEUE_ARN,
    },
    Case {
        case: "an inline policy of a group the principal belongs to",
        grant: Grant::GroupInline,
        policy_name: "encrypt.with+key",
        action: "kms:Encrypt",
        resource: KEY_ARN,
    },
    Case {
        case: "a document the request supplied",
        grant: Grant::Supplied,
        policy_name: "",
        action: "dynamodb:GetItem",
        resource: TABLE_ARN,
    },
    Case {
        case: "the resource-based policy the request supplied",
        grant: Grant::ResourcePolicy,
        policy_name: "",
        action: "s3:GetObject",
        resource: OBJECT_ARN,
    },
];

/// RULE 1 over every row: the decision is `allowed`, and both halves of the result name
/// the policy that granted it and of which kind that policy is.
#[tokio::test]
async fn every_grant_path_is_named_by_the_result_it_decided() {
    let router = router();

    for (row, case) in CASES.iter().enumerate() {
        let user_name = format!("explained{row}");
        let arn = create_user(&router, case.case, &user_name).await;
        let identity_document = allow(case.action, case.resource);

        let mut extra: Vec<(String, String)> = Vec::new();
        let expected_id = match case.grant {
            Grant::UserInline => {
                put_policy(
                    &router,
                    case.case,
                    "PutUserPolicy",
                    &[("UserName", &user_name)],
                    case.policy_name,
                    &identity_document,
                )
                .await;
                user_inline_policy_id(&user_name, case.policy_name)
            }
            Grant::UserManaged => {
                let policy_arn =
                    create_policy(&router, case.case, case.policy_name, &identity_document).await;
                expect_ok(
                    &router,
                    case.case,
                    "AttachUserPolicy",
                    &[("UserName", &user_name), ("PolicyArn", &policy_arn)],
                )
                .await;
                case.policy_name.to_string()
            }
            Grant::GroupInline => {
                let group_name = format!("explainers{row}");
                expect_ok(
                    &router,
                    case.case,
                    "CreateGroup",
                    &[("GroupName", &group_name)],
                )
                .await;
                put_policy(
                    &router,
                    case.case,
                    "PutGroupPolicy",
                    &[("GroupName", &group_name)],
                    case.policy_name,
                    &identity_document,
                )
                .await;
                expect_ok(
                    &router,
                    case.case,
                    "AddUserToGroup",
                    &[("GroupName", &group_name), ("UserName", &user_name)],
                )
                .await;
                inline_policy_id("group", &group_name, case.policy_name)
            }
            Grant::Supplied => {
                extra.push((
                    "PolicyInputList.member.1".to_string(),
                    identity_document.clone(),
                ));
                SUPPLIED_POLICY_ID.to_string()
            }
            Grant::ResourcePolicy => {
                extra.push((
                    "ResourcePolicy".to_string(),
                    allow_principal(&arn, case.action, case.resource),
                ));
                RESOURCE_POLICY_ID.to_string()
            }
        };
        // The kind each entry carries is the discriminator the member exists to give:
        // the resource-based policy the request supplied, or an identity-based policy in
        // force on the principal — which every other grant path above is.
        let expected_kind = match case.grant {
            Grant::ResourcePolicy => RESOURCE_POLICY,
            _ => IDENTITY_POLICY,
        };
        let expected = vec![(expected_id, expected_kind.to_string())];

        let (status, body) =
            simulate(&router, &arn, &[case.action], &[case.resource], &extra).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        assert_eq!(
            element(&body, "EvalDecision"),
            ALLOWED,
            "case {:?}: expected the grant to decide the action {ALLOWED}, got {body}",
            case.case
        );
        assert_eq!(
            element(&body, "EvalResourceDecision"),
            ALLOWED,
            "case {:?}: expected the grant to decide the resource {ALLOWED}, got {body}",
            case.case
        );
        let (top, per_resource) = parts(&body);
        assert_eq!(
            attribution(top),
            expected,
            "case {:?}: expected the action's result to name {expected:?}, got {body}",
            case.case
        );
        assert_eq!(
            attribution(per_resource),
            expected,
            "case {:?}: expected the resource's result to name {expected:?}, got {body}",
            case.case
        );
    }
}

/// The one policy NAME every entity below embeds its document under, so the name alone
/// cannot tell the three embeddings apart.
const SHARED_INLINE_NAME: &str = "read-objects";

/// A trust policy `CreateRole` accepts: the account's own principal, as the other role
/// suites in this crate create a fixture role with.
fn trust_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{}:root"}},"Action":"sts:AssumeRole"}}]}}"#,
        crate::user_lifecycle::account_id()
    )
}

/// RULE 1 for an INLINE policy, over every kind of entity one can be embedded in: the
/// attribution names the embedding — the entity's type, its name and the policy's own name
/// — so one policy name embedded in three different entities is told apart, and a decision
/// can be attributed to the one embedding that produced it.
///
/// The rows differ along the entity type (and therefore along which `Put*Policy` embeds the
/// document and which ARN the simulation addresses), the effect the document states, and
/// the decision that effect reaches.
#[tokio::test]
async fn an_inline_policy_is_attributed_to_the_entity_that_embeds_it() {
    let router = router();
    let resource = OBJECT_ARN;
    let action = "s3:GetObject";

    // (the entity type, its name, the effect its document states, the decision that effect
    //  reaches)
    let rows: &[(&str, &str, &str, &str)] = &[
        ("user", "embedder", "Allow", ALLOWED),
        ("group", "embedders", "Allow", ALLOWED),
        ("role", "embedding", "Allow", ALLOWED),
        ("user", "refuser", "Deny", EXPLICIT_DENY),
        ("role", "refusing", "Deny", EXPLICIT_DENY),
    ];

    for (entity_type, entity_name, effect, decision) in rows {
        let case = format!("a {entity_type}'s inline policy stating {effect}");
        // The noun the create and the put spell the entity with: `CreateUser` /
        // `PutUserPolicy`, and so on for the other two.
        let noun = match *entity_type {
            "user" => "User",
            "group" => "Group",
            _ => "Role",
        };
        let name_member = format!("{noun}Name");
        let mut params: Vec<(&str, &str)> = vec![(name_member.as_str(), entity_name)];
        let trust = trust_policy();
        if *entity_type == "role" {
            params.push(("AssumeRolePolicyDocument", trust.as_str()));
        }
        let (status, created) = call(&router, &format!("Create{noun}"), &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: Create{noun} must succeed, got {status}: {created}"
        );
        let arn = element(&created, "Arn");

        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"{resource}"}}]}}"#
        );
        put_policy(
            &router,
            &case,
            &format!("Put{noun}Policy"),
            &[(name_member.as_str(), entity_name)],
            SHARED_INLINE_NAME,
            &document,
        )
        .await;

        let (status, body) = simulate(&router, &arn, &[action], &[resource], &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {body}"
        );
        assert_eq!(
            element(&body, "EvalDecision"),
            *decision,
            "case {case:?}: expected the document to decide {decision}, got {body}"
        );
        let expected = vec![(
            inline_policy_id(entity_type, entity_name, SHARED_INLINE_NAME),
            IDENTITY_POLICY.to_string(),
        )];
        let (top, per_resource) = parts(&body);
        assert_eq!(
            attribution(top),
            expected,
            "case {case:?}: expected the action's result to name {expected:?}, got {body}"
        );
        assert_eq!(
            attribution(per_resource),
            expected,
            "case {case:?}: expected the resource's result to name {expected:?}, got {body}"
        );
    }
}


const AGGREGATE_POLICY_NAME: &str = "object-reads";
const AGGREGATE_USER: &str = "aggregated";

/// One `Allow` covering two buckets and one `Deny` naming the second of them, so the
/// three objects below reach all three decisions `PolicyEvaluationDecisionType` declares
/// from ONE document.
const AGGREGATE_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":["arn:aws:s3:::alpha/*","arn:aws:s3:::gamma/*"]},{"Effect":"Deny","Action":"s3:GetObject","Resource":"arn:aws:s3:::gamma/*"}]}"#;

/// The object the `Allow` covers, the one no statement is about, and the one the `Deny`
/// names.
const ALPHA_OBJECT: &str = "arn:aws:s3:::alpha/report.csv";
const BETA_OBJECT: &str = "arn:aws:s3:::beta/report.csv";
const GAMMA_OBJECT: &str = "arn:aws:s3:::gamma/report.csv";

/// Rows spanning how many resources one request asks about and which decisions they
/// reach, each in both orders — so the aggregate cannot be the first resource's.
const AGGREGATED: &[(&str, &[&str], &[&str])] = &[
    ("one allowed resource", &[ALPHA_OBJECT], &[ALLOWED]),
    ("one unstated resource", &[BETA_OBJECT], &[IMPLICIT_DENY]),
    ("one denied resource", &[GAMMA_OBJECT], &[EXPLICIT_DENY]),
    (
        "allowed then unstated",
        &[ALPHA_OBJECT, BETA_OBJECT],
        &[ALLOWED, IMPLICIT_DENY],
    ),
    (
        "unstated then allowed",
        &[BETA_OBJECT, ALPHA_OBJECT],
        &[IMPLICIT_DENY, ALLOWED],
    ),
    (
        "allowed then denied",
        &[ALPHA_OBJECT, GAMMA_OBJECT],
        &[ALLOWED, EXPLICIT_DENY],
    ),
    (
        "denied then allowed",
        &[GAMMA_OBJECT, ALPHA_OBJECT],
        &[EXPLICIT_DENY, ALLOWED],
    ),
    (
        "unstated then denied",
        &[BETA_OBJECT, GAMMA_OBJECT],
        &[IMPLICIT_DENY, EXPLICIT_DENY],
    ),
    (
        "all three, allowed first",
        &[ALPHA_OBJECT, BETA_OBJECT, GAMMA_OBJECT],
        &[ALLOWED, IMPLICIT_DENY, EXPLICIT_DENY],
    ),
];

/// RULE 2: the action's own result reports the MOST RESTRICTIVE of its resources'
/// decisions, each resource keeps its own, and the statements the action's result names
/// are the union across those resources.
#[tokio::test]
async fn the_actions_result_reports_the_most_restrictive_resource_decision() {
    let router = router();
    let arn = create_user(&router, "aggregate", AGGREGATE_USER).await;
    put_policy(
        &router,
        "aggregate",
        "PutUserPolicy",
        &[("UserName", AGGREGATE_USER)],
        AGGREGATE_POLICY_NAME,
        AGGREGATE_POLICY,
    )
    .await;

    for (case, resources, per_resource) in AGGREGATED {
        let (status, body) = simulate(&router, &arn, &["s3:GetObject"], resources, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {body}"
        );
        let expected_per_resource: Vec<String> =
            per_resource.iter().map(|value| value.to_string()).collect();
        assert_eq!(
            elements(&body, "EvalResourceDecision"),
            expected_per_resource,
            "case {case:?}: expected each resource to keep its own decision \
             {expected_per_resource:?}, got {body}"
        );
        // The most restrictive of the row's decisions, derived from the rule rather than
        // listed per row: an explicit deny from any resource, else an implicit deny from
        // any, else the allow.
        let expected = if per_resource.contains(&EXPLICIT_DENY) {
            EXPLICIT_DENY
        } else if per_resource.contains(&IMPLICIT_DENY) {
            IMPLICIT_DENY
        } else {
            ALLOWED
        };
        assert_eq!(
            element(&body, "EvalDecision"),
            expected,
            "case {case:?}: expected the action's result to report {expected}, got {body}"
        );
        // "In the top-level result, this field contains the union of matched statements
        // across all requested resources": the member lists the STATEMENTS that determined
        // the result, so the union names the statement that allowed (where any resource
        // was allowed) and the one that denied (where any was denied) — two statements of
        // this one document, each located by its own span in it — and names nothing where
        // no statement was about any resource.
        let named = usize::from(per_resource.contains(&ALLOWED))
            + usize::from(per_resource.contains(&EXPLICIT_DENY));
        let expected_matched: Vec<(String, String)> = std::iter::repeat_n(
            (
                user_inline_policy_id(AGGREGATE_USER, AGGREGATE_POLICY_NAME),
                IDENTITY_POLICY.to_string(),
            ),
            named,
        )
        .collect();
        let (top, _) = parts(&body);
        assert_eq!(
            attribution(top),
            expected_matched,
            "case {case:?}: expected the action's result to name {expected_matched:?}, got {body}"
        );
        // Each of those entries is a different statement, which is what the span it
        // carries says: two entries of one document are told apart by where they are in it.
        // The spans of one document TILE it — a statement begins where the statement before
        // it ended — so what has to differ is the SPAN, not every number in it.
        let columns = elements(top, "Column");
        let spans: Vec<&[String]> = columns.chunks(2).collect();
        let distinct: std::collections::BTreeSet<&[String]> = spans.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            spans.len(),
            "case {case:?}: expected every named statement to carry its own span, got \
             {spans:?} in {body}"
        );
    }
}

/// A boundary allowing the action on ONE of the two buckets the identity policy below
/// covers: "the permissions boundary sets the maximum permissions that the entity can
/// have" (`SimulatePrincipalPolicy$PermissionsBoundaryPolicyInputList`).
const BOUNDARY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::alpha/*"}]}"#;

/// An identity policy allowing the action on both buckets, so what the boundary withheld
/// is the only reason a resource is not allowed.
const ALLOW_BOTH_BUCKETS: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":["arn:aws:s3:::alpha/*","arn:aws:s3:::beta/*"]}]}"#;

/// One row of the permissions-boundary table.
struct Bounded {
    case: &'static str,
    /// The resources the request asks about.
    resources: &'static [&'static str],
    /// The decision each of them must reach, in request order.
    decisions: &'static [&'static str],
    /// The `AllowedByPermissionsBoundary` each of them must carry, as the wire spells a
    /// boolean.
    details: &'static [&'static str],
}

/// Rows spanning which resources a simulated boundary allows, in both orders.
const BOUNDED: &[Bounded] = &[
    Bounded {
        case: "every resource within the boundary",
        resources: &[ALPHA_OBJECT],
        decisions: &[ALLOWED],
        details: &["true"],
    },
    Bounded {
        case: "a resource outside the boundary, named second",
        resources: &[ALPHA_OBJECT, BETA_OBJECT],
        decisions: &[ALLOWED, IMPLICIT_DENY],
        details: &["true", "false"],
    },
    Bounded {
        case: "a resource outside the boundary, named first",
        resources: &[BETA_OBJECT, ALPHA_OBJECT],
        decisions: &[IMPLICIT_DENY, ALLOWED],
        details: &["false", "true"],
    },
    Bounded {
        case: "every resource outside the boundary",
        resources: &[BETA_OBJECT],
        decisions: &[IMPLICIT_DENY],
        details: &["false"],
    },
];

/// RULE 2 for the detail beside the decision: the action's own result reports the
/// boundary as having allowed it only where the boundary allowed it on EVERY resource
/// the request asked about, while each resource keeps its own detail.
#[tokio::test]
async fn the_actions_result_reports_the_boundary_over_every_resource() {
    let router = router();
    let arn = create_user(&router, "bounded", "bounded").await;
    put_policy(
        &router,
        "bounded",
        "PutUserPolicy",
        &[("UserName", "bounded")],
        "read-both-buckets",
        ALLOW_BOTH_BUCKETS,
    )
    .await;
    let extra = vec![(
        "PermissionsBoundaryPolicyInputList.member.1".to_string(),
        BOUNDARY.to_string(),
    )];

    for row in BOUNDED {
        let case = row.case;
        let (status, body) =
            simulate(&router, &arn, &["s3:GetObject"], row.resources, &extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {body}"
        );
        let expected_decisions: Vec<String> =
            row.decisions.iter().map(|value| value.to_string()).collect();
        assert_eq!(
            elements(&body, "EvalResourceDecision"),
            expected_decisions,
            "case {case:?}: expected the resources to be decided {expected_decisions:?}, \
             got {body}"
        );
        let (top, per_resource) = parts(&body);
        let expected_details: Vec<String> =
            row.details.iter().map(|value| value.to_string()).collect();
        assert_eq!(
            elements(per_resource, "AllowedByPermissionsBoundary"),
            expected_details,
            "case {case:?}: expected each resource to carry {expected_details:?}, got {body}"
        );
        // Allowed by the boundary only where every resource was.
        let expected = if row.details.iter().all(|allowed| *allowed == "true") {
            "true"
        } else {
            "false"
        };
        assert_eq!(
            elements(top, "AllowedByPermissionsBoundary"),
            vec![expected.to_string()],
            "case {case:?}: expected the action's result to carry {expected}, got {body}"
        );
    }
}

/// An `Allow` of one action on one resource, the document every grant path above carries.
///
/// Shared with `tests/all/simulated_policy_composition.rs`, so an identity-based grant a
/// simulation suite drives is written exactly one way.
pub(crate) fn allow(action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// The same `Allow` as a RESOURCE-based policy: the statement names the principal it
/// grants to, which is what makes it one.
fn allow_principal(principal: &str, action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{principal}"}},"Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// Create the user `user_name`, answering the ARN the account holds it under — which is
/// what a simulation addresses it by.
async fn create_user(router: &MultiServiceRouter, case: &str, user_name: &str) -> String {
    let (status, body) = call(router, "CreateUser", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateUser must succeed, got {status}: {body}"
    );
    element(&body, "Arn")
}

/// Embed `document` in the entity `owner` names, under `policy_name`, through
/// `operation` (`PutUserPolicy`, `PutGroupPolicy`).
async fn put_policy(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    owner: &[(&str, &str)],
    policy_name: &str,
    document: &str,
) {
    let mut params: Vec<(&str, &str)> = owner.to_vec();
    params.push(("PolicyName", policy_name));
    params.push(("PolicyDocument", document));
    expect_ok(router, case, operation, &params).await;
}

/// Create a managed policy carrying `document`, answering the ARN it is held under.
async fn create_policy(
    router: &MultiServiceRouter,
    case: &str,
    policy_name: &str,
    document: &str,
) -> String {
    let (status, body) = call(
        router,
        "CreatePolicy",
        &[
            ("PolicyName", policy_name),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePolicy must succeed, got {status}: {body}"
    );
    element(&body, "Arn")
}

/// Make one call that must succeed for the row to be set up at all.
async fn expect_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    params: &[(&str, &str)],
) {
    let (status, body) = call(router, operation, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must succeed, got {status}: {body}"
    );
}

// RULE 3, 4 and 5 below are about WHAT a result is and which part of the report carries
// which detail, which is the same projection rules 1 and 2 above are about:
//
// 3. the action's own result is ABOUT the action and the resource the simulation was about
//    — "the ARN template for the simulated resource type (for example,
//    `arn:${Partition}:s3:::${BucketName}/${KeyName}`), or `*` if no ARN format is defined
//    for the action. This is not a specific customer-provided resource ARN"
//    (`EvaluationResult$EvalResourceName`) where the request named several resources, the
//    one resource it named where it named one, and `*` where it named none — while each
//    `ResourceSpecificResults` entry names its own resource;
// 4. an action is decided only on the resources it can be PERFORMED on: a named resource of
//    another service than the one the action acts on is a request that action could not have
//    been made as, reported `implicitDeny` in its own entry and not part of the action's own
//    aggregate;
// 5. `EvalDecisionDetails` is the CROSS-ACCOUNT detail: "this parameter is populated for
//    only cross-account simulations … if the simulation evaluates policies within the same
//    account and includes a resource ARN, then the parameter is present but the response is
//    empty. If the simulation evaluates policies within the same account and specifies all
//    resources (`*`), then the parameter is not returned"
//    (`EvaluationResult$EvalDecisionDetails`), and `MissingContextValues` reports a key
//    where the document that needed it is in force: the documents on the PRINCIPAL by the
//    action's own result, and a key only the resource-based policy tests by the result of
//    the resource that policy is treated as attached to.

/// Resources the rows of those three suites are about: two of one service, one of another,
/// and a role, so what a result names cannot be keyed to one ARN form.
const OTHER_OBJECT_ARN: &str = "arn:aws:s3:::quarterly-reports/2026/q2.csv";
const TASK_DEFINITION_ARN: &str = "arn:aws:ecs:us-west-2:000000000000:task-definition/reports:1";
const OTHER_TASK_DEFINITION_ARN: &str =
    "arn:aws:ecs:us-west-2:000000000000:task-definition/reports:2";
const LOG_STREAM_ARN: &str = "arn:aws:logs:us-west-2:000000000000:log-group:app:log-stream:one";
const OTHER_LOG_STREAM_ARN: &str =
    "arn:aws:logs:us-west-2:000000000000:log-group:app:log-stream:two";

/// One grant covering every action those rows ask about, written on `*` so no row's decision
/// turns on how the statement is scoped: what the rows differ along is the RESOURCE the
/// request names, not what the document says about it.
const ALLOW_EVERY_PROJECTED_ACTION: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:ListAllMyBuckets","ecs:RunTask","iam:PassRole","logs:PutLogEvents"],"Resource":"*"}]}"#;

/// The text of each `EvaluationResults` member of a response, in report order: each result
/// opens with the action it is about, and what follows it up to the next one — the members
/// of that result and its `ResourceSpecificResults` — belongs to it.
fn projected_results(body: &str) -> Vec<&str> {
    const OPENS: &str = "<EvalActionName>";
    let within = body
        .split_once("<EvaluationResults>")
        .map_or(body, |(_, rest)| rest);
    let within = within
        .split_once("</EvaluationResults>")
        .map_or(within, |(part, _)| part);
    let mut found: Vec<&str> = Vec::new();
    let mut rest = within;
    while let Some(at) = rest.find(OPENS) {
        let from = &rest[at..];
        let ends = from[OPENS.len()..]
            .find(OPENS)
            .map_or(from.len(), |next| next + OPENS.len());
        found.push(&from[..ends]);
        rest = &from[ends..];
    }
    found
}

/// The text of a result's `EvalDecisionDetails` element, or `None` when the result carries
/// none: the query protocol writes a present-and-empty map either as an element with nothing
/// between its tags or as a self-closed one.
fn decision_details(part: &str) -> Option<String> {
    if part.contains("<EvalDecisionDetails/>") {
        return Some(String::new());
    }
    let (_, rest) = part.split_once("<EvalDecisionDetails>")?;
    let (within, _) = rest.split_once("</EvalDecisionDetails>")?;
    Some(within.to_string())
}

/// The keys one part of a report names under `MissingContextValues`, in report order.
fn missing_keys(part: &str) -> Vec<String> {
    const OPEN: &str = "<MissingContextValues>";
    const CLOSE: &str = "</MissingContextValues>";
    let mut found = Vec::new();
    let mut rest = part;
    while let Some(start) = rest.find(OPEN) {
        let text = &rest[start + OPEN.len()..];
        let end = text
            .find(CLOSE)
            .unwrap_or_else(|| panic!("{OPEN} is never closed in {part}"));
        found.extend(elements(&text[..end], "member"));
        rest = &text[end + CLOSE.len()..];
    }
    found
}

/// One row of the resource-name table.
struct Named {
    case: &'static str,
    action: &'static str,
    /// The resources the request names, if any.
    resources: &'static [&'static str],
    /// What the ACTION's own result must name as the resource it is about.
    expected: &'static str,
}

/// Rows spanning how many resources one request names, the service they belong to, and
/// whether the action is published with a resource type at all.
///
/// The templates are the ARN formats those services publish their resources under
/// (`crate::state::action_resource_types`'s catalog, read from the Service Authorization
/// Reference), and `*` is what an action published with no resource type has no ARN format
/// for.
const NAMED: &[Named] = &[
    Named {
        case: "a request naming no resource",
        action: "s3:GetObject",
        resources: &[],
        expected: "*",
    },
    Named {
        case: "a request naming the wildcard resource",
        action: "s3:GetObject",
        resources: &["*"],
        expected: "*",
    },
    Named {
        case: "a request naming one object",
        action: "s3:GetObject",
        resources: &[OBJECT_ARN],
        expected: OBJECT_ARN,
    },
    Named {
        case: "a request naming two objects",
        action: "s3:GetObject",
        resources: &[OBJECT_ARN, OTHER_OBJECT_ARN],
        expected: "arn:aws:s3:::${BucketName}/${KeyName}",
    },
    Named {
        case: "a request naming two resources of a regional service",
        action: "ecs:RunTask",
        resources: &[TASK_DEFINITION_ARN, OTHER_TASK_DEFINITION_ARN],
        expected: "arn:aws:ecs:${Region}:${Account}:${ResourceType}/${ResourcePath}",
    },
    Named {
        case: "a request naming two resources of another regional service",
        action: "logs:PutLogEvents",
        resources: &[LOG_STREAM_ARN, OTHER_LOG_STREAM_ARN],
        expected: "arn:aws:logs:${Region}:${Account}:${ResourceType}/${ResourcePath}",
    },
    Named {
        case: "a request naming two resources for an action with no resource type",
        action: "s3:ListAllMyBuckets",
        resources: &[OBJECT_ARN, OTHER_OBJECT_ARN],
        expected: "*",
    },
];

/// RULE 3: the action's own result names the resource the simulation was about — the one
/// resource a request named, the ARN template of the action's resource type where it named
/// several, and the wildcard where it named none — while each per-resource result names its
/// own resource.
#[tokio::test]
async fn the_actions_result_names_the_resource_the_simulation_is_about() {
    let router = router();
    let user_name = "projected-names";
    let arn = create_user(&router, "named resources", user_name).await;
    put_policy(
        &router,
        "named resources",
        "PutUserPolicy",
        &[("UserName", user_name)],
        "allow-the-asked-actions",
        ALLOW_EVERY_PROJECTED_ACTION,
    )
    .await;

    for case in NAMED {
        let (status, body) = simulate(&router, &arn, &[case.action], case.resources, &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        let reported = projected_results(&body);
        assert_eq!(
            reported.len(),
            1,
            "case {:?}: expected one result for one action, got {body}",
            case.case
        );
        let (own, per_resource) = parts(reported[0]);
        let named = element(own, "EvalResourceName");
        assert_eq!(
            named, case.expected,
            "case {:?}: expected the action's result to be about {:?}, got {named:?}: {body}",
            case.case, case.expected
        );
        // Each resource the request named keeps its own ARN in its own result: "the name of
        // the simulated resource, in Amazon Resource Name (ARN) format"
        // (`ResourceSpecificResult$EvalResourceName`).
        let per_resource_names = elements(per_resource, "EvalResourceName");
        let expected_names: Vec<String> = case
            .resources
            .iter()
            .map(|resource| (*resource).to_string())
            .collect();
        assert_eq!(
            per_resource_names, expected_names,
            "case {:?}: expected each resource to name itself {expected_names:?}, got \
             {per_resource_names:?}: {body}",
            case.case
        );
    }
}

/// RULE 4: a named resource the action cannot be performed on is reported `implicitDeny` in
/// its own entry and is not part of what the action's own result says — so two actions of
/// two services asked about one resource of each are each decided on the resource that is
/// theirs.
#[tokio::test]
async fn an_action_is_decided_only_on_the_resources_it_can_be_performed_on() {
    let router = router();
    let user_name = "projected-services";
    let arn = create_user(&router, "resources of two services", user_name).await;
    put_policy(
        &router,
        "resources of two services",
        "PutUserPolicy",
        &[("UserName", user_name)],
        "allow-the-asked-actions",
        ALLOW_EVERY_PROJECTED_ACTION,
    )
    .await;
    let role = format!("arn:aws:iam::{}:role/task-execution", account_id());
    let (status, body) = simulate(
        &router,
        &arn,
        &["ecs:RunTask", "iam:PassRole"],
        &[TASK_DEFINITION_ARN, &role],
        &[],
    )
    .await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    let reported = projected_results(&body);
    assert_eq!(
        reported.len(),
        2,
        "expected one result per action, got {body}"
    );

    // (the action, the decision each resource reaches in request order, the action's own
    // decision, the resource type its result is about): the task definition is a resource of
    // the task run alone and the role one of the role handover alone, while the grant covers
    // every resource — so what differs between the two results is which of the two named
    // resources the action can be performed on.
    let expected: [(&str, [&str; 2], &str, &str); 2] = [
        (
            "ecs:RunTask",
            [ALLOWED, IMPLICIT_DENY],
            ALLOWED,
            "arn:aws:ecs:${Region}:${Account}:${ResourceType}/${ResourcePath}",
        ),
        (
            "iam:PassRole",
            [IMPLICIT_DENY, ALLOWED],
            ALLOWED,
            "arn:aws:iam::${Account}:${ResourceType}/${ResourceName}",
        ),
    ];
    for (at, (action, per_resource_decisions, decision, about)) in expected.iter().enumerate() {
        let (own, per_resource) = parts(reported[at]);
        assert_eq!(
            element(own, "EvalActionName"),
            *action,
            "case {action:?}: expected the results in request order, got {body}"
        );
        assert_eq!(
            element(own, "EvalDecision"),
            *decision,
            "case {action:?}: expected the action's own decision to be {decision}, got {body}"
        );
        assert_eq!(
            element(own, "EvalResourceName"),
            *about,
            "case {action:?}: expected the action's result to be about {about}, got {body}"
        );
        let decisions = elements(per_resource, "EvalResourceDecision");
        let expected_decisions: Vec<String> = per_resource_decisions
            .iter()
            .map(|decision| (*decision).to_string())
            .collect();
        assert_eq!(
            decisions, expected_decisions,
            "case {action:?}: expected each resource to be decided on its own \
             {expected_decisions:?}, got {decisions:?}: {body}"
        );
    }
}

/// An identity policy whose grant is guarded by a condition over a key no request below
/// supplies: the key the PRINCIPAL's own documents needed.
const GUARDED_BY_A_TAG: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*","Condition":{"StringEquals":{"aws:PrincipalTag/team":"core"}}}]}"#;

/// The key `GUARDED_BY_A_TAG` tests, and the one the resource-based policy below tests.
const TAG_KEY: &str = "aws:PrincipalTag/team";
const TRANSPORT_KEY: &str = "aws:SecureTransport";

/// RULE 5, first half: the key a document in force on the PRINCIPAL needed is reported by
/// the action's own result, and a key only the resource-based policy tests by the result of
/// the resource that policy is treated as attached to.
#[tokio::test]
async fn a_key_is_reported_where_the_document_that_needed_it_is_in_force() {
    let router = router();
    let user_name = "projected-keys";
    let arn = create_user(&router, "keys by document", user_name).await;
    put_policy(
        &router,
        "keys by document",
        "PutUserPolicy",
        &[("UserName", user_name)],
        "guarded",
        GUARDED_BY_A_TAG,
    )
    .await;
    let resource_policy = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{arn}"}},"Action":"s3:GetObject","Resource":"{OBJECT_ARN}","Condition":{{"Bool":{{"aws:SecureTransport":"true"}}}}}}]}}"#
    );
    let extra = vec![("ResourcePolicy".to_string(), resource_policy)];
    let (status, body) = simulate(&router, &arn, &["s3:GetObject"], &[OBJECT_ARN], &extra).await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    let reported = projected_results(&body);
    assert_eq!(reported.len(), 1, "expected one result, got {body}");
    let (own, per_resource) = parts(reported[0]);
    assert_eq!(
        missing_keys(own),
        vec![TAG_KEY.to_string()],
        "expected the action's own result to report the key its own documents needed: {body}"
    );
    let mut expected = vec![TAG_KEY.to_string(), TRANSPORT_KEY.to_string()];
    expected.sort();
    let mut reported_per_resource = missing_keys(per_resource);
    reported_per_resource.sort();
    assert_eq!(
        reported_per_resource, expected,
        "expected the resource's own result to report the keys both the principal's \
         documents and the policy attached to it needed: {body}"
    );
    // Neither condition can hold without a value, so nothing granted the request: the
    // report above is of a decision and not of a list the response carries regardless.
    assert_eq!(
        element(own, "EvalDecision"),
        IMPLICIT_DENY,
        "expected the unsatisfiable conditions to leave nothing granting: {body}"
    );
}

/// RULE 5, second half: `EvalDecisionDetails` is present and empty for a same-account
/// simulation that names a resource, absent for one about all resources, and carries a
/// decision per policy type for a cross-account one.
#[tokio::test]
async fn the_decision_detail_reports_what_each_policy_type_said_across_accounts() {
    let router = router();
    let user_name = "projected-detail";
    let arn = create_user(&router, "decision detail", user_name).await;
    put_policy(
        &router,
        "decision detail",
        "PutUserPolicy",
        &[("UserName", user_name)],
        "allow-the-asked-actions",
        ALLOW_EVERY_PROJECTED_ACTION,
    )
    .await;

    // Same account, one named resource: the member is present and carries nothing.
    let (status, body) = simulate(&router, &arn, &["s3:GetObject"], &[OBJECT_ARN], &[]).await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert_eq!(
        decision_details(projected_results(&body)[0]),
        Some(String::new()),
        "expected a same-account simulation naming a resource to report the detail empty: \
         {body}"
    );

    // Same account, no resource named: the member is not returned at all.
    let (status, body) = simulate(&router, &arn, &["s3:GetObject"], &[], &[]).await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert_eq!(
        decision_details(projected_results(&body)[0]),
        None,
        "expected a simulation about all resources to report no detail: {body}"
    );

    // Another account's resource, with the policy attached to it: both sides have to allow,
    // so the detail says what each policy type decided.
    let another_account = "210987654321";
    let other_object = format!("arn:aws:s3:::{another_account}-reports/q1.csv");
    let resource_policy = allow_principal(&arn, "s3:GetObject", &other_object);
    let extra = vec![
        ("ResourcePolicy".to_string(), resource_policy),
        ("ResourceOwner".to_string(), another_account.to_string()),
    ];
    let (status, body) =
        simulate(&router, &arn, &["s3:GetObject"], &[&other_object], &extra).await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    let detail = decision_details(projected_results(&body)[0])
        .unwrap_or_else(|| panic!("a cross-account simulation must report the detail: {body}"));
    for policy_type in [IDENTITY_POLICY, RESOURCE_POLICY] {
        assert!(
            detail.contains(policy_type),
            "expected the cross-account detail to report {policy_type:?}, got {detail:?}: \
             {body}"
        );
    }
    assert!(
        detail.contains(ALLOWED),
        "expected the cross-account detail to report what each side decided, got \
         {detail:?}: {body}"
    );
    assert_eq!(
        element(projected_results(&body)[0], "EvalDecision"),
        ALLOWED,
        "expected both sides allowing to grant the cross-account request: {body}"
    );
}
