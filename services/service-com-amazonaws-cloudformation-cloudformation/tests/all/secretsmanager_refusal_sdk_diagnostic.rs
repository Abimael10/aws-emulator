// SPDX-License-Identifier: CC0-1.0

//! What a resource of an AWS Secrets Manager type reports when that service REFUSED the call
//! its provider made, end to end over the awsQuery wire.
//!
//! RULE under test: **a resource provider does not word its own diagnosis of a refused call:
//! what CloudFormation quotes in the failed resource's `ResourceStatusReason` is the exception
//! that provider's SDK raised — the owning service's own sentence followed by that SDK's
//! diagnostic of the call, naming the service as its SDK names it, the HTTP status the service
//! answered and the id of the call — closed by the `RequestToken` of the resource request and
//! the `HandlerErrorCode` class the refusal falls in; the stack then rolls back and nothing the
//! refused resource would have written is left behind.**
//!
//! Why it matters: the reason is the only place a client sees WHICH service refused a resource
//! and with what status. Automation routes a stack failure to the service that answered it and
//! quotes the call's id when it asks that service's operators about it; a reason carrying only
//! a sentence is indistinguishable from one the handler itself produced.
//!
//! The rows differ along what the rule generalises over: WHICH refusal of the owning service
//! the provider ran into — a document granting broad access while the declaration asks for
//! those to be blocked, a document that is not a policy at all, and a secret that service does
//! not hold — and so which error code answered it and which `HandlerErrorCode` class it falls
//! in. The sentences are the owning service's own constants, read from that crate rather than
//! copied here. The boundary of the rule is the last test: the same declaration with a document
//! that service ACCEPTS carries no diagnostic at all, because nothing was refused.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use service_com_amazonaws_secretsmanager_secretsmanager::state::{
    MALFORMED_POLICY_MESSAGE, PUBLIC_POLICY_MESSAGE, SECRET_NOT_FOUND_MESSAGE,
};

use crate::refused_resource_sdk_exception::parts;
use crate::stack_lifecycle::{
    create_stack, resource_event_reason, router, settled_stack, xml_text, ACCOUNT_ID,
};
use crate::stack_owned_secret_teardown::{poll_to_terminal, secretsmanager};

/// The name AWS Secrets Manager's own SDK reports that service under, which is its `sdkId`
/// (`Secrets Manager`) written as one word — and the `cloudFormationName` the type names of
/// its resources carry.
const SDK_SERVICE: &str = "Service: SecretsManager";

/// The logical id every row declares its policy under.
const POLICY: &str = "CredentialPolicy";

/// The value the secret a row attaches its policy to holds, so the secret is a real one the
/// attach could have succeeded against.
const SECRET_VALUE: &str = "a-stack-attached-credential";

/// The `ClientRequestToken` the seeded secret's first version is staked under: Secrets
/// Manager's `ClientRequestTokenType` is `@length` 32..64, so a row's own name is padded out
/// to that minimum.
fn version_token(secret: &str) -> String {
    format!("{secret:0<32}")
}

/// One row: the declaration the owning service refuses, and the exception it answers with.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The stack the row deploys, which also names the secret it seeds so no two rows collide.
    stack: &'static str,
    /// Whether the row SEEDS the secret its declaration addresses: the row about a secret the
    /// service does not hold seeds none.
    seeds_the_secret: bool,
    /// Whether the declaration asks for policies granting public access to be blocked.
    blocks_public_policies: bool,
    /// Whether the declared document is one that grants access to every principal.
    public_document: bool,
    /// Whether the declared document is not a policy document at all.
    malformed_document: bool,
    /// The error code the owning service refuses the attach with.
    expected_code: &'static str,
    /// The HTTP status it answers that refusal with.
    expected_status: u16,
    /// The `HandlerErrorCode` class the refusal falls in.
    expected_class: &'static str,
}

/// The `HandlerErrorCode` a refusal of the REQUEST the declaration asked for falls in, which
/// is what every validation refusal of an owning service is.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The class a refusal that names a resource the owning service does not hold falls in.
const NOT_FOUND: &str = "NotFound";

const ROWS: &[Row] = &[
    Row {
        // The recorded case: a wildcard-principal `Allow` under `BlockPublicPolicy`.
        case: "a document granting broad access, with public policies blocked",
        stack: "sm-diagnostic-public-policy",
        seeds_the_secret: true,
        blocks_public_policies: true,
        public_document: true,
        malformed_document: false,
        expected_code: "PublicPolicyException",
        expected_status: 400,
        expected_class: INVALID_REQUEST,
    },
    Row {
        // Another refusal of the same call, with another code: a document that is not a
        // policy document, which is refused whether or not public policies are blocked.
        case: "a document that is not a policy document",
        stack: "sm-diagnostic-malformed-policy",
        seeds_the_secret: true,
        blocks_public_policies: false,
        public_document: false,
        malformed_document: true,
        expected_code: "MalformedPolicyDocumentException",
        expected_status: 400,
        expected_class: INVALID_REQUEST,
    },
    Row {
        // A refusal of another class entirely: the secret the declaration names is not one
        // that service holds, so the attach has nothing to act on.
        case: "a secret the owning service does not hold",
        stack: "sm-diagnostic-absent-secret",
        seeds_the_secret: false,
        blocks_public_policies: false,
        public_document: false,
        malformed_document: false,
        expected_code: "ResourceNotFoundException",
        expected_status: 400,
        expected_class: NOT_FOUND,
    },
];

/// The sentence the owning service words for a row's refusal — that service's own constant,
/// read from the crate that owns it rather than copied here.
fn sentence(row: &Row) -> &'static str {
    if row.public_document {
        return PUBLIC_POLICY_MESSAGE;
    }
    if row.malformed_document {
        return MALFORMED_POLICY_MESSAGE;
    }
    SECRET_NOT_FOUND_MESSAGE
}

/// The document a row declares.
fn document(row: &Row) -> Value {
    if row.malformed_document {
        // A statement carrying no `Effect` at all, which is not a policy document.
        return json!({"Version": "2012-10-17", "Statement": [{"Principal": "*"}]});
    }
    if row.public_document {
        return json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "ReadableByEveryPrincipal",
                "Effect": "Allow",
                "Principal": "*",
                "Action": "secretsmanager:GetSecretValue",
                "Resource": "*"
            }]
        });
    }
    account_scoped_document()
}

/// A document the owning service ACCEPTS: an `Allow` to this account's own root, which grants
/// no broad access.
fn account_scoped_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "ReadableByThisAccount",
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT_ID}:root")},
            "Action": "secretsmanager:GetSecretValue",
            "Resource": "*"
        }]
    })
}

/// A template declaring ONE `AWS::SecretsManager::ResourcePolicy` over `secret`, so the
/// resource the stack fails is unambiguous.
fn template(secret: &str, declared: Value, blocks_public_policies: bool) -> String {
    let mut properties = serde_json::Map::new();
    properties.insert("SecretId".to_string(), Value::String(secret.to_string()));
    properties.insert("ResourcePolicy".to_string(), declared);
    if blocks_public_policies {
        properties.insert("BlockPublicPolicy".to_string(), Value::Bool(true));
    }
    json!({
        "Resources": {
            POLICY: {
                "Type": "AWS::SecretsManager::ResourcePolicy",
                "Properties": Value::Object(properties),
            }
        }
    })
    .to_string()
}

/// Create one secret through Secrets Manager's OWN wire, so the attach the stack makes
/// addresses a secret that service really holds.
///
/// `pub(crate)`: a sibling suite that holds the OTHER resource types of this service to the
/// same diagnostic seeds the secret they address the same way, so it reads this call rather
/// than carrying a second copy of it
/// ([`crate::secretsmanager_type_refusal_diagnostics`]).
pub(crate) async fn seed_a_secret(router: &MultiServiceRouter, case: &str, secret: &str) {
    let (status_code, answered) = secretsmanager(
        router,
        "CreateSecret",
        &json!({
            "Name": secret,
            "SecretString": SECRET_VALUE,
            "ClientRequestToken": version_token(secret)
        }),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateSecret({secret}) must answer 200, got {status_code} {answered}"
    );
}

/// The policy the owning service reports for `secret`, or `None` where it carries none — and
/// `None` too for a secret that service does not hold at all.
async fn attached_policy(
    router: &MultiServiceRouter,
    case: &str,
    secret: &str,
) -> Option<Value> {
    let (status_code, answered) =
        secretsmanager(router, "GetResourcePolicy", &json!({"SecretId": secret})).await;
    if status_code != 200 {
        return None;
    }
    let reported = answered.get("ResourcePolicy")?.as_str()?;
    Some(serde_json::from_str(reported).unwrap_or_else(|error| {
        panic!("case {case:?}: the reported policy is not JSON: {reported:?} ({error})")
    }))
}

/// Hold one quoted exception to the diagnostic AWS Secrets Manager's SDK words: its own
/// sentence, then the service as its SDK names it, the status that service answered and the id
/// of the call — each present, and each of the FORM the call answers rather than one recorded
/// value.
///
/// `pub(crate)`: the diagnostic is the SERVICE's, not one type's, so the sibling suite that
/// holds the other resource types of this service to it reads THIS assertion rather than
/// carrying a second copy of it ([`crate::secretsmanager_type_refusal_diagnostics`]).
pub(crate) fn assert_diagnostic(
    case: &str,
    quoted: &str,
    expected_sentence: &str,
    expected_status: u16,
) {
    let diagnostic = quoted
        .strip_prefix(expected_sentence)
        .map(str::trim_start)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the quoted text to open with the service's own \
                 sentence {expected_sentence:?}, got {quoted:?}"
            )
        });
    let envelope = diagnostic
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the SDK diagnostic, got {quoted:?}")
        });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id, got {envelope:?}"
    );
    assert_eq!(
        fields[0], SDK_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {expected_status}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    let request_id = fields[2].strip_prefix("Request ID: ").unwrap_or_else(|| {
        panic!("case {case:?}: expected a Request ID, got {:?}", fields[2])
    });
    assert!(
        !request_id.is_empty()
            && request_id
                .chars()
                .all(|held| held.is_ascii_hexdigit() || held == '-'),
        "case {case:?}: expected the id of the call, got {request_id:?}"
    );
}

#[tokio::test]
async fn a_refused_secretsmanager_resource_reports_its_sdk_exception() {
    for row in ROWS {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();
        if row.seeds_the_secret {
            seed_a_secret(&router, case, row.stack).await;
        }

        let (status_code, body) = create_stack(
            &router,
            row.stack,
            &template(row.stack, document(row), row.blocks_public_policies),
            &[],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreateStack must answer 200, got {status_code} {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the refused attach to roll the stack back, got {described}"
        );

        let reason = resource_event_reason(
            &router,
            row.stack,
            POLICY,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, class) = parts(case, &reason);
        assert_diagnostic(case, &quoted, sentence(row), row.expected_status);
        assert_eq!(
            class, row.expected_class,
            "case {case:?}: expected the {} class for {}, got {class:?} in {reason:?}",
            row.expected_class, row.expected_code
        );

        // The refusal was real: a seeded secret carries no policy at all afterwards.
        if row.seeds_the_secret {
            let attached = attached_policy(&router, case, row.stack).await;
            assert_eq!(
                attached, None,
                "case {case:?}: expected the refused attach to leave no policy on the secret, \
                 got {attached:?}"
            );
        }
    }
}

/// The boundary of the rule: the same declaration with a document the owning service ACCEPTS
/// carries no diagnostic at all, because nothing was refused — the stack completes and the
/// document is attached.
#[tokio::test]
async fn an_accepted_secretsmanager_resource_reports_no_exception() {
    let case = "a document the owning service accepts, with public policies blocked";
    let secret = "sm-diagnostic-accepted-policy";
    let router = router();
    seed_a_secret(&router, case, secret).await;

    let (status_code, body) = create_stack(
        &router,
        secret,
        &template(secret, account_scoped_document(), true),
        &[],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {body}"
    );
    let described = poll_to_terminal(&router, case, secret).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the accepted attach to complete, got {described}"
    );

    let reason =
        resource_event_reason(&router, secret, POLICY, resource_status::CREATE_FAILED).await;
    assert!(
        reason.is_empty(),
        "case {case:?}: expected no refused resource, got {reason:?}"
    );
    let attached = attached_policy(&router, case, secret).await;
    assert_eq!(
        attached.as_ref(),
        Some(&account_scoped_document()),
        "case {case:?}: expected the declared document attached to the secret, got {attached:?}"
    );
}
