// SPDX-License-Identifier: CC0-1.0

//! What the `Bounce` email event a configuration set's Amazon EventBridge event destination
//! publishes REPORTS about the failure — the sub-object a client that maintains a suppression
//! list, accounts for deliveries per recipient or alarms on a bounce rate reads.
//!
//! The RULE these cases pin: for each DISTINCT failure an accepted send produced, an event
//! destination of the configuration set the message named that is `Enabled` and whose
//! `MatchingEventTypes` names `BOUNCE` publishes onto its bus ONE event of the failure's own
//! documented detail-type, whose detail is the `Bounce` document of that failure — the class
//! the failure belongs to (`bounceType`, `bounceSubType`), when it was reported
//! (`timestamp`), the identifier of the report (`feedbackId`), one `bouncedRecipients` entry
//! per recipient that failed that way carrying the address as the send addressed it, what was
//! done with the message for it (`action`), the enhanced status code of the failure
//! (`status`) and what the failing side said (`diagnosticCode`) — and which names the
//! reporting mail server only where a mail server reported the failure at all; the `mail`
//! object of every one of those events names every recipient the message was accepted for.
//!
//! The acceptance of the same message is published under its own detail-type, so a
//! destination matching `SEND` and `BOUNCE` carries exactly one acceptance beside those
//! failure reports and nothing else.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service,
//! Amazon EventBridge and Amazon SQS on ONE engine — and reads each event where a client
//! observes one: on the queue the rule's target enqueues the whole envelope into. The harness
//! is the one [`crate::event_bridge_event_destination_delivery`] already drives these sends
//! through.
//!
//! The table spans what the rule generalises over: which failure class the send's recipients
//! produce (a mailbox that refuses the message, and a message SES never attempted because the
//! address is on the account's suppression list), how the address that produces it is spelled,
//! whether the class comes from the mailbox simulator or from the account's own list, how many
//! recipients one failure is about, and whether the recipients of one send fail the same way
//! or differently. The recorded case — one send to the simulator's bouncing mailbox — is the
//! first row of it.

use serde_json::{json, Value};

use crate::event_bridge_event_destination_delivery::{
    assert_event, queue_arn, settled_envelopes, Emulator, Expected, BOUNCING, REGION,
    SES_EVENT_SOURCE,
};

/// The detail-types the two event types a row publishes carry
/// ([`crate::event_bridge_email_event_detail_types`]).
const SENT_DETAIL_TYPE: &str = "Email Sent";
const BOUNCED_DETAIL_TYPE: &str = "Email Bounced";

/// The event types every row's destination matches: the acceptance of the message and the
/// failures its recipients produce.
const MATCHED_EVENT_TYPES: &[&str] = &["SEND", "BOUNCE"];

/// A mailbox of the mailbox-simulator domain its catalog does not define
/// (`state::event_publication::outcome::SIMULATOR_MAILBOXES` lists the ones it does), which
/// is no mailbox at all and so fails the way any unknown user does.
const UNDEFINED_MAILBOX: &str = "no-such-mailbox@simulator.amazonses.com";

/// The simulator's bouncing mailbox spelled in another case: a domain is compared the way DNS
/// compares one and the simulator's own mailbox names are ASCII, so this is the same mailbox.
const BOUNCING_IN_ANOTHER_CASE: &str = "Bounce@Simulator.AmazonSES.COM";

/// The simulator's mailbox that answers as SES does for an address on the account's
/// suppression list, from the same catalog.
const SUPPRESSION_LIST_MAILBOX: &str = "suppressionlist@simulator.amazonses.com";

/// An ordinary address — no mailbox of the simulator — that the account's suppression list
/// already holds when the send is made, so the same failure class arises from the account's
/// own list rather than from the simulator.
const LISTED_RECIPIENT: &str = "listed@recipients.example.com";

/// `SuppressionListReason`'s value a listed address is put on the list for by the rows that
/// list one (`smithy_query(query="shape",shape="SuppressionListReason")`: BOUNCE | COMPLAINT).
const SUPPRESSED_FOR_BOUNCE: &str = "BOUNCE";

/// One distinct failure one send produced, as the published `Bounce` document reports it.
struct Report {
    /// The recipients this one failure is about, in the order the send addressed them.
    recipients: &'static [&'static str],
    /// `bounceType`: whether the failure is final.
    bounce_type: &'static str,
    /// `bounceSubType`: which kind of failure it is.
    bounce_sub_type: &'static str,
    /// `bouncedRecipients[].action`: what was done with the message for the recipient, under
    /// the field values RFC 3464 defines for a delivery status report.
    action: &'static str,
    /// `bouncedRecipients[].status`: the enhanced status code of the failure, under RFC 3463
    /// (`5.1.1` is a bad destination mailbox address).
    status: &'static str,
    /// What the `diagnosticCode` of every entry of this report has to name: the SMTP reply
    /// the receiving server gave, or the reason SES itself did not attempt the message.
    diagnostic_names: &'static str,
    /// Whether a mail server reported this failure, which is what decides whether the report
    /// names one at all.
    reported_by_mail_server: bool,
}

/// The failure a mailbox that does not exist produces.
const fn unknown_user(recipients: &'static [&'static str]) -> Report {
    Report {
        recipients,
        bounce_type: "Permanent",
        bounce_sub_type: "General",
        action: "failed",
        status: "5.1.1",
        diagnostic_names: "550",
        reported_by_mail_server: true,
    }
}

/// The failure an address on the account's suppression list produces: SES made no attempt, so
/// no mail server was asked and none is named.
const fn on_account_suppression_list(recipients: &'static [&'static str]) -> Report {
    Report {
        recipients,
        bounce_type: "Permanent",
        bounce_sub_type: "OnAccountSuppressionList",
        action: "failed",
        status: "5.1.1",
        diagnostic_names: "suppression list for your account",
        reported_by_mail_server: false,
    }
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The configuration set the row creates and sends through.
    set: &'static str,
    /// The queue the row's rule hands the envelopes to.
    queue: &'static str,
    /// The rule matching this service's event source.
    rule: &'static str,
    /// The addresses the account's suppression list already holds when the send is made.
    listed: &'static [&'static str],
    /// The recipients of the one send, in the order the request lists them.
    recipients: &'static [&'static str],
    /// The failures the published events must report, and no others.
    reports: &'static [Report],
}

const ROWS: &[Row] = &[
    Row {
        case: "the simulator's bouncing mailbox",
        set: "bounce-report-set",
        queue: "bounce-report-queue",
        rule: "bounce-report-rule",
        listed: &[],
        recipients: &[BOUNCING],
        reports: &[unknown_user(&[BOUNCING])],
    },
    Row {
        case: "a mailbox of the simulator domain its catalog does not define",
        set: "undefined-mailbox-set",
        queue: "undefined-mailbox-queue",
        rule: "undefined-mailbox-rule",
        listed: &[],
        recipients: &[UNDEFINED_MAILBOX],
        reports: &[unknown_user(&[UNDEFINED_MAILBOX])],
    },
    Row {
        case: "the bouncing mailbox spelled in another case",
        set: "bounce-case-set",
        queue: "bounce-case-queue",
        rule: "bounce-case-rule",
        listed: &[],
        recipients: &[BOUNCING_IN_ANOTHER_CASE],
        reports: &[unknown_user(&[BOUNCING_IN_ANOTHER_CASE])],
    },
    Row {
        case: "the simulator's suppression-list mailbox",
        set: "suppression-mailbox-set",
        queue: "suppression-mailbox-queue",
        rule: "suppression-mailbox-rule",
        listed: &[],
        recipients: &[SUPPRESSION_LIST_MAILBOX],
        reports: &[on_account_suppression_list(&[SUPPRESSION_LIST_MAILBOX])],
    },
    Row {
        case: "an ordinary address the account's suppression list already holds",
        set: "listed-address-set",
        queue: "listed-address-queue",
        rule: "listed-address-rule",
        listed: &[LISTED_RECIPIENT],
        recipients: &[LISTED_RECIPIENT],
        reports: &[on_account_suppression_list(&[LISTED_RECIPIENT])],
    },
    Row {
        case: "two recipients that fail the same way are one report",
        set: "one-report-set",
        queue: "one-report-queue",
        rule: "one-report-rule",
        listed: &[],
        recipients: &[BOUNCING, UNDEFINED_MAILBOX],
        reports: &[unknown_user(&[BOUNCING, UNDEFINED_MAILBOX])],
    },
    Row {
        case: "two recipients that fail differently are two reports",
        set: "two-reports-set",
        queue: "two-reports-queue",
        rule: "two-reports-rule",
        listed: &[],
        recipients: &[BOUNCING, SUPPRESSION_LIST_MAILBOX],
        reports: &[
            unknown_user(&[BOUNCING]),
            on_account_suppression_list(&[SUPPRESSION_LIST_MAILBOX]),
        ],
    },
];

/// The addresses the `bouncedRecipients` of `detail` names, in the order the report lists
/// them.
fn bounced_addresses(detail: &Value) -> Vec<String> {
    detail["bounce"]["bouncedRecipients"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            entry["emailAddress"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// A string member of `value`, or `""` when it carries none there.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// Assert the whole `Bounce` document `envelope` carries against the failure `report`
/// describes, for a send to `recipients` that minted `message_id` through `set`.
fn assert_report(
    case: &str,
    report: &Report,
    envelope: &Value,
    recipients: &[&str],
    set: &str,
    message_id: &str,
) {
    // The envelope the bus carries, and the parts of the detail every email event of this
    // service's source carries: its source, its detail-type, the event type its detail is of,
    // the message it is about, the configuration set the send named, and the failure class.
    assert_event(
        case,
        &Expected {
            detail_type: BOUNCED_DETAIL_TYPE,
            event_type: "Bounce",
            addresses: report.recipients,
            bounce: Some((report.bounce_type, report.bounce_sub_type)),
        },
        envelope,
        set,
        message_id,
    );

    let detail = &envelope["detail"];
    // A failure is reported as an event of the MESSAGE, so its `mail` object names every
    // recipient the message was accepted for — not only the ones this failure is about.
    let addressed: Vec<String> = recipients.iter().map(|at| (*at).to_string()).collect();
    assert_eq!(
        detail["mail"]["destination"],
        json!(addressed),
        "case {case:?}: expected the failure's mail.destination to name {addressed:?}, got {:?}",
        detail["mail"]["destination"]
    );

    let bounce = &detail["bounce"];
    let stamped = text(bounce, "timestamp");
    assert!(
        ::aws_smithy_types::DateTime::from_str(
            &stamped,
            ::aws_smithy_types::date_time::Format::DateTime
        )
        .is_ok(),
        "case {case:?}: expected bounce.timestamp to be a timestamp, got {stamped:?}"
    );
    assert!(
        !text(bounce, "feedbackId").is_empty(),
        "case {case:?}: expected bounce.feedbackId to identify the report, got {:?}",
        bounce["feedbackId"]
    );

    // One entry per recipient this failure is about, each naming the address as the send
    // addressed it and carrying the same failure's own fields.
    let listed = bounced_addresses(detail);
    let wanted: Vec<String> = report
        .recipients
        .iter()
        .map(|at| (*at).to_string())
        .collect();
    assert_eq!(
        listed, wanted,
        "case {case:?}: expected bounce.bouncedRecipients to name {wanted:?}, got {listed:?}"
    );
    for entry in bounce["bouncedRecipients"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let at = text(&entry, "emailAddress");
        assert_eq!(
            text(&entry, "action"),
            report.action,
            "case {case:?}: expected action {:?} for {at:?}, got {:?}",
            report.action,
            entry["action"]
        );
        assert_eq!(
            text(&entry, "status"),
            report.status,
            "case {case:?}: expected status {:?} for {at:?}, got {:?}",
            report.status,
            entry["status"]
        );
        let diagnostic = text(&entry, "diagnosticCode");
        assert!(
            diagnostic.contains(report.diagnostic_names),
            "case {case:?}: expected the diagnosticCode of {at:?} to name {:?}, got \
             {diagnostic:?}",
            report.diagnostic_names
        );
    }

    // The mail server is named only where one answered for the failure.
    let reporting_mta = text(bounce, "reportingMTA");
    if report.reported_by_mail_server {
        assert!(
            // RFC 3464 writes the field as its address type and the server, and the server
            // answering here is this emulator's own interface in the Region of the send.
            reporting_mta.starts_with("dsn; ") && reporting_mta.contains(REGION),
            "case {case:?}: expected bounce.reportingMTA to name the reporting server in \
             {REGION:?}, got {reporting_mta:?}"
        );
    } else {
        assert!(
            bounce.get("reportingMTA").is_none(),
            "case {case:?}: a failure no mail server reported must name none, got \
             {reporting_mta:?}"
        );
    }
}

/// The envelopes of `detail_type` among `held`.
fn of_detail_type(held: &[Value], detail_type: &str) -> Vec<Value> {
    held.iter()
        .filter(|envelope| envelope["detail-type"] == json!(detail_type))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_published_bounce_reports_the_failure_its_recipients_produced() {
    for row in ROWS {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_verified_sender(case).await;
        emulator
            .given_destination(case, row.set, true, MATCHED_EVENT_TYPES)
            .await;
        for address in row.listed {
            emulator
                .given_suppressed(case, address, SUPPRESSED_FOR_BOUNCE)
                .await;
        }
        let url = emulator.given_queue(case, row.queue).await;
        // A rule on this service's own event source, so every email event the destination
        // publishes reaches the queue and the row's own count is what decides which arrived.
        emulator
            .given_rule(
                case,
                row.rule,
                &json!({"source": [SES_EVENT_SOURCE]}),
                &queue_arn(row.queue),
            )
            .await;

        let message_id = emulator.send(case, row.set, row.recipients).await;
        let wanted = row.reports.len() + 1;
        let held = settled_envelopes(&emulator, case, &url, wanted).await;
        assert_eq!(
            held.len(),
            wanted,
            "case {case:?}: expected the acceptance and {} failure report(s), got {held:?}",
            row.reports.len()
        );

        // The acceptance of the message, published beside the failures under its own
        // detail-type.
        let accepted = of_detail_type(&held, SENT_DETAIL_TYPE);
        assert_eq!(
            accepted.len(),
            1,
            "case {case:?}: expected exactly one {SENT_DETAIL_TYPE:?} event, got {accepted:?}"
        );
        assert_event(
            case,
            &Expected {
                detail_type: SENT_DETAIL_TYPE,
                event_type: "Send",
                addresses: row.recipients,
                bounce: None,
            },
            &accepted[0],
            row.set,
            &message_id,
        );

        let bounced = of_detail_type(&held, BOUNCED_DETAIL_TYPE);
        assert_eq!(
            bounced.len(),
            row.reports.len(),
            "case {case:?}: expected {} {BOUNCED_DETAIL_TYPE:?} event(s), got {bounced:?}",
            row.reports.len()
        );
        let mut identifiers: Vec<String> = Vec::new();
        for report in row.reports {
            // The report about THESE recipients, whichever order the bus handed the events
            // over in.
            let about: Vec<String> = report
                .recipients
                .iter()
                .map(|at| (*at).to_string())
                .collect();
            let envelope = bounced
                .iter()
                .find(|envelope| bounced_addresses(&envelope["detail"]) == about)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no published failure is about {about:?}: {bounced:?}")
                });
            assert_report(
                case,
                report,
                envelope,
                row.recipients,
                row.set,
                &message_id,
            );
            identifiers.push(text(&envelope["detail"]["bounce"], "feedbackId"));
        }
        // One identifier per report: two failures of the same message are two reports, and a
        // consumer keyed on the identifier must be able to tell them apart.
        let distinct: Vec<String> = {
            let mut seen = identifiers.clone();
            seen.sort();
            seen.dedup();
            seen
        };
        assert_eq!(
            distinct.len(),
            identifiers.len(),
            "case {case:?}: each report must carry its own feedbackId, got {identifiers:?}"
        );
    }
}

#[tokio::test]
async fn a_published_bounce_carries_the_feedback_identifier_the_suppression_list_reports() {
    // `FeedbackId` is "a unique identifier that's generated when an email address is added to
    // the suppression list", and the `feedbackId` a published failure carries identifies that
    // same report — so a client that reacts to the event and then reads the list back finds
    // the one identifier under both, which is what correlating the two depends on.
    let case = "the failure that put the address on the account's suppression list";
    let set = "feedback-id-set";
    let queue = "feedback-id-queue";
    let emulator = Emulator::new();
    emulator.given_verified_sender(case).await;
    emulator
        .given_destination(case, set, true, &["BOUNCE"])
        .await;
    let url = emulator.given_queue(case, queue).await;
    emulator
        .given_rule(
            case,
            "feedback-id-rule",
            &json!({"source": [SES_EVENT_SOURCE], "detail-type": [BOUNCED_DETAIL_TYPE]}),
            &queue_arn(queue),
        )
        .await;

    let message_id = emulator.send(case, set, &[BOUNCING]).await;
    let held = settled_envelopes(&emulator, case, &url, 1).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected exactly one {BOUNCED_DETAIL_TYPE:?} event, got {held:?}"
    );
    let published = text(&held[0]["detail"]["bounce"], "feedbackId");
    let listed = emulator.suppressed_feedback_id(case, BOUNCING).await;
    assert!(
        !listed.is_empty(),
        "case {case:?}: a hard failure must put {BOUNCING:?} on the list with a FeedbackId"
    );
    assert_eq!(
        published, listed,
        "case {case:?}: expected the event's feedbackId to be the FeedbackId the list reports \
         for the message {message_id:?}, got {published:?} against {listed:?}"
    );
}
