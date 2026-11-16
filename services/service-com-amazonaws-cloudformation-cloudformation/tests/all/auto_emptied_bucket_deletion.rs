// SPDX-License-Identifier: CC0-1.0

//! What a stack TEARDOWN does with the resources an unfinished removal held back when
//! another resource's deletion is REFUSED, end to end over the awsQuery wire.
//!
//! RULE under test: **a teardown reports its own failure only once it has asked about every
//! resource it still can. "CloudFormation deletes the resources in reverse order", so a
//! removal the owning service — or a custom resource's PROVIDER — has only ACCEPTED leaves
//! what that resource depends on alone until the answer comes; those resources are asked
//! about when it does, and the refusal of a DIFFERENT resource holds back only the resources
//! the refused one needs. So a sibling the refused resource has nothing to do with is
//! deleted, and the effects the provider's own `Delete` performed — a bucket it emptied — are
//! what the deletion that follows acts on.**
//!
//! Why it matters: this is the shape every `autoDeleteObjects` bucket is destroyed in. The
//! bucket's contents are removed by the provider of the custom resource that depends on it,
//! and the bucket's own deletion is the step after that answer. A teardown that reported its
//! failure the moment another resource was refused left that bucket undeleted, under the
//! status its CREATE had left it in, with nothing that would ever ask about it again.
//!
//! The rows differ along what the rule generalises over: WHERE the refused resource sits in
//! the walk relative to the one still being removed (declared before it and after it),
//! whether the refused resource NEEDS a resource of its own (which must then be left exactly
//! as it is), and whether anything is refused at all — the control row, where the same stack
//! deletes completely.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_stack, member, router_with_engine, xml_blocks, xml_text};
use crate::stack_resource_provisioning::s3;
use crate::update_rollback_recovery::parameter_value;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// How long a case waits for a stack operation to settle, and how often it looks: the
/// provider is invoked asynchronously and answers out of band, so the status is polled
/// exactly as a client polls it.
const POLL_ATTEMPTS: usize = 400;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The provider the CDK synthesises for a bucket that empties itself: on `Delete` it removes
/// every object the bucket its properties name still holds, and then answers at the request's
/// `ResponseURL`.
const EMPTIES_THE_BUCKET: &str = r#"
'use strict';

const https = require('https');
const url = require('url');
const {
    S3Client,
    ListObjectVersionsCommand,
    DeleteObjectsCommand,
} = require('@aws-sdk/client-s3');

async function emptyBucket(bucketName) {
    const client = new S3Client({});
    const listed = await client.send(new ListObjectVersionsCommand({ Bucket: bucketName }));
    const held = [...(listed.Versions || []), ...(listed.DeleteMarkers || [])];
    const objects = held.map((entry) => ({ Key: entry.Key, VersionId: entry.VersionId }));
    if (objects.length === 0) {
        return 0;
    }
    await client.send(new DeleteObjectsCommand({
        Bucket: bucketName,
        Delete: { Objects: objects },
    }));
    return objects.length;
}

function answer(event, status, reason) {
    return new Promise((resolve, reject) => {
        const document = JSON.stringify({
            Status: status,
            Reason: reason,
            PhysicalResourceId: event.PhysicalResourceId || ('auto-' + event.LogicalResourceId),
            StackId: event.StackId,
            RequestId: event.RequestId,
            LogicalResourceId: event.LogicalResourceId,
            Data: {},
        });
        const parsed = url.parse(event.ResponseURL);
        const sending = https.request({
            hostname: parsed.hostname,
            port: parsed.port,
            path: parsed.path,
            method: 'PUT',
            headers: {
                'content-type': '',
                'content-length': Buffer.byteLength(document, 'utf8'),
            },
        }, () => resolve({}));
        sending.on('error', reject);
        sending.write(document);
        sending.end();
    });
}

exports.handler = async (event) => {
    try {
        if (event.RequestType === 'Delete') {
            await emptyBucket((event.ResourceProperties || {}).BucketName);
        }
        return await answer(event, 'SUCCESS', 'emptied');
    } catch (error) {
        return await answer(event, 'FAILED', String((error && error.message) || error));
    }
};
"#;

/// One row: the stack, the two buckets, where the refused one is declared, what it needs, and
/// whether anything refuses the deletion at all.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The bucket a custom resource of the stack empties on the way out.
    auto: &'static str,
    /// The bucket nothing empties: the deletion it is left holding an object is the one
    /// Amazon S3 refuses.
    manual: &'static str,
    /// Whether the manual bucket is declared BEFORE the emptied one, which is what puts it
    /// on the other side of the walk.
    manual_first: bool,
    /// A parameter the manual bucket's declaration NEEDS, where the row has one: a resource
    /// a refused resource depends on cannot be deleted while that one is still there.
    guard: Option<&'static str>,
    /// Whether the manual bucket holds an object when the stack is deleted — the refusal.
    refused: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the refused bucket is declared after the one its sibling empties",
            stack: "auto-emptied-after",
            auto: "auto-emptied-after-auto",
            manual: "auto-emptied-after-manual",
            manual_first: false,
            guard: None,
            refused: true,
        },
        Row {
            case: "the refused bucket is declared before the one its sibling empties",
            stack: "auto-emptied-before",
            auto: "auto-emptied-before-auto",
            manual: "auto-emptied-before-manual",
            manual_first: true,
            guard: None,
            refused: true,
        },
        Row {
            case: "the refused bucket needs a parameter of its own",
            stack: "auto-emptied-guarded",
            auto: "auto-emptied-guarded-auto",
            manual: "auto-emptied-guarded-manual",
            manual_first: false,
            guard: Some("/auto-emptied-guarded/needed"),
            refused: true,
        },
        // The control row: the same stack with NOTHING refusing a deletion. The emptied
        // bucket is deleted for the same reason, and the deletion completes.
        Row {
            case: "nothing refuses the deletion",
            stack: "auto-emptied-clean",
            auto: "auto-emptied-clean-auto",
            manual: "auto-emptied-clean-manual",
            manual_first: false,
            guard: None,
            refused: false,
        },
    ]
}

/// The template one row deploys: the provider's execution role and the provider itself, the
/// bucket that provider empties, the custom resource that empties it, and the sibling bucket
/// nothing empties — with the parameter that sibling needs where the row declares one.
fn template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    let manual = {
        let mut declaration = json!({
            "Type": "AWS::S3::Bucket",
            "Properties": {"BucketName": row.manual},
        });
        if let Some(guard) = row.guard {
            resources.insert(
                "Guard".to_string(),
                json!({
                    "Type": "AWS::SSM::Parameter",
                    "Properties": {"Name": guard, "Type": "String", "Value": "needed"},
                }),
            );
            declaration["DependsOn"] = Value::String("Guard".to_string());
        }
        declaration
    };
    if row.manual_first {
        resources.insert("Manual".to_string(), manual.clone());
    }
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Action": "sts:AssumeRole",
                        "Effect": "Allow",
                        "Principal": {"Service": "lambda.amazonaws.com"},
                    }],
                },
                // What the provider needs to empty the bucket, as the CDK grants it: the
                // reads that list what the bucket holds and the deletes that remove it.
                "Policies": [{
                    "PolicyName": "empties-the-bucket",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": ["s3:GetBucket*", "s3:List*", "s3:DeleteObject*"],
                            "Effect": "Allow",
                            "Resource": [
                                format!("arn:aws:s3:::{}", row.auto),
                                format!("arn:aws:s3:::{}/*", row.auto),
                            ],
                        }],
                    },
                }],
            },
        }),
    );
    resources.insert(
        "Provider".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Code": {"ZipFile": EMPTIES_THE_BUCKET},
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                "Runtime": "nodejs20.x",
            },
        }),
    );
    resources.insert(
        "Auto".to_string(),
        json!({
            "Type": "AWS::S3::Bucket",
            "Properties": {"BucketName": row.auto},
        }),
    );
    resources.insert(
        "AutoDeleteObjects".to_string(),
        json!({
            "Type": "Custom::S3AutoDeleteObjects",
            "Properties": {
                "ServiceToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                "BucketName": {"Ref": "Auto"},
            },
        }),
    );
    if !row.manual_first {
        resources.insert("Manual".to_string(), manual);
    }
    json!({"Resources": Value::Object(resources)}).to_string()
}

/// Write one object into a bucket, through Amazon S3's own wire.
async fn write_an_object(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let written = s3(
        router,
        "PUT",
        &format!("/{bucket}/held/object.txt"),
        b"an object a deletion has to clear first",
    )
    .await;
    assert!(
        (200..300).contains(&written.status),
        "case {case:?}: expected the object to be written into {bucket:?}, got {} {}",
        written.status,
        written.body
    );
}

/// Whether Amazon S3 still holds a bucket, read as a client reads it.
async fn bucket_exists(router: &MultiServiceRouter, bucket: &str) -> bool {
    let read = s3(router, "GET", &format!("/{bucket}"), b"").await;
    (200..300).contains(&read.status)
}

/// One stack's per-resource view as `(logical id, status)`.
async fn resource_statuses(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String)> {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResources({stack}) must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            (
                xml_text(entry, "LogicalResourceId").unwrap_or_default(),
                xml_text(entry, "ResourceStatus").unwrap_or_default(),
            )
        })
        .collect()
}

/// Poll one stack until it is no longer in an `*_IN_PROGRESS` status, and answer the status
/// it settled in.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut reported = String::new();
    for _ in 0..POLL_ATTEMPTS {
        reported = described(router, case, stack).await.0;
        if !reported.ends_with("_IN_PROGRESS") {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    reported
}

/// One stack's reported status and its `StackId` — the identifier a stack whose deletion
/// COMPLETED is read by, since such a stack is no longer in the live set a name resolves
/// against.
async fn described(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks({stack}) must answer 200, got {code} {body}"
    );
    (
        xml_text(&body, "StackStatus").unwrap_or_default(),
        xml_text(&body, "StackId").unwrap_or_default(),
    )
}

/// The status a row's resource is reported in, named for the assertion.
fn status_of(statuses: &[(String, String)], logical_id: &str) -> String {
    statuses
        .iter()
        .find(|(id, _)| id == logical_id)
        .map(|(_, status)| status.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_refused_deletion_leaves_nothing_an_unfinished_removal_held_back_undeleted() {
    for row in rows() {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let (code, created) = create_stack(
            &router,
            row.stack,
            &template(&row),
            &[member("Capabilities.member.1", "CAPABILITY_IAM")],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted, got {code} {created}"
        );
        let reported = settled(&router, case, row.stack).await;
        assert_eq!(
            reported,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {reported:?} with {:?}",
            resource_statuses(&router, case, row.stack).await
        );
        // Addressed by STACK ID from here on: a deletion that completes takes the stack out
        // of the live set a name resolves against.
        let stack_id = described(&router, case, row.stack).await.1;

        // The bucket the provider empties always holds an object, so its own deletion is one
        // Amazon S3 would refuse until that provider has run. The sibling holds one only in
        // the rows where it is the refusal.
        write_an_object(&router, case, row.auto).await;
        if row.refused {
            write_an_object(&router, case, row.manual).await;
        }

        let (code, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DeleteStack must be accepted, got {code} {body}"
        );
        let reported = settled(&router, case, &stack_id).await;
        let statuses = resource_statuses(&router, case, &stack_id).await;
        let expected_stack = match row.refused {
            true => status::DELETE_FAILED,
            false => status::DELETE_COMPLETE,
        };
        assert_eq!(
            reported, expected_stack,
            "case {case:?}: expected the deletion to settle in {expected_stack:?}, got \
             {reported:?} with {statuses:?}"
        );

        // The bucket the custom resource emptied is GONE: the provider's `Delete` cleared it
        // and the deletion that follows that answer removed the bucket itself — whether or
        // not another resource's deletion was refused.
        let auto = status_of(&statuses, "Auto");
        assert_eq!(
            auto,
            resource_status::DELETE_COMPLETE,
            "case {case:?}: expected the emptied bucket's deletion to complete, got {auto:?} \
             in {statuses:?}"
        );
        assert!(
            !bucket_exists(&router, row.auto).await,
            "case {case:?}: expected Amazon S3 to no longer hold {:?}",
            row.auto
        );
        // ...and so are the resources that custom resource itself needed, which the walk had
        // left alone while its removal was still in flight.
        for freed in ["AutoDeleteObjects", "Provider", "Role"] {
            let reported = status_of(&statuses, freed);
            assert_eq!(
                reported,
                resource_status::DELETE_COMPLETE,
                "case {case:?}: expected {freed:?} to be deleted, got {reported:?} in \
                 {statuses:?}"
            );
        }

        // What the refusal holds back is the refused resource and what IT needs, and nothing
        // else: the sibling bucket reports the refusal and still holds its object, and the
        // parameter it depends on is still in Systems Manager.
        let manual = status_of(&statuses, "Manual");
        let expected_manual = match row.refused {
            true => resource_status::DELETE_FAILED,
            false => resource_status::DELETE_COMPLETE,
        };
        assert_eq!(
            manual, expected_manual,
            "case {case:?}: expected the sibling bucket to report {expected_manual:?}, got \
             {manual:?} in {statuses:?}"
        );
        assert_eq!(
            bucket_exists(&router, row.manual).await,
            row.refused,
            "case {case:?}: expected Amazon S3 to {} hold {:?}",
            if row.refused { "still" } else { "no longer" },
            row.manual
        );
        if let Some(guard) = row.guard {
            let held = parameter_value(&router, guard).await;
            assert_eq!(
                held.as_deref(),
                Some("needed"),
                "case {case:?}: expected the parameter the refused bucket needs to be left \
                 exactly as it is, got {held:?}"
            );
        }
    }
}
