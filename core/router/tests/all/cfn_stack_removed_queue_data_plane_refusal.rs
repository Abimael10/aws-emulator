// SPDX-License-Identifier: CC0-1.0

//! What Amazon SQS answers a call on a queue CLOUDFORMATION removed — over the WHOLE mounted
//! service set, the one `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **a request naming a queue this account no longer holds is refused HTTP
//! 400 in the `QueueDoesNotExist` class — reported beside the legacy query code the model binds
//! to it, `AWS.SimpleQueueService.NonExistentQueue` — carrying the FIXED sentence the wire
//! protocol the request arrived in words that class with, whatever operation was called, however
//! the queue was addressed (its `QueueUrl`, or the `QueueName` the one name-addressed operation
//! resolves), and whichever API removed the queue.** On this service's `awsJson1_0` surface that
//! sentence is "The specified queue does not exist."; on its WSDL-versioned AWS Query surface it
//! is the same statement naming that surface's version. Either way the sentence is a CONSTANT:
//! the code is what identifies the condition, so nothing of the request — no queue name, no url,
//! no account — is interpolated into it, and no clause of the other surface's wording leaks into
//! this one's.
//!
//! Two services have to agree for that to hold, which is why this suite lives here rather than in
//! either crate: CloudFormation removes the `AWS::SQS::Queue` resources a stack owned, and Amazon
//! SQS decides every later call on the queues themselves. This is the write-through negative
//! control a client makes when it reconciles a stack it tore down against the queue urls it still
//! holds — the call that proves a removal the control plane reported really reached the data plane
//! that was serving a moment earlier.
//!
//! Overlap, stated rather than hidden: the sibling `cloudcontrol_deleted_queue_data_plane_refusal`
//! suite holds the same rule for a queue the Cloud Control API removed, and the Amazon SQS crate's
//! own `queue_does_not_exist` suite drives the operation table and the absence forms against that
//! service mounted alone. What only this suite reaches is a queue a STACK owned, removed by
//! CloudFormation's own deployment, on the production service set.
//!
//! The rows differ along what the rule generalises over: the FORM of the absence (a queue whose
//! stack was torn down, a queue a stack update re-keyed onto another name and so replaced, and a
//! url of a queue the account never created), the queue's TYPE and declared properties (nothing
//! but a name, properties of its own, a FIFO queue), how the queue is ADDRESSED (the url the stack
//! published, and the name the one name-addressed operation resolves), the OPERATION called, and
//! the SURFACE the client speaks (the native JSON one, the query-compatible JSON one an SDK opts
//! into with the query-mode header, and the AWS Query form wire).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE spelling of a CloudFormation call in this test target, and the Region it and every call
// here are addressed in: a sibling suite that drives that service declares both `pub(crate)`, so
// neither the wire form nor the Region is written twice. The router is the whole emulator over one
// engine, exactly as the binary assembles it.
use crate::cloudformation_apigatewayv2_api_schema_properties::{cloudformation, REGION};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::{element, form_encode, opt_element, outputs_of};

/// Amazon SQS's own sentence for a queue the account does not hold, on the `awsJson1_0` surface
/// every modern SDK speaks — written out here rather than read off the service, so what this suite
/// asserts is AWS's text and not the emulator's idea of it.
///
/// RECORDED: a `GetQueueAttributes` on the url of a queue whose owning stack was deleted, and a
/// `GetQueueUrl` for a name the account holds no queue under, are refused HTTP 400
/// `AWS.SimpleQueueService.NonExistentQueue` with exactly this text. The model
/// (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
/// fixes no message text, so the served sentence is the requirement; it names no queue, so it has
/// no variable part.
const AWS_SENTENCE: &str = "The specified queue does not exist.";

/// The sentence the WSDL-versioned AWS Query surface of the same API words the same class with,
/// which the JSON surface above never serves. One API, two protocols, two sentences for one class,
/// with the same code and status on both.
const QUERY_API_SENTENCE: &str = "The specified queue does not exist for this wsdl version.";

/// The class the refusal names, as the `awsJson1_0` discriminator carries it, and the status the
/// model binds to it (`httpError: 400`).
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";
const REFUSAL_STATUS: u16 = 400;

/// The legacy query code the model's `awsQueryError` trait binds to that class — what a query
/// client reads as `Error.Code` — and the `code;fault` report an `awsQueryCompatible` service puts
/// beside the class on the JSON surface.
const NON_EXISTENT_QUEUE: &str = "AWS.SimpleQueueService.NonExistentQueue";
const NON_EXISTENT_QUEUE_REPORT: &str = "AWS.SimpleQueueService.NonExistentQueue;Sender";

/// The response header that report arrives in, and the request header a query-compatible SDK opts
/// into it with.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";
const QUERY_MODE_HEADER: &str = "x-amzn-query-mode";

/// The API version an AWS Query client of this endpoint names in every request
/// (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`:
/// version `2012-11-05`).
const QUERY_API_VERSION: &str = "2012-11-05";

/// The media types the two Amazon SQS surfaces carry their requests in.
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.0";
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// The statuses a stack operation reports when it is over, and the suffix every status of one
/// still running carries.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";

/// How long a wait in this suite runs before the case fails, and how often it looks again.
///
/// Every wait here is for work ANOTHER service performs — a stack's deployment, update and
/// teardown, each a further request of this emulator with the span the clock owes it — so the
/// budget is far above anything that flow takes, including on a host running the whole workspace's
/// tests at once. No case waits for a span: each re-reads until what it is after is there or this
/// deadline passes, and no case is about how fast a deployment settles.
const BUDGET: Duration = Duration::from_secs(240);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The surface one Amazon SQS call is made on.
#[derive(Debug, Clone, Copy)]
enum Surface {
    /// The `awsJson1_0` wire a modern SDK speaks.
    Json,
    /// The same wire with the query-compatibility opt-in a migrating SDK sends.
    JsonQueryCompatible,
}

impl Surface {
    fn opt_in(self) -> Option<&'static str> {
        match self {
            Self::Json => None,
            Self::JsonQueryCompatible => Some("true"),
        }
    }
}

/// Both JSON surfaces, which every absent queue here is probed on.
const JSON_SURFACES: [Surface; 2] = [Surface::Json, Surface::JsonQueryCompatible];

/// One Amazon SQS call on the JSON surface, as its status, its response headers and its parsed
/// body — the seam a refusal is read through, so nothing here asserts a status of its own.
async fn sqs_json(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    surface: Surface,
) -> (u16, http::HeaderMap, Value) {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", JSON_CONTENT_TYPE)
        .header("x-amz-target", format!("AmazonSQS.{operation}"));
    if let Some(value) = surface.opt_in() {
        builder = builder.header(QUERY_MODE_HEADER, value);
    }
    let request = builder
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, headers, parsed)
}

/// The same call, which must be served, answering the document it reported.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, _, parsed) = sqs_json(router, operation, body, Surface::Json).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One form parameter of an AWS Query request: the name, which is this suite's own literal, and
/// the value it carries.
type Param = (&'static str, String);

/// One row of the AWS Query table: the case, the action it calls, and the parameters it carries.
type QueryRow = (&'static str, &'static str, Vec<Param>);

/// One Amazon SQS call on the AWS Query form wire, as its status and the XML text it answered.
async fn sqs_query(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={QUERY_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", FORM_CONTENT_TYPE)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation call that must be accepted, answering the XML it reported.
async fn cfn_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Read the stack until it reports a status that is no longer in progress, answering the whole
/// `DescribeStacks` answer it reported then.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let started = Instant::now();
    loop {
        let body = cfn_ok(router, case, "DescribeStacks", &[("StackName", stack)]).await;
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with(IN_PROGRESS_SUFFIX) {
            return body;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the stack to reach a terminal status within {BUDGET:?}, it \
             is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The class a JSON refusal names: the local name of its `__type` discriminator.
fn class_of(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The sentence a JSON refusal carries, under either spelling of the member.
fn message_of(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The value of one response header, or `None` when the answer carries none.
fn header_of(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

/// What the owning service reports for one attribute of the queue at `url`, which it has to serve.
async fn native_attribute(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    attribute: &str,
) -> String {
    let reported = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": [attribute]}),
    )
    .await;
    reported["Attributes"][attribute]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Read the queue at `url` until the owning service no longer holds it.
///
/// The removal is another service's deployment, so this waits for the CONDITION — a reporting
/// call on that url being refused — rather than for a span. `GetQueueAttributes` is the probe
/// because it reports and changes nothing, so polling it decides nothing about what follows.
async fn settled_absent(router: &MultiServiceRouter, case: &str, url: &str) {
    let started = Instant::now();
    loop {
        let (status, _, body) = sqs_json(
            router,
            "GetQueueAttributes",
            &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
            Surface::Json,
        )
        .await;
        if status == REFUSAL_STATUS {
            return;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the queue at {url} to be gone within {BUDGET:?}, SQS still \
             answers {status} with {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every operation of this service that resolves a queue from a `QueueUrl`, each with the rest of
/// a request that is otherwise well formed — so the only thing its refusal can be about is the
/// queue.
///
/// Taken from the service's own surface (`smithy_query(query="digest",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`):
/// the url-addressed operations that need no other resource of their own. `SetQueueAttributes`
/// leads, because a write is what the write-through control asks after a removal.
fn url_requests(url: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "SetQueueAttributes",
            json!({"QueueUrl": url, "Attributes": {"VisibilityTimeout": "75"}}),
        ),
        (
            "GetQueueAttributes",
            json!({"QueueUrl": url, "AttributeNames": ["All"]}),
        ),
        ("SendMessage", json!({"QueueUrl": url, "MessageBody": "gone"})),
        (
            "SendMessageBatch",
            json!({"QueueUrl": url, "Entries": [{"Id": "one", "MessageBody": "gone"}]}),
        ),
        ("ReceiveMessage", json!({"QueueUrl": url})),
        (
            "DeleteMessage",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle"}),
        ),
        (
            "ChangeMessageVisibility",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle", "VisibilityTimeout": 30}),
        ),
        ("TagQueue", json!({"QueueUrl": url, "Tags": {"owner": "control"}})),
        ("ListQueueTags", json!({"QueueUrl": url})),
        ("PurgeQueue", json!({"QueueUrl": url})),
    ]
}

/// Assert one JSON answer is the absence refusal, whole: the status, the class, the legacy report
/// beside it, AWS's own sentence, and that the sentence interpolated nothing of the request.
fn assert_json_absence(
    case: &str,
    operation: &str,
    surface: Surface,
    probed: &[&str],
    answered: (u16, http::HeaderMap, Value),
) {
    let (status, headers, body) = answered;
    let report = header_of(&headers, QUERY_ERROR_HEADER);
    let message = message_of(&body);
    assert_eq!(
        (
            status,
            class_of(&body).as_str(),
            message.as_str(),
            report.clone()
        ),
        (
            REFUSAL_STATUS,
            QUEUE_DOES_NOT_EXIST,
            AWS_SENTENCE,
            Some(NON_EXISTENT_QUEUE_REPORT.to_string())
        ),
        "case {case:?} / {operation} on the {surface:?} surface: expected HTTP {REFUSAL_STATUS} \
         {QUEUE_DOES_NOT_EXIST} reading {AWS_SENTENCE:?} reported as \
         {NON_EXISTENT_QUEUE_REPORT:?}, got {status} with {body} and {QUERY_ERROR_HEADER} \
         {report:?}"
    );
    for value in probed {
        assert!(
            !message.contains(value),
            "case {case:?} / {operation} on the {surface:?} surface: the sentence must \
             interpolate nothing of the request, got {message:?} naming {value:?}"
        );
    }
}

/// Probe every url-addressed operation, and the name-addressed one, against a queue that is gone,
/// on both JSON surfaces.
async fn assert_absent_everywhere(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    name: Option<&str>,
) {
    let mut probes = url_requests(url);
    if let Some(name) = name {
        probes.push(("GetQueueUrl", json!({"QueueName": name})));
    }
    let probed: Vec<&str> = match name {
        Some(name) => vec![url, name],
        None => vec![url],
    };
    for surface in JSON_SURFACES {
        for (operation, request) in &probes {
            let answered = sqs_json(router, operation, request, surface).await;
            assert_json_absence(case, operation, surface, &probed, answered);
        }
    }
}

/// One queue of the torn-down stack: the output its url is published under, the name it was
/// declared with, and what makes it its own row.
struct Row {
    case: &'static str,
    output: &'static str,
    name: &'static str,
}

/// The rows of the teardown stack: see this suite's header for what they differ along.
const TEARDOWN_ROWS: &[Row] = &[
    Row {
        case: "a standard queue a stack declared with nothing but its name",
        output: "PlainUrl",
        name: "stack-gone-plain",
    },
    Row {
        case: "a standard queue a stack declared with properties of its own",
        output: "ConfiguredUrl",
        name: "stack-gone-configured",
    },
    Row {
        case: "a FIFO queue a stack declared",
        output: "OrderedUrl",
        name: "stack-gone-ordered.fifo",
    },
];

/// The stack the rows above are declared in.
const TEARDOWN_STACK: &str = "sqs-stack-teardown";

/// The template that stack is deployed from: one `AWS::SQS::Queue` per row, each publishing its
/// url as an output (`Ref` on an `AWS::SQS::Queue` returns the queue url).
///
/// `VisibilityTimeout` takes each published bound of its range here — "An integer from 0 to 43,200
/// (12 hours)", the `Attributes` documentation of `CreateQueue` — and the row that declares none
/// is left on the default, so no row's configuration is the one the rule is read off.
const TEARDOWN_TEMPLATE: &str = r#"{"Resources":{
     "Plain":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"stack-gone-plain"}},
     "Configured":{"Type":"AWS::SQS::Queue","Properties":{
        "QueueName":"stack-gone-configured","DelaySeconds":5,
        "MessageRetentionPeriod":600,"VisibilityTimeout":0}},
     "Ordered":{"Type":"AWS::SQS::Queue","Properties":{
        "QueueName":"stack-gone-ordered.fifo","FifoQueue":true,
        "VisibilityTimeout":43200}}},
   "Outputs":{"PlainUrl":{"Value":{"Ref":"Plain"}},
     "ConfiguredUrl":{"Value":{"Ref":"Configured"}},
     "OrderedUrl":{"Value":{"Ref":"Ordered"}}}}"#;

/// The name no row ever creates, which the control row addresses.
const NEVER_CREATED: &str = "stack-gone-never-created";

/// The url the same endpoint and account address the queue called `name` at, built off a url this
/// emulator itself minted so no case writes the address form by hand.
fn url_of_name(minted: &str, name: &str) -> String {
    match minted.rfind('/') {
        Some(cut) => format!("{}/{name}", &minted[..cut]),
        None => format!("{minted}/{name}"),
    }
}

/// One output of a settled stack, which the case needs to address what the stack made.
fn output(described: &str, case: &str, key: &str) -> String {
    outputs_of(described)
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
        .clone()
}

/// Every queue a torn-down stack owned is refused by every Amazon SQS call afterwards with AWS's
/// own absence sentence — and so is a url of a queue the account never created, which is the
/// control that the sentence is about absence and not about the teardown.
#[tokio::test]
async fn queues_a_stack_teardown_removed_refuse_every_sqs_call_with_awss_absence_sentence() {
    let router = router();
    let case = "the stack holding every row";

    cfn_ok(
        &router,
        case,
        "CreateStack",
        &[
            ("StackName", TEARDOWN_STACK),
            ("TemplateBody", TEARDOWN_TEMPLATE),
        ],
    )
    .await;
    let described = settled_stack(&router, case, TEARDOWN_STACK).await;
    let reached = element(&described, "StackStatus");
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy before anything is removed, got \
         {reached:?} with {described}"
    );

    // Each queue is SERVING first: that is what makes the refusals afterwards about the removal.
    let mut urls: Vec<(&Row, String)> = Vec::new();
    for row in TEARDOWN_ROWS {
        let url = output(&described, row.case, row.output);
        let arn = native_attribute(&router, row.case, &url, "QueueArn").await;
        assert!(
            arn.ends_with(&format!(":{}", row.name)),
            "case {:?}: expected the deployed queue's ARN to name it, got {arn:?}",
            row.case
        );
        urls.push((row, url));
    }
    let never = url_of_name(&urls[0].1, NEVER_CREATED);

    cfn_ok(&router, case, "DeleteStack", &[("StackName", TEARDOWN_STACK)]).await;
    for (row, url) in &urls {
        settled_absent(&router, row.case, url).await;
    }

    for (row, url) in &urls {
        assert_absent_everywhere(&router, row.case, url, Some(row.name)).await;
    }
    assert_absent_everywhere(
        &router,
        "a url of a queue the account never created",
        &never,
        Some(NEVER_CREATED),
    )
    .await;
}

/// The stack the replacement row is deployed in, and the two names it is deployed with: the
/// `QueueName` of an `AWS::SQS::Queue` is create-only, so an update that re-keys it makes a new
/// queue and drops the identity it replaced.
const RENAMED_STACK: &str = "sqs-stack-renamed";
const RENAMED_FIRST: &str = "stack-replaced-first";
const RENAMED_SECOND: &str = "stack-replaced-second";

/// A template declaring one `AWS::SQS::Queue` called `name` and publishing its url.
fn one_queue_template(name: &str) -> String {
    format!(
        r#"{{"Resources":{{"Queue":{{"Type":"AWS::SQS::Queue",
             "Properties":{{"QueueName":"{name}","VisibilityTimeout":60}}}}}},
           "Outputs":{{"Url":{{"Value":{{"Ref":"Queue"}}}}}}}}"#
    )
}

/// A stack update that re-keys a queue leaves the identity it replaced absent, and every surface
/// words that absence in its own fixed sentence — so which sentence a client reads is decided by
/// the protocol its request arrived in and by nothing else, and never by how the queue went away.
#[tokio::test]
async fn an_update_that_re_keys_a_queue_refuses_the_url_it_replaced_in_each_surfaces_sentence() {
    let router = router();
    let case = "a queue a stack update replaced";

    let created = one_queue_template(RENAMED_FIRST);
    cfn_ok(
        &router,
        case,
        "CreateStack",
        &[
            ("StackName", RENAMED_STACK),
            ("TemplateBody", created.as_str()),
        ],
    )
    .await;
    let described = settled_stack(&router, case, RENAMED_STACK).await;
    let reached = element(&described, "StackStatus");
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the create to complete before the rename, got {reached:?} with \
         {described}"
    );
    let replaced = output(&described, case, "Url");
    let arn = native_attribute(&router, case, &replaced, "QueueArn").await;
    assert!(
        arn.ends_with(&format!(":{RENAMED_FIRST}")),
        "case {case:?}: expected the deployed queue's ARN to name it, got {arn:?}"
    );

    let updated = one_queue_template(RENAMED_SECOND);
    cfn_ok(
        &router,
        case,
        "UpdateStack",
        &[
            ("StackName", RENAMED_STACK),
            ("TemplateBody", updated.as_str()),
        ],
    )
    .await;
    let described = settled_stack(&router, case, RENAMED_STACK).await;
    let reached = element(&described, "StackStatus");
    assert_eq!(
        reached, UPDATE_COMPLETE,
        "case {case:?}: expected the rename to complete, got {reached:?} with {described}"
    );
    settled_absent(&router, case, &replaced).await;

    // The replacement is SERVING, so the refusals below are about the identity that went away and
    // not about a service that stopped answering.
    let replacement = output(&described, case, "Url");
    let arn = native_attribute(&router, case, &replacement, "QueueArn").await;
    assert!(
        arn.ends_with(&format!(":{RENAMED_SECOND}")),
        "case {case:?}: expected the replacement's ARN to name it, got {arn:?}"
    );

    assert_absent_everywhere(&router, case, &replaced, Some(RENAMED_FIRST)).await;

    // The same absence on the AWS Query form wire, in that surface's own sentence. The actions are
    // spelled in this protocol's own request form: a `QueueUrl` field, the `Attribute.N.Name` /
    // `Attribute.N.Value` pairs an attribute map arrives as, and the `QueueName` the
    // name-addressed action takes.
    let never = url_of_name(&replaced, NEVER_CREATED);
    let rows: Vec<QueryRow> = vec![
        (
            "SetQueueAttributes on the replaced queue",
            "SetQueueAttributes",
            vec![
                ("QueueUrl", replaced.clone()),
                ("Attribute.1.Name", "VisibilityTimeout".to_string()),
                ("Attribute.1.Value", "75".to_string()),
            ],
        ),
        (
            "GetQueueAttributes on the replaced queue",
            "GetQueueAttributes",
            vec![
                ("QueueUrl", replaced.clone()),
                ("AttributeName.1", "All".to_string()),
            ],
        ),
        (
            "SendMessage to the replaced queue",
            "SendMessage",
            vec![
                ("QueueUrl", replaced.clone()),
                ("MessageBody", "gone".to_string()),
            ],
        ),
        (
            "SendMessage to a queue the account never created",
            "SendMessage",
            vec![("QueueUrl", never), ("MessageBody", "gone".to_string())],
        ),
        (
            "GetQueueUrl for the replaced name",
            "GetQueueUrl",
            vec![("QueueName", RENAMED_FIRST.to_string())],
        ),
    ];

    for (form, action, params) in &rows {
        let (status, answered) = sqs_query(&router, action, params).await;
        let code = element(&answered, "Code");
        let message = element(&answered, "Message");
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (REFUSAL_STATUS, NON_EXISTENT_QUEUE, QUERY_API_SENTENCE),
            "case {form:?}: expected HTTP {REFUSAL_STATUS} {NON_EXISTENT_QUEUE} reading \
             {QUERY_API_SENTENCE:?}, got {status} with {answered}"
        );
        for value in [RENAMED_FIRST, NEVER_CREATED] {
            assert!(
                !message.contains(value),
                "case {form:?}: the sentence must interpolate nothing of the request, got \
                 {message:?} naming {value:?}"
            );
        }
    }
}
