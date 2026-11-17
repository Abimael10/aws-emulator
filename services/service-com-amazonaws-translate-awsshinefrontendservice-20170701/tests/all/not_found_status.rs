// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the status Amazon Translate puts on a refusal that
//! reports a resource absent.
//!
//! The rule: this service answers such a refusal under the modelled
//! `ResourceNotFoundException` shape but at the HTTP status its API reference
//! publishes for that class — 400, the status of a client error — not at the 404
//! the shape's `@httpError` declares (`smithy_query query=shape
//! shape=ResourceNotFoundException`). It holds for every operation of this
//! service that can report a resource absent, however the request addresses that
//! resource: by terminology name, by job id, or by resource ARN.
//!
//! The suite is a TABLE whose rows vary what the rule generalises over — the
//! operation, the kind of resource reported absent, the way it is addressed, and
//! whether the resource never existed, was deleted, or was superseded — and a
//! control case per addressing form, so a row's 400 is the not-found refusal and
//! not a request this service refuses for some other reason.
//!
//! What such a refusal SAYS is the operation's own wording, and a second table
//! below pins the one this service words for a batch translation JOB: an
//! operation that resolves a job by id and finds none states the fixed sentence
//! `Request failed due to job not found.`, with nothing interpolated into it —
//! neither the job id the request asked for nor anything out of the store — and
//! both operations that resolve a job by id state it identically.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::terminology_lifecycle::{
    call, error_code, expected_arn, import, message, router, EN_FR_CSV,
};

/// The status Amazon Translate answers a resource-not-found refusal with.
const NOT_FOUND_STATUS: u16 = 400;

/// The `__type` such a refusal carries, which is the modelled shape's own name.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// A terminology that IS imported, so every row below is refused because of the
/// resource it addresses rather than because the store is empty.
const PRESENT: &str = "present-terms";

/// A terminology imported and then deleted.
const DELETED: &str = "deleted-terms";

/// A terminology imported twice, which supersedes the ARN the first import
/// minted.
const SUPERSEDED: &str = "superseded-terms";

/// A job id satisfying `JobId`'s modelled length and pattern that no submission
/// ever started.
const UNKNOWN_JOB_ID: &str = "0123456789abcdef0123456789abcdef";

/// A `DataAccessRoleArn` satisfying the member's modelled pattern.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/TranslateBatchRole";

/// One row: the request, and what it addresses that is not there.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
}

/// A request body translating one blob of text with the terminologies named.
fn translate_text_body(terminologies: &[&str]) -> Value {
    json!({
        "Text": "hello world",
        "SourceLanguageCode": "en",
        "TargetLanguageCode": "fr",
        "TerminologyNames": terminologies,
    })
}

/// The same for a document translation.
fn translate_document_body(terminologies: &[&str]) -> Value {
    json!({
        "Document": {
            "Content": aws_smithy_types::base64::encode(b"hello world"),
            "ContentType": "text/plain",
        },
        "SourceLanguageCode": "en",
        "TargetLanguageCode": "fr",
        "TerminologyNames": terminologies,
    })
}

/// A batch submission naming `terminologies` and `parallel_data`, with
/// everything else a submission needs well-formed.
fn start_job_body(terminologies: &[&str], parallel_data: &[&str]) -> Value {
    let mut body = json!({
        "ClientToken": "token-not-found",
        "DataAccessRoleArn": ROLE_ARN,
        "SourceLanguageCode": "en",
        "TargetLanguageCodes": ["fr"],
        "InputDataConfig": {
            "S3Uri": "s3://translate-batch-documents/input/",
            "ContentType": "text/plain",
        },
        "OutputDataConfig": {"S3Uri": "s3://translate-batch-documents/output/"},
    });
    if !terminologies.is_empty() {
        body["TerminologyNames"] = json!(terminologies);
    }
    if !parallel_data.is_empty() {
        body["ParallelDataNames"] = json!(parallel_data);
    }
    body
}

/// Import `PRESENT`, leave `DELETED` imported and deleted, and leave
/// `SUPERSEDED` re-imported — then answer the ARN the first import of
/// `SUPERSEDED` minted and the ARN of the deleted resource.
async fn stored_resources(router: &MultiServiceRouter) -> (String, String) {
    for name in [PRESENT, DELETED, SUPERSEDED] {
        let (status, body) = import(router, name, EN_FR_CSV, "CSV", Some("UNI")).await;
        assert_eq!(status, 200, "{name}: ImportTerminology answered {status} {body}");
    }
    let deleted_arn = expected_arn(DELETED, EN_FR_CSV);
    let superseded_arn = expected_arn(SUPERSEDED, EN_FR_CSV);

    let (status, body) = call(router, "DeleteTerminology", &json!({"Name": DELETED})).await;
    assert_eq!(status, 200, "DeleteTerminology answered {status} {body}");

    // A second import under the same name mints a new ARN, because the ARN
    // carries the imported file's revision.
    let other_file: &[u8] = b"en,fr\nhello,salut\n";
    let (status, body) = import(router, SUPERSEDED, other_file, "CSV", Some("UNI")).await;
    assert_eq!(status, 200, "the re-import answered {status} {body}");

    (deleted_arn, superseded_arn)
}

fn rows(deleted_arn: &str, superseded_arn: &str) -> Vec<Row> {
    let unknown_arn = expected_arn("no-such-terms", b"no-such-file");
    vec![
        Row {
            case: "reading a terminology name no import ever stored",
            operation: "GetTerminology",
            body: json!({"Name": "no-such-terms"}),
        },
        Row {
            case: "reading a terminology name that was deleted",
            operation: "GetTerminology",
            body: json!({"Name": DELETED}),
        },
        Row {
            case: "deleting a terminology name no import ever stored",
            operation: "DeleteTerminology",
            body: json!({"Name": "no-such-terms"}),
        },
        Row {
            case: "deleting a terminology name that is already gone",
            operation: "DeleteTerminology",
            body: json!({"Name": DELETED}),
        },
        Row {
            case: "translating text with a terminology no import stored",
            operation: "TranslateText",
            body: translate_text_body(&["no-such-terms"]),
        },
        Row {
            case: "translating text with a terminology that was deleted",
            operation: "TranslateText",
            body: translate_text_body(&[DELETED]),
        },
        Row {
            case: "translating a document with a terminology no import stored",
            operation: "TranslateDocument",
            body: translate_document_body(&["no-such-terms"]),
        },
        Row {
            case: "translating a document with a terminology that was deleted",
            operation: "TranslateDocument",
            body: translate_document_body(&[DELETED]),
        },
        Row {
            case: "tagging an ARN of a name no import ever used",
            operation: "TagResource",
            body: json!({"ResourceArn": unknown_arn, "Tags": [{"Key": "env", "Value": "prod"}]}),
        },
        Row {
            case: "tagging the ARN of a deleted resource",
            operation: "TagResource",
            body: json!({"ResourceArn": deleted_arn, "Tags": []}),
        },
        Row {
            case: "tagging the superseded ARN of a re-imported resource",
            operation: "TagResource",
            body: json!({
                "ResourceArn": superseded_arn,
                "Tags": [{"Key": "env", "Value": "prod"}, {"Key": "owner", "Value": "nlp"}],
            }),
        },
        Row {
            case: "tagging an ARN of another service",
            operation: "TagResource",
            body: json!({"ResourceArn": "arn:aws:s3:::a-bucket", "Tags": []}),
        },
        Row {
            case: "untagging an ARN of a name no import ever used",
            operation: "UntagResource",
            body: json!({"ResourceArn": unknown_arn, "TagKeys": ["env"]}),
        },
        Row {
            case: "untagging the ARN of a deleted resource, naming no key",
            operation: "UntagResource",
            body: json!({"ResourceArn": deleted_arn, "TagKeys": []}),
        },
        Row {
            case: "untagging a string that is not an ARN at all",
            operation: "UntagResource",
            body: json!({"ResourceArn": "not-an-arn", "TagKeys": ["env", "owner"]}),
        },
        Row {
            case: "listing the tags of an ARN of a name no import ever used",
            operation: "ListTagsForResource",
            body: json!({"ResourceArn": unknown_arn}),
        },
        Row {
            case: "listing the tags of the superseded ARN of a re-imported resource",
            operation: "ListTagsForResource",
            body: json!({"ResourceArn": superseded_arn}),
        },
        Row {
            case: "describing a job id no submission started",
            operation: "DescribeTextTranslationJob",
            body: json!({"JobId": UNKNOWN_JOB_ID}),
        },
        Row {
            case: "stopping a job id no submission started",
            operation: "StopTextTranslationJob",
            body: json!({"JobId": UNKNOWN_JOB_ID}),
        },
        Row {
            case: "submitting a job with a terminology no import stored",
            operation: "StartTextTranslationJob",
            body: start_job_body(&["no-such-terms"], &[]),
        },
        Row {
            case: "submitting a job with a parallel data resource this account does not hold",
            operation: "StartTextTranslationJob",
            body: start_job_body(&[], &["no-such-parallel-data"]),
        },
    ]
}

/// Every operation of this service that reports a resource absent answers the
/// modelled `ResourceNotFoundException` at the status Amazon Translate publishes
/// for that class, whichever resource the request addressed and however it
/// addressed it.
#[tokio::test]
async fn a_resource_that_is_not_there_is_refused_as_a_client_error() {
    let router = router();
    let (deleted_arn, superseded_arn) = stored_resources(&router).await;

    for row in rows(&deleted_arn, &superseded_arn) {
        let (status, answered) = call(&router, row.operation, &row.body).await;
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {:?}: {} expected HTTP {NOT_FOUND_STATUS}, got {status} {answered}",
            row.case, row.operation
        );
        assert_eq!(
            error_code(&answered),
            NOT_FOUND_CODE,
            "case {:?}: {} expected {NOT_FOUND_CODE}, got {answered}",
            row.case,
            row.operation
        );
        assert!(
            !message(&answered).is_empty(),
            "case {:?}: {} must say what was not found, got {answered}",
            row.case,
            row.operation
        );
    }
}

/// The control for the table above: the same operations, addressing the resource
/// that IS there, answer a success — so a row's refusal is about the resource it
/// named and not about the shape of its request.
#[tokio::test]
async fn the_same_calls_against_a_resource_that_is_there_succeed() {
    let router = router();
    stored_resources(&router).await;
    let present_arn = expected_arn(PRESENT, EN_FR_CSV);

    let served: [(&str, Value); 6] = [
        ("GetTerminology", json!({"Name": PRESENT})),
        ("TranslateText", translate_text_body(&[PRESENT])),
        ("TranslateDocument", translate_document_body(&[PRESENT])),
        (
            "TagResource",
            json!({"ResourceArn": present_arn, "Tags": [{"Key": "env", "Value": "prod"}]}),
        ),
        ("ListTagsForResource", json!({"ResourceArn": present_arn})),
        (
            "UntagResource",
            json!({"ResourceArn": present_arn, "TagKeys": ["env"]}),
        ),
    ];
    for (operation, body) in served {
        let (status, answered) = call(&router, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: the resource is there, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "<no __type>",
            "case {operation:?}: a served call names no error, got {answered}"
        );
    }

    // And the delete, last: it is the call that makes the resource absent, so
    // the refusal the table asserts for it follows this success.
    let (status, answered) = call(&router, "DeleteTerminology", &json!({"Name": PRESENT})).await;
    assert_eq!(
        status, 200,
        "case \"DeleteTerminology\": the resource is there, got {status} {answered}"
    );
}

/// The sentence a batch-job operation states for a job id this account holds no
/// job under, in full: it has no variable part at all.
const JOB_NOT_FOUND_MESSAGE: &str = "Request failed due to job not found.";

/// The operations whose whole request is a job id, and which therefore are the
/// ones that report a job absent (`smithy_query query=operation
/// operation=DescribeTextTranslationJob,StopTextTranslationJob`).
/// `StartTextTranslationJob` resolves no job by id — it mints one — so it is
/// not among them.
const JOB_ID_OPERATIONS: [&str; 2] = ["DescribeTextTranslationJob", "StopTextTranslationJob"];

/// The ids reported absent below, as `(case, job id)`.
///
/// Each satisfies `JobId`'s modelled length and pattern (`length 1..32`,
/// `^([\p{L}\p{Z}\p{N}_.:/=+\-%@]*)$`, `smithy_query query=shape shape=JobId`),
/// so every row reaches the handler's job lookup rather than the request walk,
/// and the rows differ along the forms that pattern and those bounds admit.
fn unknown_job_ids() -> [(&'static str, &'static str); 6] {
    [
        (
            "an id of the length a submission mints",
            UNKNOWN_JOB_ID,
        ),
        ("an id at the modelled minimum length", "j"),
        (
            "an id at the modelled maximum length, all letters",
            "abcdefghijklmnopqrstuvwxyzabcdef",
        ),
        (
            "an id of the punctuation the modelled pattern admits",
            "job_.:/=+-%@1",
        ),
        (
            "an id of letters outside ASCII and a separator",
            "trabajo señal",
        ),
        (
            "an id of digits alone",
            "00000000000000000000000000000001",
        ),
    ]
}

/// A batch submission under `token`, with everything a well-formed job needs.
fn submission(token: &str) -> Value {
    let mut body = start_job_body(&[], &[]);
    body["ClientToken"] = json!(token);
    body
}

/// The job id one submission minted.
async fn submitted_job_id(router: &MultiServiceRouter, token: &str) -> String {
    let (status, body) = call(router, "StartTextTranslationJob", &submission(token)).await;
    assert_eq!(
        status, 200,
        "setup {token:?}: StartTextTranslationJob answered {status} {body}"
    );
    body["JobId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {token:?}: no JobId in {body}"))
        .to_string()
}

/// Both operations that resolve a batch translation job by id report an id this
/// account holds no job under with the ONE job-not-found refusal — the
/// not-found status, the modelled shape, and the fixed job-not-found sentence —
/// whatever form that id takes.
#[tokio::test]
async fn an_unknown_job_id_is_reported_as_a_job_that_was_not_found() {
    let router = router();
    // A job that IS there, so every row below is refused because of the id it
    // named rather than because the store holds no job at all.
    let present = submitted_job_id(&router, "token-job-not-found").await;

    for (case, job_id) in unknown_job_ids() {
        assert_ne!(
            job_id, present,
            "case {case:?}: the row's id has to be one no submission minted"
        );
        for operation in JOB_ID_OPERATIONS {
            let (status, answered) = call(&router, operation, &json!({"JobId": job_id})).await;
            assert_eq!(
                status, NOT_FOUND_STATUS,
                "case {case:?}: {operation} expected HTTP {NOT_FOUND_STATUS}, got {status} \
                 {answered}"
            );
            assert_eq!(
                error_code(&answered),
                NOT_FOUND_CODE,
                "case {case:?}: {operation} expected {NOT_FOUND_CODE}, got {answered}"
            );
            assert_eq!(
                message(&answered),
                JOB_NOT_FOUND_MESSAGE,
                "case {case:?}: {operation} expected {JOB_NOT_FOUND_MESSAGE:?}, got {answered}"
            );
        }
    }
}

/// The control for the table above: the same two operations, naming the job a
/// submission really minted, answer that job — so a row's refusal is about the
/// id it named and not about the shape of its request.
#[tokio::test]
async fn the_same_calls_naming_a_job_that_was_submitted_answer_it() {
    let router = router();
    let job_id = submitted_job_id(&router, "token-job-is-there").await;

    for operation in JOB_ID_OPERATIONS {
        let (status, answered) = call(&router, operation, &json!({"JobId": job_id})).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: the job is there, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "<no __type>",
            "case {operation:?}: a served call names no error, got {answered}"
        );
        assert_ne!(
            message(&answered),
            JOB_NOT_FOUND_MESSAGE,
            "case {operation:?}: a served call states no refusal, got {answered}"
        );
    }
}
