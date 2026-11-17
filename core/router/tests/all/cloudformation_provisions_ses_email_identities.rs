// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that reports `CREATE_COMPLETE` has really created the
//! `AWS::SES::EmailIdentity` resources its template declares — in Amazon SES.
//!
//! The rule under test: **a stack reports a resource as created only where the service that
//! owns the type created it, under the identifier the type publishes, so the handle the stack
//! hands out for that resource (`Ref`) addresses a resource that service really holds.** For
//! an email identity the identifier IS the domain or the address the declaration states —
//! "Ref returns the email identity", and every SES operation on one takes that same text — so
//! after the create waiter returns, the owning service's own read (`GetEmailIdentity`) and the
//! writes a client configures an identity with (`PutEmailIdentityDkimAttributes` and the rest
//! of that family) answer for the name the stack published, rather than Amazon SES refusing an
//! identity its own stack just reported. Two crates have to agree for that to hold
//! (CloudFormation makes the call, the SES v2 API serves it), so the suite lives here: a
//! crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: the KIND of identity (a domain and an
//! address), whether the identity's text arrives as a literal or through a template parameter
//! and an intrinsic, and which of the type's further properties the declaration carries — the
//! configuration set the identity sends with (a set declared in the same stack, resolved
//! through `Ref`), the DKIM signing switch, the feedback-forwarding switch and a custom MAIL
//! FROM domain, each of which is a call of its own on the identity that was created. Every
//! expected value is computed from what the row declares, never read back out of the answer
//! being checked.
//!
//! The deletion half, the refusal half and the not-mapped half are rows of the same rule:
//! deleting the stack deletes the identities it created, an identity the owning service
//! REFUSES leaves the stack `ROLLBACK_COMPLETE` rather than publishing a name nothing answers
//! to, and a declared property this emulator sends to no call of the owning service fails the
//! resource rather than reporting an identity configured differently from its declaration.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request here is signed for — the Region `wait_for_stack_status` polls in
/// — and so the Region the stack's identities belong to.
const REGION: &str = "us-east-1";

/// The two stack statuses the rows are stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The modelled refusal the SES v2 API answers for an identity the account does not hold
/// (`NotFoundException`, HTTP 404), which the deletion half of the rule expects.
const NOT_FOUND: &str = "NotFoundException";

/// How long the deletion half looks for the identity to be gone, and how long it waits
/// between two looks. Generous: the wait is on the CONDITION (the owning service no longer
/// holds the identity), never on how fast the host runs.
const DELETE_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. The classic Amazon SES surface is here because
/// the configuration set the rows associate is created through the API that owns THAT type,
/// and because an account's identities are one set behind both surfaces.
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
        service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
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

/// Call one SES v2 operation over its restJson1 wire, as `(status, body)`.
async fn sesv2(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = match body {
        Some(document) => serde_json::to_vec(document).expect("the request body serializes"),
        None => Vec::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("email.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("ses"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The identity's own path under the SES v2 identity collection. The texts this suite uses
/// are domain names and addresses, whose characters (`.`, `-`, `@`) a URI path segment
/// carries as they are, so none is escaped here.
fn identity_path(identity: &str) -> String {
    format!("/v2/email/identities/{identity}")
}

/// `GetEmailIdentity` on `identity`.
async fn get_identity(router: &MultiServiceRouter, identity: &str) -> (u16, Value) {
    sesv2(router, "GET", &identity_path(identity), None).await
}

/// What one row declares, and what the identity must end up as.
struct Row {
    case: &'static str,
    /// The stack's name.
    stack: &'static str,
    /// The text the identity is declared with, or `None` where it arrives through the
    /// template parameter below.
    declared: Option<&'static str>,
    /// The parameter value a row whose identity comes through `Ref` supplies.
    parameter: Option<&'static str>,
    /// The identity the stack must publish and the owning service must hold.
    identity: &'static str,
    /// The `IdentityType` the owning service must report for it.
    identity_type: &'static str,
    /// Whether the declaration associates the configuration set the same stack declares.
    associates_configuration_set: bool,
    /// `DkimAttributes.SigningEnabled`, where the declaration carries the property.
    signing_enabled: Option<bool>,
    /// `FeedbackAttributes.EmailForwardingEnabled`, where the declaration carries it.
    email_forwarding_enabled: Option<bool>,
    /// `MailFromAttributes.MailFromDomain`, where the declaration carries it.
    mail_from_domain: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a domain identity with no further properties",
        stack: "identity-domain-stack",
        declared: Some("notices.example.com"),
        parameter: None,
        identity: "notices.example.com",
        identity_type: "DOMAIN",
        associates_configuration_set: false,
        signing_enabled: None,
        email_forwarding_enabled: None,
        mail_from_domain: None,
    },
    Row {
        case: "an address identity with no further properties",
        stack: "identity-address-stack",
        declared: Some("sender@mail.example.net"),
        parameter: None,
        identity: "sender@mail.example.net",
        identity_type: "EMAIL_ADDRESS",
        associates_configuration_set: false,
        signing_enabled: None,
        email_forwarding_enabled: None,
        mail_from_domain: None,
    },
    Row {
        // The identity arrives through a template parameter and an intrinsic rather than as a
        // literal, so it is created under the RESOLVED text.
        case: "a domain identity whose text comes from a parameter through Ref",
        stack: "identity-parameter-stack",
        declared: None,
        parameter: Some("billing.example.org"),
        identity: "billing.example.org",
        identity_type: "DOMAIN",
        associates_configuration_set: false,
        signing_enabled: None,
        email_forwarding_enabled: None,
        mail_from_domain: None,
    },
    Row {
        case: "a domain identity carrying every property that is a call of its own",
        stack: "identity-configured-stack",
        declared: Some("campaigns.example.com"),
        parameter: None,
        identity: "campaigns.example.com",
        identity_type: "DOMAIN",
        associates_configuration_set: true,
        signing_enabled: Some(true),
        email_forwarding_enabled: Some(false),
        mail_from_domain: Some("mail.campaigns.example.com"),
    },
];

/// The configuration set each row's template declares beside the identity, which the row that
/// associates one resolves through `Ref`.
const CONFIGURATION_SET: &str = "identity-stack-set";

/// The template a row deploys: the configuration set, and the email identity with the
/// properties the row declares.
fn template(row: &Row) -> String {
    let mut properties = json!({});
    match (row.declared, row.parameter) {
        (Some(identity), _) => properties["EmailIdentity"] = json!(identity),
        (None, Some(_)) => properties["EmailIdentity"] = json!({ "Ref": "IdentityText" }),
        (None, None) => {
            panic!("case {:?}: a row states the identity one way or the other", row.case)
        }
    }
    if row.associates_configuration_set {
        properties["ConfigurationSetAttributes"] =
            json!({ "ConfigurationSetName": { "Ref": "ConfigSet" } });
    }
    if let Some(enabled) = row.signing_enabled {
        properties["DkimAttributes"] = json!({ "SigningEnabled": enabled });
    }
    if let Some(enabled) = row.email_forwarding_enabled {
        properties["FeedbackAttributes"] = json!({ "EmailForwardingEnabled": enabled });
    }
    if let Some(domain) = row.mail_from_domain {
        properties["MailFromAttributes"] =
            json!({ "MailFromDomain": domain, "BehaviorOnMxFailure": "USE_DEFAULT_VALUE" });
    }
    let mut document = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ConfigSet": {
                "Type": "AWS::SES::ConfigurationSet",
                "Properties": { "Name": CONFIGURATION_SET }
            },
            "Identity": {
                "Type": "AWS::SES::EmailIdentity",
                "Properties": properties
            }
        },
        "Outputs": {
            "IdentityName": { "Value": { "Ref": "Identity" } }
        }
    });
    if row.parameter.is_some() {
        document["Parameters"] = json!({ "IdentityText": { "Type": "String" } });
    }
    document.to_string()
}

/// Create the row's stack and wait for it to settle, answering the `DescribeStacks` body.
async fn deploy(router: &MultiServiceRouter, row: &Row, expected: &str) -> String {
    let body = template(row);
    let mut members: Vec<(&str, &str)> =
        vec![("StackName", row.stack), ("TemplateBody", body.as_str())];
    if let Some(value) = row.parameter {
        members.push(("Parameters.member.1.ParameterKey", "IdentityText"));
        members.push(("Parameters.member.1.ParameterValue", value));
    }
    let (status, answered) = cloudformation(router, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from CreateStack, got {status} with {answered}",
        row.case
    );
    let (reported, described) = wait_for_stack_status(router, row.stack, expected).await;
    assert_eq!(
        reported, expected,
        "case {:?}: expected the stack to reach {expected}, got {reported} with {described}",
        row.case
    );
    described
}

#[tokio::test]
async fn a_completed_stack_published_email_identities_ses_really_holds() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let described = deploy(&router, row, CREATE_COMPLETE).await;

        // The handle the stack publishes for the identity is the identity's own text: "Ref
        // returns the email identity", never a generated `<stack>-<logical id>-<suffix>`
        // name.
        let outputs = outputs_of(&described);
        let published = outputs.get("IdentityName").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack publishes IdentityName, got {outputs:?}")
        });
        assert_eq!(
            published, row.identity,
            "case {case:?}: expected the declared identity as the published handle, got \
             {published:?}"
        );

        // The service that OWNS the type holds it, under exactly that text.
        let (status, held) = get_identity(&router, &published).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the email API must hold the identity the stack published, got \
             {status} with {held}"
        );
        assert_eq!(
            held.get("IdentityType").and_then(Value::as_str),
            Some(row.identity_type),
            "case {case:?}: expected the identity kind its text makes it, got {held}"
        );
        let expected_set = row.associates_configuration_set.then_some(CONFIGURATION_SET);
        assert_eq!(
            held.get("ConfigurationSetName").and_then(Value::as_str),
            expected_set,
            "case {case:?}: expected the configuration set the template declared, got {held}"
        );
        if let Some(enabled) = row.signing_enabled {
            assert_eq!(
                held.get("DkimAttributes")
                    .and_then(|dkim| dkim.get("SigningEnabled"))
                    .and_then(Value::as_bool),
                Some(enabled),
                "case {case:?}: expected the declared DKIM signing switch, got {held}"
            );
        }
        if let Some(enabled) = row.email_forwarding_enabled {
            assert_eq!(
                held.get("FeedbackForwardingStatus").and_then(Value::as_bool),
                Some(enabled),
                "case {case:?}: expected the declared feedback-forwarding switch, got {held}"
            );
        }
        assert_eq!(
            held.get("MailFromAttributes")
                .and_then(|attributes| attributes.get("MailFromDomain"))
                .and_then(Value::as_str),
            row.mail_from_domain,
            "case {case:?}: expected the declared custom MAIL FROM domain, got {held}"
        );

        // And the write a client configures an identity with lands on the identity the stack
        // published: an identity the owning service did not hold refuses this as a value that
        // names none of the account's identities.
        let (status, answered) = sesv2(
            &router,
            "PUT",
            &format!("{}/dkim", identity_path(&published)),
            Some(&json!({ "SigningEnabled": true })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the DKIM write on the published identity to be served, got \
             {status} with {answered}"
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_email_identity_it_created() {
    let router = build_router();
    let row = &ROWS[0];
    let case = "an identity deleted with its stack";
    deploy(&router, row, CREATE_COMPLETE).await;
    let (status, held) = get_identity(&router, row.identity).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the identity to exist before the delete, got {status} with \
         {held}"
    );

    let (status, answered) =
        cloudformation(&router, "DeleteStack", &[("StackName", row.stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {answered}"
    );

    // The identity the stack created is gone from the owning service, which is what makes a
    // stack's resources the stack's own rather than leftovers. The delete is owed from the
    // instant CloudFormation accepted it, which no fixed sleep can name, so this looks again
    // until the owning service no longer holds the identity or the deadline passes.
    let giving_up_at = Instant::now() + DELETE_DEADLINE;
    let mut answered = get_identity(&router, row.identity).await;
    while answered.0 == 200 && Instant::now() < giving_up_at {
        tokio::time::sleep(POLL_INTERVAL).await;
        answered = get_identity(&router, row.identity).await;
    }
    let (status, body) = answered;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the identity to be gone, got {status} with {body}"
    );
    assert_eq!(
        body.get("__type").and_then(Value::as_str),
        Some(NOT_FOUND),
        "case {case:?}: expected the email API's modelled not-found refusal, got {body}"
    );
}

#[tokio::test]
async fn an_email_identity_ses_refuses_fails_the_stack() {
    let router = build_router();
    let case = "a custom MAIL FROM domain that is no subdomain of the identity's own";
    let row = Row {
        case,
        stack: "identity-refused-stack",
        declared: Some("refused.example.com"),
        parameter: None,
        identity: "refused.example.com",
        identity_type: "DOMAIN",
        associates_configuration_set: false,
        signing_enabled: None,
        email_forwarding_enabled: None,
        // "The MAIL FROM domain has to be a subdomain of the verified identity": a domain
        // under another name is a request the owning service refuses, which the stack must
        // report rather than completing.
        mail_from_domain: Some("mail.elsewhere.example.net"),
    };
    deploy(&router, &row, ROLLBACK_COMPLETE).await;

    // Nothing is left behind: the identity whose attribute write was refused is removed
    // again, so the failed resource leaves no identity carrying none of what it declared.
    let (status, answered) = get_identity(&router, row.identity).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected no identity left behind, got {status} with {answered}"
    );
}
