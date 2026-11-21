// SPDX-License-Identifier: CC0-1.0

//! An API Gateway stage whose method integrates AWS LAMBDA's function-invoking action as a
//! NON-PROXY `AWS` integration: the whole path a client takes from the stage invoke URL to the
//! handler's own answer, across three services on one engine.
//!
//! RULE: a deployed method whose integration is of type `AWS` and whose `uri` names Lambda's
//! function-invoking path (`Integration$uri` gives the form
//! `arn:aws:apigateway:{region}:{subdomain}:path|action/{service_api}`, and `IntegrationType`
//! gives `AWS` "for integrating the API method request with an AWS service action, including
//! the Lambda function-invoking action") is served by INVOKING that function with the
//! integration request as its event, and the method response is mapped from what the run
//! answered through the integration response `IntegrationResponse$selectionPattern` selects.
//! WHO the call is made as is `Integration$credentials`: a role named there has the invocation
//! made under it, so that role must let `apigateway.amazonaws.com` assume it and its own
//! policies must allow `lambda:InvokeFunction` on the function; naming no role has the call
//! made as API Gateway's own principal, so the FUNCTION's resource-based policy is what admits
//! it. A role this api cannot assume is an integration that could not be performed at all
//! (`API_CONFIGURATION_ERROR`, HTTP 500 with `{"message":"Internal server error"}`) and the
//! function is never invoked; a call the role's policies do not allow is refused by Lambda
//! with its own `AccessDeniedException` (HTTP 403), which the integration responses then map
//! like any other backend answer. So the positive and the negative arms of the same api are
//! told apart: a correctly trusted and granted role serves the handler's answer, and only a
//! misconfigured one answers 5xx.
//!
//! Only a cross-service suite can pin this: the function and its run are Lambda's, the roles
//! and their policies are IAM's, and the api, the stage and the integration are API Gateway's,
//! so the rule is observable only where all three are mounted on one engine.
//!
//! The rows vary what the rule generalises over: WHICH role the integration names (one trusted
//! and granted on the function, one trusted but granted only on another function, one trusted
//! by the wrong service principal, and none at all — with and without the function's own
//! grant to API Gateway), HOW the `uri` spells the function (its full ARN and its bare name),
//! and WHAT the run answered (a document the integration passes through, and an exception,
//! whose own error the pattern of another integration response matches).
//!
//! Every expected value is computed here from what the setup declared — the function's ARN as
//! Lambda minted it, the role ARNs as IAM minted them, and the event the request's own body
//! renders into — never out of the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::form_encode;

/// The Region every request here is signed for, and the Region the integration ARNs name.
const REGION: &str = "us-east-1";

/// The stack the handlers are deployed by, and the status it has to reach: inline source a
/// template states as `Code.ZipFile` is archived into the deployment package by
/// CloudFormation, which is how a handler written out in a suite comes to exist at all.
const STACK: &str = "apigw-lambda-non-proxy-handlers";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The media type the method requests below declare, which is the key their integration
/// request template is stored under.
const JSON: &str = "application/json";

/// A trust policy that lets API Gateway assume the role it is attached to.
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The same policy for ANOTHER service's principal: a role this api cannot assume, which is
/// the only thing that differs between it and the one above.
const EC2_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The trust policy a function's EXECUTION role carries, which Lambda's own `CreateFunction`
/// checks, as the template below declares it.
fn lambda_trust() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
}

/// The IAM action an invocation of a function performs, which a grant for this integration has
/// to name: `Invoke` "requires permission for the `lambda:InvokeFunction` action".
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The service principal API Gateway invokes a function as when the integration names no role.
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The status API Gateway answers an integration it could not perform with, and the body that
/// gateway response's default template carries.
const CONFIGURATION_ERROR_STATUS: u16 = 500;
const CONFIGURATION_ERROR_BODY: &str = r#"{"message":"Internal server error"}"#;

/// The handler every row invokes: it answers a document built from the EVENT it was handed —
/// which for a non-proxy integration is the integration request the template rendered — and
/// raises when that event asks it to, so a row can observe a run that errored.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    if event.get("fail"):
        raise ValueError("deliberate %s failure" % event.get("name", "unnamed"))
    return {"greeting": "hello %s" % event["name"], "event": event}
"#;

/// The message the handler raises for the row that asks it to, as the `errorMessage` of the
/// error document Lambda answers with — which is what an integration response's
/// `selectionPattern` is matched against for a Lambda back end.
const RAISED_MESSAGE_PATTERN: &str = ".*deliberate.*";

/// The status the integration response carrying that pattern answers with.
const RAISED_STATUS: u16 = 400;

/// The functions the rows invoke: the one every role-bearing row addresses, the one a narrow
/// grant covers instead, and the two reached by the rows that name no role at all.
const ECHO: &str = "non-proxy-echo";
const OTHER: &str = "non-proxy-other";
const GRANTED: &str = "non-proxy-granted";
const UNGRANTED: &str = "non-proxy-ungranted";

/// How long to wait between looks at a condition another service owns, and the generous budget
/// one is given. Nothing here asserts on how long anything took: every wait is on the
/// CONDITION, looked at again until it holds.
const POLL: Duration = Duration::from_millis(25);

/// See [`POLL`].
const BUDGET: Duration = Duration::from_secs(60);

/// What one call answered.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the function API Gateway invokes is the one
/// AWS Lambda runs, and the roles it is judged against are the ones IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a setup call carries a credential scope so
    // the router can name the target service, and the invoke of the deployed stage carries
    // none, as a client of a public method sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answer = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", JSON),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answer.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// Call one CloudFormation operation over its awsQuery wire, as its status and its body.
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
    let answer = dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await;
    (answer.status, answer.text())
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answer = dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", JSON),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    (answer.status, answer.json())
}

/// One IAM awsQuery call that has to succeed, answering its body as text.
async fn iam_ok(router: &MultiServiceRouter, case: &str, body: String) -> String {
    let answer = dispatch(
        router,
        "POST",
        "iam.amazonaws.com",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("iam")),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup {case:?}: expected HTTP 200 from IAM, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// The member `name` of a JSON object, as text.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// One IAM role: created with `trust` as its trust policy and `document` as its one inline
/// policy. Answers the ARN IAM minted for it.
async fn role(router: &MultiServiceRouter, name: &str, trust: &str, document: String) -> String {
    let created = iam_ok(
        router,
        &format!("CreateRole({name})"),
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}&AssumeRolePolicyDocument={}",
            form_encode(trust)
        ),
    )
    .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole({name}): no <Arn> in {created}"));
    iam_ok(
        router,
        &format!("PutRolePolicy({name})"),
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
             &PolicyName=lambda-integration&PolicyDocument={}",
            form_encode(&document)
        ),
    )
    .await;
    arn
}

/// One identity-based policy allowing `action` on `resource`.
fn policy(action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// How many times this suite looks while waiting for a span it does not own.
fn looks() -> u128 {
    BUDGET.as_millis() / POLL.as_millis()
}

/// Deploy every function of `names` running [`HANDLER_SOURCE`], each as the one execution role
/// the stack declares, and wait until the stack reports itself created.
///
/// The handlers are declared as a stack's inline source, which CloudFormation archives into the
/// deployment package the create carries — the way a function whose handler is written out in a
/// suite comes to exist at all.
async fn deploy_functions(router: &MultiServiceRouter, names: &[&str]) {
    let mut resources = ::serde_json::Map::new();
    resources.insert(
        "ExecutionRole".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": { "AssumeRolePolicyDocument": lambda_trust() },
        }),
    );
    for (index, name) in names.iter().enumerate() {
        resources.insert(
            format!("Handler{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    // Generous: nothing here tests a function's own timeout, and a run shares
                    // this process with every other test of this target.
                    "Timeout": 60,
                    "Role": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            }),
        );
    }
    let template = json!({ "Resources": Value::Object(resources) }).to_string();
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares an IAM role, which CloudFormation creates only for a
            // request that acknowledges the capability ("Requires capabilities :
            // [CAPABILITY_IAM]").
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let (reported, answer) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: expected the handlers' stack to be created, got {reported} with {answer}"
    );
}

/// The ARN Lambda minted for `name`, read once that service first reports the function
/// `Active` — "the function is ready to be invoked" (`FunctionConfiguration$State`) — so an
/// answer a row reads later is the integration's and never the lifecycle's. The span waited for
/// is that service's own, so nothing here sleeps a fixed span on its behalf.
async fn active_function(router: &MultiServiceRouter, name: &str) -> String {
    for _ in 0..looks() {
        let (status, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if status == 200 && reported.get("State").and_then(Value::as_str) == Some("Active") {
            return member(name, &reported, "FunctionArn");
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup {name:?}: Lambda never reported the function Active");
}

/// Grant API Gateway the invocation of `function` through the function's OWN resource-based
/// policy, which is what admits a call the integration names no role for.
async fn add_permission(router: &MultiServiceRouter, function: &str) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke",
            "Action": INVOKE_ACTION,
            "Principal": APIGATEWAY_PRINCIPAL,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup {function:?}: expected HTTP 201 from Lambda AddPermission, got {status} \
         ({granted})"
    );
}

/// One resource of the api under its root, answering its id.
async fn resource(router: &MultiServiceRouter, api: &str, root: &str, part: &str) -> String {
    let created = apigw_ok(
        router,
        &format!("CreateResource({part})"),
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": part })),
        201,
    )
    .await;
    member(&format!("CreateResource({part})"), &created, "id")
}

/// The integration responses every method here declares: the default (no pattern) for the
/// answer of a run that finished, one whose pattern matches the ERROR a raising run reported,
/// and one per class of refusal, so what a client sees is what the back end answered rather
/// than one status for every outcome.
const MAPPED_RESPONSES: &[(&str, Option<&str>)] = &[
    ("200", None),
    ("400", Some(RAISED_MESSAGE_PATTERN)),
    ("403", Some(r"4\d{2}")),
    ("500", Some(r"5\d{2}")),
];

/// The integration request template every row's method renders the event from: the name the
/// method request carried and nothing else, so what reaches the handler is what the template
/// mapped rather than the request around it.
const EVENT_TEMPLATE: &str = r#"{"name":$input.json('$.name')}"#;

/// The same, for the method whose event asks the handler to RAISE: the flag is the template's
/// own constant, so one run of the same handler errors and the others do not.
const RAISING_EVENT_TEMPLATE: &str = r#"{"name":$input.json('$.name'),"fail":true}"#;

/// One method of the api whose integration INVOKES the function `uri` names, under
/// `credentials` (none where it is `None`), with `template` as the integration request template
/// that renders the event the handler is handed.
async fn function_method(
    router: &MultiServiceRouter,
    api: &str,
    resource_id: &str,
    uri: &str,
    credentials: Option<&str>,
    template: &str,
) {
    let case = format!("POST {uri}");
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/POST"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    let mut integration = json!({
        "type": "AWS",
        // `integrationHttpMethod` travels under its `jsonName`, which is what
        // `smithy_query(query="operation",operation="PutIntegration")` reports for it. Lambda's
        // invocation path is reached with POST whatever verb the METHOD carries.
        "httpMethod": "POST",
        "uri": uri,
        // The event the function is handed IS this rendering: a non-proxy integration maps the
        // method request into the integration request, which is what the run then reads.
        "requestTemplates": { JSON: template },
    });
    if let Some(credentials) = credentials {
        integration["credentials"] = json!(credentials);
    }
    apigw_ok(
        router,
        &case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/POST/integration"),
        Some(integration),
        201,
    )
    .await;
    for (status, pattern) in MAPPED_RESPONSES {
        apigw_ok(
            router,
            &case,
            "PUT",
            &format!("/restapis/{api}/resources/{resource_id}/methods/POST/responses/{status}"),
            Some(json!({})),
            201,
        )
        .await;
        let mut response = json!({ "statusCode": status });
        if let Some(pattern) = pattern {
            response["selectionPattern"] = json!(pattern);
        }
        apigw_ok(
            router,
            &case,
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource_id}/methods/POST/integration/responses/\
                 {status}"
            ),
            Some(response),
            201,
        )
        .await;
    }
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` — carrying no credential at all, as a
/// client of a public method sends it.
async fn invoke(router: &MultiServiceRouter, api: &str, route: &str, body: &Value) -> Answer {
    dispatch(
        router,
        "POST",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{route}"),
        &[("content-type", JSON)],
        Bytes::from(body.to_string()),
    )
    .await
}

/// What a row expects the stage to answer.
enum Expected {
    /// The handler RAN and its answer is the method response: the greeting it built from the
    /// event it was handed.
    Handled,
    /// The handler ran and RAISED, so the integration response whose pattern matches the error
    /// it reported maps the answer.
    Raised,
    /// Lambda refused the invocation the role asked for, in its own `AccessDeniedException`
    /// naming the role, the action and the function.
    Refused,
    /// The integration could not be performed at all, so the api answers its own
    /// configuration-error response and the function was never invoked.
    Unperformed,
}

/// One row: the route it is invoked through and what the stage answers.
struct Row {
    case: &'static str,
    route: &'static str,
    /// The name the request carries, which the event the handler is handed names.
    name: &'static str,
    expected: Expected,
}

#[tokio::test]
async fn a_deployed_method_integrating_a_function_invokes_it_under_the_role_it_names() {
    let router = emulator();

    // The function the rows invoke, and a second one no row invokes: the role granted only on
    // THAT one is what makes a row's grant too narrow for the call it makes. The two beside
    // them are reached by the rows that name no role at all, where the FUNCTION's own policy is
    // what admits API Gateway — granted on one of them and not on the other.
    deploy_functions(&router, &[ECHO, OTHER, GRANTED, UNGRANTED]).await;
    let function = active_function(&router, ECHO).await;
    let other = active_function(&router, OTHER).await;
    let granted = active_function(&router, GRANTED).await;
    let ungranted = active_function(&router, UNGRANTED).await;
    add_permission(&router, GRANTED).await;

    // The roles: one this api can assume and that may invoke the function, one it can assume
    // whose grant covers another function only, and one trusted by another service entirely.
    let allowed = role(
        &router,
        "non-proxy-allowed",
        APIGATEWAY_TRUST,
        policy(INVOKE_ACTION, &function),
    )
    .await;
    let narrow = role(
        &router,
        "non-proxy-narrow",
        APIGATEWAY_TRUST,
        policy(INVOKE_ACTION, &other),
    )
    .await;
    let untrusted = role(
        &router,
        "non-proxy-untrusted",
        EC2_TRUST,
        policy(INVOKE_ACTION, &function),
    )
    .await;

    let api_body = apigw_ok(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "lambda-non-proxy-integration" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api_body, "id");
    let root = member("CreateRestApi", &api_body, "rootResourceId");

    /// The invocation path of `function`, as `Integration$uri` names that action.
    fn invocation_uri(function: &str) -> String {
        format!("arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function}/invocations")
    }

    // One method per route, each naming the same action through a `uri`, a role and an event
    // template of its own.
    let routes: &[(&str, &str, Option<&str>, &str)] = &[
        // The function by ARN, under a role that may invoke it.
        ("by-arn", &function, Some(&allowed), EVENT_TEMPLATE),
        // The same function by NAME, under the same role: the spelling of the `uri` is what
        // differs, and the grant is read on the function either way.
        ("by-name", ECHO, Some(&allowed), EVENT_TEMPLATE),
        // The same function again, handed an event that asks the handler to raise.
        ("raises", &function, Some(&allowed), RAISING_EVENT_TEMPLATE),
        // A role whose grant names another function, so this call is refused by Lambda.
        ("narrow-grant", &function, Some(&narrow), EVENT_TEMPLATE),
        // A role another service's principal trusts, which this api cannot assume at all.
        ("untrusted-role", &function, Some(&untrusted), EVENT_TEMPLATE),
        // No role: the function's own resource policy admits API Gateway.
        ("resource-policy", &granted, None, EVENT_TEMPLATE),
        // No role and no grant on the function either.
        ("no-grant", &ungranted, None, EVENT_TEMPLATE),
    ];
    for (part, target, credentials, template) in routes {
        let resource_id = resource(&router, &api, &root, part).await;
        function_method(
            &router,
            &api,
            &resource_id,
            &invocation_uri(target),
            *credentials,
            template,
        )
        .await;
    }
    apigw_ok(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    let rows = [
        Row {
            case: "a trusted and granted role, with the function named by its ARN",
            route: "/by-arn",
            name: "ada",
            expected: Expected::Handled,
        },
        Row {
            case: "the same role with the function named by its name",
            route: "/by-name",
            name: "grace",
            expected: Expected::Handled,
        },
        Row {
            case: "a run that raised, matched by the pattern of another integration response",
            route: "/raises",
            name: "edsger",
            expected: Expected::Raised,
        },
        Row {
            case: "a trusted role granted the invocation of another function only",
            route: "/narrow-grant",
            name: "alan",
            expected: Expected::Refused,
        },
        Row {
            case: "a role trusted by another service's principal, which this api cannot assume",
            route: "/untrusted-role",
            name: "barbara",
            expected: Expected::Unperformed,
        },
        Row {
            case: "no role at all, with the function's own policy granting API Gateway the \
                   invocation",
            route: "/resource-policy",
            name: "katherine",
            expected: Expected::Handled,
        },
        Row {
            case: "no role at all and no grant on the function",
            route: "/no-grant",
            name: "margaret",
            expected: Expected::Unperformed,
        },
    ];

    // The call is made as a SESSION of the role the integration names, never as the role
    // itself, so that session is the identity Lambda's refusal names: a session ARN carries
    // the role's own name under the session name API Gateway opens one with, and no path.
    let narrow_session = format!(
        "arn:aws:sts::{account}:assumed-role/{role}/BackplaneAssumeRoleSession",
        account = engine::emulator_account_id(),
        role = narrow.rsplit('/').next().unwrap_or_default(),
    );

    for row in &rows {
        let answered = invoke(&router, &api, row.route, &json!({ "name": row.name })).await;
        let text = answered.text();
        match row.expected {
            Expected::Handled => {
                assert_eq!(
                    answered.status, 200,
                    "case {:?}: expected the handler's own 200, got {} with {text:?}",
                    row.case, answered.status
                );
                let document = answered.json();
                assert_eq!(
                    document["greeting"].as_str(),
                    Some(format!("hello {}", row.name).as_str()),
                    "case {:?}: expected the greeting the handler built from the event, got \
                     {text:?}",
                    row.case
                );
                // The event the run was handed is the INTEGRATION REQUEST the template
                // rendered, not the method request: it carries the name the template mapped
                // and nothing of the request around it.
                assert_eq!(
                    document["event"]["name"].as_str(),
                    Some(row.name),
                    "case {:?}: expected the event to carry the mapped name, got {text:?}",
                    row.case
                );
                assert!(
                    document["event"].get("httpMethod").is_none()
                        && document["event"].get("body").is_none(),
                    "case {:?}: a non-proxy integration hands the function its integration \
                     request, not the proxy event, got {text:?}",
                    row.case
                );
            }
            Expected::Raised => {
                assert_eq!(
                    answered.status, RAISED_STATUS,
                    "case {:?}: expected the status the integration response matching the \
                     reported error carries, got {} with {text:?}",
                    row.case, answered.status
                );
                // The body passed through is Lambda's own error document for the run.
                let document = answered.json();
                assert_eq!(
                    document["errorMessage"].as_str(),
                    Some(format!("deliberate {} failure", row.name).as_str()),
                    "case {:?}: expected the error document the run answered, got {text:?}",
                    row.case
                );
            }
            Expected::Refused => {
                assert_eq!(
                    answered.status, 403,
                    "case {:?}: expected Lambda's own refusal of the call the role may not \
                     make, got {} with {text:?}",
                    row.case, answered.status
                );
                assert_eq!(
                    answered.json()["Message"].as_str(),
                    Some(
                        format!(
                            "User: {narrow_session} is not authorized to perform: \
                             {INVOKE_ACTION} on resource: {function} because no identity-based \
                             policy allows the {INVOKE_ACTION} action"
                        )
                        .as_str()
                    ),
                    "case {:?}: expected the sentence IAM words an authorization failure in, \
                     got {text:?}",
                    row.case
                );
            }
            Expected::Unperformed => {
                assert_eq!(
                    answered.status, CONFIGURATION_ERROR_STATUS,
                    "case {:?}: expected the api's own integration-failure response, got {} \
                     with {text:?}",
                    row.case, answered.status
                );
                assert_eq!(
                    text, CONFIGURATION_ERROR_BODY,
                    "case {:?}: expected the API Gateway configuration-error body, got {text:?}",
                    row.case
                );
                // Nothing of a run reached the client, which is what says the function was
                // never invoked for this row.
                assert!(
                    !text.contains(row.name) && !text.contains("greeting"),
                    "case {:?}: a refused integration must invoke nothing, got {text:?}",
                    row.case
                );
            }
        }
    }
}
