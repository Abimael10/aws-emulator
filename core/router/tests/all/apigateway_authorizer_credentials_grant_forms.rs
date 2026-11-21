// SPDX-License-Identifier: CC0-1.0

//! HOW the `authorizerCredentials` role of an API Gateway Lambda authorizer is permitted to
//! invoke that authorizer's function, over every form that permission can be stated in.
//!
//! The rule under test: **an `Authorizer` whose `authorizerCredentials` names a role is
//! invoked UNDER that role, so its function RUNS — and the `policyDocument` it returns decides
//! the call, an `Allow` on the invoked method reaching the method's integration and an
//! explicit `Deny` refused `403` — whenever the role's trust policy admits
//! `apigateway.amazonaws.com` and the permissions in force on that role allow
//! `lambda:InvokeFunction` on the authorizer's function, WHATEVER form states them: an
//! embedded inline policy, a customer-managed policy attached to the role, an action or
//! resource stated as a wildcard, a role held under a path. The function's own resource policy
//! is not consulted for such an authorizer, because naming the role is the documented
//! alternative to it ("to use resource-based permissions on the Lambda function, specify
//! null"), and the `500` envelope API Gateway reserves
//! for an authorizer it cannot invoke is earned only where the permissions in force on the
//! role do NOT allow that invocation.**
//!
//! `Authorizer$authorizerCredentials` is "the required credentials as an IAM role for API
//! Gateway to invoke the authorizer. To specify an IAM role for API Gateway to assume, use the
//! role's Amazon Resource Name (ARN). To use resource-based permissions on the Lambda
//! function, specify null"; `AuthorizerType` gives `TOKEN` as the type whose function "receives
//! the caller's identity in a bearer token" and `REQUEST` as the one that "receives the
//! caller's identity in a request context", and both ARE Lambda functions, so the rule is one
//! rule over both.
//!
//! What [`crate::cloudformation_rest_api_token_authorizer_credentials`] already pins, and this
//! suite does not repeat: that the two documented ALTERNATIVES (a role, or a grant on the
//! function) each admit a `TOKEN` authorizer's invocation, and that a role allowed no
//! invocation at all, a role this service cannot assume, and a function granting nothing each
//! earn the configuration envelope. What only this suite reaches is the axis that sibling holds
//! fixed: the FORM the role's permission is stated in. A role is allowed an action by the
//! permissions in force on it, not by one particular way of writing them down, so a stage that
//! only reaches a decision for an exactly-worded inline statement on an exactly-named function
//! ARN fails here.
//!
//! The rows differ along what the rule generalises over: the form of the grant (an inline
//! policy, an attached customer-managed policy), how narrowly the action and resource are
//! written (an exact action on an exact ARN, a service-wide wildcard action on every
//! resource), where IAM holds the role (at the root, under a path), which Lambda authorizer
//! type names it (`TOKEN`, `REQUEST`), and — for the complement — a role whose stated grant is
//! overridden by an explicit `Deny`, which is the one case that must earn the `500`.
//!
//! Five crates have to agree for the rule to hold, which is why this suite lives here:
//! CloudFormation expands the templates and makes every create call, IAM holds both roles, the
//! managed policy and the trust documents it answers for, STS is what the assume-role reading
//! goes through, AWS Lambda holds and runs the authorizer function, and API Gateway resolves
//! the stage, invokes the authorizer under the role and evaluates what it returned.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of, physical_ids_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region of the stack's resources.
const REGION: &str = "us-east-1";

/// The `StackStatus` every row is waited for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How often the waits below look again, and how long they give up after. Generous: what is
/// waited on is a CONDITION another service owns, never an elapsed time this suite asserts on.
const LOOK: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(120);

/// The token the authorizer's own handler answers an `Allow` for, and one it answers an
/// explicit `Deny` for. They are the handler's inputs, not values read back from anything.
const ALLOW_TOKEN: &str = "allow-me";
const DENY_TOKEN: &str = "deny-me";

/// The header every authorizer below declares as its identity source, and the value it is
/// presented under on the wire.
const IDENTITY_SOURCE: &str = "method.request.header.Authorization";
const IDENTITY_HEADER: &str = "authorization";

/// The body the gated method's `MOCK` integration answers an admitted call with: the
/// template's own fixed text.
const OK_BODY: &str = r#"{"message":"ok"}"#;

/// The sentence a call the returned policy explicitly DENIES is refused with, and the status it
/// is refused at.
const EXPLICIT_DENY: &str = "User is not authorized to access this resource with an explicit \
                             deny in an identity-based policy";
const DENIED_STATUS: u16 = 403;

/// The status and the fixed body API Gateway answers when it cannot invoke the authorizer at
/// all. No variable part: the envelope names neither the authorizer nor the role, and no error
/// message is set for that class — so the response type's own default template renders the
/// `message` member as the JSON literal `null`.
const CONFIGURATION_ERROR: (u16, &str) = (500, r#"{"message":null}"#);

/// The logical ids every template below declares its function, its api-side role and its api
/// under.
const FUNCTION_ID: &str = "AuthorizerFunction";
const ROLE_ID: &str = "ApiGatewayInvokeRole";

/// The authorizer function every row deploys. It reads the presented token out of whichever
/// event its authorizer type hands it — a `TOKEN` authorizer's `authorizationToken`, a
/// `REQUEST` authorizer's `Authorization` header — and returns a policy that allows the
/// invoked method for one token and explicitly denies it for anything else, so each leg a
/// client sees is this function's own answer.
const AUTHORIZER_SOURCE: &str = r#"
def lambda_handler(event, context):
    token = event.get("authorizationToken")
    if token is None:
        headers = event.get("headers")
        if headers is not None:
            token = headers.get("Authorization")
    effect = "Allow" if token == "allow-me" else "Deny"
    return {
        "principalId": "caller",
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Action": "execute-api:Invoke",
                    "Effect": effect,
                    "Resource": event["methodArn"],
                }
            ],
        },
    }
"#;

/// Where the role's permission to invoke the function is written down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GrantForm {
    /// An inline policy embedded in the role itself (`AWS::IAM::Role`'s `Policies`).
    Inline,
    /// A customer-managed policy of its own, attached to the role (`AWS::IAM::ManagedPolicy`
    /// with `Roles`).
    ManagedAttachment,
    /// An inline policy that allows the invocation AND a statement that explicitly denies it:
    /// the permissions in force on the role do not allow it, whatever the first statement says.
    InlineOverriddenByDeny,
}

/// What the grant's statements name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GrantScope {
    /// The exact action on the exact function ARN.
    ExactActionOnTheFunction,
    /// A service-wide wildcard action on every resource.
    WildcardActionOnEverything,
}

/// One row: how this role's permission is stated, where IAM holds the role, which authorizer
/// type names it, and whether the stage therefore reaches the function's own decision.
struct Row {
    case: &'static str,
    form: GrantForm,
    scope: GrantScope,
    /// The role's IAM `Path`, or `None` for a role held at the root.
    path: Option<&'static str>,
    /// `Authorizer$type`: `TOKEN` or `REQUEST`, both of which ARE Lambda functions.
    authorizer_type: &'static str,
    /// The stage the api is deployed to and the path part the gated method sits on — the rule
    /// is indifferent to both, so no two rows share them.
    stage: &'static str,
    path_part: &'static str,
    /// Whether the authorizer is invocable, so each leg is its function's own decision.
    decided: bool,
}

/// The statements one row's grant document carries.
fn grant_statements(row: &Row) -> Value {
    let resource = match row.scope {
        GrantScope::ExactActionOnTheFunction => json!({ "Fn::GetAtt": [FUNCTION_ID, "Arn"] }),
        GrantScope::WildcardActionOnEverything => json!("*"),
    };
    let action = match row.scope {
        GrantScope::ExactActionOnTheFunction => "lambda:InvokeFunction",
        GrantScope::WildcardActionOnEverything => "lambda:*",
    };
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": action,
        "Resource": resource
    })];
    if row.form == GrantForm::InlineOverriddenByDeny {
        statements.push(json!({
            "Effect": "Deny",
            "Action": "lambda:InvokeFunction",
            "Resource": { "Fn::GetAtt": [FUNCTION_ID, "Arn"] }
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The template one row deploys: the authorizer function and its execution role, the api-side
/// role whose permission this row states in its own form, and the api — a resource, a Lambda
/// authorizer naming that role, a `CUSTOM` method with a `MOCK` integration behind it, a
/// deployment and a stage.
fn template(row: &Row) -> String {
    let document = grant_statements(row);
    let mut role = json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "apigateway.amazonaws.com" },
                    "Action": "sts:AssumeRole"
                }]
            }
        }
    });
    let properties = role
        .get_mut("Properties")
        .and_then(Value::as_object_mut)
        .expect("the role's properties");
    if let Some(path) = row.path {
        properties.insert("Path".to_string(), json!(path));
    }
    if row.form != GrantForm::ManagedAttachment {
        properties.insert(
            "Policies".to_string(),
            json!([{
                "PolicyName": "invoke-the-authorizer",
                "PolicyDocument": document.clone()
            }]),
        );
    }

    let mut resources = json!({
        "LambdaExecutionRole": {
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole"
                    }]
                }
            }
        },
        FUNCTION_ID: {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Timeout": 30,
                "Role": { "Fn::GetAtt": ["LambdaExecutionRole", "Arn"] },
                "Code": { "ZipFile": AUTHORIZER_SOURCE }
            }
        },
        ROLE_ID: role,
        "RestApi": {
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("authorizer-grant-{}", row.path_part),
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        },
        "GatedResource": {
            "Type": "AWS::ApiGateway::Resource",
            "Properties": {
                "RestApiId": { "Ref": "RestApi" },
                "ParentId": { "Fn::GetAtt": ["RestApi", "RootResourceId"] },
                "PathPart": row.path_part
            }
        },
        "GateAuthorizer": {
            "Type": "AWS::ApiGateway::Authorizer",
            "Properties": {
                "RestApiId": { "Ref": "RestApi" },
                "Name": format!("{}-authorizer", row.path_part),
                "Type": row.authorizer_type,
                "IdentitySource": IDENTITY_SOURCE,
                "AuthorizerResultTtlInSeconds": 0,
                "AuthorizerCredentials": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                "AuthorizerUri": {
                    "Fn::Sub": format!(
                        "arn:aws:apigateway:${{AWS::Region}}:lambda:path/2015-03-31/functions/\
                         ${{{FUNCTION_ID}.Arn}}/invocations"
                    )
                }
            }
        },
        "SecureMethod": {
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": "RestApi" },
                "ResourceId": { "Ref": "GatedResource" },
                "HttpMethod": "GET",
                "AuthorizationType": "CUSTOM",
                "AuthorizerId": { "Ref": "GateAuthorizer" },
                "MethodResponses": [{ "StatusCode": "200" }],
                "Integration": {
                    "Type": "MOCK",
                    "RequestTemplates": { "application/json": r#"{"statusCode": 200}"# },
                    "IntegrationResponses": [{
                        "StatusCode": "200",
                        "ResponseTemplates": { "application/json": OK_BODY }
                    }]
                }
            }
        },
        "ApiDeployment": {
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": ["SecureMethod"],
            "Properties": {
                "RestApiId": { "Ref": "RestApi" },
                "Description": "the gated deployment"
            }
        },
        "ApiStage": {
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "RestApi" },
                "DeploymentId": { "Ref": "ApiDeployment" },
                "StageName": row.stage
            }
        }
    });
    let resources = resources
        .as_object_mut()
        .expect("the resources object just built");
    if row.form == GrantForm::ManagedAttachment {
        resources.insert(
            "InvokePolicy".to_string(),
            json!({
                "Type": "AWS::IAM::ManagedPolicy",
                "Properties": {
                    "Description": "the invocation this api-side role is allowed",
                    "PolicyDocument": document,
                    "Roles": [{ "Ref": ROLE_ID }]
                }
            }),
        );
    }

    json!({
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "RestApi" } },
            "RoleArn": { "Value": { "Fn::GetAtt": [ROLE_ID, "Arn"] } },
            "InvokeUrl": {
                "Value": {
                    "Fn::Sub": format!(
                        "https://${{RestApi}}.execute-api.${{AWS::Region}}.amazonaws.com/{}/{}",
                        row.stage, row.path_part
                    )
                }
            }
        }
    })
    .to_string()
}

/// The emulator as it is ASSEMBLED, on one engine: the api CloudFormation creates is the api
/// this suite then invokes, the role IAM answers for is the one the authorizer names, and the
/// authorizer behind the method is the function AWS Lambda runs.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
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

/// Read one API Gateway control-plane resource over its restJson1 wire.
async fn apigateway(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// One Lambda `restJson1` read, for the wait below.
async fn lambda(router: &MultiServiceRouter, uri: &str) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`.
///
/// The span a created function spends coming up is that service's own, so no fixed sleep names
/// the instant it ends: the wait is on the CONDITION, looking again until it holds.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let configuration =
            lambda(router, &format!("/2015-03-31/functions/{name}/configuration")).await;
        let state = configuration
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state == "Active" || Instant::now() >= giving_up_at {
            return state;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// Invoke the deployed stage at the invoke URL the stack published, presenting `token` at the
/// authorizer's identity source.
///
/// The URL is taken apart rather than rebuilt, so what is driven is the value the stack told
/// the caller to use.
async fn invoke(router: &MultiServiceRouter, invoke_url: &str, token: &str) -> (u16, String) {
    let without_scheme = invoke_url
        .strip_prefix("https://")
        .or_else(|| invoke_url.strip_prefix("http://"))
        .unwrap_or(invoke_url);
    let (host, path) = match without_scheme.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{path}")),
        None => (without_scheme.to_string(), "/".to_string()),
    };
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .header(IDENTITY_HEADER, token)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The message an API Gateway data-plane answer carries, under either spelling the service
/// renders one by, or the whole text where it carries none.
fn message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|answered| {
            ["message", "Message"].into_iter().find_map(|member| {
                answered
                    .get(member)
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| body.to_string())
}

/// Deploy one row and return what its stack published: the api id, the api-side role's ARN and
/// the stage's invoke URL.
///
/// Every claim here is read back out of the OWNING service: the function out of Lambda, the
/// authorizer out of API Gateway, so a row whose stage is driven below is one whose authorizer
/// really does name the role this row stated a grant for.
async fn deployed(router: &MultiServiceRouter, row: &Row, stack: &str) -> (String, String, String) {
    let case = row.case;
    let body = template(row);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            // The template declares roles, which is what CAPABILITY_IAM acknowledges.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );

    let (settled, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    let failures = if settled == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described} with \
         resource failures {failures:?}"
    );

    let published = outputs_of(&described);
    let api_id = published
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ApiId output in {described}"));
    let role_arn = published
        .get("RoleArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no RoleArn output in {described}"));
    let invoke_url = published
        .get("InvokeUrl")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no InvokeUrl output in {described}"));

    let (_, stack_resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    let physical: BTreeMap<String, String> = physical_ids_of(&stack_resources);
    let function = physical.get(FUNCTION_ID).cloned().unwrap_or_else(|| {
        panic!("case {case:?}: no physical id for {FUNCTION_ID} in {stack_resources}")
    });
    let state = wait_until_active(router, &function).await;
    assert_eq!(
        state, "Active",
        "case {case:?}: expected Lambda to report {function} Active before the invoke, got \
         {state:?}"
    );

    let (status, authorizers) = apigateway(router, &format!("/restapis/{api_id}/authorizers")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetAuthorizers, got {status} with body \
         {authorizers}"
    );
    let authorizer = authorizers
        .get("item")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the api holds no authorizer: {authorizers}"));
    assert_eq!(
        authorizer.get("type").and_then(Value::as_str),
        Some(row.authorizer_type),
        "case {case:?}: expected the api's authorizer to be a {} one, got {authorizer}",
        row.authorizer_type
    );
    assert_eq!(
        authorizer
            .get("authorizerCredentials")
            .and_then(Value::as_str),
        Some(role_arn.as_str()),
        "case {case:?}: expected the authorizer to name the role the stack created, got \
         {authorizer}"
    );

    (api_id, role_arn, invoke_url)
}

/// Drive both legs of one row against the stage the stack deployed.
async fn assert_legs(router: &MultiServiceRouter, row: &Row, invoke_url: &str) {
    let case = row.case;
    let legs: [(&str, &str, u16, &str); 2] = [
        (
            "the token the authorizer allows",
            ALLOW_TOKEN,
            200,
            OK_BODY,
        ),
        (
            "the token the authorizer explicitly denies",
            DENY_TOKEN,
            DENIED_STATUS,
            EXPLICIT_DENY,
        ),
    ];
    for (which, token, decided_status, decided_body) in legs {
        let (expected_status, expected_body) = if row.decided {
            (decided_status, decided_body.to_string())
        } else {
            (CONFIGURATION_ERROR.0, CONFIGURATION_ERROR.1.to_string())
        };
        let (status, answered) = invoke(router, invoke_url, token).await;
        assert_eq!(
            status, expected_status,
            "case {case:?} / {which}: expected HTTP {expected_status} at {invoke_url}, got \
             {status} with body {answered:?}"
        );
        // An admitted call is answered with the integration's own body; a call the returned
        // policy refused with the sentence its gateway response carries; and one whose
        // authorizer was never invoked with the whole fixed envelope, whose message is unset.
        let reported = if status == 200 || !row.decided {
            answered.clone()
        } else {
            message(&answered)
        };
        assert_eq!(
            reported, expected_body,
            "case {case:?} / {which}: expected {expected_body:?}, got {answered:?}"
        );
    }
}

/// The rows whose role IS allowed the invocation, each stating that permission in a different
/// form — so each row's stage reaches the authorizer function's own decision.
const PERMITTED: &[Row] = &[
    Row {
        case: "an inline policy allowing a service-wide wildcard action on every resource",
        form: GrantForm::Inline,
        scope: GrantScope::WildcardActionOnEverything,
        path: None,
        authorizer_type: "TOKEN",
        stage: "test",
        path_part: "wildcarded",
        decided: true,
    },
    Row {
        case: "a customer-managed policy attached to the role instead of an inline one",
        form: GrantForm::ManagedAttachment,
        scope: GrantScope::ExactActionOnTheFunction,
        path: None,
        authorizer_type: "TOKEN",
        stage: "prod",
        path_part: "attached",
        decided: true,
    },
    Row {
        case: "a role IAM holds under a path, with the grant embedded in it",
        form: GrantForm::Inline,
        scope: GrantScope::ExactActionOnTheFunction,
        path: Some("/apigateway-invoke/"),
        authorizer_type: "TOKEN",
        stage: "v1",
        path_part: "pathed",
        decided: true,
    },
    Row {
        case: "a REQUEST authorizer naming the same kind of role",
        form: GrantForm::Inline,
        scope: GrantScope::ExactActionOnTheFunction,
        path: None,
        authorizer_type: "REQUEST",
        stage: "beta",
        path_part: "contextual",
        decided: true,
    },
];

/// The complement: the permissions in force on the role do NOT allow the invocation, because a
/// statement about this role explicitly denies it — the one case that earns the configuration
/// envelope, which is what tells a working authorizer from a misconfigured one.
const WITHHELD: &[Row] = &[Row {
    case: "an inline grant the role's own policy explicitly denies",
    form: GrantForm::InlineOverriddenByDeny,
    scope: GrantScope::ExactActionOnTheFunction,
    path: None,
    authorizer_type: "TOKEN",
    stage: "stage-2",
    path_part: "overridden",
    decided: false,
}];

/// RULE: for every form the role's permission is stated in, the stack settles and its stage
/// reaches the authorizer function's own decision — the allowed leg runs the method's `MOCK`
/// integration and is answered with the body the template declared, and the explicitly denied
/// leg is refused `403` with API Gateway's explicit-deny sentence.
#[tokio::test]
async fn an_authorizer_credentials_role_is_permitted_in_any_form_its_grant_is_stated() {
    let router = build_router();
    for (index, row) in PERMITTED.iter().enumerate() {
        let stack = format!("authorizer-grant-permitted-{index}");
        let (api_id, role_arn, invoke_url) = deployed(&router, row, &stack).await;
        // The stage the stack deployed is one API Gateway HOLDS, which is what serves the
        // invoke URL below.
        let (status, stage) =
            apigateway(&router, &format!("/restapis/{api_id}/stages/{}", row.stage)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from GetStage for {}, got {status} with body {stage}",
            row.case, row.stage
        );
        // The role whose permission this row stated is the one the authorizer was invoked
        // under, so the ARN the api holds is the ARN the stack created — including the path a
        // pathed role carries.
        if let Some(path) = row.path {
            assert!(
                role_arn.contains(path),
                "case {:?}: expected the role ARN to carry the path {path}, got {role_arn:?}",
                row.case
            );
        }
        assert_legs(&router, row, &invoke_url).await;
    }
}

/// RULE, the complement: a role whose stated grant is overridden by an explicit `Deny` is a
/// role the invocation is not allowed under, so every leg earns the `500` generic envelope —
/// asserted beside the permitted rows above, so an emulator that answers that envelope for
/// every configuration fails here.
#[tokio::test]
async fn an_authorizer_credentials_role_denied_the_invocation_earns_the_configuration_error() {
    let router = build_router();
    for (index, row) in WITHHELD.iter().enumerate() {
        let stack = format!("authorizer-grant-withheld-{index}");
        let (_, _, invoke_url) = deployed(&router, row, &stack).await;
        assert_legs(&router, row, &invoke_url).await;
    }
}
