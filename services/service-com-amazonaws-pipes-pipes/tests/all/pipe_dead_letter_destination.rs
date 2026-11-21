// SPDX-License-Identifier: CC0-1.0

//! WHAT a pipe may name as its dead-letter destination, end to end over the wire, with
//! Amazon SQS, Kinesis and Amazon SNS mounted beside pipes on one engine.
//!
//! The RULE asserted below: `DeadLetterConfig.Arn` names "either an Amazon SNS topic or
//! Amazon SQS queue ARN" (`smithy_query(query="doc", shape="DeadLetterConfig",
//! member="Arn")`), so a lifecycle call that would leave a pipe carrying a destination of
//! any other kind is ACCEPTED — the call answers 200 and reports the pipe in its transient
//! state — and the pipe then settles in the terminal failure state of the move that was in
//! flight (`CREATE_FAILED` for the creation, `START_FAILED` for the start, `UPDATE_FAILED`
//! for the reconfiguration), carrying a `StateReason` that names the destination, and it
//! polls nothing. A destination of either admitted kind leaves the pipe reaching the state
//! its caller asked for, and the record of a batch it gives up on really arrives there.
//!
//! The other half of the dead-letter rule — WHEN a batch is given up on and WHAT the record
//! of it says — is `tests/all/pipe_dead_letter.rs`. Whether a pipe comes up at all for
//! reasons of its SOURCE and its ROLE is `tests/all/pipe_bring_up.rs`, whose rule this one
//! joins.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::pipe_state::{
    CREATE_FAILED, RUNNING, START_FAILED, UPDATE_FAILED,
};
use service_com_amazonaws_pipes_pipes::state::NO_RECORDS_PROCESSED_STATE_REASON;
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_bring_up::settled;
use crate::pipe_data_plane::{collect_at_least, collect_during, given_queue};
use crate::pipe_dead_letter::{given_stream, put_record};
use crate::pipe_lifecycle::{call, Reply, REGION};
use crate::pipe_metrics::{form_encode, xml_text};

/// How long a wait for work nothing asked for — an attempt at a batch being spent, the record
/// of a discarded batch landing — is given before the row is a failure. Well above the poll
/// interval, the state-settle span and the backoff between attempts, so a row fails only when
/// the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(60);

/// How long a row watches a destination that must stay EMPTY, and a source nothing must be
/// taken from. Many times the poll interval, so a pipe that was going to read its source
/// would have.
const QUIET: Duration = Duration::from_secs(3);

/// The role every pipe below names. Nothing here turns on its policies: no IAM store is
/// mounted, so no stage is refused on the strength of one.
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-dead-letter-destination";

/// The partition the ARNs a row writes by hand are in, the one every resource this emulator
/// mints is named in.
const PARTITION: &str = "aws";

/// Pipes and every service a Kinesis-sourced pipe with a dead-letter destination reaches, on
/// one engine, as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// One Amazon SNS call: SNS speaks `awsQuery`, so its parameters travel as a form body and
/// its answer comes back as XML.
async fn sns(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
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
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/sns/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// A topic called `name`; answers the ARN a pipe names it by.
async fn given_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = sns(router, "CreateTopic", &[("Name", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTopic answered {status}: {created}"
    );
    xml_text(&created, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TopicArn in {created}"))
}

/// `SourceParameters` for a Kinesis stream read from its oldest record one record at a time,
/// with the dead-letter destination and retry budget the row configures.
fn source_parameters(dead_letter: Option<&str>, retries: Option<i64>) -> Value {
    let mut stream = json!({ "StartingPosition": "TRIM_HORIZON", "BatchSize": 1 });
    if let Some(arn) = dead_letter {
        stream["DeadLetterConfig"] = json!({ "Arn": arn });
    }
    if let Some(retries) = retries {
        stream["MaximumRetryAttempts"] = json!(retries);
    }
    json!({ "KinesisStreamParameters": stream })
}

/// `CreatePipe` for one case: the Kinesis source and queue target it names, and the
/// dead-letter destination the row configures.
async fn create_pipe(
    router: &MultiServiceRouter,
    name: &str,
    source_arn: &str,
    target_arn: &str,
    dead_letter: Option<&str>,
    retries: Option<i64>,
) -> Reply {
    let body = json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": ROLE,
        "SourceParameters": source_parameters(dead_letter, retries),
    });
    call(
        router,
        "POST",
        &format!("/v1/pipes/{name}"),
        Some(body),
        REGION,
    )
    .await
}

/// One row of the destination table: what the configured `DeadLetterConfig.Arn` names, and
/// where the creation lands.
struct Row {
    /// Also the pipe's name, so every row is independent.
    case: &'static str,
    /// The `DeadLetterConfig.Arn` the creation configures, built from the account id this
    /// emulator mints resources under, or `None` for a creation that configures none.
    arn: Option<fn(&str) -> String>,
    /// The `CurrentState` the pipe has to settle in.
    settles_in: &'static str,
}

/// Every ARN below is written against the account this emulator mints resources under, so no
/// row depends on a literal account id.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// RULE: a pipe whose `DeadLetterConfig.Arn` names neither an Amazon SQS queue nor an Amazon
/// SNS topic is accepted and then settles in `CREATE_FAILED` with a `StateReason` naming the
/// destination, and it polls nothing; one naming either of those two reaches RUNNING and
/// moves its source's records.
///
/// The rows differ along what the configured ARN names, which is what the rule generalises
/// over: each of the two admitted kinds, a resource of four other services, a sub-resource of
/// an admitted service rather than the destination itself, and a configuration naming no
/// destination. A value that is no ARN at all is not among them: `DeadLetterConfig$Arn`
/// carries an `@pattern` the request boundary refuses it against, so it never reaches the
/// state machine.
#[tokio::test]
async fn create_pipe_fails_a_pipe_whose_dead_letter_destination_is_no_queue_or_topic() {
    let rows = [
        Row {
            case: "no-destination-at-all",
            arn: None,
            settles_in: RUNNING,
        },
        Row {
            case: "an-sqs-queue",
            arn: Some(|account| format!("arn:{PARTITION}:sqs:{REGION}:{account}:dead-letters")),
            settles_in: RUNNING,
        },
        Row {
            case: "an-sns-topic",
            arn: Some(|account| format!("arn:{PARTITION}:sns:{REGION}:{account}:dead-letters")),
            settles_in: RUNNING,
        },
        Row {
            // A queue of ANOTHER account is still a queue: whether that account holds it is
            // not something this account's records show.
            case: "a-queue-of-another-account",
            arn: Some(|_| format!("arn:{PARTITION}:sqs:{REGION}:111111111111:dead-letters")),
            settles_in: RUNNING,
        },
        Row {
            case: "a-kinesis-stream",
            arn: Some(|account| {
                format!("arn:{PARTITION}:kinesis:{REGION}:{account}:stream/dead-letters")
            }),
            settles_in: CREATE_FAILED,
        },
        Row {
            case: "a-lambda-function",
            arn: Some(|account| {
                format!("arn:{PARTITION}:lambda:{REGION}:{account}:function:dead-letters")
            }),
            settles_in: CREATE_FAILED,
        },
        Row {
            case: "an-event-bus",
            arn: Some(|account| {
                format!("arn:{PARTITION}:events:{REGION}:{account}:event-bus/dead-letters")
            }),
            settles_in: CREATE_FAILED,
        },
        Row {
            case: "a-dynamodb-table",
            arn: Some(|account| {
                format!("arn:{PARTITION}:dynamodb:{REGION}:{account}:table/dead-letters")
            }),
            settles_in: CREATE_FAILED,
        },
        Row {
            // An SNS SUBSCRIPTION of the topic, not the topic: the member names the
            // destination itself.
            case: "an-sns-subscription",
            arn: Some(|account| {
                format!("arn:{PARTITION}:sns:{REGION}:{account}:dead-letters:3f1c9a0e")
            }),
            settles_in: CREATE_FAILED,
        },
        Row {
            // The queue namespace with a compound resource field, which addresses no queue:
            // a queue's ARN carries its name and nothing else.
            case: "a-path-under-the-queue-namespace",
            arn: Some(|account| {
                format!("arn:{PARTITION}:sqs:{REGION}:{account}:dead-letters/inner")
            }),
            settles_in: CREATE_FAILED,
        },
    ];

    for row in rows {
        let router = emulator();
        let source_arn = given_stream(&router, &format!("{}-source", row.case)).await;
        let (target_url, target_arn) =
            given_queue(&router, &format!("{}-target", row.case)).await;
        let configured = row.arn.map(|build| build(&account()));

        let created = create_pipe(
            &router,
            row.case,
            &source_arn,
            &target_arn,
            configured.as_deref(),
            Some(0),
        )
        .await;
        // The creation is ACCEPTED either way: an unusable destination is a state the pipe
        // settles in, not an answer to this call.
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePipe answered {} {}",
            row.case, created.status, created.body
        );
        assert_eq!(
            created.string("CurrentState"),
            "CREATING",
            "case {:?}: CreatePipe must report the pipe CREATING, got {}",
            row.case,
            created.body
        );

        let described = settled(&router, row.case, row.case).await;
        let state = described.string("CurrentState");
        assert_eq!(
            state, row.settles_in,
            "case {:?}: expected the pipe to settle in {}, got {state} ({})",
            row.case, row.settles_in, described.body
        );
        let reason = described.string("StateReason");
        match (row.settles_in, configured.as_deref()) {
            (CREATE_FAILED, Some(arn)) => assert!(
                reason.contains(arn),
                "case {:?}: expected the StateReason to name {arn}, got {reason:?}",
                row.case
            ),
            // A pipe over a source read PER SHARD reports what its poller made of its last
            // pass once it rests in RUNNING, and nothing has been put on the stream yet.
            _ => assert_eq!(
                reason, NO_RECORDS_PROCESSED_STATE_REASON,
                "case {:?}: a completed creation rests in RUNNING over a stream whose records \
                 it has processed none of, so its StateReason is \
                 {NO_RECORDS_PROCESSED_STATE_REASON}, got {reason:?}",
                row.case
            ),
        }

        // What a client sees of the data plane: a pipe that reached RUNNING moves the
        // record, and one that did not leaves the target empty.
        put_record(
            &router,
            &source_arn,
            row.case,
            &format!("{}-record", row.case),
        )
        .await;
        if state == RUNNING {
            let delivered = collect_at_least(&router, &target_url, 1, BUDGET).await;
            assert_eq!(
                delivered.len(),
                1,
                "case {:?}: expected the target to hold the one record the pipe moved, got \
                 {delivered:?}",
                row.case
            );
        } else {
            let delivered = collect_during(&router, &target_url, QUIET).await;
            assert!(
                delivered.is_empty(),
                "case {:?}: expected the target to hold nothing, got {delivered:?}",
                row.case
            );
        }
    }
}

/// One row of the lifecycle table: which write is asked to bring the pipe up carrying the
/// destination under test, and where that write leaves the pipe.
struct WriteRow {
    case: &'static str,
    /// The lifecycle call driven after the pipe exists.
    operation: &'static str,
    /// The `DeadLetterConfig.Arn` the pipe is CREATED with.
    created_with: Option<fn(&str) -> String>,
    /// The `DeadLetterConfig.Arn` the driven call leaves the pipe with, for an `UpdatePipe`
    /// row; `None` leaves the stored configuration alone.
    updated_to: Option<fn(&str) -> String>,
    /// The `CurrentState` the pipe has to settle in after that call.
    settles_in: &'static str,
}

/// RULE: every lifecycle write that reports a pipe on its way to a healthy state asks the
/// same question of the destination the pipe would then carry, so a start of a pipe holding
/// an unusable one settles in `START_FAILED` and an update that stores one settles in
/// `UPDATE_FAILED` — each naming the destination — while a write that leaves the pipe
/// carrying an admitted destination reaches the state its caller asked for.
///
/// The rows differ along which write is driven and along which of the two configurations —
/// the stored one or the one the write installs — carries the unusable destination, which is
/// what the rule generalises over.
#[tokio::test]
async fn every_lifecycle_write_refuses_a_dead_letter_destination_that_is_no_queue_or_topic() {
    let a_queue: fn(&str) -> String =
        |account| format!("arn:{PARTITION}:sqs:{REGION}:{account}:dead-letters");
    let a_topic: fn(&str) -> String =
        |account| format!("arn:{PARTITION}:sns:{REGION}:{account}:dead-letters");
    let a_stream: fn(&str) -> String =
        |account| format!("arn:{PARTITION}:kinesis:{REGION}:{account}:stream/dead-letters");

    let rows = [
        WriteRow {
            case: "start-with-an-unusable-stored-destination",
            operation: "StartPipe",
            created_with: Some(a_stream),
            updated_to: None,
            settles_in: START_FAILED,
        },
        WriteRow {
            case: "start-with-a-queue-destination",
            operation: "StartPipe",
            created_with: Some(a_queue),
            updated_to: None,
            settles_in: RUNNING,
        },
        WriteRow {
            case: "update-that-installs-an-unusable-destination",
            operation: "UpdatePipe",
            created_with: Some(a_queue),
            updated_to: Some(a_stream),
            settles_in: UPDATE_FAILED,
        },
        WriteRow {
            case: "update-that-installs-a-topic-destination",
            operation: "UpdatePipe",
            created_with: Some(a_stream),
            updated_to: Some(a_topic),
            settles_in: RUNNING,
        },
        WriteRow {
            case: "update-that-leaves-an-unusable-destination-stored",
            operation: "UpdatePipe",
            created_with: Some(a_stream),
            updated_to: None,
            settles_in: UPDATE_FAILED,
        },
    ];

    for row in rows {
        let router = emulator();
        let source_arn = given_stream(&router, &format!("{}-source", row.case)).await;
        let (_, target_arn) = given_queue(&router, &format!("{}-target", row.case)).await;
        let created_with = row.created_with.map(|build| build(&account()));
        let created = create_pipe(
            &router,
            row.case,
            &source_arn,
            &target_arn,
            created_with.as_deref(),
            Some(0),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePipe answered {} {}",
            row.case, created.status, created.body
        );
        // Whatever the creation came to, the pipe has to have finished settling before
        // another write is accepted on it.
        settled(&router, row.case, row.case).await;

        let updated_to = row.updated_to.map(|build| build(&account()));
        let driven = match row.operation {
            "StartPipe" => {
                call(
                    &router,
                    "POST",
                    &format!("/v1/pipes/{}/start", row.case),
                    None,
                    REGION,
                )
                .await
            }
            "UpdatePipe" => {
                let mut body = json!({ "RoleArn": ROLE });
                if let Some(arn) = updated_to.as_deref() {
                    // `UpdatePipeSourceParameters` overrides the stored object as ONE, so
                    // the retry budget travels with the destination.
                    body["SourceParameters"] = json!({
                        "KinesisStreamParameters": {
                            "BatchSize": 1,
                            "MaximumRetryAttempts": 0,
                            "DeadLetterConfig": { "Arn": arn },
                        }
                    });
                }
                call(
                    &router,
                    "PUT",
                    &format!("/v1/pipes/{}", row.case),
                    Some(body),
                    REGION,
                )
                .await
            }
            other => panic!("case {:?}: no lifecycle call named {other:?}", row.case),
        };
        assert_eq!(
            driven.status, 200,
            "case {:?}: {} answered {} {}",
            row.case, row.operation, driven.status, driven.body
        );

        let described = settled(&router, row.case, row.case).await;
        let state = described.string("CurrentState");
        assert_eq!(
            state, row.settles_in,
            "case {:?}: expected the pipe to settle in {}, got {state} ({})",
            row.case, row.settles_in, described.body
        );
        // Which destination the write left the pipe with is what it is judged on.
        let effective = updated_to.as_deref().or(created_with.as_deref());
        let reason = described.string("StateReason");
        if row.settles_in == RUNNING {
            assert_eq!(
                reason, NO_RECORDS_PROCESSED_STATE_REASON,
                "case {:?}: a completed write rests in RUNNING over a stream whose records it \
                 has processed none of, so its StateReason is \
                 {NO_RECORDS_PROCESSED_STATE_REASON}, got {reason:?}",
                row.case
            );
        } else {
            let named = effective.unwrap_or_default();
            assert!(
                reason.contains(named),
                "case {:?}: expected the StateReason to name {named}, got {reason:?}",
                row.case
            );
        }
    }
}

/// Where a row reads the record of a discarded batch back from, once it has landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An Amazon SQS queue destination: the record is a message on that queue.
    Queue,
    /// An Amazon SNS topic destination: the record is a message published to the topic,
    /// which a queue subscribed to it then holds.
    Topic,
}

/// The queue policy AWS documents for a queue subscribed to a topic: the service principal
/// `sns.amazonaws.com`, allowed `sqs:SendMessage` on that queue.
fn grant_to_sns(queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": { "Service": "sns.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string()
}

/// RULE: the record of a batch a pipe gave up on reaches the destination its
/// `DeadLetterConfig.Arn` names, whichever of the two admitted kinds that is — a message on
/// the queue, or a publication to the topic the subscribers of that topic then hold.
///
/// The rows differ along the KIND of destination configured, which is what the rule
/// generalises over. The batch is given up on because the pipe's target names a queue the
/// account does not hold, with no retry allowed beyond the first attempt.
#[tokio::test]
async fn the_record_of_a_discarded_batch_reaches_either_kind_of_destination() {
    // (case, the kind of destination configured)
    let rows: [(&str, Kind); 2] = [
        ("a-queue-destination", Kind::Queue),
        ("a-topic-destination", Kind::Topic),
    ];

    for (case, kind) in rows {
        let router = emulator();
        let source_arn = given_stream(&router, &format!("{case}-source")).await;
        // The destination itself, and the queue a client reads the record off: for a topic
        // that queue is a SUBSCRIBER of the topic rather than the destination.
        let (sink_url, sink_arn) = given_queue(&router, &format!("{case}-sink")).await;
        let destination = match kind {
            Kind::Queue => sink_arn.clone(),
            Kind::Topic => {
                let topic = given_topic(&router, case, &format!("{case}-topic")).await;
                let policy = grant_to_sns(&sink_arn);
                let (status, body) = crate::pipe_data_plane::sqs(
                    &router,
                    "SetQueueAttributes",
                    &json!({ "QueueUrl": sink_url, "Attributes": { "Policy": policy } }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: SetQueueAttributes answered {status} {body}"
                );
                let (status, subscribed) = sns(
                    &router,
                    "Subscribe",
                    &[
                        ("TopicArn", &topic),
                        ("Protocol", "sqs"),
                        ("Endpoint", &sink_arn),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: Subscribe answered {status}: {subscribed}"
                );
                topic
            }
        };
        // A target the account holds nothing at, so every attempt at the batch fails at the
        // target stage and the one attempt the pipe allows is spent at once.
        let absent_target = format!("arn:{PARTITION}:sqs:{REGION}:{}:no-such-queue", account());

        let created = create_pipe(
            &router,
            case,
            &source_arn,
            &absent_target,
            Some(&destination),
            Some(0),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        let described = settled(&router, case, case).await;
        assert_eq!(
            described.string("CurrentState"),
            RUNNING,
            "case {case:?}: expected the pipe to run, got {}",
            described.body
        );

        put_record(&router, &source_arn, case, "the-record-that-is-given-up-on").await;
        let records = collect_at_least(&router, &sink_url, 1, BUDGET).await;
        let body = records
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: no record of the discarded batch"));
        // A topic hands its subscribers the publication inside its own notification
        // envelope, whose `Message` is what was published; a queue destination holds the
        // record itself.
        let document: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the record is a JSON document ({error}): {body:?}")
        });
        let record: Value = match kind {
            Kind::Queue => document,
            Kind::Topic => {
                let published = document["Message"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: no Message in the notification {document}")
                });
                serde_json::from_str(published).unwrap_or_else(|error| {
                    panic!(
                        "case {case:?}: the published record is a JSON document ({error}): \
                         {published:?}"
                    )
                })
            }
        };
        assert_eq!(
            record["KinesisBatchInfo"]["streamArn"],
            json!(source_arn),
            "case {case:?}: expected the record to point at the source stream, got {record}"
        );
        assert_eq!(
            record["context"]["stage"],
            json!("TARGET"),
            "case {case:?}: expected the stage that gave up, got {record}"
        );
        assert_eq!(
            record["context"]["condition"],
            json!("RetryAttemptsExhausted"),
            "case {case:?}: expected the retries to be reported as spent, got {record}"
        );
    }
}
