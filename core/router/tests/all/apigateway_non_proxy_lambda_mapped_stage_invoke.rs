// SPDX-License-Identifier: CC0-1.0

//! A deployed REST method whose integration is a NON-PROXY `AWS` integration naming AWS
//! Lambda's function-invoking action, served through the stage invoke URL with the VTL
//! mappings that make such an integration what it is.
//!
//! The rule under test, one level above the recorded case:
//!
//! > A deployed method whose integration is of type `AWS` and whose `uri` names Lambda's
//! > function-invoking path is served by rendering the INTEGRATION REQUEST from the
//! > `requestTemplates` entry keyed by the media type the request declared
//! > (`Integration$requestTemplates` is "a map of Velocity templates that are applied on the
//! > request payload", and a template reads the method request's own parameters through
//! > `$input.params(…)`), invoking the function with EXACTLY that rendering as its event, and
//! > answering the body the selected `IntegrationResponse$responseTemplates` entry renders from
//! > what the run returned. WHO the call is made as is `Integration$credentials`: "to use
//! > resource-based permissions on supported Amazon Web Services services, specify null", so an
//! > integration naming no role has the call made as API Gateway's own service principal and the
//! > FUNCTION's resource-based policy is what admits it, narrowed by the invoking method's ARN as
//! > `aws:SourceArn`; an integration naming a role has the call made under that role, so the role
//! > must let `apigateway.amazonaws.com` assume it and its own policies must allow
//! > `lambda:InvokeFunction` on the function. Either way a correctly configured method answers
//! > HTTP 200 with the mapped body, and only an integration that could not be performed at all —
//! > a role this api cannot assume — answers the api's own configuration-error response, `500`
//! > with `{"message":"Internal server error"}`.
//!
//! The rows differ along what the rule generalises over: WHERE the request template reads the
//! value it mints from (a querystring parameter, a header, a path parameter), HOW MANY keys it
//! mints (one and two, which is what says the event is the template's rendering and not the
//! method request around it), WHAT the response template does with the run's answer (passes the
//! whole document through and picks one member of it), and WHICH records admit the invocation
//! (the function's own resource policy with no `credentials`, a role that trusts API Gateway and
//! may invoke the function, and a role another service's principal trusts).
//!
//! Three services have to agree for that to hold — Lambda holds the function, its resource
//! policy and the run; IAM holds the roles, their trust policies and their grants; API Gateway
//! holds the api, the integration, its templates and the deployed stage — so the suite lives
//! here rather than in any one of them.
//!
//! What the neighbouring suite pins and this one does not repeat:
//! `apigateway_lambda_non_proxy_service_integration` pins which records admit the invocation
//! over a template that reads the request BODY, and how a run that errored is mapped. What only
//! this suite reaches is the mapping itself over the method request's own PARAMETERS and the
//! integration response's `responseTemplates`, under both halves of the `credentials` choice.
//!
//! Every expected value is computed here from what the setup declared — the function ARN as
//! Lambda minted it, the role ARNs as IAM minted them, and the target each request's own
//! parameter carries — never out of the answer being checked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::form_encode;

/// The Region every request below is signed for, and the Region every ARN names.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the grant's `SourceArn` carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack the handler is deployed by, and the status it has to reach: inline source a
/// template states as `Code.ZipFile` is archived into the deployment package by CloudFormation,
/// which is how a handler written out in a suite comes to exist at all.
const STACK: &str = "apigw-non-proxy-mapped-handlers";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the api is deployed to.
const STAGE: &str = "test";

/// The media type the methods below declare, which is the key their integration request
/// template and their integration response template are stored under.
const JSON: &str = "application/json";

/// The service principal API Gateway invokes a function as when the integration names no role,
/// which is the principal the function's own resource policy grants.
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The IAM action an invocation of a function performs, which both the function's resource
/// policy and a `credentials` role's own policy have to name: `Invoke` "requires permission for
/// the `lambda:InvokeFunction` action".
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// A trust policy that lets API Gateway assume the role it is attached to.
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The same policy for ANOTHER service's principal: a role this api cannot assume, which is the
/// only thing that differs between it and the one above.
const EC2_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The status API Gateway answers an integration it could not perform at all with, and the body
/// that gateway response's default template carries.
const CONFIGURATION_ERROR_STATUS: u16 = 500;
const CONFIGURATION_ERROR_BODY: &str = r#"{"message":"Internal server error"}"#;

/// The function the rows with no `credentials` invoke, whose OWN resource policy admits API
/// Gateway, and the one the rows naming a role invoke, which carries no resource policy at all —
/// so a 200 from those rows is the role's doing and nothing else's.
const ADMITTED: &str = "non-proxy-mapped-admitted";
const ROLE_INVOKED: &str = "non-proxy-mapped-role-invoked";

/// The key the request templates below mint the greeting's target under, and the second key one
/// row's template mints beside it.
const TARGET_KEY: &str = "greetingTarget";
const SECOND_KEY: &str = "locale";

/// The handler every row invokes: it reports the keys of the EVENT it was handed — which for a
/// non-proxy integration is the integration request the template rendered — and greets the
/// target that event names, so a row observes both what reached the function and what it
/// returned.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {
        "receivedKeys": sorted(event.keys()),
        "message": "Hello, " + event.get("greetingTarget", "stranger"),
    }
"#;

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
    // the router can name the target service, and the invoke of the deployed stage carries none,
    // as a client of a public method sends none.
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

/// One identity-based policy allowing `action` on `resource`.
fn policy(action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
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
             &PolicyName=non-proxy-integration&PolicyDocument={}",
            form_encode(&document)
        ),
    )
    .await;
    arn
}

/// Deploy every function of `names` running [`HANDLER_SOURCE`], each as the one execution role
/// the stack declares, and wait until the stack reports itself created.
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package the create carries — the way a handler written out in a suite comes to
/// exist at all.
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
            // The template declares an IAM role, which CloudFormation creates only for a request
            // that acknowledges the capability ("Requires capabilities : [CAPABILITY_IAM]").
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

/// The ARN Lambda minted for `name`, read once that service first reports the function `Active`
/// — "the function is ready to be invoked" (`FunctionConfiguration$State`) — so an answer a row
/// reads later is the integration's and never the lifecycle's. The span waited for is that
/// service's own, so nothing here sleeps a fixed span on its behalf.
async fn active_function(router: &MultiServiceRouter, name: &str) -> String {
    let giving_up_at = Instant::now() + BUDGET;
    loop {
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
        assert!(
            Instant::now() < giving_up_at,
            "setup {name:?}: Lambda never reported the function Active"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Grant API Gateway the invocation of `function` through the function's OWN resource-based
/// policy — what admits a call the integration names no role for — narrowed by `source_arn` to
/// the methods of one api, as `AddPermission$SourceArn` scopes such a grant.
async fn add_permission(router: &MultiServiceRouter, function: &str, source_arn: &str) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke",
            "Action": INVOKE_ACTION,
            "Principal": APIGATEWAY_PRINCIPAL,
            "SourceArn": source_arn,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup {function:?}: expected HTTP 201 from Lambda AddPermission, got {status} \
         ({granted})"
    );
}

/// The chain of resources `path` names under the api's root, created one segment at a time.
/// Answers the id of the LAST one, which is the resource a method is put on.
async fn resource_chain(router: &MultiServiceRouter, api: &str, root: &str, path: &str) -> String {
    let mut parent = root.to_string();
    for part in path.split('/') {
        let case = format!("CreateResource({part})");
        let created = apigw_ok(
            router,
            &case,
            "POST",
            &format!("/restapis/{api}/resources/{parent}"),
            Some(json!({ "pathPart": part })),
            201,
        )
        .await;
        parent = member(&case, &created, "id");
    }
    parent
}

/// The invocation path of `function`, as `Integration$uri` names Lambda's function-invoking
/// action: `arn:aws:apigateway:{region}:lambda:path/{version}/functions/{arn}/invocations`.
fn invocation_uri(function: &str) -> String {
    format!("arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function}/invocations")
}

/// Which records admit a row's invocation: the `credentials` choice the integration states.
enum Admits {
    /// `credentials` is null, so the FUNCTION's own resource policy admits the call.
    ResourcePolicy,
    /// `credentials` names a role that trusts API Gateway and may invoke the function.
    TrustedRole,
    /// `credentials` names a role another service's principal trusts, so this api cannot assume
    /// it and the integration cannot be performed at all.
    UntrustedRole,
}

/// What a row expects the stage to answer.
enum Expected {
    /// The run's own document, passed through by `$input.json('$')`: the keys the event carried
    /// and the greeting the handler built from it.
    PassedThrough {
        keys: &'static [&'static str],
        target: &'static str,
    },
    /// One member of the run's answer, picked out by the response template.
    Picked { target: &'static str },
    /// The integration could not be performed at all, so the api answers its own
    /// configuration-error response and the function was never invoked.
    Unperformed,
}

/// One row: the method the setup declares and the request a client then sends it.
struct Row<'a> {
    case: &'static str,
    /// The resources the method hangs off, under the api's root, `/`-separated.
    path: &'static str,
    /// The path parameter the last segment declares, which the method has to name as a request
    /// parameter.
    path_parameter: Option<&'static str>,
    /// The integration request template, which renders the event the function is handed.
    request_template: &'a str,
    /// The integration response template, which renders the method response's body.
    response_template: &'a str,
    /// Which records admit the invocation.
    admits: Admits,
    /// The path under the stage the client requests, query string included.
    route: &'static str,
    /// The headers the client sends beside it.
    headers: &'static [(&'static str, &'static str)],
    expected: Expected,
}

/// The template that mints the greeting's target from one method request parameter named
/// `parameter`, which is the whole integration request: `$input.params(x)` "returns the method
/// request parameter value" for the path, query string or header named `x`.
fn one_key_template(parameter: &str) -> String {
    format!(r#"{{"{TARGET_KEY}": "$input.params('{parameter}')"}}"#)
}

/// The pass-through response template: `$input.json('$')` renders the whole document the back
/// end answered.
const PASS_THROUGH_TEMPLATE: &str = "$input.json('$')";

/// A response template that renders ONE member of what the back end answered instead of the
/// whole document.
const PICKED_TEMPLATE: &str = r#"{"greeting":$input.json('$.message')}"#;

/// Declare one method of `api` on `resource_id` as the row states it: a `GET` anyone may call,
/// integrating the function `uri` names under `credentials`, with the row's request template and
/// a `200` integration response carrying the row's response template.
async fn declare_method(
    router: &MultiServiceRouter,
    api: &str,
    resource_id: &str,
    row: &Row<'_>,
    uri: &str,
    credentials: Option<&str>,
) {
    let case = row.case;
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    let mut declaration = json!({ "authorizationType": "NONE" });
    if let Some(parameter) = row.path_parameter {
        declaration["requestParameters"] = json!({ format!("method.request.path.{parameter}"): true });
    }
    apigw_ok(router, case, "PUT", &method, Some(declaration), 201).await;
    let mut integration = json!({
        "type": "AWS",
        // `integrationHttpMethod` travels under its `jsonName`, which is what
        // `smithy_query(query="operation",operation="PutIntegration")` reports for it. Lambda's
        // invocation path is reached with POST whatever verb the METHOD carries.
        "httpMethod": "POST",
        "uri": uri,
        // The event the function is handed IS this rendering, and `WHEN_NO_TEMPLATES` is what
        // says so: a media type this map keys is mapped by its template, and nothing else is
        // passed through.
        "requestTemplates": { JSON: row.request_template },
        "passthroughBehavior": "WHEN_NO_TEMPLATES",
    });
    if let Some(credentials) = credentials {
        integration["credentials"] = json!(credentials);
    }
    apigw_ok(
        router,
        case,
        "PUT",
        &format!("{method}/integration"),
        Some(integration),
        201,
    )
    .await;
    apigw_ok(
        router,
        case,
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw_ok(
        router,
        case,
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "statusCode": "200",
            "responseTemplates": { JSON: row.response_template },
        })),
        201,
    )
    .await;
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` — carrying no credential at all and no
/// body, as a client of a public `GET` method sends it.
async fn invoke(router: &MultiServiceRouter, api: &str, row: &Row<'_>) -> Answer {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{}", row.route),
        row.headers,
        Bytes::new(),
    )
    .await
}

#[tokio::test]
async fn a_non_proxy_lambda_integration_serves_its_mapped_request_and_response() {
    let router = emulator();

    // Two functions running the same handler: the one a row with no `credentials` reaches, whose
    // own resource policy admits API Gateway, and the one every role-bearing row reaches, which
    // carries no resource policy at all — so a 200 from those rows is the role's doing.
    deploy_functions(&router, &[ADMITTED, ROLE_INVOKED]).await;
    let admitted = active_function(&router, ADMITTED).await;
    let role_invoked = active_function(&router, ROLE_INVOKED).await;

    // One role this api can assume that may invoke the role-reached function, and one another
    // service's principal trusts, which is the only thing that differs between them.
    let trusted = role(
        &router,
        "non-proxy-mapped-trusted",
        APIGATEWAY_TRUST,
        policy(INVOKE_ACTION, &role_invoked),
    )
    .await;
    let untrusted = role(
        &router,
        "non-proxy-mapped-untrusted",
        EC2_TRUST,
        policy(INVOKE_ACTION, &role_invoked),
    )
    .await;

    let api_body = apigw_ok(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "non-proxy-mapped-integration" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api_body, "id");
    let root = member("CreateRestApi", &api_body, "rootResourceId");

    let two_key_template = format!(
        r#"{{"{TARGET_KEY}": "$input.params('name')", "{SECOND_KEY}": "$input.params('locale')"}}"#
    );
    let querystring_template = one_key_template("name");
    let header_template = one_key_template("X-Target");
    let path_template = one_key_template("who");

    let rows = [
        Row {
            case: "a template minting one key from a querystring parameter, admitted by the \
                   function's own resource policy",
            path: "greet",
            path_parameter: None,
            request_template: &querystring_template,
            response_template: PASS_THROUGH_TEMPLATE,
            admits: Admits::ResourcePolicy,
            route: "/greet?name=Alice",
            headers: &[],
            expected: Expected::PassedThrough {
                keys: &[TARGET_KEY],
                target: "Alice",
            },
        },
        Row {
            case: "the same mapping under a role that trusts API Gateway and may invoke a \
                   function whose own policy grants nothing",
            path: "greet-role",
            path_parameter: None,
            request_template: &querystring_template,
            response_template: PASS_THROUGH_TEMPLATE,
            admits: Admits::TrustedRole,
            route: "/greet-role?name=Grace",
            headers: &[],
            expected: Expected::PassedThrough {
                keys: &[TARGET_KEY],
                target: "Grace",
            },
        },
        Row {
            case: "a template minting the key from a header parameter",
            path: "greet-header",
            path_parameter: None,
            request_template: &header_template,
            response_template: PASS_THROUGH_TEMPLATE,
            admits: Admits::ResourcePolicy,
            route: "/greet-header",
            headers: &[("x-target", "Katherine")],
            expected: Expected::PassedThrough {
                keys: &[TARGET_KEY],
                target: "Katherine",
            },
        },
        Row {
            case: "a template minting the key from a path parameter",
            path: "greet-path/{who}",
            path_parameter: Some("who"),
            request_template: &path_template,
            response_template: PASS_THROUGH_TEMPLATE,
            admits: Admits::TrustedRole,
            route: "/greet-path/Ada",
            headers: &[],
            expected: Expected::PassedThrough {
                keys: &[TARGET_KEY],
                target: "Ada",
            },
        },
        Row {
            case: "a template minting two keys, so the event is exactly what it rendered",
            path: "greet-two",
            path_parameter: None,
            request_template: &two_key_template,
            response_template: PASS_THROUGH_TEMPLATE,
            admits: Admits::ResourcePolicy,
            route: "/greet-two?name=Alan&locale=en-GB",
            headers: &[],
            expected: Expected::PassedThrough {
                keys: &[TARGET_KEY, SECOND_KEY],
                target: "Alan",
            },
        },
        Row {
            case: "a response template that picks one member of what the run answered",
            path: "greet-picked",
            path_parameter: None,
            request_template: &querystring_template,
            response_template: PICKED_TEMPLATE,
            admits: Admits::TrustedRole,
            route: "/greet-picked?name=Edsger",
            headers: &[],
            expected: Expected::Picked { target: "Edsger" },
        },
        Row {
            case: "a role another service's principal trusts, which this api cannot assume",
            path: "greet-untrusted",
            path_parameter: None,
            request_template: &querystring_template,
            response_template: PASS_THROUGH_TEMPLATE,
            admits: Admits::UntrustedRole,
            route: "/greet-untrusted?name=Barbara",
            headers: &[],
            expected: Expected::Unperformed,
        },
    ];

    for row in &rows {
        let resource_id = resource_chain(&router, &api, &root, row.path).await;
        let (uri, credentials) = match row.admits {
            Admits::ResourcePolicy => (invocation_uri(&admitted), None),
            Admits::TrustedRole => (invocation_uri(&role_invoked), Some(trusted.as_str())),
            Admits::UntrustedRole => (invocation_uri(&role_invoked), Some(untrusted.as_str())),
        };
        declare_method(&router, &api, &resource_id, row, &uri, credentials).await;
    }

    // The grant the function's own policy carries for the rows that name no role: every method
    // of THIS api, which is the `aws:SourceArn` such a grant is narrowed by.
    add_permission(
        &router,
        ADMITTED,
        &format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/*/*"),
    )
    .await;

    apigw_ok(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    for row in &rows {
        let answered = invoke(&router, &api, row).await;
        let text = answered.text();
        match &row.expected {
            Expected::PassedThrough { keys, target } => {
                assert_eq!(
                    answered.status, 200,
                    "case {:?}: expected the mapped 200, got {} with {text:?}",
                    row.case, answered.status
                );
                let document = answered.json();
                // The event the run was handed is the INTEGRATION REQUEST the template rendered:
                // exactly the keys it minted, and nothing of the proxy event around it.
                let reported: Vec<String> = document["receivedKeys"]
                    .as_array()
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|entry| entry.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let mut wanted: Vec<String> =
                    keys.iter().map(|key| (*key).to_string()).collect();
                wanted.sort();
                assert_eq!(
                    reported, wanted,
                    "case {:?}: expected the event to carry exactly the keys the request \
                     template minted {wanted:?}, got {text:?}",
                    row.case
                );
                assert_eq!(
                    document["message"].as_str(),
                    Some(format!("Hello, {target}").as_str()),
                    "case {:?}: expected the greeting the handler built from the mapped \
                     parameter, got {text:?}",
                    row.case
                );
            }
            Expected::Picked { target } => {
                assert_eq!(
                    answered.status, 200,
                    "case {:?}: expected the mapped 200, got {} with {text:?}",
                    row.case, answered.status
                );
                let document = answered.json();
                assert_eq!(
                    document["greeting"].as_str(),
                    Some(format!("Hello, {target}").as_str()),
                    "case {:?}: expected the member the response template picked, got {text:?}",
                    row.case
                );
                assert!(
                    document.get("receivedKeys").is_none(),
                    "case {:?}: expected only what the response template rendered, got {text:?}",
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
            }
        }
    }
}
