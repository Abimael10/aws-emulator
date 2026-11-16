// SPDX-License-Identifier: CC0-1.0

//! The deployment pattern a client ships an S3-to-Lambda pipeline as: one stack declaring the
//! execution role, the function, the `AWS::Lambda::Permission` that admits Amazon S3 and the
//! bucket whose `NotificationConfiguration` names the function — observed only through the
//! API surface, as the handler's own write.
//!
//! The rule under test: **once such a stack is `CREATE_COMPLETE`, every object write the
//! declared configuration admits is published to the declared function as one asynchronous
//! invocation, and the handler's own SDK calls take effect under the declared execution role
//! — whatever spelling the template is written in, wherever the bucket's name came from, and
//! however narrowly the grant and the role's own permissions are scoped.**
//!
//! Four crates have to agree for that to hold (CloudFormation installs the declared wiring,
//! S3 evaluates the configuration and makes the invocation, Lambda holds the grant and runs
//! the handler, and IAM answers for the execution role the handler's calls are signed as), so
//! the suite lives here. The sibling suites
//! `s3_deployed_notification_copies_within_its_bucket` and
//! `s3_notification_invokes_its_lambda_target` pin the same chain for a template written in
//! JSON whose role is granted `"*"`; what this one adds is the narrowly scoped deployment a
//! real template ships: the role's grant scoped to the bucket's own object ARNs, its logging
//! permission coming from the AWS-managed execution-role policy, the invoke grant scoped by
//! `SourceArn` and `SourceAccount`, and the whole template written in YAML with short-form
//! intrinsics and a parameter.
//!
//! The rows differ along what the rule generalises over: the template's spelling (YAML with
//! short-form intrinsics, JSON), where the bucket's name came from (a stack parameter, an
//! `Fn::Sub` over the stack's own name), how the role's permissions are granted (an inline
//! policy scoped to the bucket, the same plus an AWS-managed policy, an unscoped inline
//! policy), how the invoke grant is scoped (by source ARN and account, by neither), which
//! key-name filter the configuration carries (a prefix rule, a suffix rule, none at all),
//! which Region the whole stack is deployed into (the one a bucket with no location constraint
//! is created in, and one that is not it), whether the wiring is declared resource by resource
//! or through the Serverless Application Model's `S3` event source, which the transform has to
//! turn into the same two resources, and which service the handler's own call then reaches
//! (Amazon S3 again, or the state machine the same stack declares).
//!
//! Every request is dispatched through a BOXED future, and the whole suite runs on a thread
//! sized like the emulator's own boot path: the chain this drives — CloudFormation into S3,
//! S3's delivery into Lambda, the handler's own SDK call back into S3 — nests more future
//! than a default test thread's stack holds.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The Regions the rows deploy into: the Region a bucket is created in when the template
/// states no location constraint, and one that is not it — "buckets in Region us-east-1 have
/// a LocationConstraint of null" (`GetBucketLocationOutput$LocationConstraint`), so a
/// deployment outside that Region is the case where every ARN, every destination lookup and
/// every client of the chain names a Region of its own.
const HOME_REGION: &str = "us-east-1";
const OTHER_REGION: &str = "us-west-2";

/// The `StackStatus` a deployed pipeline must reach before it is wired.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The prefix the handler writes its marker under, and the prefix every row writes into. The
/// two differ so that the handler's own write cannot re-trigger the configuration.
const PROCESSED_PREFIX: &str = "processed/";
const WATCHED_PREFIX: &str = "incoming/";

/// How long this suite waits for work the clock owes — a stack's resources coming up, and the
/// asynchronous delivery of an object event — and how often it looks while waiting.
const WAIT: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`).
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The handler every row deploys: for each record it is handed it reads the object the event
/// named and writes one marker carrying what it read, into the bucket its own environment
/// names — the shape a deployed processor has.
const PROCESSING_HANDLER: &str = r#"import json
import os
import urllib.parse

import boto3


def handler(event, context):
    s3 = boto3.client("s3")
    processed = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        name = os.path.basename(key)
        body = s3.get_object(Bucket=bucket, Key=key)["Body"].read().decode("utf-8")
        s3.put_object(
            Bucket=os.environ["BUCKET_NAME"],
            Key="processed/" + name,
            Body=json.dumps({"key": key, "body": body}).encode("utf-8"),
        )
        processed.append(name)
    return {"processed": processed}
"#;

/// The handler the last row deploys: for the record it is handed it starts one execution of the
/// state machine its own environment names — the next hop of a deployed object pipeline, and
/// the case where the handler's own SDK call reaches a service other than the one that
/// published the event.
const STARTING_HANDLER: &str = r#"import json
import os
import urllib.parse

import boto3


def handler(event, context):
    states = boto3.client("stepfunctions")
    started = []
    for record in event["Records"]:
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        answer = states.start_execution(
            stateMachineArn=os.environ["STATE_MACHINE_ARN"],
            input=json.dumps({"key": key}),
        )
        started.append(answer["executionArn"])
    return {"started": started}
"#;

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread
/// runtime the deliveries are made on: a notification delivery is a task of its own, so it
/// runs on this same thread and needs the same depth.
fn driven<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()>>>
        + Send
        + 'static,
{
    ::std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = ::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(body());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// The emulator as it is ASSEMBLED, on one engine.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(region: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One CloudFormation call over its awsQuery wire, answered with the status and the XML.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(region, "cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One response of S3's REST surface.
struct Answer {
    status: u16,
    body: String,
}

/// Dispatch one request against S3's REST surface, path-style.
async fn s3(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Answer {
    let host = format!("s3.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(region, "s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One CloudWatch Logs call, as its status and parsed body. A group nothing has created yet is
/// a refusal a caller polls through, so the status travels back rather than being asserted.
async fn logs(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization(region, "logs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Step Functions call (`awsJson1_0`), as its status and parsed body.
async fn states(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("states.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
        .header("authorization", authorization(region, "states"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Everything the function logged, which is what a client reads to see why a deployed handler
/// produced no effect. Quoted in this suite's failures for exactly that reason.
async fn logged(router: &MultiServiceRouter, region: &str, function: &str) -> Vec<String> {
    let group = format!("/aws/lambda/{function}");
    let (status, answered) = logs(
        router,
        region,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    if status != 200 {
        return vec![format!("FilterLogEvents({group:?}) answered {status}: {answered}")];
    }
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| event["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every resource the stack reports a FAILED status for, with the reason it gave.
async fn reported_failures(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> Vec<String> {
    let (status, body) =
        cloudformation(router, region, "DescribeStackEvents", &[("StackName", stack)]).await;
    if status != 200 {
        return vec![format!("DescribeStackEvents answered {status}: {body}")];
    }
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let resource_status = elements(member, "ResourceStatus").into_iter().next()?;
            if !resource_status.contains("FAILED") {
                return None;
            }
            let logical = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default();
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical} {resource_status}: {reason}"))
        })
        .collect()
}

/// The `StackStatus` the stack reports right now, with the answer it was read from.
async fn stack_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let (status, body) =
        cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks({stack:?}) must answer 200, got {status} with {body}"
    );
    (opt_element(&body, "StackStatus").unwrap_or_default(), body)
}

/// Create `stack` from `template` with `parameters`, wait for `CREATE_COMPLETE`, and answer its
/// outputs. A stack that did not get there is quoted with every resource failure it reported.
async fn deployed(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
    template: &str,
    parameters: &[(&str, &str)],
) -> BTreeMap<String, String> {
    let mut members: Vec<(String, String)> = vec![
        ("StackName".to_string(), stack.to_string()),
        ("TemplateBody".to_string(), template.to_string()),
        ("Capabilities.member.1".to_string(), "CAPABILITY_IAM".to_string()),
        // A template declaring a transform is acknowledged with CAPABILITY_AUTO_EXPAND, and
        // the roles these templates declare with CAPABILITY_IAM.
        (
            "Capabilities.member.2".to_string(),
            "CAPABILITY_AUTO_EXPAND".to_string(),
        ),
    ];
    for (index, (key, value)) in parameters.iter().enumerate() {
        let position = index + 1;
        members.push((
            format!("Parameters.member.{position}.ParameterKey"),
            (*key).to_string(),
        ));
        members.push((
            format!("Parameters.member.{position}.ParameterValue"),
            (*value).to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = cloudformation(router, region, "CreateStack", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut reported = stack_status(router, region, stack).await;
    for _ in 0..looks {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
        reported = stack_status(router, region, stack).await;
    }
    if reported.0 != CREATE_COMPLETE {
        let failures = reported_failures(router, region, stack).await;
        panic!(
            "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {:?}; the failures it \
             reported: {failures:?}",
            reported.0
        );
    }
    outputs_of(&reported.1)
}

/// The keys `bucket` holds under [`PROCESSED_PREFIX`], read through `ListObjectsV2` — the probe
/// a client polling for the handler's effect makes.
async fn processed_keys(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    bucket: &str,
) -> Vec<String> {
    let answer = s3(
        router,
        region,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={}", form_encode(PROCESSED_PREFIX)),
        "",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: ListObjectsV2({bucket:?}) must answer 200, got {} with {}",
        answer.status, answer.body
    );
    elements(&answer.body, "Key")
}

/// Poll `ListObjectsV2` until the bucket holds a marker, and answer the keys it then holds.
async fn awaited_markers(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    bucket: &str,
) -> Vec<String> {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut found = processed_keys(router, region, case, bucket).await;
    for _ in 0..looks {
        if !found.is_empty() {
            return found;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
        found = processed_keys(router, region, case, bucket).await;
    }
    found
}

/// One line of `source` indented by `spaces`, which is how a block scalar carries a handler's
/// own source inside a YAML template.
fn indented(source: &str, spaces: usize) -> String {
    let padding = " ".repeat(spaces);
    source
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{padding}{line}")
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// The YAML pipeline: a parameterised bucket name, the execution role's logging permission
/// coming from the AWS-managed execution-role policy and its object permissions scoped to the
/// bucket's own ARNs, the invoke grant scoped by source ARN and account, and the notification
/// filtered by a prefix rule — the template a client ships, in the spelling it ships it in.
fn yaml_pipeline() -> String {
    let code = indented(PROCESSING_HANDLER, 10);
    format!(
        "AWSTemplateFormatVersion: '2010-09-09'
Parameters:
  DataBucketName:
    Type: String
Resources:
  HandlerRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: '2012-10-17'
        Statement:
          - Effect: Allow
            Principal:
              Service: lambda.amazonaws.com
            Action: sts:AssumeRole
      ManagedPolicyArns:
        - !Sub 'arn:${{AWS::Partition}}:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole'
      Policies:
        - PolicyName: process-the-objects
          PolicyDocument:
            Version: '2012-10-17'
            Statement:
              - Effect: Allow
                Action:
                  - s3:GetObject
                  - s3:PutObject
                Resource: !Sub 'arn:${{AWS::Partition}}:s3:::${{DataBucketName}}/*'
  Processor:
    Type: AWS::Lambda::Function
    Properties:
      Runtime: python3.12
      Handler: index.handler
      Role: !GetAtt HandlerRole.Arn
      Timeout: 30
      Environment:
        Variables:
          BUCKET_NAME: !Ref DataBucketName
      Code:
        ZipFile: |
{code}
  InvokePermission:
    Type: AWS::Lambda::Permission
    Properties:
      FunctionName: !GetAtt Processor.Arn
      Action: lambda:InvokeFunction
      Principal: s3.amazonaws.com
      SourceAccount: !Ref AWS::AccountId
      SourceArn: !Sub 'arn:${{AWS::Partition}}:s3:::${{DataBucketName}}'
  DataBucket:
    Type: AWS::S3::Bucket
    DependsOn: InvokePermission
    Properties:
      BucketName: !Ref DataBucketName
      NotificationConfiguration:
        LambdaConfigurations:
          - Event: 's3:ObjectCreated:*'
            Function: !GetAtt Processor.Arn
            Filter:
              S3Key:
                Rules:
                  - Name: prefix
                    Value: {WATCHED_PREFIX}
Outputs:
  BucketName:
    Value: !Ref DataBucket
  FunctionName:
    Value: !Ref Processor
"
    )
}

/// The SAM pipeline: the same wiring declared the way the Serverless Application Model
/// declares it — one `AWS::Serverless::Function` whose `Events` names an `S3` source on a
/// bucket of the same template, which the transform has to turn into that bucket's own
/// notification configuration plus the grant Amazon S3 needs on the function.
fn sam_pipeline(bucket: &str) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "DataBucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": bucket },
            },
            "Processor": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": 30,
                    "InlineCode": PROCESSING_HANDLER,
                    "Environment": { "Variables": { "BUCKET_NAME": bucket } },
                    "Policies": [{
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["s3:GetObject", "s3:PutObject", "logs:*"],
                            "Resource": "*",
                        }],
                    }],
                    "Events": {
                        "ObjectCreated": {
                            "Type": "S3",
                            "Properties": {
                                "Bucket": { "Ref": "DataBucket" },
                                "Events": "s3:ObjectCreated:*",
                                "Filter": {
                                    "S3Key": {
                                        "Rules": [
                                            { "Name": "prefix", "Value": WATCHED_PREFIX },
                                        ],
                                    },
                                },
                            },
                        },
                    },
                },
            },
        },
        "Outputs": {
            "BucketName": { "Value": { "Ref": "DataBucket" } },
            "FunctionName": { "Value": { "Ref": "Processor" } },
        },
    })
}

/// The JSON pipeline, whose bucket name is the stack's own name rather than a parameter.
/// `scoped_role` scopes the role's object permissions to that bucket (rather than to every
/// resource), `scoped_grant` scopes the invoke grant by source ARN and account, and `filter`
/// is the key-name filter the configuration carries.
fn json_pipeline(bucket: &str, scoped_role: bool, scoped_grant: bool, filter: Option<Value>) -> Value {
    let resource = if scoped_role {
        json!(format!("arn:aws:s3:::{bucket}/*"))
    } else {
        json!("*")
    };
    let mut permission = json!({
        "Type": "AWS::Lambda::Permission",
        "Properties": {
            "FunctionName": { "Fn::GetAtt": ["Processor", "Arn"] },
            "Action": "lambda:InvokeFunction",
            "Principal": "s3.amazonaws.com",
        },
    });
    if scoped_grant {
        permission["Properties"]["SourceArn"] = json!(format!("arn:aws:s3:::{bucket}"));
        permission["Properties"]["SourceAccount"] = json!({ "Ref": "AWS::AccountId" });
    }
    let mut configuration = json!({
        "Event": "s3:ObjectCreated:*",
        "Function": { "Fn::GetAtt": ["Processor", "Arn"] },
    });
    if let Some(filter) = filter {
        configuration["Filter"] = filter;
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "HandlerRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "process-the-objects",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": ["s3:GetObject", "s3:PutObject"],
                                    "Resource": resource,
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": "logs:*",
                                    "Resource": "*",
                                },
                            ],
                        },
                    }],
                },
            },
            "Processor": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                    "Timeout": 30,
                    "Environment": { "Variables": { "BUCKET_NAME": bucket } },
                    "Code": { "ZipFile": PROCESSING_HANDLER },
                },
            },
            "InvokePermission": permission,
            "DataBucket": {
                "Type": "AWS::S3::Bucket",
                // The configuration S3 records is validated against the function's own
                // policy, so the grant has to be in place before the bucket carries it.
                "DependsOn": "InvokePermission",
                "Properties": {
                    "BucketName": bucket,
                    "NotificationConfiguration": {
                        "LambdaConfigurations": [configuration],
                    },
                },
            },
        },
        "Outputs": {
            "BucketName": { "Value": { "Ref": "DataBucket" } },
            "FunctionName": { "Value": { "Ref": "Processor" } },
        },
    })
}

/// The pipeline whose handler starts an execution of a state machine the SAME stack declares:
/// the next hop of a deployed object pipeline, where the handler's own SDK call reaches a
/// service other than the one that published the event.
fn machine_pipeline(bucket: &str) -> Value {
    let mut template = json_pipeline(bucket, false, true, None);
    template["Resources"]["MachineRole"] = json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "states.amazonaws.com" },
                    "Action": "sts:AssumeRole",
                }],
            },
        },
    });
    template["Resources"]["Pipeline"] = json!({
        "Type": "AWS::StepFunctions::StateMachine",
        "Properties": {
            "RoleArn": { "Fn::GetAtt": ["MachineRole", "Arn"] },
            "DefinitionString": json!({
                "StartAt": "Record",
                "States": { "Record": { "Type": "Pass", "End": true } },
            })
            .to_string(),
        },
    });
    // The handler calls Step Functions rather than Amazon S3, so that is what its own code and
    // its role's grant are: "Ref returns the ARN of the state machine", which is what
    // `StartExecution` addresses.
    template["Resources"]["Processor"]["Properties"]["Code"]["ZipFile"] =
        json!(STARTING_HANDLER);
    template["Resources"]["Processor"]["Properties"]["Environment"]["Variables"]
        ["STATE_MACHINE_ARN"] = json!({ "Ref": "Pipeline" });
    template["Resources"]["HandlerRole"]["Properties"]["Policies"][0]["PolicyDocument"]
        ["Statement"][0]["Action"] = json!(["states:StartExecution"]);
    template["Outputs"]["StateMachineArn"] = json!({ "Value": { "Ref": "Pipeline" } });
    template
}

/// One row's deployment: where it is deployed, what it deploys, and the object it writes.
struct Deployment<'a> {
    region: &'a str,
    case: &'a str,
    stack: &'a str,
    template: &'a str,
    parameters: &'a [(&'a str, &'a str)],
    /// The key written into the deployed bucket.
    key: &'a str,
    /// The bytes written at that key, which the handler reads back into its marker.
    body: &'a str,
}

/// Deploy one row's stack, write the object it names, and assert the marker the handler owes —
/// quoting what the function logged when none appears, because that is where a deployed
/// handler says why it produced nothing.
async fn assert_pipeline_runs(router: &MultiServiceRouter, row: &Deployment<'_>) {
    let Deployment {
        region,
        case,
        stack,
        template,
        parameters,
        key,
        body,
    } = *row;
    let outputs = deployed(router, region, case, stack, template, parameters).await;
    let bucket = outputs
        .get("BucketName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the bucket's name"));
    let function = outputs
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the function's name"));

    let answer = s3(router, region, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );

    let markers = awaited_markers(router, region, case, &bucket).await;
    let name = key.rsplit('/').next().unwrap_or(key);
    let wanted = format!("{PROCESSED_PREFIX}{name}");
    if !markers.contains(&wanted) {
        let lines = logged(router, region, &function).await;
        panic!(
            "case {case:?}: the write of {key:?} must run the deployed function, which writes \
             {wanted:?} into {bucket:?} — got {markers:?}; the function logged {lines:?}"
        );
    }

    let read = s3(router, region, "GET", &format!("/{bucket}/{wanted}"), "").await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetObject({wanted:?}) must succeed, got {} with {}",
        read.status, read.body
    );
    let marker: Value = serde_json::from_str(&read.body).unwrap_or(Value::Null);
    assert_eq!(
        marker["key"].as_str(),
        Some(key),
        "case {case:?}: the marker must name the key the event carried, got {}",
        read.body
    );
    assert_eq!(
        marker["body"].as_str(),
        Some(body),
        "case {case:?}: the marker must carry the bytes the handler read back, got {}",
        read.body
    );
}

/// The template a client ships: YAML with short-form intrinsics, a parameterised bucket name,
/// an AWS-managed policy beside an inline policy scoped to the bucket, an invoke grant scoped
/// by source ARN and account, and a prefix-filtered configuration.
#[test]
fn a_yaml_deployed_scoped_pipeline_runs_its_handler() {
    driven("cfn-yaml-s3-notification-pipeline", || {
        Box::pin(async {
            let router = build_router();
            let stack = "cfn-yaml-notify-pipeline";
            let bucket = format!("{stack}-data");
            assert_pipeline_runs(
                &router,
                &Deployment {
                    region: HOME_REGION,
                    case: "a YAML template whose role and grant are scoped to the bucket",
                    stack,
                    template: &yaml_pipeline(),
                    parameters: &[("DataBucketName", &bucket)],
                    key: "incoming/sample.txt",
                    body: "the object the notification named",
                },
            )
            .await;
        })
    });
}

/// The same pipeline in JSON, with the role's object permissions scoped to the bucket and the
/// grant left unscoped, under a suffix-filtered configuration.
#[test]
fn a_scoped_execution_role_may_write_the_handlers_marker() {
    driven("cfn-json-scoped-role-pipeline", || {
        Box::pin(async {
            let router = build_router();
            let stack = "cfn-scoped-role-pipeline";
            let bucket = format!("{stack}-data");
            let filter = json!({
                "S3Key": { "Rules": [{ "Name": "suffix", "Value": ".csv" }] },
            });
            let template = json_pipeline(&bucket, true, false, Some(filter));
            assert_pipeline_runs(
                &router,
                &Deployment {
                    region: HOME_REGION,
                    case: "an inline policy scoped to the bucket's own object ARNs",
                    stack,
                    template: &template.to_string(),
                    parameters: &[],
                    key: "incoming/report.csv",
                    body: "quarterly,data",
                },
            )
            .await;
        })
    });
}

/// And in JSON with the grant scoped by source ARN and account while the role is granted every
/// resource, under a configuration carrying no filter at all.
#[test]
fn a_source_scoped_invoke_grant_admits_the_buckets_events() {
    driven("cfn-json-scoped-grant-pipeline", || {
        Box::pin(async {
            let router = build_router();
            let stack = "cfn-scoped-grant-pipeline";
            let bucket = format!("{stack}-data");
            let template = json_pipeline(&bucket, false, true, None);
            assert_pipeline_runs(
                &router,
                &Deployment {
                    region: HOME_REGION,
                    case: "an invoke grant scoped by source ARN and account",
                    stack,
                    template: &template.to_string(),
                    parameters: &[],
                    key: "incoming/sample.json",
                    body: "{\"rows\":3}",
                },
            )
            .await;
        })
    });
}

/// And the same pipeline deployed OUTSIDE the Region a location-constraint-free bucket is
/// created in: every ARN the stack publishes, the destination the configuration resolves and
/// every client of the chain then names that other Region.
#[test]
fn a_pipeline_deployed_outside_the_home_region_runs_its_handler() {
    driven("cfn-other-region-notification-pipeline", || {
        Box::pin(async {
            let router = build_router();
            let stack = "cfn-other-region-pipeline";
            let bucket = format!("{stack}-data");
            let filter = json!({
                "S3Key": { "Rules": [{ "Name": "prefix", "Value": WATCHED_PREFIX }] },
            });
            let template = json_pipeline(&bucket, true, true, Some(filter));
            assert_pipeline_runs(
                &router,
                &Deployment {
                    region: OTHER_REGION,
                    case: "a stack deployed in a Region of its own",
                    stack,
                    template: &template.to_string(),
                    parameters: &[],
                    key: "incoming/sample.txt",
                    body: "the object a bucket outside the home Region holds",
                },
            )
            .await;
        })
    });
}

/// And the same pipeline declared the way the Serverless Application Model declares it: one
/// function whose `Events` names an `S3` source on a bucket of the same template. The
/// configuration the transform owes is read back off the BUCKET through Amazon S3's own API
/// before the delivery is asserted, so a transform that expanded nothing is told apart from a
/// delivery that did not happen.
#[test]
fn a_sam_object_event_source_deploys_the_buckets_notification() {
    driven("cfn-sam-object-event-pipeline", || {
        Box::pin(async {
            let case = "a SAM function whose Events name an S3 source";
            let router = build_router();
            let stack = "cfn-sam-event-pipeline";
            let bucket = format!("{stack}-data");
            let template = sam_pipeline(&bucket).to_string();
            let outputs = deployed(&router, HOME_REGION, case, stack, &template, &[]).await;
            let function = outputs
                .get("FunctionName")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output the function"));

            // What the BUCKET carries, read through Amazon S3's own API: the function the
            // event source named, the event class it listed and the prefix rule it stated.
            let answer = s3(
                &router,
                HOME_REGION,
                "GET",
                &format!("/{bucket}?notification"),
                "",
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: GetBucketNotificationConfiguration must answer 200, got {} \
                 with {}",
                answer.status, answer.body
            );
            for expected in [function.as_str(), "s3:ObjectCreated:*", WATCHED_PREFIX] {
                assert!(
                    answer.body.contains(expected),
                    "case {case:?}: expected the bucket's configuration to carry {expected:?}, \
                     got {}",
                    answer.body
                );
            }

            // And the delivery that configuration owes: the write the prefix rule admits runs
            // the deployed function, whose marker reads back.
            let key = "incoming/sample.txt";
            let written = "the object a SAM-declared event source names";
            let answer = s3(
                &router,
                HOME_REGION,
                "PUT",
                &format!("/{bucket}/{key}"),
                written,
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: PutObject({key:?}) must succeed, got {} with {}",
                answer.status, answer.body
            );
            let markers = awaited_markers(&router, HOME_REGION, case, &bucket).await;
            let wanted = format!("{PROCESSED_PREFIX}sample.txt");
            if !markers.contains(&wanted) {
                let lines = logged(&router, HOME_REGION, &function).await;
                panic!(
                    "case {case:?}: the write of {key:?} must run the function the event source \
                     named, which writes {wanted:?} — got {markers:?}; the function logged \
                     {lines:?}"
                );
            }
        })
    });
}

/// The next hop: the notification-driven handler starts an execution of the state machine the
/// same stack declares, which a client reads back through `ListExecutions` and
/// `DescribeExecution`. The row differs from the ones above in WHICH service the handler's own
/// call reaches, which the rule generalises over the same way.
#[test]
fn a_deployed_pipelines_handler_starts_its_state_machine() {
    driven("cfn-notification-starts-a-state-machine", || {
        Box::pin(async {
            let case = "a deployed pipeline whose handler starts an execution";
            let router = build_router();
            let stack = "cfn-notify-sfn-pipeline";
            let bucket = format!("{stack}-data");
            let template = machine_pipeline(&bucket).to_string();
            let outputs = deployed(&router, HOME_REGION, case, stack, &template, &[]).await;
            let function = outputs
                .get("FunctionName")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output the function"));
            let machine = outputs
                .get("StateMachineArn")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output the machine"));

            let key = "incoming/started.txt";
            let answer = s3(
                &router,
                HOME_REGION,
                "PUT",
                &format!("/{bucket}/{key}"),
                "the object the pipeline starts an execution for",
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: PutObject({key:?}) must succeed, got {} with {}",
                answer.status, answer.body
            );

            let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
            let mut executions = Value::Null;
            for _ in 0..looks {
                let (status, answered) = states(
                    &router,
                    HOME_REGION,
                    "ListExecutions",
                    &json!({ "stateMachineArn": machine }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: ListExecutions must answer 200, got {status} with {answered}"
                );
                executions = answered["executions"].clone();
                if executions.as_array().is_some_and(|listed| !listed.is_empty()) {
                    break;
                }
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
            let listed = executions.as_array().cloned().unwrap_or_default();
            if listed.len() != 1 {
                let lines = logged(&router, HOME_REGION, &function).await;
                panic!(
                    "case {case:?}: the write of {key:?} must run the handler, which starts one \
                     execution of {machine:?} — got {executions}; the function logged {lines:?}"
                );
            }
            let arn = listed[0]["executionArn"].as_str().unwrap_or_default();
            let (status, described) = states(
                &router,
                HOME_REGION,
                "DescribeExecution",
                &json!({ "executionArn": arn }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeExecution must answer 200, got {status} with {described}"
            );
            let input: Value = serde_json::from_str(described["input"].as_str().unwrap_or("null"))
                .unwrap_or(Value::Null);
            assert_eq!(
                input["key"].as_str(),
                Some(key),
                "case {case:?}: the execution must carry the input the handler sent, got \
                 {described}"
            );
        })
    });
}
