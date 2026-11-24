// SPDX-License-Identifier: CC0-1.0

//! What an SDK client running INSIDE a function reads out of an API Gateway REST
//! control-plane listing, including the listing a caller makes right after a management
//! call its execution role did not cover.
//!
//! > **RULE: every 200 answer of a paged listing of the API Gateway REST control plane
//! > carries its page member — the wire key `item`, which an SDK surfaces to its caller as
//! > `items` (`smithy_query(query="shape", shape="Deployments")`) — as a LIST of "the
//! > current page of elements from this collection". So an SDK client's INDEXED read of that
//! > member resolves for every collection of the plane and whatever the collection holds, a
//! > collection holding nothing answering the EMPTY list rather than an answer the member is
//! > missing from.**
//!
//! That is only observable where a real SDK parses the answer: a client writes the
//! effect-absent assertion after a refused write as one indexed read of the collection
//! member, and an answer that omits the member raises a key error in the client instead of
//! reporting that nothing is there. The suites inside the API Gateway crate read the raw wire
//! body, so this suite drives the other end — a handler holding a `boto3` client, whose own
//! calls are signed as its execution-role session — which is why it is a router suite: IAM
//! holds the role and the document on it, the token service mints the session, AWS Lambda runs
//! the handler and signs its calls, and API Gateway serves the listings.
//!
//! The rows span what the rule generalises over: WHICH collection is listed (an api's
//! deployments, its authorizers, its request validators, its models, its resources, and the
//! account's own apis), and HOW MANY elements it holds at the instant of the read — none at
//! all, the elements an api is created already holding, one element, and a collection emptied
//! again — read across three beats, so what is asserted is the member's presence whatever the
//! count and not one recorded listing.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region every ARN below names.
const REGION: &str = "us-east-1";

/// An access key id neither the token service nor IAM holds a record for: the position the
/// account's own published credential is in, which the setup calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The awsQuery API versions the setup services are called at.
const IAM_VERSION: &str = "2010-05-08";
const CFN_VERSION: &str = "2010-05-15";

/// The error shape and status an authorization refusal carries: IAM's own, in no operation's
/// modelled error set, because the decision is taken ahead of the service.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on this plane.
const BASIC_EXECUTION: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The stage the deployment the function makes names.
const STAGE: &str = "prod";

/// Where Lambda binds the collection a function is read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the probe function carries, how long one invocation is given, and the `State`
/// a function reports before it may be invoked. The timeout is well past what a run needs on
/// any host: what is under test is what the listings answer, not how fast an interpreter gets
/// through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;
const ACTIVE: &str = "Active";

/// The `StackStatus` the function's stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long an observation of work the CLOCK owes — a stack completing, a function going
/// `Active`, an invocation finding a run slot — waits before calling it a hang, and how long
/// between two looks. Generous, so a loaded host delays the setup instead of deciding the
/// outcome; each wait is on its own CONDITION rather than on a span.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One answered request.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    text: String,
}

impl Answered {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

/// A SigV4 `Authorization` whose credential names `access_key_id` and whose scope names
/// `signing_name` and [`REGION`], which is what routes the request and what Region the
/// decision is taken in. The signature itself is never checked here.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    access_key_id: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answered {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let response = router
        .dispatch(
            http::Request::builder()
                .method(method)
                .uri(format!("https://{host}{path}"))
                .header("host", &host)
                .header("authorization", authorization(access_key_id, signing_name))
                .header("content-type", content_type)
                .body(bytes::Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Answered {
        status,
        headers,
        text: String::from_utf8_lossy(response.body().as_ref()).to_string(),
    }
}

/// One awsQuery call that must be served, answered with its XML body.
async fn served_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        signing_name,
        ACCOUNT_KEY_ID,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        form.into_bytes(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "setup: {signing_name} {action} must answer 200, got {} with {}",
        answered.status, answered.text
    );
    answered.text
}

/// One API Gateway REST control-plane call the account's own credential must be served,
/// answered with its document.
async fn served_control_plane(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    expected_status: u16,
) -> Value {
    let answered = dispatch(
        router,
        "apigateway",
        ACCOUNT_KEY_ID,
        method,
        path,
        "application/json",
        body.map(|value| value.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await;
    assert_eq!(
        answered.status, expected_status,
        "setup: {what} must answer HTTP {expected_status}, got {} {}",
        answered.status, answered.text
    );
    answered.json()
}

/// A required non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what}: expected a non-empty {member:?}, got {body}"))
}

/// The ARN of an API Gateway resource whose URI path is `path`: this plane's ARNs carry an
/// EMPTY account field, and their resource portion IS the path of the operation that reaches
/// them.
fn apigateway_arn(path: &str) -> String {
    format!("arn:aws:apigateway:{REGION}::{path}")
}

/// An identity-based policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// The trust policy of a role one AWS service assumes.
fn service_trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The one name an inline document is put on a role under.
const INLINE_POLICY: &str = "grants";

/// Hold one role named `role_name` trusting `trust`, carrying the AWS-managed execution-role
/// policy and `inline` embedded in it, through IAM's own calls. Answers the role's ARN.
async fn given_role(router: &MultiServiceRouter, role_name: &str, inline: &Value) -> String {
    let created = served_query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name),
            (
                "AssumeRolePolicyDocument",
                &service_trust_policy(LAMBDA_PRINCIPAL),
            ),
        ],
    )
    .await;
    let role_arn = element(&created, "Arn");
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "AttachRolePolicy",
        &[("RoleName", role_name), ("PolicyArn", BASIC_EXECUTION)],
    )
    .await;
    put_role_policy(router, role_name, inline).await;
    role_arn
}

/// Put `document` on `role_name` as its one inline policy — what a client grants an existing
/// role a missing permission with.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &Value) {
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
}

/// One REST API carrying one `GET` method on its root resource, on a `MOCK` integration: an
/// api with a method on some resource is what a deployment can snapshot at all. Answers its id.
async fn api_with_a_method(router: &MultiServiceRouter, name: &str) -> String {
    let created = served_control_plane(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(&json!({ "name": name, "endpointConfiguration": { "types": ["REGIONAL"] } })),
        201,
    )
    .await;
    let api = as_str("setup: CreateRestApi", &created, "id");
    let root = as_str("setup: CreateRestApi", &created, "rootResourceId");
    served_control_plane(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{root}/methods/GET"),
        Some(&json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    served_control_plane(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{root}/methods/GET/integration"),
        Some(&json!({ "type": "MOCK" })),
        201,
    )
    .await;
    api
}

/// The handler the probe function runs: `import boto3` and the control-plane client at MODULE
/// scope — so a runtime shipping no SDK fails initialization rather than one of these calls —
/// then either the ONE management write the event names or the listings it names, each read
/// the way a client writing an effect-absent assertion reads one: the collection member
/// INDEXED, which raises rather than reporting a page where the answer omits it.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

APIGATEWAY = boto3.client("apigateway")


def listing(collection, api):
    if collection == "get_deployments":
        return APIGATEWAY.get_deployments(restApiId=api)
    if collection == "get_authorizers":
        return APIGATEWAY.get_authorizers(restApiId=api)
    if collection == "get_request_validators":
        return APIGATEWAY.get_request_validators(restApiId=api)
    if collection == "get_models":
        return APIGATEWAY.get_models(restApiId=api)
    if collection == "get_resources":
        return APIGATEWAY.get_resources(restApiId=api)
    if collection == "get_rest_apis":
        return APIGATEWAY.get_rest_apis()
    raise AssertionError("this handler lists no collection called " + collection)


def handler(event, context):
    try:
        if event["call"] == "create_deployment":
            created = APIGATEWAY.create_deployment(
                restApiId=event["api"], stageName=event["stage"]
            )
            return {"ok": True, "deployment_id": created["id"]}
        pages = {}
        for collection in event["collections"]:
            answered = listing(collection, event["api"])
            page = answered["items"]
            pages[collection] = {
                "count": len(page),
                "ids": [element.get("id") for element in page],
            }
        return {"ok": True, "pages": pages}
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "error_code": error.response["Error"]["Code"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"#;

/// The stack the probe function is deployed by, the role it runs as and its own name: the
/// inline source is archived into the deployment package by CloudFormation, which is how a
/// function whose handler is written here comes to exist at all.
const STACK: &str = "apigw-listing-probe-stack";
const EXECUTION_ROLE: &str = "apigw-listing-probe-role";
const FUNCTION: &str = "apigw-listing-probe";

/// The stack declaring the probe function.
fn function_stack(role_arn: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string()
}

/// Look again until the stack `stack` reports [`CREATE_COMPLETE`], and answer the last status
/// it reported. Waits on that CONDITION rather than on a span.
async fn wait_for_stack(router: &MultiServiceRouter, stack: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = served_query(
            router,
            "cloudformation",
            CFN_VERSION,
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE || Instant::now() >= giving_up_at {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until `function` reports [`ACTIVE`], which is the state it cannot be invoked
/// before. Waits on that CONDITION rather than on a span.
async fn wait_for_active(router: &MultiServiceRouter, function: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let answered = dispatch(
            router,
            "lambda",
            ACCOUNT_KEY_ID,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            "application/json",
            Vec::new(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "setup: GetFunctionConfiguration({function}) must be served, got {} {}",
            answered.status, answered.text
        );
        let configuration = answered.json();
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {function} never reported {ACTIVE}, last {last}");
}

/// One invocation of the probe function with `event`, as it came back.
async fn invoke_once(router: &MultiServiceRouter, event: &Value) -> Answered {
    dispatch(
        router,
        "lambda",
        ACCOUNT_KEY_ID,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        "application/json",
        event.to_string().into_bytes(),
    )
    .await
}

/// The status Lambda answers an invocation that found no run capacity with: "Lambda returns
/// `TooManyRequestsException` if running the function would cause you to exceed a concurrency
/// limit" (`smithy_query(query="operation", operation="Invoke")`).
const THROTTLED: u16 = 429;

/// Invoke the probe function with `event` and answer what the handler reported.
///
/// A host running many functions at once has only so many run slots, and an invocation that
/// finds none is answered the service's own capacity refusal ([`THROTTLED`]) — which is not
/// what is under test, so this looks again until one is not throttled, as a client retries a
/// throttled invocation. The wait is on that CONDITION, bounded by [`DEADLINE`], so a loaded
/// host delays this rather than deciding it; every other answer is asserted at once. A handler
/// that RAISED — which is what an answer omitting the collection member makes the indexed read
/// do — is reported by Lambda as a function error, and fails here naming the traceback.
async fn invoke(router: &MultiServiceRouter, case: &str, event: &Value) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut answered = invoke_once(router, event).await;
    while answered.status == THROTTLED && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        answered = invoke_once(router, event).await;
    }
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke({FUNCTION}), got {} {}",
        answered.status, answered.text
    );
    let signal = answered
        .headers
        .get("x-amz-function-error")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
    assert_eq!(
        signal, None,
        "case {case:?}: expected the handler's indexed read of the collection member to \
         resolve, got the function error {signal:?} with {}",
        answered.text
    );
    answered.json()
}

/// The collections the handler lists in one invocation, named by the client call that lists
/// each: an api's deployments — the collection a caller reads to prove a refused write created
/// nothing — three more of its children, the resources and models an api is created already
/// holding, and the account's own apis.
const COLLECTIONS: &[&str] = &[
    "get_deployments",
    "get_authorizers",
    "get_request_validators",
    "get_models",
    "get_resources",
    "get_rest_apis",
];

/// Assert the page `collection` reported holds exactly `ids`, read where the handler passed it
/// back. The member's PRESENCE is what the handler's indexed read already established; this is
/// the other half, that the count is the collection's own and not an empty page every listing
/// answers.
fn assert_page(reported: &Value, collection: &str, ids: &[String], case: &str) {
    let page = &reported["pages"][collection];
    let count = page["count"].as_u64().unwrap_or_else(|| {
        panic!("case {case:?}: expected {collection} to report a page size, got {reported}")
    });
    let listed: Vec<String> = page["ids"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        count,
        listed.len() as u64,
        "case {case:?}: expected {collection} to report as many elements as it counted, got \
         {page}"
    );
    if ids.is_empty() {
        assert_eq!(
            count, 0,
            "case {case:?}: expected {collection} to report the empty page, got {page}"
        );
    } else {
        assert_eq!(
            listed, ids,
            "case {case:?}: expected {collection} to report {ids:?}, got {page}"
        );
    }
}

/// The number of elements an api is created already holding, which is what the two rows the
/// api did not start empty must report: its root resource, and the two models
/// (`Error` and `Empty`) `CreateRestApi` seeds an api with.
const ROOT_RESOURCE: usize = 1;
const DEFAULT_MODELS: usize = 2;

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = ::aws_emulator_router::build_router(::aws_emulator_router::new_engine());
    // Signature verification belongs to the emulator's front door, not to this suite: what is
    // under test is what the listings answer. A handler's own calls are signed by the runtime
    // with the credential it was given.
    router.set_request_authentication(false);
    router
}

#[tokio::test]
async fn an_sdk_in_a_function_reads_the_page_member_of_every_listing_whatever_it_holds() {
    let router = emulator();
    let api = api_with_a_method(&router, "in-function-listing-page-member").await;
    let deployments_path = format!("/restapis/{api}/deployments");

    // The execution role as a client creates one for a handler that only READS the plane: the
    // AWS-managed execution-role policy and a grant of the read verb on this plane's paths,
    // and no statement covering a write.
    let role_arn = given_role(
        &router,
        EXECUTION_ROLE,
        &allow("apigateway:GET", &apigateway_arn("/restapis*")),
    )
    .await;
    served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &function_stack(&role_arn)),
        ],
    )
    .await;
    let reached = wait_for_stack(&router, STACK).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reached}"
    );
    wait_for_active(&router, FUNCTION).await;

    let write_event = json!({ "call": "create_deployment", "api": api, "stage": STAGE });
    let read_event = json!({ "call": "list", "api": api, "collections": COLLECTIONS });

    // The write no document in force on the role covers, which is the call the effect-absent
    // read below is made after.
    let case = "an ungranted function deploying the api";
    let refused = invoke(&router, case, &write_event).await;
    assert_eq!(
        (
            refused["ok"].clone(),
            refused["error_code"].clone(),
            refused["status"].clone(),
        ),
        (
            json!(false),
            json!(ACCESS_DENIED),
            json!(ACCESS_DENIED_STATUS),
        ),
        "case {case:?}: expected the write to be refused {ACCESS_DENIED_STATUS} \
         {ACCESS_DENIED}, got {refused}"
    );

    // Beat one: every collection read right after that refusal. The deployments the refused
    // write asked for are not there, and neither is anything in the api's other empty
    // collections — each of which answers the member the handler indexed, as the empty list.
    let case = "the collections of an api after a refused deployment";
    let reported = invoke(&router, case, &read_event).await;
    assert_eq!(
        reported["ok"],
        json!(true),
        "case {case:?}: expected the granted listings to be served, got {reported}"
    );
    for collection in ["get_deployments", "get_authorizers", "get_request_validators"] {
        assert_page(&reported, collection, &[], case);
    }
    // The two collections an api is created already holding, counted rather than named: the
    // ids are the service's own, and what the rule is about is that the member is a list of
    // what the collection holds.
    assert_eq!(
        reported["pages"]["get_resources"]["count"],
        json!(ROOT_RESOURCE),
        "case {case:?}: expected the api's resources to report its root resource, got {reported}"
    );
    assert_eq!(
        reported["pages"]["get_models"]["count"],
        json!(DEFAULT_MODELS),
        "case {case:?}: expected the api's models to report the models it was created with, \
         got {reported}"
    );
    let apis = reported["pages"]["get_rest_apis"]["ids"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        apis.contains(&api),
        "case {case:?}: expected the account's apis to report {api:?}, got {apis:?}"
    );

    // Beat two: the grant a client then writes on the SAME role, so the identical write is
    // served — and the deployments collection reports the one deployment the handler made,
    // through the very member that answered the empty list a moment ago.
    put_role_policy(
        &router,
        EXECUTION_ROLE,
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                { "Effect": "Allow", "Action": "apigateway:GET", "Resource": apigateway_arn("/restapis*") },
                {
                    "Effect": "Allow",
                    "Action": "apigateway:POST",
                    "Resource": apigateway_arn(&format!("{deployments_path}*")),
                },
            ],
        }),
    )
    .await;

    let case = "a granted function deploying the api";
    let deployed = invoke(&router, case, &write_event).await;
    assert_eq!(
        deployed["ok"],
        json!(true),
        "case {case:?}: expected the granted deployment to be served, got {deployed}"
    );
    let deployment = as_str(&format!("case {case:?}"), &deployed, "deployment_id");

    let case = "the collections of an api holding one deployment";
    let reported = invoke(&router, case, &read_event).await;
    assert_page(
        &reported,
        "get_deployments",
        std::slice::from_ref(&deployment),
        case,
    );
    for collection in ["get_authorizers", "get_request_validators"] {
        assert_page(&reported, collection, &[], case);
    }

    // Beat three: the collection EMPTIED again — the stage that references the deployment
    // dropped, then the deployment itself — which is the second way a page comes to hold
    // nothing, and is answered the same present-and-empty member as the first.
    served_control_plane(
        &router,
        "DeleteStage",
        "DELETE",
        &format!("/restapis/{api}/stages/{STAGE}"),
        None,
        202,
    )
    .await;
    served_control_plane(
        &router,
        "DeleteDeployment",
        "DELETE",
        &format!("{deployments_path}/{deployment}"),
        None,
        202,
    )
    .await;

    let case = "the collections of an api emptied of its deployment";
    let reported = invoke(&router, case, &read_event).await;
    assert_page(&reported, "get_deployments", &[], case);
    assert_eq!(
        reported["pages"]["get_resources"]["count"],
        json!(ROOT_RESOURCE),
        "case {case:?}: expected the api's resources to be unchanged by the deletes, got \
         {reported}"
    );
}
