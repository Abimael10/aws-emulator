// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed email identity is the one a function the SAME stack deploys can
//! configure — the DKIM signing switch it sets lands on the identity the stack says it created.
//!
//! The rule under test: **a stack reports a resource as created only where the service that owns
//! the type created it, under the identifier that type publishes, so the handle the stack hands
//! out for it (`Ref`, and the physical resource id a client reads back) addresses the resource
//! that service really holds.** For an email identity the identifier IS the domain or the
//! address the declaration states — "Ref returns the email identity", and every SES operation on
//! one takes that same text — so a function handed that handle in its environment, which is the
//! ordinary shape of this deployment, sets the identity's DKIM signing through its own SDK and
//! is served, rather than refused for an identity nothing created. A generated
//! `<stack>-<logical id>-<suffix>` name in that handle would make every such call name an
//! identity the owning service never held.
//!
//! Five crates have to agree for that to hold — CloudFormation deploys the identity, the role
//! and the function, IAM answers for the execution role, AWS Lambda runs the handler and
//! supplies its SDK, and the SES v2 API decides the write and reports the identity back — so the
//! suite lives here: a crate-local test can only ever see one side of it.
//! `core/router/tests/all/cloudformation_provisions_ses_email_identities.rs` drives the
//! provisioning side of the same rule with the writes made by the account's own credential; what
//! only this suite reaches is the write a DEPLOYED FUNCTION makes against the handle its
//! environment was given.
//!
//! The rows differ along what the rule generalises over: the KIND of identity (a domain and an
//! address), whether the identity's text arrives in the declaration as a literal or through a
//! template parameter and an intrinsic, and the value of the switch the handler sets (both of
//! them, since the member is modelled with a default). Each row also asserts the other half of
//! the rule: the same handler asking for an identity the stack did NOT create is refused by the
//! owning service, with that service's own code and sentence — so what the positive leg proves
//! is an identity that service holds, and the two legs of such a deployment's own check are
//! told apart rather than answering alike.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, dispatch, emulator, lambda, logs_policy, trust_policy, wait_until_active,
    Answered, CREATE_COMPLETE, FUNCTION_TIMEOUT,
};
use crate::query_wire::{outputs_of, physical_ids_of};

/// The logical ids every row's template declares.
const IDENTITY_ID: &str = "RealIdentity";
const ROLE_ID: &str = "ExecRole";
const FUNCTION_ID: &str = "ValidFn";

/// The template parameter a row whose identity arrives through an intrinsic declares.
const IDENTITY_PARAMETER: &str = "IdentityText";

/// The environment variable the function resolves the identity's handle into, and the managed
/// runtime and entry point it runs under.
const IDENTITY_VARIABLE: &str = "IDENTITY_NAME";
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The modelled refusal the SES v2 API answers an attribute write whose `EmailIdentity` names
/// none of the account's identities: `BadRequestException`, "the input you provided is invalid",
/// bound to HTTP 400 (`smithy_query(query="operation",
/// operation="PutEmailIdentityDkimAttributes")`).
const BAD_REQUEST: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u64 = 400;

/// The status a served write answers under, which that operation's `@http` binding declares
/// (code 200) and whose output shape carries no member at all.
const SERVED_STATUS: u64 = 200;

/// The handler every row deploys: it sets the DKIM signing switch of the identity its
/// ENVIRONMENT names — or of the one the event names, which is the negative leg — and reports
/// what the owning service answered, so the invocation's payload is that service's own answer
/// and never something the runtime composed.
const HANDLER_SOURCE: &str = r#"
import os

import boto3
import botocore.exceptions


def handler(event, context):
    client = boto3.client("sesv2")
    identity = event.get("identity") or os.environ["IDENTITY_NAME"]
    enabled = event.get("signing", True)
    try:
        answered = client.put_email_identity_dkim_attributes(
            EmailIdentity=identity, SigningEnabled=enabled
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "identity": identity,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "identity": identity,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "members": sorted(key for key in answered if key != "ResponseMetadata"),
    }
"#;

/// One row: the identity a stack declares, how the declaration states it, what the deployed
/// handler sets on it, and the identity nothing ever created that the negative leg asks for.
struct Row {
    case: &'static str,
    /// The stack's name.
    stack: &'static str,
    /// The function's name, which the row invokes it by.
    function: &'static str,
    /// The text the declaration states the identity with, or `None` for a row that states it
    /// through the template parameter below.
    declared: Option<&'static str>,
    /// The parameter value a row whose declaration resolves an intrinsic supplies.
    parameter: Option<&'static str>,
    /// The identity the stack must publish and the owning service must hold.
    identity: &'static str,
    /// The `IdentityType` the owning service must report for it, which its text decides.
    identity_type: &'static str,
    /// The switch the handler sets, which every later read of the identity must report.
    signing_enabled: bool,
    /// An identity no call here ever creates, which the negative leg asks for.
    absent: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a domain identity the declaration names, with signing switched on",
        stack: "xsvc-identity-dkim-domain",
        function: "xsvc-identity-dkim-domain-fn",
        declared: Some("notices.example.com"),
        parameter: None,
        identity: "notices.example.com",
        identity_type: "DOMAIN",
        signing_enabled: true,
        absent: "never-created.example.net",
    },
    Row {
        // An address identity, whose refusal sentence names the domain part of the value rather
        // than the whole of it.
        case: "an address identity the declaration names, with signing switched off",
        stack: "xsvc-identity-dkim-address",
        function: "xsvc-identity-dkim-address-fn",
        declared: Some("sender@mail.example.org"),
        parameter: None,
        identity: "sender@mail.example.org",
        identity_type: "EMAIL_ADDRESS",
        signing_enabled: false,
        absent: "nobody@absent.example.org",
    },
    Row {
        // The identity's text reaches the declaration through a parameter and an intrinsic, so
        // the identity is created under the RESOLVED text and that text is what the handle
        // carries.
        case: "a domain identity whose text the declaration resolves from a parameter",
        stack: "xsvc-identity-dkim-parameter",
        function: "xsvc-identity-dkim-parameter-fn",
        declared: None,
        parameter: Some("billing.example.com"),
        identity: "billing.example.com",
        identity_type: "DOMAIN",
        signing_enabled: true,
        absent: "billing.example.net",
    },
];

/// The domain an email identity belongs to: the part after the at sign of an address, or the
/// whole text of a domain identity. DKIM signs a DOMAIN's mail, so this is the variable part of
/// the sentence the write answers for an identity the account does not hold.
fn identity_domain(identity: &str) -> &str {
    match identity.rsplit_once('@') {
        Some((_, domain)) => domain,
        None => identity,
    }
}

/// What the execution role may do: the basic-execution log grant the crate states once, plus the
/// one write the handler makes. The handler's call is made BY this role and the email API decides
/// it against the policies in force on it, so a role short of the action reads a `403` instead of
/// the service's own verdict — which is that rule rather than this one.
fn role_policy() -> Value {
    let mut statements = logs_policy()["Statement"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    statements.push(json!({
        "Effect": "Allow",
        "Action": "ses:PutEmailIdentityDkimAttributes",
        "Resource": "*",
    }));
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The template a row deploys: the email identity, the execution role, and the function whose
/// environment resolves the identity's handle through `Ref`.
fn template(row: &Row) -> String {
    let declared_identity = match (row.declared, row.parameter) {
        (Some(text), _) => json!(text),
        (None, Some(_)) => json!({ "Ref": IDENTITY_PARAMETER }),
        (None, None) => panic!(
            "case {:?}: a row states the identity as a literal or through the parameter",
            row.case
        ),
    };
    let mut document = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{}-exec", row.function),
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "configure-email-identity",
                        "PolicyDocument": role_policy()
                    }]
                }
            },
            IDENTITY_ID: {
                "Type": "AWS::SES::EmailIdentity",
                "Properties": { "EmailIdentity": declared_identity }
            },
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": {
                        IDENTITY_VARIABLE: { "Ref": IDENTITY_ID }
                    } },
                    "Code": { "ZipFile": HANDLER_SOURCE }
                }
            }
        },
        "Outputs": {
            "IdentityName": { "Value": { "Ref": IDENTITY_ID } },
            "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
        }
    });
    if row.parameter.is_some() {
        document["Parameters"] = json!({ IDENTITY_PARAMETER: { "Type": "String" } });
    }
    document.to_string()
}

/// One SES v2 call over its `restJson1` wire, made by the account's own credential.
async fn sesv2(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(
        router,
        "ses",
        method,
        path,
        &[("content-type", "application/json")],
        bytes::Bytes::new(),
    )
    .await
}

/// `GetEmailIdentity` on `identity`, as the owning service answers it. The texts here are domain
/// names and addresses, whose characters a URI path segment carries as they are.
async fn get_identity(router: &MultiServiceRouter, identity: &str) -> Answered {
    sesv2(router, "GET", &format!("/v2/email/identities/{identity}")).await
}

/// `Invoke` `function` synchronously with `event`, as `(the FunctionError signal, the payload)`.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let answered = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        Some(event.clone()),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} with {:?}",
        answered.status,
        answered.text()
    );
    (answered.function_error.clone(), answered.json())
}

#[tokio::test]
async fn a_deployed_functions_dkim_write_reaches_the_identity_its_stack_created() {
    let router = emulator();

    for row in ROWS {
        let case = row.case;

        // 1. The stack: the identity, the execution role and the function pointed at it.
        let body = template(row);
        let mut members: Vec<(&str, &str)> = vec![
            ("StackName", row.stack),
            ("TemplateBody", body.as_str()),
            // The template declares a NAMED IAM role, which is acknowledged with
            // CAPABILITY_NAMED_IAM.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ];
        if let Some(value) = row.parameter {
            members.push(("Parameters.member.1.ParameterKey", IDENTITY_PARAMETER));
            members.push(("Parameters.member.1.ParameterValue", value));
        }
        let created = cloudformation(&router, "CreateStack", &members).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );
        let (reached, described) = wait_for_stack_status(&router, row.stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reached} with \
             {described}"
        );

        // 2. The handle the stack publishes for the identity is the identity's own text — the
        //    declaration's, resolved — and never a generated `<stack>-<logical id>-<suffix>`
        //    name, both as the output `Ref` fills and as the physical resource id a client reads
        //    back off the resource.
        let outputs = outputs_of(&described);
        let published = outputs.get("IdentityName").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack publishes IdentityName, got {outputs:?}")
        });
        assert_eq!(
            published, row.identity,
            "case {case:?}: expected the declared identity as the published handle, got \
             {published:?}"
        );
        let function = outputs.get("FunctionName").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack publishes FunctionName, got {outputs:?}")
        });
        let resources = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            resources.status,
            200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {} with {:?}",
            resources.status,
            resources.text()
        );
        let described_resources = resources.text();
        assert_eq!(
            physical_ids_of(&described_resources).get(IDENTITY_ID),
            Some(&row.identity.to_string()),
            "case {case:?}: expected the identity's own text as its physical resource id, got \
             {described_resources}"
        );

        // 3. The service that OWNS the type holds it, under exactly that text.
        let held = get_identity(&router, &published).await;
        assert_eq!(
            held.status,
            200,
            "case {case:?}: the email API must hold the identity the stack published, got {} with \
             {:?}",
            held.status,
            held.text()
        );
        assert_eq!(
            held.json().get("IdentityType").and_then(Value::as_str),
            Some(row.identity_type),
            "case {case:?}: expected the identity kind its text makes it, got {}",
            held.json()
        );

        // 4. The function's environment carries the RESOLVED handle, which is what the handler
        //    addresses the identity by.
        let configuration = wait_until_active(&router, case, &function).await;
        let variables = configuration
            .get("Environment")
            .and_then(|environment| environment.get("Variables"))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            variables.get(IDENTITY_VARIABLE).and_then(Value::as_str),
            Some(row.identity),
            "case {case:?}: expected the environment to resolve the declared identity, got \
             {variables}"
        );

        // 5. And the write the deployed handler makes against that handle is SERVED: the
        //    operation's own 200 with an output carrying no member, rather than a refusal of the
        //    identity its own stack reported creating.
        let (signal, payload) = invoke(
            &router,
            case,
            &function,
            &json!({ "signing": row.signing_enabled }),
        )
        .await;
        assert_eq!(
            signal, None,
            "case {case:?}: the handler's own call must succeed, got {payload}"
        );
        assert_eq!(
            payload.get("ok").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the handler's DKIM write to be served, got {payload}"
        );
        assert_eq!(
            payload.get("identity").and_then(Value::as_str),
            Some(row.identity),
            "case {case:?}: expected the handler to address the declared identity, got {payload}"
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_u64),
            Some(SERVED_STATUS),
            "case {case:?}: expected the operation's own success status, got {payload}"
        );
        assert_eq!(
            payload.get("members").and_then(Value::as_array),
            Some(&Vec::new()),
            "case {case:?}: expected an output carrying no member, got {payload}"
        );

        // 6. The switch landed on the identity the stack created, as the owning service's own
        //    read reports it.
        let read_back = get_identity(&router, row.identity).await;
        assert_eq!(
            read_back.status,
            200,
            "case {case:?}: expected the identity to be readable after the write, got {} with \
             {:?}",
            read_back.status,
            read_back.text()
        );
        let reported = read_back.json();
        assert_eq!(
            reported
                .get("DkimAttributes")
                .and_then(|dkim| dkim.get("SigningEnabled"))
                .and_then(Value::as_bool),
            Some(row.signing_enabled),
            "case {case:?}: expected the signing switch the handler set, got {reported}"
        );

        // 7. The other half of the rule: an identity the stack did NOT create is the owning
        //    service's own refusal — its modelled invalid-input code, its 400, and the sentence
        //    naming the domain the write was asked to sign for — so the leg above is an identity
        //    that service holds rather than an answer the runtime composed, and a client can
        //    tell the two apart.
        let (signal, payload) = invoke(
            &router,
            case,
            &function,
            &json!({ "identity": row.absent, "signing": true }),
        )
        .await;
        assert_eq!(
            signal, None,
            "case {case:?}: the handler reports the refusal rather than failing, got {payload}"
        );
        assert_eq!(
            payload.get("ok").and_then(Value::as_bool),
            Some(false),
            "case {case:?}: expected the identity nothing created to be refused, got {payload}"
        );
        assert_eq!(
            payload.get("code").and_then(Value::as_str),
            Some(BAD_REQUEST),
            "case {case:?}: expected the email API's own invalid-input code, got {payload}"
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_u64),
            Some(BAD_REQUEST_STATUS),
            "case {case:?}: expected that refusal's modelled status, got {payload}"
        );
        let refusal = format!(
            "Domain {} is not verified for DKIM signing.",
            identity_domain(row.absent)
        );
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some(refusal.as_str()),
            "case {case:?}: expected the refusal to be worded {refusal:?}, got {payload}"
        );
        // And nothing the refused write asked for was done: the identity the stack DID create
        // still reports the switch its own successful write set.
        let unchanged = get_identity(&router, row.identity).await.json();
        assert_eq!(
            unchanged
                .get("DkimAttributes")
                .and_then(|dkim| dkim.get("SigningEnabled"))
                .and_then(Value::as_bool),
            Some(row.signing_enabled),
            "case {case:?}: expected the refused write to leave the created identity as it was, \
             got {unchanged}"
        );
    }
}
