// SPDX-License-Identifier: CC0-1.0

//! What becomes of an object event whose destination cannot take it AT THE INSTANT of the write
//! — the window a client falls into whenever it wires a bucket to a function and writes into it
//! before that function can serve an invocation.
//!
//! The rule under test: **an object event Amazon S3 publishes is delivered AT LEAST ONCE, so a
//! delivery the destination's own service refuses for a condition that CAN PASS — it reports that
//! it is throttling the publication, that it is not in a state to take it yet, or that it faulted
//! serving it — is attempted again until the destination takes the event or the publication's own
//! window is spent; only a refusal that is the destination's FINAL answer (it does not exist) ends
//! the publication. Either way the write that produced the event is answered exactly as it would
//! have been, because the delivery is made after that write was accepted.**
//!
//! Lambda names both halves of that. A function whose reserved concurrency admits nothing has
//! every invocation refused with the modelled throttle — "Lambda returns `TooManyRequestsException`
//! if running the function would cause you to exceed a concurrency limit" (`Invoke`) — and for the
//! asynchronous invocation an event notification makes, "if your function is throttled, Lambda
//! returns the event to the queue and attempts to run the function again". A function Lambda is
//! still provisioning answers the same way: "when you create a function, Lambda provisions an
//! instance of the function and its supporting resources […] during this time, you can't invoke or
//! modify the function" (`CreateFunction`). A one-shot delivery loses the event to either for
//! good: the configuration is live, every API call the client made answered success, and the
//! function is never invoked — a working configuration indistinguishable from a broken one.
//!
//! Four crates have to agree for the rule to hold (CloudFormation provisions the function and its
//! execution role, Lambda holds the reservation that refuses the invocation and then runs the
//! handler, IAM answers for the role the handler's own write is signed as, and Amazon S3 evaluates
//! the configuration, makes the publication and takes the handler's write), so the suite lives
//! here: a crate-local suite can only ever see one side of it. Its siblings reach the destination
//! that was READY all along — [`crate::s3_notification_invokes_its_lambda_target`] pins what a
//! key-name `Filter` admits, and
//! [`crate::s3_notification_granted_function_writes_back_into_the_bucket`] pins the grant leg — and
//! what only this suite reaches is the publication whose destination REFUSED it and then became
//! able to take it.
//!
//! The rows differ along what the rule generalises over: whether the destination could take the
//! publication when the object was written, the shape of the written key, and whether the
//! configuration restricts the keys it covers. Each row asserts the write was answered and what
//! the handler left behind — a marker carrying the key the RECORD named — so a row cannot pass on a
//! delivery that never carried the record.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]): the assembled router's
//! dispatch future carries the whole emulator's handler state inline, so a test holding several of
//! them across await points lays that state out on the test thread's stack and overflows it as the
//! emulator grows.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};
use crate::s3_notification_invokes_its_lambda_target::{
    authorization, build_router, create_bucket, query, s3, Answer,
};

/// The Region every request here is signed for, and so the Region the bucket, the stack and the
/// function belong to.
const REGION: &str = "us-east-1";

/// The XML namespace a client's Amazon S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// Where Lambda binds the function collection and a function's resource policy.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on a
/// function destination ("for Amazon Web Services services, the principal is a domain-style
/// identifier that the service defines, such as s3.amazonaws.com", `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class every configuration below names: "you can enable notifications for every event
/// of a type by using the wildcard", so this is every object-created event of the bucket.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The `StackStatus` the one stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function serves invocations in, and the one it is in while Lambda is still
/// provisioning it — "Pending: Lambda is creating or configuring the function"
/// (`FunctionConfiguration$State`), which is the window this suite writes its objects in.
const FUNCTION_ACTIVE: &str = "Active";
const FUNCTION_PENDING: &str = "Pending";

/// The prefix the handler keys every marker it writes under. The handler SKIPS a record whose key
/// already carries it, which closes the chain its own write into the same bucket would otherwise
/// start.
const MARKER_PREFIX: &str = "processed/";

/// How long this suite waits for work the CLOCK owes — a stack's resources coming up, and the
/// asynchronous delivery of an object event — and how often it looks while waiting. Every wait is
/// on a CONDITION; nothing here asserts how long one took.
const WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long the row that must see NOTHING keeps looking before reading the absence as an absence:
/// well past the span a delivered row takes to leave its marker, and read only after such a row
/// has already left one, so the pipeline is known live by then.
const QUIET_WINDOW: Duration = Duration::from_secs(8);

/// The `Timeout` every function below is created with. Nothing here tests a function's own
/// timeout, so it is given generous room: a run shares this process with the rest of this test
/// target, and a handler that imports the SDK on a loaded host must not be stopped by a limit this
/// suite is not about.
const FUNCTION_TIMEOUT: u64 = 120;

/// The runtime every function below is created with: a managed Python runtime, whose bundled AWS
/// SDK is what the handler's own write is made with.
const RUNTIME: &str = "python3.12";

/// The handler attribute inside the module a stack's inline source is archived as.
const HANDLER_NAME: &str = "index.handler";

/// The handler every row deploys: for each record it is handed it decodes the key the record
/// carried — "the object key name value is URL encoded" (Amazon S3 User Guide, *Event message
/// structure*) — and writes one marker object carrying that key, through the AWS SDK its runtime
/// supplies, so the write is a real Amazon S3 call made with the function's own execution-role
/// credentials. A record whose key is already a marker is skipped, so the bucket ends up holding
/// exactly one marker per written object.
const MARKING_HANDLER: &str = r#"
import os
import urllib.parse

import boto3

S3 = boto3.client("s3")
PREFIX = os.environ["MARKER_PREFIX"]


def handler(event, context):
    written = []
    for record in event["Records"]:
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        if key.startswith(PREFIX):
            continue
        name = key.split("/")[-1]
        S3.put_object(
            Bucket=os.environ["MARKER_BUCKET"],
            Key=PREFIX + name,
            Body=key.encode("utf-8"),
        )
        written.append(key)
    return {"written": written}
"#;

/// One Lambda `restJson1` call.
///
/// Lambda is the only surface this suite reaches that the shared harness does not: the router, the
/// credential-scope `Authorization`, the `Answer` they answer with, the awsQuery call and the
/// Amazon S3 request are the sibling suite's ([`crate::s3_notification_invokes_its_lambda_target`]),
/// so there is one of each for both.
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
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// The body of `answer` as JSON, or `Null` when there is none to read.
fn json_of(answer: &Answer) -> Value {
    serde_json::from_str(&answer.body).unwrap_or(Value::Null)
}

/// How many looks fit in `window` at [`LOOK_INTERVAL`] apart.
fn looks(window: Duration) -> u128 {
    window.as_millis() / LOOK_INTERVAL.as_millis()
}

/// The stack that brings up one function running [`MARKING_HANDLER`] under its own execution role,
/// writing its markers into `markers`.
///
/// The handler is declared as the stack's INLINE source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to exist
/// at all.
fn handler_stack(function: &str, markers: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // The handler writes its marker through Amazon S3 with temporary credentials
                    // for this role, so the role has to grant that write.
                    "Policies": [{
                        "PolicyName": "mark-the-event",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["s3:GetObject", "s3:PutObject", "logs:*"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Handler": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER_NAME,
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {
                        "MARKER_BUCKET": markers,
                        "MARKER_PREFIX": MARKER_PREFIX,
                    }},
                    "Code": {"ZipFile": MARKING_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy `stack` and answer the ARN of the EXECUTION ROLE it declares, once the stack is
/// complete and Lambda reports the function it declares ready to be invoked.
///
/// The function the stack brings up is what [`package_of`] reads the deployment package off, and
/// its role is the one every function this suite then creates directly runs as — so one stack
/// supplies both, and no row writes an archive or a policy of its own.
async fn deployed_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    function: &str,
    markers: &str,
) -> String {
    let template = handler_stack(function, markers);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
            // The stack declares an execution role, which CloudFormation creates only when the
            // caller acknowledges it ("Requires capabilities : [CAPABILITY_IAM]").
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must succeed, got {status} with {answered}"
    );
    let mut described = String::new();
    for _ in 0..looks(WAIT) {
        described = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await
        .1;
        if elements(&described, "StackStatus")
            .first()
            .is_some_and(|status| status == CREATE_COMPLETE)
        {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        elements(&described, "StackStatus").first(),
        Some(&CREATE_COMPLETE.to_string()),
        "case {case:?}: the stack {stack:?} must reach {CREATE_COMPLETE}, last saw {described}"
    );
    let mut reported = Value::Null;
    for _ in 0..looks(WAIT) {
        reported = json_of(
            &lambda(
                router,
                "GET",
                &format!("{FUNCTIONS_URI}/{function}/configuration"),
                None,
            )
            .await,
        );
        if reported["State"].as_str() == Some(FUNCTION_ACTIVE) {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    reported["Role"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the deployed function must report its execution role, got \
                 {reported}"
            )
        })
        .to_string()
}

/// The deployment package `function` holds, base64-encoded the way `FunctionCode$ZipFile` carries
/// one.
///
/// It is the archive CLOUDFORMATION built from the stack's inline source, read back from where
/// `GetFunction` announces it — "a presigned URL that you can use to download the deployment
/// package" (`FunctionCodeLocation$Location`), an address this emulator answers on. So this suite
/// writes no archive of its own, and the functions it then creates DIRECTLY run the very handler
/// the stack deployed.
async fn package_of(router: &MultiServiceRouter, case: &str, function: &str) -> String {
    let read = lambda(router, "GET", &format!("{FUNCTIONS_URI}/{function}"), None).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetFunction({function:?}) must succeed, got {} with {}",
        read.status, read.body
    );
    let reported = json_of(&read);
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
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    assert_eq!(
        status, 200,
        "case {case:?}: the announced location must serve the package, got {status} with {}",
        String::from_utf8_lossy(response.body())
    );
    ::aws_smithy_types::base64::encode(response.body().as_ref())
}

/// Create `function` directly from `package`, as `role_arn`, writing its markers into `markers`;
/// answers its ARN.
///
/// NOTHING is waited for: a function Lambda has just been asked for is one it is still
/// provisioning, which is the state the rows of this suite write their object in.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    role_arn: &str,
    markers: &str,
    package: &str,
) -> String {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(&json!({
            "FunctionName": function,
            "Runtime": RUNTIME,
            "Handler": HANDLER_NAME,
            "Role": role_arn,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": {"Variables": {
                "MARKER_BUCKET": markers,
                "MARKER_PREFIX": MARKER_PREFIX,
            }},
            "Code": {"ZipFile": package},
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateFunction({function:?}) must succeed, got {} with {}",
        created.status, created.body
    );
    let reported = json_of(&created);
    // The window this suite is about, as the creating call itself reports it: the function is not
    // yet in a state Lambda invokes.
    assert_eq!(
        reported["State"].as_str(),
        Some(FUNCTION_PENDING),
        "case {case:?}: a function Lambda has just been asked for is one it is still \
         provisioning, got {reported}"
    );
    reported["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateFunction must answer an ARN, got {reported}")
        })
        .to_string()
}

/// Look until Lambda reports `function` ready to be invoked.
async fn awaited_active(router: &MultiServiceRouter, case: &str, function: &str) {
    let mut reported = Value::Null;
    for _ in 0..looks(WAIT) {
        reported = json_of(
            &lambda(
                router,
                "GET",
                &format!("{FUNCTIONS_URI}/{function}/configuration"),
                None,
            )
            .await,
        );
        if reported["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {reported}");
}

/// Let the Amazon S3 service principal invoke `function` for `bucket`, which is what
/// `PutBucketNotificationConfiguration` checks before it records a Lambda destination.
async fn given_invoke_grant(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    bucket: &str,
    statement: &str,
) {
    let granted = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        Some(&json!({
            "StatementId": statement,
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": format!("arn:aws:s3:::{bucket}"),
        })),
    )
    .await;
    assert_eq!(
        granted.status, 201,
        "case {case:?}: AddPermission({function:?}) must succeed, got {} with {}",
        granted.status, granted.body
    );
}

/// `PUT /<bucket>?notification` with one Lambda configuration naming `function_arn` for
/// [`OBJECT_CREATED`], filtered by a key-name `prefix` rule when one is given.
///
/// The elements are the ones this service's own model binds the Lambda list and its ARN member to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, and
/// `LambdaFunctionConfiguration$LambdaFunctionArn` as `CloudFunction`).
async fn given_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    function_arn: &str,
    prefix: Option<&str>,
) {
    let filter = prefix.map_or_else(String::new, |value| {
        format!(
            "<Filter><S3Key><FilterRule><Name>prefix</Name><Value>{value}</Value>\
             </FilterRule></S3Key></Filter>"
        )
    });
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>marker</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{OBJECT_CREATED}</Event>{filter}\
         </CloudFunctionConfiguration></NotificationConfiguration>"
    );
    let answer = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({bucket:?}) must succeed, got {} \
         with {}",
        answer.status, answer.body
    );
}

/// Write `key` into `bucket` and assert the write itself was answered: the delivery is made after
/// the write was accepted, so nothing about the destination changes this answer.
async fn written(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str, body: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must be answered whatever the destination \
         is doing, got {} with {}",
        answer.status, answer.body
    );
}

/// The marker keys `bucket` holds, read through `ListObjectsV2`.
async fn marker_keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let answer = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={}", form_encode(MARKER_PREFIX)),
        "",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: ListObjectsV2({bucket:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
    elements(&answer.body, "Key")
}

/// Look until `bucket` holds a marker, or until the row's own window is spent, and answer the
/// marker keys it holds. The wait is on the CONDITION; nothing here asserts how long it took.
async fn awaited_markers(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    expected: bool,
) -> Vec<String> {
    let window = if expected { WAIT } else { QUIET_WINDOW };
    let mut keys = Vec::new();
    for _ in 0..looks(window) {
        keys = marker_keys(router, case, bucket).await;
        if expected && !keys.is_empty() {
            return keys;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    keys
}

/// Remove every marker `bucket` holds, so each row starts from an empty listing of the shared
/// marker bucket.
async fn drained(router: &MultiServiceRouter, case: &str, bucket: &str, keys: &[String]) {
    for key in keys {
        let removed = s3(router, "DELETE", &format!("/{bucket}/{key}"), "").await;
        assert_eq!(
            removed.status, 204,
            "case {case:?}: deleting the row's marker {key:?} must succeed, got {} with {}",
            removed.status, removed.body
        );
    }
}

/// One row: what was written, what the configuration covering it said, and whether the destination
/// could take the publication when the write happened.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The key the row writes.
    key: &'static str,
    /// The key-name `prefix` rule the configuration carries, if any.
    prefix: Option<&'static str>,
    /// Whether the row waits for its function to report itself ready before writing.
    ready_first: bool,
}

/// The rows, differing along what the rule generalises over: whether the destination could take the
/// publication when the object was written, the shape of the written key, and whether the
/// configuration restricts the keys it covers.
const ROWS: &[Row] = &[
    Row {
        case: "written while the destination is still being provisioned",
        key: "incoming/sample.txt",
        prefix: None,
        ready_first: false,
    },
    Row {
        case: "a key several prefixes deep, written inside that window",
        key: "incoming/2026/01/quarterly-report.v2.csv",
        prefix: None,
        ready_first: false,
    },
    Row {
        case: "a flat key the configuration's prefix rule admits, inside that window",
        key: "raw-sample",
        prefix: Some("raw-"),
        ready_first: false,
    },
    Row {
        case: "written once the destination reports itself ready",
        key: "incoming/after-the-window.txt",
        prefix: None,
        ready_first: true,
    },
];

/// What every row writes as the object's body. Its content is immaterial: what each row observes is
/// the marker the handler derived from the RECORD.
const OBJECT_BODY: &str = "the object the event named";

/// Every row: the write is answered, and the event reaches the function and leaves its marker —
/// whether or not the destination could take the publication when the object was written.
///
/// Driven on a sized thread; see [`crate::s3_notification_invokes_its_lambda_target::driven`].
#[test]
fn an_object_event_outlives_a_destination_that_cannot_take_it_yet() {
    crate::s3_notification_invokes_its_lambda_target::driven(
        "s3-notification-delivery-outlives-an-unready-destination",
        || Box::pin(an_object_event_outlives_a_destination_that_cannot_take_it_yet_body()),
    );
}

async fn an_object_event_outlives_a_destination_that_cannot_take_it_yet_body() {
    let router = build_router();
    let setup = "the shared wiring";
    let markers = "unready-destination-markers";
    create_bucket(&router, setup, markers).await;
    // One stack brings up the handler and the execution role once; every row's own function is
    // then created DIRECTLY from the package that stack archived, so each row has a function
    // Lambda is still provisioning at the instant its object is written.
    let deployed = "unready-destination-deployed";
    let role_arn = deployed_function(
        &router,
        setup,
        "unready-destination-stack",
        deployed,
        markers,
    )
    .await;
    let package = package_of(&router, setup, deployed).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let bucket = format!("unready-source-{index}");
        let function = format!("unready-marker-{index}");
        create_bucket(&router, case, &bucket).await;
        let function_arn =
            given_function(&router, case, &function, &role_arn, markers, &package).await;
        if row.ready_first {
            awaited_active(&router, case, &function).await;
        }
        given_invoke_grant(&router, case, &function, &bucket, "s3-notification").await;
        given_notification(&router, case, &bucket, &function_arn, row.prefix).await;
        written(&router, case, &bucket, row.key, OBJECT_BODY).await;

        let keys = awaited_markers(&router, case, markers, true).await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: expected exactly one marker for the write of {:?}, got {keys:?}",
            row.key
        );
        let expected = format!("{MARKER_PREFIX}{}", row.key.rsplit('/').next().unwrap_or(""));
        assert_eq!(
            keys[0], expected,
            "case {case:?}: expected the marker the handler derives from the record, got {:?}",
            keys[0]
        );
        let read = s3(&router, "GET", &format!("/{markers}/{expected}"), "").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetObject({markers:?}, {expected:?}) must succeed, got {} with {}",
            read.status, read.body
        );
        assert_eq!(
            read.body, row.key,
            "case {case:?}: the marker must carry the key the record named, got {:?}",
            read.body
        );
        drained(&router, case, markers, &keys).await;
    }
}

/// The other half of the rule, as far as a CLIENT can see it: a destination that no longer exists
/// is published nothing, and the write that produced the event is answered exactly as in every row
/// above. What this pins is precisely that pair — the write is served and no marker ever appears —
/// and not the moment the publication gave up: a destination that does not exist leaves no marker
/// whether its refusal ended the publication at once or was re-attempted until the budget ran out,
/// so no assertion here could tell those apart. What DOES pin the classification is the status set
/// itself, in the owning crate's own test
/// (`notifications::tests::a_publication_outlives_only_the_refusals_that_can_pass`).
///
/// Read AFTER a delivered write, so the absence is the delivery Amazon S3 did not make and never a
/// pipeline that was never live.
///
/// Driven on a sized thread; see [`crate::s3_notification_invokes_its_lambda_target::driven`].
#[test]
fn an_object_event_whose_destination_is_gone_is_not_waited_for() {
    crate::s3_notification_invokes_its_lambda_target::driven(
        "s3-notification-delivery-gives-up-on-a-destination-that-is-gone",
        || Box::pin(an_object_event_whose_destination_is_gone_is_not_waited_for_body()),
    );
}

async fn an_object_event_whose_destination_is_gone_is_not_waited_for_body() {
    let case = "a destination deleted after the configuration was recorded";
    let router = build_router();
    let markers = "gone-destination-markers";
    create_bucket(&router, case, markers).await;
    let bucket = "gone-destination-source";
    let deployed = "gone-destination-deployed";
    let function = "gone-destination-marker";
    create_bucket(&router, case, bucket).await;
    let role_arn =
        deployed_function(&router, case, "gone-destination-stack", deployed, markers).await;
    let package = package_of(&router, case, deployed).await;
    let function_arn =
        given_function(&router, case, function, &role_arn, markers, &package).await;
    awaited_active(&router, case, function).await;
    given_invoke_grant(&router, case, function, bucket, "s3-notification").await;
    given_notification(&router, case, bucket, &function_arn, None).await;

    // The pipeline is live first, so the absence below is read against a configuration that has
    // already delivered an event.
    written(&router, case, bucket, "incoming/live.txt", OBJECT_BODY).await;
    let keys = awaited_markers(&router, case, markers, true).await;
    assert_eq!(
        keys.len(),
        1,
        "case {case:?}: the configuration must deliver before its destination is removed, got \
         {keys:?}"
    );
    drained(&router, case, markers, &keys).await;

    let removed = lambda(
        &router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{function}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status, 204,
        "case {case:?}: DeleteFunction({function:?}) must succeed, got {} with {}",
        removed.status, removed.body
    );

    written(
        &router,
        case,
        bucket,
        "incoming/after-removal.txt",
        OBJECT_BODY,
    )
    .await;
    let keys = awaited_markers(&router, case, markers, false).await;
    assert!(
        keys.is_empty(),
        "case {case:?}: a destination that does not exist is no destination to wait for, got the \
         marker(s) {keys:?}"
    );
}
