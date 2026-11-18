// SPDX-License-Identifier: CC0-1.0

//! A CodeCommit write that moves a reference reaches an EventBridge rule's Amazon SQS
//! target on the emulator a client STARTS: the built binary, on its own port, with request
//! authentication on, reached over TLS with SigV4-signed calls.
//!
//! The RULE under test: **an accepted CodeCommit write that leaves a branch reference
//! pointing somewhere new publishes one event onto the account's default EventBridge bus as
//! `source` `aws.codecommit` / `detail-type` `CodeCommit Repository State Change`, naming
//! the repository's ARN in `resources` and, in `detail`, the class of the write, the
//! reference and the commit the write's own answer reported; a call the service refuses
//! moves no reference and publishes nothing.**
//!
//! [`crate::codecommit_reference_write_publishes_a_repository_state_change`] pins that rule
//! over a router assembled inside the test process, with authentication off, on a runtime
//! the test chose. The emulator a client runs is a different configuration of the same
//! code: the publication CodeCommit makes is signed with the credentials THAT process
//! vends, EventBridge's delivery to the queue is authorized by the queue's own policy, and
//! both happen on the binary's own multi-thread runtime, where the write's publication is
//! handed to a task of its own. This suite is the one that pins the chain THERE, so an
//! event that is published in-process and not on the started emulator is a failure a test
//! reports rather than one only a client sees.
//!
//! The legs span the two halves of the rule: the accepted write that creates the branch and
//! the accepted fast-forward that moves it, each read off the queue, and then the write
//! refused for a parent the branch has moved past, whose silence is read against the
//! accepted write that follows it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::element;

/// The Region the started emulator's calls are signed for, which is the `region` the
/// envelopes must report. It must stay the one [`Started`] signs with.
const REGION: &str = "us-east-1";

/// What the envelope says about itself, spelled out here rather than imported from the
/// service crate, so a wrong constant in the implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.codecommit";
const REPOSITORY_STATE_CHANGE: &str = "CodeCommit Repository State Change";
const REFERENCE_CREATED: &str = "referenceCreated";
const REFERENCE_UPDATED: &str = "referenceUpdated";
const REFERENCE_TYPE_BRANCH: &str = "branch";
const REFS_HEADS: &str = "refs/heads/";

/// The service principal EventBridge delivers to a queue target as, which is what the
/// queue's own resource policy has to allow.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The repository, the branch and the file every leg writes.
const REPOSITORY: &str = "started-emulator-reference-events";
const BRANCH: &str = "main";
const FILE: &str = "README.md";

/// How long a leg waits for an envelope to arrive, and how often it looks.
///
/// Generous on purpose: the wait is on the ENVELOPE, looked for again until it is there, so
/// a fast machine never waits the whole span and a loaded one is given room to spend.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// One `X-Amz-Target` RPC call over the started emulator's signed-call plumbing, with
/// whatever status it answered.
fn rpc(
    emulator: &Started,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, text) = emulator.rpc(signing_name, target, content_type, body);
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, parsed)
}

/// The same call, which must answer 200.
fn ok(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(emulator, signing_name, target, content_type, body);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {parsed}; the \
         log:\n{}",
        emulator.log()
    );
    parsed
}

/// One CodeCommit call that must answer 200.
fn codecommit(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    ok(
        emulator,
        case,
        "codecommit",
        &format!("CodeCommit_20150413.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One EventBridge call that must answer 200.
fn events(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    ok(
        emulator,
        case,
        "events",
        &format!("AWSEvents.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One Amazon SQS call that must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    ok(
        emulator,
        case,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
}

/// A `PutFile` on `BRANCH`, with whatever status it answered.
fn put_file(
    emulator: &Started,
    parent: Option<&str>,
    content: &str,
) -> (u16, Value) {
    let mut body = json!({
        "repositoryName": REPOSITORY,
        "branchName": BRANCH,
        "filePath": FILE,
        "fileContent": ::aws_smithy_types::base64::encode(content.as_bytes()),
        "commitMessage": "a revision",
    });
    if let Some(parent) = parent {
        body["parentCommitId"] = json!(parent);
    }
    rpc(
        emulator,
        "codecommit",
        "CodeCommit_20150413.PutFile",
        "application/x-amz-json-1.1",
        &body,
    )
}

/// The same write, which must be accepted; answers the commit id it reported.
fn accepted_put_file(
    emulator: &Started,
    case: &str,
    parent: Option<&str>,
    content: &str,
) -> String {
    let (status, written) = put_file(emulator, parent, content);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutFile, got {status} with {written}; the \
         log:\n{}",
        emulator.log()
    );
    written["commitId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no commitId in {written}"))
        .to_string()
}

/// Every envelope waiting on the queue at `url`, deleting what it hands over.
fn drain(emulator: &Started, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not an event envelope ({error}): {body}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
    }
    envelopes
}

/// Poll the queue at `url` until one envelope has arrived, failing the case at the deadline.
fn delivered_one(emulator: &Started, case: &str, url: &str) -> Value {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let envelopes = drain(emulator, case, url);
        if let Some(envelope) = envelopes.into_iter().next() {
            return envelope;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected an envelope on {url:?} within {DELIVERY_DEADLINE:?}; the \
             log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Assert the envelope's own members and the `detail` members this suite reads.
fn assert_envelope(
    case: &str,
    envelope: &Value,
    repository_arn: &str,
    event: &str,
    commit_id: &str,
    old_commit_id: Option<&str>,
    caller_arn: &str,
) {
    for (member, expected) in [
        ("source", EVENT_SOURCE.to_string()),
        ("detail-type", REPOSITORY_STATE_CHANGE.to_string()),
        ("region", REGION.to_string()),
    ] {
        assert_eq!(
            envelope[member].as_str(),
            Some(expected.as_str()),
            "case {case:?}: {member} must be {expected:?}, got {envelope}"
        );
    }
    assert_eq!(
        envelope["resources"],
        json!([repository_arn]),
        "case {case:?}: the repository ARN must be the one resource, got {envelope}"
    );
    let detail = &envelope["detail"];
    for (member, expected) in [
        ("event", event.to_string()),
        ("referenceType", REFERENCE_TYPE_BRANCH.to_string()),
        ("referenceName", BRANCH.to_string()),
        ("referenceFullName", format!("{REFS_HEADS}{BRANCH}")),
        ("commitId", commit_id.to_string()),
        ("repositoryName", REPOSITORY.to_string()),
        ("callerUserArn", caller_arn.to_string()),
    ] {
        assert_eq!(
            detail[member].as_str(),
            Some(expected.as_str()),
            "case {case:?}: detail.{member} must be {expected:?}, got {envelope}"
        );
    }
    assert_eq!(
        detail["oldCommitId"].as_str(),
        old_commit_id,
        "case {case:?}: detail.oldCommitId must be {old_commit_id:?}, got {envelope}"
    );
}

/// RULE: on the emulator a client starts, an accepted CodeCommit write that moves a
/// reference is published onto the default bus and delivered to the SQS target of a rule
/// matching it, and a write refused for a stale parent publishes nothing.
#[test]
fn a_reference_write_publishes_its_state_change_on_the_emulator_a_client_starts() {
    let setup = "the wiring on a started emulator";
    let emulator = Started::start();

    let created = codecommit(
        &emulator,
        setup,
        "CreateRepository",
        &json!({ "repositoryName": REPOSITORY }),
    );
    let repository_arn = created["repositoryMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {setup:?}: no repositoryMetadata.Arn in {created}"))
        .to_string();

    let queue = sqs(
        &emulator,
        setup,
        "CreateQueue",
        &json!({ "QueueName": "started-emulator-codecommit-events" }),
    );
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {setup:?}: no QueueUrl in {queue}"))
        .to_string();
    let attributes = sqs(
        &emulator,
        setup,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &queue_url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {setup:?}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &queue_arn,
        }],
    })
    .to_string();
    sqs(
        &emulator,
        setup,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &queue_url, "Attributes": { "Policy": policy } }),
    );

    let pattern = json!({
        "source": [EVENT_SOURCE],
        "detail-type": [REPOSITORY_STATE_CHANGE],
        "resources": [&repository_arn],
    });
    events(
        &emulator,
        setup,
        "PutRule",
        &json!({
            "Name": "started-emulator-codecommit-events",
            "State": "ENABLED",
            "EventPattern": pattern.to_string(),
        }),
    );
    let targets = events(
        &emulator,
        setup,
        "PutTargets",
        &json!({
            "Rule": "started-emulator-codecommit-events",
            "Targets": [{ "Id": "sqs-target", "Arn": &queue_arn }],
        }),
    );
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "case {setup:?}: PutTargets reported a failed entry: {targets}"
    );

    // The identity the events must name as the one whose call moved the reference.
    let (status, identity) = emulator.form_call("sts", "2011-06-15", "GetCallerIdentity", &[]);
    assert_eq!(
        status, 200,
        "case {setup:?}: expected HTTP 200 from STS GetCallerIdentity, got {status} with \
         {identity}"
    );
    let caller = element(&identity, "Arn");

    // The branch-creating write: the positive control for the whole chain.
    let created_case = "the write that creates the branch";
    let seed = accepted_put_file(&emulator, created_case, None, "seed\n");
    let envelope = delivered_one(&emulator, created_case, &queue_url);
    assert_envelope(
        created_case,
        &envelope,
        &repository_arn,
        REFERENCE_CREATED,
        &seed,
        None,
        &caller,
    );

    // A fast-forward write, which moves the reference it already held.
    let updated_case = "a fast-forward write that moves the branch";
    let tip = accepted_put_file(&emulator, updated_case, Some(&seed), "the current tip\n");
    let envelope = delivered_one(&emulator, updated_case, &queue_url);
    assert_envelope(
        updated_case,
        &envelope,
        &repository_arn,
        REFERENCE_UPDATED,
        &tip,
        Some(&seed),
        &caller,
    );

    // A write naming the parent the branch has already left behind is refused.
    let refused_case = "a write naming a parent the branch has moved past";
    let (status, refused) = put_file(&emulator, Some(&seed), "content offered twice\n");
    assert_eq!(
        status, 400,
        "case {refused_case:?}: a stale parentCommitId must be refused, got {status} with \
         {refused}; the log:\n{}",
        emulator.log()
    );
    let code = refused["__type"].as_str().unwrap_or_default();
    assert!(
        code.ends_with("ParentCommitIdOutdatedException"),
        "case {refused_case:?}: expected ParentCommitIdOutdatedException, got {code:?} with \
         {refused}"
    );

    // The same content against the CURRENT tip is accepted, and its envelope is the only one
    // that arrives: the refused write moved no reference, so it published nothing.
    let released_case = "the same content committed against the current tip";
    let released = accepted_put_file(
        &emulator,
        released_case,
        Some(&tip),
        "content offered twice\n",
    );
    let envelope = delivered_one(&emulator, released_case, &queue_url);
    assert_envelope(
        released_case,
        &envelope,
        &repository_arn,
        REFERENCE_UPDATED,
        &released,
        Some(&tip),
        &caller,
    );
    let nothing = drain(&emulator, refused_case, &queue_url);
    assert!(
        nothing.is_empty(),
        "case {refused_case:?}: the refused write must publish nothing, got {nothing:?}"
    );
}
