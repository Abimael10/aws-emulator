// SPDX-License-Identifier: CC0-1.0

//! What an S3 Batch Operations job whose action is `LambdaInvoke` DOES, over the four
//! crates that hold the answer: CloudFormation deploys the estate, IAM holds the role the
//! job runs as, Lambda runs the function, and S3 Control drives the job over the objects
//! the manifest lists.
//!
//! RULE: a job whose `Operation` is `LambdaInvoke` INVOKES the named function once per
//! manifest entry, handing it the invocation document the job's
//! `InvocationSchemaVersion` declares (the invocation's id, the job's id and the object the
//! task acts on — the bucket by ARN under the default schema, by name and beside the job's
//! `UserArguments` under `2.0`), and folds the `resultCode` the function answered for each
//! task into `ProgressSummary`: a task the function answered `Succeeded` counts as
//! succeeded, every other verdict — another result code, a function that raised, an answer
//! carrying no verdict for the task — counts as failed, and the job reaches the terminal
//! `Complete` however many of its tasks failed, carrying no `FailureReasons`. Whatever the
//! function itself performed on the object is there to read through Amazon S3 afterwards.
//!
//! Sources: `JobOperation$LambdaInvoke` ("directs the specified job to invoke an Lambda
//! function on every object in the manifest"),
//! `LambdaInvokeOperation$InvocationSchemaVersion` ("version `1.0` is the default", `2.0`
//! "required … if you need to specify `UserArguments`", whose function code "uses bucket
//! name rather than bucket ARN"), `JobProgressSummary` and
//! `JobDescriptor$FailureReasons` ("if the specified job failed, this field contains
//! information describing the failure").
//!
//! Only a cross-service suite can pin it: the job is S3 Control's, the function and its
//! verdict are Lambda's, the role is IAM's, and the objects the handler touches are Amazon
//! S3's.
//!
//! The rows span what the rule generalises over: which verdict the handler answers
//! (`Succeeded` for every task, `PermanentFailure` for one of several, a handler that
//! raises instead of answering), how many entries the manifest carries, the shape of the
//! keys it lists (a plain key, a key under a prefix), and which invocation schema the job
//! names — which is what decides how the payload addresses each task's bucket.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The account the S3 Control calls name, in `x-amz-account-id`: the caller's own, which is
/// the only account S3 Control authorizes a request for.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The XML namespace S3 Control's model declares.
const XMLNS: &str = "http://awss3control.amazonaws.com/doc/2018-08-20/";

/// The bucket every object, manifest and job of this suite lives in.
const BUCKET: &str = "xsvc-batch-lambda-invoke";

/// The stack that deploys the estate, and the functions it declares.
const STACK: &str = "xsvc-batch-lambda-invoke-stack";
const TAGGING_FUNCTION: &str = "xsvc-batch-tagger";
const SELECTIVE_FUNCTION: &str = "xsvc-batch-selective";
const ARGUMENT_FUNCTION: &str = "xsvc-batch-by-name";
const RAISING_FUNCTION: &str = "xsvc-batch-raiser";

/// The tag the handlers of the first two rows put on every object whose task they carried
/// out.
const TAG_KEY: &str = "processed";
const TAG_VALUE: &str = "true";

/// The `UserArguments` the schema-`2.0` payload carries when a job names them. They are
/// pinned by the s3control crate's own unit test over the payload rather than here, because
/// this emulator's XML request codec reads no map member out of a request body yet — the
/// schema-`2.0` row below therefore varies only the member that addresses the bucket.
const BUCKET_BY_NAME_TAG_KEY: &str = "addressed-by-name";
const BUCKET_BY_NAME_TAG_VALUE: &str = "true";

/// The `Timeout` every function is created with: well past what one run needs on any host,
/// because nothing here tests a function's own timeout.
const FUNCTION_TIMEOUT: u64 = 120;

/// The terminal stack status a deployment of this estate reaches.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function serves invocations in.
const FUNCTION_ACTIVE: &str = "Active";

/// The terminal `JobStatus` a job whose tasks RAN reaches, however many of them failed, and
/// the one a job that could not be run at all reaches.
const JOB_COMPLETE: &str = "Complete";
const JOB_FAILED: &str = "Failed";

/// How long this suite looks for work the clock owes — a stack settling, a function being
/// provisioned, a submitted job running its tasks — and how often. The deadline bounds a
/// hang and is generous, so a loaded host delays a row rather than deciding it; nothing here
/// sleeps for a fixed span expecting work to be done.
const WAIT: Duration = Duration::from_secs(180);
const LOOK: Duration = Duration::from_millis(50);

/// The handler of the first row: it tags every object it is handed and answers `Succeeded`
/// for that task, addressing the bucket through the `s3BucketArn` the default schema carries
/// and decoding the key as AWS's own sample handler does.
const TAGGING_HANDLER: &str = r#"
import urllib.parse

import boto3


def handler(event, context):
    s3 = boto3.client("s3")
    results = []
    for task in event["tasks"]:
        bucket = task["s3BucketArn"].split(":")[-1]
        key = urllib.parse.unquote_plus(task["s3Key"])
        s3.put_object_tagging(
            Bucket=bucket,
            Key=key,
            Tagging={"TagSet": [{"Key": "processed", "Value": "true"}]},
        )
        results.append(
            {"taskId": task["taskId"], "resultCode": "Succeeded", "resultString": key}
        )
    return {
        "invocationSchemaVersion": event["invocationSchemaVersion"],
        "treatMissingKeysAs": "PermanentFailure",
        "invocationId": event["invocationId"],
        "results": results,
    }
"#;

/// The handler of the second row: it carries out only the keys it accepts and answers
/// `PermanentFailure` for the rest, which is the verdict a job accounts as a failed task.
const SELECTIVE_HANDLER: &str = r#"
import urllib.parse

import boto3


def handler(event, context):
    s3 = boto3.client("s3")
    results = []
    for task in event["tasks"]:
        bucket = task["s3BucketArn"].split(":")[-1]
        key = urllib.parse.unquote_plus(task["s3Key"])
        if "reject" in key:
            results.append(
                {
                    "taskId": task["taskId"],
                    "resultCode": "PermanentFailure",
                    "resultString": "this handler does not process " + key,
                }
            )
            continue
        s3.put_object_tagging(
            Bucket=bucket,
            Key=key,
            Tagging={"TagSet": [{"Key": "processed", "Value": "true"}]},
        )
        results.append({"taskId": task["taskId"], "resultCode": "Succeeded"})
    return {
        "invocationSchemaVersion": event["invocationSchemaVersion"],
        "treatMissingKeysAs": "PermanentFailure",
        "invocationId": event["invocationId"],
        "results": results,
    }
"#;

/// The handler of the schema-`2.0` row: it reads the bucket off `s3Bucket`, the member only
/// that schema's payload carries, so a payload addressing the bucket the other way would
/// leave it with nothing to call.
const ARGUMENT_HANDLER: &str = r#"
import urllib.parse

import boto3


def handler(event, context):
    s3 = boto3.client("s3")
    results = []
    for task in event["tasks"]:
        key = urllib.parse.unquote_plus(task["s3Key"])
        s3.put_object_tagging(
            Bucket=task["s3Bucket"],
            Key=key,
            Tagging={
                "TagSet": [{"Key": "addressed-by-name", "Value": "true"}]
            },
        )
        results.append({"taskId": task["taskId"], "resultCode": "Succeeded"})
    return {
        "invocationSchemaVersion": event["invocationSchemaVersion"],
        "invocationId": event["invocationId"],
        "results": results,
    }
"#;

/// The handler of the last row: it answers no verdict at all, because it raises.
const RAISING_HANDLER: &str = r#"
def handler(event, context):
    raise RuntimeError("this handler never carries a task out")
"#;

/// The emulator as it is ASSEMBLED, on one engine: the job's invocations and the handler's
/// own calls are dispatched back into this same router.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one call answered: its status, its body and the `ETag` header a write reports.
struct Answer {
    status: u16,
    body: String,
    etag: String,
}

/// One awsQuery call of `service`.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    answered(router.dispatch(request).await)
}

/// One path-style Amazon S3 REST call, as the emulator serves it.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/octet-stream")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    answered(router.dispatch(request).await)
}

/// One S3 Control call for this account.
async fn s3control(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    let host = format!("{ACCOUNT}.s3-control.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/xml")
        .header("x-amz-account-id", ACCOUNT)
        .header("authorization", authorization("s3"))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    answered(router.dispatch(request).await)
}

/// One Lambda `restJson1` read.
async fn lambda(router: &MultiServiceRouter, path: &str) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    answered(router.dispatch(request).await)
}

/// One dispatched response as this suite reads it.
fn answered(response: http::Response<Bytes>) -> Answer {
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).into_owned(),
        etag: parts
            .headers
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string(),
    }
}

/// The template this suite deploys: the bucket, the execution role every function runs as,
/// the role the jobs are carried out under — trusted by the batch-operations principal and
/// granted the invoking action — and the four handlers the rows name.
fn template() -> String {
    let function = |name: &str, source: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["HandlerRole", "Arn"]},
                "Timeout": FUNCTION_TIMEOUT,
                "Code": {"ZipFile": source},
            },
        })
    };
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Objects": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": BUCKET},
            },
            "HandlerRole": {
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
                    "Policies": [{
                        "PolicyName": "read-and-tag-the-objects-of-the-manifest",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "s3:GetObject",
                                    "s3:PutObjectTagging",
                                    "s3:GetObjectTagging",
                                    "logs:*",
                                ],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "BatchOperationsRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "batchoperations.s3.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "read-the-manifest-and-invoke-the-function",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "s3:GetObject",
                                    "s3:GetBucketLocation",
                                    "lambda:InvokeFunction",
                                ],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Tagger": function(TAGGING_FUNCTION, TAGGING_HANDLER),
            "Selective": function(SELECTIVE_FUNCTION, SELECTIVE_HANDLER),
            "ByName": function(ARGUMENT_FUNCTION, ARGUMENT_HANDLER),
            "Raiser": function(RAISING_FUNCTION, RAISING_HANDLER),
        },
        "Outputs": {
            "BatchRoleArn": {"Value": {"Fn::GetAtt": ["BatchOperationsRole", "Arn"]}},
        },
    })
    .to_string()
}

/// How many looks fit in [`WAIT`] at [`LOOK`] apart.
fn looks() -> usize {
    usize::try_from(WAIT.as_millis() / LOOK.as_millis().max(1)).unwrap_or(usize::MAX)
}

/// Deploy the estate and answer the ARN of the role the jobs run as, once every function it
/// declares is active.
async fn given_estate(router: &MultiServiceRouter) -> String {
    let body = template();
    let created = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // The template declares the two IAM roles the estate needs, which CloudFormation
            // serves only once the caller acknowledges it ("Requires capabilities :
            // [CAPABILITY_IAM]").
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the estate's stack must be accepted, got {} with {}",
        created.status, created.body
    );

    let mut described = String::new();
    for _ in 0..looks() {
        let answer = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        described = answer.body;
        if opt_element(&described, "StackStatus").as_deref() == Some(CREATE_COMPLETE) {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "setup: the estate must deploy, last described {described}"
    );
    let role_arn = {
        let keys = elements(&described, "OutputKey");
        let values = elements(&described, "OutputValue");
        keys.into_iter()
            .zip(values)
            .find(|(key, _)| key == "BatchRoleArn")
            .map(|(_, value)| value)
            .unwrap_or_else(|| panic!("setup: the stack must publish the role's ARN: {described}"))
    };

    for name in [
        TAGGING_FUNCTION,
        SELECTIVE_FUNCTION,
        ARGUMENT_FUNCTION,
        RAISING_FUNCTION,
    ] {
        let mut last = String::new();
        for _ in 0..looks() {
            let answer = lambda(
                router,
                &format!("/2015-03-31/functions/{name}/configuration"),
            )
            .await;
            last = answer.body;
            let state = serde_json::from_str::<Value>(&last)
                .ok()
                .and_then(|document| {
                    document
                        .get("State")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            if state.as_deref() == Some(FUNCTION_ACTIVE) {
                break;
            }
            tokio::time::sleep(LOOK).await;
        }
        assert!(
            last.contains(FUNCTION_ACTIVE),
            "setup: the function {name} must reach {FUNCTION_ACTIVE}, last read {last}"
        );
    }
    role_arn
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The function the job's `LambdaInvoke` names.
    function: &'static str,
    /// The `InvocationSchemaVersion` the job names, or `None` to leave it out.
    schema_version: Option<&'static str>,
    /// The keys the manifest lists, in manifest order.
    listed: &'static [&'static str],
    /// The keys the job's run must leave carrying the tag the handler writes, and the tag
    /// itself.
    tagged: &'static [&'static str],
    tag: (&'static str, &'static str),
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a handler that answers Succeeded for every task",
            function: TAGGING_FUNCTION,
            schema_version: None,
            listed: &["plain.txt", "under/a/prefix.txt"],
            tagged: &["plain.txt", "under/a/prefix.txt"],
            tag: (TAG_KEY, TAG_VALUE),
        },
        Row {
            case: "a handler that answers PermanentFailure for one task of three",
            function: SELECTIVE_FUNCTION,
            schema_version: Some("1.0"),
            listed: &["kept-one.txt", "reject-this.txt", "kept/two.txt"],
            tagged: &["kept-one.txt", "kept/two.txt"],
            tag: (TAG_KEY, TAG_VALUE),
        },
        Row {
            case: "the schema that names each task's bucket rather than its ARN",
            function: ARGUMENT_FUNCTION,
            schema_version: Some("2.0"),
            listed: &["by-name.txt"],
            tagged: &["by-name.txt"],
            tag: (BUCKET_BY_NAME_TAG_KEY, BUCKET_BY_NAME_TAG_VALUE),
        },
        Row {
            case: "a handler that raises instead of answering a verdict",
            function: RAISING_FUNCTION,
            schema_version: None,
            listed: &["never-processed.txt"],
            tagged: &[],
            tag: (TAG_KEY, TAG_VALUE),
        },
    ]
}

impl Row {
    /// The `<LambdaInvoke>` element this row's job carries.
    fn operation(&self) -> String {
        let function_arn =
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{}", self.function);
        let mut element = format!("<LambdaInvoke><FunctionArn>{function_arn}</FunctionArn>");
        if let Some(version) = self.schema_version {
            element.push_str(&format!(
                "<InvocationSchemaVersion>{version}</InvocationSchemaVersion>"
            ));
        }
        element.push_str("</LambdaInvoke>");
        element
    }

    /// The key the manifest of this row's job is stored at.
    fn manifest_key(&self, at: usize) -> String {
        format!("manifests/row-{at}.csv")
    }

    /// The prefix this row's objects live under, so one row's keys never collide with
    /// another's.
    fn key(&self, at: usize, listed: &str) -> String {
        format!("row-{at}/{listed}")
    }
}

/// The three `ProgressSummary` counters a settled job reports.
fn progress(case: &str, described: &str) -> (i64, i64, i64) {
    let read = |name: &str| -> i64 {
        opt_element(described, name)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the settled job must report {name}, got {described}")
            })
            .parse()
            .unwrap_or_else(|error| {
                panic!("case {case:?}: {name} is a number: {error}, got {described}")
            })
    };
    (
        read("TotalNumberOfTasks"),
        read("NumberOfTasksSucceeded"),
        read("NumberOfTasksFailed"),
    )
}

/// Poll `DescribeJob` until the job reports a terminal status, exactly as a client waits for
/// one, and answer the body it was last described with.
async fn settled_job(router: &MultiServiceRouter, case: &str, job_id: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let described = s3control(router, "GET", &format!("/v20180820/jobs/{job_id}"), "").await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribeJob must answer 200, got {} with {}",
            described.status, described.body
        );
        let status = opt_element(&described.body, "Status").unwrap_or_default();
        if status == JOB_COMPLETE || status == JOB_FAILED || status == "Cancelled" {
            return described.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status; last status {status:?} in \
             {}",
            described.body
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The tag set `BUCKET` holds on `key`, as (key, value) pairs.
async fn object_tags(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<(String, String)> {
    let read = s3(router, "GET", &format!("/{BUCKET}/{key}?tagging"), "").await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetObjectTagging({key:?}) must answer 200, got {} with {}",
        read.status, read.body
    );
    elements(&read.body, "Key")
        .into_iter()
        .zip(elements(&read.body, "Value"))
        .collect()
}

#[test]
fn a_lambda_invoke_job_runs_its_function_over_every_manifest_entry() {
    std::thread::Builder::new()
        .name("xsvc-batch-lambda-invoke".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let router = build_router();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .expect("a multi-thread runtime");

            runtime.block_on(async {
                let role_arn = given_estate(&router).await;

                for (at, row) in rows().iter().enumerate() {
                    let case = row.case;

                    // The objects the job's tasks act on, and the manifest that lists them:
                    // both written through Amazon S3 itself.
                    let mut manifest = String::new();
                    for listed in row.listed {
                        let key = row.key(at, listed);
                        let written =
                            s3(&router, "PUT", &format!("/{BUCKET}/{key}"), "the object's bytes")
                                .await;
                        assert_eq!(
                            written.status, 200,
                            "setup {case:?}: the object {key} must be written, got {} with {}",
                            written.status, written.body
                        );
                        manifest.push_str(&format!("{BUCKET},{key}\n"));
                    }
                    let manifest_key = row.manifest_key(at);
                    let listed_manifest = s3(
                        &router,
                        "PUT",
                        &format!("/{BUCKET}/{manifest_key}"),
                        &manifest,
                    )
                    .await;
                    assert_eq!(
                        listed_manifest.status, 200,
                        "setup {case:?}: the manifest must be written, got {} with {}",
                        listed_manifest.status, listed_manifest.body
                    );

                    let submitted = s3control(
                        &router,
                        "POST",
                        "/v20180820/jobs",
                        &format!(
                            "<CreateJobRequest xmlns=\"{XMLNS}\">\
                             <ClientRequestToken>lambda-invoke-{at}</ClientRequestToken>\
                             <ConfirmationRequired>false</ConfirmationRequired>\
                             <Operation>{}</Operation>\
                             <Report><Enabled>false</Enabled></Report>\
                             <Manifest><Spec>\
                             <Format>S3BatchOperations_CSV_20180820</Format>\
                             <Fields><member>Bucket</member><member>Key</member></Fields>\
                             </Spec><Location>\
                             <ObjectArn>arn:aws:s3:::{BUCKET}/{manifest_key}</ObjectArn>\
                             <ETag>{}</ETag></Location></Manifest>\
                             <Priority>1</Priority><RoleArn>{role_arn}</RoleArn>\
                             </CreateJobRequest>",
                            row.operation(),
                            listed_manifest.etag
                        ),
                    )
                    .await;
                    assert_eq!(
                        submitted.status, 200,
                        "case {case:?}: CreateJob must answer 200, got {} with {}",
                        submitted.status, submitted.body
                    );
                    let job_id = element(&submitted.body, "JobId");
                    assert!(
                        !job_id.is_empty(),
                        "case {case:?}: CreateJob must answer a JobId, got {}",
                        submitted.body
                    );

                    // Nothing else is called: the job runs on its own and the poll observes
                    // the status it settled in.
                    let settled = settled_job(&router, case, &job_id).await;
                    assert_eq!(
                        opt_element(&settled, "Status").as_deref(),
                        Some(JOB_COMPLETE),
                        "case {case:?}: a job whose tasks ran is Complete however many of them \
                         failed, got {settled}"
                    );
                    assert!(
                        opt_element(&settled, "FailureReason").is_none(),
                        "case {case:?}: a job that completed carries no FailureReasons, got \
                         {settled}"
                    );

                    let total = i64::try_from(row.listed.len()).expect("the row count fits");
                    let succeeded = i64::try_from(row.tagged.len()).expect("the row count fits");
                    assert_eq!(
                        progress(case, &settled),
                        (total, succeeded, total - succeeded),
                        "case {case:?}: expected every manifest entry accounted for by the \
                         verdict the function answered, got {settled}"
                    );

                    // What the invocations left behind in Amazon S3: the tag the handler
                    // wrote on exactly the objects whose task it carried out.
                    for listed in row.listed {
                        let key = row.key(at, listed);
                        let tags = object_tags(&router, case, &key).await;
                        let expected: Vec<(String, String)> = if row.tagged.contains(listed) {
                            vec![(row.tag.0.to_string(), row.tag.1.to_string())]
                        } else {
                            Vec::new()
                        };
                        assert_eq!(
                            tags, expected,
                            "case {case:?}: expected {key} to carry {expected:?}, got {tags:?}"
                        );
                    }
                }
            });
        })
        .expect("spawn the batch-operations thread")
        .join()
        .expect("a LambdaInvoke job runs its function over every manifest entry");
}
