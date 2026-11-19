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

use serde_json::{json, Value};
use service_com_amazonaws_sqs_amazonsqs::state::QUEUE_DOES_NOT_EXIST_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use super::support::{
    account, call, call_in_query_mode, call_with_headers, create_ok, emulator, error_code, HOST,
    OTHER_ACCOUNT, QUERY_ERROR_HEADER,
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

/// Every operation addressed by a `QueueUrl` refuses every form of a url no queue of this
/// account answers to with the same code and the same fixed message.
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
            "a url with no account segment",
            format!("https://{HOST}/present"),
            "present".to_string(),
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
