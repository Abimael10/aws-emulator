// SPDX-License-Identifier: CC0-1.0

//! WHEN a rule is evaluated at all, and what a handover its target refuses leaves behind —
//! end to end over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter` with Amazon SQS beside EventBridge on one engine, so
//! each row is read where a client reads it: on the target queue, and on the queue the target's
//! `DeadLetterConfig` names.
//!
//! The RULE asserted here: a rule is evaluated only while its `State` is not `DISABLED`
//! (`RuleState` is `ENABLED | DISABLED | ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS`,
//! `smithy_query(query="shape", shape="RuleState")`). An event published while a rule is
//! `DISABLED` is matched by nothing — not delivered, not dead-lettered, and not evaluated
//! LATE once the rule is enabled again — while from the first event published after
//! `EnableRule` the rule is evaluated as before: every matching event reaches each of its
//! targets, and a handover the target's own documents refuse is parked on the queue
//! `Targets[].DeadLetterConfig.Arn` names ("the ARN of the SQS queue specified as the target
//! for the dead-letter queue") carrying the rule, the target and the refusal, once per
//! refused handover.
//!
//! The rows span the class rather than one recorded case: they differ along the state the
//! rule is switched THROUGH (the two states that are evaluated, and the one that is not),
//! along HOW MANY events are published inside the silent window (so a rule that replayed
//! them on enable fails), along what the target does with the handover (a queue whose policy
//! grants the send takes it; one granting nothing refuses it and the event is dead-lettered),
//! and along the SPELLING of the grant the dead-letter queue carries — unconditioned, and
//! scoped by `aws:SourceArn` to the invoking rule, which is the form AWS's own dead-letter
//! guidance writes.
//!
//! What a dead letter's `ERROR_CODE`/`ERROR_MESSAGE` must be for one refusal class is
//! `tests/all/queue_policy_refusal_is_the_permission_class.rs`'s and
//! `tests/all/queue_target_failure_class_follows_its_answer.rs`' rule and is not re-asserted
//! here: this suite asserts that the attributes are PRESENT and name the failed delivery's
//! rule and target, which is what a consumer of the queue triages on.
//!
//! The harness (the router with EventBridge, Amazon SQS and the services a target lives in on
//! one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::target_dispatch::{
    drain, emulator, events, given_queue_with_policy, Emulator, DETAIL_TYPE, SOURCE,
};

/// The service principal EventBridge invokes a resource-policy target as, written out here
/// rather than imported so a wrong constant in the implementation cannot make a row pass.
const SERVICE_PRINCIPAL: &str = "events.amazonaws.com";

/// The custom bus and the rule every row below publishes through.
const BUS: &str = "rule-state-bus";
const RULE: &str = "rule-state-rule";

/// The dead-letter message attributes a consumer of the queue triages on: "EventBridge adds
/// the following custom attributes to the dead-letter message".
const RULE_ARN_ATTRIBUTE: &str = "RULE_ARN";
const TARGET_ARN_ATTRIBUTE: &str = "TARGET_ARN";
const ERROR_CODE_ATTRIBUTE: &str = "ERROR_CODE";
const ERROR_MESSAGE_ATTRIBUTE: &str = "ERROR_MESSAGE";

/// How long a row watches a queue to conclude that nothing was published to it. A bound on an
/// ABSENCE, so it is the one span a slow host may spend without changing the verdict: the
/// enabled half of every row proves the same wiring delivers in a fraction of it.
const SILENT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// How long a row waits for a delivery or a dead letter it expects. Generous, because what it
/// waits for is a CONDITION rather than a span: a slow host delays the row instead of breaking
/// it.
const DELIVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the poll loops pause between looks at a queue.
const POLL_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

/// What the target of the row's rule does with a handover.
#[derive(Clone, Copy, Debug)]
enum Target {
    /// A queue whose own `Policy` grants this service the send: the event is delivered.
    Granting,
    /// A queue whose own `Policy` grants nothing: the send is refused, so the event is
    /// dead-lettered instead.
    GrantingNothing,
}

/// How the dead-letter queue's own grant is written.
#[derive(Clone, Copy, Debug)]
enum Grant {
    /// `sqs:SendMessage` for the service principal on that queue, unconditionally.
    Unconditional,
    /// The same grant, scoped by `aws:SourceArn` to the ARN of the invoking rule.
    ScopedToTheRule,
}

/// The document a row's queue carries, built from that queue's own ARN.
type Policy = Box<dyn Fn(&str) -> String>;

/// One row: the case, the state the rule is switched to for its silent window, how many events
/// are published inside that window, what the target does with a handover, and how the
/// dead-letter queue's grant is written.
struct Row {
    case: &'static str,
    silent_state: Option<&'static str>,
    published_while_silent: usize,
    target: Target,
    grant: Grant,
}

/// The `detail` of the event with `marker`, which is how a row tells its events apart.
fn entry(marker: &str) -> Value {
    json!({
        "Source": SOURCE,
        "DetailType": DETAIL_TYPE,
        "Detail": json!({ "marker": marker }).to_string(),
        "EventBusName": BUS,
    })
}

/// The marker of each message's envelope, in ascending order.
fn markers(messages: &[Value]) -> Vec<String> {
    let mut found: Vec<String> = messages
        .iter()
        .filter_map(|message| {
            let body = message["Body"].as_str()?;
            let envelope: Value = serde_json::from_str(body).ok()?;
            Some(envelope.pointer("/detail/marker")?.as_str()?.to_string())
        })
        .collect();
    found.sort();
    found
}

/// The string value of `message`'s message attribute `name`, or the empty string.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Everything on the queue at `url`, looked at until `wanted` messages have been collected or
/// a generous deadline passes: the wait is on the CONDITION, so a slow host delays the row
/// instead of breaking it.
async fn collected(emulator: &Emulator, url: &str, wanted: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
    let mut messages: Vec<Value> = Vec::new();
    loop {
        messages.extend(drain(emulator, url).await);
        if messages.len() >= wanted || std::time::Instant::now() >= deadline {
            return messages;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Watch the queue at `url` for [`SILENT_WINDOW`] and answer everything that arrived on it.
async fn watched(emulator: &Emulator, url: &str) -> Vec<Value> {
    let watching = std::time::Instant::now();
    let mut messages: Vec<Value> = Vec::new();
    while watching.elapsed() < SILENT_WINDOW {
        messages.extend(drain(emulator, url).await);
        tokio::time::sleep(POLL_PAUSE).await;
    }
    messages
}

/// The queue policy granting this service `sqs:SendMessage` on `queue_arn`, scoped to
/// `rule_arn` when the row's grant is.
fn document(queue_arn: &str, rule_arn: Option<&str>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": SERVICE_PRINCIPAL },
        "Action": "sqs:SendMessage",
        "Resource": queue_arn,
    });
    if let Some(rule_arn) = rule_arn {
        statement["Condition"] = json!({ "ArnEquals": { "aws:SourceArn": rule_arn } });
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// The bus and the rule, in `state`; answers the rule's ARN as this service minted it.
async fn given_rule(emulator: &Emulator, case: &str, state: &str) -> String {
    let (status, body) = events(emulator, "CreateEventBus", &json!({ "Name": BUS })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventBus answered {status} {body}"
    );
    put_rule(emulator, case, state).await
}

/// The row's rule, written in `state` — create or update, which is what `PutRule` is; answers
/// the ARN it reports.
async fn put_rule(emulator: &Emulator, case: &str, state: &str) -> String {
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": json!({ "source": [SOURCE] }).to_string(),
            "State": state,
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: PutRule answered {status} {body}");
    body["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no RuleArn in {body}"))
        .to_string()
}

/// `DescribeRule`'s `State` for the row's rule.
async fn described_state(emulator: &Emulator, case: &str) -> String {
    let (status, body) = events(
        emulator,
        "DescribeRule",
        &json!({ "Name": RULE, "EventBusName": BUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeRule answered {status} {body}"
    );
    body["State"].as_str().unwrap_or_default().to_string()
}

/// Publish one matching entry carrying `marker`, and answer the `EventId` it was ingested
/// under.
async fn publish(emulator: &Emulator, case: &str, marker: &str) -> String {
    let (status, body) = events(emulator, "PutEvents", &json!({ "Entries": [entry(marker)] })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutEvents answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: the entry was accepted, got {body}"
    );
    body["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no EventId in {body}"))
        .to_string()
}

/// Switch the row's rule with `operation` and answer nothing; the state it left is read back
/// through `DescribeRule` by the caller.
async fn switch(emulator: &Emulator, case: &str, operation: &str) {
    let (status, body) = events(
        emulator,
        operation,
        &json!({ "Name": RULE, "EventBusName": BUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} answered {status} {body}"
    );
}

/// A rule is evaluated only while its `State` is not `DISABLED`: a matching event published
/// inside a disabled window reaches neither the target nor the dead-letter queue, then or
/// after the rule is enabled again, and every matching event published after `EnableRule`
/// reaches the target — or its dead-letter queue, when the target refuses the handover.
#[tokio::test]
async fn a_rule_is_evaluated_only_while_it_is_not_disabled() {
    let rows = [
        Row {
            case: "disabled for one event, then enabled, with a refusing target and a \
                   rule-scoped dead-letter grant",
            silent_state: Some("DISABLED"),
            published_while_silent: 1,
            target: Target::GrantingNothing,
            grant: Grant::ScopedToTheRule,
        },
        Row {
            case: "disabled for three events, then enabled, with a refusing target",
            silent_state: Some("DISABLED"),
            published_while_silent: 3,
            target: Target::GrantingNothing,
            grant: Grant::Unconditional,
        },
        Row {
            case: "disabled for two events, then enabled, with a target that takes the send",
            silent_state: Some("DISABLED"),
            published_while_silent: 2,
            target: Target::Granting,
            grant: Grant::Unconditional,
        },
        Row {
            case: "never disabled: an enabled rule is evaluated throughout",
            silent_state: None,
            published_while_silent: 0,
            target: Target::GrantingNothing,
            grant: Grant::ScopedToTheRule,
        },
        Row {
            case: "switched to the CloudTrail-management state, which is not a disabled rule",
            silent_state: Some("ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS"),
            published_while_silent: 0,
            target: Target::Granting,
            grant: Grant::Unconditional,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let rule_arn = given_rule(&emulator, case, "ENABLED").await;

        // The target: a queue that takes the send, or one that has granted nothing.
        let target_policy: Option<Policy> = match row.target {
            Target::Granting => Some(Box::new(|queue_arn: &str| document(queue_arn, None))),
            Target::GrantingNothing => None,
        };
        let (target_url, target_arn) = given_queue_with_policy(
            &emulator,
            &format!("rule-state-target-{index}"),
            target_policy.as_deref(),
        )
        .await;

        // The dead-letter queue, carrying the row's spelling of the grant.
        let scoped = match row.grant {
            Grant::Unconditional => None,
            Grant::ScopedToTheRule => Some(rule_arn.clone()),
        };
        let dead_letter_policy = move |queue_arn: &str| document(queue_arn, scoped.as_deref());
        let (dead_letter_url, dead_letter_arn) = given_queue_with_policy(
            &emulator,
            &format!("rule-state-dlq-{index}"),
            Some(&dead_letter_policy),
        )
        .await;

        let (status, body) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "EventBusName": BUS,
                "Targets": [{
                    "Id": "only",
                    "Arn": &target_arn,
                    "RetryPolicy": {
                        "MaximumRetryAttempts": 185,
                        "MaximumEventAgeInSeconds": 86_400,
                    },
                    "DeadLetterConfig": { "Arn": &dead_letter_arn },
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {body}"
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets reported a failed entry: {body}"
        );

        // The silent window: the state the row switches to, and the events published inside
        // it. A state that is NOT `DISABLED` is evaluated, so those events are delivered —
        // which is why only the disabled rows publish anything here.
        let silent_markers: Vec<String> = (0..row.published_while_silent)
            .map(|at| format!("silent-{at}"))
            .collect();
        if let Some(state) = row.silent_state {
            let operation = if state == "DISABLED" {
                "DisableRule"
            } else {
                // Every other state is written through `PutRule`, which is what sets a state
                // `EnableRule`/`DisableRule` do not name.
                "PutRule"
            };
            if operation == "DisableRule" {
                switch(&emulator, case, operation).await;
            } else {
                put_rule(&emulator, case, state).await;
            }
            let described = described_state(&emulator, case).await;
            assert_eq!(
                described, state,
                "case {case:?}: DescribeRule must report the state the switch left, got \
                 {described:?}"
            );
            for marker in &silent_markers {
                publish(&emulator, case, marker).await;
            }
            if state == "DISABLED" {
                // A disabled rule is not evaluated at all: neither the target nor the
                // dead-letter queue sees anything, however long they are watched.
                let delivered = watched(&emulator, &target_url).await;
                assert!(
                    delivered.is_empty(),
                    "case {case:?}: a disabled rule delivers nothing, got {:?}",
                    markers(&delivered)
                );
                let parked = watched(&emulator, &dead_letter_url).await;
                assert!(
                    parked.is_empty(),
                    "case {case:?}: a disabled rule dead-letters nothing, got {:?}",
                    markers(&parked)
                );
            }
        }

        // Enabled again — which is the state every row publishes its own events under.
        switch(&emulator, case, "EnableRule").await;
        let described = described_state(&emulator, case).await;
        assert_eq!(
            described, "ENABLED",
            "case {case:?}: DescribeRule must report ENABLED after EnableRule, got \
             {described:?}"
        );

        // The events published after the rule is enabled again, each with its own marker.
        let live_markers: Vec<String> = (0..2usize).map(|at| format!("live-{at}")).collect();
        let mut event_ids: Vec<String> = Vec::new();
        for marker in &live_markers {
            event_ids.push(publish(&emulator, case, marker).await);
        }

        match row.target {
            Target::Granting => {
                // A target that takes the send receives exactly the events published while
                // the rule was evaluated — and nothing from the silent window, which is what
                // a rule that replayed its disabled period would have added.
                let delivered = collected(&emulator, &target_url, live_markers.len()).await;
                assert_eq!(
                    markers(&delivered),
                    live_markers,
                    "case {case:?}: expected exactly the events published while the rule was \
                     evaluated on the target, got {:?}",
                    markers(&delivered)
                );
                let parked = watched(&emulator, &dead_letter_url).await;
                assert!(
                    parked.is_empty(),
                    "case {case:?}: a delivery the target took is not dead-lettered, got {:?}",
                    markers(&parked)
                );
            }
            Target::GrantingNothing => {
                // A handover the target refuses is parked on the dead-letter queue instead,
                // once per refused handover, and the target itself stays empty.
                let parked = collected(&emulator, &dead_letter_url, live_markers.len()).await;
                assert_eq!(
                    markers(&parked),
                    live_markers,
                    "case {case:?}: expected one dead letter per refused handover, got {:?}",
                    markers(&parked)
                );
                let delivered = watched(&emulator, &target_url).await;
                assert!(
                    delivered.is_empty(),
                    "case {case:?}: a refused handover enqueues nothing on the target, got \
                     {:?}",
                    markers(&delivered)
                );
                // Each dead letter names the delivery that failed — the rule, the target —
                // and carries the condition and the sentence the refusal was reported under,
                // which is what a consumer of the queue triages on.
                for message in &parked {
                    let named = attribute(message, RULE_ARN_ATTRIBUTE);
                    assert_eq!(
                        named, rule_arn,
                        "case {case:?}: expected {RULE_ARN_ATTRIBUTE} to name the invoking \
                         rule, got {named:?}"
                    );
                    let named = attribute(message, TARGET_ARN_ATTRIBUTE);
                    assert_eq!(
                        named, target_arn,
                        "case {case:?}: expected {TARGET_ARN_ATTRIBUTE} to name the refused \
                         target, got {named:?}"
                    );
                    for name in [ERROR_CODE_ATTRIBUTE, ERROR_MESSAGE_ATTRIBUTE] {
                        let stated = attribute(message, name);
                        assert!(
                            !stated.is_empty(),
                            "case {case:?}: a dead letter states why the delivery failed, so \
                             {name} must carry a value, got {message:?}"
                        );
                    }
                }
            }
        }

        // Nothing of the silent window arrives late on either queue: a disabled rule is not
        // evaluated at all, so its events are gone rather than deferred.
        for (url, which) in [(&target_url, "target"), (&dead_letter_url, "dead-letter queue")] {
            let late = watched(&emulator, url).await;
            assert!(
                late.is_empty(),
                "case {case:?}: nothing more reaches the {which} after the events published \
                 while the rule was evaluated, got {:?}",
                markers(&late)
            );
        }
        assert_eq!(
            event_ids.len(),
            live_markers.len(),
            "case {case:?}: one EventId per published entry, got {event_ids:?}"
        );
    }
}
