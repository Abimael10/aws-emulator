// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the checks `CreateCrawler` makes on the crawler role's access to the
//! stores a crawl READS its targets through: the Amazon SQS queue an S3 target is crawled
//! from, and the Amazon DynamoDB table a `DynamoDBTarget` names.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler. The IAM records
//! the check reads are seeded into the SAME shared engine the service reads them from —
//! the store `CreateRole`, `PutRolePolicy` and `AttachRolePolicy` write.
//!
//! The rule under test: a crawl runs AS the crawler's `Role`, so `CreateCrawler` refuses the
//! create with `InvalidInputException` (HTTP 400) — storing no crawler — when the policies in
//! force on that role do not admit the calls a crawl makes on a store one of its targets
//! names, and creates the identically-shaped crawler whose role does hold them. A role
//! nothing can be shown about is not refused.
//!
//! For an S3 target that names an `EventQueueArn`, those calls are the queue operations an
//! event-mode crawl makes. WHICH refusal it is follows from what those same policies show the
//! role about the queue ITSELF: a role they admit some call of the queue's own service on it
//! to is told the grant its crawl needs, naming the queue calls a crawl in event mode makes,
//! because a queue it may touch is a queue that is there and a missing grant is all there is
//! left to report; a role they admit no call on it to is shown nothing that tells an absent
//! queue from one it may not reach, and gets exactly the sentence that conflates those two
//! and asks for no grant.
//!
//! For a DynamoDB target they are the reads the crawl makes on the table it names — its
//! definition and the scan of its items — and the refusal names that whole grant.
//!
//! Either refusal is Glue's OWN sentence for a store a crawl cannot read, not that store's
//! authorization answer: its one variable part is the target the request carried, and it names
//! neither the identity the crawl would run as nor which of IAM's rules withheld the grant.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::EVENT_QUEUE_ACTIONS;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    name_key, role_arn, ComAmazonawsIamAwsidentitymanagementv20100508State as IamState,
    InlinePolicy, DEFAULT_PATH,
};

use crate::wire::{call, emulator_on, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The queue an S3 target is crawled from in these tests.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT_ID}:{name}")
}

/// Seed `role_name` into the shared engine's IAM store, carrying `documents` as its
/// inline policies — the records `CreateRole` and `PutRolePolicy` write.
fn seed_role(engine: &Arc<engine::InMemoryEngine>, role_name: &str, documents: &[Value]) -> String {
    let state = engine.state::<IamState>();
    let mut store = state.lock();
    let arn = role_arn(DEFAULT_PATH, role_name);
    store.roles.insert(
        name_key(role_name),
        Role {
            path: DEFAULT_PATH.to_string(),
            role_name: role_name.to_string(),
            role_id: format!("AROA{}", role_name.to_uppercase()),
            arn: arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            assume_role_policy_document: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "glue.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                })
                .to_string(),
            ),
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let mut policies: BTreeMap<String, InlinePolicy> = BTreeMap::new();
    for (index, document) in documents.iter().enumerate() {
        let policy_name = format!("{role_name}-policy-{index}");
        policies.insert(
            name_key(&policy_name),
            InlinePolicy {
                policy_name,
                policy_document: document.to_string(),
            },
        );
    }
    store
        .role_inline_policies
        .insert(name_key(role_name), policies);
    arn
}

/// An identity policy document allowing `actions` on `resource`.
fn allow(actions: &[&str], resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }],
    })
}

/// The sentence Glue refuses an event-mode create with when the role is shown nothing about
/// the queue a target names: the queue ARN as the request supplied it, inside fixed text that
/// conflates the queue's absence with the role's access to it, and no queue call named.
///
/// Spelled out here rather than built from the service's own constructor, so the wording this
/// suite pins is the test's and not the code's.
fn expected_inaccessible(queue_arn: &str) -> String {
    format!("SQS queue {queue_arn} does not exist or the role provided does not have access to it.")
}

/// The sentence Glue refuses an event-mode create with when the role is shown SOMETHING about
/// the queue but may not consume it: the conflating sentence above, then the grant a crawl
/// needs on that queue — the ACTIONS written out here in the order a crawl makes them, so the
/// wording this suite pins is the test's and not the code's.
fn expected_grant_missing(queue_arn: &str) -> String {
    format!(
        "SQS queue {queue_arn} does not exist or the role provided does not have access to \
         it. Please make sure the role has SQS:ReceiveMessage, SQS:GetQueueAttributes and \
         SQS:DeleteMessage permissions on the queue."
    )
}

/// What a row of the tables below expects of its create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    /// The role holds the whole grant, so the create is admitted.
    Created,
    /// The role is shown nothing about the queue: the conflating sentence, naming no action.
    Inaccessible,
    /// The role may make some call on the queue but not the crawl's: the sentence naming the
    /// grant.
    GrantMissing,
}

impl Expected {
    /// The message a refused row must carry, or `None` where the row is created.
    fn message(self, queue_arn: &str) -> Option<String> {
        match self {
            Expected::Created => None,
            Expected::Inaccessible => Some(expected_inaccessible(queue_arn)),
            Expected::GrantMissing => Some(expected_grant_missing(queue_arn)),
        }
    }
}

/// A `CreateCrawler` body whose single S3 target is crawled from `queue`, in event mode.
fn event_mode_crawler(name: &str, role: &str, queue: &str) -> Value {
    json!({
        "Name": name,
        "Role": role,
        "Targets": {
            "S3Targets": [{ "Path": "s3://crawler-bucket/data/", "EventQueueArn": queue }],
        },
        "RecrawlPolicy": { "RecrawlBehavior": "CRAWL_EVENT_MODE" },
    })
}

/// One row of the table below: what the case is, the identity policy documents the
/// crawler's role carries, and what they earn the create.
type Row = (String, Vec<Value>, Expected);

/// A role whose policies do not admit the crawl's queue operations cannot back an
/// event-mode crawler, and the identically-shaped request whose role does admit them is
/// created.
///
/// The rows differ along WHAT the role's policies grant: nothing at all, the queue
/// operations on another queue, another service's action on the queue's own ARN, one unrelated
/// call on the queue, the queue's management calls without any of the crawl's, a subset of the
/// crawl's calls (one row per action the crawl makes, each missing in turn), a `Deny` beside
/// an `Allow` of all, and the full grant — written as the exact ARN, through a wildcard, and
/// spread over two documents.
///
/// Whether a `Deny` or an absent grant withheld a call decides nothing about the wording; what
/// does is whether those documents admit the role ANY call on that queue, which is the one
/// thing that tells "the queue may not be there" from "grant your role these queue calls".
#[tokio::test]
async fn an_event_mode_crawler_needs_its_role_to_consume_the_event_queue() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let queue = queue_arn("glue-crawler-events");
    let every_action: Vec<&str> = EVENT_QUEUE_ACTIONS.to_vec();
    // The queue call the crawl makes first, which is the one a role granted nothing is
    // refused at.
    let first_action = EVENT_QUEUE_ACTIONS[0];
    let last_action = EVENT_QUEUE_ACTIONS[EVENT_QUEUE_ACTIONS.len() - 1];
    // A document DENYING `actions` on the queue, whatever else is granted.
    let deny = |actions: &[&str]| {
        json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Deny", "Action": actions, "Resource": queue }],
        })
    };

    // (case, the documents the role carries, what they earn the create)
    let mut rows: Vec<Row> = vec![
        (
            "no policy at all".to_string(),
            Vec::new(),
            Expected::Inaccessible,
        ),
        (
            "the queue operations on another queue".to_string(),
            vec![allow(&every_action, &queue_arn("some-other-queue"))],
            Expected::Inaccessible,
        ),
        (
            "another service's action on the queue's own ARN".to_string(),
            vec![allow(&["s3:GetObject"], &queue)],
            Expected::Inaccessible,
        ),
        (
            "one queue call, and none of the crawl's".to_string(),
            vec![allow(&["sqs:GetQueueUrl"], &queue)],
            Expected::GrantMissing,
        ),
        (
            "a send grant on the queue, and none of the crawl's".to_string(),
            vec![allow(&["sqs:SendMessage"], &queue)],
            Expected::GrantMissing,
        ),
        (
            "the queue's management calls but none of the crawl's".to_string(),
            vec![allow(
                &["sqs:GetQueueUrl", "sqs:SetQueueAttributes", "sqs:PurgeQueue"],
                &queue,
            )],
            Expected::GrantMissing,
        ),
        (
            "every queue operation, named exactly".to_string(),
            vec![allow(&every_action, &queue)],
            Expected::Created,
        ),
        (
            "every queue operation, through a wildcard".to_string(),
            vec![allow(&["sqs:*"], &format!("{}*", queue_arn("")))],
            Expected::Created,
        ),
        (
            "every queue operation, spread over two documents".to_string(),
            vec![
                allow(&every_action[..1], &queue),
                allow(&every_action[1..], &queue),
            ],
            Expected::Created,
        ),
        (
            "an explicit deny of the first operation beside the grant".to_string(),
            vec![allow(&every_action, &queue), deny(&[first_action])],
            Expected::GrantMissing,
        ),
        (
            "an explicit deny of the last operation beside the grant".to_string(),
            vec![allow(&every_action, &queue), deny(&[last_action])],
            Expected::GrantMissing,
        ),
        (
            "a deny of every queue call beside the grant".to_string(),
            vec![allow(&every_action, &queue), deny(&["sqs:*"])],
            Expected::Inaccessible,
        ),
    ];
    // One row per action the crawl makes, each one missing from an otherwise complete
    // grant: every action is required, so each omission refuses on its own — and what is left
    // is still a call on that queue, so each omission is told which grant to add.
    for missing in EVENT_QUEUE_ACTIONS {
        let granted: Vec<&str> = every_action
            .iter()
            .copied()
            .filter(|action| *action != missing)
            .collect();
        rows.push((
            format!("every queue operation except {missing}"),
            vec![allow(&granted, &queue)],
            Expected::GrantMissing,
        ));
    }

    for (index, (case, documents, expected)) in rows.iter().enumerate() {
        let role_name = format!("crawler-role-{index}");
        let role = seed_role(&engine, &role_name, documents);
        let crawler = format!("event-crawler-{index}");
        let request = event_mode_crawler(&crawler, &role, &queue);

        let (status, body) = call(&router, "CreateCrawler", &request).await;
        let Some(expected_message) = expected.message(&queue) else {
            assert_eq!(
                status, 200,
                "case {case:?}: the role holds the grant, so the create must be admitted, \
                 got {status} with {body}"
            );
            let read = ok(&router, "GetCrawler", &json!({ "Name": crawler })).await;
            assert_eq!(
                read["Crawler"]["Name"].as_str(),
                Some(crawler.as_str()),
                "case {case:?}: the created crawler must be readable, got {read}"
            );
            continue;
        };

        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {}",
            error_code(&body)
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected the {expected:?} refusal naming {queue}, got {message:?}"
        );
        // No crawler was stored: the refused create left nothing behind.
        let (status, body) = call(&router, "GetCrawler", &json!({ "Name": crawler })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no crawler, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case:?}: expected EntityNotFoundException, got {}",
            error_code(&body)
        );
    }
}

/// Either refusal is worded from the REQUEST: whichever way the request names the role — its
/// bare name or its ARN — the one variable part of the sentence is the queue the target
/// carried, exactly as sent, and the role the crawl would run as is not disclosed.
#[tokio::test]
async fn the_refusal_is_worded_from_the_request() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    // A role granted nothing at all, so nothing is shown about any queue.
    let silent_arn = seed_role(&engine, "worded-role", &[]);
    // ... and one granted ONE queue call on every queue of this account, so it may touch each
    // of them and consume none.
    let one_call = allow(&["sqs:GetQueueUrl"], &format!("{}*", queue_arn("")));
    let reaching_arn = seed_role(&engine, "reaching-role", &[one_call]);

    // (case, the `Role` the request carries, the queue the target names, what it earns)
    let rows = [
        (
            "the role's ARN and a plainly named queue",
            silent_arn.clone(),
            queue_arn("worded-events"),
            Expected::Inaccessible,
        ),
        (
            "the role's bare NAME and another queue",
            "worded-role".to_string(),
            queue_arn("worded-events-2.fifo"),
            Expected::Inaccessible,
        ),
        (
            "a reaching role's ARN and a plainly named queue",
            reaching_arn.clone(),
            queue_arn("worded-events-3"),
            Expected::GrantMissing,
        ),
        (
            "a reaching role's bare NAME and another queue",
            "reaching-role".to_string(),
            queue_arn("worded-events-4.fifo"),
            Expected::GrantMissing,
        ),
    ];

    for (index, (case, role, queue, expected)) in rows.iter().enumerate() {
        let crawler = format!("worded-crawler-{index}");
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &event_mode_crawler(&crawler, role, queue),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message,
            expected
                .message(queue)
                .expect("every row of this table is a refusal"),
            "case {case:?}: the refusal must name the queue the request carried, got \
             {message:?}"
        );
        // Glue words its own refusal, so the identity the crawl would run as is not in it.
        for role_arn in [&silent_arn, &reaching_arn] {
            assert!(
                !message.contains(role_arn),
                "case {case:?}: the refusal must not disclose the role, got {message:?}"
            );
        }
    }
}

/// The two refusals are told APART by whether the role's policies admit it any call on the
/// queue, and the one for a role that may touch the queue is the one that names the grant.
///
/// Both halves matter to a caller: a client keyed on Glue's sentence for a queue it cannot
/// read matches either, and one keyed on the grant to add matches only the second — so the
/// sentence for a role shown nothing about the queue names no queue call at all, and the one
/// for a role that may touch it names every call the crawl makes.
///
/// The rows differ along what the role is granted: nothing, another service's action on the
/// queue's ARN, the crawl's own calls on a DIFFERENT queue, and — one row per name the queue
/// service publishes as grantable (`state::authorization::policy_action_names` of that
/// service) — that one call on this queue, so no call it serves is left out of the question
/// and the rows reach far past the ones this suite names by hand.
#[tokio::test]
async fn the_refusal_names_the_grant_only_when_the_role_may_touch_the_queue() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let queue = queue_arn("told-apart-events");
    let crawl_actions: Vec<&str> = EVENT_QUEUE_ACTIONS.to_vec();

    // (case, the documents the role carries, whether they admit it any call on THIS queue)
    let mut rows: Vec<(String, Vec<Value>, bool)> = vec![
        (
            "a role granted nothing on the queue".to_string(),
            Vec::new(),
            false,
        ),
        (
            "a role granted another service's action on the queue's ARN".to_string(),
            vec![allow(&["s3:GetObject"], &queue)],
            false,
        ),
        (
            "a role granted the crawl's calls on another queue".to_string(),
            vec![allow(&crawl_actions, &queue_arn("told-apart-other"))],
            false,
        ),
    ];
    // One row per action an Amazon SQS policy may state, each granted ALONE: a role admitted
    // any one of them may touch the queue, so each is told which grant the crawl still needs —
    // including the names this suite never writes out.
    for action in ::service_com_amazonaws_sqs_amazonsqs::state::authorization::policy_action_names()
    {
        let qualified = format!("sqs:{action}");
        // A role granted every call the crawl makes is not refused at all, so the one action
        // it is granted here must leave at least one of them withheld; granting one at a time
        // always does, since the crawl makes more than one.
        rows.push((
            format!("a role granted {qualified} alone"),
            vec![allow(&[qualified.as_str()], &queue)],
            true,
        ));
    }

    for (index, (case, documents, reachable)) in rows.iter().enumerate() {
        let role = seed_role(&engine, &format!("told-apart-{index}"), documents);
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &event_mode_crawler(&format!("told-apart-crawler-{index}"), &role, &queue),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        let message = error_message(&body);
        // The clause that conflates an absent queue with one the role may not reach is in
        // BOTH refusals, naming the queue the target carried.
        assert!(
            message.starts_with(&expected_inaccessible(&queue)),
            "case {case:?}: every refusal must carry Glue's sentence for a queue a crawl \
             cannot read, got {message:?}"
        );
        for action in EVENT_QUEUE_ACTIONS {
            assert_eq!(
                message.contains(action),
                *reachable,
                "case {case:?}: the grant {action} must be named only where the role may \
                 touch the queue (may touch it: {reachable}), got {message:?}"
            );
        }
        assert_eq!(
            message,
            if *reachable {
                expected_grant_missing(&queue)
            } else {
                expected_inaccessible(&queue)
            },
            "case {case:?}: wrong refusal for a role that may{} touch the queue, got \
             {message:?}",
            if *reachable { "" } else { " not" }
        );
    }
}

/// The check is about the queue the TARGET names, so it applies to each S3 target of a
/// create and to the dead-letter queue beside the event queue — and a target naming no
/// queue at all is crawled without one, so nothing is checked for it.
#[tokio::test]
async fn the_check_covers_every_queue_a_target_names() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let events = queue_arn("target-events");
    let dead_letter = queue_arn("target-dead-letter");
    let actions: Vec<&str> = EVENT_QUEUE_ACTIONS.to_vec();

    // A role granted the queue operations on the EVENT queue only.
    let role = seed_role(&engine, "partial-role", &[allow(&actions, &events)]);

    // A target naming no queue is crawled without one: nothing is checked, so the
    // create is admitted even though the role grants nothing for any queue.
    let plain = seed_role(&engine, "no-queue-role", &[]);
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "plain-crawler",
            "Role": plain,
            "Targets": { "S3Targets": [{ "Path": "s3://crawler-bucket/plain/" }] },
        }),
    )
    .await;

    // The dead-letter queue of the same target is a queue the crawl consumes too, so a
    // role with no grant on it is refused.
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "dlq-crawler",
            "Role": role,
            "Targets": {
                "S3Targets": [{
                    "Path": "s3://crawler-bucket/data/",
                    "EventQueueArn": events,
                    "DlqEventQueueArn": dead_letter,
                }],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a dead-letter queue the role cannot consume\": expected 400, got {status} \
         with {body}"
    );
    assert!(
        error_message(&body).contains(&dead_letter),
        "case \"a dead-letter queue the role cannot consume\": the refusal must name that \
         queue, got {:?}",
        error_message(&body)
    );

    // A SECOND S3 target's queue is checked as well, not only the first.
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "second-target-crawler",
            "Role": role,
            "Targets": {
                "S3Targets": [
                    { "Path": "s3://crawler-bucket/first/", "EventQueueArn": events },
                    { "Path": "s3://crawler-bucket/second/", "EventQueueArn": dead_letter },
                ],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a second target's queue\": expected 400, got {status} with {body}"
    );

    // And the crawler whose every named queue the role can consume is created.
    let both = seed_role(
        &engine,
        "both-queues-role",
        &[allow(&actions, &events), allow(&actions, &dead_letter)],
    );
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "both-queues-crawler",
            "Role": both,
            "Targets": {
                "S3Targets": [{
                    "Path": "s3://crawler-bucket/data/",
                    "EventQueueArn": events,
                    "DlqEventQueueArn": dead_letter,
                }],
            },
        }),
    )
    .await;
    let read = ok(
        &router,
        "GetCrawler",
        &json!({ "Name": "both-queues-crawler" }),
    )
    .await;
    assert_eq!(
        read["Crawler"]["Targets"]["S3Targets"][0]["EventQueueArn"].as_str(),
        Some(events.as_str()),
        "the created crawler must report the queue it was created with, got {read}"
    );
}

/// The number of queues one create may have decided about is bounded
/// (`MAX_CHECKED_EVENT_QUEUES`): a create naming that many DISTINCT queues is served, one
/// naming more is refused with the operation's modelled `InvalidInputException`, and the
/// same queue named by several targets counts once.
#[tokio::test]
async fn the_number_of_checked_event_queues_is_bounded() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    // A role granted the crawl's queue operations on every queue of this account, so a
    // refusal below can only be the ceiling and never a missing grant.
    let role = seed_role(
        &engine,
        "every-queue-role",
        &[allow(&["sqs:*"], &format!("{}*", queue_arn("")))],
    );

    let ceiling = service_com_amazonaws_glue_awsglue::state::MAX_CHECKED_EVENT_QUEUES;
    let targets = |count: usize| {
        let s3_targets: Vec<Value> = (0..count)
            .map(|index| {
                json!({
                    "Path": format!("s3://crawler-bucket/{index}/"),
                    "EventQueueArn": queue_arn(&format!("bounded-{index}")),
                })
            })
            .collect();
        json!({ "S3Targets": s3_targets })
    };

    // At the ceiling: served.
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "at-the-ceiling",
            "Role": role,
            "Targets": targets(ceiling),
        }),
    )
    .await;

    // One past it: refused, and nothing stored.
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "past-the-ceiling",
            "Role": role,
            "Targets": targets(ceiling + 1),
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"one queue past the ceiling\": expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case \"one queue past the ceiling\": expected InvalidInputException, got {}",
        error_code(&body)
    );
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &json!({ "Name": "past-the-ceiling" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"one queue past the ceiling\": a refused create stores nothing, got \
         {status} with {body}"
    );

    // The SAME queue named by more targets than the ceiling is one queue to decide
    // about, so it is served.
    let repeated: Vec<Value> = (0..ceiling + 10)
        .map(|index| {
            json!({
                "Path": format!("s3://crawler-bucket/repeat-{index}/"),
                "EventQueueArn": queue_arn("bounded-0"),
            })
        })
        .collect();
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "one-queue-many-targets",
            "Role": role,
            "Targets": { "S3Targets": repeated },
        }),
    )
    .await;
}

/// A `Role` that names no role this account holds grounds no refusal: the check refuses
/// only what the account's own IAM records SHOW, so such a create goes ahead.
#[tokio::test]
async fn a_role_this_account_does_not_hold_is_not_refused() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let queue = queue_arn("unknown-role-events");

    for (case, role) in [
        (
            "a role ARN of another account",
            "arn:aws:iam::210987654321:role/foreign-crawler".to_string(),
        ),
        (
            "a role name this account holds no role under",
            "never-created".to_string(),
        ),
    ] {
        let name = format!("unchecked-{}", case.replace(' ', "-"));
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &event_mode_crawler(&name, &role, &queue),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a role nothing can be shown about grounds no refusal, got \
             {status} with {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// The DynamoDB table a DynamoDBTarget names
// ---------------------------------------------------------------------------

/// The resource a policy statement about the DynamoDB table `table` names, written out here
/// rather than built from the crate's own helper so this suite pins the form the check decides
/// against: `arn:aws:dynamodb:<region>:<account>:table/<name>`, which is how that service
/// names a table.
fn table_arn(table: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT_ID}:table/{table}")
}

/// The whole sentence Glue refuses a create with when the crawler's role may not read the
/// DynamoDB table a target names: the table as the request supplied it inside fixed text that
/// conflates the table's absence with the role's access to it, then the grant the crawl needs
/// on it — the ACTIONS written out here in the order the crawl makes them — and last the
/// clause naming the component that refused, its status, its code and the id of the request
/// it refused. Written out here rather than read from the crate's own constructor, so the
/// wording this suite pins is the test's and not the code's; the request id is the clause's
/// one per-run field, so it is matched by [`without_request_id`] instead.
fn expected_table_grant_missing(table: &str) -> String {
    format!(
        "DynamoDB table {table} does not exist or the role provided does not have access to \
         it. Please make sure the role has dynamodb:DescribeTable and dynamodb:Scan \
         permissions on the table. (Service: glue; Status Code: 400; Error Code: \
         InvalidInputException; Request ID: <id>; Proxy: null)"
    )
}

/// `message` with the id of the request the refusal names replaced by a placeholder, so the
/// sentence a suite compares is everything BUT that per-run field.
///
/// The id is asserted to be there and to have the form this emulator's refusals report an id
/// in (`service_common::query_error_envelope::is_request_id`, the 8-4-4-4-12 form), and only
/// then taken out of the comparison.
fn without_request_id(case: &str, message: &str) -> String {
    let Some((head, tail)) = message.split_once("Request ID: ") else {
        panic!("case {case:?}: the refusal must name the request it answers, got {message:?}");
    };
    let Some((id, rest)) = tail.split_once(';') else {
        panic!("case {case:?}: the request id must end its clause, got {message:?}");
    };
    assert!(
        service_common::query_error_envelope::is_request_id(id),
        "case {case:?}: the refusal must name the request by an id of the form every other \
         refusal reports, got {id:?}"
    );
    format!("{head}Request ID: <id>;{rest}")
}

/// A `CreateCrawler` body whose single target is the DynamoDB table `table`.
fn dynamodb_crawler(name: &str, role: &str, table: &str) -> Value {
    json!({
        "Name": name,
        "Role": role,
        "Targets": { "DynamoDBTargets": [{ "Path": table }] },
    })
}

/// A role whose policies do not admit the reads a crawl of a DynamoDB target makes on that
/// table cannot back such a crawler, and the identically-shaped request whose role does admit
/// them is created.
///
/// The rows differ along WHAT the role's policies grant: nothing at all, the table reads on
/// another table, another service's action on the table's own ARN, one unrelated DynamoDB
/// call on it, a subset of the crawl's reads (one row per read it makes, each missing in
/// turn), an explicit `Deny` beside an `Allow` of all, and the full grant — written as the
/// exact ARN, through a wildcard, and spread over two documents. A role the account holds no
/// document for is shown nothing about the table, so it is admitted: only what the records
/// SHOW refuses a create. A refused row is told which
/// call was withheld: the first the crawl makes that the documents do not admit.
#[tokio::test]
async fn a_dynamodb_target_needs_its_role_to_read_the_table() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let table = "crawled-items";
    let arn = table_arn(table);
    let every_action: Vec<&str> =
        service_com_amazonaws_glue_awsglue::state::DYNAMODB_TARGET_ACTIONS.to_vec();
    let first_action = every_action[0];
    let last_action = every_action[every_action.len() - 1];
    let deny = |actions: &[&str]| {
        json!({
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Deny", "Action": actions, "Resource": arn }],
        })
    };

    // (case, the documents the role carries, whether they admit the whole crawl)
    let mut rows: Vec<(String, Vec<Value>, bool)> = vec![
        // A role the account holds no document for is shown NOTHING about the table, so it
        // grounds no refusal — the reading the crawl's own read authorization makes of the
        // same records.
        ("no policy at all".to_string(), Vec::new(), true),
        (
            "the table reads on another table".to_string(),
            vec![allow(&every_action, &table_arn("some-other-table"))],
            false,
        ),
        (
            "another service's action on the table's own ARN".to_string(),
            vec![allow(&["s3:GetObject"], &arn)],
            false,
        ),
        (
            "one DynamoDB call on the table, and none of the crawl's".to_string(),
            vec![allow(&["dynamodb:PutItem"], &arn)],
            false,
        ),
        (
            "every read the crawl makes, named exactly".to_string(),
            vec![allow(&every_action, &arn)],
            true,
        ),
        (
            "every read the crawl makes, through a wildcard".to_string(),
            vec![allow(&["dynamodb:*"], &format!("{}*", table_arn("")))],
            true,
        ),
        (
            "every read the crawl makes, spread over two documents".to_string(),
            vec![
                allow(&every_action[..1], &arn),
                allow(&every_action[1..], &arn),
            ],
            true,
        ),
        (
            "an explicit deny of the first read beside the grant".to_string(),
            vec![allow(&every_action, &arn), deny(&[first_action])],
            false,
        ),
        (
            "an explicit deny of the last read beside the grant".to_string(),
            vec![allow(&every_action, &arn), deny(&[last_action])],
            false,
        ),
    ];
    // One row per read the crawl makes, each one missing from an otherwise complete grant:
    // every one of them is required, so each omission refuses on its own.
    for missing in service_com_amazonaws_glue_awsglue::state::DYNAMODB_TARGET_ACTIONS {
        let granted: Vec<&str> = every_action
            .iter()
            .copied()
            .filter(|action| *action != missing)
            .collect();
        rows.push((
            format!("every read the crawl makes except {missing}"),
            vec![allow(&granted, &arn)],
            false,
        ));
    }

    for (index, (case, documents, admitted)) in rows.iter().enumerate() {
        let role = seed_role(&engine, &format!("ddb-crawler-role-{index}"), documents);
        let crawler = format!("ddb-crawler-{index}");
        let (status, body) = call(
            &router,
            "CreateCrawler",
            &dynamodb_crawler(&crawler, &role, table),
        )
        .await;

        if *admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: the role holds the grant, so the create must be admitted, \
                 got {status} with {body}"
            );
            let read = ok(&router, "GetCrawler", &json!({ "Name": crawler })).await;
            assert_eq!(
                read["Crawler"]["Name"].as_str(),
                Some(crawler.as_str()),
                "case {case:?}: the created crawler must be readable, got {read}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {}",
            error_code(&body)
        );
        let message = error_message(&body);
        assert_eq!(
            without_request_id(case, &message),
            expected_table_grant_missing(table),
            "case {case:?}: expected the refusal naming the table {table} and the grant the \
             crawl needs, got {message:?}"
        );
        // Glue words its own refusal, so the identity the crawl would run as is not in it.
        assert!(
            !message.contains(&role),
            "case {case:?}: the refusal must not disclose the role, got {message:?}"
        );
        // No crawler was stored: the refused create left nothing behind.
        let (status, body) = call(&router, "GetCrawler", &json!({ "Name": crawler })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no crawler, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case:?}: expected EntityNotFoundException, got {}",
            error_code(&body)
        );
    }
}

/// The check is about the table a TARGET names, so a crawler naming several is admitted only
/// when the role may read every one of them, the same table named twice is one table to decide
/// about, and a crawler with no DynamoDB target at all has nothing checked for it.
#[tokio::test]
async fn the_check_covers_every_dynamodb_table_a_target_names() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    let actions: Vec<&str> =
        service_com_amazonaws_glue_awsglue::state::DYNAMODB_TARGET_ACTIONS.to_vec();
    // A role granted the crawl's reads on ONE of the two tables below.
    let role = seed_role(
        &engine,
        "one-table-role",
        &[allow(&actions, &table_arn("readable-items"))],
    );

    // The target it may read is admitted, twice over: the same table named by two targets is
    // one table to decide about.
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "readable-crawler",
            "Role": role,
            "Targets": {
                "DynamoDBTargets": [
                    { "Path": "readable-items" },
                    { "Path": "readable-items", "scanAll": false },
                ],
            },
        }),
    )
    .await;

    // The second target names a table the role may not read, so the create is refused for
    // that table and nothing is stored.
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "mixed-crawler",
            "Role": role,
            "Targets": {
                "DynamoDBTargets": [
                    { "Path": "readable-items" },
                    { "Path": "closed-items" },
                ],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a second target the role may not read\": expected 400, got {status} with {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        without_request_id("a second target the role may not read", &message),
        expected_table_grant_missing("closed-items"),
        "case \"a second target the role may not read\": the refusal names that table, got \
         {message:?}"
    );

    // A crawler with no DynamoDB target has no table to decide about, so the same role —
    // which grants nothing for Amazon S3 — creates one.
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "no-table-crawler",
            "Role": role,
            "Targets": { "S3Targets": [{ "Path": "s3://crawler-bucket/plain/" }] },
        }),
    )
    .await;
}

/// The number of DynamoDB tables one create may have decided about is bounded
/// (`MAX_CHECKED_DYNAMODB_TARGETS`): a create naming that many DISTINCT tables is served, one
/// naming more is refused with the operation's modelled `InvalidInputException`.
#[tokio::test]
async fn the_number_of_checked_dynamodb_tables_is_bounded() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    // A role granted the crawl's reads on every table of this account, so a refusal below can
    // only be the ceiling and never a missing grant.
    let role = seed_role(
        &engine,
        "every-table-role",
        &[allow(&["dynamodb:*"], &format!("{}*", table_arn("")))],
    );

    let ceiling = service_com_amazonaws_glue_awsglue::state::MAX_CHECKED_DYNAMODB_TARGETS;
    let targets = |count: usize| {
        let tables: Vec<Value> = (0..count)
            .map(|index| json!({ "Path": format!("bounded-items-{index}") }))
            .collect();
        json!({ "DynamoDBTargets": tables })
    };

    // At the ceiling: served.
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "at-the-table-ceiling",
            "Role": role,
            "Targets": targets(ceiling),
        }),
    )
    .await;

    // One past it: refused, and nothing stored.
    let (status, body) = call(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "past-the-table-ceiling",
            "Role": role,
            "Targets": targets(ceiling + 1),
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"one table past the ceiling\": expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case \"one table past the ceiling\": expected InvalidInputException, got {}",
        error_code(&body)
    );
    let (status, body) = call(
        &router,
        "GetCrawler",
        &json!({ "Name": "past-the-table-ceiling" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"one table past the ceiling\": a refused create stores nothing, got {status} \
         with {body}"
    );
}
