// SPDX-License-Identifier: CC0-1.0

//! What a stack whose `AWS::Lambda::Function` names a deployment package Amazon S3 will not
//! hand over does, and what it leaves behind.
//!
//! RULE under test: **a declared resource is created by the service that OWNS it, so a
//! create that service REFUSES fails that resource with the refusal it answered — one
//! `*_FAILED` event whose `ResourceStatusReason` quotes the owning service's own exception —
//! rolls the stack back to a terminal rollback status, and leaves the resource absent from
//! that service. For an `AWS::Lambda::Function` whose `Code` names an Amazon S3 location,
//! that means the object is FETCHED before the function exists: a location Amazon S3 will not
//! serve, or an object that is no deployment package, is Lambda's own
//! `InvalidParameterValueException` relaying what Amazon S3 answered, and no function is
//! created.**
//!
//! Why it matters: a typo'd or not-yet-uploaded code key deploys green while AWS fails the
//! stack, and the function it leaves behind has no deployment package at all — so every later
//! read and every invocation is made against a resource AWS would never have created.
//!
//! Four crates have to agree for the rule to hold (CloudFormation makes the create call and
//! rolls the stack back, Lambda follows the code reference and refuses, Amazon S3 answers for
//! the object, and IAM answers for the execution role), so the suite lives here rather than in
//! any one crate.
//!
//! The rows differ along what the rule generalises over: WHAT makes the declared location
//! unreadable (a key the bucket holds nothing under, a key under a prefix nothing was staged
//! under at all, a bucket Amazon S3 holds no record of, and an object that is handed over but
//! is no .zip archive), and — in the test beside the table — the positive control: the same
//! template whose key names an object that IS there reaches `CREATE_COMPLETE` and the function
//! resolves through Lambda's own `GetFunction`.
//!
//! Every expected value is computed here from what the row declares and from the wording the
//! answering services publish for these refusals — never read back out of the answer being
//! checked.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use bytes::Bytes;

// The awsQuery XML readers this crate writes ONCE (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, members_of};
// The emulator, the CloudFormation wire and the stack waits the sibling suite about a
// completed stack's outputs already owns, so neither suite can drift about what settling is.
use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, create_stack, dispatch, reported_failures, settled_status,
    Answered, ACCOUNT, CREATE_COMPLETE, REGION,
};

/// The status a create whose resource failed rolls the stack back to.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The event a resource whose create was refused reports.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical ids every template below declares.
const FUNCTION: &str = "LambdaFunction";
const ROLE: &str = "ExecutionRole";

/// The SDK's own name for AWS Lambda, as the exception its SDK raises states it.
const LAMBDA_SERVICE: &str = "Lambda";

/// The HTTP status AWS Lambda refuses a `CreateFunction` whose deployment package it will not
/// store with: `InvalidParameterValueException`'s own `httpError`
/// (`smithy_query(query="operation",operation="CreateFunction")`).
const REFUSED_PARAMETER_STATUS: u16 = 400;

/// The `HandlerErrorCode` class such a refusal falls in, a value of the CloudFormation model's
/// own `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The error code AWS Lambda answers a read of a function it does not hold with, and its
/// status: `ResourceNotFoundException`'s own `httpError`
/// (`smithy_query(query="operation",operation="GetFunction")`).
const FUNCTION_ABSENT: (u16, &str) = (404, "ResourceNotFoundException");

/// A .zip archive holding no entries: the 22-byte end-of-central-directory record — its
/// signature and then the five counts, the directory's size and offset, and a zero comment
/// length, all zero — which is a complete archive a reader opens and finds empty. The positive
/// control's object, so the control differs from the rows ONLY in whether Amazon S3 hands a
/// package over.
const EMPTY_ARCHIVE: &[u8] = b"PK\x05\x06\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00";

/// Bytes that are no .zip archive at all, for the row whose object Amazon S3 DOES hand over.
const NOT_AN_ARCHIVE: &[u8] = b"this object is not a deployment package";

/// The sentences the ANSWERING services word for the three unreadable locations below,
/// spelled out here rather than taken from the emulator's own constants so the suite pins
/// AWS's wording and not the implementation's.
///
/// Sources: Amazon S3's `NoSuchKey` ("The specified key does not exist.") and `NoSuchBucket`
/// ("The specified bucket does not exist") error responses, each the `Message` of the
/// `GetObject` refusal this read is made with; and the sentence AWS Lambda refuses a package
/// it cannot unzip with, as the issue report records it.
const NO_SUCH_KEY_SENTENCE: &str = "The specified key does not exist.";
const NO_SUCH_BUCKET_SENTENCE: &str = "The specified bucket does not exist";
const COULD_NOT_UNZIP_SENTENCE: &str =
    "Could not unzip uploaded file. Please check your file, then try to upload again.";

/// Call one Amazon S3 operation over its REST surface, path-style — the addressing the
/// emulated service routes on.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &'static [u8],
) -> Answered {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::from_static(body),
    )
    .await
}

/// `GetFunction` for `name`, as AWS Lambda's model binds it: `GET
/// /2015-03-31/functions/{FunctionName}`.
async fn get_function(router: &MultiServiceRouter, name: &str) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("lambda.{REGION}.amazonaws.com"),
        &format!("/2015-03-31/functions/{name}"),
        &[("authorization", &authorization("lambda"))],
        Bytes::new(),
    )
    .await
}

/// The template every case deploys: the function's execution role and the function itself,
/// whose `Code` names the Amazon S3 location the deployment's parameters state — so every case
/// deploys the SAME template and differs only in the location it passes.
fn template(function: &str) -> String {
    json!({
        "Parameters": {
            "CodeBucket": { "Type": "String" },
            "CodeKey": { "Type": "String" },
        },
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                        }],
                    },
                },
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": [ROLE, "Arn"] },
                    "Code": {
                        "S3Bucket": { "Ref": "CodeBucket" },
                        "S3Key": { "Ref": "CodeKey" },
                    },
                },
            },
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": FUNCTION } },
        },
    })
    .to_string()
}

/// The exception a `ResourceStatusReason` quotes, and the `HandlerErrorCode` class it reports
/// beside it.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one quoted exception to the shape an AWS Lambda SDK raises — the service's own
/// sentence, then the SDK's diagnostic of the refused call (its name for the service, the HTTP
/// status, the id of the call and the attempts it made) — and answer the sentence.
fn asserted_sentence(case: &str, quoted: &str, status: u16) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    const ATTEMPTS: &str = ") (SDK Attempt Count: ";
    let (sentence, diagnostic) = quoted.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, after the \
             service's own sentence, got {quoted:?}"
        )
    });
    let (envelope, attempts) = diagnostic.split_once(ATTEMPTS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the attempt count this service's SDK words after the \
             envelope, got {quoted:?}"
        )
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, got \
         {envelope:?}"
    );
    assert_eq!(
        fields[0], LAMBDA_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {status}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    let request_id = fields[2]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[2]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
    );
    let counted = attempts
        .strip_suffix(')')
        .and_then(|counted| counted.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a whole attempt count, got {attempts:?}")
        });
    assert!(
        counted >= 1,
        "case {case:?}: expected at least the one attempt the SDK was asked for, got {counted}"
    );
    sentence.to_string()
}

/// Whether `value` has the 8-4-4-4-12 lowercase-hexadecimal form an AWS request id carries.
fn is_a_request_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .chars()
                .all(|held| held.is_ascii_hexdigit() && !held.is_ascii_uppercase())
        })
}

/// Every `ResourceStatusReason` the stack recorded for `logical_id` reaching `reported`, in
/// document order: the rule is ONE such event, so the count is what a case asserts.
async fn failure_reasons(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Vec<String> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    // The read itself has to have been served, so a case asserting that NO such event exists
    // cannot pass on a refused `DescribeStackEvents`.
    assert_eq!(
        answered.status, 200,
        "expected HTTP 200 from DescribeStackEvents({stack:?}), got {} with {body:?}",
        answered.status
    );
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let named = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id);
            let matching = elements(member, "ResourceStatus")
                .into_iter()
                .next()
                .is_some_and(|status| status == reported);
            (named && matching)
                .then(|| elements(member, "ResourceStatusReason").into_iter().next())
                .flatten()
        })
        .collect()
}

/// The sentence AWS Lambda refuses a package Amazon S3 would not hand over with: fixed text
/// around the code Amazon S3 classified the refusal under and the message it worded.
fn refused_by_s3(code: &str, message: &str) -> String {
    format!("Error occurred while GetObject. S3 Error Code: {code}. S3 Error Message: {message}")
}

/// What one row makes of the declared code location.
#[derive(Clone, Copy)]
enum Location {
    /// The bucket exists and one object is staged in it, and the declared key names another.
    KeyNothingIsUnder,
    /// The bucket exists and is empty, and the declared key names an object under a prefix.
    PrefixNothingIsUnder,
    /// No bucket of that name exists at all.
    BucketNothingIsUnder,
    /// The bucket exists and holds the declared key, but the object is no .zip archive.
    ObjectThatIsNoArchive,
}

/// One row: the location the declaration names, and the sentence Lambda must refuse it with.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    bucket: &'static str,
    /// The key the declaration names.
    key: &'static str,
    location: Location,
}

/// The key a row stages a real object under, when it stages one.
const STAGED_KEY: &str = "code/good.zip";

const ROWS: &[Row] = &[
    Row {
        // The recorded shape: the bucket and a real package are both there, and the
        // declaration names a key beside it.
        case: "a key the bucket holds nothing under",
        stack: "lambda-code-missing-key",
        function: "lambda-code-missing-key-fn",
        bucket: "lambda-code-location-bucket-a",
        key: "nonexistent/code.zip",
        location: Location::KeyNothingIsUnder,
    },
    Row {
        case: "a key under a prefix nothing was ever staged under",
        stack: "lambda-code-empty-bucket",
        function: "lambda-code-empty-bucket-fn",
        bucket: "lambda-code-location-bucket-b",
        key: "builds/2026/worker.zip",
        location: Location::PrefixNothingIsUnder,
    },
    Row {
        case: "a bucket Amazon S3 holds no record of",
        stack: "lambda-code-missing-bucket",
        function: "lambda-code-missing-bucket-fn",
        bucket: "lambda-code-location-bucket-c",
        key: STAGED_KEY,
        location: Location::BucketNothingIsUnder,
    },
    Row {
        case: "an object Amazon S3 hands over that is no deployment package",
        stack: "lambda-code-not-an-archive",
        function: "lambda-code-not-an-archive-fn",
        bucket: "lambda-code-location-bucket-d",
        key: "code/not-a-zip.bin",
        location: Location::ObjectThatIsNoArchive,
    },
];

/// Create `bucket`, asserting Amazon S3 took the create.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected Amazon S3 to create {bucket:?}, got {} with {:?}",
        created.status,
        created.text()
    );
}

/// Stage `body` at `bucket`/`key`, asserting Amazon S3 took the write.
async fn stage_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    body: &'static [u8],
) {
    let staged = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        staged.status,
        200,
        "case {case:?}: expected Amazon S3 to hold {key:?} in {bucket:?}, got {} with {:?}",
        staged.status,
        staged.text()
    );
}

/// What one row stages in Amazon S3 before the stack is deployed, and the sentence the refusal
/// must then carry.
async fn stage(router: &MultiServiceRouter, row: &Row) -> String {
    match row.location {
        Location::KeyNothingIsUnder => {
            create_bucket(router, row.case, row.bucket).await;
            stage_object(router, row.case, row.bucket, STAGED_KEY, EMPTY_ARCHIVE).await;
            // Amazon S3's own wording for a key it holds nothing under.
            refused_by_s3("NoSuchKey", NO_SUCH_KEY_SENTENCE)
        }
        Location::PrefixNothingIsUnder => {
            create_bucket(router, row.case, row.bucket).await;
            refused_by_s3("NoSuchKey", NO_SUCH_KEY_SENTENCE)
        }
        // Nothing is staged at all: the bucket itself is what Amazon S3 holds no record of.
        Location::BucketNothingIsUnder => {
            refused_by_s3("NoSuchBucket", NO_SUCH_BUCKET_SENTENCE)
        }
        Location::ObjectThatIsNoArchive => {
            create_bucket(router, row.case, row.bucket).await;
            stage_object(router, row.case, row.bucket, row.key, NOT_AN_ARCHIVE).await;
            // An object Amazon S3 hands over that is no archive is Lambda's own refusal, so the
            // sentence is that service's rather than a relay of Amazon S3's.
            COULD_NOT_UNZIP_SENTENCE.to_string()
        }
    }
}

/// The ARN AWS Lambda names a function of this account and Region by.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

#[tokio::test]
async fn a_function_whose_code_object_is_unreadable_fails_its_resource_and_rolls_the_stack_back() {
    // ONE emulator for the whole table: each row names its own bucket, stack and function, so
    // the rows cannot reach one another's state and the assembled router is built once.
    let router = crate::cloudformation_completed_stack_output_values::emulator();
    for row in ROWS {
        let case = row.case;
        let expected_sentence = stage(&router, row).await;

        create_stack(
            &router,
            case,
            row.stack,
            &template(row.function),
            &[("CodeBucket", row.bucket), ("CodeKey", row.key)],
        )
        .await;

        // 1. The stack does not report success: the resource its owning service refused rolls
        // the whole deployment back.
        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to roll back for a code location Amazon S3 will \
             not serve, got {settled:?}; the failures it reported: {:?}; last answer {described}",
            reported_failures(&router, row.stack).await
        );

        // 2. ONE failed event for the function, whose reason quotes AWS Lambda's own exception.
        let reasons = failure_reasons(&router, row.stack, FUNCTION, CREATE_FAILED).await;
        assert_eq!(
            reasons.len(),
            1,
            "case {case:?}: expected exactly one {CREATE_FAILED} reason for {FUNCTION}, got \
             {reasons:?}"
        );
        let (quoted, class) = quoted_and_class(case, &reasons[0]);
        let sentence = asserted_sentence(case, &quoted, REFUSED_PARAMETER_STATUS);
        assert_eq!(
            sentence, expected_sentence,
            "case {case:?}: expected the sentence the owning services word for this code \
             location, got {sentence:?} in {:?}",
            reasons[0]
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the class a refusal of the request falls in, got {class:?} \
             in {:?}",
            reasons[0]
        );

        // 3. And the function is NOT there: a resource whose create was refused exists in no
        // service, so the read every client makes of it answers that it is absent.
        let read = get_function(&router, row.function).await;
        assert_eq!(
            (read.status, error_code(&read)),
            (FUNCTION_ABSENT.0, FUNCTION_ABSENT.1.to_string()),
            "case {case:?}: expected AWS Lambda to hold no function {:?}, got {} with {:?}",
            row.function,
            read.status,
            read.text()
        );
        assert_eq!(
            refusal_message(&read).as_deref(),
            Some(format!("Function not found: {}", function_arn(row.function)).as_str()),
            "case {case:?}: expected the sentence AWS Lambda words for a function it does not \
             hold, naming the ARN the request resolved to, got {:?}",
            read.text()
        );
    }
}

/// The sentence a restJson1 refusal carries, under either spelling of the member an AWS error
/// document words it in (`Message`, `message`).
fn refusal_message(answered: &Answered) -> Option<String> {
    let document = answered.json();
    ["Message", "message"]
        .into_iter()
        .find_map(|member| document.get(member).and_then(Value::as_str))
        .map(str::to_string)
}

/// An error code as a client reads it off a restJson1 refusal: the shape name, with any Smithy
/// namespace prefix the envelope carries dropped.
fn error_code(answered: &Answered) -> String {
    answered
        .json()
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The CONTROL: the identical template, whose declared key names an object that IS staged,
/// completes and leaves the function resolvable through AWS Lambda's own read — so what the
/// rows pin is the code location and not the template.
#[tokio::test]
async fn the_same_template_whose_code_object_is_staged_completes_and_creates_the_function() {
    let case = "a key the bucket holds the staged package under";
    let router = crate::cloudformation_completed_stack_output_values::emulator();
    let stack = "lambda-code-staged-key";
    let function = "lambda-code-staged-key-fn";
    let bucket = "lambda-code-location-bucket-control";

    create_bucket(&router, case, bucket).await;
    stage_object(&router, case, bucket, STAGED_KEY, EMPTY_ARCHIVE).await;

    create_stack(
        &router,
        case,
        stack,
        &template(function),
        &[("CodeBucket", bucket), ("CodeKey", STAGED_KEY)],
    )
    .await;

    let (settled, described) = settled_status(&router, stack).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the stack to complete for a code object Amazon S3 serves, got \
         {settled:?}; the failures it reported: {:?}; last answer {described}",
        reported_failures(&router, stack).await
    );
    let reasons = failure_reasons(&router, stack, FUNCTION, CREATE_FAILED).await;
    assert!(
        reasons.is_empty(),
        "case {case:?}: expected no failed event for {FUNCTION}, got {reasons:?}"
    );

    let read = get_function(&router, function).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected AWS Lambda to hold the function the stack reported as created, \
         got {} with {:?}",
        read.status,
        read.text()
    );
    let configuration = read.json().get("Configuration").cloned().unwrap_or(Value::Null);
    assert_eq!(
        configuration.get("FunctionArn").and_then(Value::as_str),
        Some(function_arn(function).as_str()),
        "case {case:?}: expected the function to be held under the ARN of {function:?}, got \
         {configuration}"
    );
}
