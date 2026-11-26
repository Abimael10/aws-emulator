// SPDX-License-Identifier: CC0-1.0

//! A target grant SCOPED to the invoking rule, where that rule lives on a CUSTOM event bus —
//! end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter` with Amazon
//! SQS beside EventBridge on one engine, so each row is read where a consumer reads it: on the
//! queue the rule targets.
//!
//! The RULE asserted here: EventBridge invokes an Amazon SQS target as the service principal
//! `events.amazonaws.com` ON BEHALF OF the rule being invoked, so a statement in the queue's
//! own `Policy` conditioned on `aws:SourceArn` admits the deliveries of the rule whose ARN it
//! names and no others — and a rule's ARN is qualified by its event bus
//! (`arn:aws:events:<region>:<account>:rule/<bus>/<name>` for a rule on a bus other than
//! `default`), so two same-named rules on two buses are two different principals-on-behalf-of
//! as far as that grant is concerned.
//!
//! That the queue's document decides the delivery at all, and the operators a condition may be
//! written under, is `tests/all/queue_target_policy_authorization.rs`'s rule, asserted there
//! over a rule of the `default` bus — whose ARN carries NO bus segment. This suite is the
//! custom-bus half: the rows differ along WHICH rule ARN the grant names relative to the one
//! invoking — the invoking rule itself, a wildcard covering every rule of that bus, another
//! rule of the same bus, and a same-named rule of another bus — so a grant matched on the bare
//! rule name, or on an ARN built without the bus segment, fails here whichever way it leant.
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

/// The bus the invoking rule lives on, the OTHER bus a row scopes its grant to, and the rule
/// name both buses carry.
const BUS: &str = "scoped-grant-bus";
const OTHER_BUS: &str = "scoped-grant-other-bus";
const RULE: &str = "scoped-grant-rule";

/// How long a row watches the queue to conclude that a delivery the grant does not cover was
/// not made. A bound on an ABSENCE, so it is the one span a slow host may spend without
/// changing the verdict: the admitted rows prove the same wiring delivers in a fraction of it.
const WITHHELD_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// How long an admitted row waits for its delivery. Generous, because what it waits for is a
/// CONDITION rather than a span.
const DELIVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the poll loops pause between looks at the queue.
const POLL_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

/// Which rule the row's grant is scoped to, relative to the rule that invokes the target.
#[derive(Clone, Copy, Debug)]
enum ScopedTo {
    /// The invoking rule's own ARN.
    TheInvokingRule,
    /// Every rule of the invoking rule's bus, through a trailing wildcard.
    EveryRuleOfItsBus,
    /// Another rule of the same bus.
    AnotherRuleOfItsBus,
    /// A rule of the same NAME on another bus.
    TheSameNameOnAnotherBus,
}

/// One row: the case, which rule the grant names, and whether the delivery is admitted.
struct Row {
    case: &'static str,
    scoped_to: ScopedTo,
    admitted: bool,
}

/// The one entry every row publishes.
fn entry() -> Value {
    json!({
        "Source": SOURCE,
        "DetailType": DETAIL_TYPE,
        "Detail": json!({ "marker": "scoped" }).to_string(),
        "EventBusName": BUS,
    })
}

/// A bus called `name` and a rule on it matching the published source; answers the rule's ARN
/// as this service minted it.
async fn given_rule_on(emulator: &Emulator, case: &str, bus: &str) -> String {
    let (status, body) = events(emulator, "CreateEventBus", &json!({ "Name": bus })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventBus answered {status} {body}"
    );
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": bus,
            "EventPattern": json!({ "source": [SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: PutRule answered {status} {body}");
    body["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no RuleArn in {body}"))
        .to_string()
}

/// The queue policy granting this service `sqs:SendMessage` on `queue_arn`, scoped by
/// `aws:SourceArn` to `named` under `operator`.
fn document(queue_arn: &str, operator: &str, named: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowOneRule",
            "Effect": "Allow",
            "Principal": { "Service": SERVICE_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { operator: { "aws:SourceArn": named } },
        }],
    })
    .to_string()
}

/// A grant conditioned on `aws:SourceArn` admits the deliveries of the rule whose
/// bus-qualified ARN it names, and no others.
#[tokio::test]
async fn a_grant_scoped_to_a_rule_admits_that_rules_deliveries_alone() {
    let rows = [
        Row {
            case: "scoped to the invoking rule's own ARN",
            scoped_to: ScopedTo::TheInvokingRule,
            admitted: true,
        },
        Row {
            case: "scoped to every rule of the invoking rule's bus",
            scoped_to: ScopedTo::EveryRuleOfItsBus,
            admitted: true,
        },
        Row {
            case: "scoped to another rule of the same bus",
            scoped_to: ScopedTo::AnotherRuleOfItsBus,
            admitted: false,
        },
        Row {
            case: "scoped to a rule of the same name on another bus",
            scoped_to: ScopedTo::TheSameNameOnAnotherBus,
            admitted: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let rule_arn = given_rule_on(&emulator, case, BUS).await;
        // The ARN the row's grant names, and the operator it is written under. Each is built
        // from an ARN this service itself minted for a rule it holds, never from text this
        // suite assembles.
        let (operator, named) = match row.scoped_to {
            ScopedTo::TheInvokingRule => ("ArnEquals", rule_arn.clone()),
            ScopedTo::EveryRuleOfItsBus => (
                "ArnLike",
                format!(
                    "{}*",
                    rule_arn
                        .strip_suffix(RULE)
                        .unwrap_or_else(|| panic!("case {case:?}: {rule_arn} ends with the rule"))
                ),
            ),
            ScopedTo::AnotherRuleOfItsBus => {
                ("ArnEquals", format!("{rule_arn}-sibling"))
            }
            ScopedTo::TheSameNameOnAnotherBus => {
                ("ArnEquals", given_rule_on(&emulator, case, OTHER_BUS).await)
            }
        };
        assert_ne!(
            named, "",
            "case {case:?}: the grant must name a rule ARN"
        );

        let policy = move |queue_arn: &str| document(queue_arn, operator, &named);
        let (url, arn) =
            given_queue_with_policy(&emulator, &format!("scoped-grant-{index}"), Some(&policy))
                .await;
        let (status, body) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "EventBusName": BUS,
                "Targets": [{ "Id": "queue", "Arn": arn }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {body}"
        );
        let (status, body) = events(&emulator, "PutEvents", &json!({ "Entries": [entry()] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutEvents answered {status} {body}"
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {case:?}: the entry was accepted, got {body}"
        );

        if row.admitted {
            let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
            let mut messages: Vec<Value> = Vec::new();
            while messages.is_empty() && std::time::Instant::now() < deadline {
                messages.extend(drain(&emulator, &url).await);
                tokio::time::sleep(POLL_PAUSE).await;
            }
            assert_eq!(
                messages.len(),
                1,
                "case {case:?}: a grant covering the invoking rule admits its delivery, got \
                 {messages:?}"
            );
            let body = messages[0]["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("case {case:?}: the body is not JSON ({error})"));
            assert_eq!(
                envelope.pointer("/detail/marker").and_then(Value::as_str),
                Some("scoped"),
                "case {case:?}: the delivered envelope carries the published detail, got {body}"
            );
        } else {
            let watching = std::time::Instant::now();
            while watching.elapsed() < WITHHELD_WINDOW {
                let messages = drain(&emulator, &url).await;
                assert!(
                    messages.is_empty(),
                    "case {case:?}: a grant naming another rule admits nothing of this one, \
                     got {messages:?}"
                );
                tokio::time::sleep(POLL_PAUSE).await;
            }
        }
    }
}
