// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::GameLift::Build` or an
//! `AWS::GameLift::Script` deploys: the build and the script are created in Amazon GameLift
//! Servers' own control plane from the Amazon S3 object the declaration names, the stack
//! publishes the id that service minted and the ARN built around it, deleting the stack takes
//! the resource away — and a declaration that service REFUSES fails the resource with its own
//! refusal and rolls the stack back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the
//! stack publishes is what that service answered, and what that service refuses is a FAILED
//! RESOURCE rather than a template-format error.** Four crates have to agree for that to hold
//! (CloudFormation submits the template and makes the calls, Amazon GameLift Servers serves
//! them, Amazon S3 holds the zip file it reads and IAM holds the role it reads it under), so
//! the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows of the deploy table differ along what the rule generalises over: which of the two
//! types the stack declares (a build, a script, both in one stack), the declared properties
//! (each operating system and server SDK version, each documented Node.js version, a version
//! label, the type's own `Tags`, a storage location naming an object version), whether the
//! role the location names is declared IN the stack or named outright, which published return
//! value the output reads (`Ref`, `Fn::GetAtt BuildId`/`Id`, `Fn::GetAtt Arn`), and the
//! template spelling (JSON with long-form intrinsics, YAML with the short-form tags).
//!
//! The refusal rows differ along the CLASS of declaration Amazon GameLift Servers refuses: a
//! script with no runtime version at all, a script on a runtime that service does not run, a
//! build with no operating system, and a storage location missing the role that reads it. Each
//! is the owning service's own refusal, reported on the resource that failed — never the
//! `Template format error: Unrecognized resource types` a type CloudFormation does not know is
//! refused with, which no row of either table may ever see.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stacks belong to. The
/// buckets below are created with no `CreateBucketConfiguration`, which is Amazon S3's own
/// rule for this Region: "If you don't specify a Region, the bucket is created in the US East
/// (N. Virginia) Region (us-east-1) by default".
const REGION: &str = "us-east-1";

/// How long a wait for a condition may run before the test gives up. Generous on purpose: the
/// conditions below come due after spans of a few hundred milliseconds, and a loaded host may
/// take far longer to get there — what is asserted is the condition, never the wall clock.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long to leave between two looks at a condition that has not held yet. Long enough to
/// leave the host to the work being waited on rather than to this suite's polling.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for the two types AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_gamelift_gamelift::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request")
    ;
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon GameLift Servers operation over the awsJson1_1 wire its model declares,
/// answering the status and the parsed document.
async fn gamelift(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("gamelift.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("GameLift.{operation}"))
        .header("authorization", authorization("gamelift"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Store one object in the emulator's own object storage, creating its bucket first, so the
/// zip file the declaration names is one Amazon S3 really holds — Amazon GameLift Servers
/// READS it while serving the create, and a build whose files are not there is left in the
/// status that says so.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    for (path, body) in [
        (format!("/{bucket}"), Bytes::new()),
        (
            format!("/{bucket}/{key}"),
            Bytes::from_static(b"PK\x03\x04 a game build"),
        ),
    ] {
        let request = http::Request::builder()
            .method("PUT")
            .uri(&path)
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", authorization("s3"))
            .body(body)
            .expect("a well-formed request");
        let response = router.dispatch(request).await;
        assert!(
            response.status().is_success(),
            "case {case:?}: seeding s3:/{path} must succeed, got {} with body {}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
    }
}

/// The `DescribeStacks` answer for a stack whose create has settled in a terminal status: a
/// stack reported `CREATE_IN_PROGRESS` is still being deployed, so the body is read again
/// until it is not.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never left {reported}, last body {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Submit one template and wait for the deployment to settle, answering the `DescribeStacks`
/// body. The create itself must be ACCEPTED: a published resource type is never refused as a
/// template-format error.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an AWS::GameLift::Build and an \
         AWS::GameLift::Script are types a template may declare — got {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected both types to be recognised, got {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// The record Amazon GameLift Servers reports for one build or script, by the id the stack
/// published, or `None` where that service holds none.
async fn described(
    router: &MultiServiceRouter,
    kind: Kind,
    id: &str,
) -> Option<Value> {
    let (status, body) = gamelift(
        router,
        kind.describe_operation(),
        &json!({ kind.id_member(): id }),
    )
    .await;
    if status != 200 {
        return None;
    }
    body.get(kind.record_member()).cloned()
}

/// Which of the two resources a row is about. The two differ only in what their records are
/// called and in which members carry a resource's identity, so one set of helpers serves both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Build,
    Script,
}

impl Kind {
    fn resource_type(self) -> &'static str {
        match self {
            Self::Build => "AWS::GameLift::Build",
            Self::Script => "AWS::GameLift::Script",
        }
    }

    fn describe_operation(self) -> &'static str {
        match self {
            Self::Build => "DescribeBuild",
            Self::Script => "DescribeScript",
        }
    }

    fn record_member(self) -> &'static str {
        match self {
            Self::Build => "Build",
            Self::Script => "Script",
        }
    }

    fn id_member(self) -> &'static str {
        match self {
            Self::Build => "BuildId",
            Self::Script => "ScriptId",
        }
    }

    fn arn_member(self) -> &'static str {
        match self {
            Self::Build => "BuildArn",
            Self::Script => "ScriptArn",
        }
    }

    /// The prefix the id that service mints carries (`BuildId`: `^build-\S+$`, `ScriptId`:
    /// `^script-\S+$`).
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Build => "build-",
            Self::Script => "script-",
        }
    }

    /// The resource path the ARN built around that id carries (`Build$BuildArn`:
    /// `^arn:.*:build\/build-\S+$`).
    fn arn_infix(self) -> &'static str {
        match self {
            Self::Build => ":build/",
            Self::Script => ":script/",
        }
    }
}

/// What one row expects of one resource the stack declares.
struct Declared {
    kind: Kind,
    /// The output key whose value is the id that service minted.
    id_output: &'static str,
    /// The output key whose value is the ARN it built around that id, where the row reads one.
    arn_output: Option<&'static str>,
    /// The members the record that service reports has to carry, under the names its own model
    /// spells them with — each the value the template declared.
    reported: Value,
}

/// One row: a template, the object it ships the code from, and the resources it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    bucket: &'static str,
    key: &'static str,
    template: String,
    declared: Vec<Declared>,
}

/// The role the storage locations below are read under, for the rows that name one outright
/// rather than declaring it in the stack.
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/GameLiftS3Access";

/// The trust policy a role declared IN the stack carries, so the role the location names is
/// one the owning service could assume.
fn assume_role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "gamelift.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own build stack: the role is declared IN the stack and the storage
        // location names it with `Fn::GetAtt`, with every published return value read back.
        Row {
            case: "a build whose storage location names a role declared in the stack",
            stack_name: "gamelift-build-round-trip",
            bucket: "studio-build-artifacts",
            key: "build.zip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Role": {
                        "Type": "AWS::IAM::Role",
                        "Properties": {
                            "RoleName": "gamelift-build-access",
                            "AssumeRolePolicyDocument": assume_role_policy(),
                        }
                    },
                    "Build": {
                        "Type": "AWS::GameLift::Build",
                        "Properties": {
                            "Name": "demo-build",
                            "OperatingSystem": "AMAZON_LINUX_2023",
                            "ServerSdkVersion": "5.1.2",
                            "StorageLocation": {
                                "Bucket": "studio-build-artifacts",
                                "Key": "build.zip",
                                "RoleArn": {"Fn::GetAtt": ["Role", "Arn"]},
                            },
                        }
                    }
                },
                "Outputs": {
                    "BuildRef": {"Value": {"Ref": "Build"}},
                    "BuildId": {"Value": {"Fn::GetAtt": ["Build", "BuildId"]}},
                    "BuildArn": {"Value": {"Fn::GetAtt": ["Build", "Arn"]}},
                }
            })
            .to_string(),
            declared: vec![Declared {
                kind: Kind::Build,
                id_output: "BuildRef",
                arn_output: Some("BuildArn"),
                reported: json!({
                    "Name": "demo-build",
                    "OperatingSystem": "AMAZON_LINUX_2023",
                    "ServerSdkVersion": "5.1.2",
                }),
            }],
        },
        // Another operating system and server SDK version, a version label, the type's own
        // `Tags`, and the role named outright — read back through `Fn::GetAtt BuildId` alone.
        Row {
            case: "a Windows build with a version label and tags, the role named outright",
            stack_name: "gamelift-build-windows",
            bucket: "studio-windows-artifacts",
            key: "builds/win.zip",
            template: json!({
                "Resources": {
                    "Build": {
                        "Type": "AWS::GameLift::Build",
                        "Properties": {
                            "Name": "windows-build",
                            "OperatingSystem": "WINDOWS_2016",
                            "ServerSdkVersion": "4.0.2",
                            "Version": "2.3.1",
                            "StorageLocation": {
                                "Bucket": "studio-windows-artifacts",
                                "Key": "builds/win.zip",
                                "RoleArn": ROLE_ARN,
                            },
                            "Tags": [{"Key": "team", "Value": "liveops"}],
                        }
                    }
                },
                "Outputs": {"BuildKey": {"Value": {"Fn::GetAtt": ["Build", "BuildId"]}}}
            })
            .to_string(),
            declared: vec![Declared {
                kind: Kind::Build,
                id_output: "BuildKey",
                arn_output: None,
                reported: json!({
                    "Name": "windows-build",
                    "OperatingSystem": "WINDOWS_2016",
                    "ServerSdkVersion": "4.0.2",
                    "Version": "2.3.1",
                }),
            }],
        },
        // The script type in YAML with short-form intrinsics, on the newer documented Node.js
        // version, with a storage location naming an object version.
        Row {
            case: "a script in the YAML spelling, from a versioned object",
            stack_name: "gamelift-script-round-trip",
            bucket: "studio-script-artifacts",
            key: "script.zip",
            template: "\
Resources:
  Script:
    Type: AWS::GameLift::Script
    Properties:
      Name: demo-script
      NodeJsVersion: 24.x
      Version: \"1.0.0\"
      StorageLocation:
        Bucket: studio-script-artifacts
        Key: script.zip
        RoleArn: arn:aws:iam::123456789012:role/GameLiftS3Access
        ObjectVersion: \"3HL4kqtJlcpXroDTDmjVBH40Nrjfkd\"
Outputs:
  ScriptRef:
    Value: !Ref Script
  ScriptArn:
    Value: !GetAtt Script.Arn
  ScriptId:
    Value: !GetAtt Script.Id
"
            .to_string(),
            declared: vec![Declared {
                kind: Kind::Script,
                id_output: "ScriptRef",
                arn_output: Some("ScriptArn"),
                reported: json!({
                    "Name": "demo-script",
                    "NodeJsVersion": "24.x",
                    "Version": "1.0.0",
                }),
            }],
        },
        // A build and a script in ONE stack, on the older documented Node.js version and a
        // third operating system: each publishes its OWN minted identity.
        Row {
            case: "a build and a script in one stack",
            stack_name: "gamelift-pair",
            bucket: "studio-shared-artifacts",
            key: "game.zip",
            template: json!({
                "Resources": {
                    "Build": {
                        "Type": "AWS::GameLift::Build",
                        "Properties": {
                            "Name": "paired-build",
                            "OperatingSystem": "AMAZON_LINUX_2",
                            "StorageLocation": {
                                "Bucket": "studio-shared-artifacts",
                                "Key": "game.zip",
                                "RoleArn": ROLE_ARN,
                            },
                        }
                    },
                    "Script": {
                        "Type": "AWS::GameLift::Script",
                        "Properties": {
                            "Name": "paired-script",
                            "NodeJsVersion": "10.x",
                            "StorageLocation": {
                                "Bucket": "studio-shared-artifacts",
                                "Key": "game.zip",
                                "RoleArn": ROLE_ARN,
                            },
                            "Tags": [{"Key": "env", "Value": "prod"}],
                        }
                    }
                },
                "Outputs": {
                    "BuildRef": {"Value": {"Ref": "Build"}},
                    "BuildArn": {"Value": {"Fn::GetAtt": ["Build", "Arn"]}},
                    "ScriptRef": {"Value": {"Ref": "Script"}},
                    "ScriptArn": {"Value": {"Fn::GetAtt": ["Script", "Arn"]}},
                }
            })
            .to_string(),
            declared: vec![
                Declared {
                    kind: Kind::Build,
                    id_output: "BuildRef",
                    arn_output: Some("BuildArn"),
                    reported: json!({
                        "Name": "paired-build",
                        "OperatingSystem": "AMAZON_LINUX_2",
                    }),
                },
                Declared {
                    kind: Kind::Script,
                    id_output: "ScriptRef",
                    arn_output: Some("ScriptArn"),
                    reported: json!({
                        "Name": "paired-script",
                        "NodeJsVersion": "10.x",
                    }),
                },
            ],
        },
    ]
}

/// The build status that service reports once it has read the files at the location the
/// declaration named: "READY -- the game build has been successfully uploaded", from the
/// modelled `BuildStatus` enum.
const BUILD_STATUS_READY: &str = "READY";

/// One deployment per row, then the refusal table, on ONE emulator: every phase of this
/// suite shares a single assembled emulator and runs as a single test, so the router's test
/// binary stands one of them up rather than one per row and one per phase.
#[tokio::test]
async fn a_stack_publishes_deletes_and_rolls_back_the_gamelift_resources_it_declares() {
    let router = build_router();
    for row in rows() {
        let case = row.case;
        put_object(&router, case, row.bucket, row.key).await;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body}"
        );
        let outputs: BTreeMap<String, String> = outputs_of(&body);

        for declared in &row.declared {
            let kind = declared.kind;
            let id = outputs
                .get(declared.id_output)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {} output in {body}", declared.id_output)
                })
                .clone();
            assert!(
                id.starts_with(kind.id_prefix()) && id.len() > kind.id_prefix().len(),
                "case {case:?}: expected the published identity of {} to be an id of the \
                 modelled form, got {id:?}",
                kind.resource_type()
            );

            // The identifier the stack published is one the OWNING service resolves, and the
            // resource it resolves to is the one the template declared.
            let record = described(&router, kind, &id).await.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected Amazon GameLift Servers to report {} for {id:?}",
                    kind.resource_type()
                )
            });
            for (member, expected) in declared
                .reported
                .as_object()
                .expect("the expected members are an object")
            {
                assert_eq!(
                    record.get(member),
                    Some(expected),
                    "case {case:?}: expected {} to report {member} as {expected}, got {record}",
                    kind.resource_type()
                );
            }
            assert_eq!(
                record.get(kind.id_member()).and_then(Value::as_str),
                Some(id.as_str()),
                "case {case:?}: expected the record to carry the published id, got {record}"
            );

            // The ARN the resource publishes is the one that service built around that id,
            // read off its own record rather than recomputed by CloudFormation.
            let reported_arn = record
                .get(kind.arn_member())
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: no {} in {record}", kind.arn_member()))
                .to_string();
            assert!(
                reported_arn.contains(kind.arn_infix()) && reported_arn.ends_with(&id),
                "case {case:?}: expected the reported ARN to carry {} and the minted id, got \
                 {reported_arn:?}",
                kind.arn_infix()
            );
            if let Some(key) = declared.arn_output {
                let published = outputs
                    .get(key)
                    .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"));
                assert_eq!(
                    published, &reported_arn,
                    "case {case:?}: expected the published ARN to be the one Amazon GameLift \
                     Servers reports, got {published:?}"
                );
            }

            // A build's files are read from the location the declaration named, and that
            // service reports the build READY once it has: the status is ITS transition, so
            // it is waited for on its own report rather than asserted at a fixed instant.
            if kind == Kind::Build {
                let mut last;
                let deadline = Instant::now() + DEADLINE;
                let mut ready = false;
                loop {
                    let record = described(&router, kind, &id).await.unwrap_or(Value::Null);
                    let status = record
                        .get("Status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    last = record;
                    if status == BUILD_STATUS_READY {
                        ready = true;
                        break;
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                assert!(
                    ready,
                    "case {case:?}: expected Amazon GameLift Servers to report the build \
                     {BUILD_STATUS_READY} once it had read the declared object, last report \
                     {last}"
                );
            }
        }

        // Deleting the stack takes the resources away in the service that held them: its own
        // read of either id then answers its modelled `NotFoundException` rather than a
        // record, which is what makes the stack's teardown real rather than bookkeeping.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", row.stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
        );
        for declared in &row.declared {
            let kind = declared.kind;
            let id = outputs
                .get(declared.id_output)
                .expect("the published id was read above")
                .clone();
            let deadline = Instant::now() + DEADLINE;
            let mut gone = false;
            loop {
                let (status, answered) = gamelift(
                    &router,
                    kind.describe_operation(),
                    &json!({ kind.id_member(): &id }),
                )
                .await;
                let code = answered
                    .get("__type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if status == 400 && code.ends_with("NotFoundException") {
                    gone = true;
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            assert!(
                gone,
                "case {case:?}: expected {} {id} to be gone from Amazon GameLift Servers after \
                 the stack was deleted",
                kind.resource_type()
            );
        }
    }

    refusals_roll_the_stack_back(&router).await;
}

/// One refusal row: a declaration Amazon GameLift Servers refuses, and what the failed
/// resource must report.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    kind: Kind,
    /// The properties the declaration states, which that service refuses.
    properties: Value,
    /// The FIXED text of the sentence that service words that class of refusal with, where it
    /// is recorded; `None` where only the envelope and the class are.
    sentence: Option<&'static str>,
    /// The `HandlerErrorCode` class the provider puts that refusal in.
    handler_error_code: &'static str,
}

/// The bucket and key the refusal rows below ship their code from, which really holds an
/// object: what each row is about is the declaration, not a missing object.
const REFUSED_BUCKET: &str = "studio-refused-artifacts";
const REFUSED_KEY: &str = "game.zip";

fn refused() -> Vec<Refused> {
    vec![
        // The issue's own refusal: the runtime version a Realtime script runs on is a
        // required parameter of its create, and the sentence is that service's own.
        Refused {
            case: "a script with no runtime version at all",
            stack_name: "gamelift-script-no-runtime",
            kind: Kind::Script,
            properties: json!({
                "Name": "runtime-less",
                "StorageLocation": {
                    "Bucket": REFUSED_BUCKET, "Key": REFUSED_KEY, "RoleArn": ROLE_ARN,
                },
            }),
            sentence: Some(
                "The NodeJsVersion parameter is required. See the GameLift API Reference for \
                 CreateScript() for a list of supported runtime versions.",
            ),
            handler_error_code: "InvalidRequest",
        },
        // A runtime version in the modelled form that service does not run: the same class of
        // refusal for a different reason, so the resource fails the same way.
        Refused {
            case: "a script on a runtime version the service does not run",
            stack_name: "gamelift-script-bad-runtime",
            kind: Kind::Script,
            properties: json!({
                "Name": "node-twelve",
                "NodeJsVersion": "12.x",
                "StorageLocation": {
                    "Bucket": REFUSED_BUCKET, "Key": REFUSED_KEY, "RoleArn": ROLE_ARN,
                },
            }),
            sentence: None,
            handler_error_code: "InvalidRequest",
        },
        // A build with no operating system: "You must specify a valid operating system in
        // this request. There is no default value."
        Refused {
            case: "a build with no operating system",
            stack_name: "gamelift-build-no-os",
            kind: Kind::Build,
            properties: json!({
                "Name": "os-less",
                "StorageLocation": {
                    "Bucket": REFUSED_BUCKET, "Key": REFUSED_KEY, "RoleArn": ROLE_ARN,
                },
            }),
            sentence: None,
            handler_error_code: "InvalidRequest",
        },
        // A storage location missing the role that reads it: the location "must also specify
        // a role ARN that you set up to allow Amazon GameLift Servers to access your Amazon
        // S3 bucket".
        Refused {
            case: "a build whose storage location names no role",
            stack_name: "gamelift-build-no-role",
            kind: Kind::Build,
            properties: json!({
                "Name": "role-less",
                "OperatingSystem": "AMAZON_LINUX_2023",
                "StorageLocation": {"Bucket": REFUSED_BUCKET, "Key": REFUSED_KEY},
            }),
            sentence: None,
            handler_error_code: "InvalidRequest",
        },
    ]
}

/// The refusal table, driven on the emulator the deploy table above already stood up: a
/// declaration the owning service refuses fails that resource and rolls the stack back.
#[allow(clippy::too_many_lines)]
async fn refusals_roll_the_stack_back(router: &MultiServiceRouter) {
    put_object(router, "the refusal table", REFUSED_BUCKET, REFUSED_KEY).await;
    for row in refused() {
        let case = row.case;
        let template = json!({
            "Resources": {
                "Resource": {
                    "Type": row.kind.resource_type(),
                    "Properties": row.properties,
                }
            },
            "Outputs": {"ResourceRef": {"Value": {"Ref": "Resource"}}}
        })
        .to_string();

        let body = create_and_settle(router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             id Amazon GameLift Servers never minted, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, and
        // never a refusal of the template: no event reports a validation error at all.
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", row.stack_name)]).await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        // The envelope the provider's SDK wraps that service's sentence in, and the class the
        // provider files the refusal under.
        let expected_envelope = "(Service: GameLift, Status Code: 400, Request ID:";
        let expected_suffix = format!("HandlerErrorCode: {}", row.handler_error_code);
        assert!(
            reasons.iter().any(|reason| reason.contains(expected_envelope)
                && reason.contains("(SDK Attempt Count:")
                && reason.contains(&expected_suffix)),
            "case {case:?}: expected a failure reason carrying {expected_envelope:?} and \
             {expected_suffix:?}, got {reasons:?}"
        );
        if let Some(sentence) = row.sentence {
            assert!(
                reasons.iter().any(|reason| reason.contains(sentence)),
                "case {case:?}: expected a failure reason quoting {sentence:?}, got {reasons:?}"
            );
        }
        assert!(
            !reasons.iter().any(|reason| reason.contains("NotImplemented")),
            "case {case:?}: expected the owning service's own refusal rather than an \
             unimplemented one, got {reasons:?}"
        );

        // And nothing was created: the stack publishes no identifier, so there is nothing for
        // a client to resolve.
    }
}
