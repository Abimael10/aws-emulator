// SPDX-License-Identifier: CC0-1.0

//! What a repository trigger SENDS to its destination, read where a client reads it: off
//! the Amazon SQS queue subscribed to the topic a trigger names, and through the answer
//! Lambda gives for a function a trigger names — both for a TEST of a trigger and for a
//! reference that really changed.
//!
//! THE FIRST RULE UNDER TEST: "tests the functionality of repository triggers by sending
//! information to the trigger target. If real data is available in the repository, the
//! test sends data from the last commit. If no data is available, sample data is
//! generated"
//! (`smithy_query(query="operation",operation="TestRepositoryTriggers")`). So each
//! trigger a caller submits is tested by DELIVERING one record to the destination it
//! names — a publish to the topic, an invocation of the function — carrying the
//! repository the trigger is on, the trigger that produced it, the caller's identity,
//! the trigger's own custom data and the reference the trigger is in scope for at the
//! commit that reference points at; a trigger whose destination accepted the record is
//! reported in `successfulExecutions` and one whose destination refused it in
//! `failedExecutions`, with what the destination answered. A test is a test: it stores
//! nothing, so the configuration the repository holds is the same before and after, and
//! its record is labelled `TriggerEventTest` rather than as a reference that moved.
//!
//! THE SECOND RULE UNDER TEST: `RepositoryTrigger$events` are "the repository events that
//! cause the trigger to run actions in another service, such as sending a notification
//! through Amazon SNS"
//! (`smithy_query(query="doc",shape="RepositoryTrigger",member="events")`), and those
//! events are the reference events `RepositoryTriggerEventEnum` declares. So a write that
//! creates, moves or removes a reference runs every trigger of the STORED configuration
//! that is in scope for both that branch and that class of event — and each trigger that
//! runs sends one record, labelled `ReferenceChanges`, naming the fully qualified
//! reference and the commit it now points at; a write no stored trigger is in scope for
//! sends nothing.
//!
//! The tables below span what those rules generalise over: the kind of destination (a
//! topic and a function), whether the destination exists, the branch scope a trigger
//! names (none, an empty list, one branch, several, one the repository does not hold),
//! whether the repository holds real data at all, whether the trigger carries custom
//! data, how many triggers one request submits, and — for a reference write — the class
//! of write and the operation that makes it, rather than one recorded request.
//!
//! Everything is driven over the real wire path: this service, Amazon SNS, Amazon SQS
//! and Lambda are mounted on ONE shared engine, the topic, its queue subscription and
//! the function are created through those services' own APIs, and every delivered
//! record is read back through them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_codecommit_codecommit_20150413::state::trigger_notifications::{
    SAMPLE_BRANCH, TRIGGER_EVENT_NAME, TRIGGER_EVENT_SOURCE, TRIGGER_EVENT_VERSION,
    TRIGGER_TEST_EVENT_NAME,
};

use crate::harness::error_with_message;
use crate::repository_lifecycle::{base64, is_object_id, is_uuid, ok, string_at, REGION};
use crate::wire::{caller_arn, hold_long_term_key, DEFAULT_ACCESS_KEY_ID};

/// The caller the CONSUMER side — Amazon SNS, Amazon SQS and Lambda — is driven as: the
/// account's own credential, which is the principal an emulator publishes for its
/// operator, so creating a queue, a topic or a function needs no identity-based policy
/// of its own. The CodeCommit calls are signed as [`DEFAULT_ACCESS_KEY_ID`], the IAM
/// user whose ARN a record then names as the identity the test was made by.
const CONSUMER: &str = "AKIDACCOUNT";

/// How long a case polls for a delivery before it gives up.
///
/// Generous on purpose: it bounds a POLL LOOP that ends as soon as the record is on the
/// queue, so a fast machine never waits for it and a machine running the whole
/// workspace's tests at once is not failed by it.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The execution role the test's function is created with; Lambda only stores it.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/lambda-exec";

/// This service beside Amazon SNS, Amazon SQS and Lambda on one shared engine, so a
/// trigger's destination really receives what this service sends it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codecommit_codecommit_20150413::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    hold_long_term_key(&engine, DEFAULT_ACCESS_KEY_ID);
    router
}

/// One `awsQuery` call against the Amazon SNS emulator through the same front door,
/// answering its XML body.
async fn sns(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
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
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={CONSUMER}/20260101/{REGION}/sns/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: SNS {action} must succeed, got {status} with {text}"
    );
    text
}

/// Percent-encode one `awsQuery` form field.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of one element of an `awsQuery` XML answer.
fn element(body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body
        .find(&open)
        .unwrap_or_else(|| panic!("no <{name}> in {body}"))
        + open.len();
    let end = body[start..]
        .find(&close)
        .unwrap_or_else(|| panic!("no </{name}> in {body}"));
    body[start..start + end].to_string()
}

/// One `awsJson1_0` call against the Amazon SQS emulator through the same front door.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={CONSUMER}/20260101/{REGION}/sqs/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "setup: SQS {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// One `restJson1` call against the Lambda emulator through the same front door,
/// answering its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={CONSUMER}/20260101/{REGION}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The queue policy AWS documents for a queue subscribed to a topic: the
/// `sns.amazonaws.com` service principal, allowed `sqs:SendMessage` on that queue.
fn grant_to_sns(queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": "sns.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string()
}

/// A topic named `name` with one Amazon SQS queue subscribed to it: the topic's ARN
/// (what a `destinationArn` names) and the queue's URL (where a record is read).
async fn given_topic(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", &json!({"QueueName": format!("{name}-inbox")})).await;
    let queue_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateQueue({name}) answered no QueueUrl: {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": queue_url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the queue {name} reported no QueueArn: {attributes}"))
        .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        &json!({"QueueUrl": queue_url, "Attributes": {"Policy": grant_to_sns(&queue_arn)}}),
    )
    .await;

    let topic_arn = element(&sns(router, "CreateTopic", &[("Name", name)]).await, "TopicArn");
    sns(
        router,
        "Subscribe",
        &[
            ("TopicArn", &topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    )
    .await;
    (topic_arn, queue_url)
}

/// The ARN of a topic `name` in this account and Region, whether or not Amazon SNS
/// holds it: what a trigger naming a destination that does not exist carries.
fn topic_arn(name: &str) -> String {
    format!(
        "arn:aws:sns:{REGION}:{}:{name}",
        engine::emulator_account_id()
    )
}

/// The ARN of a Lambda function `name` in this account and Region.
fn function_arn(name: &str) -> String {
    format!(
        "arn:aws:lambda:{REGION}:{}:function:{name}",
        engine::emulator_account_id()
    )
}

/// Receive everything waiting on the queue at `url`, parsing each delivery's SNS
/// envelope into the record this service sent.
async fn drain_into(router: &MultiServiceRouter, url: &str, records: &mut Vec<Value>) {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        let body = message["Body"].as_str().unwrap_or_default();
        // The SNS envelope carries what was published in `Message`.
        let envelope: Value = serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("a delivery is not an SNS envelope ({error}): {body}"));
        let published = envelope["Message"].as_str().unwrap_or_default().to_string();
        let record: Value = serde_json::from_str(&published).unwrap_or_else(|error| {
            panic!("a published notification is not a JSON document ({error}): {published}")
        });
        records.push(record);
    }
}

/// Poll the queue at `url` until `records` holds `expected` records in all.
///
/// The accumulator is the caller's, because a received message stays invisible for the
/// queue's visibility timeout: a second look reports what has arrived SINCE, so a case
/// that waits twice folds both looks into one accumulator rather than waiting out that
/// timeout.
async fn await_records(
    router: &MultiServiceRouter,
    url: &str,
    records: &mut Vec<Value>,
    expected: usize,
) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        drain_into(router, url, records).await;
        if records.len() >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected} delivered record(s) on {url}, got {}: {records:?}",
            records.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the queue at `url` until it has delivered `expected` records, answering them.
async fn awaited_records(router: &MultiServiceRouter, url: &str, expected: usize) -> Vec<Value> {
    let mut records = Vec::new();
    await_records(router, url, &mut records, expected).await;
    records
}

/// What one `TestRepositoryTriggers` call reported: the names it tested successfully,
/// and the `(trigger, failureMessage)` pairs it could not.
type Report = (Vec<String>, Vec<(String, String)>);

/// Test `triggers` against `repository`, answering the report.
async fn test_triggers(
    router: &MultiServiceRouter,
    repository: &str,
    triggers: &Value,
) -> Report {
    let answered = ok(
        router,
        "TestRepositoryTriggers",
        &json!({ "repositoryName": repository, "triggers": triggers }),
    )
    .await;
    let successful: Vec<String> = answered["successfulExecutions"]
        .as_array()
        .unwrap_or_else(|| panic!("no successfulExecutions in {answered}"))
        .iter()
        .map(|name| name.as_str().unwrap_or_default().to_string())
        .collect();
    let failed: Vec<(String, String)> = answered["failedExecutions"]
        .as_array()
        .unwrap_or_else(|| panic!("no failedExecutions in {answered}"))
        .iter()
        .map(|entry| {
            (
                entry["trigger"].as_str().unwrap_or_default().to_string(),
                entry["failureMessage"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    (successful, failed)
}

/// Store `triggers` on `repository`, answering the `configurationId` the write minted.
async fn put_triggers(router: &MultiServiceRouter, repository: &str, triggers: &Value) -> String {
    string_at(
        &ok(
            router,
            "PutRepositoryTriggers",
            &json!({ "repositoryName": repository, "triggers": triggers }),
        )
        .await,
        "/configurationId",
    )
}

/// Write one commit on `branch` through `PutFile`, answering its id. `parent` is the
/// commit the write is made on top of, which a branch that already has one requires.
async fn put_file(
    router: &MultiServiceRouter,
    repository: &str,
    branch: &str,
    parent: Option<&str>,
    content: &[u8],
) -> String {
    let mut body = json!({
        "repositoryName": repository,
        "branchName": branch,
        "filePath": "README.md",
        "fileContent": base64(content),
        "commitMessage": "a revision",
    });
    if let Some(parent) = parent {
        body["parentCommitId"] = json!(parent);
    }
    string_at(&ok(router, "PutFile", &body).await, "/commitId")
}

/// Branch `branch` off `commit_id`, the way a caller opens a branch of its own.
async fn create_branch(
    router: &MultiServiceRouter,
    repository: &str,
    branch: &str,
    commit_id: &str,
) {
    ok(
        router,
        "CreateBranch",
        &json!({
            "repositoryName": repository,
            "branchName": branch,
            "commitId": commit_id,
        }),
    )
    .await;
}

/// The single record the destination received, with every member that is the same
/// whatever the trigger asserted.
fn sole_record(case: &str, records: &[Value], repository_arn: &str, trigger: &str) -> Value {
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected exactly one delivered record, got {records:?}"
    );
    let entries = records[0]["Records"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no Records in {}", records[0]))
        .clone();
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: a record carries one entry, got {entries:?}"
    );
    let entry = entries[0].clone();
    for (member, expected) in [
        ("awsRegion", REGION),
        // A test's record is labelled as the probe it is rather than as a reference
        // that changed, which is how a consumer tells the two apart.
        ("eventName", TRIGGER_TEST_EVENT_NAME),
        ("eventSource", TRIGGER_EVENT_SOURCE),
        ("eventSourceARN", repository_arn),
        ("eventVersion", TRIGGER_EVENT_VERSION),
        ("eventTriggerName", trigger),
        ("userIdentityARN", &caller_arn(DEFAULT_ACCESS_KEY_ID)),
    ] {
        assert_eq!(
            entry[member].as_str(),
            Some(expected),
            "case {case:?}: {member} must be {expected:?}, got {entry}"
        );
    }
    for (member, expected) in [("eventPartNumber", 1), ("eventTotalParts", 1)] {
        assert_eq!(
            entry[member].as_i64(),
            Some(expected),
            "case {case:?}: {member} must be {expected}, got {entry}"
        );
    }
    let event_time = entry["eventTime"].as_str().unwrap_or_default();
    assert!(
        event_time.ends_with('Z') && event_time.contains('T'),
        "case {case:?}: eventTime must be an instant, got {event_time:?}"
    );
    entry
}

/// The `(ref, commit)` the record's single reference names.
fn sole_reference(case: &str, entry: &Value) -> (String, String) {
    let references = entry["codecommit"]["references"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no codecommit.references in {entry}"))
        .clone();
    assert_eq!(
        references.len(),
        1,
        "case {case:?}: expected one reference, got {references:?}"
    );
    (
        references[0]["ref"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        references[0]["commit"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// One row of the table below: a trigger's branch scope and custom data, and the
/// reference and commit the record it is tested with must name.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    /// The trigger's `branches` as sent; `None` leaves the member out altogether.
    branches: Option<Vec<&'static str>>,
    /// The trigger's `customData` as sent, or `None` for a trigger carrying none.
    custom_data: Option<&'static str>,
    /// The reference the record must name.
    reference: &'static str,
    /// The commit that reference must point at.
    commit: Commit,
}

/// What a row expects the record's reference to point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Commit {
    /// The tip of `main`, the older of the repository's two commits.
    Main,
    /// The tip of `feature`, the repository's LAST commit.
    Feature,
    /// No commit of the repository: generated sample data.
    Sample,
}

#[tokio::test]
async fn each_tested_trigger_sends_the_last_commit_in_scope_to_its_destination() {
    let router = emulator();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "trigger-test" }),
    )
    .await;
    let repository_arn = format!(
        "arn:aws:codecommit:{REGION}:{}:trigger-test",
        engine::emulator_account_id()
    );
    // Two branches, written in this order, so the repository's LAST commit is the tip
    // of `feature` while `main` keeps an older one.
    let main_tip = put_file(&router, "trigger-test", "main", None, b"# first\n").await;
    create_branch(&router, "trigger-test", "feature", &main_tip).await;
    let feature_tip = put_file(
        &router,
        "trigger-test",
        "feature",
        Some(&main_tip),
        b"# second\n",
    )
    .await;
    assert_ne!(
        main_tip, feature_tip,
        "setup: the two commits must differ, got {main_tip}"
    );

    let rows: Vec<Row> = vec![
        Row {
            case: "no branches member: the trigger applies to every branch, so the last commit",
            branches: None,
            custom_data: None,
            reference: "refs/heads/feature",
            commit: Commit::Feature,
        },
        Row {
            case: "an empty branch list applies to every branch too",
            branches: Some(Vec::new()),
            custom_data: None,
            reference: "refs/heads/feature",
            commit: Commit::Feature,
        },
        Row {
            case: "one branch named: the last commit on THAT branch",
            branches: Some(vec!["main"]),
            custom_data: None,
            reference: "refs/heads/main",
            commit: Commit::Main,
        },
        Row {
            case: "two branches named, with custom data: the last commit among them",
            branches: Some(vec!["main", "feature"]),
            custom_data: Some("team=platform"),
            reference: "refs/heads/feature",
            commit: Commit::Feature,
        },
        Row {
            case: "a branch the repository does not hold: no data in scope, so sample data",
            branches: Some(vec!["not-yet"]),
            custom_data: Some("{\"pipeline\":\"build\"}"),
            reference: "refs/heads/not-yet",
            commit: Commit::Sample,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let Row {
            case,
            branches,
            custom_data,
            reference: expected_reference,
            commit: expected_commit,
        } = row;
        let name = format!("trigger-{index:02}");
        let (destination, queue_url) = given_topic(&router, &name).await;
        let mut trigger = json!({
            "name": name,
            "destinationArn": destination,
            "events": ["all"],
        });
        if let Some(branches) = &branches {
            trigger["branches"] = json!(branches);
        }
        if let Some(custom_data) = custom_data {
            trigger["customData"] = json!(custom_data);
        }
        let sent = json!([trigger]);
        // Configured first, as a caller configures a trigger before testing it: the
        // record names the configuration the repository holds.
        let configuration_id = put_triggers(&router, "trigger-test", &sent).await;

        let (successful, failed) = test_triggers(&router, "trigger-test", &sent).await;
        assert_eq!(
            (successful.as_slice(), failed.as_slice()),
            (&[name.clone()][..], &[][..]),
            "case {case:?}: the trigger's destination accepted the record, so it must be \
             reported successful"
        );

        let records = awaited_records(&router, &queue_url, 1).await;
        let entry = sole_record(case, &records, &repository_arn, &name);
        assert_eq!(
            entry["eventTriggerConfigId"].as_str(),
            Some(configuration_id.as_str()),
            "case {case:?}: the record must name the configuration the repository holds, got \
             {entry}"
        );
        assert_eq!(
            entry.get("customData").and_then(Value::as_str),
            custom_data,
            "case {case:?}: customData must be the trigger's own, got {entry}"
        );
        let (reference, commit) = sole_reference(case, &entry);
        assert_eq!(
            reference, expected_reference,
            "case {case:?}: the record must name the reference in scope, got {reference:?}"
        );
        match expected_commit {
            Commit::Main => assert_eq!(
                commit, main_tip,
                "case {case:?}: expected the tip of main, got {commit:?}"
            ),
            Commit::Feature => assert_eq!(
                commit, feature_tip,
                "case {case:?}: expected the tip of feature, got {commit:?}"
            ),
            Commit::Sample => {
                assert!(
                    is_object_id(&commit),
                    "case {case:?}: generated sample data must still be a git object name, got \
                     {commit:?}"
                );
                assert!(
                    commit != main_tip && commit != feature_tip,
                    "case {case:?}: no commit is in scope, so the sample must name none of the \
                     repository's own, got {commit:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_repository_with_no_data_is_sent_generated_sample_data() {
    let router = emulator();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "trigger-empty" }),
    )
    .await;
    let repository_arn = format!(
        "arn:aws:codecommit:{REGION}:{}:trigger-empty",
        engine::emulator_account_id()
    );
    let (destination, queue_url) = given_topic(&router, "trigger-empty").await;
    let sent = json!([{
        "name": "on-any-reference",
        "destinationArn": destination,
        "events": ["all"],
    }]);

    // Twice, with nothing stored in between: a repository nobody has written to has no
    // data to send, and the generated sample is the same both times — it is derived
    // rather than drawn at random. The repository also holds no configuration, so the
    // record carries the id of the configuration this test was made against.
    let mut sampled: Vec<String> = Vec::new();
    let mut records: Vec<Value> = Vec::new();
    for round in 1..=2 {
        let case = format!("round {round}");
        let (successful, failed) = test_triggers(&router, "trigger-empty", &sent).await;
        assert_eq!(
            (successful.as_slice(), failed.as_slice()),
            (&["on-any-reference".to_string()][..], &[][..]),
            "case {case:?}: the record must reach the topic"
        );
        await_records(&router, &queue_url, &mut records, round).await;
        let entry = sole_record(
            &case,
            &records[round - 1..round],
            &repository_arn,
            "on-any-reference",
        );
        assert!(
            is_uuid(entry["eventTriggerConfigId"].as_str().unwrap_or_default()),
            "case {case:?}: a record names the configuration it was sent for, got {entry}"
        );
        assert!(
            entry.get("customData").is_none(),
            "case {case:?}: a trigger carrying no custom data sends none, got {entry}"
        );
        let (reference, commit) = sole_reference(&case, &entry);
        assert_eq!(
            reference,
            format!("refs/heads/{SAMPLE_BRANCH}"),
            "case {case:?}: an empty repository's sample names the branch a client checks out, \
             got {reference:?}"
        );
        assert!(
            is_object_id(&commit),
            "case {case:?}: sample data must still be a git object name, got {commit:?}"
        );
        sampled.push(commit);
    }
    assert_eq!(
        sampled[0], sampled[1],
        "the generated sample must be derived, not random, got {sampled:?}"
    );

    // And nothing was stored by either test: the repository is still unconfigured.
    let read = ok(
        &router,
        "GetRepositoryTriggers",
        &json!({ "repositoryName": "trigger-empty" }),
    )
    .await;
    assert_eq!(
        (
            read.get("configurationId").and_then(Value::as_str),
            read.get("triggers").cloned().unwrap_or(Value::Null)
        ),
        (None, json!([])),
        "a test stores nothing, so the repository must still hold no configuration, got {read}"
    );
}

#[tokio::test]
async fn a_function_destination_is_invoked_and_a_missing_one_is_reported() {
    let router = emulator();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "trigger-functions" }),
    )
    .await;
    put_file(&router, "trigger-functions", "main", None, b"# code\n").await;

    // A function this emulator holds: created through Lambda's own API, and polled
    // until Lambda reports it ready to be invoked.
    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": "on-reference-change",
            "Role": EXECUTION_ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": {"ZipFile": "cGFja2FnZQ=="},
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateFunction must succeed, got {status} with {created}"
    );
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let (status, reported) = lambda(
            &router,
            "GET",
            "/2015-03-31/functions/on-reference-change/configuration",
            &Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration must succeed, got {status} with {reported}"
        );
        if reported["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported the function Active, last {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // (case, the trigger's name, the destination it names, whether the destination
    // accepts the record)
    let rows: Vec<(&str, &str, String, bool)> = vec![
        (
            "a function this account holds",
            "to-a-function",
            function_arn("on-reference-change"),
            true,
        ),
        (
            "a function no account holds",
            "to-a-missing-function",
            function_arn("never-created"),
            false,
        ),
        (
            "a topic no account holds",
            "to-a-missing-topic",
            topic_arn("never-created"),
            false,
        ),
    ];

    for (case, name, destination, accepted) in rows {
        let sent = json!([{
            "name": name,
            "destinationArn": destination,
            "events": ["updateReference"],
        }]);
        let (successful, failed) = test_triggers(&router, "trigger-functions", &sent).await;
        if accepted {
            assert_eq!(
                (successful.as_slice(), failed.as_slice()),
                (&[name.to_string()][..], &[][..]),
                "case {case:?}: a destination that accepts the record is a successful execution"
            );
            continue;
        }
        assert!(
            successful.is_empty(),
            "case {case:?}: a destination that refuses the record is no success, got \
             {successful:?}"
        );
        assert_eq!(
            failed.len(),
            1,
            "case {case:?}: expected one failed execution, got {failed:?}"
        );
        let (trigger, message) = &failed[0];
        assert_eq!(
            trigger, name,
            "case {case:?}: a failure names the trigger that did not run, got {trigger:?}"
        );
        assert!(
            message.contains(&destination),
            "case {case:?}: the failure must say which destination could not be reached, got \
             {message:?}"
        );
    }
}

#[tokio::test]
async fn one_destination_s_refusal_does_not_stop_the_other_triggers() {
    let router = emulator();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "trigger-mixed" }),
    )
    .await;
    let repository_arn = format!(
        "arn:aws:codecommit:{REGION}:{}:trigger-mixed",
        engine::emulator_account_id()
    );
    put_file(&router, "trigger-mixed", "main", None, b"# mixed\n").await;
    let (first, first_queue) = given_topic(&router, "mixed-first").await;
    let (second, second_queue) = given_topic(&router, "mixed-second").await;

    // Three triggers in one request, with the unreachable destination BETWEEN the two
    // reachable ones: each is tested on its own.
    let sent = json!([
        { "name": "first", "destinationArn": first, "events": ["all"] },
        {
            "name": "unreachable",
            "destinationArn": topic_arn("never-created"),
            "events": ["all"],
        },
        { "name": "second", "destinationArn": second, "events": ["createReference"] },
    ]);
    let (successful, failed) = test_triggers(&router, "trigger-mixed", &sent).await;
    // Every submitted trigger is answered for, in exactly one of the two lists.
    assert_eq!(
        successful.len() + failed.len(),
        3,
        "each of the three submitted triggers must be reported once, got {successful:?} and \
         {failed:?}"
    );
    assert_eq!(
        successful,
        vec!["first".to_string(), "second".to_string()],
        "both reachable destinations must be reported successful, with the failed one left out"
    );
    assert_eq!(
        failed.len(),
        1,
        "exactly the unreachable destination must fail, got {failed:?}"
    );
    assert_eq!(
        failed[0].0, "unreachable",
        "the failure must name the trigger whose destination refused the record, got {failed:?}"
    );

    // Both reachable destinations received their OWN trigger's record.
    for (case, queue_url, trigger) in [
        ("the first topic", &first_queue, "first"),
        ("the second topic", &second_queue, "second"),
    ] {
        let records = awaited_records(&router, queue_url, 1).await;
        let entry = sole_record(case, &records, &repository_arn, trigger);
        let (reference, _) = sole_reference(case, &entry);
        assert_eq!(
            reference, "refs/heads/main",
            "case {case:?}: the record must name the repository's only branch, got {reference:?}"
        );
    }
}

#[tokio::test]
async fn a_configuration_the_repository_cannot_hold_is_refused_and_nothing_is_sent() {
    let router = emulator();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "trigger-refused" }),
    )
    .await;
    let (destination, queue_url) = given_topic(&router, "trigger-refused").await;
    // The configuration a refusal must leave in place, and the id it was stored under.
    let stored = json!([{
        "name": "already-stored",
        "destinationArn": destination,
        "events": ["all"],
    }]);
    let stored_id = put_triggers(&router, "trigger-refused", &stored).await;

    // (case, the `triggers` value as sent, the modelled error it must be refused with):
    // the same judging `PutRepositoryTriggers` applies, so this operation cannot accept
    // a configuration the other refuses — one row per member a trigger carries.
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a trigger whose name is blank",
            json!([{ "name": "   ", "destinationArn": destination, "events": ["all"] }]),
            "RepositoryTriggerNameRequiredException",
        ),
        (
            "two triggers sharing one name",
            json!([
                { "name": "same", "destinationArn": destination, "events": ["all"] },
                { "name": "same", "destinationArn": destination, "events": ["all"] },
            ]),
            "InvalidRepositoryTriggerNameException",
        ),
        (
            "a destination that is not an ARN at all",
            json!([{ "name": "t", "destinationArn": "releases", "events": ["all"] }]),
            "InvalidRepositoryTriggerDestinationArnException",
        ),
        (
            "a destination of a service a trigger cannot run",
            json!([{
                "name": "t",
                "destinationArn": format!("arn:aws:s3:::{}-bucket", engine::emulator_account_id()),
                "events": ["all"],
            }]),
            "InvalidRepositoryTriggerDestinationArnException",
        ),
        (
            "a destination in another Region than the repository",
            json!([{
                "name": "t",
                "destinationArn": format!(
                    "arn:aws:sns:eu-central-1:{}:elsewhere",
                    engine::emulator_account_id()
                ),
                "events": ["all"],
            }]),
            "InvalidRepositoryTriggerRegionException",
        ),
        (
            "a trigger naming no event",
            json!([{ "name": "t", "destinationArn": destination, "events": [] }]),
            "RepositoryTriggerEventsListRequiredException",
        ),
        (
            "an event outside the modelled enum",
            json!([{
                "name": "t",
                "destinationArn": destination,
                "events": ["pushReference"],
            }]),
            "InvalidRepositoryTriggerEventsException",
        ),
        (
            "a branch name no repository can hold",
            json!([{
                "name": "t",
                "destinationArn": destination,
                "events": ["all"],
                "branches": ["not a branch"],
            }]),
            "InvalidRepositoryTriggerBranchNameException",
        ),
    ];

    for (case, triggers, expected) in rows {
        let (code, message) = error_with_message(
            &router,
            "TestRepositoryTriggers",
            &json!({ "repositoryName": "trigger-refused", "triggers": triggers }),
            400,
        )
        .await;
        assert_eq!(
            code, expected,
            "case {case:?}: expected {expected}, got {code}: {message}"
        );
        assert!(
            !message.is_empty(),
            "case {case:?}: {code} must say what is wrong, got an empty message"
        );
    }

    // A repository the store does not hold is refused for the repository.
    let (code, message) = error_with_message(
        &router,
        "TestRepositoryTriggers",
        &json!({
            "repositoryName": "never-created",
            "triggers": [{
                "name": "t",
                "destinationArn": destination,
                "events": ["all"],
            }],
        }),
        400,
    )
    .await;
    assert_eq!(
        code, "RepositoryDoesNotExistException",
        "a test against a repository that does not exist must be refused, got {code}: {message}"
    );

    // A request carrying no trigger at all tests nothing and sends nothing.
    let (successful, failed) = test_triggers(&router, "trigger-refused", &json!([])).await;
    assert!(
        successful.is_empty() && failed.is_empty(),
        "an empty list tests nothing, got {successful:?} and {failed:?}"
    );

    // Not one of those calls sent anything to the configured destination, and the
    // configuration the repository holds is untouched.
    let mut records = Vec::new();
    drain_into(&router, &queue_url, &mut records).await;
    assert!(
        records.is_empty(),
        "a refused test must send nothing, got {records:?}"
    );
    let read = ok(
        &router,
        "GetRepositoryTriggers",
        &json!({ "repositoryName": "trigger-refused" }),
    )
    .await;
    assert_eq!(
        (
            read.get("configurationId").and_then(Value::as_str),
            read.get("triggers").cloned().unwrap_or(Value::Null)
        ),
        (Some(stored_id.as_str()), stored),
        "a test must leave the stored configuration exactly as it was, got {read}"
    );
}

#[tokio::test]
async fn a_test_stores_nothing_of_the_configuration_it_was_made_against() {
    let router = emulator();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "trigger-unstored" }),
    )
    .await;
    put_file(&router, "trigger-unstored", "main", None, b"# stored\n").await;
    let (stored_destination, _) = given_topic(&router, "unstored-stored").await;
    let (tested_destination, tested_queue) = given_topic(&router, "unstored-tested").await;

    let stored = json!([{
        "name": "configured",
        "destinationArn": stored_destination,
        "events": ["all"],
        "customData": "stored",
    }]);
    let stored_id = put_triggers(&router, "trigger-unstored", &stored).await;

    // A test of a DIFFERENT configuration: it is the submitted triggers that are
    // tested, and the stored configuration that stays.
    let tested = json!([{
        "name": "only-tested",
        "destinationArn": tested_destination,
        "events": ["deleteReference"],
        "branches": ["main"],
        "customData": "tested",
    }]);
    let (successful, failed) = test_triggers(&router, "trigger-unstored", &tested).await;
    assert_eq!(
        (successful.as_slice(), failed.as_slice()),
        (&["only-tested".to_string()][..], &[][..]),
        "the submitted trigger is the one tested"
    );
    let records = awaited_records(&router, &tested_queue, 1).await;
    assert_eq!(
        records[0]["Records"][0]["customData"].as_str(),
        Some("tested"),
        "the record must carry the SUBMITTED trigger's custom data, got {records:?}"
    );

    let read = ok(
        &router,
        "GetRepositoryTriggers",
        &json!({ "repositoryName": "trigger-unstored" }),
    )
    .await;
    assert_eq!(
        (
            read.get("configurationId").and_then(Value::as_str),
            read.get("triggers").cloned().unwrap_or(Value::Null)
        ),
        (Some(stored_id.as_str()), stored),
        "a test must not replace the configuration a write stored, got {read}"
    );
}

/// git's null object name: the value a reference update carries for a reference that
/// points at nothing, which is what the record of a REMOVED reference names.
const NULL_COMMIT: &str = "0000000000000000000000000000000000000000";

/// The file a row adds and then removes, beside the one every commit writes.
const EXTRA_FILE: &str = "extra.txt";

/// What one row of [`a_reference_write_runs_the_triggers_in_scope_and_no_others`] writes,
/// each through the operation a client makes it with.
#[derive(Debug, Clone, Copy)]
enum ReferenceWrite {
    /// A commit on a branch that already has one, through `PutFile`.
    Commit(&'static str),
    /// A commit on a branch adding [`EXTRA_FILE`], through `CreateCommit`.
    AddFile(&'static str),
    /// A commit on a branch removing [`EXTRA_FILE`], through `DeleteFile`.
    RemoveFile(&'static str),
    /// A reference the repository did not hold, at `main`'s tip, through `CreateBranch`.
    CreateReference(&'static str),
    /// A reference the repository no longer holds, through `DeleteBranch`.
    DeleteReference(&'static str),
}

impl ReferenceWrite {
    /// The branch this write writes.
    fn branch(self) -> &'static str {
        match self {
            Self::Commit(branch)
            | Self::AddFile(branch)
            | Self::RemoveFile(branch)
            | Self::CreateReference(branch)
            | Self::DeleteReference(branch) => branch,
        }
    }
}

/// The trigger names of `records`, sorted: which triggers one write ran.
fn triggers_that_ran(records: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = records
        .iter()
        .map(|record| {
            record["Records"][0]["eventTriggerName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// What every record of a REFERENCE WRITE has to say, whatever the trigger that ran: the
/// class of event, the repository it is on, the configuration it belongs to, the identity
/// whose call produced it, and the reference with the commit it now points at.
fn assert_reference_record(
    case: &str,
    record: &Value,
    repository_arn: &str,
    configuration_id: &str,
    reference: &str,
    commit: &str,
) {
    let entry = record["Records"][0].clone();
    for (member, expected) in [
        ("awsRegion", REGION),
        ("eventName", TRIGGER_EVENT_NAME),
        ("eventSource", TRIGGER_EVENT_SOURCE),
        ("eventSourceARN", repository_arn),
        ("eventVersion", TRIGGER_EVENT_VERSION),
        ("eventTriggerConfigId", configuration_id),
        ("userIdentityARN", &caller_arn(DEFAULT_ACCESS_KEY_ID)),
    ] {
        assert_eq!(
            entry[member].as_str(),
            Some(expected),
            "case {case:?}: {member} must be {expected:?}, got {entry}"
        );
    }
    let (named_reference, named_commit) = sole_reference(case, &entry);
    assert_eq!(
        (named_reference.as_str(), named_commit.as_str()),
        (reference, commit),
        "case {case:?}: the record must name {reference} at {commit}, got {entry}"
    );
}

#[tokio::test]
async fn a_reference_write_runs_the_triggers_in_scope_and_no_others() {
    let router = emulator();
    let repository = "reference-change-triggers";
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": repository }),
    )
    .await;
    let repository_arn = format!(
        "arn:aws:codecommit:{REGION}:{}:{repository}",
        engine::emulator_account_id()
    );
    let (destination, queue) = given_topic(&router, "reference-changes").await;

    let seed = put_file(&router, repository, "main", None, b"# seed\n").await;
    create_branch(&router, repository, "unlisted", &seed).await;

    // A write made while the repository held no configuration ran no trigger at all.
    let unconfigured = "a write made before a configuration was stored";
    let mut records: Vec<Value> = Vec::new();
    drain_into(&router, &queue, &mut records).await;
    assert!(
        records.is_empty(),
        "case {unconfigured:?}: a repository with no stored trigger owes nothing, got \
         {records:?}"
    );

    // Four triggers of ONE configuration, all on the same topic, differing in the scope
    // the rule reads: the branches they name and the events they name.
    let configuration_id = put_triggers(
        &router,
        repository,
        &json!([
            {
                "name": "main-updates",
                "destinationArn": destination,
                "branches": ["main"],
                "events": ["updateReference"],
            },
            // No `branches` member at all: the trigger applies to every branch.
            {"name": "every-branch", "destinationArn": destination, "events": ["all"]},
            {
                "name": "feature-life",
                "destinationArn": destination,
                "branches": ["feature"],
                "events": ["createReference", "deleteReference"],
            },
            // In scope for `main`, but for a class of write no row below is.
            {
                "name": "main-creates",
                "destinationArn": destination,
                "branches": ["main"],
                "events": ["createReference"],
            },
        ]),
    )
    .await;

    // (case, the write, the triggers that must run for it)
    let rows: &[(&str, ReferenceWrite, &[&str])] = &[
        (
            "a commit on a branch one trigger lists",
            ReferenceWrite::Commit("main"),
            &["every-branch", "main-updates"],
        ),
        (
            "a commit on a branch no trigger lists",
            ReferenceWrite::Commit("unlisted"),
            &["every-branch"],
        ),
        (
            "a commit written through CreateCommit",
            ReferenceWrite::AddFile("main"),
            &["every-branch", "main-updates"],
        ),
        (
            "a commit written through DeleteFile",
            ReferenceWrite::RemoveFile("main"),
            &["every-branch", "main-updates"],
        ),
        (
            "a reference the repository did not hold",
            ReferenceWrite::CreateReference("feature"),
            &["every-branch", "feature-life"],
        ),
        (
            "a reference the repository no longer holds",
            ReferenceWrite::DeleteReference("feature"),
            &["every-branch", "feature-life"],
        ),
    ];

    let mut tips: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    tips.insert("main".to_string(), seed.clone());
    tips.insert("unlisted".to_string(), seed.clone());

    for (index, (case, write, expected)) in rows.iter().enumerate() {
        let branch = write.branch();
        let parent = tips.get(branch).cloned();
        let commit = match write {
            ReferenceWrite::Commit(_) => {
                put_file(
                    &router,
                    repository,
                    branch,
                    parent.as_deref(),
                    format!("# revision {index}\n").as_bytes(),
                )
                .await
            }
            ReferenceWrite::AddFile(_) => string_at(
                &ok(
                    &router,
                    "CreateCommit",
                    &json!({
                        "repositoryName": repository,
                        "branchName": branch,
                        "parentCommitId": parent,
                        "commitMessage": "a file added",
                        "putFiles": [{
                            "filePath": EXTRA_FILE,
                            "fileContent": base64(format!("added at {index}\n").as_bytes()),
                        }],
                    }),
                )
                .await,
                "/commitId",
            ),
            ReferenceWrite::RemoveFile(_) => string_at(
                &ok(
                    &router,
                    "DeleteFile",
                    &json!({
                        "repositoryName": repository,
                        "branchName": branch,
                        "filePath": EXTRA_FILE,
                        "parentCommitId": parent,
                        "commitMessage": "a file removed",
                    }),
                )
                .await,
                "/commitId",
            ),
            ReferenceWrite::CreateReference(_) => {
                let at = tips
                    .get("main")
                    .cloned()
                    .unwrap_or_else(|| panic!("case {case:?}: main holds no tip"));
                create_branch(&router, repository, branch, &at).await;
                at
            }
            ReferenceWrite::DeleteReference(_) => {
                let removed = ok(
                    &router,
                    "DeleteBranch",
                    &json!({ "repositoryName": repository, "branchName": branch }),
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
            ReferenceWrite::DeleteReference(_) => {
                tips.remove(branch);
            }
            _ => {
                tips.insert(branch.to_string(), commit.clone());
            }
        }

        // A received message stays invisible for the queue's visibility timeout, so every
        // row reads into ONE accumulator and asserts over what arrived since.
        let already = records.len();
        await_records(&router, &queue, &mut records, already + expected.len()).await;
        let arrived = records[already..].to_vec();
        let ran = triggers_that_ran(&arrived);
        let wanted: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
        assert_eq!(
            ran, wanted,
            "case {case:?}: expected the triggers {wanted:?} to run, got {ran:?} from {arrived:?}"
        );
        // A further look finds nothing more: a trigger out of scope sent no record.
        let total = records.len();
        drain_into(&router, &queue, &mut records).await;
        assert_eq!(
            records.len(),
            total,
            "case {case:?}: only the triggers in scope run, got the further records {:?}",
            &records[total..]
        );
        let reference = format!("refs/heads/{branch}");
        for record in &arrived {
            assert_reference_record(
                case,
                record,
                &repository_arn,
                &configuration_id,
                &reference,
                &commit,
            );
        }
    }
}

#[tokio::test]
async fn a_tested_trigger_is_labelled_a_probe_and_a_real_write_is_not() {
    let router = emulator();
    let repository = "probe-and-change";
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": repository }),
    )
    .await;
    let repository_arn = format!(
        "arn:aws:codecommit:{REGION}:{}:{repository}",
        engine::emulator_account_id()
    );
    let (destination, queue) = given_topic(&router, "probe-and-change").await;
    let seed = put_file(&router, repository, "main", None, b"# seed\n").await;
    let triggers = json!([{
        "name": "everything",
        "destinationArn": destination,
        "branches": ["main"],
        "events": ["all"],
    }]);
    let configuration_id = put_triggers(&router, repository, &triggers).await;

    // The probe: labelled as the test it is, and reported as tested.
    let case = "a test of the stored trigger";
    let (successful, failed) = test_triggers(&router, repository, &triggers).await;
    assert_eq!(
        (successful.as_slice(), failed.as_slice()),
        (&["everything".to_string()][..], &[][..]),
        "case {case:?}: the trigger's destination accepted the probe"
    );
    let mut records: Vec<Value> = Vec::new();
    await_records(&router, &queue, &mut records, 1).await;
    let probe = records[0]["Records"][0].clone();
    assert_eq!(
        probe["eventName"].as_str(),
        Some(TRIGGER_TEST_EVENT_NAME),
        "case {case:?}: a test's record is labelled as a test, got {probe}"
    );

    // The same trigger, the same destination, a reference that really moved: labelled as
    // a reference change, which is how a consumer tells the two apart.
    let case = "a reference that really moved";
    let commit = put_file(&router, repository, "main", Some(&seed), b"# revision\n").await;
    await_records(&router, &queue, &mut records, 2).await;
    let changed = records[1..].to_vec();
    assert_eq!(
        changed.len(),
        1,
        "case {case:?}: the one trigger in scope sends one record, got {changed:?}"
    );
    assert_reference_record(
        case,
        &changed[0],
        &repository_arn,
        &configuration_id,
        "refs/heads/main",
        &commit,
    );
    assert_eq!(
        changed[0]["Records"][0]["eventTriggerName"].as_str(),
        Some("everything"),
        "case {case:?}: the record must name the trigger that ran, got {:?}",
        changed[0]
    );
}
