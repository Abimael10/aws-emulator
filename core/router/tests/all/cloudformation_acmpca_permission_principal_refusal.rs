// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack does with an `AWS::ACMPCA::Permission` whose declared
//! `Principal` Amazon Web Services Private CA does not grant to.
//!
//! The rule under test: **a permission on a private CA can only be granted to a service
//! principal that service supports — "at this time, the only valid principal is
//! `acm.amazonaws.com`" (`CreatePermissionRequest$Principal`) — so a declaration naming any
//! other principal is refused by the owning service before anything is granted, the resource
//! FAILS with the exception that provider's SDK raised quoted in its `ResourceStatusReason`,
//! the deployment does not complete, and the certificate authority the stack created holds no
//! grant at all.**
//!
//! Why it matters: a client that relies on this guard would otherwise read a completed
//! deployment while holding a CA whose resource policy grants a principal AWS would never
//! have accepted — infrastructure that is invalid in AWS, modelled here as valid — and every
//! negative test of the guard would pass vacuously.
//!
//! Two crates have to agree for the rule to hold (CloudFormation makes the call and reports
//! the failure; Amazon Web Services Private CA judges the grant), so the suite lives here
//! rather than in either one.
//!
//! The rows differ along what the rule generalises over: HOW the declared principal fails to
//! be a supported one (another service's principal, the supported name with text ahead of it,
//! the supported name in another case, an identity ARN), and the form the template is written
//! in. The control row is the same stack declaring the one principal that service DOES grant
//! to: it completes, and the grant is really there.
//!
//! Every expected value is computed here from what the row declares, except the CA's ARN,
//! which that service mints: it is read off the stack's own record of the CA.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, form_encode, members_of, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The `ResourceStatus` the refused resource reaches.
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The one principal a permission can be granted to
/// (`CreatePermissionRequest$Principal`: "at this time, the only valid principal is
/// acm.amazonaws.com").
const SUPPORTED_PRINCIPAL: &str = "acm.amazonaws.com";

/// The sentence Amazon Web Services Private CA refuses a grant to any other principal with:
/// entirely fixed text, with nothing of the request in it.
const UNSUPPORTED_PRINCIPAL_SENTENCE: &str = "The principal specified is not supported.";

/// The SDK's own name for that service, as the exception its SDK raises states it.
const SDK_SERVICE: &str = "AWSACMPCA";

/// The error code and HTTP status that service answers such a refusal with.
const VALIDATION_EXCEPTION: &str = "ValidationException";
const REFUSED_REQUEST_STATUS: u16 = 400;

/// The `HandlerErrorCode` class CloudFormation reports the refusal under — one of its model's
/// own `HandlerErrorCode` values.
const EXPECTED_CLASS: &str = "GeneralServiceException";

/// The logical ids the templates below use.
const CA_LOGICAL_ID: &str = "Ca";
const PERMISSION_LOGICAL_ID: &str = "CaPermission";

/// The three actions a grant has to name (`ActionType`).
const EVERY_ACTION: [&str; 3] = ["IssueCertificate", "GetCertificate", "ListPermissions"];

/// How long a wait for the deployment to settle, or for its event to be recorded, is given.
///
/// Generous on purpose: what is waited FOR is the stack reaching a terminal status, which is
/// on its own clock, and every wait below ends as soon as the condition holds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_acmpca_acmprivateca::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon Web Services Private CA operation over its awsJson1_1 wire.
async fn acm_pca(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm-pca.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ACMPrivateCA.{operation}"))
        .header("authorization", authorization("acm-pca"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The template of one row: a root CA and a permission over it granting `principal` every
/// action, written as JSON.
fn json_template(principal: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            CA_LOGICAL_ID: {
                "Type": "AWS::ACMPCA::CertificateAuthority",
                "Properties": {
                    "Type": "ROOT",
                    "KeyAlgorithm": "RSA_2048",
                    "SigningAlgorithm": "SHA256WITHRSA",
                    "Subject": { "CommonName": "root.acme-widgets.internal" },
                    "UsageMode": "SHORT_LIVED_CERTIFICATE"
                }
            },
            PERMISSION_LOGICAL_ID: {
                "Type": "AWS::ACMPCA::Permission",
                "Properties": {
                    "CertificateAuthorityArn": { "Ref": CA_LOGICAL_ID },
                    "Principal": principal,
                    "Actions": EVERY_ACTION
                }
            }
        }
    })
    .to_string()
}

/// The same stack written as YAML, so the rule is not about the template's form.
fn yaml_template(principal: &str) -> String {
    [
        "Resources:".to_string(),
        format!("  {CA_LOGICAL_ID}:"),
        "    Type: AWS::ACMPCA::CertificateAuthority".to_string(),
        "    Properties:".to_string(),
        "      Type: ROOT".to_string(),
        "      KeyAlgorithm: RSA_2048".to_string(),
        "      SigningAlgorithm: SHA256WITHRSA".to_string(),
        "      Subject:".to_string(),
        "        CommonName: root.acme-widgets.internal".to_string(),
        format!("  {PERMISSION_LOGICAL_ID}:"),
        "    Type: AWS::ACMPCA::Permission".to_string(),
        "    Properties:".to_string(),
        format!("      CertificateAuthorityArn: !Ref {CA_LOGICAL_ID}"),
        format!("      Principal: '{principal}'"),
        "      Actions:".to_string(),
        format!("        - {}", EVERY_ACTION[0]),
        format!("        - {}", EVERY_ACTION[1]),
        format!("        - {}", EVERY_ACTION[2]),
        String::new(),
    ]
    .join("\n")
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in with the body that reported it.
async fn settled_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (String, String) {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return (reached, body);
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceStatusReason` of the event `logical_id` reached `reported` in, or `None` where
/// the stack has recorded no such event.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together, so
/// a reason is never read off another resource's — or another transition's — event.
async fn recorded_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Option<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    members_of(&body).into_iter().find_map(|member| {
        let named = elements(member, "LogicalResourceId")
            .into_iter()
            .next()
            .is_some_and(|named| named == logical_id);
        let failed = elements(member, "ResourceStatus")
            .into_iter()
            .next()
            .is_some_and(|status| status == reported);
        (named && failed)
            .then(|| elements(member, "ResourceStatusReason").into_iter().next())
            .flatten()
            .filter(|reason| !reason.is_empty())
    })
}

/// The same reason, looked for again until the stack has recorded it or [`SETTLE_DEADLINE`]
/// passes.
async fn failure_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        if let Some(reason) =
            recorded_reason(router, case, stack, logical_id, RESOURCE_CREATE_FAILED).await
        {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} reported no {RESOURCE_CREATE_FAILED} reason within \
             {SETTLE_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The physical id of every resource the stack recorded, by logical id.
async fn physical_ids(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    physical_ids_of(&body)
}

/// Every grant Amazon Web Services Private CA holds on `ca_arn`, keyed by the principal that
/// holds it.
async fn granted_permissions(
    router: &MultiServiceRouter,
    case: &str,
    ca_arn: &str,
) -> BTreeMap<String, Value> {
    let (status, answered) = acm_pca(
        router,
        "ListPermissions",
        &json!({ "CertificateAuthorityArn": ca_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListPermissions to answer for {ca_arn:?}, got {status} \
         {answered:?}"
    );
    answered
        .get("Permissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|permission| {
            let principal = permission.get("Principal")?.as_str()?.to_string();
            Some((principal, permission))
        })
        .collect()
}

/// The provider exception a `ResourceStatusReason` quotes, and the `HandlerErrorCode` class it
/// reports beside it.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one quoted exception to the shape that service's SDK raises: its own sentence, then
/// the diagnostic of the refused call — the SDK's name for the service, the HTTP status it
/// answered, the error code it answered, the id of the call and the proxy the call was made
/// through. Answers the sentence.
fn asserted_sentence(case: &str, quoted: &str) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    let (sentence, diagnostic) = quoted.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, after the \
             service's own sentence, got {quoted:?}"
        )
    });
    let envelope = diagnostic.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the diagnostic to close, got {quoted:?}")
    });
    let fields: Vec<&str> = envelope.split("; ").collect();
    assert_eq!(
        fields.len(),
        5,
        "case {case:?}: expected the service, the status, the code, the call's id and the proxy, \
         got {envelope:?}"
    );
    assert_eq!(
        fields[0], SDK_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {REFUSED_REQUEST_STATUS}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    assert_eq!(
        fields[2],
        format!("Error Code: {VALIDATION_EXCEPTION}"),
        "case {case:?}: expected the code the owning service answered, got {:?}",
        fields[2]
    );
    let request_id = fields[3]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[3]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
    );
    assert_eq!(
        fields[4], "Proxy: null",
        "case {case:?}: expected the proxy the call was made through, got {:?}",
        fields[4]
    );
    sentence.to_string()
}

/// Whether `value` has the 8-4-4-4-12 lowercase-hexadecimal form an AWS request id carries.
fn is_a_request_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .chars()
                .all(|held| held.is_ascii_hexdigit() && !held.is_ascii_uppercase())
        })
}

/// One row: how the declared principal misses, the principal itself, and the form the template
/// is written in.
struct Row {
    case: &'static str,
    principal: &'static str,
    template: fn(&str) -> String,
}

/// The rows differ along HOW the declared principal fails to be one this service grants to,
/// and along the form the template is written in. Each value is built here from the supported
/// principal rather than quoted from any one run.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the supported principal with text ahead of it, JSON",
            principal: "not-acm.amazonaws.com",
            template: json_template,
        },
        Row {
            case: "another service's principal, JSON",
            principal: "ram.amazonaws.com",
            template: json_template,
        },
        Row {
            case: "the supported principal in another case, YAML",
            principal: "ACM.amazonaws.com",
            template: yaml_template,
        },
        Row {
            case: "an identity ARN rather than a service principal, YAML",
            principal: "arn:aws:iam::000000000000:root",
            template: yaml_template,
        },
    ]
}

/// Every row: the deployment FAILS on the permission, the failed resource quotes the owning
/// service's refusal, and the CA the stack created holds no grant.
#[tokio::test]
async fn a_declared_permission_to_an_unsupported_principal_fails_the_stack_and_grants_nothing() {
    let router = build_router();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let stack_name = format!("acmpca-permission-principal-{index}");
        let template = (row.template)(row.principal);
        assert_ne!(
            row.principal, SUPPORTED_PRINCIPAL,
            "case {case:?}: the row must declare a principal the service does not grant to"
        );

        // `DisableRollback` keeps the failed state as it failed, so the resource's own failure
        // is what the stack reports rather than a rollback over it.
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template),
                ("DisableRollback", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (settled, described) = settled_status(&router, case, &stack_name).await;
        assert_eq!(
            settled, CREATE_FAILED,
            "case {case:?}: expected the deployment to fail, got {settled:?} ({described})"
        );

        let reason = failure_reason(&router, case, &stack_name, PERMISSION_LOGICAL_ID).await;
        let (quoted, class) = quoted_and_class(case, &reason);
        let sentence = asserted_sentence(case, &quoted);
        assert_eq!(
            sentence, UNSUPPORTED_PRINCIPAL_SENTENCE,
            "case {case:?}: expected the owning service's own sentence, got {sentence:?}"
        );
        assert_eq!(
            class, EXPECTED_CLASS,
            "case {case:?}: expected the refusal reported as {EXPECTED_CLASS}, got {class:?}"
        );

        // Nothing was granted: the CA the stack created holds no permission at all.
        let recorded = physical_ids(&router, case, &stack_name).await;
        let ca_arn = recorded.get(CA_LOGICAL_ID).cloned().unwrap_or_default();
        assert!(
            ca_arn.starts_with(&format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/"
            )),
            "case {case:?}: expected the CA to report the ARN that service minted, got \
             {ca_arn:?} ({recorded:?})"
        );
        let grants = granted_permissions(&router, case, &ca_arn).await;
        assert!(
            grants.is_empty(),
            "case {case:?}: a refused declaration must grant nothing, got {grants:?}"
        );
    }
}

/// The control: the same stack declaring the one principal that service DOES grant to
/// completes, and the grant is really held on the CA it was declared over.
#[tokio::test]
async fn a_declared_permission_to_the_supported_principal_completes_and_grants_it() {
    let router = build_router();
    let case = "the supported principal";
    let stack_name = "acmpca-permission-principal-supported";
    let template = json_template(SUPPORTED_PRINCIPAL);

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (settled, described) = settled_status(&router, case, stack_name).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the deployment to complete, got {settled:?} ({described})"
    );
    assert!(
        recorded_reason(
            &router,
            case,
            stack_name,
            PERMISSION_LOGICAL_ID,
            RESOURCE_CREATE_FAILED
        )
        .await
        .is_none(),
        "case {case:?}: nothing refuses a supported principal, so the resource records no \
         failure"
    );

    let recorded = physical_ids(&router, case, stack_name).await;
    let ca_arn = recorded.get(CA_LOGICAL_ID).cloned().unwrap_or_default();
    let grants = granted_permissions(&router, case, &ca_arn).await;
    assert_eq!(
        grants.len(),
        1,
        "case {case:?}: expected the one declared grant, got {grants:?}"
    );
    let permission = grants.get(SUPPORTED_PRINCIPAL).unwrap_or_else(|| {
        panic!("case {case:?}: expected a grant to {SUPPORTED_PRINCIPAL:?}, got {grants:?}")
    });
    let mut expected_actions: Vec<&str> = EVERY_ACTION.to_vec();
    expected_actions.sort_unstable();
    assert_eq!(
        permission.get("Actions"),
        Some(&json!(expected_actions)),
        "case {case:?}: expected the declared actions, got {permission:?}"
    );
    assert_eq!(
        permission
            .get("CertificateAuthorityArn")
            .and_then(Value::as_str),
        Some(ca_arn.as_str()),
        "case {case:?}: expected the grant to name the CA it is on, got {permission:?}"
    );
}
