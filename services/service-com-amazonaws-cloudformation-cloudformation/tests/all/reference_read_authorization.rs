// SPDX-License-Identifier: CC0-1.0

//! A document submitted BY REFERENCE is read AS THE CALLER, end to end over the awsQuery
//! wire.
//!
//! RULE: a `TemplateURL` (and every other reference a request carries) says WHERE a document
//! is, and CloudFormation goes and fetches it with the CALLER's own credentials — so the
//! object is read only if the calling principal's IAM policies allow that read
//! (`s3:GetObject`, or `s3:GetObjectVersion` for a reference naming a version). A principal
//! those policies do not allow it for is answered the refusal Amazon S3 would have given the
//! read — a `ValidationError` reporting `Access Denied` — and nothing is created, changed or
//! recorded for it.
//!
//! The rows differ along what the rule generalises over: what the caller's policy says (no
//! grant at all, a grant on the object, a grant on another key of the same bucket, a grant on
//! another bucket, a wildcard grant, a wildcard grant with an explicit deny over the object,
//! a grant of the versioned action), WHICH action the reference asks for (a plain reference
//! and one naming a `versionId`), and whether the caller is an IAM user at all (the account's
//! own published credential, which no identity policy bounds). Beside the table, the same
//! decision is asserted for a second reference member (`StackPolicyURL`) and for two further
//! operations that take a `TemplateURL` (`CreateChangeSet` and `ValidateTemplate`), since the
//! rule is about the READ rather than about one operation.

use serde_json::json;

use crate::harness::{call_as, member, router_with_engine, xml_text, REGION};
use crate::stack_lifecycle::user_with_inline_policy;
use crate::template_by_reference::{create_staging_bucket, stage, STAGING_BUCKET};
use smithy_server_runtime::MultiServiceRouter;

/// The template every row submits by reference. Its content is irrelevant to the rule: what
/// decides each row is whether the caller may READ the object.
fn staged_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Data": {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": "cfn-read-as-caller-data"}}
        }
    })
    .to_string()
}

/// The key the template is staged under, and the key of a second object no row grants.
const TEMPLATE_KEY: &str = "templates/main.json";

/// The stack policy one case submits by reference, staged beside the template.
const POLICY_KEY: &str = "policies/stack-policy.json";

/// A stack policy that permits every update action, so what a case asserts about it is the
/// READ of the object rather than anything the document says.
fn staged_policy() -> String {
    json!({
        "Statement": [{"Effect": "Allow", "Action": "Update:*", "Principal": "*", "Resource": "*"}]
    })
    .to_string()
}

/// The URL one staged object is addressed by, path-style.
fn staged_url(key: &str) -> String {
    format!("https://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{key}")
}

/// The sentence CloudFormation answers for a read Amazon S3 would not serve, stated here
/// independently of the crate: what Amazon S3 answered, under the prefix CloudFormation
/// reports another service's refusal under, and the line that error is documented on.
fn access_denied() -> String {
    "S3 error: Access Denied\nFor more information check \
     http://docs.aws.amazon.com/AmazonS3/latest/API/ErrorResponses.html"
        .to_string()
}

/// Create an IAM user carrying `document` as its one inline policy, and answer the access key
/// id a request of that user signs with — the shared minting of a principal to call as
/// ([`user_with_inline_policy`]).
async fn user_with_policy(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    document: &str,
) -> String {
    user_with_inline_policy(router, case, user_name, "template-read", document).await
}

/// A router with the staging bucket created and both objects staged, as a client's own
/// `PutObject` stages them before it deploys.
async fn staged_router() -> MultiServiceRouter {
    let (router, _engine) = router_with_engine();
    create_staging_bucket(&router).await;
    stage(&router, TEMPLATE_KEY, &staged_template()).await;
    stage(&router, POLICY_KEY, &staged_policy()).await;
    router
}

/// One inline policy document granting `actions` on `resources`, plus the CloudFormation
/// actions every row's caller needs to reach the operation at all.
fn policy(statements: serde_json::Value) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The statement every row carries: the caller may call CloudFormation. What each row varies
/// is what it may read in Amazon S3.
fn may_call_cloudformation() -> serde_json::Value {
    json!({"Effect": "Allow", "Action": "cloudformation:*", "Resource": "*"})
}

/// One row: what the caller's policy says, and whether the reference may then be read.
struct Row {
    case: &'static str,
    user: &'static str,
    stack: &'static str,
    /// The S3 statements the caller's policy carries beside [`may_call_cloudformation`].
    s3: serde_json::Value,
    /// The reference the row submits: the template's key, with a `versionId` where the row
    /// names one.
    version: Option<&'static str>,
    /// Whether the read is authorized — the create is accepted — or refused.
    readable: bool,
}

fn rows() -> Vec<Row> {
    let object = format!("arn:aws:s3:::{STAGING_BUCKET}/{TEMPLATE_KEY}");
    vec![
        Row {
            case: "a caller holding no Amazon S3 grant at all",
            user: "cfn-reader-nogrant",
            stack: "cfnread-nogrant",
            s3: json!([]),
            version: None,
            readable: false,
        },
        Row {
            case: "a grant of s3:GetObject on the object the reference addresses",
            user: "cfn-reader-granted",
            stack: "cfnread-granted",
            s3: json!([{"Effect": "Allow", "Action": "s3:GetObject", "Resource": object}]),
            version: None,
            readable: true,
        },
        Row {
            case: "a grant on a prefix of the same bucket the object is not under",
            user: "cfn-reader-otherprefix",
            stack: "cfnread-otherprefix",
            s3: json!([{
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": format!("arn:aws:s3:::{STAGING_BUCKET}/elsewhere/*"),
            }]),
            version: None,
            readable: false,
        },
        Row {
            case: "a grant on another bucket's objects",
            user: "cfn-reader-otherbucket",
            stack: "cfnread-otherbucket",
            s3: json!([{
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": "arn:aws:s3:::another-bucket/*",
            }]),
            version: None,
            readable: false,
        },
        Row {
            case: "a wildcard grant of every Amazon S3 action",
            user: "cfn-reader-wildcard",
            stack: "cfnread-wildcard",
            s3: json!([{"Effect": "Allow", "Action": "s3:*", "Resource": "*"}]),
            version: None,
            readable: true,
        },
        Row {
            case: "a wildcard grant with an explicit deny over the object",
            user: "cfn-reader-denied",
            stack: "cfnread-denied",
            s3: json!([
                {"Effect": "Allow", "Action": "s3:*", "Resource": "*"},
                {"Effect": "Deny", "Action": "s3:GetObject", "Resource": object},
            ]),
            version: None,
            readable: false,
        },
        Row {
            case: "a reference naming a version, under a grant of the unversioned action alone",
            user: "cfn-reader-unversioned",
            stack: "cfnread-unversioned",
            s3: json!([{"Effect": "Allow", "Action": "s3:GetObject", "Resource": object}]),
            version: Some("8vX1kQw0RqZt2mBnPdLe5CsYhJfUaG7T"),
            readable: false,
        },
    ]
}

/// RULE: the object a reference addresses is read as the CALLER, so a create naming a
/// `TemplateURL` the calling principal's own policies do not let it read is refused with the
/// refusal that read would be, and one it may read goes ahead.
#[tokio::test]
async fn a_template_reference_is_read_with_the_callers_permissions() {
    let router = staged_router().await;
    for row in rows() {
        let document = policy(json!([may_call_cloudformation()]
            .into_iter()
            .chain(row.s3.as_array().cloned().unwrap_or_default())
            .collect::<Vec<_>>()));
        let access_key_id = user_with_policy(&router, row.case, row.user, &document).await;
        let mut url = staged_url(TEMPLATE_KEY);
        if let Some(version) = row.version {
            url.push_str(&format!("?versionId={version}"));
        }
        let (code, body) = call_as(
            &router,
            &access_key_id,
            "CreateStack",
            &[member("StackName", row.stack), member("TemplateURL", &url)],
        )
        .await;
        if row.readable {
            assert_eq!(
                code, 200,
                "case {:?}: the create must be accepted, got {code} {body}",
                row.case
            );
            assert!(
                xml_text(&body, "StackId").is_some(),
                "case {:?}: an accepted create answers a stack id, got {body}",
                row.case
            );
            continue;
        }
        assert_eq!(
            code, 400,
            "case {:?}: the create must be refused, got {code} {body}",
            row.case
        );
        assert_eq!(
            xml_text(&body, "Code").as_deref(),
            Some("ValidationError"),
            "case {:?}: expected a ValidationError, got {body}",
            row.case
        );
        assert_eq!(
            xml_text(&body, "Message"),
            Some(access_denied()),
            "case {:?}: expected the refusal Amazon S3 would have given, got {body}",
            row.case
        );
        // ...and nothing was created for it: the stack the refused create named is not there.
        let (code, described) = call_as(
            &router,
            &access_key_id,
            "DescribeStacks",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            code, 400,
            "case {:?}: a refused create must leave no stack, got {code} {described}",
            row.case
        );
    }
}

/// RULE: the decision is about the READ, so every reference member and every operation that
/// takes one is held to it — a caller that may read the TEMPLATE object and not the stack
/// POLICY object is refused for the policy, and the same unreadable template refuses
/// `CreateChangeSet` and `ValidateTemplate` too.
#[tokio::test]
async fn every_reference_of_every_operation_is_read_the_same_way() {
    let router = staged_router().await;
    let template = format!("arn:aws:s3:::{STAGING_BUCKET}/{TEMPLATE_KEY}");
    let template_only = policy(json!([
        may_call_cloudformation(),
        {"Effect": "Allow", "Action": "s3:GetObject", "Resource": template},
    ]));
    let access_key_id =
        user_with_policy(&router, "the template alone", "cfn-reader-template", &template_only)
            .await;

    // The template is readable, so the create reaches the POLICY reference — which this
    // caller may not read.
    let (code, body) = call_as(
        &router,
        &access_key_id,
        "CreateStack",
        &[
            member("StackName", "cfnread-policyref"),
            member("TemplateURL", &staged_url(TEMPLATE_KEY)),
            member("StackPolicyURL", &staged_url(POLICY_KEY)),
        ],
    )
    .await;
    assert_eq!(
        code, 400,
        "case \"a stack policy by reference\": the create must be refused, got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "Message"),
        Some(access_denied()),
        "case \"a stack policy by reference\": expected the read's own refusal, got {body}"
    );

    // ...and with both objects readable the same create is accepted, which is what says the
    // refusal above was about the policy object and not about the request's shape.
    let both = policy(json!([
        may_call_cloudformation(),
        {"Effect": "Allow", "Action": "s3:GetObject", "Resource": format!("arn:aws:s3:::{STAGING_BUCKET}/*")},
    ]));
    let allowed =
        user_with_policy(&router, "both objects", "cfn-reader-both", &both).await;
    let (code, body) = call_as(
        &router,
        &allowed,
        "CreateStack",
        &[
            member("StackName", "cfnread-bothref"),
            member("TemplateURL", &staged_url(TEMPLATE_KEY)),
            member("StackPolicyURL", &staged_url(POLICY_KEY)),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case \"both objects readable\": the create must be accepted, got {code} {body}"
    );

    // A caller holding no Amazon S3 grant is refused the same read whichever operation takes
    // the reference.
    let none = policy(json!([may_call_cloudformation()]));
    let unauthorized = user_with_policy(&router, "no grant", "cfn-reader-none", &none).await;
    for (action, members) in [
        (
            "CreateChangeSet",
            vec![
                member("StackName", "cfnread-changeset"),
                member("ChangeSetName", "cfnread-changeset-plan"),
                member("ChangeSetType", "CREATE"),
                member("TemplateURL", &staged_url(TEMPLATE_KEY)),
            ],
        ),
        (
            "ValidateTemplate",
            vec![member("TemplateURL", &staged_url(TEMPLATE_KEY))],
        ),
    ] {
        let (code, body) = call_as(&router, &unauthorized, action, &members).await;
        assert_eq!(
            code, 400,
            "case {action:?}: the reference must be refused, got {code} {body}"
        );
        assert_eq!(
            xml_text(&body, "Message"),
            Some(access_denied()),
            "case {action:?}: expected the read's own refusal, got {body}"
        );
    }
}

/// RULE: the account's own published credential is not a principal any identity policy
/// bounds, so a request signed with it reads the reference as before — which is what every
/// other suite in this crate relies on.
#[tokio::test]
async fn the_accounts_own_credential_reads_a_reference_as_before() {
    let router = staged_router().await;
    let (code, body) = crate::harness::call(
        &router,
        "CreateStack",
        &[
            member("StackName", "cfnread-ambient"),
            member("TemplateURL", &staged_url(TEMPLATE_KEY)),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case \"the account's own credential\": the create must be accepted, got {code} {body}"
    );
    assert!(
        xml_text(&body, "StackId").is_some(),
        "case \"the account's own credential\": expected a stack id, got {body}"
    );
}
