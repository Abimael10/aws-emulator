// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function's own `sns:Publish` reaches, however the handler came
//! by its client: the publication is carried out and fans out to the `sqs`-protocol
//! subscriber, and the invocation that made it is HTTP 200 with no function error.
//!
//! RULE: a managed `python3.x` runtime puts the AWS SDK on the handler's import path and
//! exports the TEMPORARY credentials of the function's execution-role session into the run's
//! environment, so EVERY client the handler builds out of that SDK — at module scope or inside
//! the handler, from `boto3.client`, from a `boto3.Session`, with a Region or a
//! `botocore.config.Config` of its own, or with those exported credentials passed in by hand —
//! is signed as that session; a `Publish` the session's policies permit is therefore carried
//! out by Amazon SNS (`Publish(Message required, TopicArn) -> {MessageId}`, whose only modelled
//! refusals are `AuthorizationErrorException`, `NotFoundException`,
//! `InvalidParameterException` and the KMS family —
//! `smithy_query(query="operation", operation="Publish")`), the message takes the topic's normal
//! delivery path so each confirmed `sqs`-protocol subscription whose queue policy admits the
//! service principal receives one notification envelope naming that topic and that id and
//! carrying the published body VERBATIM as its `Message`, and the invocation answers 200 with no
//! `X-Amz-Function-Error`, because what decides that header is whether the handler returned
//! (`InvocationResponse$FunctionError`). None of that depends on how the deployment package
//! reached Lambda.
//!
//! Five crates have to agree for it to hold — CloudFormation archives each inline handler into a
//! deployment package and deploys the stack, IAM holds the execution role and its inline
//! document, the token service vends the session the run is given, Lambda runs the handler and
//! supplies it the SDK, Amazon SNS takes the publication and fans it out, and Amazon SQS holds
//! what arrived — so the suite lives here: a crate-local suite sees one side of it only.
//!
//! What the neighbours pin and this one does not repeat:
//! [`super::lambda_handler_publish_reaches_every_topic_subscriber`] drives the BYTES of the
//! published body through one module-scope client and on to two subscribers,
//! [`super::lambda_handler_fans_out_to_sns_sqs_and_eventbridge`] drives one body to three
//! destination services and the refusal each answers an ungranted role with (Amazon SNS's is
//! `AuthorizationError`, HTTP 403), and
//! [`super::lambda_python_runtime_bundles_the_aws_sdk`] drives the runtimes the SDK is on the
//! import path of. What only this suite reaches is WHERE the handler's client comes from and how
//! its package was uploaded — the two things a handler author chooses freely and the rule must
//! be indifferent to.
//!
//! The rows differ along exactly that: the construction the client is obtained by (a module-scope
//! client reused across invocations, one built per call, one built from an explicit session, one
//! built for the Region the runtime reports, one configured through the SDK's own `Config`, and
//! one built with the credentials the runtime exported), and — for the last row — whether the
//! package was archived by a template or uploaded straight to `CreateFunction`. Each row
//! publishes a body of its own, so a notification is matched to the invocation that published it
//! rather than to any publication at all.
//!
//! Nothing here sleeps for a delivery: it polls for the condition — a stack settling, a function
//! becoming invocable, a notification arriving — and the closing "nothing else was queued" read
//! is taken at the instant a message published through the same path AFTER the last invocation
//! has arrived, rather than after a span of the clock.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::{element, form_encode};

/// The Region every request here is signed for, and so the Region the topic, the queue, the role
/// and the functions belong to. The shared CloudFormation wait reads the stack in this Region.
const REGION: &str = "us-east-1";

/// Where Lambda binds the function collection, each configuration read and each invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The topic the handlers publish to, which is also the name of the queue confirmed on it.
const TOPIC: &str = "xsvc-infn-publish-topic";

/// The execution role every function below runs as, and the stack deploying the functions.
const ROLE: &str = "xsvc-infn-publish-role";
const STACK: &str = "xsvc-infn-publish";

/// The module and attribute each deployment package carries the handler as: the names a
/// console-authored Python function is written under, and so the file stem CloudFormation
/// archives the inline source as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime every function is created with, and the `State` Lambda serves invocations in.
const RUNTIME: &str = "python3.12";
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` the stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The action the handlers' own call performs, which the execution role's inline document grants
/// on the topic alone.
const PUBLISH_ACTION: &str = "sns:Publish";

/// The `Type` member an Amazon SNS delivery to a `sqs`-protocol subscription carries.
const NOTIFICATION: &str = "Notification";

/// The status an accepted publication answers with, as the handler reads it off the SDK's
/// `ResponseMetadata`.
const PUBLISH_STATUS: u64 = 200;

/// The timeout every function is created with. Nothing here is about a function's own timeout, so
/// it is given generous room: a run shares this process with every other test of this target.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long this suite waits for work the clock owes — a stack settling, a function becoming
/// invocable, a notification arriving — before giving up, and how often it looks again. The wait
/// is on the CONDITION; the deadline is generous so a loaded host delays a leg instead of
/// breaking it.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(25);

/// The body the function created DIRECTLY — from the very archive the template built, uploaded
/// as `FunctionCode$ZipFile` — publishes.
const UPLOADED_MESSAGE: &str = "uploaded-straight-to-lambda";

/// One way a handler comes by the client it publishes with: `preamble` runs at IMPORT time and
/// `client` is the expression the handler itself evaluates, so a row builds its client once per
/// environment or once per call depending on which of the two carries it.
struct Row {
    /// What the row is, for the message of every assertion it makes.
    case: &'static str,
    /// The suffix the row's function is named with.
    slug: &'static str,
    /// Module-scope source, run when the package is imported.
    preamble: &'static str,
    /// The expression the handler evaluates to get its client.
    client: &'static str,
    /// The body this row publishes, which is what must arrive on the subscribed queue.
    message: &'static str,
}

/// The constructions the rule is asserted over: every form a handler can come by an SDK client
/// in, each publishing a body of its own.
///
/// `boto3.client`, `boto3.Session`, a client's own `region_name`, a `botocore.config.Config` and
/// the credentials passed by keyword are the SDK's own client-building interface; the three
/// variables the last row reads are the ones a managed runtime exports the invocation's
/// credentials in (the Lambda Developer Guide's reserved runtime environment variables:
/// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`), beside the `AWS_REGION`
/// and `AWS_DEFAULT_REGION` the Region rows read.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client built once at module scope",
            slug: "module-scope",
            preamble: r#"SNS = boto3.client("sns")"#,
            client: "SNS",
            message: "from-lambda-sdk",
        },
        Row {
            case: "a client built inside the handler, on the call itself",
            slug: "in-handler",
            preamble: "",
            client: r#"boto3.client("sns")"#,
            message: r#"{"marker":"built-in-the-handler","attempt":2}"#,
        },
        Row {
            case: "a client built from a session created inside the handler",
            slug: "session",
            preamble: "",
            client: r#"boto3.Session().client("sns")"#,
            message: "paiement validé — 10 € ☑",
        },
        Row {
            case: "a client built for the Region the runtime reports",
            slug: "region-named",
            preamble: "",
            client: r#"boto3.client("sns", region_name=os.environ["AWS_REGION"])"#,
            message: "<Message>a &amp; b</Message>?x=1&y=2+3%20",
        },
        Row {
            case: "a client configured through the SDK's own Config",
            slug: "configured",
            preamble: "",
            client: r#"boto3.client("sns", config=Config(region_name=os.environ["AWS_DEFAULT_REGION"]))"#,
            message: "x",
        },
        Row {
            case: "a client built with the credentials the runtime exported",
            slug: "exported-credentials",
            preamble: "",
            client: r#"boto3.client(
        "sns",
        aws_access_key_id=os.environ["AWS_ACCESS_KEY_ID"],
        aws_secret_access_key=os.environ["AWS_SECRET_ACCESS_KEY"],
        aws_session_token=os.environ["AWS_SESSION_TOKEN"],
    )"#,
            message: "line one\n\tline two\r\nline three",
        },
    ]
}

/// The handler a row runs: it publishes the body the event names to the topic the event names
/// with the client the row builds, and reports what the publication answered.
///
/// A refusal is caught and reported in the return value rather than left to fail the run, so a
/// publication the policies in force did not permit is visible as the service's own code and
/// message instead of as a bare function error — and the invocation's own status stays a
/// statement about whether the handler RETURNED.
fn handler_source(row: &Row) -> String {
    format!(
        r#"
import os

import boto3
import botocore.exceptions
from botocore.config import Config

{preamble}


def lambda_handler(event, context):
    sns = {client}
    try:
        answered = sns.publish(TopicArn=event["topic"], Message=event["message"])
    except botocore.exceptions.ClientError as refused:
        return {{
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }}
    return {{
        "ok": True,
        "message_id": answered["MessageId"],
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
    }}
"#,
        preamble = row.preamble,
        client = row.client,
    )
}

/// One function this suite drives: what it is, what it is called, and the body it publishes.
struct Deployed {
    case: String,
    function: String,
    message: String,
}

/// What one Lambda call answered.
struct Answer {
    status: u16,
    function_error: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }
}

/// One Lambda `restJson1` call.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda", REGION))
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        // "X-Amz-Function-Error — If present, indicates that an error occurred during function
        // execution" (`Invoke`'s own output).
        function_error: parts
            .headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: answered.to_vec(),
    }
}

/// One form-wire (`awsQuery`) call against the service whose signing name is `signing_name`,
/// which must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, REGION))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from {signing_name}, got {status} with {text}"
    );
    text
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), which must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs", REGION))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The execution role, trusted by Lambda — which `CreateFunction` checks — and carrying an inline
/// document that grants the log writes every execution role makes plus the publication on `topic`
/// and on nothing else, so an authorization that was never evaluated is visible as a publication
/// that went through for a role granted nothing. Answers its ARN.
async fn given_execution_role(router: &MultiServiceRouter, case: &str, topic: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": PUBLISH_ACTION,
                "Resource": topic,
            },
        ],
    })
    .to_string();
    query(
        router,
        case,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "publish-to-the-topic"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// The name the function of `slug` is created under.
fn function_name(slug: &str) -> String {
    format!("xsvc-infn-publish-{slug}")
}

/// Deploy one function per row, each running that row's handler as `role_arn`, and wait until
/// Lambda reports every one of them invocable.
///
/// Each handler is declared as the stack's inline source, which CloudFormation archives into the
/// deployment package — which is what makes a package hold exactly ONE module, the handler's, and
/// no SDK of its own.
async fn given_functions(router: &MultiServiceRouter, case: &str, role_arn: &str, rows: &[Row]) {
    let mut resources = serde_json::Map::new();
    for (index, row) in rows.iter().enumerate() {
        resources.insert(
            format!("Publisher{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(row.slug),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": handler_source(row)},
                },
            }),
        );
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (status, answer) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: the stack {STACK:?} must reach {CREATE_COMPLETE}, last saw {answer}"
    );
    for row in rows {
        wait_until_active(router, case, &function_name(row.slug)).await;
    }
}

/// Look until Lambda reports `function` ready to be invoked.
///
/// The provisioning window the create documents ("During this time, you can't invoke or modify
/// the function") is owed by the clock, which no fixed sleep names: this looks until the function
/// reports itself invocable.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, function: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut configuration;
    loop {
        configuration = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await
        .json();
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) || Instant::now() >= giving_up_at
        {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        configuration["State"].as_str(),
        Some(FUNCTION_ACTIVE),
        "case {case:?}: {function:?} must reach {FUNCTION_ACTIVE}, last saw {configuration}"
    );
}

/// The deployment package `function` holds, base64-encoded the way `FunctionCode$ZipFile` carries
/// one.
///
/// It is the archive CloudFormation built from the stack's inline source, read back from where
/// `GetFunction` announces it — "a presigned URL that you can use to download the deployment
/// package" (`FunctionCodeLocation$Location`), an address this emulator answers on. So this suite
/// writes no archive of its own, and the function it then creates DIRECTLY runs the very handler
/// the stack deployed.
async fn package_of(router: &MultiServiceRouter, case: &str, function: &str) -> String {
    let read = lambda(router, "GET", &format!("{FUNCTIONS_URI}/{function}"), None).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetFunction({function:?}) must succeed, got {} with {}",
        read.status,
        read.text()
    );
    let reported = read.json();
    let location = reported["Code"]["Location"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: GetFunction must announce where the package can be downloaded, \
                 got {reported}"
            )
        })
        .to_string();
    // The announced URL is absolute; what this router dispatches is its path and query.
    let path = location
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|at| rest[at..].to_string()))
        .unwrap_or_else(|| panic!("case {case:?}: {location:?} names no path to fetch"));
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        // A presigned URL is followed with NO `Authorization` header: the signature it carries is
        // what makes the announced address a presigned one.
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "case {case:?}: the announced location must serve the package, got {status} with {}",
        String::from_utf8_lossy(response.body())
    );
    ::aws_smithy_types::base64::encode(response.body().as_ref())
}

/// A function created DIRECTLY from `package` — `CreateFunction` carrying the archive as
/// `FunctionCode$ZipFile`, the way a client that built its own package uploads one — running as
/// `role_arn`. Answers the name it was created under, once Lambda reports it invocable.
async fn given_uploaded_function(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
    package: &str,
) -> String {
    let function = function_name("uploaded");
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(&json!({
            "FunctionName": function,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Role": role_arn,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {"ZipFile": package},
        })),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction({function:?}) must answer 201, got {} with {}",
        created.status,
        created.text()
    );
    wait_until_active(router, case, &function).await;
    function
}

/// Invoke `function` synchronously, telling it which topic to publish `message` to.
async fn invoke(
    router: &MultiServiceRouter,
    function: &str,
    topic: &str,
    message: &str,
) -> Answer {
    lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        Some(&json!({"topic": topic, "message": message})),
    )
    .await
}

/// Take every notification waiting on the queue at `url`, as the envelope the queue carries it
/// in, deleting what it hands over so a later read sees only what arrived since. The envelope is
/// handed back WHOLE: its `Message` is the published body, which is not a document.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not an SNS envelope ({error}): {body}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    envelopes
}

/// Drain until `wanted` envelopes have arrived, or the deadline passes. Driven off what has
/// ARRIVED rather than off a span of the clock, and it keeps whatever one read handed over past
/// `wanted` — so a duplicate shows up as a count the caller refuses rather than being hidden.
async fn drain_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, url).await);
        if arrived.len() >= wanted || Instant::now() >= giving_up_at {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    arrived
}

/// The one envelope the queue holds for this row, with the shape every notification carries
/// asserted on it: the delivery kind, the topic it came from and the id the publication answered.
/// Answers its `Message`, which is the published body.
fn the_one_envelope(case: &str, arrived: &[Value], topic: &str, id: &str) -> String {
    assert_eq!(
        arrived.len(),
        1,
        "case {case:?}: expected exactly one notification on the subscribed queue, got {arrived:?}"
    );
    let envelope = &arrived[0];
    assert_eq!(
        envelope["Type"],
        json!(NOTIFICATION),
        "case {case:?}: expected a notification envelope, got {envelope}"
    );
    assert_eq!(
        envelope["TopicArn"],
        json!(topic),
        "case {case:?}: expected the envelope to name the published topic, got {envelope}"
    );
    assert_eq!(
        envelope["MessageId"],
        json!(id),
        "case {case:?}: expected the envelope to carry the id the publication answered ({id}), \
         got {envelope}"
    );
    envelope["Message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Message member on the envelope: {envelope}"))
        .to_string()
}

/// However the handler came by its client, and however its package reached Lambda, the granted
/// publication is carried out, the body arrives on the subscribed queue verbatim, and the
/// invocation that made it reports no function error.
#[tokio::test]
async fn every_client_form_publishes_as_the_execution_role_and_fans_out() {
    let setup = "the topic, its subscriber and the granted functions";
    let router = router();
    // The subscriber's grant is conditioned on the publishing topic — the form the documented
    // queue policy takes — from the setup this suite shares with its neighbours.
    let (topic, queue_url) =
        given_topic_with_subscriber(&router, setup, REGION, TOPIC, Grant::ScopedToTopic).await;
    let role = given_execution_role(&router, setup, &topic).await;
    let rows = rows();
    given_functions(&router, setup, &role, &rows).await;

    let mut driven: Vec<Deployed> = rows
        .iter()
        .map(|row| Deployed {
            case: format!("{}, archived by the template", row.case),
            function: function_name(row.slug),
            message: row.message.to_string(),
        })
        .collect();
    // The same handler as the first row, in the same archive, created through the API instead:
    // how the package reached Lambda is nothing the rule depends on.
    let first = &rows[0];
    let package = package_of(&router, setup, &function_name(first.slug)).await;
    driven.push(Deployed {
        case: format!("{}, uploaded straight to CreateFunction", first.case),
        function: given_uploaded_function(&router, setup, &role, &package).await,
        message: UPLOADED_MESSAGE.to_string(),
    });

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for deployed in &driven {
        let case = deployed.case.as_str();
        let answer = invoke(&router, &deployed.function, &topic, &deployed.message).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.function_error, None,
            "case {case:?}: expected no function error, got {:?} with {}",
            answer.function_error,
            answer.text()
        );
        let report = answer.json();
        assert_eq!(
            report["ok"],
            json!(true),
            "case {case:?}: expected the handler's publication to be carried out under a role \
             granted {PUBLISH_ACTION} on the topic, got {report}"
        );
        assert_eq!(
            report["status"],
            json!(PUBLISH_STATUS),
            "case {case:?}: expected the publication to answer {PUBLISH_STATUS}, got {report}"
        );
        let id = report["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !id.is_empty(),
            "case {case:?}: expected the topic's own message id in the handler's answer, got \
             {report}"
        );
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: expected a message id of this publication's own, but {id:?} was \
             already answered for an earlier one"
        );

        let arrived = drain_until(&router, case, &queue_url, 1).await;
        let delivered = the_one_envelope(case, &arrived, &topic, &id);
        assert_eq!(
            delivered, deployed.message,
            "case {case:?}: expected the body published through the handler's own client to \
             arrive on the subscribed queue verbatim; expected {:?}, got {delivered:?}",
            deployed.message
        );
    }

    // Nothing else was queued behind those deliveries. The read is taken at the instant a message
    // published through the SAME path after the last invocation has arrived, so it is an
    // observation rather than a span of the clock.
    let sentinel = "planted-after-the-last-invocation";
    let planted = element(
        &query(
            &router,
            setup,
            "sns",
            "2010-03-31",
            "Publish",
            &[("TopicArn", &topic), ("Message", sentinel)],
        )
        .await,
        "MessageId",
    );
    let arrived = drain_until(&router, setup, &queue_url, 1).await;
    let delivered = the_one_envelope(setup, &arrived, &topic, &planted);
    assert_eq!(
        delivered, sentinel,
        "case {setup:?}: the only thing left on the subscribed queue must be the planted \
         message, got {delivered:?}"
    );
}
