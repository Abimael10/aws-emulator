// SPDX-License-Identifier: CC0-1.0

//! The JSONata a subscriber's own configuration carries, evaluated over the
//! event being delivered.
//!
//! Several members of a subscriber are "a literal or a JSONata expression (e.g.
//! `{% $events.Data.groupId %}`)" — `SqsParameters`' and `SnsParameters`' scalar
//! values, an `SqsMessageAttributeValue$StringValue` ("a JSONata expression
//! resolves once per delivered event"), the `EventBusV2Parameters` metadata a
//! forwarded event carries — and a `Transformer` of type `JSONATA` carries one
//! as its whole transformation (`JsonataConfiguration$Expression`, "JSONata
//! expression to transform the event. Must be wrapped in {% %} delimiters").
//! So one reading serves them all: a value written inside those delimiters is
//! EVALUATED over the event, and every other value is the literal it is written
//! as.
//!
//! The expression is a caller's own text, so it is evaluated through
//! [`::service_common::jsonata`] — this workspace's one bounded seam onto the
//! maintained JSONata interpreter — rather than by a hand-written subset of the
//! language. Every ceiling that confines an engine evaluating caller-written
//! text (the text, its parse, the stack it runs on, its depth, its memory, how
//! many evaluations are alive at once) is stated and met there; what this module
//! adds is this service's own half: what an expression READS, how long one
//! delivery may spend on it, and how the value it produced becomes the TEXT a
//! delivery carries.
//!
//! Nothing here writes to the store, opens a socket or reads a host file: it
//! computes one value out of the event it is handed.

use super::subscribers::DeliveredEvent;

/// What opens one expression, per `JsonataConfiguration$Expression`'s own
/// pattern (`^\{%[\s\S]+?%\}$`).
pub const EXPRESSION_OPEN: &str = "{%";

/// What closes it.
pub const EXPRESSION_CLOSE: &str = "%}";

/// The term an expression reads the event through: `$events`, as the example in
/// `SqsParameters`' own documentation writes it (`{% $events.Data.groupId %}`).
///
/// It is bound to the event as this service models one — its `Data`, the
/// publisher's `Metadata` and its `SystemMetadata`
/// ([`DeliveredEvent::document`]) — which is also the expression's own context
/// item (`$`), so a bare path reads the same document the term does.
const EVENTS_TERM: &str = "events";

/// The longest ONE expression of one delivery is evaluated for, in
/// milliseconds.
///
/// EMULATOR bound, not an AWS limit: a delivery evaluates an expression per
/// configured value per event, and the publish that makes the delivery due is a
/// request with a client waiting on it, so one evaluation is bounded here and
/// the engine's own clock stops it.
pub const EVALUATION_TIME_LIMIT_MS: usize = 250;

/// How many expressions the deliveries of ONE call may evaluate.
///
/// EMULATOR bound: what one publish evaluates is the PRODUCT of how many events
/// it accepted (`PutEventsRequestEntryList` carries up to 100), how many
/// subscribers its bus holds and how many configured values each of them writes
/// an expression in (`SqsParameters` admits 50 message attributes alone). None of
/// those factors bounds the product, so the product is bounded here, for the
/// whole call rather than per event.
pub const MAX_EVALUATIONS_PER_CALL: usize = 2048;

/// How long the expressions of ONE call may run for, together, in milliseconds.
///
/// EMULATOR bound, for the same reason: the per-expression ceiling above bounds
/// one of them, and this bounds what they take together, so a publish cannot
/// hold the client that made it for the sum of its deliveries' evaluations.
pub const MAX_EVALUATION_MS_PER_CALL: usize = 2_000;

/// What the expressions of ONE call may spend: how many of them may be
/// evaluated, and how long they may take together.
///
/// Held by the delivery that owns the call and charged as each expression is
/// evaluated, so neither the number of configured expressions nor what they cost
/// is left to the configuration. A delivery past the budget carries no value for
/// the expression it could not evaluate, exactly as one whose expression
/// produced nothing does.
#[derive(Debug)]
pub struct EvaluationBudget {
    evaluations: usize,
    allowed_ms: usize,
}

impl Default for EvaluationBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl EvaluationBudget {
    /// The allowance one call gets.
    pub fn new() -> Self {
        Self {
            evaluations: MAX_EVALUATIONS_PER_CALL,
            allowed_ms: MAX_EVALUATION_MS_PER_CALL,
        }
    }

    /// An allowance already spent, which evaluates nothing further: what a
    /// caller hands on when the work it was doing did not finish, so a call
    /// that failed to resolve one window does not go on resolving others.
    pub fn spent() -> Self {
        Self {
            evaluations: 0,
            allowed_ms: 0,
        }
    }

    /// Charge ONE evaluation, and answer how many milliseconds it may run for:
    /// its own ceiling, or what is left of the call's, whichever is less.
    ///
    /// The `Err` is a call that has spent its allowance, which is why it says so
    /// rather than naming a fault of the expression.
    fn charge(&mut self) -> Result<usize, String> {
        let Some(left) = self.evaluations.checked_sub(1) else {
            return Err(format!(
                "these deliveries asked for more than the {MAX_EVALUATIONS_PER_CALL} expressions \
                 this emulator evaluates for one call"
            ));
        };
        self.evaluations = left;
        if self.allowed_ms == 0 {
            return Err(format!(
                "these deliveries' expressions asked for more than the \
                 {MAX_EVALUATION_MS_PER_CALL} milliseconds this emulator evaluates for one call"
            ));
        }
        Ok(self.allowed_ms.min(EVALUATION_TIME_LIMIT_MS))
    }

    /// Spend what one evaluation TOOK, whether it produced a value or not: the
    /// span it ran for is work the call did.
    fn spend(&mut self, took: ::std::time::Duration) {
        let took = usize::try_from(took.as_millis()).unwrap_or(usize::MAX);
        self.allowed_ms = self.allowed_ms.saturating_sub(took);
    }
}

/// The most bytes the value an expression produces may serialize to when it is
/// the whole BODY of a delivery.
///
/// A delivered body travels as one message on the target
/// (`SqsParameters`: "values are forwarded to the SQS `SendMessageBatch` API"),
/// and 262,144 bytes is the most one Amazon SQS message carries
/// (`com.amazonaws.sqs#InvalidMessageContents`' own 256 KiB limit), so a
/// transformation larger than that could not be delivered by any target this
/// service calls.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// The most bytes the value an expression produces may serialize to when it
/// fills one configured SCALAR.
///
/// The members that carry such an expression are modelled `@length max 8192`
/// (`SqsParameters$MessageGroupId`, `$MessageDeduplicationId`, `$DelaySeconds`),
/// so a value past that is not one the member could have been written with
/// either.
pub const MAX_SCALAR_BYTES: usize = 8192;

/// Whether `text` is ONE whole expression — the only form a configured value is
/// evaluated in, since the language delimits one with [`EXPRESSION_OPEN`] and
/// [`EXPRESSION_CLOSE`] and a value carrying anything else is the literal it is
/// written as.
pub fn is_expression(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.len() > EXPRESSION_OPEN.len() + EXPRESSION_CLOSE.len()
        && trimmed.starts_with(EXPRESSION_OPEN)
        && trimmed.ends_with(EXPRESSION_CLOSE)
}

/// The expression inside the delimiters of `text`, for a `text`
/// [`is_expression`] answers for — and the text itself for one it does not, so
/// no reading of a caller's value can depend on an index into it.
fn inner(text: &str) -> &str {
    let trimmed = text.trim();
    trimmed
        .strip_prefix(EXPRESSION_OPEN)
        .and_then(|inside| inside.strip_suffix(EXPRESSION_CLOSE))
        .unwrap_or(trimmed)
        .trim()
}

/// The TEXT a configured value carries for `event`: what its expression
/// produced when it is one, and the value itself when it is a literal.
///
/// `Ok(None)` is an expression that produced NOTHING — the language's own
/// `undefined` — so the member it was written in carries no value, exactly as if
/// the subscriber had configured none. The `Err` is why nothing was produced,
/// for the caller to report.
pub fn configured_text(
    configured: &str,
    event: &DeliveredEvent,
    allowance: usize,
    budget: &mut EvaluationBudget,
) -> Result<Option<String>, String> {
    if !is_expression(configured) {
        return Ok(Some(configured.to_string()));
    }
    evaluated_text(configured, event, allowance, budget)
}

/// The TEXT the expression `configured` produces over `event`.
///
/// A value the expression produced becomes text the way the language's own types
/// map onto one: a STRING is the characters it holds, unquoted, because a string
/// is already text and quoting it would deliver a JSON document where the
/// expression produced a value; every other value — an object, an array, a
/// number, a boolean — is its JSON serialization, which is the one text such a
/// value has.
///
/// The evaluation is charged to `budget` BEFORE it is made — which is also what
/// hands the seam the time it may run for — and what it TOOK is spent from the
/// same budget, so the expressions of one call are bounded in number and in
/// wall clock together.
pub fn evaluated_text(
    configured: &str,
    event: &DeliveredEvent,
    allowance: usize,
    budget: &mut EvaluationBudget,
) -> Result<Option<String>, String> {
    let allowed_ms = budget.charge()?;
    let document = event.document();
    let began = ::std::time::Instant::now();
    let produced = ::service_common::jsonata::evaluate(&::service_common::jsonata::Expression {
        text: inner(configured),
        input: &document,
        variables: &[(EVENTS_TERM, &document)],
        allowed_ms,
        allowance,
    });
    budget.spend(began.elapsed());
    Ok(produced.map_err(refusal)?.map(|(value, _)| as_text(value)))
}

/// One produced value as the text a delivery carries ([`evaluated_text`]).
///
/// A document's members are serialized IN KEY ORDER, so one value has one
/// text: the order the engine happens to hold an object's members in is no part
/// of the value it produced, and two deliveries of the same value must be the
/// same bytes — which is what lets a client compare them, and what makes a
/// content-based deduplication of a forwarded event
/// ([`super::ForwardedEvent`]) decide the same way twice.
fn as_text(value: ::serde_json::Value) -> String {
    match value {
        ::serde_json::Value::String(text) => text,
        other => in_key_order(other, ::service_common::jsonata::MAX_EVALUATION_DEPTH).to_string(),
    }
}

/// `value` with the members of every object in it in key order.
///
/// `depth` bounds the walk at the depth the seam evaluates over, which is the
/// deepest value an expression can have produced, so this cannot recurse past
/// what was already admitted.
fn in_key_order(value: ::serde_json::Value, depth: usize) -> ::serde_json::Value {
    let Some(depth) = depth.checked_sub(1) else {
        return value;
    };
    match value {
        ::serde_json::Value::Object(members) => {
            let mut ordered: Vec<(String, ::serde_json::Value)> = members.into_iter().collect();
            ordered.sort_by(|(left, _), (right, _)| left.cmp(right));
            ::serde_json::Value::Object(
                ordered
                    .into_iter()
                    .map(|(name, value)| (name, in_key_order(value, depth)))
                    .collect(),
            )
        }
        ::serde_json::Value::Array(items) => ::serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| in_key_order(item, depth))
                .collect(),
        ),
        other => other,
    }
}

/// Why an expression produced no value, as a log reports it: the seam's own
/// account of the refusal, which is either the engine's diagnosis of the
/// caller's text or the ceiling the evaluation reached.
fn refusal(refused: ::service_common::jsonata::Refused) -> String {
    match refused {
        ::service_common::jsonata::Refused::TooLarge(why)
        | ::service_common::jsonata::Refused::Threw(why)
        | ::service_common::jsonata::Refused::NotEvaluated(why) => why,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        configured_text, is_expression, EvaluationBudget, MAX_BODY_BYTES,
        MAX_EVALUATIONS_PER_CALL, MAX_SCALAR_BYTES,
    };
    use crate::state::{EventPayload, StoredEvent, StructuredPayload};

    /// The event every row below is evaluated over: one structured event, as a
    /// subscriber of its bus sees it.
    fn event() -> crate::state::DeliveredEvent {
        let subscriber = crate::state::test_subscriber();
        let stored = StoredEvent {
            event_id: "e-1".to_string(),
            sequence_number: 7,
            payload: EventPayload::Structured(StructuredPayload {
                source: "orders.service".to_string(),
                detail_type: "order.created".to_string(),
                detail: Some(r#"{"orderId":"A-1","quantity":3}"#.to_string()),
                resources: vec!["arn:aws:s3:::orders".to_string()],
            }),
            time: ::std::time::SystemTime::UNIX_EPOCH,
            received_at: ::std::time::SystemTime::UNIX_EPOCH,
            deduplication_id: None,
            deduplication_key: None,
            event_group_id: Some("group-1".to_string()),
        };
        crate::state::delivered_event(&subscriber, &stored)
    }

    /// Whether a value is one whole expression is decided by the delimiters the
    /// language writes one in, and nothing else.
    #[test]
    fn one_expression_is_what_the_delimiters_enclose() {
        // (case, the configured text, whether it is one expression)
        let rows: [(&str, &str, bool); 7] = [
            ("an expression", "{% $events.Data.detail.orderId %}", true),
            ("surrounding whitespace", "  {% 1 + 1 %}  ", true),
            ("a literal", "group-1", false),
            ("a literal that merely opens one", "{% group-1", false),
            ("a literal that merely closes one", "group-1 %}", false),
            ("empty delimiters", "{%%}", false),
            ("no text at all", "", false),
        ];
        for (case, text, expected) in rows {
            let read = is_expression(text);
            assert_eq!(
                read, expected,
                "case {case:?}: expected is_expression({text:?}) to be {expected}, got {read}"
            );
        }
    }

    /// RULE: a configured value written inside the delimiters is evaluated over
    /// the event and becomes the text its value has — a string unquoted, every
    /// other value its JSON serialization — and a value written outside them is
    /// the literal it is written as.
    ///
    /// The rows differ along the TYPE the expression produces (string, object,
    /// array, number, boolean, nothing), along the part of the event it reads
    /// (`Data`, `Metadata`, `SystemMetadata`, the context item) and along
    /// whether the value is an expression at all.
    #[test]
    fn a_configured_value_is_its_expressions_text_or_its_own() {
        // (case, the configured value, the text it carries)
        let rows: [(&str, &str, Option<&str>); 11] = [
            ("a literal", "group-1", Some("group-1")),
            (
                "a string out of the payload, unquoted",
                "{% $events.Data.detail.orderId %}",
                Some("A-1"),
            ),
            (
                "a string read through the context item",
                "{% Data.detail.orderId %}",
                Some("A-1"),
            ),
            (
                "a number, serialized",
                "{% $events.Data.detail.quantity %}",
                Some("3"),
            ),
            (
                "an object, serialized",
                "{% $events.Data.detail %}",
                Some(r#"{"orderId":"A-1","quantity":3}"#),
            ),
            (
                "an array, serialized",
                "{% [$events.Data.detail.orderId] %}",
                Some(r#"["A-1"]"#),
            ),
            (
                "a boolean, serialized",
                "{% $events.Data.detail.quantity > 2 %}",
                Some("true"),
            ),
            (
                "the publisher's metadata",
                "{% $events.Metadata.Source %}",
                Some("orders.service"),
            ),
            (
                "the event's system metadata",
                "{% $events.SystemMetadata.EventGroupId %}",
                Some("group-1"),
            ),
            (
                "a computed string",
                "{% 'order-' & $events.Data.detail.orderId %}",
                Some("order-A-1"),
            ),
            // The language's `undefined`: the member carries no value at all.
            (
                "a path the event does not hold",
                "{% $events.Data.detail.missing %}",
                None,
            ),
        ];
        let event = event();
        let mut budget = EvaluationBudget::new();
        for (case, configured, expected) in rows {
            let carried = configured_text(configured, &event, MAX_BODY_BYTES, &mut budget);
            let carried = carried
                .as_ref()
                .map(|text| text.as_deref())
                .unwrap_or_else(|why| panic!("case {case:?}: expected {expected:?}, got {why}"));
            // A document's text is compared AS a document: the order a JSON
            // object's members are serialized in is no part of the value the
            // expression produced.
            match (
                carried.and_then(|text| ::serde_json::from_str::<::serde_json::Value>(text).ok()),
                expected.and_then(|text| ::serde_json::from_str::<::serde_json::Value>(text).ok()),
            ) {
                (Some(carried_value), Some(expected_value)) => assert_eq!(
                    carried_value, expected_value,
                    "case {case:?}: expected {expected:?}, got {carried:?}"
                ),
                _ => assert_eq!(
                    carried, expected,
                    "case {case:?}: expected {expected:?}, got {carried:?}"
                ),
            }
        }
    }

    /// RULE: one value has ONE text — a document an expression produced is
    /// serialized with the members of every object in it in key order — so the
    /// same value delivered twice is the same bytes however the engine happens
    /// to hold it.
    #[test]
    fn a_produced_document_is_serialized_in_key_order() {
        let event = event();
        let mut budget = EvaluationBudget::new();
        // (case, the expression, the one text its value has)
        let rows: [(&str, &str, &str); 3] = [
            (
                "an object of the event's own payload",
                "{% $events.Data.detail %}",
                r#"{"orderId":"A-1","quantity":3}"#,
            ),
            (
                "an object the expression builds, whose members it writes out of order",
                r#"{% { "zebra": 1, "alpha": 2 } %}"#,
                r#"{"alpha":2,"zebra":1}"#,
            ),
            (
                "an object inside an array",
                r#"{% [{ "zebra": 1, "alpha": 2 }] %}"#,
                r#"[{"alpha":2,"zebra":1}]"#,
            ),
        ];
        for (case, configured, expected) in rows {
            // Twice: the same value is the same bytes both times.
            for pass in 0..2 {
                let carried = configured_text(configured, &event, MAX_BODY_BYTES, &mut budget);
                assert_eq!(
                    carried.as_ref().map(|text| text.as_deref()),
                    Ok(Some(expected)),
                    "case {case:?} (pass {pass}): expected {expected:?}, got {carried:?}"
                );
            }
        }
    }

    /// A value larger than the allowance the member it fills may carry is
    /// refused rather than delivered, and the reason is reported.
    #[test]
    fn a_value_past_the_allowance_is_refused() {
        let event = event();
        let mut budget = EvaluationBudget::new();
        let refused = configured_text("{% $events.Data %}", &event, 4, &mut budget);
        assert!(
            refused.is_err(),
            "expected a value past a 4-byte allowance to be refused, got {refused:?}"
        );
        // The same expression inside the scalar allowance produces its text.
        let carried = configured_text(
            "{% $events.Data.detail.orderId %}",
            &event,
            MAX_SCALAR_BYTES,
            &mut budget,
        );
        assert_eq!(
            carried.as_ref().map(|text| text.as_deref()),
            Ok(Some("A-1")),
            "expected the payload's orderId, got {carried:?}"
        );
    }

    /// RULE: the expressions of ONE call are bounded in NUMBER as well as
    /// individually, so a configuration cannot buy more evaluation work than one
    /// budget sells however many values it writes an expression in. A literal
    /// costs nothing, because nothing is evaluated for it.
    #[test]
    fn the_expressions_of_one_call_are_bounded_in_number() {
        let event = event();
        let mut budget = EvaluationBudget::new();
        for spent in 0..MAX_EVALUATIONS_PER_CALL {
            let carried = configured_text(
                "{% $events.Data.detail.orderId %}",
                &event,
                MAX_SCALAR_BYTES,
                &mut budget,
            );
            assert_eq!(
                carried.as_ref().map(|text| text.as_deref()),
                Ok(Some("A-1")),
                "case {spent}: an evaluation inside the budget produces its value, got \
                 {carried:?}"
            );
        }
        let refused = configured_text(
            "{% $events.Data.detail.orderId %}",
            &event,
            MAX_SCALAR_BYTES,
            &mut budget,
        );
        assert!(
            refused.is_err(),
            "expected the evaluation past the budget to be refused, got {refused:?}"
        );
        let literal = configured_text("plain", &event, MAX_SCALAR_BYTES, &mut budget);
        assert_eq!(
            literal.as_ref().map(|text| text.as_deref()),
            Ok(Some("plain")),
            "a literal is carried whatever the budget holds, got {literal:?}"
        );
    }
}
