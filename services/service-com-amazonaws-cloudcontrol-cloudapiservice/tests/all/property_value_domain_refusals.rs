// SPDX-License-Identifier: CC0-1.0

//! WHICH `HandlerErrorCode` a value outside a property's published domain is refused under.
//!
//! The rule every case here holds the service to: **a value the `DesiredState` supplies for a
//! property of the resource type that lies outside the DOMAIN that type's schema publishes for
//! the property — the range of a count, the two values of a flag, the set of a choice — is
//! invalid input from the user: the request terminates `FAILED` with `ProgressEvent.ErrorCode`
//! `InvalidRequest`, "invalid input from the user has generated a generic exception"
//! (`smithy_query(query="doc",shape="InvalidRequestException")`), and leaves no resource
//! behind. `GeneralServiceException`, "the downstream service generated an error that doesn't
//! map to any other handler error code"
//! (`smithy_query(query="doc",shape="GeneralServiceException")`), is for a refusal the model's
//! own values do not settle — a name the owning service holds to a charset of its own, an
//! attribute no queue of that kind has.**
//!
//! Nothing of the store decides such a refusal: the one value describes no resource of the type
//! at all, so a client reading `ErrorCode` is told to repair its model rather than to retry an
//! opaque downstream fault.
//!
//! The rows differ along what the rule generalises over: which property carries the value,
//! which KIND of domain it breaks (a numeric range, a boolean's two values, a documented value
//! set), which END of a range it falls past, which kind of queue the model describes, and
//! whether the value arrives on a create or on an update. The controls are each published bound
//! itself, which is accepted and reads back, and the refusals the model's values do not settle,
//! which keep the downstream code.
//!
//! Catalog source: the `Attributes` documentation of `CreateQueue`, which is what the
//! `AWS::SQS::Queue` schema publishes with each of its attribute properties —
//! `DelaySeconds` "An integer from 0 to 900 seconds (15 minutes)", `VisibilityTimeout` "An
//! integer from 0 to 43,200 (12 hours)", `MessageRetentionPeriod` "An integer from 60 seconds
//! (1 minute) to 1,209,600 seconds (14 days)", `MaximumMessageSize` "An integer from 1,024
//! bytes (1 KiB) to 1,048,576 bytes (1 MiB)", `ReceiveMessageWaitTimeSeconds` "An integer from
//! 0 to 20 (seconds)", `KmsDataKeyReusePeriodSeconds` "between 60 seconds (1 minute) and 86,400
//! seconds (24 hours)", `ContentBasedDeduplication` and `SqsManagedSseEnabled` "Valid values are
//! true and false", `DeduplicationScope` "Valid values are messageGroup and queue" and
//! `FifoThroughputLimit` "Valid values are perQueue and perMessageGroupId".

use serde_json::{json, Value};

use super::emulator::{progress, text, Emulator, STATUS_FAILED, STATUS_SUCCESS};
use super::sqs_queue_read_through::{queue_url, QUEUE};

/// The handler code for invalid input from the user.
const MODEL_FAULT: &str = "InvalidRequest";

/// The handler code for an error the owning service raised that the handler maps to no verdict
/// of its own.
const DOWNSTREAM: &str = "GeneralServiceException";

/// One property's published range: the property, its smallest value and its largest.
///
/// Catalog source: this module's header — each bound is the one that property's own entry in the
/// `Attributes` documentation publishes.
struct Range {
    /// The property that carries the count.
    property: &'static str,
    /// The smallest value it admits.
    min: i64,
    /// The largest value it admits.
    max: i64,
}

/// Every attribute property of `AWS::SQS::Queue` whose domain is a RANGE, with its bounds.
const RANGES: &[Range] = &[
    Range {
        property: "DelaySeconds",
        min: 0,
        max: 900,
    },
    Range {
        property: "VisibilityTimeout",
        min: 0,
        max: 43_200,
    },
    Range {
        property: "MessageRetentionPeriod",
        min: 60,
        max: 1_209_600,
    },
    Range {
        property: "MaximumMessageSize",
        min: 1_024,
        max: 1_048_576,
    },
    Range {
        property: "ReceiveMessageWaitTimeSeconds",
        min: 0,
        max: 20,
    },
    Range {
        property: "KmsDataKeyReusePeriodSeconds",
        min: 60,
        max: 86_400,
    },
];

/// The sentence a refused value carries: the owning service's fixed text for a value a
/// parameter does not accept, naming the property at fault and nothing else.
fn invalid_value(property: &str) -> String {
    format!("Invalid value for the parameter {property}.")
}

/// A desired state for a queue called `name` carrying `property` set to `value`, of the kind of
/// queue that HAS that property.
fn desired(name: &str, property: &str, value: Value, fifo: bool) -> Value {
    let mut model = json!({ "QueueName": name });
    if fifo {
        model["FifoQueue"] = json!(true);
    }
    model[property] = value;
    model
}

#[tokio::test]
async fn a_value_past_either_end_of_a_published_range_fails_as_invalid_input() {
    let emulator = Emulator::with_sqs();

    // Two rows per property: one below its smallest value and one above its largest, so the
    // rule is asserted at BOTH ends of every range the type publishes rather than at the one
    // end an example happens to break.
    let mut rows: Vec<(String, String, &'static str, Value)> = Vec::new();
    for (row, range) in RANGES.iter().enumerate() {
        rows.push((
            format!("{} below its smallest value", range.property),
            format!("domain-under-{row}"),
            range.property,
            json!(range.min - 1),
        ));
        rows.push((
            format!("{} above its largest value", range.property),
            format!("domain-over-{row}"),
            range.property,
            json!(range.max + 1),
        ));
    }

    // Started together, then waited for: the creates are independent, so one span covers them
    // all.
    let mut accepted = Vec::new();
    for (case, name, property, value) in &rows {
        let model = desired(name, property, value.clone(), false);
        let (status, body) = emulator.create(QUEUE, &model, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the type's schema publishes no row this front door judges, so the \
             call must be accepted, got {status} with body {body:?}"
        );
        accepted.push((
            case.clone(),
            name.clone(),
            *property,
            text(&progress(&body), "RequestToken"),
        ));
    }

    for (case, name, property, request_token) in accepted {
        let settled = emulator.terminal(&request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: a value outside the property's domain must not be created, got \
             {settled:?}"
        );
        assert_eq!(
            text(&settled, "ErrorCode"),
            MODEL_FAULT,
            "case {case:?}: expected the {MODEL_FAULT} handler code, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "StatusMessage"),
            invalid_value(property),
            "case {case:?}: expected the refusal to name {property:?}, got {settled:?}"
        );
        // Nothing was created, in either store: a refused create leaves no resource behind.
        let (status, body) = emulator.get(QUEUE, &queue_url(&name)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must leave no queue, got {status} with body \
             {body:?}"
        );
    }
}

#[tokio::test]
async fn each_published_bound_is_accepted_and_reads_back() {
    let emulator = Emulator::with_sqs();

    // The control for the rule above, at the same bounds: the value ON each published bound is
    // inside the domain, so the create succeeds and the queue reads back carrying it.
    let mut rows: Vec<(String, String, &'static str, i64)> = Vec::new();
    for (row, range) in RANGES.iter().enumerate() {
        rows.push((
            format!("{} at its smallest value", range.property),
            format!("domain-min-{row}"),
            range.property,
            range.min,
        ));
        rows.push((
            format!("{} at its largest value", range.property),
            format!("domain-max-{row}"),
            range.property,
            range.max,
        ));
    }

    let mut accepted = Vec::new();
    for (case, name, property, value) in &rows {
        let model = desired(name, property, json!(value), false);
        let event = emulator.create_ok(QUEUE, &model, None).await;
        accepted.push((
            case.clone(),
            name.clone(),
            *property,
            *value,
            text(&event, "RequestToken"),
        ));
    }

    for (case, name, property, value, request_token) in accepted {
        let settled = emulator.terminal(&request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: a value on a published bound must be created, got {settled:?}"
        );
        let model = emulator.properties(QUEUE, &queue_url(&name)).await;
        assert_eq!(
            model[property],
            json!(value),
            "case {case:?}: expected the queue to carry {value} for {property:?}, got {model:?}"
        );
    }
}

#[tokio::test]
async fn a_value_outside_a_published_set_or_a_flags_two_values_fails_as_invalid_input() {
    let emulator = Emulator::with_sqs();

    // The rows differ along the KIND of domain the value breaks and the kind of queue the
    // property belongs to: a documented value set and a flag's two values, on a FIFO queue for
    // the properties only that kind of queue has and on a standard one for a property every
    // queue has.
    let rows: [(&str, &str, &str, Value, bool); 4] = [
        (
            "a deduplication scope outside the set published for it",
            "domain-scope.fifo",
            "DeduplicationScope",
            json!("everywhere"),
            true,
        ),
        (
            "a throughput limit outside the set published for it",
            "domain-throughput.fifo",
            "FifoThroughputLimit",
            json!("perEverything"),
            true,
        ),
        (
            "a flag of a FIFO queue carrying neither of its two values",
            "domain-dedup.fifo",
            "ContentBasedDeduplication",
            json!("maybe"),
            true,
        ),
        (
            "a flag every queue has carrying neither of its two values",
            "domain-sse",
            "SqsManagedSseEnabled",
            json!("maybe"),
            false,
        ),
    ];

    let mut accepted = Vec::new();
    for (case, name, property, value, fifo) in rows {
        let model = desired(name, property, value, fifo);
        let (status, body) = emulator.create(QUEUE, &model, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the call must be accepted, got {status} with body {body:?}"
        );
        accepted.push((case, name, property, text(&progress(&body), "RequestToken")));
    }

    for (case, name, property, request_token) in accepted {
        let settled = emulator.terminal(&request_token).await;
        assert_eq!(
            (
                text(&settled, "OperationStatus"),
                text(&settled, "ErrorCode"),
                text(&settled, "StatusMessage"),
            ),
            (
                STATUS_FAILED.to_string(),
                MODEL_FAULT.to_string(),
                invalid_value(property),
            ),
            "case {case:?}: expected the {MODEL_FAULT} handler code naming {property:?}, got \
             {settled:?}"
        );
        let (status, body) = emulator.get(QUEUE, &queue_url(name)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must leave no queue, got {status} with body \
             {body:?}"
        );
    }
}

#[tokio::test]
async fn a_refusal_the_models_values_do_not_settle_keeps_the_downstream_code() {
    let emulator = Emulator::with_sqs();

    // The other side of the rule: a refusal the one value does not settle is the owning
    // service's error, whichever rule of its own refused the call — a name it holds to a
    // charset of its own, and an attribute no queue of this kind has at all, which follows from
    // the KIND of queue rather than from the value.
    let rows: [(&str, Value); 2] = [
        (
            "a name carrying a character the owning service's charset excludes",
            json!({ "QueueName": "domain name!" }),
        ),
        (
            "a flag that applies to no queue of this kind",
            json!({ "QueueName": "domain-standard", "ContentBasedDeduplication": true }),
        ),
    ];

    let mut accepted = Vec::new();
    for (case, model) in rows {
        let (status, body) = emulator.create(QUEUE, &model, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the call must be accepted, got {status} with body {body:?}"
        );
        accepted.push((case, text(&progress(&body), "RequestToken")));
    }

    for (case, request_token) in accepted {
        let settled = emulator.terminal(&request_token).await;
        assert_eq!(
            (
                text(&settled, "OperationStatus"),
                text(&settled, "ErrorCode"),
            ),
            (STATUS_FAILED.to_string(), DOWNSTREAM.to_string()),
            "case {case:?}: expected the {DOWNSTREAM} handler code, got {settled:?}"
        );
    }
}

#[tokio::test]
async fn a_patch_whose_value_leaves_a_published_domain_fails_as_invalid_input() {
    let emulator = Emulator::with_sqs();
    let identifier = emulator
        .provision(QUEUE, &json!({ "QueueName": "domain-patched" }))
        .await;
    let before = emulator.properties(QUEUE, &identifier).await;

    // The same rule on the UPDATE path, where the value arrives in a patch rather than in a
    // desired state: the rows differ along the property and the end of its range, and each
    // leaves the queue exactly as it was.
    let rows: [(&str, &str, Value); 3] = [
        (
            "a count patched above its largest value",
            "VisibilityTimeout",
            json!(43_201),
        ),
        (
            "a count patched below its smallest value",
            "MessageRetentionPeriod",
            json!(59),
        ),
        (
            "a flag patched to neither of its two values",
            "SqsManagedSseEnabled",
            json!("maybe"),
        ),
    ];

    for (case, property, value) in rows {
        let patch = json!([{ "op": "add", "path": format!("/{property}"), "value": value }]);
        let (status, accepted) = emulator.update(QUEUE, &identifier, &patch, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the patch must be accepted, got {status} with body {accepted:?}"
        );
        let settled = emulator
            .terminal(&text(&progress(&accepted), "RequestToken"))
            .await;
        assert_eq!(
            (
                text(&settled, "OperationStatus"),
                text(&settled, "ErrorCode"),
                text(&settled, "StatusMessage"),
            ),
            (
                STATUS_FAILED.to_string(),
                MODEL_FAULT.to_string(),
                invalid_value(property),
            ),
            "case {case:?}: expected the {MODEL_FAULT} handler code naming {property:?}, got \
             {settled:?}"
        );
        let after = emulator.properties(QUEUE, &identifier).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused patch must leave the queue as it was, got {after:?}"
        );
    }
}
