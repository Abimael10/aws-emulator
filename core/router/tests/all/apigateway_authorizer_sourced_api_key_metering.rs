// SPDX-License-Identifier: CC0-1.0

//! Which API KEY a deployed REST stage admits and meters a key-required method's request by,
//! on the emulator as it is ASSEMBLED.
//!
//! RULE: **`RestApi$apiKeySource` names WHERE the API key of a metered request is read from —
//! "the source of the API key for metering requests according to a usage plan. Valid values
//! are: `HEADER` to read the API key from the `X-API-Key` header of a request. `AUTHORIZER`
//! to read the API key from the `UsageIdentifierKey` from a custom authorizer" — so for an
//! api whose source is `AUTHORIZER` the value the method's custom authorizer RETURNED is the
//! only key the call presents and the `X-API-Key` header decides nothing, and for an api whose
//! source is `HEADER` the header's value is the only key it presents and what the authorizer
//! returned decides nothing. Whichever source it came from, the value admits the request only
//! when it is the value of an ENABLED stored key a usage plan over this api stage associates —
//! and that plan then METERS the call, so the call spends that key's quota — and a value no
//! such key carries is refused `403` through the api's `INVALID_API_KEY` gateway response.**
//!
//! Three crates have to agree for that to hold — IAM holds the execution role, Lambda archives
//! the authorizer's inline source into a package it can run and RUNS it, and API Gateway
//! resolves the stage, asks the authorizer and takes the admission — so the suite lives here
//! rather than in any one of them. CloudFormation archives the inline handler, which is how a
//! function whose source is written here comes to exist at all.
//!
//! The rows differ along what the rule generalises over: the api's own `apiKeySource`
//! (`AUTHORIZER`, and then `HEADER` after an `UpdateRestApi` replaces it and a redeploy), WHAT
//! the authorizer returned as its `usageIdentifierKey` (an enabled key a plan associates with
//! this api stage, an enabled key no plan associates, a DISABLED key a plan associates, a
//! syntactically valid value no stored key carries, and no member at all), and what the
//! request carried in `X-API-Key` (nothing, the associated key, an unassociated key, a value no
//! key carries) — so each source is driven with the OTHER source's value both present and
//! absent. A last pair spends a one-request quota through a key only the authorizer named,
//! which is what shows the admitted call was metered against that key rather than merely let
//! through.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling router suites do.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region of the function and of the
/// api's own invoke host. It is the Region the shared stack-settling helper
/// ([`wait_for_stack_status`]) reads a stack in.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the grant's `SourceArn` carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that archives the inline authorizer source, and the status it must reach.
const STACK: &str = "xsvc-authorizer-metered-key";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The role the function runs as, and the service that assumes it.
const EXECUTION_ROLE: &str = "xsvc-authorizer-metered-key-role";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The authorizer's function, its runtime and the handler the archived package declares.
const FUNCTION: &str = "xsvc-authorizer-metered-key-authorizer";
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The stage the api is deployed to, and the resource the metered method sits on.
const STAGE: &str = "live";
const PATH_PART: &str = "metered";

/// The header the authorizer declares as its identity source, and the value every request
/// below carries there: `Authorizer$identitySource` is "the identity source for which
/// authorization is requested", and a `REQUEST` authorizer is not called at all for a request
/// carrying none of its sources.
const IDENTITY_HEADER: &str = "authorization";
const IDENTITY_VALUE: &str = "caller-token";

/// The header the authorizer's own code reads to decide WHICH `usageIdentifierKey` to return,
/// so a row drives the authorizer's answer without a second function per row. It is not an API
/// key header and nothing in the data plane reads it.
const USAGE_KEY_HEADER: &str = "x-usage-key";

/// The header `apiKeySource` of `HEADER` reads the key from, as that member documents it.
const API_KEY_HEADER: &str = "x-api-key";

/// `Authorizer$authorizerResultTtlInSeconds` of zero: "if it equals 0, authorization caching
/// is disabled", so every row's own request reaches the function and gets its own answer.
const NO_CACHING: u32 = 0;

/// The one key type a `UsagePlanKey` takes: "Currently, the valid key type is `API_KEY`".
const API_KEY_TYPE: &str = "API_KEY";

/// How long a clock-owed transition another service owes (a created function becoming
/// `Active`) is waited for, and how often it is looked for. The wait is on the CONDITION.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The principal the authorizer names for every caller, which the integration's own response
/// template reads back — so an admitted row's body shows the authorizer really decided it.
const PRINCIPAL: &str = "metered-caller";

/// The body the method's `MOCK` integration answers an admitted call with, rendered from the
/// integration response template below.
fn served_body() -> String {
    format!("{{\"served\":\"the integration ran\",\"principal\":\"{PRINCIPAL}\"}}")
}

/// The body the api's `INVALID_API_KEY` gateway response renders, whose
/// `$context.error.responseType` substitutes to the fixed text of the type the refusal is
/// answered through.
const REFUSED_BODY: &str = "{\"refused\":\"INVALID_API_KEY\"}";

/// The template that renders it; see [`REFUSED_BODY`].
const REFUSED_TEMPLATE: &str = "{\"refused\":\"$context.error.responseType\"}";

/// The authorizer's own code: it ALLOWS every call to the method it was asked about, names one
/// principal, and returns as its `usageIdentifierKey` whatever the request carried at
/// [`USAGE_KEY_HEADER`] — returning the member not at all for a request carrying nothing
/// there, which is the authorizer that identifies no usage.
const AUTHORIZER_HANDLER: &str = r#"
def _header(event, name):
    for key, value in (event.get("headers") or {}).items():
        if key.lower() == name:
            return value
    return None


def handler(event, context):
    returned = {
        "principalId": "metered-caller",
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Action": "execute-api:Invoke",
                    "Effect": "Allow",
                    "Resource": event["methodArn"],
                }
            ],
        },
    }
    identified = _header(event, "x-usage-key")
    if identified:
        returned["usageIdentifierKey"] = identified
    return returned
"#;

/// One answered request: its status and its body.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The status and the body, for an assertion's message.
    fn summary(&self) -> String {
        format!("HTTP {} with {:?}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: the credential scope
    // is what routes an SDK request, and an invoke of the deployed stage carries none at all,
    // as a client of that URL sends none.
    router.set_request_authentication(false);
    router
}

/// The access key id this emulator publishes, which speaks for the account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// A SigV4 `Authorization` whose credential scope names `signing_name`. The signature itself
/// is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
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
) -> Answered {
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
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One awsQuery call of `service` that has to be accepted, answered with its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &format!("{service}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(service)),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: {service} {action} must answer 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status,
        expected,
        "setup {what:?}: must answer HTTP {expected}, got {}",
        answered.summary()
    );
    answered.json()
}

/// A required, non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// One request to the deployed stage's invoke endpoint — the api's own
/// `{api-id}.execute-api.{region}.amazonaws.com` host — carrying `headers` and no credential,
/// as a client of a custom-authorized method sends none.
async fn invoke(router: &MultiServiceRouter, api: &str, headers: &[(&str, &str)]) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}/{PATH_PART}"),
        headers,
        Bytes::new(),
    )
    .await
}

/// Create the execution role the function runs as, answering the ARN IAM holds it under.
async fn create_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": LAMBDA_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({EXECUTION_ROLE:?}) must answer an ARN"))
}

/// Deploy the stack declaring the authorizer's function: the inline source is archived into a
/// deployment package by CloudFormation, which is how a function whose handler is written here
/// comes to exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deploy_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Authorizer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Code": { "ZipFile": AUTHORIZER_HANDLER },
                },
            },
        },
    })
    .to_string();
    accepted_query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described}"
    );
    active_function_arn(router).await
}

/// The ARN Lambda reports for the function, once it reports it `Active` — a transition that
/// service owes, so this looks until it reports it rather than sleeping a span on its behalf.
async fn active_function_arn(router: &MultiServiceRouter) -> String {
    let deadline = Instant::now() + BUDGET;
    let configuration = loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if reported.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup: Lambda must report {FUNCTION:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Grant `apigateway.amazonaws.com` the invocation of the function, scoped to the AUTHORIZER
/// whose function it is — the `aws:SourceArn` such a grant is narrowed by.
async fn grant(router: &MultiServiceRouter, api: &str, authorizer: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke-the-authorizer",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/authorizers/{authorizer}"
            ),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission({FUNCTION:?}) must answer 201, got {}",
        granted.summary()
    );
}

/// The api every row is served by: its id, and the usage plans and keys the records hold.
struct Metered {
    api: String,
    /// An enabled key a usage plan over this api stage associates.
    associated: String,
    /// An enabled key NO usage plan associates.
    unassociated: String,
    /// A DISABLED key a usage plan over this api stage associates.
    disabled: String,
    /// An enabled key whose own plan admits ONE request a day, for the metering leg.
    rationed: String,
}

/// A distinct API key value of the 40 characters AWS mints: a key's value is unique per
/// account, so no two keys here may carry the same one.
fn key_value(n: u8) -> String {
    format!("{}{n:02}", "k".repeat(38))
}

/// A value of that same shape that no stored key carries.
fn unknown_key_value() -> String {
    format!("{}99", "z".repeat(38))
}

/// Stand the api up: the authorizer, the key-required `CUSTOM` method behind a `MOCK`
/// integration, the `INVALID_API_KEY` gateway response, the deployment, and the keys and plans.
async fn metered_api(router: &MultiServiceRouter, function_arn: &str) -> Metered {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({
            "name": "authorizer-sourced-api-key",
            "apiKeySource": "AUTHORIZER",
            "endpointConfiguration": { "types": ["REGIONAL"] },
        })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &created, "id");
    let root = as_str("CreateRestApi", &created, "rootResourceId");
    // The create's own report of the source, read back through the reader of the record.
    assert_source(router, &api, "AUTHORIZER").await;

    let authorizer = apigw(
        router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "metered-key-authorizer",
            "type": "REQUEST",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
            "identitySource": format!("method.request.header.{IDENTITY_HEADER}"),
            "authorizerResultTtlInSeconds": NO_CACHING,
        })),
        201,
    )
    .await;
    let authorizer = as_str("CreateAuthorizer", &authorizer, "id");
    grant(router, &api, &authorizer).await;

    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": PATH_PART })),
        201,
    )
    .await;
    let resource = as_str("CreateResource", &resource, "id");
    let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({
            "authorizationType": "CUSTOM",
            "authorizerId": authorizer,
            "apiKeyRequired": true,
        })),
        201,
    )
    .await;
    // A `MOCK` integration, so what an ADMITTED call is answered with is the api's own records
    // and nothing else: the key resolution is the only thing the rows vary.
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": r#"{"statusCode": 200}"# },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "responseTemplates": {
                "application/json": "{\"served\":\"the integration ran\",\
                                      \"principal\":\"$context.authorizer.principalId\"}",
            },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutGatewayResponse",
        "PUT",
        &format!("/restapis/{api}/gatewayresponses/INVALID_API_KEY"),
        Some(json!({
            "responseTemplates": { "application/json": REFUSED_TEMPLATE },
        })),
        201,
    )
    .await;
    deploy(router, &api).await;

    // The keys, each differing from the admitting one in exactly one property, and the plans
    // that associate them with this api stage.
    let associated = key_value(1);
    let unassociated = key_value(2);
    let disabled = key_value(3);
    let rationed = key_value(4);
    let associated_id = create_key(router, "associated", &associated, true).await;
    create_key(router, "unassociated", &unassociated, true).await;
    let disabled_id = create_key(router, "disabled", &disabled, false).await;
    let rationed_id = create_key(router, "rationed", &rationed, true).await;
    meter(
        router,
        "plan-over-this-api-stage",
        &api,
        &[&associated_id, &disabled_id],
        None,
    )
    .await;
    // "The target maximum number of requests that can be made in a given time period"
    // (`QuotaSettings$limit`): a plan admitting ONE request a day, so the SECOND call metered
    // against this key has no request left.
    meter(
        router,
        "plan-rationed-to-one-request",
        &api,
        &[&rationed_id],
        Some(json!({ "limit": 1, "period": "DAY" })),
    )
    .await;

    Metered {
        api,
        associated,
        unassociated,
        disabled,
        rationed,
    }
}

/// `GetRestApi` must report `wanted` as the api's `apiKeySource`.
async fn assert_source(router: &MultiServiceRouter, api: &str, wanted: &str) {
    let read = apigw(
        router,
        "GetRestApi",
        "GET",
        &format!("/restapis/{api}"),
        None,
        200,
    )
    .await;
    assert_eq!(
        read.get("apiKeySource").and_then(Value::as_str),
        Some(wanted),
        "setup: GetRestApi must report apiKeySource {wanted:?}, got {read}"
    );
}

/// `CreateDeployment` of the api into [`STAGE`], which is what the invoke URL serves.
async fn deploy(router: &MultiServiceRouter, api: &str) {
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
}

/// `CreateApiKey` for `value`, answering the key's id.
async fn create_key(
    router: &MultiServiceRouter,
    name: &str,
    value: &str,
    enabled: bool,
) -> String {
    let created = apigw(
        router,
        "CreateApiKey",
        "POST",
        "/apikeys",
        Some(json!({ "name": name, "value": value, "enabled": enabled })),
        201,
    )
    .await;
    as_str("CreateApiKey", &created, "id")
}

/// `CreateUsagePlan` over this api stage with `keys` added to it, which is how a client
/// associates an API key with an API stage — the association that admits and meters a call.
async fn meter(router: &MultiServiceRouter, name: &str, api: &str, keys: &[&str], quota: Option<Value>) {
    let mut request = json!({
        "name": name,
        "apiStages": [{ "apiId": api, "stage": STAGE }],
    });
    if let Some(quota) = quota {
        request["quota"] = quota;
    }
    let plan = apigw(router, "CreateUsagePlan", "POST", "/usageplans", Some(request), 201).await;
    let plan = as_str(name, &plan, "id");
    for key in keys {
        apigw(
            router,
            "CreateUsagePlanKey",
            "POST",
            &format!("/usageplans/{plan}/keys"),
            Some(json!({ "keyId": key, "keyType": API_KEY_TYPE })),
            201,
        )
        .await;
    }
}

/// WHICH stored key a row presents at a source, as the records hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// Nothing at all at that source.
    Nothing,
    /// The enabled key a plan over this api stage associates.
    Associated,
    /// An enabled key no plan associates.
    Unassociated,
    /// A disabled key a plan over this api stage associates.
    Disabled,
    /// A value of a key's own shape that no stored key carries.
    Unknown,
}

impl Presented {
    /// The value this presents, or `None` for [`Presented::Nothing`].
    fn value(self, metered: &Metered) -> Option<String> {
        match self {
            Presented::Nothing => None,
            Presented::Associated => Some(metered.associated.clone()),
            Presented::Unassociated => Some(metered.unassociated.clone()),
            Presented::Disabled => Some(metered.disabled.clone()),
            Presented::Unknown => Some(unknown_key_value()),
        }
    }
}

/// One row: what the authorizer returns, what the request header carries, and whether the
/// stage ADMITS the call.
struct Row {
    case: &'static str,
    /// The `usageIdentifierKey` the authorizer's function returns for this request.
    returned: Presented,
    /// The value the request carries in `X-API-Key`.
    header: Presented,
    /// Whether the key the api's own source names admits the call.
    admitted: bool,
}

/// The table for an api whose `apiKeySource` is `AUTHORIZER`: the authorizer's returned key
/// decides, and the header decides nothing — which the rows drive with the header absent,
/// valid, unassociated and unknown.
const SOURCED_FROM_THE_AUTHORIZER: &[Row] = &[
    Row {
        case: "the authorizer returns an associated key and the request carries no header",
        returned: Presented::Associated,
        header: Presented::Nothing,
        admitted: true,
    },
    Row {
        case: "the authorizer returns an associated key and the header carries an unknown value",
        returned: Presented::Associated,
        header: Presented::Unknown,
        admitted: true,
    },
    Row {
        case: "the authorizer returns an associated key and the header carries an unassociated \
               key",
        returned: Presented::Associated,
        header: Presented::Unassociated,
        admitted: true,
    },
    Row {
        case: "the authorizer returns an unknown value and the request carries no header",
        returned: Presented::Unknown,
        header: Presented::Nothing,
        admitted: false,
    },
    Row {
        case: "the authorizer returns an unknown value and the header carries the associated key",
        returned: Presented::Unknown,
        header: Presented::Associated,
        admitted: false,
    },
    Row {
        case: "the authorizer returns no key at all and the header carries the associated key",
        returned: Presented::Nothing,
        header: Presented::Associated,
        admitted: false,
    },
    Row {
        case: "the authorizer returns an enabled key no plan associates",
        returned: Presented::Unassociated,
        header: Presented::Nothing,
        admitted: false,
    },
    Row {
        case: "the authorizer returns a disabled key a plan associates",
        returned: Presented::Disabled,
        header: Presented::Nothing,
        admitted: false,
    },
];

/// The same table for an api whose `apiKeySource` is `HEADER`: every verdict above inverts,
/// because the header decides and the authorizer's returned key decides nothing.
const SOURCED_FROM_THE_HEADER: &[Row] = &[
    Row {
        case: "the header carries the associated key and the authorizer returns no key",
        returned: Presented::Nothing,
        header: Presented::Associated,
        admitted: true,
    },
    Row {
        case: "the header carries the associated key and the authorizer returns an unknown value",
        returned: Presented::Unknown,
        header: Presented::Associated,
        admitted: true,
    },
    Row {
        case: "the header carries no key and the authorizer returns an associated key",
        returned: Presented::Associated,
        header: Presented::Nothing,
        admitted: false,
    },
    Row {
        case: "the header carries an unassociated key and the authorizer returns an associated \
               key",
        returned: Presented::Associated,
        header: Presented::Unassociated,
        admitted: false,
    },
    Row {
        case: "the header carries a disabled key and the authorizer returns an associated key",
        returned: Presented::Associated,
        header: Presented::Disabled,
        admitted: false,
    },
];

/// Drive one table against the deployed stage, asserting each row's own verdict.
async fn run(router: &MultiServiceRouter, metered: &Metered, source: &str, rows: &[Row]) {
    for row in rows {
        let returned = row.returned.value(metered);
        let header = row.header.value(metered);
        let mut headers: Vec<(&str, &str)> = vec![(IDENTITY_HEADER, IDENTITY_VALUE)];
        if let Some(returned) = returned.as_deref() {
            headers.push((USAGE_KEY_HEADER, returned));
        }
        if let Some(header) = header.as_deref() {
            headers.push((API_KEY_HEADER, header));
        }
        let answered = invoke(router, &metered.api, &headers).await;
        let (status, body) = if row.admitted {
            (200, served_body())
        } else {
            (403, REFUSED_BODY.to_string())
        };
        assert_eq!(
            answered.status,
            status,
            "case {source:?}/{:?}: expected HTTP {status}, got {}",
            row.case,
            answered.summary()
        );
        assert_eq!(
            answered.text(),
            body,
            "case {source:?}/{:?}: expected {body:?}, got {:?}",
            row.case,
            answered.text()
        );
    }
}

/// RULE: `apiKeySource` names where the key of a metered request is read from, the key read
/// there admits the call only when a usage plan over this api stage associates that enabled
/// key, and the plan then meters the call.
#[test]
fn the_api_key_source_names_which_key_admits_and_meters_a_metered_method() {
    std::thread::Builder::new()
        .name("apigateway-authorizer-sourced-key".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on (`core/router/src/main.rs`),
            // because one invoke here drives a nested in-process call on it: the authorizer's
            // own invocation.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a multi-thread runtime");
            runtime.block_on(suite());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}

async fn suite() {
    let router = emulator();
    let role_arn = create_role(&router).await;
    let function_arn = deploy_function(&router, &role_arn).await;
    let metered = metered_api(&router, &function_arn).await;

    // 1. The api reads its key from the authorizer.
    run(
        &router,
        &metered,
        "AUTHORIZER",
        SOURCED_FROM_THE_AUTHORIZER,
    )
    .await;

    // 2. The key the authorizer named is METERED, not merely admitted: the plan behind the
    //    rationed key admits one request a day, so the second call the authorizer identifies
    //    that key for has no request left and is answered through `QUOTA_EXCEEDED`.
    let rationed: Vec<(&str, &str)> = vec![
        (IDENTITY_HEADER, IDENTITY_VALUE),
        (USAGE_KEY_HEADER, metered.rationed.as_str()),
    ];
    let first = invoke(&router, &metered.api, &rationed).await;
    assert_eq!(
        first.status,
        200,
        "case \"the first call metered against the rationed key\": expected HTTP 200, got {}",
        first.summary()
    );
    let second = invoke(&router, &metered.api, &rationed).await;
    assert_eq!(
        second.status,
        429,
        "case \"the second call metered against the rationed key\": expected the quota to be \
         spent, got {}",
        second.summary()
    );

    // 3. `UpdateRestApi` replaces the source and the api is redeployed, which inverts every
    //    verdict: the header decides and the authorizer's returned key decides nothing.
    apigw(
        &router,
        "UpdateRestApi",
        "PATCH",
        &format!("/restapis/{}", metered.api),
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/apiKeySource", "value": "HEADER" },
            ],
        })),
        200,
    )
    .await;
    assert_source(&router, &metered.api, "HEADER").await;
    deploy(&router, &metered.api).await;
    run(&router, &metered, "HEADER", SOURCED_FROM_THE_HEADER).await;
}
