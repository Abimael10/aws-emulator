// SPDX-License-Identifier: CC0-1.0

//! The deployment package a stack-deployed `AWS::Lambda::Function` is identified by, read back
//! through AWS Lambda's own reads.
//!
//! RULE under test: **a function's reported package identity is measured from the BYTES of the
//! deployment package it holds — `CodeSha256` is their base64-encoded SHA-256 ("The SHA256 hash
//! of the function's deployment package") and `CodeSize` their length ("The size of the
//! function's deployment package, in bytes") — whichever route named those bytes, an Amazon S3
//! object a template's `Code` names included, and both move to the new object's bytes when a
//! deployment repoints that location and stay on the bytes in force when it does not.** The two
//! members are `FunctionConfiguration`'s own
//! (`smithy_query(query="shape",shape="FunctionConfiguration")`), so every read that answers a
//! `FunctionConfiguration` reports them: `GetFunction` under `Configuration`, and
//! `GetFunctionConfiguration` as the document itself.
//!
//! Why it matters: the digest is the only thing a client can tell WHICH bytes a function runs
//! from. A deploy that repoints an object key is verified by comparing it, content-addressed
//! deploys (a CDK asset hash) and drift detection have nothing else to compare, and a caller
//! that reads the member on a function whose package came from Amazon S3 must find a value
//! rather than no member at all.
//!
//! The rows differ along what the rule generalises over: the archive's own bytes and so its
//! length (a bare handler, one padded to several kilobytes, one padded further), the FORM of the
//! key the object sits under (a flat name, a nested date prefix, a name carrying dots), whether
//! the repointed location names another key of the same bucket or an object in a DIFFERENT
//! bucket, and which route the first package took (an Amazon S3 object, and the template's own
//! inline source — whose archive CloudFormation itself writes, so the row states only that a
//! digest over it is reported and that it MOVES to the object's own on the repoint).
//!
//! Every expected digest and size here is computed from the bytes this suite staged, never read
//! out of the answer being checked.

use serde_json::{json, Value};
use sha2::Digest as _;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, REGION,
};
use crate::hotswapped_function_code::{lambda, package};
use crate::stack_resource_provisioning::s3;

/// The runtime every function below declares, and so the extension of the one source file the
/// archive carries: a Python handler of `index.handler` lives in `index.py`.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";
const SOURCE_FILE: &str = "index.py";

/// The logical ids every template below declares its two resources under.
const FUNCTION_LOGICAL_ID: &str = "Fn";
const ROLE_LOGICAL_ID: &str = "ExecRole";

/// The handler source every archive carries, before a row's padding lengthens it.
const SOURCE: &str = "def handler(event, context):\n    return {'ok': True}\n";

/// One line of padding, repeated to make one row's archive longer than another's: a comment, so
/// a padded package is still the same handler.
const PADDING_LINE: &str = "# padding that lengthens this deployment package\n";

/// The statuses a healthy create and a healthy in-place update settle on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The status a stack settles on when a resource's change was refused and the stack put the
/// resource back.
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// An Amazon S3 object this suite stages and a template then names as a function's package.
#[derive(Clone, Copy, Debug)]
struct Object {
    /// The bucket it is staged in.
    bucket: &'static str,
    /// The key it is staged under.
    key: &'static str,
    /// How many padding lines the archived source carries, which is what makes one object's
    /// bytes — and so its digest and its length — differ from another's.
    padding: usize,
}

impl Object {
    /// The archive's own bytes: the handler, padded, in a one-entry .zip package.
    fn archive(self) -> Vec<u8> {
        let mut source = SOURCE.to_string();
        for _ in 0..self.padding {
            source.push_str(PADDING_LINE);
        }
        package(SOURCE_FILE, &source)
    }
}

/// Where one stage of a row declares the function's deployment package.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// An Amazon S3 object, named by `Code.S3Bucket`/`Code.S3Key` the way `sam package` and
    /// `cdk deploy` name an uploaded one.
    S3Object(Object),
    /// The template's own `Code.ZipFile` source text, which CloudFormation archives itself.
    InlineSource,
}

/// One row: the package the create deploys, and the one the update repoints the function at.
#[derive(Clone, Copy, Debug)]
struct Row {
    case: &'static str,
    /// The stack name, which the function and role names are derived from so one row's
    /// resources are never another's.
    stack: &'static str,
    deployed: Source,
    repointed: Source,
}

/// The class the rule covers.
const ROWS: &[Row] = &[
    Row {
        case: "a bare archive at a flat key, repointed to a longer one in the same bucket",
        stack: "pkgid-flat",
        deployed: Source::S3Object(Object {
            bucket: "pkgid-flat-code",
            key: "handler.zip",
            padding: 0,
        }),
        repointed: Source::S3Object(Object {
            bucket: "pkgid-flat-code",
            key: "handler-v2.zip",
            padding: 200,
        }),
    },
    Row {
        case: "an archive under a nested date prefix, repointed to a SHORTER one",
        stack: "pkgid-nested",
        deployed: Source::S3Object(Object {
            bucket: "pkgid-nested-code",
            key: "builds/2026/01/31/handler.zip",
            padding: 400,
        }),
        repointed: Source::S3Object(Object {
            bucket: "pkgid-nested-code",
            key: "builds/2026/02/01/handler.zip",
            padding: 0,
        }),
    },
    Row {
        case: "a key carrying dots, repointed to an object in another bucket",
        stack: "pkgid-dotted",
        deployed: Source::S3Object(Object {
            bucket: "pkgid-dotted-code",
            key: "app.handler.v1.zip",
            padding: 10,
        }),
        repointed: Source::S3Object(Object {
            bucket: "pkgid-dotted-next",
            key: "app.handler.v2.zip",
            padding: 20,
        }),
    },
    Row {
        case: "the template's own inline source, repointed at an Amazon S3 object",
        stack: "pkgid-inline",
        deployed: Source::InlineSource,
        repointed: Source::S3Object(Object {
            bucket: "pkgid-inline-code",
            key: "staged/handler.zip",
            padding: 35,
        }),
    },
];

/// The base64-encoded SHA-256 of `bytes` — the independent computation of what `CodeSha256`
/// must report, so no case reads the value it is checking out of the answer.
fn expected_digest(bytes: &[u8]) -> String {
    aws_smithy_types::base64::encode(sha2::Sha256::digest(bytes))
}

/// What AWS Lambda must report for one stage's package: the digest and the size of the bytes
/// this suite staged, or `None` where the archive is one CloudFormation wrote from the
/// template's inline source (its bytes are that service's own, so what the row states about it
/// is that a digest over them is reported at all and that it MOVES when the package does).
fn expected_identity(source: &Source) -> Option<(String, i64)> {
    match source {
        Source::S3Object(object) => {
            let archive = object.archive();
            let size = i64::try_from(archive.len()).expect("an archive shorter than i64::MAX");
            Some((expected_digest(&archive), size))
        }
        Source::InlineSource => None,
    }
}

/// The function name a row's template declares, and so the physical id the stack publishes.
fn function_name(row: &Row) -> String {
    format!("{}-fn", row.stack)
}

/// The execution role name a row's template declares.
fn role_name(row: &Row) -> String {
    format!("{}-exec", row.stack)
}

/// The capabilities a template declaring a NAMED IAM role has to be submitted with.
fn capabilities() -> Vec<(String, String)> {
    vec![member("Capabilities.member.1", "CAPABILITY_NAMED_IAM")]
}

/// Create `bucket` in Amazon S3 if this suite has not already, the way a client creates the
/// bucket it stages a package in: a bucket this account already owns is no failure, because
/// two stages of one row share a bucket.
async fn ensure_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert!(
        (200..300).contains(&created.status) || created.status == 409,
        "case {case:?}: expected Amazon S3 to hold the bucket {bucket}, got {} {}",
        created.status,
        created.body
    );
}

/// Stage one object's archive in Amazon S3, as a client uploads a deployment package before the
/// stack that names it is submitted, and answer the bytes that were stored.
async fn stage(router: &MultiServiceRouter, case: &str, object: Object) -> Vec<u8> {
    ensure_bucket(router, case, object.bucket).await;
    let archive = object.archive();
    let staged = s3(
        router,
        "PUT",
        &format!("/{}/{}", object.bucket, object.key),
        &archive,
    )
    .await;
    assert!(
        (200..300).contains(&staged.status),
        "case {case:?}: expected the package {} to be staged, got {} {}",
        object.key,
        staged.status,
        staged.body
    );
    archive
}

/// The `Code` property one stage declares, with its Amazon S3 object staged first where it
/// names one.
async fn declared_code(router: &MultiServiceRouter, case: &str, source: &Source) -> Value {
    match source {
        Source::S3Object(object) => {
            stage(router, case, *object).await;
            json!({ "S3Bucket": object.bucket, "S3Key": object.key })
        }
        Source::InlineSource => json!({ "ZipFile": SOURCE }),
    }
}

/// The template one stage deploys: the execution role the function runs as, and the function
/// with the `Code` this stage states.
fn template(row: &Row, code: &Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role_name(row),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole"
                        }]
                    }
                }
            },
            FUNCTION_LOGICAL_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(row),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                    "Code": code
                }
            }
        }
    })
    .to_string()
}

/// The `(CodeSha256, CodeSize)` pair one `FunctionConfiguration` document reports, with the
/// missing member spelled as the empty string and a missing size as `-1`, so an absent member
/// fails the assertion that names it rather than passing as a default.
fn reported_identity(case: &str, document: &Value) -> (String, i64) {
    (
        document
            .get("CodeSha256")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        document.get("CodeSize").and_then(Value::as_i64).map_or(-1, |size| {
            assert!(
                size >= 0,
                "case {case:?}: expected a package length, got {size} in {document}"
            );
            size
        }),
    )
}

/// What `GetFunction` reports about the package `name` holds, read under `Configuration`.
async fn get_function_identity(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, i64) {
    let answered = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected AWS Lambda to hold the function {name:?}, got {} with body {}",
        answered.status, answered.body
    );
    let configuration = answered
        .body
        .get("Configuration")
        .cloned()
        .unwrap_or(Value::Null);
    reported_identity(case, &configuration)
}

/// The same pair as `GetFunctionConfiguration` reports it, which answers the same
/// `FunctionConfiguration` shape as the document itself.
async fn get_configuration_identity(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, i64) {
    let answered = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected AWS Lambda to hold the configuration of {name:?}, got {} with \
         body {}",
        answered.status, answered.body
    );
    reported_identity(case, &answered.body)
}

/// Deploy `template` as a new stack and answer the status it settled on.
async fn created(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) -> String {
    let (status, answered) = create_stack(router, stack, body, &capabilities()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status} {answered}"
    );
    settled_stack_status(case, router, REGION, stack).await
}

/// Submit `template` as an update of the stack and answer the status it settled on.
async fn updated(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) -> String {
    let (status, answered) = call(
        router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", body),
            member("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateStack to be accepted, got {status} {answered}"
    );
    settled_stack_status(case, router, REGION, stack).await
}

/// Hold one stage's reported identity to what the row expects of it: the staged bytes' own
/// digest and length, or — for the archive CloudFormation wrote itself — a digest and a length
/// over bytes it really holds.
fn assert_identity(case: &str, stage: &str, expected: Option<(String, i64)>, got: (String, i64)) {
    match expected {
        Some(expected) => assert_eq!(
            got, expected,
            "case {case:?} ({stage}): expected (CodeSha256, CodeSize) {expected:?}, got {got:?}"
        ),
        None => assert!(
            !got.0.is_empty() && got.1 > 0,
            "case {case:?} ({stage}): expected a digest and a length over the archive the \
             deployment wrote, got {got:?}"
        ),
    }
}

/// RULE: the package identity a stack-deployed function reports is the digest and the length of
/// the bytes its `Code` names — an Amazon S3 object's own — and it moves with the location the
/// deployment repoints.
#[tokio::test]
async fn a_stack_deployed_function_reports_the_digest_and_size_of_the_package_it_holds() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let function = function_name(row);

        // ---- The create: the package the template's first `Code` names. ----
        let code = declared_code(&router, case, &row.deployed).await;
        let settled = created(&router, case, row.stack, &template(row, &code)).await;
        assert_eq!(
            settled,
            CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, got {settled:?} with events {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        let deployed = get_function_identity(&router, case, &function).await;
        assert_identity(
            case,
            "the create",
            expected_identity(&row.deployed),
            deployed.clone(),
        );

        // The same pair, from the read that answers the configuration on its own...
        let from_configuration = get_configuration_identity(&router, case, &function).await;
        assert_eq!(
            from_configuration, deployed,
            "case {case:?}: expected GetFunctionConfiguration to report the same \
             (CodeSha256, CodeSize) as GetFunction's Configuration ({deployed:?}), got \
             {from_configuration:?}"
        );
        // ...and reading it again reports the same package: a read does not change which bytes
        // the function holds.
        let read_again = get_function_identity(&router, case, &function).await;
        assert_eq!(
            read_again, deployed,
            "case {case:?}: expected a second read to report the same package {deployed:?}, got \
             {read_again:?}"
        );

        // ---- The update: the same function, its `Code` repointed. ----
        let repointed = declared_code(&router, case, &row.repointed).await;
        let settled = updated(&router, case, row.stack, &template(row, &repointed)).await;
        assert_eq!(
            settled,
            UPDATE_COMPLETE,
            "case {case:?}: expected the repointing update to complete, got {settled:?} with \
             events {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        let after = get_function_identity(&router, case, &function).await;
        assert_identity(
            case,
            "the repointing update",
            expected_identity(&row.repointed),
            after.clone(),
        );
        assert_ne!(
            after.0, deployed.0,
            "case {case:?}: expected the repointed package to report a different CodeSha256 \
             than {:?}, got {:?}",
            deployed.0, after.0
        );
        let after_configuration = get_configuration_identity(&router, case, &function).await;
        assert_eq!(
            after_configuration, after,
            "case {case:?}: expected both reads to report the repointed package {after:?}, got \
             {after_configuration:?}"
        );
    }
}

/// One row of the refusal half: the location the update repoints the function at, which Amazon
/// S3 hands no object over for.
#[derive(Clone, Copy, Debug)]
struct MissingRow {
    case: &'static str,
    stack: &'static str,
    /// The object the create deploys, which Amazon S3 does hold.
    deployed: Object,
    /// The bucket and key the update repoints `Code` at.
    bucket: &'static str,
    key: &'static str,
}

/// The two ways the repointed location names no object: a key the staged bucket holds nothing
/// under, and a bucket Amazon S3 holds no record of at all.
const MISSING_ROWS: &[MissingRow] = &[
    MissingRow {
        case: "a key the bucket holds nothing under",
        stack: "pkgid-missing-key",
        deployed: Object {
            bucket: "pkgid-missing-key-code",
            key: "handler.zip",
            padding: 5,
        },
        bucket: "pkgid-missing-key-code",
        key: "never-uploaded.zip",
    },
    MissingRow {
        case: "a bucket Amazon S3 holds no record of",
        stack: "pkgid-missing-bucket",
        deployed: Object {
            bucket: "pkgid-missing-bucket-code",
            key: "builds/handler.zip",
            padding: 0,
        },
        bucket: "pkgid-missing-bucket-elsewhere",
        key: "handler.zip",
    },
];

/// RULE (the same one, where the repoint does NOT take): the reported identity is always over
/// bytes the function really holds, so an update whose new location names no object leaves the
/// digest and the size of the package in force.
#[tokio::test]
async fn a_repoint_at_an_object_amazon_s3_serves_nothing_for_leaves_the_package_in_force() {
    for row in MISSING_ROWS {
        let case = row.case;
        let router = router();
        let deployed_row = Row {
            case: row.case,
            stack: row.stack,
            deployed: Source::S3Object(row.deployed),
            repointed: Source::S3Object(row.deployed),
        };
        let function = function_name(&deployed_row);

        let code = declared_code(&router, case, &Source::S3Object(row.deployed)).await;
        let settled = created(
            &router,
            case,
            row.stack,
            &template(&deployed_row, &code),
        )
        .await;
        assert_eq!(
            settled,
            CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, got {settled:?} with events {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        let before = get_function_identity(&router, case, &function).await;
        let expected = expected_identity(&Source::S3Object(row.deployed));
        assert_identity(case, "the create", expected, before.clone());

        // The repoint names a location Amazon S3 serves no object for, so AWS Lambda refuses the
        // change and the stack puts the resource back.
        let broken = json!({ "S3Bucket": row.bucket, "S3Key": row.key });
        let settled = updated(
            &router,
            case,
            row.stack,
            &template(&deployed_row, &broken),
        )
        .await;
        assert_eq!(
            settled,
            UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to roll the refused repoint back, got \
             {settled:?} with events {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        let after = get_function_identity(&router, case, &function).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the function to keep the package it holds ({before:?}), got \
             {after:?}"
        );
    }
}
