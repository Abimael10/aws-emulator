// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-deployed Lambda function may read out of AWS Secrets Manager is
//! decided by the identity policies in force on the execution role the stack gave it — and the
//! verdict the handler itself reached is what `Invoke` answers with.
//!
//! The rule pinned here: **a function's own AWS SDK call is authorized against the identity
//! policies in force on its execution role, so the call is served when some statement in force
//! allows that action on that resource and is refused with the AWS-common
//! `AccessDeniedException` when none does — an explicit `Deny` refusing whatever an `Allow`
//! says — regardless of HOW the grant came to be in force (a standalone policy resource, the
//! role's own inline policies, a policy a template `Condition` gated) — and `Invoke` answers a
//! `RequestResponse` invocation with the payload the handler returned, so that verdict is what
//! the caller reads.**
//!
//! That is one rule over two services and three more that have to agree: CloudFormation
//! evaluates the template and makes the calls, IAM holds the role and the documents, STS vends
//! the role's session, Lambda RUNS the handler with the AWS SDK its runtime supplies, and
//! Secrets Manager takes the decision — so the suite lives here rather than in any one crate.
//! Every observation is made where a client makes one: `DescribeStacks`,
//! `DescribeStackResources`, `GetSecretValue` and `Invoke`, never an emulator internal.
//!
//! The rows differ along exactly what the rule generalises over: WHERE the grant is attached
//! (an `AWS::IAM::Policy` whose `Condition` decides whether it exists at all, the role's own
//! inline `Policies` property, a standalone `AWS::IAM::RolePolicy`, or nothing at all), WHAT
//! the statement in force covers (this secret by its own ARN, an ARN wildcard that ARN matches,
//! a different action of the same service) and whether a second statement explicitly denies the
//! read — plus the shape of the `SecretString` the stack deploys. The denied and the served
//! branch are each reached through more than one attachment, so no verdict rests on one
//! spelling, and the first two rows are ONE template deployed twice — into the same emulator,
//! with its gating parameter flipped: the positive control that attributes the refusal to the
//! withheld statement rather than to the plumbing.
//!
//! `cloudformation_condition_gated_secrets_policy` pins the `Conditions` section itself (the
//! forms a condition can be written in, and what the stack therefore has and publishes), and
//! `lambda_in_function_secretsmanager_grant_revocation` pins WHICH secret a statement names and
//! what revoking it does; this suite holds both of those fixed and varies where the grant is
//! ATTACHED and what its statements cover, which neither of them reaches.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries, read from the emulator itself rather
/// than written out, so the ARN pattern one row's grant names cannot drift from it.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The terminal `StackStatus` a create that succeeded settles to.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The statuses a `CreateStack` can settle to, as `DescribeStacks` reports them.
const TERMINAL_STATUSES: [&str; 4] = [
    CREATE_COMPLETE,
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];

/// The logical ids the template declares, and the parameter the gating condition reads.
const PARAMETER: &str = "AllowSecretsAccess";
const SECRET: &str = "Secret";
const ROLE: &str = "Role";
const GRANT: &str = "Grant";
const FUNCTION: &str = "Fn";

/// The condition the gated grant names.
const CONDITION: &str = "GrantSecretsPolicy";

/// The output keys the template declares.
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const SECRET_ARN_OUTPUT: &str = "SecretArn";

/// The runtime and handler every row's function declares.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation of this handler is given, as the function's own `Timeout`: far past
/// what a run needs, because what is under test is the authorization decision and never how
/// fast a loaded host gets an interpreter through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The action the handler's read performs, as IAM names it ("Actions defined by AWS Secrets
/// Manager": `secretsmanager:GetSecretValue`).
const READ_ACTION: &str = "secretsmanager:GetSecretValue";

/// Another action of the same service, which a statement granting it does NOT make the read
/// above allowed ("Actions defined by AWS Secrets Manager": `secretsmanager:DescribeSecret`).
const OTHER_ACTION: &str = "secretsmanager:DescribeSecret";

/// The error code AWS answers a call no policy in force allows with — the AWS-common
/// `AccessDeniedException`, which is the code the SDK hands the handler and the handler
/// reports back as its own `error_code`.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// How long to wait between polls, and how many polls to make. Each loop below exits the
/// instant its CONDITION holds, so the product is a DEADLINE and not a wait: past what either
/// condition needs even on a loaded host, because what this suite asserts is the authorization
/// decision and never how fast a stack is provisioned or an interpreter warmed, and still short
/// enough that a row that never converges reports in seconds.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 1_200;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// handler's own SDK call is dispatched back into this same router, so the secret it reads is
/// the secret this stack deployed.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
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

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// Call one Secrets Manager operation over its `awsJson1_1` wire, as the emulator's own
/// account credentials rather than as the function's role.
async fn secretsmanager(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The handler every row's function declares inline: it reads the secret the stack named it
/// through its environment with the AWS SDK its runtime supplies, and reports either the value
/// it was served or the error code the service refused the read with.
const READER_SOURCE: &str = r#"
import os

import boto3
import botocore.exceptions


def handler(event, context):
    client = boto3.client("secretsmanager")
    try:
        answered = client.get_secret_value(SecretId=os.environ["SECRET_ID"])
    except botocore.exceptions.ClientError as error:
        return {"ok": False, "error_code": error.response["Error"]["Code"]}
    return {"ok": True, "secret": answered["SecretString"]}
"#;

/// WHERE a row attaches the grant to the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attachment {
    /// An `AWS::IAM::Policy` resource gated on a template `Condition`, with the value the
    /// deployment supplies for the parameter the condition reads: `"false"` leaves the
    /// statement out of the stack altogether.
    ConditionGatedPolicy(&'static str),
    /// The role's own inline `Policies` property ("adds or updates an inline policy document
    /// that is embedded in the specified IAM role").
    RoleInlinePolicies,
    /// A standalone `AWS::IAM::RolePolicy` resource, which names the role it embeds into.
    RolePolicyResource,
    /// No policy of any kind: the role carries its trust policy and nothing else.
    NoPolicy,
}

/// WHAT the statements of the attached document cover. Ignored by
/// [`Attachment::NoPolicy`], which attaches no document for them to be written into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Statements {
    /// Allow the read on exactly this stack's own secret.
    ReadOnTheSecret,
    /// Allow the read on an ARN wildcard the secret's own ARN matches.
    ReadOnAnArnWildcard,
    /// Allow a different action of the same service on this secret.
    AnotherActionOnTheSecret,
    /// Allow the read, and explicitly deny it in a second statement.
    ReadAllowedAndExplicitlyDenied,
}

/// One row: where the grant is attached, what it covers, whether the read is therefore served,
/// and the `SecretString` the stack deploys.
struct Row {
    case: &'static str,
    attachment: Attachment,
    statements: Statements,
    /// Whether the handler is therefore served the secret rather than refused.
    served: bool,
    secret: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the gated grant is withheld, so the read is refused",
        attachment: Attachment::ConditionGatedPolicy("false"),
        statements: Statements::ReadOnTheSecret,
        served: false,
        secret: "a-plain-secret-string",
    },
    Row {
        case: "the same template with the gate open, so the read is served",
        attachment: Attachment::ConditionGatedPolicy("true"),
        statements: Statements::ReadOnTheSecret,
        served: true,
        secret: "a-plain-secret-string",
    },
    Row {
        case: "the role carries no policy at all, so the read falls to the default deny",
        attachment: Attachment::NoPolicy,
        statements: Statements::ReadOnTheSecret,
        served: false,
        secret: r#"{"endpoint":"db.internal","port":5432}"#,
    },
    Row {
        case: "the inline policy grants another action of the same service, so the read is \
               refused",
        attachment: Attachment::RoleInlinePolicies,
        statements: Statements::AnotherActionOnTheSecret,
        served: false,
        // `SecretString` is free text, so a value whose shape is neither plain nor JSON
        // travels as written (its only constraint is a length of at least one —
        // `smithy_query(query="shape", shape="SecretStringType")`).
        secret: "line one\nline two\nline three\n",
    },
    Row {
        case: "the inline policy allows the read and denies it, so the explicit deny wins",
        attachment: Attachment::RoleInlinePolicies,
        statements: Statements::ReadAllowedAndExplicitlyDenied,
        served: false,
        secret: "line one\nline two\nline three\n",
    },
    Row {
        case: "a standalone role policy names an ARN wildcard the secret's own ARN matches, so \
               the read is served",
        attachment: Attachment::RolePolicyResource,
        statements: Statements::ReadOnAnArnWildcard,
        served: true,
        secret: "a-secret-with-unicode-£-and-a-space",
    },
];

/// The trust policy the stack's role carries, so Lambda can be handed it.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The document a row's grant carries, written against the template's own references so that
/// nothing here has to know an identifier the deployment computes.
///
/// The one exception is the ARN WILDCARD row, whose whole point is a pattern rather than an
/// identifier: a secret's ARN is
/// `arn:aws:secretsmanager:<region>:<account>:secret:<name>-<six characters>`, so the pattern
/// is the name this stack declares followed by IAM's `*` (any run).
fn policy_document(row: &Row, stack: &str) -> Option<Value> {
    if row.attachment == Attachment::NoPolicy {
        return None;
    }
    let read_on_the_secret = json!({
        "Effect": "Allow",
        "Action": READ_ACTION,
        "Resource": { "Ref": SECRET },
    });
    let statements = match row.statements {
        Statements::ReadOnTheSecret => vec![read_on_the_secret],
        Statements::ReadOnAnArnWildcard => vec![json!({
            "Effect": "Allow",
            "Action": READ_ACTION,
            "Resource": format!(
                "arn:aws:secretsmanager:{REGION}:{ACCOUNT}:secret:{stack}-secret-*"
            ),
        })],
        Statements::AnotherActionOnTheSecret => vec![json!({
            "Effect": "Allow",
            "Action": OTHER_ACTION,
            "Resource": { "Ref": SECRET },
        })],
        Statements::ReadAllowedAndExplicitlyDenied => vec![
            read_on_the_secret,
            json!({
                "Effect": "Deny",
                "Action": READ_ACTION,
                "Resource": { "Ref": SECRET },
            }),
        ],
    };
    Some(json!({ "Version": "2012-10-17", "Statement": statements }))
}

/// The template a row deploys: the secret, the execution role with the grant attached the way
/// this row attaches it, the function that performs the read, and the outputs a client reads
/// the deployment's identifiers out of.
fn template(row: &Row, stack: &str) -> String {
    let document = policy_document(row, stack);
    let policy_name = format!("{stack}-read-the-secret");

    let mut role = json!({
        "RoleName": format!("{stack}-role"),
        "AssumeRolePolicyDocument": trust_policy(),
    });
    if let (Attachment::RoleInlinePolicies, Some(document)) = (row.attachment, document.as_ref()) {
        role["Policies"] = json!([{
            "PolicyName": policy_name,
            "PolicyDocument": document,
        }]);
    }

    let mut resources = json!({
        SECRET: {
            "Type": "AWS::SecretsManager::Secret",
            "Properties": {
                "Name": format!("{stack}-secret"),
                "SecretString": row.secret,
            },
        },
        ROLE: {
            "Type": "AWS::IAM::Role",
            "Properties": role,
        },
        FUNCTION: {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": format!("{stack}-fn"),
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Role": { "Fn::GetAtt": [ROLE, "Arn"] },
                "Environment": { "Variables": { "SECRET_ID": { "Ref": SECRET } } },
                "Code": { "ZipFile": READER_SOURCE },
            },
        },
    });
    match (row.attachment, document) {
        (Attachment::ConditionGatedPolicy(_), Some(document)) => {
            resources[GRANT] = json!({
                "Type": "AWS::IAM::Policy",
                "Condition": CONDITION,
                "Properties": {
                    "PolicyName": policy_name,
                    "Roles": [{ "Ref": ROLE }],
                    "PolicyDocument": document,
                },
            });
        }
        (Attachment::RolePolicyResource, Some(document)) => {
            resources[GRANT] = json!({
                "Type": "AWS::IAM::RolePolicy",
                "Properties": {
                    "PolicyName": policy_name,
                    "RoleName": { "Ref": ROLE },
                    "PolicyDocument": document,
                },
            });
        }
        // Every other row attaches the document on the role itself, or attaches none at all.
        _ => {}
    }

    let mut body = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": {
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
            SECRET_ARN_OUTPUT: { "Value": { "Ref": SECRET } },
        },
    });
    if let Attachment::ConditionGatedPolicy(_) = row.attachment {
        body["Parameters"] = json!({
            PARAMETER: { "Type": "String", "AllowedValues": ["true", "false"] },
        });
        body["Conditions"] = json!({
            CONDITION: { "Fn::Equals": [{ "Ref": PARAMETER }, "true"] },
        });
    }
    body.to_string()
}

/// What the handler must answer for a row: the secret's own value when a statement in force
/// allows the read, and the refusal's error code when none does.
fn expected_payload(row: &Row) -> Value {
    if row.served {
        json!({ "ok": true, "secret": row.secret })
    } else {
        json!({ "ok": false, "error_code": ACCESS_DENIED })
    }
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active` — "the
/// function is ready to be invoked" (`FunctionConfiguration$State`) — and fail, naming the
/// deadline and the state last reported, when it does not within [`POLL`] * [`POLLS`].
///
/// The span a created function spends `Pending` is that service's own, so no fixed sleep names
/// the instant it ends.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (status, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration on {name:?}, got \
             {status} with {configuration}"
        );
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(POLL).await;
    }
    panic!(
        "case {case:?}: expected Lambda to report {name:?} Active within {:?}, last reported \
         {last}",
        POLL * u32::try_from(POLLS).expect("a poll count that fits a duration multiplier")
    );
}

/// Poll `DescribeStacks` until the stack first reports a TERMINAL status, and answer it with
/// the answer it was read from, failing — naming the deadline and the status last reported —
/// when it does not settle within [`POLL`] * [`POLLS`].
async fn wait_for_terminal_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let mut last = stack_status(router, stack).await;
    for _ in 0..POLLS {
        if TERMINAL_STATUSES.contains(&last.0.as_str()) {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    panic!(
        "expected {stack:?} to settle to a terminal StackStatus within {:?}, last reported {} \
         with {}",
        POLL * u32::try_from(POLLS).expect("a poll count that fits a duration multiplier"),
        last.0,
        last.1
    );
}

/// Deploy one row's stack, and answer `(the outputs, the logical -> physical id map)`.
async fn deploy(
    router: &MultiServiceRouter,
    row: &Row,
    stack: &str,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let case = row.case;
    let body = template(row, stack);
    let mut members = vec![
        ("StackName", stack.to_string()),
        ("TemplateBody", body),
        // The template declares IAM resources, one of them NAMED, which is what these two
        // capabilities acknowledge.
        ("Capabilities.member.1", "CAPABILITY_IAM".to_string()),
        ("Capabilities.member.2", "CAPABILITY_NAMED_IAM".to_string()),
    ];
    if let Attachment::ConditionGatedPolicy(supplied) = row.attachment {
        members.push(("Parameters.member.1.ParameterKey", PARAMETER.to_string()));
        members.push(("Parameters.member.1.ParameterValue", supplied.to_string()));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let (status, answered) = cloudformation(router, "CreateStack", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );

    let (reached, described) = wait_for_terminal_status(router, stack).await;
    let reasons = if reached == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described} with \
         resource failures {reasons:?}"
    );

    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {resources}"
    );
    (outputs_of(&described), physical_ids_of(&resources))
}

/// One deploy, one read-back of the deployed secret, and one invocation per row.
#[tokio::test]
async fn the_grant_in_force_on_the_execution_role_decides_the_handlers_read() {
    // ONE emulator for every row, so the refused rows and the served ones are stacks of the
    // same account: a refusal here cannot be an account in which nothing works.
    let router = build_router();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-role-grant-{index}");
        let (outputs, physical) = deploy(&router, row, &stack).await;

        // What the stack HAS: the resources it declared, and the gated grant exactly when its
        // condition held.
        for logical in [SECRET, ROLE, FUNCTION] {
            assert!(
                physical.contains_key(logical),
                "case {case:?}: expected a physical id for {logical}, got {physical:?}"
            );
        }
        let grant_expected = match row.attachment {
            Attachment::ConditionGatedPolicy(supplied) => supplied == "true",
            Attachment::RolePolicyResource => true,
            Attachment::RoleInlinePolicies | Attachment::NoPolicy => false,
        };
        assert_eq!(
            physical.contains_key(GRANT),
            grant_expected,
            "case {case:?}: expected the stack to hold {GRANT} == {grant_expected}, got \
             {physical:?}"
        );

        let function = outputs
            .get(FUNCTION_NAME_OUTPUT)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no {FUNCTION_NAME_OUTPUT} output in {outputs:?}")
            });
        let secret_arn = outputs
            .get(SECRET_ARN_OUTPUT)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no {SECRET_ARN_OUTPUT} output in {outputs:?}")
            });

        // The secret the stack reported as created is one Secrets Manager really holds, read
        // through that service's own API: whatever the function is refused, the secret and its
        // value are there, so a refusal is the authorization decision and not a missing value.
        let (status, held) =
            secretsmanager(&router, "GetSecretValue", &json!({ "SecretId": secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetSecretValue on the published ARN, got \
             {status} with {held}"
        );
        assert_eq!(
            held.get("SecretString").and_then(Value::as_str),
            Some(row.secret),
            "case {case:?}: expected the deployed SecretString {:?}, got {held}",
            row.secret
        );

        // What the grant MEANS: the handler runs, and its own verdict is what Invoke answers.
        wait_until_active(&router, case, &function).await;
        let (status, headers, payload) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            b"{}",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {status} with payload {payload}"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            signal, None,
            "case {case:?}: expected no function error, got {signal:?} with payload {payload}"
        );
        let expected = expected_payload(row);
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the handler's own result {expected}, got {payload}"
        );
    }
}
