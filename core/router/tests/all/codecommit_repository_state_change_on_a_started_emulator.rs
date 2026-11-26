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
//!
//! The whole scenario is one routine ([`reference_writes_reach_their_queue`]) parameterised
//! over WHERE a client addresses its calls ([`Reached`]) and how long its receives wait, so
//! a second addressing is a second row rather than a second copy: the sibling suite
//! [`crate::codecommit_reference_event_reaches_a_client_pointed_at_the_emulator`] drives the
//! same legs from the position an SDK configured with this emulator's `endpoint_url` is in.

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

/// The instant no delivered envelope names: an envelope whose `time` opens with this is one
/// whose publication instant was lost rather than carried, which a consumer filtering the
/// events of its own run by `time` reads as an event from another era.
const EPOCH_YEAR: &str = "1970";

/// The service principal EventBridge delivers to a queue target as, which is what the
/// queue's own resource policy has to allow.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The repository, the branch and the file every leg writes, and the queue and rule the
/// envelopes are read off.
const REPOSITORY: &str = "started-emulator-reference-events";
const BRANCH: &str = "main";
const FILE: &str = "README.md";
const QUEUE: &str = "started-emulator-codecommit-events";
const RULE: &str = "started-emulator-codecommit-events";

/// The two spans a receive may wait: "the duration (in seconds) for which the call waits for
/// a message to arrive in the queue before returning. If a message is available, the call
/// returns sooner" (`ReceiveMessageRequest$WaitTimeSeconds`, an integer from 0 to 20). A
/// short poll answers with what the queue holds at that instant; a long poll is the wait a
/// consumer of an event queue makes.
pub(crate) const SHORT_POLL_SECONDS: u32 = 0;
/// See [`SHORT_POLL_SECONDS`].
pub(crate) const LONG_POLL_SECONDS: u32 = 5;

/// The window a published envelope must ARRIVE inside, which is the limit these legs test:
/// the published ceiling of the wait a consumer's own receive may ask for
/// (`ReceiveMessageRequest$WaitTimeSeconds`, "an integer from 0 to 20"). An envelope that
/// takes longer than one such poll is one a consumer polling the queue never sees, so a
/// delivery that is merely SLOW fails these legs rather than passing them — while the wait
/// is still on the ENVELOPE, looked for again until it is there, so a fast machine never
/// spends the window and nothing here is asserted about how long a delivery took.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(20);
/// How often a leg looks again inside [`DELIVERY_DEADLINE`].
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// WHERE a client addresses the emulator, which is the one thing the two suites driving
/// these legs differ in.
///
/// A stock SDK resolving AWS's own names carries a service host; one configured with this
/// emulator's `endpoint_url` carries the authority this process answers on, so every Region
/// such a call decides comes from its SigV4 credential scope alone and every address the
/// emulator mints for it — a queue url among them — is minted from that authority.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Reached {
    /// `<signing name>.<region>.amazonaws.com`, as a client resolving AWS's names carries.
    ServiceHost,
    /// `127.0.0.1:<port>`, as a client handed this emulator's endpoint url carries.
    EmulatorAuthority,
}

/// One `X-Amz-Target` RPC call over the started emulator's signed-call plumbing, with
/// whatever status it answered.
fn rpc(
    emulator: &Started,
    reached: Reached,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> (u16, Value) {
    let headers = [("content-type", content_type), ("x-amz-target", target)];
    let sent = body.to_string().into_bytes();
    let (status, text) = call(emulator, reached, signing_name, &headers, sent);
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, parsed)
}

/// One signed call, addressed as `reached` says.
fn call(
    emulator: &Started,
    reached: Reached,
    signing_name: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, String) {
    match reached {
        Reached::ServiceHost => emulator.call(signing_name, "POST", "/", headers, body),
        Reached::EmulatorAuthority => emulator
            .call_at(
                &emulator.authority(),
                signing_name,
                "POST",
                "/",
                headers,
                body,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "the emulator must answer {signing_name} at its own authority: {error}; \
                     its log:\n{}",
                    emulator.log()
                )
            }),
    }
}

/// The same call, which must answer 200.
fn ok(
    emulator: &Started,
    case: &str,
    reached: Reached,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(emulator, reached, signing_name, target, content_type, body);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {parsed}; the \
         log:\n{}",
        emulator.log()
    );
    parsed
}

/// One CodeCommit call that must answer 200.
fn codecommit(
    emulator: &Started,
    case: &str,
    reached: Reached,
    operation: &str,
    body: &Value,
) -> Value {
    ok(
        emulator,
        case,
        reached,
        "codecommit",
        &format!("CodeCommit_20150413.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One EventBridge call that must answer 200.
fn events(
    emulator: &Started,
    case: &str,
    reached: Reached,
    operation: &str,
    body: &Value,
) -> Value {
    ok(
        emulator,
        case,
        reached,
        "events",
        &format!("AWSEvents.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One Amazon SQS call that must answer 200.
fn sqs(
    emulator: &Started,
    case: &str,
    reached: Reached,
    operation: &str,
    body: &Value,
) -> Value {
    ok(
        emulator,
        case,
        reached,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
}

/// A `PutFile` on `BRANCH`, with whatever status it answered.
fn put_file(
    emulator: &Started,
    reached: Reached,
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
        reached,
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
    reached: Reached,
    parent: Option<&str>,
    content: &str,
) -> String {
    let (status, written) = put_file(emulator, reached, parent, content);
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

/// Every envelope waiting on the queue at `url`, deleting what it hands over so a later look
/// reads only what arrived since. The receive waits `wait` seconds for one to arrive.
fn drain(emulator: &Started, case: &str, reached: Reached, url: &str, wait: u32) -> Vec<Value> {
    let received = sqs(
        emulator,
        case,
        reached,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": wait }),
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
                reached,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
    }
    envelopes
}

/// Poll the queue at `url` until ONE envelope has arrived, failing the case at the deadline —
/// and failing it too when more than one did, since a write publishes exactly one event and
/// every envelope a look hands over is deleted: a second one in the same batch is a
/// publication this leg would otherwise discard unexamined.
fn delivered_one(emulator: &Started, case: &str, reached: Reached, url: &str, wait: u32) -> Value {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let envelopes = drain(emulator, case, reached, url, wait);
        assert!(
            envelopes.len() <= 1,
            "case {case:?}: expected one envelope on {url:?}, got {}: {envelopes:?}",
            envelopes.len()
        );
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

/// What the envelope of one write must say, read off this run's own calls.
struct Published<'a> {
    /// The ARN and the id `CreateRepository` reported for the repository the write was on.
    repository_arn: &'a str,
    repository_id: &'a str,
    /// The account `sts:GetCallerIdentity` reported, and the ARN of the identity the writes
    /// are signed with.
    account: &'a str,
    caller_arn: &'a str,
    /// The class of the write, the commit the reference now points at, and the commit it
    /// pointed at before where it had one.
    event: &'a str,
    commit_id: &'a str,
    old_commit_id: Option<&'a str>,
}

/// Assert the envelope's own members and the `detail` members this suite reads.
///
/// Every expected value is one this run's own control-plane calls reported — the commit ids
/// from each write's answer, the repository's ARN and id from `CreateRepository`, the account
/// and caller from `sts:GetCallerIdentity` — so nothing here is asserted against a value the
/// suite put there.
fn assert_envelope(case: &str, envelope: &Value, published: &Published<'_>) {
    for (member, expected) in [
        ("source", EVENT_SOURCE),
        ("detail-type", REPOSITORY_STATE_CHANGE),
        ("region", REGION),
        ("account", published.account),
    ] {
        assert_eq!(
            envelope[member].as_str(),
            Some(expected),
            "case {case:?}: {member} must be {expected:?}, got {envelope}"
        );
    }
    // The envelope members a consumer reads to tell one delivery from another: the id the
    // bus minted for this event, and the instant the write happened in.
    assert!(
        envelope["id"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: the envelope must carry the id the bus minted, got {envelope}"
    );
    let time = envelope["time"].as_str().unwrap_or_default();
    assert!(
        !time.is_empty() && !time.starts_with(EPOCH_YEAR),
        "case {case:?}: time must be the instant of the write, got {time:?} in {envelope}"
    );
    assert_eq!(
        envelope["resources"],
        json!([published.repository_arn]),
        "case {case:?}: the repository ARN must be the one resource, got {envelope}"
    );
    let detail = &envelope["detail"];
    for (member, expected) in [
        ("event", published.event.to_string()),
        ("referenceType", REFERENCE_TYPE_BRANCH.to_string()),
        ("referenceName", BRANCH.to_string()),
        ("referenceFullName", format!("{REFS_HEADS}{BRANCH}")),
        ("commitId", published.commit_id.to_string()),
        ("repositoryName", REPOSITORY.to_string()),
        ("repositoryId", published.repository_id.to_string()),
        ("callerUserArn", published.caller_arn.to_string()),
    ] {
        assert_eq!(
            detail[member].as_str(),
            Some(expected.as_str()),
            "case {case:?}: detail.{member} must be {expected:?}, got {envelope}"
        );
    }
    assert_eq!(
        detail["oldCommitId"].as_str(),
        published.old_commit_id,
        "case {case:?}: detail.oldCommitId must be {:?}, got {envelope}",
        published.old_commit_id
    );
}

/// Drive every leg of the rule on a freshly started emulator, with each call addressed as
/// `reached` says and each receive waiting `wait` seconds.
///
/// The ONE place these legs are written, for the reason the module docs give: the two
/// addressings a client can take are two rows of one scenario, not two suites.
pub(crate) fn reference_writes_reach_their_queue(reached: Reached, wait: u32) {
    let setup = "the wiring on a started emulator";
    let emulator = Started::start();

    let created = codecommit(
        &emulator,
        setup,
        reached,
        "CreateRepository",
        &json!({ "repositoryName": REPOSITORY }),
    );
    let repository_arn = created["repositoryMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {setup:?}: no repositoryMetadata.Arn in {created}"))
        .to_string();
    let repository_id = created["repositoryMetadata"]["repositoryId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {setup:?}: no repositoryMetadata.repositoryId in {created}")
        })
        .to_string();

    // The queue url this client is HANDED is the one it then polls.
    let queue = sqs(
        &emulator,
        setup,
        reached,
        "CreateQueue",
        &json!({ "QueueName": QUEUE }),
    );
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {setup:?}: no QueueUrl in {queue}"))
        .to_string();
    let attributes = sqs(
        &emulator,
        setup,
        reached,
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
        reached,
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
        reached,
        "PutRule",
        &json!({
            "Name": RULE,
            "State": "ENABLED",
            "EventPattern": pattern.to_string(),
        }),
    );
    let targets = events(
        &emulator,
        setup,
        reached,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "Targets": [{ "Id": "sqs-target", "Arn": &queue_arn }],
        }),
    );
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "case {setup:?}: PutTargets reported a failed entry: {targets}"
    );

    // The identity the events must name as the one whose call moved the reference, and the
    // account the envelope must name as the one it was published in.
    let (status, identity) = call(
        &emulator,
        reached,
        "sts",
        &[("content-type", "application/x-www-form-urlencoded")],
        b"Action=GetCallerIdentity&Version=2011-06-15".to_vec(),
    );
    assert_eq!(
        status, 200,
        "case {setup:?}: expected HTTP 200 from STS GetCallerIdentity, got {status} with \
         {identity}"
    );
    let caller = element(&identity, "Arn");
    let account = element(&identity, "Account");

    // The branch-creating write: the positive control for the whole chain.
    let created_case = "the write that creates the branch";
    let seed = accepted_put_file(&emulator, created_case, reached, None, "seed\n");
    let envelope = delivered_one(&emulator, created_case, reached, &queue_url, wait);
    assert_envelope(
        created_case,
        &envelope,
        &Published {
            repository_arn: &repository_arn,
            repository_id: &repository_id,
            account: &account,
            caller_arn: &caller,
            event: REFERENCE_CREATED,
            commit_id: &seed,
            old_commit_id: None,
        },
    );

    // A fast-forward write, which moves the reference it already held.
    let updated_case = "a fast-forward write that moves the branch";
    let tip = accepted_put_file(
        &emulator,
        updated_case,
        reached,
        Some(&seed),
        "the current tip\n",
    );
    let envelope = delivered_one(&emulator, updated_case, reached, &queue_url, wait);
    assert_envelope(
        updated_case,
        &envelope,
        &Published {
            repository_arn: &repository_arn,
            repository_id: &repository_id,
            account: &account,
            caller_arn: &caller,
            event: REFERENCE_UPDATED,
            commit_id: &tip,
            old_commit_id: Some(&seed),
        },
    );

    // A write naming the parent the branch has already left behind is refused.
    let refused_case = "a write naming a parent the branch has moved past";
    let (status, refused) = put_file(&emulator, reached, Some(&seed), "content offered twice\n");
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
        reached,
        Some(&tip),
        "content offered twice\n",
    );
    let envelope = delivered_one(&emulator, released_case, reached, &queue_url, wait);
    assert_envelope(
        released_case,
        &envelope,
        &Published {
            repository_arn: &repository_arn,
            repository_id: &repository_id,
            account: &account,
            caller_arn: &caller,
            event: REFERENCE_UPDATED,
            commit_id: &released,
            old_commit_id: Some(&tip),
        },
    );
    let nothing = drain(&emulator, refused_case, reached, &queue_url, wait);
    assert!(
        nothing.is_empty(),
        "case {refused_case:?}: the refused write must publish nothing, got {nothing:?}"
    );
}

/// RULE: on the emulator a client starts, an accepted CodeCommit write that moves a
/// reference is published onto the default bus and delivered to the SQS target of a rule
/// matching it, and a write refused for a stale parent publishes nothing.
#[test]
fn a_reference_write_publishes_its_state_change_on_the_emulator_a_client_starts() {
    reference_writes_reach_their_queue(Reached::ServiceHost, SHORT_POLL_SECONDS);
}
