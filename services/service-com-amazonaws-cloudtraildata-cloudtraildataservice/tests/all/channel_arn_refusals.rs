// SPDX-License-Identifier: CC0-1.0

//! How `PutAuditEvents` refuses a call it cannot serve, driven over HTTP through the
//! same `MultiServiceRouter` the emulator serves with.
//!
//! Four rules, each asserted over a TABLE whose rows differ along what the rule
//! generalises over — the form of the channel reference, the account it names, whether
//! the channel it names is registered here, and what else the same request got wrong:
//!
//! 1. A refusal states the message AWS words for its class: the sentence the model
//!    documents, with nothing interpolated where that wording leaves no slot for a
//!    caller value, and the channel the request named where it does.
//! 2. A channel reference outside the modelled `@pattern` of `ChannelArn` is refused by
//!    the SERVICE, under the error its model declares for that fault, rather than by the
//!    generic constraint walk under a code this service's model does not contain.
//! 3. A channel ARN in another account is refused by authorization, with HTTP 403 and
//!    the common `AccessDeniedException` code, before the channel is looked up — so the
//!    answer is the same whether or not such a channel is registered here.
//! 4. Both ways this service can state a refusal found before its handler ran — the
//!    wrapper that serves the operation, and the service's own refusal seam — take ONE
//!    decision about a channel reference, so the same violation set is answered the same
//!    way whichever of them answers it.
//!
//! The client is this crate's one test harness, reached from the suite that owns it.

use std::sync::Arc;

use crate::put_audit_events::{channel_arn, ev, Ev, Harness, Reply, REGION};
use service_com_amazonaws_cloudtraildata_cloudtraildataservice::ops::put_audit_events::CHANNEL_ARN_PATTERN;
use service_com_amazonaws_cloudtraildata_cloudtraildataservice::state::Channel;
use service_com_amazonaws_cloudtraildata_cloudtraildataservice::EmulatedService;
use smithy_server_runtime::{Refusal, Service, Violation, ViolationKind};

/// The sentence `InvalidChannelARN` carries, as the model documents the shape
/// (`smithy_query(query="shape", shape="InvalidChannelARN")`).
const INVALID_CHANNEL_ARN: &str = "The specified channel ARN is not a valid channel ARN.";

/// The sentence `ChannelNotFound` carries: the channel the request asked about, then the
/// fact the model documents for the shape ("The channel could not be found.").
fn channel_not_found(reference: &str) -> String {
    format!("The channel {reference} could not be found.")
}

/// The sentence `DuplicatedAuditEventId` carries, from the same place.
const DUPLICATED_AUDIT_EVENT_ID: &str =
    "Two or more entries in the request have the same event ID.";

/// The error code the authorization layer names, and the status it answers with.
const ACCESS_DENIED: (u16, &str) = (403, "AccessDeniedException");

/// A JSON audit event that passes per-event validation, so nothing but the channel
/// reference decides these calls: the one payload carrying the content CloudTrail's open
/// audit event schema requires, from the suite that owns this crate's test client.
const GOOD_EVENT: &str = crate::put_audit_events::AUDIT_EVENT;

/// The prose member of a refusal: the modelled error shapes of this service spell it
/// `message`, and the authorization layer's `AccessDeniedException` — which is not one of
/// this service's shapes — spells it `Message`.
pub fn message_of(reply: &Reply) -> String {
    for member in ["message", "Message"] {
        if let Some(text) = reply.body.get(member).and_then(serde_json::Value::as_str) {
            return text.to_string();
        }
    }
    String::new()
}

/// The denial AWS words for a call no policy allows: who the request spoke for, the
/// action it asked for, and the resource it named. The principal is the one identity this
/// emulator publishes (what `sts:GetCallerIdentity` answers with), and the resource is
/// the ARN from the request — nothing here is a literal from a recording.
fn denial(resource: &str) -> String {
    format!(
        "User: {} is not authorized to perform: cloudtrail-data:PutAuditEvents on \
         resource: {resource} because no resource-based policy allows the \
         cloudtrail-data:PutAuditEvents action",
        engine::emulator_arn("iam", "", "user/emulator")
    )
}

/// One refusal to assert.
struct Case {
    name: &'static str,
    channel: String,
    events: Vec<(&'static str, &'static str)>,
    status: u16,
    code: &'static str,
    /// What AWS words for this refusal, with whatever slots that wording has filled from
    /// THIS request.
    message: String,
    /// Whether that wording has no slot at all: the sentence the model documents for the
    /// shape, which must be stated verbatim with nothing from the request in it.
    fixed_sentence: bool,
}

/// A refusal states what AWS words for its class: the sentence the model documents, with
/// nothing from the request interpolated where that wording has no slot for one, and the
/// channel the request named where it does. The emulator never appends the offending
/// value to a sentence that has no room for it.
#[tokio::test]
async fn a_documented_refusal_states_its_sentence_and_echoes_no_request_value() {
    let harness = Harness::new();
    let registered = channel_arn("9a0b1c2d-3e4f-5061-7283-94a5b6c7d8e9");
    harness.register(Channel::from_arn(&registered, "Custom").expect("a channel ARN"));

    let unheld = channel_arn("0b1c2d3e-4f50-6172-8394-a5b6c7d8e9fa");
    let other_region = engine::emulator_arn(
        "cloudtrail",
        "eu-west-3",
        "channel/9a0b1c2d-3e4f-5061-7283-94a5b6c7d8e9",
    );

    let cases: Vec<Case> = vec![
        Case {
            name: "an arn: value that is not a channel ARN at all",
            channel: "arn:aws:cloudtrail:us-east-1:000000000000:not-a-channel-resource"
                .to_string(),
            events: vec![("evt-1", GOOD_EVENT)],
            status: 400,
            code: "InvalidChannelARN",
            message: INVALID_CHANNEL_ARN.to_string(),
            fixed_sentence: true,
        },
        Case {
            name: "another service's ARN",
            channel: "arn:aws:s3:::a-bucket-name".to_string(),
            events: vec![("evt-1", GOOD_EVENT)],
            status: 400,
            code: "InvalidChannelARN",
            message: INVALID_CHANNEL_ARN.to_string(),
            fixed_sentence: true,
        },
        Case {
            name: "a channel ARN whose channel id is empty",
            channel: engine::emulator_arn("cloudtrail", REGION, "channel/"),
            events: vec![("evt-1", GOOD_EVENT)],
            status: 400,
            code: "InvalidChannelARN",
            message: INVALID_CHANNEL_ARN.to_string(),
            fixed_sentence: true,
        },
        Case {
            name: "a value outside the member's modelled pattern",
            channel: "channel-reference-that-is-no-arn".to_string(),
            events: vec![("evt-1", GOOD_EVENT)],
            status: 400,
            code: "InvalidChannelARN",
            message: INVALID_CHANNEL_ARN.to_string(),
            fixed_sentence: true,
        },
        Case {
            name: "a well-formed channel ARN this emulator does not hold",
            channel: unheld.clone(),
            events: vec![("evt-1", GOOD_EVENT)],
            status: 400,
            code: "ChannelNotFound",
            message: channel_not_found(&unheld),
            fixed_sentence: false,
        },
        Case {
            name: "a channel ARN in a region this emulator holds no channel in",
            channel: other_region.clone(),
            events: vec![("evt-1", GOOD_EVENT)],
            status: 400,
            code: "ChannelNotFound",
            message: channel_not_found(&other_region),
            fixed_sentence: false,
        },
        Case {
            name: "two entries of a batch sharing one event id",
            channel: registered.clone(),
            events: vec![
                ("repeated-event-id", GOOD_EVENT),
                ("repeated-event-id", GOOD_EVENT),
            ],
            status: 400,
            code: "DuplicatedAuditEventId",
            message: DUPLICATED_AUDIT_EVENT_ID.to_string(),
            fixed_sentence: true,
        },
        Case {
            name: "a longer batch repeating its first event id last",
            channel: registered.clone(),
            events: vec![
                ("first-event", GOOD_EVENT),
                ("second-event", GOOD_EVENT),
                ("first-event", GOOD_EVENT),
            ],
            status: 400,
            code: "DuplicatedAuditEventId",
            message: DUPLICATED_AUDIT_EVENT_ID.to_string(),
            fixed_sentence: true,
        },
    ];

    for case in &cases {
        let events: Vec<_> = case
            .events
            .iter()
            .map(|(id, data)| ev(*id, *data))
            .collect();
        let reply = harness.put(&case.channel, None, &events).await;
        let name = case.name;
        assert_eq!(
            reply.status, case.status,
            "case {name:?}: expected HTTP {}, got {} with body {}",
            case.status, reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            case.code,
            "case {name:?}: expected error code {:?}, got body {}",
            case.code,
            reply.body
        );
        let stated = message_of(&reply);
        assert_eq!(
            stated, case.message,
            "case {name:?}: expected the message AWS words for this refusal ({:?}), got \
             {stated:?}",
            case.message
        );
        if case.fixed_sentence {
            // The point of a held-constant sentence: no value the caller sent reaches
            // it, neither the channel reference nor an event id.
            assert!(
                !stated.contains(&case.channel),
                "case {name:?}: the message must not echo the channel reference, got \
                 {stated:?}"
            );
            for (id, _) in &case.events {
                assert!(
                    !stated.contains(id),
                    "case {name:?}: the message must not echo the event id {id:?}, got \
                     {stated:?}"
                );
            }
        }
    }

    assert_eq!(
        harness.ingest_log(&registered).len(),
        0,
        "every call above was refused, so the registered channel must hold nothing: got \
         {:?}",
        harness.ingest_log(&registered)
    );
}

/// A channel reference the member's modelled `@pattern ^arn:.*$` rejects is refused by
/// the service under its own `InvalidChannelARN` — never under an error code this
/// service's model does not declare. Spans the forms such a value takes: a word, a
/// mis-cased ARN prefix, a bare channel id, a URL, an empty value and an oversized one.
#[tokio::test]
async fn a_channel_reference_outside_the_modelled_pattern_is_the_services_own_refusal() {
    let harness = Harness::new();
    let long = "c".repeat(400);

    // (case name, the channelArn value)
    let rows: Vec<(&str, String)> = vec![
        ("a bare word", "probe-value".to_string()),
        (
            "an upper-case ARN prefix",
            "ARN:aws:cloudtrail:us-east-1:000000000000:channel/abc".to_string(),
        ),
        ("the prefix without its colon", "arn".to_string()),
        (
            "a bare channel id (the ARN suffix the doc also names)",
            "1a2b3c4d-5e6f-7081-92a3-b4c5d6e7f809".to_string(),
        ),
        (
            "a URL",
            "https://cloudtrail.us-east-1.amazonaws.com/channel".to_string(),
        ),
        ("an empty value", String::new()),
        ("a value far longer than any ARN", long),
    ];

    for (case, value) in &rows {
        let reply = harness.put(value, None, &[ev("evt-1", GOOD_EVENT)]).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "InvalidChannelARN",
            "case {case:?}: a channel reference this service cannot read is its own \
             modelled refusal, got body {}",
            reply.body
        );
        let stated = message_of(&reply);
        assert_eq!(
            stated, INVALID_CHANNEL_ARN,
            "case {case:?}: expected the documented sentence {INVALID_CHANNEL_ARN:?}, \
             got {stated:?}"
        );
    }
}

/// A channel ARN in another account is refused by AUTHORIZATION — HTTP 403,
/// `AccessDeniedException`, and a `Message` naming the principal, the action and the
/// resource — taken before the channel is resolved, so the answer cannot report whether
/// that channel exists. Spans several foreign accounts and regions, a channel registered
/// here under a foreign ARN and one that is not, with the caller's own account as the
/// control.
#[tokio::test]
async fn a_channel_arn_in_another_account_is_denied_without_disclosing_existence() {
    let harness = Harness::new();

    // (case name, account, region, channel id, registered in this emulator?)
    let rows: Vec<(&str, &str, &str, &str, bool)> = vec![
        (
            "another account, nothing registered",
            "111122223333",
            REGION,
            "11111111-2222-3333-4444-555555555555",
            false,
        ),
        (
            "another account, a channel registered under that ARN",
            "111122223333",
            REGION,
            "22222222-3333-4444-5555-666666666666",
            true,
        ),
        (
            "another account in another region",
            "444455556666",
            "ap-southeast-2",
            "33333333-4444-5555-6666-777777777777",
            false,
        ),
        (
            "an account whose id differs from the caller's by one digit",
            "000000000001",
            REGION,
            "44444444-5555-6666-7777-888888888888",
            false,
        ),
    ];

    let mut redacted_bodies: Vec<(&str, String)> = Vec::new();
    for (case, account, region, channel_id, registered) in &rows {
        let arn = format!("arn:aws:cloudtrail:{region}:{account}:channel/{channel_id}");
        if *registered {
            harness.register(Channel::from_arn(&arn, "Custom").expect("a channel ARN"));
        }
        let reply = harness.put(&arn, None, &[ev("evt-1", GOOD_EVENT)]).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            ACCESS_DENIED,
            "case {case:?}: expected {ACCESS_DENIED:?}, got {} with body {}",
            reply.status,
            reply.body
        );
        let stated = message_of(&reply);
        assert_eq!(
            stated,
            denial(&arn),
            "case {case:?}: expected the denial naming the principal, the action and the \
             resource, got {stated:?}"
        );
        assert!(
            harness.ingest_log(&arn).is_empty(),
            "case {case:?}: a denied call must persist nothing, got {:?}",
            harness.ingest_log(&arn)
        );
        // The denial is the same answer whichever channel the ARN names, so collect it
        // with the one request-derived value removed and compare the set below.
        redacted_bodies.push((case, reply.body.to_string().replace(&arn, "<resource>")));
    }

    let (first_case, first) = &redacted_bodies[0];
    for (case, body) in &redacted_bodies[1..] {
        assert_eq!(
            body, first,
            "case {case:?}: the denial must not differ from case {first_case:?} by \
             anything but the resource it names, got {body}"
        );
    }

    // The control: the SAME call in the caller's own account is served by the service,
    // which does report that it holds no such channel.
    let own = channel_arn("55555555-6666-7777-8888-999999999999");
    let reply = harness.put(&own, None, &[ev("evt-1", GOOD_EVENT)]).await;
    assert_eq!(
        (reply.status, reply.error_code()),
        (400, "ChannelNotFound".to_string()),
        "case \"the caller's own account\": the gate is keyed on the account the ARN \
         names, got {} with body {}",
        reply.status,
        reply.body
    );
}

/// The code the framework's own default names a constraint refusal with on this service's
/// JSON protocol, which is what a pre-handler refusal about anything BUT a channel
/// reference is answered under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// What one call of the wrapper path answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WireAnswer {
    status: u16,
    code: String,
    message: String,
}

/// One row of the refusal seam's table: the refusal it is handed, and the answer it has
/// to take for it.
struct SeamRow {
    case: &'static str,
    refusal: Refusal,
    /// The code and the sentence the seam must answer with, or `None` when stating this
    /// refusal is not its decision to take.
    answer: Option<(String, String)>,
}

/// One violation as the framework's validating walk hands it to a refusal renderer: the
/// member it was found at, what the value failed, and the value it held.
fn violation(path: &str, kind: ViolationKind, value: &str) -> Violation {
    Violation {
        path: path.to_string(),
        kind,
        value: Some(value.to_string()),
        sensitive: false,
    }
}

/// A pre-handler refusal of this service can be stated on either of two paths — the
/// wrapper that serves the operation (which re-reads the request) and the service's own
/// refusal seam (which is handed the violations alone) — and both take the SAME decision
/// about a channel reference: a violation set carrying a `ChannelArn` `@pattern` fault is
/// answered under this service's own `InvalidChannelARN` with the sentence its model
/// documents, and a set carrying none is left to the constraint envelope.
///
/// Driven through the operation on one path and through the seam on the other, over a
/// table whose rows differ along what ELSE the violation set carries and in which order,
/// so a decision taken in only one of the two places shows up as the two paths answering
/// one fault differently.
#[tokio::test]
async fn one_decision_answers_a_channel_reference_fault_on_both_refusal_paths() {
    let harness = Harness::new();
    let registered = channel_arn("3c4d5e6f-7081-4293-a4b5-c6d7e8f9a0b1");
    harness.register(Channel::from_arn(&registered, "Custom").expect("a channel ARN"));

    // Path one: the wrapper that serves this operation. (case, channelArn, externalId,
    // the batch) — every row carries a reference outside `ChannelArn`'s expression, and
    // the rows differ along what else the same request broke.
    let through_the_operation: Vec<(&str, &str, Option<&str>, Vec<Ev>)> = vec![
        (
            "a reference outside the expression, nothing else wrong",
            "probe-value",
            None,
            vec![ev("evt-1", GOOD_EVENT)],
        ),
        (
            "and a batch under the member's length minimum",
            "not-an-arn",
            None,
            Vec::new(),
        ),
        (
            // `ExternalId` is `len 2..1224`
            // (`smithy_query(query="shape", shape="ExternalId")`).
            "and an external id under its length minimum",
            "channel-reference-that-is-no-arn",
            Some("x"),
            vec![ev("evt-1", GOOD_EVENT)],
        ),
        (
            // `ExternalId`'s own expression is `^[\w+=,.@:\/-]*$`, from the same shape.
            "and an external id outside its own expression",
            "",
            Some("bad~id"),
            vec![ev("evt-1", GOOD_EVENT)],
        ),
        (
            // `AuditEvent$id` is `pattern ^[-_A-Za-z0-9]+$`
            // (`smithy_query(query="shape", shape="AuditEvent")`).
            "and an event id outside its own expression",
            "3c4d5e6f-7081-4293-a4b5-c6d7e8f9a0b1",
            None,
            vec![ev("bad id!", GOOD_EVENT)],
        ),
        (
            // `AuditEvents` is `len 1..100`, from the same model.
            "and a batch past the member's length maximum",
            "https://cloudtrail.us-east-1.amazonaws.com/channel",
            None,
            (0..101)
                .map(|i| ev(format!("evt-{i}"), GOOD_EVENT))
                .collect(),
        ),
    ];

    let mut on_the_wire: Vec<(&str, WireAnswer)> = Vec::new();
    for (case, channel, external_id, events) in &through_the_operation {
        let reply = harness.put(channel, *external_id, events).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "InvalidChannelARN"),
            "case {case:?}: a channel reference fault is answered under this service's \
             own error, got {} with body {}",
            reply.status,
            reply.body
        );
        let stated = message_of(&reply);
        assert_eq!(
            stated, INVALID_CHANNEL_ARN,
            "case {case:?}: expected the documented sentence {INVALID_CHANNEL_ARN:?}, \
             got {stated:?}"
        );
        on_the_wire.push((
            case,
            WireAnswer {
                status: reply.status,
                code: reply.error_code(),
                message: stated,
            },
        ));
    }

    // Every row carried the SAME answer: the decision does not depend on what else the
    // request broke, so the rows are compared to one another and not only to the
    // expectation each was asserted against.
    let (first_case, first_answer) = on_the_wire
        .first()
        .cloned()
        .expect("the wrapper path answered at least one row");
    for (case, answered) in &on_the_wire[1..] {
        assert_eq!(
            *answered, first_answer,
            "case {case:?}: expected the answer case {first_case:?} took \
             ({first_answer:?}), got {answered:?}"
        );
    }

    // The same wrapper, on requests whose only faults are elsewhere: the decision is
    // keyed to the expression a channel reference is modelled with, not to any
    // `@pattern` fault, so these are the constraint envelope instead.
    let elsewhere: Vec<(&str, Option<&str>, Vec<Ev>)> = vec![
        ("an external id outside its own expression", Some("bad~id"), vec![ev("evt-1", GOOD_EVENT)]),
        ("an event id outside its own expression", None, vec![ev("bad id!", GOOD_EVENT)]),
        ("a batch under the member's length minimum", None, Vec::new()),
    ];
    for (case, external_id, events) in &elsewhere {
        let reply = harness.put(&registered, *external_id, events).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: a request with no channel reference fault is the constraint \
             envelope, got {} with body {}",
            reply.status,
            reply.body
        );
    }

    // Path two: the service's own refusal seam, which answers a refusal that reaches it
    // without passing the wrapper. It is handed the violations alone, so it is driven
    // with violation sets built the way the validating walk builds them.
    let channel_fault = violation(
        "channelArn",
        ViolationKind::Pattern(CHANNEL_ARN_PATTERN.to_string()),
        "probe-value",
    );
    let short_batch = violation("auditEvents", ViolationKind::MinLength(1), "0 items");
    let long_batch = violation("auditEvents", ViolationKind::MaxLength(100), "101 items");
    let external_id_fault = violation(
        "externalId",
        ViolationKind::Pattern(r"^[\w+=,.@:\/-]*$".to_string()),
        "bad~id",
    );
    let absent_reference = Violation {
        path: "channelArn".to_string(),
        kind: ViolationKind::MissingRequiredMember,
        value: None,
        sensitive: false,
    };

    // The seam's answer must be the answer the wire carried above, so take it from there
    // rather than restating it.
    let on_the_wire_answer = Some((first_answer.code, first_answer.message));
    let through_the_seam: Vec<SeamRow> = vec![
        SeamRow {
            case: "the channel reference fault alone",
            refusal: Refusal::Invalid(vec![channel_fault.clone()]),
            answer: on_the_wire_answer.clone(),
        },
        SeamRow {
            case: "another fault listed before it",
            refusal: Refusal::Invalid(vec![short_batch.clone(), channel_fault.clone()]),
            answer: on_the_wire_answer.clone(),
        },
        SeamRow {
            case: "another fault listed after it",
            refusal: Refusal::Invalid(vec![channel_fault.clone(), external_id_fault.clone()]),
            answer: on_the_wire_answer.clone(),
        },
        SeamRow {
            case: "several other faults around it",
            refusal: Refusal::Invalid(vec![
                long_batch.clone(),
                channel_fault,
                external_id_fault.clone(),
            ]),
            answer: on_the_wire_answer,
        },
        SeamRow {
            case: "another member's expression fault only",
            refusal: Refusal::Invalid(vec![external_id_fault]),
            answer: None,
        },
        SeamRow {
            case: "a length fault only",
            refusal: Refusal::Invalid(vec![short_batch, long_batch]),
            answer: None,
        },
        SeamRow {
            case: "the same member absent rather than outside its expression",
            refusal: Refusal::Invalid(vec![absent_reference]),
            answer: None,
        },
        SeamRow {
            case: "no violations at all",
            refusal: Refusal::Invalid(Vec::new()),
            answer: None,
        },
        SeamRow {
            case: "a caller the request authentication refused",
            refusal: Refusal::Unauthenticated,
            answer: None,
        },
    ];

    let service = EmulatedService::new(Arc::clone(harness.engine()));
    for row in &through_the_seam {
        let answered = service.override_refusal(&row.refusal);
        let stated = answered
            .as_ref()
            .map(|refused| (refused.code.clone(), refused.message.clone()));
        let case = row.case;
        assert_eq!(
            stated, row.answer,
            "case {case:?}: the seam must take the same decision the wrapper takes, got \
             {answered:?}"
        );
    }
}
