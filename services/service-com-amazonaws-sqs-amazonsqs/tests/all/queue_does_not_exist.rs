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
//! no queue carries, and a queue that was deleted). The expected message is the
//! constant exported by [`service_com_amazonaws_sqs_amazonsqs::state`], which one test
//! pins to AWS's own sentence written out literally.

use serde_json::{json, Value};
use service_com_amazonaws_sqs_amazonsqs::state::QUEUE_DOES_NOT_EXIST_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use super::support::{account, call, create_ok, emulator, error_code, HOST, OTHER_ACCOUNT};

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
