// SPDX-License-Identifier: CC0-1.0

//! What ONE published trigger notification REPORTS, and how many of them a
//! lifecycle event owes.
//!
//! The RULE asserted here: a deployment group's trigger publishes to "the Amazon
//! Resource Name (ARN) of the Amazon Simple Notification Service topic through
//! which notifications about deployment or instance events are sent" for "the event
//! type or types for which notifications are triggered"
//! (`smithy_query(query="shape",shape="TriggerConfig")`), so each event a
//! deployment reaches is published ONCE, and the notification reports that
//! deployment as the service's own `GetDeployment` reports it at the instant the
//! event was reached — the same `deploymentId`, `applicationName`,
//! `deploymentGroupName`, `deploymentConfigName`, `createTime`, `completeTime` and
//! `errorInformation`, under the trigger CONFIGURATION's own `triggerName`.
//!
//! The sibling suite [`crate::deployment_trigger_notifications`] asserts WHICH
//! topic an event reaches; these cases assert what is in the message that gets
//! there, that a publish no topic can accept leaves the other triggers publishing,
//! and that however many later calls read the store an event is published once. The
//! rows span what the rule generalises over: the outcome the deployment reaches,
//! whether the event is reached in flight or at the outcome, the compute platform,
//! and whether every selected event is one the deployment reaches.
//!
//! Each case reads its notifications where a client reads them — off the Amazon SQS
//! queue subscribed to the topic — through the one harness that sibling suite
//! builds topics, queues and groups with.

use std::time::Duration;

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{terminal_deployment_within, Emulator};
use crate::deployment_trigger_notifications::{
    awaited_taken_inbox, create_deployment, drain_into, given_group, given_topic, text, Inbox,
    Trigger, REGION,
};

/// How long a case here waits for the outcome the clock owes a deployment.
///
/// Deliberately generous, and far above the span the emulator gives a deployment: a case
/// here waits for the CONDITION — the deployment holding a terminal status — and asserts
/// nothing about how long reaching it took, so the wait has room on a host running every
/// other suite of the workspace beside it.
const OUTCOME_DEADLINE: Duration = Duration::from_secs(60);

/// The epoch second the instant one notification member names falls on.
///
/// A notification renders an instant as text, and `GetDeployment` answers the same
/// instant as an epoch timestamp, so the two are compared on the second they share.
fn notified_second(case: &str, document: &Value, member: &str) -> i64 {
    let rendered = text(document, member);
    ::aws_smithy_types::DateTime::from_str(
        &rendered,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|error| {
        panic!("case {case:?}: {member} {rendered:?} is no instant ({error}): {document:?}")
    })
    .secs()
}

/// The epoch second `GetDeployment` reports one of its own instants on, or `None`
/// for an instant it does not report.
fn reported_second(info: &Value, member: &str) -> Option<i64> {
    info[member].as_f64().map(|seconds| seconds.trunc() as i64)
}

/// RULE: a notification reports the deployment it is about as the service's own
/// `GetDeployment` reports it — every member read off the one stored record, so a
/// notification and a read of the same instant tell one story.
#[tokio::test]
async fn a_notification_reports_the_deployment_the_store_holds() {
    /// One row: the group the trigger is configured on, the events it selects, the
    /// terminal status the deployment reaches, and the statuses the notifications
    /// it owes report — one status per notification, compared as a SET, since which
    /// of two publishes SNS fans out first is not part of the rule.
    struct Row {
        case: &'static str,
        /// The compute platform and the group members that decide the outcome: a
        /// group that resolves at least one target ends `Succeeded`, one that
        /// resolves none ends `Failed` (see `codedeploy_deployment_outcome`).
        platform: &'static str,
        extras: Value,
        events: &'static [&'static str],
        outcome: &'static str,
        statuses: &'static [&'static str],
        /// Whether the deployment this row deploys ends carrying
        /// `errorInformation` — the one a deployment that resolves no target
        /// reports, which its notification then has to carry too.
        error_information: bool,
    }

    let rows = vec![
        Row {
            case: "a succeeding deployment reporting its start and its outcome",
            platform: "Lambda",
            extras: json!({}),
            events: &["DeploymentStart", "DeploymentSuccess"],
            outcome: "Succeeded",
            statuses: &["in_progress", "succeeded"],
            error_information: false,
        },
        Row {
            case: "a failing deployment reporting why it failed",
            platform: "Server",
            extras: json!({}),
            events: &["DeploymentStart", "DeploymentFailure"],
            outcome: "Failed",
            statuses: &["in_progress", "failed"],
            error_information: true,
        },
        Row {
            case: "every deployment event selected reports only the ones reached",
            platform: "ECS",
            extras: json!({"ecsServices": [{"clusterName": "cluster", "serviceName": "web"}]}),
            events: &[
                "DeploymentStart",
                "DeploymentSuccess",
                "DeploymentFailure",
                "DeploymentStop",
                "DeploymentRollback",
            ],
            outcome: "Succeeded",
            statuses: &["in_progress", "succeeded"],
            error_information: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::with_sns_and_sqs();
        let (topic_arn, queue_url) = given_topic(&emulator, "reports").await;
        let application = "trigger-reports";
        given_group(
            &emulator,
            application,
            row.platform,
            &row.extras,
            &[Trigger {
                name: "reporter",
                topic: Some("alpha"),
                arn: "",
                events: row.events,
            }],
            &[("alpha", topic_arn)],
        )
        .await;
        // The one-time verification notification the stored trigger owes its
        // topic, taken off the queue so the documents below are what is left.
        awaited_taken_inbox(&emulator, case, &queue_url, 0, 1).await;

        let deployment_id = create_deployment(&emulator, application).await;
        let info =
            terminal_deployment_within(&emulator, &deployment_id, case, OUTCOME_DEADLINE).await;
        assert_eq!(
            info["status"].as_str(),
            Some(row.outcome),
            "case {case:?}: expected the terminal status {}, got {info:?}",
            row.outcome
        );
        // The row says whether this deployment ends carrying error information, so
        // the comparison below is one that can fail rather than one a missing
        // member satisfies.
        assert_eq!(
            !info["errorInformation"].is_null(),
            row.error_information,
            "case {case:?}: expected errorInformation present={}, got {info:?}",
            row.error_information
        );

        let delivered =
            awaited_taken_inbox(&emulator, case, &queue_url, row.statuses.len(), 0).await;
        let mut statuses: Vec<String> = delivered
            .documents
            .iter()
            .map(|document| text(document, "status"))
            .collect();
        statuses.sort();
        let mut expected: Vec<String> = row.statuses.iter().map(|s| (*s).to_string()).collect();
        expected.sort();
        assert_eq!(
            statuses, expected,
            "case {case:?}: expected the statuses {expected:?}, got {delivered:?}"
        );

        // The configuration the deployment ran under, as the service itself reports it: read
        // first, so the comparison below cannot be satisfied by two absences.
        let reported_config = text(&info, "deploymentConfigName");
        assert!(
            !reported_config.is_empty() && reported_config != "<missing>",
            "case {case:?}: expected GetDeployment to report the deployment's \
             deploymentConfigName, got {info:?}"
        );

        for document in &delivered.documents {
            assert_eq!(
                (
                    text(document, "eventTriggerName"),
                    text(document, "deploymentId"),
                    text(document, "applicationName"),
                    text(document, "deploymentGroupName"),
                    text(document, "deploymentConfigName"),
                    text(document, "region"),
                    text(document, "accountId"),
                ),
                (
                    "reporter".to_string(),
                    deployment_id.clone(),
                    application.to_string(),
                    "dg".to_string(),
                    reported_config.clone(),
                    REGION.to_string(),
                    ::engine::emulator_account_id().to_string(),
                ),
                "case {case:?}: the notification must name the deployment the store holds, \
                 which {info:?} reports; got {document:?}"
            );
            assert_eq!(
                notified_second(case, document, "createTime"),
                reported_second(&info, "createTime").unwrap_or_else(|| {
                    panic!("case {case:?}: GetDeployment reported no createTime: {info:?}")
                }),
                "case {case:?}: the notification must carry the deployment's own createTime, \
                 got {document:?}"
            );

            // The record AT THE INSTANT the event was reached: a deployment still
            // in flight has not completed and has nothing to report about an
            // error, and one that has completed carries both as `GetDeployment`
            // does.
            let in_flight = text(document, "status") == "in_progress";
            assert_eq!(
                document.get("completeTime").is_some(),
                !in_flight,
                "case {case:?}: completeTime belongs to a completed deployment only, got \
                 {document:?}"
            );
            if !in_flight {
                assert_eq!(
                    Some(notified_second(case, document, "completeTime")),
                    reported_second(&info, "completeTime"),
                    "case {case:?}: the notification must carry the deployment's own \
                     completeTime, which {info:?} reports; got {document:?}"
                );
            }
            let notified_error = document.get("errorInformation").cloned();
            if row.error_information && !in_flight {
                let reported_error = info["errorInformation"].clone();
                let reported_code = text(&reported_error, "code");
                let reported_message = text(&reported_error, "message");
                // Read off the service first, so the comparison below is against values it
                // really reports rather than against two absent members.
                assert!(
                    ![reported_code.as_str(), reported_message.as_str()]
                        .iter()
                        .any(|value| value.is_empty() || *value == "<missing>"),
                    "case {case:?}: expected GetDeployment to report the failure's code and \
                     message, got {reported_error:?}"
                );
                assert_eq!(
                    notified_error
                        .as_ref()
                        .map(|error| (text(error, "code"), text(error, "message"))),
                    Some((reported_code, reported_message)),
                    "case {case:?}: the notification must carry the error information \
                     {reported_error:?} the deployment reports, got {document:?}"
                );
            } else {
                assert!(
                    notified_error.is_none(),
                    "case {case:?}: a deployment reporting no error information publishes none, \
                     got {document:?}"
                );
            }
        }
    }
}

/// RULE: publishing is per TRIGGER, so a trigger whose `triggerTargetArn` names a
/// topic Amazon SNS does not hold publishes nothing and changes nothing else — the
/// group is stored with both triggers, the deployment keeps the outcome it reached,
/// and the trigger that names a topic still publishes to it.
#[tokio::test]
async fn a_topic_amazon_sns_does_not_hold_leaves_the_other_triggers_publishing() {
    let case = "one trigger naming a topic nothing holds";
    let emulator = Emulator::with_sns_and_sqs();
    let (topic_arn, queue_url) = given_topic(&emulator, "held").await;
    let application = "trigger-missing-topic";
    // A well-formed Amazon SNS topic ARN, in the Region these calls are signed for, that no
    // `CreateTopic` ever created — the account label is written out because the fixture takes
    // a literal `triggerTargetArn`, and what makes this one unpublishable-to is that Amazon
    // SNS holds no topic under it, which is not this service's failure.
    let triggers = [
        Trigger {
            name: "to-nothing",
            topic: None,
            arn: "arn:aws:sns:us-east-1:000000000000:codedeploy-no-such-topic",
            events: &["DeploymentSuccess"],
        },
        Trigger {
            name: "on-success",
            topic: Some("alpha"),
            arn: "",
            events: &["DeploymentSuccess"],
        },
    ];
    given_group(
        &emulator,
        application,
        "Lambda",
        &json!({}),
        &triggers,
        &[("alpha", topic_arn)],
    )
    .await;

    // Both triggers are stored, whatever either one's topic can accept.
    let group = emulator
        .ok(
            "GetDeploymentGroup",
            json!({"applicationName": application, "deploymentGroupName": "dg"}),
        )
        .await["deploymentGroupInfo"]
        .clone();
    let stored: Vec<String> = group["triggerConfigurations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|trigger| text(trigger, "triggerName"))
        .collect();
    assert_eq!(
        stored,
        vec!["to-nothing".to_string(), "on-success".to_string()],
        "case {case:?}: both triggers must round-trip, got {group:?}"
    );

    awaited_taken_inbox(&emulator, case, &queue_url, 0, 1).await;
    let deployment_id = create_deployment(&emulator, application).await;
    let info = terminal_deployment_within(&emulator, &deployment_id, case, OUTCOME_DEADLINE).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the deployment must keep the outcome it reached, got {info:?}"
    );

    let delivered = awaited_taken_inbox(&emulator, case, &queue_url, 1, 0).await;
    let reported: Vec<(String, String)> = delivered
        .documents
        .iter()
        .map(|document| (text(document, "eventTriggerName"), text(document, "status")))
        .collect();
    assert_eq!(
        reported,
        vec![("on-success".to_string(), "succeeded".to_string())],
        "case {case:?}: the trigger whose topic exists must still publish the outcome, got \
         {delivered:?}"
    );
}

/// RULE: a deployment reaches a lifecycle event once, so the event is published
/// once — however many later calls read the deployment, its group or the store's
/// history.
#[tokio::test]
async fn each_event_is_published_once_however_many_calls_read_the_store() {
    let case = "repeated reads after the outcome";
    let emulator = Emulator::with_sns_and_sqs();
    let (topic_arn, queue_url) = given_topic(&emulator, "once").await;
    let application = "trigger-once";
    given_group(
        &emulator,
        application,
        "Lambda",
        &json!({}),
        &[Trigger {
            name: "once",
            topic: Some("alpha"),
            arn: "",
            events: &["DeploymentStart", "DeploymentSuccess"],
        }],
        &[("alpha", topic_arn)],
    )
    .await;
    let verification = awaited_taken_inbox(&emulator, case, &queue_url, 0, 1).await;
    assert_eq!(
        verification.plain.len(),
        1,
        "case {case:?}: the stored trigger owes its topic one verification notification, got \
         {verification:?}"
    );

    let deployment_id = create_deployment(&emulator, application).await;
    terminal_deployment_within(&emulator, &deployment_id, case, OUTCOME_DEADLINE).await;
    let delivered = awaited_taken_inbox(&emulator, case, &queue_url, 2, 0).await;
    let mut statuses: Vec<String> = delivered
        .documents
        .iter()
        .map(|document| text(document, "status"))
        .collect();
    statuses.sort();
    assert_eq!(
        statuses,
        vec!["in_progress".to_string(), "succeeded".to_string()],
        "case {case:?}: the two events the deployment reached are published once each, got \
         {delivered:?}"
    );

    // Every one of these calls passes over the store, which is where a
    // notification is found to be due: an event already published is owed nothing
    // by any of them. The queue is looked at TWICE, with another round of passes
    // in between, so a publish still in flight at the first look is caught by the
    // second rather than waited out on a clock.
    for look in 0..2 {
        for _ in 0..4 {
            emulator
                .ok("GetDeployment", json!({"deploymentId": deployment_id}))
                .await;
            emulator
                .ok(
                    "BatchGetDeployments",
                    json!({"deploymentIds": [deployment_id]}),
                )
                .await;
            emulator
                .ok(
                    "ListDeployments",
                    json!({"applicationName": application, "deploymentGroupName": "dg"}),
                )
                .await;
            emulator
                .ok(
                    "GetDeploymentGroup",
                    json!({"applicationName": application, "deploymentGroupName": "dg"}),
                )
                .await;
        }
        let mut after = Inbox::default();
        drain_into(&emulator, &queue_url, &mut after, true).await;
        assert_eq!(
            (after.documents.len(), after.plain.len()),
            (0, 0),
            "case {case:?}: reading the store publishes nothing of its own, look {look}: got \
             {after:?}"
        );
    }
}
