// SPDX-License-Identifier: CC0-1.0

//! What Amazon SQS answers a call on a queue CLOUD CONTROL API removed — over the WHOLE
//! mounted service set, the one `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **a request naming a queue this account no longer holds is refused
//! HTTP 400 in the `QueueDoesNotExist` class — reported beside the legacy query code the
//! model binds to it, `AWS.SimpleQueueService.NonExistentQueue` — carrying the FIXED sentence
//! the wire protocol the request arrived in words that class with, whatever operation was
//! called, however the queue was addressed (its `QueueUrl`, or the `QueueName` the one
//! name-addressed operation resolves), and whichever API removed the queue.** On this
//! service's `awsJson1_0` surface that sentence is "The specified queue does not exist."; on
//! its WSDL-versioned AWS Query surface it is the same statement naming that surface's
//! version. Either way the sentence is a CONSTANT: the code is what identifies the condition,
//! so nothing of the request — no queue name, no url, no account — is interpolated into it,
//! and no clause of the other surface's wording leaks into this one's.
//!
//! Two services have to agree for that to hold: Cloud Control API accepts the `DeleteResource`
//! for an `AWS::SQS::Queue`, and Amazon SQS decides every later call on the queue itself. This
//! is the write-through negative control — the call that proves a delete a uniform control
//! plane accepted really reached the data plane that was serving a moment earlier — so a
//! client matching AWS's own absence sentence to confirm the delete propagated can confirm it.
//!
//! Overlap, stated rather than hidden: the Amazon SQS crate's `queue_does_not_exist` suite
//! drives the table of operations and absence forms against that service mounted alone, its
//! `query_compatible_error_envelope` suite pins the AWS Query surface's own sentence, and the
//! Cloud Control API crate's `sqs_queue_read_through` suite holds the read/write/delete flow
//! with the two services it needs mounted. What only this suite can reach is the same flow on
//! the PRODUCTION service set, where which mounted model claims each call is decidable at all.
//!
//! The rows differ along what the rule generalises over: the queue's TYPE and configuration
//! (a queue created with nothing but its name, one created with attributes of its own, a FIFO
//! queue), the property value Cloud Control API writes through to it before the delete (each
//! published bound of `VisibilityTimeout`'s range and a value between them), the FORM of the
//! absence (a queue Cloud Control API deleted, and a url of a queue the account never
//! created), the OPERATION called, and the SURFACE the client speaks (the native JSON one, the
//! query-compatible JSON one an SDK opts into with the query-mode header, and the AWS Query
//! form wire).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE spelling of a Cloud Control API call in this test target, and the Region it and
// every call here are addressed in: a sibling suite that drives that service declares both
// `pub(crate)`, so neither the wire form nor the Region is written twice.
use crate::cloudformation_apigatewayv2_api_schema_properties::{cloud_control, REGION};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::{element, form_encode};

/// The registry type whose resources live in the Amazon SQS service's own store, and whose
/// `primaryIdentifier` is the queue url that service assigned (`/properties/QueueUrl`).
const QUEUE_TYPE: &str = "AWS::SQS::Queue";

/// Amazon SQS's own sentence for a queue the account does not hold, on the `awsJson1_0`
/// surface every modern SDK speaks — written out here rather than read off the service, so
/// what this suite asserts is AWS's text and not the emulator's idea of it.
///
/// RECORDED: a `SetQueueAttributes` on the url of a queue that was deleted, and a
/// `GetQueueUrl` for a name the account holds no queue under, are refused HTTP 400
/// `AWS.SimpleQueueService.NonExistentQueue` with exactly this text. The model
/// (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
/// fixes no message text, so the served sentence is the requirement; it names no queue, so it
/// has no variable part.
const AWS_SENTENCE: &str = "The specified queue does not exist.";

/// The sentence the WSDL-versioned AWS Query surface of the same API words the same class
/// with, which the JSON surface above never serves. One API, two protocols, two sentences for
/// one class, with the same code and status on both.
const QUERY_API_SENTENCE: &str = "The specified queue does not exist for this wsdl version.";

/// The class the refusal names, as the `awsJson1_0` discriminator carries it, and the status
/// the model binds to it (`httpError: 400`).
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";
const REFUSAL_STATUS: u16 = 400;

/// The legacy query code the model's `awsQueryError` trait binds to that class — what a query
/// client reads as `Error.Code` — and the `code;fault` report an `awsQueryCompatible` service
/// puts beside the class on the JSON surface.
const NON_EXISTENT_QUEUE: &str = "AWS.SimpleQueueService.NonExistentQueue";
const NON_EXISTENT_QUEUE_REPORT: &str = "AWS.SimpleQueueService.NonExistentQueue;Sender";

/// The response header that report arrives in, and the request header a query-compatible SDK
/// opts into it with.
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

/// What Cloud Control API reports for a resource the owning service does not hold.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The terminal statuses an accepted Cloud Control API request settles in.
const STATUS_SUCCESS: &str = "SUCCESS";
const STATUS_FAILED: &str = "FAILED";

/// How long a settle may take before the suite calls the request stuck, and how often it looks
/// again. The wait is for the request's own STATUS and never for a span, so a loaded machine
/// delays a case instead of failing it; the budget is far above anything this flow takes —
/// including on a host running the whole workspace's tests at once — because no case here is
/// about how fast a provisioning settles.
const SETTLE_BUDGET: Duration = Duration::from_secs(120);
const SETTLE_INTERVAL: Duration = Duration::from_millis(20);

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

/// One form parameter of an AWS Query request: the name, which is this suite's own literal,
/// and the value it carries.
type Param = (&'static str, String);

/// One row of the AWS Query table: the case, the action it calls, and the parameters it
/// carries.
type QueryRow = (&'static str, &'static str, Vec<Param>);

/// One Amazon SQS call on the AWS Query form wire, as its status and the XML text it answered.
async fn sqs_query(
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> (u16, String) {
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

/// A string member of a document, or `""` where it carries none — so a missing member fails an
/// assertion rather than panicking the suite.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The `ProgressEvent` an accepted Cloud Control API request answers with.
fn progress(body: &Value) -> Value {
    body["ProgressEvent"].clone()
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

/// Read the Cloud Control API request `token` names until it reports a terminal status,
/// requiring it to have SUCCEEDED, and answer the `ProgressEvent` it reported then.
async fn settled(router: &MultiServiceRouter, case: &str, token: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = cloud_control(
            router,
            "GetResourceRequestStatus",
            json!({ "RequestToken": token }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetResourceRequestStatus, got {status} with \
             {body}"
        );
        let event = progress(&body);
        let reached = text(&event, "OperationStatus");
        if reached == STATUS_SUCCESS {
            return event;
        }
        assert_ne!(
            reached, STATUS_FAILED,
            "case {case:?}: the request must not fail, got {event}"
        );
        assert!(
            started.elapsed() < SETTLE_BUDGET,
            "case {case:?}: expected the request to settle within {SETTLE_BUDGET:?}, it is \
             still {reached:?}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// Every operation of this service that resolves a queue from a `QueueUrl`, each with the rest
/// of a request that is otherwise well formed — so the only thing its refusal can be about is
/// the queue.
///
/// Taken from the service's own surface (`smithy_query(query="digest",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`):
/// the url-addressed operations that need no other resource of their own. `SetQueueAttributes`
/// leads, because a write is what the write-through control asks after a delete.
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

/// Assert one JSON answer is the absence refusal, whole: the status, the class, the legacy
/// report beside it, AWS's own sentence, and that the sentence interpolated nothing of the
/// request.
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
        "case {case:?} / {operation} on the {surface:?} surface: expected HTTP \
         {REFUSAL_STATUS} {QUEUE_DOES_NOT_EXIST} reading {AWS_SENTENCE:?} reported as \
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

/// One case: a queue Amazon SQS's own API creates, which Cloud Control API then reads, writes
/// through to and deletes.
struct Row {
    case: &'static str,
    name: &'static str,
    /// The attributes `sqs:CreateQueue` is given for it.
    attributes: Value,
    /// The `VisibilityTimeout` Cloud Control API patches it to before the delete. The published
    /// range of that attribute is 0 to 43,200 seconds (the `Attributes` documentation of
    /// `CreateQueue`), so the rows take each bound of it and a value between them.
    visibility_timeout: i64,
}

/// The rows: see this suite's header for what they differ along.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a standard queue created with nothing but its name",
            name: "cc-write-through-plain",
            attributes: json!({}),
            visibility_timeout: 75,
        },
        Row {
            case: "a standard queue created carrying attributes of its own",
            name: "cc-write-through-configured",
            attributes: json!({"DelaySeconds": "5", "MessageRetentionPeriod": "600"}),
            visibility_timeout: 0,
        },
        Row {
            case: "a FIFO queue",
            name: "cc-write-through-ordered.fifo",
            attributes: json!({"FifoQueue": "true"}),
            visibility_timeout: 43_200,
        },
    ]
}

/// The url the same endpoint and account address the queue called `name` at, built off a url
/// this emulator itself minted so no case writes the address form by hand.
fn url_of_name(minted: &str, name: &str) -> String {
    match minted.rfind('/') {
        Some(cut) => format!("{}/{name}", &minted[..cut]),
        None => format!("{minted}/{name}"),
    }
}

/// `sqs:CreateQueue` for one row, answering the url the owning service assigned.
async fn create_native(router: &MultiServiceRouter, row: &Row) -> String {
    let mut request = json!({"QueueName": row.name});
    if row.attributes.as_object().is_some_and(|map| !map.is_empty()) {
        request["Attributes"] = row.attributes.clone();
    }
    let created = sqs_ok(router, row.case, "CreateQueue", &request).await;
    let url = text(&created, "QueueUrl");
    assert!(
        url.ends_with(&format!("/{}", row.name)),
        "case {:?}: expected the created queue's url to address it, got {url:?}",
        row.case
    );
    url
}

/// What the owning service reports for one attribute of the queue at `url`.
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

/// A queue natively created, read and written through Cloud Control API, then deleted through
/// it, is refused by every Amazon SQS call afterwards with AWS's own absence sentence.
#[tokio::test]
async fn a_queue_cloud_control_deleted_refuses_every_sqs_call_with_awss_absence_sentence() {
    let router = router();
    let never = "cc-write-through-never-created";

    for row in rows() {
        let case = row.case;

        // Created over the OWNING service's own API alone: nothing of this queue was ever
        // named to Cloud Control API.
        let url = create_native(&router, &row).await;

        // Cloud Control API resolves it by the identifier that service holds it under...
        let (status, body) =
            cloud_control(&router, "GetResource", json!({"TypeName": QUEUE_TYPE, "Identifier": url}))
                .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetResource must resolve the natively created queue, got {status} \
             with {body}"
        );
        let described = &body["ResourceDescription"];
        assert_eq!(
            text(described, "Identifier"),
            url,
            "case {case:?}: expected the queue's own url as the identifier, got {body}"
        );

        // ...and a write through it is a write to that queue, which the owning service reports.
        let (status, body) = cloud_control(
            &router,
            "UpdateResource",
            json!({
                "TypeName": QUEUE_TYPE,
                "Identifier": url,
                "PatchDocument": json!([{
                    "op": "add",
                    "path": "/VisibilityTimeout",
                    "value": row.visibility_timeout,
                }])
                .to_string(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateResource must be accepted, got {status} with {body}"
        );
        settled(&router, case, &text(&progress(&body), "RequestToken")).await;
        let reported = native_attribute(&router, case, &url, "VisibilityTimeout").await;
        assert_eq!(
            reported,
            row.visibility_timeout.to_string(),
            "case {case:?}: expected the owning service to carry the patched VisibilityTimeout \
             {}, got {reported:?}",
            row.visibility_timeout
        );

        // The delete is accepted and settles...
        let (status, body) = cloud_control(
            &router,
            "DeleteResource",
            json!({"TypeName": QUEUE_TYPE, "Identifier": url}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteResource must be accepted, got {status} with {body}"
        );
        settled(&router, case, &text(&progress(&body), "RequestToken")).await;

        // ...and from there every call naming that queue is refused, in the one sentence, over
        // both forms of the absence and on both JSON surfaces.
        let absences: Vec<(String, String)> = vec![
            (
                format!("{case}, deleted through Cloud Control API"),
                url.clone(),
            ),
            (
                format!("{case}, a url of a queue the account never created"),
                url_of_name(&url, never),
            ),
        ];
        for (form, probed_url) in &absences {
            for (operation, request) in url_requests(probed_url) {
                for surface in [Surface::Json, Surface::JsonQueryCompatible] {
                    let answered = sqs_json(&router, operation, &request, surface).await;
                    assert_json_absence(
                        form,
                        operation,
                        surface,
                        &[row.name, never, probed_url],
                        answered,
                    );
                }
            }
        }

        // The name-addressed operation resolves it no longer either.
        for (form, probed_name) in [
            ("the name Cloud Control API deleted", row.name),
            ("a name the account never created", never),
        ] {
            for surface in [Surface::Json, Surface::JsonQueryCompatible] {
                let answered = sqs_json(
                    &router,
                    "GetQueueUrl",
                    &json!({"QueueName": probed_name}),
                    surface,
                )
                .await;
                assert_json_absence(form, "GetQueueUrl", surface, &[probed_name, &url], answered);
            }
        }

        // And Cloud Control API itself reports the resource as one that cannot be found.
        let (status, body) = cloud_control(
            &router,
            "GetResource",
            json!({"TypeName": QUEUE_TYPE, "Identifier": url}),
        )
        .await;
        assert_eq!(
            (status, class_of(&body).as_str()),
            (REFUSAL_STATUS, RESOURCE_NOT_FOUND),
            "case {case:?}: GetResource after the delete: expected HTTP {REFUSAL_STATUS} \
             {RESOURCE_NOT_FOUND}, got {status} with {body}"
        );
    }
}

/// The AWS Query surface of the same API answers the same class, with the same code and status,
/// in the sentence that surface words it with — so which sentence a client reads is decided by
/// the protocol its request arrived in and by nothing else.
#[tokio::test]
async fn the_query_surface_words_the_same_absence_in_its_own_sentence() {
    let router = router();
    let name = "cc-query-surface-deleted";

    let url = create_native(
        &router,
        &Row {
            case: "a queue reached on the query surface after its Cloud Control delete",
            name,
            attributes: json!({}),
            visibility_timeout: 30,
        },
    )
    .await;
    let case = "a queue Cloud Control API deleted, reached on the AWS Query surface";
    let (status, body) = cloud_control(
        &router,
        "DeleteResource",
        json!({"TypeName": QUEUE_TYPE, "Identifier": url}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteResource must be accepted, got {status} with {body}"
    );
    settled(&router, case, &text(&progress(&body), "RequestToken")).await;

    // The actions are spelled in this protocol's own request form: a `QueueUrl` field, the
    // `Attribute.N.Name` / `Attribute.N.Value` pairs an attribute map arrives as, and the
    // `QueueName` the name-addressed action takes.
    let never = url_of_name(&url, "cc-query-surface-never-created");
    let rows: Vec<QueryRow> = vec![
        (
            "SetQueueAttributes on the deleted queue",
            "SetQueueAttributes",
            vec![
                ("QueueUrl", url.clone()),
                ("Attribute.1.Name", "VisibilityTimeout".to_string()),
                ("Attribute.1.Value", "75".to_string()),
            ],
        ),
        (
            "GetQueueAttributes on the deleted queue",
            "GetQueueAttributes",
            vec![
                ("QueueUrl", url.clone()),
                ("AttributeName.1", "All".to_string()),
            ],
        ),
        (
            "SendMessage to the deleted queue",
            "SendMessage",
            vec![
                ("QueueUrl", url.clone()),
                ("MessageBody", "gone".to_string()),
            ],
        ),
        (
            "SendMessage to a queue the account never created",
            "SendMessage",
            vec![("QueueUrl", never), ("MessageBody", "gone".to_string())],
        ),
        (
            "GetQueueUrl for the deleted name",
            "GetQueueUrl",
            vec![("QueueName", name.to_string())],
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
        assert!(
            !message.contains(name),
            "case {form:?}: the sentence must interpolate nothing of the request, got \
             {message:?}"
        );
    }
}
