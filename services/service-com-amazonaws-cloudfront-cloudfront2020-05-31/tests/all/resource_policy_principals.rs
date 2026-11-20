// SPDX-License-Identifier: CC0-1.0

//! WHO a resource policy may be attached FOR: the identities CloudFront's resource-policy
//! surface resolves before it stores a document, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! RULE: a resource policy states who the resource is SHARED WITH, and a share with an
//! identity that does not exist grants nothing — so every principal the document's statements
//! name is resolved before anything is stored, and a document naming one that does not
//! resolve is refused `InvalidArgument` (HTTP 400) with
//! [`RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE`], leaving the resource with the policy it
//! already carried. This emulator serves ONE account, so an identity under any other account
//! id — a whole account, its root, or one of its roles, users or sessions — is an identity
//! nothing holds; an IAM role or user of THIS account is resolved against the account's own
//! IAM, which is why IAM is mounted beside CloudFront here and the roles a row names are ones
//! a client really created through IAM's own API; and a principal that names no single
//! identity at all — the `*` wildcard, a service principal, this account's own root — is
//! resolved by neither reading and refuses nothing.
//!
//! The rows differ along what the rule generalises over: which form the principal takes (a
//! bare account id, an account's root ARN, a user ARN, a role ARN, a session ARN, a service
//! principal, the wildcard), whether the identity it names exists, which account it belongs
//! to, which element names it (`Principal`, `NotPrincipal`), whether it is written as a value
//! or in a list of them, and which statement of a two-statement document carries it.
//!
//! The harness (the router's request helpers, the VPC origin every policy is attached to, and
//! the baseline document) is the lifecycle suites', reused rather than copied.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    EMPTY_RESOURCE_POLICY_DOCUMENT, RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE,
};
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{Answer, ACCOUNT_ID};
use crate::resource_policy_lifecycle::{
    created_vpc_origin_arn, get_resource_policy, granted_principal, message, policy,
    put_resource_policy, reported_document, stored,
};

/// IAM's own endpoint and API version, for the awsQuery calls that stage a role this account
/// really holds.
const IAM_HOST: &str = "iam.amazonaws.com";
const IAM_VERSION: &str = "2010-05-08";

/// The Region the staging calls are signed for. IAM is global; this is the Region a client
/// signs its calls to it in.
const REGION: &str = "us-east-1";

/// An account id this emulator does not serve: the documentation placeholder account, which
/// exists nowhere.
const ANOTHER_ACCOUNT: &str = "111122223333";

/// A second one, so a row can name two accounts that are not this one.
const A_THIRD_ACCOUNT: &str = "444455556666";

/// The name of the IAM role a client stages through IAM's own API, and of a role no client
/// ever created.
const HELD_ROLE: &str = "cf-resource-policy-grantee";
const UNHELD_ROLE: &str = "cf-resource-policy-never-created";

/// One router carrying CloudFront and IAM over one shared engine: whether a role or user this
/// account's policy names exists is IAM's answer, so IAM has to be there to give it.
fn emulator_with_iam() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_servicequotas_servicequotasv20190624::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Call one IAM operation over awsQuery and answer its XML body.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &[(&str, String)]) -> String {
    let mut body = format!("Action={action}&Version={IAM_VERSION}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(
            &::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        );
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", IAM_HOST)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-www-form-urlencoded")
        .body(::bytes::Bytes::from(body))
        .expect("the IAM request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    assert_eq!(
        status, 200,
        "case {action}: expected HTTP 200 from IAM, got {status} with {text}"
    );
    text
}

/// Create the IAM role `name` through IAM's own API and answer the ARN IAM minted for it, so
/// the identity a row grants to is one a client really created rather than a value written
/// here.
async fn created_role(router: &MultiServiceRouter, name: &str) -> String {
    let trust = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"cloudfront.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}";
    let answered = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust.to_string()),
        ],
    )
    .await;
    let open = "<Arn>";
    let close = "</Arn>";
    let start = answered
        .find(open)
        .unwrap_or_else(|| panic!("IAM must report the role's ARN, got {answered}"))
        + open.len();
    let end = answered[start..]
        .find(close)
        .unwrap_or_else(|| panic!("IAM must report the role's ARN, got {answered}"))
        + start;
    answered[start..end].to_string()
}

/// A one-statement document sharing `resource_arn` with `principal`, written as the policy
/// language writes a typed principal: the baseline every row varies the PRINCIPAL of.
fn sharing_with(resource_arn: &str, principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"{principal}\"}},\
         \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{resource_arn}\"}}]}}"
    )
}

/// The same, with the principals written as a LIST under the `AWS` type.
fn sharing_with_all(resource_arn: &str, principals: &[&str]) -> String {
    let named: Vec<String> = principals
        .iter()
        .map(|principal| format!("\"{principal}\""))
        .collect();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":[{}]}},\
         \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{resource_arn}\"}}]}}",
        named.join(",")
    )
}

/// A two-statement document whose SECOND statement names `principal` under the principal
/// TYPE `principal_type`, in `element` — the element a statement grants to, or its
/// complement.
fn second_statement_naming_typed(
    resource_arn: &str,
    element: &str,
    principal_type: &str,
    principal: &str,
) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Sid\":\"first\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
         \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{resource_arn}\"}},\
         {{\"Sid\":\"second\",\"Effect\":\"Allow\",\
         \"{element}\":{{\"{principal_type}\":\"{principal}\"}},\
         \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{resource_arn}\"}}]}}"
    )
}

/// [`second_statement_naming_typed`] under the `AWS` type, which is the type an account or an
/// IAM identity is named under.
fn second_statement_naming(resource_arn: &str, element: &str, principal: &str) -> String {
    second_statement_naming_typed(resource_arn, element, "AWS", principal)
}

/// How one row writes its document: from the ARN of the VPC origin the policy is attached to
/// and the ARN of the role IAM really holds.
type Document = Box<dyn Fn(&str, &str) -> String>;

/// One row: what the document names as its principals, and whether the surface attaches it.
struct Row {
    case: &'static str,
    /// The document, built from the ARN of the VPC origin it is attached to and the ARN of
    /// the role IAM really holds.
    document: Document,
    /// Whether the surface stores the document, rather than refusing it on its principals.
    attached: bool,
}

/// Every row the rule is asserted over.
fn rows() -> Vec<Row> {
    vec![
        // Identities this account holds, or principals that name no single identity: attached.
        Row {
            case: "this account's root",
            document: Box::new(|arn, _| sharing_with(arn, &granted_principal())),
            attached: true,
        },
        Row {
            case: "this account by its bare id",
            document: Box::new(|arn, _| sharing_with(arn, ACCOUNT_ID)),
            attached: true,
        },
        Row {
            case: "the wildcard, which names every principal rather than one",
            document: Box::new(|arn, _| sharing_with(arn, "*")),
            attached: true,
        },
        Row {
            case: "a service principal, which is no identity of any account",
            document: Box::new(|arn, _| {
                second_statement_naming_typed(
                    arn,
                    "Principal",
                    "Service",
                    "cloudfront.amazonaws.com",
                )
            }),
            attached: true,
        },
        Row {
            case: "a role of this account that IAM holds",
            document: Box::new(sharing_with),
            attached: true,
        },
        Row {
            case: "this account's root beside the wildcard, as a list",
            document: Box::new(|arn, _| {
                sharing_with_all(arn, &[&granted_principal(), "*"])
            }),
            attached: true,
        },
        // Identities nothing holds: refused.
        Row {
            case: "another account's root",
            document: Box::new(|arn, _| {
                sharing_with(arn, &format!("arn:aws:iam::{ANOTHER_ACCOUNT}:root"))
            }),
            attached: false,
        },
        Row {
            case: "another account by its bare id",
            document: Box::new(|arn, _| sharing_with(arn, ANOTHER_ACCOUNT)),
            attached: false,
        },
        Row {
            case: "another account's IAM user",
            document: Box::new(|arn, _| {
                sharing_with(arn, &format!("arn:aws:iam::{ANOTHER_ACCOUNT}:user/builder"))
            }),
            attached: false,
        },
        Row {
            case: "another account's IAM role",
            document: Box::new(|arn, _| {
                sharing_with(
                    arn,
                    &format!("arn:aws:iam::{A_THIRD_ACCOUNT}:role/service-role/pull"),
                )
            }),
            attached: false,
        },
        Row {
            case: "another account's assumed-role session",
            document: Box::new(|arn, _| {
                sharing_with(
                    arn,
                    &format!("arn:aws:sts::{ANOTHER_ACCOUNT}:assumed-role/pull/session"),
                )
            }),
            attached: false,
        },
        Row {
            case: "a role of this account that IAM does not hold",
            document: Box::new(|arn, _| {
                sharing_with(arn, &format!("arn:aws:iam::{ACCOUNT_ID}:role/{UNHELD_ROLE}"))
            }),
            attached: false,
        },
        Row {
            case: "this account's root beside another account's, as a list",
            document: Box::new(|arn, _| {
                sharing_with_all(
                    arn,
                    &[
                        &granted_principal(),
                        &format!("arn:aws:iam::{ANOTHER_ACCOUNT}:root"),
                    ],
                )
            }),
            attached: false,
        },
        Row {
            case: "another account's root, named by the second statement",
            document: Box::new(|arn, _| {
                second_statement_naming(
                    arn,
                    "Principal",
                    &format!("arn:aws:iam::{ANOTHER_ACCOUNT}:root"),
                )
            }),
            attached: false,
        },
        Row {
            case: "another account's root, named by the complement element",
            document: Box::new(|arn, _| {
                second_statement_naming(
                    arn,
                    "NotPrincipal",
                    &format!("arn:aws:iam::{ANOTHER_ACCOUNT}:root"),
                )
            }),
            attached: false,
        },
    ]
}

/// Assert `arn` reports `expected` as its policy document, which is what says a refused write
/// stored nothing.
async fn assert_carries(router: &MultiServiceRouter, case: &str, arn: &str, expected: &str) {
    let read = get_resource_policy(router, arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected HTTP 200 from the read, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        reported_document(&read),
        expected,
        "case {case:?}: expected the stored document {expected:?}, got {:?}",
        reported_document(&read)
    );
}

/// Assert `answer` is the refusal a document naming an identity that does not exist earns.
fn assert_refused_on_its_principals(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 from the write, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidArgument",
        "case {case:?}: expected InvalidArgument, got {}",
        answer.raw
    );
    assert_eq!(
        message(answer),
        RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE,
        "case {case:?}: expected the message {RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE:?}, got \
         {:?}",
        message(answer)
    );
}

/// A document every one of whose principals resolves is attached and reads back; one naming an
/// identity that does not exist is refused on its principals and attaches nothing.
#[tokio::test]
async fn a_policy_is_attached_only_for_identities_that_exist() {
    let router = emulator_with_iam();
    let role = created_role(&router, HELD_ROLE).await;

    for (index, row) in rows().into_iter().enumerate() {
        // One VPC origin per row, so what a row attaches cannot change what the next reads.
        let arn = created_vpc_origin_arn(&router, &format!("vo-principal-{index}")).await;
        let document = (row.document)(&arn, &role);
        let answer = put_resource_policy(&router, &arn, &document).await;

        if row.attached {
            assert_eq!(
                answer.status, 200,
                "case {:?}: expected HTTP 200 from the write, got {} with {}",
                row.case, answer.status, answer.raw
            );
            assert_carries(&router, row.case, &arn, &stored(&document)).await;
        } else {
            assert_refused_on_its_principals(&answer, row.case);
            // The refusal stores nothing: the resource still reports the empty document.
            assert_carries(&router, row.case, &arn, EMPTY_RESOURCE_POLICY_DOCUMENT).await;
        }
    }
}

/// The refusal is about the PRINCIPALS and nothing else: the same resource takes the same
/// document once its principal is one that exists, and a write that replaces an attached
/// policy with a document naming an identity that does not exist leaves the attached one
/// standing.
#[tokio::test]
async fn a_refusal_on_the_principals_leaves_the_attached_policy_standing() {
    let router = emulator_with_iam();
    let arn = created_vpc_origin_arn(&router, "vo-principal-superseded").await;

    let attached = policy(&arn, "share");
    let written = put_resource_policy(&router, &arn, &attached).await;
    assert_eq!(
        written.status, 200,
        "the baseline write must be accepted, got {} with {}",
        written.status, written.raw
    );

    for (case, principal) in [
        ("another account's root", format!("arn:aws:iam::{ANOTHER_ACCOUNT}:root")),
        ("another account by its bare id", ANOTHER_ACCOUNT.to_string()),
        (
            "a role of this account that IAM does not hold",
            format!("arn:aws:iam::{ACCOUNT_ID}:role/{UNHELD_ROLE}"),
        ),
    ] {
        let refused = put_resource_policy(&router, &arn, &sharing_with(&arn, &principal)).await;
        assert_refused_on_its_principals(&refused, case);
        assert_carries(&router, case, &arn, &stored(&attached)).await;
    }

    // And the same statement, granting to an identity that exists, replaces it.
    let accepted = sharing_with(&arn, &granted_principal());
    let answer = put_resource_policy(&router, &arn, &accepted).await;
    assert_eq!(
        answer.status, 200,
        "the write naming an identity that exists must be accepted, got {} with {}",
        answer.status, answer.raw
    );
    assert_carries(
        &router,
        "the accepted replacement",
        &arn,
        &stored(&accepted),
    )
    .await;
}
