// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ONE refusal every operation of this service shares.
//!
//! The rule under test: **a request naming a queue this account does not have is refused
//! as `QueueDoesNotExist` carrying Amazon SQS's own fixed message for that absence — "The
//! specified queue does not exist." — whatever the request named, whichever operation it
//! called, and whether it addressed the queue by its `QueueUrl` or by the `QueueName` the
//! one name-addressed operation resolves.** The message is a constant — the error CODE
//! (`AWS.SimpleQueueService.NonExistentQueue` / `QueueDoesNotExist`, from the model's
//! `awsQueryError`) is what identifies the condition — so a client matching on AWS's
//! text recognises the absence, and the queue the caller probed for is never reported
//! back to it. No clause of the retired Query/XML endpoint is appended to it.
//!
//! The table spans what the rule generalises over: the OPERATION (every one that resolves
//! a queue, addressed by url and by name), and the FORM of the absence (a name never
//! created, a url of another account, an owner id that is not this emulator's, a bare name
//! no queue carries, and a queue that was deleted), and the DIALECT the client speaks (the
//! native JSON one and the query-compatible one, each answered the legacy query code and
//! fault BESIDE the same sentence, because an `awsQueryCompatible` service reports both
//! names of a class on every error whatever the request asked for). The expected message is
//! the constant exported by [`service_com_amazonaws_sqs_amazonsqs::state`], which one test
//! pins to AWS's own sentence written out literally.
//!
//! They span one axis further: the WIRE SURFACE the request arrived on. This one API is
//! published over two of them, and each words this class with its own fixed sentence —
//! the sentence above on the `awsJson1_0` surface every current SDK sends, and the
//! WSDL-versioned one on the form-encoded AWS Query surface. The surface decides the
//! sentence; the operation never does, so no operation that resolves a queue may answer
//! one surface's wording to the other's client.
//!
//! The rows further span WHAT the queue was and held when it went away — nothing, messages
//! nobody had received, a message received and still in flight (probed with the receipt
//! handle that queue itself minted), tags and a changed attribute, a FIFO queue's message
//! group, the dead-letter role a live source queue still redrives to — because the condition
//! is the absence of the queue and nothing of its history or its type is part of it. And they
//! pin the one class this sentence must NOT be collapsed into: the same error code also
//! carries a second, distinct fixed sentence for a queue that IS there but is withheld from
//! the caller, and no refusal of an absent queue may read it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_sqs_amazonsqs::state::{
    QUEUE_DOES_NOT_EXIST_IN_THIS_VERSION, QUEUE_DOES_NOT_EXIST_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::support::{
    account, call, call_in_query_mode, call_with_headers, create_ok, dead_letter_queue, emulator,
    error_code, fifo_attributes, messages, ok, query_call, send_all, xml_element, QueryField, HOST,
    OTHER_ACCOUNT, QUERY_ERROR_HEADER, XML_CONTENT_TYPE,
};

/// AWS's own sentence for a queue the account does not hold, written out here rather than
/// read off the service, so the constant the service exports stays pinned to it.
///
/// RECORDED: a `SendMessage` on the url of a queue that was deleted is refused HTTP 400
/// `AWS.SimpleQueueService.NonExistentQueue` with exactly this text, and a `GetQueueUrl`
/// for a name the account holds no queue under with the same. The model
/// (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
/// fixes no text, so the served sentence is the requirement; it names no queue, so it has
/// no variable part.
const AWS_SENTENCE: &str = "The specified queue does not exist.";

/// The `message` member of a refusal, as awsJson renders it.
fn message_of(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// Every operation that resolves a queue from a `QueueUrl`, with the rest of a request
/// that is otherwise well formed — so the only thing the refusal can be about is the
/// queue.
fn url_requests(url: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("GetQueueAttributes", json!({"QueueUrl": url, "AttributeNames": ["All"]})),
        (
            "SetQueueAttributes",
            json!({"QueueUrl": url, "Attributes": {"VisibilityTimeout": "10"}}),
        ),
        (
            "AddPermission",
            json!({"QueueUrl": url, "Label": "shared",
                   "AWSAccountIds": [OTHER_ACCOUNT], "Actions": ["SendMessage"]}),
        ),
        (
            "RemovePermission",
            json!({"QueueUrl": url, "Label": "shared"}),
        ),
        ("SendMessage", json!({"QueueUrl": url, "MessageBody": "hello"})),
        (
            "SendMessageBatch",
            json!({"QueueUrl": url,
                   "Entries": [{"Id": "one", "MessageBody": "hello"}]}),
        ),
        ("ReceiveMessage", json!({"QueueUrl": url})),
        (
            "ChangeMessageVisibility",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle",
                   "VisibilityTimeout": 30}),
        ),
        (
            "ChangeMessageVisibilityBatch",
            json!({"QueueUrl": url,
                   "Entries": [{"Id": "one", "ReceiptHandle": "not-a-handle",
                                "VisibilityTimeout": 30}]}),
        ),
        (
            "DeleteMessage",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle"}),
        ),
        (
            "DeleteMessageBatch",
            json!({"QueueUrl": url,
                   "Entries": [{"Id": "one", "ReceiptHandle": "not-a-handle"}]}),
        ),
        ("DeleteQueue", json!({"QueueUrl": url})),
        ("PurgeQueue", json!({"QueueUrl": url})),
        ("ListQueueTags", json!({"QueueUrl": url})),
        ("ListDeadLetterSourceQueues", json!({"QueueUrl": url})),
        ("TagQueue", json!({"QueueUrl": url, "Tags": {"team": "platform"}})),
        ("UntagQueue", json!({"QueueUrl": url, "TagKeys": ["team"]})),
    ]
}

/// Assert one refusal: a 400 `QueueDoesNotExist` whose message is AWS's constant for the
/// form the request addressed the queue in, and which mentions nothing of the request.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    probed: &str,
    expected: &str,
) {
    let (status, parsed) = call(router, operation, request).await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "QueueDoesNotExist"),
        "case {operation} on {case}: expected a 400 QueueDoesNotExist, got {status} with {parsed}"
    );
    let message = message_of(&parsed);
    assert_eq!(
        message, expected,
        "case {operation} on {case}: expected AWS's fixed message, got {message:?}"
    );
    assert!(
        !message.contains(probed),
        "case {operation} on {case}: the refusal must not report the probed {probed:?} back, \
         got {message:?}"
    );
}

/// Every operation addressed by a `QueueUrl` refuses every form of an ADDRESS no queue of
/// this account answers to with the same code and the same fixed message.
///
/// Every row here is a queue ADDRESS — a `/<accountId>/<QueueName>` pair, or a bare queue
/// name — whose queue is absent. A value that is no address of this endpoint at all is the
/// other fault, refused `InvalidAddress`; that class belongs to the addressing suite
/// (`tests/all/queue_url_endpoint_form.rs`).
#[tokio::test]
async fn every_url_addressed_operation_refuses_an_absent_queue_with_the_fixed_message() {
    let router = emulator();
    let _ = create_ok(&router, "present", &json!({})).await;

    let cases: Vec<(&str, String, String)> = vec![
        (
            "a name that was never created",
            format!("https://{HOST}/{}/never-created", account()),
            "never-created".to_string(),
        ),
        (
            "another account's queue of a name this account has",
            format!("https://{HOST}/210987654321/present"),
            "210987654321".to_string(),
        ),
        (
            "a bare name no queue of this account carries",
            "never-created".to_string(),
            "never-created".to_string(),
        ),
        (
            "this account's address of a name no queue carries, with no endpoint before it",
            format!("{}/never-created", account()),
            "never-created".to_string(),
        ),
    ];

    for (case, url, probed) in &cases {
        for (operation, request) in url_requests(url) {
            assert_refused(
                &router,
                case,
                operation,
                &request,
                probed,
                QUEUE_DOES_NOT_EXIST_MESSAGE,
            )
            .await;
        }
    }
}

/// `GetQueueUrl` resolves a queue by NAME, and refuses a name this account has no queue
/// for — and a request naming another owner — with the message AWS answers a name with:
/// nothing of the name or the owner id the caller supplied appears in it.
#[tokio::test]
async fn get_queue_url_refuses_an_absent_name_with_the_fixed_message() {
    let router = emulator();
    let url = create_ok(&router, "resolvable", &json!({})).await;

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a name that was never created",
            json!({"QueueName": "absent-queue-name"}),
            "absent-queue-name",
        ),
        (
            "a name differing only in case from one that exists",
            json!({"QueueName": "Resolvable"}),
            "Resolvable",
        ),
        (
            "an owner that is not this emulator's account",
            json!({"QueueName": "resolvable", "QueueOwnerAWSAccountId": "210987654321"}),
            "210987654321",
        ),
    ];

    for (case, request, probed) in &cases {
        assert_refused(
            &router,
            case,
            "GetQueueUrl",
            request,
            probed,
            QUEUE_DOES_NOT_EXIST_MESSAGE,
        )
        .await;
    }

    // The queue that does exist still resolves, so the refusals above are about the queue
    // and not about the operation.
    let (status, parsed) = call(&router, "GetQueueUrl", &json!({"QueueName": "resolvable"})).await;
    assert_eq!(
        (status, parsed.get("QueueUrl").and_then(Value::as_str)),
        (200, Some(url.as_str())),
        "case a name that exists: expected its url, got {status} with {parsed}"
    );
}

/// A queue that EXISTED and was deleted is the same absence: the refusal is identical to
/// one for a name that was never created, so nothing leaks the fact that the queue once
/// existed.
#[tokio::test]
async fn a_deleted_queue_is_refused_exactly_as_one_that_never_existed() {
    let router = emulator();
    let url = create_ok(&router, "transient", &json!({})).await;
    let (status, parsed) = call(&router, "DeleteQueue", &json!({"QueueUrl": url})).await;
    assert_eq!(
        status, 200,
        "case deleting the queue: expected HTTP 200, got {status} with {parsed}"
    );

    for (operation, request) in url_requests(&url) {
        assert_refused(
            &router,
            "a queue that was deleted",
            operation,
            &request,
            "transient",
            QUEUE_DOES_NOT_EXIST_MESSAGE,
        )
        .await;
    }
    assert_refused(
        &router,
        "the name of a queue that was deleted",
        "GetQueueUrl",
        &json!({"QueueName": "transient"}),
        "transient",
        QUEUE_DOES_NOT_EXIST_MESSAGE,
    )
    .await;
}

/// Every refusal of an absent queue carries AWS's own sentence and nothing after it —
/// asserted against the literal text rather than the service's constants, so a wording
/// that drifts away from what AWS serves (the retired Query/XML endpoint's trailing
/// clause, say) fails here.
///
/// The rows span both forms of address (`QueueUrl` and `QueueName`), every operation that
/// resolves a queue, and both ways a queue can be absent (never created, and created then
/// deleted): one sentence answers all of them.
#[tokio::test]
async fn every_refusal_of_an_absent_queue_carries_awss_own_sentence() {
    let router = emulator();
    let deleted = create_ok(&router, "sentence-deleted", &json!({})).await;
    let (status, parsed) = call(&router, "DeleteQueue", &json!({"QueueUrl": deleted})).await;
    assert_eq!(
        status, 200,
        "case deleting the queue: expected HTTP 200, got {status} with {parsed}"
    );
    let never_created = format!("https://{HOST}/{}/sentence-never-created", account());

    let mut requests: Vec<(&str, &'static str, Value)> = Vec::new();
    for (case, url) in [
        ("a url that was deleted", &deleted),
        ("a url that was never created", &never_created),
    ] {
        for (operation, request) in url_requests(url) {
            requests.push((case, operation, request));
        }
    }
    requests.push((
        "a name that was never created",
        "GetQueueUrl",
        json!({"QueueName": "sentence-never-created"}),
    ));
    requests.push((
        "a name that was deleted",
        "GetQueueUrl",
        json!({"QueueName": "sentence-deleted"}),
    ));

    for (case, operation, request) in &requests {
        let (status, parsed) = call(&router, operation, request).await;
        let message = message_of(&parsed);
        assert_eq!(
            (status, error_code(&parsed).as_str(), message.as_str()),
            (400, "QueueDoesNotExist", AWS_SENTENCE),
            "case {operation} on {case}: expected a 400 QueueDoesNotExist reading \
             {AWS_SENTENCE:?}, got {status} with {parsed}"
        );
    }
}

/// The legacy query code this class travels under on every answer that reports it: the
/// model's `awsQueryError.code` for the error shape
/// (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`),
/// beside the fault side a refusal the caller caused is reported under.
const NON_EXISTENT_QUEUE_REPORT: &str = "AWS.SimpleQueueService.NonExistentQueue;Sender";

/// The value of one response header, or `None` when the answer carries none.
fn header_of(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

/// The DIALECT a client speaks decides NOTHING about this refusal: the fixed sentence, the
/// error class and the legacy query code reported beside it are the same for a client that
/// opts into the legacy report and for a native one that sends no opt-in at all — an
/// `awsQueryCompatible` service reports both names of a class on every error it answers, so
/// no clause of either vocabulary is withheld from either client.
///
/// This is the half of the rule a dialect could drift on: the rows run every operation
/// that resolves a queue, over the forms of absence, through BOTH dialects of the one
/// answer and compare the sentence and the report across them.
#[tokio::test]
async fn the_refusal_reads_the_same_in_both_wire_dialects() {
    let router = emulator();
    let deleted = create_ok(&router, "dialect-deleted", &json!({})).await;
    let (status, parsed) = call(&router, "DeleteQueue", &json!({"QueueUrl": deleted})).await;
    assert_eq!(
        status, 200,
        "case deleting the queue: expected HTTP 200, got {status} with {parsed}"
    );

    let forms: Vec<(&str, String)> = vec![
        ("a url that was deleted", deleted.clone()),
        (
            "a url that was never created",
            format!("https://{HOST}/{}/dialect-never-created", account()),
        ),
        (
            "a bare name no queue of this account carries",
            "dialect-never-created".to_string(),
        ),
        (
            "another account's url for a name this account held",
            format!("https://{HOST}/{OTHER_ACCOUNT}/dialect-deleted"),
        ),
    ];

    let mut requests: Vec<(&str, &'static str, Value)> = Vec::new();
    for (case, url) in &forms {
        for (operation, request) in url_requests(url) {
            requests.push((case, operation, request));
        }
    }
    requests.push((
        "a name that was never created",
        "GetQueueUrl",
        json!({"QueueName": "dialect-never-created"}),
    ));
    requests.push((
        "a name that was deleted",
        "GetQueueUrl",
        json!({"QueueName": "dialect-deleted"}),
    ));

    for (case, operation, request) in &requests {
        let (query_status, query_headers, query_body) =
            call_in_query_mode(&router, operation, request).await;
        let query_message = message_of(&query_body);
        assert_eq!(
            (
                query_status,
                error_code(&query_body).as_str(),
                query_message.as_str(),
                header_of(&query_headers, QUERY_ERROR_HEADER),
            ),
            (
                400,
                "QueueDoesNotExist",
                AWS_SENTENCE,
                Some(NON_EXISTENT_QUEUE_REPORT.to_string()),
            ),
            "case {operation} on {case} in the query-compatible dialect: expected a 400 \
             QueueDoesNotExist reading {AWS_SENTENCE:?} reported as \
             {NON_EXISTENT_QUEUE_REPORT:?}, got {query_status} with {query_body} and \
             {QUERY_ERROR_HEADER} {:?}",
            header_of(&query_headers, QUERY_ERROR_HEADER)
        );

        let (native_status, native_headers, native_body) =
            call_with_headers(&router, operation, request).await;
        let native_message = message_of(&native_body);
        assert_eq!(
            (
                native_status,
                error_code(&native_body).as_str(),
                native_message.as_str(),
                header_of(&native_headers, QUERY_ERROR_HEADER),
            ),
            (
                400,
                "QueueDoesNotExist",
                AWS_SENTENCE,
                Some(NON_EXISTENT_QUEUE_REPORT.to_string()),
            ),
            "case {operation} on {case} in the native dialect: expected a 400 \
             QueueDoesNotExist reading {AWS_SENTENCE:?} reported as \
             {NON_EXISTENT_QUEUE_REPORT:?}, got \
             {native_status} with {native_body} and {QUERY_ERROR_HEADER} {:?}",
            header_of(&native_headers, QUERY_ERROR_HEADER)
        );

        assert_eq!(
            (query_message.as_str(), header_of(&query_headers, QUERY_ERROR_HEADER)),
            (
                native_message.as_str(),
                header_of(&native_headers, QUERY_ERROR_HEADER)
            ),
            "case {operation} on {case}: neither the sentence nor the legacy report may \
             depend on the dialect the client speaks, got {query_message:?} / {:?} for a \
             query-compatible client and {native_message:?} / {:?} for a native one",
            header_of(&query_headers, QUERY_ERROR_HEADER),
            header_of(&native_headers, QUERY_ERROR_HEADER)
        );
    }
}

/// The sentence this service exports for the absence is AWS's own, so every operation that
/// renders it renders what AWS serves.
#[test]
fn the_exported_refusal_message_is_awss_own_sentence() {
    assert_eq!(
        QUEUE_DOES_NOT_EXIST_MESSAGE, AWS_SENTENCE,
        "case the exported refusal message: expected {AWS_SENTENCE:?}, got {:?}",
        QUEUE_DOES_NOT_EXIST_MESSAGE
    );
}

/// The OTHER fixed sentence AWS serves under this same error code: the one for the class
/// where the queue IS there and the caller may not address it, which names both facts
/// without saying which holds.
///
/// RECORDED, and written out here so the two classes of the one code stay distinguishable:
/// every row of this suite is the ABSENCE class, so none of them may read this sentence.
const NOT_DISCLOSED_SENTENCE: &str =
    "The specified queue does not exist or you do not have access to it.";

/// How long a row looks for the message it sent before giving up on taking a receipt handle
/// from it. Generous: what the row needs is a handle at all, never how fast the host runs.
const RECEIVE_DEADLINE: Duration = Duration::from_secs(60);

/// The longest one receive in that loop waits, inside the documented
/// `ReceiveMessageRequest$WaitTimeSeconds` range ("an integer from 0 to 20").
const RECEIVE_WAIT_SECONDS: u64 = 1;

/// A receipt handle the queue at `url` minted, by receiving one of its messages and leaving
/// it in flight — looked for in a loop until it is handed over or [`RECEIVE_DEADLINE`]
/// passes.
async fn outstanding_handle(router: &MultiServiceRouter, case: &str, url: &str) -> String {
    let deadline = Instant::now() + RECEIVE_DEADLINE;
    while Instant::now() < deadline {
        let reply = ok(
            router,
            "ReceiveMessage",
            json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 1,
                "WaitTimeSeconds": RECEIVE_WAIT_SECONDS,
            }),
        )
        .await;
        if let Some(handle) = messages(&reply)
            .first()
            .and_then(|message| message.get("ReceiptHandle"))
            .and_then(Value::as_str)
        {
            return handle.to_string();
        }
    }
    panic!("case {case}: expected a message of {url} to come over so the row can keep its handle")
}

/// What a row's queue WAS, and held, at the moment it was deleted — the axis these rows span,
/// and the one thing the refusal they then earn may not depend on.
#[derive(Debug, Clone, Copy)]
enum Held {
    /// Nothing at all: created and deleted.
    Nothing,
    /// Messages nobody had received.
    AvailableMessages,
    /// One message received and not deleted, whose receipt handle the row probes with too.
    AnInFlightMessage,
    /// Tags and an attribute changed away from its default.
    TagsAndAttributes,
    /// A FIFO queue holding one message group.
    AFifoMessageGroup,
    /// The dead-letter target a source queue that is still there redrives to.
    ADeadLetterRole,
}

/// A queue in the state `held` describes, as `(its url, a receipt handle it minted, the url of
/// a queue that must still be there afterwards)`.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    held: Held,
) -> (String, Option<String>, Option<String>) {
    match held {
        Held::Nothing => (create_ok(router, name, &json!({})).await, None, None),
        Held::AvailableMessages => {
            let url = create_ok(router, name, &json!({})).await;
            send_all(router, &url, &["one".to_string(), "two".to_string()]).await;
            (url, None, None)
        }
        Held::AnInFlightMessage => {
            let url = create_ok(router, name, &json!({})).await;
            send_all(router, &url, &["in flight".to_string()]).await;
            let handle = outstanding_handle(router, case, &url).await;
            (url, Some(handle), None)
        }
        Held::TagsAndAttributes => {
            let url = create_ok(router, name, &json!({"VisibilityTimeout": "45"})).await;
            ok(
                router,
                "TagQueue",
                json!({"QueueUrl": &url, "Tags": {"team": "platform"}}),
            )
            .await;
            (url, None, None)
        }
        Held::AFifoMessageGroup => {
            let url = create_ok(
                router,
                name,
                &fifo_attributes(&[("ContentBasedDeduplication", "true")]),
            )
            .await;
            ok(
                router,
                "SendMessage",
                json!({"QueueUrl": &url, "MessageBody": "grouped", "MessageGroupId": "one"}),
            )
            .await;
            (url, None, None)
        }
        Held::ADeadLetterRole => {
            let (dead_letter_url, _, source_url) = dead_letter_queue(router, name).await;
            (dead_letter_url, None, Some(source_url))
        }
    }
}

/// The absence of a queue is the whole condition: every operation that resolves one refuses a
/// queue that is gone with the one fixed sentence, whatever that queue was and held when it
/// went away — and never with the second sentence this error code carries for a queue that is
/// there but withheld from the caller.
///
/// The rows differ along what the queue held and what kind of queue it was ([`Held`]); the
/// columns are every operation that resolves a `QueueUrl`, the one that resolves a
/// `QueueName`, and — for the row that kept one — the operations taking the receipt handle
/// that queue itself minted, which names an incarnation that is now gone.
#[tokio::test]
async fn an_absent_queue_earns_the_one_sentence_whatever_it_held() {
    let router = emulator();

    let rows: [(&str, &str, Held); 6] = [
        ("a queue deleted while empty", "gone-empty", Held::Nothing),
        (
            "a queue deleted while it held messages nobody had received",
            "gone-with-messages",
            Held::AvailableMessages,
        ),
        (
            "a queue deleted while a received message was still in flight",
            "gone-with-inflight",
            Held::AnInFlightMessage,
        ),
        (
            "a queue deleted while it carried tags and a changed attribute",
            "gone-with-tags",
            Held::TagsAndAttributes,
        ),
        (
            "a FIFO queue deleted while it held a message group",
            "gone-fifo.fifo",
            Held::AFifoMessageGroup,
        ),
        (
            "a dead-letter queue deleted while its source queue is still there",
            "gone-dead-letter",
            Held::ADeadLetterRole,
        ),
    ];

    for (case, name, held) in rows {
        let (url, handle, still_there) = given_queue(&router, case, name, held).await;
        let (status, parsed) = call(&router, "DeleteQueue", &json!({"QueueUrl": &url})).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the delete of {url} to be taken, got {status} with {parsed}"
        );

        let mut probes: Vec<(&str, Value)> = url_requests(&url);
        probes.push(("GetQueueUrl", json!({"QueueName": name})));
        if let Some(handle) = &handle {
            // A handle names the incarnation that minted it, so a handle of a queue that is
            // gone addresses a queue that does not exist — the same absence, worded the same.
            probes.push((
                "DeleteMessage",
                json!({"QueueUrl": &url, "ReceiptHandle": handle}),
            ));
            probes.push((
                "ChangeMessageVisibility",
                json!({"QueueUrl": &url, "ReceiptHandle": handle, "VisibilityTimeout": 30}),
            ));
            probes.push((
                "DeleteMessageBatch",
                json!({"QueueUrl": &url,
                       "Entries": [{"Id": "one", "ReceiptHandle": handle}]}),
            ));
        }

        for (operation, request) in &probes {
            let (status, parsed) = call(&router, operation, request).await;
            let message = message_of(&parsed);
            assert_eq!(
                (status, error_code(&parsed).as_str(), message.as_str()),
                (400, "QueueDoesNotExist", AWS_SENTENCE),
                "case {operation} on {case}: expected a 400 QueueDoesNotExist reading \
                 {AWS_SENTENCE:?}, got {status} with {parsed}"
            );
            assert_ne!(
                message, NOT_DISCLOSED_SENTENCE,
                "case {operation} on {case}: the absence of a queue and a queue withheld from \
                 the caller are two classes of this one error code and keep their own \
                 sentences, got {message:?}"
            );
            assert!(
                !message.contains(name),
                "case {operation} on {case}: the refusal must not report the queue it was \
                 probed with back, got {message:?}"
            );
        }

        // The refusals above are about the queue that is gone and nothing else: a queue of the
        // same account that is still there is still served.
        if let Some(source_url) = &still_there {
            let (status, parsed) = call(
                &router,
                "GetQueueAttributes",
                &json!({"QueueUrl": source_url, "AttributeNames": ["All"]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: expected the source queue at {source_url} to still be served, \
                 got {status} with {parsed}"
            );
        }
    }
}

/// The sentence the WSDL-versioned AWS Query surface words this class with, written out here
/// so neither surface's wording can drift into the other's: it is the JSON surface's sentence
/// plus that surface's own trailing clause, and no client of the JSON surface may be handed
/// it.
const QUERY_SURFACE_SENTENCE: &str = "The specified queue does not exist for this wsdl version.";

/// The legacy query code this class is named by on either surface — the model's
/// `awsQueryError.code` for the error shape — which is what the query surface's
/// `<Code>` element carries.
const NON_EXISTENT_QUEUE_CODE: &str = "AWS.SimpleQueueService.NonExistentQueue";

/// The fault side a refusal the caller caused is reported under, which that surface's
/// `<Type>` element carries.
const SENDER_FAULT: &str = "Sender";

/// The SAME requests as [`url_requests`], in the form the AWS Query protocol carries them in,
/// so one row drives one operation on both surfaces of this API.
///
/// A list or a map arrives under the wire names this API's own model gives it
/// (`smithy_query(query="shape", shape="TagQueueRequest,UntagQueueRequest,
/// AddPermissionRequest,SendMessageBatchRequest,DeleteMessageBatchRequest,
/// ChangeMessageVisibilityBatchRequest,GetQueueAttributesRequest,SetQueueAttributesRequest",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`):
/// `Tag.N.Key`/`Tag.N.Value` for `Tags` (`@xmlName("Tag") @xmlFlattened` over a `TagMap`
/// whose key and value are `@xmlName("Key")`/`@xmlName("Value")`), `TagKey.N` for `TagKeys`,
/// `AWSAccountId.N` and `ActionName.N` for `AddPermission`, `AttributeName.N` for an
/// attribute-name list, `Attribute.N.Name`/`Attribute.N.Value` for an attribute map, and
/// `<EntryShape>.N.<member>` for each batch's entry list.
///
/// Everything beside the `QueueUrl` is well formed, so the only thing either answer can be
/// about is the queue.
fn url_query_requests(url: &str) -> Vec<(&'static str, Vec<QueryField>)> {
    let fields = |rest: &[(&'static str, &str)]| -> Vec<QueryField> {
        let mut fields: Vec<QueryField> = vec![("QueueUrl", url.to_string())];
        fields.extend(
            rest.iter()
                .map(|(name, value)| (*name, (*value).to_string())),
        );
        fields
    };
    vec![
        ("GetQueueAttributes", fields(&[("AttributeName.1", "All")])),
        (
            "SetQueueAttributes",
            fields(&[
                ("Attribute.1.Name", "VisibilityTimeout"),
                ("Attribute.1.Value", "10"),
            ]),
        ),
        (
            "AddPermission",
            fields(&[
                ("Label", "shared"),
                ("AWSAccountId.1", OTHER_ACCOUNT),
                ("ActionName.1", "SendMessage"),
            ]),
        ),
        ("RemovePermission", fields(&[("Label", "shared")])),
        ("SendMessage", fields(&[("MessageBody", "hello")])),
        (
            "SendMessageBatch",
            fields(&[
                ("SendMessageBatchRequestEntry.1.Id", "one"),
                ("SendMessageBatchRequestEntry.1.MessageBody", "hello"),
            ]),
        ),
        ("ReceiveMessage", fields(&[])),
        (
            "ChangeMessageVisibility",
            fields(&[
                ("ReceiptHandle", "not-a-handle"),
                ("VisibilityTimeout", "30"),
            ]),
        ),
        (
            "ChangeMessageVisibilityBatch",
            fields(&[
                ("ChangeMessageVisibilityBatchRequestEntry.1.Id", "one"),
                (
                    "ChangeMessageVisibilityBatchRequestEntry.1.ReceiptHandle",
                    "not-a-handle",
                ),
                (
                    "ChangeMessageVisibilityBatchRequestEntry.1.VisibilityTimeout",
                    "30",
                ),
            ]),
        ),
        (
            "DeleteMessage",
            fields(&[("ReceiptHandle", "not-a-handle")]),
        ),
        (
            "DeleteMessageBatch",
            fields(&[
                ("DeleteMessageBatchRequestEntry.1.Id", "one"),
                (
                    "DeleteMessageBatchRequestEntry.1.ReceiptHandle",
                    "not-a-handle",
                ),
            ]),
        ),
        ("DeleteQueue", fields(&[])),
        ("PurgeQueue", fields(&[])),
        ("ListQueueTags", fields(&[])),
        ("ListDeadLetterSourceQueues", fields(&[])),
        (
            "TagQueue",
            fields(&[("Tag.1.Key", "team"), ("Tag.1.Value", "platform")]),
        ),
        ("UntagQueue", fields(&[("TagKey.1", "team")])),
    ]
}

/// The SURFACE a request arrived on decides the sentence, and the operation decides nothing:
/// every operation that resolves a queue answers the `awsJson1_0` surface AWS's current
/// sentence and the form-encoded AWS Query surface that surface's WSDL-versioned one, with
/// HTTP 400 and the one legacy code on both.
///
/// This is the half of the rule a single operation could drift on, because each one reaches
/// the shared refusal through its own resolution step: the rows run EVERY operation that
/// resolves a `QueueUrl` over the forms of absence, on both surfaces, and compare the two
/// sentences — so a JSON client is never handed the query surface's trailing clause and a
/// query client is never handed the JSON surface's sentence.
#[tokio::test]
async fn the_wire_surface_decides_the_sentence_and_the_operation_decides_nothing() {
    let router = emulator();
    let deleted = create_ok(&router, "surface-deleted", &json!({})).await;
    let (status, parsed) = call(&router, "DeleteQueue", &json!({"QueueUrl": &deleted})).await;
    assert_eq!(
        status, 200,
        "case deleting the queue: expected HTTP 200, got {status} with {parsed}"
    );

    let forms: Vec<(&str, String)> = vec![
        ("a url that was deleted", deleted.clone()),
        (
            "a url that was never created",
            format!("https://{HOST}/{}/surface-never-created", account()),
        ),
        (
            "another account's url for a name this account held",
            format!("https://{HOST}/{OTHER_ACCOUNT}/surface-deleted"),
        ),
        (
            "a bare name no queue of this account carries",
            "surface-never-created".to_string(),
        ),
    ];

    for (case, url) in &forms {
        let json_rows = url_requests(url);
        let query_rows = url_query_requests(url);
        // The two tables are one table read on two surfaces: an operation named by only one
        // of them would leave that surface unasserted for it.
        let json_operations: Vec<&str> = json_rows
            .iter()
            .map(|(operation, _)| *operation)
            .collect();
        let query_operations: Vec<&str> = query_rows
            .iter()
            .map(|(operation, _)| *operation)
            .collect();
        assert_eq!(
            json_operations, query_operations,
            "case {case}: every operation driven on one surface must be driven on the other, \
             got {json_operations:?} and {query_operations:?}"
        );

        for (operation, request) in &json_rows {
            let (status, parsed) = call(&router, operation, request).await;
            let message = message_of(&parsed);
            assert_eq!(
                (status, error_code(&parsed).as_str(), message.as_str()),
                (400, "QueueDoesNotExist", AWS_SENTENCE),
                "case {operation} on {case} over the JSON surface: expected a 400 \
                 QueueDoesNotExist reading {AWS_SENTENCE:?}, got {status} with {parsed}"
            );
            assert_ne!(
                message, QUERY_SURFACE_SENTENCE,
                "case {operation} on {case} over the JSON surface: the WSDL-versioned \
                 surface's sentence may not be answered to a JSON client, got {message:?}"
            );
        }

        for (operation, fields) in &query_rows {
            let answer = query_call(&router, operation, fields).await;
            let code = xml_element(&answer.body, "Code");
            let message = xml_element(&answer.body, "Message");
            assert_eq!(
                (
                    answer.status,
                    answer.content_type.as_deref(),
                    code.as_deref(),
                    message.as_deref(),
                    xml_element(&answer.body, "Type").as_deref(),
                ),
                (
                    400,
                    Some(XML_CONTENT_TYPE),
                    Some(NON_EXISTENT_QUEUE_CODE),
                    Some(QUERY_SURFACE_SENTENCE),
                    Some(SENDER_FAULT),
                ),
                "case {operation} on {case} over the AWS Query surface: expected a 400 \
                 {NON_EXISTENT_QUEUE_CODE} reading {QUERY_SURFACE_SENTENCE:?} as \
                 {XML_CONTENT_TYPE}, got {} with {:?} and {}",
                answer.status,
                answer.content_type,
                answer.body
            );
            assert_ne!(
                message.as_deref(),
                Some(AWS_SENTENCE),
                "case {operation} on {case} over the AWS Query surface: that surface words \
                 this class with its own sentence, got {message:?}"
            );
        }
    }
}

/// Each surface's sentence is the string that surface serves, written out above: the JSON
/// surface's is AWS's current one, and the query surface's is that one with its own trailing
/// clause — so neither constant can be edited into the other's wording.
#[test]
fn each_surface_exports_the_sentence_it_serves() {
    assert_eq!(
        QUEUE_DOES_NOT_EXIST_MESSAGE, AWS_SENTENCE,
        "case the JSON surface's exported sentence: expected {AWS_SENTENCE:?}, got {:?}",
        QUEUE_DOES_NOT_EXIST_MESSAGE
    );
    assert_eq!(
        QUEUE_DOES_NOT_EXIST_IN_THIS_VERSION, QUERY_SURFACE_SENTENCE,
        "case the AWS Query surface's exported sentence: expected \
         {QUERY_SURFACE_SENTENCE:?}, got {:?}",
        QUEUE_DOES_NOT_EXIST_IN_THIS_VERSION
    );
    assert_ne!(
        QUEUE_DOES_NOT_EXIST_MESSAGE, QUEUE_DOES_NOT_EXIST_IN_THIS_VERSION,
        "case the two surfaces' sentences: expected them to stay distinct, got {:?} for both",
        QUEUE_DOES_NOT_EXIST_MESSAGE
    );
}

/// The two sentences this one error code carries are two values: an absent queue reads the
/// absence sentence, and the one for a queue withheld from the caller is a different string,
/// so a client can tell the classes apart.
#[test]
fn the_absence_sentence_is_not_the_one_for_a_queue_withheld_from_the_caller() {
    assert_ne!(
        AWS_SENTENCE, NOT_DISCLOSED_SENTENCE,
        "case the two sentences of this error code: expected them to stay distinct, got \
         {AWS_SENTENCE:?} for both"
    );
    assert_ne!(
        QUEUE_DOES_NOT_EXIST_MESSAGE, NOT_DISCLOSED_SENTENCE,
        "case the exported refusal message: expected the sentence for an absent queue rather \
         than the one for a queue withheld from the caller, got {:?}",
        QUEUE_DOES_NOT_EXIST_MESSAGE
    );
}
