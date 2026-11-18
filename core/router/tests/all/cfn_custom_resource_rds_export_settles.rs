// SPDX-License-Identifier: CC0-1.0

//! A stack whose `Custom::` resource's provider drives an Amazon RDS SNAPSHOT EXPORT to
//! Amazon S3, end to end across the services that deployment spans.
//!
//! RULE under test: **a deployment of a Lambda-backed `Custom::<Type>` resource always reaches
//! a TERMINAL stack status, and which one is decided by the answer its provider PUTs to the
//! request's `ResponseURL` — `CREATE_COMPLETE` when the provider answers `SUCCESS`, with the
//! `Data` it published resolving through `Fn::GetAtt` into the stack's Outputs, and
//! `ROLLBACK_COMPLETE` carrying that provider's own reason on the custom resource's
//! `CREATE_FAILED` event when it answers `FAILED` — because the call the provider makes into
//! another service always ANSWERS: an export Amazon RDS then HOLDS and reports, or one of that
//! operation's modelled faults. Neither leg may leave the client polling: a stack that never
//! settles gives a deploy nothing to branch on.**
//!
//! Only a cross-service suite can pin this: the deployment is served by six services over ONE
//! engine, exactly as `core/router/src/lib.rs` mounts them — CloudFormation sends the provider
//! its request and settles the stack on the answer, AWS Lambda RUNS the provider, Amazon RDS
//! owns the snapshot and the export task, IAM the roles the export and the provider act under,
//! KMS the key the exported data is encrypted under, and Amazon S3 the objects the export
//! writes and the bucket it writes them into.
//!
//! The rows differ along what the rule generalises over: whether the provider's call is SERVED
//! or REFUSED, and — for a refusal — which class of the operation's modelled faults it is
//! refused with (a source this account holds nothing under, a role the export principal cannot
//! assume, a destination the object store holds nothing under), each named by the
//! `awsQueryError` code its shape carries
//! (`smithy_query(query="operation",operation="StartExportTask")`). What SENTENCE each of those
//! faults is worded with is the owning service's own suites' subject; what this suite asserts
//! is that the refusal REACHED the provider, with its code, and that the stack settled on it.
//!
//! Nothing here writes to a `ResponseURL` and nothing here starts an export: the provider's own
//! code makes that call through the SDK its runtime supplies, so what the stack settles on is
//! what the emulated services really answered. Every wait is on the CONDITION the next step
//! needs, looked at again until it holds or a deadline far beyond this emulator's own spans
//! passes, so a loaded host cannot decide any outcome here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, element, form_encode, opt_element, outputs_of};
// The dispatch, the stack poll and the failure-event reader this crate already writes once for
// its stack suites, reused rather than copied: the request wire
// ([`crate::cloudformation_completed_stack_output_values::dispatch`]), the `CreateStack` that
// asserts its own acceptance, the poll for a status that is no longer `*_IN_PROGRESS`, and the
// reasons a deployment recorded for the resources whose own operation did not succeed.
use crate::cloudformation_completed_stack_output_values::{
    authorization, create_stack, dispatch, reported_failures, settled_status, Answered, ACCOUNT,
    CREATE_COMPLETE, POLL, REGION, SETTLE_DEADLINE,
};

/// The status a deployment whose provider answered `FAILED` has to reach: the create fails and
/// the stack rolls back, which is where a rolled-back create settles.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The logical ids every template below declares.
const BUCKET: &str = "ExportBucket";
const KEY: &str = "ExportKey";
const EXPORT_ROLE: &str = "ExportRole";
const TRIGGER_ROLE: &str = "TriggerRole";
const TRIGGER_FUNCTION: &str = "TriggerFunction";
const TRIGGER: &str = "TriggerResource";

/// The master user every DB instance below is created with, and its password — test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The statuses an export task can have reached once nothing more is owed on it
/// (`ExportTask$Status`), which is what a client polling one waits for.
const TERMINAL_EXPORT_STATUSES: [&str; 3] = ["COMPLETE", "FAILED", "CANCELED"];

/// The provider every row deploys: it starts the export its properties describe through the
/// AWS SDK its runtime supplies, and PUTs its own answer to the request's `ResponseURL` —
/// `SUCCESS` carrying what the service answered about the export it started, or `FAILED`
/// carrying the code and sentence the service refused the call with. A provider that answers
/// either way is what lets the stack settle either way.
const PROVIDER_SOURCE: &str = r#"
import json
import urllib.request

import boto3
import botocore.exceptions


def answer(event, status, reason, data):
    document = json.dumps(
        {
            "Status": status,
            "Reason": reason,
            "PhysicalResourceId": event["ResourceProperties"]["ExportTaskIdentifier"],
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": data,
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=document,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()


def handler(event, context):
    properties = event["ResourceProperties"]
    if event["RequestType"] == "Delete":
        answer(event, "SUCCESS", "", {})
        return {}
    client = boto3.client("rds")
    try:
        started = client.start_export_task(
            ExportTaskIdentifier=properties["ExportTaskIdentifier"],
            SourceArn=properties["SourceArn"],
            S3BucketName=properties["BucketName"],
            S3Prefix=properties["ExportTaskIdentifier"],
            IamRoleArn=properties["ExportRoleArn"],
            KmsKeyId=properties["KmsKeyId"],
        )
    except botocore.exceptions.ClientError as error:
        refusal = error.response["Error"]
        answer(
            event,
            "FAILED",
            "StartExportTask was refused: %s: %s" % (refusal["Code"], refusal["Message"]),
            {},
        )
        return {}
    answer(
        event,
        "SUCCESS",
        "",
        {
            "S3Arn": "arn:aws:s3:::" + started["S3Bucket"],
            "ExportStatus": started["Status"],
        },
    )
    return {}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the provider CloudFormation invokes is the
/// function the row's own stack created, and the export it starts is read back out of the
/// service that owns it.
///
/// The whole router rather than the named services the sibling suite mounts: this chain spans
/// CloudFormation, AWS Lambda, Amazon RDS, IAM, KMS and Amazon S3 at once.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so
    // the router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router
}

/// One awsQuery call to the service `signing_name`, as an SDK client sends one.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> Answered {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("{signing_name}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body),
    )
    .await
}

/// One Amazon RDS call.
async fn rds(router: &MultiServiceRouter, action: &str, params: &[(&str, String)]) -> Answered {
    query(router, "rds", "2014-10-31", action, params).await
}

/// `[(name, value.to_string()), ..]` — the parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Read the Amazon RDS `action` until the element `tag` of its answer carries one of `wanted`,
/// and hand back the answer it then read. The wait is on that CONDITION: the call is made again
/// until it holds or [`SETTLE_DEADLINE`] passes.
async fn read_rds_until(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, String)],
    tag: &str,
    wanted: &[&str],
) -> String {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = served(case, rds(router, action, params).await);
        if wanted.contains(&element(&body, tag).as_str()) {
            return body;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {action} to report {tag} as one of {wanted:?}, got {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Assert `answered` was served, and hand back its body.
fn served(case: &str, answered: Answered) -> String {
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: the call must be served, got HTTP {} with {}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// The key policy the stack's key declares: the account's own root, which is how a key
/// delegates every decision about itself to IAM.
fn root_key_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "EnableRootPermissions",
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "kms:*",
            "Resource": "*",
        }],
    })
}

/// A trust policy letting `principal` assume the role it is declared on.
fn trust_policy(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": principal},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The identity policy the EXPORT role carries, as the user guide's export role carries it:
/// the Amazon S3 calls an export of a snapshot performs on its destination, and the two KMS
/// calls it performs on the key the exported data is encrypted under.
fn export_role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "s3:PutObject*",
                    "s3:GetObject*",
                    "s3:ListBucket",
                    "s3:DeleteObject*",
                    "s3:GetBucketLocation",
                ],
                "Resource": [
                    {"Fn::GetAtt": [BUCKET, "Arn"]},
                    {"Fn::Join": ["", [{"Fn::GetAtt": [BUCKET, "Arn"]}, "/*"]]},
                ],
            },
            {
                "Effect": "Allow",
                "Action": [
                    "kms:CreateGrant",
                    "kms:DescribeKey",
                    "kms:Encrypt",
                    "kms:Decrypt",
                    "kms:GenerateDataKey*",
                ],
                "Resource": {"Fn::GetAtt": [KEY, "Arn"]},
            },
        ],
    })
}

/// The identity policy the PROVIDER's execution role carries: the export call its code makes,
/// and the pass-role that call's own role reference needs.
fn trigger_role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["rds:StartExportTask", "rds:DescribeExportTasks", "iam:PassRole"],
            "Resource": "*",
        }],
    })
}

/// Which role the trigger tells the export to run under.
#[derive(Clone, Copy)]
enum Role {
    /// The stack's export role, which trusts the export service principal.
    TheExportRole,
    /// The provider's own execution role, which this account HOLDS and which trusts
    /// `lambda.amazonaws.com` only — a role the export principal cannot assume.
    TheProvidersOwnRole,
}

impl Role {
    fn reference(self) -> Value {
        match self {
            Self::TheExportRole => json!({"Fn::GetAtt": [EXPORT_ROLE, "Arn"]}),
            Self::TheProvidersOwnRole => json!({"Fn::GetAtt": [TRIGGER_ROLE, "Arn"]}),
        }
    }
}

/// What the trigger's properties name, which is what the provider's call carries.
struct Trigger {
    /// The export task the provider starts, which is also the prefix it writes under.
    task: String,
    /// The snapshot it exports.
    source: String,
    /// The destination bucket: the stack's own, or a name nothing holds.
    bucket: Value,
    role: Role,
}

/// The template one row deploys: the destination bucket, the key the export is encrypted
/// under, the role the export runs under, the provider's execution role, the provider itself,
/// and the `Custom::` resource it backs — plus the Outputs a client reads the deployment's
/// answer off.
fn template(trigger: &Trigger) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            BUCKET: {"Type": "AWS::S3::Bucket", "Properties": {}},
            KEY: {
                "Type": "AWS::KMS::Key",
                "Properties": {
                    "Description": "the export's own key",
                    "KeyPolicy": root_key_policy(),
                },
            },
            EXPORT_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy("export.rds.amazonaws.com"),
                    "Policies": [{
                        "PolicyName": "export-to-s3",
                        "PolicyDocument": export_role_policy(),
                    }],
                },
            },
            TRIGGER_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy("lambda.amazonaws.com"),
                    "Policies": [{
                        "PolicyName": "start-the-export",
                        "PolicyDocument": trigger_role_policy(),
                    }],
                },
            },
            TRIGGER_FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"ZipFile": PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [TRIGGER_ROLE, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            TRIGGER: {
                "Type": "Custom::RdsExportTrigger",
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [TRIGGER_FUNCTION, "Arn"]},
                    "ExportTaskIdentifier": trigger.task,
                    "SourceArn": trigger.source,
                    "BucketName": trigger.bucket,
                    "ExportRoleArn": trigger.role.reference(),
                    "KmsKeyId": {"Ref": KEY},
                },
            },
        },
        "Outputs": {
            "BucketName": {"Value": {"Ref": BUCKET}},
            "ResolvedS3Arn": {"Value": {"Fn::GetAtt": [TRIGGER, "S3Arn"]}},
        },
    })
    .to_string()
}

/// Deploy `template` as `stack` and answer the terminal status it reached with the answer it
/// was read from — through this crate's own `CreateStack` (which carries the capability a
/// template creating IAM resources is deployed with, and asserts the submission itself was
/// accepted) and its own poll for a status that is no longer `*_IN_PROGRESS`.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: String,
) -> (String, String) {
    create_stack(router, case, stack, &template, &[]).await;
    settled_status(router, stack).await
}

/// The source every row exports: a DB instance polled to `available`, snapshotted, and the
/// snapshot polled to `available`. Its ARN — the one Amazon RDS minted for it — is what a
/// trigger's `SourceArn` names.
async fn available_snapshot(router: &MultiServiceRouter, instance: &str, snapshot: &str) -> String {
    let case = "the source the export reads";
    let created = rds(
        router,
        "CreateDBInstance",
        &params![
            "DBInstanceIdentifier" => instance,
            "Engine" => "mysql",
            "DBInstanceClass" => "db.t3.micro",
            "AllocatedStorage" => 20,
            "MasterUsername" => MASTER_USER,
            "MasterUserPassword" => MASTER_PASSWORD,
            "BackupRetentionPeriod" => 1,
        ],
    )
    .await;
    served(case, created);
    read_rds_until(
        router,
        case,
        "DescribeDBInstances",
        &params!["DBInstanceIdentifier" => instance],
        "DBInstanceStatus",
        &["available"],
    )
    .await;
    let snapshotted = rds(
        router,
        "CreateDBSnapshot",
        &params![
            "DBInstanceIdentifier" => instance,
            "DBSnapshotIdentifier" => snapshot,
        ],
    )
    .await;
    served(case, snapshotted);
    let described = read_rds_until(
        router,
        case,
        "DescribeDBSnapshots",
        &params!["DBSnapshotIdentifier" => snapshot],
        "Status",
        &["available"],
    )
    .await;
    let arn = element(&described, "DBSnapshotArn");
    assert!(
        arn.contains(":snapshot:"),
        "the service reports the snapshot's own ARN, got {arn:?} in {described}"
    );
    arn
}

/// The export task Amazon RDS reports under `task`, as the `member -> value` map of the ONE it
/// holds — empty where it holds none, and a panic where it answered with more than one, since
/// an identifier "narrows the report" to the task it names and a second entry would mean the
/// assertions below were comparing the wrong task.
///
/// The list's entries are each wrapped in the member name the model gives them
/// (`ExportTasksList$member` carries `xmlName` `ExportTask`), so the members are read from the
/// one block that carries them.
async fn reported_export_task(
    router: &MultiServiceRouter,
    case: &str,
    task: &str,
) -> BTreeMap<String, String> {
    let body = served(
        case,
        rds(
            router,
            "DescribeExportTasks",
            &params!["ExportTaskIdentifier" => task],
        )
        .await,
    );
    let entries = blocks_of(&body, "ExportTask");
    assert!(
        entries.len() <= 1,
        "case {case:?}: a report narrowed to {task:?} carries at most that one task, got \
         {} in {body}",
        entries.len()
    );
    let Some(entry) = entries.first() else {
        return BTreeMap::new();
    };
    [
        "ExportTaskIdentifier",
        "Status",
        "S3Bucket",
        "S3Prefix",
        "FailureCause",
    ]
    .into_iter()
    .filter_map(|tag| {
        let value = opt_element(entry, tag)?;
        Some((tag.to_string(), value))
    })
    .collect()
}

/// RULE (the served leg): a stack whose `Custom::` resource's provider starts an export that
/// Amazon RDS SERVES settles at `CREATE_COMPLETE`, the `Data` that provider published is what
/// `Fn::GetAtt` resolves into the stack's Outputs, and the export the provider started is one
/// Amazon RDS really holds — it reaches a terminal status of its own and the objects it wrote
/// are in the destination bucket.
#[tokio::test]
async fn a_custom_resource_backed_export_completes_and_the_export_it_started_runs() {
    let case = "a provider whose export call is served";
    let router = emulator();
    let snapshot_arn = available_snapshot(&router, "cfn-export-source", "cfn-export-snapshot").await;
    let task = "cfn-export-task".to_string();
    let (settled, described) = deployed(
        &router,
        case,
        "cfn-rds-export-complete",
        template(&Trigger {
            task: task.clone(),
            source: snapshot_arn,
            bucket: json!({"Ref": BUCKET}),
            role: Role::TheExportRole,
        }),
    )
    .await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(&router, "cfn-rds-export-complete").await;
        panic!(
            "case {case:?}: expected the stack to settle at {CREATE_COMPLETE}, got {settled:?}; \
             the failures it reported: {failures:?}; last answer {described}"
        );
    }

    // 1. The stack's Outputs: the bucket it created, and the attribute the PROVIDER published,
    //    which is the destination the service answered its call with.
    let outputs = outputs_of(&described);
    let bucket = outputs
        .get("BucketName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack publishes its bucket, got {outputs:?}"));
    assert_eq!(
        outputs.get("ResolvedS3Arn").map(String::as_str),
        Some(format!("arn:aws:s3:::{bucket}").as_str()),
        "case {case:?}: expected Fn::GetAtt of the published attribute to resolve to the ARN of \
         the bucket the export writes into, got {outputs:?}"
    );

    // 2. The export the provider started is one Amazon RDS HOLDS, and it reaches a terminal
    //    status of its own: a client that polls it gets an answer.
    let described = read_rds_until(
        &router,
        case,
        "DescribeExportTasks",
        &params!["ExportTaskIdentifier" => task],
        "Status",
        &TERMINAL_EXPORT_STATUSES,
    )
    .await;
    assert_eq!(
        element(&described, "Status"),
        "COMPLETE",
        "case {case:?}: expected the export the provider started to settle at COMPLETE, got \
         {described}"
    );
    let reported = reported_export_task(&router, case, &task).await;
    assert_eq!(
        (
            reported.get("ExportTaskIdentifier").map(String::as_str),
            reported.get("S3Bucket").map(String::as_str),
            reported.get("S3Prefix").map(String::as_str),
            reported.get("FailureCause").map(String::as_str),
        ),
        (Some(task.as_str()), Some(bucket.as_str()), Some(task.as_str()), None),
        "case {case:?}: expected the stored export to report the identifier, destination and \
         prefix the provider's call named and no failure cause, got {reported:?}"
    );

    // 3. ...and the objects that export wrote are in the destination bucket, read through the
    //    service that owns them.
    let listed = served(
        case,
        dispatch(
            &router,
            "GET",
            &format!("s3.{REGION}.amazonaws.com"),
            &format!("/{bucket}?list-type=2&prefix={task}%2F"),
            &[("authorization", &authorization("s3"))],
            Bytes::new(),
        )
        .await,
    );
    // `ListObjectsV2` reports `KeyCount` as "the number of keys returned with this request", so
    // an answer carrying none, or one carrying something that is no count, is a wire-shape
    // regression rather than an empty prefix: it is reported as itself.
    let reported_count = opt_element(&listed, "KeyCount");
    let key_count: i64 = reported_count
        .as_deref()
        .unwrap_or_else(|| panic!("case {case:?}: expected the listing to report KeyCount, got {listed}"))
        .parse()
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected KeyCount to be a count, got {reported_count:?} \
                 ({error}) in {listed}"
            )
        });
    assert!(
        key_count > 0,
        "case {case:?}: expected the export's own prefix to hold the objects it wrote, got \
         KeyCount {key_count} in {listed}"
    );
}

/// One refusal row: how the trigger is declared, and the `awsQueryError` code the fault that
/// declaration provokes carries.
struct Row {
    case: &'static str,
    stack: &'static str,
    task: &'static str,
    /// Whether the trigger names the snapshot the account holds, or one it holds nothing under.
    held_source: bool,
    /// Whether the trigger names the bucket the stack created, or one nothing holds.
    held_bucket: bool,
    role: Role,
    /// The code the refusal reaching the provider must carry: the `awsQueryError` code of the
    /// fault's own shape (`smithy_query(query="operation",operation="StartExportTask")`).
    code: &'static str,
}

/// One row per class of refusal the provider's call can meet, each provoked by a different
/// part of what the trigger names.
const ROWS: &[Row] = &[
    Row {
        case: "a source this account holds nothing under",
        stack: "cfn-rds-export-absent-source",
        task: "cfn-export-absent-source",
        held_source: false,
        held_bucket: true,
        role: Role::TheExportRole,
        code: "DBSnapshotNotFound",
    },
    Row {
        // A role this account HOLDS whose trust policy admits only another principal is the
        // not-found class rather than the missing-permissions one: that fault's own sentence
        // reports the export principal not being allowed to assume the role OR the role not
        // existing, so the two conditions share it, and the permissions class is for a role the
        // export CAN assume but that cannot make one of the calls the export makes on the
        // destination.
        case: "a role the export principal cannot assume",
        stack: "cfn-rds-export-unassumable-role",
        task: "cfn-export-unassumable-role",
        held_source: true,
        held_bucket: true,
        role: Role::TheProvidersOwnRole,
        code: "IamRoleNotFound",
    },
    Row {
        case: "a destination the object store holds nothing under",
        stack: "cfn-rds-export-absent-bucket",
        task: "cfn-export-absent-bucket",
        held_source: true,
        held_bucket: false,
        role: Role::TheExportRole,
        code: "InvalidS3BucketFault",
    },
];

/// RULE (the refused leg): a stack whose `Custom::` resource's provider is REFUSED by the
/// service it calls still settles — at `ROLLBACK_COMPLETE`, with the provider's own reason on
/// the custom resource's `CREATE_FAILED` event — and the export that was refused is one
/// Amazon RDS holds nothing of, so nothing downstream reports a success that never happened.
#[tokio::test]
async fn a_refused_provider_call_rolls_the_stack_back_with_the_refusal_it_was_given() {
    let router = emulator();
    let snapshot_arn =
        available_snapshot(&router, "cfn-refusal-source", "cfn-refusal-snapshot").await;
    // A snapshot ARN of this Region that names nothing: the form `SourceArn` takes, under a
    // name this account holds no snapshot under.
    let absent_source =
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:cfn-export-no-such-snapshot");

    for row in ROWS {
        let case = row.case;
        let (settled, described) = deployed(
            &router,
            case,
            row.stack,
            template(&Trigger {
                task: row.task.to_string(),
                source: match row.held_source {
                    true => snapshot_arn.clone(),
                    false => absent_source.clone(),
                },
                bucket: match row.held_bucket {
                    true => json!({"Ref": BUCKET}),
                    false => json!("cfn-export-no-such-bucket"),
                },
                role: row.role,
            }),
        )
        .await;

        // 1. It settles, and on the status a failed create rolls back to — not on a status it
        //    is still working through, and not on no answer at all.
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE}, got \
             {settled:?} with {described}"
        );

        // 2. The reason the custom resource's failure carries is the PROVIDER's, which is how
        //    the refusal the service gave reaches the client that deployed the stack. The
        //    shared reader reports each failure as `<logical id> <status>: <reason>`, so the
        //    one this row asserts on is the custom resource's own.
        let failures = reported_failures(&router, row.stack).await;
        assert!(
            failures
                .iter()
                .any(|failure| failure.starts_with(TRIGGER) && failure.contains(row.code)),
            "case {case:?}: expected the custom resource's own failure to carry the code the \
             refusal was answered with ({}), got {failures:?}",
            row.code
        );

        // 3. And nothing was started: a refused export is one the owning service holds no
        //    record of, so a client reading it back is told there is none.
        let reported = reported_export_task(&router, case, row.task).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: expected the refused export to leave no export task behind, got \
             {reported:?}"
        );
    }
}
