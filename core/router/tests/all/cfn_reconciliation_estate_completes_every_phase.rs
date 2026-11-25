// SPDX-License-Identifier: CC0-1.0

//! The whole LIFECYCLE of a stack-managed estate on the assembled emulator: deploy it, read
//! the facts it is made of in the services that own them, run work through it, empty what the
//! destroy cannot empty, destroy it, and reap what the stack never owned.
//!
//! RULE under test: **each phase of a stack-managed estate reaches a TERMINAL, observable
//! state in bounded time, and what each phase reports is backed by the service that owns the
//! resource:**
//!
//! * `CreateStack` settles `CREATE_COMPLETE` with every declared resource really held by its
//!   owning service — so each identifier the stack publishes resolves through that service's
//!   own read;
//! * a `DeleteStack` whose declared resource the OWNING service refuses to remove settles
//!   `DELETE_FAILED`, whose `StackStatusReason` is the fixed resource-failure summary naming
//!   that logical id while the service's own refusal is carried on that resource's event — and
//!   the resource, with everything in it, is left exactly as it was;
//! * once the client has cleared what the refusal was about, a repeated `DeleteStack` settles
//!   `DELETE_COMPLETE` and every resource the stack declared is gone from the service that
//!   held it;
//! * a resource no stack declared — the default log group a run's own delivery creates —
//!   outlives the stack's destroy and goes only when the client deletes it, which is a call
//!   that answers rather than one that can be waited on forever.
//!
//! `DeleteBucket`'s own documentation states the refusal the middle phases turn on: "All
//! objects (including all object versions and delete markers) in the bucket must be deleted
//! before the bucket itself can be deleted", answered `BucketNotEmpty` with HTTP 409. A
//! bucket declared with the default deletion policy is therefore the resource whose removal
//! the owning service refuses while the estate's own work is still archived in it — which is
//! why an operator's teardown has an EMPTYING step at all, and why that step is load-bearing
//! rather than cosmetic.
//!
//! Why it matters: a deploy-exercise-teardown script — the AWS CLI's
//! `cloudformation wait stack-create-complete` and `stack-delete-complete`, a CI job, an
//! operator's runbook — branches on each phase's terminal status and on the live reads
//! between them. A phase that never terminates leaves that client with no estate, no
//! diagnosis and nothing to compare: a target that implements none of the lifecycle is then
//! indistinguishable from one that implements part of it.
//!
//! Six crates have to agree for the rule to hold (CloudFormation drives every phase; Amazon
//! EC2 owns the estate's network facts, AWS Lambda the reconciler, Amazon S3 the archive whose
//! refusal decides the teardown, Amazon SQS the queues, IAM the execution role and Amazon
//! CloudWatch Logs the group the runs are delivered to), so the suite lives here rather than
//! in any one of them. What the neighbours own and this one does not restate:
//! [`crate::cfn_order_intake_estate_settles_terminally`] pins that a stack operation leaves
//! `*_IN_PROGRESS` at all, [`crate::cloudformation_lambda_function_log_destination`] WHERE a
//! deployed function's records land (and owns the assembled-emulator harness reused here), and
//! [`crate::lambda_log_group_outlives_delete_function`] the same log-group boundary for a
//! function deleted directly. What only this suite drives is the estate's teardown THROUGH the
//! refusal: the destroy that fails on content, the client's emptying, and the destroy that
//! then completes.
//!
//! The rows differ along what the rule generalises over: how much work ran through the estate
//! before the teardown (two trials, one, none), and so how many objects the archive holds when
//! the destroy is asked for — which is what decides whether the destroy is refused at all, the
//! row holding nothing being the control that keeps the refusal from being vacuous — and where
//! those objects sit (a nested prefix, the bucket root).
//!
//! Nothing waits a span of its own choosing: every wait looks again for the CONDITION it
//! asserts (a stack's terminal status, a function's `Active`, a run's records) until it holds
//! or a generous deadline passes, and nothing asserts on elapsed time.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The assembled emulator, the wire harness, the Lambda invocation and the CloudWatch Logs
// readers this test crate already publishes for a DEPLOYED function and its log, rather than
// second copies of them.
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, dispatch, emulator, groups_named, invoke, lambda, logs, logs_policy,
    messages_at_least, trust_policy, wait_until_active, Answered, CREATE_COMPLETE,
    DEFAULT_GROUP_PREFIX, FUNCTION_TIMEOUT, HANDLER_SOURCE, RECORDS_PER_RUN, REPORT_PREFIX,
};
use crate::query_wire::{elements, form_encode, opt_element, outputs_of, path_encode};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    failed_action, resource_failure_summary,
};

/// Every TERMINAL member of `com.amazonaws.cloudformation#StackStatus` — the whole enum less
/// the members whose names end in `_IN_PROGRESS`. Source:
/// `smithy_query(query="shape",shape="StackStatus")`. A phase is over when the stack reports
/// one of these, whichever way it went.
const TERMINAL_STATUSES: &[&str] = &[
    "CREATE_FAILED",
    "CREATE_COMPLETE",
    "ROLLBACK_FAILED",
    "ROLLBACK_COMPLETE",
    "DELETE_FAILED",
    "DELETE_COMPLETE",
    "UPDATE_COMPLETE",
    "UPDATE_FAILED",
    "UPDATE_ROLLBACK_FAILED",
    "UPDATE_ROLLBACK_COMPLETE",
    "IMPORT_COMPLETE",
    "IMPORT_ROLLBACK_FAILED",
    "IMPORT_ROLLBACK_COMPLETE",
];

/// The two terminal members the teardown phases below are stated about.
const DELETE_FAILED: &str = "DELETE_FAILED";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// How long a phase is given to reach a terminal status, and how often the stack is asked.
/// Generous: the deadline is a safety net rather than a measurement — what is asserted is the
/// status the phase settled in, and each loop stops the instant the stack reports one.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The addresses the estate's network facts are declared with: a VPC block, the subnet block
/// inside it, and the zone the subnet is placed in.
const VPC_CIDR: &str = "10.30.0.0/16";
const SUBNET_CIDR: &str = "10.30.1.0/24";
const ZONE: &str = "us-east-1a";

/// The logical ids the estate template declares, and the one whose removal the owning service
/// refuses while the estate's work is still archived in it.
const NETWORK_ID: &str = "Network";
const PARTITION_ID: &str = "Partition";
const BACKLOG_ID: &str = "Backlog";
const QUARANTINE_ID: &str = "Quarantine";
const ROLE_ID: &str = "ReconcilerRole";
const RECONCILER_ID: &str = "Reconciler";
const ARCHIVE_ID: &str = "Archive";

/// The runtime the reconciler is declared with.
const RUNTIME: &str = "python3.12";

/// The sentence Amazon S3 words its refusal of a `DeleteBucket` of a bucket that still holds
/// objects with — "The bucket you tried to delete is not empty", under the code
/// `BucketNotEmpty` ("All objects … must be deleted before the bucket itself can be deleted",
/// `DeleteBucket`) — which is what has to reach the failing resource's own event.
const BUCKET_NOT_EMPTY_CODE: &str = "BucketNotEmpty";
const BUCKET_NOT_EMPTY_MESSAGE: &str = "The bucket you tried to delete is not empty";

/// What one line of a reconciler's run prints, as the shared handler writes it
/// ([`HANDLER_SOURCE`] prints `logged <marker>`): the line a client looks for in the group to
/// know that run's records really arrived.
const PRINTED_PREFIX: &str = "logged ";

/// The error code Amazon CloudWatch Logs refuses a group it does not hold with, and the HTTP
/// status it carries.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const LOGS_REFUSAL_STATUS: u16 = 400;

/// The refusal AWS Lambda answers a function it holds no record of with: the modelled
/// `ResourceNotFoundException` (HTTP 404) whose message is `Function not found: <arn>`.
const LAMBDA_NOT_FOUND_STATUS: u16 = 404;
const LAMBDA_NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The codes Amazon EC2 refuses an identifier naming nothing it holds with, per resource.
const INVALID_VPC_ID: &str = "InvalidVpcID.NotFound";
const INVALID_SUBNET_ID: &str = "InvalidSubnetID.NotFound";

/// The class Amazon SQS refuses a queue it does not hold with — `QueueDoesNotExist`, whose
/// `awsQueryError` code is `AWS.SimpleQueueService.NonExistentQueue`.
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";

/// Call one Amazon EC2 operation over its ec2Query wire.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Answered {
    let mut body = format!("Action={action}&Version=2016-11-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "ec2",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        Bytes::from(body),
    )
    .await
}

/// Call one Amazon SQS operation over its awsJson1_0 wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "sqs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &format!("AmazonSQS.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One request on Amazon S3's REST surface, path-style: the bucket and the key live in the
/// URI, as a client's own object calls carry them.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Answered {
    dispatch(
        router,
        "s3",
        method,
        path,
        &[("content-type", "application/octet-stream")],
        Bytes::copy_from_slice(body),
    )
    .await
}

/// The wire error code of an awsJson refusal: the body's `__type`, less its shape namespace.
fn json_error_code(answered: &Answered) -> String {
    answered
        .json()
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The estate one row deploys, named after its own stack: the network facts the estate is
/// asserted on, the queues its work is handed through, the reconciler that runs it under its
/// execution role, and the archive bucket — declared with the DEFAULT deletion policy, so it
/// is the resource whose removal Amazon S3 refuses while the estate's work is still in it.
fn estate_template(stack: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            NETWORK_ID: {
                "Type": "AWS::EC2::VPC",
                "Properties": { "CidrBlock": VPC_CIDR },
            },
            PARTITION_ID: {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": NETWORK_ID },
                    "CidrBlock": SUBNET_CIDR,
                    "AvailabilityZone": ZONE,
                },
            },
            BACKLOG_ID: {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": format!("{stack}-backlog") },
            },
            QUARANTINE_ID: {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": format!("{stack}-quarantine") },
            },
            ROLE_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "basic-execution",
                        "PolicyDocument": logs_policy(),
                    }],
                },
            },
            RECONCILER_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{stack}-reconciler"),
                    "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
            ARCHIVE_ID: {
                "Type": "AWS::S3::Bucket",
                "DeletionPolicy": "Delete",
                "Properties": { "BucketName": format!("{stack}-archive") },
            },
        },
        "Outputs": {
            "VpcId": { "Value": { "Ref": NETWORK_ID } },
            "SubnetId": { "Value": { "Ref": PARTITION_ID } },
            "FunctionName": { "Value": { "Ref": RECONCILER_ID } },
            "ArchiveBucket": { "Value": { "Ref": ARCHIVE_ID } },
            "BacklogQueueUrl": { "Value": { "Ref": BACKLOG_ID } },
            "QuarantineQueueUrl": { "Value": { "Ref": QUARANTINE_ID } },
        },
    })
    .to_string()
}

/// Look at the stack until it reports a TERMINAL status, and answer that status with the
/// `DescribeStacks` answer it was read from.
///
/// The neighbours' shared helper waits for a status the caller NAMES; what is asserted here is
/// WHICH terminal status a phase settled in — a destroy that completes and one that is refused
/// are both outcomes this suite reads — so the condition waited on is "the phase is over".
/// Each look is a `DescribeStacks`, which is also what materialises the transitions the stack's
/// own clock owes, and the stack is addressed by its ID, which resolves it in every phase
/// including after its deletion.
async fn settled(router: &MultiServiceRouter, case: &str, stack_id: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = cloudformation(router, "DescribeStacks", &[("StackName", stack_id)]).await;
        let body = described.text();
        assert_eq!(
            described.status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks({stack_id}), got {} with \
             {body}",
            described.status
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if TERMINAL_STATUSES.contains(&reported.as_str()) {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the stack never left its in-progress status; it reports \
             {reported:?} in {body}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every `(LogicalResourceId, ResourceStatus, ResourceStatusReason)` the stack's events
/// report, which is where the owning service's own refusal of one resource is carried.
async fn resource_events(
    router: &MultiServiceRouter,
    case: &str,
    stack_id: &str,
) -> Vec<(String, String, String)> {
    let answered =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_id)]).await;
    let body = answered.text();
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents({stack_id}), got {} with \
         {body}",
        answered.status
    );
    crate::query_wire::members_of(&body)
        .iter()
        .map(|event| {
            (
                opt_element(event, "LogicalResourceId").unwrap_or_default(),
                opt_element(event, "ResourceStatus").unwrap_or_default(),
                opt_element(event, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// Every key `bucket` holds right now, read the way a teardown reads it: `ListObjectsV2`,
/// which is the listing a client empties a bucket through.
async fn listed_keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let answered = s3(router, "GET", &format!("/{bucket}?list-type=2"), &[]).await;
    let body = answered.text();
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from ListObjectsV2 of {bucket:?}, got {} with {body}",
        answered.status
    );
    let mut keys = elements(&body, "Key");
    keys.sort();
    keys
}

/// One row: how much work ran through the estate before the teardown, and where each trial's
/// archived result is written.
struct Row {
    case: &'static str,
    /// How many reconciliation trials are driven through the estate, each leaving one object
    /// in the archive — so this is also how many objects the destroy meets.
    trials: usize,
    /// The prefix each trial's archived object is written under; empty means the bucket root.
    prefix: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "two trials, each archived under the estate's own prefix",
        trials: 2,
        prefix: "reconciled/",
    },
    Row {
        case: "one trial, archived at the bucket root",
        trials: 1,
        prefix: "",
    },
    Row {
        case: "no trial at all, so the archive holds nothing when the destroy is asked for",
        trials: 0,
        prefix: "reconciled/",
    },
];

impl Row {
    /// The key the trial `trial` archives its result under.
    fn key(&self, trial: usize) -> String {
        format!("{}trial-{trial}.json", self.prefix)
    }
}

#[tokio::test]
async fn the_reconciliation_estate_reaches_a_terminal_state_in_every_phase() {
    // ONE emulator for every row: each row deploys an estate of its own under its own names,
    // so what a row reads is still that row's alone.
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("reconciliation-estate-{index}");
        let template = estate_template(&stack);

        // ---- phase 1: deploy. The estate settles at CREATE_COMPLETE.
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template),
                // The estate declares an execution role, which is acknowledged with
                // CAPABILITY_IAM.
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        let created_body = created.text();
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {created_body}",
            created.status
        );
        let stack_id = opt_element(&created_body, "StackId").unwrap_or_default();
        assert!(
            !stack_id.is_empty(),
            "case {case:?}: expected CreateStack to publish a StackId, got {created_body}"
        );

        let (deployed, described) = settled(&router, case, &stack_id).await;
        assert_eq!(
            deployed, CREATE_COMPLETE,
            "case {case:?}: expected the deploy to settle in {CREATE_COMPLETE}, got \
             {deployed:?} with the recorded events {:?}",
            resource_events(&router, case, &stack_id).await
        );

        // ---- phase 2: the estate's own facts, read in the service that owns each one.
        let outputs = outputs_of(&described);
        for key in [
            "VpcId",
            "SubnetId",
            "FunctionName",
            "ArchiveBucket",
            "BacklogQueueUrl",
            "QuarantineQueueUrl",
        ] {
            assert!(
                outputs.get(key).is_some_and(|value| !value.is_empty()),
                "case {case:?}: expected the estate to publish a non-empty {key}, got \
                 {outputs:?}"
            );
        }
        let vpc = outputs.get("VpcId").cloned().unwrap_or_default();
        let subnet = outputs.get("SubnetId").cloned().unwrap_or_default();
        let function = outputs.get("FunctionName").cloned().unwrap_or_default();
        let archive = outputs.get("ArchiveBucket").cloned().unwrap_or_default();
        let queues: Vec<String> = ["BacklogQueueUrl", "QuarantineQueueUrl"]
            .iter()
            .map(|key| outputs.get(*key).cloned().unwrap_or_default())
            .collect();

        let network = ec2(&router, "DescribeVpcs", &[("VpcId.1", &vpc)]).await;
        let network_body = network.text();
        assert_eq!(
            network.status, 200,
            "case {case:?}: expected Amazon EC2 to hold the VPC {vpc}, got {} with \
             {network_body}",
            network.status
        );
        assert_eq!(
            opt_element(&network_body, "cidrBlock").as_deref(),
            Some(VPC_CIDR),
            "case {case:?}: expected the VPC to carry the declared block, got {network_body}"
        );

        let partition = ec2(&router, "DescribeSubnets", &[("SubnetId.1", &subnet)]).await;
        let partition_body = partition.text();
        assert_eq!(
            partition.status, 200,
            "case {case:?}: expected Amazon EC2 to hold the subnet {subnet}, got {} with \
             {partition_body}",
            partition.status
        );
        for (member, expected) in [
            ("vpcId", vpc.as_str()),
            ("cidrBlock", SUBNET_CIDR),
            ("availabilityZone", ZONE),
        ] {
            assert_eq!(
                opt_element(&partition_body, member).as_deref(),
                Some(expected),
                "case {case:?}: expected the subnet's {member} to be {expected:?}, got \
                 {partition_body}"
            );
        }

        for url in &queues {
            let reported = sqs(
                &router,
                "GetQueueAttributes",
                json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
            assert_eq!(
                reported.status, 200,
                "case {case:?}: expected Amazon SQS to hold the queue at {url}, got {} with \
                 {:?}",
                reported.status,
                reported.text()
            );
            let arn = reported.json()["Attributes"]["QueueArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                url.ends_with(arn.rsplit(':').next().unwrap_or_default()),
                "case {case:?}: expected the queue's ARN to name the queue its url addresses, \
                 got {arn:?} for {url}"
            );
        }

        let held = s3(&router, "HEAD", &format!("/{archive}"), &[]).await;
        assert_eq!(
            held.status, 200,
            "case {case:?}: expected Amazon S3 to hold the archive {archive}, got {}",
            held.status
        );

        // The reconciler has to be invocable before any work is driven through it: "during
        // this time, you can't invoke or modify the function" (`CreateFunction`).
        wait_until_active(&router, case, &function).await;

        // ---- phase 3: the work. Each trial is one run of the reconciler whose result is
        //      archived, which is what leaves the archive holding objects by teardown time.
        let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
        let mut archived: Vec<String> = Vec::new();
        for trial in 0..row.trials {
            let marker = format!("{stack}-trial-{trial}");
            invoke(&router, case, &function, &marker).await;
            let key = row.key(trial);
            let written = s3(
                &router,
                "PUT",
                &format!("/{archive}/{}", path_encode(&key)),
                format!("the reconciled result of {marker}").as_bytes(),
            )
            .await;
            assert_eq!(
                written.status, 200,
                "case {case:?}: expected the archive write of {key:?} to be accepted, got {} \
                 with {:?}",
                written.status,
                written.text()
            );
            archived.push(key);
        }
        archived.sort();
        let in_archive = listed_keys(&router, case, &archive).await;
        assert_eq!(
            in_archive, archived,
            "case {case:?}: expected the archive to hold exactly what the trials wrote"
        );

        // Each run's records really reached the group the function logs to, which is the
        // resource the last phase reaps.
        if row.trials > 0 {
            let messages =
                messages_at_least(&router, &group, row.trials * RECORDS_PER_RUN).await;
            for trial in 0..row.trials {
                let printed = format!("{PRINTED_PREFIX}{stack}-trial-{trial}");
                assert!(
                    messages.iter().any(|message| message.contains(&printed)),
                    "case {case:?}: expected the group {group} to hold the line trial {trial} \
                     printed ({printed:?}), got {messages:?}"
                );
            }
            assert!(
                messages
                    .iter()
                    .filter(|message| message.starts_with(REPORT_PREFIX))
                    .count()
                    >= row.trials,
                "case {case:?}: expected the group {group} to hold one closing frame per run, \
                 got {messages:?}"
            );
        }

        // ---- phase 4: the destroy, as it is first asked for. An archive still holding
        //      objects is one Amazon S3 refuses to delete, so the stack settles DELETE_FAILED
        //      and nothing of the archive changes.
        let asked = cloudformation(&router, "DeleteStack", &[("StackName", &stack)]).await;
        assert_eq!(
            asked.status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {} with {:?}",
            asked.status,
            asked.text()
        );
        let (first, after_first) = settled(&router, case, &stack_id).await;
        let expected_first = if row.trials == 0 {
            DELETE_COMPLETE
        } else {
            DELETE_FAILED
        };
        assert_eq!(
            first, expected_first,
            "case {case:?}: expected the first destroy to settle in {expected_first}, got \
             {first:?} with the recorded events {:?}",
            resource_events(&router, case, &stack_id).await
        );

        if first == DELETE_FAILED {
            // The stack-level reason is the fixed summary naming the logical id that failed,
            // and nothing else; the owning service's own refusal is carried on that
            // resource's event.
            assert_eq!(
                opt_element(&after_first, "StackStatusReason").unwrap_or_default(),
                resource_failure_summary(failed_action::DELETE, [ARCHIVE_ID]),
                "case {case:?}: expected the stack's reason to summarise what failed, got \
                 {after_first}"
            );
            let events = resource_events(&router, case, &stack_id).await;
            let diagnosed = events.iter().any(|(logical, status, reason)| {
                logical == ARCHIVE_ID
                    && status == DELETE_FAILED
                    && reason.contains(BUCKET_NOT_EMPTY_MESSAGE)
            });
            assert!(
                diagnosed,
                "case {case:?}: expected the archive's {DELETE_FAILED} event to carry Amazon \
                 S3's own refusal ({BUCKET_NOT_EMPTY_CODE}: {BUCKET_NOT_EMPTY_MESSAGE:?}), got \
                 {events:?}"
            );

            // A refused removal took nothing away: the archive is still there, still holding
            // exactly what the trials wrote.
            let surviving = s3(&router, "HEAD", &format!("/{archive}"), &[]).await;
            assert_eq!(
                surviving.status, 200,
                "case {case:?}: expected the refused archive to survive the failed destroy, \
                 got {}",
                surviving.status
            );
            let still_held = listed_keys(&router, case, &archive).await;
            assert_eq!(
                still_held, archived,
                "case {case:?}: expected the failed destroy to leave the archive's contents \
                 untouched"
            );

            // ---- phase 5: the cleanup the destroy cannot do — the client empties the
            //      archive through its own listing.
            for key in &still_held {
                let removed = s3(
                    &router,
                    "DELETE",
                    &format!("/{archive}/{}", path_encode(key)),
                    &[],
                )
                .await;
                assert_eq!(
                    removed.status, 204,
                    "case {case:?}: expected the delete of {key:?} to be accepted, got {} with \
                     {:?}",
                    removed.status,
                    removed.text()
                );
            }
            let emptied = listed_keys(&router, case, &archive).await;
            assert!(
                emptied.is_empty(),
                "case {case:?}: expected the cleanup to leave the archive empty, got \
                 {emptied:?}"
            );

            // ---- phase 6: the destroy, asked for again now that the refusal has cleared.
            let again = cloudformation(&router, "DeleteStack", &[("StackName", &stack)]).await;
            assert_eq!(
                again.status, 200,
                "case {case:?}: expected HTTP 200 from the retried DeleteStack, got {} with \
                 {:?}",
                again.status,
                again.text()
            );
            let (second, _) = settled(&router, case, &stack_id).await;
            assert_eq!(
                second, DELETE_COMPLETE,
                "case {case:?}: expected the destroy to settle in {DELETE_COMPLETE} once the \
                 archive was emptied, got {second:?} with the recorded events {:?}",
                resource_events(&router, case, &stack_id).await
            );
        }

        // ---- nothing left behind: every resource the estate declared is gone from the
        //      service that held it.
        let gone = s3(&router, "HEAD", &format!("/{archive}"), &[]).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: expected the destroyed archive to be gone from Amazon S3, got {}",
            gone.status
        );
        let reconciler = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            reconciler.status, LAMBDA_NOT_FOUND_STATUS,
            "case {case:?}: expected the destroyed reconciler to be gone from AWS Lambda, got \
             {} with {:?}",
            reconciler.status,
            reconciler.text()
        );
        // The message member AWS Lambda words that refusal in, read under either spelling of
        // its name, as the neighbour suites read it.
        let refusal = reconciler.json();
        let worded = refusal
            .get("message")
            .or_else(|| refusal.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            worded.starts_with(LAMBDA_NOT_FOUND_PREFIX) && worded.contains(&function),
            "case {case:?}: expected the refusal to be worded {LAMBDA_NOT_FOUND_PREFIX:?} \
             naming the destroyed reconciler, got {:?}",
            reconciler.text()
        );
        for url in &queues {
            let refused = sqs(
                &router,
                "GetQueueAttributes",
                json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {case:?}: expected the queue at {url} to be gone from Amazon SQS, got {} \
                 with {:?}",
                refused.status,
                refused.text()
            );
            assert_eq!(
                json_error_code(&refused),
                QUEUE_DOES_NOT_EXIST,
                "case {case:?}: expected {QUEUE_DOES_NOT_EXIST} for {url}, got {:?}",
                refused.text()
            );
        }
        for (action, member, id, code) in [
            ("DescribeSubnets", "SubnetId.1", &subnet, INVALID_SUBNET_ID),
            ("DescribeVpcs", "VpcId.1", &vpc, INVALID_VPC_ID),
        ] {
            let refused = ec2(&router, action, &[(member, id)]).await;
            let body = refused.text();
            assert_eq!(
                refused.status, 400,
                "case {case:?}: expected {action} on the destroyed {id} to be refused, got {} \
                 with {body}",
                refused.status
            );
            assert_eq!(
                opt_element(&body, "Code").as_deref(),
                Some(code),
                "case {case:?}: expected {action} on the destroyed {id} to be refused with \
                 {code}, got {body}"
            );
        }

        // ---- phase 7: the reap. The default log group is a resource of Amazon CloudWatch
        //      Logs that no stack declared, so it outlives the destroy — and the client's own
        //      delete of it ANSWERS, after which the group is gone and a second delete is
        //      refused.
        if row.trials > 0 {
            let outliving = groups_named(&router, case, &group).await;
            assert_eq!(
                outliving.len(),
                1,
                "case {case:?}: expected the group {group} the runs wrote to outlive the \
                 destroyed estate, got {outliving:?}"
            );
            let reaped = logs(&router, "DeleteLogGroup", json!({ "logGroupName": group })).await;
            assert_eq!(
                reaped.status, 200,
                "case {case:?}: expected HTTP 200 from DeleteLogGroup({group}), got {} with \
                 {:?}",
                reaped.status,
                reaped.text()
            );
            let left = groups_named(&router, case, &group).await;
            assert!(
                left.is_empty(),
                "case {case:?}: expected the reaped group {group} to be gone from the next \
                 read, got {left:?}"
            );
            let twice = logs(&router, "DeleteLogGroup", json!({ "logGroupName": group })).await;
            assert_eq!(
                twice.status, LOGS_REFUSAL_STATUS,
                "case {case:?}: expected a second delete of the reaped {group} to be refused, \
                 got {} with {:?}",
                twice.status,
                twice.text()
            );
            assert_eq!(
                json_error_code(&twice),
                RESOURCE_NOT_FOUND,
                "case {case:?}: expected {RESOURCE_NOT_FOUND} for the reaped {group}, got {:?}",
                twice.text()
            );
        }
    }
}
