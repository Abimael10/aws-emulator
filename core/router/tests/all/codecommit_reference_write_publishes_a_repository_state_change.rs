// SPDX-License-Identifier: CC0-1.0

//! An accepted CodeCommit write that moves a reference reaches an EventBridge rule's
//! Amazon SQS target — through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **a CodeCommit write that is ACCEPTED and leaves a branch
//! reference pointing somewhere new publishes one event onto the account's DEFAULT
//! EventBridge bus, with no opt-in and no trigger configuration, as `source`
//! `aws.codecommit` with `detail-type` `CodeCommit Repository State Change`, carrying the
//! repository's ARN as the only entry of `resources` and a `detail` that names the class
//! of the write (`referenceCreated` for a reference the repository did not hold,
//! `referenceUpdated` for one that moved, `referenceDeleted` for one it no longer holds),
//! the reference both as the BARE name the API takes and as the fully qualified ref a git
//! client sees, `referenceType` `branch`, the commit the reference now points at — the
//! id that same call's own response reported — the commit it pointed at before for a
//! write on a reference that already existed, the repository's stored id and name, and
//! the ARN of the identity whose call made it. EventBridge routes the envelope by its
//! CONTENT, so a rule whose `EventPattern` matches hands it unchanged to its targets and
//! a rule pinned to one repository's ARN is handed that repository's events alone. A call
//! the service REFUSES moves no reference, so it publishes nothing.**
//!
//! Source for the envelope: the CodeCommit EventBridge events, which the Smithy model does
//! not describe — it models the control plane only
//! (`smithy_query(query="operation", operation="PutFile")` models the synchronous
//! response). So every `detail` member asserted here is asserted against what the SAME
//! write's control-plane response, or a later read of the same repository, reported for it,
//! never against a value this suite put there: the commit ids come from each write's own
//! answer, the repository id and ARN from `CreateRepository`, and the caller ARN from
//! `sts:GetCallerIdentity` under the credential the writes are signed with.
//!
//! Three crates have to agree for the rule to hold — CodeCommit publishes the write,
//! EventBridge matches the envelope against each installed pattern and hands it over, and
//! Amazon SQS holds what was handed to it — so the suite lives here rather than in any one
//! of them.
//!
//! The rows differ along exactly what the rule generalises over: the CLASS of the write
//! (a reference created, moved, removed) and the operation that makes it (`PutFile` with
//! no parent and as a fast-forward, `CreateBranch`, `CreateCommit`, `DeleteFile`,
//! `DeleteBranch`), whether the reference existed before (and so whether the envelope
//! names a commit it pointed at before), and which repository the write is on.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::element;

/// The Region the whole wiring lives in: the repository, the bus, the rule and its queue.
const REGION: &str = "us-west-2";

/// CodeCommit's `X-Amz-Target` prefix and its signing name.
const CODECOMMIT_TARGET_PREFIX: &str = "CodeCommit_20150413";
const CODECOMMIT_SIGNING_NAME: &str = "codecommit";

/// What the envelope says about itself, spelled out here rather than imported from the
/// service crate, so a wrong constant in the implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.codecommit";
const REPOSITORY_STATE_CHANGE: &str = "CodeCommit Repository State Change";

/// The `detail.event` of each class of reference write.
const REFERENCE_CREATED: &str = "referenceCreated";
const REFERENCE_UPDATED: &str = "referenceUpdated";
const REFERENCE_DELETED: &str = "referenceDeleted";

/// The `detail.referenceType` of a reference under `refs/heads/`.
const REFERENCE_TYPE_BRANCH: &str = "branch";

/// The prefix that qualifies a branch name as a git reference.
const REFS_HEADS: &str = "refs/heads/";

/// git's null object name: the value a reference update carries for a reference that
/// points at nothing, which is what the event of a REMOVED reference names.
const NULL_COMMIT: &str = "0000000000000000000000000000000000000000";

/// The service principal EventBridge delivers to a queue target as, which is what that
/// queue's own resource policy has to allow: "for Lambda functions, Amazon SNS topics and
/// Amazon SQS queues, EventBridge relies on resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The branches the rows write, and the files they write on them.
const MAIN: &str = "main";
const RELEASE: &str = "release";
const FILE: &str = "README.md";
const EXTRA_FILE: &str = "extra.txt";

/// How many times a leg polls a queue before calling an envelope undelivered, and the long
/// poll each look makes.
///
/// Generous on purpose: together they bound a loop that ends as soon as the envelopes are
/// on the queue, so a fast machine never waits for the whole budget and a machine running
/// the whole workspace's tests at once is not failed by it.
const POLLS: usize = 30;
const WAIT_TIME_SECONDS: u32 = 1;

/// One `X-Amz-Target` RPC call, with whatever status it answered.
async fn rpc_answered(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
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
    (status, parsed)
}

/// The same call, which must answer 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc_answered(
        router,
        signing_name,
        target_prefix,
        content_type,
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CodeCommit call (`awsJson1_1`, `CodeCommit_20150413.<Operation>`) that must answer
/// 200.
async fn codecommit(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
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

/// One CodeCommit call with whatever status it answered: what a REFUSED write is read by.
async fn codecommit_answered(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc_answered(
        router,
        CODECOMMIT_SIGNING_NAME,
        CODECOMMIT_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "events",
        "AWSEvents",
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
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

/// The ARN `sts:GetCallerIdentity` answers for the credential every write below is signed
/// with: the identity the event must name as the one whose call moved the reference.
async fn caller_arn(router: &MultiServiceRouter, case: &str) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sts.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sts", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from("Action=GetCallerIdentity&Version=2011-06-15"))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from STS GetCallerIdentity, got {status} with {body}"
    );
    element(&body, "Arn")
}

/// A queue called `name`, carrying the grant a rule's target needs; answers its url and the
/// ARN a rule names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// An ENABLED rule called `name` on the default bus whose `EventPattern` is `pattern`, with
/// the queue `target_arn` as its only target.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
    target_arn: &str,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({ "Name": name, "State": "ENABLED", "EventPattern": pattern.to_string() }),
    )
    .await;
    let targets = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "Targets": [{ "Id": format!("{name}-target"), "Arn": target_arn }],
        }),
    )
    .await;
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {targets}"
    );
}

/// The pattern a rule for this service's repository events carries, optionally pinned to
/// one repository's ARN.
fn pattern(repository_arn: Option<&str>) -> Value {
    let mut pattern = json!({
        "source": [EVENT_SOURCE],
        "detail-type": [REPOSITORY_STATE_CHANGE],
    });
    if let Some(repository_arn) = repository_arn {
        pattern["resources"] = json!([repository_arn]);
    }
    pattern
}

/// Every envelope waiting on the queue at `url`, deleting what it hands over so a later
/// look reads only what arrived since.
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
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the message body is not an event envelope ({error}): {body}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
    }
    envelopes
}

/// Poll the queue at `url` until `wanted` envelopes have arrived or the polls run out.
///
/// The leg waits for the ENVELOPES rather than for a span: one not delivered yet is looked
/// for again, and one that is never delivered leaves the collection short, which is what
/// the caller asserts on.
async fn delivered(
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

/// A repository of this emulator; answers its ARN and the `repositoryId` the create
/// reported, which the event has to name.
async fn given_repository(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = codecommit(
        router,
        case,
        "CreateRepository",
        &json!({ "repositoryName": name }),
    )
    .await;
    let metadata = &created["repositoryMetadata"];
    (
        metadata["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no repositoryMetadata.Arn in {created}"))
            .to_string(),
        metadata["repositoryId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no repositoryMetadata.repositoryId in {created}")
            })
            .to_string(),
    )
}

/// Write `content` at `FILE` on `branch` through `PutFile`, answering the commit id the
/// call reported.
async fn put_file(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    branch: &str,
    parent: Option<&str>,
    content: &str,
) -> String {
    let (status, written) = put_file_answered(router, repository, branch, parent, content).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CodeCommit PutFile, got {status} with {written}"
    );
    written["commitId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no commitId in {written}"))
        .to_string()
}

/// The same `PutFile`, with whatever status it answered.
async fn put_file_answered(
    router: &MultiServiceRouter,
    repository: &str,
    branch: &str,
    parent: Option<&str>,
    content: &str,
) -> (u16, Value) {
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
    codecommit_answered(router, "PutFile", &body).await
}

/// What one row writes.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// The repository's FIRST commit, through `PutFile` with no parent: the write that
    /// creates the branch.
    Seed(&'static str),
    /// A fast-forward commit on a branch that already has one, through `PutFile`.
    PutFile(&'static str),
    /// A reference the repository did not hold, at another branch's tip, through
    /// `CreateBranch`.
    CreateBranch {
        branch: &'static str,
        from: &'static str,
    },
    /// A commit on a branch adding [`EXTRA_FILE`], through `CreateCommit`.
    CreateCommit(&'static str),
    /// A commit on a branch removing [`FILE`], through `DeleteFile`.
    DeleteFile(&'static str),
    /// A reference the repository no longer holds, through `DeleteBranch`.
    DeleteBranch(&'static str),
}

impl Write {
    /// The branch this write writes.
    fn branch(self) -> &'static str {
        match self {
            Self::Seed(branch)
            | Self::PutFile(branch)
            | Self::CreateBranch { branch, .. }
            | Self::CreateCommit(branch)
            | Self::DeleteFile(branch)
            | Self::DeleteBranch(branch) => branch,
        }
    }
}

/// Make one write, answering where the reference pointed BEFORE it (`None` for a reference
/// the repository did not hold) and where it points after — both read off the writes' own
/// answers, which is what the envelope is then asserted against.
async fn perform(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    write: Write,
    revision: usize,
    tips: &mut BTreeMap<&'static str, String>,
) -> (Option<String>, String) {
    let branch = write.branch();
    let before = tips.get(branch).cloned();
    let after = match write {
        Write::Seed(branch) => put_file(router, case, repository, branch, None, "seed\n").await,
        Write::PutFile(branch) => {
            let parent = before
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: {branch} has no tip to move from"));
            put_file(
                router,
                case,
                repository,
                branch,
                Some(&parent),
                &format!("revision {revision}\n"),
            )
            .await
        }
        Write::CreateBranch { branch, from } => {
            let commit = tips
                .get(from)
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: {from} has no tip to branch from"));
            codecommit(
                router,
                case,
                "CreateBranch",
                &json!({
                    "repositoryName": repository,
                    "branchName": branch,
                    "commitId": commit,
                }),
            )
            .await;
            commit
        }
        Write::CreateCommit(branch) => {
            let parent = before
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: {branch} has no tip to commit on"));
            let written = codecommit(
                router,
                case,
                "CreateCommit",
                &json!({
                    "repositoryName": repository,
                    "branchName": branch,
                    "parentCommitId": parent,
                    "commitMessage": "another file",
                    "putFiles": [{
                        "filePath": EXTRA_FILE,
                        "fileContent": ::aws_smithy_types::base64::encode(b"extra\n"),
                    }],
                }),
            )
            .await;
            written["commitId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no commitId in {written}"))
                .to_string()
        }
        Write::DeleteFile(branch) => {
            let parent = before
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: {branch} has no tip to delete from"));
            let written = codecommit(
                router,
                case,
                "DeleteFile",
                &json!({
                    "repositoryName": repository,
                    "branchName": branch,
                    "filePath": FILE,
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
        Write::DeleteBranch(branch) => {
            codecommit(
                router,
                case,
                "DeleteBranch",
                &json!({ "repositoryName": repository, "branchName": branch }),
            )
            .await;
            NULL_COMMIT.to_string()
        }
    };
    match write {
        Write::DeleteBranch(branch) => {
            tips.remove(branch);
        }
        _ => {
            tips.insert(branch, after.clone());
        }
    }
    (before, after)
}

/// What ONE write's envelope has to say.
struct Expected<'a> {
    case: &'a str,
    event: &'a str,
    branch: &'a str,
    commit_id: &'a str,
    old_commit_id: Option<&'a str>,
    repository: &'a str,
    repository_id: &'a str,
    repository_arn: &'a str,
    caller_arn: &'a str,
}

/// Assert the envelope and its `detail`, member by member.
fn assert_envelope(envelope: &Value, expected: &Expected<'_>) {
    let case = expected.case;
    for (member, value) in [
        ("source", EVENT_SOURCE.to_string()),
        ("detail-type", REPOSITORY_STATE_CHANGE.to_string()),
        ("account", engine::emulator_account_id().to_string()),
        ("region", REGION.to_string()),
    ] {
        assert_eq!(
            envelope[member].as_str(),
            Some(value.as_str()),
            "case {case:?}: {member} must be {value:?}, got {envelope}"
        );
    }
    assert_eq!(
        envelope["resources"],
        json!([expected.repository_arn]),
        "case {case:?}: the repository ARN must be the one resource, got {envelope}"
    );
    assert!(
        envelope["time"].is_string(),
        "case {case:?}: the envelope must be stamped with when the write happened, got {envelope}"
    );
    let detail = &envelope["detail"];
    let qualified = format!("{REFS_HEADS}{}", expected.branch);
    for (member, value) in [
        ("event", expected.event.to_string()),
        ("referenceType", REFERENCE_TYPE_BRANCH.to_string()),
        ("referenceName", expected.branch.to_string()),
        ("referenceFullName", qualified),
        ("commitId", expected.commit_id.to_string()),
        ("repositoryName", expected.repository.to_string()),
        ("repositoryId", expected.repository_id.to_string()),
        ("callerUserArn", expected.caller_arn.to_string()),
    ] {
        assert_eq!(
            detail[member].as_str(),
            Some(value.as_str()),
            "case {case:?}: detail.{member} must be {value:?}, got {envelope}"
        );
    }
    assert_eq!(
        detail["oldCommitId"].as_str(),
        expected.old_commit_id,
        "case {case:?}: detail.oldCommitId must be {:?}, got {envelope}",
        expected.old_commit_id
    );
}

#[tokio::test]
async fn every_accepted_reference_write_publishes_its_repository_state_change() {
    let router = router();
    let setup = "the wiring every row reads through";
    let repository = "reference-state-change-events";
    let (repository_arn, repository_id) = given_repository(&router, setup, repository).await;
    let (queue_url, queue_arn) = given_queue(&router, setup, "codecommit-repository-events").await;
    given_rule(
        &router,
        setup,
        "codecommit-repository-events",
        &pattern(Some(&repository_arn)),
        &queue_arn,
    )
    .await;
    let caller = caller_arn(&router, setup).await;

    // (case, the write, the class it must be published as)
    let rows: &[(&str, Write, &str)] = &[
        (
            "the repository's first commit, which creates its branch",
            Write::Seed(MAIN),
            REFERENCE_CREATED,
        ),
        (
            "a fast-forward commit through PutFile, which moves that branch",
            Write::PutFile(MAIN),
            REFERENCE_UPDATED,
        ),
        (
            "a branch created at another branch's tip through CreateBranch",
            Write::CreateBranch {
                branch: RELEASE,
                from: MAIN,
            },
            REFERENCE_CREATED,
        ),
        (
            "a commit written through CreateCommit",
            Write::CreateCommit(RELEASE),
            REFERENCE_UPDATED,
        ),
        (
            "a file removed through DeleteFile",
            Write::DeleteFile(RELEASE),
            REFERENCE_UPDATED,
        ),
        (
            "a branch removed through DeleteBranch",
            Write::DeleteBranch(RELEASE),
            REFERENCE_DELETED,
        ),
    ];

    let mut tips: BTreeMap<&'static str, String> = BTreeMap::new();
    for (revision, (case, write, event)) in rows.iter().enumerate() {
        let (before, after) =
            perform(&router, case, repository, *write, revision, &mut tips).await;
        let envelopes = delivered(&router, case, &queue_url, 1).await;
        assert_eq!(
            envelopes.len(),
            1,
            "case {case:?}: the accepted write must publish exactly one envelope, got \
             {envelopes:?}"
        );
        assert_envelope(
            &envelopes[0],
            &Expected {
                case,
                event,
                branch: write.branch(),
                commit_id: &after,
                old_commit_id: before.as_deref(),
                repository,
                repository_id: &repository_id,
                repository_arn: &repository_arn,
                caller_arn: &caller,
            },
        );
    }
}

#[tokio::test]
async fn a_write_refused_for_a_stale_parent_publishes_nothing() {
    let router = router();
    let setup = "the wiring the refusal is read through";
    let repository = "stale-parent-publishes-nothing";
    let (repository_arn, repository_id) = given_repository(&router, setup, repository).await;
    let (queue_url, queue_arn) = given_queue(&router, setup, "codecommit-stale-parent").await;
    given_rule(
        &router,
        setup,
        "codecommit-stale-parent",
        &pattern(Some(&repository_arn)),
        &queue_arn,
    )
    .await;
    let caller = caller_arn(&router, setup).await;

    // Two accepted writes, so the branch has a tip and a tip it has already left behind.
    let seed = put_file(&router, setup, repository, MAIN, None, "seed\n").await;
    let tip = put_file(
        &router,
        setup,
        repository,
        MAIN,
        Some(&seed),
        "the current tip\n",
    )
    .await;
    let accepted = delivered(&router, setup, &queue_url, 2).await;
    assert_eq!(
        accepted.len(),
        2,
        "case {setup:?}: the two accepted writes must publish two envelopes, got {accepted:?}"
    );

    // The same content, against the parent the branch has already left behind.
    let case = "a write naming a parent the branch has moved past";
    let (status, refused) = put_file_answered(
        &router,
        repository,
        MAIN,
        Some(&seed),
        "content offered twice\n",
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a stale parentCommitId must be refused, got {status} with {refused}"
    );
    let code = refused["__type"].as_str().unwrap_or_default();
    assert!(
        code.ends_with("ParentCommitIdOutdatedException"),
        "case {case:?}: expected ParentCommitIdOutdatedException, got {code:?} with {refused}"
    );

    // The same content against the CURRENT tip is accepted, and its envelope is the only
    // one on the queue: the refused call moved no reference, so it published nothing.
    let released = "the same content committed against the current tip";
    let after = put_file(
        &router,
        released,
        repository,
        MAIN,
        Some(&tip),
        "content offered twice\n",
    )
    .await;
    let envelopes = delivered(&router, released, &queue_url, 1).await;
    assert_eq!(
        envelopes.len(),
        1,
        "case {released:?}: exactly the accepted write's envelope must arrive, got {envelopes:?}"
    );
    assert_envelope(
        &envelopes[0],
        &Expected {
            case: released,
            event: REFERENCE_UPDATED,
            branch: MAIN,
            commit_id: &after,
            old_commit_id: Some(&tip),
            repository,
            repository_id: &repository_id,
            repository_arn: &repository_arn,
            caller_arn: &caller,
        },
    );
    let nothing = drain(&router, case, &queue_url).await;
    assert!(
        nothing.is_empty(),
        "case {case:?}: the refused write must publish nothing, got {nothing:?}"
    );
}

#[tokio::test]
async fn a_rule_pinned_to_one_repository_is_handed_that_repositorys_writes_alone() {
    let router = router();
    let setup = "two repositories behind one bus";
    let subject = "reference-events-subject";
    let other = "reference-events-other";
    let (subject_arn, subject_id) = given_repository(&router, setup, subject).await;
    let (other_arn, _) = given_repository(&router, setup, other).await;
    // One rule pinned to the subject's ARN, and a CONTROL rule on the source and
    // detail-type alone: the control's envelopes are what say both publications really
    // happened, so the scoped queue holding one of them is a refusal to deliver rather
    // than a look taken too early.
    let (scoped_url, scoped_arn) = given_queue(&router, setup, "codecommit-scoped-events").await;
    given_rule(
        &router,
        setup,
        "codecommit-scoped-events",
        &pattern(Some(&subject_arn)),
        &scoped_arn,
    )
    .await;
    let (control_url, control_arn) = given_queue(&router, setup, "codecommit-all-events").await;
    given_rule(
        &router,
        setup,
        "codecommit-all-events",
        &pattern(None),
        &control_arn,
    )
    .await;
    let caller = caller_arn(&router, setup).await;

    let other_commit = put_file(&router, setup, other, MAIN, None, "elsewhere\n").await;
    let subject_commit = put_file(&router, setup, subject, MAIN, None, "here\n").await;

    let control = delivered(&router, setup, &control_url, 2).await;
    assert_eq!(
        control.len(),
        2,
        "case {setup:?}: both repositories' writes must be published, got {control:?}"
    );
    let mut published: Vec<(String, String)> = control
        .iter()
        .map(|envelope| {
            (
                envelope["resources"][0].as_str().unwrap_or_default().to_string(),
                envelope["detail"]["commitId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    published.sort();
    let mut expected = vec![
        (other_arn.clone(), other_commit.clone()),
        (subject_arn.clone(), subject_commit.clone()),
    ];
    expected.sort();
    assert_eq!(
        published, expected,
        "case {setup:?}: the control rule must be handed each repository's own write, got \
         {control:?}"
    );

    let case = "a rule pinned to one repository's ARN";
    let scoped = delivered(&router, case, &scoped_url, 1).await;
    assert_eq!(
        scoped.len(),
        1,
        "case {case:?}: only the pinned repository's write must be delivered, got {scoped:?}"
    );
    assert_envelope(
        &scoped[0],
        &Expected {
            case,
            event: REFERENCE_CREATED,
            branch: MAIN,
            commit_id: &subject_commit,
            old_commit_id: None,
            repository: subject,
            repository_id: &subject_id,
            repository_arn: &subject_arn,
            caller_arn: &caller,
        },
    );
    let nothing = drain(&router, case, &scoped_url).await;
    assert!(
        nothing.is_empty(),
        "case {case:?}: a rule pinned to {subject_arn} must not be handed another \
         repository's write, got {nothing:?}"
    );
}