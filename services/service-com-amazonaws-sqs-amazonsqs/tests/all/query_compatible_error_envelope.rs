// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ERROR ENVELOPE this service answers in, driven over the wire
//! through the shared harness in [`super::support`].
//!
//! The rule under test: **this service's model carries `awsQueryCompatible`, so every error
//! it answers names its class TWICE — the body's `__type` names the error SHAPE that was
//! thrown, and the `x-amzn-query-error` response header reports that class's LEGACY QUERY
//! CODE and the fault side (`Sender` for a refusal the caller caused, `Receiver` for a
//! failure the service caused) — and nothing in the REQUEST decides whether it does.** The
//! code is the error shape's modelled `awsQueryError` where the model states one and the
//! shape's own error code where the model defaults that binding; a parameter refusal raised
//! by the request-validation framework in front of the service names that framework's shape
//! in the body and the service's own legacy code in the header, which is why the two names
//! cannot be read off each other.
//!
//! `x-amzn-query-mode` is a REQUEST-side opt-in with which a client says which vocabulary
//! it prefers to read — not a switch that turns the response carrier on — so an error answer
//! is the same whether a client sends it, sends it set to something else, or sends none at
//! all. That is what makes two SDKs agree about one refusal: one that resolves a class the
//! model shapes no exception for from the legacy code (the AWS SDK for JavaScript v3, which
//! sends no opt-in) and one that reads it from the same header after opting in (botocore)
//! report the same error class.
//!
//! The table spans what the rule generalises over rather than one recorded request: the
//! LAYER the refusal comes from (a handler's modelled error, a refusal this service words
//! for a value the boundary walk rejected, and the framework parameter refusals), the
//! source of the CODE (stated by the model's `awsQueryError`, defaulted by the model so the
//! shape's own name is the code, and not modelled at all), the operation, the STATUS (400
//! and 403 — both a Sender fault, which is what the fault column pins) and the OPT-IN the
//! answer is asked for with. Every expected shape id and legacy code is the constant the
//! model or the service itself declares, never a string copied out of an answer.
//!
//! A MODELLED error's `__type` is its FULLY QUALIFIED shape id — `com.amazonaws.sqs#<Shape>`,
//! the id the service's own model declares for the class — which is what a modern client
//! resolves its typed exception from; a parameter refusal raised by the request-validation
//! framework names that framework's own shape id instead, because the class belongs to that
//! layer and not to this service's model.
//!
//! # The other wire protocol the same API is served over
//!
//! Amazon SQS publishes ONE API over TWO wire protocols, so the suite also pins what the
//! OTHER one answers. A client that sends the AWS Query protocol — a `POST` of a
//! form-encoded body naming the operation in its `Action` field — is answered with an XML
//! document: `<{Operation}Response><{Operation}Result>…</…><ResponseMetadata><RequestId>`
//! for a call that was served, `<ErrorResponse><Error><Type>…<Code>…<Message>` for one that
//! was refused, `text/xml` either way, with the refusal named by the LEGACY query code its
//! class is known by. Both protocols reach the same queues, because they are two spellings
//! of one API. A JSON request this endpoint cannot resolve an operation for is refused with
//! the class an AWS endpoint answers for an unresolvable target
//! (`UnknownOperationException`), and a request declaring a media type neither protocol
//! reads a document in is not read at all.

use serde_json::{json, Value};
use service_com_amazonaws_sqs_amazonsqs::scaffolded::{
    INVALID_PARAMETER_VALUE, INVALID_PARAMETER_VALUE_SHAPE_ID, MISSING_PARAMETER,
    MISSING_PARAMETER_SHAPE_ID,
};
use smithy_server_runtime::MultiServiceRouter;

// The AWS Query request builder, the answer a client reads off the wire and that surface's
// media types and API version are the shared harness's
// ([`super::support::query_call`]): this suite and the absence suite drive the same
// surface, so they drive it through one builder rather than two.
use super::support::{
    authorization_in, call, call_with_query_mode, create_ok, dispatched as answered, emulator,
    error_shape, expected_url, messages, ok, query_call as query, request_with, QueryField,
    WireAnswer as Answered, FORM_CONTENT_TYPE, HOST, QUERY_API_VERSION, QUERY_ERROR_HEADER,
    QUERY_MODE_ON, REGION, XML_CONTENT_TYPE,
};

/// The legacy query code the model states for a queue that does not exist, and for a purge
/// already under way: from
/// `smithy_query(query="shape", shape="QueueDoesNotExist,PurgeQueueInProgress",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`,
/// whose `awsQueryError.code` is what a query-era client knows those classes by. Neither is
/// the shape's own name, which is what makes them the rows that pin "the model's code wins".
const NON_EXISTENT_QUEUE: &str = "AWS.SimpleQueueService.NonExistentQueue";
const PURGE_IN_PROGRESS: &str = "AWS.SimpleQueueService.PurgeQueueInProgress";

/// The namespace this service's model declares its shapes in
/// (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`:
/// service shape `com.amazonaws.sqs#AmazonSQS`), which is the package a body names one of its
/// error classes under.
const NAMESPACE: &str = "com.amazonaws.sqs";

/// The shape id a body names the modelled class `local` under.
fn modelled(local: &str) -> String {
    format!("{NAMESPACE}#{local}")
}

/// Every way a client can spell the opt-in, including not spelling it at all: the first row
/// is a native JSON client (and the AWS SDK for JavaScript v3), the second a
/// query-compatible SDK, and the rest spell the header in ways nothing about the ANSWER may
/// key on. Every row of [`rows`] is asked for with each of these.
fn opt_ins() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        ("no opt-in header at all", None),
        (
            "the opt-in a query-compatible SDK sends",
            Some(QUERY_MODE_ON),
        ),
        ("the opt-in in another case", Some("TRUE")),
        ("an opt-in turned off", Some("false")),
        ("an opt-in carrying something else", Some("maybe")),
        ("an empty opt-in", Some("")),
    ]
}

/// One row: the case, the operation, the request, the status it owes, the error SHAPE its
/// `__type` names and the LEGACY CODE its header reports.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    status: u16,
    shape: String,
    code: String,
}

/// The fault side the report names for `status` — the caller's for a refusal (4xx), the
/// service's for a failure of its own (5xx).
fn fault_for(status: u16) -> &'static str {
    if status >= 500 {
        "Receiver"
    } else {
        "Sender"
    }
}

/// The report `status` and `code` owe: `<code>;<fault>`.
fn expected_report(code: &str, status: u16) -> String {
    format!("{code};{}", fault_for(status))
}

/// The value of one response header, or `None` when the answer carries none.
fn header(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

/// Every row, over an emulator already holding the queues they name.
///
/// The queues are made through this service's own `CreateQueue`, and the purge row's queue
/// has already been purged once, so the refusal it drives is the state the service really
/// holds rather than one asserted into place.
async fn rows(router: &MultiServiceRouter) -> Vec<Row> {
    let queue = create_ok(router, "envelope-queue", &json!({})).await;
    let purged = create_ok(router, "envelope-purged", &json!({})).await;
    ok(router, "PurgeQueue", json!({"QueueUrl": purged})).await;
    let absent = expected_url("envelope-never-created");

    vec![
        // A handler's refusal of a parameter VALUE: not modelled on the service at all, so
        // the body names the validation framework's shape and the header the legacy code.
        Row {
            case: "a VisibilityTimeout outside the range the parameter documents",
            operation: "ChangeMessageVisibility",
            request: json!({
                "QueueUrl": queue,
                "ReceiptHandle": "any-handle",
                "VisibilityTimeout": -1,
            }),
            status: 400,
            shape: INVALID_PARAMETER_VALUE_SHAPE_ID.to_string(),
            code: INVALID_PARAMETER_VALUE.to_string(),
        },
        Row {
            case: "a MaxNumberOfMessages outside the range the parameter documents",
            operation: "ReceiveMessage",
            request: json!({"QueueUrl": queue, "MaxNumberOfMessages": 11}),
            status: 400,
            shape: INVALID_PARAMETER_VALUE_SHAPE_ID.to_string(),
            code: INVALID_PARAMETER_VALUE.to_string(),
        },
        // The same class on a send: the answer a client with no typed exception for the
        // modern code has to read its error off the LEGACY code in.
        Row {
            case: "a DelaySeconds above the range the parameter documents",
            operation: "SendMessage",
            request: json!({"QueueUrl": queue, "MessageBody": "body", "DelaySeconds": 901}),
            status: 400,
            shape: INVALID_PARAMETER_VALUE_SHAPE_ID.to_string(),
            code: INVALID_PARAMETER_VALUE.to_string(),
        },
        // A required parameter supplied with nothing in it: the other framework class.
        Row {
            case: "a MessageBody holding no character",
            operation: "SendMessage",
            request: json!({"QueueUrl": queue, "MessageBody": ""}),
            status: 400,
            shape: MISSING_PARAMETER_SHAPE_ID.to_string(),
            code: MISSING_PARAMETER.to_string(),
        },
        // A modelled class whose `awsQueryError` binding the model DEFAULTS: the code is
        // the shape's own name, and the framework's own renderer states none, so the
        // report is what this service completes.
        Row {
            case: "an attribute value outside what the attribute accepts, on a create",
            operation: "CreateQueue",
            request: json!({
                "QueueName": "envelope-bad-attribute",
                "Attributes": {"VisibilityTimeout": "-1"},
            }),
            status: 400,
            shape: modelled("InvalidAttributeValue"),
            code: "InvalidAttributeValue".to_string(),
        },
        Row {
            case: "an attribute value outside what the attribute accepts, on an update",
            operation: "SetQueueAttributes",
            request: json!({"QueueUrl": queue, "Attributes": {"DelaySeconds": "1000000"}}),
            status: 400,
            shape: modelled("InvalidAttributeValue"),
            code: "InvalidAttributeValue".to_string(),
        },
        Row {
            case: "a message body outside the allowed character set",
            operation: "SendMessage",
            request: json!({"QueueUrl": queue, "MessageBody": "ring\u{7}ring"}),
            status: 400,
            shape: modelled("InvalidMessageContents"),
            code: "InvalidMessageContents".to_string(),
        },
        // The BOUNDARY walk's refusal, worded by this service: rendered outside the
        // per-operation sequence, so it is the row that pins the report reaching that
        // layer too.
        Row {
            case: "an attribute name no queue has",
            operation: "GetQueueAttributes",
            request: json!({"QueueUrl": queue, "AttributeNames": ["NotAnAttribute"]}),
            status: 400,
            shape: modelled("InvalidAttributeName"),
            code: "InvalidAttributeName".to_string(),
        },
        // A modelled class whose code the model STATES: the report carries that code, not
        // the shape's name.
        Row {
            case: "a url naming a queue this account does not have",
            operation: "GetQueueAttributes",
            request: json!({"QueueUrl": absent, "AttributeNames": ["All"]}),
            status: 400,
            shape: modelled("QueueDoesNotExist"),
            code: NON_EXISTENT_QUEUE.to_string(),
        },
        // The same class reached by NAME rather than by url, which is the operation a
        // client resolves a queue through before it has a url at all.
        Row {
            case: "a name naming a queue this account does not have",
            operation: "GetQueueUrl",
            request: json!({"QueueName": "envelope-never-created"}),
            status: 400,
            shape: modelled("QueueDoesNotExist"),
            code: NON_EXISTENT_QUEUE.to_string(),
        },
        // A refusal that is a 4xx but not a 400: still the caller's fault.
        Row {
            case: "a purge of a queue whose purge is still running",
            operation: "PurgeQueue",
            request: json!({"QueueUrl": purged}),
            status: 403,
            shape: modelled("PurgeQueueInProgress"),
            code: PURGE_IN_PROGRESS.to_string(),
        },
    ]
}

/// Every refusal reports its legacy query code and fault side beside the error shape in the
/// body — for a client that opted in, for one that sent no opt-in at all, and for one that
/// spelled the opt-in any other way.
#[tokio::test]
async fn every_refusal_reports_its_legacy_query_code_and_fault() {
    let router = emulator();
    for row in rows(&router).await {
        let expected = expected_report(&row.code, row.status);
        for (opt_in_case, opt_in) in opt_ins() {
            let (status, headers, body) =
                call_with_query_mode(&router, row.operation, &row.request, opt_in).await;
            let report = header(&headers, QUERY_ERROR_HEADER);
            assert_eq!(
                (status, error_shape(&body), report.clone()),
                (row.status, row.shape.clone(), Some(expected.clone())),
                "case {:?} with {opt_in_case:?}: expected HTTP {} with __type {:?} and \
                 {QUERY_ERROR_HEADER} {expected:?}, got {status} with {body} and {report:?}",
                row.case,
                row.status,
                row.shape,
            );
        }
    }
}

/// The whole error answer — status, the class its body names, the sentence and the report
/// beside them — is the same for every way a client can spell the opt-in, which is what "the
/// request does not decide it" means: a client that sends nothing reads the answer a
/// query-compatible SDK reads.
#[tokio::test]
async fn the_opt_in_decides_nothing_about_an_error_answer() {
    let router = emulator();
    for row in rows(&router).await {
        let mut answers = Vec::new();
        for (opt_in_case, opt_in) in opt_ins() {
            let (status, headers, body) =
                call_with_query_mode(&router, row.operation, &row.request, opt_in).await;
            answers.push((
                opt_in_case,
                status,
                error_shape(&body),
                body.get("message").cloned().unwrap_or(Value::Null),
                header(&headers, QUERY_ERROR_HEADER),
            ));
        }
        let (first_case, first_status, first_shape, first_message, first_report) =
            answers[0].clone();
        for (opt_in_case, status, shape, message, report) in &answers[1..] {
            assert_eq!(
                (*status, shape, message, report),
                (first_status, &first_shape, &first_message, &first_report),
                "case {:?} with {opt_in_case:?}: expected the same answer as with \
                 {first_case:?} (HTTP {first_status}, __type {first_shape}, message \
                 {first_message}, {QUERY_ERROR_HEADER} {first_report:?}), got {status} with \
                 __type {shape}, message {message} and {report:?}",
                row.case,
            );
        }
    }
}

/// An ACCEPTED call names no error class, so it carries no report — whatever the request
/// said about the opt-in.
#[tokio::test]
async fn an_accepted_call_reports_no_error_class() {
    let router = emulator();
    let url = create_ok(&router, "envelope-accepted", &json!({})).await;
    for (case, operation, request) in [
        (
            "a create",
            "CreateQueue",
            json!({"QueueName": "envelope-created"}),
        ),
        (
            "a send",
            "SendMessage",
            json!({"QueueUrl": url, "MessageBody": "hello"}),
        ),
        (
            "a read",
            "GetQueueAttributes",
            json!({"QueueUrl": url, "AttributeNames": ["All"]}),
        ),
    ] {
        for (opt_in_case, opt_in) in opt_ins() {
            let (status, headers, body) =
                call_with_query_mode(&router, operation, &request, opt_in).await;
            let report = header(&headers, QUERY_ERROR_HEADER);
            assert_eq!(
                (status, report.clone()),
                (200, None),
                "case {case:?} with {opt_in_case:?}: expected HTTP 200 with no \
                 {QUERY_ERROR_HEADER}, got {status} with {body} and {report:?}"
            );
        }
    }
}

/// The media type this endpoint's `awsJson1_0` wire carries a document in.
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.0";

/// The target prefix a JSON client of this endpoint names the service by: the model's own
/// service shape name, which is what an SDK builds `X-Amz-Target` from.
const TARGET_PREFIX: &str = "AmazonSQS";

/// The class this endpoint answers a JSON request whose target it cannot resolve to an
/// operation, and the status it carries.
const UNKNOWN_OPERATION: &str = "com.amazon.coral.service#UnknownOperationException";
const UNKNOWN_OPERATION_STATUS: u16 = 400;

/// The sentence the AWS Query protocol words a queue that does not exist with — the
/// WSDL-versioned surface's own, which is NOT the sentence the `awsJson1_0` surface answers
/// (`state::QUEUE_DOES_NOT_EXIST_MESSAGE`): one API, two protocols, two sentences for the
/// one class, and the same code and status on both.
const QUEUE_DOES_NOT_EXIST_IN_THIS_VERSION: &str =
    "The specified queue does not exist for this wsdl version.";

/// The whole document that refusal carries: the class as its own empty element.
const UNKNOWN_OPERATION_DOCUMENT: &str = "<UnknownOperationException/>\n";

/// One form parameter of a query request: the name, which is this suite's own literal, and
/// the value it carries — the shared harness's field type, so a row built here is a row
/// that builder takes.
type Param = QueryField;

/// One row of the SERVED table: the case, the action, its parameters, and the elements the
/// answer owes as `(element, expected text)`.
type ServedRow<'a> = (&'a str, &'a str, Vec<Param>, Vec<(&'a str, String)>);

/// One row of the REFUSED table: the case, the action, its parameters, the status and the
/// code the answer owes, and the sentence this protocol words the class with when it words
/// one of its own.
type RefusedRow<'a> = (&'a str, &'a str, Vec<Param>, u16, &'a str, Option<&'a str>);

/// One form parameter, for the rows that build a request's parameters: the name is this
/// suite's own literal, and only the value is owned.
fn param(name: &'static str, value: &str) -> Param {
    (name, value.to_string())
}

/// The XML document an answer carries, which must parse: this is the step a client's own
/// parser takes, and the one a JSON body served to a query client fails at.
fn document<'a>(answer: &'a Answered, case: &str) -> roxmltree::Document<'a> {
    assert_eq!(
        answer.content_type.as_deref(),
        Some(XML_CONTENT_TYPE),
        "case {case}: expected {XML_CONTENT_TYPE}, got {:?} with {}",
        answer.content_type,
        answer.body
    );
    roxmltree::Document::parse(&answer.body)
        .unwrap_or_else(|why| panic!("case {case}: the answer must be XML ({why}): {}", answer.body))
}

/// The text of the first element called `name` in `document`, read through the parser.
fn element(document: &roxmltree::Document<'_>, name: &str) -> Option<String> {
    document
        .descendants()
        .find(|node| node.has_tag_name(name))
        .map(|node| node.text().unwrap_or_default().to_string())
}

/// Every element called `name`, in document order.
fn elements(document: &roxmltree::Document<'_>, name: &str) -> Vec<String> {
    document
        .descendants()
        .filter(|node| node.has_tag_name(name))
        .map(|node| node.text().unwrap_or_default().to_string())
        .collect()
}

/// A call the AWS Query protocol SERVES is answered with that protocol's XML envelope:
/// `<{Action}Response>` wrapping `<{Action}Result>` and a `<ResponseMetadata><RequestId>`,
/// as `text/xml`, with each member of the answer under the name this API's own model gives
/// it on that wire.
///
/// The rows span the kinds of answer the envelope has to carry: a scalar member, a
/// flattened MAP (`<Attribute><Name>…<Value>…`), a flattened LIST of scalars
/// (`<QueueUrl>…`), a flattened list of STRUCTURES (`<Message><Body>…`) and an answer with
/// no member at all.
#[tokio::test]
async fn the_query_protocol_answers_the_xml_envelope_for_a_call_it_serves() {
    let router = emulator();
    let name = "query-served";
    let url = expected_url(name);

    // Created through the query protocol itself, with an attribute supplied in the
    // `Attribute.N.Name` / `Attribute.N.Value` form that protocol carries a map in.
    let created = query(
        &router,
        "CreateQueue",
        &[
            param("QueueName", name),
            param("Attribute.1.Name", "VisibilityTimeout"),
            param("Attribute.1.Value", "45"),
        ],
    )
    .await;
    let created_document = document(&created, "a create");
    assert_eq!(
        (created.status, element(&created_document, "QueueUrl")),
        (200, Some(url.clone())),
        "case a create: expected HTTP 200 carrying {url}, got {} with {}",
        created.status,
        created.body
    );
    assert_eq!(
        created_document.root_element().tag_name().name(),
        "CreateQueueResponse",
        "case a create: expected the operation's own response element, got {}",
        created.body
    );

    let sent = query(
        &router,
        "SendMessage",
        &[param("QueueUrl", &url), param("MessageBody", "payload")],
    )
    .await;
    let sent_document = document(&sent, "a send");
    let message_id = element(&sent_document, "MessageId").unwrap_or_default();
    assert!(
        sent.status == 200 && !message_id.is_empty(),
        "case a send: expected HTTP 200 carrying a MessageId, got {} with {}",
        sent.status,
        sent.body
    );

    // (case, action, parameters, the elements the answer owes as (name, expected text))
    let rows: Vec<ServedRow<'_>> = vec![
        (
            "a url looked up by name",
            "GetQueueUrl",
            vec![param("QueueName", name)],
            vec![("QueueUrl", url.clone())],
        ),
        (
            "a map of attributes",
            "GetQueueAttributes",
            vec![
                param("QueueUrl", &url),
                param("AttributeName.1", "VisibilityTimeout"),
            ],
            vec![
                ("Name", "VisibilityTimeout".to_string()),
                ("Value", "45".to_string()),
            ],
        ),
        (
            "a list of queue urls",
            "ListQueues",
            vec![param("QueueNamePrefix", name)],
            vec![("QueueUrl", url.clone())],
        ),
        (
            "a list of messages",
            "ReceiveMessage",
            vec![param("QueueUrl", &url), param("VisibilityTimeout", "0")],
            vec![("Body", "payload".to_string())],
        ),
        (
            "an answer with no member at all",
            "SetQueueAttributes",
            vec![
                param("QueueUrl", &url),
                param("Attribute.1.Name", "VisibilityTimeout"),
                param("Attribute.1.Value", "45"),
            ],
            vec![],
        ),
    ];

    for (case, action, params, owed) in &rows {
        let answer = query(&router, action, params).await;
        let parsed = document(&answer, case);
        assert_eq!(
            (answer.status, parsed.root_element().tag_name().name()),
            (200, format!("{action}Response").as_str()),
            "case {case}: expected HTTP 200 in a <{action}Response>, got {} with {}",
            answer.status,
            answer.body
        );
        // Every answer of this protocol names the request it answered.
        let request_id = element(&parsed, "RequestId").unwrap_or_default();
        assert!(
            !request_id.is_empty(),
            "case {case}: expected a ResponseMetadata RequestId, got {}",
            answer.body
        );
        for (owed_element, expected) in owed {
            assert_eq!(
                element(&parsed, owed_element).as_deref(),
                Some(expected.as_str()),
                "case {case}: expected <{owed_element}>{expected}</{owed_element}>, got {}",
                answer.body
            );
        }
    }
}

/// A call the AWS Query protocol REFUSES is answered with that protocol's error document —
/// `<ErrorResponse><Error><Type>…</Type><Code>…</Code><Message>…</Message></Error>`, as
/// `text/xml` — with the class named by the LEGACY query code it is known by and the status
/// its class carries.
///
/// The rows span the layer the refusal comes from (a handler's modelled error, a refusal
/// this service words for a value it will not take, and the protocol's own routing
/// failure), the source of the code (stated by the model's `awsQueryError`, defaulted to
/// the shape's name, and not modelled at all) and the status (400 and 403).
#[tokio::test]
async fn the_query_protocol_answers_the_xml_error_document_for_a_refusal() {
    let router = emulator();
    let url = create_ok(&router, "query-refused", &json!({})).await;
    let purged = create_ok(&router, "query-refused-purged", &json!({})).await;
    ok(&router, "PurgeQueue", json!({"QueueUrl": purged})).await;
    let absent = expected_url("query-never-created");
    let no_such_action = "The action NotAnAction is not valid for this web service.";

    // (case, action, parameters, status, code, the sentence this protocol words it with)
    let rows: Vec<RefusedRow<'_>> = vec![
        (
            "a send to a queue this account does not have",
            "SendMessage",
            vec![param("QueueUrl", &absent), param("MessageBody", "payload")],
            400,
            NON_EXISTENT_QUEUE,
            Some(QUEUE_DOES_NOT_EXIST_IN_THIS_VERSION),
        ),
        (
            "a name lookup for a queue this account does not have",
            "GetQueueUrl",
            vec![param("QueueName", "query-never-created")],
            400,
            NON_EXISTENT_QUEUE,
            Some(QUEUE_DOES_NOT_EXIST_IN_THIS_VERSION),
        ),
        (
            "a create naming a value the attribute does not take",
            "CreateQueue",
            vec![
                param("QueueName", "query-bad-attribute"),
                param("Attribute.1.Name", "VisibilityTimeout"),
                param("Attribute.1.Value", "-1"),
            ],
            400,
            "InvalidAttributeValue",
            None,
        ),
        (
            "a read naming an attribute no queue has",
            "GetQueueAttributes",
            vec![
                param("QueueUrl", &url),
                param("AttributeName.1", "NotAnAttribute"),
            ],
            400,
            "InvalidAttributeName",
            None,
        ),
        (
            "a send whose required parameter carries nothing",
            "SendMessage",
            vec![param("QueueUrl", &url), param("MessageBody", "")],
            400,
            MISSING_PARAMETER,
            None,
        ),
        (
            "a send carrying a body character the service will not take",
            "SendMessage",
            vec![param("QueueUrl", &url), param("MessageBody", "ring\u{7}ring")],
            400,
            "InvalidMessageContents",
            None,
        ),
        (
            "a purge of a queue whose purge is still running",
            "PurgeQueue",
            vec![param("QueueUrl", &purged)],
            403,
            PURGE_IN_PROGRESS,
            None,
        ),
        (
            "an action this API does not publish",
            "NotAnAction",
            vec![],
            400,
            "InvalidAction",
            Some(no_such_action),        ),
    ];

    for (case, action, params, status, code, worded) in &rows {
        let answer = query(&router, action, params).await;
        let parsed = document(&answer, case);
        let fault = fault_for(*status);
        assert_eq!(
            (
                answer.status,
                parsed.root_element().tag_name().name(),
                element(&parsed, "Code").as_deref(),
                element(&parsed, "Type").as_deref(),
            ),
            (*status, "ErrorResponse", Some(*code), Some(fault)),
            "case {case}: expected HTTP {status} <Code>{code}</Code> <Type>{fault}</Type>, \
             got {} with {}",
            answer.status,
            answer.body
        );
        let message = element(&parsed, "Message").unwrap_or_default();
        assert!(
            !message.is_empty(),
            "case {case}: a refusal must carry the sentence the caller fixes the request \
             from, got {}",
            answer.body
        );
        if let Some(expected) = worded {
            assert_eq!(
                message, *expected,
                "case {case}: expected the sentence this protocol words the class with, got {}",
                answer.body
            );
        }
    }

    // A request that supplied no action at all names the other class this protocol has for
    // a request that named no operation.
    let authorization = authorization_in(REGION);
    let no_action = answered(
        &router,
        request_with(
            &[
                ("host", HOST),
                ("content-type", FORM_CONTENT_TYPE),
                ("authorization", authorization.as_str()),
            ],
            format!("Version={QUERY_API_VERSION}").into_bytes(),
        ),
    )
    .await;
    let parsed = document(&no_action, "a request that named no action");
    assert_eq!(
        (
            no_action.status,
            element(&parsed, "Code").as_deref(),
            element(&parsed, "Message").as_deref(),
        ),
        (
            400,
            Some("MissingAction"),
            Some("No action was supplied with this request.")
        ),
        "case a request that named no action: expected the class this protocol answers for \
         one, got {} with {}",
        no_action.status,
        no_action.body
    );
}

/// The two wire protocols reach ONE queue: what a query client sends, a JSON client
/// receives, and what a JSON client configures, a query client reports — in both
/// directions, because the protocols are two spellings of one API.
#[tokio::test]
async fn a_query_client_and_a_json_client_share_one_queue() {
    let router = emulator();
    let name = "query-and-json";
    // Created by the QUERY client; every later call names the url it minted.
    let created = query(&router, "CreateQueue", &[param("QueueName", name)]).await;
    let url = element(&document(&created, "a shared create"), "QueueUrl").unwrap_or_default();
    assert_eq!(
        url,
        expected_url(name),
        "case a shared create: expected the url of {name}, got {}",
        created.body
    );

    // Query in, JSON out.
    let sent = query(
        &router,
        "SendMessage",
        &[param("QueueUrl", &url), param("MessageBody", "from-query")],
    )
    .await;
    let query_message_id =
        element(&document(&sent, "a send by the query client"), "MessageId").unwrap_or_default();
    let received = messages(
        &ok(
            &router,
            "ReceiveMessage",
            json!({"QueueUrl": url, "VisibilityTimeout": 0}),
        )
        .await,
    );
    assert_eq!(
        received
            .iter()
            .map(|message| (
                message["MessageId"].as_str().unwrap_or_default().to_string(),
                message["Body"].as_str().unwrap_or_default().to_string()
            ))
            .collect::<Vec<_>>(),
        vec![(query_message_id, "from-query".to_string())],
        "case query in, JSON out: the JSON client must be handed the query client's message"
    );

    // JSON in, query out.
    let sent = ok(
        &router,
        "SendMessage",
        json!({"QueueUrl": url, "MessageBody": "from-json"}),
    )
    .await;
    let json_message_id = sent["MessageId"].as_str().unwrap_or_default().to_string();
    let handed = query(
        &router,
        "ReceiveMessage",
        &[
            param("QueueUrl", &url),
            param("MaxNumberOfMessages", "10"),
            param("VisibilityTimeout", "0"),
        ],
    )
    .await;
    let parsed = document(&handed, "a receive by the query client");
    assert!(
        elements(&parsed, "Body").contains(&"from-json".to_string())
            && elements(&parsed, "MessageId").contains(&json_message_id),
        "case JSON in, query out: the query client must be handed the JSON client's \
         message, got {}",
        handed.body
    );

    // Configured by the JSON client, reported to the query client.
    let _ = ok(
        &router,
        "SetQueueAttributes",
        json!({"QueueUrl": url, "Attributes": {"VisibilityTimeout": "77"}}),
    )
    .await;
    let reported = query(
        &router,
        "GetQueueAttributes",
        &[
            param("QueueUrl", &url),
            param("AttributeName.1", "VisibilityTimeout"),
        ],
    )
    .await;
    let parsed = document(&reported, "a configuration read by the query client");
    assert_eq!(
        (
            element(&parsed, "Name").as_deref(),
            element(&parsed, "Value").as_deref()
        ),
        (Some("VisibilityTimeout"), Some("77")),
        "case the shared configuration: the query client must report what the JSON client \
         set, got {}",
        reported.body
    );
}

/// A JSON request this endpoint cannot resolve an operation for is refused with the class
/// an AWS endpoint answers for an unresolvable target — the whole document, with no
/// member beside the class, because there is no parameter or value for one to be about.
///
/// The rows span the ways a request can name no operation: no target header at all, a
/// target naming nothing this API publishes, and a target with no operation in it.
#[tokio::test]
async fn a_json_request_that_names_no_operation_is_refused_as_an_unknown_operation() {
    let router = emulator();
    let authorization = authorization_in(REGION);
    let body = json!({"QueueName": "unresolvable"}).to_string().into_bytes();
    // (case, the headers the request carries)
    let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "no target header at all",
            vec![
                ("host", HOST),
                ("content-type", JSON_CONTENT_TYPE),
                ("authorization", authorization.as_str()),
            ],
        ),
        (
            "no target header and no content type either",
            vec![("host", HOST), ("authorization", authorization.as_str())],
        ),
        (
            "a target naming an operation this API does not publish",
            vec![
                ("host", HOST),
                ("content-type", JSON_CONTENT_TYPE),
                ("x-amz-target", "AmazonSQS.NotAnOperation"),
                ("authorization", authorization.as_str()),
            ],
        ),
        (
            "a target naming no operation at all",
            vec![
                ("host", HOST),
                ("content-type", JSON_CONTENT_TYPE),
                ("x-amz-target", TARGET_PREFIX),
                ("authorization", authorization.as_str()),
            ],
        ),
    ];

    for (case, headers) in &rows {
        let answer = answered(&router, request_with(headers, body.clone())).await;
        let parsed: Value = serde_json::from_str(&answer.body).unwrap_or(Value::Null);
        assert_eq!(
            (answer.status, parsed),
            (
                UNKNOWN_OPERATION_STATUS,
                json!({ "__type": UNKNOWN_OPERATION })
            ),
            "case {case}: expected HTTP {UNKNOWN_OPERATION_STATUS} carrying only \
             {UNKNOWN_OPERATION}, got {} with {}",
            answer.status,
            answer.body
        );
    }
}

/// A request DECLARING a media type neither of this endpoint's protocols reads a document
/// in is not served: it is answered HTTP 404 with no document at all, whatever operation
/// its target names, and nothing it carried is read — the queue it asked for does not
/// appear. A request declaring a media type one of the two protocols does read, or
/// declaring none at all, is served exactly as before.
///
/// The rows span both sides of that line: the media types a JSON document, an XML document,
/// a CBOR document and plain text are carried in, against this endpoint's own JSON media
/// type (bare and with a parameter), the AWS Query protocol's form encoding, and a request
/// that declares no media type at all.
#[tokio::test]
async fn a_request_in_a_media_type_this_endpoint_does_not_read_is_not_served() {
    let router = emulator();
    let authorization = authorization_in(REGION);
    // (case, the media type the request declares, served)
    let rows: &[(&str, Option<&str>, bool)] = &[
        ("this endpoint's own JSON media type", Some(JSON_CONTENT_TYPE), true),
        (
            "the same with a charset parameter",
            Some("application/x-amz-json-1.0; charset=utf-8"),
            true,
        ),
        (
            "the JSON media type of an adjacent protocol version",
            Some("application/x-amz-json-1.1"),
            true,
        ),
        ("no media type at all", None, true),
        ("a plain JSON document", Some("application/json"), false),
        ("an XML document", Some("application/xml"), false),
        ("a CBOR document", Some("application/cbor"), false),
        ("plain text", Some("text/plain"), false),
    ];

    for (index, (case, declared, served)) in rows.iter().enumerate() {
        let name = format!("media-type-{index}");
        let body = json!({"QueueName": name}).to_string().into_bytes();
        let mut headers: Vec<(&str, &str)> = vec![
            ("host", HOST),
            ("x-amz-target", "AmazonSQS.CreateQueue"),
            ("authorization", authorization.as_str()),
        ];
        if let Some(declared) = declared {
            headers.push(("content-type", declared));
        }
        let answer = answered(&router, request_with(&headers, body)).await;
        if *served {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the request to be read and served, got {} with {}",
                answer.status, answer.body
            );
        } else {
            assert_eq!(
                (answer.status, answer.body.as_str()),
                (404, UNKNOWN_OPERATION_DOCUMENT),
                "case {case}: expected HTTP 404 naming the unresolvable operation, got {} with \
                 {}",
                answer.status,
                answer.body
            );
        }
        // Nothing a request this endpoint did not read carried may have reached the store:
        // the queue it named exists only for the rows that were served.
        let (status, reported) = call(&router, "GetQueueUrl", &json!({"QueueName": name})).await;
        let resolved = status == 200;
        assert_eq!(
            resolved, *served,
            "case {case}: expected the queue to exist={served}, got {status} with {reported}"
        );
    }
}
