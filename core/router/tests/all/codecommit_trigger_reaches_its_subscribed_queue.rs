// SPDX-License-Identifier: CC0-1.0

//! A repository trigger reaches the queue subscribed to its topic when a reference really
//! changes — through the WHOLE mounted service set, the one `core/router/src/lib.rs`
//! assembles for the binary.
//!
//! The RULE under test: **a write that creates, moves or removes a reference runs every
//! trigger of the repository's stored configuration that is in scope for BOTH that branch
//! and that class of event — a trigger whose `branches` name the written branch or which
//! names none at all ("if you specify an empty array, the trigger applies to all
//! branches"), and whose `events` name that class or `all` — and each trigger that runs
//! sends ONE record to the destination it names, labelled `ReferenceChanges` and naming
//! the repository's ARN, the trigger, the stored `configurationId` and the fully qualified
//! reference with the commit it now points at; a write no stored trigger is in scope for
//! sends nothing.** The record `TestRepositoryTriggers` sends is labelled
//! `TriggerEventTest` instead, so a consumer tells a probe apart from a reference that
//! really moved. Delivery past the topic follows each subscribed queue's OWN policy: a
//! queue whose policy does not allow the publishing topic receives nothing while it stays
//! subscribed, and one whose policy is repaired receives the next record.
//!
//! Three services have to agree for that to hold — CodeCommit publishes, SNS fans out,
//! SQS holds what arrived — and the publish CodeCommit makes is an in-process call
//! addressed the way a client's own `sns`-scoped call is addressed, so WHICH mounted model
//! claims it is only decidable with every service mounted. The crate-local suite
//! `services/service-com-amazonaws-codecommit-codecommit-20150413/tests/all/repository_trigger_test_notifications.rs`
//! pins what a TESTED trigger sends over tables of destinations and branch scopes; what
//! only this suite can reach is the reference-change path over the production service set.
//!
//! The rows differ along what the rule generalises over: the class of reference write
//! (created, updated, removed), the operation that writes it (`PutFile`, `CreateCommit`,
//! `DeleteFile`, `CreateBranch`, `DeleteBranch`), the branch scope a trigger names (one
//! branch, no branch at all, another branch) and the events it names (`all`, one class,
//! a class the write is not).

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::{element, elements, form_encode};

/// The Region the whole wiring lives in. A trigger's destination has to be in the
/// repository's own Region, so the topic, the queue and the repository share it.
const REGION: &str = "us-west-2";

/// CodeCommit's `X-Amz-Target` prefix and its signing name.
const CODECOMMIT_TARGET_PREFIX: &str = "CodeCommit_20150413";
const CODECOMMIT_SIGNING_NAME: &str = "codecommit";

/// What a record says about itself, spelled out here rather than imported from the
/// service crate, so a wrong constant in the implementation cannot make the test pass.
const EVENT_SOURCE: &str = "aws:codecommit";
const REFERENCE_CHANGES: &str = "ReferenceChanges";
const TRIGGER_EVENT_TEST: &str = "TriggerEventTest";

/// git's null object name: the value a reference update carries for a reference that
/// points at nothing, which is what the record of a REMOVED reference names.
const NULL_COMMIT: &str = "0000000000000000000000000000000000000000";

/// The service principal SNS delivers to a subscribed queue as, which is what that
/// queue's own policy has to allow.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The file every row writes, and the one a row adds and then deletes.
const FILE: &str = "README.md";
const EXTRA_FILE: &str = "extra.txt";

/// How many times a leg polls the queue before calling a record undelivered, and the long
/// poll each look makes. Generous on purpose: it bounds a loop that ends as soon as the
/// records are on the queue, so a fast machine never waits for it and a machine running
/// the whole workspace's tests at once is not failed by it.
const POLLS: usize = 30;
const WAIT_TIME_SECONDS: u32 = 1;

/// One `X-Amz-Target` RPC call that must answer 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name, REGION))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CodeCommit call (`awsJson1_1`, `CodeCommit_20150413.<Operation>`).
async fn codecommit(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        CODECOMMIT_SIGNING_NAME,
        CODECOMMIT_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "sqs",
        "AmazonSQS",
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
}

/// One Amazon SNS call over the `awsQuery` form wire that service serves.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sns", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {text}"
    );
    text
}

/// The queue policy AWS documents for a queue subscribed to a topic — the
/// `sns.amazonaws.com` service principal, allowed `sqs:SendMessage` on that queue —
/// scoped by condition to the topic whose notifications it admits, which is the grant
/// `aws:SourceArn` narrows.
fn grant_to(queue_arn: &str, topic_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": SNS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": {"ArnEquals": {"aws:SourceArn": topic_arn}},
        }],
    })
    .to_string()
}

/// A topic of this emulator, answering its ARN — what a trigger's `destinationArn` names.
async fn given_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    element(
        &sns(router, case, "CreateTopic", &[("Name", name)]).await,
        "TopicArn",
    )
}

/// A queue of this emulator, answering its url and its ARN.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// Put `policy` on the queue at `url`, which is the document SNS's delivery to it is
/// decided against.
async fn set_queue_policy(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    policy: &str,
) {
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    )
    .await;
}

/// Subscribe the queue `queue_arn` to `topic`, over the `sqs` protocol.
async fn subscribe(router: &MultiServiceRouter, case: &str, topic: &str, queue_arn: &str) {
    sns(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", topic),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn),
        ],
    )
    .await;
}

/// The endpoints still subscribed to `topic`, as SNS reports them.
async fn subscribed_endpoints(router: &MultiServiceRouter, case: &str, topic: &str) -> Vec<String> {
    elements(
        &sns(
            router,
            case,
            "ListSubscriptionsByTopic",
            &[("TopicArn", topic)],
        )
        .await,
        "Endpoint",
    )
}

/// A repository of this emulator, answering its ARN — what a record's `eventSourceARN`
/// must name.
async fn create_repository(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = codecommit(
        router,
        case,
        "CreateRepository",
        &json!({"repositoryName": name}),
    )
    .await;
    created["repositoryMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no repositoryMetadata.Arn in {created}"))
        .to_string()
}

/// Store `triggers` on `repository`, answering the `configurationId` the write minted —
/// the id every record of that configuration must carry.
async fn put_triggers(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    triggers: &Value,
) -> String {
    let stored = codecommit(
        router,
        case,
        "PutRepositoryTriggers",
        &json!({"repositoryName": repository, "triggers": triggers}),
    )
    .await;
    stored["configurationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no configurationId in {stored}"))
        .to_string()
}

/// Write one commit on `branch` through `PutFile`, answering its id.
async fn put_file(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    branch: &str,
    parent: Option<&str>,
    content: &str,
) -> String {
    let mut body = json!({
        "repositoryName": repository,
        "branchName": branch,
        "filePath": FILE,
        "fileContent": ::aws_smithy_types::base64::encode(content.as_bytes()),
        "commitMessage": "a revision",
    });
    if let Some(parent) = parent {
        body["parentCommitId"] = json!(parent);
    }
    let written = codecommit(router, case, "PutFile", &body).await;
    written["commitId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no commitId in {written}"))
        .to_string()
}

/// Take every record waiting on the queue at `url`, read out of the SNS envelope the queue
/// carries it in, deleting what it hands over so a later look reads only what arrived
/// since. Each delivery carries ONE `Records` entry, which is the record itself.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": WAIT_TIME_SECONDS,
        }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut records = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
        });
        let inner = envelope["Message"].as_str().unwrap_or_default();
        let document: Value = serde_json::from_str(inner).unwrap_or_else(|error| {
            panic!("case {case:?}: the message is not a trigger record ({error}): {inner}")
        });
        let entries = document["Records"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no Records in {document}"))
            .clone();
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: a record carries one entry, got {entries:?}"
        );
        records.push(entries[0].clone());
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    records
}

/// Poll the queue at `url` until `wanted` records have arrived or the polls run out.
async fn records_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let mut arrived: Vec<Value> = Vec::new();
    for _ in 0..POLLS {
        if arrived.len() >= wanted {
            break;
        }
        arrived.extend(drain(router, case, url).await);
    }
    arrived
}

/// The trigger names of `records`, sorted — which triggers the write ran.
fn triggers_that_ran(records: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = records
        .iter()
        .map(|record| {
            record["eventTriggerName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// The `(ref, commit)` the record's single reference names.
fn sole_reference(case: &str, record: &Value) -> (String, String) {
    let references = record["codecommit"]["references"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no codecommit.references in {record}"))
        .clone();
    assert_eq!(
        references.len(),
        1,
        "case {case:?}: expected one reference, got {references:?}"
    );
    (
        references[0]["ref"].as_str().unwrap_or_default().to_string(),
        references[0]["commit"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// What every record of one reference write has to say, whatever the trigger: the class of
/// event, the repository it is on, the configuration it belongs to, the identity whose
/// call produced it, and the reference with the commit it now points at.
fn assert_record(
    case: &str,
    record: &Value,
    event_name: &str,
    repository_arn: &str,
    configuration_id: &str,
    reference: &str,
    commit: &str,
) {
    for (member, expected) in [
        ("awsRegion", REGION),
        ("eventName", event_name),
        ("eventSource", EVENT_SOURCE),
        ("eventSourceARN", repository_arn),
        ("eventTriggerConfigId", configuration_id),
    ] {
        assert_eq!(
            record[member].as_str(),
            Some(expected),
            "case {case:?}: {member} must be {expected:?}, got {record}"
        );
    }
    let identity = record["userIdentityARN"].as_str().unwrap_or_default();
    assert!(
        identity.starts_with("arn:"),
        "case {case:?}: userIdentityARN must name the calling identity, got {identity:?}"
    );
    let (named_reference, named_commit) = sole_reference(case, record);
    assert_eq!(
        named_reference, reference,
        "case {case:?}: the record must name {reference}, got {named_reference}"
    );
    assert_eq!(
        named_commit, commit,
        "case {case:?}: the record must name commit {commit}, got {named_commit}"
    );
}

/// What one row writes.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// A commit on a branch that already has one, through `PutFile`.
    PutFile(&'static str),
    /// A commit on a branch, adding [`EXTRA_FILE`], through `CreateCommit`.
    AddFile(&'static str),
    /// A commit on a branch, removing [`EXTRA_FILE`], through `DeleteFile`.
    RemoveFile(&'static str),
    /// A reference the repository did not hold, at `main`'s tip, through `CreateBranch`.
    CreateBranch(&'static str),
    /// A reference the repository no longer holds, through `DeleteBranch`.
    DeleteBranch(&'static str),
}

impl Write {
    /// The branch this write writes.
    fn branch(self) -> &'static str {
        match self {
            Self::PutFile(branch)
            | Self::AddFile(branch)
            | Self::RemoveFile(branch)
            | Self::CreateBranch(branch)
            | Self::DeleteBranch(branch) => branch,
        }
    }
}

#[tokio::test]
async fn a_reference_write_runs_the_triggers_in_scope_and_no_others() {
    let router = router();
    let setup = "the configured wiring";
    let topic = given_topic(&router, setup, "codecommit-reference-changes").await;
    let (queue_url, queue_arn) = given_queue(&router, setup, "codecommit-reference-inbox").await;
    set_queue_policy(&router, setup, &queue_url, &grant_to(&queue_arn, &topic)).await;
    subscribe(&router, setup, &topic, &queue_arn).await;

    let repository = "reference-change-triggers";
    let repository_arn = create_repository(&router, setup, repository).await;
    let seed = put_file(&router, setup, repository, "main", None, "seed").await;
    create_branch(&router, setup, repository, "unlisted", &seed).await;

    // A write made while the repository held no configuration ran no trigger at all.
    let unconfigured = "a write made before a configuration was stored";
    let nothing = drain(&router, unconfigured, &queue_url).await;
    assert!(
        nothing.is_empty(),
        "case {unconfigured:?}: a repository with no stored trigger owes nothing, got \
         {nothing:?}"
    );

    // Four triggers of ONE configuration, all on the same topic, differing in the scope
    // the rule reads: the branches they name and the events they name.
    let configuration_id = put_triggers(
        &router,
        setup,
        repository,
        &json!([
            {
                "name": "main-updates",
                "destinationArn": topic,
                "branches": ["main"],
                "events": ["updateReference"],
            },
            // No `branches` member at all: the trigger applies to every branch.
            {"name": "every-branch", "destinationArn": topic, "events": ["all"]},
            {
                "name": "feature-life",
                "destinationArn": topic,
                "branches": ["feature"],
                "events": ["createReference", "deleteReference"],
            },
            // In scope for `main`, but for a class of write none of the rows below is.
            {
                "name": "main-creates",
                "destinationArn": topic,
                "branches": ["main"],
                "events": ["createReference"],
            },
        ]),
    )
    .await;

    // (case, the write, the triggers it must run)
    let rows: &[(&str, Write, &[&str])] = &[
        (
            "a commit on a branch one trigger lists",
            Write::PutFile("main"),
            &["every-branch", "main-updates"],
        ),
        (
            "a commit on a branch no trigger lists",
            Write::PutFile("unlisted"),
            &["every-branch"],
        ),
        (
            "a commit written through CreateCommit",
            Write::AddFile("main"),
            &["every-branch", "main-updates"],
        ),
        (
            "a commit written through DeleteFile",
            Write::RemoveFile("main"),
            &["every-branch", "main-updates"],
        ),
        (
            "a reference the repository did not hold",
            Write::CreateBranch("feature"),
            &["every-branch", "feature-life"],
        ),
        (
            "a reference the repository no longer holds",
            Write::DeleteBranch("feature"),
            &["every-branch", "feature-life"],
        ),
    ];

    let mut tips: BTreeMap<String, String> = BTreeMap::new();
    tips.insert("main".to_string(), seed.clone());
    tips.insert("unlisted".to_string(), seed.clone());

    for (index, (case, write, expected)) in rows.iter().enumerate() {
        let branch = write.branch();
        let parent = tips.get(branch).cloned();
        let commit = match write {
            Write::PutFile(_) => {
                put_file(
                    &router,
                    case,
                    repository,
                    branch,
                    parent.as_deref(),
                    &format!("revision {index}"),
                )
                .await
            }
            Write::AddFile(_) => {
                let written = codecommit(
                    &router,
                    case,
                    "CreateCommit",
                    &json!({
                        "repositoryName": repository,
                        "branchName": branch,
                        "parentCommitId": parent,
                        "commitMessage": "a file added",
                        "putFiles": [{
                            "filePath": EXTRA_FILE,
                            "fileContent": ::aws_smithy_types::base64::encode(
                                format!("added at row {index}").as_bytes(),
                            ),
                        }],
                    }),
                )
                .await;
                written["commitId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no commitId in {written}"))
                    .to_string()
            }
            Write::RemoveFile(_) => {
                let written = codecommit(
                    &router,
                    case,
                    "DeleteFile",
                    &json!({
                        "repositoryName": repository,
                        "branchName": branch,
                        "filePath": EXTRA_FILE,
                        "parentCommitId": parent,
                        "commitMessage": "a file removed",
                    }),
                )
                .await;
                written["commitId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no commitId in {written}"))
                    .to_string()
            }
            Write::CreateBranch(_) => {
                let at = tips
                    .get("main")
                    .cloned()
                    .unwrap_or_else(|| panic!("case {case:?}: main has no tip"));
                create_branch(&router, case, repository, branch, &at).await;
                at
            }
            Write::DeleteBranch(_) => {
                let removed = codecommit(
                    &router,
                    case,
                    "DeleteBranch",
                    &json!({"repositoryName": repository, "branchName": branch}),
                )
                .await;
                assert_eq!(
                    removed["deletedBranch"]["branchName"].as_str(),
                    Some(branch),
                    "case {case:?}: the removal must report the reference it removed, got \
                     {removed}"
                );
                // A removed reference points at nothing, which is what its record names.
                NULL_COMMIT.to_string()
            }
        };
        match write {
            Write::DeleteBranch(_) => {
                tips.remove(branch);
            }
            _ => {
                tips.insert(branch.to_string(), commit.clone());
            }
        }

        let records = records_until(&router, case, &queue_url, expected.len()).await;
        let ran = triggers_that_ran(&records);
        let wanted: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
        assert_eq!(
            ran, wanted,
            "case {case:?}: expected the triggers {wanted:?} to run, got {ran:?} from {records:?}"
        );
        // A further look finds nothing more: a trigger out of scope sent no record.
        let extra = drain(&router, case, &queue_url).await;
        assert!(
            extra.is_empty(),
            "case {case:?}: only the triggers in scope run, got the further records {extra:?}"
        );
        let reference = format!("refs/heads/{branch}");
        for record in &records {
            assert_record(
                case,
                record,
                REFERENCE_CHANGES,
                &repository_arn,
                &configuration_id,
                &reference,
                &commit,
            );
        }
    }
}

/// Branch `branch` off `commit_id`, the way a caller opens a branch of its own.
async fn create_branch(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    branch: &str,
    commit_id: &str,
) {
    codecommit(
        router,
        case,
        "CreateBranch",
        &json!({
            "repositoryName": repository,
            "branchName": branch,
            "commitId": commit_id,
        }),
    )
    .await;
}

#[tokio::test]
async fn a_tested_trigger_is_labelled_a_probe_and_a_real_write_is_not() {
    let router = router();
    let case = "a test probe beside a reference that really changed";
    let topic = given_topic(&router, case, "codecommit-probe-and-change").await;
    let (queue_url, queue_arn) = given_queue(&router, case, "codecommit-probe-inbox").await;
    set_queue_policy(&router, case, &queue_url, &grant_to(&queue_arn, &topic)).await;
    subscribe(&router, case, &topic, &queue_arn).await;

    let repository = "probe-and-change";
    let repository_arn = create_repository(&router, case, repository).await;
    let seed = put_file(&router, case, repository, "main", None, "seed").await;
    let trigger = json!([{
        "name": "everything",
        "destinationArn": topic,
        "branches": ["main"],
        "events": ["all"],
    }]);
    let configuration_id = put_triggers(&router, case, repository, &trigger).await;

    // The test sends its probe to the destination, and reports the trigger as tested.
    let tested = codecommit(
        &router,
        case,
        "TestRepositoryTriggers",
        &json!({"repositoryName": repository, "triggers": trigger}),
    )
    .await;
    assert_eq!(
        tested["successfulExecutions"],
        json!(["everything"]),
        "case {case:?}: the test must report the trigger it sent to, got {tested}"
    );
    let probes = records_until(&router, case, &queue_url, 1).await;
    assert_eq!(
        probes.len(),
        1,
        "case {case:?}: the test sends one record, got {probes:?}"
    );
    assert_record(
        case,
        &probes[0],
        TRIGGER_EVENT_TEST,
        &repository_arn,
        &configuration_id,
        "refs/heads/main",
        &seed,
    );

    // The same trigger, the same destination, a reference that really moved: the record is
    // labelled as a reference change, which is how a consumer tells the two apart.
    let commit = put_file(&router, case, repository, "main", Some(&seed), "a revision").await;
    let changes = records_until(&router, case, &queue_url, 1).await;
    assert_eq!(
        changes.len(),
        1,
        "case {case:?}: the write runs the one trigger in scope, got {changes:?}"
    );
    assert_record(
        case,
        &changes[0],
        REFERENCE_CHANGES,
        &repository_arn,
        &configuration_id,
        "refs/heads/main",
        &commit,
    );
    assert_eq!(
        changes[0]["eventTriggerName"].as_str(),
        Some("everything"),
        "case {case:?}: the record must name the trigger that ran, got {}",
        changes[0]
    );
}

#[tokio::test]
async fn delivery_follows_each_subscribed_queues_own_policy() {
    let router = router();
    let case = "two queues of one topic, one scoped to another topic";
    let topic = given_topic(&router, case, "codecommit-scoped-delivery").await;
    // Another topic of the account, which the second queue's policy names instead.
    let other_topic = given_topic(&router, case, "codecommit-another-topic").await;
    let (scoped_url, scoped_arn) = given_queue(&router, case, "codecommit-scoped-inbox").await;
    let (denied_url, denied_arn) = given_queue(&router, case, "codecommit-denied-inbox").await;
    set_queue_policy(&router, case, &scoped_url, &grant_to(&scoped_arn, &topic)).await;
    set_queue_policy(
        &router,
        case,
        &denied_url,
        &grant_to(&denied_arn, &other_topic),
    )
    .await;
    subscribe(&router, case, &topic, &scoped_arn).await;
    subscribe(&router, case, &topic, &denied_arn).await;

    let repository = "scoped-delivery";
    let repository_arn = create_repository(&router, case, repository).await;
    let seed = put_file(&router, case, repository, "main", None, "seed").await;
    let configuration_id = put_triggers(
        &router,
        case,
        repository,
        &json!([{
            "name": "main-updates",
            "destinationArn": topic,
            "branches": ["main"],
            "events": ["updateReference"],
        }]),
    )
    .await;

    // Two writes on the branch the trigger lists.
    let first = put_file(&router, case, repository, "main", Some(&seed), "first").await;
    let second = put_file(&router, case, repository, "main", Some(&first), "second").await;

    let delivered = records_until(&router, case, &scoped_url, 2).await;
    let mut commits: Vec<String> = delivered
        .iter()
        .map(|record| sole_reference(case, record).1)
        .collect();
    commits.sort();
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort();
    assert_eq!(
        commits, expected,
        "case {case:?}: the correctly scoped queue must receive both records, got {delivered:?}"
    );
    for record in &delivered {
        assert_eq!(
            record["eventName"].as_str(),
            Some(REFERENCE_CHANGES),
            "case {case:?}: got {record}"
        );
        assert_eq!(
            record["eventSourceARN"].as_str(),
            Some(repository_arn.as_str()),
            "case {case:?}: got {record}"
        );
        assert_eq!(
            record["eventTriggerConfigId"].as_str(),
            Some(configuration_id.as_str()),
            "case {case:?}: got {record}"
        );
    }

    // The queue whose policy admits another topic received nothing, and is still
    // subscribed: what withheld the delivery is its policy, not its subscription.
    let withheld = drain(&router, case, &denied_url).await;
    assert!(
        withheld.is_empty(),
        "case {case:?}: a queue whose policy does not allow the publishing topic receives \
         nothing, got {withheld:?}"
    );
    let endpoints = subscribed_endpoints(&router, case, &topic).await;
    assert!(
        endpoints.contains(&denied_arn),
        "case {case:?}: the queue must stay subscribed, got {endpoints:?}"
    );

    // Repaired, it receives the next record.
    set_queue_policy(&router, case, &denied_url, &grant_to(&denied_arn, &topic)).await;
    let third = put_file(&router, case, repository, "main", Some(&second), "third").await;
    let repaired = records_until(&router, case, &denied_url, 1).await;
    assert_eq!(
        repaired.len(),
        1,
        "case {case:?}: the repaired queue must receive the next record, got {repaired:?}"
    );
    assert_record(
        case,
        &repaired[0],
        REFERENCE_CHANGES,
        &repository_arn,
        &configuration_id,
        "refs/heads/main",
        &third,
    );
}
