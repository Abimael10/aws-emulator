// SPDX-License-Identifier: CC0-1.0

//! How the `AccessDenied` message of a refused IAM CREATE names the resource the request
//! was made on, end to end over the awsQuery wire.
//!
//! The rule under test: a denial reports "the resource the request was made on", and for a
//! call that CREATES an entity that is the entity being created — which does not exist yet,
//! so the only name the refusal can carry for it is the ARN it WOULD have, the very value
//! the decision is made against. A call on an entity the account ALREADY HOLDS keeps AWS's
//! other spelling, the entity's type followed by the name the request carried, and a call
//! that addresses a resource by ARN keeps the ARN the request supplied.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: which
//! create is refused and therefore which resource type the ARN names, whether the entity
//! lives at the default path, at one the request supplied, at IAM's own reserved one or
//! under no path at all, and whether the entity is named by a request member or by a
//! document the request uploads — and, for the contrast rows, whether the request
//! addresses a held entity, a resource named by ARN or a whole collection.
//!
//! `tests/all/access_denied_message_wording.rs` is the broader wording table and
//! `tests/all/refusal_message_points_at_the_authorization_detail.rs` pins where the message
//! ENDS; this suite pins the RESOURCE a create's denial names. The awsQuery harness and the
//! fixture principal are the ones `tests/all/user_lifecycle.rs` and
//! `tests/all/identity_policy_enforcement.rs` own.

use crate::identity_policy_enforcement::principal;
use crate::role_lifecycle::borrowed;
use crate::user_lifecycle::{
    account_id, assert_access_denied_message, call, call_signed_by, element, error_code, router,
};

/// A trust policy a `CreateRole` row carries: enough of a document to reach the
/// authorization gate, which decides before anything about the document is resolved.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// A policy document a `CreatePolicy` row carries, for the same reason.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// One row: the awsQuery call a zero-permission caller makes, the IAM action its decision
/// is made on, and the resource the refusal must name.
struct Row {
    case: &'static str,
    action: &'static str,
    params: Vec<(String, String)>,
    iam_action: &'static str,
    named_as: String,
}

/// The ARN of an IAM entity of `resource_type` at `path` named `name`.
fn entity_arn(resource_type: &str, path: &str, name: &str) -> String {
    format!(
        "arn:aws:iam::{}:{resource_type}{path}{name}",
        account_id()
    )
}

/// The rows whose resource is an entity the create would mint, one per entity type a
/// create of this service mints by name.
fn create_rows() -> Vec<Row> {
    let saml_metadata = crate::saml_providers::metadata_document(
        crate::saml_providers::MetadataForm::Entity,
        "https://idp.example.com/denied",
        None,
    );
    // Real key material, so the upload row reaches the gate rather than being refused for
    // its PEM (the generator is `tests/all/server_certificates.rs`'s).
    let certificate = crate::server_certificates::material(
        &rcgen::PKCS_ECDSA_P256_SHA256,
        (2020, 2035),
        "denied.example.com",
    );
    vec![
        Row {
            case: "a user at the default path",
            action: "CreateUser",
            params: vec![("UserName".to_string(), "denied-user".to_string())],
            iam_action: "iam:CreateUser",
            named_as: entity_arn("user", "/", "denied-user"),
        },
        Row {
            case: "a user at a supplied path",
            action: "CreateUser",
            params: vec![
                ("UserName".to_string(), "denied-eng-user".to_string()),
                ("Path".to_string(), "/eng/".to_string()),
            ],
            iam_action: "iam:CreateUser",
            named_as: entity_arn("user", "/eng/", "denied-eng-user"),
        },
        Row {
            case: "a role",
            action: "CreateRole",
            params: vec![
                ("RoleName".to_string(), "denied-role".to_string()),
                (
                    "AssumeRolePolicyDocument".to_string(),
                    TRUST_POLICY.to_string(),
                ),
            ],
            iam_action: "iam:CreateRole",
            named_as: entity_arn("role", "/", "denied-role"),
        },
        Row {
            case: "a group at a supplied path",
            action: "CreateGroup",
            params: vec![
                ("GroupName".to_string(), "denied-group".to_string()),
                ("Path".to_string(), "/eng/".to_string()),
            ],
            iam_action: "iam:CreateGroup",
            named_as: entity_arn("group", "/eng/", "denied-group"),
        },
        Row {
            case: "an instance profile",
            action: "CreateInstanceProfile",
            params: vec![(
                "InstanceProfileName".to_string(),
                "denied-profile".to_string(),
            )],
            iam_action: "iam:CreateInstanceProfile",
            named_as: entity_arn("instance-profile", "/", "denied-profile"),
        },
        Row {
            case: "a virtual MFA device",
            action: "CreateVirtualMFADevice",
            params: vec![(
                "VirtualMFADeviceName".to_string(),
                "denied-device".to_string(),
            )],
            iam_action: "iam:CreateVirtualMFADevice",
            named_as: entity_arn("mfa", "/", "denied-device"),
        },
        // A create whose path is IAM's own reserved one and whose name is the service's,
        // neither of them anything the request spelled: the refusal still names the ARN
        // the role would carry.
        Row {
            case: "a service-linked role",
            action: "CreateServiceLinkedRole",
            params: vec![(
                "AWSServiceName".to_string(),
                "autoscaling.amazonaws.com".to_string(),
            )],
            iam_action: "iam:CreateServiceLinkedRole",
            named_as: entity_arn(
                "role",
                "/aws-service-role/autoscaling.amazonaws.com/",
                "AWSServiceRoleForAutoScaling",
            ),
        },
        // A create whose entity is named by a document the request uploads rather than by a
        // path: its ARN carries no path at all.
        Row {
            case: "a SAML identity provider",
            action: "CreateSAMLProvider",
            params: vec![
                ("Name".to_string(), "denied-provider".to_string()),
                ("SAMLMetadataDocument".to_string(), saml_metadata),
            ],
            iam_action: "iam:CreateSAMLProvider",
            named_as: format!(
                "arn:aws:iam::{}:saml-provider/denied-provider",
                account_id()
            ),
        },
        // The upload that mints an entity, which is a create under another name.
        Row {
            case: "a server certificate",
            action: "UploadServerCertificate",
            params: vec![
                (
                    "ServerCertificateName".to_string(),
                    "denied-certificate".to_string(),
                ),
                ("CertificateBody".to_string(), certificate.certificate),
                ("PrivateKey".to_string(), certificate.private_key),
            ],
            iam_action: "iam:UploadServerCertificate",
            named_as: entity_arn("server-certificate", "/", "denied-certificate"),
        },
    ]
}

/// The contrast rows: calls whose resource is NOT an entity being created, which keep the
/// spelling AWS words them with.
fn contrast_rows() -> Vec<Row> {
    vec![
        // A create addressing a resource by ARN: AWS words a managed policy as its type
        // followed by the ARN, which is the ARN the create would mint.
        Row {
            case: "a managed policy, named by its ARN",
            action: "CreatePolicy",
            params: vec![
                ("PolicyName".to_string(), "denied-policy".to_string()),
                ("PolicyDocument".to_string(), POLICY_DOCUMENT.to_string()),
            ],
            iam_action: "iam:CreatePolicy",
            named_as: format!("policy {}", entity_arn("policy", "/", "denied-policy")),
        },
        // A call on an entity the account HOLDS: the caller itself, named by type and name.
        Row {
            case: "a key minted for a held user",
            action: "CreateAccessKey",
            params: vec![("UserName".to_string(), "held-user".to_string())],
            iam_action: "iam:CreateAccessKey",
            named_as: "user held-user".to_string(),
        },
        // A read of a whole collection, named by the collection's own ARN.
        Row {
            case: "a user listing",
            action: "ListUsers",
            params: Vec::new(),
            iam_action: "iam:ListUsers",
            named_as: entity_arn("user", "/", ""),
        },
    ]
}

/// The clause closing the sentence of a call no statement in force allows.
fn no_allow(iam_action: &str) -> String {
    format!("because no identity-based policy allows the {iam_action} action")
}

/// Drive `row` as a zero-permission caller and assert the whole refusal: HTTP 403, the
/// `AccessDenied` code, and the sentence naming `row.named_as` as the resource.
async fn assert_refusal(row: &Row, index: usize) {
    let router = router();
    // `CreateAccessKey`'s row addresses a user the account holds, created here with the
    // account's own credentials so the row's refusal is about the key and not about it.
    let (status, created) = call(&router, "CreateUser", &[("UserName", "held-user")]).await;
    assert_eq!(
        status, 200,
        "case {:?}: the fixture CreateUser must succeed, got {status}: {created}",
        row.case
    );

    let user_name = format!("nopriv-create-{index}");
    // No identity-based policy at all: every call below is IAM's implicit deny.
    let access_key_id = principal(&router, row.case, &user_name, &[]).await;

    let (status, body) = call_signed_by(
        &router,
        Some(&access_key_id),
        row.action,
        &borrowed(&row.params),
    )
    .await;
    assert_eq!(
        status, 403,
        "case {:?}: expected HTTP 403, got {status}: {body}",
        row.case
    );
    assert_eq!(
        error_code(&body),
        "AccessDenied",
        "case {:?}: expected the error code AccessDenied, got {:?}",
        row.case,
        error_code(&body)
    );
    assert_access_denied_message(
        row.case,
        &element(&body, "Message"),
        &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
        row.iam_action,
        &row.named_as,
        &no_allow(row.iam_action),
    );
}

/// RULE: a refused CREATE names the resource by the ARN the entity it would have minted
/// would carry — whichever entity type, and whether the path came from the request, from
/// the default or from IAM's own reserved one.
#[tokio::test]
async fn a_refused_create_names_the_arn_the_new_entity_would_carry() {
    for (index, row) in create_rows().iter().enumerate() {
        assert_refusal(row, index).await;
    }
}

/// RULE: the same message for a call that is NOT creating an entity by name keeps AWS's
/// own spelling for it — a held entity by type and name, a managed policy by type and ARN,
/// a collection by its ARN — so the create spelling is not applied where it does not hold.
#[tokio::test]
async fn a_refusal_of_another_call_keeps_the_spelling_aws_words_it_with() {
    for (index, row) in contrast_rows().iter().enumerate() {
        assert_refusal(row, index + create_rows().len()).await;
    }
}
