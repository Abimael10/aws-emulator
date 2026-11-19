// SPDX-License-Identifier: CC0-1.0

//! What ONE matched event comes to when it fans out over SEVERAL targets of the same deployed API
//! Gateway REST API method whose roles are scoped differently: every target is decided by its own
//! role, and the one whose role may not invoke the method leaves a dead letter rather than
//! nothing at all.
//!
//! RULE: EventBridge invokes an `execute-api` target AS the role that target names — "for EC2
//! instances, Kinesis data streams, Step Functions state machines and API Gateway APIs,
//! EventBridge relies on IAM roles that you specify in the `RoleARN` argument in `PutTargets`" —
//! so the identity-based documents in force on THAT role, and on no other target's, decide THAT
//! delivery: a role allowed `execute-api:Invoke` on the target's own ARN has its request served
//! by the stage, and an identity that is not is answered `403 Forbidden`, which, under a
//! `RetryPolicy` that allows no retry, writes the event ONCE to that target's own
//! `DeadLetterConfig` queue carrying `ERROR_CODE` `NO_PERMISSIONS`, the sentence EventBridge words
//! a status an invoke answered with, the invoking rule's ARN and the target's ARN — while nothing
//! of that delivery reaches what the method integrates with. Each target of a matched event is
//! decided on its own, so the outcome of one says nothing about the outcome of the next, whether
//! they hang on one rule or on rules of one pattern and in whichever order they were wired.
//!
//! Only a cross-service suite can pin this: the bus, its rules, their targets and the dead letters
//! are EventBridge's, the stage and its integration are API Gateway's, the roles whose documents
//! decide each delivery are IAM's, and the sink the method writes into — which is what shows
//! whether anything reached the stage at all — is Amazon SQS's. The setup calls and the wording of
//! a refused invoke are the ones [`crate::events_execute_api_target_role_grant`] already makes and
//! spells, imported rather than written again; what is this suite's own is the FAN-OUT below.
//!
//! The rows vary the shape the fan-out takes, which is what the rule generalises over: how many
//! rules of one pattern carry the targets, how many targets one rule carries, WHERE the permitted
//! target sits among the refused ones (first, last, between), and how each refused role's document
//! withholds the invoke. Every leg names a dead-letter queue of its own, so what a leg produced is
//! read off that leg's queue and not inferred from a shared one.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::events_execute_api_target_role_grant::{
    allow_and_deny, assert_forbidden_sentence, deployed, drained, Deployed, INVOKE_ACTION,
    MAX_EVENT_AGE_SECONDS, RETRIES_ALLOWED,
};
use crate::events_rest_api_stage_target::{
    attribute, emulator, events, messages, policy, queue, role, sqs, DETAIL_TYPE, EVENTS_PRINCIPAL,
    EVENTS_TRUST, NO_PERMISSIONS, REGION, SOURCE_HEADER, STAGE,
};

/// The `source` the one event of a row carries, and what that row's own rules match on: each row
/// publishes under a source of its own, so a row's publication reaches that row's targets and no
/// other row's.
fn source_of(index: usize) -> String {
    format!("sim2real.orders.fanout.{index}")
}

/// Another action of the same service, which is a document that grants the role everything except
/// the one action the delivery performs.
const OTHER_ACTION: &str = "execute-api:InvalidateCache";

/// An api id this emulator holds no api for, which an invoke grant naming it allows no invoke of
/// the api the targets below address.
const OTHER_API: &str = "zzz9999999";

/// How the one identity-based document in force on a leg's role is scoped, which is what decides
/// that leg's delivery.
#[derive(Clone, Copy, Debug)]
enum Grant {
    /// The invoke on the target's own ARN: the grant the delivery needs.
    Invoke,
    /// Another action of the same service on the target's own ARN.
    OtherAction,
    /// No identity-based document at all.
    Nothing,
    /// The invoke allowed on the target's own ARN and explicitly denied there as well.
    AllowAndDeny,
    /// The invoke on another api's methods, so no invoke of this one.
    OtherApi,
}

impl Grant {
    /// The document written as the role's one inline policy, built from the target's own ARN —
    /// `None` for a role carrying no identity-based document at all.
    fn document(self, stage: &Deployed) -> Option<String> {
        match self {
            Self::Invoke => Some(policy(INVOKE_ACTION, &stage.target_arn())),
            Self::OtherAction => Some(policy(OTHER_ACTION, &stage.target_arn())),
            Self::Nothing => None,
            Self::AllowAndDeny => Some(allow_and_deny(INVOKE_ACTION, &stage.target_arn())),
            Self::OtherApi => Some(policy(
                INVOKE_ACTION,
                &format!(
                    "arn:aws:execute-api:{REGION}:{}:{OTHER_API}/{STAGE}/POST/orders/*",
                    stage.account
                ),
            )),
        }
    }

    /// Whether the stage serves a delivery made as a role carrying this document, which is what
    /// the sink shows: only the grant of the action the delivery performs, on the ARN it performs
    /// it on, is one the invoke endpoint admits.
    fn served(self) -> bool {
        matches!(self, Self::Invoke)
    }
}

/// One target of a row's fan-out: which of the row's rules carries it, what it contributes as
/// [`SOURCE_HEADER`] (which is how the sink says WHICH leg reached the stage), and how its role is
/// scoped.
struct Leg {
    rule: usize,
    header: &'static str,
    grant: Grant,
}

/// One row of the fan-out table: how many rules of the row's one pattern carry its legs, and the
/// legs themselves in the order they are wired.
struct Row {
    case: &'static str,
    rules: usize,
    legs: &'static [Leg],
}

/// One leg as it stands once wired: what it contributes, whether the stage serves it, the ARN of
/// the rule that invokes it and the queue it dead-letters to.
struct Wired {
    header: &'static str,
    served: bool,
    rule_arn: String,
    dead_letter_url: String,
}

/// One queue of this account that admits the dead-letter write EventBridge makes as its own
/// service principal ("for … Amazon SQS queues, EventBridge relies on resource-based policies"),
/// so what reaches it is decided by the delivery and not by this queue.
async fn dead_letter(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (url, arn) = queue(router, case, name).await;
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": {
                "Policy": json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": arn,
                    }],
                })
                .to_string(),
            },
        }),
    )
    .await;
    (url, arn)
}

/// One rule of the custom bus matching `source` and the detail type every row publishes, answering
/// its own ARN.
async fn rule_of(
    router: &MultiServiceRouter,
    case: &str,
    stage: &Deployed,
    name: &str,
    source: &str,
) -> String {
    let created = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": stage.bus,
            "EventPattern": json!({
                "source": [source],
                "detail-type": [DETAIL_TYPE],
            })
            .to_string(),
        }),
    )
    .await;
    created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {case:?}: no RuleArn in {created}"))
        .to_string()
}

/// Add one leg's target to `rule`: the same method of the same stage every leg of a row addresses,
/// under this leg's own role, contributing this leg's own header and dead-lettering to this leg's
/// own queue.
///
/// The leg's header is what it is identified by here as well, since a target's id has to be its
/// rule's own (`PutTargets` refuses a duplicate) and every leg of a row contributes a header no
/// other leg does.
async fn target_of(
    router: &MultiServiceRouter,
    case: &str,
    stage: &Deployed,
    rule: &str,
    role_arn: &str,
    header: &str,
    dead_letter_arn: &str,
) {
    let answer = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": rule,
            "EventBusName": stage.bus,
            "Targets": [{
                "Id": header,
                "Arn": stage.target_arn(),
                "RoleArn": role_arn,
                "HttpParameters": {
                    // The one `*` of the target's ARN is filled by what the event carries at this
                    // path, so the request each leg makes names the event's own resource.
                    "PathParameterValues": ["$.detail.id"],
                    "HeaderParameters": { SOURCE_HEADER: header },
                },
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": {
                    "MaximumRetryAttempts": RETRIES_ALLOWED,
                    "MaximumEventAgeInSeconds": MAX_EVENT_AGE_SECONDS,
                },
            }],
        }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "setup {case:?}: expected the target to be accepted, got {answer}"
    );
}

/// Publish the row's one matching entry on the custom bus, which has to be accepted.
async fn published(router: &MultiServiceRouter, case: &str, stage: &Deployed, source: &str, id: &str) {
    let answer = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": stage.bus,
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "id": id, "src": "runner" }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {answer}"
    );
}

/// The document a message carries as its body, which for a message at the sink is the one the
/// method's integration template wrote out of the request the stage served.
fn body_of(case: &str, message: &Value) -> Value {
    message["Body"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a document as the body, got {message}"))
}

#[tokio::test]
async fn every_execute_api_target_of_one_matched_event_is_decided_by_its_own_role() {
    let router = emulator();
    let stage = deployed(&router, "exec-api-fanout").await;
    let rows: &[Row] = &[
        Row {
            case: "two rules of one pattern, the permitted target wired first",
            rules: 2,
            legs: &[
                Leg {
                    rule: 0,
                    header: "rule-good",
                    grant: Grant::Invoke,
                },
                Leg {
                    rule: 1,
                    header: "rule-denied",
                    grant: Grant::OtherAction,
                },
            ],
        },
        Row {
            case: "two rules of one pattern, the refused target wired first",
            rules: 2,
            legs: &[
                Leg {
                    rule: 0,
                    header: "rule-denied-first",
                    grant: Grant::OtherApi,
                },
                Leg {
                    rule: 1,
                    header: "rule-good-second",
                    grant: Grant::Invoke,
                },
            ],
        },
        Row {
            case: "one rule carrying both targets of the same method",
            rules: 1,
            legs: &[
                Leg {
                    rule: 0,
                    header: "one-rule-good",
                    grant: Grant::Invoke,
                },
                Leg {
                    rule: 0,
                    header: "one-rule-denied",
                    grant: Grant::Nothing,
                },
            ],
        },
        Row {
            case: "one rule whose permitted target sits between two refused differently",
            rules: 1,
            legs: &[
                Leg {
                    rule: 0,
                    header: "trio-denied-by-deny",
                    grant: Grant::AllowAndDeny,
                },
                Leg {
                    rule: 0,
                    header: "trio-good",
                    grant: Grant::Invoke,
                },
                Leg {
                    rule: 0,
                    header: "trio-denied-by-other-action",
                    grant: Grant::OtherAction,
                },
            ],
        },
    ];

    for (row_index, row) in rows.iter().enumerate() {
        let source = source_of(row_index);
        let mut rule_arns: Vec<(String, String)> = Vec::new();
        for index in 0..row.rules {
            let name = format!("exec-api-fanout-{row_index}-{index}");
            let arn = rule_of(&router, row.case, &stage, &name, &source).await;
            rule_arns.push((name, arn));
        }

        let mut wired: Vec<Wired> = Vec::new();
        for (leg_index, leg) in row.legs.iter().enumerate() {
            let document = leg.grant.document(&stage);
            let role_arn = role(
                &router,
                &format!("exec-api-fanout-role-{row_index}-{leg_index}"),
                EVENTS_TRUST,
                document.as_deref(),
            )
            .await;
            let (dead_letter_url, dead_letter_arn) = dead_letter(
                &router,
                row.case,
                &format!("exec-api-fanout-dlq-{row_index}-{leg_index}"),
            )
            .await;
            let (rule_name, rule_arn) = &rule_arns[leg.rule];
            target_of(
                &router,
                row.case,
                &stage,
                rule_name,
                &role_arn,
                leg.header,
                &dead_letter_arn,
            )
            .await;
            wired.push(Wired {
                header: leg.header,
                served: leg.grant.served(),
                rule_arn: rule_arn.clone(),
                dead_letter_url,
            });
        }

        let id = format!("ord-{row_index}");
        published(&router, row.case, &stage, &source, &id).await;

        // What the stage served: one request per permitted leg, each carrying the path the event
        // names and the header its own target contributes.
        let served: Vec<&Wired> = wired.iter().filter(|leg| leg.served).collect();
        let held = messages(&router, row.case, &stage.sink_url, served.len()).await;
        assert_eq!(
            held.len(),
            served.len(),
            "case {:?}: expected the {} request(s) the permitted target(s) made at the stage, got \
             {held:?}",
            row.case,
            served.len()
        );
        for leg in &served {
            let found = held.iter().find(|message| {
                body_of(row.case, message)["orderSrc"].as_str() == Some(leg.header)
            });
            let message = found.unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the stage to have served the leg contributing {:?}, got \
                     {held:?}",
                    row.case, leg.header
                )
            });
            let body = body_of(row.case, message);
            assert_eq!(
                body["pathId"].as_str(),
                Some(id.as_str()),
                "case {:?}: expected the leg contributing {:?} to address the path the event \
                 names, got {body}",
                row.case,
                leg.header
            );
            let dead = messages(&router, row.case, &leg.dead_letter_url, 0).await;
            assert!(
                dead.is_empty(),
                "case {:?}: a delivery the stage served dead-letters nothing, got {dead:?}",
                row.case
            );
        }
        // Nothing else reached the stage: a refused leg's request was never made, so nothing it
        // contributes is behind the method.
        let extra = messages(&router, row.case, &stage.sink_url, 0).await;
        assert!(
            extra.is_empty(),
            "case {:?}: expected nothing at the sink beyond the permitted deliveries, got \
             {extra:?}",
            row.case
        );
        drained(&router, row.case, &stage.sink_url).await;

        // What each refused leg left: one dead letter on ITS OWN queue, naming the permission, the
        // answer the invoke endpoint gives such an identity, its own rule and its own target.
        let mut request_ids: Vec<String> = Vec::new();
        for leg in wired.iter().filter(|leg| !leg.served) {
            let dead = messages(&router, row.case, &leg.dead_letter_url, 1).await;
            assert_eq!(
                dead.len(),
                1,
                "case {:?}: expected the one dead letter the leg contributing {:?} produced, got \
                 {dead:?}",
                row.case,
                leg.header
            );
            assert_eq!(
                attribute(&dead[0], "ERROR_CODE").as_deref(),
                Some(NO_PERMISSIONS),
                "case {:?}: expected the {NO_PERMISSIONS:?} condition for the leg contributing \
                 {:?}, got {}",
                row.case,
                leg.header,
                dead[0]
            );
            request_ids.push(assert_forbidden_sentence(
                row.case,
                &attribute(&dead[0], "ERROR_MESSAGE").unwrap_or_default(),
                &format!("/_aws/execute-api/{}/{STAGE}/orders/{id}", stage.api),
            ));
            assert_eq!(
                attribute(&dead[0], "TARGET_ARN").as_deref(),
                Some(stage.target_arn().as_str()),
                "case {:?}: expected the target's own ARN on the dead letter of the leg \
                 contributing {:?}, got {}",
                row.case,
                leg.header,
                dead[0]
            );
            assert_eq!(
                attribute(&dead[0], "RULE_ARN").as_deref(),
                Some(leg.rule_arn.as_str()),
                "case {:?}: expected the invoking rule's ARN on the dead letter of the leg \
                 contributing {:?}, got {}",
                row.case,
                leg.header,
                dead[0]
            );
            // The event itself is what is parked, so a consumer of the queue has the delivery that
            // failed beside why it failed.
            let parked = body_of(row.case, &dead[0]);
            assert_eq!(
                parked["detail"]["id"].as_str(),
                Some(id.as_str()),
                "case {:?}: expected the parked event to be the one published, got {parked}",
                row.case
            );
            drained(&router, row.case, &leg.dead_letter_url).await;
        }
        // Each attempt is answered on its own, so each sentence names the id of its own answer.
        for (first, id_of_first) in request_ids.iter().enumerate() {
            for id_of_second in &request_ids[first + 1..] {
                assert_ne!(
                    id_of_first, id_of_second,
                    "case {:?}: expected each refused delivery to name the id of its own answer, \
                     got {id_of_first:?} twice",
                    row.case
                );
            }
        }
    }
}
