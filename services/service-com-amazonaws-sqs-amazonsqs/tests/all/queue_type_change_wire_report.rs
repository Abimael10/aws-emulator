// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for THE THREE FIELDS a refusal of a queue-type change states its class
//! in, driven over the wire (`POST /` with `X-Amz-Target: AmazonSQS.<Operation>`) through the
//! shared harness in [`super::support`].
//!
//! The rule under test: **a `SetQueueAttributes` naming the attribute that designates what
//! kind of queue a queue is states ONE class in all three places an answer states one — the
//! body's `__type` SHAPE ID, the body's `message`, and the legacy `x-amzn-query-error` report
//! beside the caller's fault — and the client's legacy opt-in decides none of them. The queue
//! that CARRIES the designator (a FIFO queue, which reports it) draws the value class, since
//! the kind is settled when the queue is made: `com.amazonaws.sqs#InvalidAttributeValue`,
//! `InvalidAttributeValue;Sender` and `Invalid value for the parameter <Attribute>. Reason:
//! Modifying queue type is not supported.` A queue that carries no such attribute at all draws
//! the name class: `com.amazonaws.sqs#InvalidAttributeName`, `InvalidAttributeName;Sender` and
//! `Unknown Attribute <Attribute>.` The attribute in the sentence is the one the REQUEST
//! spelled.**
//!
//! [`super::queue_type_immutability`] owns the rule itself — every value the designator may
//! arrive with, every map it may arrive in, and what a refused update leaves the queue
//! reporting — and [`super::unknown_attribute_names`] owns the sibling names that draw the name
//! class. This suite adds only what neither asserts: the three fields of one answer as a single
//! report, over both queue kinds and over whether the client sent the legacy opt-in at all, so
//! a drift that moved one of them (the body's discriminator, say) while leaving the others
//! would fail here. A query-era SDK reads the header, a native JSON client reads the
//! discriminator, and a client matching the `Reason` clause reads the sentence: the three have
//! to agree.

use serde_json::{json, Value};

use super::support::{
    call_with_query_mode, create_ok, emulator, error_shape, fifo_attributes, header_of, message_of,
    QUERY_ERROR_HEADER, QUERY_MODE_ON,
};

/// The namespace this service's model declares its shapes in (`smithy_query(
/// query="service_info",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`:
/// service shape `com.amazonaws.sqs#AmazonSQS`), which is the package a body names one of its
/// error classes under.
const NAMESPACE: &str = "com.amazonaws.sqs";

/// The attribute that designates a queue as FIFO, and the one value that designates it:
/// "Designates a queue as FIFO. ... You can provide this attribute only during queue creation.
/// You can't change it for an existing queue" (`FifoQueue`'s own documentation in
/// `smithy_query(query="operation",operation="CreateQueue",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`).
/// Spelled out here rather than imported, so a wrong constant in the service cannot make a row
/// pass.
const DESIGNATOR: &str = "FifoQueue";
const DESIGNATES: &str = "true";

/// The two modelled attribute-refusal classes, by their local shape names.
const INVALID_ATTRIBUTE_VALUE: &str = "InvalidAttributeValue";
const INVALID_ATTRIBUTE_NAME: &str = "InvalidAttributeName";

/// The fault side the legacy report names for a refusal the caller caused.
const SENDER_FAULT: &str = "Sender";

/// The one sentence Amazon SQS states the queue-type rule with, spelled out here rather than
/// imported: the fixed `Reason` text of the refusal of a change no queue admits.
const QUEUE_TYPE_NOT_MODIFIABLE: &str = "Modifying queue type is not supported.";

/// The whole wire report of one refusal: its status, the body's `__type`, its `message` and the
/// legacy header beside them.
type Report = (u16, String, String, Option<String>);

/// The report a refusal of modelled class `local` owes, stating `message`: the status, the shape
/// id the body names the class under, the sentence, and the legacy code beside the fault.
fn report(local: &str, message: String) -> Report {
    (
        400,
        format!("{NAMESPACE}#{local}"),
        message,
        Some(format!("{local};{SENDER_FAULT}")),
    )
}

/// What a call naming the designator on a queue of this kind owes, in all three fields.
fn expected(fifo: bool) -> Report {
    if fifo {
        report(
            INVALID_ATTRIBUTE_VALUE,
            format!("Invalid value for the parameter {DESIGNATOR}. Reason: {QUEUE_TYPE_NOT_MODIFIABLE}"),
        )
    } else {
        report(
            INVALID_ATTRIBUTE_NAME,
            format!("Unknown Attribute {DESIGNATOR}."),
        )
    }
}

/// Every way a client spells the legacy opt-in that matters to an error answer: not at all (a
/// native JSON client) and the way a query-compatible SDK sends it. Nothing about the answer may
/// key on it.
const OPT_INS: &[(&str, Option<&str>)] = &[
    ("a native JSON client", None),
    ("a query-compatible SDK", Some(QUERY_MODE_ON)),
];

/// Three values the designator arrives with, spanning the change it can ask a queue of either
/// kind for: the one that designates (no change at all for a FIFO queue), its opposite, and text
/// the attribute has no meaning for. The answer is the same for each, because what such a call
/// asks for is decided by the attribute it named.
const VALUES: &[(&str, &str)] = &[
    ("the designating value", DESIGNATES),
    ("the opposite boolean", "false"),
    ("a word outside the attribute's vocabulary", "yes"),
];

/// The answer to one update, as the four fields that carry its class.
async fn answered(
    router: &smithy_server_runtime::MultiServiceRouter,
    url: &str,
    attributes: Value,
    opt_in: Option<&str>,
) -> (Report, Value) {
    let (status, headers, parsed) = call_with_query_mode(
        router,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": attributes}),
        opt_in,
    )
    .await;
    (
        (
            status,
            error_shape(&parsed),
            message_of(&parsed),
            header_of(&headers, QUERY_ERROR_HEADER),
        ),
        parsed,
    )
}

/// The rule: one class in the discriminator, in the sentence and in the legacy report — the value
/// class on the queue that carries the designator and the name class on the one that does not —
/// whatever value the call supplied and whatever the client said about the opt-in.
#[tokio::test]
async fn a_refused_queue_type_change_states_one_class_in_all_three_fields() {
    let router = emulator();
    let queues = [
        (
            "a standard queue",
            false,
            create_ok(&router, "type-report-standard", &json!({})).await,
        ),
        (
            "a FIFO queue",
            true,
            create_ok(&router, "type-report-queue.fifo", &fifo_attributes(&[])).await,
        ),
    ];
    for (kind, fifo, url) in &queues {
        let owed = expected(*fifo);
        for (value_case, value) in VALUES {
            for (client_case, opt_in) in OPT_INS {
                let case = format!("{kind} with {value_case} ({value:?}), as {client_case}");
                let (got, parsed) = answered(
                    &router,
                    url,
                    json!({ DESIGNATOR.to_string(): value.to_string() }),
                    *opt_in,
                )
                .await;
                assert_eq!(
                    got, owed,
                    "case {case}: expected the report {owed:?}, got {got:?} with body {parsed}"
                );
            }
        }
    }

    // The attribute the sentence names is the one the REQUEST spelled: another casing of the
    // designator is a name no call of this API has, on either kind of queue, and it is echoed
    // exactly as it arrived rather than in the spelling the service knows.
    let other_casing = DESIGNATOR.to_lowercase();
    for (kind, _, url) in &queues {
        let owed = report(
            INVALID_ATTRIBUTE_NAME,
            format!("Unknown Attribute {other_casing}."),
        );
        let (got, parsed) = answered(
            &router,
            url,
            json!({ other_casing.clone(): DESIGNATES }),
            Some(QUERY_MODE_ON),
        )
        .await;
        assert_eq!(
            got, owed,
            "case {kind} with the designator in another casing ({other_casing:?}): expected the \
             report {owed:?}, got {got:?} with body {parsed}"
        );
    }
}
