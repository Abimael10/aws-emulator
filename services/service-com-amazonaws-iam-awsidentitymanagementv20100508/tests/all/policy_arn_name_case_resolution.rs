// SPDX-License-Identifier: CC0-1.0

//! WHICH managed policy an ARN names when its name segment is spelled in another case, end
//! to end over the awsQuery wire.
//!
//! RULE: IAM entity names "are not distinguished by case", and a managed policy's name is
//! the LAST segment of its ARN, so an ARN that spells a held policy's name in any other case
//! names THAT policy: every operation addressed by a `PolicyArn` — the reads, the version
//! writes, the tag writes and the attachments — resolves the same record, and reports it
//! under the ARN the create minted. Everything else the ARN spells stays part of WHICH
//! policy is named: an ARN carrying another path, another account or another partition names
//! no policy and is refused as the missing entity.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: the case the
//! name was created in, the case the request spells it in (lower, upper, swapped), the
//! policy's path (root, one segment, nested), and the operation that resolves the ARN. No
//! single request is the specification.
//!
//! The awsQuery harness (`call`, `element`, `error_code`, `param`, `borrowed`) is the one
//! `tests/all/user_lifecycle.rs` and `tests/all/role_lifecycle.rs` own and every suite in
//! this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::role_lifecycle::{borrowed, param};
use crate::user_lifecycle::{account_id, call, element, error_code, router};

/// A managed policy document, the content every fixture policy is created with.
const POLICY_DOCUMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"sts:GetCallerIdentity","Resource":"*"}]}"#;

/// The trust policy a role takes to exist, for the attachment row.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// A managed policy ARN is `arn:aws:iam::<account>:policy<path><name>` — IAM is global, so
/// the region segment is EMPTY.
fn policy_arn(path: &str, policy_name: &str) -> String {
    format!("arn:aws:iam::{}:policy{path}{policy_name}", account_id())
}

/// Every spelling of `name` that differs from it only in case: lower, upper and swapped.
fn other_cases(name: &str) -> Vec<(&'static str, String)> {
    let swapped: String = name
        .chars()
        .map(|character| {
            if character.is_uppercase() {
                character.to_ascii_lowercase()
            } else {
                character.to_ascii_uppercase()
            }
        })
        .collect();
    vec![
        ("the name lower-cased", name.to_ascii_lowercase()),
        ("the name upper-cased", name.to_ascii_uppercase()),
        ("the name's case swapped", swapped),
    ]
}

/// Create a policy named `policy_name` under `path`, and answer the ARN the create minted.
async fn create_policy(router: &MultiServiceRouter, policy_name: &str, path: &str) -> String {
    let params = vec![
        param("PolicyName", policy_name),
        param("Path", path),
        param("PolicyDocument", POLICY_DOCUMENT),
    ];
    let (status, body) = call(router, "CreatePolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {policy_name:?}: the fixture CreatePolicy must succeed, got {status}: {body}"
    );
    element(&body, "Arn")
}

/// One row: a policy name, the path it is created under, and what each row is called.
struct PolicyCase {
    case: &'static str,
    policy_name: &'static str,
    path: &'static str,
}

/// The table: names mixed, upper and lower, carrying the punctuation `policyNameType`
/// admits, under the root, a one-segment path and a nested path.
fn policy_cases() -> Vec<PolicyCase> {
    vec![
        PolicyCase {
            case: "a mixed-case name at the root",
            policy_name: "MixedCasePolicy",
            path: "/",
        },
        PolicyCase {
            case: "a lower-case name under one path segment",
            policy_name: "lowercasepolicy",
            path: "/boundaries/",
        },
        PolicyCase {
            case: "an upper-case name under a nested path",
            policy_name: "UPPERCASEPOLICY",
            path: "/team/eng/tools/",
        },
        PolicyCase {
            // `policyNameType`'s pattern is `^[\w+=,.@-]+$`
            // (`smithy_query(query="shape", shape="policyNameType")`), so the folding has
            // to leave every other character of that alphabet alone.
            case: "a name spanning the published name alphabet",
            policy_name: "Policy_with+Every=allowed.Char@-2",
            path: "/mixed/",
        },
    ]
}

/// RULE: `GetPolicy` on an ARN that spells the held name in another case reports the SAME
/// policy — the same id, the same path, and the ARN the create minted, not the one the
/// request spelled.
#[tokio::test]
async fn a_read_resolves_a_policy_arn_whatever_case_its_name_is_spelled_in() {
    let router = router();

    for case in policy_cases() {
        let minted = create_policy(&router, case.policy_name, case.path).await;
        let (status, created) = call(&router, "GetPolicy", &[("PolicyArn", minted.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {:?}: the fixture must read back, got {status}: {created}",
            case.case
        );
        let policy_id = element(&created, "PolicyId");

        for (spelling, name) in other_cases(case.policy_name) {
            let addressed = policy_arn(case.path, &name);
            let row = format!("{} addressed by {spelling}", case.case);
            let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", addressed.as_str())])
                .await;
            assert_eq!(
                status, 200,
                "case {row:?}: {addressed:?} names the held policy, so the read must \
                 succeed, got {status}: {body}"
            );
            assert_eq!(
                element(&body, "PolicyId"),
                policy_id,
                "case {row:?}: the read must report the HELD policy, got {body}"
            );
            assert_eq!(
                element(&body, "Arn"),
                minted,
                "case {row:?}: the read must report the ARN the create minted, got {body}"
            );
            assert_eq!(
                element(&body, "PolicyName"),
                case.policy_name,
                "case {row:?}: the read must report the name the create stored, got {body}"
            );
            assert_eq!(
                element(&body, "Path"),
                case.path,
                "case {row:?}: the read must report the stored path, got {body}"
            );
        }
    }
}

/// RULE: it is ONE resolution, so every other operation addressed by a `PolicyArn` answers
/// the same way — a version written through the miscased ARN is a version of the held
/// policy, a tag written through it is the held policy's tag, and an attachment of it
/// attaches that policy.
#[tokio::test]
async fn every_operation_addressed_by_a_policy_arn_resolves_the_name_alike() {
    let router = router();
    let minted = create_policy(&router, "SharedCasePolicy", "/shared/").await;
    let (_, created) = call(&router, "GetPolicy", &[("PolicyArn", minted.as_str())]).await;
    let policy_id = element(&created, "PolicyId");

    let (status, body) = call(
        &router,
        "CreateRole",
        &[
            ("RoleName", "case-role"),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(status, 200, "the fixture role must be created, got {body}");

    let lowered = policy_arn("/shared/", "sharedcasepolicy");
    let uppered = policy_arn("/shared/", "SHAREDCASEPOLICY");

    // A version written through the lower-cased ARN belongs to the held policy, which the
    // read of the MINTED ARN reports as its default.
    let (status, body) = call(
        &router,
        "CreatePolicyVersion",
        &[
            ("PolicyArn", lowered.as_str()),
            ("PolicyDocument", POLICY_DOCUMENT),
            ("SetAsDefault", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreatePolicyVersion through the lower-cased ARN must succeed, got {status}: {body}"
    );
    assert_eq!(
        element(&body, "VersionId"),
        "v2",
        "the version must be the held policy's second, got {body}"
    );
    let (_, read) = call(&router, "GetPolicy", &[("PolicyArn", minted.as_str())]).await;
    assert_eq!(
        element(&read, "DefaultVersionId"),
        "v2",
        "the held policy must carry the version written through the miscased ARN, got {read}"
    );
    assert_eq!(
        element(&read, "PolicyId"),
        policy_id,
        "the write must not have created a second policy, got {read}"
    );

    // A tag written through the upper-cased ARN is the held policy's tag.
    let (status, body) = call(
        &router,
        "TagPolicy",
        &[
            ("PolicyArn", uppered.as_str()),
            ("Tags.member.1.Key", "env"),
            ("Tags.member.1.Value", "prod"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "TagPolicy through the upper-cased ARN must succeed, got {status}: {body}"
    );
    let (_, read) = call(&router, "GetPolicy", &[("PolicyArn", minted.as_str())]).await;
    assert_eq!(
        element(&read, "Key"),
        "env",
        "the held policy must carry the tag written through the miscased ARN, got {read}"
    );

    // An attachment made through the lower-cased ARN attaches the held policy, counted once.
    let (status, body) = call(
        &router,
        "AttachRolePolicy",
        &[("RoleName", "case-role"), ("PolicyArn", lowered.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "AttachRolePolicy through the lower-cased ARN must succeed, got {status}: {body}"
    );
    // Attaching the same policy again under ANOTHER spelling is the same attachment, so the
    // policy is attached once.
    let (status, body) = call(
        &router,
        "AttachRolePolicy",
        &[("RoleName", "case-role"), ("PolicyArn", uppered.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "a repeat attach under another spelling must succeed, got {status}: {body}"
    );
    let (_, read) = call(&router, "GetPolicy", &[("PolicyArn", minted.as_str())]).await;
    assert_eq!(
        element(&read, "AttachmentCount"),
        "1",
        "two spellings of one ARN are one attachment, got {read}"
    );

    let (status, body) = call(
        &router,
        "ListAttachedRolePolicies",
        &[("RoleName", "case-role")],
    )
    .await;
    assert_eq!(
        status, 200,
        "ListAttachedRolePolicies must succeed, got {status}: {body}"
    );
    assert_eq!(
        element(&body, "PolicyArn"),
        lowered,
        "the role must carry exactly the ARN the attach named, got {body}"
    );

    // And a detach naming the OTHER spelling removes that one attachment.
    let (status, body) = call(
        &router,
        "DetachRolePolicy",
        &[("RoleName", "case-role"), ("PolicyArn", uppered.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "DetachRolePolicy through another spelling must succeed, got {status}: {body}"
    );
    let (_, read) = call(&router, "GetPolicy", &[("PolicyArn", minted.as_str())]).await;
    assert_eq!(
        element(&read, "AttachmentCount"),
        "0",
        "the detach must have removed the attachment, got {read}"
    );
}

/// RULE (the other half): only the NAME is folded. An ARN carrying another path, another
/// account or another partition names no policy, whatever case it spells the name in, and is
/// refused as the missing entity with the reading operation's sentence.
#[tokio::test]
async fn only_the_name_segment_is_folded() {
    let router = router();
    let minted = create_policy(&router, "FoldedNamePolicy", "/held/").await;
    let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", minted.as_str())]).await;
    assert_eq!(status, 200, "the fixture must read back, got {body}");

    let account = account_id();
    // Rows: the held name (in another case) at the account root, under another path, under a
    // nested path, in another account and in another partition.
    let absent = [
        (
            "the held name at the account root",
            format!("arn:aws:iam::{account}:policy/foldednamepolicy"),
        ),
        (
            "the held name under another path",
            format!("arn:aws:iam::{account}:policy/elsewhere/FOLDEDNAMEPOLICY"),
        ),
        (
            "the held name under a nested path",
            format!("arn:aws:iam::{account}:policy/held/nested/foldednamepolicy"),
        ),
        (
            "the held name in another account",
            "arn:aws:iam::210987654321:policy/held/foldednamepolicy".to_string(),
        ),
        (
            "the held name in another partition",
            format!("arn:aws-us-gov:iam::{account}:policy/held/foldednamepolicy"),
        ),
    ];

    for (case, arn) in absent {
        let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", arn.as_str())]).await;
        assert_eq!(
            status, 404,
            "case {case:?}: {arn:?} names no policy, so the read must answer 404, got \
             {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            format!("Policy {arn} was not found."),
            "case {case:?}: the refusal must echo the ARN the request supplied, got {:?}",
            element(&body, "Message")
        );
    }
}
