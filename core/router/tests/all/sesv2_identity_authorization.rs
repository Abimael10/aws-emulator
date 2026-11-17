// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of the SESv2 API, over its restJson1 wire.
//!
//! The rule under test: every request to SESv2 is a signed call, and such a call is served
//! only to a caller whose identity-based IAM policies allow THAT operation's own action
//! (`ses:<OperationName>`) on the resource the request ACTS ON — the ARN of the email
//! identity, configuration set, contact list, dedicated IP pool or template its URI names,
//! the ARN of the resource a CREATE's own input names (a create acts on the one resource it
//! is about to make, which its URI does not carry), and IAM's any-resource wildcard for a
//! call that acts on no single resource (an account-level read, a send). Every other caller
//! is refused HTTP 403 under the error code `AccessDeniedException`, with IAM's own sentence,
//! BEFORE the operation reads or writes anything — so a refused caller learns nothing about
//! what the account holds, a refused send delivers nothing and a refused create stores
//! nothing.
//!
//! This is a CROSS-SERVICE rule and only a cross-service suite can pin it: the principals
//! below are made with IAM's and STS's OWN operations over the same engine (`CreateRole` +
//! `PutRolePolicy` + `AssumeRole`, `CreateUser` + `PutUserPolicy` + `CreateAccessKey`), so
//! the documents SESv2 decides on are the ones those services wrote and the principal a
//! refusal blames is the identity STS reports for the credential that signed. It is the same
//! decision a running Lambda handler's own SDK call is subject to, because such a call is
//! signed with a session of the function's execution role.
//!
//! The rows vary what the rule generalises over: the OPERATION asked for (a send, an
//! account-level read, a read of each resource family the URI space names, and the create of
//! each family, which names the resource it makes in its input), the kind of principal (the
//! account's own published credential, an IAM user, an assumed-role session), and what its
//! policies say (nothing about this service, a wildcard `Allow`, an `Allow` for this action
//! on this resource, an `Allow` on another resource of the same kind, and an explicit `Deny`
//! beside a wildcard `Allow`).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for, so the ARNs the decisions are made about are
/// the ones this Region's resources carry.
const REGION: &str = "us-east-1";

/// The access-key id standing for the account's own published credential: neither STS nor IAM
/// holds a record for it, so it resolves to the account's ambient identity, which holds no IAM
/// policies — the position AWS account root credentials are in.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal must
/// blame it by — what `sts:GetCallerIdentity` reports for that key.
#[derive(Debug)]
struct Principal {
    access_key_id: String,
    arn: String,
}

/// A SigV4 `Authorization` header whose credential scope names `service`. The signature is
/// never checked — the router here runs with request authentication off — and the access key
/// id is what resolves the caller's identity.
fn authorization(access_key_id: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// The emulator these rows drive: SESv2 beside the IAM and STS services the principals and
/// their policies are made with, on ONE engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One SESv2 call: `restJson1` under the `ses` credential scope, in SESv2's own `/v2/email/…`
/// path space, signed with `access_key_id`.
async fn sesv2(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Reply {
    let payload = body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(value).expect("the request body serializes")
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://email.{REGION}.amazonaws.com{path}"))
        .header("host", format!("email.{REGION}.amazonaws.com"))
        .header("authorization", authorization(access_key_id, "ses"));
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let payload = response.into_body();
    let body = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_code,
        body,
    }
}

/// One awsQuery call to IAM or STS, signed with the account's own credential, which must
/// succeed: these are the suite's setup legs.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {action:?}: the setup call must succeed, got {status} with {text}"
    );
    text
}

/// Create an IAM role carrying `policy` as its inline policy, assume it through STS, and
/// answer the session's access key id with the session ARN.
async fn assumed_role(router: &MultiServiceRouter, role_name: &str, policy: &str) -> Principal {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    let assumed = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{account}:role/{role_name}"),
            ),
            ("RoleSessionName", "probe".to_string()),
        ],
    )
    .await;
    Principal {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// Create an IAM user carrying `policy` as its inline policy, mint a long-term access key for
/// it, and answer that key id with the user's own ARN.
async fn user(router: &MultiServiceRouter, user_name: &str, policy: &str) -> Principal {
    let created = query(
        router,
        "iam",
        "2010-05-08",
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    let arn = element(&created, "Arn");
    query(
        router,
        "iam",
        "2010-05-08",
        "PutUserPolicy",
        &[
            ("UserName", user_name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    let minted = query(
        router,
        "iam",
        "2010-05-08",
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    Principal {
        access_key_id: element(&minted, "AccessKeyId"),
        arn,
    }
}

/// A policy document allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// A policy document allowing every action of this service on everything, and explicitly
/// denying `denied`.
fn allow_every_action_but(denied: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "ses:*", "Resource": "*" },
            { "Effect": "Deny", "Action": denied, "Resource": "*" },
        ],
    })
    .to_string()
}

/// The IAM action an operation of this service is performed under: `ses` is SESv2's
/// `arnNamespace` and its SigV4 signing name alike.
fn action_of(operation: &str) -> String {
    format!("ses:{operation}")
}

/// The ARN of one resource of this service, as SESv2 mints one: `<type>/<id>` in the resource
/// field.
fn resource_arn(resource_type: &str, id: &str) -> String {
    format!(
        "arn:aws:ses:{REGION}:{}:{resource_type}/{id}",
        engine::emulator_account_id()
    )
}

/// The sentence AWS refuses an identity-based decision with when no readable policy allows the
/// action.
fn no_policy_allows(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The sentence AWS refuses an identity-based decision with when a policy explicitly denies
/// it.
fn explicit_deny(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         with an explicit deny in an identity-based policy"
    )
}

/// The email identity, configuration set, contact list and template the rows below address.
const PROBE: &str = "authz-probe";

/// One operation of this service: the request that drives it, and the resource the decision
/// about it is made on.
struct OperationCase {
    case: &'static str,
    operation: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    resource: String,
}

/// The table: a send, an account-level read, a read of each resource family this model's URI
/// space names, and the create of each of those families — a create acts on the resource it
/// is about to make, which its own input names.
fn operation_cases() -> Vec<OperationCase> {
    vec![
        OperationCase {
            case: "a send, which names no single resource in its URI",
            operation: "SendEmail",
            method: "POST",
            path: "/v2/email/outbound-emails".to_string(),
            body: Some(json!({
                "FromEmailAddress": format!("sender@{PROBE}.example.com"),
                "Destination": { "ToAddresses": ["recipient@example.com"] },
                "Content": { "Simple": {
                    "Subject": { "Data": "probe" },
                    "Body": { "Text": { "Data": "probe" } },
                } },
            })),
            resource: "*".to_string(),
        },
        OperationCase {
            case: "an account-level read",
            operation: "GetAccount",
            method: "GET",
            path: "/v2/email/account".to_string(),
            body: None,
            resource: "*".to_string(),
        },
        OperationCase {
            case: "a read of one email identity",
            operation: "GetEmailIdentity",
            method: "GET",
            path: format!("/v2/email/identities/{PROBE}.example.com"),
            body: None,
            resource: resource_arn("identity", &format!("{PROBE}.example.com")),
        },
        OperationCase {
            case: "a read of one configuration set",
            operation: "GetConfigurationSet",
            method: "GET",
            path: format!("/v2/email/configuration-sets/{PROBE}"),
            body: None,
            resource: resource_arn("configuration-set", PROBE),
        },
        OperationCase {
            case: "a read of one contact list",
            operation: "GetContactList",
            method: "GET",
            path: format!("/v2/email/contact-lists/{PROBE}"),
            body: None,
            resource: resource_arn("contact-list", PROBE),
        },
        OperationCase {
            case: "a read of one email template",
            operation: "GetEmailTemplate",
            method: "GET",
            path: format!("/v2/email/templates/{PROBE}"),
            body: None,
            resource: resource_arn("template", PROBE),
        },
        OperationCase {
            case: "the create of a contact list, which its input names",
            operation: "CreateContactList",
            method: "POST",
            path: "/v2/email/contact-lists".to_string(),
            body: Some(json!({ "ContactListName": format!("{PROBE}-created") })),
            resource: resource_arn("contact-list", &format!("{PROBE}-created")),
        },
        OperationCase {
            case: "the create of an email identity, which its input names",
            operation: "CreateEmailIdentity",
            method: "POST",
            path: "/v2/email/identities".to_string(),
            body: Some(json!({ "EmailIdentity": format!("{PROBE}-created.example.com") })),
            resource: resource_arn("identity", &format!("{PROBE}-created.example.com")),
        },
        OperationCase {
            case: "the create of a configuration set, which its input names",
            operation: "CreateConfigurationSet",
            method: "POST",
            path: "/v2/email/configuration-sets".to_string(),
            body: Some(json!({ "ConfigurationSetName": format!("{PROBE}-created") })),
            resource: resource_arn("configuration-set", &format!("{PROBE}-created")),
        },
        OperationCase {
            case: "the create of a dedicated IP pool, which its input names",
            operation: "CreateDedicatedIpPool",
            method: "POST",
            path: "/v2/email/dedicated-ip-pools".to_string(),
            body: Some(json!({ "PoolName": format!("{PROBE}-created") })),
            resource: resource_arn("dedicated-ip-pool", &format!("{PROBE}-created")),
        },
        OperationCase {
            case: "the create of an email template, which its input names",
            operation: "CreateEmailTemplate",
            method: "POST",
            path: "/v2/email/templates".to_string(),
            body: Some(json!({
                "TemplateName": format!("{PROBE}-created"),
                "TemplateContent": {
                    "Subject": "probe",
                    "Text": "probe",
                },
            })),
            resource: resource_arn("template", &format!("{PROBE}-created")),
        },
    ]
}

/// Assert `reply` is the authorization refusal carrying `message`.
fn assert_denied(case: &str, reply: &Reply, message: &str) {
    assert_eq!(
        reply.status, 403,
        "case {case:?}: expected HTTP 403, got {reply:?}"
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("AccessDeniedException"),
        "case {case:?}: expected the error code AccessDeniedException, got {:?}",
        reply.error_code
    );
    assert_eq!(
        reply.body.get("message").and_then(Value::as_str),
        Some(message),
        "case {case:?}: expected the message {message:?}, got {:?}",
        reply.body
    );
}

/// Assert `reply` is not an authorization refusal: whatever the operation then answers about
/// the request or about stored state, the caller was allowed to make the call.
fn assert_not_denied(case: &str, reply: &Reply) {
    assert_ne!(
        reply.status, 403,
        "case {case:?}: expected the call to be authorized, got {reply:?}"
    );
    assert_ne!(
        reply.error_code.as_deref(),
        Some("AccessDeniedException"),
        "case {case:?}: expected the call to be authorized, got {reply:?}"
    );
}

/// RULE: a principal whose identity-based policies say nothing about this service is refused
/// every operation of it, with the sentence naming the principal, the action and the resource
/// the request addressed; a principal whose policies allow every action of this service, and
/// the account's own published credential, are refused none of them.
#[tokio::test]
async fn an_identity_policy_decides_every_sesv2_operation() {
    let router = build_router();
    let logs_only = assumed_role(
        &router,
        "sesv2-authz-logs-only",
        &allow("logs:CreateLogStream", "*"),
    )
    .await;
    let every_action = assumed_role(&router, "sesv2-authz-sending", &allow("ses:*", "*")).await;
    let other_service_user = user(&router, "sesv2-authz-reader", &allow("s3:GetObject", "*")).await;

    for operation in operation_cases() {
        let OperationCase {
            case,
            operation: name,
            method,
            path,
            body,
            resource,
        } = operation;
        let action = action_of(name);

        // A role carrying a grant for ANOTHER service: IAM's default deny.
        let refused = sesv2(
            &router,
            &logs_only.access_key_id,
            method,
            &path,
            body.as_ref(),
        )
        .await;
        assert_denied(
            &format!("{case}, as a role granting another service's actions"),
            &refused,
            &no_policy_allows(&logs_only.arn, &action, &resource),
        );

        // An IAM user, the other kind of principal a long-term credential speaks for, with
        // policies that say nothing about this service either.
        let refused = sesv2(
            &router,
            &other_service_user.access_key_id,
            method,
            &path,
            body.as_ref(),
        )
        .await;
        assert_denied(
            &format!("{case}, as an IAM user granting another service's actions"),
            &refused,
            &no_policy_allows(&other_service_user.arn, &action, &resource),
        );

        // A role granting every action of this service.
        let allowed = sesv2(
            &router,
            &every_action.access_key_id,
            method,
            &path,
            body.as_ref(),
        )
        .await;
        assert_not_denied(
            &format!("{case}, as a role granting every action of this service"),
            &allowed,
        );

        // The account's own published credential, which holds no IAM policies.
        let allowed = sesv2(&router, ACCOUNT_KEY, method, &path, body.as_ref()).await;
        assert_not_denied(
            &format!("{case}, as the account's own published credential"),
            &allowed,
        );
    }
}

/// RULE: the decision is taken on the action the operation names, so a grant for ONE action of
/// this service does not carry any other — and an explicit `Deny` beside a wildcard `Allow`
/// refuses that one action with the clause naming the deny.
#[tokio::test]
async fn a_grant_covers_the_one_action_it_names() {
    let router = build_router();
    let cases = operation_cases();
    let read_account = cases
        .iter()
        .find(|case| case.operation == "GetAccount")
        .expect("the table carries the account-level read");

    let one_action = assumed_role(
        &router,
        "sesv2-authz-one-action",
        &allow(&action_of("GetAccount"), "*"),
    )
    .await;
    let all_but_one = assumed_role(
        &router,
        "sesv2-authz-denied-action",
        &allow_every_action_but(&action_of("GetAccount")),
    )
    .await;

    let allowed = sesv2(
        &router,
        &one_action.access_key_id,
        read_account.method,
        &read_account.path,
        None,
    )
    .await;
    assert_not_denied("the one action the policy grants", &allowed);

    let refused = sesv2(
        &router,
        &all_but_one.access_key_id,
        read_account.method,
        &read_account.path,
        None,
    )
    .await;
    assert_denied(
        "the one action the policy denies beside a wildcard allow",
        &refused,
        &explicit_deny(
            &all_but_one.arn,
            &action_of("GetAccount"),
            &read_account.resource,
        ),
    );

    // Every OTHER operation of the table, under the policy that grants the account read
    // alone: a grant for one action of this service carries no other.
    for operation in cases.iter().filter(|case| case.operation != "GetAccount") {
        let refused = sesv2(
            &router,
            &one_action.access_key_id,
            operation.method,
            &operation.path,
            operation.body.as_ref(),
        )
        .await;
        assert_denied(
            &format!("{}, under a grant for another action", operation.case),
            &refused,
            &no_policy_allows(
                &one_action.arn,
                &action_of(operation.operation),
                &operation.resource,
            ),
        );
    }
}

/// RULE: a request that acts on ONE resource is decided against that resource's ARN — the
/// resource its URI names, or the one a create's own input names — so a statement scoped to it
/// allows the call and the same statement scoped to another resource of the same kind does
/// not.
#[tokio::test]
async fn a_statement_scoped_to_one_resource_decides_the_request_that_names_it() {
    let router = build_router();
    for (index, operation) in operation_cases()
        .into_iter()
        .filter(|case| case.resource != "*")
        .enumerate()
    {
        let action = action_of(operation.operation);
        // The resource the request addresses, and another resource of the SAME kind: the ARN
        // differs only in the id it names.
        let elsewhere = operation.resource.replace(PROBE, "authz-elsewhere");
        assert_ne!(
            elsewhere, operation.resource,
            "case {:?}: the other resource's ARN must differ from the one addressed",
            operation.case
        );

        let scoped = assumed_role(
            &router,
            &format!("sesv2-authz-scoped-{index}"),
            &allow(&action, &operation.resource),
        )
        .await;
        let allowed = sesv2(
            &router,
            &scoped.access_key_id,
            operation.method,
            &operation.path,
            operation.body.as_ref(),
        )
        .await;
        assert_not_denied(
            &format!("{}, under a grant on the ARN it names", operation.case),
            &allowed,
        );

        let elsewhere_role = assumed_role(
            &router,
            &format!("sesv2-authz-elsewhere-{index}"),
            &allow(&action, &elsewhere),
        )
        .await;
        let refused = sesv2(
            &router,
            &elsewhere_role.access_key_id,
            operation.method,
            &operation.path,
            operation.body.as_ref(),
        )
        .await;
        assert_denied(
            &format!(
                "{}, under a grant on another resource of the same kind",
                operation.case
            ),
            &refused,
            &no_policy_allows(&elsewhere_role.arn, &action, &operation.resource),
        );
    }
}
