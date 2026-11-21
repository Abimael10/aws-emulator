// SPDX-License-Identifier: CC0-1.0

//! Which of a BATCH's entries reaches a queue target, and which document admits it: the
//! rule's `EventPattern` decides which entries are handed over, and the queue's own `Policy`
//! — scoped by the invoking rule where it carries a condition — decides whether the handover
//! is made. End to end over the wire through the real `MultiServiceRouter` with Amazon SQS
//! mounted beside EventBridge on one engine.
//!
//! The RULE asserted here: every accepted entry of one `PutEvents` is matched against the
//! bus's enabled rules on its own, so a rule whose pattern names a value NESTED in `detail`
//! takes the entries whose detail carries that value and silently drops the others — no
//! message and no error for them. Each handover it does make is then decided by the TARGET's
//! own document, since a queue target names no role: "for Lambda functions, Amazon SNS topics
//! and Amazon SQS queues, EventBridge relies on resource-based policies" (`PutTargets`), and
//! a grant conditioned on `aws:SourceArn` authorizes the deliveries of the rule that ARN
//! names and no other — EventBridge publishes the INVOKING rule's ARN for that key. So one
//! request of two entries, one matching and one not, leaves exactly one message on a queue
//! whose policy admits that rule, and none at all on a queue whose policy admits another rule
//! or grants nothing.
//!
//! The rows differ along what the rule generalises over: the FORM of the grant's condition
//! (an exact ARN, a wildcard over the account's rules, another rule's ARN, no condition at
//! all) and whether the queue carries a policy at all — with the two-entry batch, and so the
//! pattern half, held across all of them.
//!
//! The harness (the router with EventBridge, SQS, Step Functions, IAM and CloudWatch on one
//! engine) is the one `target_dispatch` owns.

use serde_json::{json, Value};

use crate::target_dispatch::{drain, emulator, events, given_queue_with_policy, REGION};

/// The bus every rule below lives on is the default one, and this is the rule's name — which
/// with the bus is what its ARN is minted from.
const RULE: &str = "state-change-filter";

/// The envelope members the rule's pattern names, and the member NESTED in `detail` it is
/// scoped by.
const SOURCE: &str = "com.demo.distribution";
const DETAIL_TYPE: &str = "Distribution State Change";
const MATCHED_STATE: &str = "Deployed";
const OTHER_STATE: &str = "InProgress";

/// The markers the two entries of every row's batch carry, one per entry, so which of them
/// reached the queue is read off the message body.
const MATCHED_MARKER: &str = "E-MATCHED";
const OTHER_MARKER: &str = "E-OTHER";

/// The service principal EventBridge delivers to a queue target as, which the queue's own
/// policy has to allow the send for — the literal AWS publishes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The action such a delivery performs.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// How long the queue is looked at for the handover a row expects, and how often. Generous:
/// what is asserted is which messages arrive, never how fast the host runs.
const BUDGET: ::std::time::Duration = ::std::time::Duration::from_secs(60);
const LOOK_AGAIN: ::std::time::Duration = ::std::time::Duration::from_millis(50);

/// How long a row that expects NOTHING keeps looking before concluding the delivery was
/// withheld, and how long every row keeps looking for a SECOND message after the first. The
/// delivering rows prove the same wiring delivers in a fraction of it.
const WITHHELD_WINDOW: ::std::time::Duration = ::std::time::Duration::from_secs(3);

/// The account every ARN below belongs to.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The ARN EventBridge mints for a rule of the DEFAULT bus, which is the value it publishes
/// for `aws:SourceArn` when that rule's target is delivered to.
fn rule_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{}:rule/{name}", account())
}

/// What the row's queue policy scopes its grant by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No policy on the queue at all.
    None,
    /// A grant with no condition.
    Unscoped,
    /// A grant conditioned on the invoking rule's own ARN.
    ThisRule,
    /// A grant conditioned on a wildcard covering the account's rules.
    AnyRule,
    /// A grant conditioned on another rule's ARN.
    AnotherRule,
}

impl Grant {
    /// The document this grant writes for the queue `queue_arn`, or `None` for a queue
    /// carrying no policy.
    fn document(self, queue_arn: &str) -> Option<String> {
        let condition = match self {
            Self::None => return None,
            Self::Unscoped => None,
            Self::ThisRule => Some(json!({ "ArnEquals": { "aws:SourceArn": rule_arn(RULE) } })),
            Self::AnyRule => Some(json!({
                "ArnLike": { "aws:SourceArn": format!("arn:aws:events:{REGION}:{}:rule/*", account()) }
            })),
            Self::AnotherRule => {
                Some(json!({ "ArnEquals": { "aws:SourceArn": rule_arn("another-rule") } }))
            }
        };
        let mut statement = json!({
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": SEND_MESSAGE_ACTION,
            "Resource": queue_arn,
        });
        if let Some(condition) = condition {
            statement["Condition"] = condition;
        }
        Some(
            json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string(),
        )
    }
}

/// One row: what the queue's policy grants, and whether the matched entry is delivered.
struct Row {
    case: &'static str,
    grant: Grant,
    delivered: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a grant conditioned on the invoking rule's own ARN",
            grant: Grant::ThisRule,
            delivered: true,
        },
        Row {
            case: "a grant conditioned on a wildcard over the account's rules",
            grant: Grant::AnyRule,
            delivered: true,
        },
        Row {
            case: "a grant with no condition at all",
            grant: Grant::Unscoped,
            delivered: true,
        },
        Row {
            case: "a grant conditioned on another rule's ARN",
            grant: Grant::AnotherRule,
            delivered: false,
        },
        Row {
            case: "a queue carrying no policy",
            grant: Grant::None,
            delivered: false,
        },
    ]
}

/// The two entries every row publishes in ONE request: the one whose detail carries the state
/// the rule's pattern names, and one whose detail carries another state.
fn entries() -> Vec<Value> {
    [(MATCHED_MARKER, MATCHED_STATE), (OTHER_MARKER, OTHER_STATE)]
        .into_iter()
        .map(|(marker, state)| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "distributionId": marker, "state": state }).to_string(),
            })
        })
        .collect()
}

/// Every row: the pattern decides which entries are handed over, and the queue's own document
/// decides whether the handover is made.
#[tokio::test]
async fn a_rule_scoped_grant_admits_only_the_matching_entry() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let document = |queue_arn: &str| {
            row.grant
                .document(queue_arn)
                .expect("a row with a policy writes one")
        };
        let policy: Option<&dyn Fn(&str) -> String> = if row.grant == Grant::None {
            None
        } else {
            Some(&document)
        };
        let (url, queue_arn) =
            given_queue_with_policy(&emulator, &format!("state-change-{index}"), policy).await;

        let (status, written) = events(
            &emulator,
            "PutRule",
            &json!({
                "Name": RULE,
                "EventPattern": json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                    "detail": { "state": [MATCHED_STATE] },
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRule answered {status} {written}"
        );
        // The ARN the grant is scoped to is the one this service mints for the rule, so a row
        // whose condition names it is decided against the rule that is really invoking.
        assert_eq!(
            written["RuleArn"].as_str(),
            Some(rule_arn(RULE).as_str()),
            "case {case:?}: expected the rule's own ARN in {written}"
        );
        let (status, targeted) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "Targets": [{ "Id": "queue-target", "Arn": &queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {targeted}"
        );
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the queue target to be accepted, got {targeted}"
        );

        let (status, published) = events(
            &emulator,
            "PutEvents",
            &json!({ "Entries": entries() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutEvents answered {status} {published}"
        );
        assert_eq!(
            published["FailedEntryCount"],
            json!(0),
            "case {case:?}: both entries are accepted whatever the delivery comes to, got \
             {published}"
        );

        // What the queue holds, looked for rather than waited out: every row keeps looking
        // past the first message, so an entry the pattern should have dropped would be seen.
        let deadline = ::std::time::Instant::now() + if row.delivered { BUDGET } else { WITHHELD_WINDOW };
        let mut bodies: Vec<String> = Vec::new();
        let mut settled: Option<::std::time::Instant> = None;
        loop {
            for message in drain(&emulator, &url).await {
                bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
            }
            if row.delivered && !bodies.is_empty() && settled.is_none() {
                settled = Some(::std::time::Instant::now() + WITHHELD_WINDOW);
            }
            let now = ::std::time::Instant::now();
            if now >= deadline || settled.is_some_and(|until| now >= until) {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }

        let expected: Vec<&str> = if row.delivered {
            vec![MATCHED_MARKER]
        } else {
            Vec::new()
        };
        let carried: Vec<&str> = [MATCHED_MARKER, OTHER_MARKER]
            .into_iter()
            .filter(|marker| bodies.iter().any(|body| body.contains(marker)))
            .collect();
        assert_eq!(
            carried, expected,
            "case {case:?}: expected the queue to carry {expected:?}, got {bodies:?}"
        );
    }
}
